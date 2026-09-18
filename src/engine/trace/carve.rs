//! The hole a portal cuts in the wall it sits on.
//!
//! `CPortalSimulator::CreatePolyhedrons` and `CarveWallBrushes_Sub`
//! (`game/shared/portal/portalsimulation.cpp:3315`, `:3716`), which are
//! `portdocs/PORTAL.md` §4. A portal splits the collision near it into two
//! sets:
//!
//! - the **World**, everything in *front* of the portal plane, clipped to the
//!   environment box and otherwise left alone;
//! - the **Wall**, everything *behind* it, with a rectangular hole removed.
//!
//! Both live in one [`CarvedWall`], because both are needed together: the
//! substitutive trace ([`Tracer::with_hole`](super::Tracer::with_hole)) takes
//! whichever of the real world and the carved geometry goes *further*, so a
//! carved set holding only the holed wall would let a player in a portal
//! environment walk through the floor in front of it.
//!
//! Both also carry the **tube** ([`CarvedWall::tube`]) — the thin sleeve
//! lining the hole that `CreateTubePolyhedrons` (`:3812`) builds, which is
//! what an object has to fit inside to be eligible to pass — and, when the
//! portal is linked, the **remote** set: the exit portal's World geometry plus
//! this portal's tube moved into the exit's space, which is what the ray
//! transformed through the pair is swept against. That is
//! `portdocs/PORTAL.md` §5, and it is what holds a player up on the far room's
//! floor while their box straddles the plane.
//!
//! **No polyhedron library.** Valve's `CPolyhedron`s exist to become
//! `CPhysCollide`s and this port traces BSP brushes, so a carved piece is the
//! original brush's planes plus the clip planes plus four side planes — no
//! vertices generated and no hull built. That is `portdocs/PORTAL.md` §4.3,
//! and it is what deletes `mathlib/polyhedron.cpp` and
//! `staticcollisionpolyhedroncache.cpp`.
//!
//! **An empty piece still has to be detected**, which §4.3 originally said it
//! did not: an infeasible plane set does report a clean miss for a *point*,
//! but a swept box pushes every plane out by `|normal · extents|` and turns
//! two opposed planes with nothing between them into a solid slab. See
//! [`Pieces::piece`].

use std::collections::HashMap;

use glam::{Mat4, Vec3};

use super::model::{BrushSides, CBrush, CBrushSide, CPlane, CollisionBsp};
use super::result::SURFACE_INDEX_INVALID;
use super::{Contents, Ray, Surface, Tracer};

/// What the carve collects — the four brush sets' masks, unioned.
///
/// `PS_SD_Static_Brushes_t` (`portalsimulation.h:158-161`) asks four times,
/// for `MASK_SOLID_BRUSHONLY & ~CONTENTS_GRATE`, `CONTENTS_GRATE`,
/// `CONTENTS_PLAYERCLIP` and `CONTENTS_MONSTERCLIP`. **The four exist to hand
/// vphysics four collideables with four collision filters**; a BSP brush
/// carries its own contents and the trace masks against it, so this port asks
/// once. The union is the commented-out line Valve left at
/// `portalsimulation.cpp:1167`.
pub const CARVE: Contents =
    Contents(Contents::MASK_SOLID_BRUSHONLY.0 | Contents::PLAYERCLIP.0 | Contents::MONSTERCLIP.0);

/// `portal_environment_radius` (`portalsimulation.cpp:125`) — how far around
/// itself a portal clones the world.
const ENVIRONMENT_RADIUS: f32 = 75.0;

/// `PORTAL_HOLE_HALF_WIDTH_MOD` and `PORTAL_HOLE_HALF_HEIGHT_MOD`
/// (`portalsimulation.cpp:72`) — the hole is a tenth of a unit larger than the
/// portal in each direction.
const HOLE_MOD: f32 = 0.1;

/// `PORTAL_WALL_MIN_THICKNESS` (`portalsimulation.cpp:68`) — how far the four
/// slabs are pushed back from the hole's edge, so that two of them never meet
/// exactly on a plane.
const WALL_MIN_THICKNESS: f32 = 0.1;

/// `PORTAL_POLYHEDRON_CUT_EPSILON` (`portalsimulation.cpp:69`) — how thin a
/// piece has to be before Valve's clipper collapses it to nothing, and the
/// threshold [`Pieces::piece`] drops one at.
const CUT_EPSILON: f32 = 1.0 / 1024.0;

/// `PORTAL_WORLD_WALL_HALF_SEPARATION_AMOUNT` (`portalsimulation.cpp:71`) —
/// the gap between the World set and the Wall set, *"separating the world
/// collision from wall collision by a small amount gets rid of extremely thin
/// erroneous collision at the separating plane"*.
const WORLD_WALL_SEPARATION: f32 = 1.0 / 16.0;

/// How far the four slabs run before they stop mattering — `fHalfWidth * 40`
/// and `fHalfHeight * 40` (`portalsimulation.cpp:3598`).
const FAR: f32 = 40.0;

/// `PORTAL_WALL_TUBE_DEPTH` (`portalsimulation.cpp:66`) — how far back into
/// the wall the sleeve reaches. One unit, and Valve's commented-out
/// alternative was 1/128.
const TUBE_DEPTH: f32 = 1.0;

/// `PORTAL_WALL_TUBE_OFFSET` (`portalsimulation.cpp:67`) — how far *behind*
/// the portal plane the sleeve starts, so that it never pokes out into the
/// room.
///
/// Valve adds `VPHYSICS_SHRINK` (0.5) to this when the portal sits on a brush
/// entity using `SOLID_VPHYSICS`, because VBSP shrinks those brushes by half a
/// unit on the way to a physics model. **Not ported**: this module cuts BSP
/// brushes, which are not shrunk, so there is nothing to match.
const TUBE_OFFSET: f32 = 0.01;

/// What the tube is made of.
///
/// The tube is generated geometry rather than a cut piece of the map, so it
/// has no brush to take contents from. Valve's collideable has none either —
/// a trace that stops on it is given the *portal's* surface properties, taken
/// from the trace that placed it (`PS_SD_Static_SurfaceProperties_t`), which
/// this port has no equivalent of. Plain `SOLID` is what every mask a player
/// or a carve uses agrees on.
const TUBE_CONTENTS: Contents = Contents::SOLID;

/// How far in front of its own plane a portal's trigger box reaches —
/// `GetLocalMaxs().x` (`portal_base2d.h:174`), the same 64 units
/// `server::classes::portal::OBB_DEPTH` names.
///
/// Repeated rather than shared because the seam forbids the sharing:
/// `server/` names no engine collision type and `trace/` names no server type,
/// so a constant that both need is written twice against one reference.
const TOUCH_DEPTH: f32 = 64.0;

/// Where a portal is, in the form the carve needs.
///
/// `PS_PlacementData_t`'s first six fields (`portalsimulation.h:114`) and
/// nothing else. The [`PortalState`](crate::server::PortalState) seam already
/// carries every number here, which is what `portdocs/PORTAL.md` §12 means by
/// "the one thing stage 3 adds to the seam is nothing at all".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortalHole {
    /// `ptCenter` — the portal entity's own origin.
    pub center: Vec3,
    /// The portal plane's normal, pointing out of the wall.
    pub forward: Vec3,
    /// **The negation of the angle matrix's second column**, which is Valve's
    /// `m_vRight = -m_vRight` (`portal_base2d.cpp:1213`) — column 1 is *left*.
    /// [`PortalHole::new`] does it; a hand-built one has to.
    pub right: Vec3,
    pub up: Vec3,
    pub half_width: f32,
    pub half_height: f32,
}

impl PortalHole {
    /// The placement a `prop_portal` has: an origin, a `QAngle` and its two
    /// half-sizes.
    pub fn new(origin: Vec3, angles: Vec3, half_width: f32, half_height: f32) -> PortalHole {
        let rotation = crate::math::angle_matrix(angles);
        PortalHole {
            center: origin,
            forward: rotation * Vec3::X,
            right: -(rotation * Vec3::Y),
            up: rotation * Vec3::Z,
            half_width,
            half_height,
        }
    }

    /// `vCollisionCloneExtents` (`portalsimulation.cpp:447`) — how far in each
    /// of the portal's own directions the world is cloned.
    ///
    /// `x` is along `forward`, `y` along `right`, `z` along `up`, and the
    /// asymmetry on `x` is Valve's: it is `MAX(halfWidth, halfHeight)` where
    /// the other two are the matching half-size.
    fn clone_extents(&self) -> Vec3 {
        Vec3::new(
            self.half_width.max(self.half_height) + ENVIRONMENT_RADIUS,
            self.half_width + ENVIRONMENT_RADIUS,
            self.half_height + ENVIRONMENT_RADIUS,
        )
    }

    /// The **World** query box — the OBB at `portalsimulation.cpp:3370`, as an
    /// AABB. It reaches forward of the portal plane only.
    fn world_bounds(&self) -> (Vec3, Vec3) {
        let extents = self.clone_extents();
        swept_box(
            self.center,
            self.forward * extents.x,
            self.right * extents.y,
            self.up * extents.z,
        )
    }

    /// The **(holy) Wall** query box — the OBB at `portalsimulation.cpp:3524`,
    /// as an AABB.
    ///
    /// `-forward` by `2 × MAX(halfHeight, halfWidth)`, and `±4 × halfWidth` by
    /// `±4 × halfHeight` across it. Larger across than the World box and
    /// smaller through it, which is the shape of the thing being asked: a wall
    /// is wide and thin.
    fn wall_bounds(&self) -> (Vec3, Vec3) {
        let depth = 2.0 * self.half_height.max(self.half_width);
        swept_box(
            self.center,
            -self.forward * depth,
            self.right * (self.half_width * 4.0),
            self.up * (self.half_height * 4.0),
        )
    }

    /// Whether the axis-aligned box `mins`..`maxs` is inside this portal's
    /// trigger volume — **the stand-in for `m_hPortalEnvironment`**.
    ///
    /// The real answer is §6.1's: `CPortal_Base2D::TestCollision` against the
    /// portal's OBB, plus the three filters that decide whether a *teleport*
    /// is due, and the result is a networked handle on the player that
    /// `HandlePortalling` reassigns. None of that exists until stage 4, and
    /// the carved geometry is useless without *some* answer, so this is the
    /// first of those tests on its own: the box overlaps the OBB
    /// `(0, -halfWidth, -halfHeight)` to `(64, halfWidth, halfHeight)` in the
    /// portal's frame.
    ///
    /// **Exact for an axis-aligned portal and conservative for any other.**
    /// The three portal axes are tested and the box's own three are not, which
    /// is half of a separating-axis test — so an oblique portal can answer
    /// `true` for a box just clear of its corner. That errs towards using the
    /// carved geometry near a portal, which is the safe direction: §2 says 19
    /// of the 21 shipped portals are at axis-aligned yaws anyway.
    pub fn touches(&self, mins: Vec3, maxs: Vec3) -> bool {
        let center = (mins + maxs) * 0.5;
        let extents = maxs - center;
        let axis = |normal: Vec3, lo: f32, hi: f32| {
            let along = normal.dot(center - self.center);
            let radius = normal.abs().dot(extents);
            along + radius >= lo && along - radius <= hi
        };
        axis(self.forward, 0.0, TOUCH_DEPTH)
            && axis(self.right, -self.half_width, self.half_width)
            && axis(self.up, -self.half_height, self.half_height)
    }
}

/// The AABB of `origin + t·f + b·r + c·u` for `t` in `0..=1` and `b`, `c` in
/// `-1..=1` — Valve's eight-corner loop, written as the interval it computes.
fn swept_box(origin: Vec3, f: Vec3, r: Vec3, u: Vec3) -> (Vec3, Vec3) {
    let center = origin + f * 0.5;
    let extents = f.abs() * 0.5 + r.abs() + u.abs();
    (center - extents, center + extents)
}

/// A portal's partner, and the transforms between the two.
///
/// `PS_PlacementData_t`'s `pLinkedPortal` half, reduced to what a trace needs:
/// where the exit is, and the matrix each way. Both matrices are carried
/// rather than one being inverted, because the partner has already computed
/// the other one — they are each other's `m_matrixThisToLinked` — and two
/// spellings of the same transform that disagree in the last bit would show up
/// as a player drifting a hair every time they went through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortalLink {
    /// The exit portal's [`LivePortal::id`] — `m_hLinkedPortal`, as a key.
    pub exit_id: u64,
    /// Where the exit portal is.
    pub exit: PortalHole,
    /// `m_matrixThisToLinked` — a point at this portal, moved to the exit.
    /// **The 180° turn about up is in it**; see
    /// `server::classes::portal::teleport_matrix`, which is where this port
    /// computes it and the only place it is computed.
    pub to_exit: Mat4,
    /// The exit's own `m_matrixThisToLinked`, which is [`to_exit`](Self::to_exit)
    /// undone.
    pub to_entrance: Mat4,
}

/// One portal, as [`PortalHoles::sync`] is told about it.
///
/// The whole input to the carve: a key, a placement, and a partner when there
/// is one. An unlinked portal still carves — the hole in the wall is real
/// whether or not anything is on the other side — it simply has no remote set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LivePortal {
    /// [`PortalState::id`](crate::server::PortalState::id).
    pub id: u64,
    pub hole: PortalHole,
    /// `m_hLinkedPortal`, if this portal found a partner.
    pub link: Option<PortalLink>,
}

/// `CalculateExtentShift` (`portal_gamemovement.cpp:1587`) — how far along the
/// exit normal the transformed box has to move so that its *near face* lands
/// on the exit plane rather than its centre.
///
/// An AABB cannot rotate, so a box that is touching a wall portal with 16
/// units of itself in front of the plane comes out of a floor portal with 36
/// units of itself below it — 20 units into the floor. The shift is the
/// difference between the two projected radii, along the exit normal, and
/// without it the remote trace is asking about a box in the wrong place.
///
/// Valve's loop sums the per-axis products rather than taking the largest,
/// which is the same thing for an axis-aligned normal and a deliberate
/// over-estimate for any other.
pub(super) fn extent_shift(
    local_extents: Vec3,
    local_normal: Vec3,
    remote_extents: Vec3,
    remote_normal: Vec3,
) -> Vec3 {
    let radius = |normal: Vec3, extents: Vec3| normal.abs().dot(extents.abs());
    remote_normal * (radius(remote_normal, remote_extents) - radius(local_normal, local_extents))
}

/// One portal's collision, carved.
///
/// The pieces are a [`CollisionBsp`] of their own with a **one-leaf tree**, so
/// every line of the ordinary traversal applies to them unchanged — the
/// descent reaches the single leaf, the leaf lists every piece, and
/// [`clip_box_to_brush`](super::brush) does the rest. That is why there is no
/// second sweep in this module.
#[derive(Debug)]
pub struct CarvedWall {
    /// Which portal this belongs to — [`PortalState::id`](crate::server::PortalState::id).
    id: u64,
    hole: PortalHole,
    /// The partner, when there is one — see [`PortalLink`].
    link: Option<PortalLink>,
    pieces: CollisionBsp,
    /// The sleeve lining the hole, as its own model.
    ///
    /// Separate from [`pieces`](Self::pieces) because it is separate in the
    /// shipped engine (`Wall.Local.Tube` is its own collideable), because it
    /// is generated rather than cut, and because the *remote* trace wants it
    /// without the rest.
    tube: CollisionBsp,
    /// The exit portal's World geometry and this portal's tube, both in the
    /// exit's space — what `ray_remote` is swept against. `None` while
    /// unlinked.
    remote: Option<CollisionBsp>,
    /// How many brushes went in, before the split into pieces — the two sets
    /// added together, so a brush that lands in both is counted twice.
    /// Reported by [`summary`](CarvedWall::summary) and by nothing else.
    sources: usize,
}

impl CarvedWall {
    /// Carves the world around one portal.
    ///
    /// `tracer` is only used to enumerate — [`Tracer::brushes_in_box`] — so it
    /// may be any tracer over the map being carved, and one shared across a
    /// whole [`PortalHoles::sync`] is the intended use.
    pub fn build(
        tracer: &mut Tracer<'_>,
        id: u64,
        hole: &PortalHole,
        link: Option<PortalLink>,
    ) -> CarvedWall {
        let collision = tracer.collision();

        let (lo, hi) = hole.world_bounds();
        let world = tracer.brushes_in_box(lo, hi, CARVE);
        let (lo, hi) = hole.wall_bounds();
        let wall = tracer.brushes_in_box(lo, hi, CARVE);

        let mut out = Pieces::default();
        let clip = clip_planes(&mut out, hole);
        let sides = side_planes(&mut out, hole);

        // The World set: in front of the plane, clipped and not holed.
        for &index in &world {
            let own = out.source_sides(collision, index);
            out.piece(collision.brushes[index].contents, &own, &clip.world);
        }

        // The Wall set: behind the plane, and the four slabs around the hole.
        for &index in &wall {
            let own = out.source_sides(collision, index);
            for slab in &sides {
                let mut planes = [0u32; 10];
                planes[..6].copy_from_slice(&clip.wall);
                planes[6..].copy_from_slice(slab);
                out.piece(collision.brushes[index].contents, &own, &planes);
            }
        }

        // The tube, in this portal's own space.
        let tube = {
            let mut out = Pieces::default();
            tube_pieces(
                &mut out,
                hole.center,
                hole.forward,
                hole.right,
                hole.up,
                hole,
            );
            CollisionBsp::from_pieces(out.planes, out.sides, out.brushes, out.surfaces)
        };

        // The remote set: what the exit portal's simulator would answer for
        // the transformed ray. Two halves, and both are Valve's — the *exit's*
        // World brushes, traced with `bTraceHolyWall` false so the exit's own
        // holed wall is deliberately not in it, and *this* portal's tube moved
        // into the exit's space, which is what tests that the player fits
        // through the hole in the configuration they would leave in.
        //
        // The transformed tube lands in *front* of the exit plane, not behind
        // it: the matrix's half turn maps "behind the entrance" to "in front of
        // the exit". That is the whole reason it cannot be the exit portal's
        // own tube.
        let remote = link.map(|link| {
            let mut out = Pieces::default();
            let clip = clip_planes(&mut out, &link.exit);
            let (lo, hi) = link.exit.world_bounds();
            for &index in &tracer.brushes_in_box(lo, hi, CARVE) {
                let own = out.source_sides(collision, index);
                out.piece(collision.brushes[index].contents, &own, &clip.world);
            }
            let moved = |v: Vec3| link.to_exit.transform_vector3(v);
            tube_pieces(
                &mut out,
                link.to_exit.transform_point3(hole.center),
                moved(hole.forward),
                moved(hole.right),
                moved(hole.up),
                hole,
            );
            CollisionBsp::from_pieces(out.planes, out.sides, out.brushes, out.surfaces)
        });

        CarvedWall {
            id,
            hole: *hole,
            link,
            sources: world.len() + wall.len(),
            pieces: CollisionBsp::from_pieces(out.planes, out.sides, out.brushes, out.surfaces),
            tube,
            remote,
        }
    }

    /// The pieces, as a collision model.
    ///
    /// `wall.collision().tracer().trace(&ray, mask)` is a sweep against the
    /// carved geometry *alone*, which is what the tests want and what
    /// `UTIL_Portal_TraceRay` (`portal_util_shared.cpp:638`) is: it traces the
    /// portal simulator's own collideables and never the real world.
    pub fn collision(&self) -> &CollisionBsp {
        &self.pieces
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn hole(&self) -> &PortalHole {
        &self.hole
    }

    /// The sleeve lining the hole — see [`CarvedWall::tube`].
    pub fn tube(&self) -> &CollisionBsp {
        &self.tube
    }

    /// The exit portal's geometry, in the exit's space — `None` while
    /// unlinked. Swept with the ray [`remote_ray`](CarvedWall::remote_ray)
    /// builds and nothing else.
    pub fn remote(&self) -> Option<&CollisionBsp> {
        self.remote.as_ref()
    }

    /// This portal's partner and the transforms between them — `None` while
    /// unlinked.
    pub fn link(&self) -> Option<&PortalLink> {
        self.link.as_ref()
    }

    /// The same sweep, as the exit portal sees it — `ray_remote`
    /// (`portal_gamemovement.cpp:1943`), and the shift that built it.
    ///
    /// Three things happen to the ray and each one matters:
    ///
    /// - the **centre** goes through the matrix, so the box lands at the exit;
    /// - the **delta** is rotated only, because it is a direction;
    /// - the **extents stay axis-aligned**, because an AABB does not rotate
    ///   when it goes through a portal — which is exactly why this trace is a
    ///   different question from the local one rather than the same one in
    ///   other coordinates.
    ///
    /// `exit_extents` is the half-size the box would have on the far side,
    /// which differs from the local one only when the transition forces a
    /// crouch; `None` means "the same box". The returned shift is needed again
    /// to bring an answer back, so it is handed out rather than recomputed.
    ///
    /// `None` when this portal is unlinked, which is also when there is
    /// nothing to sweep.
    pub fn remote_ray(&self, ray: &Ray, exit_extents: Option<Vec3>) -> Option<(Ray, Vec3)> {
        let link = self.link.as_ref()?;
        self.remote.as_ref()?;

        let extents = exit_extents.unwrap_or(ray.extents);
        let shift = extent_shift(ray.extents, self.hole.forward, extents, link.exit.forward);
        let centre = link.to_exit.transform_point3(ray.start) + shift;
        let delta = link.to_exit.transform_vector3(ray.delta);

        // `mins`/`maxs` symmetric about the start, so that the ray's centring
        // offset is zero and its `start` *is* the box centre — which is what
        // `TracePlayerBBox` builds by hand with
        // `ray_remote.m_StartOffset = vec3_origin`.
        Some((Ray::hull(centre, centre + delta, -extents, extents), shift))
    }

    /// How many of the map's brushes went in, before the split into pieces —
    /// see [`CarvedWall::sources`] for what a brush in both sets counts as.
    pub fn sources(&self) -> usize {
        self.sources
    }

    /// How many pieces came out — at most four per Wall brush and one per
    /// World brush, less the ones [`Pieces::is_empty`] proved enclose nothing.
    pub fn pieces(&self) -> usize {
        self.pieces.brushes.len()
    }

    /// How many slabs the sleeve has — four, unless a degenerate placement
    /// collapsed one.
    pub fn tube_slabs(&self) -> usize {
        self.tube.brushes.len()
    }

    /// How many pieces the far side holds, or zero while unlinked.
    pub fn remote_pieces(&self) -> usize {
        self.remote.as_ref().map_or(0, |set| set.brushes.len())
    }

    /// Counts, for `status`-style reporting.
    pub fn summary(&self) -> String {
        format!(
            "{} brushes carved into {} pieces, {} planes, {} tube slabs{}",
            self.sources(),
            self.pieces(),
            self.pieces.planes.len(),
            self.tube_slabs(),
            match self.remote.is_some() {
                true => format!(", {} remote pieces", self.remote_pieces()),
                false => String::from(", unlinked"),
            },
        )
    }
}

/// Every active portal's carved collision, kept in step with where the portals
/// are.
///
/// `CPortalSimulator`'s lifecycle reduced to what it does to *collision*:
/// built on `MoveTo`, rebuilt when the placement changes, and thrown away when
/// the portal deactivates. A portal that has not moved keeps the carve it had,
/// which is the whole point — carving is the expensive half and a portal moves
/// about as often as one is shot.
#[derive(Default)]
pub struct PortalHoles {
    walls: Vec<CarvedWall>,
}

impl PortalHoles {
    /// Rebuilds the store from the portals that exist now.
    ///
    /// A portal not in `live` is dropped, one whose [`PortalHole`] is
    /// unchanged is kept as it is, and anything else is carved. Cheap to call
    /// every frame: with no portal moving it is one comparison each.
    pub fn sync(&mut self, collision: &CollisionBsp, live: &[LivePortal]) {
        self.walls
            .retain(|wall| live.iter().any(|portal| portal.id == wall.id));

        // **The link counts as part of the placement.** A portal that has not
        // moved but has just found — or lost — a partner has no remote set or
        // the wrong one, and the remote set is the half that makes the module
        // work.
        let stale = |walls: &[CarvedWall], portal: &LivePortal| {
            !walls
                .iter()
                .any(|w| w.id == portal.id && w.hole == portal.hole && w.link == portal.link)
        };
        if !live.iter().any(|portal| stale(&self.walls, portal)) {
            return;
        }

        // One tracer for the whole rebuild: it allocates a visit stamp per
        // brush in the map, which is the only allocation the enumeration makes.
        let mut tracer = collision.tracer();
        for portal in live {
            if !stale(&self.walls, portal) {
                continue;
            }
            let wall = CarvedWall::build(&mut tracer, portal.id, &portal.hole, portal.link);
            match self.walls.iter().position(|w| w.id == portal.id) {
                Some(index) => self.walls[index] = wall,
                None => self.walls.push(wall),
            }
        }
    }

    /// The carved geometry for one portal.
    ///
    /// **This is how `m_hPortalEnvironment` becomes geometry.** The player
    /// carries the *portal* they are in, decided by
    /// `client::movement::handle_portalling`'s selection at the end of each
    /// move, and the engine turns it back into a carve here.
    /// [`touching`](PortalHoles::touching) is the unswept, filterless version
    /// of the same question and is what the `trace` command reports.
    pub fn get(&self, id: u64) -> Option<&CarvedWall> {
        self.walls.iter().find(|wall| wall.id == id)
    }

    /// The portal whose trigger volume the box `mins`..`maxs` is in, if any —
    /// see [`PortalHole::touches`] for what that means and what it stands in
    /// for.
    ///
    /// **The first match wins**, which is the list's order and so the order
    /// the game handed the portals over in. Two portals close enough to share
    /// a player is a case Valve resolves by touch order too; no shipped map
    /// places a pair that near.
    pub fn touching(&self, mins: Vec3, maxs: Vec3) -> Option<&CarvedWall> {
        self.walls.iter().find(|wall| wall.hole.touches(mins, maxs))
    }

    pub fn iter(&self) -> impl Iterator<Item = &CarvedWall> {
        self.walls.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.walls.is_empty()
    }
}

/// The six planes each set is clipped to — `collisionClip`
/// (`portalsimulation.cpp:3355` for the World, `:3601` for the Wall).
///
/// The two differ in **one plane**: the World's first is the portal plane
/// facing backwards and shifted a sixteenth forward, the Wall's is the portal
/// plane facing forwards and unshifted. The other five — forward or backward
/// by `vCollisionCloneExtents.x`, and `±right`, `±up` by its other two
/// components — are the environment box and are shared.
struct ClipPlanes {
    world: [u32; 6],
    wall: [u32; 6],
}

/// Builds both sets of clip planes into `out`.
fn clip_planes(out: &mut Pieces, hole: &PortalHole) -> ClipPlanes {
    let extents = hole.clone_extents();
    let at = |normal: Vec3, distance: f32| cplane(normal, normal.dot(hole.center) + distance);

    // Shared: the environment box, minus the face the portal plane replaces.
    let sides = [
        at(hole.right, extents.y),
        at(-hole.right, extents.y),
        at(hole.up, extents.z),
        at(-hole.up, extents.z),
    ];

    // The World reaches *forward* from a sixteenth in front of the plane; the
    // Wall reaches *backward* from the plane itself.
    let mut set = |first: CPlane, second: CPlane| {
        let mut planes = [0u32; 6];
        planes[0] = out.plane(first);
        planes[1] = out.plane(second);
        for (slot, side) in planes[2..].iter_mut().zip(sides) {
            *slot = out.plane(side);
        }
        planes
    };
    ClipPlanes {
        world: set(
            at(-hole.forward, -WORLD_WALL_SEPARATION),
            at(hole.forward, extents.x),
        ),
        wall: set(at(hole.forward, 0.0), at(-hole.forward, extents.x)),
    }
}

/// The four slabs' side planes — `CarveWallBrushes_Sub`
/// (`portalsimulation.cpp:3716`), which is four clips of the same four normals
/// at four sets of distances.
///
/// The normals are always up, down, left, right, in that order.
fn side_planes(out: &mut Pieces, hole: &PortalHole) -> [[u32; 4]; 4] {
    let (up, down) = (hole.up, -hole.up);
    let (right, left) = (hole.right, -hole.right);

    // The hole is a tenth larger than the portal, and the slabs stop another
    // tenth back from it.
    let edge_up = hole.half_height + HOLE_MOD + WALL_MIN_THICKNESS;
    let edge_right = hole.half_width + HOLE_MOD + WALL_MIN_THICKNESS;

    // Where a slab stops mattering. **Three of the four are `halfHeight * 40`
    // and one is `halfWidth * 40`** — including the *left* one, which is the
    // only plane in the set measured across the portal and which Valve
    // nonetheless builds out of the height (`fFarLeftPlaneDistance`,
    // `portalsimulation.cpp:3599`). Ported as written: for a shipped portal the
    // two are 2,240 and 1,280 units, both an order of magnitude past any wall,
    // so they can only differ where there is nothing to cut — and "correcting"
    // it would change the shipped game's geometry on a guess about what it
    // meant.
    let far = hole.half_height * FAR;
    let far_right = hole.half_width * FAR;

    let at = |normal: Vec3, distance: f32| cplane(normal, normal.dot(hole.center) + distance);
    let mut piece = |planes: [CPlane; 4]| planes.map(|p| out.plane(p));

    [
        // Upper: above the hole, full width.
        piece([
            at(up, far),
            at(down, -edge_up),
            at(left, far),
            at(right, far_right),
        ]),
        // Lower: below it, full width.
        piece([
            at(up, -edge_up),
            at(down, far),
            at(left, far),
            at(right, far_right),
        ]),
        // Left: beside it, only as tall as the hole.
        piece([
            at(up, edge_up),
            at(down, edge_up),
            at(left, far),
            at(right, -edge_right),
        ]),
        // Right: the same on the other side.
        piece([
            at(up, edge_up),
            at(down, edge_up),
            at(left, -edge_right),
            at(right, far_right),
        ]),
    ]
}

/// The four slabs that line the hole — `CreateTubePolyhedrons`
/// (`portalsimulation.cpp:3812`), which is the same plane treatment
/// [`side_planes`] does with one fewer degree of freedom.
///
/// A rectangular sleeve, [`WALL_MIN_THICKNESS`] thick, running from
/// [`TUBE_OFFSET`] behind the plane to [`TUBE_DEPTH`] further back. It fills
/// exactly the tenth of a unit between the hole's edge (`half + 0.1`) and
/// where the carved wall's slabs begin (`half + 0.2`), so for the first unit
/// of depth the opening is the portal's own size and after that it is a tenth
/// wider. `portalsimulation.h:215` calls it *"a minimal tube, an object must
/// fit inside this to be eligible for portaling"*.
///
/// The basis is passed in rather than read from `hole` because the remote set
/// needs the same sleeve through the pair's matrix, and transforming a rigid
/// basis is the whole of transforming the sleeve. `hole` supplies the two
/// half-sizes and nothing else.
fn tube_pieces(
    out: &mut Pieces,
    center: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    hole: &PortalHole,
) {
    let (back, left, down) = (-forward, -right, -up);
    let half_width = hole.half_width + HOLE_MOD;
    let half_height = hole.half_height + HOLE_MOD;
    let (wide, tall) = (
        half_width + WALL_MIN_THICKNESS,
        half_height + WALL_MIN_THICKNESS,
    );

    let at = |normal: Vec3, distance: f32| cplane(normal, normal.dot(center) + distance);
    // The first two planes never change: every slab is the same depth.
    let depth = [
        at(forward, -TUBE_OFFSET),
        at(back, TUBE_DEPTH + TUBE_OFFSET),
    ];

    let slabs = [
        // Upper, full width plus the thickness at each end.
        [
            at(up, tall),
            at(down, -half_height),
            at(left, wide),
            at(right, wide),
        ],
        // Lower, the same.
        [
            at(up, -half_height),
            at(down, tall),
            at(left, wide),
            at(right, wide),
        ],
        // Left, only as tall as the hole.
        [
            at(up, half_height),
            at(down, half_height),
            at(left, wide),
            at(right, -half_width),
        ],
        // Right, the same on the other side.
        [
            at(up, half_height),
            at(down, half_height),
            at(left, -half_width),
            at(right, wide),
        ],
    ];

    for slab in slabs {
        let mut planes = [0u32; 6];
        for (slot, plane) in planes.iter_mut().zip(depth.iter().chain(slab.iter())) {
            *slot = out.plane(*plane);
        }
        out.piece(TUBE_CONTENTS, &[], &planes);
    }
}

/// A plane, with [`CPlane::axis`] filled in the way
/// [`CollisionBsp::build`] fills it.
fn cplane(normal: Vec3, dist: f32) -> CPlane {
    CPlane {
        normal,
        dist,
        axis: (0..3).find(|&i| normal[i].abs() == 1.0),
    }
}

/// The carved pieces under construction.
///
/// Source planes and source surfaces are remapped into compact tables of their
/// own rather than the map's being cloned — a shipped map has tens of
/// thousands of planes and a carve touches tens of them.
#[derive(Default)]
struct Pieces {
    planes: Vec<CPlane>,
    sides: Vec<CBrushSide>,
    brushes: Vec<CBrush>,
    surfaces: Vec<Surface>,
    plane_of: HashMap<u32, u32>,
    surface_of: HashMap<u16, u16>,
}

impl Pieces {
    fn plane(&mut self, plane: CPlane) -> u32 {
        self.planes.push(plane);
        self.planes.len() as u32 - 1
    }

    /// Whether a piece's plane set encloses nothing — **as far as a pair of
    /// opposed planes can tell**.
    ///
    /// Two planes facing each other with `n₁ · x ≤ d₁` and `-n₁ · x ≤ d₂`
    /// enclose the slab `-d₂ ≤ n₁ · x ≤ d₁`, which is empty when
    /// `d₁ + d₂ < 0` — and thinner than Valve's clipper would keep when it is
    /// below [`CUT_EPSILON`], which is the threshold used here for the same
    /// reason Valve uses it: a wall a thousandth of a unit thick is not a wall.
    ///
    /// **Sound but not complete.** It proves emptiness and never guesses at
    /// it, so no real geometry is ever dropped; what it misses is a piece that
    /// is empty only because of planes that are *not* parallel, which survives
    /// as a sliver at a corner once the box expansion has been applied. That
    /// is the same rounding `portdocs/PORTAL.md` §4.4 records for the hole's
    /// own corners, it is bounded by the sweeping box's extents, and it hugs
    /// the brush it came from — which is solid anyway. The parallel case is
    /// the one worth the test because it is the one that produces a *slab*
    /// rather than a sliver, and because the arrangement that causes it is
    /// guaranteed rather than incidental.
    ///
    /// `O(k²)` over a piece's dozen or so planes, which is cheaper than the
    /// one extra brush it saves the trace.
    fn is_empty(&self, own: &[CBrushSide], planes: &[u32]) -> bool {
        let all: Vec<&CPlane> = own
            .iter()
            .map(|side| side.plane)
            .chain(planes.iter().copied())
            .map(|index| &self.planes[index as usize])
            .collect();

        all.iter().enumerate().any(|(i, a)| {
            all[i + 1..].iter().any(|b| {
                // Anti-parallel to within a thousandth of a degree, which is
                // the only case where adding the two distances means anything.
                a.normal.dot(b.normal) <= -0.999_999 && a.dist + b.dist < CUT_EPSILON
            })
        })
    }

    /// One of the map's planes, in this table's numbering.
    fn source_plane(&mut self, collision: &CollisionBsp, index: u32) -> u32 {
        match self.plane_of.get(&index) {
            Some(&mapped) => mapped,
            None => {
                let mapped = self.plane(collision.planes[index as usize]);
                self.plane_of.insert(index, mapped);
                mapped
            }
        }
    }

    /// One of the map's surfaces, in this table's numbering. The null surface
    /// stays null.
    fn source_surface(&mut self, collision: &CollisionBsp, index: u16) -> u16 {
        if index == SURFACE_INDEX_INVALID {
            return SURFACE_INDEX_INVALID;
        }
        let Some(surface) = collision.surfaces.get(index as usize) else {
            return SURFACE_INDEX_INVALID;
        };
        match self.surface_of.get(&index) {
            Some(&mapped) => mapped,
            None => {
                self.surfaces.push(surface.clone());
                let mapped = self.surfaces.len() as u16 - 1;
                self.surface_of.insert(index, mapped);
                mapped
            }
        }
    }

    /// One source brush's own sides, remapped.
    ///
    /// **A box brush becomes a plane brush**, because a carved box is not a
    /// box. `IntersectRayWithBoxBrush`'s slab test and
    /// `CM_ClipBoxToBrush`'s plane loop agree to within how each applies
    /// `DIST_EPSILON` — the box path grows the box, the plane path shortens
    /// the fraction — so this changes which of two equivalent paths a wall
    /// near a portal takes and nothing else.
    fn source_sides(&mut self, collision: &CollisionBsp, index: usize) -> Vec<CBrushSide> {
        match collision.brushes[index].sides {
            BrushSides::Planes { first, count } => (first..first + count)
                .map(|i| {
                    let side = collision.brush_sides[i as usize];
                    CBrushSide {
                        plane: self.source_plane(collision, side.plane),
                        surface: self.source_surface(collision, side.surface),
                        bevel: side.bevel,
                    }
                })
                .collect(),
            BrushSides::Box(index) => {
                let brush = collision.box_brushes[index as usize];
                // `BoxBrush::surfaces` is `[-x, -y, -z, +x, +y, +z]`, and a
                // side's plane faces *out*, so the minimum faces carry a
                // negated distance — `extract_box`'s rule, inverted.
                (0..6)
                    .map(|face| {
                        let axis = face % 3;
                        let mut normal = Vec3::ZERO;
                        let dist = match face < 3 {
                            true => {
                                normal[axis] = -1.0;
                                -brush.mins[axis]
                            }
                            false => {
                                normal[axis] = 1.0;
                                brush.maxs[axis]
                            }
                        };
                        CBrushSide {
                            plane: self.plane(cplane(normal, dist)),
                            surface: self.source_surface(collision, brush.surfaces[face]),
                            bevel: false,
                        }
                    })
                    .collect()
            }
        }
    }

    /// One carved piece: a brush's own sides plus the planes that cut it.
    ///
    /// The generated sides name the **null surface**, so a trace that stops on
    /// a cut face reports `**empty**` rather than the wall's material. Valve
    /// gives its whole carved collideable one `csurface_t`, taken from the
    /// trace that placed the portal (`PS_SD_Static_SurfaceProperties_t`); this
    /// port has no such trace, and naming the originating brush's material for
    /// a face that brush does not have would be a worse answer than none.
    ///
    /// **An empty piece is dropped**, and `portdocs/PORTAL.md` §4.3 used to say
    /// it did not have to be. It is wrong, and the reason is the one thing in
    /// this module that a *point* test cannot show: a swept box clips against
    /// every plane pushed out by `|normal · extents|`
    /// ([`clip_box_to_brush`](super::brush)), so two opposed planes with
    /// nothing between them end up with a whole hull's width between them, and
    /// a piece whose true volume is empty becomes a solid slab. The case is
    /// not hypothetical or rare — it is the **wall's own front face against
    /// the World set's clip plane**, which are anti-parallel by construction
    /// because a portal's forward *is* the surface normal of the wall it sits
    /// on. Left in, every portal in the game has an invisible pane of glass
    /// across it.
    ///
    /// See [`is_empty`](Pieces::is_empty) for what the test does and does not
    /// catch.
    fn piece(&mut self, contents: Contents, own: &[CBrushSide], planes: &[u32]) {
        if self.is_empty(own, planes) {
            return;
        }
        let first = self.sides.len() as u32;
        self.sides.extend_from_slice(own);
        self.sides.extend(planes.iter().map(|&plane| CBrushSide {
            plane,
            surface: SURFACE_INDEX_INVALID,
            bevel: false,
        }));
        self.brushes.push(CBrush {
            contents,
            sides: BrushSides::Planes {
                first,
                count: self.sides.len() as u32 - first,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::trace::fixture::{self, Fixture};
    use crate::engine::trace::Ray;

    /// `PORTAL_HALF_WIDTH`, and the half-height this port settled on —
    /// `server::classes::portal::DEFAULT_HALF_HEIGHT`, which is 56 and not the
    /// reference tree's deliberately-wrong 14.
    const HALF_WIDTH: f32 = 32.0;
    const HALF_HEIGHT: f32 = 56.0;

    /// Where the four slabs begin: the hole is a tenth larger than the portal
    /// and the slabs stop another tenth back from it.
    const EDGE_RIGHT: f32 = HALF_WIDTH + HOLE_MOD + WALL_MIN_THICKNESS;
    const EDGE_UP: f32 = HALF_HEIGHT + HOLE_MOD + WALL_MIN_THICKNESS;

    /// A Portal 2 player, standing.
    const HULL_MIN: Vec3 = Vec3::new(-16.0, -16.0, 0.0);
    const HULL_MAX: Vec3 = Vec3::new(16.0, 16.0, 72.0);

    /// A wall slab across `x = -8 .. 0`, a floor well in front of and below
    /// it, and a portal on the wall's front face at the origin facing `+x`.
    ///
    /// With `angles` of zero the portal's basis is the world's: `forward` is
    /// `+X`, `up` is `+Z`, and `right` is `-Y` — the *negation* of the angle
    /// matrix's second column, which is the one thing about a portal's frame
    /// that is easy to get backwards.
    ///
    /// `axial` false builds the wall out of planes rather than as a box brush,
    /// which is the other half of [`Pieces::source_sides`] and changes nothing
    /// about the geometry.
    fn wall(axial: bool) -> (CollisionBsp, PortalHole) {
        let mut fixture = Fixture::default();
        let (mins, maxs) = (
            Vec3::new(-8.0, -256.0, -256.0),
            Vec3::new(0.0, 256.0, 256.0),
        );
        match axial {
            // A real surface, so that a trace stopping on the wall's own face
            // can be told from one stopping on a face the carve invented.
            true => fixture.add_surfaced_box(
                mins,
                maxs,
                Contents::SOLID,
                crate::engine::world::bsp::surf::NOLIGHT,
            ),
            false => fixture.add_box(mins, maxs, Contents::SOLID, false),
        };
        // Inside the World box (131 forward, 107 across, 131 up), so the
        // substitutive trace has something in front of the plane to keep.
        fixture.add_box(
            Vec3::new(0.0, -100.0, -70.0),
            Vec3::new(100.0, 100.0, -60.0),
            Contents::SOLID,
            true,
        );
        let collision = fixture.single_leaf();
        let hole = PortalHole::new(Vec3::ZERO, Vec3::ZERO, HALF_WIDTH, HALF_HEIGHT);
        (collision, hole)
    }

    /// One unlinked portal, as the store is told about it.
    fn live(id: u64, hole: PortalHole) -> LivePortal {
        LivePortal {
            id,
            hole,
            link: None,
        }
    }

    fn carve(collision: &CollisionBsp, hole: &PortalHole) -> CarvedWall {
        CarvedWall::build(&mut collision.tracer(), 1, hole, None)
    }

    /// Whether a *point* is inside anything in this model.
    ///
    /// A point rather than a box on purpose: the position test applies no
    /// Minkowski expansion to a zero-extent query, so it reads the pieces'
    /// true volumes and nothing the box sweep's plane offsets would add.
    fn solid_at(collision: &CollisionBsp, point: Vec3) -> bool {
        collision
            .tracer()
            .trace(&Ray::line(point, point), CARVE)
            .start_solid
    }

    /// A player-sized hull swept from one pair of **feet** positions to
    /// another.
    fn walk(from: Vec3, to: Vec3) -> Ray {
        Ray::hull(from, to, HULL_MIN, HULL_MAX)
    }

    /// Feet for a hull whose centre sits at `height` — the hull is 72 tall, so
    /// the two differ by 36, which is the mistake [`Ray`] exists to stop
    /// anyone making twice.
    fn feet(x: f32, y: f32, height: f32) -> Vec3 {
        Vec3::new(x, y, height - 36.0)
    }

    /// The whole of the carve, as one table: over a grid on the wall's face, a
    /// point is inside the carved geometry exactly when it is **outside** the
    /// hole rectangle.
    ///
    /// This is the test that catches a sign error in §4.3's distance table,
    /// and it needs no sweep, no BSP descent and no epsilon — a point either
    /// satisfies a piece's plane set or it does not.
    #[test]
    fn the_four_slabs_leave_a_hole_the_size_of_the_portal() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);

        let mut inside = 0;
        let mut outside = 0;
        // Inside the wall clip box, which bounds the pieces at 107 across and
        // 131 up; the step misses both slab edges by more than a unit.
        let mut y = -100.0f32;
        while y <= 100.0 {
            let mut z = -120.0f32;
            while z <= 120.0 {
                let point = Vec3::new(-4.0, y, z);
                // `right` is `-Y`, so the hole's half-width is measured along
                // `y` either way; `up` is `+Z`.
                let in_hole = y.abs() < EDGE_RIGHT && z.abs() < EDGE_UP;
                match in_hole {
                    true => inside += 1,
                    false => outside += 1,
                }
                assert_eq!(
                    solid_at(carved.collision(), point),
                    !in_hole,
                    "{point} is {} the hole",
                    if in_hole { "inside" } else { "outside" },
                );
                z += 4.0;
            }
            y += 4.0;
        }
        assert!(inside > 100 && outside > 100, "{inside} in, {outside} out");
    }

    /// The same table against a wall of plane brushes rather than box brushes,
    /// which is the other half of [`Pieces::source_sides`].
    #[test]
    fn a_wall_of_plane_brushes_carves_to_the_same_hole() {
        let (collision, hole) = wall(false);
        let carved = carve(&collision, &hole);

        for (y, z) in [
            (0.0, 0.0),
            (28.0, 40.0),
            (-28.0, -40.0),
            (60.0, 0.0),
            (0.0, 90.0),
        ] {
            let point = Vec3::new(-4.0, y, z);
            let in_hole = (y as f32).abs() < EDGE_RIGHT && (z as f32).abs() < EDGE_UP;
            assert_eq!(solid_at(carved.collision(), point), !in_hole, "{point}");
        }
    }

    /// A hull walks through the middle of the hole and is stopped by the wall
    /// beside it — the acceptance test `portdocs/PORTAL.md` §11 asks for.
    #[test]
    fn a_hull_passes_through_the_hole_and_not_through_the_wall() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);

        // Down the middle: the hull is 32 wide and 72 tall, the hole 64.2 by
        // 112.2, so it fits with 16 units to spare in each direction.
        let through = walk(feet(40.0, 0.0, 0.0), feet(-40.0, 0.0, 0.0));
        let hit = carved.collision().tracer().trace(&through, CARVE);
        assert_eq!(hit.fraction, 1.0, "the hull did not get through the hole");
        assert!(!hit.start_solid);

        // The same sweep against the uncarved world stops at the wall, which
        // is what makes the line above mean something.
        let real = collision.tracer().trace(&through, CARVE);
        assert!(real.fraction < 1.0 && !real.start_solid, "{real:?}");

        // High enough that the hull's underside clears the top of the hole.
        let above = walk(feet(40.0, 0.0, 96.0), feet(-40.0, 0.0, 96.0));
        let hit = carved.collision().tracer().trace(&above, CARVE);
        let real = collision.tracer().trace(&above, CARVE);
        assert!(
            hit.fraction < 1.0,
            "the wall above the hole was carved away"
        );
        assert!(
            (hit.fraction - real.fraction).abs() < 1e-4,
            "the carve moved the wall: {} against {}",
            hit.fraction,
            real.fraction
        );
    }

    /// The substitutive path end to end: the hole is taken where there is one
    /// and the world is kept everywhere else.
    #[test]
    fn the_substitutive_trace_takes_the_hole_and_keeps_the_rest() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);

        let through = walk(feet(40.0, 0.0, 0.0), feet(-40.0, 0.0, 0.0));
        let with = collision.tracer().with_hole(&carved).trace(&through, CARVE);
        let without = collision.tracer().trace(&through, CARVE);
        assert_eq!(with.fraction, 1.0, "the hole was not substituted in");
        assert!(
            without.fraction < 1.0,
            "the wall was not there to begin with"
        );

        let above = walk(feet(40.0, 0.0, 96.0), feet(-40.0, 0.0, 96.0));
        let with = collision.tracer().with_hole(&carved).trace(&above, CARVE);
        let without = collision.tracer().trace(&above, CARVE);
        assert_eq!(with.fraction, without.fraction, "the wall moved");
    }

    /// **The case that needs the World set.** A hull dropped in front of the
    /// portal lands on the floor whether or not a hole is attached.
    ///
    /// Without the geometry in front of the plane in the carved store the
    /// substitutive rule — take whichever trace went further — would find
    /// nothing under the player, prefer it, and drop them through the floor.
    #[test]
    fn the_carved_store_keeps_the_floor_in_front_of_the_portal() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);

        let down = walk(Vec3::new(30.0, 0.0, -20.0), Vec3::new(30.0, 0.0, -100.0));
        let without = collision.tracer().trace(&down, CARVE);
        assert!(without.did_hit(), "the fixture has no floor");

        let with = collision.tracer().with_hole(&carved).trace(&down, CARVE);
        assert_eq!(
            with.fraction, without.fraction,
            "the floor in front of the portal was not in the carved store"
        );
    }

    /// A player standing *in* the hole is inside the wall as far as the real
    /// world is concerned, and not inside anything as far as the carve is —
    /// which is the `startsolid` branch of the reconciliation and the reason
    /// it exists.
    #[test]
    fn a_hull_standing_in_the_hole_is_not_solid() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);

        let standing = walk(feet(-4.0, 0.0, 0.0), feet(-4.0, 0.0, 0.0));
        assert!(
            collision.tracer().trace(&standing, CARVE).start_solid,
            "the fixture's wall is not where the test thinks it is"
        );
        assert!(
            !collision
                .tracer()
                .with_hole(&carved)
                .trace(&standing, CARVE)
                .start_solid
        );
    }

    /// A cut face is not a surface: it reports the null material, where a face
    /// the wall actually has reports the wall's.
    #[test]
    fn a_cut_face_has_no_material_and_the_walls_own_faces_keep_theirs() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);
        let pieces = carved.collision();

        // Into the front of the wall, above the hole: the wall's own face.
        let at_the_wall = Ray::line(Vec3::new(40.0, 0.0, 96.0), Vec3::new(-40.0, 0.0, 96.0));
        let hit = pieces.tracer().trace(&at_the_wall, CARVE);
        assert!(hit.did_hit());
        assert_eq!(
            pieces.surface_name(hit.surface),
            collision.surface_name(Some(0))
        );
        assert_ne!(pieces.surface_name(hit.surface), "**empty**");

        // Sideways out of the hole into the slab beside it: a face the carve
        // invented, so there is no material to report.
        let out_of_the_hole = Ray::line(Vec3::new(-4.0, 0.0, 0.0), Vec3::new(-4.0, 200.0, 0.0));
        let hit = pieces.tracer().trace(&out_of_the_hole, CARVE);
        assert!(hit.did_hit(), "the slab beside the hole is missing");
        assert_eq!(pieces.surface_name(hit.surface), "**empty**");
        // The slab's inner face, a tenth of a unit outside the hole.
        assert!(
            (hit.end.y - EDGE_RIGHT).abs() < 0.1,
            "the hole's edge is at {}",
            hit.end.y
        );
    }

    /// The two query boxes, which are the easiest thing in this module to get
    /// the wrong way round: the wall is *behind* the portal and the world is
    /// *in front* of it.
    #[test]
    fn the_wall_box_is_behind_the_plane_and_the_world_box_in_front() {
        let (_, hole) = wall(true);

        let (mins, maxs) = hole.wall_bounds();
        assert_eq!(
            mins,
            Vec3::new(-2.0 * HALF_HEIGHT, -4.0 * HALF_WIDTH, -4.0 * HALF_HEIGHT)
        );
        assert_eq!(maxs, Vec3::new(0.0, 4.0 * HALF_WIDTH, 4.0 * HALF_HEIGHT));

        let extents = hole.clone_extents();
        let (mins, maxs) = hole.world_bounds();
        assert_eq!(mins, Vec3::new(0.0, -extents.y, -extents.z));
        assert_eq!(maxs, Vec3::new(extents.x, extents.y, extents.z));
        assert_eq!(
            extents,
            Vec3::new(HALF_HEIGHT + 75.0, HALF_WIDTH + 75.0, HALF_HEIGHT + 75.0)
        );
    }

    /// The trigger volume reaches 64 units in front of the portal and nothing
    /// at all behind it — `GetLocalMaxs().x` and a one-sided box.
    #[test]
    fn the_trigger_box_reaches_forward_of_the_portal_and_not_behind_it() {
        let (_, hole) = wall(true);
        let touches = |x: f32, y: f32, height: f32| {
            let feet = feet(x, y, height);
            hole.touches(feet + HULL_MIN, feet + HULL_MAX)
        };
        assert!(touches(20.0, 0.0, 0.0), "up against the portal");
        assert!(touches(-4.0, 0.0, 0.0), "standing in the hole");
        assert!(!touches(200.0, 0.0, 0.0), "across the room");
        assert!(!touches(-60.0, 0.0, 0.0), "behind the wall");
        assert!(!touches(20.0, 0.0, 200.0), "above it");
        assert!(!touches(20.0, 200.0, 0.0), "beside it");
    }

    /// The store's whole lifecycle: carved on arrival, rebuilt when the portal
    /// moves, kept when it does not, and dropped when it goes.
    #[test]
    fn the_store_carves_on_arrival_rebuilds_on_a_move_and_drops_on_removal() {
        let (collision, hole) = wall(true);
        let mut holes = PortalHoles::default();
        assert!(holes.is_empty());

        holes.sync(&collision, &[live(7, hole)]);
        let carved = holes.get(7).expect("a portal that arrived was carved");
        assert!(carved.pieces() > 0, "{}", carved.summary());
        assert_eq!(carved.hole(), &hole);
        assert_eq!(carved.id(), 7);

        // Unchanged: the same placement, and nothing is asked of the map.
        holes.sync(&collision, &[live(7, hole)]);
        assert_eq!(holes.get(7).expect("still there").hole(), &hole);

        // `NewLocation`: the hole follows the portal.
        let moved = PortalHole::new(
            Vec3::new(0.0, 80.0, 0.0),
            Vec3::ZERO,
            HALF_WIDTH,
            HALF_HEIGHT,
        );
        holes.sync(&collision, &[live(7, moved)]);
        assert_eq!(holes.get(7).expect("still there").hole(), &moved);
        let carved = holes.get(7).expect("still there");
        assert!(solid_at(carved.collision(), Vec3::new(-4.0, 0.0, 0.0)));
        assert!(!solid_at(carved.collision(), Vec3::new(-4.0, 80.0, 0.0)));

        // The player is in it, and in nothing else.
        let in_front = feet(20.0, 80.0, 0.0);
        assert_eq!(
            holes
                .touching(in_front + HULL_MIN, in_front + HULL_MAX)
                .map(|wall| wall.id()),
            Some(7)
        );
        let elsewhere = feet(20.0, 0.0, 0.0);
        assert!(holes
            .touching(elsewhere + HULL_MIN, elsewhere + HULL_MAX)
            .is_none());

        // Deactivated.
        holes.sync(&collision, &[]);
        assert!(holes.is_empty());
        assert!(holes.get(7).is_none());
    }

    /// A portal in mid-air carves nothing and is not an error — four of the
    /// game's twenty-one are exactly this, parked by their map and moved from
    /// script.
    #[test]
    fn a_portal_with_no_wall_behind_it_carves_nothing() {
        let (collision, _) = wall(true);
        let hole = PortalHole::new(
            Vec3::new(600.0, 0.0, 0.0),
            Vec3::ZERO,
            HALF_WIDTH,
            HALF_HEIGHT,
        );
        let carved = carve(&collision, &hole);
        assert_eq!(carved.sources(), 0, "{}", carved.summary());
        assert_eq!(carved.pieces(), 0);

        // And a trace against nothing is a clean miss rather than a panic.
        let through = walk(feet(640.0, 0.0, 0.0), feet(560.0, 0.0, 0.0));
        let hit = carved.collision().tracer().trace(&through, CARVE);
        assert_eq!(hit.fraction, 1.0);
        assert!(!hit.start_solid && !hit.all_solid);
    }

    /// The tube is a sleeve, not a wall: it fills the tenth of a unit between
    /// the hole's edge and the carved wall's, and only for the first unit of
    /// depth.
    ///
    /// Three points a tenth of a unit apart decide it, which is why this is a
    /// test and not a reading of the constants.
    #[test]
    fn the_tube_lines_the_hole_and_stops_a_unit_in() {
        let (collision, hole) = wall(true);
        let carved = carve(&collision, &hole);
        let tube = carved.tube();

        // Along `up`, half a unit behind the plane: inside the portal, in the
        // seam, and past the seam.
        let at = |up: f32, depth: f32| solid_at(tube, Vec3::new(-depth, 0.0, up));
        assert!(!at(HALF_HEIGHT, 0.5), "the portal's own opening is blocked");
        assert!(
            at(HALF_HEIGHT + HOLE_MOD + WALL_MIN_THICKNESS * 0.5, 0.5),
            "the seam between the hole and the wall is not lined"
        );
        assert!(
            !at(HALF_HEIGHT + HOLE_MOD + WALL_MIN_THICKNESS * 2.0, 0.5),
            "the sleeve is thicker than PORTAL_WALL_MIN_THICKNESS"
        );

        // And it is one unit deep, so past that the seam is open again — which
        // is what makes it a *guide* rather than a narrower hole.
        let seam = HALF_HEIGHT + HOLE_MOD + WALL_MIN_THICKNESS * 0.5;
        assert!(at(seam, TUBE_OFFSET + TUBE_DEPTH * 0.5));
        assert!(!at(seam, TUBE_OFFSET + TUBE_DEPTH * 2.0));
        // Nothing in front of the plane at all.
        assert!(!at(seam, -0.5));

        // Four slabs, always.
        assert_eq!(tube.brushes.len(), 4);
    }

    /// The remote ray puts the player's box at the **exit** portal, with the
    /// delta rotated and the extents left alone.
    #[test]
    fn the_remote_ray_asks_the_exit_portal_about_the_same_sweep() {
        let rooms = fixture::portal_rooms();
        let mut holes = PortalHoles::default();
        holes.sync(&rooms.collision, &rooms.live());
        let blue = holes.get(fixture::PortalRooms::BLUE_ID).expect("carved");

        // A hull standing in blue's hole, sweeping two units down.
        let feet = Vec3::new(-4.0, 0.0, fixture::PORTAL_ROOM_FLOOR);
        let ray = walk(feet, feet - Vec3::Z * 2.0);
        let (far, shift) = blue.remote_ray(&ray, None).expect("a linked portal");

        // Both portals are on walls and the hull is the same, so nothing has to
        // shift along the exit normal.
        assert_eq!(shift, Vec3::ZERO);
        // Four units behind blue's plane becomes four units in front of
        // orange's, and orange faces `+Y`.
        let centre = feet + Vec3::new(0.0, 0.0, 36.0);
        assert!(
            (far.origin() - Vec3::new(1000.0, 4.0, centre.z)).length() < 1e-3,
            "the box landed at {}",
            far.origin()
        );
        // Down is still down: both portals' up is world up.
        assert!((far.delta - Vec3::new(0.0, 0.0, -2.0)).length() < 1e-3);
        assert_eq!(far.extents, ray.extents);
    }

    /// **The whole of stage 4's first half.** A box straddling the portal
    /// plane is held up by a ledge in the room at the *other* end, and the
    /// answer comes back in this room's frame.
    ///
    /// The ledge is higher than anything on this side, which is what makes the
    /// test mean something: **the bottom of the hole is itself a ledge** — the
    /// wall below the portal is still solid — so a carve with no far side
    /// catches the player too, just lower down. Two answers that differ only
    /// in the last decimal place would prove nothing, which is why this fixture
    /// puts a platform at the far end 16 units above the hole's own lip.
    #[test]
    fn a_ledge_in_the_far_room_holds_a_player_up_through_the_portal() {
        // In orange's room, over where a player entering blue comes out, with
        // its top well above blue's hole lip.
        const LEDGE_TOP: f32 = -40.0;
        let ledge = (
            Vec3::new(960.0, 0.0, LEDGE_TOP - 8.0),
            Vec3::new(1040.0, 60.0, LEDGE_TOP),
        );
        let rooms = fixture::portal_rooms_with(&[ledge]);

        // Straddling: the box's centre is a unit past blue's plane, so half of
        // it is in the tunnel and half still in the room. Dropped from well
        // above the hole's lip so that both answers are real fractions.
        let feet = Vec3::new(-1.0, 0.0, -10.0);
        let ray = walk(feet, feet - Vec3::Z * 60.0);

        let real = rooms
            .collision
            .tracer()
            .trace(&ray, Contents::MASK_PLAYERSOLID);
        assert!(
            real.start_solid,
            "the fixture's wall is not where it thinks"
        );

        let mut holes = PortalHoles::default();
        holes.sync(&rooms.collision, &rooms.live());
        let blue = holes.get(fixture::PortalRooms::BLUE_ID).expect("carved");
        let landed = rooms
            .collision
            .tracer()
            .with_hole(blue)
            .trace(&ray, Contents::MASK_PLAYERSOLID);

        // The normal came back through the matrix: both portals' up is world
        // up, so a floor at the far end is still a floor here.
        assert!(
            (landed.normal - Vec3::Z).length() < 1e-3,
            "{}",
            landed.normal
        );
        // And the endpoint is in the **caller's** frame — the player's feet, on
        // this side of the portal, at the height of the far room's ledge.
        assert!(
            (landed.end - Vec3::new(feet.x, 0.0, LEDGE_TOP)).length() < 0.1,
            "the player ended at {}",
            landed.end
        );

        // The control: the same carve with the pair broken. **This** room's
        // floor still catches them — the box is straddling, so half of it is
        // still over it — 16 units lower and later.
        let mut unlinked = PortalHoles::default();
        unlinked.sync(&rooms.collision, &rooms.unlinked());
        let alone = unlinked.get(fixture::PortalRooms::BLUE_ID).expect("carved");
        let fell = rooms
            .collision
            .tracer()
            .with_hole(alone)
            .trace(&ray, Contents::MASK_PLAYERSOLID);
        assert!(
            fell.fraction > landed.fraction + 0.1,
            "the far room's ledge was not preferred: {} against {}",
            landed.fraction,
            fell.fraction
        );
        assert!(
            (fell.end.z - fixture::PORTAL_ROOM_FLOOR).abs() < 0.1,
            "the unlinked carve stopped at {} rather than on this room's floor",
            fell.end.z
        );
    }

    /// A portal that finds or loses a partner is recarved, because the far side
    /// is part of what was carved.
    #[test]
    fn finding_a_partner_recarves_the_portal() {
        let rooms = fixture::portal_rooms();
        let mut holes = PortalHoles::default();

        holes.sync(&rooms.collision, &rooms.unlinked());
        let blue = holes.get(fixture::PortalRooms::BLUE_ID).expect("carved");
        assert!(blue.remote().is_none() && blue.link().is_none());
        let alone = blue.pieces();

        holes.sync(&rooms.collision, &rooms.live());
        let blue = holes.get(fixture::PortalRooms::BLUE_ID).expect("carved");
        let remote = blue.remote().expect("the far side was carved");
        assert_eq!(
            blue.link().map(|link| link.exit_id),
            Some(fixture::PortalRooms::ORANGE_ID)
        );
        // The pieces on *this* side did not change — only the far set arrived.
        assert_eq!(blue.pieces(), alone);
        // The far room's floor, plus the four slabs of the moved tube.
        assert!(
            remote.brushes.len() > 4,
            "the remote set is only the tube: {}",
            blue.summary()
        );

        // …and the moved tube is in *front* of the exit plane, not behind it,
        // which is the half turn in the matrix and the reason this cannot be
        // the exit portal's own tube.
        let seam = 32.0 + HOLE_MOD + WALL_MIN_THICKNESS * 0.5;
        let front = Vec3::new(1000.0 + seam, TUBE_OFFSET + TUBE_DEPTH * 0.5, 0.0);
        let behind = Vec3::new(1000.0 + seam, -(TUBE_OFFSET + TUBE_DEPTH * 0.5), 0.0);
        assert!(solid_at(remote, front), "the moved tube is not in front");
        assert!(!solid_at(remote, behind), "the moved tube is behind");

        holes.sync(&rooms.collision, &rooms.unlinked());
        assert!(holes
            .get(fixture::PortalRooms::BLUE_ID)
            .expect("carved")
            .remote()
            .is_none());
    }

    /// Every `prop_portal` the game places, carved against the map it is on:
    /// **the hole is exactly the hole, and nothing else moved.**
    ///
    /// Two properties, both decided with *point* tests so that no Minkowski
    /// expansion is involved and the pieces are read as the volumes they are:
    ///
    /// 1. **Inside the hole rectangle and behind the plane, nothing is
    ///    solid** — whatever the map had there, the carve took it away.
    /// 2. **Outside it, the carved answer is the uncarved one**, brush for
    ///    brush. The reference is the same enumerated brushes with no clip and
    ///    no side planes, which is the only comparison that means anything: the
    ///    *world's* own position test would answer "solid" for a point in the
    ///    void outside the map, where the carve holds no brushes and correctly
    ///    answers nothing.
    ///
    /// The sample points stay inside the wall clip box in all three
    /// directions, because the carve deliberately holds nothing beyond it.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release carves_a_hole -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn every_shipped_portal_carves_a_hole_in_its_wall() {
        use crate::engine::world::bsp::Bsp;

        let Ok(dir) = std::env::var("KISAK_GAME_DIR") else {
            panic!("set KISAK_GAME_DIR to a directory holding gameinfo.txt");
        };
        let dir = std::path::PathBuf::from(dir);
        let base = dir.parent().unwrap_or(&dir).to_path_buf();
        let vfs = crate::filesystem::Vfs::mount_game(&dir, &base, &Default::default())
            .expect("mount the game");

        let mut names: Vec<String> = vfs
            .list("maps")
            .expect("maps/")
            .into_iter()
            .filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".bsp"))
            .map(|e| e.name.trim_end_matches(".bsp").to_owned())
            .collect();
        names.sort();

        let number = |value: Option<&str>, or: f32| -> f32 {
            value.and_then(|v| v.trim().parse().ok()).unwrap_or(or)
        };
        let vector = |value: Option<&str>| -> Vec3 {
            let mut parts = value.unwrap_or("").split_whitespace();
            let mut next = || parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            Vec3::new(next(), next(), next())
        };

        let (mut portals, mut maps) = (0usize, 0usize);
        let (mut sources, mut pieces, mut planes, mut empty) = (0usize, 0usize, 0usize, 0usize);
        let (mut inside, mut outside, mut solid_outside) = (0usize, 0usize, 0usize);
        let (mut blocked, mut walked_in, mut open, mut in_solid) = (0usize, 0usize, 0usize, 0usize);
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;

        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let entities = bsp.entities();
            let placed: Vec<_> = entities
                .iter()
                .filter(|e| e.classname() == Some("prop_portal"))
                .collect();
            if placed.is_empty() {
                continue;
            }
            maps += 1;
            let collision = CollisionBsp::build(&bsp);

            for entity in placed {
                portals += 1;
                let hole = PortalHole::new(
                    vector(entity.get("origin")),
                    vector(entity.get("angles")),
                    number(entity.get("HalfWidth"), HALF_WIDTH),
                    number(entity.get("HalfHeight"), HALF_HEIGHT),
                );

                let started = std::time::Instant::now();
                let carved =
                    CarvedWall::build(&mut collision.tracer(), portals as u64, &hole, None);
                let took = started.elapsed();
                worst = worst.max(took);
                total += took;

                // The reference: the wall box's brushes, uncut. Built from the
                // same enumeration and the same remapping, so the two models
                // differ in the clip and side planes and in nothing else.
                let wall = {
                    let (lo, hi) = hole.wall_bounds();
                    collision.tracer().brushes_in_box(lo, hi, CARVE)
                };
                let uncut = {
                    let mut out = Pieces::default();
                    for &index in &wall {
                        let own = out.source_sides(&collision, index);
                        out.piece(collision.brushes[index].contents, &own, &[]);
                    }
                    CollisionBsp::from_pieces(out.planes, out.sides, out.brushes, out.surfaces)
                };

                sources += carved.sources();
                pieces += carved.pieces();
                planes += carved.pieces.planes.len();
                // What the four-way split would have produced with nothing
                // dropped; the World set contributes one piece each.
                empty += 4 * wall.len() + (carved.sources() - wall.len()) - carved.pieces();

                // **The stage's own outcome**: a player-sized hull walked
                // straight at the portal. The real world stops it at the wall;
                // with the hole attached it has to get further. Measured rather
                // than asserted per portal, because the two that are *proud* of
                // their wall keep a thin slab of it in front of the plane — the
                // World set's, exactly as the shipped game does — and the four
                // parked in mid-air have no wall at all.
                let centre = -(HULL_MIN + HULL_MAX) * 0.5;
                let walk = Ray::hull(
                    hole.center + hole.forward * 40.0 + centre,
                    hole.center - hole.forward * 40.0 + centre,
                    HULL_MIN,
                    HULL_MAX,
                );
                let real = collision.tracer().trace(&walk, Contents::MASK_PLAYERSOLID);
                if real.start_solid {
                    in_solid += 1;
                } else if !real.did_hit() {
                    open += 1;
                } else {
                    blocked += 1;
                    let through = collision
                        .tracer()
                        .with_hole(&carved)
                        .trace(&walk, Contents::MASK_PLAYERSOLID);
                    match through.fraction > real.fraction + 1e-3 {
                        true => walked_in += 1,
                        false => println!(
                            "  {name}/{}: the hull still stops at {:.4}",
                            entity.get("targetname").unwrap_or("?"),
                            real.fraction
                        ),
                    }
                }

                let (hw, hh) = (hole.half_width, hole.half_height);
                // Across and up, as offsets from the centre. Everything stays
                // inside `vCollisionCloneExtents` — 107 across and 131 up for a
                // shipped portal — because the carve holds nothing beyond it.
                let across = [
                    0.0,
                    hw * 0.5,
                    -hw * 0.5,
                    hw + 4.0,
                    -hw - 4.0,
                    hw + 60.0,
                    -hw - 60.0,
                ];
                let ups = [
                    0.0,
                    hh * 0.5,
                    -hh * 0.5,
                    hh + 4.0,
                    -hh - 4.0,
                    hh + 30.0,
                    -hh - 30.0,
                ];
                for depth in [2.0f32, 6.0, 12.0, 24.0, 40.0] {
                    for right in across {
                        for up in ups {
                            let point = hole.center - hole.forward * depth
                                + hole.right * right
                                + hole.up * up;
                            let carved_solid = solid_at(carved.collision(), point);
                            match right.abs() <= hw && up.abs() <= hh {
                                true => {
                                    inside += 1;
                                    assert!(
                                        !carved_solid,
                                        "{name}: {point} is {depth} behind the portal at \
                                         ({right}, {up}), inside the hole, and still solid"
                                    );
                                }
                                false => {
                                    outside += 1;
                                    let was = solid_at(&uncut, point);
                                    solid_outside += usize::from(was);
                                    assert_eq!(
                                        carved_solid, was,
                                        "{name}: {point} is {depth} behind the portal at \
                                         ({right}, {up}), outside the hole, and the carve \
                                         changed it"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        println!(
            "{portals} portals across {maps} maps: {sources} brushes carved into {pieces} \
             pieces over {planes} planes, {empty} pieces dropped as empty ({:.1}%);\n  \
             {inside} points inside the hole, all of them clear; {outside} outside it, \
             {solid_outside} of them solid and every one unchanged;\n  \
             a player hull walked at {blocked} of them and got into {walked_in}; \
             {open} had nothing in the way and {in_solid} started solid;\n  \
             carving one portal took {:.2} ms on average and {:.2} ms at worst.",
            match pieces + empty {
                0 => 0.0,
                n => 100.0 * empty as f32 / n as f32,
            },
            total.as_secs_f32() * 1000.0 / portals.max(1) as f32,
            worst.as_secs_f32() * 1000.0,
        );
        assert_eq!(portals, 21, "twenty-one portals in the game");
        assert!(pieces > 0, "not one portal carved anything");
        // Four of the twenty-one are parked in mid-air by their map and moved
        // from script, so they have no wall to cut; the rest do.
        assert!(
            walked_in * 2 > blocked,
            "a hull got into only {walked_in} of the {blocked} portals that stopped it"
        );
        assert!(
            solid_outside * 4 > outside,
            "only {solid_outside} of {outside} points outside the holes were solid, \
             so the portals are not on walls and the test proves nothing"
        );
    }

    /// An empty piece has to be dropped, not left to the plane loop.
    ///
    /// The wall's own front face and the World set's clip plane are
    /// anti-parallel and a sixteenth of a unit apart the wrong way round, so
    /// the World piece of the wall brush encloses nothing — and a *swept box*
    /// would clip against both of them pushed out by the hull's extents and
    /// find a slab a whole hull thick across the middle of the hole. This is
    /// the test for the thing `portdocs/PORTAL.md` §4.3 said could not happen.
    #[test]
    fn an_empty_piece_does_not_become_a_pane_of_glass_across_the_hole() {
        let (collision, hole) = wall(true);
        let mut out = Pieces::default();
        let clip = clip_planes(&mut out, &hole);
        let own = out.source_sides(&collision, 0);
        out.piece(Contents::SOLID, &own, &clip.world);
        assert!(
            out.brushes.is_empty(),
            "the wall's World piece encloses nothing and was kept anyway"
        );

        // …and with it dropped, a hull really does get through.
        let carved = carve(&collision, &hole);
        let through = walk(feet(40.0, 0.0, 0.0), feet(-40.0, 0.0, 0.0));
        assert_eq!(
            carved.collision().tracer().trace(&through, CARVE).fraction,
            1.0
        );
    }
}
