//! Where a portal may go — `game/shared/portal/portal_placement.cpp` (1,663
//! lines), the rules the portal gun fires through.
//!
//! One question, answered by a great many traces: given the point a shot hit
//! and the angles the surface gives it, is there room for a 64 x 112 oval
//! there, and if not, is there room a little to one side? The answer is a
//! [`PlacementResult`] and, when it is a success, the position the portal was
//! **bumped** to — which is the whole of Portal 2's "the portal slides into
//! the corner" feel. [`verify`] is `VerifyPortalPlacement`, and everything
//! else here is something it calls.
//!
//! # How the fit works
//!
//! [`fit_on_surface`] traces from the portal's centre towards each of its
//! four corners, three ways at once: one unit **inside** the wall (does the
//! surface stop before the corner?), one unit **in front** of it (is there a
//! wall across the way?), and against the other portals and the bumping
//! entities. Each corner that comes up short is an *intersection* with a
//! line, and the portal is moved away from the lines until no corner crosses
//! one — by one of five cases on how many corners hit and how their lines
//! meet, recursing at most six times. It is Valve's algorithm case for case;
//! the recursion and its bookkeeping are kept, including the places the C++
//! copies whole fit records over each other.
//!
//! # What this module reaches the world through
//!
//! [`ShotWorld`] — a snapshot of the server's entities that placement asks
//! about (the portals, the bumpers, the no-portal volumes, the brush entities
//! a trace can stop on), the engine's [`TouchQuery`] for the world, and the
//! physics environment for the studio models the engine does not hold. Every
//! trace goes through [`ShotWorld::trace_line`], which is `UTIL_TraceLine`
//! with the portal-shot filter.
//!
//! # Deliberately absent
//!
//! - **Paint.** `IsOnPortalPaint` makes any surface painted with portal gel
//!   portalable. There is no paint, so it is always false.
//! - **`UTIL_TestForOrientationVolumes`** is `#if !defined( PORTAL2 )`.
//! - **`prop_door_rotating`** is special-cased in two places here and placed
//!   by no Portal 2 map.
//! - **The implicit-velocity half of "is this surface moving"**
//!   (`UTIL_IsEntityMovingOrRotating`'s *"func_brush attached to an animating
//!   entity doesn't give the entity a velocity"*). This port's movers carry
//!   their velocity on the entity; a brush riding an animation is covered by
//!   the parent walk.
//! - **The debug overlay** (`sv_portal_placement_debug`).

use glam::Vec3;

use crate::math::angle_vectors;

use super::entity::{EntityId, EntityList};
use super::physics::Physics;
use super::{ShotHit, TouchQuery};

// ---------------------------------------------------------------------------
// The fixed vocabulary
// ---------------------------------------------------------------------------

/// `CONTENTS_SOLID`.
const CONTENTS_SOLID: u32 = 0x1;
/// `CONTENTS_SLIME`.
const CONTENTS_SLIME: u32 = 0x10;
/// `CONTENTS_WATER`.
const CONTENTS_WATER: u32 = 0x20;
/// `CONTENTS_MONSTER`.
const CONTENTS_MONSTER: u32 = 0x2000000;
/// `MASK_SHOT_PORTAL` (`public/bspflags.h:138`) — solid, moveable, window,
/// monster. No `GRATE`: a portal is not shot through a grate.
pub const MASK_SHOT_PORTAL: u32 = 0x1 | 0x4000 | 0x2 | CONTENTS_MONSTER;
/// `MASK_SOLID_BRUSHONLY` — solid, moveable, window, grate.
pub const MASK_SOLID_BRUSHONLY: u32 = 0x1 | 0x4000 | 0x2 | 0x8;
/// `MASK_ALL`.
const MASK_ALL: u32 = 0xFFFF_FFFF;

/// `SURF_SKY` (`public/bspflags.h:79`).
const SURF_SKY: i32 = 0x0004;
/// `SURF_NOPORTAL` (`public/bspflags.h:84`) — *"the surface can not have a
/// portal placed on it"*.
///
/// > **`PortalSurfaceType` does not read this constant.** It reads
/// > `CEG_SURF_NO_PORTAL_FLAG`, which starts as `0xffff` — *"portals can't be
/// > placed until correctly initialized"* — and is overwritten at DLL init by
/// > `CEG_GET_CONSTANT_VALUE( SurfNoPortalFlag )`, an anti-tamper macro whose
/// > value is not in this tree. `bspflags.h` is, and the only flag with this
/// > meaning is `0x20`; the `0xffff` default would refuse every surface with
/// > *any* flag, lightmapped walls included, which is not the shipped game.
const SURF_NOPORTAL: i32 = 0x0020;

/// `CHAR_TEX_GLASS` (`game/shared/decals.h`) — `'Y'`.
const CHAR_TEX_GLASS: u16 = b'Y' as u16;

/// `PORTAL_BUMP_FORGIVENESS` (`portal_shareddefs.h:19`) — how far inside its
/// real edges a portal's corners are fitted, and how much further each bump
/// goes than it strictly has to.
pub const PORTAL_BUMP_FORGIVENESS: f32 = 2.0;

/// `g_ppszPortalPassThroughMaterials` (`portal_shareddefs.cpp:9`) — a shot
/// passes *through* a surface wearing one of these, as it does through sky.
/// Matched as a substring of the lowercased name, as `IsMaterialInList` does.
const PASS_THROUGH_MATERIALS: &[&str] = &["lights/light_orange001"];

/// How many times [`fit_on_surface`] may recurse — `if ( iRecursions >= 6 )`.
const MAX_FIT_RECURSIONS: u32 = 6;

/// `LINE_EPS` (`mathlib_base.cpp:3918`).
const LINE_EPS: f32 = 0.000_001;

/// `PortalPlacementResult_t` (`portal_shareddefs.h:21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementResult {
    /// Placed exactly where shot.
    Success,
    /// A placement helper positioned it.
    UsedHelper,
    /// Placed, but moved to fit.
    Bumped,
    /// No room.
    CantFit,
    /// The shot hit a fizzler.
    Cleanser,
    /// It would overlap a portal that cannot move — in single player, any.
    OverlapLinked,
    /// It would overlap the co-op partner's portal. Unreachable in single
    /// player, where every portal counts as linked — kept because it is the
    /// vocabulary, and the co-op half of `IsPortalOverlappingOtherPortals` is
    /// where it would come from.
    #[allow(dead_code)]
    OverlapPartnerPortal,
    /// Inside a `func_noportal_volume`.
    InvalidVolume,
    /// On a surface a portal may not go on.
    InvalidSurface,
    /// The shot went through, or hit nothing at all.
    PassthroughSurface,
}

impl PlacementResult {
    /// `PortalPlacementSucceeded` (`portal_placement.cpp:1644`).
    pub fn succeeded(self) -> bool {
        matches!(
            self,
            PlacementResult::Success | PlacementResult::Bumped | PlacementResult::UsedHelper
        )
    }
}

/// `PortalPlacedBy_t` (`portal_shareddefs.h:58`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacedBy {
    /// A map's own portal, activated where it was put.
    Fixed,
    /// A gun with no owner — `weapon_portalgun` on a pedestal, firing from
    /// its `muzzle` attachment. Every shipped pedestal in single player is a
    /// `prop_dynamic` the player takes the gun *from*, so nothing here fires
    /// as one.
    #[allow(dead_code)]
    Pedestal,
    /// The player's gun.
    Player,
}

// ---------------------------------------------------------------------------
// What placement asks the server about
// ---------------------------------------------------------------------------

/// One `prop_portal`, as placement sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortalInfo {
    pub id: EntityId,
    pub origin: Vec3,
    pub angles: Vec3,
    pub half_width: f32,
    pub half_height: f32,
    /// `IsActive()`.
    pub active: bool,
    /// `GetFiredByPlayer()` — who shot it here, `None` for a map's own.
    pub fired_by: Option<EntityId>,
}

/// Which of the three brush classes `TraceBumpingEntities` asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumperKind {
    /// `func_portal_bumper` — a **soft** bump: it moves a portal once and is
    /// then ignored for the rest of the fit.
    Bumper,
    /// `trigger_portal_cleanser` — a fizzler, which a portal is fitted away
    /// from as if it were a wall.
    Cleanser,
    /// `func_noportal_volume`, fitted away from with two units to spare.
    NoPortalVolume,
}

/// One active bumping entity: its brush model, where it is, and its box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bumper {
    pub id: EntityId,
    pub kind: BumperKind,
    /// `"*N"`'s `N`.
    pub model: usize,
    pub origin: Vec3,
    pub angles: Vec3,
    /// `WorldSpaceSurroundingBounds`, which is also the broadphase
    /// `AllEdictsAlongRay` stands in for.
    pub mins: Vec3,
    pub maxs: Vec3,
}

/// One enabled `info_placement_helper`, as the gun's snap reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Helper {
    pub id: EntityId,
    pub origin: Vec3,
    /// `GetTargetAngles()` — the entity's own angles.
    pub angles: Vec3,
    /// `GetTargetRadius()`.
    pub radius: f32,
    /// `ShouldUseHelperAngles()`.
    pub use_angles: bool,
}

/// A brush entity a trace can stop on, and the two things placement asks
/// about it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BrushInfo {
    model: usize,
    id: EntityId,
    /// `FClassnameIs( tr.m_pEnt, "func_door" )`.
    is_func_door: bool,
    /// `UTIL_IsEntityMovingOrRotating`.
    moving: bool,
}

/// The player, for the vertical-hop guard at the end of [`verify`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerBox {
    pub origin: Vec3,
    pub mins: Vec3,
    pub maxs: Vec3,
}

/// `trace_t`, as placement reads it: a [`ShotHit`] with the entity resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trace {
    pub hit: ShotHit,
    /// `m_pEnt` — the brush entity, portal or bumper the trace stopped on.
    /// `None` for the world, a static prop, or nothing.
    pub entity: Option<EntityId>,
    /// The surface was a studio model's — `surface.name == "**studio**"`.
    pub studio: bool,
}

impl Trace {
    /// `UTIL_ClearTrace` — every field zero but the fraction.
    fn clear() -> Trace {
        Trace {
            hit: ShotHit::miss(Vec3::ZERO, Vec3::ZERO),
            entity: None,
            studio: false,
        }
    }

    fn fraction(&self) -> f32 {
        self.hit.fraction
    }
}

/// Everything placement can see. See the module docs.
pub struct ShotWorld<'a> {
    query: &'a mut dyn TouchQuery,
    physics: Option<&'a Physics>,
    /// `CProp_Portal_Shared::AllPortals`, active or not.
    pub portals: Vec<PortalInfo>,
    /// The active bumpers, fizzlers and no-portal volumes.
    pub bumpers: Vec<Bumper>,
    /// The enabled placement helpers.
    pub helpers: Vec<Helper>,
    /// The brush entities a trace can name, sorted by model.
    brushes: Vec<BrushInfo>,
    pub player: Option<PlayerBox>,
    /// `g_FuncBumpingEntityList` — soft bumpers (and other people's portals)
    /// that have already moved this portal once.
    bumped: Vec<EntityId>,
    /// `g_bBumpedByLinkedPortal`.
    bumped_by_linked_portal: bool,
}

impl<'a> ShotWorld<'a> {
    /// A snapshot of `entities` for one shot.
    ///
    /// `brush_models` is the server's `(model, entity)` table; the physics
    /// environment is optional because a server built for a unit test has
    /// none, and a world with no studio models needs none.
    pub fn new(
        query: &'a mut dyn TouchQuery,
        physics: Option<&'a Physics>,
        entities: &EntityList,
        brush_models: &[(usize, EntityId)],
        player: Option<EntityId>,
    ) -> ShotWorld<'a> {
        let mut portals = Vec::new();
        let mut bumpers = Vec::new();
        let mut helpers = Vec::new();
        for (id, entity) in entities.iter() {
            if let Some(helper) = entity
                .behaviour
                .downcast_ref::<super::classes::PlacementHelper>()
            {
                if helper.enabled {
                    helpers.push(Helper {
                        id,
                        origin: entity.core.origin,
                        angles: entity.core.angles,
                        radius: helper.radius,
                        use_angles: helper.use_angles,
                    });
                }
                continue;
            }
            if let Some(portal) = entity
                .behaviour
                .downcast_ref::<super::classes::PropPortal>()
            {
                portals.push(PortalInfo {
                    id,
                    origin: entity.core.origin,
                    angles: entity.core.angles,
                    half_width: portal.half_width,
                    half_height: portal.half_height,
                    active: portal.activated,
                    fired_by: portal.fired_by,
                });
                continue;
            }
            let kind = match entity.behaviour.downcast_ref::<super::classes::PortalVolume>() {
                Some(volume) if volume.active => volume.kind,
                _ => continue,
            };
            let Some(model) = entity.core.brush_model_index() else {
                continue;
            };
            let (mins, maxs) = entity.core.world_space_aabb();
            bumpers.push(Bumper {
                id,
                kind,
                model,
                origin: entity.core.origin,
                angles: entity.core.angles,
                mins,
                maxs,
            });
        }

        let brushes = brush_models
            .iter()
            .filter_map(|&(model, id)| {
                let entity = entities.get(id)?;
                Some(BrushInfo {
                    model,
                    id,
                    is_func_door: entity.core.class.name == "func_door",
                    moving: is_moving_or_rotating(entities, id),
                })
            })
            .collect();

        let player = player.and_then(|id| entities.get(id)).map(|entity| PlayerBox {
            origin: entity.core.origin,
            mins: entity.core.model_bounds.mins,
            maxs: entity.core.model_bounds.maxs,
        });

        ShotWorld {
            query,
            physics,
            portals,
            bumpers,
            helpers,
            brushes,
            player,
            bumped: Vec::new(),
            bumped_by_linked_portal: false,
        }
    }

    /// A world with nothing in it but what the query answers — for tests,
    /// which then fill in the public lists themselves.
    #[cfg(test)]
    pub fn bare(query: &'a mut dyn TouchQuery) -> ShotWorld<'a> {
        ShotWorld {
            query,
            physics: None,
            portals: Vec::new(),
            bumpers: Vec::new(),
            helpers: Vec::new(),
            brushes: Vec::new(),
            player: None,
            bumped: Vec::new(),
            bumped_by_linked_portal: false,
        }
    }

    /// `UTIL_TraceLine( start, end, mask, &traceFilterPortalShot, &tr )`.
    ///
    /// The engine's world and brush entities, then the static studio models,
    /// nearer wins — `CEngineTrace::TraceRay`'s own order. What the shot filter
    /// ignores by classname (the player, every physics prop) is never asked:
    /// see [`Physics::sweep_studio`].
    pub fn trace_line(&mut self, start: Vec3, end: Vec3, mask: u32) -> Trace {
        let hit = self.query.shot_trace(start, end, mask);
        let entity = hit.model.and_then(|model| self.brush(model)).map(|b| b.id);
        let mut trace = Trace {
            hit,
            entity,
            studio: false,
        };
        if mask & CONTENTS_SOLID == 0 || trace.hit.start_solid {
            return trace;
        }
        let Some(sweep) = self.physics.and_then(|physics| physics.sweep_studio(start, end)) else {
            return trace;
        };
        if sweep.start_solid || sweep.fraction < trace.hit.fraction {
            trace = Trace {
                hit: ShotHit {
                    start,
                    end: start + (end - start) * sweep.fraction,
                    fraction: sweep.fraction,
                    fraction_left_solid: 0.0,
                    normal: sweep.normal,
                    plane_dist: sweep.normal.dot(start + (end - start) * sweep.fraction),
                    start_solid: sweep.start_solid,
                    all_solid: sweep.start_solid && sweep.fraction == 0.0,
                    surface: None,
                    surface_flags: 0,
                    game_material: 0,
                    model: None,
                },
                entity: self.physics.and_then(|physics| physics.owner(sweep.body)),
                studio: true,
            };
        }
        trace
    }

    fn brush(&self, model: usize) -> Option<&BrushInfo> {
        self.brushes.iter().find(|b| b.model == model)
    }

    /// Whether the entity a trace stopped on is a `func_door`.
    fn is_func_door(&self, entity: Option<EntityId>) -> bool {
        entity.is_some_and(|id| self.brushes.iter().any(|b| b.id == id && b.is_func_door))
    }

    /// `UTIL_IsEntityMovingOrRotating( tr.m_pEnt )` for whatever a trace hit.
    fn is_moving(&self, entity: Option<EntityId>) -> bool {
        entity.is_some_and(|id| self.brushes.iter().any(|b| b.id == id && b.moving))
    }

    /// `IsPassThroughMaterial` (`portal_placement.cpp:151`) — sky, or a
    /// material on the list.
    fn is_pass_through(&self, trace: &Trace) -> bool {
        if trace.hit.surface_flags & SURF_SKY != 0 {
            return true;
        }
        let Some(surface) = trace.hit.surface else {
            return false;
        };
        let name = self.query.surface_name(surface).to_ascii_lowercase();
        PASS_THROUGH_MATERIALS.iter().any(|m| name.contains(m))
    }
}

/// `UTIL_IsEntityMovingOrRotating` (`portal_util_shared.cpp:2588`), for a
/// brush entity: a velocity on it, or an angular velocity on it or anything
/// it is parented to.
///
/// `GetAbsVelocity()` includes the parent's motion, which this port keeps as
/// each entity's own velocity along the chain — so the linear half walks the
/// chain too, where Valve's reads one composed value.
fn is_moving_or_rotating(entities: &EntityList, id: EntityId) -> bool {
    let mut check = Some(id);
    while let Some(at) = check {
        let Some(entity) = entities.get(at) else {
            break;
        };
        if entity.core.velocity != Vec3::ZERO || entity.core.angular_velocity != Vec3::ZERO {
            return true;
        }
        check = entity.core.parent();
    }
    false
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// `IsNoPortalMaterial` — `PortalSurfaceType( tr ) == PORTAL_SURFACE_INVALID`
/// (`portal_placement.cpp:130`).
///
/// In order: portal paint (none here), the `SURF_NOPORTAL` flag, a glass game
/// material, and any studio model at all — *"Skipping all studio models"*.
fn is_no_portal_material(trace: &Trace) -> bool {
    trace.hit.surface_flags & SURF_NOPORTAL != 0
        || trace.hit.game_material == CHAR_TEX_GLASS
        || trace.studio
}

// ---------------------------------------------------------------------------
// The traces the fit is built from
// ---------------------------------------------------------------------------

/// `TracePortals` (`portal_placement.cpp:163`) — the line against every other
/// active portal **on the same face**, as boxes.
fn trace_portals(world: &ShotWorld<'_>, ignore: EntityId, forward: Vec3, start: Vec3, end: Vec3) -> Trace {
    let mut trace = Trace::clear();
    for portal in &world.portals {
        if portal.id == ignore || !portal.active {
            continue;
        }
        let (other_forward, _, _) = angle_vectors(portal.angles);
        // "If they're not on the same face then don't worry about overlap"
        if forward.dot(other_forward) < 0.95 {
            continue;
        }
        let (mins, maxs) = portal_aabb(portal);
        if let Some(hit) = intersect_ray_with_box(start, end, mins, maxs) {
            if hit.fraction < 1.0 && hit.fraction < trace.fraction() {
                trace = Trace {
                    hit,
                    entity: Some(portal.id),
                    studio: false,
                };
            }
        }
    }
    trace
}

/// `TraceBumpingEntities` (`portal_placement.cpp:216`) — the line against the
/// active bumpers, fizzlers and no-portal volumes. Returns the trace and
/// whether the nearest was a soft bumper.
///
/// The three differ in how they bump: a bumper is soft and bumps once, a
/// fizzler is a wall, and a no-portal volume is a wall two units nearer than
/// it really is, *"so that the portal isn't touching the no portal volume"*.
fn trace_bumping_entities(world: &mut ShotWorld<'_>, start: Vec3, end: Vec3) -> (Trace, bool) {
    let mut trace = Trace::clear();
    let mut closest_is_soft = false;
    let length = (end - start).length();
    let bumpers = world.bumpers.clone();
    for bumper in &bumpers {
        // `AllEdictsAlongRay` — the partition's broadphase.
        if intersect_ray_with_box(start, end, bumper.mins, bumper.maxs).is_none() {
            continue;
        }
        let clip = |world: &mut ShotWorld<'_>| {
            let hit = world.query.clip_to_model(
                bumper.model,
                bumper.origin,
                bumper.angles,
                start,
                end,
                MASK_ALL,
            );
            Trace {
                hit,
                entity: Some(bumper.id),
                studio: false,
            }
        };
        let (mut temp, soft) = match bumper.kind {
            BumperKind::Bumper | BumperKind::Cleanser => {
                let mut temp = clip(world);
                if temp.hit.start_solid {
                    temp.hit.fraction = 1.0;
                }
                (temp, bumper.kind == BumperKind::Bumper)
            }
            BumperKind::NoPortalVolume => {
                let mut temp = clip(world);
                let delta = temp.hit.end - temp.hit.start;
                let distance = (delta.length() - 2.0).max(0.0);
                let direction = delta.normalize_or_zero();
                temp.hit.fraction = match length > 0.0 {
                    true => distance / length,
                    false => 0.0,
                };
                temp.hit.end = temp.hit.start + direction * distance;
                (temp, false)
            }
        };
        if temp.hit.fraction >= 1.0 {
            temp.entity = None;
        }
        // "If this is the closest and has only bumped once (for soft bumpers)"
        if temp.fraction() < trace.fraction() && (!soft || !world.bumped.contains(&bumper.id)) {
            trace = temp;
            closest_is_soft = soft;
        }
    }
    (trace, closest_is_soft)
}

/// `TracePortalCorner` (`portal_placement.cpp:307`) — does the line from the
/// portal's centre to one corner cross anything it has to be fitted away
/// from? `Some((trace, soft))` if so.
///
/// Three things are asked and the nearest wins: the **surface's own edge**
/// (a line one unit inside the wall, which leaves solid where the wall
/// stops), an **enclosing wall** (a line one unit in front of it), and the
/// other portals and bumping entities. When none of those crosses, the corner
/// itself is checked for sitting on a surface a portal may not go on — and a
/// binary search finds where the good surface stops, so that a portal half
/// on a `SURF_NOPORTAL` panel is slid off it rather than refused.
#[allow(clippy::too_many_arguments)]
fn trace_portal_corner(
    world: &mut ShotWorld<'_>,
    ignore: Placing,
    origin: Vec3,
    corner: Vec3,
    forward: Vec3,
    placed_by: PlacedBy,
) -> Option<(Trace, bool)> {
    let origin_to_corner = corner - origin;

    // Check for surface edge.
    let mut edge = world.trace_line(
        origin - forward,
        corner - forward,
        MASK_SHOT_PORTAL | CONTENTS_WATER | CONTENTS_SLIME,
    );
    if edge.hit.start_solid {
        let mut total = edge.hit.fraction_left_solid;
        while edge.hit.start_solid && edge.hit.fraction_left_solid > 0.0 && total < 1.0 {
            edge = world.trace_line(
                origin + origin_to_corner * (total + 0.05) - forward,
                corner + origin_to_corner * (total + 0.05) - forward,
                MASK_SHOT_PORTAL,
            );
            if edge.hit.start_solid {
                total += edge.hit.fraction_left_solid + 0.05;
            }
        }
        if total < 1.0 {
            edge = world.trace_line(
                origin + origin_to_corner * (total + 0.05) - forward,
                origin - forward,
                MASK_SHOT_PORTAL,
            );
            if edge.hit.start_solid {
                edge.hit.fraction = 1.0;
            } else {
                edge.hit.fraction = total;
                edge.hit.normal = -edge.hit.normal;
            }
        } else {
            edge.hit.fraction = 1.0;
        }
    } else {
        edge.hit.fraction = 1.0;
    }

    // Check for enclosing wall.
    let mut wall = world.trace_line(
        origin + forward,
        corner + forward,
        MASK_SOLID_BRUSHONLY | CONTENTS_MONSTER | CONTENTS_WATER | CONTENTS_SLIME,
    );
    if edge.fraction() < wall.fraction() {
        wall.hit.fraction = edge.hit.fraction;
        wall.hit.normal = edge.hit.normal;
    }

    let portal = match placed_by {
        PlacedBy::Fixed => Trace::clear(),
        _ => trace_portals(world, ignore.id, forward, origin + forward, corner + forward),
    };
    let (bumping, soft_bumper) = trace_bumping_entities(world, origin + forward, corner + forward);

    if wall.fraction() >= 1.0 && portal.fraction() >= 1.0 && bumping.fraction() >= 1.0 {
        // "check for a surface change between center and corner so we can
        // bump when we partially overlap a non-portal surface"
        let corner_trace = world.trace_line(corner, corner - forward, MASK_SHOT_PORTAL);
        if corner_trace.hit.did_hit() && is_no_portal_material(&corner_trace) {
            return Some((
                binary_search_surface_change(world, origin, origin_to_corner, forward, corner_trace),
                false,
            ));
        }
        return None;
    }

    if wall.fraction() <= portal.fraction() && wall.fraction() <= bumping.fraction() {
        return Some((wall, false));
    }
    if portal.fraction() <= wall.fraction()
        && portal.fraction() <= bumping.fraction()
        && !portal.entity.is_some_and(|id| world.bumped.contains(&id))
    {
        let other = portal
            .entity
            .and_then(|id| world.portals.iter().find(|p| p.id == id))
            .map(|p| p.fired_by);
        let own = other.is_some_and(|fired_by| fired_by == ignore.fired_by);
        if own {
            world.bumped_by_linked_portal = true;
        }
        return Some((portal, !own));
    }
    if !bumping.hit.start_solid
        && bumping.fraction() <= wall.fraction()
        && bumping.fraction() <= portal.fraction()
    {
        return Some((bumping, soft_bumper));
    }
    None
}

/// The half of [`trace_portal_corner`] that finds where a good surface turns
/// into a no-portal one between the centre and a corner — to a hundredth of
/// a unit, and then the direction of the boundary to a tenth of a degree.
///
/// **The trace it returns is whichever trace ran last**, with its positions
/// and plane overwritten — Valve's `tr = cornerTrace`, where `cornerTrace` is
/// the one variable every probe writes into. Its surface fields are
/// therefore the last probe's, which is why every probe below assigns it.
#[allow(unused_assignments)]
fn binary_search_surface_change(
    world: &mut ShotWorld<'_>,
    origin: Vec3,
    origin_to_corner: Vec3,
    forward: Vec3,
    mut corner_trace: Trace,
) -> Trace {
    const MAX_DELTA: f32 = 0.01;
    let good_surface = |world: &mut ShotWorld<'_>, spot: Vec3| {
        let t = world.trace_line(spot, spot - forward, MASK_SHOT_PORTAL);
        let ok = t.hit.did_hit() && !is_no_portal_material(&t);
        (ok, t)
    };

    let full_length = origin_to_corner.length();
    let mut bad_length = full_length;
    let mut good_length = 0.0f32;
    let direction = origin_to_corner.normalize_or_zero();
    // "overwatch is soon. And I think this loop might have locked up once or
    // twice without an absolute loop limit."
    let mut searches = 0;
    while bad_length - good_length >= MAX_DELTA && searches < 100 {
        let test_length = (bad_length + good_length) * 0.5;
        let (ok, t) = good_surface(world, origin + direction * test_length);
        corner_trace = t;
        match ok {
            true => good_length = test_length,
            false => bad_length = test_length,
        }
        searches += 1;
    }

    let good_spot = origin + direction * good_length;
    let mut impact_normal = Vec3::ZERO;
    // "try spots at 4x the delta in a circular pattern to find the normal of
    // impact"
    let mut good_direction = forward.cross(direction);
    let (ok, t) = good_surface(world, good_spot + good_direction * (MAX_DELTA * 4.0));
    corner_trace = t;
    if !ok {
        good_direction = -good_direction;
    }
    let (ok, t) = good_surface(world, good_spot + good_direction * (MAX_DELTA * 4.0));
    corner_trace = t;
    if ok {
        // Valve's angles here are in *degrees* and go straight into `cosf` and
        // `sinf`, which take radians — `90.0f` is a little over 14 turns. The
        // search still converges on *a* direction, just not the one the
        // comment describes; kept, because a corrected search would bump
        // portals somewhere the shipped game does not.
        let mut bad_angle = 0.0f32;
        let mut good_angle = 90.0f32;
        for _ in 0..10 {
            let test_angle = (bad_angle + good_angle) * 0.5;
            let test_direction = direction * test_angle.cos() + good_direction * test_angle.sin();
            let (ok, t) = good_surface(world, good_spot + test_direction * (MAX_DELTA * 4.0));
            corner_trace = t;
            match ok {
                true => good_angle = test_angle,
                false => bad_angle = test_angle,
            }
        }
        good_direction = direction * good_angle.cos() + good_direction * good_angle.sin();
        impact_normal = forward.cross(good_direction);
        if impact_normal.dot(direction) > 0.0 {
            impact_normal = -impact_normal;
        }
    }

    let end = origin + direction * good_length;
    corner_trace.hit.start = origin;
    corner_trace.hit.end = end;
    corner_trace.hit.fraction = match full_length > 0.0 {
        true => good_length / full_length,
        false => 0.0,
    };
    corner_trace.hit.fraction_left_solid = 1.0;
    corner_trace.hit.normal = impact_normal;
    corner_trace.hit.plane_dist = impact_normal.dot(end);
    corner_trace
}

// ---------------------------------------------------------------------------
// The fit
// ---------------------------------------------------------------------------

/// `CPortalCornerFitData` — what one corner's trace found.
#[derive(Debug, Clone, Copy)]
struct CornerFit {
    trace: Trace,
    intersection_point: Vec3,
    intersection_direction: Vec3,
    bump_direction: Vec3,
    corner_intersection: bool,
    soft_bump: bool,
}

impl Default for CornerFit {
    /// `memset( sFitData, 0, sizeof( sFitData ) )`.
    fn default() -> CornerFit {
        let mut trace = Trace::clear();
        trace.hit.fraction = 0.0;
        CornerFit {
            trace,
            intersection_point: Vec3::ZERO,
            intersection_direction: Vec3::ZERO,
            bump_direction: Vec3::ZERO,
            corner_intersection: false,
            soft_bump: false,
        }
    }
}

/// The frame a portal is fitted in: its axes and its four half-edges, pulled
/// in by [`PORTAL_BUMP_FORGIVENESS`].
#[derive(Debug, Clone, Copy)]
struct Frame {
    forward: Vec3,
    right: Vec3,
    top: Vec3,
    bottom: Vec3,
    right_edge: Vec3,
    left_edge: Vec3,
    half_width: f32,
}

/// `CalcDistanceToLine` (`mathlib_base.cpp:3801`).
fn distance_to_line(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let direction = b - a;
    let div = direction.dot(direction);
    let t = match div < 0.00001 {
        true => 0.0,
        false => (direction.dot(p) - direction.dot(a)) / div,
    };
    p.distance(a + direction * t)
}

/// `CalcLineToLineIntersectionSegment` (`mathlib_base.cpp:3931`) — the
/// closest points of two infinite lines, `None` when either is degenerate or
/// they are parallel.
fn line_to_line(p1: Vec3, p2: Vec3, p3: Vec3, p4: Vec3) -> Option<(Vec3, Vec3)> {
    let p13 = p1 - p3;
    let p43 = p4 - p3;
    if p43.abs().max_element() < LINE_EPS {
        return None;
    }
    let p21 = p2 - p1;
    if p21.abs().max_element() < LINE_EPS {
        return None;
    }
    let d1343 = p13.dot(p43);
    let d4321 = p43.dot(p21);
    let d1321 = p13.dot(p21);
    let d4343 = p43.dot(p43);
    let d2121 = p21.dot(p21);
    let denom = d2121 * d4343 - d4321 * d4321;
    if denom.abs() < LINE_EPS {
        return None;
    }
    let numer = d1343 * d4321 - d1321 * d4343;
    let t1 = numer / denom;
    let t2 = (d1343 + d4321 * t1) / d4343;
    Some((p1 + p21 * t1, p3 + p43 * t2))
}

/// `FindBumpVectorInCorner` (`portal_placement.cpp:522`) — how far to move a
/// portal whose two corners have crossed two lines that meet at an angle.
///
/// The two intersections and the point the lines meet make a triangle; the
/// portal's edge between the two corners has to fit across a similar, larger
/// one, and the bump is how far the first corner moves to get there.
///
/// > **The one divergence**: when the two lines do not meet,
/// > `CalcLineToLineIntersectionSegment` returns false and Valve goes on to
/// > read the two points it never wrote. That is no bump here — the answer the
/// > function's own `FIXME` gives for the other degenerate case.
#[allow(clippy::too_many_arguments)]
fn find_bump_vector_in_corner(
    corner1: Vec3,
    corner2: Vec3,
    intersection1: Vec3,
    intersection2: Vec3,
    direction1: Vec3,
    direction2: Vec3,
    bump1: Vec3,
    bump2: Vec3,
) -> Vec3 {
    let Some((closest1, closest2)) = line_to_line(
        intersection1,
        intersection1 + direction1,
        intersection2,
        intersection2 + direction2,
    ) else {
        return Vec3::ZERO;
    };
    let line_intersection = (closest1 + closest2) * 0.5;

    let mut short_leg = intersection1 - line_intersection;
    let mut short_leg2 = intersection2 - line_intersection;
    let short_leg_length = short_leg.length();
    let short_leg2_length = short_leg2.length();
    if short_leg_length == 0.0 || short_leg2_length == 0.0 {
        // "FIXME: Our triangle is actually a point or a line, so there's
        // nothing we can do"
        return Vec3::ZERO;
    }
    short_leg /= short_leg_length;
    short_leg2 /= short_leg2_length;

    let corner_to_corner = (corner2 - corner1).normalize_or_zero();
    let edge_dot_leg = corner_to_corner.dot(short_leg);
    let edge_dot_leg2 = corner_to_corner.dot(short_leg2);
    if !(-0.9999..=0.9999).contains(&edge_dot_leg) || !(-0.9999..=0.9999).contains(&edge_dot_leg2) {
        // A one-corner bump with each corner.
        let distance1 = distance_to_line(corner1, intersection1, intersection1 + direction1)
            + PORTAL_BUMP_FORGIVENESS;
        let distance2 = distance_to_line(corner2, intersection2, intersection2 + direction2)
            + PORTAL_BUMP_FORGIVENESS;
        return bump1 * distance1 + bump2 * distance2;
    }

    let legs_dot = short_leg.dot(short_leg2);
    let long_base_length = corner1.distance(corner2);
    let short_leg2_angle = corner_to_corner.dot(-short_leg).acos();
    let short_base_angle = legs_dot.acos();
    let short_leg_angle = std::f32::consts::PI - short_base_angle - short_leg2_angle;
    if short_leg_angle.sin() == 0.0 {
        return Vec3::splat(1000.0);
    }
    let short_base_length = short_base_angle.sin() * (short_leg_length / short_leg_angle.sin());
    if short_base_length == 0.0 {
        return Vec3::ZERO;
    }
    let long_leg_length = long_base_length * (short_leg_length / short_base_length);
    let new_corner = line_intersection + short_leg * long_leg_length;
    new_corner - corner1
}

/// The portal being placed: never in its own way, and whose shooter decides
/// whether another portal it bumps into is its own.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placing {
    id: EntityId,
    /// `pIgnorePortal->GetFiredByPlayer()`.
    fired_by: Option<EntityId>,
}

/// The four corners, in Valve's order: top-left, top-right, bottom-left,
/// bottom-right.
fn corners(origin: Vec3, frame: &Frame) -> [Vec3; 4] {
    [
        origin + frame.top + frame.left_edge,
        origin + frame.top + frame.right_edge,
        origin + frame.bottom + frame.left_edge,
        origin + frame.bottom + frame.right_edge,
    ]
}

/// Whether a side trace towards a perpendicular wall hits anything — the
/// "check if perpendicular wall is near" probe the parallel cases make.
fn probe(
    world: &mut ShotWorld<'_>,
    ignore: Placing,
    origin: Vec3,
    toward: Vec3,
    frame: &Frame,
    placed_by: PlacedBy,
) -> (bool, bool) {
    match trace_portal_corner(world, ignore, origin, origin + toward, frame.forward, placed_by) {
        Some((_, soft)) => (true, soft),
        None => (false, false),
    }
}

/// `FitPortalOnSurface` (`portal_placement.cpp:604`). Moves `origin` until
/// no corner crosses anything; `false` if it cannot.
///
/// `fits`, `indices` and `count` are the recursion's carried state —
/// `pPortalCornerFitData`, `p_piIntersectionIndex` and `piIntersectionCount`
/// — and each call works on its own copy of them, as the C++'s by-value
/// locals do.
#[allow(clippy::too_many_arguments)]
fn fit_on_surface(
    world: &mut ShotWorld<'_>,
    ignore: Placing,
    origin: &mut Vec3,
    frame: &Frame,
    placed_by: PlacedBy,
    recursions: u32,
    mut fits: [CornerFit; 4],
    mut indices: [usize; 4],
    mut count: usize,
) -> bool {
    if recursions >= MAX_FIT_RECURSIONS {
        return false;
    }
    let corner = corners(*origin, frame);
    let old_count = count;

    let mut no_normal_min = Vec3::ZERO;
    let mut no_normal_max = Vec3::ZERO;
    let mut no_normal_additive = Vec3::ZERO;
    let mut new_intersection = [false; 4];

    // Find intersections from center to each corner.
    for i in 0..4 {
        // "HACK: In weird cases intersection count can go over 3 and index
        // outside of our arrays. Don't let this happen!"
        if count >= 4 {
            continue;
        }
        if !fits[i].corner_intersection {
            let found =
                trace_portal_corner(world, ignore, *origin, corner[i], frame.forward, placed_by);
            fits[i].corner_intersection = found.is_some();
            if let Some((trace, soft)) = found {
                fits[i].trace = trace;
                fits[i].soft_bump = soft;
                fits[i].intersection_point = *origin + (corner[i] - *origin) * trace.fraction();
                if trace.hit.normal == Vec3::ZERO {
                    let push = fits[i].intersection_point - corner[i];
                    no_normal_min = no_normal_min.min(push);
                    no_normal_max = no_normal_max.max(push);
                    no_normal_additive += push;
                }
                new_intersection[i] = true;
                indices[count] = i;
                count += 1;
            } else {
                // `TracePortalCorner` clears the trace and the soft flag on a
                // miss, through its out-parameters.
                fits[i].trace = Trace::clear();
                fits[i].soft_bump = false;
            }
        } else {
            // "We shouldn't be intersecting with any old corners"
            fits[i].trace.hit.fraction = 1.0;
        }
    }

    // "clip the additive vector of pushes from intersections with no normal.
    // We're going to give them all a shared normal that agrees"
    no_normal_additive = no_normal_additive.min(no_normal_max).max(no_normal_min);
    no_normal_additive = no_normal_additive.normalize_or_zero();

    for i in 0..4 {
        if !new_intersection[i] {
            continue;
        }
        if fits[i].trace.hit.normal == Vec3::ZERO {
            fits[i].trace.hit.normal = no_normal_additive;
            fits[i].trace.hit.plane_dist = no_normal_additive.dot(fits[i].intersection_point);
        }
        fits[i].trace.hit.normal = fits[i].trace.hit.normal.normalize_or_zero();
        fits[i].intersection_direction =
            fits[i].trace.hit.normal.cross(frame.forward).normalize_or_zero();
        fits[i].bump_direction = frame
            .forward
            .cross(fits[i].intersection_direction)
            .normalize_or_zero();
    }

    // "Remember soft bumpers so we don't bump with it twice"
    for fit in &fits {
        if fit.soft_bump {
            if let Some(id) = fit.trace.entity {
                world.bumped.push(id);
            }
        }
    }

    // "If no new intersections were found then it already fits"
    if old_count == count {
        return true;
    }

    let recurse = |world: &mut ShotWorld<'_>,
                   origin: &mut Vec3,
                   fits: [CornerFit; 4],
                   indices: [usize; 4],
                   count: usize| {
        fit_on_surface(
            world,
            ignore,
            origin,
            frame,
            placed_by,
            recursions + 1,
            fits,
            indices,
            count,
        )
    };
    let bump_distance = |corner: Vec3, fit: &CornerFit| {
        distance_to_line(
            corner,
            fit.intersection_point,
            fit.intersection_point + fit.intersection_direction,
        ) + PORTAL_BUMP_FORGIVENESS
    };

    match count {
        0 => true,
        1 => {
            let fit = fits[indices[0]];
            let distance = bump_distance(corner[indices[0]], &fit);
            *origin += fit.bump_direction * distance;
            recurse(world, origin, fits, indices, count)
        }
        2 => {
            let (a, b) = (indices[0], indices[1]);
            if fits[a].intersection_point == fits[b].intersection_point {
                return false;
            }
            let dot = fits[a].bump_direction.dot(fits[b].bump_direction);

            // "If there are parallel intersections try scooting it away from a
            // near wall"
            if dot < -0.9 {
                let along = fits[a].intersection_direction * frame.half_width * 2.0;
                let (mut dir1, soft1) = probe(world, ignore, *origin, along, frame, placed_by);
                let (dir2, soft2) = probe(world, ignore, *origin, -along, frame, placed_by);
                // "No fit if there's blocking walls on both sides it can't fit"
                if dir1 && dir2 {
                    if soft1 {
                        dir1 = false;
                    } else if soft2 {
                        dir1 = true;
                    } else {
                        return false;
                    }
                }
                // "If there's no assumption to make, just pick a direction."
                if !dir1 && !dir2 {
                    dir1 = true;
                }
                let direction = fits[a].intersection_direction;
                *origin += match dir1 {
                    true => direction * -frame.half_width,
                    false => direction * frame.half_width,
                };
                fits[a].corner_intersection = false;
                fits[b].corner_intersection = false;
                return recurse(world, origin, fits, indices, 0);
            }

            // "If they are the same there's an easy way"
            if dot > 0.9 {
                let closest = match origin.distance(fits[a].intersection_point)
                    < origin.distance(fits[b].intersection_point)
                {
                    true => 0,
                    false => 1,
                };
                let closest_fit = fits[indices[closest]];
                let distances = [
                    bump_distance(corner[a], &closest_fit),
                    bump_distance(corner[b], &closest_fit),
                ];
                let largest = match distances[0] > distances[1] {
                    true => 0,
                    false => 1,
                };
                *origin += closest_fit.bump_direction * distances[largest];

                // "If they were parallel to the intersection line don't
                // invalidate both before recursion"
                if distances[0] == distances[1] {
                    fits[a].corner_intersection = false;
                    fits[b].corner_intersection = false;
                    return recurse(world, origin, fits, indices, 0);
                }
                if largest != closest {
                    fits[indices[largest]] = fits[indices[closest]];
                }
                fits[indices[1 - largest]].corner_intersection = false;
                indices[0] = indices[largest];
                return recurse(world, origin, fits, indices, 1);
            }

            // "Intersections are angled, bump based on math using the corner"
            *origin += find_bump_vector_in_corner(
                corner[a],
                corner[b],
                fits[a].intersection_point,
                fits[b].intersection_point,
                fits[a].intersection_direction,
                fits[b].intersection_direction,
                fits[a].bump_direction,
                fits[b].bump_direction,
            );
            recurse(world, origin, fits, indices, count)
        }
        3 => fit_three(world, ignore, origin, frame, placed_by, recursions, fits, indices, count),
        _ => {
            let soft = indices.iter().any(|&i| fits[i].soft_bump);
            if !soft {
                // "All corners intersect with no soft bumps, so it can't be fit"
                return false;
            }
            for &i in &indices {
                fits[i].corner_intersection = false;
            }
            recurse(world, origin, fits, indices, 0)
        }
    }
}

/// [`fit_on_surface`]'s three-corner case, which is long enough to read on
/// its own.
#[allow(clippy::too_many_arguments)]
fn fit_three(
    world: &mut ShotWorld<'_>,
    ignore: Placing,
    origin: &mut Vec3,
    frame: &Frame,
    placed_by: PlacedBy,
    recursions: u32,
    mut fits: [CornerFit; 4],
    mut indices: [usize; 4],
    mut count: usize,
) -> bool {
    let corner = corners(*origin, frame);
    let recurse = |world: &mut ShotWorld<'_>,
                   origin: &mut Vec3,
                   fits: [CornerFit; 4],
                   indices: [usize; 4],
                   count: usize| {
        fit_on_surface(
            world,
            ignore,
            origin,
            frame,
            placed_by,
            recursions + 1,
            fits,
            indices,
            count,
        )
    };
    let bump_distance = |corner: Vec3, fit: &CornerFit| {
        distance_to_line(
            corner,
            fit.intersection_point,
            fit.intersection_point + fit.intersection_direction,
        ) + PORTAL_BUMP_FORGIVENESS
    };

    // "Get the relationships of the intersections"
    let dot = [
        fits[indices[0]].bump_direction.dot(fits[indices[1]].bump_direction),
        fits[indices[1]].bump_direction.dot(fits[indices[2]].bump_direction),
        fits[indices[2]].bump_direction.dot(fits[indices[0]].bump_direction),
    ];

    let mut similar = 0;
    for d in 0..3 {
        if dot[d] < -0.99 {
            // Parallel intersections: scoot away from a near wall, as the
            // two-corner case does.
            let along = fits[indices[d]].intersection_direction * frame.half_width * 2.0;
            let (mut dir1, soft1) = probe(world, ignore, *origin, along, frame, placed_by);
            let (dir2, soft2) = probe(world, ignore, *origin, -along, frame, placed_by);
            if dir1 && dir2 {
                if soft1 {
                    dir1 = false;
                } else if soft2 {
                    dir1 = true;
                } else {
                    return false;
                }
            }
            if !dir1 && !dir2 {
                dir1 = true;
            }
            let direction = fits[indices[d]].intersection_direction;
            *origin += match dir1 {
                true => direction * -frame.half_width,
                false => direction * frame.half_width,
            };
            for &i in &indices[..3] {
                fits[i].corner_intersection = false;
            }
            return recurse(world, origin, fits, indices, 0);
        } else if dot[d] > 0.99 {
            similar += 1;
        }
    }

    // "If no intersections are similar"
    if similar == 0 {
        let total: f32 = dot.iter().map(|d| d.acos()).sum();
        // "If it's in a triangle, it can't be fit"
        let pi = std::f32::consts::PI;
        if pi - 0.01 < total && total < pi + 0.01 {
            let soft = indices[..3].iter().any(|&i| fits[i].soft_bump);
            if !soft {
                return false;
            }
            for &i in &indices[..3] {
                fits[i].corner_intersection = false;
            }
            return recurse(world, origin, fits, indices, 0);
        }
    }

    // "If the intersections are all similar there's an easy way"
    if similar == 3 {
        let mut closest = 0;
        let mut closest_distance = origin.distance(fits[indices[0]].intersection_point);
        for (at, &i) in indices[..3].iter().enumerate().skip(1) {
            let distance = origin.distance(fits[i].intersection_point);
            if closest_distance > distance {
                closest = at;
                closest_distance = distance;
            }
        }
        let closest_fit = fits[indices[closest]];
        let distances = [
            bump_distance(corner[indices[0]], &closest_fit),
            bump_distance(corner[indices[1]], &closest_fit),
            bump_distance(corner[indices[2]], &closest_fit),
        ];
        let mut largest = match distances[0] > distances[1] {
            true => 0,
            false => 1,
        };
        largest = match distances[largest] > distances[2] {
            true => largest,
            false => 2,
        };
        *origin += closest_fit.bump_direction * distances[largest];

        // "Invalidate corners that were closer to the intersection line"
        let mut still = 0;
        for at in 0..3 {
            if distances[at] != distances[largest] {
                fits[indices[at]].corner_intersection = false;
                count -= 1;
            } else {
                fits[indices[at]] = fits[indices[closest]];
                indices[still] = indices[at];
                still += 1;
            }
        }
        return recurse(world, origin, fits, indices, count);
    }

    // "Get info for which corners are diagonal from each other"
    let mut longest = 0.0f32;
    let mut longest_at = 0;
    for at in 0..3 {
        let distance = corner[indices[at]].distance(corner[indices[(at + 1) % 3]]);
        if longest < distance {
            longest = distance;
            longest_at = at;
        }
    }
    let (i1, i2, i3) = match longest_at {
        0 => (0, 1, 2),
        1 => (1, 2, 0),
        _ => (2, 0, 1),
    };
    let (a, b, c) = (indices[i1], indices[i2], indices[i3]);

    // "If corner is 90 degrees there my be an easy way"
    let corner_dot = fits[a].intersection_direction.dot(fits[b].intersection_direction);
    if corner_dot.abs() < 0.0001 {
        // "Check if portal is aligned perfectly with intersection normals"
        let portal_dot = fits[a].intersection_direction.dot(frame.right);
        if portal_dot.abs() < 0.0001 || portal_dot > 0.9999 || portal_dot < -0.9999 {
            let bump1 = bump_distance(corner[a], &fits[a]);
            let bump2 = bump_distance(corner[b], &fits[b]);
            *origin += fits[a].bump_direction * bump1;
            *origin += fits[b].bump_direction * bump2;
            fits[a].corner_intersection = false;
            fits[b].corner_intersection = false;
            fits[c].corner_intersection = false;
            return recurse(world, origin, fits, indices, 0);
        }
    }

    *origin += find_bump_vector_in_corner(
        corner[a],
        corner[b],
        fits[a].intersection_point,
        fits[b].intersection_point,
        fits[a].intersection_direction,
        fits[b].intersection_direction,
        fits[a].bump_direction,
        fits[b].bump_direction,
    );
    fits[c].corner_intersection = false;
    recurse(world, origin, fits, indices, 0)
}

/// `FitPortalAroundOtherPortals` (`portal_placement.cpp:1156`) — slide a
/// player's portal sideways off another one on the same face, before the fit
/// runs, so that two portals can sit side by side.
fn fit_around_other_portals(
    world: &ShotWorld<'_>,
    ignore: EntityId,
    origin: &mut Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    half_width: f32,
    half_height: f32,
) {
    for portal in &world.portals {
        if portal.id == ignore || !portal.active {
            continue;
        }
        let (other_forward, _, _) = angle_vectors(portal.angles);
        if forward.dot(other_forward) < 0.95
            || (origin.dot(forward) - portal.origin.dot(other_forward)).abs() > 1.0
        {
            continue;
        }
        let diff = *origin - portal.origin;
        let mut along_right = right * diff.dot(right);
        let along_up = up * diff.dot(up);
        let right_length = along_right.length();
        let up_length = along_up.length();
        along_right = along_right.normalize_or_zero();
        if right_length < 1.0 {
            along_right = right;
        }
        if up_length < half_height * 2.0 && right_length < half_width * 2.0 {
            *origin += along_right * (half_width * 2.0 - right_length + 1.0);
        }
    }
}

// ---------------------------------------------------------------------------
// The whole-portal tests
// ---------------------------------------------------------------------------

/// `IsPortalIntersectingNoPortalVolume` (`portal_placement.cpp:1203`).
///
/// The box test is `UTIL_IsBoxIntersectingPortal`, the volume's box against
/// the portal's two triangles — which this module asks as the box against the
/// rectangle they tile, a zero-thickness OBB. The volume is shrunk by the
/// bump forgiveness on the two axes the portal lies across, so that a portal
/// fitted flush against one is not refused by it.
pub fn is_intersecting_no_portal_volume(
    world: &ShotWorld<'_>,
    origin: Vec3,
    angles: Vec3,
    forward: Vec3,
    half_width: f32,
    half_height: f32,
) -> bool {
    world
        .bumpers
        .iter()
        .filter(|b| b.kind == BumperKind::NoPortalVolume)
        .any(|volume| {
            let center = (volume.mins + volume.maxs) * 0.5;
            let mut extents = (volume.maxs - volume.mins) * 0.5;
            let shrink = |f: f32| match f > 0.5 || f < -0.5 {
                true => 0.0,
                false => -PORTAL_BUMP_FORGIVENESS,
            };
            extents += Vec3::new(shrink(forward.x), shrink(forward.y), shrink(forward.z));
            super::obb::swept_box_touches_obb(
                center,
                center,
                -extents,
                extents,
                origin,
                angles,
                Vec3::new(0.0, -half_width, -half_height),
                Vec3::new(0.0, half_width, half_height),
            )
        })
}

/// `IsPortalOverlappingOtherPortals` (`portal_placement.cpp:1242`), for single
/// player: any active portal on the same face whose box overlaps this one's is
/// a [`PlacementResult::OverlapLinked`] — `bLinkedPortal` is
/// `!GameRules()->IsMultiplayer() || …`, so the co-op branches cannot be
/// reached.
///
/// `fizzle_all` is `bFizzleAll`: a *map's* portal that overlaps a gun's
/// fizzles it instead of being refused — the returned ids are the ones to
/// fizzle.
pub fn overlapping_other_portals(
    world: &ShotWorld<'_>,
    ignore: EntityId,
    origin: Vec3,
    angles: Vec3,
    half_width: f32,
    half_height: f32,
    fizzle_all: bool,
) -> (PlacementResult, Vec<EntityId>) {
    let (forward, _, _) = angle_vectors(angles);
    let mins = Vec3::new(0.0, -half_width, -half_height);
    let maxs = Vec3::new(1.0, half_width, half_height);
    let mut fizzled = Vec::new();
    for portal in &world.portals {
        if portal.id == ignore || !portal.active {
            continue;
        }
        let (other_forward, _, _) = angle_vectors(portal.angles);
        if forward.dot(other_forward) < 0.95 {
            continue;
        }
        // `GetLocalMins`/`GetLocalMaxs` — the other portal's trigger box,
        // which reaches `OBB_DEPTH` out of the wall.
        let other_mins = Vec3::new(0.0, -portal.half_width, -portal.half_height);
        let other_maxs = Vec3::new(
            super::classes::portal::OBB_DEPTH,
            portal.half_width,
            portal.half_height,
        );
        if !super::obb::obb_intersects_obb(
            origin,
            angles,
            mins,
            maxs,
            portal.origin,
            portal.angles,
            other_mins,
            other_maxs,
        ) {
            continue;
        }
        if !fizzle_all {
            return (PlacementResult::OverlapLinked, fizzled);
        }
        fizzled.push(portal.id);
    }
    match fizzled.is_empty() {
        true => (PlacementResult::Success, fizzled),
        false => (PlacementResult::OverlapLinked, fizzled),
    }
}

/// `IsPortalOnValidSurface` (`portal_placement.cpp:1331`) — the centre and
/// four corners, each pulled in by a little more than the bump forgiveness,
/// must each be on a portalable surface: not inside a solid, not over open
/// air (unless a bumper stands in for the surface), not on a `func_door`, not
/// on sky or a pass-through material, and not on a no-portal one.
#[allow(clippy::too_many_arguments)]
fn on_valid_surface(
    world: &mut ShotWorld<'_>,
    origin: Vec3,
    forward: Vec3,
    right: Vec3,
    up: Vec3,
    half_width: f32,
    half_height: f32,
) -> bool {
    let inset = PORTAL_BUMP_FORGIVENESS * 1.1;
    for corner in 0..5 {
        let mut point = origin;
        if corner < 4 {
            point += match corner / 2 == 0 {
                true => up * (half_height - inset),
                false => up * -(half_height - inset),
            };
            point += match corner % 2 == 0 {
                true => right * -(half_width - inset),
                false => right * (half_width - inset),
            };
        }
        let mut trace = world.trace_line(point + forward, point - forward, MASK_SOLID_BRUSHONLY);
        if trace.hit.start_solid {
            // "Portal center/corner in solid"
            return false;
        }
        if trace.hit.fraction == 1.0 {
            // "Check if there's a portal bumper to act as a surface"
            let (bump, _) = trace_bumping_entities(world, point + forward, point - forward);
            trace = bump;
            if trace.hit.fraction == 1.0 {
                // "No surface behind the portal"
                return false;
            }
        }
        if world.is_func_door(trace.entity) {
            return false;
        }
        if world.is_pass_through(&trace) || is_no_portal_material(&trace) {
            return false;
        }
    }
    true
}

/// `UTIL_Portal_EntityIsInPortalHole` (`portal_util_shared.cpp:2540`) for the
/// player against a floor portal: does the player's box cross the portal's
/// plane with its whole cross-section inside the quad, one unit of slack each
/// way?
///
/// Valve's `OBBHasFullyContainedIntersectionWithQuad` clips the box against
/// the plane and tests the polygon. The player's box is axis-aligned and the
/// only portals this is asked about face within 37° of straight up, so the
/// cross-section is taken as the box's own footprint — exact for a level
/// floor, the case the check exists for.
fn player_in_portal_hole(player: &PlayerBox, portal: &PortalInfo) -> bool {
    let (forward, right, up) = angle_vectors(portal.angles);
    let lo = player.origin + player.mins;
    let hi = player.origin + player.maxs;
    let plane = forward.dot(portal.origin);
    let mut above = false;
    let mut below = false;
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        match forward.dot(corner) >= plane {
            true => above = true,
            false => below = true,
        }
    }
    if !(above && below) {
        return false;
    }
    let right = -right;
    [(lo.x, lo.y), (hi.x, lo.y), (lo.x, hi.y), (hi.x, hi.y)]
        .into_iter()
        .all(|(x, y)| {
            let offset = Vec3::new(x, y, portal.origin.z) - portal.origin;
            offset.dot(right).abs() <= portal.half_width + 1.0
                && offset.dot(up).abs() <= portal.half_height + 1.0
        })
}

// ---------------------------------------------------------------------------
// VerifyPortalPlacement
// ---------------------------------------------------------------------------

/// `VerifyPortalPlacement` (`portal_placement.cpp:1418`) — may a portal go at
/// `origin`, facing along `angles`, and if it has to move to fit, where to?
///
/// `ignore` is the portal being placed, which is never in its own way.
/// `origin` is moved in place on a bump. The result is
/// [`Bumped`](PlacementResult::Bumped) for every success — including one that
/// did not move, which is Valve's, and is why no caller of this ever sees
/// [`Success`](PlacementResult::Success).
pub fn verify(
    world: &mut ShotWorld<'_>,
    ignore: EntityId,
    origin: &mut Vec3,
    angles: Vec3,
    half_width: f32,
    half_height: f32,
    placed_by: PlacedBy,
) -> PlacementResult {
    let original = *origin;
    let (forward, right, up) = angle_vectors(angles);
    let (forward, right, up) = (
        forward.normalize_or_zero(),
        right.normalize_or_zero(),
        up.normalize_or_zero(),
    );
    let ignore_info = world.portals.iter().find(|p| p.id == ignore).copied();
    let placing = Placing {
        id: ignore,
        fired_by: ignore_info.and_then(|p| p.fired_by),
    };

    // "Check if center is on a surface" — with `prop_portal` added to the
    // filter's classname list, which a brush-and-studio trace never sees.
    let center = world.trace_line(*origin + forward, *origin - forward, MASK_SHOT_PORTAL);
    if center.hit.fraction == 1.0 {
        return PlacementResult::InvalidSurface;
    }
    // "Check if the surface is moving" — `sv_allow_mobile_portals` is 0.
    if world.is_moving(center.entity) {
        return PlacementResult::InvalidSurface;
    }
    if world.is_pass_through(&center) {
        return PlacementResult::PassthroughSurface;
    }
    if is_no_portal_material(&center) {
        return PlacementResult::InvalidSurface;
    }

    world.bumped_by_linked_portal = false;
    if placed_by == PlacedBy::Player {
        // "Bump away from linked portal so it can be fit next to it"
        fit_around_other_portals(world, ignore, origin, forward, right, up, half_width, half_height);
    }

    world.bumped.clear();
    let top = up * (half_height - PORTAL_BUMP_FORGIVENESS);
    let right_edge = right * (half_width - PORTAL_BUMP_FORGIVENESS);
    let frame = Frame {
        forward,
        right,
        top,
        bottom: -top,
        right_edge,
        left_edge: -right_edge,
        half_width,
    };
    let fits = [CornerFit::default(); 4];
    if !fit_on_surface(
        world,
        placing,
        origin,
        &frame,
        placed_by,
        0,
        fits,
        [0; 4],
        0,
    ) {
        if world.bumped_by_linked_portal {
            return PlacementResult::OverlapLinked;
        }
        return PlacementResult::CantFit;
    }

    // "Check if it's moved too far from it's original location" —
    // `MAXIMUM_BUMP_DISTANCE`, half the squared diagonal.
    let width = half_width * 2.0;
    let height = half_height * 2.0;
    let maximum = (width * width + height * height) / 2.0;
    if origin.distance_squared(original) > maximum {
        return PlacementResult::CantFit;
    }

    // "if we're less than a unit from floor, we're going to bump to match it
    // exactly and help game movement code run smoothly"
    if up.z > 0.7 {
        let small_forward = forward * 0.05;
        let start = *origin + small_forward;
        let floor = world.trace_line(start, start - up * (half_height + 1.5), MASK_SOLID_BRUSHONLY);
        if floor.hit.fraction < 1.0 {
            let verify = world.trace_line(start, start - up * (half_height - 0.1), MASK_SOLID_BRUSHONLY);
            if verify.hit.fraction == 1.0 {
                *origin = floor.hit.end + up * half_height - small_forward;
            }
        }
    }

    if is_intersecting_no_portal_volume(world, *origin, angles, forward, half_width, half_height) {
        return PlacementResult::InvalidVolume;
    }
    let (overlap, _) =
        overlapping_other_portals(world, ignore, *origin, angles, half_width, half_height, false);
    if overlap != PlacementResult::Success {
        return overlap;
    }
    if !on_valid_surface(world, *origin, forward, right, up, half_width, half_height) {
        return PlacementResult::InvalidSurface;
    }

    // "Is a floor being moved to another floor" — the vertical-hop exploit:
    // a floor portal may not be re-placed close by while the player is
    // standing in its hole.
    if let (Some(me), Some(player)) = (ignore_info, world.player) {
        let (my_forward, _, _) = angle_vectors(me.angles);
        let close = me.origin.distance_squared(*origin)
            < half_width * half_width + half_height * half_height;
        if me.active && my_forward.z > 0.8 && forward.z > 0.8 && close && player_in_portal_hole(&player, &me) {
            return PlacementResult::OverlapLinked;
        }
    }

    PlacementResult::Bumped
}

/// `VerifyPortalPlacementAndFizzleBlockingPortals` (`portal_placement.cpp:1614`)
/// — [`verify`], and for a map's own portal (`PlacedBy::Fixed`) that overlaps
/// one, fizzle the overlapped portals and try again. Returns the ids fizzled.
pub fn verify_and_fizzle_blocking(
    world: &mut ShotWorld<'_>,
    ignore: EntityId,
    origin: &mut Vec3,
    angles: Vec3,
    half_width: f32,
    half_height: f32,
    placed_by: PlacedBy,
) -> (PlacementResult, Vec<EntityId>) {
    let mut result = verify(world, ignore, origin, angles, half_width, half_height, placed_by);
    let mut fizzled = Vec::new();
    if placed_by == PlacedBy::Fixed && result == PlacementResult::OverlapLinked {
        let (_, ids) =
            overlapping_other_portals(world, ignore, *origin, angles, half_width, half_height, true);
        for id in &ids {
            if let Some(portal) = world.portals.iter_mut().find(|p| p.id == *id) {
                portal.active = false;
            }
        }
        fizzled = ids;
        result = verify(world, ignore, origin, angles, half_width, half_height, placed_by);
    }
    (result, fizzled)
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// `UTIL_Portal_AABB` — a portal's trigger box, bounded in world space.
fn portal_aabb(portal: &PortalInfo) -> (Vec3, Vec3) {
    let (forward, right, up) = angle_vectors(portal.angles);
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for x in [0.0, super::classes::portal::OBB_DEPTH] {
        for y in [-portal.half_width, portal.half_width] {
            for z in [-portal.half_height, portal.half_height] {
                let p = portal.origin + forward * x + right * y + up * z;
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
    }
    (lo, hi)
}

/// `IntersectRayWithBox` for a line — the slab test, `None` for a miss. A
/// line that starts inside reports `start_solid` and a fraction of zero.
pub(crate) fn intersect_ray_with_box(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Option<ShotHit> {
    let delta = end - start;
    let mut enter = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    let mut normal = Vec3::ZERO;
    for axis in 0..3 {
        let (s, d, lo, hi) = (start[axis], delta[axis], mins[axis], maxs[axis]);
        if d.abs() < 1e-8 {
            if s < lo || s > hi {
                return None;
            }
            continue;
        }
        let (t0, t1, n0) = match d > 0.0 {
            true => ((lo - s) / d, (hi - s) / d, -1.0),
            false => ((hi - s) / d, (lo - s) / d, 1.0),
        };
        if t0 > enter {
            enter = t0;
            normal = Vec3::ZERO;
            normal[axis] = n0;
        }
        exit = exit.min(t1);
    }
    if enter > exit || exit < 0.0 || enter > 1.0 {
        return None;
    }
    let start_solid = enter < 0.0;
    let fraction = enter.max(0.0);
    let mut hit = ShotHit::miss(start, end);
    hit.fraction = fraction;
    hit.end = start + delta * fraction;
    hit.normal = normal;
    hit.start_solid = start_solid;
    Some(hit)
}

#[cfg(test)]
pub(crate) mod tests;
