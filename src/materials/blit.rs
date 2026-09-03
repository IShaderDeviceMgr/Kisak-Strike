//! Drawing one texture over the whole frame.
//!
//! **This is stage 2's verification path and nothing more.** It exists because
//! `portdocs/MATERIALSYSTEM.md` §9 makes the deliverable of the texture stage
//! "a full-screen textured quad from a real `.vtf` out of a VPK" — a `.vtf`
//! that parses is not evidence that the bytes reached the GPU in the right
//! order, in the right format, with the right rows; a picture on the screen is.
//!
//! It is **not** the beginning of the material system. Stage 3 brings the real
//! pipeline: a `.vmt`-driven bind-group layout (§7.4), the WGSL prelude (§7.5)
//! and a pipeline cache keyed on real state. Nothing here should grow features
//! — when the `UnlitGeneric` path can draw a quad, delete this file.

use super::renderer::Frame;
use super::texture::Texture;

/// A pipeline that draws one bound texture across the frame.
///
/// Owns its bind group, so it is built per texture rather than per frame.
pub struct TextureBlit {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

impl TextureBlit {
    /// Builds the pipeline and binds `texture` to it.
    ///
    /// `None` if the texture is not a plain 2D one. The shader declares
    /// `texture_2d<f32>`, and a cubemap or volume texture needs a different
    /// binding type and different sampling coordinates. Handling all three
    /// would mean three pipelines and three shaders for a debug path that
    /// stage 3 deletes.
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        texture: &Texture,
    ) -> Option<TextureBlit> {
        if texture.view_dimension != wgpu::TextureViewDimension::D2 {
            return None;
        }

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blit.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture.view()),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(texture.sampler()),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            // No immediate (push-constant) data: the blit's only input is the
            // bound texture.
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                // No vertex buffer: the triangle is generated from the vertex
                // index. See shaders/blit.wgsl.
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // Opaque: this replaces the frame rather than composing
                    // over it, so a texture's alpha is shown as colour rather
                    // than silently blended away.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Some(TextureBlit {
            pipeline,
            bind_group,
        })
    }

    /// Records the draw into an in-progress pass.
    pub(super) fn record(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

impl Frame<'_> {
    /// Draws a texture over the whole frame, replacing whatever was there.
    ///
    /// Stage 2's only draw call. See [`TextureBlit`] for why it is temporary.
    pub fn blit(&mut self, blit: &TextureBlit) {
        let mut pass = self.begin_color_pass("blit", wgpu::LoadOp::Load);
        blit.record(&mut pass);
    }
}

/// End-to-end checks: a `.vtf`'s bytes, through the loader, onto the GPU,
/// through the shader, and back to the CPU.
///
/// These are the only tests in `src/materials/` that touch a GPU, and they earn
/// it. Everything between `Vtf::parse` and a pixel — row pitch, block layout,
/// channel order, the vertical flip between clip space and texture space — is
/// invisible to a unit test and produces a *plausible* wrong picture rather than
/// a crash. Rendering to an offscreen target and reading it back is the only way
/// to pin it down without a person looking at a window.
///
/// They **skip** rather than fail where there is no usable adapter, so a
/// machine with no GPU (or no BC support) still gets a green `cargo test`. The
/// skip prints, so it cannot quietly become "these never ran anywhere".
#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::image_format::{ColorSpace, ImageFormat};
    use crate::materials::texture::{sampler_key, Texture};
    use crate::materials::vtf::{TextureFlags, Vtf};

    /// Side of the offscreen target. 64 x 4 bytes is exactly `wgpu`'s 256-byte
    /// row alignment for `copy_texture_to_buffer`, so the readback needs no
    /// padding arithmetic to get wrong.
    const TARGET: u32 = 64;

    /// A single-mip VTF 7.5 in memory. Version 7.5 has the resource
    /// dictionary, so this also exercises the path shipped content takes.
    fn vtf_bytes(format: ImageFormat, width: u16, height: u16, flags: u32, image: &[u8]) -> Vec<u8> {
        let mut file = vec![0u8; 80];
        file[0..4].copy_from_slice(b"VTF\0");
        file[4..8].copy_from_slice(&7u32.to_le_bytes());
        file[8..12].copy_from_slice(&5u32.to_le_bytes());
        file[12..16].copy_from_slice(&88u32.to_le_bytes());
        file[16..18].copy_from_slice(&width.to_le_bytes());
        file[18..20].copy_from_slice(&height.to_le_bytes());
        file[20..24].copy_from_slice(&flags.to_le_bytes());
        file[24..26].copy_from_slice(&1u16.to_le_bytes());
        file[48..52].copy_from_slice(&1.0f32.to_le_bytes());
        file[52..56].copy_from_slice(&(format as i32).to_le_bytes());
        file[56] = 1;
        file[57..61].copy_from_slice(&(-1i32).to_le_bytes());
        file[63..65].copy_from_slice(&1u16.to_le_bytes());
        file[68..72].copy_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&0x30u32.to_le_bytes());
        file.extend_from_slice(&88u32.to_le_bytes());
        file.extend_from_slice(image);
        file
    }

    /// A device, or `None` if this machine cannot give us one.
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
        );
        let adapter = match pollster::block_on(instance.request_adapter(&Default::default())) {
            Ok(adapter) => adapter,
            Err(err) => {
                eprintln!("skipping: no usable GPU adapter: {err}");
                return None;
            }
        };
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            eprintln!("skipping: adapter has no BC texture support");
            return None;
        }
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
            ..Default::default()
        }))
        .ok()
    }

    /// Draws `vtf` over a `TARGET`-square image and reads it back as RGBA8.
    ///
    /// The target is `Rgba8Unorm` and the texture is loaded as `Linear`, so no
    /// sRGB curve is applied in either direction and the bytes that come back
    /// are the bytes that went in. Encoding is a separate concern and the swap
    /// chain's, not this path's.
    fn load(device: &wgpu::Device, queue: &wgpu::Queue, vtf: &Vtf) -> Texture {
        let sampler = device.create_sampler(&sampler_key(vtf.flags, vtf.mip_count).descriptor());
        Texture::from_vtf(device, queue, "test", vtf, 0, ColorSpace::Linear, sampler)
            .expect("uploadable")
    }

    fn render(device: &wgpu::Device, queue: &wgpu::Queue, texture: &Texture) -> Vec<u8> {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let blit = TextureBlit::new(device, format, texture).expect("2D texture");

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("readback target"),
            size: wgpu::Extent3d {
                width: TARGET,
                height: TARGET,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (TARGET * TARGET * 4) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit test"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            blit.record(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
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
            r.expect("map readback buffer");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("gpu work to finish");
        let pixels = readback
            .slice(..)
            .get_mapped_range()
            .expect("mapped readback buffer")
            .to_vec();
        readback.unmap();
        pixels
    }

    /// The RGBA at an output pixel.
    fn pixel(image: &[u8], x: u32, y: u32) -> [u8; 4] {
        let at = ((y * TARGET + x) * 4) as usize;
        image[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn an_uncompressed_texture_reaches_the_screen_the_right_way_up() {
        let Some((device, queue)) = device() else {
            return;
        };

        // 2x2 BGRA8888, one distinct colour per texel, written top row first —
        // the order a `.vtf` stores rows in.
        //
        //   red   green
        //   blue  white
        //
        // Point-sampled and clamped so each output pixel is exactly one source
        // texel: with linear filtering a channel-order or flip mistake could
        // hide inside an interpolated edge.
        #[rustfmt::skip]
        let image: Vec<u8> = vec![
            0, 0, 255, 255,   0, 255, 0, 255,
            255, 0, 0, 255,   255, 255, 255, 255,
        ];
        let flags =
            TextureFlags::POINT_SAMPLE.0 | TextureFlags::CLAMP_S.0 | TextureFlags::CLAMP_T.0;
        let vtf = Vtf::parse(vtf_bytes(ImageFormat::Bgra8888, 2, 2, flags, &image))
            .expect("valid vtf");
        let out = render(&device, &queue, &load(&device, &queue, &vtf));

        // Top-left of the *image* must be top-left on screen. Clip space puts
        // -1 at the bottom and texture space puts v=0 at the top, so getting
        // this wrong renders a perfectly plausible upside-down picture.
        assert_eq!(pixel(&out, 0, 0), [255, 0, 0, 255], "top-left is red");
        assert_eq!(
            pixel(&out, TARGET - 1, 0),
            [0, 255, 0, 255],
            "top-right is green"
        );
        assert_eq!(
            pixel(&out, 0, TARGET - 1),
            [0, 0, 255, 255],
            "bottom-left is blue"
        );
        assert_eq!(
            pixel(&out, TARGET - 1, TARGET - 1),
            [255, 255, 255, 255],
            "bottom-right is white"
        );
    }

    #[test]
    fn a_dxt1_texture_is_decoded_by_the_hardware() {
        let Some((device, queue)) = device() else {
            return;
        };

        // One 4x4 DXT1 block of solid magenta: both endpoints the same colour
        // and every index 0, which is the degenerate encoding every DXT1
        // decoder agrees on.
        let magenta = ((255u16 >> 3) << 11) | ((0u16 >> 2) << 5) | (255u16 >> 3);
        let mut block = Vec::new();
        block.extend_from_slice(&magenta.to_le_bytes());
        block.extend_from_slice(&magenta.to_le_bytes());
        block.extend_from_slice(&[0, 0, 0, 0]);

        let vtf = Vtf::parse(vtf_bytes(ImageFormat::Dxt1, 4, 4, 0, &block)).expect("valid vtf");
        assert_eq!(vtf.format, ImageFormat::Dxt1);

        let out = render(&device, &queue, &load(&device, &queue, &vtf));
        // 565 cannot hold 255 exactly in green's neighbours, but red and blue
        // are all-ones and must come back saturated.
        let centre = pixel(&out, TARGET / 2, TARGET / 2);
        assert_eq!(centre[0], 255, "red channel of a solid magenta block");
        assert_eq!(centre[1], 0, "green channel");
        assert_eq!(centre[2], 255, "blue channel");
        assert_eq!(centre[3], 255, "DXT1 without alpha is opaque");
    }

    #[test]
    fn a_missing_texture_draws_the_error_checkerboard() {
        let Some((device, queue)) = device() else {
            return;
        };

        // Nearest filtering by default, so each output pixel is one texel of
        // the 32x32 checkerboard scaled 2x into the 64x64 target.
        let sampler = device.create_sampler(&Default::default());
        let error = Texture::error(&device, &queue, sampler);
        assert_eq!((error.width, error.height), (32, 32));

        // This is what every later stage falls back to, so the colours and the
        // 4-texel period are worth pinning down: `CCheckerboardTexture` at
        // `texturemanager.cpp:96` alternates `(255,0,255,255)` and
        // `(0,0,0,128)` on `(x & 4) ^ (y & 4)`.
        let out = render(&device, &queue, &error);
        for (x, y, expected) in [
            (0, 0, [255, 0, 255, 255]),
            (7, 7, [255, 0, 255, 255]),
            (8, 0, [0, 0, 0, 128]),
            (0, 8, [0, 0, 0, 128]),
            (8, 8, [255, 0, 255, 255]),
        ] {
            assert_eq!(pixel(&out, x, y), expected, "checkerboard at {x},{y}");
        }
    }
}
