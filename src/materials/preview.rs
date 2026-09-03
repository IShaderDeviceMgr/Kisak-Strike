//! Drawing one material over the whole frame.
//!
//! **This is stage 3's verification path and nothing more**, and it replaces
//! `blit.rs`, which was stage 2's. `portdocs/MATERIALSYSTEM.md` §9 makes the
//! deliverable of the material stage "a quad drawn through a real `.vmt` and a
//! real WGSL shader": a `.vmt` that *parses* is no evidence that its shader
//! compiled, that its bind groups match their layouts, that its texture
//! transform has the right handedness, or that the matrix convention this port
//! just committed to is the one the shader reads.
//!
//! What it owns is the render context's job, not a material's: the vertex and
//! index buffers, the per-frame and per-draw uniform blocks, and the decision
//! of where the quad goes. All of that is stage 4
//! (`portdocs/MATERIALSYSTEM.md` §9), and this file is what it deletes.
//!
//! **Do not grow this.** When there is a mesh API and a render context, delete
//! `preview.rs`, `Frame::draw_material` and the `-vmt` switch together — and
//! move the tests at the bottom onto whatever draws a quad then, because they
//! are the only place the whole path is checked against real pixels.

use super::material::Material;
use super::pipeline::BindLayouts;
use super::pipeline::Vertex;
use super::renderer::Frame;
use super::uniforms::{ColumnMajor, DrawUniforms, FrameUniforms};

/// The quad, in world units: the unit square, `y` down.
///
/// `y` down so that a texture coordinate can be the position — `v = 0` is the
/// top of the image and the top of the screen — which keeps [`VIEW_PROJ`] the
/// only place the flip happens.
const QUAD: [Vertex; 4] = [
    Vertex {
        position: [0.0, 0.0, 0.0],
        texcoord: [0.0, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [1.0, 0.0, 0.0],
        texcoord: [1.0, 0.0],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [1.0, 1.0, 0.0],
        texcoord: [1.0, 1.0],
        color: [1.0, 1.0, 1.0, 1.0],
    },
    Vertex {
        position: [0.0, 1.0, 0.0],
        texcoord: [0.0, 1.0],
        color: [1.0, 1.0, 1.0, 1.0],
    },
];

/// Two triangles, wound so that they survive back-face culling.
///
/// Worth stating, because [`VIEW_PROJ`] flips `y` and that reverses the
/// winding: `FrontFace::Ccw` means counter-clockwise **in clip space**, where
/// `y` is up, so the corners have to run the other way round than they do in
/// [`QUAD`], where `y` is down. Hence `0, 2, 1` rather than `0, 1, 2`.
///
/// Getting it backwards does not draw a mirrored quad — it draws nothing at
/// all, silently, which is exactly the kind of thing the tests at the bottom of
/// this file exist for. This one was wrong first time round.
const QUAD_INDICES: [u16; 6] = [0, 2, 1, 0, 3, 2];

/// World space to clip space: the unit square, filling the viewport.
///
/// Column-major, and applied as `m * v` — the convention
/// [`uniforms`](super::uniforms) sets. Deliberately *not* the identity: with an
/// identity view-projection a transposed matrix would still draw a perfectly
/// centred quad, and the convention would go unchecked until something with a
/// real camera depended on it.
///
///   world (0, 0) -> clip (-1,  1), the top-left corner
///   world (1, 1) -> clip ( 1, -1), the bottom-right
const VIEW_PROJ: ColumnMajor = [
    [2.0, 0.0, 0.0, 0.0],
    [0.0, -2.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0, 1.0],
];

/// The geometry and the shared bind groups a preview draw needs.
///
/// One per renderer, not one per material: the material's own bind group comes
/// from the [`Material`], and everything here is what the render context would
/// own.
pub struct MaterialPreview {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    frame_uniforms: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
    draw_uniforms: wgpu::Buffer,
    draw_bind_group: wgpu::BindGroup,
}

impl MaterialPreview {
    pub fn new(device: &wgpu::Device, layouts: &BindLayouts) -> MaterialPreview {
        let vertices = buffer(
            device,
            "preview quad",
            wgpu::BufferUsages::VERTEX,
            bytemuck::cast_slice(&QUAD),
        );
        let indices = buffer(
            device,
            "preview quad",
            wgpu::BufferUsages::INDEX,
            bytemuck::cast_slice(&QUAD_INDICES),
        );

        let frame_uniforms = uniform_buffer(device, "frame", size_of::<FrameUniforms>());
        let draw_uniforms = uniform_buffer(device, "draw", size_of::<DrawUniforms>());

        MaterialPreview {
            frame_bind_group: bind(device, "frame", layouts.frame(), &frame_uniforms),
            draw_bind_group: bind(device, "draw", layouts.draw(), &draw_uniforms),
            vertices,
            indices,
            frame_uniforms,
            draw_uniforms,
        }
    }

    /// Writes the per-frame and per-draw blocks.
    ///
    /// `size` is the target's size in pixels, for `cScreenSize`. The eye is at
    /// the origin: there is no camera, and range fog is off, so nothing reads
    /// it — but it is part of the block, and writing a real value beats
    /// leaving it whatever the buffer happened to hold.
    pub fn update(&self, queue: &wgpu::Queue, size: (u32, u32), draw: &DrawUniforms) {
        let frame = FrameUniforms::new(VIEW_PROJ, [0.0, 0.0, 0.0], size);
        queue.write_buffer(&self.frame_uniforms, 0, bytemuck::bytes_of(&frame));
        queue.write_buffer(&self.draw_uniforms, 0, bytemuck::bytes_of(draw));
    }

    /// Records the draw into an in-progress pass.
    ///
    /// The pipeline is passed in rather than looked up here because asking the
    /// [`PipelineCache`](super::pipeline::PipelineCache) borrows it mutably,
    /// and a pass borrows the frame — so the lookup has to happen first.
    pub(super) fn record(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        material: &Material,
        pipeline: &wgpu::RenderPipeline,
    ) {
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &self.frame_bind_group, &[]);
        pass.set_bind_group(1, material.bind_group(), &[]);
        pass.set_bind_group(2, &self.draw_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..QUAD_INDICES.len() as u32, 0, 0..1);
    }
}

fn buffer(
    device: &wgpu::Device,
    label: &str,
    usage: wgpu::BufferUsages,
    contents: &[u8],
) -> wgpu::Buffer {
    // `wgpu::util::DeviceExt::create_buffer_init` would do this in one call,
    // and `mapped_at_creation` is the same thing without the `util` module.
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: contents.len() as u64,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .expect("a just-created mapped buffer")
        .copy_from_slice(contents);
    buffer.unmap();
    buffer
}

fn uniform_buffer(device: &wgpu::Device, label: &str, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn bind(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniforms.as_entire_binding(),
        }],
    })
}

impl Frame<'_> {
    /// Draws a material over the whole frame. Stage 3's only draw call.
    ///
    /// See [`MaterialPreview`] for why it is temporary.
    pub fn draw_material(
        &mut self,
        preview: &MaterialPreview,
        material: &Material,
        pipeline: &wgpu::RenderPipeline,
    ) {
        let mut pass = self.begin_color_pass("material preview", wgpu::LoadOp::Load);
        preview.record(&mut pass, material, pipeline);
    }
}

/// End-to-end checks: a `.vmt` and a `.vtf`, through the material system, onto
/// the GPU, through real WGSL, and back to the CPU.
///
/// These are the only tests in `src/materials/` that touch a GPU, and they earn
/// it. Everything between `Vmt::from_keyvalues` and a pixel — the bind group
/// layouts matching the WGSL, the matrix convention, the winding, the texture
/// transform, the alpha test, the modulation — is invisible to a unit test and
/// produces a *plausible* wrong picture rather than a crash.
///
/// They **skip** rather than fail where there is no usable adapter, so a
/// machine with no GPU (or no BC support) still gets a green `cargo test`. The
/// skip prints, so it cannot quietly become "these never ran anywhere".
#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::keyvalues;
    use crate::materials::image_format::{ColorSpace, ImageFormat};
    use crate::materials::material::{MaterialCache, TextureFallbacks};
    use crate::materials::pipeline::{PipelineCache, TargetFormat};
    use crate::materials::texture::{sampler_key, Texture, TextureCache};
    use crate::materials::uniforms::IDENTITY;
    use crate::materials::vmt::Vmt;
    use crate::materials::vtf::{TextureFlags, Vtf};
    use std::sync::Arc;

    /// Side of the offscreen target. 64 x 4 bytes is exactly `wgpu`'s 256-byte
    /// row alignment for `copy_texture_to_buffer`, so the readback needs no
    /// padding arithmetic to get wrong.
    const TARGET: u32 = 64;

    /// The target format. Unorm rather than sRGB so that the bytes that come
    /// back are the bytes the shader wrote: encoding is the swap chain's job,
    /// and mixing it in here would mean asserting on a curve instead of on a
    /// colour.
    const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

    /// A single-mip VTF 7.5 in memory. Version 7.5 has the resource
    /// dictionary, so this also exercises the path shipped content takes.
    fn vtf_bytes(
        format: ImageFormat,
        width: u16,
        height: u16,
        flags: u32,
        image: &[u8],
    ) -> Vec<u8> {
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
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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

    /// Uploads a `.vtf`'s bytes as a texture, the way the cache would.
    fn texture(device: &wgpu::Device, queue: &wgpu::Queue, vtf: &Vtf) -> Arc<Texture> {
        let sampler = device.create_sampler(&sampler_key(vtf.flags, vtf.mip_count).descriptor());
        Arc::new(
            Texture::from_vtf(device, queue, "test", vtf, 0, ColorSpace::Linear, sampler)
                .expect("uploadable"),
        )
    }

    /// Builds a material from `.vmt` text, with `$basetexture` resolving to
    /// `base` whatever it is called.
    fn material(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &PipelineCache,
        body: &str,
        base: Arc<Texture>,
    ) -> Material {
        let text = format!("\"UnlitGeneric\" {{ \"$basetexture\" \"test\" {body} }}");
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        let vmt = Vmt::from_keyvalues("test.vmt", &document).expect("a shader block");
        // The fallbacks are `base` too: this material names its base texture,
        // so neither is reachable, and a test that quietly drew one instead
        // would be worth failing.
        let fallback = TextureFallbacks {
            white: Arc::clone(&base),
            error: Arc::clone(&base),
        };
        Material::new(
            device,
            queue,
            pipelines.layouts(),
            "test",
            &vmt,
            &fallback,
            |_, _| Arc::clone(&base),
        )
        .expect("UnlitGeneric is ported")
    }

    /// Draws one material over a `TARGET`-square image and reads it back.
    ///
    /// The frame is cleared to transparent black first, so anything the
    /// material does not cover reads as zero — which is how the culling and
    /// discard tests tell "not drawn" from "drawn dark".
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &mut PipelineCache,
        material: &Material,
        draw: &DrawUniforms,
    ) -> Vec<u8> {
        let preview = MaterialPreview::new(device, pipelines.layouts());
        preview.update(queue, (TARGET, TARGET), draw);
        let pipeline =
            pipelines.get(&material.pipeline_key(TargetFormat::color_only(TARGET_FORMAT)));

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
            format: TARGET_FORMAT,
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
                label: Some("preview test"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            });
            preview.record(&mut pass, material, &pipeline);
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

    /// A 2x2 BGRA texture, one distinct colour per texel, written top row
    /// first — the order a `.vtf` stores rows in.
    ///
    ///   red   green
    ///   blue  white
    ///
    /// Point-sampled and clamped so each output pixel is exactly one source
    /// texel: with linear filtering a channel-order or flip mistake could hide
    /// inside an interpolated edge.
    fn four_corner_vtf() -> Vtf {
        #[rustfmt::skip]
        let image: Vec<u8> = vec![
            0, 0, 255, 255,   0, 255, 0, 255,
            255, 0, 0, 255,   255, 255, 255, 255,
        ];
        let flags =
            TextureFlags::POINT_SAMPLE.0 | TextureFlags::CLAMP_S.0 | TextureFlags::CLAMP_T.0;
        Vtf::parse(vtf_bytes(ImageFormat::Bgra8888, 2, 2, flags, &image)).expect("valid vtf")
    }

    #[test]
    fn a_material_draws_its_base_texture_the_right_way_up() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pipelines = PipelineCache::new(&device);
        let base = texture(&device, &queue, &four_corner_vtf());
        // `$gammacolorread` so the texture is loaded linear and the bytes that
        // come back are the bytes that went in.
        let material = material(
            &device,
            &queue,
            &pipelines,
            r#""$gammacolorread" "1""#,
            base,
        );

        let out = render(
            &device,
            &queue,
            &mut pipelines,
            &material,
            &DrawUniforms::identity(),
        );

        // Top-left of the *image* must be top-left on screen. Three
        // conventions compose to decide this — the view-projection's y flip,
        // the quad's texture coordinates, and WebGPU's framebuffer origin — and
        // getting any one of them wrong renders a plausible upside-down or
        // mirrored picture.
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
        let mut pipelines = PipelineCache::new(&device);

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
        let base = texture(&device, &queue, &vtf);
        let material = material(
            &device,
            &queue,
            &pipelines,
            r#""$gammacolorread" "1""#,
            base,
        );

        let out = render(
            &device,
            &queue,
            &mut pipelines,
            &material,
            &DrawUniforms::identity(),
        );
        // 565 cannot hold 255 exactly in green's neighbours, but red and blue
        // are all-ones and must come back saturated.
        let centre = pixel(&out, TARGET / 2, TARGET / 2);
        assert_eq!(centre[0], 255, "red channel of a solid magenta block");
        assert_eq!(centre[1], 0, "green channel");
        assert_eq!(centre[2], 255, "blue channel");
        assert_eq!(centre[3], 255, "DXT1 without alpha is opaque");
    }

    #[test]
    fn the_error_material_draws_the_checkerboard() {
        let Some((device, queue)) = device() else {
            return;
        };
        // Through the real cache, so this is the object a failed `load`
        // returns rather than something assembled for the test.
        let mut materials = MaterialCache::new(&device, &queue);
        let error = materials.error_material();

        let out = render(
            &device,
            &queue,
            materials.pipelines(),
            &error,
            &DrawUniforms::identity(),
        );

        // `CCheckerboardTexture` (`texturemanager.cpp:96`) alternates
        // (255,0,255,255) and (0,0,0,128) on `(x & 4) ^ (y & 4)`, 32x32, drawn
        // here at 2x into the 64x64 target.
        //
        // The sample points are the *interiors* of the 4-texel cells, because
        // the error texture's sampler is bilinear — it has no
        // `TEXTUREFLAGS_POINTSAMPLE`, so `sampler_key` gives it linear
        // filtering, exactly as the original does. Anything within two output
        // pixels of a cell edge reads as a blend of the two colours rather than
        // as either.
        //
        // The magenta survives the sRGB round trip exactly because 0 and 1 are
        // the two fixed points of the transfer function: the texture is an
        // sRGB format, the sampler decodes, the shader writes linear, and the
        // Unorm target stores it unchanged.
        for (x, y, expected) in [
            (2, 2, [255, 0, 255, 255]),
            (6, 6, [255, 0, 255, 255]),
            (10, 2, [0, 0, 0, 128]),
            (2, 10, [0, 0, 0, 128]),
            (10, 10, [255, 0, 255, 255]),
        ] {
            assert_eq!(pixel(&out, x, y), expected, "checkerboard at {x},{y}");
        }
    }

    #[test]
    fn the_model_matrix_places_the_quad() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pipelines = PipelineCache::new(&device);
        let base = texture(&device, &queue, &four_corner_vtf());
        let material = material(
            &device,
            &queue,
            &pipelines,
            r#""$gammacolorread" "1""#,
            base,
        );

        // Half scale: the unit square becomes the top-left quarter of the
        // viewport. Column-major, so the scale is on the diagonal and would
        // still look right transposed — which is why the *view* projection
        // carries a translation and this one does not have to.
        let mut model = IDENTITY;
        model[0][0] = 0.5;
        model[1][1] = 0.5;
        let draw = DrawUniforms {
            model,
            modulation: [1.0, 1.0, 1.0, 1.0],
        };

        let out = render(&device, &queue, &mut pipelines, &material, &draw);

        assert_eq!(pixel(&out, 1, 1), [255, 0, 0, 255], "still red at top-left");
        assert_eq!(
            pixel(&out, TARGET / 2 - 1, TARGET / 2 - 1),
            [255, 255, 255, 255],
            "the quad's bottom-right corner is now the middle of the screen"
        );
        assert_eq!(
            pixel(&out, TARGET / 2 + 2, TARGET / 2 + 2),
            [0, 0, 0, 0],
            "and the rest of the frame is untouched"
        );
    }

    #[test]
    fn colour_modulation_multiplies_the_texture() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pipelines = PipelineCache::new(&device);
        let base = texture(&device, &queue, &four_corner_vtf());
        // `$color` and `$color2` both apply, and they multiply.
        let material = material(
            &device,
            &queue,
            &pipelines,
            r#""$gammacolorread" "1" "$color" "[0.5 1 1]" "$color2" "[1 0.5 1]""#,
            base,
        );
        assert_eq!(material.modulation, [0.5, 0.5, 1.0, 1.0]);

        let draw = DrawUniforms {
            model: IDENTITY,
            modulation: material.modulation,
        };
        let out = render(&device, &queue, &mut pipelines, &material, &draw);

        // Bottom-right is white, so it reads the modulation directly. 0.5 of
        // 255 lands on 127 or 128 depending on the rounding the hardware does.
        let white = pixel(&out, TARGET - 1, TARGET - 1);
        assert!((127..=128).contains(&white[0]), "red halved: {white:?}");
        assert!((127..=128).contains(&white[1]), "green halved: {white:?}");
        assert_eq!(white[2], 255, "blue untouched: {white:?}");
        assert_eq!(white[3], 255, "alpha is not modulated by $color");
    }

    #[test]
    fn an_alpha_tested_material_discards_below_its_reference() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pipelines = PipelineCache::new(&device);

        // Two texels: opaque white on the left, quarter-alpha white on the
        // right. With a reference of 0.5 only the left survives.
        #[rustfmt::skip]
        let image: Vec<u8> = vec![
            255, 255, 255, 255,   255, 255, 255, 64,
            255, 255, 255, 255,   255, 255, 255, 64,
        ];
        let flags =
            TextureFlags::POINT_SAMPLE.0 | TextureFlags::CLAMP_S.0 | TextureFlags::CLAMP_T.0;
        let vtf = Vtf::parse(vtf_bytes(ImageFormat::Bgra8888, 2, 2, flags, &image)).unwrap();
        let base = texture(&device, &queue, &vtf);

        let material = material(
            &device,
            &queue,
            &pipelines,
            r#""$gammacolorread" "1" "$alphatest" "1" "$alphatestreference" "0.5""#,
            base,
        );
        let out = render(
            &device,
            &queue,
            &mut pipelines,
            &material,
            &DrawUniforms::identity(),
        );

        assert_eq!(
            pixel(&out, 4, 4),
            [255, 255, 255, 0],
            "the opaque half draws — with alpha masked off, because an \
             alpha-tested material is not fully opaque"
        );
        assert_eq!(
            pixel(&out, TARGET - 4, 4),
            [0, 0, 0, 0],
            "the transparent half is discarded, not blended"
        );
    }

    #[test]
    fn a_material_with_no_base_texture_draws_white_not_the_checkerboard() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pipelines = PipelineCache::new(&device);
        // The real standard textures, from the real cache.
        let textures = TextureCache::new(&device, &queue);

        // Valve's own `___flat.vmt`: an `UnlitGeneric` with no `$basetexture`
        // at all, whose colour is entirely `$color` and the vertex stream.
        let text = r#""UnlitGeneric" { "$color" "[1 0 0]" "$gammacolorread" "1" }"#;
        let document = keyvalues::parse("flat.vmt", text).unwrap();
        let vmt = Vmt::from_keyvalues("flat.vmt", &document).unwrap();
        let fallback = TextureFallbacks {
            white: textures.white_texture(),
            error: textures.error_texture(),
        };
        let flat = Material::new(
            &device,
            &queue,
            pipelines.layouts(),
            "flat",
            &vmt,
            &fallback,
            |_, _| unreachable!("nothing to resolve: the material names no texture"),
        )
        .expect("UnlitGeneric is ported");

        let draw = DrawUniforms {
            model: IDENTITY,
            modulation: flat.modulation,
        };
        let out = render(&device, &queue, &mut pipelines, &flat, &draw);

        // White texture times red modulation is red — and emphatically not
        // magenta, which is what binding the error texture here would give.
        assert_eq!(pixel(&out, TARGET / 2, TARGET / 2), [255, 0, 0, 255]);
    }

    #[test]
    fn materials_with_the_same_state_share_one_pipeline() {
        let Some((device, queue)) = device() else {
            return;
        };
        let mut pipelines = PipelineCache::new(&device);
        let base = texture(&device, &queue, &four_corner_vtf());
        let target = TargetFormat::color_only(TARGET_FORMAT);

        // Two materials that differ in everything except pipeline state.
        let opaque = material(&device, &queue, &pipelines, "", Arc::clone(&base));
        let also_opaque = material(
            &device,
            &queue,
            &pipelines,
            r#""$color" "[1 0 0]""#,
            Arc::clone(&base),
        );
        // And one that differs in state.
        let translucent = material(&device, &queue, &pipelines, r#""$alpha" "0.5""#, base);

        pipelines.get(&opaque.pipeline_key(target));
        pipelines.get(&also_opaque.pipeline_key(target));
        assert_eq!(pipelines.len(), 1, "colour is not pipeline state");

        pipelines.get(&translucent.pipeline_key(target));
        assert_eq!(pipelines.len(), 2, "blending is");
    }
}
