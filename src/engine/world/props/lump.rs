//! Reading the `sprp` game lump — the map's static prop placements.
//!
//! `CStaticPropMgr::UnserializeModels` (`engine/staticpropmgr.cpp:1519`),
//! minus everything that belongs to a subsystem this port does not have yet.
//! Design and the measurements that scoped it: `portdocs/STUDIO.md` §4.1.
//!
//! The payload is three counted arrays back to back, in this order:
//!
//! ```text
//! i32 dict_count;   StaticPropDictLump_t[dict_count]   // char name[128]
//! i32 leaf_count;   u16[leaf_count]                    // StaticPropLeafLump_t
//! i32 prop_count;   StaticPropLumpV9_t[prop_count]     // 72 bytes each
//! ```
//!
//! # The 72 bytes are the trap
//!
//! `StaticPropLumpV9_t`'s fields sum to **69**, and `gamebspfile.h`'s prop
//! structs are the only ones on this path *not* under `#pragma pack(1)` — so
//! the compiler adds **3 bytes of tail padding** to align the struct to its
//! widest member, and the file inherits that. Nothing in the lump says so.
//! Valve's reader gets it for free because `UnserializeModels` does
//! `buf.Get( &lump, sizeof(StaticPropLumpV9_t) )` (`staticpropmgr.cpp:1599`)
//! and `sizeof` includes the padding; a hand-written reader has to know.
//!
//! Confirmed twice: arithmetically, and by measuring
//! `(lump_end - prop_array_start) / prop_count` = exactly 72.0 on all 104
//! shipped maps that have props. A reader that used 69 would walk three bytes
//! further into each successive prop and produce a map whose props drift.

use super::{PropFlags, StaticProp};
use crate::engine::world::bsp::GameLump;
use glam::Vec3;

/// `STATIC_PROP_NAME_LENGTH` (`gamebspfile.h:124`).
const NAME_LENGTH: usize = 128;

/// `sizeof(StaticPropLumpV9_t)` — see the module docs. **Not 69.**
const PROP_STRIDE_V9: usize = 72;

/// `sizeof(StaticPropLump_t)`, version 10: V9 plus an `int m_FlagsEx`, which
/// re-triggers the same 4-byte alignment and so adds exactly 4.
const PROP_STRIDE_V10: usize = 76;

/// `GAMELUMP_STATIC_PROPS_MIN_VERSION` (`gamebspfile.h:38`).
const MIN_VERSION: u16 = 4;

/// The versions this reader decodes.
///
/// Portal 2 ships version 9 on every one of its 106 maps. 10 is accepted
/// because it is V9 with one trailing field, so reading it as V9 at the wider
/// stride is exact rather than approximate — and it is the version a map
/// compiled by a later branch's tools would carry.
const READABLE: std::ops::RangeInclusive<u16> = 9..=10;

/// Everything the `sprp` lump holds, decoded.
#[derive(Debug, Clone, Default)]
pub struct StaticPropLump {
    /// The model dictionary — `models/props_bts/gantry_rails_a.mdl` and so on,
    /// with the extension, indexed by [`StaticProp::model`].
    pub models: Vec<String>,
    /// `StaticPropLeafLump_t[]`, the leaves each prop touches, sliced by
    /// [`StaticProp::first_leaf`].
    ///
    /// A PVS acceleration structure: read and kept because it is the map's
    /// only record of it and re-deriving it means re-running the BSP query for
    /// every prop, but **unused until visibility lands** — every prop is drawn
    /// every frame today.
    pub leaves: Vec<u16>,
    pub props: Vec<StaticProp>,
}

/// Why an `sprp` lump could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PropLumpError {
    #[error(
        "{map} has version {version} static props; this engine reads versions {} to {}",
        READABLE.start(),
        READABLE.end()
    )]
    Version { map: String, version: u16 },

    #[error("{map}'s static prop lump is truncated: {what}")]
    Truncated { map: String, what: String },

    #[error("{map}'s static prop lump is internally inconsistent: {what}")]
    Corrupt { map: String, what: String },
}

impl StaticPropLump {
    /// Decodes one `sprp` game lump.
    ///
    /// `map` names the file only for error messages.
    pub fn parse(map: &str, lump: &GameLump) -> Result<StaticPropLump, PropLumpError> {
        if !READABLE.contains(&lump.version) {
            // A version below the minimum is not a static prop lump at all;
            // one above is a layout this reader has never seen. Both are
            // refused rather than guessed at, because the failure mode of
            // guessing is a stride that is wrong by a few bytes, and that
            // draws a map full of props in almost the right places.
            let _ = MIN_VERSION;
            return Err(PropLumpError::Version {
                map: map.to_owned(),
                version: lump.version,
            });
        }
        let stride = if lump.version >= 10 {
            PROP_STRIDE_V10
        } else {
            PROP_STRIDE_V9
        };

        let r = Cursor {
            map,
            bytes: &lump.data,
            at: 0,
        };
        let mut r = r;

        let dict_count = r.count("the model dictionary")?;
        let mut models = Vec::with_capacity(dict_count);
        for _ in 0..dict_count {
            models.push(r.fixed_string(NAME_LENGTH)?);
        }

        let leaf_count = r.count("the leaf list")?;
        let mut leaves = Vec::with_capacity(leaf_count);
        for _ in 0..leaf_count {
            leaves.push(r.u16()?);
        }

        let prop_count = r.count("the prop list")?;

        // The stride is asserted rather than assumed: the remaining bytes must
        // divide evenly by it. This is the check that catches a version whose
        // layout changed without its number doing so — and it is exactly the
        // measurement that established 72 in the first place.
        let remaining = r.bytes.len() - r.at;
        if prop_count > 0 && remaining / prop_count != stride {
            return Err(PropLumpError::Corrupt {
                map: map.to_owned(),
                what: format!(
                    "{prop_count} version {} props occupy {remaining} bytes, \
                     which is {:.2} bytes each and not {stride}",
                    lump.version,
                    remaining as f64 / prop_count as f64
                ),
            });
        }

        let mut props = Vec::with_capacity(prop_count);
        for i in 0..prop_count {
            let base = r.at;
            let origin = r.vec3()?;
            // QAngle, in Valve's order: pitch, yaw, roll — *not* x, y, z.
            let angles = r.vec3()?;
            let model = r.u16()?;
            let first_leaf = r.u16()?;
            let leaf_count_here = r.u16()?;
            let solid = r.u8()?;
            let flags = PropFlags::from_bits_truncate(r.u8()?);
            let skin = r.i32()?;
            let fade_min = r.f32()?;
            let fade_max = r.f32()?;
            let lighting_origin = r.vec3()?;
            let forced_fade_scale = r.f32()?;
            let (min_cpu, max_cpu, min_gpu, max_gpu) = (r.u8()?, r.u8()?, r.u8()?, r.u8()?);
            let diffuse_modulation = [r.u8()?, r.u8()?, r.u8()?, r.u8()?];
            // `m_bDisableX360`, then the tail padding. Skipped together by
            // seeking to the next record rather than by counting bytes, which
            // is what makes the V10 stride a one-line difference.
            r.at = base + stride;
            if r.at > r.bytes.len() {
                return Err(PropLumpError::Truncated {
                    map: map.to_owned(),
                    what: format!("prop {i} of {prop_count} runs past the lump"),
                });
            }

            if model as usize >= models.len() {
                return Err(PropLumpError::Corrupt {
                    map: map.to_owned(),
                    what: format!(
                        "prop {i} names model {model} of {} in the dictionary",
                        models.len()
                    ),
                });
            }
            let end = first_leaf as usize + leaf_count_here as usize;
            if end > leaves.len() {
                return Err(PropLumpError::Corrupt {
                    map: map.to_owned(),
                    what: format!(
                        "prop {i} names leaves {first_leaf}..{end} of {}",
                        leaves.len()
                    ),
                });
            }

            props.push(StaticProp {
                origin,
                angles,
                model,
                first_leaf,
                leaf_count: leaf_count_here,
                solid,
                flags,
                skin,
                fade_min,
                fade_max,
                // `STATIC_PROP_USE_LIGHTING_ORIGIN` is what says the field
                // means anything; without it `vrad` wrote zeroes and the
                // model's own `illumposition` is the sample point. Resolving
                // that needs the `.mdl`, so the raw field is carried and
                // [`StaticProp::lighting_origin`] decides.
                lighting_origin,
                forced_fade_scale,
                cpu_level: (min_cpu, max_cpu),
                gpu_level: (min_gpu, max_gpu),
                diffuse_modulation,
            });
        }

        Ok(StaticPropLump {
            models,
            leaves,
            props,
        })
    }
}

/// A forward-only reader over the lump's bytes.
///
/// The `sprp` payload is the one structure in this port that is genuinely
/// sequential — three arrays back to back with no offsets anywhere — so a
/// cursor is the honest shape for it, unlike the `.mdl`'s offset-chasing.
struct Cursor<'a> {
    map: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], PropLumpError> {
        let end = self.at.checked_add(n).ok_or_else(|| self.truncated(n))?;
        if end > self.bytes.len() {
            return Err(self.truncated(n));
        }
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn truncated(&self, n: usize) -> PropLumpError {
        PropLumpError::Truncated {
            map: self.map.to_owned(),
            what: format!(
                "{n} bytes wanted at offset {} of {}",
                self.at,
                self.bytes.len()
            ),
        }
    }

    fn u8(&mut self) -> Result<u8, PropLumpError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PropLumpError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("2 bytes")))
    }

    fn i32(&mut self) -> Result<i32, PropLumpError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn f32(&mut self) -> Result<f32, PropLumpError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn vec3(&mut self) -> Result<Vec3, PropLumpError> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }

    /// A count that the rest of the lump has to be big enough for.
    fn count(&mut self, what: &str) -> Result<usize, PropLumpError> {
        let n = self.i32()?;
        usize::try_from(n).map_err(|_| PropLumpError::Corrupt {
            map: self.map.to_owned(),
            what: format!("{what} declares {n} entries"),
        })
    }

    /// A fixed-width NUL-padded string, as the dictionary stores model names.
    fn fixed_string(&mut self, width: usize) -> Result<String, PropLumpError> {
        let bytes = self.take(width)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        // Valve wrote these with `strncpy` from a tool's command line and never
        // declared an encoding, so a stray high byte is a content quirk rather
        // than a reason to fail the map.
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }
}
