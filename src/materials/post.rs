//! The post-processing chain: render the scene somewhere it can be read, look
//! at it, put it on the screen.
//!
//! `DoEnginePostProcessing` (`game/client/viewpostprocess.cpp:2485`) reduced to
//! the two steps auto exposure needs — `UpdateScreenEffectTexture`, which made
//! the frame readable, and the final full-screen pass that put it back. Bloom,
//! colour correction, depth of field, the software AA and the vignette are the
//! rest of that function and are not ported; each is a screen-space pass
//! between these two, which is why the seam is here rather than somewhere it
//! would have to move.
//!
//! # Why the scene stops being drawn straight to the back buffer
//!
//! A swap-chain image is a render attachment and nothing else: it cannot be
//! sampled, and on some backends it cannot be copied from either. The exposure
//! controller has to measure the frame it is exposing, so the frame has to
//! exist somewhere a shader can read. That is exactly what
//! `_rt_FullFrameFB` was, and [`PostProcess::scene`] is it — with the copy
//! folded into the rendering rather than done afterwards, because there is no
//! reason to draw into one texture and then copy it into another.
//!
//! The cost is one full-screen textured triangle per frame and one screen-sized
//! texture. The benefit, beyond exposure, is that portal views, water
//! reflections and the rest of the post chain all need this target to exist.
//!
//! # What it does not change
//!
//! **The scene target has the back buffer's exact format**, so every pipeline
//! built for the back buffer draws into it unchanged —
//! [`TargetFormat`](super::pipeline::TargetFormat) is part of
//! [`PipelineKey`](super::pipeline::PipelineKey), and a different colour format
//! here would double the pipeline count. It is 8-bit and sRGB for the same
//! reason Valve's was: this port's shaders apply the exposure scalar
//! themselves and write encoded, which is `HDR_TYPE_INTEGER`'s frame buffer.
//! A float target would be `HDR_TYPE_FLOAT` and is a separate decision — see
//! `portdocs/MATERIALSYSTEM.md` §10.
//!
//! The UI is drawn *after* [`PostProcess::resolve`], straight onto the back
//! buffer, so it is not measured and not resolved. That is where `vgui` sat
//! too: `DoEnginePostProcessing`'s `bPostVGui` argument exists precisely
//! because the one thing drawn after the UI is a second AA pass.

use super::histogram::{Counts, Histogram, Region};
use super::pipeline::TargetFormat;
use super::renderer::Frame;
use super::target::RenderTarget;

/// The scene target, the thing that measures it, and the pass that presents it.
pub struct PostProcess {
    device: wgpu::Device,
    /// The back buffer's format, which the scene target copies. Cached because
    /// reallocating the target needs it and a [`Frame`] is not always in hand.
    format: TargetFormat,
    scene: Option<RenderTarget>,
    blit: Blit,
    histogram: Histogram,
}

impl PostProcess {
    /// Builds the chain for a given back-buffer format and set of histogram
    /// bucket boundaries.
    ///
    /// `bounds` is the tone mapper's bucket table — see
    /// [`Histogram::new`]. It is a parameter rather than a constant because
    /// what the buckets are is exposure policy, and this module holds none.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: TargetFormat,
        bounds: &[f32],
    ) -> PostProcess {
        PostProcess {
            blit: Blit::new(device, format.color),
            histogram: Histogram::new(device, queue, bounds),
            format,
            scene: None,
            device: device.clone(),
        }
    }

    /// The offscreen target this frame's scene should be drawn into.
    ///
    /// Allocated on first use and reallocated when the size changes, which is
    /// a window resize. Both the measurement and the presenting pass are
    /// repointed at the new texture here, so a caller that draws into what this
    /// returns cannot end up measuring or presenting the old one.
    ///
    /// # Panics
    ///
    /// If either dimension is zero. A frame is only acquired when the surface
    /// is configured, and a surface with no area is not configured — see
    /// [`Renderer::begin_frame`](super::Renderer::begin_frame).
    pub fn scene(&mut self, size: (u32, u32)) -> &RenderTarget {
        let (width, height) = size;
        assert!(width > 0 && height > 0, "scene target {width}x{height}");
        if self.scene.as_ref().map(RenderTarget::size) != Some(size) {
            let target = RenderTarget::new(
                &self.device,
                "scene",
                width,
                height,
                self.format.color,
                // Its own depth buffer. The one the renderer keeps for the back
                // buffer is still there and is still what a pass drawn straight
                // to the screen uses; a depth attachment must match its colour
                // attachment's dimensions, not merely its size, so they cannot
                // be the same allocation.
                true,
            );
            self.blit.set_source(&self.device, target.texture().view());
            self.histogram.set_source(target.texture().view());
            self.scene = Some(target);
        }
        self.scene.as_ref().expect("just allocated")
    }

    /// The newest luminance measurement that has come back from the GPU.
    ///
    /// Call once at the top of a frame, before anything is recorded: this is
    /// also what arms the readback of the previous frame's measurement, and
    /// that is only safe once the previous frame has been submitted. See
    /// [`Histogram::take`].
    ///
    /// `None` means no measurement finished this frame — because none was
    /// recorded, or because the GPU is still working on it. The caller should
    /// leave the exposure where it is, not reset it.
    pub fn measurement(&mut self) -> Option<Counts> {
        self.histogram.take()
    }

    /// Measures the scene and puts it on the back buffer.
    ///
    /// `measure` is the fraction of the target's width and height the exposure
    /// should be taken over — `mat_exposure_center_region_x`/`_y`. `None`
    /// measures nothing and only presents, which is what
    /// `mat_dynamic_tonemapping 0` asks for.
    ///
    /// Measuring happens **before** the presenting pass and against the scene
    /// target rather than the back buffer, which is the whole reason the target
    /// exists. Nothing drawn after this — the UI — is measured.
    ///
    /// Does nothing at all if no scene target has been allocated, which is the
    /// case for a frame that drew straight to the screen.
    pub fn resolve(&mut self, frame: &mut Frame<'_>, measure: Option<(f32, f32)>) {
        let (encoder, view, _depth) = frame.parts();
        self.record(encoder, view, measure);
    }

    /// [`resolve`](PostProcess::resolve) against an encoder and a colour view
    /// rather than against a [`Frame`].
    ///
    /// Public for the same reason
    /// [`RenderContext::offscreen_pass`](super::context::RenderContext::offscreen_pass)
    /// is: a `Frame` needs a swap chain and a swap chain needs a window, so
    /// this is the only way the chain can be driven by a screenshot, a warm-up
    /// or a test. The caller submits the encoder itself.
    pub fn record(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        destination: &wgpu::TextureView,
        measure: Option<(f32, f32)>,
    ) {
        let Some(scene) = &self.scene else {
            return;
        };
        let (width, height) = scene.size();

        if let Some((fraction_x, fraction_y)) = measure {
            self.histogram.record(
                encoder,
                Region::centered(width, height, fraction_x, fraction_y),
            );
        }
        self.blit.record(encoder, destination);
    }

    /// How many buckets a [`measurement`](PostProcess::measurement) comes back
    /// with. The tone mapper's own bucket count, round-tripped.
    #[allow(dead_code)]
    pub fn buckets(&self) -> usize {
        self.histogram.len()
    }
}

/// One texture onto another, full screen.
///
/// `shaders/blit.wgsl` and the pipeline for it. Separate from
/// [`PipelineCache`](super::pipeline::PipelineCache) for the same reason
/// [`UiRenderer`](super::UiRenderer) is: it has no material, no vertex buffer
/// and none of the constant ABI, and wrapping it in those shapes would buy
/// nothing.
struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bind_group: Option<wgpu::BindGroup>,
}

impl Blit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Blit {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blit.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        Blit {
            pipeline: device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("blit"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // The full-screen triangle is written in one winding and
                    // reaches the rasterizer in another depending on nothing at
                    // all, so it is simply never culled.
                    cull_mode: None,
                    ..Default::default()
                },
                // **No depth attachment**, which the presenting pass must also
                // leave off — a pass with one would fail validation against
                // this pipeline. The scene target has already resolved every
                // depth question by the time this runs.
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            }),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("blit"),
                // Source and destination are the same size, so every fetch is
                // on a texel centre and `Nearest` is exact. Linear would give
                // the same answer at 1:1 and quietly soften the picture if a
                // resolution scale ever made them differ.
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            }),
            layout,
            bind_group: None,
        }
    }

    /// Points the pass at a texture. See
    /// [`Histogram::set_source`](super::histogram::Histogram::set_source) for
    /// why this is explicit.
    fn set_source(&mut self, device: &wgpu::Device, source: &wgpu::TextureView) {
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        }));
    }

    fn record(&self, encoder: &mut wgpu::CommandEncoder, destination: &wgpu::TextureView) {
        let Some(bind_group) = &self.bind_group else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Every texel is written, so there is nothing to preserve.
                    // `Clear` rather than `Load` because on a tile-based GPU —
                    // which is every Apple one — `Load` pulls the previous
                    // contents into tile memory and `Clear` does not, and the
                    // pull is pure waste when the whole attachment is about to
                    // be overwritten.
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::target::DEPTH_FORMAT;

    /// 64 x 4 bytes is exactly `wgpu`'s 256-byte row alignment for
    /// `copy_texture_to_buffer`, so the readback needs no padding arithmetic to
    /// get wrong. Same constant and same reason as `preview.rs`'s.
    const SIZE: u32 = 64;

    /// The back buffer's format, as the real one is: sRGB, so that the round
    /// trip through the scene target is the round trip the game does.
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    fn target_format() -> TargetFormat {
        TargetFormat {
            color: FORMAT,
            depth: Some(DEPTH_FORMAT),
            samples: 1,
        }
    }

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

    /// `pow( i / 16, 2.5 )`, the tone mapper's distribution. See
    /// [`super::super::histogram`]'s tests for why it is rebuilt rather than
    /// imported.
    fn bounds() -> [f32; 17] {
        let mut bounds = [0.0f32; 17];
        for (i, bound) in bounds.iter_mut().enumerate() {
            *bound = (i as f32 / 16.0).powf(2.5);
        }
        bounds
    }

    /// Clears a render target to a colour, the way a scene pass would if it
    /// drew nothing.
    fn clear(encoder: &mut wgpu::CommandEncoder, target: &RenderTarget, color: wgpu::Color) {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view(),
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// The bytes of a render target, after everything queued has run.
    fn readback(device: &wgpu::Device, queue: &wgpu::Queue, target: &RenderTarget) -> Vec<u8> {
        let (width, height) = target.size();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (width * height * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            target.color_texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the readback completes");
        let bytes = buffer
            .slice(..)
            .get_mapped_range()
            .expect("mapped")
            .to_vec();
        buffer.unmap();
        bytes
    }

    #[test]
    fn the_scene_target_matches_the_back_buffer_and_is_reused_until_the_size_changes() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut post = PostProcess::new(&device, &queue, target_format(), &bounds());

        // Every world pipeline is built for the back buffer's format, so a
        // scene target that differed would double the pipeline count and fail
        // validation on every draw.
        assert_eq!(post.scene((SIZE, SIZE)).format(), target_format());
        assert_eq!(post.scene((SIZE, SIZE)).size(), (SIZE, SIZE));

        // Reused, not rebuilt: what was drawn into it last frame is still
        // there. Asking again every frame is the API, so reallocating every
        // frame would be a screen-sized texture per frame.
        let mut encoder = device.create_command_encoder(&Default::default());
        clear(&mut encoder, post.scene((SIZE, SIZE)), wgpu::Color::WHITE);
        queue.submit([encoder.finish()]);
        assert_eq!(readback(&device, &queue, post.scene((SIZE, SIZE)))[0], 255);

        // A resize rebuilds it, and the new one has not been drawn into.
        assert_eq!(post.scene((SIZE * 2, SIZE)).size(), (SIZE * 2, SIZE));
        assert_eq!(
            readback(&device, &queue, post.scene((SIZE * 2, SIZE)))[0],
            0
        );
    }

    #[test]
    fn the_scene_reaches_the_back_buffer_unchanged() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut post = PostProcess::new(&device, &queue, target_format(), &bounds());
        let back = RenderTarget::new(&device, "back buffer", SIZE, SIZE, FORMAT, false);

        // A colour with a different value in every channel, so a swapped
        // component is caught. Clear values are linear and the target is sRGB,
        // so the hardware encodes on the way in.
        let color = wgpu::Color {
            r: 0.1,
            g: 0.4,
            b: 0.8,
            a: 1.0,
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        clear(&mut encoder, post.scene((SIZE, SIZE)), color);
        post.record(&mut encoder, back.view(), None);
        queue.submit([encoder.finish()]);

        let scene = readback(&device, &queue, post.scene((SIZE, SIZE)));
        let presented = readback(&device, &queue, &back);
        assert_eq!(presented.len(), (SIZE * SIZE * 4) as usize);
        // Both are sRGB-encoded bytes, and decode-then-encode is an identity to
        // within a step of 255. The blit is a copy, not a colour transform.
        for (i, (a, b)) in scene.iter().zip(&presented).enumerate() {
            assert!(
                a.abs_diff(*b) <= 1,
                "texel byte {i}: scene {a}, presented {b}"
            );
        }
        // ...and not a copy of something black. `0.1` linear encodes to about
        // 89, `0.4` to about 168, `0.8` to about 231.
        assert!(presented[0] > 80 && presented[0] < 100, "{}", presented[0]);
        assert!(presented[2] > 220, "{}", presented[2]);
    }

    #[test]
    fn the_measurement_is_of_the_scene_and_not_of_the_back_buffer() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut post = PostProcess::new(&device, &queue, target_format(), &bounds());
        let back = RenderTarget::new(&device, "back buffer", SIZE, SIZE, FORMAT, false);
        assert_eq!(post.buckets(), 16);

        // Mid-grey in *linear* light, which is what the buckets are calibrated
        // in: 0.2159 sits inside bucket 8, `[0.1768, 0.2373)`.
        let grey: f64 = 0.215_86;
        let mut encoder = device.create_command_encoder(&Default::default());
        clear(
            &mut encoder,
            post.scene((SIZE, SIZE)),
            wgpu::Color {
                r: grey,
                g: grey,
                b: grey,
                a: 1.0,
            },
        );
        post.record(&mut encoder, back.view(), Some((1.0, 1.0)));
        queue.submit([encoder.finish()]);

        let counts = (0..1000)
            .find_map(|_| {
                let taken = post.measurement();
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
                taken
            })
            .expect("the measurement comes back");
        assert_eq!(counts.total(), SIZE * SIZE);
        assert_eq!(
            counts.buckets[8],
            SIZE * SIZE,
            "mid-grey binned as {:?}",
            counts.as_slice()
        );
    }

    #[test]
    fn no_measurement_is_taken_when_none_is_asked_for() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut post = PostProcess::new(&device, &queue, target_format(), &bounds());
        let back = RenderTarget::new(&device, "back buffer", SIZE, SIZE, FORMAT, false);
        let mut encoder = device.create_command_encoder(&Default::default());
        clear(&mut encoder, post.scene((SIZE, SIZE)), wgpu::Color::WHITE);
        // `mat_dynamic_tonemapping 0`: still presented, never measured.
        post.record(&mut encoder, back.view(), None);
        queue.submit([encoder.finish()]);
        for _ in 0..8 {
            assert_eq!(post.measurement(), None);
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
    }

    #[test]
    fn resolving_before_a_scene_target_exists_does_nothing() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut post = PostProcess::new(&device, &queue, target_format(), &bounds());
        let back = RenderTarget::new(&device, "back buffer", SIZE, SIZE, FORMAT, false);
        let mut encoder = device.create_command_encoder(&Default::default());
        // The `-vmt` preview and the no-map clear both draw straight to the
        // back buffer and never ask for a scene target.
        post.record(&mut encoder, back.view(), Some((1.0, 1.0)));
        queue.submit([encoder.finish()]);
        assert_eq!(post.measurement(), None);
    }
}
