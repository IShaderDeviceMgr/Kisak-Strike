//! Things that are not the player going through a portal —
//! `CPortal_Base2D::Touch`, `ShouldTeleportTouchingEntity` and
//! `TeleportTouchingEntity` (`portal_base2d.cpp:707`,
//! `portal_base2d_shared.cpp:251`, `:349`) for a physics prop, and the
//! portal half of carrying one.
//!
//! The player's teleport is the movement's (`client/movement.rs`,
//! `HandlePortalling`), because `CPortal_Base2D::Touch` returns at once for a
//! player. Everything else that goes through a portal is simulated by vphysics
//! and teleported by the portal it is touching, and that is this module plus
//! [`Physics::step`](super::physics::Physics::step).
//!
//! # Ownership
//!
//! A portal *owns* a prop — `CPortalSimulator::TakeOwnershipOfEntity` — from
//! the tick the prop's collision first overlaps the portal's trigger box with
//! its centre **in front of** the plane, until it stops overlapping it. While
//! owned, the prop does not collide with the wall the portal is on inside the
//! portal's rectangle ([`PortalHole`]), so it can go *into* the hole; and once
//! its centre is behind the plane, moving inwards and inside the hole, it is
//! teleported to the partner, which takes ownership of it there.
//!
//! The centre-in-front rule is what stops a prop resting against the *back*
//! of a thin wall from falling through a portal on the front of it.
//!
//! # Carrying one through
//!
//! `CPortal_Player::m_bHeldObjectOnOppositeSideOfPortal` and
//! `m_hHeldObjectPortal` are one field here, [`Carry::through`] — the portal on
//! the *player's* side whose matrix takes the carry target to where the object
//! really is. It is toggled, exactly as Valve toggles it, whenever the object
//! goes through a portal ([`held_object_teleported`]) and whenever the player
//! does ([`player_teleported`]).
//!
//! [`Carry::through`]: super::grab::Carry::through

use glam::{Mat3, Mat4, Quat, Vec3};

use super::classes::portal::OBB_DEPTH;
use super::classes::PropPortal;
use super::entity::{EntityId, EntityList};
use super::TouchQuery;
use crate::math::angle_matrix;
use crate::vphysics::env::PortalHole;

/// How far behind its plane a portal's hole reaches, for a prop.
///
/// Valve's `pHoleShapeCollideable` is the portal's own rectangle extruded
/// into the wall, and the world clone the simulator builds reaches the same
/// distance on the far side — `vCollisionCloneExtents.x`. 64 is the same
/// distance the trigger box reaches in front, [`OBB_DEPTH`]; a prop's centre
/// is teleported the moment it crosses the plane, so no shipped prop has more
/// than half of itself (a cube's half-diagonal is about 28 units) behind it.
pub const HOLE_DEPTH: f32 = 64.0;

/// `cos( 30° )` — `COS_PI_OVER_SIX`, the floor test `GetExitSpeedRange` uses
/// (`portal_base2d_shared.cpp:990`).
const COS_PI_OVER_SIX: f32 = 0.866_025_4;

/// `CProp_Portal::GetMaximumExitSpeed` (`prop_portal_shared.cpp:271`).
pub const EXIT_SPEED_MAX: f32 = 1000.0;

/// `CProp_Portal::GetMinimumExitSpeed` for something that is not a player,
/// out of a floor portal: **225** if it went in through a floor as well,
/// **50** otherwise (`prop_portal_shared.cpp:209`).
const EXIT_SPEED_FLOOR_TO_FLOOR: f32 = 225.0;
const EXIT_SPEED_TO_FLOOR: f32 = 50.0;

/// One portal that is on and has a partner — everything a prop or a hold
/// trace needs of `CPortal_Base2D`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkedPortal {
    pub id: EntityId,
    pub linked: EntityId,
    pub origin: Vec3,
    /// `m_vForward`, out of the wall.
    pub forward: Vec3,
    /// `m_vRight` — the negated second column of the angle matrix.
    pub right: Vec3,
    pub up: Vec3,
    /// The portal's frame as a rotation: local x forward, y left, z up.
    pub rotation: Quat,
    pub half_width: f32,
    pub half_height: f32,
    /// `m_matrixThisToLinked`.
    pub matrix: Mat4,
}

impl LinkedPortal {
    /// Signed distance of `point` in front of the plane —
    /// `m_plane_Origin.normal.Dot( p ) - m_plane_Origin.dist`.
    pub fn plane_distance(&self, point: Vec3) -> f32 {
        self.forward.dot(point - self.origin)
    }

    /// `m_plane_Origin.normal.z > COS_PI_OVER_SIX`.
    pub fn is_floor(&self) -> bool {
        self.forward.z > COS_PI_OVER_SIX
    }

    /// The hole, as the solver's contact filter takes it.
    pub fn hole(&self) -> PortalHole {
        PortalHole {
            center: self.origin,
            forward: self.forward,
            right: self.right,
            up: self.up,
            half_width: self.half_width,
            half_height: self.half_height,
            depth: HOLE_DEPTH,
        }
    }

    /// The trigger box — `(0, -halfWidth, -halfHeight)` to
    /// `(64, halfWidth, halfHeight)` in the portal's frame, one-sided — as
    /// `(centre, rotation, half-extents)`.
    pub fn trigger_box(&self) -> (Vec3, Quat, Vec3) {
        (
            self.origin + self.forward * (OBB_DEPTH * 0.5),
            self.rotation,
            Vec3::new(OBB_DEPTH * 0.5, self.half_width, self.half_height),
        )
    }

    /// The hole shape `EntityIsInPortalHole` tests against — the rectangle,
    /// [`HOLE_DEPTH`] into the wall.
    pub fn hole_box(&self) -> (Vec3, Quat, Vec3) {
        (
            self.origin - self.forward * (HOLE_DEPTH * 0.5),
            self.rotation,
            Vec3::new(HOLE_DEPTH * 0.5, self.half_width, self.half_height),
        )
    }

    /// The rotation half of [`matrix`](LinkedPortal::matrix).
    pub fn turn(&self) -> Quat {
        Quat::from_mat3(&Mat3::from_mat4(self.matrix)).normalize()
    }

    /// Where the segment `start`–`end` goes **into** the portal, as a fraction
    /// along it — from in front of the plane to behind it, through the
    /// rectangle. `UTIL_IntersectRayWithPortal`, one-sided.
    pub fn entered_by(&self, start: Vec3, end: Vec3) -> Option<f32> {
        let (before, after) = (self.plane_distance(start), self.plane_distance(end));
        if before < 0.0 || after >= 0.0 {
            return None;
        }
        let t = before / (before - after);
        let at = start + (end - start) * t - self.origin;
        let inside = at.dot(self.right).abs() <= self.half_width
            && at.dot(self.up).abs() <= self.half_height;
        inside.then_some(t)
    }
}

/// Every portal that is on and linked to a partner that is on too.
pub fn linked_portals(entities: &EntityList) -> Vec<LinkedPortal> {
    let active = |id: EntityId| {
        entities
            .get(id)
            .filter(|e| !e.core.removed)
            .and_then(|e| e.behaviour.downcast_ref::<PropPortal>())
            .is_some_and(|p| p.activated)
    };
    entities
        .iter()
        .filter(|(_, e)| !e.core.removed)
        .filter_map(|(id, entity)| {
            let portal = entity.behaviour.downcast_ref::<PropPortal>()?;
            let linked = portal.linked.filter(|_| portal.is_active_and_linked())?;
            if !active(linked) {
                return None;
            }
            let core = &entity.core;
            Some(LinkedPortal {
                id,
                linked,
                origin: core.origin,
                forward: PropPortal::forward(core),
                right: PropPortal::right(core),
                up: PropPortal::up(core),
                rotation: Quat::from_mat3(&angle_matrix(core.angles)).normalize(),
                half_width: portal.half_width,
                half_height: portal.half_height,
                matrix: portal.matrix,
            })
        })
        .collect()
}

/// `CPortal_Base2D::GetExitSpeedRange` for something that is **not** a
/// player — `(minimum, maximum)`.
///
/// Only a floor exit has a minimum, and it depends on the entrance too: 225
/// from floor to floor, 50 from anywhere else, which is what stops a cube
/// dropped into a floor portal from sitting in the exit forever.
pub fn exit_speed_range(entrance: &LinkedPortal, exit: &LinkedPortal) -> (f32, f32) {
    let minimum = match (exit.is_floor(), entrance.is_floor()) {
        (true, true) => EXIT_SPEED_FLOOR_TO_FLOOR,
        (true, false) => EXIT_SPEED_TO_FLOOR,
        (false, _) => f32::NEG_INFINITY,
    };
    (minimum, EXIT_SPEED_MAX)
}

/// `TeleportTouchingEntity`'s *"velocity hacks"* block
/// (`portal_base2d_shared.cpp:529`) for something that is not a player.
///
/// **Scaled, not added to** — the player's own version adds speed along the
/// exit's forward, and this one does not: a still object is given
/// `exit_forward * minimum`, a slow one is scaled up to the minimum *in the
/// direction it was already going*, and a fast one is scaled down.
pub fn clamp_exit_velocity(velocity: Vec3, exit_forward: Vec3, minimum: f32, maximum: f32) -> Vec3 {
    let speed = velocity.length();
    if speed == 0.0 {
        return match minimum >= 0.0 {
            true => exit_forward * minimum,
            false => velocity,
        };
    }
    if speed < minimum {
        velocity * (minimum / speed)
    } else if speed > maximum {
        velocity * (maximum / speed)
    } else {
        velocity
    }
}

/// The held object went through `portal` —
/// `ToggleHeldObjectOnOppositeSideOfPortal` and
/// `SetHeldObjectPortal( this )` (`portal_base2d_shared.cpp:720`).
pub fn held_object_teleported(through: Option<EntityId>, portal: &LinkedPortal) -> Option<EntityId> {
    match through {
        Some(_) => None,
        None => Some(portal.id),
    }
}

/// The player went through `portal` while carrying something —
/// `ToggleHeldObjectOnOppositeSideOfPortal` and
/// `SetHeldObjectPortal( m_hLinkedPortal )` (`:744`). The portal on the
/// player's side is now the one they came out of.
pub fn player_teleported(through: Option<EntityId>, portal: &LinkedPortal) -> Option<EntityId> {
    match through {
        Some(_) => None,
        None => Some(portal.linked),
    }
}

/// A line trace that goes through at most one portal —
/// `UTIL_Portal_TraceRay` as the grab controller uses it
/// (`portal_grabcontroller_shared.cpp:1735`), `MASK_SOLID_BRUSHONLY`.
///
/// Returns the fraction along `start`–`end` the *whole* trace got, counting
/// the part after the portal as if the line had carried straight on — which is
/// how Valve's caller uses it: `distance * tr.fraction` is a reach in the
/// player's own space whether or not a portal was on the way.
pub fn trace_line_through(
    query: &mut dyn TouchQuery,
    portals: &[LinkedPortal],
    start: Vec3,
    end: Vec3,
) -> f32 {
    let hit = query.solid_trace(start, end, Vec3::ZERO, Vec3::ZERO);
    // The first portal the line enters no later than it hits the wall — a
    // portal's own wall stops the trace at the plane, so "no later" has to
    // allow for the trace stopping a hair short.
    let entered = portals
        .iter()
        .filter_map(|portal| Some((portal.entered_by(start, end)?, portal)))
        .filter(|(t, _)| *t <= hit.fraction + 1e-3)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    let Some((t, portal)) = entered else {
        return hit.fraction;
    };
    let at = start + (end - start) * t;
    let exit_start = portal.matrix.transform_point3(at);
    let exit_end = portal.matrix.transform_point3(end);
    let beyond = query.solid_trace(exit_start, exit_end, Vec3::ZERO, Vec3::ZERO);
    t + (1.0 - t) * beyond.fraction
}

/// `EntityId::to_int`'s inverse over the live list — how a portal key from
/// the client ([`PlayerState::portal_entered`](super::PlayerState::portal_entered))
/// becomes an entity again.
pub fn portal_by_key(portals: &[LinkedPortal], key: u64) -> Option<&LinkedPortal> {
    portals.iter().find(|portal| portal.id.to_int() == key)
}

#[cfg(test)]
mod tests;
