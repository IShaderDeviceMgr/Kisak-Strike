//! Error type for the material system.
//!
//! Replaces the `bool`-return convention of `IShaderDeviceMgr::SetMode`,
//! `IShaderDevice::CreateVertexShader` and friends, plus the `Error()`/
//! `Sys_Error()` calls those paths made on failure — `materialsystem/
//! cmaterialsystem.cpp:712` `CreateShaderAPI` returned `false` and left the
//! caller to guess what went wrong. See ../../PORTING.md's "What idiomatic
//! means concretely".

/// Anything that can go wrong bringing the renderer up.
///
/// Every variant here is a *startup* failure. The per-frame surface conditions
/// that `wgpu` reports (outdated, occluded, lost) are not errors — see
/// [`Renderer::begin_frame`], which handles them internally.
///
/// [`Renderer::begin_frame`]: super::Renderer::begin_frame
#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    /// No GPU adapter could satisfy the request.
    #[error("no usable GPU adapter: {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),

    /// `-adapter <n>` named an adapter that does not exist.
    #[error("-adapter {requested} does not exist; {available} adapter(s) present")]
    NoSuchAdapter { requested: usize, available: usize },

    /// The window could not be turned into a presentable surface.
    #[error("could not create a render surface for the game window: {0}")]
    SurfaceCreation(#[from] wgpu::CreateSurfaceError),

    /// The surface exists but the chosen adapter cannot present to it. Reported
    /// as an empty format list from `Surface::get_capabilities`.
    #[error("the selected GPU adapter ({adapter}) cannot present to the game window")]
    SurfaceUnsupported { adapter: String },

    /// The adapter refused the requested features/limits.
    #[error("could not open the GPU device: {0}")]
    DeviceCreation(#[from] wgpu::RequestDeviceError),
}
