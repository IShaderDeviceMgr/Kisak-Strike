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

    /// The adapter cannot sample BC (S3TC/DXT) textures.
    ///
    /// Reported before `request_device` so the message names the real problem
    /// rather than "device creation failed". Essentially every texture Valve
    /// ships is DXT1 or DXT5, so there is no useful fallback short of a CPU
    /// decompressor — see `rustdocs/MATERIALS.md`.
    #[error("the selected GPU adapter ({adapter}) cannot sample compressed (BC/DXT) textures, which all Source content uses")]
    NoBlockCompression { adapter: String },

    /// The adapter refused the requested features/limits.
    #[error("could not open the GPU device: {0}")]
    DeviceCreation(#[from] wgpu::RequestDeviceError),
}

/// Anything that can go wrong reading a `.vtf`.
///
/// Replaces the `Warning(...)`-then-`return false` pattern of
/// `CVTFTexture::Unserialize` (`vtf/vtf.cpp:1046`), which told the caller only
/// that *something* was wrong and printed the reason somewhere else entirely.
#[derive(Debug, thiserror::Error)]
pub enum VtfError {
    #[error("not a VTF file")]
    BadSignature,

    /// A `.360.vtf` or `.ps3.vtf`. Both are byte-swapped and tiled, and both
    /// platforms are permanently out of scope (`PORTING.md`, "Supported
    /// platforms").
    #[error("this is a {platform} texture, which is not supported")]
    ConsoleFormat { platform: &'static str },

    #[error("unsupported VTF version {major}.{minor}")]
    UnsupportedVersion { major: u32, minor: u32 },

    #[error("truncated VTF: needs {needed} bytes, file is {actual}")]
    Truncated { needed: usize, actual: usize },

    /// A format value with no counterpart here — a depth, console or
    /// runtime-compression format. See `image_format::unsupported_name`.
    #[error("VTF uses image format {name} ({raw}), which cannot appear in a PC texture")]
    UnsupportedFormat { raw: i32, name: &'static str },

    /// The header contradicts itself.
    #[error("malformed VTF: {0}")]
    Invalid(&'static str),

    /// A 7.3+ file whose resource dictionary has no image entry.
    #[error("VTF contains no image data")]
    NoImageData,
}

/// Anything that can go wrong reading a `.vmt`.
///
/// Replaces the `Warning( "CMaterial::PrecacheVars: error loading vmt file for
/// %s" )` of `materialsystem/cmaterial.cpp:2326`, which reported every one of
/// these the same way and left the caller with a material bound to the
/// wireframe shader.
#[derive(Debug, thiserror::Error)]
pub enum VmtError {
    /// The `.vmt` could not be read out of the game's content. Transparent for
    /// the same reason as [`TextureError::Read`]: the path is already in there.
    #[error(transparent)]
    Read(#[from] crate::filesystem::VfsError),

    /// The document has no outermost block, so it names no shader.
    #[error("{name}: contains no shader block")]
    NoShader { name: String },

    /// A `patch` block with no `include` key. The original tries to load the
    /// empty string and reports the failed read instead.
    #[error("{name}: is a patch with no $include")]
    PatchWithoutInclude { name: String },

    /// The document names a shader this port does not have. Valve's wording,
    /// from `CMaterial::InitializeShader` (`cmaterial.cpp:1613`), minus its
    /// "using wireframe instead" — the substitute here is the error material.
    #[error("{name}: uses unknown shader \"{shader}\"")]
    UnknownShader { name: String, shader: String },
}

/// Anything that can go wrong turning a name into a texture on the GPU.
#[derive(Debug, thiserror::Error)]
pub enum TextureError {
    /// The `.vtf` could not be read out of the game's content.
    ///
    /// Transparent because every `VfsError` already names the path it happened
    /// on, so wrapping it in "materials/x.vtf: ..." would print the path twice.
    #[error(transparent)]
    Read(#[from] crate::filesystem::VfsError),

    #[error("{name}: {source}")]
    Vtf {
        name: String,
        #[source]
        source: VtfError,
    },

    /// A format we can name but cannot put on the GPU — see
    /// `ImageFormat::gpu_format` for the five cases and why each one is
    /// deliberate.
    #[error("{name}: image format {format} cannot be uploaded")]
    UnsupportedFormat { name: String, format: &'static str },

    /// A block-compressed texture whose base level is not a whole number of
    /// 4x4 blocks. WebGPU requires it; D3D9 did not.
    #[error("{name}: {width}x{height} is not a whole number of {format} blocks")]
    NotBlockAligned {
        name: String,
        format: &'static str,
        width: u32,
        height: u32,
    },

    /// Bigger than the device's `max_texture_dimension_2d`.
    #[error("{name}: {width}x{height} exceeds the device limit of {limit}")]
    TooLarge {
        name: String,
        width: u32,
        height: u32,
        limit: u32,
    },
}
