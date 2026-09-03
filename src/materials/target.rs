//! What a pass draws into: the depth buffer, and offscreen render targets.
//!
//! Between them these are the `RenderTargetStackElement_t` of
//! `materialsystem/cmatrendercontext.h:177` — up to four colour targets, an
//! optional depth texture, and a viewport — minus the stack, which
//! [`RenderContext`](super::context::RenderContext) handles differently and
//! documents there.
//!
//! # Multiple render targets are not implemented
//!
//! `MAX_RENDER_TARGETS` is 4 and `SetRenderTargetEx( nIndex, ... )` binds them
//! individually, but the only things in the tree that bind more than one are
//! the lighting-preview G-buffer path (`MATERIAL_VAR2_USE_GBUFFER0`, dead code
//! behind an `#if 0` at `lightmappedgeneric_dx9_helper.cpp:677`) and CS:GO's
//! `character_ssao`. Neither is in scope, so a target here has one colour
//! attachment. `wgpu` takes a slice of them, so adding the rest is widening a
//! field rather than reshaping anything.

use std::sync::Arc;

use super::pipeline::TargetFormat;
use super::texture::{SamplerKey, Texture};

/// The depth-stencil format everything renders against.
///
/// 24-bit depth plus 8 bits of stencil, which is D3D9's `D3DFMT_D24S8` and what
/// `ShaderDeviceInfo_t` asked for. Three reasons it is this and not
/// `Depth32Float`:
///
/// - **The stencil is load-bearing for Portal 2.** Portal surfaces are drawn
///   with stencil masking, and `stdshaders/BufferClearObeyStencil_dx9.cpp`
///   exists to clear around them. Choosing the format once avoids invalidating
///   every cached pipeline later, since it is part of
///   [`PipelineKey`](super::pipeline::PipelineKey).
/// - **The depth bias is already in these units.**
///   [`DepthBias::Decal`](super::pipeline::DepthBias) converts Valve's
///   `mat_depthbias_decal` of -0.0000038 into -64 by multiplying by 2^24. For a
///   float depth format `wgpu`'s bias constant is scaled by the exponent of the
///   maximum depth in the primitive instead, and the decal offset would be
///   quietly wrong rather than absent.
/// - It is a mandatory format in WebGPU core, so it costs nothing from the
///   single capability tier of `portdocs/MATERIALSYSTEM.md` §4.6.
///
/// Nothing uses the stencil yet: pipelines carry `wgpu::StencilState::default()`
/// (write mask zero, always-pass) and passes leave `stencil_ops` at `None`,
/// which `wgpu` reads as a read-only stencil aspect and which is compatible
/// with exactly that pipeline state.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

/// What the depth buffer is cleared to at the start of a frame.
///
/// The far plane. Source's depth test is the conventional direction —
/// `SHADER_DEPTHFUNC_NEAREROREQUAL` is `LessEqual`, `bReverseDepth` being a
/// debug convar and `ReverseDepthOnX360` a console — so "nothing drawn yet" is
/// 1.0, and `ClearBuffers` clears to it (`shaderapidx8.cpp`'s `D3DCLEAR_ZBUFFER`
/// with `Z = 1.0f`).
pub const CLEAR_DEPTH: f32 = 1.0;

/// A depth-stencil attachment sized to whatever it is drawn alongside.
///
/// Not a [`Texture`]: nothing samples it. Shadow mapping is the one thing that
/// would (`RenderTargetStackElement_t::m_pDepthTexture` exists for exactly
/// that), and it is not ported — when it is, this grows a `TEXTURE_BINDING`
/// usage and a view, not a second type.
#[derive(Debug)]
pub struct DepthBuffer {
    // Kept because dropping it would take the view with it.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

#[allow(dead_code)]
impl DepthBuffer {
    /// Allocates a depth buffer. Both dimensions must be non-zero, which is the
    /// caller's job to check — a minimized window reports zero, and
    /// `wgpu` rejects a zero-sized texture just as it rejects a zero-sized
    /// surface configuration.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> DepthBuffer {
        assert!(width > 0 && height > 0, "depth buffer {width}x{height}");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        DepthBuffer {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            texture,
            width,
            height,
        }
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Reallocates if the size changed, and reports whether it did.
    ///
    /// A depth attachment must match its colour attachment's dimensions
    /// exactly, so this is called from wherever the colour target is resized —
    /// for the back buffer, [`Renderer::resize`](super::Renderer::resize).
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) -> bool {
        if (self.width, self.height) == (width, height) {
            return false;
        }
        *self = DepthBuffer::new(device, width, height);
        true
    }
}

/// An offscreen surface to draw into and then sample.
///
/// `CTexture::InitRenderTarget` plus the render-target half of
/// `RenderTargetStackElement_t`. Portal views, water reflections, the
/// framebuffer copy and every post-processing pass are one of these; §8 of
/// `portdocs/MATERIALSYSTEM.md` is why Portal 2 leans on them harder than most
/// games.
///
/// Deliberately *not* registered in [`TextureCache`](super::texture::TextureCache):
/// `CMaterialSystem::CreateNamedRenderTargetTextureEx` put render targets in the
/// same dictionary as `.vtf` files, so a material could name one as
/// `$basetexture` and a `.vtf` of the same name would silently shadow it. Here a
/// render target is a value the caller holds, and binding one to a material is
/// an explicit act.
// No caller in the binary yet -- portal views, water reflections and the
// post-processing chain are what allocate these, and none is ported. The GPU
// tests render through one, which is where the render-to-texture-and-sample
// path is checked.
#[allow(dead_code)]
pub struct RenderTarget {
    color: Arc<Texture>,
    depth: Option<DepthBuffer>,
    format: TargetFormat,
}

#[allow(dead_code)]
impl RenderTarget {
    /// Allocates a render target, with a depth buffer if `depth` is set.
    ///
    /// Whether to have depth is the caller's: a portal view needs it, a
    /// post-processing pass reading one full-screen texture and writing another
    /// does not, and allocating one anyway is a screen-sized texture per
    /// target for nothing.
    pub fn new(
        device: &wgpu::Device,
        name: &str,
        width: u32,
        height: u32,
        color_format: wgpu::TextureFormat,
        depth: bool,
    ) -> RenderTarget {
        let sampler = device.create_sampler(&SamplerKey::simple().descriptor());
        RenderTarget {
            color: Arc::new(Texture::render_target(
                device,
                name,
                width,
                height,
                color_format,
                sampler,
            )),
            depth: depth.then(|| DepthBuffer::new(device, width, height)),
            format: TargetFormat {
                color: color_format,
                depth: depth.then_some(DEPTH_FORMAT),
                samples: 1,
            },
        }
    }

    /// The target as something a material can bind as a texture.
    ///
    /// An `Arc` because that is what
    /// [`Material`](super::material::Material) holds its textures as, so
    /// binding a render target costs a refcount rather than a copy.
    pub fn texture(&self) -> &Arc<Texture> {
        &self.color
    }

    /// The pipeline state a draw into this target must have been compiled for.
    ///
    /// A `wgpu::RenderPipeline` declares its attachment formats, so drawing
    /// with one built for a different target is a validation error rather than
    /// a slow path. Pass this to
    /// [`Material::pipeline_key`](super::material::Material::pipeline_key).
    pub fn format(&self) -> TargetFormat {
        self.format
    }

    pub fn size(&self) -> (u32, u32) {
        (self.color.width, self.color.height)
    }

    pub(super) fn view(&self) -> &wgpu::TextureView {
        self.color.view()
    }

    /// The colour attachment itself, for `copy_texture_to_buffer`.
    ///
    /// `ReadPixels` and `CopyRenderTargetToTexture` both need the texture
    /// rather than a view; today its only caller is the readback in the GPU
    /// tests.
    pub(super) fn color_texture(&self) -> &wgpu::Texture {
        self.color.texture()
    }

    pub(super) fn depth_view(&self) -> Option<&wgpu::TextureView> {
        self.depth.as_ref().map(DepthBuffer::view)
    }
}
