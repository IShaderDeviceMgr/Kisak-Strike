//! Reading `.vtf` files — Valve Texture Format.
//!
//! Replaces `vtf/vtf.cpp`'s `CVTFTexture::Unserialize` and the parts of
//! `IVTFTexture` (`public/vtf/vtf.h`) that describe a file already on disk.
//! The header layout itself comes from the `VTFFileHeaderV7_1_t` ..
//! `VTFFileHeader_t` chain in that header, read as **the POSIX `pack(1)`
//! layout**, which is the one shipped files actually use. See
//! [`Vtf::parse`] for the byte map.
//!
//! # What this is not
//!
//! `CVTFTexture` is a texture *editor* as much as a reader: two thirds of
//! `vtf.cpp` is mip generation, spheremap projection, cubemap border blending,
//! S3TC palette matching, reflectivity computation and format conversion — the
//! `vtex` content pipeline, compiled into the engine. None of that is here. We
//! read files Valve's tools already produced; we do not produce them.
//!
//! Two structural departures follow from that:
//!
//! - **The file is kept as it was read and never rearranged.** `CVTFTexture`
//!   copies the image into a second buffer in a different order (`vtf.h:258`:
//!   frame, then face, then mip descending) because its editing operations
//!   wanted that order. Nothing here edits, so [`Vtf::mip_data`] indexes into
//!   the original bytes and the copy does not exist.
//! - **Mip levels that are not in the file are not invented.** `Unserialize`
//!   allocates the *full* chain down to 1x1 and leaves levels the file does not
//!   contain uninitialized, "for backward compatibility" (`vtf.cpp:1080`).
//!   [`Vtf::mip_count`] is the number of levels that are really there.

use super::error::VtfError;
use super::image_format::{full_mip_count, mip_dimensions, ImageFormat};

/// `"VTF\0"`.
const SIGNATURE: &[u8; 4] = b"VTF\0";

/// The only major version this reads. `VTF_MAJOR_VERSION`, `public/vtf/vtf.h:296`.
const MAJOR_VERSION: u32 = 7;

/// `VTF_MINOR_VERSION`, `public/vtf/vtf.h:297`.
const MAX_MINOR_VERSION: u32 = 5;

/// `VTF_X360_MAJOR_VERSION` / `VTF_PS3_MAJOR_VERSION` (`vtf.h:417`, `vtf.h:440`).
/// Recognized only so the error can say what the file is.
const X360_MAJOR_VERSION: u32 = 0x0360;
const PS3_MAJOR_VERSION: u32 = 0x0333;

/// Size of the fixed header for 7.0/7.1: `sizeof(VTFFileHeaderV7_1_t)` (63)
/// plus the one alignment byte `ReadHeaderFromBufferPastBaseHeader` skips
/// (`vtf/vtf.cpp:949`).
const HEADER_SIZE_7_1: usize = 64;

/// Size of the fixed header for 7.2 and later: `sizeof(VTFFileHeaderV7_2_t)`
/// (65) plus 15 skipped bytes (`vtf/vtf.cpp:939`), which is also exactly
/// `sizeof(VTFFileHeaderV7_3_t)` on POSIX. The resource dictionary of 7.3+
/// starts here.
const HEADER_SIZE_7_2: usize = 80;

/// Bytes per `ResourceEntryInfo` (`public/vtf/vtf.h:385`).
const RESOURCE_ENTRY_SIZE: usize = 8;

/// `VTF_LEGACY_RSRC_LOW_RES_IMAGE`, `MK_VTF_RSRC_ID( 0x01, 0, 0 )`.
const RSRC_LOW_RES_IMAGE: u32 = 0x01;

/// `VTF_LEGACY_RSRC_IMAGE`, `MK_VTF_RSRC_ID( 0x30, 0, 0 )`.
const RSRC_IMAGE: u32 = 0x30;

/// `RSRCF_MASK` — the high byte of a resource type is flags, not identity.
const RSRCF_MASK: u32 = 0xFF00_0000;

/// `RSRCF_HAS_NO_DATA_CHUNK`: the entry's `resData` *is* the data, not an
/// offset to it.
const RSRCF_HAS_NO_DATA_CHUNK: u32 = 0x0200_0000;

/// Flags a 7.3-or-earlier file cannot have set, so any bits found there are
/// stale. `VERSIONED_VTF_FLAGS_MASK_7_3` (`public/vtf/vtf_declarations.h:88`),
/// written the way it is meant rather than as a double negative.
const FLAGS_ADDED_AFTER_7_3: u32 = 0xD178_0400;

/// Compiled texture flags. `CompiledVtfFlags`,
/// `public/vtf/vtf_declarations.h:26`.
///
/// A plain newtype rather than a `bitflags!` dependency: the set is fixed by
/// the file format and never grows, and the three operations we need are
/// [`contains`](Self::contains), construction and equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureFlags(pub u32);

// The whole flag set is declared even though only a handful are read today.
// These are file-format data, not an API surface we chose: a `.vtf` on disk can
// have any of them set, and a reader that names only the ones it currently
// consults invites the next person to re-derive the other twenty from the C++.
#[allow(dead_code)]
impl TextureFlags {
    pub const POINT_SAMPLE: Self = Self(0x0000_0001);
    pub const TRILINEAR: Self = Self(0x0000_0002);
    pub const CLAMP_S: Self = Self(0x0000_0004);
    pub const CLAMP_T: Self = Self(0x0000_0008);
    pub const ANISOTROPIC: Self = Self(0x0000_0010);
    pub const HINT_DXT5: Self = Self(0x0000_0020);
    pub const PWL_CORRECTED: Self = Self(0x0000_0040);
    /// The texture is a normal map — vectors, not colour.
    pub const NORMAL: Self = Self(0x0000_0080);
    pub const NO_MIP: Self = Self(0x0000_0100);
    pub const NO_LOD: Self = Self(0x0000_0200);
    pub const ALL_MIPS: Self = Self(0x0000_0400);
    pub const PROCEDURAL: Self = Self(0x0000_0800);
    pub const ONE_BIT_ALPHA: Self = Self(0x0000_1000);
    pub const EIGHT_BIT_ALPHA: Self = Self(0x0000_2000);
    /// Cubemap: six faces, and on 7.1-7.4 a seventh on disk. See [`Vtf::parse`].
    pub const ENVMAP: Self = Self(0x0000_4000);
    pub const RENDER_TARGET: Self = Self(0x0000_8000);
    pub const DEPTH_RENDER_TARGET: Self = Self(0x0001_0000);
    pub const NO_DEBUG_OVERRIDE: Self = Self(0x0002_0000);
    pub const SINGLE_COPY: Self = Self(0x0004_0000);
    /// "SRGB correction has already been applied to this texture."
    pub const SRGB: Self = Self(0x0008_0000);
    pub const DEFAULT_POOL: Self = Self(0x0010_0000);
    pub const COMBINED: Self = Self(0x0020_0000);
    pub const ASYNC_DOWNLOAD: Self = Self(0x0040_0000);
    pub const NO_DEPTH_BUFFER: Self = Self(0x0080_0000);
    pub const SKIP_INITIAL_DOWNLOAD: Self = Self(0x0100_0000);
    pub const CLAMP_U: Self = Self(0x0200_0000);
    pub const VERTEX_TEXTURE: Self = Self(0x0400_0000);
    /// Self-shadowing bump map — also vectors, not colour.
    pub const SSBUMP: Self = Self(0x0800_0000);
    pub const MOST_MIPS: Self = Self(0x1000_0000);
    /// Clamp to a border colour on every coordinate.
    pub const BORDER: Self = Self(0x2000_0000);

    /// Whether every bit of `other` is set here.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The 8-bit-per-channel thumbnail 7.0-7.5 files carry ahead of the image.
///
/// Used by the original for average-colour queries without touching the real
/// image; nothing in this port reads the pixels yet, but where they are and how
/// big they are is needed to find the image data in a 7.0-7.2 file.
// Same as `Vtf` above: the fields are the file's, and the reader states them
// whether or not anything consults them yet.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct LowResImage {
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    offset: usize,
    size: usize,
}

/// A parsed `.vtf`, holding the file bytes it was parsed from.
///
// A reader for a file format exposes the whole header; not every field has a
// consumer on the day it lands. `reflectivity`, `bump_scale` and `start_frame`
// are read by the material path in stage 3 (`$envmaptint` defaults, `$bumpscale`
// and animated-texture playback respectively). Reading them here and letting
// them sit is much cheaper than a second parse later.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Vtf {
    /// `(major, minor)`. Always `(7, 0..=5)` — see [`Vtf::parse`].
    pub version: (u32, u32),
    pub width: u32,
    pub height: u32,
    /// 1 for an ordinary 2D texture. `> 1` only for volume textures, which
    /// arrived in 7.2.
    pub depth: u32,
    pub format: ImageFormat,
    pub flags: TextureFlags,
    /// Animation frames. At least 1.
    pub frame_count: u32,
    /// Which frame an animated texture starts on. Carried because the file has
    /// it; `CVTFTexture` itself comments "FIXME: Why is this needed?".
    pub start_frame: u32,
    /// 6 for a cubemap, 1 otherwise.
    pub face_count: u32,
    /// Mip levels **actually present in the file**, largest first. Level `i`
    /// measures `width >> i` by `height >> i`, floored at 1.
    pub mip_count: u32,
    /// Average colour of the texture, used by the original for dynamic lighting
    /// on models and for `$envmaptint` defaults. Stored blue, green, red — the
    /// order `IVTFTexture::Reflectivity` documents at `public/vtf/vtf.h:101`.
    pub reflectivity: [f32; 3],
    pub bump_scale: f32,
    pub low_res: Option<LowResImage>,

    /// Faces stored per frame in the file, which is **not** always
    /// [`face_count`](Self::face_count). See [`Vtf::parse`].
    faces_on_disk: u32,
    /// Where the image data starts.
    image_offset: usize,
    /// The file, verbatim.
    data: Vec<u8>,
}

// Same reasoning as the struct: `is_normal_map` and `low_res_data` answer
// questions the material path asks in stage 3, and both are one line on top of
// state `parse` already established.
#[allow(dead_code)]
impl Vtf {
    /// Parses a `.vtf` from the bytes of the whole file.
    ///
    /// # The header, byte by byte
    ///
    /// `public/vtf/vtf.h` describes the header as four inherited `pack(1)`
    /// structs, and carries a shouting comment (`vtf.h:300`) that their sizes
    /// "ARE NOT what they appear" because a `VectorAligned` member makes the PC
    /// compiler insert padding the 360 compiler does not. The POSIX build
    /// resolves that by declaring the padding by hand, so the layout below is
    /// unambiguous and is what shipped files contain:
    ///
    /// ```text
    ///  0  signature "VTF\0"        16  width      u16      48  bumpScale       f32
    ///  4  major version    u32     18  height     u16      52  imageFormat     i32
    ///  8  minor version    u32     20  flags      u32      56  numMipLevels    u8
    /// 12  headerSize       u32     24  numFrames  u16      57  lowResFormat    i32
    ///                              26  startFrame u16      61  lowResWidth     u8
    ///                              28  (padding)            62  lowResHeight    u8
    ///                              32  reflectivity 3xf32  -- 7.0/7.1 end at 64 --
    ///                              44  (padding)           63  depth           u16
    ///                                                      -- 7.2 ends at 80 --
    ///                                                      65  (padding)
    ///                                                      68  numResources    u32
    ///                                                      72  (padding)
    ///                                                      80  resource entries
    /// ```
    ///
    /// `headerSize` is read by nothing, here or in the original: 7.3+ finds its
    /// data through the resource dictionary's absolute offsets, and earlier
    /// versions have a fixed header size. It is left in the map above only so
    /// nobody wonders where field 12 went.
    ///
    /// # The seventh cubemap face
    ///
    /// The trap in this format. Cubemaps written by versions **7.1 through
    /// 7.4** store *seven* faces per frame: the six real ones and a spheremap
    /// fallback for hardware that could not sample a cube. 7.0 and 7.5 store
    /// six. `LoadImageData` handles this by reading six and seeking past a
    /// seventh (`vtf/vtf.cpp:753`), with a retry if that overruns the file
    /// (`vtf/vtf.cpp:761`) because Valve shipped a bad 7.4 writer that stripped
    /// the spheremap without changing the version.
    ///
    /// The same two candidate layouts are tried here, in the same order, so a
    /// file that loads in the original loads here — but as arithmetic done once
    /// at parse time rather than as a failed read and a `goto`.
    pub fn parse(data: Vec<u8>) -> Result<Vtf, VtfError> {
        let need = |n: usize| -> Result<(), VtfError> {
            if data.len() < n {
                Err(VtfError::Truncated {
                    needed: n,
                    actual: data.len(),
                })
            } else {
                Ok(())
            }
        };

        need(HEADER_SIZE_7_1)?;
        if &data[0..4] != SIGNATURE {
            return Err(VtfError::BadSignature);
        }

        let major = u32(&data, 4);
        let minor = u32(&data, 8);
        match major {
            X360_MAJOR_VERSION => {
                return Err(VtfError::ConsoleFormat {
                    platform: "Xbox 360",
                })
            }
            PS3_MAJOR_VERSION => return Err(VtfError::ConsoleFormat { platform: "PS3" }),
            MAJOR_VERSION => {}
            _ => return Err(VtfError::UnsupportedVersion { major, minor }),
        }
        if minor > MAX_MINOR_VERSION {
            return Err(VtfError::UnsupportedVersion { major, minor });
        }

        let width = u16(&data, 16) as u32;
        let height = u16(&data, 18) as u32;
        let mut flags = TextureFlags(u32(&data, 20));
        let frame_count = u16(&data, 24) as u32;
        let start_frame = u16(&data, 26) as u32;
        let reflectivity = [f32at(&data, 32), f32at(&data, 36), f32at(&data, 40)];
        let bump_scale = f32at(&data, 48);
        let raw_format = u32(&data, 52) as i32;
        let disk_mip_count = data[56] as u32;
        let raw_low_res_format = u32(&data, 57) as i32;
        let low_res_width = data[61] as u32;
        let low_res_height = data[62] as u32;

        // 7.0 and 7.1 predate volume textures; `ReadHeader`'s version fixups
        // (`vtf/vtf.cpp:1022`) force the depth rather than leaving it zeroed.
        let depth = if minor >= 2 {
            need(HEADER_SIZE_7_2)?;
            u16(&data, 63) as u32
        } else {
            1
        };
        let resource_count = if minor >= 3 { u32(&data, 68) } else { 0 };
        // Same fixup chain: bits that only exist in 7.4+ are noise in an
        // older file.
        if minor <= 3 {
            flags.0 &= !FLAGS_ADDED_AFTER_7_3;
        }

        // `Unserialize`'s validity checks, `vtf/vtf.cpp:1055`.
        if width == 0 || height == 0 || depth == 0 {
            return Err(VtfError::Invalid("texture has a zero dimension"));
        }
        let is_cubemap = flags.contains(TextureFlags::ENVMAP);
        if is_cubemap && width != height {
            return Err(VtfError::Invalid("cubemap faces are not square"));
        }
        if is_cubemap && depth != 1 {
            return Err(VtfError::Invalid("cubemap is also a volume texture"));
        }
        // Not checked by the original, which would simply produce a texture
        // with no pixels in it and report success.
        if frame_count == 0 {
            return Err(VtfError::Invalid("texture has no frames"));
        }

        let format = ImageFormat::from_raw(raw_format).ok_or(VtfError::UnsupportedFormat {
            raw: raw_format,
            name: super::image_format::unsupported_name(raw_format).unwrap_or("unrecognized"),
        })?;

        let full_mips = full_mip_count(width, height, depth);
        if disk_mip_count == 0 || disk_mip_count > full_mips {
            return Err(VtfError::Invalid(
                "mip count is zero or longer than a full chain",
            ));
        }

        let face_count = if is_cubemap { 6 } else { 1 };

        // The low-res thumbnail. A zero dimension means there is none, which is
        // how `Unserialize` reads it (`vtf/vtf.cpp:1094`).
        let low_res_present = low_res_width > 0 && low_res_height > 0;
        let low_res_format = if low_res_present {
            ImageFormat::from_raw(raw_low_res_format)
        } else {
            None
        };
        let low_res_size = match low_res_format {
            Some(fmt) => fmt.mem_required(low_res_width, low_res_height, 1),
            None => 0,
        };

        // Where the image and thumbnail live. 7.3 replaced the fixed order with
        // a dictionary of absolute offsets (`public/vtf/vtf.h:267`).
        let (image_offset, low_res_offset) = if minor >= 3 {
            let dictionary_end = HEADER_SIZE_7_2 + resource_count as usize * RESOURCE_ENTRY_SIZE;
            need(dictionary_end)?;

            let mut image = None;
            let mut low_res = None;
            for i in 0..resource_count as usize {
                let at = HEADER_SIZE_7_2 + i * RESOURCE_ENTRY_SIZE;
                let raw_type = u32(&data, at);
                // The high byte is flags; an entry that carries its data inline
                // has no offset to read.
                if raw_type & RSRCF_HAS_NO_DATA_CHUNK != 0 {
                    continue;
                }
                let offset = u32(&data, at + 4) as usize;
                match raw_type & !RSRCF_MASK {
                    RSRC_IMAGE => image = Some(offset),
                    RSRC_LOW_RES_IMAGE => low_res = Some(offset),
                    // Sheet data, LOD settings, CRC. Nothing reads them yet.
                    _ => {}
                }
            }
            (image.ok_or(VtfError::NoImageData)?, low_res)
        } else {
            let header_size = if minor >= 2 {
                HEADER_SIZE_7_2
            } else {
                HEADER_SIZE_7_1
            };
            // The image sits directly behind the thumbnail, so a thumbnail in a
            // format we cannot size makes the image unfindable. 7.3+ is immune,
            // which is why this is checked only here.
            if low_res_present && low_res_format.is_none() {
                return Err(VtfError::UnsupportedFormat {
                    raw: raw_low_res_format,
                    name: super::image_format::unsupported_name(raw_low_res_format)
                        .unwrap_or("unrecognized"),
                });
            }
            let low_res = (low_res_size > 0).then_some(header_size);
            (header_size + low_res_size, low_res)
        };

        // Sizes of everything, so that `mip_data` cannot fail later.
        let face_size: usize = (0..disk_mip_count)
            .map(|level| {
                let (w, h, d) = mip_dimensions(width, height, depth, level);
                format.mem_required(w, h, d)
            })
            .sum();

        // See "The seventh cubemap face" above: try the layout the version
        // implies, then the other one.
        let spheremap_versions = (1..5).contains(&minor);
        let candidates: [u32; 2] = if is_cubemap && spheremap_versions {
            [7, face_count]
        } else {
            [face_count, face_count]
        };
        let mut faces_on_disk = None;
        // The candidates are ordered widest-first, so the last one tried is the
        // least the file could possibly have contained — the honest number to
        // report if none of them fit.
        let mut smallest_need = 0;
        for faces in candidates {
            let total = face_size
                .saturating_mul(frame_count as usize)
                .saturating_mul(faces as usize);
            let end = image_offset.saturating_add(total);
            smallest_need = end;
            if end <= data.len() {
                faces_on_disk = Some(faces);
                break;
            }
        }
        let Some(faces_on_disk) = faces_on_disk else {
            return Err(VtfError::Truncated {
                needed: smallest_need,
                actual: data.len(),
            });
        };

        let low_res = match (low_res_format, low_res_offset) {
            (Some(format), Some(offset)) if low_res_size > 0 => {
                need(offset.saturating_add(low_res_size))?;
                Some(LowResImage {
                    width: low_res_width,
                    height: low_res_height,
                    format,
                    offset,
                    size: low_res_size,
                })
            }
            _ => None,
        };

        Ok(Vtf {
            version: (major, minor),
            width,
            height,
            depth,
            format,
            flags,
            frame_count,
            start_frame,
            face_count,
            mip_count: disk_mip_count,
            reflectivity,
            bump_scale,
            low_res,
            faces_on_disk,
            image_offset,
            data,
        })
    }

    /// Whether this is a cubemap — six faces rather than one.
    pub fn is_cubemap(&self) -> bool {
        self.flags.contains(TextureFlags::ENVMAP)
    }

    /// Whether this is a volume texture.
    pub fn is_volume(&self) -> bool {
        self.depth > 1
    }

    /// Whether the file says its texels are vectors rather than colour.
    ///
    /// `TEXTUREFLAGS_NORMAL` or `TEXTUREFLAGS_SSBUMP`. This is the one part of
    /// the sRGB decision a `.vtf` can answer on its own — see
    /// `rustdocs/MATERIALS.md`.
    pub fn is_normal_map(&self) -> bool {
        self.flags.contains(TextureFlags::NORMAL) || self.flags.contains(TextureFlags::SSBUMP)
    }

    /// Dimensions of one mip level, floored at 1 in each axis.
    pub fn mip_dimensions(&self, level: u32) -> (u32, u32, u32) {
        mip_dimensions(self.width, self.height, self.depth, level)
    }

    /// The bytes of one mip level of one face of one frame.
    ///
    /// `None` if any index is out of range. In range, the slice is exactly
    /// [`ImageFormat::mem_required`] of the level's dimensions — the bounds were
    /// established at parse time.
    ///
    /// Levels are indexed largest-first (0 is the full-size image), which is the
    /// order `wgpu` and every other modern API use. The file stores them the
    /// other way round, smallest first, so that a truncated read yields the
    /// small levels rather than half of a big one (`public/vtf/vtf.h:253`);
    /// that reversal happens here, in [`Vtf::mip_offset`].
    pub fn mip_data(&self, frame: u32, face: u32, level: u32) -> Option<&[u8]> {
        if frame >= self.frame_count || face >= self.face_count || level >= self.mip_count {
            return None;
        }
        let (w, h, d) = self.mip_dimensions(level);
        let size = self.format.mem_required(w, h, d);
        let start = self.mip_offset(frame, face, level);
        self.data.get(start..start + size)
    }

    /// The thumbnail's bytes, in [`LowResImage::format`].
    pub fn low_res_data(&self) -> Option<&[u8]> {
        let low_res = self.low_res?;
        self.data.get(low_res.offset..low_res.offset + low_res.size)
    }

    /// Byte offset of one mip level of one face of one frame.
    ///
    /// The file's order is: for each mip level from smallest to largest, for
    /// each frame, for each face. So everything *smaller* than `level` comes
    /// first, then whole frames, then faces within the frame.
    fn mip_offset(&self, frame: u32, face: u32, level: u32) -> usize {
        let frames = self.frame_count as usize;
        let faces = self.faces_on_disk as usize;

        let mut offset = self.image_offset;
        for smaller in (level + 1)..self.mip_count {
            let (w, h, d) = self.mip_dimensions(smaller);
            offset += self.format.mem_required(w, h, d) * frames * faces;
        }

        let (w, h, d) = self.mip_dimensions(level);
        let face_size = self.format.mem_required(w, h, d);
        offset + face_size * (frame as usize * faces + face as usize)
    }
}

fn u16(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn f32at(data: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `.vtf` in memory, the way `CVTFTexture::Serialize` would.
    ///
    /// Every field is settable because the interesting cases are all malformed
    /// or archaic files: 7.1 cubemaps with a spheremap, 7.4 cubemaps without
    /// one, partial mip chains, truncation.
    struct VtfBuilder {
        minor: u32,
        width: u16,
        height: u16,
        depth: u16,
        flags: u32,
        frames: u16,
        format: ImageFormat,
        mips: u8,
        low_res: Option<(u8, u8, ImageFormat)>,
        /// Faces actually written, overriding what the version implies.
        faces_on_disk: Option<u32>,
        /// Truncate the finished file to this many bytes.
        truncate_to: Option<usize>,
    }

    impl VtfBuilder {
        fn new(minor: u32) -> Self {
            VtfBuilder {
                minor,
                width: 8,
                height: 8,
                depth: 1,
                flags: 0,
                frames: 1,
                format: ImageFormat::Bgra8888,
                mips: 1,
                low_res: None,
                faces_on_disk: None,
                truncate_to: None,
            }
        }

        fn size(mut self, w: u16, h: u16) -> Self {
            self.width = w;
            self.height = h;
            self
        }
        fn format(mut self, format: ImageFormat) -> Self {
            self.format = format;
            self
        }
        fn mips(mut self, mips: u8) -> Self {
            self.mips = mips;
            self
        }
        fn frames(mut self, frames: u16) -> Self {
            self.frames = frames;
            self
        }
        fn cubemap(mut self) -> Self {
            self.flags |= TextureFlags::ENVMAP.0;
            self
        }
        fn flags(mut self, flags: u32) -> Self {
            self.flags |= flags;
            self
        }
        fn low_res(mut self, w: u8, h: u8, format: ImageFormat) -> Self {
            self.low_res = Some((w, h, format));
            self
        }
        fn faces_on_disk(mut self, faces: u32) -> Self {
            self.faces_on_disk = Some(faces);
            self
        }
        fn truncate_to(mut self, bytes: usize) -> Self {
            self.truncate_to = Some(bytes);
            self
        }

        /// The byte at `(frame, face, level)`'s start, so tests can assert that
        /// `mip_data` landed on the right slice.
        fn marker(frame: u32, face: u32, level: u32) -> u8 {
            0x10u8
                .wrapping_add((frame as u8) << 6)
                .wrapping_add((face as u8) << 3)
                .wrapping_add(level as u8)
        }

        fn build(&self) -> Vec<u8> {
            let is_cubemap = self.flags & TextureFlags::ENVMAP.0 != 0;
            let faces = self.faces_on_disk.unwrap_or({
                if is_cubemap && (1..5).contains(&self.minor) {
                    7
                } else if is_cubemap {
                    6
                } else {
                    1
                }
            });

            let header_size = if self.minor >= 2 {
                HEADER_SIZE_7_2
            } else {
                HEADER_SIZE_7_1
            };
            let mut file = vec![0u8; header_size];
            file[0..4].copy_from_slice(SIGNATURE);
            file[4..8].copy_from_slice(&MAJOR_VERSION.to_le_bytes());
            file[8..12].copy_from_slice(&self.minor.to_le_bytes());
            file[16..18].copy_from_slice(&self.width.to_le_bytes());
            file[18..20].copy_from_slice(&self.height.to_le_bytes());
            file[20..24].copy_from_slice(&self.flags.to_le_bytes());
            file[24..26].copy_from_slice(&self.frames.to_le_bytes());
            file[32..36].copy_from_slice(&0.5f32.to_le_bytes());
            file[36..40].copy_from_slice(&0.25f32.to_le_bytes());
            file[40..44].copy_from_slice(&0.125f32.to_le_bytes());
            file[48..52].copy_from_slice(&2.0f32.to_le_bytes());
            file[52..56].copy_from_slice(&(self.format as i32).to_le_bytes());
            file[56] = self.mips;
            match self.low_res {
                Some((w, h, fmt)) => {
                    file[57..61].copy_from_slice(&(fmt as i32).to_le_bytes());
                    file[61] = w;
                    file[62] = h;
                }
                // `IMAGE_FORMAT_UNKNOWN` with zero dimensions, as vtex writes
                // when there is no thumbnail.
                None => file[57..61].copy_from_slice(&(-1i32).to_le_bytes()),
            }
            if self.minor >= 2 {
                file[63..65].copy_from_slice(&self.depth.to_le_bytes());
            }

            let low_res_bytes = self
                .low_res
                .map(|(w, h, fmt)| fmt.mem_required(w as u32, h as u32, 1))
                .unwrap_or(0);

            if self.minor >= 3 {
                // Two entries, sorted ascending by type as the writer does.
                let mut entries: Vec<(u32, u32)> = Vec::new();
                let dictionary_end =
                    HEADER_SIZE_7_2 + RESOURCE_ENTRY_SIZE * if low_res_bytes > 0 { 2 } else { 1 };
                if low_res_bytes > 0 {
                    entries.push((RSRC_LOW_RES_IMAGE, dictionary_end as u32));
                }
                entries.push((RSRC_IMAGE, (dictionary_end + low_res_bytes) as u32));
                file[68..72].copy_from_slice(&(entries.len() as u32).to_le_bytes());
                for (kind, offset) in entries {
                    file.extend_from_slice(&kind.to_le_bytes());
                    file.extend_from_slice(&offset.to_le_bytes());
                }
            }

            file.extend(std::iter::repeat(0xEE).take(low_res_bytes));

            // Image data: smallest mip first, then frame, then face.
            let depth = self.depth as u32;
            for level in (0..self.mips as u32).rev() {
                let (w, h, d) = mip_dimensions(self.width as u32, self.height as u32, depth, level);
                let size = self.format.mem_required(w, h, d);
                for frame in 0..self.frames as u32 {
                    for face in 0..faces {
                        let mut block = vec![0u8; size];
                        if !block.is_empty() {
                            block[0] = Self::marker(frame, face, level);
                        }
                        file.extend_from_slice(&block);
                    }
                }
            }

            if let Some(limit) = self.truncate_to {
                file.truncate(limit);
            }
            file
        }

        fn parse(&self) -> Result<Vtf, VtfError> {
            Vtf::parse(self.build())
        }
    }

    #[test]
    fn reads_a_minimal_7_5_texture() {
        let vtf = VtfBuilder::new(5)
            .size(64, 32)
            .format(ImageFormat::Dxt5)
            .mips(7)
            .parse()
            .expect("valid 7.5 file");

        assert_eq!(vtf.version, (7, 5));
        assert_eq!((vtf.width, vtf.height, vtf.depth), (64, 32, 1));
        assert_eq!(vtf.format, ImageFormat::Dxt5);
        assert_eq!(vtf.frame_count, 1);
        assert_eq!(vtf.face_count, 1);
        assert_eq!(vtf.mip_count, 7);
        assert_eq!(vtf.bump_scale, 2.0);
        assert_eq!(vtf.reflectivity, [0.5, 0.25, 0.125]);
        assert!(!vtf.is_cubemap() && !vtf.is_volume());
        assert!(vtf.low_res.is_none());
    }

    #[test]
    fn every_supported_version_finds_its_image_data() {
        // The header size changes at 7.2 and the data-location mechanism at
        // 7.3, so each version reaches the pixels a different way.
        for minor in 0..=MAX_MINOR_VERSION {
            let vtf = VtfBuilder::new(minor)
                .size(4, 4)
                .mips(3)
                .parse()
                .unwrap_or_else(|e| panic!("7.{minor}: {e}"));
            assert_eq!(vtf.version, (7, minor));
            for level in 0..3 {
                let data = vtf.mip_data(0, 0, level).expect("in range");
                assert_eq!(
                    data[0],
                    VtfBuilder::marker(0, 0, level),
                    "7.{minor} mip {level} landed on the wrong bytes"
                );
            }
        }
    }

    #[test]
    fn mip_levels_are_indexed_largest_first() {
        let vtf = VtfBuilder::new(5)
            .size(8, 8)
            .format(ImageFormat::Bgra8888)
            .mips(4)
            .parse()
            .unwrap();

        // Level 0 is the full 8x8 image even though it is last in the file.
        assert_eq!(vtf.mip_dimensions(0), (8, 8, 1));
        assert_eq!(vtf.mip_data(0, 0, 0).unwrap().len(), 8 * 8 * 4);
        assert_eq!(vtf.mip_dimensions(3), (1, 1, 1));
        assert_eq!(vtf.mip_data(0, 0, 3).unwrap().len(), 4);
        for level in 0..4 {
            assert_eq!(
                vtf.mip_data(0, 0, level).unwrap()[0],
                VtfBuilder::marker(0, 0, level)
            );
        }
        assert!(vtf.mip_data(0, 0, 4).is_none(), "past the end of the chain");
    }

    #[test]
    fn frames_and_faces_are_addressed_independently() {
        let vtf = VtfBuilder::new(5)
            .size(4, 4)
            .frames(3)
            .cubemap()
            .mips(2)
            .parse()
            .unwrap();

        assert_eq!(vtf.frame_count, 3);
        assert_eq!(vtf.face_count, 6);
        for frame in 0..3 {
            for face in 0..6 {
                for level in 0..2 {
                    assert_eq!(
                        vtf.mip_data(frame, face, level).unwrap()[0],
                        VtfBuilder::marker(frame, face, level),
                        "frame {frame} face {face} mip {level}"
                    );
                }
            }
        }
        assert!(vtf.mip_data(3, 0, 0).is_none());
        assert!(vtf.mip_data(0, 6, 0).is_none());
    }

    #[test]
    fn a_7_1_cubemap_skips_the_spheremap_face() {
        // 7.1-7.4 write seven faces and the seventh is a spheremap fallback we
        // never want. Getting this wrong reads face N+1's pixels for face N.
        let vtf = VtfBuilder::new(1).size(4, 4).cubemap().parse().unwrap();
        assert_eq!(vtf.face_count, 6);
        for face in 0..6 {
            assert_eq!(
                vtf.mip_data(0, face, 0).unwrap()[0],
                VtfBuilder::marker(0, face, 0),
                "face {face}"
            );
        }
    }

    #[test]
    fn a_7_0_cubemap_has_only_six_faces() {
        let vtf = VtfBuilder::new(0).size(4, 4).cubemap().parse().unwrap();
        for face in 0..6 {
            assert_eq!(
                vtf.mip_data(0, face, 0).unwrap()[0],
                VtfBuilder::marker(0, face, 0)
            );
        }
    }

    #[test]
    fn a_7_4_cubemap_written_without_a_spheremap_still_loads() {
        // `vtf.cpp:761`: Valve shipped a writer that stripped the seventh face
        // without bumping the version, and the reader retries when the
        // seven-face layout overruns the file.
        let vtf = VtfBuilder::new(4)
            .size(4, 4)
            .cubemap()
            .faces_on_disk(6)
            .parse()
            .expect("stale 7.4 cubemap");
        for face in 0..6 {
            assert_eq!(
                vtf.mip_data(0, face, 0).unwrap()[0],
                VtfBuilder::marker(0, face, 0),
                "face {face}"
            );
        }
    }

    #[test]
    fn a_partial_mip_chain_is_reported_as_what_is_there() {
        // 64x64 is a 7-level chain, but the file carries 3. The original
        // allocates 7 and leaves 4 uninitialized; we say 3.
        let vtf = VtfBuilder::new(5).size(64, 64).mips(3).parse().unwrap();
        assert_eq!(vtf.mip_count, 3);
        assert_eq!(vtf.mip_dimensions(0), (64, 64, 1));
        assert_eq!(vtf.mip_dimensions(2), (16, 16, 1));
        assert!(vtf.mip_data(0, 0, 3).is_none());
    }

    #[test]
    fn the_thumbnail_is_found_and_sized() {
        for minor in [2, 5] {
            let vtf = VtfBuilder::new(minor)
                .size(16, 16)
                .low_res(4, 4, ImageFormat::Dxt1)
                .parse()
                .unwrap_or_else(|e| panic!("7.{minor}: {e}"));
            let low_res = vtf.low_res.expect("thumbnail");
            assert_eq!((low_res.width, low_res.height), (4, 4));
            assert_eq!(low_res.format, ImageFormat::Dxt1);
            assert_eq!(vtf.low_res_data().unwrap(), &[0xEE; 8]);
            // ...and the image data still lands correctly behind it.
            assert_eq!(
                vtf.mip_data(0, 0, 0).unwrap()[0],
                VtfBuilder::marker(0, 0, 0)
            );
        }
    }

    #[test]
    fn flags_added_after_7_3_are_stripped_from_older_files() {
        // `VERSIONED_VTF_FLAGS_MASK_7_3`. `MOST_MIPS` in a 7.2 file is stale
        // data, not an instruction.
        let stale = VtfBuilder::new(2)
            .flags(TextureFlags::MOST_MIPS.0 | TextureFlags::CLAMP_S.0)
            .parse()
            .unwrap();
        assert!(!stale.flags.contains(TextureFlags::MOST_MIPS));
        assert!(stale.flags.contains(TextureFlags::CLAMP_S));

        let current = VtfBuilder::new(5)
            .flags(TextureFlags::MOST_MIPS.0)
            .parse()
            .unwrap();
        assert!(current.flags.contains(TextureFlags::MOST_MIPS));
    }

    #[test]
    fn volume_textures_multiply_through_the_chain() {
        let vtf = VtfBuilder::new(5)
            .size(8, 8)
            .format(ImageFormat::Bgra8888)
            .mips(4)
            .parse()
            .unwrap();
        assert!(!vtf.is_volume());

        let mut builder = VtfBuilder::new(5).size(8, 8).mips(4);
        builder.depth = 8;
        let vtf = builder.parse().unwrap();
        assert!(vtf.is_volume());
        assert_eq!(vtf.mip_dimensions(1), (4, 4, 4));
        assert_eq!(vtf.mip_data(0, 0, 1).unwrap().len(), 4 * 4 * 4 * 4);
    }

    #[test]
    fn normal_maps_identify_themselves() {
        assert!(VtfBuilder::new(5)
            .flags(TextureFlags::NORMAL.0)
            .parse()
            .unwrap()
            .is_normal_map());
        assert!(VtfBuilder::new(5)
            .flags(TextureFlags::SSBUMP.0)
            .parse()
            .unwrap()
            .is_normal_map());
        assert!(!VtfBuilder::new(5).parse().unwrap().is_normal_map());
    }

    #[test]
    fn junk_is_rejected_rather_than_read() {
        // Long enough that the signature, not the length, is what rejects it.
        let mut junk = b"not a texture".to_vec();
        junk.resize(HEADER_SIZE_7_2 * 2, 0);
        assert!(matches!(Vtf::parse(junk), Err(VtfError::BadSignature)));
        assert!(matches!(
            Vtf::parse(b"VTF\0".to_vec()),
            Err(VtfError::Truncated { .. })
        ));
    }

    #[test]
    fn console_files_are_named_rather_than_called_corrupt() {
        for (major, platform) in [(X360_MAJOR_VERSION, "Xbox 360"), (PS3_MAJOR_VERSION, "PS3")] {
            let mut file = VtfBuilder::new(5).build();
            file[4..8].copy_from_slice(&major.to_le_bytes());
            match Vtf::parse(file) {
                Err(VtfError::ConsoleFormat { platform: got }) => assert_eq!(got, platform),
                other => panic!("{major:#x}: {other:?}"),
            }
        }
    }

    #[test]
    fn future_versions_are_refused() {
        let mut file = VtfBuilder::new(5).build();
        file[8..12].copy_from_slice(&6u32.to_le_bytes());
        assert!(matches!(
            Vtf::parse(file),
            Err(VtfError::UnsupportedVersion { major: 7, minor: 6 })
        ));
    }

    #[test]
    fn a_truncated_image_is_an_error_not_a_short_read() {
        let full = VtfBuilder::new(5).size(16, 16).mips(5).build();
        let short = VtfBuilder::new(5)
            .size(16, 16)
            .mips(5)
            .truncate_to(full.len() - 1)
            .build();
        assert!(matches!(Vtf::parse(short), Err(VtfError::Truncated { .. })));
    }

    #[test]
    fn structurally_impossible_headers_are_refused() {
        let mut file = VtfBuilder::new(5).size(8, 4).cubemap().build();
        assert!(matches!(
            Vtf::parse(file.clone()),
            Err(VtfError::Invalid("cubemap faces are not square"))
        ));

        file = VtfBuilder::new(5).build();
        file[24..26].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            Vtf::parse(file.clone()),
            Err(VtfError::Invalid("texture has no frames"))
        ));

        file = VtfBuilder::new(5).build();
        file[16..18].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            Vtf::parse(file.clone()),
            Err(VtfError::Invalid("texture has a zero dimension"))
        ));

        // 8x8 is a 4-level chain; claiming 9 means the file is lying.
        file = VtfBuilder::new(5).size(8, 8).build();
        file[56] = 9;
        assert!(matches!(
            Vtf::parse(file.clone()),
            Err(VtfError::Invalid(_))
        ));
        file[56] = 0;
        assert!(matches!(Vtf::parse(file), Err(VtfError::Invalid(_))));
    }

    #[test]
    fn a_format_we_have_no_variant_for_is_named_in_the_error() {
        let mut file = VtfBuilder::new(5).build();
        // `IMAGE_FORMAT_LINEAR_DXT1`, an X360 format.
        file[52..56].copy_from_slice(&60i32.to_le_bytes());
        match Vtf::parse(file) {
            Err(VtfError::UnsupportedFormat { raw, name }) => {
                assert_eq!(raw, 60);
                assert_eq!(name, "LINEAR_DXT1");
            }
            other => panic!("{other:?}"),
        }
    }
}
