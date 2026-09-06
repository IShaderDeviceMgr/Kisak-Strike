//! Drawing one material on a cube, and the GPU tests that pin the path down.
//!
//! **This is stage 4's verification path**, and it replaces stage 3's, which
//! replaced stage 2's. `portdocs/MATERIALSYSTEM.md` §9 makes the deliverable of
//! the mesh-and-context stage typed vertex buffers, static and dynamic
//! geometry, and a depth buffer that actually resolves occlusion — and none of
//! those is a thing a unit test can see. A full-screen quad could not tell a
//! working depth buffer from an absent one; two overlapping cubes can.
//!
//! What changed from stage 3's version, and why it is no longer the same kind
//! of file: it owned the vertex and index buffers, both uniform blocks, the
//! bind groups and the choice of where the quad went. All of that is now
//! [`RenderContext`](super::context::RenderContext)'s, which is what
//! stage 3's own docs said would happen to it. What is left is a cube, a
//! camera, and a clock — a *scene*, which is the engine's job and goes away
//! when there is a map to load instead.
//!
//! **Do not grow this.** When map loading lands, delete `preview.rs` and the
//! `-vmt` switch, and move the tests at the bottom onto whatever draws the
//! world — they are the only place the whole path is checked against real
//! pixels.

use glam::{Mat4, Vec3};

use super::context::{Camera, Load, Pass, RenderContext};
use super::material::Material;
use super::mesh::{IndexBuffer, ModelVertex, SimpleVertex, VertexBuffer, VertexLayout};
use super::uniforms::{Light, ModelLighting, AMBIENT_CUBE_FACES, MAX_LIGHTS};

// The non-deprecated spelling of `Mat4::look_at_rh`. Right-handed, Y-up.
use glam::camera::rh::view::look_at_mat4 as look_at;

/// How far the camera sits from the origin, in world units.
const EYE_DISTANCE: f32 = 3.2;

/// Horizontal field of view. `CViewSetup::fov`'s default is 90.
const FOV_X: f32 = 75.0;

/// Where the two cubes sit. Deliberately overlapping along the view axis, so
/// that the depth buffer is what decides which one is visible where they cross
/// — with depth testing off, the second one drawn simply paints over the first,
/// which is a visibly different and obviously wrong picture.
const CUBE_OFFSETS: [Vec3; 2] = [Vec3::new(-0.45, 0.0, 0.45), Vec3::new(0.45, 0.0, -0.45)];

/// The geometry and the camera a preview draw needs.
///
/// One per renderer. Holds no uniform buffers, no bind groups and no pipeline:
/// those belong to the render context and the pipeline cache, and the fact that
/// this file no longer mentions them is the measure of stage 4.
pub struct MaterialPreview {
    vertices: VertexBuffer,
    /// The same cube in [`VertexLayout::Model`], for the shaders that read one.
    ///
    /// Two buffers rather than one, because a draw's vertices must be the
    /// layout its material's shader declared and `Pass::draw` panics otherwise.
    /// Which one a `-vmt` uses is decided by the `.vmt`, so the preview cannot
    /// pick at construction time and builds both — 24 vertices each.
    model_vertices: VertexBuffer,
    indices: IndexBuffer,
    /// The ground quad's indices, uploaded once even though its vertices are
    /// rebuilt every frame — see [`MaterialPreview::draw`].
    ground_indices: IndexBuffer,
}

impl MaterialPreview {
    pub fn new(device: &wgpu::Device) -> MaterialPreview {
        let (vertices, indices) = cube();
        let model_vertices = model_cube();
        MaterialPreview {
            vertices: VertexBuffer::new(device, "preview cube", &vertices),
            model_vertices: VertexBuffer::new(device, "preview model cube", &model_vertices),
            indices: IndexBuffer::new(device, "preview cube", &indices),
            ground_indices: IndexBuffer::new(device, "preview ground", &QUAD_INDICES),
        }
    }

    /// The camera the scene is viewed through, for a target of `size`.
    ///
    /// Orbits so that a still image cannot hide a wrong transform: a cube seen
    /// straight down an axis looks like a square whichever way its matrix is
    /// transposed.
    pub fn camera(&self, size: (u32, u32), seconds: f32) -> Camera {
        let angle = seconds * 0.6;
        let eye = Vec3::new(
            angle.sin() * EYE_DISTANCE,
            EYE_DISTANCE * 0.45,
            angle.cos() * EYE_DISTANCE,
        );
        let aspect = size.0.max(1) as f32 / size.1.max(1) as f32;
        Camera::perspective(
            eye,
            look_at(eye, Vec3::ZERO, Vec3::Y),
            FOV_X,
            aspect,
            0.1,
            100.0,
        )
    }

    /// Records the scene: two cubes from static buffers, a ground quad from the
    /// frame's dynamic ones.
    ///
    /// The ground quad is dynamic on purpose rather than because it changes:
    /// the dynamic vertex path is what every immediate-mode draw in the engine
    /// uses and what stage 4 added, and a path that only the tests exercise is
    /// a path that rots. Its indices are static because the world's are too —
    /// static vertices with dynamic indices, and dynamic vertices with static
    /// indices, are both shapes the API has to allow (see
    /// [`mesh`](super::mesh)).
    pub fn draw(&self, pass: &mut Pass<'_>, material: &Material) {
        let model_layout = material.shader.vertex_layout() == VertexLayout::Model;
        if model_layout {
            // A model shader reads group 3, and nothing here is a real
            // lighting environment — so the preview supplies one that makes
            // the shading visible and the cube's *ordering* checkable. See
            // `preview_lighting`.
            pass.set_model_lighting(&preview_lighting());
        }
        let vertices = if model_layout {
            self.model_vertices.slice()
        } else {
            self.vertices.slice()
        };

        for offset in CUBE_OFFSETS {
            pass.draw(
                material,
                &vertices,
                &self.indices.slice(),
                Mat4::from_translation(offset),
            );
        }

        const Y: f32 = -0.9;
        const R: f32 = 2.5;
        // Wound so the face normal is +y and the camera above it sees the
        // front. The corners run `+z` first, which looks backwards written
        // down and is not: `u × v` for `u = +x`, `v = -z` is `+y`. With them
        // the other way round the ground is back-face culled and simply
        // absent — which is what happened first time round, and is why
        // `the_preview_scene_draws_its_ground` exists.
        const CORNERS: [([f32; 3], [f32; 2]); 4] = [
            ([-R, Y, R], [0.0, 4.0]),
            ([R, Y, R], [4.0, 4.0]),
            ([R, Y, -R], [4.0, 0.0]),
            ([-R, Y, -R], [0.0, 0.0]),
        ];
        let ground = if model_layout {
            let vertices: Vec<ModelVertex> = CORNERS
                .iter()
                .map(|&(position, texcoord)| {
                    let mut vertex = ModelVertex::new(position, [0.0, 1.0, 0.0], texcoord);
                    // +x is the texture's u; the binormal sign is -1 for the
                    // same reason it is on the cube's faces.
                    vertex.tangent = [1.0, 0.0, 0.0, -1.0];
                    vertex
                })
                .collect();
            pass.vertices(&vertices)
        } else {
            let vertices: Vec<SimpleVertex> = CORNERS
                .iter()
                .map(|&(position, texcoord)| SimpleVertex::new(position, texcoord))
                .collect();
            pass.vertices(&vertices)
        };
        pass.draw(
            material,
            &ground,
            &self.ground_indices.slice(),
            Mat4::IDENTITY,
        );
    }

    /// The cube's buffers, for the winding test.
    #[cfg(test)]
    fn cube_slices(&self) -> (super::mesh::VertexSlice, super::mesh::IndexSlice) {
        (self.vertices.slice(), self.indices.slice())
    }
}

/// Two triangles over four corners, wound counter-clockwise as seen from `+n`.
const QUAD_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

/// For each cube face: the outward normal, and two in-plane axes chosen so that
/// `u cross v == n`. That is exactly the condition that makes
/// `c0 -> c1 -> c2 -> c3` run counter-clockwise seen from `+n`.
///
/// Shared by [`cube`] and [`model_cube`], which is the whole reason it is not
/// a local: a model vertex needs the normal and the tangent that these axes
/// *are*, and re-deriving them from the positions would be a second chance to
/// get the winding wrong.
const CUBE_FACES: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
    ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
    ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
    ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
];

/// A unit cube centred on the origin, one quad per face.
///
/// Twenty-four vertices rather than eight, because each face needs the whole
/// texture: a corner shared between three faces cannot hold three different
/// texture coordinates. That is the same reason `.mdl` data is far larger than
/// its vertex positions suggest.
///
/// Built rather than written out, so that the winding is right by construction.
/// `FrontFace::Ccw` with back-face culling means each face's corners must run
/// counter-clockwise *as seen from outside*, and getting one face backwards
/// does not draw it mirrored — it draws a hole, silently.
fn cube() -> (Vec<SimpleVertex>, Vec<u16>) {
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, u, v) in CUBE_FACES {
        let base = vertices.len() as u16;
        // (-u, -v), (+u, -v), (+u, +v), (-u, +v), each at half extent from the
        // face's centre, which is the normal scaled to the half extent too.
        for (su, sv, texcoord) in [
            (-0.5, -0.5, [0.0, 1.0]),
            (0.5, -0.5, [1.0, 1.0]),
            (0.5, 0.5, [1.0, 0.0]),
            (-0.5, 0.5, [0.0, 0.0]),
        ] {
            let position = std::array::from_fn(|i| normal[i] * 0.5 + u[i] * su + v[i] * sv);
            vertices.push(SimpleVertex::new(position, texcoord));
        }
        indices.extend(QUAD_INDICES.iter().map(|i| base + i));
    }
    (vertices, indices)
}

/// The same cube as [`cube`], in [`VertexLayout::Model`].
///
/// Shares that function's face table, and gains the two attributes a model
/// vertex has and a simple one does not: the face normal, and a tangent frame.
///
/// **The binormal sign is -1, and it is not arbitrary.** `cube`'s texture
/// coordinates run `u` along `+u` and `v` along `-v` (the corner table's `v`
/// goes from 1 to 0 as `sv` goes from -0.5 to 0.5). Valve's shader builds the
/// binormal as `cross( normal, tangent ) * tangent.w`, and `cross( n, u )` is
/// `+v` by the table's own `u × v == n` invariant — so reaching the texture's
/// increasing-`v` direction needs the sign flipped. With `+1` a normal map
/// previews lit from the wrong side along one axis only, which is exactly the
/// kind of half-wrong that survives a glance.
fn model_cube() -> Vec<ModelVertex> {
    let (simple, _) = cube();
    let mut vertices = Vec::with_capacity(simple.len());
    for (face, chunk) in simple.chunks(4).enumerate() {
        let (normal, u, _) = CUBE_FACES[face];
        for vertex in chunk {
            let mut model = ModelVertex::new(vertex.position, normal, vertex.texcoord);
            model.tangent = [u[0], u[1], u[2], -1.0];
            vertices.push(model);
        }
    }
    vertices
}

/// The lighting a `-vmt` preview of a model material is drawn under.
///
/// There is no lighting environment here to be faithful to — `-vmt` is a
/// verification path, not a scene — so this is chosen to make the two things
/// that are easy to get wrong *visible*:
///
///   - **The ambient cube's axis order.** The six entries are deliberately
///     unequal and unequal per axis, so a swapped pair shows as a cube face
///     with the wrong tint rather than as nothing. Up (`+y` in the preview's
///     Y-up world, which is index 2) is the brightest, as a sky is.
///   - **The local light path.** One white point light, offset so that its
///     falloff is visible across the ground quad and the two cubes shade
///     differently from each other.
///
/// `static_light` is off: a preview cube has no `vrad` bake, and its colour
/// stream is [`ModelVertex::new`]'s black. Leaving it on would be dishonest
/// in the other direction — the model would be lit by a stream that means
/// nothing.
fn preview_lighting() -> ModelLighting {
    let mut lighting = ModelLighting::fullbright();
    // +x, -x, +y, -y, +z, -z.
    let cube: [[f32; 4]; AMBIENT_CUBE_FACES] = [
        [0.16, 0.15, 0.14, 0.0],
        [0.10, 0.11, 0.14, 0.0],
        [0.34, 0.36, 0.40, 0.0],
        [0.05, 0.05, 0.06, 0.0],
        [0.14, 0.15, 0.16, 0.0],
        [0.11, 0.10, 0.10, 0.0],
    ];
    lighting.ambient_cube = cube;
    lighting.ambient_light = 1;
    lighting.static_light = 0;
    lighting.lights = [Light::NONE; MAX_LIGHTS];
    // Constant 0, linear 0, quadratic 1/r² at about 4 units: bright at the
    // cubes, dim at the edge of the ground quad.
    lighting.lights[0] = Light::point([1.0, 0.95, 0.9], [1.6, 2.4, 1.6], [0.0, 0.0, 0.06]);
    lighting.count = 1;
    lighting
}

impl RenderContext {
    /// Draws the preview scene over a frame. Stage 4's only scene.
    ///
    /// Bundled here rather than in `context.rs` for the same reason
    /// `Frame::draw_material` was in stage 3's version of this file: it is the
    /// verification path, it is deleted with the rest of the file, and the
    /// render context should not know that a preview exists.
    pub fn draw_preview(
        &mut self,
        frame: &mut super::renderer::Frame<'_>,
        pipelines: &mut super::pipeline::PipelineCache,
        preview: &MaterialPreview,
        material: &Material,
        seconds: f32,
    ) {
        let camera = preview.camera(frame.size(), seconds);
        let mut pass = self.pass(
            frame,
            pipelines,
            &camera,
            Load::Clear(super::renderer::CLEAR_COLOR),
        );
        preview.draw(&mut pass, material);
    }
}

/// End-to-end checks: a `.vmt` and a `.vtf`, through the material system, onto
/// the GPU, through real WGSL, and back to the CPU.
///
/// These are the only tests in `src/materials/` that touch a GPU, and they earn
/// it. Everything between `Vmt::from_keyvalues` and a pixel — the bind group
/// layouts matching the WGSL, the matrix convention, the winding, the texture
/// transform, the alpha test, the modulation, the depth test, the per-draw
/// uniform offsets — is invisible to a unit test and produces a *plausible*
/// wrong picture rather than a crash.
///
/// They **skip** rather than fail where there is no usable adapter, so a
/// machine with no GPU (or no BC support) still gets a green `cargo test`. The
/// skip prints, so it cannot quietly become "these never ran anywhere".
#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::keyvalues;
    use crate::materials::context::{Load, RenderContext, StateOverride};
    use crate::materials::image_format::{ColorSpace, ImageFormat};
    use crate::materials::material::{MaterialCache, TextureFallbacks};
    use crate::materials::pipeline::PipelineCache;
    use crate::materials::shader::TextureDimension;
    use crate::materials::target::RenderTarget;
    use crate::materials::texture::{sampler_key, Texture, TextureCache};
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
        shader_material(device, queue, pipelines, "UnlitGeneric", body, base)
    }

    /// [`material`] for a shader other than `UnlitGeneric`.
    ///
    /// The cube fallback is the real black one rather than `base`, because a
    /// `VertexLitGeneric` bind group has a cube entry that no test material
    /// names and a 2D texture cannot fill.
    fn shader_material(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &PipelineCache,
        shader: &str,
        body: &str,
        base: Arc<Texture>,
    ) -> Material {
        let text = format!("\"{shader}\" {{ \"$basetexture\" \"test\" {body} }}");
        let document = keyvalues::parse("test.vmt", &text).expect("valid keyvalues");
        let vmt = Vmt::from_keyvalues("test.vmt", &document).expect("a shader block");
        // The fallbacks are `base` too: this material names its base texture,
        // so neither is reachable, and a test that quietly drew one instead
        // would be worth failing.
        let fallback = TextureFallbacks {
            white: Arc::clone(&base),
            error: Arc::clone(&base),
            black_cube: black_cube(device, queue),
        };
        Material::new(
            device,
            queue,
            pipelines.layouts(),
            "test",
            &vmt,
            &fallback,
            |_, _, dimension| match dimension {
                TextureDimension::Cube => black_cube(device, queue),
                TextureDimension::D2 => Arc::clone(&base),
            },
        )
        .unwrap_or_else(|| panic!("{shader} is ported"))
    }

    /// The 1x1x6 black cubemap, for the fallback sets the tests build by hand.
    fn black_cube(device: &wgpu::Device, queue: &wgpu::Queue) -> Arc<Texture> {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        Arc::new(Texture::black_cube(device, queue, sampler))
    }

    /// A flat-coloured 1x1 BGRA8 `.vtf`, which is the simplest thing a
    /// material can point at.
    fn flat_texture(device: &wgpu::Device, queue: &wgpu::Queue, rgba: [u8; 4]) -> Arc<Texture> {
        // Point-sampled and clamped, so that a 1x1 texture reads back as
        // exactly its own colour rather than as something filtered against the
        // edge.
        let flags =
            TextureFlags::POINT_SAMPLE.0 | TextureFlags::CLAMP_S.0 | TextureFlags::CLAMP_T.0;
        let vtf = Vtf::parse(vtf_bytes(
            ImageFormat::Bgra8888,
            1,
            1,
            flags,
            &[rgba[2], rgba[1], rgba[0], rgba[3]],
        ))
        .expect("a valid 7.5 file");
        texture(device, queue, &vtf)
    }

    /// A 2x2 texture with a different colour in each corner: blue top-left,
    /// green top-right, red bottom-left, white bottom-right (BGRA on disk).
    ///
    /// Point-sampled and clamped, so each output pixel reads exactly one texel
    /// and orientation questions have unambiguous answers.
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

    /// A screen-space quad covering `rect` in `0..1` coordinates, at depth `z`.
    ///
    /// Wound for [`Camera::screen`], whose projection flips `y` and therefore
    /// reverses the winding relative to the corner order — which is why this is
    /// a helper and not four literals repeated per test.
    fn quad(rect: [f32; 4], z: f32) -> ([SimpleVertex; 4], [u16; 6]) {
        let [x0, y0, x1, y1] = rect;
        (
            [
                SimpleVertex::new([x0, y0, z], [0.0, 0.0]),
                SimpleVertex::new([x1, y0, z], [1.0, 0.0]),
                SimpleVertex::new([x1, y1, z], [1.0, 1.0]),
                SimpleVertex::new([x0, y1, z], [0.0, 1.0]),
            ],
            [0, 2, 1, 0, 3, 2],
        )
    }

    /// A GPU, a render context, a pipeline cache and an offscreen target: the
    /// fixture every test below starts from.
    struct Harness {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipelines: PipelineCache,
        context: RenderContext,
        target: RenderTarget,
    }

    impl Harness {
        /// `None` when this machine has no usable adapter, which the caller
        /// turns into a skip.
        fn new(depth: bool) -> Option<Harness> {
            let (device, queue) = device()?;
            let pipelines = PipelineCache::new(&device);
            let context = RenderContext::new(&device, &queue, &pipelines);
            let target = RenderTarget::new(
                &device,
                "readback target",
                TARGET,
                TARGET,
                TARGET_FORMAT,
                depth,
            );
            Some(Harness {
                device,
                queue,
                pipelines,
                context,
                target,
            })
        }

        fn material(&self, body: &str, base: Arc<Texture>) -> Material {
            material(&self.device, &self.queue, &self.pipelines, body, base)
        }

        /// A `VertexLitGeneric` material. Its base texture is white unless the
        /// caller wants otherwise, so that a lighting test reads the lighting
        /// and nothing else.
        fn model_material(&self, body: &str) -> Material {
            shader_material(
                &self.device,
                &self.queue,
                &self.pipelines,
                "VertexLitGeneric",
                body,
                self.texture([255, 255, 255, 255]),
            )
        }

        fn texture(&self, rgba: [u8; 4]) -> Arc<Texture> {
            flat_texture(&self.device, &self.queue, rgba)
        }

        /// Runs `record` inside a pass against the offscreen target and reads
        /// the result back.
        ///
        /// The target is cleared to transparent black first, so anything not
        /// covered reads as zero — which is how the culling and discard tests
        /// tell "not drawn" from "drawn dark". There is no swap chain here, so
        /// the pass is opened directly rather than through
        /// [`RenderContext::pass`]; that difference is confined to this
        /// function.
        fn render(&mut self, record: impl FnOnce(&mut Pass<'_>)) -> Vec<u8> {
            self.render_with(&Camera::screen(), record)
        }

        /// [`render`](Harness::render) with a camera other than the screen one.
        fn render_with(&mut self, camera: &Camera, record: impl FnOnce(&mut Pass<'_>)) -> Vec<u8> {
            self.context.begin_frame();

            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size: (TARGET * TARGET * 4) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            let mut encoder = self.device.create_command_encoder(&Default::default());
            {
                let mut pass = self.context.offscreen_pass(
                    &mut encoder,
                    &mut self.pipelines,
                    &self.target,
                    camera,
                    Load::Clear(wgpu::Color::TRANSPARENT),
                );
                record(&mut pass);
            }
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: self.target.color_texture(),
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
            self.queue.submit([encoder.finish()]);

            readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
                r.expect("readback mapped");
            });
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the queue drained");
            let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
            readback.unmap();
            pixels
        }
    }

    /// The pixel at `(x, y)` of a `TARGET`-square readback.
    fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * TARGET + x) * 4) as usize;
        pixels[offset..offset + 4].try_into().expect("four bytes")
    }

    /// The pixel at the centre.
    fn centre(pixels: &[u8]) -> [u8; 4] {
        pixel(pixels, TARGET / 2, TARGET / 2)
    }

    macro_rules! harness {
        ($depth:expr) => {
            match Harness::new($depth) {
                Some(harness) => harness,
                None => return,
            }
        };
    }

    #[test]
    fn the_depth_buffer_decides_which_of_two_overlapping_quads_is_seen() {
        // The headline of stage 4. Two quads that overlap in the middle: the
        // near one is drawn *second*, so without a depth test the far one wins
        // on the left-hand overlap purely by draw order.
        let mut h = harness!(true);
        let far = h.material("", h.texture([255, 0, 0, 255]));
        let near = h.material("", h.texture([0, 0, 255, 255]));

        // `Camera::screen`'s clip range is -1..1 in z, so a smaller z is
        // nearer. The far quad covers the left two thirds, the near one the
        // right two thirds; they overlap in the middle third.
        let (far_v, far_i) = quad([0.0, 0.0, 0.66, 1.0], 0.5);
        let (near_v, near_i) = quad([0.33, 0.0, 1.0, 1.0], -0.5);

        let pixels = h.render(|pass| {
            let v = pass.vertices(&far_v);
            let i = pass.indices(&far_i);
            pass.draw(&far, &v, &i, Mat4::IDENTITY);

            let v = pass.vertices(&near_v);
            let i = pass.indices(&near_i);
            pass.draw(&near, &v, &i, Mat4::IDENTITY);
        });

        let y = TARGET / 2;
        assert_eq!(pixel(&pixels, 5, y), [255, 0, 0, 255], "only the far quad");
        assert_eq!(
            pixel(&pixels, TARGET - 5, y),
            [0, 0, 255, 255],
            "only the near quad"
        );
        assert_eq!(
            centre(&pixels),
            [0, 0, 255, 255],
            "in the overlap the nearer quad wins, whatever the draw order"
        );
    }

    #[test]
    fn without_a_depth_buffer_the_last_draw_wins() {
        // The control for the test above: the same two quads, the same order,
        // against a target with no depth attachment. If this produced the same
        // picture, that test would be proving nothing.
        let mut h = harness!(false);
        let far = h.material("", h.texture([255, 0, 0, 255]));
        let near = h.material("", h.texture([0, 0, 255, 255]));
        let (far_v, far_i) = quad([0.0, 0.0, 0.66, 1.0], 0.5);
        let (near_v, near_i) = quad([0.33, 0.0, 1.0, 1.0], -0.5);

        let pixels = h.render(|pass| {
            let v = pass.vertices(&far_v);
            let i = pass.indices(&far_i);
            pass.draw(&far, &v, &i, Mat4::IDENTITY);
            let v = pass.vertices(&near_v);
            let i = pass.indices(&near_i);
            pass.draw(&near, &v, &i, Mat4::IDENTITY);
        });

        assert_eq!(centre(&pixels), [0, 0, 255, 255], "the second draw");

        // And with the order reversed the *far* quad wins the overlap, which
        // is what "no depth test" means and what the test above rules out.
        let pixels = h.render(|pass| {
            let v = pass.vertices(&near_v);
            let i = pass.indices(&near_i);
            pass.draw(&near, &v, &i, Mat4::IDENTITY);
            let v = pass.vertices(&far_v);
            let i = pass.indices(&far_i);
            pass.draw(&far, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels), [255, 0, 0, 255]);
    }

    #[test]
    fn each_draw_gets_its_own_model_matrix() {
        // The per-draw uniform arena's whole reason for existing. Both draws
        // are recorded into one command buffer, and `Queue::write_buffer`
        // stages its copy ahead of that command buffer — so a single
        // `DrawUniforms` buffer rewritten between them would give *both* draws
        // the second matrix, and the first quad would land on top of the
        // second instead of on the left.
        let mut h = harness!(false);
        let left = h.material("", h.texture([255, 0, 0, 255]));
        let right = h.material("", h.texture([0, 0, 255, 255]));
        // One quad shape, placed twice by its model matrix alone.
        let (vertices, indices) = quad([0.0, 0.0, 0.5, 1.0], 0.0);

        let pixels = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&left, &v, &i, Mat4::IDENTITY);
            pass.draw(
                &right,
                &v,
                &i,
                Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0)),
            );
        });

        assert_eq!(
            pixel(&pixels, TARGET / 4, TARGET / 2),
            [255, 0, 0, 255],
            "the untranslated draw is still on the left"
        );
        assert_eq!(
            pixel(&pixels, TARGET * 3 / 4, TARGET / 2),
            [0, 0, 255, 255],
            "the translated one moved right"
        );
    }

    #[test]
    fn a_render_target_can_be_drawn_into_and_then_sampled() {
        // The replacement for `PushRenderTargetAndViewport`: draw the inner
        // view into a target, end that pass, then sample it in the next one.
        // `portdocs/MATERIALSYSTEM.md` §10 calls the render-target stack the
        // highest-risk unknown after the shaders; this is the shape that
        // answers it.
        let mut h = harness!(false);
        let inner = RenderTarget::new(&h.device, "inner", TARGET, TARGET, TARGET_FORMAT, false);
        let source = h.material("", h.texture([200, 100, 50, 255]));

        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        h.context.begin_frame();
        let mut encoder = h.device.create_command_encoder(&Default::default());
        {
            let mut pass = h.context.offscreen_pass(
                &mut encoder,
                &mut h.pipelines,
                &inner,
                &Camera::screen(),
                Load::Clear(wgpu::Color::TRANSPARENT),
            );
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&source, &v, &i, Mat4::IDENTITY);
        }
        h.queue.submit([encoder.finish()]);

        // Now a material whose base texture *is* that target, drawn into the
        // outer one.
        let sampling = h.material("", Arc::clone(inner.texture()));
        let pixels = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&sampling, &v, &i, Mat4::IDENTITY);
        });

        assert_eq!(centre(&pixels), [200, 100, 50, 255]);
    }

    #[test]
    fn a_state_override_turns_the_depth_test_off() {
        // `OverrideDepthEnable( true, false, false )` — the `$ignorez` path.
        // Same geometry as the depth test above, same depth buffer, but the
        // near quad's depth is ignored, so draw order decides again.
        let mut h = harness!(true);
        let far = h.material("", h.texture([255, 0, 0, 255]));
        let near = h.material("", h.texture([0, 0, 255, 255]));
        let (far_v, far_i) = quad([0.0, 0.0, 0.66, 1.0], -0.5);
        let (near_v, near_i) = quad([0.33, 0.0, 1.0, 1.0], 0.5);

        let pixels = h.render(|pass| {
            pass.set_state_override(StateOverride {
                depth_test: Some(false),
                depth_write: Some(false),
                ..Default::default()
            });
            // The first quad is the *nearer* one this time, so with the depth
            // test on the second would lose the overlap.
            let v = pass.vertices(&far_v);
            let i = pass.indices(&far_i);
            pass.draw(&far, &v, &i, Mat4::IDENTITY);
            let v = pass.vertices(&near_v);
            let i = pass.indices(&near_i);
            pass.draw(&near, &v, &i, Mat4::IDENTITY);
        });

        assert_eq!(
            centre(&pixels),
            [0, 0, 255, 255],
            "with depth ignored the later draw wins even though it is further"
        );
    }

    #[test]
    fn back_faces_are_culled_and_a_state_override_can_stop_it() {
        let mut h = harness!(false);
        let material = h.material("", h.texture([255, 255, 255, 255]));
        // The winding `quad` produces, reversed: a back face.
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);
        let reversed: [u16; 6] = [
            indices[2], indices[1], indices[0], indices[5], indices[4], indices[3],
        ];

        let culled = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&reversed);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&culled), [0, 0, 0, 0], "nothing was drawn");

        let kept = h.render(|pass| {
            pass.set_state_override(StateOverride {
                cull: Some(false),
                ..Default::default()
            });
            let v = pass.vertices(&vertices);
            let i = pass.indices(&reversed);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&kept), [255, 255, 255, 255]);
    }

    #[test]
    fn a_static_vertex_buffer_can_be_drawn_with_dynamic_indices() {
        // The pattern both the world renderer and `studiorender` use, and the
        // reason `mesh` keeps vertex and index buffers apart: vertices built
        // once, indices gathered per frame.
        let mut h = harness!(false);
        let material = h.material("", h.texture([90, 90, 220, 255]));
        // Six corners: a left quad and a right quad sharing a buffer.
        let vertices = [
            SimpleVertex::new([0.0, 0.0, 0.0], [0.0, 0.0]),
            SimpleVertex::new([0.5, 0.0, 0.0], [1.0, 0.0]),
            SimpleVertex::new([0.5, 1.0, 0.0], [1.0, 1.0]),
            SimpleVertex::new([0.0, 1.0, 0.0], [0.0, 1.0]),
            SimpleVertex::new([1.0, 0.0, 0.0], [1.0, 0.0]),
            SimpleVertex::new([1.0, 1.0, 0.0], [1.0, 1.0]),
        ];
        let statics = VertexBuffer::new(&h.device, "test", &vertices);

        // Only the left quad's indices.
        let left: [u16; 6] = [0, 2, 1, 0, 3, 2];
        let pixels = h.render(|pass| {
            let i = pass.indices(&left);
            pass.draw(&material, &statics.slice(), &i, Mat4::IDENTITY);
        });

        assert_eq!(pixel(&pixels, TARGET / 4, TARGET / 2), [90, 90, 220, 255]);
        assert_eq!(
            pixel(&pixels, TARGET * 3 / 4, TARGET / 2),
            [0, 0, 0, 0],
            "the right quad's indices were never submitted"
        );
    }

    #[test]
    fn an_odd_number_of_indices_draws() {
        // Three indices is one triangle and six bytes, which is *not* a
        // multiple of `COPY_BUFFER_ALIGNMENT`. `Queue::write_buffer` rejects a
        // copy that is not, so the dynamic arena pads — and the slice must
        // still report three, or the padding index gets drawn as part of a
        // second, garbage triangle.
        let mut h = harness!(false);
        let material = h.material("", h.texture([10, 200, 200, 255]));
        // The lower-left half of the target, wound for `Camera::screen`.
        let vertices = [
            SimpleVertex::new([0.0, 0.0, 0.0], [0.0, 0.0]),
            SimpleVertex::new([0.0, 1.0, 0.0], [0.0, 1.0]),
            SimpleVertex::new([1.0, 1.0, 0.0], [1.0, 1.0]),
        ];
        let indices: [u16; 3] = [0, 1, 2];

        let pixels = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            assert_eq!(i.len(), 3, "the padding must not reach the draw");
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });

        assert_eq!(
            pixel(&pixels, 4, TARGET - 4),
            [10, 200, 200, 255],
            "inside the triangle"
        );
        assert_eq!(
            pixel(&pixels, TARGET - 4, 4),
            [0, 0, 0, 0],
            "the other side of the hypotenuse is empty, not a second triangle"
        );
    }

    #[test]
    fn an_index_range_draws_only_its_batch() {
        // `IMesh::Draw( nFirstIndex, nIndexCount )`: one material's batch out
        // of a shared index buffer, which is how the world renderer submits a
        // sort group.
        let mut h = harness!(false);
        let material = h.material("", h.texture([250, 250, 40, 255]));
        let vertices = [
            SimpleVertex::new([0.0, 0.0, 0.0], [0.0, 0.0]),
            SimpleVertex::new([0.5, 0.0, 0.0], [1.0, 0.0]),
            SimpleVertex::new([0.5, 1.0, 0.0], [1.0, 1.0]),
            SimpleVertex::new([0.0, 1.0, 0.0], [0.0, 1.0]),
            SimpleVertex::new([1.0, 0.0, 0.0], [1.0, 0.0]),
            SimpleVertex::new([1.0, 1.0, 0.0], [1.0, 1.0]),
        ];
        let statics = VertexBuffer::new(&h.device, "test", &vertices);
        // Both quads, back to back: left is 0..6, right is 6..12.
        let all: [u16; 12] = [0, 2, 1, 0, 3, 2, 1, 5, 4, 1, 2, 5];
        let indices = IndexBuffer::new(&h.device, "test", &all);

        let pixels = h.render(|pass| {
            pass.draw(
                &material,
                &statics.slice(),
                &indices.range(6, 6),
                Mat4::IDENTITY,
            );
        });

        assert_eq!(
            pixel(&pixels, TARGET / 4, TARGET / 2),
            [0, 0, 0, 0],
            "the first six indices were skipped"
        );
        assert_eq!(
            pixel(&pixels, TARGET * 3 / 4, TARGET / 2),
            [250, 250, 40, 255]
        );
    }

    #[test]
    fn modulation_multiplies_the_material_by_the_instance() {
        // `IMesh::DrawModulated`. Half the material's colour, twice.
        let mut h = harness!(false);
        let material = h.material("", h.texture([200, 200, 200, 255]));
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let pixels = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw_modulated(&material, &v, &i, Mat4::IDENTITY, [0.5, 0.5, 0.5, 1.0]);
        });

        let [r, g, b, a] = centre(&pixels);
        assert_eq!(a, 255);
        for channel in [r, g, b] {
            assert!(
                (99..=101).contains(&channel),
                "half of 200 is 100, got {channel}"
            );
        }
    }

    #[test]
    fn identical_states_share_one_pipeline() {
        // §10 asks how many pipeline variants really survive the combo cull.
        // Two materials that differ only in their texture must not be two
        // pipelines, or the answer is "one per material" and the cache is
        // pointless.
        let mut h = harness!(false);
        let first = h.material("", h.texture([255, 0, 0, 255]));
        let second = h.material("", h.texture([0, 255, 0, 255]));
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&first, &v, &i, Mat4::IDENTITY);
            pass.draw(&second, &v, &i, Mat4::IDENTITY);
        });

        assert_eq!(h.pipelines.len(), 1);
    }

    #[test]
    fn a_material_draws_its_base_texture_the_right_way_up() {
        let mut h = harness!(false);
        let base = texture(&h.device, &h.queue, &four_corner_vtf());
        // `$gammacolorread` so the texture is loaded linear and the bytes that
        // come back are the bytes that went in.
        let material = h.material(r#""$gammacolorread" "1""#, base);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let out = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });

        // Top-left of the *image* must be top-left on screen. Three
        // conventions compose to decide this — the camera's `y` flip, the
        // quad's texture coordinates, and WebGPU's framebuffer origin — and
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
        let mut h = harness!(false);

        // One 4x4 DXT1 block of solid magenta: both endpoints the same colour
        // and every index 0, which is the degenerate encoding every DXT1
        // decoder agrees on.
        // RGB565: red and blue all-ones, green zero.
        let magenta = ((255u16 >> 3) << 11) | (255u16 >> 3);
        let mut block = Vec::new();
        block.extend_from_slice(&magenta.to_le_bytes());
        block.extend_from_slice(&magenta.to_le_bytes());
        block.extend_from_slice(&[0, 0, 0, 0]);

        let vtf = Vtf::parse(vtf_bytes(ImageFormat::Dxt1, 4, 4, 0, &block)).expect("valid vtf");
        assert_eq!(vtf.format, ImageFormat::Dxt1);
        let base = texture(&h.device, &h.queue, &vtf);
        let material = h.material(r#""$gammacolorread" "1""#, base);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let out = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });

        // 565 cannot hold 255 exactly in green's neighbours, but red and blue
        // are all-ones and must come back saturated.
        let centre = centre(&out);
        assert_eq!(centre[0], 255, "red channel of a solid magenta block");
        assert_eq!(centre[1], 0, "green channel");
        assert_eq!(centre[2], 255, "blue channel");
        assert_eq!(centre[3], 255, "DXT1 without alpha is opaque");
    }

    #[test]
    fn the_error_material_draws_the_checkerboard() {
        let mut h = harness!(false);
        // Through the real cache, so this is the object a failed `load`
        // returns rather than something assembled for the test.
        let materials = MaterialCache::new(&h.device, &h.queue);
        let error = materials.error_material();
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let out = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&error, &v, &i, Mat4::IDENTITY);
        });

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
    fn colour_modulation_multiplies_the_texture() {
        let mut h = harness!(false);
        let base = texture(&h.device, &h.queue, &four_corner_vtf());
        // `$color` and `$color2` both apply, and they multiply.
        let material = h.material(
            r#""$gammacolorread" "1" "$color" "[0.5 1 1]" "$color2" "[1 0.5 1]""#,
            base,
        );
        assert_eq!(material.modulation, [0.5, 0.5, 1.0, 1.0]);
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let out = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });

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
        let mut h = harness!(false);

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
        let base = texture(&h.device, &h.queue, &vtf);

        let material = h.material(
            r#""$gammacolorread" "1" "$alphatest" "1" "$alphatestreference" "0.5""#,
            base,
        );
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let out = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });

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
        let mut h = harness!(false);
        // The real standard textures, from the real cache.
        let textures = TextureCache::new(&h.device, &h.queue);

        // Valve's own `___flat.vmt`: an `UnlitGeneric` with no `$basetexture`
        // at all, whose colour is entirely `$color` and the vertex stream.
        let text = r#""UnlitGeneric" { "$color" "[1 0 0]" "$gammacolorread" "1" }"#;
        let document = keyvalues::parse("flat.vmt", text).unwrap();
        let vmt = Vmt::from_keyvalues("flat.vmt", &document).unwrap();
        let fallback = TextureFallbacks {
            white: textures.white_texture(),
            error: textures.error_texture(),
            black_cube: textures.black_cube_texture(),
        };
        let flat = Material::new(
            &h.device,
            &h.queue,
            h.pipelines.layouts(),
            "flat",
            &vmt,
            &fallback,
            |_, _, _| unreachable!("nothing to resolve: the material names no texture"),
        )
        .expect("UnlitGeneric is ported");
        let (vertices, indices) = quad([0.0, 0.0, 1.0, 1.0], 0.0);

        let out = h.render(|pass| {
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&flat, &v, &i, Mat4::IDENTITY);
        });

        // White texture times red modulation is red — and emphatically not
        // magenta, which is what binding the error texture here would give.
        assert_eq!(centre(&out), [255, 0, 0, 255]);
    }

    #[test]
    fn the_preview_scene_draws_its_ground() {
        // The cubes are checked by the winding test above; the ground quad is
        // the piece nothing else covers, and a quad wound the wrong way round
        // is not a wrong picture but an absent one. It was wound the wrong way
        // round first time, and only rendering the scene and looking at it
        // found that.
        let mut h = harness!(true);
        let material = h.material("", h.texture([230, 190, 120, 255]));
        let preview = MaterialPreview::new(&h.device);
        let camera = preview.camera((TARGET, TARGET), 1.0);

        let pixels = h.render_with(&camera, |pass| preview.draw(pass, &material));

        // The camera looks down at the origin from above, so the ground fills
        // the lower half of the frame well outside the cubes' silhouette.
        let bottom_row_covered = (0..TARGET)
            .filter(|x| pixel(&pixels, *x, TARGET - 1)[3] != 0)
            .count();
        assert!(
            bottom_row_covered > TARGET as usize / 2,
            "the ground quad is missing: {bottom_row_covered} of {TARGET} pixels \
             covered along the bottom edge"
        );

        // And the scene as a whole covers most of the frame, which it cannot
        // do from the cubes alone.
        let covered = (0..TARGET * TARGET)
            .filter(|i| pixels[(*i as usize) * 4 + 3] != 0)
            .count();
        assert!(
            covered > (TARGET * TARGET) as usize / 2,
            "only {covered} of {} pixels drawn",
            TARGET * TARGET
        );
    }

    #[test]
    fn the_cube_is_wound_so_that_every_face_survives_culling() {
        // A hole in a cube is invisible from most angles and obvious from one,
        // so it is exactly the kind of thing that ships. Look at each face
        // straight on and check something opaque is there.
        let mut h = harness!(true);
        let material = h.material("", h.texture([255, 255, 255, 255]));
        let preview = MaterialPreview::new(&h.device);
        let (vertices, indices) = preview.cube_slices();

        for (name, eye) in [
            ("+x", Vec3::X),
            ("-x", -Vec3::X),
            ("+y", Vec3::Y),
            ("-y", -Vec3::Y),
            ("+z", Vec3::Z),
            ("-z", -Vec3::Z),
        ] {
            let eye = eye * 3.0;
            // `look_at` needs an up vector that is not the view direction.
            let up = if eye.x == 0.0 && eye.z == 0.0 {
                Vec3::Z
            } else {
                Vec3::Y
            };
            let camera =
                Camera::perspective(eye, look_at(eye, Vec3::ZERO, up), FOV_X, 1.0, 0.1, 100.0);
            let pixels = h.render_with(&camera, |pass| {
                pass.draw(&material, &vertices, &indices, Mat4::IDENTITY)
            });
            assert_eq!(
                centre(&pixels),
                [255, 255, 255, 255],
                "the {name} face was culled away"
            );
        }
    }

    // ---------------------------------------------------------------------
    // VertexLitGeneric
    // ---------------------------------------------------------------------
    // These are the only place the model shader's WGSL is compiled at all, so
    // the first of them failing to *build a pipeline* is a syntax error and
    // not a maths error. Every one after that pins a number that produces a
    // plausible wrong picture rather than an error.

    /// A screen-space quad in [`VertexLayout::Model`], with a chosen normal and
    /// baked static-light colour.
    ///
    /// Same corners and same winding as [`quad`] — the difference is only the
    /// two attributes a model vertex has. The normal is a parameter because
    /// every lighting test below is a question about it.
    fn model_quad(normal: [f32; 3], static_light: [f32; 4]) -> ([ModelVertex; 4], [u16; 6]) {
        let corner = |x: f32, y: f32, u: f32, v: f32| {
            let mut vertex = ModelVertex::new([x, y, 0.0], normal, [u, v]);
            vertex.color = static_light;
            vertex
        };
        (
            [
                corner(0.0, 0.0, 0.0, 0.0),
                corner(1.0, 0.0, 1.0, 0.0),
                corner(1.0, 1.0, 1.0, 1.0),
                corner(0.0, 1.0, 0.0, 1.0),
            ],
            [0, 2, 1, 0, 3, 2],
        )
    }

    /// Lighting with everything off: no ambient cube, no baked light, no local
    /// lights. Whatever a test switches on is then the only source.
    fn dark_lighting() -> ModelLighting {
        ModelLighting {
            ambient_cube: [[0.0; 4]; AMBIENT_CUBE_FACES],
            lights: [Light::NONE; MAX_LIGHTS],
            count: 0,
            static_light: 0,
            ambient_light: 0,
            _padding: 0,
        }
    }

    #[test]
    fn the_ambient_cube_lights_each_axis_from_its_own_entry() {
        // Gotcha #1 of the whole block: the cube is stored `+x, -x, +y, -y,
        // +z, -z`, and a swapped pair lights every model in the game from the
        // wrong side while looking entirely plausible. Six draws, six normals,
        // six distinct answers.
        let mut h = harness!(false);
        let material = h.model_material("");

        let mut lighting = dark_lighting();
        lighting.ambient_light = 1;
        // Values chosen to land exactly on a byte: n/255 for n in 51..=204.
        let levels = [51u8, 68, 85, 119, 153, 204];
        for (face, level) in levels.iter().enumerate() {
            lighting.ambient_cube[face] = [*level as f32 / 255.0, 0.0, 0.0, 0.0];
        }

        let axes = [
            ("+x", [1.0, 0.0, 0.0]),
            ("-x", [-1.0, 0.0, 0.0]),
            ("+y", [0.0, 1.0, 0.0]),
            ("-y", [0.0, -1.0, 0.0]),
            ("+z", [0.0, 0.0, 1.0]),
            ("-z", [0.0, 0.0, -1.0]),
        ];
        for (face, (name, normal)) in axes.iter().enumerate() {
            let (vertices, indices) = model_quad(*normal, [0.0; 4]);
            let pixels = h.render(|pass| {
                pass.set_model_lighting(&lighting);
                let v = pass.vertices(&vertices);
                let i = pass.indices(&indices);
                pass.draw(&material, &v, &i, Mat4::IDENTITY);
            });
            assert_eq!(
                centre(&pixels)[0],
                levels[face],
                "a {name} normal read the wrong ambient cube entry"
            );
        }
    }

    #[test]
    fn the_ambient_cube_is_ignored_when_it_is_disabled() {
        // `m_bAmbientLight`. A cube that is present but not enabled must
        // contribute nothing, or a model with no lighting environment is lit
        // by whatever the previous instance left behind.
        let mut h = harness!(false);
        let material = h.model_material("");
        let mut lighting = dark_lighting();
        lighting.ambient_cube = [[1.0, 1.0, 1.0, 0.0]; AMBIENT_CUBE_FACES];
        lighting.ambient_light = 0;

        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.0; 4]);
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels)[0], 0);
    }

    #[test]
    fn baked_vertex_light_is_gamma_decoded_and_doubled() {
        // `GammaToLinear( staticLightingColor * cOverbright )`
        // (`common_vs_fxc.h:852`), and both halves matter: the stream is in
        // gamma space *and* pre-multiplied by a half, so a baked 0.5 is a
        // linear 1.0. Dropping the doubling halves every prop in the game;
        // dropping the decode brightens the darks. Neither errors.
        let mut h = harness!(false);
        let material = h.model_material("");
        let mut lighting = dark_lighting();
        lighting.static_light = 1;

        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.5, 0.5, 0.5, 1.0]);
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels), [255, 255, 255, 255]);

        // A quarter is `GammaToLinear( 0.5 )` = 0.5^2.2 = 0.2176.
        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.25, 0.25, 0.25, 1.0]);
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        let expected = (0.5f32.powf(2.2) * 255.0).round() as i32;
        assert!(
            (centre(&pixels)[0] as i32 - expected).abs() <= 1,
            "expected about {expected}, got {:?}",
            centre(&pixels)
        );
    }

    #[test]
    fn baked_vertex_light_is_ignored_when_it_is_disabled() {
        // `g_flStaticLightEnabled`. The stream is always in the vertex buffer;
        // this flag is the only thing that says whether it means anything.
        let mut h = harness!(false);
        let material = h.model_material("");
        let lighting = dark_lighting();

        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.5, 0.5, 0.5, 1.0]);
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels)[0], 0);
    }

    #[test]
    fn a_directional_light_ignores_distance_and_a_point_light_does_not() {
        // The two `lerp`s at the end of `VertexAttenInternal`, which are how a
        // shader with no branches encoded the light type. `color.w` selects
        // directional and `direction.w` selects spot; get them the wrong way
        // round and every point light in the game becomes unattenuated.
        let mut h = harness!(false);
        let material = h.model_material("");
        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.0; 4]);

        // A directional light shining along -z onto a +z-facing quad: N.L = 1,
        // no attenuation, so a half-grey light reads back as half grey.
        let mut lighting = dark_lighting();
        lighting.lights[0] = Light::directional([0.5, 0.5, 0.5], [0.0, 0.0, -1.0]);
        lighting.count = 1;
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(
            centre(&pixels)[0],
            128,
            "a directional light is unattenuated"
        );

        // The same light as a point light above the quad's centre, with a
        // purely quadratic falloff.
        //
        // **The expected value is computed at a corner, not at the centre**,
        // and that is the second thing this test pins: the unbumped path lights
        // in the *vertex* shader (`vertexlit_and_unlit_generic_vs20.fxc:437`),
        // so what the middle pixel shows is the interpolation of four corner
        // values, not the lighting of the middle. The four corners are
        // equidistant from a light above the centre, so the interpolant is flat
        // and the corner value is the answer — but it is emphatically not
        // `1/2² * 0.5`, which is what the same light would give if this shader
        // were Phong-shaded. Unify the two paths and this test is what fails.
        let mut lighting = dark_lighting();
        const HEIGHT: f32 = 2.0;
        lighting.lights[0] = Light::point([0.5, 0.5, 0.5], [0.5, 0.5, HEIGHT], [0.0, 0.0, 1.0]);
        lighting.count = 1;
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        // A corner is at (0, 0, 0) and the light at (0.5, 0.5, 2).
        let distance_squared = 0.5 * 0.5 + 0.5 * 0.5 + HEIGHT * HEIGHT;
        let cosine = HEIGHT / distance_squared.sqrt();
        let expected = (0.5 * cosine / distance_squared * 255.0).round() as i32;
        assert!(
            (centre(&pixels)[0] as i32 - expected).abs() <= 1,
            "expected about {expected} from 1/d^2 at a corner, got {:?}",
            centre(&pixels)
        );
    }

    #[test]
    fn half_lambert_lights_a_surface_that_lambert_leaves_black() {
        // `$halflambert`, and the reason it is here at all: the tree this port
        // is derived from hard-codes `bHalfLambert = false` "for CSGO"
        // (`vertexlitgeneric_dx9_helper.cpp:679`) over a commented-out read of
        // the material flag. Portal 2 reads the flag, so this port does.
        //
        // A surface exactly edge-on to the light has N.L = 0: Lambert gives
        // black, half-Lambert gives (0.5)^2 = a quarter.
        let mut h = harness!(false);
        let mut lighting = dark_lighting();
        lighting.lights[0] = Light::directional([1.0, 1.0, 1.0], [0.0, -1.0, 0.0]);
        lighting.count = 1;
        // Normal +z, light shining along -y: perpendicular.
        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.0; 4]);

        let lambert = h.model_material("");
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&lambert, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels)[0], 0, "N.L is zero, so Lambert is black");

        let half = h.model_material(r#""$halflambert" "1""#);
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&half, &v, &i, Mat4::IDENTITY);
        });
        let expected = (0.25 * 255.0f32).round() as i32;
        assert!(
            (centre(&pixels)[0] as i32 - expected).abs() <= 1,
            "expected about {expected}, got {:?}",
            centre(&pixels)
        );
    }

    #[test]
    fn self_illumination_emits_where_the_lighting_is_black() {
        // `$selfillum` lerps the lit colour toward `$selfillumtint * albedo` by
        // base alpha. With no lighting at all, an alpha of 1 is the whole
        // difference between a black model and a lit-looking one.
        let mut h = harness!(false);
        let lighting = dark_lighting();
        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.0; 4]);

        let material = shader_material(
            &h.device,
            &h.queue,
            &h.pipelines,
            "VertexLitGeneric",
            r#""$selfillum" "1" "$selfillumtint" "[1 0 0]""#,
            // Opaque white: base alpha is the self-illum mask.
            h.texture([255, 255, 255, 255]),
        );
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(
            centre(&pixels)[0..3],
            [255, 0, 0],
            "a fully self-illuminating material emits its tint"
        );
    }

    #[test]
    fn a_bumped_model_takes_no_baked_vertex_light() {
        // The asymmetry between Valve's two files, and the one most likely to
        // read as a bug: `vertexlit_and_unlit_generic_bump_ps2x.fxc:452` calls
        // `PixelShaderDoLighting` with `bStaticLight = false`, so a material
        // with a `$bumpmap` is lit by the ambient cube and the local lights
        // and by nothing else — the same model without the bump map is not.
        let mut h = harness!(false);
        let mut lighting = dark_lighting();
        lighting.static_light = 1;
        let (vertices, indices) = model_quad([0.0, 0.0, 1.0], [0.5, 0.5, 0.5, 1.0]);

        let unbumped = h.model_material("");
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&unbumped, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels)[0], 255, "unbumped reads the baked stream");

        let bumped = h.model_material(r#""$bumpmap" "test""#);
        let pixels = h.render(|pass| {
            pass.set_model_lighting(&lighting);
            let v = pass.vertices(&vertices);
            let i = pass.indices(&indices);
            pass.draw(&bumped, &v, &i, Mat4::IDENTITY);
        });
        assert_eq!(centre(&pixels)[0], 0, "bumped does not");
    }

    #[test]
    fn model_lighting_is_per_instance_and_two_draws_can_differ() {
        // The same hazard `DrawUniforms` has and for the same reason:
        // `Queue::write_buffer` stages its copy ahead of the whole command
        // buffer, so a single lighting buffer rewritten between draws would
        // give every draw in the frame the last values written. Two quads,
        // side by side, two lighting states.
        let mut h = harness!(false);
        let material = h.model_material("");

        let mut bright = dark_lighting();
        bright.static_light = 1;
        let dark = dark_lighting();

        let (left, indices) = model_quad([0.0, 0.0, 1.0], [0.5, 0.5, 0.5, 1.0]);
        let mut right = left;
        for vertex in &mut right {
            vertex.position[0] = vertex.position[0] * 0.5 + 0.5;
        }
        let mut left = left;
        for vertex in &mut left {
            vertex.position[0] *= 0.5;
        }

        let pixels = h.render(|pass| {
            let i = pass.indices(&indices);

            pass.set_model_lighting(&bright);
            let v = pass.vertices(&left);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);

            pass.set_model_lighting(&dark);
            let v = pass.vertices(&right);
            pass.draw(&material, &v, &i, Mat4::IDENTITY);
        });

        assert_eq!(pixel(&pixels, TARGET / 4, TARGET / 2)[0], 255);
        assert_eq!(pixel(&pixels, 3 * TARGET / 4, TARGET / 2)[0], 0);
    }
}
