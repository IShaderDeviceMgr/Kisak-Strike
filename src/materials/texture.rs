//! Textures on the GPU, and the name-to-texture dictionary in front of them.
//!
//! Replaces `materialsystem/ctexture.cpp` (`CTexture`) and
//! `materialsystem/texturemanager.cpp` (`CTextureManager`) — between them about
//! 7,600 lines, of which this is a small fraction because most of what they do
//! is gone:
//!
//! | In the original | Here |
//! |---|---|
//! | `Download`/`ReleaseTextureHandles` around D3D9 device loss | nothing; `wgpu` has no lost-device dance |
//! | `ITextureRegenerator` for procedural textures | ordinary functions that build bytes, e.g. [`Texture::error`] |
//! | Texture exclusion lists, streaming, `TEXTURE_GROUP_*` budgets | not ported; measure before rebuilding any of it |
//! | `m_pTextureHandles[frame]`, one D3D texture per animation frame | kept — see [`Texture::from_vtf`] |
//! | `SetFilterState`/`SetWrapState` calling into `IShaderAPI` per frame | one immutable [`wgpu::Sampler`], built once |
//!
//! The last row is the real structural change. Valve's sampler state was
//! per-*texture* but applied by mutating global device state at draw time
//! (`ctexture.cpp:2580`); `wgpu` bakes it into a sampler object bound with the
//! texture, so it is decided at load and never touched again.

use std::collections::HashMap;
use std::sync::Arc;

use super::error::TextureError;
use super::image_format::ColorSpace;
use super::vtf::{TextureFlags, Vtf};
use crate::filesystem::Vfs;

/// Where `.vtf` files live, relative to the search paths.
/// `ctexture.cpp:3235` builds `"materials/%s" TEXTURE_FNAME_EXTENSION`, and
/// `TEXTURE_FNAME_EXTENSION` is `PLATFORM_EXT ".vtf"` with `PLATFORM_EXT` empty
/// on PC.
const TEXTURE_DIR: &str = "materials/";
const TEXTURE_EXT: &str = ".vtf";

/// Side length of the error checkerboard. `ERROR_TEXTURE_SIZE`,
/// `texturemanager.cpp:50`.
const ERROR_TEXTURE_SIZE: u32 = 32;

/// The checker mask. `CCheckerboardTexture` is constructed with 4
/// (`texturemanager.cpp:646`) and uses it as a *mask*, not a size:
/// `(x & 4) ^ (y & 4)` alternates every 4 texels.
const ERROR_CHECKER_MASK: u32 = 4;

/// A texture living on the GPU, with the sampler it is meant to be read
/// through.
///
/// One `Texture` is one animation frame of one `.vtf`, matching
/// `CTexture::m_pTextureHandles[iFrame]`. Cube faces and volume slices are
/// layers *inside* it; animation frames are not.
// `name`, `depth` and `frame` describe the texture rather than being needed to
// draw it, so they have no reader until materials can report what they loaded.
// The `texture` handle is kept because dropping it would take the view with it.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Texture {
    /// The `.vtf`'s name without the `materials/` prefix or the extension,
    /// lowercased — the same string the original used as a dictionary key.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Slices for a volume texture, 1 otherwise.
    pub depth: u32,
    /// Mip levels actually uploaded.
    pub mip_count: u32,
    pub format: wgpu::TextureFormat,
    /// `Cube` for a cubemap, `D3` for a volume texture, `D2` otherwise. A
    /// bind-group layout has to declare it, so it is recorded rather than
    /// re-derived from the flags.
    pub view_dimension: wgpu::TextureViewDimension,
    /// Which `.vtf` frame this is.
    pub frame: u32,
    /// Whether the file claims an alpha channel worth reading —
    /// `TEXTUREFLAGS_ONEBITALPHA | TEXTUREFLAGS_EIGHTBITALPHA`.
    ///
    /// Not derived from the pixel format: a DXT5 texture whose alpha is all
    /// 255 is opaque, and only the flags in the header say so. See
    /// [`Texture::is_translucent`].
    translucent: bool,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl Texture {
    /// The view to bind. Already the right dimension: `Cube` for a cubemap,
    /// `D3` for a volume texture, `D2` otherwise.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The sampler the `.vtf`'s flags asked for. See [`sampler_key`].
    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
    }

    /// The texture itself, for the copies that need one rather than a view —
    /// `ReadPixels`, `CopyRenderTargetToTexture`.
    pub(super) fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Whether the texture carries transparency. `CTexture::IsTranslucent`
    /// (`ctexture.cpp:3094`).
    ///
    /// Read by the shadow phase: whether a material blends depends on whether
    /// its base texture has an alpha channel at all
    /// (`CBaseShader::TextureIsTranslucent`), which is why this is on the
    /// texture rather than on the `.vtf` that is dropped after loading.
    pub fn is_translucent(&self) -> bool {
        self.translucent
    }

    /// Uploads one frame of a parsed `.vtf`.
    ///
    /// `color_space` is the caller's, not the file's: in the original the
    /// *shader* decided, per sampler, with `IShaderShadow::EnableSRGBRead`.
    /// [`Vtf::is_normal_map`] is what a caller should consult when it has no
    /// better information; nothing is inferred here, because silently
    /// overriding the caller is worse than obeying a wrong one.
    ///
    /// Every mip level in the file is uploaded, and the texture is created with
    /// exactly that many — a `.vtf` with a partial chain gets a partial chain,
    /// not a full one with uninitialized tail levels the way
    /// `CVTFTexture::Unserialize` allocates one.
    pub fn from_vtf(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &str,
        vtf: &Vtf,
        frame: u32,
        color_space: ColorSpace,
        sampler: wgpu::Sampler,
    ) -> Result<Texture, TextureError> {
        let format =
            vtf.format
                .gpu_format(color_space)
                .ok_or_else(|| TextureError::UnsupportedFormat {
                    name: name.to_owned(),
                    format: vtf.format.name(),
                })?;

        let (block_w, block_h) = format.block_dimensions();
        if vtf.width % block_w != 0 || vtf.height % block_h != 0 {
            // D3D9 tolerated this and padded internally; WebGPU requires the
            // base level to be a whole number of blocks. Nothing in shipped
            // Portal 2 content trips it — every `.vtf` is a power of two — but
            // saying so beats a validation-layer panic.
            return Err(TextureError::NotBlockAligned {
                name: name.to_owned(),
                format: vtf.format.name(),
                width: vtf.width,
                height: vtf.height,
            });
        }

        let limit = device.limits().max_texture_dimension_2d;
        if vtf.width > limit || vtf.height > limit {
            return Err(TextureError::TooLarge {
                name: name.to_owned(),
                width: vtf.width,
                height: vtf.height,
                limit,
            });
        }

        // Cube faces and volume slices are layers of one texture; animation
        // frames are separate textures, as they were in the original.
        let (dimension, view_dimension, layers) = if vtf.is_cubemap() {
            (
                wgpu::TextureDimension::D2,
                wgpu::TextureViewDimension::Cube,
                6,
            )
        } else if vtf.is_volume() {
            (
                wgpu::TextureDimension::D3,
                wgpu::TextureViewDimension::D3,
                1,
            )
        } else {
            (
                wgpu::TextureDimension::D2,
                wgpu::TextureViewDimension::D2,
                1,
            )
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(name),
            size: wgpu::Extent3d {
                width: vtf.width,
                height: vtf.height,
                depth_or_array_layers: if vtf.is_volume() { vtf.depth } else { layers },
            },
            mip_level_count: vtf.mip_count,
            sample_count: 1,
            dimension,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let block_size = format
            .block_copy_size(None)
            .expect("a colour format has one aspect");

        for level in 0..vtf.mip_count {
            let (width, height, depth) = vtf.mip_dimensions(level);

            // The *physical* size of the level, not the logical one. The tail
            // of a compressed chain is levels smaller than a 4x4 block — a
            // 64x64 DXT1 texture ends 2x2, 1x1 — and WebGPU requires a copy
            // into a compressed texture to be a whole number of blocks wide
            // and high, so those levels are written as the 4x4 block they
            // physically occupy. That is also exactly how many bytes the file
            // holds for them: `ImageFormat::mem_required` rounds the same way,
            // because `GetMemRequired` did.
            let copy_width = width.next_multiple_of(block_w);
            let copy_height = height.next_multiple_of(block_h);
            let bytes_per_row = (copy_width / block_w) * block_size;
            let rows = copy_height / block_h;

            for face in 0..layers {
                // In range by construction: `mip_count`, `frame_count` and
                // `face_count` all come from the same header `Vtf::parse`
                // validated the file against.
                let Some(src) = vtf.mip_data(frame, face, level) else {
                    continue;
                };
                let texels = (width * height * depth) as usize;
                let bytes = vtf.format.to_gpu_bytes(src, texels);

                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: level,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: face,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(bytes_per_row),
                        rows_per_image: Some(rows),
                    },
                    wgpu::Extent3d {
                        width: copy_width,
                        height: copy_height,
                        depth_or_array_layers: depth,
                    },
                );
            }
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(name),
            dimension: Some(view_dimension),
            ..Default::default()
        });

        Ok(Texture {
            name: name.to_owned(),
            width: vtf.width,
            height: vtf.height,
            depth: vtf.depth,
            mip_count: vtf.mip_count,
            format,
            view_dimension,
            frame,
            translucent: vtf.flags.contains(TextureFlags::ONE_BIT_ALPHA)
                || vtf.flags.contains(TextureFlags::EIGHT_BIT_ALPHA),
            texture,
            view,
            sampler,
        })
    }

    /// The magenta-and-black checkerboard a failed load resolves to.
    ///
    /// `CTextureManager::Init` (`texturemanager.cpp:632`) builds this as a
    /// 32x32 procedural texture with a 4-texel checker of `(0,0,0,128)` and
    /// `(255,0,255,255)`. Same size, same colours, same checker: it is the
    /// thing every Source player recognizes instantly as "that texture is
    /// missing", and reproducing it exactly is the whole point.
    ///
    /// Built here rather than loaded, so it exists before the filesystem does
    /// and cannot itself fail.
    pub fn error(device: &wgpu::Device, queue: &wgpu::Queue, sampler: wgpu::Sampler) -> Texture {
        let size = ERROR_TEXTURE_SIZE;
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                if (x & ERROR_CHECKER_MASK) ^ (y & ERROR_CHECKER_MASK) != 0 {
                    pixels.extend_from_slice(&[0, 0, 0, 128]);
                } else {
                    pixels.extend_from_slice(&[255, 0, 255, 255]);
                }
            }
        }
        Texture::procedural(device, queue, "error", size, &pixels, sampler)
    }

    /// The 1x1 opaque white texture an *undefined* texture parameter binds.
    ///
    /// `TEXTURE_WHITE`, `CTextureManager::Init` (`texturemanager.cpp:659`) —
    /// one white texel, no mips. Distinct from [`Texture::error`] and the
    /// distinction matters: a `.vmt` with no `$basetexture` is not broken, it
    /// is a material whose colour comes entirely from `$color` and the vertex
    /// stream, and every shader binds white for it rather than complaining
    /// (`vertexlitgeneric_dx9_helper.cpp:1255`). Valve's own `___flat.vmt` is
    /// one. Drawing a checkerboard there would report a failure that did not
    /// happen.
    ///
    /// The rest of the standard family — `black`, `grey`, `greyalphazero`, the
    /// normalization cubemap — is not here: each is reached only from a shader
    /// feature that is not ported (an envmap with no base texture binds black,
    /// for instance), and each is four lines when the shader that wants it
    /// lands.
    pub fn white(device: &wgpu::Device, queue: &wgpu::Queue, sampler: wgpu::Sampler) -> Texture {
        Texture::procedural(device, queue, "white", 1, &[255, 255, 255, 255], sampler)
    }

    /// A texture the GPU writes by drawing into it.
    ///
    /// `CTexture::InitRenderTarget` (`ctexture.cpp:1130`), reduced to what it
    /// is: a texture with `RENDER_ATTACHMENT` on top of `TEXTURE_BINDING`.
    /// Valve's version also carried a `RenderTargetSizeMode_t` (sizes derived
    /// from the frame buffer at various fractions), the auto-mipmap flag and
    /// the depth-buffer-sharing rules, none of which are decisions this makes
    /// for the caller.
    ///
    /// Not translucent, whatever it ends up containing:
    /// [`is_translucent`](Texture::is_translucent) reports what a `.vtf`'s
    /// flags claimed, and a render target has no flags. A material that blends
    /// one has to say so itself.
    pub(super) fn render_target(
        device: &wgpu::Device,
        name: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        sampler: wgpu::Sampler,
    ) -> Texture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(name),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // `COPY_SRC` so a test — and later `CopyRenderTargetToTexture` and
            // `ReadPixels` — can get the result back off the GPU.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Texture {
            name: name.to_string(),
            width,
            height,
            depth: 1,
            mip_count: 1,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
            frame: 0,
            translucent: false,
            texture,
            view,
            sampler,
        }
    }

    /// A square RGBA texture built in code rather than loaded.
    ///
    /// The survivor of `ITextureRegenerator` (`texturemanager.cpp:178`), which
    /// was an interface with a `RegenerateTextureBits` callback so that D3D9
    /// could ask for the pixels again after a lost device. There is no lost
    /// device, so there is no callback — just the bytes.
    fn procedural(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &'static str,
        size: u32,
        pixels: &[u8],
        sampler: wgpu::Sampler,
    ) -> Texture {
        debug_assert_eq!(pixels.len(), (size * size * 4) as usize);
        // `TEXTUREFLAGS_SRGB` in the original, and these are colours a human
        // picked, so the hardware should decode them.
        Texture::from_pixels(
            device,
            queue,
            name,
            size,
            size,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            pixels,
            sampler,
        )
    }

    /// A texture of any format built from bytes the CPU already has.
    ///
    /// The general form of [`procedural`](Texture::procedural), split out for
    /// the lightmap atlas, whose pages are neither square nor 8-bit and are
    /// assembled a surface at a time before any of this is reached. Both are
    /// what is left of `ITextureRegenerator` (`texturemanager.cpp:178`) —
    /// an interface with a `RegenerateTextureBits` callback, so that D3D9
    /// could ask for the pixels again after a lost device. There is no lost
    /// device, so there is no callback: just the bytes.
    ///
    /// One mip level, because everything that reaches this has exactly one.
    /// `pixels` must be tightly packed at `format`'s block size; the caller is
    /// assumed to have laid it out, so there is no padding step here.
    // Eight parameters, one over clippy's bar: every one is a distinct
    // property of the texture being built and bundling them into a struct
    // would only move the same list somewhere less local.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_pixels(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        pixels: &[u8],
        sampler: wgpu::Sampler,
    ) -> Texture {
        // `block_copy_size`, not `target_pixel_byte_cost`: the latter is
        // `wgpu`'s render-target memory estimate and is twice the texel size
        // for the 8-bit formats.
        let bytes_per_texel = format
            .block_copy_size(None)
            .expect("a CPU-built texture is uncompressed");
        debug_assert_eq!(pixels.len(), (width * height * bytes_per_texel) as usize);

        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(name),
            size: extent,
            // `TEXTUREFLAGS_NOMIP`.
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * bytes_per_texel),
                rows_per_image: Some(height),
            },
            extent,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some(name),
            ..Default::default()
        });

        Texture {
            name: name.to_owned(),
            width,
            height,
            depth: 1,
            mip_count: 1,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
            frame: 0,
            // Nothing built here is created with an alpha flag
            // (`texturemanager.cpp:651`), so a material falling back to one
            // stays opaque rather than turning translucent.
            translucent: false,
            texture,
            view,
            sampler,
        }
    }
}

/// The sampler state a `.vtf`'s flags ask for.
///
/// Deliberately a small hashable value rather than a `wgpu::Sampler`: the whole
/// game shares a handful of distinct sampler states across thousands of
/// textures, so [`TextureCache`] builds each one once and hands out clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SamplerKey {
    address_u: AddressMode,
    address_v: AddressMode,
    address_w: AddressMode,
    point_sample: bool,
    /// Whether the mip filter interpolates between levels — Valve's
    /// `LINEAR_MIPMAP_LINEAR` (trilinear) against `LINEAR_MIPMAP_NEAREST`.
    trilinear: bool,
    anisotropic: bool,
    /// Whether mip levels exist to filter between at all.
    mipmapped: bool,
}

/// The subset of `ShaderTexWrapMode_t` that survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AddressMode {
    Repeat,
    Clamp,
}

impl From<AddressMode> for wgpu::AddressMode {
    fn from(mode: AddressMode) -> Self {
        match mode {
            AddressMode::Repeat => wgpu::AddressMode::Repeat,
            AddressMode::Clamp => wgpu::AddressMode::ClampToEdge,
        }
    }
}

/// Reads sampler state out of a `.vtf`'s flags.
///
/// Ports `CTexture::SetWrapState` (`ctexture.cpp:2580`) and
/// `CTexture::SetFilterState` (`ctexture.cpp:2626`), minus the parts that
/// consulted runtime configuration that does not exist yet:
/// `g_config.m_nForceAnisotropicLevel` (a video-options convar) and
/// `HardwareConfig()->MaximumAnisotropicLevel()` (one of the ~50 caps queries
/// `portdocs/MATERIALSYSTEM.md` §4.6 deletes).
///
/// **Deliberate divergence:** `TEXTUREFLAGS_BORDER` asked for
/// `SHADER_TEXWRAPMODE_BORDER` on all three axes. `wgpu`'s
/// `AddressMode::ClampToBorder` needs the `ADDRESS_MODE_CLAMP_TO_BORDER`
/// feature, which is outside the single capability tier, so the flag clamps to
/// edge instead. The visible difference is confined to the outermost texel of a
/// texture sampled outside `[0,1]`.
pub fn sampler_key(flags: TextureFlags, mip_count: u32) -> SamplerKey {
    let border = flags.contains(TextureFlags::BORDER);
    let clamp = |flag: TextureFlags| {
        if border || flags.contains(flag) {
            AddressMode::Clamp
        } else {
            AddressMode::Repeat
        }
    };

    SamplerKey {
        address_u: clamp(TextureFlags::CLAMP_S),
        address_v: clamp(TextureFlags::CLAMP_T),
        address_w: clamp(TextureFlags::CLAMP_U),
        point_sample: flags.contains(TextureFlags::POINT_SAMPLE),
        trilinear: flags.contains(TextureFlags::TRILINEAR),
        anisotropic: flags.contains(TextureFlags::ANISOTROPIC),
        // `SetFilterState` keys off `TEXTUREFLAGS_NOMIP`; having only one level
        // in the file means the same thing and is the stronger statement.
        mipmapped: mip_count > 1 && !flags.contains(TextureFlags::NO_MIP),
    }
}

impl SamplerKey {
    /// The sampler state for a texture that is drawn once, at full size, with
    /// no wrapping — the error checkerboard, the debug blit, and render
    /// targets, which `CTexture::InitRenderTarget` gives
    /// `TEXTUREFLAGS_CLAMPS | TEXTUREFLAGS_CLAMPT | TEXTUREFLAGS_NOMIP`.
    pub(super) fn simple() -> SamplerKey {
        SamplerKey {
            address_u: AddressMode::Clamp,
            address_v: AddressMode::Clamp,
            address_w: AddressMode::Clamp,
            point_sample: false,
            trilinear: false,
            anisotropic: false,
            mipmapped: false,
        }
    }

    /// `pub(super)` so the end-to-end tests in `blit.rs` can build the same
    /// sampler the cache would.
    pub(super) fn descriptor(&self) -> wgpu::SamplerDescriptor<'static> {
        // Anisotropic filtering requires all three filters to be linear, both
        // in `wgpu` (which panics otherwise) and in the hardware.
        let anisotropic = self.anisotropic && !self.point_sample && self.mipmapped;
        let filter = if self.point_sample && !anisotropic {
            wgpu::FilterMode::Nearest
        } else {
            wgpu::FilterMode::Linear
        };
        let mipmap_filter = if !self.mipmapped {
            // With one level there is nothing to interpolate; `SetFilterState`
            // returns plain linear for `TEXTUREFLAGS_NOMIP` at
            // `ctexture.cpp:2640` for the same reason.
            wgpu::MipmapFilterMode::Nearest
        } else if anisotropic || self.trilinear {
            wgpu::MipmapFilterMode::Linear
        } else {
            // `SHADER_TEXFILTERMODE_LINEAR_MIPMAP_NEAREST`: bilinear within a
            // level, no blending between levels. Point-sampled textures land
            // here too, which is what `SetFilterState` does for them.
            wgpu::MipmapFilterMode::Nearest
        };

        wgpu::SamplerDescriptor {
            label: None,
            address_mode_u: self.address_u.into(),
            address_mode_v: self.address_v.into(),
            address_mode_w: self.address_w.into(),
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter,
            // 16x is the ceiling every GPU this port targets supports, and is
            // what `mat_forceaniso 16` asked for.
            anisotropy_clamp: if anisotropic { 16 } else { 1 },
            ..Default::default()
        }
    }
}

/// The name-to-texture dictionary. `CTextureManager`.
///
/// Holds `wgpu::Device` and `wgpu::Queue` by value — both are cheap handles to
/// shared state, so this is a clone of a reference, not of a device. That keeps
/// the cache independent of [`Renderer`](super::Renderer) rather than living
/// inside it, which is what `portdocs/MATERIALSYSTEM.md` §5.3 asks for: the
/// queued render context is deleted, but the reason it existed — build work off
/// the main thread, submit once — is still the architecture to leave room for.
///
/// There is no refcounting and no eviction. `CTextureManager` had both, plus
/// exclusion lists and streaming; none of it is worth rebuilding before there is
/// a map to measure it against.
pub struct TextureCache {
    device: wgpu::Device,
    queue: wgpu::Queue,
    textures: HashMap<(String, ColorSpace), Arc<Texture>>,
    samplers: HashMap<SamplerKey, wgpu::Sampler>,
    error: Arc<Texture>,
    white: Arc<Texture>,
}

impl TextureCache {
    /// Builds the cache and, with it, the error checkerboard.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> TextureCache {
        let mut samplers = HashMap::new();
        let sampler = sampler_for(device, &mut samplers, SamplerKey::simple());
        let error = Arc::new(Texture::error(device, queue, sampler.clone()));
        let white = Arc::new(Texture::white(device, queue, sampler));

        TextureCache {
            device: device.clone(),
            queue: queue.clone(),
            textures: HashMap::new(),
            samplers,
            error,
            white,
        }
    }

    /// The checkerboard. `CTextureManager::ErrorTexture`.
    pub fn error_texture(&self) -> Arc<Texture> {
        Arc::clone(&self.error)
    }

    /// The 1x1 white texture. `TEXTURE_WHITE`, via `BindStandardTexture`.
    ///
    /// What an *undefined* texture parameter binds — which is not the same as
    /// a failed one. See [`Texture::white`].
    pub fn white_texture(&self) -> Arc<Texture> {
        Arc::clone(&self.white)
    }

    /// Loads `materials/<name>.vtf`, or returns the error checkerboard.
    ///
    /// This is the interface the rest of the engine should use.
    /// `CTextureManager::FindOrLoadTexture` behaves the same way — a missing or
    /// broken texture is a checkerboard and a warning, never a failed map load —
    /// and every later stage depends on it, because a material referencing one
    /// bad texture must still draw.
    ///
    /// The failure is logged once per name; a repeat lookup gets the cached
    /// checkerboard silently.
    pub fn load(&mut self, vfs: &Vfs, name: &str, color_space: ColorSpace) -> Arc<Texture> {
        let key = (normalize_name(name), color_space);
        if let Some(texture) = self.textures.get(&key) {
            return Arc::clone(texture);
        }

        let texture = match self.build(vfs, &key.0, color_space) {
            Ok(texture) => Arc::new(texture),
            Err(err) => {
                eprintln!("source-engine: materials: {err}");
                Arc::clone(&self.error)
            }
        };
        self.textures.insert(key, Arc::clone(&texture));
        texture
    }

    /// Reads, parses and uploads one `.vtf`. `name` is already normalized.
    fn build(
        &mut self,
        vfs: &Vfs,
        name: &str,
        color_space: ColorSpace,
    ) -> Result<Texture, TextureError> {
        let path = format!("{TEXTURE_DIR}{name}{TEXTURE_EXT}");

        let bytes = vfs.read(&path).map_err(TextureError::Read)?;
        let vtf = Vtf::parse(bytes).map_err(|source| TextureError::Vtf {
            name: path.clone(),
            source,
        })?;

        let sampler = sampler_for(
            &self.device,
            &mut self.samplers,
            sampler_key(vtf.flags, vtf.mip_count),
        );
        Texture::from_vtf(
            &self.device,
            &self.queue,
            name,
            &vtf,
            0,
            color_space,
            sampler,
        )
    }
}

/// Interns a sampler for `key`, building it the first time.
fn sampler_for(
    device: &wgpu::Device,
    samplers: &mut HashMap<SamplerKey, wgpu::Sampler>,
    key: SamplerKey,
) -> wgpu::Sampler {
    samplers
        .entry(key)
        .or_insert_with(|| device.create_sampler(&key.descriptor()))
        .clone()
}

/// The dictionary key for a texture name.
///
/// `CTexture::Init` lowercases and forward-slashes the name before storing it,
/// so `Metal\Wall` and `metal/wall` are one texture rather than two. The
/// filesystem is case-insensitive on its own (see `rustdocs/FILESYSTEM.md`), so
/// this exists for the *cache*, not for the lookup.
fn normalize_name(name: &str) -> String {
    name.trim_matches(['/', '\\'])
        .chars()
        .map(|c| match c {
            '\\' => '/',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing here touches a GPU: `SamplerKey` is pure policy read out of the
    /// file's flags, which is exactly the part worth pinning down. The upload
    /// path is verified by running it — see `rustdocs/MATERIALS.md`.
    fn key(flags: u32, mip_count: u32) -> SamplerKey {
        sampler_key(TextureFlags(flags), mip_count)
    }

    #[test]
    fn names_normalize_to_one_dictionary_key() {
        assert_eq!(normalize_name("Metal\\Wall01"), "metal/wall01");
        assert_eq!(normalize_name("/metal/wall01"), "metal/wall01");
        assert_eq!(normalize_name("metal/wall01"), "metal/wall01");
    }

    #[test]
    fn wrap_mode_follows_the_clamp_flags_per_axis() {
        let repeat = key(0, 1);
        assert_eq!(repeat.address_u, AddressMode::Repeat);
        assert_eq!(repeat.address_v, AddressMode::Repeat);

        let clamped = key(TextureFlags::CLAMP_S.0, 1);
        assert_eq!(clamped.address_u, AddressMode::Clamp);
        assert_eq!(clamped.address_v, AddressMode::Repeat, "T is independent");

        // `TEXTUREFLAGS_BORDER` clamps every axis regardless of the others —
        // `SetWrapState` returns early on it (`ctexture.cpp:2583`).
        let border = key(TextureFlags::BORDER.0, 1);
        assert_eq!(border.address_u, AddressMode::Clamp);
        assert_eq!(border.address_v, AddressMode::Clamp);
        assert_eq!(border.address_w, AddressMode::Clamp);
    }

    #[test]
    fn point_sampled_textures_are_not_filtered() {
        let d = key(TextureFlags::POINT_SAMPLE.0, 8).descriptor();
        assert_eq!(d.mag_filter, wgpu::FilterMode::Nearest);
        assert_eq!(d.min_filter, wgpu::FilterMode::Nearest);
        assert_eq!(d.mipmap_filter, wgpu::MipmapFilterMode::Nearest);
        assert_eq!(d.anisotropy_clamp, 1);
    }

    #[test]
    fn trilinear_blends_between_mip_levels_and_bilinear_does_not() {
        let bilinear = key(0, 8).descriptor();
        assert_eq!(bilinear.min_filter, wgpu::FilterMode::Linear);
        assert_eq!(
            bilinear.mipmap_filter,
            wgpu::MipmapFilterMode::Nearest,
            "LINEAR_MIPMAP_NEAREST"
        );

        let trilinear = key(TextureFlags::TRILINEAR.0, 8).descriptor();
        assert_eq!(
            trilinear.mipmap_filter,
            wgpu::MipmapFilterMode::Linear,
            "LINEAR_MIPMAP_LINEAR"
        );
    }

    #[test]
    fn anisotropy_needs_mip_levels_and_forces_linear_filtering() {
        let aniso = key(TextureFlags::ANISOTROPIC.0, 8).descriptor();
        assert_eq!(aniso.anisotropy_clamp, 16);
        assert_eq!(aniso.min_filter, wgpu::FilterMode::Linear);
        assert_eq!(aniso.mipmap_filter, wgpu::MipmapFilterMode::Linear);

        // wgpu panics if anisotropy is asked for without all three filters
        // linear, so the flag has to lose to POINT_SAMPLE and to having no
        // mips rather than the other way round.
        let single = key(TextureFlags::ANISOTROPIC.0, 1).descriptor();
        assert_eq!(single.anisotropy_clamp, 1);
        let point = key(
            TextureFlags::ANISOTROPIC.0 | TextureFlags::POINT_SAMPLE.0,
            8,
        )
        .descriptor();
        assert_eq!(point.anisotropy_clamp, 1);
        assert_eq!(point.min_filter, wgpu::FilterMode::Nearest);
    }

    #[test]
    fn a_texture_with_no_mips_never_filters_between_levels() {
        // Either statement of it — the flag, or the file simply having one
        // level — has to reach the same sampler.
        assert!(!key(TextureFlags::NO_MIP.0, 8).mipmapped);
        assert!(!key(0, 1).mipmapped);
        assert!(key(0, 8).mipmapped);
    }

    #[test]
    fn identical_flags_produce_one_sampler_key() {
        // The whole point of interning: two textures with the same policy must
        // hash and compare equal.
        assert_eq!(
            key(TextureFlags::CLAMP_S.0 | TextureFlags::CLAMP_T.0, 4),
            key(TextureFlags::CLAMP_S.0 | TextureFlags::CLAMP_T.0, 4)
        );
        assert_ne!(key(TextureFlags::CLAMP_S.0, 4), key(0, 4));
    }
}
