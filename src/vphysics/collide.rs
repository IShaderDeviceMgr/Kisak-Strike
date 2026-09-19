//! Reading Valve's collision models: `.phy` files and a `.bsp`'s
//! `LUMP_PHYSCOLLIDE`.
//!
//! `portdocs/VPHYSICS.md` §2 is the format and the evidence. The short form:
//! both are the same payload under different headers, and the payload is an
//! **IVP compact surface** — a binary tree of bounding volumes whose leaves
//! ("ledges") are convex hulls.
//!
//! This is the one part of `legacy/vphysics/` that has to be *ported* rather
//! than replaced. Every physics engine wants those hulls; no physics engine
//! can read them.
//!
//! # The layout, and where it is written down
//!
//! | struct | size | declared in |
//! |---|---|---|
//! | `phyheader_t` | 16 | `legacy/public/phyfile.h:14` |
//! | `dphysmodel_t` | 16 | `legacy/public/bspfile.h` |
//! | `compactsurfaceheader_t` | 28 | `legacy/vphysics/physics_collide.cpp:188` |
//! | `IVP_Compact_Surface` | 48 | `legacy/ivp/ivp_surface_manager/ivp_compact_surface.hxx:36` |
//! | `IVP_Compact_Ledgetree_Node` | 28 | `legacy/ivp/ivp_collision/ivp_compact_ledge.hxx:186` |
//! | `IVP_Compact_Ledge` | 16 | `legacy/ivp/ivp_collision/ivp_compact_ledge.hxx:121` |
//! | `IVP_Compact_Triangle` | 16 | `legacy/ivp/ivp_collision/ivp_compact_ledge.hxx:83` |
//! | `IVP_Compact_Poly_Point` | 16 | `legacy/ivp/ivp_collision/ivp_compact_ledge.hxx:26` |
//!
//! # Three traps, all of which cost a wrong answer before they were found
//!
//! 1. **`phyheader_t::id` is zero.** `studiomdl` writes a literal `0`
//!    (`utils/studiomdl/collisionmodel.cpp:2781`); the `'VPHY'` tag lives one
//!    level down, on each solid, and even there it is optional.
//! 2. **A ledgetree node is 28 bytes, not 48.** `left_son()` is `this + 1`,
//!    and a model whose root node is *terminal* — which most models are —
//!    parses correctly whatever size you guess. The world does not.
//! 3. **`IVP_Compact_Ledge::get_n_points()` is a guess and must not be used.**
//!    The header offers `size_div_16 - n_triangles - 1` behind
//!    `#if defined(LINUX) || …`, which is the warning that it is arithmetic
//!    about a layout rather than a field. Reading a world ledge's points that
//!    way produced a map sixteen units wide. [`Ledge`] gathers the indices the
//!    *triangles* name instead, which is what a convex hull wants anyway.

use glam::Vec3;

use crate::filesystem::keyvalues::{self, Block};

/// `METERS_PER_INCH` (`legacy/public/vphysics_interface.h:41`).
///
/// A compact surface's points, mass centre and rotational inertia are in IVP
/// units, which are **metres**; everything this port does is in Source units,
/// which are inches. `legacy/vphysics/convert.cpp:17` chooses between this and
/// `1.0` with an `#if 1`, and the branch the shipped game compiles is this one.
pub const METERS_PER_INCH: f32 = 0.0254;

/// The reciprocal, which is what the reader actually multiplies by, and also
/// [`crate::vphysics::env`]'s `length_unit`: **39.3701 Source units to the
/// metre**.
pub const UNITS_PER_METER: f32 = 1.0 / METERS_PER_INCH;

/// `IVP_COMPACT_SURFACE_ID` — `MAKEID('I','V','P','S')`
/// (`physics_collide.cpp:164`).
const IVPS: i32 = i32::from_le_bytes(*b"IVPS");
/// `IVP_COMPACT_SURFACE_ID_SWAPPED`, the big-endian console form.
const SPVI: i32 = i32::from_le_bytes(*b"SPVI");
/// `VPHYSICS_COLLISION_ID` — `MAKEID('V','P','H','Y')`.
const VPHY: i32 = i32::from_le_bytes(*b"VPHY");
/// `IVP_COMPACT_MOPP_ID`. Recognised only in order to refuse it — see
/// [`CollideError::Mopp`].
const MOPP: i32 = i32::from_le_bytes(*b"MOPP");

/// `COLLIDE_POLY` (`legacy/vphysics/physics_trace.h:27`).
const COLLIDE_POLY: i16 = 0;
/// `COLLIDE_MOPP`.
const COLLIDE_MOPP: i16 = 1;

/// `IVP_MAX_TRIANGLES_PER_LEDGE` (`ivp_compact_ledge.hxx:38`).
const MAX_TRIANGLES_PER_LEDGE: i32 = 8192;

/// A sanity bound on the ledge tree walk. The largest shipped surface,
/// `sp_a4_finale4`'s worldspawn, has a few thousand; this is four orders of
/// magnitude of headroom and exists only so that a corrupt `offset_right_node`
/// cannot loop for ever.
const MAX_LEDGES: usize = 1 << 20;

/// Why a collision model could not be read.
#[derive(Debug, thiserror::Error)]
pub enum CollideError {
    #[error("{what}'s collision data is truncated: wanted {wanted} bytes at {at}, have {have}")]
    Truncated {
        what: String,
        at: usize,
        wanted: usize,
        have: usize,
    },

    #[error("{what}'s collision data is not a .phy: header size is {size}, expected 16")]
    NotPhy { what: String, size: i32 },

    #[error("{what}'s collision data is internally inconsistent: {why}")]
    Corrupt { what: String, why: String },

    /// Havok's compressed mesh. **The shipped game cannot read one either** —
    /// `ENABLE_IVP_MOPP` is `0` at `physics_collide.cpp:169`, and
    /// `UnserializeFromBuffer` returns `NULL` with a `"Null physics model"`
    /// message. Refused here rather than silently dropped so that a map that
    /// somehow contains one says so.
    #[error("{what} is a Havok MOPP collision model, which the shipped game does not read either")]
    Mopp { what: String },

    #[error("{what}'s collision keydata: {0}", .source)]
    Keys {
        what: String,
        #[source]
        source: crate::filesystem::error::VfsError,
    },
}

type Result<T> = std::result::Result<T, CollideError>;

/// One convex piece of a collision model — `IVP_Compact_Ledge`.
///
/// Points are in **Source units, in the model's own frame**, already through
/// the axis swap of [`ivp_to_source`]. They are deduplicated: a ledge's point
/// array is shared with its siblings in the file and typically holds points
/// this ledge does not use, so [`points`](Ledge::points) holds only the ones
/// its triangles name and [`triangles`](Ledge::triangles) is re-indexed onto
/// them.
#[derive(Debug, Clone, PartialEq)]
pub struct Ledge {
    pub points: Vec<Vec3>,
    /// Triangles, in the file's winding. A convex hull does not care, and
    /// nothing here reads them except [`Ledge::points`]'s own construction and
    /// the tests — but they are the only record of which points form a face,
    /// and re-deriving that costs a hull computation.
    pub triangles: Vec<[u16; 3]>,
    /// `IVP_Compact_Triangle::material_index`, of the ledge's **first**
    /// triangle.
    ///
    /// > **A ledge is not guaranteed to have one material and the world's
    /// > usually does not.** Measured over the 106 shipped maps: **73,856 of
    /// > 115,225 world ledges name more than one**, against **0 of 4,643**
    /// > model ledges. A collider has one friction coefficient, so this is
    /// > the best single answer available and `portdocs/VPHYSICS.md` §3.5
    /// > bounds what it costs.
    pub material: u8,
    /// Whether any triangle disagreed with [`material`](Ledge::material).
    pub mixed_materials: bool,
}

/// One `CPhysCollide` — an `IVP_Compact_Surface` and the tree under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Solid {
    /// `IVP_Compact_Surface::mass_center`, in Source units in the model's
    /// frame.
    pub mass_center: Vec3,
    /// `IVP_Compact_Surface::rotation_inertia`, **converted to in² but
    /// otherwise exactly as stored**.
    ///
    /// > **This is not a moment of inertia**, however much it looks like one.
    /// > `IVP_Rot_Inertia_Solver` writes `sqrt(⟨y²⟩² + ⟨z²⟩²)` where the
    /// > moment is `⟨y²⟩ + ⟨z²⟩` — see [`crate::vphysics::env`] and
    /// > `portdocs/VPHYSICS.md` §3.3, which derives the √2/2 and checks it
    /// > against `metal_box.phy` to four figures. It is reproduced rather than
    /// > corrected because Portal 2 is tuned against it.
    pub rotation_inertia: Vec3,
    /// `IVP_Compact_Surface::upper_limit_radius`, in Source units — the radius
    /// of a sphere about the mass centre that contains the whole solid.
    pub radius: f32,
    /// The leaves of the ledge tree, in tree order.
    pub ledges: Vec<Ledge>,
}

/// Inspection helpers. Nothing in the engine calls these — a body is built
/// from [`Solid::ledges`] directly — but they are what a test, a console
/// command or anyone debugging a model reaches for, and deriving a bound at
/// the call site is how two callers come to disagree about what it is.
#[allow(dead_code)]
impl Solid {
    /// Every point of every ledge, which is what a bounding box is taken over.
    pub fn points(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.ledges.iter().flat_map(|l| l.points.iter().copied())
    }

    /// The axis-aligned bounds of the whole solid, or `None` if it has no
    /// points at all.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut points = self.points();
        let first = points.next()?;
        Some(points.fold((first, first), |(mn, mx), p| (mn.min(p), mx.max(p))))
    }

    pub fn triangle_count(&self) -> usize {
        self.ledges.iter().map(|l| l.triangles.len()).sum()
    }
}

/// A whole collision model: `vcollide_t`.
///
/// One `.phy` file, or one entry of a `.bsp`'s `LUMP_PHYSCOLLIDE`.
#[derive(Debug, Clone, PartialEq)]
pub struct VCollide {
    pub solids: Vec<Solid>,
    /// The text tail — `solid { }`, `staticsolid { }`, `materialtable { }`,
    /// `virtualterrain {}` and friends.
    ///
    /// > **`legacy/vphysics/vcollide_parse.cpp` is deleted, all 1,040 lines of
    /// > it.** It is a KeyValues reader written from scratch because vphysics
    /// > was a separate `.so` that could not link `tier1`. One binary has no
    /// > such excuse, and [`crate::filesystem::keyvalues`] reads the same
    /// > text — including the duplicate top-level keys the world's four
    /// > `staticsolid` blocks are, which is exactly the case that parser
    /// > already had to handle for `gameinfo.txt`'s `SearchPaths`.
    pub keys: Block,
    /// `phyheader_t::checkSum` — of the source `.mdl`. Always 0 for a lump-29
    /// entry, which has no such field.
    pub checksum: i32,
}

impl VCollide {
    /// Reads a `.phy` file.
    ///
    /// `what` names the file for error messages only.
    pub fn read_phy(what: &str, bytes: &[u8]) -> Result<VCollide> {
        let r = Reader::new(what, bytes);
        // `phyheader_t`. `size` is `sizeof(phyheader_t)` and doubles as the
        // version; `id` is a literal zero and is not a magic number.
        let size = r.i32_at(0)?;
        let _id = r.i32_at(4)?;
        let solid_count = r.i32_at(8)?;
        let checksum = r.i32_at(12)?;
        if size != 16 {
            return Err(CollideError::NotPhy {
                what: what.to_owned(),
                size,
            });
        }
        let (solids, at) = read_solids(&r, size as usize, solid_count)?;
        let keys = r.keys_from(at)?;
        Ok(VCollide {
            solids,
            keys,
            checksum,
        })
    }

    /// Reads a `.bsp`'s `LUMP_PHYSCOLLIDE`, returning one entry per brush
    /// model.
    ///
    /// The key is `dphysmodel_t::modelIndex`, which is the **brush model
    /// number**: 0 is worldspawn, and the engine's model index — the `N` of a
    /// `"*N"` model name — is the same number. (Valve's `modelinfo` index is
    /// this plus one, because index 0 there is the level itself.)
    ///
    /// A `modelIndex` of -1 terminates the lump, which is how `vbsp` writes
    /// it; a lump that simply runs out is accepted too, because the terminator
    /// is not in the struct and nothing else depends on it.
    pub fn read_lump(what: &str, bytes: &[u8]) -> Result<Vec<(usize, VCollide)>> {
        let r = Reader::new(what, bytes);
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 16 <= bytes.len() {
            let model_index = r.i32_at(at)?;
            if model_index < 0 {
                break;
            }
            let data_size = r.i32_at(at + 4)?;
            let keydata_size = r.i32_at(at + 8)?;
            let solid_count = r.i32_at(at + 12)?;
            if data_size < 0 || keydata_size < 0 {
                return Err(CollideError::Corrupt {
                    what: what.to_owned(),
                    why: format!(
                        "brush model {model_index} declares {data_size} bytes of solids \
                         and {keydata_size} of keydata"
                    ),
                });
            }
            let body_at = at + 16;
            let keys_at = body_at + data_size as usize;
            let end = keys_at + keydata_size as usize;
            if end > bytes.len() {
                return Err(CollideError::Truncated {
                    what: what.to_owned(),
                    at: body_at,
                    wanted: end - body_at,
                    have: bytes.len().saturating_sub(body_at),
                });
            }
            let (solids, _) = read_solids(&r, body_at, solid_count)?;
            let keys = r.keys_in(keys_at, end)?;
            out.push((
                model_index as usize,
                VCollide {
                    solids,
                    keys,
                    checksum: 0,
                },
            ));
            at = end;
        }
        Ok(out)
    }

    /// The first `solid { }` block, which is the one
    /// `PhysModelParseSolidByIndex` takes when no index is asked for
    /// (`physics_shared.cpp:183`).
    pub fn solid_params(&self) -> SolidParams {
        self.keys
            .entries()
            .iter()
            .find(|e| e.key.eq_ignore_ascii_case("solid"))
            .and_then(|e| match &e.value {
                keyvalues::Value::Block(b) => Some(SolidParams::from_block(b)),
                keyvalues::Value::String(_) => None,
            })
            .unwrap_or_default()
    }

    /// Every `staticsolid { }` block, as `(index, contents)` — the world's
    /// description of its own solids (`PhysCreateWorld_Shared`).
    pub fn static_solids(&self) -> Vec<(usize, u32)> {
        self.keys
            .entries()
            .iter()
            .filter(|e| e.key.eq_ignore_ascii_case("staticsolid"))
            .filter_map(|e| match &e.value {
                keyvalues::Value::Block(b) => {
                    let index = b.find_string("index").and_then(|s| s.parse().ok())?;
                    let contents = b
                        .find_string("contents")
                        .and_then(|s| s.parse::<i64>().ok())
                        .unwrap_or(0) as u32;
                    Some((index, contents))
                }
                keyvalues::Value::String(_) => None,
            })
            .collect()
    }

    /// Whether the keydata carries a `virtualterrain` block — the world's
    /// statement that its displacements are *not* in this lump. See
    /// `portdocs/VPHYSICS.md` §2.5; **all 106 shipped maps set it**.
    pub fn virtual_terrain(&self) -> bool {
        self.keys
            .entries()
            .iter()
            .any(|e| e.key.eq_ignore_ascii_case("virtualterrain"))
    }

    /// The `materialtable { }` block — the map's mapping from a triangle's
    /// seven-bit `material_index` to a surface property name.
    ///
    /// `SetWorldMaterialIndexTable` (`physics_material.cpp:615`). Returned as
    /// `(index, name)` pairs in file order; the same name appears twice in
    /// every shipped map, which is why this is not a map.
    ///
    /// **Read by nothing yet** — it is the data behind the one divergence
    /// `rustdocs/VPHYSICS.md` gotcha 7 records, and the first thing a
    /// per-triangle material would need.
    #[allow(dead_code)]
    pub fn material_table(&self) -> Vec<(usize, String)> {
        self.keys
            .entries()
            .iter()
            .find(|e| e.key.eq_ignore_ascii_case("materialtable"))
            .and_then(|e| match &e.value {
                keyvalues::Value::Block(b) => Some(b),
                keyvalues::Value::String(_) => None,
            })
            .map(|b| {
                b.values()
                    .filter_map(|(name, index)| Some((index.parse().ok()?, name.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The `solid { }` block — `solid_t` (`legacy/public/vcollide_parse.h`) minus
/// everything no consumer here reads.
///
/// Defaults are `g_PhysDefaultObjectParams` (`physics_shared.cpp:43`), which
/// is what `CSolidSetDefaults::SetDefaults` splats over the struct before
/// parsing.
#[derive(Debug, Clone, PartialEq)]
pub struct SolidParams {
    /// Which [`Solid`] of the [`VCollide`] this block describes.
    pub index: usize,
    /// Kilograms.
    pub mass: f32,
    /// The multiplier on [`Solid::rotation_inertia`], **not an inertia**.
    pub inertia: f32,
    /// `objectparams_t::damping` — linear.
    pub damping: f32,
    /// `objectparams_t::rotdamping` — angular.
    pub rot_damping: f32,
    /// The surface property name, `""` when the block does not name one.
    pub surface_prop: String,
    /// `"volume"`, in in³. Informational: Valve computes its own from the hull
    /// when this is zero, and so does Rapier.
    pub volume: f32,
    /// `"name"` — the QC's name for the solid, for diagnostics.
    pub name: String,
}

impl Default for SolidParams {
    fn default() -> SolidParams {
        SolidParams {
            index: 0,
            mass: 1.0,
            inertia: 1.0,
            damping: 0.1,
            rot_damping: 0.1,
            surface_prop: String::new(),
            volume: 0.0,
            name: String::new(),
        }
    }
}

impl SolidParams {
    fn from_block(b: &Block) -> SolidParams {
        let mut out = SolidParams::default();
        let num = |key: &str| b.find_string(key).and_then(|s| s.trim().parse::<f32>().ok());
        if let Some(v) = num("index") {
            out.index = v.max(0.0) as usize;
        }
        if let Some(v) = num("mass") {
            out.mass = v;
        }
        if let Some(v) = num("inertia") {
            out.inertia = v;
        }
        if let Some(v) = num("damping") {
            out.damping = v;
        }
        if let Some(v) = num("rotdamping") {
            out.rot_damping = v;
        }
        if let Some(v) = num("volume") {
            out.volume = v;
        }
        if let Some(s) = b.find_string("surfaceprop") {
            out.surface_prop = s.to_owned();
        }
        if let Some(s) = b.find_string("name") {
            out.name = s.to_owned();
        }
        out
    }
}

/// `ConvertPositionToHL` (`legacy/vphysics/convert.h:150`) — IVP metres to
/// Source units.
///
/// `ConvertPositionToIVP` is `(x, −z, y) × METERS_PER_INCH`, so this is its
/// inverse. **The axis map's determinant is +1**, so it is a rotation:
/// handedness is preserved and triangle winding survives it unchanged.
#[inline]
pub fn ivp_to_source(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, z, -y) * UNITS_PER_METER
}

/// The same map for a quantity that is a **direction rather than a position**,
/// which in IVP means it is not scaled — `ConvertDirectionToHL`
/// (`convert.h:184`).
#[inline]
pub fn ivp_direction_to_source(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, z, -y)
}

/// Reads `count` solids starting at `at`, returning them and the offset just
/// past the last one.
fn read_solids(r: &Reader<'_>, at: usize, count: i32) -> Result<(Vec<Solid>, usize)> {
    if count < 0 {
        return Err(CollideError::Corrupt {
            what: r.what.to_owned(),
            why: format!("a negative solid count ({count})"),
        });
    }
    let mut solids = Vec::with_capacity(count.min(64) as usize);
    let mut at = at;
    for i in 0..count {
        let size = r.i32_at(at)?;
        at += 4;
        if size < 0 || at + size as usize > r.bytes.len() {
            return Err(CollideError::Truncated {
                what: r.what.to_owned(),
                at,
                wanted: size.max(0) as usize,
                have: r.bytes.len().saturating_sub(at),
            });
        }
        solids.push(read_solid(r, at, size as usize, i)?);
        at += size as usize;
    }
    Ok((solids, at))
}

/// `CPhysCollide::UnserializeFromBuffer` (`physics_collide.cpp:318`) — sniff
/// the two container layouts, then read the compact surface.
fn read_solid(r: &Reader<'_>, at: usize, size: usize, which: i32) -> Result<Solid> {
    let tag = r.i32_at(at)?;
    if tag == VPHY {
        // `compactsurfaceheader_t`: the four-byte tag, two shorts, then
        // `surfaceSize`, `dragAxisAreas` and `axisMapSize` — 28 bytes.
        let version = r.i16_at(at + 4)?;
        let model_type = r.i16_at(at + 6)?;
        let surface_size = r.i32_at(at + 8)?;
        let _ = version;
        if model_type == COLLIDE_MOPP {
            return Err(CollideError::Mopp {
                what: r.what.to_owned(),
            });
        }
        if model_type != COLLIDE_POLY {
            return Err(CollideError::Corrupt {
                what: r.what.to_owned(),
                why: format!("solid {which} has collision model type {model_type}"),
            });
        }
        return read_compact_surface(r, at + 28, surface_size.max(0) as usize, which);
    }
    // A bare `IVP_Compact_Surface`, identified by `dummy[2]`. Zero is the
    // "old format .PHY" case `UnserializeFromBuffer` warns about and reads
    // anyway.
    let dummy2 = r.i32_at(at + 44)?;
    match dummy2 {
        MOPP => Err(CollideError::Mopp {
            what: r.what.to_owned(),
        }),
        IVPS | SPVI | 0 => read_compact_surface(r, at, size, which),
        other => Err(CollideError::Corrupt {
            what: r.what.to_owned(),
            why: format!("solid {which} is tagged {other:#010x}, which is neither IVPS nor VPHY"),
        }),
    }
}

/// `IVP_Compact_Surface` and the ledge tree hanging off it.
fn read_compact_surface(r: &Reader<'_>, base: usize, size: usize, which: i32) -> Result<Solid> {
    let mass_center = ivp_to_source(r.f32_at(base)?, r.f32_at(base + 4)?, r.f32_at(base + 8)?);
    // The rotational inertia is a per-unit-mass second moment in m², so it
    // rides the *direction* map (no scale, because it is not a length) and is
    // then converted to in² by hand. Valve's `GetInertia` takes the absolute
    // value of each component after the swap, which is why the sign the map
    // introduces does not matter.
    let inertia = ivp_direction_to_source(
        r.f32_at(base + 12)?,
        r.f32_at(base + 16)?,
        r.f32_at(base + 20)?,
    )
    .abs()
        * (UNITS_PER_METER * UNITS_PER_METER);
    let radius = r.f32_at(base + 24)? * UNITS_PER_METER;
    let packed = r.u32_at(base + 28)?;
    let byte_size = (packed >> 8) as usize;
    let root = r.i32_at(base + 32)?;

    if byte_size != 0 && size != 0 && byte_size != size {
        // Not fatal: `read_solid` already bounded the read, and a mismatch has
        // never been seen in the shipped depot. It is recorded here because it
        // is the first thing to look at if a model ever parses into nonsense.
        return Err(CollideError::Corrupt {
            what: r.what.to_owned(),
            why: format!(
                "solid {which} says it is {byte_size} bytes, but its container says {size}"
            ),
        });
    }

    let mut ledges = Vec::new();
    let mut stack = vec![offset(r, base, root, "the ledge tree root")?];
    while let Some(node) = stack.pop() {
        if ledges.len() + stack.len() > MAX_LEDGES {
            return Err(CollideError::Corrupt {
                what: r.what.to_owned(),
                why: format!("solid {which}'s ledge tree does not terminate"),
            });
        }
        let right = r.i32_at(node)?;
        let ledge = r.i32_at(node + 4)?;
        if right == 0 {
            ledges.push(read_ledge(r, offset(r, node, ledge, "a terminal ledge")?)?);
        } else {
            // `right_son()` before `left_son()`, because this is a stack and
            // the tree order is what `IVP_Compact_Ledge_Solver::get_all_ledges`
            // produces — left first. Nothing depends on the order, but a
            // depot test that asserts an exact ledge count is easier to trust
            // when it matches the reference's walk.
            stack.push(offset(r, node, right, "a ledgetree right son")?);
            stack.push(node + 28);
        }
    }
    Ok(Solid {
        mass_center,
        rotation_inertia: inertia,
        radius,
        ledges,
    })
}

/// One `IVP_Compact_Ledge` and its triangles.
fn read_ledge(r: &Reader<'_>, at: usize) -> Result<Ledge> {
    let point_offset = r.i32_at(at)?;
    let packed = r.u32_at(at + 8)?;
    let _size_div_16 = packed >> 8;
    let n_triangles = r.i16_at(at + 12)? as i32;
    if !(0..=MAX_TRIANGLES_PER_LEDGE).contains(&n_triangles) {
        return Err(CollideError::Corrupt {
            what: r.what.to_owned(),
            why: format!("a ledge with {n_triangles} triangles"),
        });
    }
    let points_at = offset(r, at, point_offset, "a ledge's point array")?;

    // Two passes: gather the indices the triangles name, then read exactly
    // those points. See the module docs, trap 3 — the ledge's own point count
    // is not a field and the array is shared between siblings.
    let mut triangles: Vec<[u16; 3]> = Vec::with_capacity(n_triangles as usize);
    let mut material = 0u8;
    let mut mixed = false;
    let mut order: Vec<u16> = Vec::new();
    for t in 0..n_triangles as usize {
        let tri = at + 16 + t * 16;
        let word = r.u32_at(tri)?;
        let tri_material = ((word >> 24) & 0x7f) as u8;
        if t == 0 {
            material = tri_material;
        } else if tri_material != material {
            mixed = true;
        }
        let mut corners = [0u16; 3];
        for (e, corner) in corners.iter_mut().enumerate() {
            // `IVP_Compact_Edge`: `start_point_index:16` is the low half.
            let edge = r.u32_at(tri + 4 + e * 4)?;
            let index = (edge & 0xffff) as u16;
            *corner = index;
            if !order.contains(&index) {
                order.push(index);
            }
        }
        triangles.push(corners);
    }

    let mut points = Vec::with_capacity(order.len());
    for &index in &order {
        let p = points_at + index as usize * 16;
        points.push(ivp_to_source(r.f32_at(p)?, r.f32_at(p + 4)?, r.f32_at(p + 8)?));
    }
    // Re-index onto the dense array.
    for corners in &mut triangles {
        for corner in corners.iter_mut() {
            *corner = order
                .iter()
                .position(|&i| i == *corner)
                .expect("every corner was pushed into `order` above") as u16;
        }
    }

    Ok(Ledge {
        points,
        triangles,
        material,
        mixed_materials: mixed,
    })
}

/// Applies one of IVP's signed byte offsets, which are always relative to the
/// struct that holds them.
fn offset(r: &Reader<'_>, from: usize, delta: i32, what: &str) -> Result<usize> {
    let at = from as i64 + delta as i64;
    if at < 0 || at as usize >= r.bytes.len() {
        return Err(CollideError::Corrupt {
            what: r.what.to_owned(),
            why: format!("{what} is at {delta} from {from}, which is outside the solid"),
        });
    }
    Ok(at as usize)
}

/// A bounds-checked cursor. Every read in this module goes through one, so no
/// malformed file can do worse than produce a [`CollideError`].
struct Reader<'a> {
    what: &'a str,
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(what: &'a str, bytes: &'a [u8]) -> Reader<'a> {
        Reader { what, bytes }
    }

    fn take<const N: usize>(&self, at: usize) -> Result<[u8; N]> {
        self.bytes
            .get(at..at + N)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| CollideError::Truncated {
                what: self.what.to_owned(),
                at,
                wanted: N,
                have: self.bytes.len().saturating_sub(at),
            })
    }

    fn i16_at(&self, at: usize) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take(at)?))
    }

    fn i32_at(&self, at: usize) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(at)?))
    }

    fn u32_at(&self, at: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(at)?))
    }

    fn f32_at(&self, at: usize) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(at)?))
    }

    /// The keydata tail of a `.phy`: everything from `at` to the first NUL, or
    /// to the end.
    fn keys_from(&self, at: usize) -> Result<Block> {
        self.keys_in(at, self.bytes.len())
    }

    fn keys_in(&self, at: usize, end: usize) -> Result<Block> {
        let end = end.min(self.bytes.len());
        if at >= end {
            return Ok(Block::default());
        }
        let slice = &self.bytes[at..end];
        let slice = match slice.iter().position(|&b| b == 0) {
            Some(nul) => &slice[..nul],
            None => slice,
        };
        let text = String::from_utf8_lossy(slice);
        keyvalues::parse(self.what, &text).map_err(|source| CollideError::Keys {
            what: self.what.to_owned(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `.phy` in memory from a box, laid out exactly the way
    /// `studiomdl` writes one.
    ///
    /// This exists because the reader's whole job is byte offsets, and a test
    /// that reads a file the test itself wrote is the only way to check them
    /// without a copy of the game. The depot test in
    /// [`crate::vphysics`](super::super) checks the same reader against 1,056
    /// real ones.
    struct PhyWriter {
        /// Half-extents **in Source units**, which the writer converts to IVP
        /// metres on the way in — so a test states what it means.
        half: Vec3,
        mass_center: Vec3,
        rotation_inertia: Vec3,
        keys: &'static str,
        /// Use the bare `IVP_Compact_Surface` layout rather than wrapping it
        /// in a `compactsurfaceheader_t`. Both ship.
        bare: bool,
        material: u8,
    }

    impl Default for PhyWriter {
        fn default() -> PhyWriter {
            PhyWriter {
                half: Vec3::splat(16.0),
                mass_center: Vec3::ZERO,
                rotation_inertia: Vec3::splat(1.0),
                keys: "solid {\n\"index\" \"0\"\n\"mass\" \"40\"\n\"surfaceprop\" \"metal\"\n}\n",
                bare: false,
                material: 0,
            }
        }
    }

    /// `ConvertPositionToIVP` — the writer's side of [`ivp_to_source`].
    fn source_to_ivp(v: Vec3) -> [f32; 3] {
        [
            v.x * METERS_PER_INCH,
            -v.z * METERS_PER_INCH,
            v.y * METERS_PER_INCH,
        ]
    }

    /// The eight corners and twelve triangles of a box, as IVP writes them.
    fn box_corners(half: Vec3) -> ([Vec3; 8], [[u16; 3]; 12]) {
        let mut corners = [Vec3::ZERO; 8];
        for (i, corner) in corners.iter_mut().enumerate() {
            *corner = Vec3::new(
                if i & 1 == 0 { -half.x } else { half.x },
                if i & 2 == 0 { -half.y } else { half.y },
                if i & 4 == 0 { -half.z } else { half.z },
            );
        }
        let faces = [
            [0, 2, 3],
            [0, 3, 1],
            [4, 5, 7],
            [4, 7, 6],
            [0, 1, 5],
            [0, 5, 4],
            [2, 6, 7],
            [2, 7, 3],
            [0, 4, 6],
            [0, 6, 2],
            [1, 3, 7],
            [1, 7, 5],
        ];
        (corners, faces)
    }

    impl PhyWriter {
        fn write(&self) -> Vec<u8> {
            let (corners, faces) = box_corners(self.half);

            // The compact surface, built relative to its own base.
            let mut surface: Vec<u8> = Vec::new();
            let push_f32 = |out: &mut Vec<u8>, v: f32| out.extend_from_slice(&v.to_le_bytes());
            let push_i32 = |out: &mut Vec<u8>, v: i32| out.extend_from_slice(&v.to_le_bytes());

            let node_at = 48i32;
            let ledge_at = node_at + 28;
            let tris_at = ledge_at + 16;
            let points_at = tris_at + 16 * faces.len() as i32;
            let total = points_at + 16 * corners.len() as i32;

            for c in source_to_ivp(self.mass_center) {
                push_f32(&mut surface, c);
            }
            // `rotation_inertia` rides the direction map, which is the axis
            // swap without the scale.
            let ri = self.rotation_inertia * METERS_PER_INCH * METERS_PER_INCH;
            for c in [ri.x, -ri.z, ri.y] {
                push_f32(&mut surface, c);
            }
            push_f32(&mut surface, self.half.length() * METERS_PER_INCH);
            // max_factor_surface_deviation in the low byte, byte_size above.
            push_i32(&mut surface, ((total as u32) << 8 | 0xcd) as i32);
            push_i32(&mut surface, node_at);
            push_i32(&mut surface, 0);
            push_i32(&mut surface, 0);
            push_i32(&mut surface, IVPS);
            assert_eq!(surface.len(), 48);

            // The ledgetree root, terminal.
            push_i32(&mut surface, 0);
            push_i32(&mut surface, ledge_at - node_at);
            for _ in 0..3 {
                push_f32(&mut surface, 0.0);
            }
            push_f32(&mut surface, 1.0);
            surface.extend_from_slice(&[0, 0, 0, 0]);
            assert_eq!(surface.len(), ledge_at as usize);

            // The ledge.
            push_i32(&mut surface, points_at - ledge_at);
            push_i32(&mut surface, 0);
            push_i32(&mut surface, (((total / 16) as u32) << 8) as i32);
            surface.extend_from_slice(&(faces.len() as i16).to_le_bytes());
            surface.extend_from_slice(&0i16.to_le_bytes());
            assert_eq!(surface.len(), tris_at as usize);

            for (t, face) in faces.iter().enumerate() {
                let word = (t as u32 & 0xfff) | (self.material as u32 & 0x7f) << 24;
                surface.extend_from_slice(&word.to_le_bytes());
                for &corner in face {
                    surface.extend_from_slice(&(corner as u32).to_le_bytes());
                }
            }
            assert_eq!(surface.len(), points_at as usize);

            for corner in corners {
                for c in source_to_ivp(corner) {
                    push_f32(&mut surface, c);
                }
                // The fourth float of an `IVP_U_Float_Hesse` is not a
                // coordinate, and a reader that treated it as one would read
                // every point at the wrong stride.
                push_f32(&mut surface, 12345.0);
            }
            assert_eq!(surface.len(), total as usize);

            // The container.
            let mut solid: Vec<u8> = Vec::new();
            if !self.bare {
                solid.extend_from_slice(&VPHY.to_le_bytes());
                solid.extend_from_slice(&0x0100i16.to_le_bytes());
                solid.extend_from_slice(&COLLIDE_POLY.to_le_bytes());
                solid.extend_from_slice(&(surface.len() as i32).to_le_bytes());
                for _ in 0..3 {
                    solid.extend_from_slice(&1.0f32.to_le_bytes());
                }
                solid.extend_from_slice(&0i32.to_le_bytes());
                assert_eq!(solid.len(), 28);
            }
            solid.extend_from_slice(&surface);

            let mut out: Vec<u8> = Vec::new();
            out.extend_from_slice(&16i32.to_le_bytes());
            out.extend_from_slice(&0i32.to_le_bytes());
            out.extend_from_slice(&1i32.to_le_bytes());
            out.extend_from_slice(&0x1234i32.to_le_bytes());
            out.extend_from_slice(&(solid.len() as i32).to_le_bytes());
            out.extend_from_slice(&solid);
            out.extend_from_slice(self.keys.as_bytes());
            out.push(0);
            out
        }
    }

    fn approx(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-3
    }

    #[test]
    fn a_box_round_trips_through_the_compact_surface_layout() {
        let bytes = PhyWriter::default().write();
        let phy = VCollide::read_phy("box.phy", &bytes).expect("a .phy this test wrote");
        assert_eq!(phy.checksum, 0x1234);
        assert_eq!(phy.solids.len(), 1);
        let solid = &phy.solids[0];
        assert_eq!(solid.ledges.len(), 1, "one terminal root node, one ledge");
        assert_eq!(solid.ledges[0].points.len(), 8);
        assert_eq!(solid.triangle_count(), 12);
        let (mins, maxs) = solid.bounds().expect("eight points");
        assert!(approx(mins, Vec3::splat(-16.0)), "{mins}");
        assert!(approx(maxs, Vec3::splat(16.0)), "{maxs}");
    }

    #[test]
    fn the_bare_ivps_layout_reads_the_same_as_the_vphy_one() {
        let wrapped = VCollide::read_phy("a.phy", &PhyWriter::default().write()).unwrap();
        let bare = VCollide::read_phy(
            "b.phy",
            &PhyWriter {
                bare: true,
                ..Default::default()
            }
            .write(),
        )
        .unwrap();
        assert_eq!(wrapped.solids, bare.solids);
    }

    /// The whole of `portdocs/VPHYSICS.md` §2.4 in one assertion: a point that
    /// is different on all three axes, so that a swapped or negated component
    /// cannot hide.
    #[test]
    fn ivp_space_is_metres_with_y_and_z_exchanged_and_one_of_them_negated() {
        assert!(approx(
            ivp_to_source(1.0, 2.0, 3.0),
            Vec3::new(39.3701, 118.1102, -78.7402)
        ));
        // And it is a rotation, not a reflection: winding survives it.
        let m = glam::Mat3::from_cols(
            ivp_to_source(1.0, 0.0, 0.0),
            ivp_to_source(0.0, 1.0, 0.0),
            ivp_to_source(0.0, 0.0, 1.0),
        );
        assert!(m.determinant() > 0.0, "{}", m.determinant());
    }

    #[test]
    fn a_ledges_points_come_from_the_indices_its_triangles_name() {
        // The writer emits eight points and twelve triangles that use all
        // eight. Dropping two faces leaves two corners unreferenced, and the
        // reader must produce six points rather than eight — which is the
        // whole reason `get_n_points()` is not used.
        let mut bytes = PhyWriter::default().write();
        // Rewrite `n_triangles` from 12 to 10, which removes the two faces
        // that are the only users of corners 1 and 7... in fact the last two
        // faces use 1, 3, 7 and 1, 7, 5, so dropping them frees nothing on its
        // own; assert on what it does free.
        let ledge = 16 + 4 + 28 + 48 + 28;
        bytes[ledge + 12..ledge + 14].copy_from_slice(&2i16.to_le_bytes());
        let phy = VCollide::read_phy("box.phy", &bytes).unwrap();
        let ledge = &phy.solids[0].ledges[0];
        assert_eq!(ledge.triangles.len(), 2);
        assert_eq!(
            ledge.points.len(),
            4,
            "the first two faces name corners 0, 2, 3 and 0, 3, 1"
        );
        for tri in &ledge.triangles {
            for &corner in tri {
                assert!(
                    (corner as usize) < ledge.points.len(),
                    "every triangle is re-indexed onto the dense array"
                );
            }
        }
    }

    #[test]
    fn the_keydata_tail_is_ordinary_key_values() {
        let phy = VCollide::read_phy("box.phy", &PhyWriter::default().write()).unwrap();
        let params = phy.solid_params();
        assert_eq!(params.index, 0);
        assert_eq!(params.mass, 40.0);
        assert_eq!(params.surface_prop, "metal");
        // The defaults `g_PhysDefaultObjectParams` supplies for what the block
        // does not say.
        assert_eq!(params.inertia, 1.0);
        assert_eq!(params.damping, 0.1);
        assert_eq!(params.rot_damping, 0.1);
    }

    #[test]
    fn a_solid_with_no_solid_block_still_has_valves_defaults() {
        let phy = VCollide::read_phy(
            "box.phy",
            &PhyWriter {
                keys: "editparams {\n\"rootname\" \"\"\n}\n",
                ..Default::default()
            }
            .write(),
        )
        .unwrap();
        assert_eq!(phy.solid_params(), SolidParams::default());
    }

    #[test]
    fn the_world_keydata_answers_the_three_questions_phys_create_world_asks() {
        let keys = concat!(
            "staticsolid {\n\"index\" \"0\"\n\"contents\" \"33570819\"\n}\n",
            "staticsolid {\n\"index\" \"1\"\n\"contents\" \"8\"\n}\n",
            "virtualterrain {}\n",
            "materialtable {\n\"default\" \"1\"\n\"concrete\" \"3\"\n}\n",
        );
        let phy = VCollide::read_phy(
            "world.phy",
            &PhyWriter {
                keys: Box::leak(keys.to_owned().into_boxed_str()),
                ..Default::default()
            }
            .write(),
        )
        .unwrap();
        assert_eq!(phy.static_solids(), vec![(0, 33_570_819), (1, 8)]);
        assert!(phy.virtual_terrain());
        assert_eq!(
            phy.material_table(),
            vec![(1, "default".to_owned()), (3, "concrete".to_owned())]
        );
    }

    #[test]
    fn a_truncated_file_is_an_error_rather_than_a_panic() {
        let full = PhyWriter::default().write();
        for cut in 0..full.len() {
            // Every prefix must either parse or fail; none may panic, and none
            // may read past the end. (Short prefixes can parse "successfully"
            // into zero solids, which is fine — they are not malformed, only
            // empty.)
            let _ = VCollide::read_phy("cut.phy", &full[..cut]);
        }
    }

    #[test]
    fn a_ledge_tree_that_points_at_itself_is_refused_rather_than_looped_on() {
        let mut bytes = PhyWriter::default().write();
        // Make the root node non-terminal with a right son that is the root.
        let node = 16 + 4 + 28 + 48;
        bytes[node..node + 4].copy_from_slice(&0i32.to_le_bytes());
        // `offset_right_node` of zero is terminal, so use a self-reference of
        // one byte instead, which is a cycle of length one.
        bytes[node..node + 4].copy_from_slice(&0i32.to_le_bytes());
        let ok = VCollide::read_phy("loop.phy", &bytes);
        assert!(ok.is_ok(), "a terminal root is still fine");

        bytes[node..node + 4].copy_from_slice(&(-28i32).to_le_bytes());
        match VCollide::read_phy("loop.phy", &bytes) {
            Err(CollideError::Corrupt { .. }) | Err(CollideError::Truncated { .. }) => {}
            other => panic!("a cyclic ledge tree gave {other:?}"),
        }
    }

    #[test]
    fn a_mopp_is_refused_the_way_the_shipped_game_refuses_it() {
        let mut bytes = PhyWriter::default().write();
        // `modelType` sits two shorts into the `compactsurfaceheader_t`.
        let model_type = 16 + 4 + 6;
        bytes[model_type..model_type + 2].copy_from_slice(&COLLIDE_MOPP.to_le_bytes());
        assert!(matches!(
            VCollide::read_phy("mopp.phy", &bytes),
            Err(CollideError::Mopp { .. })
        ));
    }

    #[test]
    fn a_lump_29_entry_is_the_same_solids_under_a_different_header() {
        // One brush model, built out of the same solid the .phy writer emits.
        let phy = PhyWriter::default().write();
        let solid = &phy[16..phy.len() - 1 - "solid {\n\"index\" \"0\"\n\"mass\" \"40\"\n\"surfaceprop\" \"metal\"\n}\n".len()];
        let keys = b"staticsolid {\n\"index\" \"0\"\n\"contents\" \"1\"\n}\n";
        let mut lump: Vec<u8> = Vec::new();
        lump.extend_from_slice(&7i32.to_le_bytes()); // modelIndex
        lump.extend_from_slice(&(solid.len() as i32).to_le_bytes());
        lump.extend_from_slice(&(keys.len() as i32).to_le_bytes());
        lump.extend_from_slice(&1i32.to_le_bytes()); // solidCount
        lump.extend_from_slice(solid);
        lump.extend_from_slice(keys);
        lump.extend_from_slice(&(-1i32).to_le_bytes());

        let models = VCollide::read_lump("a.bsp", &lump).expect("a lump this test wrote");
        assert_eq!(models.len(), 1);
        let (index, collide) = &models[0];
        assert_eq!(*index, 7, "the brush model number, which is the *N of *7");
        assert_eq!(collide.solids.len(), 1);
        assert_eq!(collide.solids[0].ledges[0].points.len(), 8);
        assert_eq!(collide.static_solids(), vec![(0, 1)]);
    }

    #[test]
    fn a_ledge_reports_whether_its_triangles_disagree_about_the_material() {
        let one = VCollide::read_phy(
            "a.phy",
            &PhyWriter {
                material: 6,
                ..Default::default()
            }
            .write(),
        )
        .unwrap();
        let ledge = &one.solids[0].ledges[0];
        assert_eq!(ledge.material, 6);
        assert!(!ledge.mixed_materials);

        // Rewrite the second triangle's material and nothing else.
        let mut bytes = PhyWriter {
            material: 6,
            ..Default::default()
        }
        .write();
        let second = 16 + 4 + 28 + 48 + 28 + 16 + 16;
        let word = u32::from_le_bytes(bytes[second..second + 4].try_into().unwrap());
        let word = (word & 0x80ff_ffff) | 9 << 24;
        bytes[second..second + 4].copy_from_slice(&word.to_le_bytes());
        let mixed = VCollide::read_phy("b.phy", &bytes).unwrap();
        assert!(mixed.solids[0].ledges[0].mixed_materials);
    }
}
