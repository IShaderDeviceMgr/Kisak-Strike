//! The material system: GPU device, textures, materials, shaders, meshes.
//!
//! Replaces `materialsystem/` — and, because the shader-API tower below it is
//! deleted rather than ported, also `materialsystem/shaderapidx9/`,
//! `materialsystem/glmgr/`, `materialsystem/ps3gcm/`,
//! `materialsystem/shaderapiempty/` and `togl/`. `wgpu` is used directly from
//! inside this module; there is no device abstraction, no `IShaderAPI`, and no
//! second backend to swap in. `wgpu` already *is* the backend abstraction.
//!
//! Design doc: `portdocs/MATERIALSYSTEM.md` (named after the original module).
//! API reference: `rustdocs/MATERIALS.md` (named after this one).
//!
//! # Status
//!
//! Stages 1 to 5 of `portdocs/MATERIALSYSTEM.md` §9.
//!
//! - [`Renderer`] brings up the GPU and owns the frame boundary.
//! - [`Vtf`](vtf::Vtf) reads `.vtf` files,
//!   [`ImageFormat`](image_format::ImageFormat) says what each pixel format
//!   becomes on the GPU, and [`TextureCache`](texture::TextureCache) turns a
//!   texture name into a
//!   [`Texture`](texture::Texture) — or into the error checkerboard, which is
//!   the same thing as far as a caller is concerned.
//! - [`Vmt`](vmt::Vmt) reads `.vmt` files, [`MaterialVar`](var::MaterialVar)
//!   holds what they say, and [`MaterialCache`] turns a material name into a
//!   [`Material`] — or into the error material, likewise.
//! - [`ShaderKind`](shader::ShaderKind) is the shader set, three deep so far:
//!   `UnlitGeneric`, `LightmappedGeneric` and `VertexLitGeneric`, written in
//!   WGSL against the constant ABI in [`uniforms`] and compiled through the
//!   cache in [`pipeline`].
//! - [`mesh`] is geometry: [`SimpleVertex`](mesh::SimpleVertex) and the
//!   [`VertexLayout`](mesh::VertexLayout) a shader declares,
//!   [`VertexBuffer`](mesh::VertexBuffer)/[`IndexBuffer`](mesh::IndexBuffer)
//!   for what outlives a frame, and
//!   [`DynamicBuffers`](mesh::DynamicBuffers) for what does not.
//! - [`RenderContext`] opens passes. A [`Pass`](context::Pass) has a target, a
//!   [`Camera`](context::Camera) and a depth buffer, and everything Valve
//!   stacked is a parameter of one or the other — see its docs.
//! - [`target`] is what a pass draws into:
//!   [`DepthBuffer`](target::DepthBuffer) and
//!   [`RenderTarget`](target::RenderTarget).
//! - [`MaterialPreview`] draws one material on a cube. That is a
//!   *verification* path, not a scene graph; see its docs.
//!
//! - Group 3 is *where a shader's lighting comes from*, and there are two
//!   answers ([`LightingBinding`](shader::LightingBinding)): a lightmap atlas
//!   page for a brush surface, and
//!   [`ModelLighting`](uniforms::ModelLighting) — an ambient cube plus up to
//!   four local lights — for a model. A shader that reads neither declares no
//!   group 3 at all.
//!
//! - [`lightmap`] packs a map's baked light samples into atlas pages:
//!   [`ImagePacker`](lightmap::ImagePacker) is `CImagePacker`,
//!   [`LightmapAtlas`](lightmap::LightmapAtlas) is the CPU half and
//!   [`LightmapPages`](lightmap::LightmapPages) the GPU one, and a page is
//!   bound per batch with
//!   [`Pass::bind_lightmap_page`](context::Pass::bind_lightmap_page) rather
//!   than living in a material.
//!
//! Stage 6 is `VertexLitGeneric`, which is done. The rest of §7.8's shader set
//! (`Phong`, `WorldVertexTransition`, water, portals, sprites) and stages 7-8
//! (paint maps, GPU morph) are not started.

pub mod context;
pub mod error;
pub mod image_format;
pub mod lightmap;
pub mod material;
pub mod mesh;
pub mod pipeline;
pub mod preview;
pub mod renderer;
pub mod shader;
pub mod target;
pub mod texture;
pub mod ui;
pub mod uniforms;
pub mod var;
pub mod vmt;
pub mod vtf;

// Re-exported because something outside this module names them. Everything
// else is reachable at its own path (`materials::vtf::Vtf`,
// `materials::image_format::ImageFormat`, ...) and is re-exported when it
// acquires a caller, so that this list stays a statement about what the rest of
// the engine actually uses.
pub use context::RenderContext;
pub use error::RendererError;
pub use image_format::ColorSpace;
pub use material::{Material, MaterialCache};
pub use preview::MaterialPreview;
pub use renderer::{Renderer, RendererOptions, CLEAR_COLOR};
pub use ui::UiRenderer;
