//! What a trace is asked: a swept box, and the contents it is testing against.

use glam::Vec3;

/// A swept box — the query every trace takes.
///
/// **`start` is the *centre* of the box, not the caller's start point.**
/// `Ray_t` (`public/cmodel.h`) is built that way so the brush clip can push a
/// plane out by a single `|normal · extents|` instead of choosing a corner per
/// plane, and [`Trace::start`](super::Trace::start) puts the caller's frame
/// back by adding [`offset`](Ray::offset). A Portal 2 player's hull is
/// `(-16,-16,0)`-`(16,16,72)`, so the centre sits 36 units above the feet:
/// conflating the two puts the player a hull-height off the floor. See
/// `portdocs/ENGINE_TRACE.md` §4.1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    /// The centre of the box at the start of the sweep.
    pub(super) start: Vec3,
    /// Direction *and* length: the sweep ends at `start + delta`.
    pub(super) delta: Vec3,
    /// `start + offset` is the point the caller passed in.
    pub(super) offset: Vec3,
    /// Half the box's diagonal. Zero for a line.
    pub(super) extents: Vec3,
    /// Whether the extents are (near enough) zero.
    ///
    /// Not a convenience: it selects the algorithm. A point trace skips bevel
    /// planes and computes
    /// [`fraction_left_solid`](super::Trace::fraction_left_solid); a box trace
    /// offsets every plane and does neither.
    pub(super) is_ray: bool,
    /// Whether the sweep goes anywhere. A zero-length sweep is a *position
    /// test* and takes a different path entirely (`CM_UnsweptBoxTrace`).
    pub(super) is_swept: bool,
}

impl Ray {
    /// A line from `start` to `end` — `Ray_t::Init(start, end)`.
    pub fn line(start: Vec3, end: Vec3) -> Ray {
        let delta = end - start;
        Ray {
            start,
            delta,
            offset: Vec3::ZERO,
            extents: Vec3::ZERO,
            is_ray: true,
            is_swept: delta.length_squared() != 0.0,
        }
    }

    /// A box swept from `start` to `end` — `Ray_t::Init(start, end, mins,
    /// maxs)`.
    ///
    /// `mins`/`maxs` are relative to `start`, so a player hull is
    /// `Ray::hull(feet, feet + move, vec3(-16.0, -16.0, 0.0), vec3(16.0, 16.0,
    /// 72.0))`.
    pub fn hull(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Ray {
        let delta = end - start;
        let extents = (maxs - mins) * 0.5;
        // `Ray_t::Init`'s test, kept exactly: a hull small enough to be a point
        // takes the cheaper and *differently behaved* point path.
        let is_ray = extents.length_squared() < 1e-6;
        let centre = (mins + maxs) * 0.5;
        Ray {
            start: start + centre,
            delta,
            offset: -centre,
            extents,
            is_ray,
            is_swept: delta.length_squared() != 0.0,
        }
    }

    /// The point the caller passed as the start, undoing the centring.
    pub fn origin(&self) -> Vec3 {
        self.start + self.offset
    }

    /// `Ray_t::InvDelta` — reciprocals with a sentinel for the zero axes,
    /// because the slab test multiplies by these rather than dividing.
    pub(super) fn inv_delta(&self) -> Vec3 {
        let axis = |d: f32| if d != 0.0 { 1.0 / d } else { f32::MAX };
        Vec3::new(axis(self.delta.x), axis(self.delta.y), axis(self.delta.z))
    }
}

/// `CONTENTS_*` (`public/bspflags.h`) — what a brush is made of, and what a
/// trace is willing to hit.
///
/// One 32-bit set used two ways: a brush declares its bits, a trace passes a
/// mask, and they collide when the two intersect. The `MASK_*` constants are
/// Valve's named combinations, kept under their own names because gameplay
/// code names them constantly and a reader needs to be able to grep the C++.
///
/// Only the bits and combinations something here uses are defined — the same
/// rule [`surf`](crate::engine::world::bsp::surf) follows. The full tables are
/// in `public/bspflags.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Contents(pub u32);

// A fixed external vocabulary, not this module's invention: these are the bits
// `.bsp` files are compiled with and the names gameplay code says out loud.
// Defining the ones stage 1 happens to consume and no others would mean the
// next module to need `LADDER` re-derives its value from `bspflags.h` — which
// is exactly the transcription error `rustdocs/`'s "verify against the source"
// rule exists to prevent. Most are consumed by `client/` stage 4.
#[allow(dead_code)]
impl Contents {
    pub const EMPTY: Contents = Contents(0);

    pub const SOLID: Contents = Contents(0x1);
    pub const WINDOW: Contents = Contents(0x2);
    /// Alpha-tested grates: bullets and sight pass, solids do not.
    pub const GRATE: Contents = Contents(0x8);
    pub const SLIME: Contents = Contents(0x10);
    pub const WATER: Contents = Contents(0x20);
    pub const OPAQUE: Contents = Contents(0x80);
    /// Ignore `OPAQUE` on surfaces marked `SURF_NODRAW`.
    pub const IGNORE_NODRAW_OPAQUE: Contents = Contents(0x2000);
    /// Doors, platforms — anything `MOVETYPE_PUSH`.
    pub const MOVEABLE: Contents = Contents(0x4000);
    pub const PLAYERCLIP: Contents = Contents(0x10000);
    pub const MONSTERCLIP: Contents = Contents(0x20000);
    /// Portal 2's, and the reason this vocabulary is not CS:GO baggage.
    pub const BRUSH_PAINT: Contents = Contents(0x40000);
    /// Never on a brush — set on entities by the game.
    pub const MONSTER: Contents = Contents(0x2000000);
    pub const DEBRIS: Contents = Contents(0x4000000);
    pub const LADDER: Contents = Contents(0x20000000);

    pub const MASK_ALL: Contents = Contents(0xFFFF_FFFF);
    /// Everything normally solid.
    pub const MASK_SOLID: Contents = Contents(
        Self::SOLID.0 | Self::MOVEABLE.0 | Self::WINDOW.0 | Self::MONSTER.0 | Self::GRATE.0,
    );
    /// Everything that blocks player movement — `PlayerSolidMask()`, and so
    /// the mask every one of `client/` stage 4's traces will use.
    pub const MASK_PLAYERSOLID: Contents = Contents(
        Self::SOLID.0
            | Self::MOVEABLE.0
            | Self::PLAYERCLIP.0
            | Self::WINDOW.0
            | Self::MONSTER.0
            | Self::GRATE.0,
    );
    /// Everything normally solid, minus entities — world and brush models only.
    pub const MASK_SOLID_BRUSHONLY: Contents =
        Contents(Self::SOLID.0 | Self::MOVEABLE.0 | Self::WINDOW.0 | Self::GRATE.0);
    pub const MASK_WATER: Contents = Contents(Self::WATER.0 | Self::MOVEABLE.0 | Self::SLIME.0);
    /// Everything that blocks lighting.
    pub const MASK_OPAQUE: Contents = Contents(Self::SOLID.0 | Self::MOVEABLE.0 | Self::OPAQUE.0);

    /// Whether the two sets share a bit — the test the whole traversal is
    /// built on.
    pub fn intersects(self, other: Contents) -> bool {
        self.0 & other.0 != 0
    }

    /// The bits in both.
    pub fn and(self, other: Contents) -> Contents {
        Contents(self.0 & other.0)
    }

    /// The bits in either.
    pub fn or(self, other: Contents) -> Contents {
        Contents(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for Contents {
    /// The set as hex, which is how every Valve tool and every mapper writes
    /// it. Named bits would need all 26 and would still be a list.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}
