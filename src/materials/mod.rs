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
//! Stages 1 and 2 of `portdocs/MATERIALSYSTEM.md` §9.
//!
//! - [`Renderer`] brings up the GPU and owns the frame boundary.
//! - [`Vtf`] reads `.vtf` files, [`ImageFormat`] says what each pixel format
//!   becomes on the GPU, and [`TextureCache`] turns a texture name into a
//!   [`Texture`] — or into the error checkerboard, which is the same thing as
//!   far as a caller is concerned.
//! - [`TextureBlit`] draws one over the frame. That is a *verification* path,
//!   not the beginning of a renderer; see its docs.
//!
//! Stage 3 (`.vmt` materials, the bind-group layout, the WGSL prelude) and
//! stage 4 (meshes and the render context) are not started, so nothing here
//! reads a `.vmt` or a real shader.

pub mod blit;
pub mod error;
pub mod image_format;
pub mod renderer;
pub mod texture;
pub mod vtf;

// Re-exported because something outside this module names them. Everything
// else is reachable at its own path (`materials::vtf::Vtf`,
// `materials::image_format::ImageFormat`, ...) and is re-exported when it
// acquires a caller, so that this list stays a statement about what the rest of
// the engine actually uses.
pub use blit::TextureBlit;
pub use error::RendererError;
pub use image_format::ColorSpace;
pub use renderer::{Renderer, RendererOptions, CLEAR_COLOR};
pub use texture::TextureCache;
