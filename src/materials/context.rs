//! The render context: what a pass draws into, with which camera, and the
//! per-frame and per-draw constants underneath it.
//!
//! Replaces `materialsystem/cmatrendercontext.cpp` (3,455 lines) and
//! `cmatrendercontext.h`'s `CMatRenderContextBase`. `CMatQueuedRenderContext`
//! and `cmaterial_queuefriendly` are deleted outright — see
//! `portdocs/MATERIALSYSTEM.md` §5.3; `wgpu` records command buffers on any
//! thread, so the machinery that existed to funnel D3D9 calls onto one thread
//! has nothing to do.
//!
//! # The three stacks are gone, and this is why
//!
//! `CMatRenderContextBase` carries `m_MatrixStacks[NUM_MATRIX_MODES]`,
//! `m_RenderTargetStack` and `m_ScissorRectStack`, and the engine drives them
//! in the fixed-function idiom OpenGL 1.x taught it:
//!
//! ```text
//! MatrixMode( MATERIAL_PROJECTION ); PushMatrix(); LoadIdentity();
//! Scale( 1, -1, 1 ); Ortho( 0, 0, width, height, -99999, 99999 );
//! ... draw ...
//! MatrixMode( MATERIAL_PROJECTION ); PopMatrix();
//! ```
//!
//! (`engine/gl_rmain.cpp:920-985`, the screen-fade quad, doing it three times
//! over for model, view and projection.) Those stacks exist because D3D9 had
//! one global device whose state every draw shared, so anything that wanted
//! different state had to save it, change it, and put it back.
//!
//! **A `wgpu` render pass already is that saved state**, and it is scoped by
//! the borrow checker rather than by a `CUtlStack`. So:
//!
//! | `CMatRenderContext` | Here |
//! |---|---|
//! | `m_RenderTargetStack` entry: targets + depth + viewport | the arguments to [`RenderContext::pass`] |
//! | `MATERIAL_VIEW` / `MATERIAL_PROJECTION` stacks | [`Camera`], likewise a pass argument |
//! | `MATERIAL_MODEL` stack | a parameter of [`Pass::draw`] |
//! | `m_ScissorRectStack` | [`Pass::set_scissor`], which a pass ends |
//! | `PushRenderTargetAndViewport` / `Pop` | opening a pass and letting it drop |
//!
//! One thing genuinely changes shape, and it is worth knowing before designing
//! around it: **`wgpu` render passes do not nest.** A pass must end before the
//! next one begins on the same encoder. Portal views, water reflections and
//! post-processing therefore run *innermost first* — render the portal's view
//! into a [`RenderTarget`], end that pass, then open the main pass and sample
//! it — rather than pushing a target in the middle of a draw sequence. That is
//! the resolution of the open question `portdocs/MATERIALSYSTEM.md` §10 calls
//! the highest risk after the shaders: the RT stack does not need
//! restructuring, it needs deleting, and the dependency order it implied
//! becomes explicit.
//!
//! # Uniforms are arenas, not single buffers
//!
//! The one hazard this module exists to prevent. Every pass recorded into a
//! frame is submitted as **one command buffer**, and `Queue::write_buffer`
//! stages its copy to run *before* that whole command buffer — not at the point
//! in the recording where it was called. So writing one uniform buffer per draw
//! and re-writing it for the next draw does not give each draw its own
//! constants: it gives every draw in the frame the *last* values written.
//!
//! Both blocks are therefore bump-allocated out of a large buffer, one slot per
//! pass for [`FrameUniforms`] and one per draw for [`DrawUniforms`], bound with
//! a dynamic offset. That is the same reason `shaderapidx9`'s dynamic vertex
//! buffers sub-allocated rather than re-locking, one level up.

// `directx` names the NDC convention, not the API: right-handed Y-up view
// space in, Z in `0..1` and Y-up out, which is exactly WebGPU's. The `opengl`
// module produces Z in `-1..1` and the `vulkan` one flips Y; either would draw
// a picture, just the wrong one. `Mat4::perspective_rh` is the deprecated
// spelling of this same function.
use glam::camera::rh::proj::directx;
use glam::{Mat4, Vec3};

use super::material::Material;
use super::mesh::{DynamicBuffers, IndexSlice, VertexSlice};
use super::pipeline::{PipelineCache, PipelineKey, RenderState, TargetFormat};
use super::renderer::Frame;
use super::shader::LightingBinding;
use super::target::{RenderTarget, CLEAR_DEPTH};
use super::uniforms::{self, DrawUniforms, FrameUniforms, ModelLighting};

/// Where a camera is and what it sees.
///
/// The `MATERIAL_VIEW` and `MATERIAL_PROJECTION` matrices, as one value,
/// because nothing ever wanted one without the other: `RecomputeViewProjState`
/// (`cmatrendercontext.cpp`) existed to cache their product and every consumer
/// read that.
///
/// Both are column-major and multiply on the left — see
/// [`uniforms`](super::uniforms). A matrix transcribed from Valve's row-major
/// `VMatrix` goes through
/// [`from_row_major`](super::uniforms::from_row_major) first.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// World space to view space.
    pub view: Mat4,
    /// View space to clip space. Depth must land in `0..1`, which is `wgpu`'s
    /// convention and D3D9's; `glam`'s `camera::rh::proj::opengl` module
    /// produces `-1..1` instead, which halves the usable depth range without
    /// producing an error. Use [`Camera::perspective`] and
    /// [`Camera::orthographic`] rather than picking from `glam` directly.
    pub projection: Mat4,
    /// The eye in world space. `cEyePos_WaterHeightW`, which range fog reads.
    pub eye: Vec3,
}

// `screen` and `orthographic` have no caller in the binary yet: the 2D and
// ortho views are the HUD, the screen-space post passes and the shadow
// cascades, none of which are ported. Both are exercised by the GPU tests.
#[allow(dead_code)]
impl Camera {
    /// A camera that draws a `0..1` square over the whole target, `y` down and
    /// `z` away from the viewer.
    ///
    /// The 2D setup `engine/gl_rmain.cpp:926` builds with
    /// `Ortho( 0, 0, width, height, -99999, 99999 )` after `Scale( 1, -1, 1 )`,
    /// in units of the target rather than pixels. Deliberately not the
    /// identity, so that a transposed matrix shows up rather than drawing a
    /// correctly centred quad anyway.
    ///
    /// **The `near`/`far` arguments come in reversed, and that is the point.**
    /// `glam`'s are *distances along `-z`*, because a right-handed camera looks
    /// down `-z`; passing `-1.0, 1.0` would therefore put the near plane behind
    /// the viewer and make a *larger* `z` mean *nearer* — a painter's-order
    /// trap for anything that thinks in layers. Passing them the other way
    /// round gives the convention `Ortho` had and the one this camera's `y`
    /// flip already implies: `z = -1` at the front, `z = +1` at the back.
    /// `the_screen_camera_puts_z_into_the_screen` pins it.
    pub fn screen() -> Camera {
        Camera {
            view: Mat4::IDENTITY,
            projection: directx::orthographic(0.0, 1.0, 1.0, 0.0, 1.0, -1.0),
            eye: Vec3::ZERO,
        }
    }

    /// A perspective camera. `PerspectiveX( fovX, aspect, zNear, zFar )`.
    ///
    /// `fov_x_degrees` is the *horizontal* field of view, as every Valve
    /// entry point takes it (`CViewSetup::fov`); `glam` wants the vertical one,
    /// so the aspect ratio converts between them exactly as
    /// `CalcFovY` (`engine/view.cpp`) does.
    pub fn perspective(
        eye: Vec3,
        view: Mat4,
        fov_x_degrees: f32,
        aspect: f32,
        z_near: f32,
        z_far: f32,
    ) -> Camera {
        let fov_x = fov_x_degrees.to_radians();
        let fov_y = 2.0 * ((fov_x * 0.5).tan() / aspect).atan();
        Camera {
            view,
            projection: directx::perspective(fov_y, aspect, z_near, z_far),
            eye,
        }
    }

    /// An orthographic camera over the given clip box.
    ///
    /// The six clip-box arguments are `Ortho( left, top, right, bottom, zNear,
    /// zFar )`'s, reordered to `glam`'s; grouping them into a rectangle type
    /// would hide which pair is which rather than clarify it.
    #[allow(clippy::too_many_arguments)]
    pub fn orthographic(
        eye: Vec3,
        view: Mat4,
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        z_near: f32,
        z_far: f32,
    ) -> Camera {
        Camera {
            view,
            projection: directx::orthographic(left, right, bottom, top, z_near, z_far),
            eye,
        }
    }

    /// World space straight to clip space: `projection * view`.
    ///
    /// `m_viewProjMatrix`, which `RecomputeViewProjState` cached behind a dirty
    /// flag. Not cached here — it is one 4x4 multiply per *pass*, not per draw,
    /// because a pass has one camera.
    pub fn view_proj(&self) -> Mat4 {
        self.projection * self.view
    }
}

/// What a pass does with whatever the target already holds.
// `Keep` has no caller while there is one pass per frame; the second pass over
// a target is what wants it, which is the HUD or a post-processing chain.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum Load {
    /// `ClearBuffers( true, true )`: colour to `color`, depth to the far plane.
    Clear(wgpu::Color),
    /// Keep it. What every pass after the first in a frame wants.
    Keep,
}

/// Per-pass and per-draw overrides of a material's pipeline state.
///
/// `CMatRenderContext`'s `OverrideDepthEnable`, `CullMode` and `FlipCullMode`,
/// which are the only parts of the render-state stack the engine actually
/// pushes: everything else in `RenderState` comes from the `.vmt` and does not
/// vary by who is drawing.
///
/// `FlipCullMode` is not a debug feature — a mirror or a portal view flips the
/// view matrix horizontally, which reverses every triangle's winding, and
/// without the flip the whole reflected world is back-face culled.
#[derive(Debug, Clone, Copy, Default)]
pub struct StateOverride {
    /// `Some(false)` is `CullMode( MATERIAL_CULLMODE_NONE )`; `Some(true)`
    /// forces culling back on for a material that asked for `$nocull`.
    pub cull: Option<bool>,
    /// `OverrideDepthEnable( true, write, test )`. `None` leaves the
    /// material's own choice alone.
    pub depth_test: Option<bool>,
    /// The other half of `OverrideDepthEnable`.
    pub depth_write: Option<bool>,
}

impl StateOverride {
    fn apply(self, mut state: RenderState) -> RenderState {
        if let Some(cull) = self.cull {
            state.cull = cull;
        }
        if let Some(test) = self.depth_test {
            state.depth_test = test;
        }
        if let Some(write) = self.depth_write {
            state.depth_write = write;
        }
        state
    }
}

/// Everything a frame needs that is not a material, a texture or a pipeline.
///
/// One per renderer. Holds `wgpu::Device` and `wgpu::Queue` by value for the
/// same reason [`TextureCache`](super::texture::TextureCache) does: both are
/// cheap handles to shared state, so this stays independent of
/// [`Renderer`](super::Renderer) rather than living inside it.
pub struct RenderContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// One slot per pass. See the module docs on why this is an arena.
    frames: UniformArena,
    /// One slot per draw.
    draws: UniformArena,
    /// One slot per `set_model_lighting`. Group 3 for the shaders that light a
    /// model rather than sample a lightmap page.
    lights: UniformArena,
    dynamic: DynamicBuffers,
    /// Group 3 for a pass that has not bound a real lightmap page.
    ///
    /// `MATERIAL_SYSTEM_LIGHTMAP_PAGE_WHITE`, which `AllocateWhiteLightmap`
    /// (`cmatlightmaps.cpp:562`) hands to unlit surfaces. A lit shader with no
    /// page bound is a caller mistake, but the cheap answer to it is fullbright
    /// rather than a `wgpu` validation error, and the white page is what the
    /// engine binds for exactly that case anyway.
    white_lightmap: WhiteLightmap,
}

/// The one-texel white page and the bind group over it.
struct WhiteLightmap {
    /// Held so the view the bind group refers to stays alive.
    #[allow(dead_code)]
    texture: super::texture::Texture,
    bind_group: wgpu::BindGroup,
}

// `target_pass` and `offscreen_pass` are the render-target half of stage 4 and
// have no caller in the binary until portal views or post-processing land;
// `offscreen_pass` is what the GPU tests render through.
#[allow(dead_code)]
impl RenderContext {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipelines: &PipelineCache,
    ) -> RenderContext {
        let layouts = pipelines.layouts();
        RenderContext {
            white_lightmap: white_lightmap(device, queue, layouts),
            frames: UniformArena::new(
                device,
                "frame uniforms",
                layouts.frame(),
                size_of::<FrameUniforms>() as u64,
                INITIAL_PASSES,
            ),
            draws: UniformArena::new(
                device,
                "draw uniforms",
                layouts.draw(),
                size_of::<DrawUniforms>() as u64,
                INITIAL_DRAWS,
            ),
            lights: UniformArena::new(
                device,
                "model lighting",
                layouts.model_lighting(),
                size_of::<ModelLighting>() as u64,
                INITIAL_LIGHTING,
            ),
            dynamic: DynamicBuffers::new(device),
            device: device.clone(),
            queue: queue.clone(),
        }
    }

    /// Reclaims everything last frame allocated.
    ///
    /// Call once per frame, before the first pass and after the previous
    /// frame's [`Frame::present`](super::renderer::Frame::present). Anything
    /// still holding a slice or a uniform slot from the previous frame will
    /// read whatever overwrites it.
    pub fn begin_frame(&mut self) {
        self.frames.begin_frame(&self.device, &self.queue);
        self.draws.begin_frame(&self.device, &self.queue);
        self.lights.begin_frame(&self.device, &self.queue);
        self.dynamic.begin_frame(&self.device);
    }

    /// Opens a pass against the swap-chain image and the renderer's depth
    /// buffer.
    ///
    /// The pass ends when the returned [`Pass`] drops, which is also when the
    /// next one may begin — `wgpu` allows one open pass per encoder, so the
    /// borrow of `frame` is what enforces it.
    pub fn pass<'a>(
        &'a mut self,
        frame: &'a mut Frame<'_>,
        pipelines: &'a mut PipelineCache,
        camera: &Camera,
        load: Load,
    ) -> Pass<'a> {
        let target = frame.target_format();
        let size = frame.size();
        let (encoder, color, depth) = frame.parts();
        self.open(
            encoder, color, depth, target, size, pipelines, camera, load, "screen",
        )
    }

    /// Opens a pass against an offscreen [`RenderTarget`].
    ///
    /// The other half of `PushRenderTargetAndViewport`. Because passes do not
    /// nest, this runs *before* the pass that samples the target, not inside
    /// it — see the module docs.
    pub fn target_pass<'a>(
        &'a mut self,
        frame: &'a mut Frame<'_>,
        pipelines: &'a mut PipelineCache,
        target: &'a RenderTarget,
        camera: &Camera,
        load: Load,
    ) -> Pass<'a> {
        let (encoder, _, _) = frame.parts();
        self.offscreen_pass(encoder, pipelines, target, camera, load)
    }

    /// [`target_pass`](RenderContext::target_pass) with a caller-supplied
    /// encoder, for rendering that is not part of a presented frame.
    ///
    /// Screenshots, warming a render target before the first frame, and the
    /// GPU tests all want to draw without a swap-chain image in hand. The
    /// caller submits the encoder itself.
    pub fn offscreen_pass<'a>(
        &'a mut self,
        encoder: &'a mut wgpu::CommandEncoder,
        pipelines: &'a mut PipelineCache,
        target: &RenderTarget,
        camera: &Camera,
        load: Load,
    ) -> Pass<'a> {
        let format = target.format();
        let size = target.size();
        self.open(
            encoder,
            target.view(),
            target.depth_view(),
            format,
            size,
            pipelines,
            camera,
            load,
            "render target",
        )
    }

    /// The body both entry points share.
    #[allow(clippy::too_many_arguments)]
    fn open<'a>(
        &'a mut self,
        encoder: &'a mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        target: TargetFormat,
        size: (u32, u32),
        pipelines: &'a mut PipelineCache,
        camera: &Camera,
        load: Load,
        label: &'static str,
    ) -> Pass<'a> {
        // Written before the pass opens, into this pass's own slot. The offset
        // is what keeps it distinct from every other pass in the frame.
        let frame_uniforms = FrameUniforms::new(
            uniforms::from_mat4(camera.view_proj()),
            camera.eye.to_array(),
            size,
        );
        let frame_offset = self.frames.push(
            &self.device,
            &self.queue,
            bytemuck::bytes_of(&frame_uniforms),
        );
        // One slot per pass, so there is nothing to batch: flushed here rather
        // than in `Pass::drop` because the pass does not hold this arena.
        self.frames.flush(&self.queue);

        // Every pass starts with one fullbright lighting slot, so that a model
        // draw before `set_model_lighting` binds something well-defined rather
        // than reading whichever instance's lights were last in the arena. One
        // slot per pass, which is the same cost as the frame block above.
        let lighting_offset = self.lights.push(
            &self.device,
            &self.queue,
            bytemuck::bytes_of(&ModelLighting::fullbright()),
        );

        let (color_load, depth_load) = match load {
            Load::Clear(color) => (wgpu::LoadOp::Clear(color), wgpu::LoadOp::Clear(CLEAR_DEPTH)),
            Load::Keep => (wgpu::LoadOp::Load, wgpu::LoadOp::Load),
        };

        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: color_load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: depth.map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: depth_load,
                    store: wgpu::StoreOp::Store,
                }),
                // `None` is a read-only stencil aspect, which is what the
                // pipelines' `StencilState::default()` declares. See
                // `target::DEPTH_FORMAT` for why the format has a stencil at
                // all when nothing writes it yet.
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        Pass {
            pass,
            device: &self.device,
            queue: &self.queue,
            draws: &mut self.draws,
            lights: &mut self.lights,
            lighting_offset,
            dynamic: &mut self.dynamic,
            frame_bind_group: self.frames.bind_group().clone(),
            frame_offset,
            pipelines,
            target,
            overrides: StateOverride::default(),
            white_lightmap: self.white_lightmap.bind_group.clone(),
            lightmap: None,
            static_light: None,
            bound: BoundState::default(),
        }
    }
}

/// Builds [`RenderContext::white_lightmap`]: one opaque white texel in the
/// page format, so that binding it is indistinguishable from binding a real
/// page whose surface happens to be fullbright.
fn white_lightmap(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layouts: &super::pipeline::BindLayouts,
) -> WhiteLightmap {
    // 1.0 as IEEE binary16, four channels. Matches `lightmap`'s page 0.
    const ONE: u16 = 0x3C00;
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("white lightmap"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..wgpu::SamplerDescriptor::default()
    });
    let texture = super::texture::Texture::from_pixels(
        device,
        queue,
        "white lightmap",
        1,
        1,
        wgpu::TextureFormat::Rgba16Float,
        bytemuck::cast_slice(&[ONE; 4]),
        sampler,
    );
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("white lightmap"),
        layout: layouts.lightmap(),
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
    WhiteLightmap {
        texture,
        bind_group,
    }
}

/// One open render pass: a target, a camera, and the draws recorded into it.
///
/// Ends when dropped. Everything Valve stacked is either a field here (set for
/// the pass) or an argument to [`draw`](Pass::draw) (set per draw) — see the
/// module docs for the mapping.
pub struct Pass<'a> {
    pass: wgpu::RenderPass<'a>,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    draws: &'a mut UniformArena,
    /// The model-lighting arena, group 3 for the shaders that read one.
    lights: &'a mut UniformArena,
    /// The slot in it that subsequent model draws bind. Set by
    /// [`set_model_lighting`](Pass::set_model_lighting); starts at the
    /// fullbright block this pass allocated when it opened.
    lighting_offset: u32,
    /// Cloned rather than borrowed because the arena it belongs to may be
    /// replaced by a growing draw allocation, and `set_bind_group` needs
    /// something that outlives that. `wgpu::BindGroup` is a refcounted handle.
    frame_bind_group: wgpu::BindGroup,
    frame_offset: u32,
    pipelines: &'a mut PipelineCache,
    dynamic: &'a mut DynamicBuffers,
    target: TargetFormat,
    overrides: StateOverride,
    /// The fallback for a lit draw with no page bound. Cloned for the same
    /// reason `frame_bind_group` is.
    white_lightmap: wgpu::BindGroup,
    /// The page [`bind_lightmap_page`](Pass::bind_lightmap_page) last named.
    lightmap: Option<wgpu::BindGroup>,
    /// The baked static light slot 1 binds. See
    /// [`bind_static_light`](Pass::bind_static_light).
    static_light: Option<VertexSlice>,
    /// What is already set on the encoder, so a draw only changes what
    /// differs. See [`BoundState`].
    bound: BoundState,
}

/// A reserved slot in the pass's model-lighting arena.
///
/// Opaque because it is a dynamic-offset into a buffer that may be replaced
/// mid-pass; only [`Pass`] knows when that stays valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightingSlot(u32);

/// The render state the pass has already recorded.
///
/// **A redundant `set_*` is not free.** Each one is a validation pass and an
/// encoder command, and a scene of static props issues the same pipeline, the
/// same material bind group and the same vertex buffer for every instance of
/// a model — 1,080 times over on `sp_a1_intro1`. Skipping the ones that do not
/// change is worth roughly as much as batching the uniform writes was, and the
/// two together are what took that map from a 78 fps CPU ceiling to a
/// four-figure one (`world::bench`).
///
/// `wgpu`'s resource handles compare by identity, so this is a pointer
/// comparison and not a deep one. Group 2 is deliberately absent: its dynamic
/// offset is different for every draw by construction, so it is bound every
/// time and there is nothing to remember.
#[derive(Default)]
struct BoundState {
    /// The key as well as the pipeline, so that a repeat draw skips the
    /// cache's hash lookup and not just the `set_pipeline`.
    pipeline: Option<(PipelineKey, std::sync::Arc<wgpu::RenderPipeline>)>,
    /// Group 0. Constant for the whole pass, so this is set exactly once.
    frame: Option<(wgpu::BindGroup, u32)>,
    /// Group 1 — the material's textures and constants.
    material: Option<wgpu::BindGroup>,
    /// Group 3 — a lightmap page or a model's lighting block, with its offset.
    lighting: Option<(wgpu::BindGroup, u32)>,
    vertices: Option<(wgpu::Buffer, u64, u32)>,
    /// Slot 1, the static-light stream.
    static_light: Option<(wgpu::Buffer, u64, u32)>,
    indices: Option<(wgpu::Buffer, u64, u32)>,
}

// Viewport, scissor and depth range are per-pass state the engine sets and
// nothing in this binary does yet: split screen, the view model's compressed
// depth range, and `m_ScissorRectStack`'s users respectively.
#[allow(dead_code)]
impl Pass<'_> {
    /// Restricts drawing to part of the target. `IMatRenderContext::Viewport`.
    ///
    /// In physical pixels, origin top-left, which is both `wgpu`'s convention
    /// and D3D9's. Resets to the whole target when the pass ends, so there is
    /// nothing to put back.
    pub fn set_viewport(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.pass.set_viewport(x, y, width, height, 0.0, 1.0);
    }

    /// `DepthRange( zNear, zFar )`, which is the depth half of the viewport.
    ///
    /// Used to compress everything into a sliver of the depth range so that a
    /// view model cannot poke through the world.
    pub fn set_depth_range(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        near: f32,
        far: f32,
    ) {
        self.pass.set_viewport(x, y, width, height, near, far);
    }

    /// `m_ScissorRectStack`'s entry, with the stack replaced by the pass.
    pub fn set_scissor(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.pass.set_scissor_rect(x, y, width, height);
    }

    /// Binds the lightmap atlas page every subsequent lit draw samples.
    ///
    /// `IMatRenderContext::BindLightmapPage( lightmapPageID )`, which the world
    /// draw calls once per sort ID before the batch that uses it
    /// (`gl_rsurf.cpp:1150`). It is render-context state rather than a material
    /// property because it is not one: a material's surfaces are spread over
    /// however many atlas pages the packer needed.
    ///
    /// Applies from here to the end of the pass, like
    /// [`set_state_override`](Pass::set_state_override). Draws of a shader that
    /// does not read a lightmap ignore it.
    pub fn bind_lightmap_page(&mut self, page: &super::lightmap::LightmapPage) {
        self.lightmap = Some(page.bind_group().clone());
    }

    /// Sets the per-vertex baked static light every subsequent model draw
    /// reads — vertex buffer slot 1.
    ///
    /// `CStaticProp`'s colour mesh (`engine/l_studio.cpp:4290`), which the
    /// original binds beside the model's own geometry for exactly this reason:
    /// `vrad` bakes one colour per vertex **per placement**, so a thousand
    /// copies of one crate share a vertex buffer and have a thousand lighting
    /// streams. See [`StaticLightVertex`](super::mesh::StaticLightVertex).
    ///
    /// Applies from here to the end of the pass or the next call, like
    /// [`bind_lightmap_page`](Pass::bind_lightmap_page). Every draw of a
    /// [`VertexLayout::Model`] material needs one bound — including a model
    /// with no baked lighting, which binds a black stream and clears
    /// [`ModelLighting::static_light`] so the shader ignores it.
    ///
    /// # Panics
    ///
    /// If `vertices` is not a [`VertexLayout::StaticLight`] buffer. Binding a
    /// model's *geometry* here would otherwise reinterpret 48-byte vertices as
    /// 4-byte colours and light every prop from its own positions.
    pub fn bind_static_light(&mut self, vertices: &VertexSlice) {
        assert_eq!(
            vertices.layout(),
            super::mesh::VertexLayout::StaticLight,
            "the static light stream must be a StaticLightVertex buffer, got {:?}",
            vertices.layout()
        );
        self.static_light = Some(vertices.clone());
    }

    /// Sets the ambient cube and local lights every subsequent model draw is
    /// lit by.
    ///
    /// `R_StudioSetupLighting` plus the two per-instance commands it feeds —
    /// `PI_SetVertexShaderAmbientLightCube` and `PI_SetPixelShaderLocalLighting`
    /// (`vertexlitgeneric_dx9_helper.cpp:634`). It is pass state rather than an
    /// argument to a draw for the reason those are per-*instance* commands: a
    /// model is one lighting state and many meshes, so binding it per draw
    /// would re-upload the same 432 bytes once per material the model wears.
    ///
    /// Applies from here to the end of the pass or the next call, like
    /// [`bind_lightmap_page`](Pass::bind_lightmap_page). Draws of a shader that
    /// does not light a model ignore it.
    ///
    /// Each call takes its own arena slot, so a caller may set it, draw, set it
    /// again and draw again within one pass — which is exactly what a scene of
    /// props does.
    pub fn set_model_lighting(&mut self, lighting: &ModelLighting) {
        let slot = self.push_model_lighting(lighting);
        self.set_model_lighting_slot(slot);
    }

    /// Reserves a lighting slot without binding it, for a caller that wants
    /// many and will draw with them in a different order.
    ///
    /// A scene of static props wants exactly this: one lighting state per
    /// instance, but drawn **batch-major** so that a model's pipeline, material
    /// and buffers are bound once rather than once per instance. Pushing as it
    /// draws would cost one slot per (batch, instance) instead of one per
    /// instance. See [`PropModels::draw`].
    ///
    /// Slots stay valid for the rest of the pass, including across a growth of
    /// the arena — see [`UniformArena::grow`].
    ///
    /// [`PropModels::draw`]: crate::engine::world::props::PropModels::draw
    pub fn push_model_lighting(&mut self, lighting: &ModelLighting) -> LightingSlot {
        LightingSlot(
            self.lights
                .push(self.device, self.queue, bytemuck::bytes_of(lighting)),
        )
    }

    /// Binds a slot taken by [`push_model_lighting`](Pass::push_model_lighting)
    /// for every subsequent model draw.
    pub fn set_model_lighting_slot(&mut self, slot: LightingSlot) {
        self.lighting_offset = slot.0;
    }

    /// Overrides part of every subsequent draw's pipeline state.
    ///
    /// See [`StateOverride`]. Applies from here to the end of the pass or the
    /// next call, whichever comes first.
    pub fn set_state_override(&mut self, overrides: StateOverride) {
        self.overrides = overrides;
    }

    /// The format every pipeline used in this pass must be built for.
    pub fn target_format(&self) -> TargetFormat {
        self.target
    }

    /// Writes vertices that live for this frame. `GetDynamicMesh`'s vertex
    /// half.
    ///
    /// On the pass rather than on [`RenderContext`] because that is how the
    /// engine draws: `GetDynamicMesh` -> fill -> `Draw`, once per batch, over
    /// and over inside what is one pass here
    /// (`engine/gl_rsurf.cpp:1168`, `studiorender/r_studiodraw.cpp:2268`). An
    /// API that made a caller allocate everything before opening a pass would
    /// be unusable for exactly the two call sites stage 4 was designed against.
    pub fn vertices<V: super::mesh::Vertex>(&mut self, vertices: &[V]) -> VertexSlice {
        self.dynamic.vertices(self.device, self.queue, vertices)
    }

    /// Writes indices that live for this frame.
    pub fn indices(&mut self, indices: &[u16]) -> IndexSlice {
        self.dynamic.indices(self.device, self.queue, indices)
    }

    /// `GetMaxIndicesToRender`. The world renderer reads this before every
    /// batch and splits when one would not fit (`engine/gl_rsurf.cpp:1162`).
    pub fn indices_remaining(&self) -> u32 {
        self.dynamic.indices_remaining()
    }

    /// `GetMaxVerticesToRender`.
    pub fn vertices_remaining(&self, layout: super::mesh::VertexLayout) -> u32 {
        self.dynamic.vertices_remaining(layout)
    }

    /// Draws indexed geometry with a material.
    ///
    /// `model` is object space to world space — the `MATERIAL_MODEL` matrix,
    /// as a parameter rather than a stack, because unlike view and projection
    /// it genuinely changes between draws and a caller doing a hierarchical
    /// traversal already has it in hand.
    ///
    /// # Panics
    ///
    /// If `vertices` is not the layout the material's shader declared. That is
    /// a programming error rather than a data error — both halves are ours, and
    /// the alternative is `wgpu` reading a model's bone weights as a
    /// lightmap coordinate and drawing something that merely looks wrong.
    pub fn draw(
        &mut self,
        material: &Material,
        vertices: &VertexSlice,
        indices: &IndexSlice,
        model: Mat4,
    ) {
        self.draw_modulated(material, vertices, indices, model, [1.0, 1.0, 1.0, 1.0]);
    }

    /// [`draw`](Pass::draw) with a per-instance colour multiplied into the
    /// material's own.
    ///
    /// `IMesh::DrawModulated`, whose `Vector4D diffuseModulation`
    /// `CBaseMeshDX8::DrawMesh` (`shaderapidx9/meshdx8.cpp:2378`) multiplies by
    /// the material's colour and alpha before every draw.
    pub fn draw_modulated(
        &mut self,
        material: &Material,
        vertices: &VertexSlice,
        indices: &IndexSlice,
        model: Mat4,
        modulation: [f32; 4],
    ) {
        let expected = material.shader.vertex_layout();
        assert_eq!(
            vertices.layout(),
            expected,
            "material {:?} wants {:?} vertices, got {:?}",
            material.name,
            expected,
            vertices.layout()
        );

        if indices.is_empty() || vertices.is_empty() {
            return;
        }

        let key = PipelineKey {
            shader: material.shader,
            state: self.overrides.apply(material.state),
            target: self.target,
        };
        // The cache lookup is a hash of the whole key; consecutive draws of one
        // material's instances all want the same pipeline, so the last answer
        // is remembered and the hash skipped.
        let pipeline = match &self.bound.pipeline {
            Some((last_key, pipeline)) if *last_key == key => pipeline.clone(),
            _ => {
                let pipeline = self.pipelines.get(&key);
                self.pass.set_pipeline(&pipeline);
                self.bound.pipeline = Some((key, pipeline.clone()));
                pipeline
            }
        };
        // Bound above when it changed; nothing to do with it here.
        drop(pipeline);

        let draw = DrawUniforms {
            model: uniforms::from_mat4(model),
            modulation: [
                material.modulation[0] * modulation[0],
                material.modulation[1] * modulation[1],
                material.modulation[2] * modulation[2],
                material.modulation[3] * modulation[3],
            ],
        };
        let offset = self
            .draws
            .push(self.device, self.queue, bytemuck::bytes_of(&draw));

        // Everything below only touches the encoder when it would actually
        // change something — see [`BoundState`]. The one exception is group 2,
        // whose dynamic offset is different for every draw by construction.
        let frame = (self.frame_bind_group.clone(), self.frame_offset);
        if self.bound.frame.as_ref() != Some(&frame) {
            self.pass.set_bind_group(0, &frame.0, &[frame.1]);
            self.bound.frame = Some(frame);
        }

        if self.bound.material.as_ref() != Some(material.bind_group()) {
            self.pass.set_bind_group(1, material.bind_group(), &[]);
            self.bound.material = Some(material.bind_group().clone());
        }

        // Always set: the offset below is different for every draw by
        // construction, so there is nothing here to elide.
        self.pass.set_bind_group(2, self.draws.bind_group(), &[offset]);

        match material.shader.lighting_binding() {
            None => {}
            Some(LightingBinding::LightmapPage) => {
                let page = self.lightmap.as_ref().unwrap_or(&self.white_lightmap);
                if self.bound.lighting.as_ref().map(|(g, _)| g) != Some(page) {
                    self.pass.set_bind_group(3, page, &[]);
                    self.bound.lighting = Some((page.clone(), 0));
                }
            }
            // Read after any `set_model_lighting` in this pass, because a push
            // that grew the arena replaced the bind group this names.
            Some(LightingBinding::ModelLighting) => {
                let group = self.lights.bind_group();
                let wanted = (group, self.lighting_offset);
                if self.bound.lighting.as_ref().map(|(g, o)| (g, *o)) != Some(wanted) {
                    self.pass.set_bind_group(3, group, &[self.lighting_offset]);
                    self.bound.lighting = Some((group.clone(), self.lighting_offset));
                }
            }
        }

        let (buffer, offset, count) = vertices.identity();
        if self.bound.vertices.as_ref().map(|(b, o, c)| (b, *o, *c)) != Some((buffer, offset, count))
        {
            self.pass.set_vertex_buffer(0, vertices.buffer_slice());
            self.bound.vertices = Some((buffer.clone(), offset, count));
        }
        if expected.takes_static_light() {
            // A missing stream is a programming error the same way a wrong
            // vertex layout is, and it fails here rather than as a `wgpu`
            // validation message naming a slot number.
            let light = self.static_light.clone().unwrap_or_else(|| {
                panic!(
                    "material {:?} takes model vertices, so a static light stream must be \
                     bound before drawing it — see Pass::bind_static_light",
                    material.name
                )
            });
            assert!(
                light.len() >= vertices.len(),
                "material {:?}: the static light stream has {} vertices for {} of geometry",
                material.name,
                light.len(),
                vertices.len()
            );
            let (buffer, offset, count) = light.identity();
            if self.bound.static_light.as_ref().map(|(b, o, c)| (b, *o, *c))
                != Some((buffer, offset, count))
            {
                self.pass.set_vertex_buffer(1, light.buffer_slice());
                self.bound.static_light = Some((buffer.clone(), offset, count));
            }
        }

        let (buffer, offset, count) = indices.identity();
        if self.bound.indices.as_ref().map(|(b, o, c)| (b, *o, *c)) != Some((buffer, offset, count))
        {
            self.pass
                .set_index_buffer(indices.buffer_slice(), indices.format());
            self.bound.indices = Some((buffer.clone(), offset, count));
        }
        self.pass.draw_indexed(0..indices.len(), 0, 0..1);
    }
}

impl Drop for Pass<'_> {
    /// Hands the pass's staged uniform slots to the queue.
    ///
    /// Correct here rather than at submit time because `write_buffer` orders
    /// its copy ahead of every command buffer submitted afterwards, and this
    /// runs before the encoder is finished, let alone submitted. Doing it per
    /// pass rather than per frame keeps a context that never opens another
    /// pass from holding a frame's writes indefinitely.
    fn drop(&mut self) {
        self.draws.flush(self.queue);
        self.lights.flush(self.queue);
    }
}

/// Slots for a fresh [`RenderContext`]. Both grow; these are the sizes at which
/// an ordinary frame needs no growth.
const INITIAL_PASSES: u64 = 64;
const INITIAL_DRAWS: u64 = 4096;
/// One per model instance rather than per draw — `R_StudioSetupLighting` runs
/// once and every mesh of that model is drawn under it.
const INITIAL_LIGHTING: u64 = 1024;

/// A uniform buffer sub-allocated a slot at a time, bound with a dynamic
/// offset.
///
/// The per-frame and per-draw constant blocks are both this, at different
/// rates. See the module docs for the hazard it exists to prevent.
struct UniformArena {
    label: &'static str,
    layout: wgpu::BindGroupLayout,
    /// The uniform block's real size, which is the binding window `wgpu`
    /// bounds a dynamic offset against.
    size: u64,
    /// That size rounded up to the alignment a dynamic offset must have.
    slot: u64,
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    slots: u64,
    used: u64,
    demand: u64,
    /// A CPU-side mirror of the slots written this frame, flushed to the GPU
    /// in one `write_buffer` rather than one per slot.
    ///
    /// **This is a frame-rate decision, not a tidiness one.** `write_buffer`
    /// is not a memcpy: every call takes a staging-belt chunk, maps it and
    /// records a buffer-to-buffer copy, and at one call per draw a map with
    /// 1,080 static props made ~3,300 of them a frame. Measured on
    /// `sp_a1_intro1` (`world::bench`), collapsing them to one call per pass
    /// per arena is most of the difference between a 78 fps and a 500 fps
    /// ceiling from CPU alone.
    staged: Vec<u8>,
    /// Slots already handed to the queue. A pass flushes what it added rather
    /// than the whole frame's worth, so several passes cost several small
    /// writes rather than one growing quadratically.
    flushed: u64,
}

impl UniformArena {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        layout: &wgpu::BindGroupLayout,
        size: u64,
        slots: u64,
    ) -> UniformArena {
        // A dynamic offset must be a multiple of
        // `min_uniform_buffer_offset_alignment` — 256 on the portable floor
        // this port targets, so a 96-byte block still costs a 256-byte slot.
        // That is the price of one bind group per draw instead of one buffer
        // per draw, and it is a good trade.
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let slot = size.next_multiple_of(alignment);
        let (buffer, bind_group) = Self::allocate(device, label, layout, slot, slots, size);
        UniformArena {
            label,
            layout: layout.clone(),
            size,
            slot,
            buffer,
            bind_group,
            slots,
            used: 0,
            demand: 0,
            staged: vec![0; (slot * slots) as usize],
            flushed: 0,
        }
    }

    fn allocate(
        device: &wgpu::Device,
        label: &'static str,
        layout: &wgpu::BindGroupLayout,
        slot: u64,
        slots: u64,
        size: u64,
    ) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: slot * slots,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                // The binding is one *slot*, not the whole buffer: a dynamic
                // offset moves this window, and its size is what bounds the
                // offset `wgpu` will accept.
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(size),
                }),
            }],
        });
        (buffer, bind_group)
    }

    fn begin_frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.demand > self.slots {
            self.grow(device, queue, self.demand);
        }
        self.used = 0;
        self.demand = 0;
        self.flushed = 0;
    }

    fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, wanted: u64) {
        // Whatever is staged belongs to the buffer being replaced: draws
        // already recorded name its bind group, and it stays alive for them.
        // So it has to be handed over before the swap, not after.
        self.flush(queue);
        self.slots = wanted.next_power_of_two();
        let (buffer, bind_group) = Self::allocate(
            device,
            self.label,
            &self.layout,
            self.slot,
            self.slots,
            self.size,
        );
        self.buffer = buffer;
        self.bind_group = bind_group;
        // **The staged bytes are kept and `used` is not reset**, so every
        // offset handed out before the growth still names the same block. That
        // is what lets a caller take a batch of slots and draw with them later
        // (`push_model_lighting`): the draws recorded before the growth name
        // the old bind group, which was just flushed with the right contents,
        // and the ones after name the new one, which is about to be flushed
        // with all of it.
        self.staged.resize((self.slot * self.slots) as usize, 0);
        self.flushed = 0;
    }

    /// Hands every slot staged since the last flush to the queue.
    ///
    /// Correct to call at any point before `submit`, because `write_buffer`
    /// stages its copy ahead of the whole command buffer — which is the same
    /// property that makes a *rewritten* slot reach every draw in the frame,
    /// and so the reason each draw gets its own slot in the first place.
    fn flush(&mut self, queue: &wgpu::Queue) {
        if self.used <= self.flushed {
            return;
        }
        let from = (self.flushed * self.slot) as usize;
        let to = (self.used * self.slot) as usize;
        queue.write_buffer(&self.buffer, from as u64, &self.staged[from..to]);
        self.flushed = self.used;
    }

    fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Reserves a slot, writes `bytes` into it, and returns the dynamic offset.
    fn push(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> u32 {
        self.demand += 1;
        if self.used >= self.slots {
            self.grow(device, queue, self.demand.max(self.used + 1));
        }
        let offset = self.used * self.slot;
        let at = offset as usize;
        self.staged[at..at + bytes.len()].copy_from_slice(bytes);
        self.used += 1;
        offset as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_camera_puts_the_unit_square_over_the_viewport() {
        // `y` down in world space, `y` up in clip space: the flip lives in the
        // projection so that a texture coordinate can double as a position.
        // This is the convention `preview.rs`'s quad and its winding depend on.
        let clip = Camera::screen().view_proj();
        let corner = |x: f32, y: f32| {
            let v = clip * glam::Vec4::new(x, y, 0.0, 1.0);
            (v.x, v.y)
        };
        assert_eq!(corner(0.0, 0.0), (-1.0, 1.0), "world origin is top-left");
        assert_eq!(corner(1.0, 1.0), (1.0, -1.0), "world (1,1) is bottom-right");
    }

    #[test]
    fn the_screen_camera_puts_z_into_the_screen() {
        // Larger `z` must be *further away*, matching the `y`-down flip and
        // `Ortho`'s own convention. Getting this backwards draws every layered
        // 2D element in reverse order, which looks like a z-fighting bug
        // rather than a projection one.
        let clip = Camera::screen().view_proj();
        let depth = |z: f32| {
            let v = clip * glam::Vec4::new(0.5, 0.5, z, 1.0);
            v.z / v.w
        };
        assert!((depth(-1.0) - 0.0).abs() < 1e-6, "{}", depth(-1.0));
        assert!((depth(1.0) - 1.0).abs() < 1e-6, "{}", depth(1.0));
        assert!(depth(-0.5) < depth(0.5), "nearer must compare less");
    }

    #[test]
    fn the_projection_puts_depth_in_zero_to_one() {
        // `wgpu` and D3D9 clip to `0..1`; OpenGL clips to `-1..1`. glam offers
        // both, one `_gl` suffix apart, and picking the wrong one halves the
        // usable depth range and breaks every depth comparison in a way that
        // still draws a picture.
        let camera = Camera::perspective(Vec3::ZERO, Mat4::IDENTITY, 90.0, 1.0, 1.0, 100.0);
        let depth_of = |z: f32| {
            let v = camera.view_proj() * glam::Vec4::new(0.0, 0.0, z, 1.0);
            v.z / v.w
        };
        // Right-handed view space looks down -z, so the near plane is at -1.
        assert!((depth_of(-1.0) - 0.0).abs() < 1e-5, "{}", depth_of(-1.0));
        assert!(
            (depth_of(-100.0) - 1.0).abs() < 1e-5,
            "{}",
            depth_of(-100.0)
        );
    }

    #[test]
    fn a_horizontal_field_of_view_is_converted_the_way_valve_converts_it() {
        // `PerspectiveX` takes the horizontal fov and every Valve entry point
        // is in those terms; glam wants the vertical one. At an aspect of 1
        // they are equal, which is the check that the conversion is not
        // applied backwards.
        let square = Camera::perspective(Vec3::ZERO, Mat4::IDENTITY, 90.0, 1.0, 1.0, 100.0);
        let expected = directx::perspective(90f32.to_radians(), 1.0, 1.0, 100.0);
        assert!(square.projection.abs_diff_eq(expected, 1e-5));

        // At a wide aspect the vertical fov must be *smaller* than the
        // horizontal one, not larger.
        let wide = Camera::perspective(Vec3::ZERO, Mat4::IDENTITY, 90.0, 16.0 / 9.0, 1.0, 100.0);
        // A larger y scale in the projection means a narrower vertical fov.
        assert!(wide.projection.y_axis.y > square.projection.y_axis.y);
    }

    #[test]
    fn state_overrides_only_touch_what_they_name() {
        let base = RenderState::default();

        assert_eq!(StateOverride::default().apply(base), base);

        let no_cull = StateOverride {
            cull: Some(false),
            ..Default::default()
        }
        .apply(base);
        assert!(!no_cull.cull);
        assert_eq!(no_cull.depth_test, base.depth_test);
        assert_eq!(no_cull.blend, base.blend);

        // `OverrideDepthEnable( true, false, false )`: the `$ignorez` case.
        let ignore_z = StateOverride {
            depth_test: Some(false),
            depth_write: Some(false),
            ..Default::default()
        }
        .apply(base);
        assert!(!ignore_z.depth_test);
        assert!(!ignore_z.depth_write);
        assert!(ignore_z.cull, "culling was not named, so it is untouched");
    }
}
