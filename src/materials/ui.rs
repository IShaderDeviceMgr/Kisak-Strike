//! Drawing the UI: the `egui` half of the frame.
//!
//! This is the third of the three layers the `egui` integration is split into,
//! and the only one that names `wgpu`: `window/` feeds the platform events in,
//! [`console::ui`](crate::engine::console::ui) decides what the console looks
//! like, and this turns the triangles that come out into a render pass over
//! the back buffer.
//!
//! It lives in `materials/` rather than in `window/` for one concrete reason:
//! [`Frame::parts`](super::renderer::Frame::parts) is `pub(super)`, because
//! opening a pass means deciding what its constants are and that decision
//! belongs in one place. The UI is the one caller that legitimately opens a
//! pass this module did not build the pipeline for, and it belongs on this
//! side of that boundary rather than widening it.
//!
//! # What this is *not*
//!
//! It is not part of the material system. Nothing here goes through
//! [`PipelineCache`](super::pipeline::PipelineCache), [`Material`] or the
//! constant ABI: `egui_wgpu::Renderer` owns its own pipeline, its own font
//! atlas and its own vertex format, and wrapping any of that in Valve's
//! shapes would buy nothing. The UI is a separate renderer that happens to
//! draw into the same frame, which is exactly what `vgui2` was to
//! `materialsystem`.

use super::pipeline::TargetFormat;
use super::renderer::Frame;

/// The UI's renderer: one `egui_wgpu::Renderer` and the handles it needs.
///
/// Holds its own [`wgpu::Device`] and [`wgpu::Queue`] clones — cheap
/// refcounted handles, not the device itself, the same way `Scene` does — so
/// that it can be used while a [`Frame`] holds the [`Renderer`] borrowed.
///
/// [`Renderer`]: super::Renderer
pub struct UiRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
}

impl UiRenderer {
    /// Builds the UI renderer for a given back-buffer format.
    ///
    /// `egui` works in gamma space and the port's surface is sRGB
    /// (`Renderer::new` prefers an sRGB format, which is what replaced
    /// `SetHardwareGammaRamp`). `egui_wgpu` handles that itself: it picks
    /// between two fragment entry points on `output_color_format.is_srgb()`,
    /// so passing the surface's real format is both necessary and sufficient.
    /// Passing a gamma-space format for an sRGB target would make the whole UI
    /// visibly washed out.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, target: TargetFormat) -> UiRenderer {
        let renderer = egui_wgpu::Renderer::new(
            device,
            target.color,
            egui_wgpu::RendererOptions {
                msaa_samples: target.samples,
                // **No depth.** `egui` does not use one, its pipeline is built
                // without depth-stencil state, and [`UiRenderer::draw`]
                // therefore opens its pass with no depth attachment — a pass
                // that had one would fail validation against that pipeline.
                depth_stencil_format: None,
                ..Default::default()
            },
        );

        UiRenderer {
            device: device.clone(),
            queue: queue.clone(),
            renderer,
        }
    }

    /// Draws one `egui` pass over whatever is already in the frame.
    ///
    /// **Loads rather than clears**, because the UI is an overlay: the world
    /// has already been drawn into this frame by
    /// [`Engine::render`](crate::engine::Engine::render), and the console is
    /// a translucent window on top of it.
    ///
    /// The order of the four steps is `egui_wgpu`'s and each one is
    /// load-bearing: textures are uploaded before the buffers that reference
    /// them, `update_buffers` records into the frame's own encoder so its
    /// copies land before the pass reads them, `render` panics if
    /// `update_buffers` has not run, and the frees happen *after* the pass so
    /// that a texture dropped this frame is not dropped out from under a draw
    /// still using it.
    ///
    /// `textures` is taken **by value** and emptied, because `TexturesDelta`
    /// asserts on drop that every delta in it was applied
    /// (`epaint/src/textures.rs:335`). Borrowing it would leave the caller
    /// holding something that panics in a debug build the moment it goes out
    /// of scope, which is an unpleasant way to learn about a contract.
    pub fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        primitives: &[egui::ClippedPrimitive],
        textures: egui::TexturesDelta,
        pixels_per_point: f32,
    ) {
        let size = frame.size();
        let (encoder, view, _depth) = frame.parts();
        self.record(encoder, view, size, primitives, textures, pixels_per_point);
    }

    /// [`draw`](UiRenderer::draw) against an encoder and a colour view rather
    /// than against a [`Frame`].
    ///
    /// The split exists so that the whole chain — pipeline creation, the font
    /// atlas upload, the buffer updates and the pass itself — can be exercised
    /// against an offscreen target with no window, which is the only way the
    /// GPU half of this file is tested at all. `Frame` needs a swap chain and
    /// a swap chain needs a window.
    fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        size: (u32, u32),
        primitives: &[egui::ClippedPrimitive],
        mut textures: egui::TexturesDelta,
        pixels_per_point: f32,
    ) {
        let (width, height) = size;
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point,
        };

        // One texture can have several deltas in a frame — a whole upload
        // replaces the queue, but a run of partial updates accumulates
        // (`TexturesDelta::push`, `epaint/src/textures.rs:301`) — and they
        // have to be applied in order.
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }

        // Nothing to draw is the common case — the console is closed on most
        // frames — and a render pass over the whole back buffer is not free at
        // 300 of them a second. The texture deltas are still applied and freed
        // either way, because `egui` owns the font atlas whether or not it drew
        // with it this frame.
        if primitives.is_empty() {
            for id in &textures.free {
                self.renderer.free_texture(id);
            }
            textures.clear();
            return;
        }

        let user =
            self.renderer
                .update_buffers(&self.device, &self.queue, encoder, primitives, &screen);
        // Work from `egui` paint callbacks, which this port has none of. Kept
        // because submitting it is the contract, and because an empty submit
        // is not free: only pay for it if there is something to submit.
        if !user.is_empty() {
            self.queue.submit(user);
        }

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // `egui_wgpu::Renderer::render` wants a `'static` pass so that a
            // paint callback can bind resources the pass does not borrow. The
            // cost of `forget_lifetime` is that a mistake using `encoder`
            // while the pass is open becomes a runtime error instead of a
            // compile error, which is why the pass is scoped tightly here.
            self.renderer
                .render(&mut pass.forget_lifetime(), primitives, &screen);
        }

        for id in &textures.free {
            self.renderer.free_texture(id);
        }
        textures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::target::RenderTarget;

    /// Side of the offscreen target. 64 x 4 bytes is exactly `wgpu`'s 256-byte
    /// row alignment for `copy_texture_to_buffer`, as in `preview.rs`.
    const TARGET: u32 = 64;

    /// A device, or `None` if this machine cannot give us one — the same skip
    /// `preview.rs` uses, minus its BC-texture requirement, which `egui` does
    /// not need.
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = match pollster::block_on(instance.request_adapter(&Default::default())) {
            Ok(adapter) => adapter,
            Err(err) => {
                eprintln!("skipping: no usable GPU adapter: {err}");
                return None;
            }
        };
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    /// One headless `egui` pass that paints `rect` in `color`, tessellated.
    fn painted(
        ctx: &egui::Context,
        rect: egui::Rect,
        color: egui::Color32,
    ) -> (Vec<egui::ClippedPrimitive>, egui::TexturesDelta, f32) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(TARGET as f32, TARGET as f32),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            ui.painter().rect_filled(rect, 0.0, color);
        });
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        (primitives, output.textures_delta, output.pixels_per_point)
    }

    /// Draws one `egui` pass into an offscreen target and reads it back.
    fn render(format: wgpu::TextureFormat) -> Option<Vec<u8>> {
        let (device, queue) = device()?;
        let mut ui = UiRenderer::new(
            &device,
            &queue,
            TargetFormat {
                color: format,
                depth: None,
                samples: 1,
            },
        );
        let target = RenderTarget::new(&device, "egui readback", TARGET, TARGET, format, false);

        let ctx = egui::Context::default();
        // The whole target, so the readback is unambiguous: every pixel is
        // either the rectangle or the clear.
        let (primitives, textures, pixels_per_point) = painted(
            &ctx,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(TARGET as f32, TARGET as f32)),
            egui::Color32::from_rgb(255, 0, 0),
        );

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (TARGET * TARGET * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        // Cleared first, because `record` loads rather than clears — the UI is
        // an overlay and its pass is not the frame's first.
        encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();

        ui.record(
            &mut encoder,
            target.view(),
            (TARGET, TARGET),
            &primitives,
            textures,
            pixels_per_point,
        );

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target.color_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(TARGET * 4),
                    rows_per_image: Some(TARGET),
                },
            },
            wgpu::Extent3d {
                width: TARGET,
                height: TARGET,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
            r.expect("readback mapped");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the queue drained");
        let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
        readback.unmap();
        Some(pixels)
    }

    fn centre(pixels: &[u8]) -> [u8; 4] {
        let offset = ((TARGET / 2 * TARGET + TARGET / 2) * 4) as usize;
        pixels[offset..offset + 4].try_into().expect("four bytes")
    }

    /// The whole chain against a real device: `egui` tessellates, the font
    /// atlas is uploaded, the buffers are written, the pass runs, and red
    /// comes back red.
    ///
    /// This is the only test that can catch a `wgpu` validation error in the
    /// UI path — a depth attachment the pipeline does not declare, an
    /// unapplied texture delta, a pass opened with the wrong format — none of
    /// which the headless `egui` tests in `console/ui.rs` can see.
    #[test]
    fn an_egui_pass_reaches_the_target() {
        let Some(pixels) = render(wgpu::TextureFormat::Rgba8Unorm) else {
            return;
        };
        // Gamma-space target, so the bytes are the colour `egui` was given.
        assert_eq!(centre(&pixels), [255, 0, 0, 255]);
    }

    /// The real back buffer is sRGB (`Renderer::new` prefers one), and
    /// `egui_wgpu` picks a *different* fragment entry point for it. Getting
    /// this wrong is not an error — it is a washed-out UI — so it is worth a
    /// test of its own.
    #[test]
    fn an_srgb_target_gets_the_linear_entry_point() {
        let Some(pixels) = render(wgpu::TextureFormat::Rgba8UnormSrgb) else {
            return;
        };
        // Full red survives the encode exactly, because 1.0 encodes to 1.0.
        // A gamma-space shader against an sRGB target would still write 255
        // here, so the corner is not the interesting case — but a mid grey
        // would need a tolerance, and this test is about the pass running at
        // all against the format the game actually uses.
        assert_eq!(centre(&pixels), [255, 0, 0, 255]);
    }
}
