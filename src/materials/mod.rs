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
//! Stage 1 of `portdocs/MATERIALSYSTEM.md` §9 — [`Renderer`] brings up the GPU
//! and clears a window. Stages 2-4 (textures, materials and the WGSL prelude,
//! meshes and the render context) are not started, and nothing here yet reads
//! a `.vtf`, a `.vmt` or a shader.

pub mod error;
pub mod renderer;

pub use error::RendererError;
pub use renderer::{Renderer, RendererOptions, CLEAR_COLOR};
