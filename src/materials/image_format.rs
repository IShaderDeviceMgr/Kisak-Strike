//! Valve's `ImageFormat`, the size arithmetic that goes with it, and what each
//! format becomes on the GPU.
//!
//! Three original files land here:
//!
//! - `public/bitmap/imageformat_declarations.h` — the enum itself. **Its
//!   numbering is on-disk data**: every `.vtf` stores the format as a raw
//!   `int`, so the discriminants below are fixed by the file format and are not
//!   ours to renumber (`PORTING.md`, "Format is fixed regardless of crate
//!   choice").
//! - `bitmap/imageformat.cpp` — `GetMemRequired`, `GetNumMipMapLevels` and
//!   `GetMipMapLevelDimensions`. This is the arithmetic that decides where each
//!   mip level starts inside a `.vtf`, so it is ported exactly rather than
//!   reimplemented from first principles.
//! - `materialsystem/shaderapidx9/colorformatdx8.cpp` — the
//!   `ImageFormat` → API format table, and the knowledge of which formats the
//!   API had no equivalent for and had to be emulated.
//!   `portdocs/MATERIALSYSTEM.md` §6 names it as one of the four files in that
//!   directory worth reading before deleting it; this module is the reading.
//!
//! # What is deliberately not here
//!
//! Valve's enum has 68 values. Twenty-nine of them are depth-stencil formats
//! (`D24S8`, `INTZ`, …), X360/PS3 tiled or byte-swapped variants (`LINEAR_*`,
//! `LE_*`) and runtime-compression markers (`DXT1_RUNTIME`). None can appear in
//! a PC `.vtf`, and the console ones are permanently out of scope
//! (`PORTING.md`, "Supported platforms"), so they get no variants —
//! [`unsupported_name`] names them for error messages and nothing else.

use std::borrow::Cow;

/// How the values in a texture are to be interpreted when sampled.
///
/// In the original this is not a property of the texture at all: the *shader*
/// decided, per sampler, by calling `IShaderShadow::EnableSRGBRead`
/// (`materialsystem/stdshaders/BaseVSShader.cpp:815` and ~100 other sites).
/// `wgpu` has no such switch — sRGB is baked into the `TextureFormat` — so the
/// decision moves to load time and the caller has to make it. See
/// `rustdocs/MATERIALS.md` for the rule.
// `Linear` has no caller until stage 3: the only thing loading textures today
// is the `-vtf` debug switch, and that is always looking at colour. Normal
// maps, masks and DUDV maps are what ask for it, and they arrive with `.vmt`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpace {
    /// Values are used as-is. Normal maps, masks, DUDV maps, HDR data.
    Linear,
    /// Values are sRGB-encoded and the hardware decodes them on read.
    /// Everything that is a colour a human picked.
    Srgb,
}

/// A pixel format a Valve texture can be stored in.
///
/// Only the formats a PC `.vtf` can actually contain — see the module docs.
/// The discriminants are the on-disk values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ImageFormat {
    Rgba8888 = 0,
    Abgr8888 = 1,
    Rgb888 = 2,
    Bgr888 = 3,
    Rgb565 = 4,
    /// Intensity: one channel, broadcast to RGB on read, alpha 1.
    I8 = 5,
    /// Intensity + alpha.
    Ia88 = 6,
    /// Paletted. The palette is not in the file; see [`ImageFormat::gpu_format`].
    P8 = 7,
    A8 = 8,
    /// RGB where pure blue (0,0,255) means transparent.
    Rgb888Bluescreen = 9,
    Bgr888Bluescreen = 10,
    Argb8888 = 11,
    Bgra8888 = 12,
    Dxt1 = 13,
    Dxt3 = 14,
    Dxt5 = 15,
    Bgrx8888 = 16,
    Bgr565 = 17,
    Bgrx5551 = 18,
    Bgra4444 = 19,
    Dxt1OneBitAlpha = 20,
    Bgra5551 = 21,
    /// Signed two-channel, a DUDV/normal map. `D3DFMT_V8U8`.
    Uv88 = 22,
    Uvwq8888 = 23,
    Rgba16161616F = 24,
    Rgba16161616 = 25,
    Uvlx8888 = 26,
    R32F = 27,
    Rgb323232F = 28,
    Rgba32323232F = 29,
    Rg1616F = 30,
    Rg3232F = 31,
    Rgbx8888 = 32,
    /// "Dummy format which takes no video memory" — a render-target marker.
    Null = 33,
    /// Two-channel block compression. BC5. Valve's comment calls this the
    /// "one-surface" format and `ATI1N` the "two-surface" one; both that comment
    /// and the name column of `g_ImageFormatInfo` have the two swapped. The
    /// block sizes in `GetMemRequired` (8 for ATI1N, 16 for ATI2N) are right,
    /// and are what the format actually is.
    Ati2n = 34,
    /// Single-channel block compression. BC4.
    Ati1n = 35,
    Rgba1010102 = 36,
    Bgra1010102 = 37,
    R16F = 38,
}

impl ImageFormat {
    /// The format with this on-disk value, if it is one we can name.
    pub fn from_raw(raw: i32) -> Option<Self> {
        use ImageFormat::*;
        Some(match raw {
            0 => Rgba8888,
            1 => Abgr8888,
            2 => Rgb888,
            3 => Bgr888,
            4 => Rgb565,
            5 => I8,
            6 => Ia88,
            7 => P8,
            8 => A8,
            9 => Rgb888Bluescreen,
            10 => Bgr888Bluescreen,
            11 => Argb8888,
            12 => Bgra8888,
            13 => Dxt1,
            14 => Dxt3,
            15 => Dxt5,
            16 => Bgrx8888,
            17 => Bgr565,
            18 => Bgrx5551,
            19 => Bgra4444,
            20 => Dxt1OneBitAlpha,
            21 => Bgra5551,
            22 => Uv88,
            23 => Uvwq8888,
            24 => Rgba16161616F,
            25 => Rgba16161616,
            26 => Uvlx8888,
            27 => R32F,
            28 => Rgb323232F,
            29 => Rgba32323232F,
            30 => Rg1616F,
            31 => Rg3232F,
            32 => Rgbx8888,
            33 => Null,
            34 => Ati2n,
            35 => Ati1n,
            36 => Rgba1010102,
            37 => Bgra1010102,
            38 => R16F,
            _ => return None,
        })
    }

    /// Valve's name for the format, as it appears in engine logs.
    pub fn name(self) -> &'static str {
        use ImageFormat::*;
        match self {
            Rgba8888 => "RGBA8888",
            Abgr8888 => "ABGR8888",
            Rgb888 => "RGB888",
            Bgr888 => "BGR888",
            Rgb565 => "RGB565",
            I8 => "I8",
            Ia88 => "IA88",
            P8 => "P8",
            A8 => "A8",
            Rgb888Bluescreen => "RGB888_BLUESCREEN",
            Bgr888Bluescreen => "BGR888_BLUESCREEN",
            Argb8888 => "ARGB8888",
            Bgra8888 => "BGRA8888",
            Dxt1 => "DXT1",
            Dxt3 => "DXT3",
            Dxt5 => "DXT5",
            Bgrx8888 => "BGRX8888",
            Bgr565 => "BGR565",
            Bgrx5551 => "BGRX5551",
            Bgra4444 => "BGRA4444",
            Dxt1OneBitAlpha => "DXT1_ONEBITALPHA",
            Bgra5551 => "BGRA5551",
            Uv88 => "UV88",
            Uvwq8888 => "UVWQ8888",
            Rgba16161616F => "RGBA16161616F",
            Rgba16161616 => "RGBA16161616",
            Uvlx8888 => "UVLX8888",
            R32F => "R32F",
            Rgb323232F => "RGB323232F",
            Rgba32323232F => "RGBA32323232F",
            Rg1616F => "RG1616F",
            Rg3232F => "RG3232F",
            Rgbx8888 => "RGBX8888",
            Null => "NULL",
            Ati2n => "ATI2N",
            Ati1n => "ATI1N",
            Rgba1010102 => "RGBA1010102",
            Bgra1010102 => "BGRA1010102",
            R16F => "R16F",
        }
    }

    /// Whether this is a 4x4 block-compressed format.
    ///
    /// `ImageFormatInfo_t::m_bIsCompressed`.
    pub fn is_compressed(self) -> bool {
        use ImageFormat::*;
        matches!(
            self,
            Dxt1 | Dxt3 | Dxt5 | Dxt1OneBitAlpha | Ati1n | Ati2n
        )
    }

    /// Bytes per 4x4 block for a compressed format, bytes per texel otherwise.
    ///
    /// `ImageFormatInfo_t::m_nNumBytes` for the uncompressed formats and the
    /// `switch` inside `GetMemRequired` (`bitmap/imageformat.cpp:162`) for the
    /// compressed ones.
    ///
    /// **Deliberate divergence:** that `switch` has no case for
    /// `IMAGE_FORMAT_DXT1_ONEBITALPHA`, so `GetMemRequired` returns 0 for it and
    /// any `.vtf` written in that format is unreadable by the original engine.
    /// Here it is 8, the same as `DXT1`, which is what it self-evidently is.
    /// Reproducing a size of zero would not be fidelity to a behavior, it would
    /// be a copy of a defect.
    pub fn bytes_per_block(self) -> usize {
        use ImageFormat::*;
        match self {
            Dxt1 | Dxt1OneBitAlpha | Ati1n => 8,
            Dxt3 | Dxt5 | Ati2n => 16,

            I8 | P8 | A8 => 1,
            Rgb565 | Ia88 | Bgr565 | Bgrx5551 | Bgra4444 | Bgra5551 | Uv88 | R16F => 2,
            Rgb888 | Bgr888 | Rgb888Bluescreen | Bgr888Bluescreen => 3,
            Rgba8888 | Abgr8888 | Argb8888 | Bgra8888 | Bgrx8888 | Uvwq8888 | Uvlx8888 | R32F
            | Rg1616F | Rgbx8888 | Null | Rgba1010102 | Bgra1010102 => 4,
            Rgba16161616F | Rgba16161616 | Rg3232F => 8,
            Rgb323232F => 12,
            Rgba32323232F => 16,
        }
    }

    /// Bytes one mip level of these dimensions occupies **in a `.vtf` file**.
    ///
    /// A direct port of `ImageLoader::GetMemRequired( w, h, d, fmt, false )`
    /// (`bitmap/imageformat.cpp:132`), including its treatment of compressed
    /// levels smaller than one block: a dimension below 4 is rounded *up* to 4
    /// and the level still costs a whole block.
    ///
    /// Note the truncating `>> 2` rather than a rounding-up division. That is
    /// Valve's, it is what the bytes in the file are laid out by, and it differs
    /// from a ceiling only for dimensions that are not multiples of 4 — which
    /// `GetMemRequired` asserts never happens and [`crate::materials::Vtf`]
    /// rejects up front. There is a second, *different* size routine in the
    /// original (`ImageLoader::SizeInBytes( fmt, w, h )`,
    /// `public/bitmap/imageformat.h:621`) which rounds up and charges 8 bytes a
    /// block for everything except DXT5 — a bug its own comment admits. It does
    /// not participate in file layout and is not ported.
    pub fn mem_required(self, width: u32, height: u32, depth: u32) -> usize {
        let depth = depth.max(1);

        if !self.is_compressed() {
            return width as usize * height as usize * depth as usize * self.bytes_per_block();
        }

        let width = if (1..4).contains(&width) { 4 } else { width };
        let height = if (1..4).contains(&height) { 4 } else { height };
        let depth = if (2..4).contains(&depth) { 4 } else { depth };

        let blocks = (width >> 2) as usize * (height >> 2) as usize * depth as usize;
        blocks * self.bytes_per_block()
    }

    /// The `wgpu` format these bytes are uploaded as, or `None` if we cannot
    /// upload them at all.
    ///
    /// Where the answer is not the identity, [`to_gpu_bytes`](Self::to_gpu_bytes)
    /// does the conversion. The uploaded format is *never* narrower than the
    /// source, so this never loses precision.
    ///
    /// `color_space` is ignored by every format that has no sRGB variant — the
    /// snorm, float, BC4/BC5 and 10:10:10:2 formats. That is not a silent
    /// failure to honour the request so much as a category error in making it:
    /// those formats never hold colour a human picked.
    ///
    /// `None` covers five cases, all deliberate:
    ///
    /// - `P8` — paletted, and the palette is not stored in the `.vtf`. It was
    ///   already unloadable in the original.
    /// - `Null` — a render-target marker that owns no pixels.
    /// - `Rgba16161616` — 16-bit unorm needs `wgpu`'s
    ///   `TEXTURE_FORMAT_16BIT_NORM` feature, which is outside the single
    ///   capability tier of `portdocs/MATERIALSYSTEM.md` §4.6. Its one real user
    ///   is HDR lightmaps, which are stage 5.
    /// - `Bgra1010102` — `wgpu` has `Rgb10a2Unorm` but no BGR ordering of it,
    ///   and expanding to `Rgba16Float` to fix channel order would cost 4x the
    ///   memory for a format no shipped `.vtf` uses.
    /// - `Uvlx8888` — two signed channels then two unsigned ones in a single
    ///   32-bit texel. No API has ever had a format for that; the original
    ///   emulated it and nothing in Portal 2 asks for it.
    pub fn gpu_format(self, color_space: ColorSpace) -> Option<wgpu::TextureFormat> {
        use wgpu::TextureFormat as Tf;
        use ColorSpace::Srgb;
        use ImageFormat::*;

        let srgb = color_space == Srgb;
        Some(match self {
            // Straight through.
            Bgra8888 | Bgrx8888 => srgb_pick(srgb, Tf::Bgra8UnormSrgb, Tf::Bgra8Unorm),
            Dxt1 | Dxt1OneBitAlpha => srgb_pick(srgb, Tf::Bc1RgbaUnormSrgb, Tf::Bc1RgbaUnorm),
            Dxt3 => srgb_pick(srgb, Tf::Bc2RgbaUnormSrgb, Tf::Bc2RgbaUnorm),
            Dxt5 => srgb_pick(srgb, Tf::Bc3RgbaUnormSrgb, Tf::Bc3RgbaUnorm),
            Ati1n => Tf::Bc4RUnorm,
            Ati2n => Tf::Bc5RgUnorm,
            Uv88 => Tf::Rg8Snorm,
            Uvwq8888 => Tf::Rgba8Snorm,
            Rgba16161616F => Tf::Rgba16Float,
            R16F => Tf::R16Float,
            R32F => Tf::R32Float,
            Rg1616F => Tf::Rg16Float,
            Rg3232F => Tf::Rg32Float,
            Rgba32323232F => Tf::Rgba32Float,
            Rgba1010102 => Tf::Rgb10a2Unorm,

            // Widened or reordered by `to_gpu_bytes`.
            Rgba8888 | Abgr8888 | Argb8888 | Rgbx8888 | Rgb888 | Bgr888 | Rgb888Bluescreen
            | Bgr888Bluescreen | Rgb565 | Bgr565 | Bgrx5551 | Bgra5551 | Bgra4444 | I8 | Ia88
            | A8 => srgb_pick(srgb, Tf::Rgba8UnormSrgb, Tf::Rgba8Unorm),
            Rgb323232F => Tf::Rgba32Float,

            P8 | Null | Rgba16161616 | Bgra1010102 | Uvlx8888 => return None,
        })
    }

    /// Rewrites one mip level into the layout [`gpu_format`](Self::gpu_format)
    /// expects.
    ///
    /// Borrows when the bytes already are what the GPU wants, which is the case
    /// for every block-compressed format and for `BGRA8888` — between them,
    /// essentially all shipped content. `texels` is width x height x depth of
    /// the level; it is ignored for compressed formats, which never convert.
    ///
    /// 5- and 6-bit channels are widened by bit replication (31 -> 255), which
    /// is what `RescaleBitNumber` (`bitmap/colorconversion.cpp:304`) does, and
    /// which its own comment notes is identical to rescaling through a float.
    ///
    /// # Panics
    ///
    /// If `src` is shorter than the level it claims to be. Callers inside this
    /// module take their slices from [`mem_required`](Self::mem_required), so
    /// the two agree by construction.
    pub fn to_gpu_bytes(self, src: &[u8], texels: usize) -> Cow<'_, [u8]> {
        use ImageFormat::*;

        if self.is_compressed() {
            return Cow::Borrowed(src);
        }

        assert!(
            src.len() >= texels * self.bytes_per_block(),
            "{}: {} texels need {} bytes, got {}",
            self.name(),
            texels,
            texels * self.bytes_per_block(),
            src.len()
        );

        // `Rgba8888` and `Bgra8888` are already exactly their GPU format.
        if matches!(self, Rgba8888 | Bgra8888) {
            return Cow::Borrowed(&src[..texels * 4]);
        }

        let mut out: Vec<u8> = Vec::new();
        match self {
            // 32-bit reorderings and alpha fixups.
            Abgr8888 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 4].chunks_exact(4) {
                    out.extend_from_slice(&[p[3], p[2], p[1], p[0]]);
                }
            }
            Argb8888 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 4].chunks_exact(4) {
                    out.extend_from_slice(&[p[1], p[2], p[3], p[0]]);
                }
            }
            // The X byte is undefined in the file. `D3DFMT_X8R8G8B8` read it as
            // opaque; leaving whatever vtex happened to write there would make
            // alpha-blended draws sample garbage.
            Bgrx8888 | Rgbx8888 => {
                out.extend_from_slice(&src[..texels * 4]);
                for p in out.chunks_exact_mut(4) {
                    p[3] = 0xFF;
                }
            }

            // 24-bit widened to 32.
            Rgb888 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 3].chunks_exact(3) {
                    out.extend_from_slice(&[p[0], p[1], p[2], 0xFF]);
                }
            }
            Bgr888 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 3].chunks_exact(3) {
                    out.extend_from_slice(&[p[2], p[1], p[0], 0xFF]);
                }
            }
            // Pure blue is the transparency key. `vtex` produced these from
            // artwork with a blue-screen background; the alpha channel is
            // derived, not stored.
            Rgb888Bluescreen => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 3].chunks_exact(3) {
                    let clear = p[0] == 0 && p[1] == 0 && p[2] == 0xFF;
                    out.extend_from_slice(&[p[0], p[1], p[2], if clear { 0 } else { 0xFF }]);
                }
            }
            Bgr888Bluescreen => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 3].chunks_exact(3) {
                    let clear = p[2] == 0 && p[1] == 0 && p[0] == 0xFF;
                    out.extend_from_slice(&[p[2], p[1], p[0], if clear { 0 } else { 0xFF }]);
                }
            }

            // 16-bit packed. Bitfield order is least-significant-first, which is
            // what "change the order of names to change the order of the output"
            // means in `public/bitmap/imageformat.h:110`.
            Bgr565 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 2].chunks_exact(2) {
                    let v = u16::from_le_bytes([p[0], p[1]]);
                    out.extend_from_slice(&[
                        expand5((v >> 11) & 0x1F),
                        expand6((v >> 5) & 0x3F),
                        expand5(v & 0x1F),
                        0xFF,
                    ]);
                }
            }
            Rgb565 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 2].chunks_exact(2) {
                    let v = u16::from_le_bytes([p[0], p[1]]);
                    out.extend_from_slice(&[
                        expand5(v & 0x1F),
                        expand6((v >> 5) & 0x3F),
                        expand5((v >> 11) & 0x1F),
                        0xFF,
                    ]);
                }
            }
            Bgra5551 | Bgrx5551 => {
                let opaque = self == Bgrx5551;
                out.reserve_exact(texels * 4);
                for p in src[..texels * 2].chunks_exact(2) {
                    let v = u16::from_le_bytes([p[0], p[1]]);
                    let a = if opaque || (v & 0x8000) != 0 { 0xFF } else { 0 };
                    out.extend_from_slice(&[
                        expand5((v >> 10) & 0x1F),
                        expand5((v >> 5) & 0x1F),
                        expand5(v & 0x1F),
                        a,
                    ]);
                }
            }
            Bgra4444 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 2].chunks_exact(2) {
                    let v = u16::from_le_bytes([p[0], p[1]]);
                    out.extend_from_slice(&[
                        expand4((v >> 8) & 0xF),
                        expand4((v >> 4) & 0xF),
                        expand4(v & 0xF),
                        expand4((v >> 12) & 0xF),
                    ]);
                }
            }

            // Single- and dual-channel formats whose read semantics are a
            // swizzle the API used to perform. `D3DFMT_L8` broadcast luminance
            // to RGB and `D3DFMT_A8` read as (0,0,0,A); `wgpu` has neither, and
            // no component swizzle on a texture view. Expanding here costs
            // memory but keeps the swizzle out of every shader that samples one
            // — where getting it wrong would be silent.
            I8 => {
                out.reserve_exact(texels * 4);
                for &i in &src[..texels] {
                    out.extend_from_slice(&[i, i, i, 0xFF]);
                }
            }
            Ia88 => {
                out.reserve_exact(texels * 4);
                for p in src[..texels * 2].chunks_exact(2) {
                    out.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
                }
            }
            A8 => {
                out.reserve_exact(texels * 4);
                for &a in &src[..texels] {
                    out.extend_from_slice(&[0, 0, 0, a]);
                }
            }

            // The one float widening. `RGB323232F` has no D3D9 equivalent
            // either — `public/bitmap/imageformat_declarations.h:60` says so in
            // a comment.
            Rgb323232F => {
                out.reserve_exact(texels * 16);
                for p in src[..texels * 12].chunks_exact(12) {
                    out.extend_from_slice(&p[0..12]);
                    out.extend_from_slice(&1.0f32.to_le_bytes());
                }
            }

            // Already handled, or has no GPU format at all.
            Rgba8888 | Bgra8888 | Dxt1 | Dxt1OneBitAlpha | Dxt3 | Dxt5 | Ati1n | Ati2n | Uv88
            | Uvwq8888 | Rgba16161616F | R16F | R32F | Rg1616F | Rg3232F | Rgba32323232F
            | Rgba1010102 | P8 | Null | Rgba16161616 | Bgra1010102 | Uvlx8888 => {
                return Cow::Borrowed(src)
            }
        }
        Cow::Owned(out)
    }
}

/// The name of a format value this port has no variant for, for error messages.
///
/// Covers exactly the range [`ImageFormat::from_raw`] rejects: the
/// depth-stencil formats, the X360/PS3 `LINEAR_*` and `LE_*` variants, the
/// runtime-compression markers and `INTZ`. Kept as a flat string table rather
/// than as enum variants because nothing will ever *do* anything with one — see
/// the module docs.
pub fn unsupported_name(raw: i32) -> Option<&'static str> {
    const NAMES: [&str; 29] = [
        "D16",
        "D15S1",
        "D32",
        "D24S8",
        "LINEAR_D24S8",
        "D24X8",
        "D24X4S4",
        "D24FS8",
        "D16_SHADOW",
        "D24X8_SHADOW",
        "LINEAR_BGRX8888",
        "LINEAR_RGBA8888",
        "LINEAR_ABGR8888",
        "LINEAR_ARGB8888",
        "LINEAR_BGRA8888",
        "LINEAR_RGB888",
        "LINEAR_BGR888",
        "LINEAR_BGRX5551",
        "LINEAR_I8",
        "LINEAR_RGBA16161616",
        "LINEAR_A8",
        "LINEAR_DXT1",
        "LINEAR_DXT3",
        "LINEAR_DXT5",
        "LE_BGRX8888",
        "LE_BGRA8888",
        "DXT1_RUNTIME",
        "DXT5_RUNTIME",
        "INTZ",
    ];
    match raw {
        -1 => Some("UNKNOWN"),
        -2 => Some("DEFAULT"),
        39..=67 => NAMES.get((raw - 39) as usize).copied(),
        _ => None,
    }
}

/// Number of mip levels in a full chain down to 1x1x1.
///
/// `ImageLoader::GetNumMipMapLevels` (`bitmap/imageformat.cpp:322`).
pub fn full_mip_count(width: u32, height: u32, depth: u32) -> u32 {
    let (mut w, mut h, mut d) = (width, height, depth.max(1));
    if w < 1 || h < 1 {
        return 0;
    }
    let mut levels = 1;
    while w != 1 || h != 1 || d != 1 {
        w = (w >> 1).max(1);
        h = (h >> 1).max(1);
        d = (d >> 1).max(1);
        levels += 1;
    }
    levels
}

/// Dimensions of one mip level. `CVTFTexture::ComputeMipLevelDimensions`
/// (`vtf/vtf.cpp:1796`): a plain shift, floored at 1.
pub fn mip_dimensions(width: u32, height: u32, depth: u32, level: u32) -> (u32, u32, u32) {
    (
        (width >> level).max(1),
        (height >> level).max(1),
        (depth >> level).max(1),
    )
}

fn srgb_pick(srgb: bool, yes: wgpu::TextureFormat, no: wgpu::TextureFormat) -> wgpu::TextureFormat {
    if srgb {
        yes
    } else {
        no
    }
}

/// 4 -> 8 bits by replication: 0xF becomes 0xFF.
fn expand4(v: u16) -> u8 {
    let v = (v & 0xF) as u8;
    (v << 4) | v
}

/// 5 -> 8 bits by replication: 0x1F becomes 0xFF.
fn expand5(v: u16) -> u8 {
    let v = (v & 0x1F) as u8;
    (v << 3) | (v >> 2)
}

/// 6 -> 8 bits by replication: 0x3F becomes 0xFF.
fn expand6(v: u16) -> u8 {
    let v = (v & 0x3F) as u8;
    (v << 2) | (v >> 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values_match_the_on_disk_enum() {
        // The discriminants are file-format data, so spot-check the ones that
        // actually appear in shipped content against
        // `public/bitmap/imageformat_declarations.h`.
        assert_eq!(ImageFormat::from_raw(0), Some(ImageFormat::Rgba8888));
        assert_eq!(ImageFormat::from_raw(12), Some(ImageFormat::Bgra8888));
        assert_eq!(ImageFormat::from_raw(13), Some(ImageFormat::Dxt1));
        assert_eq!(ImageFormat::from_raw(15), Some(ImageFormat::Dxt5));
        assert_eq!(ImageFormat::from_raw(24), Some(ImageFormat::Rgba16161616F));
        assert_eq!(ImageFormat::from_raw(38), Some(ImageFormat::R16F));
        assert_eq!(ImageFormat::from_raw(39), None, "D16 is a depth format");
        assert_eq!(ImageFormat::from_raw(-1), None, "UNKNOWN");
        assert_eq!(unsupported_name(39), Some("D16"));
        assert_eq!(unsupported_name(67), Some("INTZ"));
        assert_eq!(unsupported_name(68), None, "past NUM_IMAGE_FORMATS");
    }

    #[test]
    fn compressed_sizes_match_get_mem_required() {
        // 8 bytes per 4x4 block for DXT1, 16 for DXT5.
        assert_eq!(ImageFormat::Dxt1.mem_required(4, 4, 1), 8);
        assert_eq!(ImageFormat::Dxt5.mem_required(4, 4, 1), 16);
        assert_eq!(ImageFormat::Dxt1.mem_required(256, 256, 1), 32_768);
        assert_eq!(ImageFormat::Dxt5.mem_required(256, 256, 1), 65_536);
        // ATI1N is 8 bytes a block and ATI2N is 16, despite the names in
        // `g_ImageFormatInfo` being swapped.
        assert_eq!(ImageFormat::Ati1n.mem_required(8, 8, 1), 32);
        assert_eq!(ImageFormat::Ati2n.mem_required(8, 8, 1), 64);
    }

    #[test]
    fn compressed_levels_below_one_block_still_cost_a_block() {
        // `GetMemRequired` rounds a dimension under 4 up to 4. This is what
        // makes the tail of a mip chain 8/8/8 rather than 8/2/0.
        for (w, h) in [(2, 2), (1, 1), (4, 1), (1, 4)] {
            assert_eq!(ImageFormat::Dxt1.mem_required(w, h, 1), 8, "{w}x{h}");
        }
        assert_eq!(ImageFormat::Dxt5.mem_required(1, 1, 1), 16);
    }

    #[test]
    fn uncompressed_sizes_are_texels_times_stride() {
        assert_eq!(ImageFormat::Bgra8888.mem_required(16, 8, 1), 512);
        assert_eq!(ImageFormat::Bgr888.mem_required(16, 8, 1), 384);
        assert_eq!(ImageFormat::I8.mem_required(16, 8, 1), 128);
        assert_eq!(ImageFormat::Rgba16161616F.mem_required(4, 4, 1), 128);
        // Volume textures multiply through by depth.
        assert_eq!(ImageFormat::Bgra8888.mem_required(4, 4, 4), 256);
        // A zero depth is treated as one, as `GetMemRequired` does.
        assert_eq!(ImageFormat::Bgra8888.mem_required(4, 4, 0), 64);
    }

    #[test]
    fn mip_chains_match_get_num_mip_map_levels() {
        assert_eq!(full_mip_count(1, 1, 1), 1);
        assert_eq!(full_mip_count(256, 256, 1), 9);
        // Non-square: the chain runs until *both* reach 1.
        assert_eq!(full_mip_count(256, 4, 1), 9);
        assert_eq!(full_mip_count(8, 8, 8), 4);
        assert_eq!(mip_dimensions(256, 4, 1, 3), (32, 1, 1));
        assert_eq!(mip_dimensions(256, 4, 1, 8), (1, 1, 1));
        assert_eq!(mip_dimensions(256, 4, 1, 31), (1, 1, 1), "no shift overflow");
    }

    #[test]
    fn a_full_dxt1_chain_sums_the_way_the_file_is_laid_out() {
        // 8x8 DXT1 is four levels: 8x8 is 4 blocks (32 bytes), 4x4 is one (8),
        // and 2x2 and 1x1 still cost a whole block each (8 + 8). The tail
        // costing three quarters of the chain is the thing to notice: it is
        // why the padding rule in `mem_required` cannot be dropped.
        let total: usize = (0..full_mip_count(8, 8, 1))
            .map(|m| {
                let (w, h, d) = mip_dimensions(8, 8, 1, m);
                ImageFormat::Dxt1.mem_required(w, h, d)
            })
            .sum();
        assert_eq!(total, 32 + 8 + 8 + 8);
    }

    #[test]
    fn srgb_only_applies_where_a_variant_exists() {
        use wgpu::TextureFormat as Tf;
        let (lin, srgb) = (ColorSpace::Linear, ColorSpace::Srgb);
        assert_eq!(ImageFormat::Dxt5.gpu_format(srgb), Some(Tf::Bc3RgbaUnormSrgb));
        assert_eq!(ImageFormat::Dxt5.gpu_format(lin), Some(Tf::Bc3RgbaUnorm));
        assert_eq!(
            ImageFormat::Bgr888.gpu_format(srgb),
            Some(Tf::Rgba8UnormSrgb),
            "widened, but still sRGB"
        );
        // No sRGB variant exists for these, and asking for one changes nothing.
        assert_eq!(ImageFormat::Uv88.gpu_format(srgb), Some(Tf::Rg8Snorm));
        assert_eq!(ImageFormat::Ati2n.gpu_format(srgb), Some(Tf::Bc5RgUnorm));
        assert_eq!(
            ImageFormat::Rgba16161616F.gpu_format(srgb),
            Some(Tf::Rgba16Float)
        );
    }

    #[test]
    fn formats_we_cannot_upload_say_so() {
        for fmt in [
            ImageFormat::P8,
            ImageFormat::Null,
            ImageFormat::Rgba16161616,
            ImageFormat::Bgra1010102,
            ImageFormat::Uvlx8888,
        ] {
            assert_eq!(fmt.gpu_format(ColorSpace::Linear), None, "{}", fmt.name());
        }
    }

    #[test]
    fn formats_the_gpu_takes_as_is_are_not_copied() {
        let src = vec![0xABu8; 64];
        for fmt in [ImageFormat::Dxt1, ImageFormat::Dxt5, ImageFormat::Bgra8888] {
            assert!(
                matches!(fmt.to_gpu_bytes(&src, 16), Cow::Borrowed(_)),
                "{} should not be copied",
                fmt.name()
            );
        }
    }

    #[test]
    fn channel_orders_come_out_as_rgba() {
        // One texel, distinct values per channel, so a swizzle mistake shows.
        let rgba = ImageFormat::Abgr8888.to_gpu_bytes(&[4, 3, 2, 1], 1);
        assert_eq!(&rgba[..], &[1, 2, 3, 4], "ABGR reversed");

        let rgba = ImageFormat::Argb8888.to_gpu_bytes(&[1, 2, 3, 4], 1);
        assert_eq!(&rgba[..], &[2, 3, 4, 1], "A leads, moves to the end");

        let rgba = ImageFormat::Bgr888.to_gpu_bytes(&[1, 2, 3], 1);
        assert_eq!(&rgba[..], &[3, 2, 1, 255]);

        let rgba = ImageFormat::Rgb888.to_gpu_bytes(&[1, 2, 3], 1);
        assert_eq!(&rgba[..], &[1, 2, 3, 255]);
    }

    #[test]
    fn the_undefined_x_byte_becomes_opaque() {
        // Whatever vtex left in the X byte must not reach the GPU as alpha.
        let rgba = ImageFormat::Bgrx8888.to_gpu_bytes(&[1, 2, 3, 0x7F], 1);
        assert_eq!(&rgba[..], &[1, 2, 3, 255]);
        let rgba = ImageFormat::Rgbx8888.to_gpu_bytes(&[1, 2, 3, 0x7F], 1);
        assert_eq!(&rgba[..], &[1, 2, 3, 255]);
    }

    /// One packed 16-bit texel, converted. Owned, because the `Cow` would
    /// otherwise borrow the `to_le_bytes` temporary.
    fn packed(format: ImageFormat, texel: u16) -> Vec<u8> {
        format.to_gpu_bytes(&texel.to_le_bytes(), 1).into_owned()
    }

    #[test]
    fn packed_16_bit_channels_widen_to_full_range() {
        // All bits set must reach 255 exactly, or white drifts grey.
        assert_eq!(packed(ImageFormat::Bgr565, 0xFFFF), [255, 255, 255, 255]);
        assert_eq!(packed(ImageFormat::Bgra4444, 0xFFFF), [255, 255, 255, 255]);
        assert_eq!(packed(ImageFormat::Bgra5551, 0xFFFF), [255, 255, 255, 255]);

        // Channel placement: 0xF800 is the top 5 bits, which BGR565 calls red...
        assert_eq!(packed(ImageFormat::Bgr565, 0xF800), [255, 0, 0, 255]);
        // ...and RGB565 calls blue.
        assert_eq!(packed(ImageFormat::Rgb565, 0xF800), [0, 0, 255, 255]);

        // The 1-bit alpha is the top bit, and BGRX5551 ignores it.
        assert_eq!(packed(ImageFormat::Bgra5551, 0x7FFF)[3], 0);
        assert_eq!(packed(ImageFormat::Bgrx5551, 0x7FFF)[3], 255);
    }

    #[test]
    fn single_channel_formats_expand_the_way_d3d_read_them() {
        // `D3DFMT_L8` broadcast to RGB with alpha 1...
        let rgba = ImageFormat::I8.to_gpu_bytes(&[0x40], 1);
        assert_eq!(&rgba[..], &[0x40, 0x40, 0x40, 255]);
        // ...`D3DFMT_A8L8` kept the alpha...
        let rgba = ImageFormat::Ia88.to_gpu_bytes(&[0x40, 0x80], 1);
        assert_eq!(&rgba[..], &[0x40, 0x40, 0x40, 0x80]);
        // ...and `D3DFMT_A8` read as black.
        let rgba = ImageFormat::A8.to_gpu_bytes(&[0x80], 1);
        assert_eq!(&rgba[..], &[0, 0, 0, 0x80]);
    }

    #[test]
    fn bluescreen_blue_becomes_transparent() {
        let keyed = ImageFormat::Rgb888Bluescreen.to_gpu_bytes(&[0, 0, 255], 1);
        assert_eq!(&keyed[..], &[0, 0, 255, 0]);
        let kept = ImageFormat::Rgb888Bluescreen.to_gpu_bytes(&[1, 0, 255], 1);
        assert_eq!(&kept[..], &[1, 0, 255, 255]);
        // BGR order: the blue byte is first in the source.
        let keyed = ImageFormat::Bgr888Bluescreen.to_gpu_bytes(&[255, 0, 0], 1);
        assert_eq!(&keyed[..], &[0, 0, 255, 0]);
    }

    #[test]
    fn rgb_float_gains_an_opaque_alpha() {
        let src: Vec<u8> = [1.0f32, 2.0, 3.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let out = ImageFormat::Rgb323232F.to_gpu_bytes(&src, 1);
        assert_eq!(out.len(), 16);
        let a = f32::from_le_bytes([out[12], out[13], out[14], out[15]]);
        assert_eq!(a, 1.0);
        assert_eq!(&out[..12], &src[..]);
    }
}
