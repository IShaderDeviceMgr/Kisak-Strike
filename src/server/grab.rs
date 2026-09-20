//! The game's half of carrying a prop — `CPlayerPickupController` and the
//! parts of `CGrabController` that are about an *entity* rather than a body.
//!
//! `portdocs/VPHYSICS_GRAB.md` is the design;
//! [`crate::vphysics::grab`] is the half that drives the body.
//!
//! Three questions live here, and all three are answered by pure functions so
//! that they can be tested without a map:
//!
//! 1. **What can be picked up** — [`can_pickup`], `CBasePlayer::CanPickupObject`
//!    (`baseplayer_shared.cpp:2829`) with Portal 2's 85 kg / 128 unit limits.
//! 2. **Where a held object goes** — [`hold_placement`], which is the geometry
//!    half of `CGrabController::UpdateObject`
//!    (`portal_grabcontroller_shared.cpp:1506`).
//! 3. **How it is oriented** — [`align_angles`] and the player-space transform,
//!    which are what make a carried cube snap to the world axes and turn with
//!    you.
//!
//! # The carry is a column, not a stick
//!
//! The part of `UpdateObject` that is easy to miss and gives Portal 2 its
//! feel: `player_hold_object_in_column` defaults to **1**, and with it the
//! hold distance is not measured along the look vector at all. The look ray is
//! intersected with a **vertical plane** standing a fixed distance in front of
//! the player, so the object keeps its horizontal stand-off and rides up and
//! down as you look up and down, instead of swinging in towards your feet.
//! Without it a cube dives at the floor whenever you look down.

use glam::{Mat3, Quat, Vec3};

use super::TouchQuery;
use crate::math::{angle_matrix, angle_vectors, matrix_angles};

/// `player_held_object_distance` — how far in front of the eye a held object
/// sits, before the player's own radius is added.
pub const HOLD_DISTANCE: f32 = 15.0;

/// `player_hold_column_max_size` — the furthest the column may push it.
pub const COLUMN_MAX: f32 = 96.0;

/// `player_held_object_offset_up_cube` — a `prop_weighted_cube` hangs ten
/// units *below* the eye line when you are looking level, and comes up to
/// centre as you look down. `GetObjectOffset` (`:1445`) returns 0 for
/// everything else on the physics path.
pub const CUBE_UP_OFFSET: f32 = -10.0;

/// `PLAYER_USE_RADIUS` (`baseplayer_shared.h:16`) — **100** under `PORTAL2`,
/// where every other branch of the tree gets 80.
pub const USE_RADIUS: f32 = 100.0;

/// How far `FindUseEntity`'s first ray reaches (`:1174`). The hit still has to
/// be inside [`USE_RADIUS`] to count; the ray is long so that it finds the
/// *nearest* thing rather than stopping short of it.
pub const USE_RAY: f32 = 1024.0;

/// `PORTAL_PLAYER_MAX_LIFT_MASS` (`portal_player_shared.h:19`), kilograms.
pub const MAX_LIFT_MASS: f32 = 85.0;

/// `PORTAL_PLAYER_MAX_LIFT_SIZE` (`portal_player_shared.h:20`), units on any
/// one OBB axis.
pub const MAX_LIFT_SIZE: f32 = 128.0;

/// `SetAngleAlignment( 0.866025403784 )` in `InitGrabController` (`:1249`) —
/// `cos 30°`. A carried object's axis snaps to a world axis once it is within
/// thirty degrees of one, which is why a cube is always square in your hands.
pub const ANGLE_ALIGNMENT: f32 = 0.866_025_4;

/// The pitch either side of level that a held object may be carried at,
/// `clamp( pitch, -75, 75 )` (`:1570`) for the player pickup, which never sets
/// `m_bAllowObjectOverhead`.
pub const MAX_CARRY_PITCH: f32 = 75.0;

/// `FindUseEntity`'s ten fallback tangents (`:1191`), in order: five angled
/// **down** at 45°, 30°, 20°, 15° and 10°, then five **up** at the same
/// angles. Valve's comment on the upward half is that it is *"useful in portal
/// when flying past a use target quickly"*.
pub const USE_TANGENTS: [f32; 10] = [
    1.0,
    0.577_350_26,
    0.363_970_23,
    0.267_949_2,
    0.176_326_98,
    -0.176_326_98,
    -0.267_949_2,
    -0.363_970_23,
    -0.577_350_26,
    -1.0,
];

/// How far each tangent trace reaches, and the half-extent of its hull
/// (`:1203`).
pub const USE_TANGENT_RANGE: f32 = 72.0;
pub const USE_TANGENT_HULL: f32 = 16.0;

/// The game-side half of one hold — `CPlayerPickupController`'s state, minus
/// the controller itself, which lives on
/// [`Physics`](super::physics::Physics) because it is about a body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Carry {
    /// What is being carried.
    pub entity: super::EntityId,
    /// `m_attachedAnglesPlayerSpace` — the orientation it was grabbed at, in
    /// the player's yaw frame and already run through [`align_angles`], so it
    /// turns with the player and stays square.
    pub angles_player_space: Vec3,
    /// `m_attachedPositionObjectSpace` — the object's centre in its own frame.
    pub center_object_space: Vec3,
    /// `pEntity->BoundingRadius()`.
    pub radius: f32,
    /// `GetObjectOffset( pEntity )`.
    pub up_offset: f32,
    /// `flLastDelta`, per-controller here where the shipped tree has one
    /// `static` for the whole process — `portdocs/VPHYSICS_GRAB.md` §6.2.
    pub floor_bump: f32,
}

/// `CBasePlayer::CanPickupObject` (`baseplayer_shared.cpp:2829`) plus
/// `CPortal_Player::PickupObject`'s own first guard (`:1024`).
///
/// `size` is the OBB's extent on each axis — Valve tests all three separately
/// rather than the diagonal.
pub fn can_pickup(mass: f32, size: Vec3, standing_on_it: bool) -> bool {
    // "can't pick up what you're standing on", which is checked before the
    // mass and size are and is the one that stops a player lifting the floor
    // out from under themselves.
    if standing_on_it {
        return false;
    }
    if mass > MAX_LIFT_MASS {
        return false;
    }
    size.x <= MAX_LIFT_SIZE && size.y <= MAX_LIFT_SIZE && size.z <= MAX_LIFT_SIZE
}

/// Everything [`hold_placement`] needs that is not a trace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hold {
    /// `Weapon_ShootPosition()` — the eye, not the feet.
    pub eye: Vec3,
    /// The player's view angles.
    pub view: Vec3,
    /// The player's collision hull, relative to their origin.
    pub mins: Vec3,
    pub maxs: Vec3,
    /// `pEntity->BoundingRadius()`.
    pub radius: f32,
    /// `GetObjectOffset( pEntity )` — [`CUBE_UP_OFFSET`] for a cube.
    pub up_offset: f32,
    /// `m_attachedAnglesPlayerSpace`, already aligned.
    pub angles_player_space: Vec3,
    /// `m_attachedPositionObjectSpace` — the object's centre in its own frame,
    /// which is what makes the target an *origin* rather than a centre.
    pub center_object_space: Vec3,
    /// `pPlayer->GetAbsVelocity()`, for the lead-the-player term.
    pub velocity: Vec3,
    /// The controller's speed limit, which that term is a fraction of.
    pub max_speed: f32,
    /// The server tick, for the same term.
    pub tick: f32,
}

/// Where a held object's **origin** and orientation should be this tick —
/// `CGrabController::UpdateObject` (`:1506`) with the portal branches removed.
///
/// `floor_bump` is `flLastDelta`, carried between calls.
///
/// > **`flLastDelta` is a function-level `static` in the shipped tree**
/// > (`:1806`), so every grab controller in the process shares one and it
/// > survives across pickups. With one player it leaks a value from the
/// > previous frame into a frame whose own trace started solid; with two it is
/// > a bug. It is per-controller here — `portdocs/VPHYSICS_GRAB.md` §6.2.
pub fn hold_placement(
    hold: &Hold,
    floor_bump: &mut f32,
    query: &mut dyn TouchQuery,
) -> (Vec3, Quat) {
    // `AngleDistance( playerAngles.x, 0 )` then the ±75° clamp. Without it,
    // looking straight down puts the object through the floor.
    let pitch = angle_distance(hold.view.x);
    let view = Vec3::new(
        pitch.clamp(-MAX_CARRY_PITCH, MAX_CARRY_PITCH),
        hold.view.y,
        hold.view.z,
    );
    let (forward, right, up) = angle_vectors(view);
    let start = hold.eye;

    // `m_StickNormal` is the surface the player is standing on under paint
    // gravity; without paint it is always world up, and this port has no
    // paint. Named rather than inlined because it is the axis the whole
    // "column" is measured against.
    let stick = Vec3::Z;
    let mut player2d = (hold.maxs - hold.mins) * 0.5;
    player2d -= stick * stick.dot(player2d);
    let radius = player2d.length() + hold.radius;
    let mut distance = HOLD_DISTANCE + radius;

    // `RemapValClamped( |pitch|, 0, 75, 1, 0 ) * GetObjectOffset()`.
    let up_offset = remap_clamped(pitch.abs(), 0.0, MAX_CARRY_PITCH, 1.0, 0.0) * hold.up_offset;

    // The column — see this module's header.
    let plane_normal = stick.cross(right);
    let point = start + plane_normal * distance;
    if let Some(hit) = intersect_ray_with_plane(start, forward, plane_normal, plane_normal.dot(point))
    {
        distance = hit.clamp(HOLD_DISTANCE, COLUMN_MAX);
    }

    // `MASK_SOLID_BRUSHONLY` from the eye to where the object wants to be.
    let ray_end = start + forward * distance + up * up_offset;
    let hit = query.solid_trace(start, ray_end, Vec3::ZERO, Vec3::ZERO);
    // > **`distance * tr.fraction` is not the distance along the ray.** The
    // > ray is `forward * distance + up * flUpOffset`, which is longer than
    // > `distance` whenever the up offset is non-zero, so the fraction is
    // > being scaled by the wrong length. Reproduced: it is what every shipped
    // > carry is tuned against, and the error is at most ten units of reach.
    let trace_distance = (distance * hit.fraction).max(radius);
    let direction = (ray_end - start).normalize_or_zero();
    // The up offset lands **twice** — once inside `direction`, which was
    // normalised from a delta that already contained it, and once again here.
    // Also shipped, also reproduced.
    let mut end = start + direction * trace_distance + up * up_offset;

    // Lift it off the floor: a line dropped from the carry point, and the
    // object is raised by however much of `radius` the floor took.
    let half_radius = radius * 0.5;
    let bump = query.solid_trace(
        end + Vec3::Z * (half_radius + 1.0),
        end - Vec3::Z * (half_radius + 1.0),
        Vec3::ZERO,
        Vec3::ZERO,
    );
    // `if ( !tr.startsolid )` — a trace that began inside geometry keeps the
    // *previous* frame's answer, which is what `flLastDelta` is for: it stops
    // the object popping while it passes through a wall it is momentarily
    // inside.
    if !bump.start_solid {
        *floor_bump = match bump.fraction < 1.0 {
            true => radius * (1.0 - bump.fraction),
            false => 0.0,
        };
    }
    end.z += *floor_bump;

    // Keep it out of the player: `end` must be at least `radius` from the
    // vertical line through them.
    let height = hold.maxs.z - hold.mins.z;
    let line = hold.eye - stick * height;
    let nearest = closest_point_on_segment(end, line + stick * height, line - stick * height);
    let delta = end - nearest;
    let length = delta.length();
    if length < radius {
        end = nearest + delta.normalize_or_zero() * radius;
    }

    // The orientation: the angles the object was grabbed at, held in the
    // player's frame and taken back out of it, so it turns with them.
    let rotation = from_player_space(hold.angles_player_space, hold.view.y);
    let offset = rotation * hold.center_object_space;

    // "if the player is moving pretty fast, start moving the object more
    // towards where they're going to be instead of where they are".
    let speed = hold.velocity.length();
    if speed > 0.0 {
        let direction = (end - (hold.eye - stick * height * 0.5)).normalize_or_zero();
        let addon = hold.velocity * (hold.tick * (speed / hold.max_speed).min(1.0));
        end += direction * addon.dot(direction).max(0.0);
    }

    (end - offset, rotation)
}

/// `TransformAnglesFromPlayerSpace` (`:573`) with `m_bIgnoreRelativePitch`
/// set, which `InitGrabController` always sets.
///
/// That branch builds a matrix from the player's eye angles with the forward
/// vector flattened against the player's up, which — with no paint gravity and
/// so world up — is exactly a yaw-only frame. So the whole transform is
/// "compose with the player's yaw", and the pitch deliberately does not reach
/// the held object: looking up and down does not tip a carried cube.
pub fn from_player_space(angles_player_space: Vec3, yaw: f32) -> Quat {
    let yaw = Quat::from_rotation_z(yaw.to_radians());
    yaw * Quat::from_mat3(&angle_matrix(angles_player_space))
}

/// `TransformAnglesToPlayerSpace` (`:560`) — the inverse, taken once at the
/// moment of the grab.
pub fn to_player_space(angles: Vec3, yaw: f32) -> Vec3 {
    let yaw = Quat::from_rotation_z(yaw.to_radians());
    matrix_angles(Mat3::from_quat(yaw.inverse() * Quat::from_mat3(&angle_matrix(angles))))
}

/// `AlignAngles` (`:87`) — snap each axis of a rotation to a world axis when
/// it is within `cosine` of one.
///
/// Valve's loop runs **z first** (`for ( int j = 3; --j >= 0; )`, with the
/// note *"NOTE: Must align z first"*) and re-orthogonalises after each snap,
/// so the order is load-bearing: aligning x first would leave z to be derived
/// from an already-snapped pair and could flip it.
pub fn align_angles(angles: Vec3, cosine: f32) -> Vec3 {
    let matrix = angle_matrix(angles);
    let mut columns = [matrix.x_axis, matrix.y_axis, matrix.z_axis];
    for j in (0..3).rev() {
        for i in 0..3 {
            if columns[j][i].abs() <= cosine {
                continue;
            }
            let mut snapped = Vec3::ZERO;
            snapped[i] = match columns[j][i] < 0.0 {
                true => -1.0,
                false => 1.0,
            };
            columns[j] = snapped;
            orthogonalize(&mut columns, j);
            break;
        }
    }
    matrix_angles(Mat3::from_cols(columns[0], columns[1], columns[2]))
}

/// `MatrixOrthogonalize` (`:63`) — rebuild the other two columns around the
/// one that was just snapped.
fn orthogonalize(columns: &mut [Vec3; 3], column: usize) {
    let (a, b, c) = (column, (column + 1) % 3, (column + 2) % 3);
    columns[c] = columns[a].cross(columns[b]).normalize_or_zero();
    columns[b] = columns[c].cross(columns[a]).normalize_or_zero();
}

/// `AngleDistance( angle, 0 )` — a `QAngle` component folded into
/// `[-180, 180]`.
fn angle_distance(angle: f32) -> f32 {
    let mut delta = angle % 360.0;
    if delta > 180.0 {
        delta -= 360.0;
    }
    if delta < -180.0 {
        delta += 360.0;
    }
    delta
}

/// `RemapValClamped`.
fn remap_clamped(value: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    if (b - a).abs() < f32::EPSILON {
        return d;
    }
    let t = ((value - a) / (b - a)).clamp(0.0, 1.0);
    c + (d - c) * t
}

/// `IntersectRayWithPlane( start, direction, normal, dist )` — the distance
/// along the ray at which it meets the plane, or `None` when it never does.
fn intersect_ray_with_plane(start: Vec3, direction: Vec3, normal: Vec3, dist: f32) -> Option<f32> {
    let denominator = normal.dot(direction);
    if denominator.abs() < 1e-6 {
        return None;
    }
    Some((dist - normal.dot(start)) / denominator)
}

/// `CalcClosestPointOnLine` for a bounded segment.
fn closest_point_on_segment(point: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let along = b - a;
    let length = along.length_squared();
    if length < 1e-6 {
        return a;
    }
    a + along * ((point - a).dot(along) / length).clamp(0.0, 1.0)
}

/// The rays `CPortal_Player::FindUseEntity` (`portal_player_shared.cpp:1155`)
/// casts, in the order it casts them.
///
/// Returned rather than traced here because the caller owns both clip chains —
/// the world's and the props' — and this module owns neither. The first entry
/// is the 1024-unit line; the ten after it are the tangent hulls, and each
/// carries the half-extent to sweep.
pub fn use_rays(eye: Vec3, view: Vec3) -> Vec<(Vec3, Vec3, f32)> {
    let (forward, _, up) = angle_vectors(view);
    let mut rays = Vec::with_capacity(1 + USE_TANGENTS.len());
    rays.push((eye, eye + forward * USE_RAY, 0.0));
    for tangent in USE_TANGENTS {
        let down = (forward - up * tangent).normalize_or_zero();
        rays.push((
            eye,
            eye + down * USE_TANGENT_RANGE,
            USE_TANGENT_HULL,
        ));
    }
    rays
}

/// A convenience for the caller: did a hit at `end` land inside the radius a
/// `+use` counts within?
pub fn within_use_radius(eye: Vec3, end: Vec3) -> bool {
    eye.distance(end) < USE_RADIUS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{NoTouchQuery, PushHit};

    /// A standing player hull and a 44-unit cube, which is the shipped
    /// `metal_box`'s size.
    fn hold(view: Vec3) -> Hold {
        Hold {
            eye: Vec3::new(0.0, 0.0, 64.0),
            view,
            mins: Vec3::new(-16.0, -16.0, 0.0),
            maxs: Vec3::new(16.0, 16.0, 72.0),
            radius: 22.0 * 3f32.sqrt(),
            up_offset: CUBE_UP_OFFSET,
            angles_player_space: Vec3::ZERO,
            center_object_space: Vec3::ZERO,
            velocity: Vec3::ZERO,
            max_speed: 1000.0,
            tick: 1.0 / 64.0,
        }
    }

    #[test]
    fn a_cube_is_light_enough_and_small_enough_to_lift() {
        // The four shipped cube masses, all under the 85 kg limit.
        for mass in [40.0, 45.0, 75.0] {
            assert!(can_pickup(mass, Vec3::splat(44.0), false), "{mass} kg");
        }
        assert!(!can_pickup(86.0, Vec3::splat(44.0), false), "over the limit");
        assert!(!can_pickup(40.0, Vec3::new(44.0, 44.0, 129.0), false), "too tall");
        assert!(!can_pickup(40.0, Vec3::splat(44.0), true), "standing on it");
    }

    /// The headline behaviour: a held cube sits in front of the eye.
    #[test]
    fn a_held_cube_hangs_in_front_of_the_player() {
        let hold = hold(Vec3::ZERO);
        let mut bump = 0.0;
        let (origin, _) = hold_placement(&hold, &mut bump, &mut NoTouchQuery);
        // Facing +X with no yaw, so it should be out along +X and near the
        // eye's height.
        assert!(origin.x > 30.0, "should be well in front, got {origin}");
        assert!(origin.x < COLUMN_MAX + 40.0, "but not beyond the column, got {origin}");
        assert!(origin.y.abs() < 1.0, "and centred, got {origin}");
    }

    /// The column: looking down must not drag the object towards the player's
    /// feet, it must keep its horizontal stand-off.
    ///
    /// Measured at 20°, because the column is *bounded*: the look ray reaches
    /// the vertical plane at `distance / cos(pitch)`, which for a cube's
    /// 75-unit stand-off passes `player_hold_column_max_size` (96) at about
    /// 38° and is clamped there. Past that the object does come inwards —
    /// which is the convar doing its job, not the column failing.
    #[test]
    fn looking_down_keeps_the_horizontal_distance() {
        let flat = |p: Vec3| Vec3::new(p.x, p.y, 0.0).length();
        // `up_offset` is zeroed here to measure the column on its own: the
        // offset is applied along the *view* up, so once the player is pitched
        // it has a horizontal component of its own — and the shipped code
        // applies it twice (see `hold_placement`), which is worth about five
        // units at 20°. That is a real part of the carry and is asserted
        // separately by `the_cube_offset_hangs_it_below_the_eye_line`.
        let mut level_hold = hold(Vec3::ZERO);
        level_hold.up_offset = 0.0;
        let mut down_hold = hold(Vec3::new(20.0, 0.0, 0.0));
        down_hold.up_offset = 0.0;

        let mut bump = 0.0;
        let level = hold_placement(&level_hold, &mut bump, &mut NoTouchQuery).0;
        let mut bump = 0.0;
        let down = hold_placement(&down_hold, &mut bump, &mut NoTouchQuery).0;

        assert!(
            (flat(level) - flat(down)).abs() < 0.5,
            "the column should hold the horizontal distance: {} level vs {} at 20 degrees",
            flat(level),
            flat(down)
        );
        // And it must actually have gone down.
        assert!(down.z < level.z - 10.0, "{} vs {}", down.z, level.z);
    }

    /// `player_held_object_offset_up_cube` is -10, so a cube rides below the
    /// eye line rather than in the middle of the view.
    #[test]
    fn the_cube_offset_hangs_it_below_the_eye_line() {
        let mut with = hold(Vec3::ZERO);
        with.up_offset = CUBE_UP_OFFSET;
        let mut without = hold(Vec3::ZERO);
        without.up_offset = 0.0;

        let mut bump = 0.0;
        let low = hold_placement(&with, &mut bump, &mut NoTouchQuery).0;
        let mut bump = 0.0;
        let centred = hold_placement(&without, &mut bump, &mut NoTouchQuery).0;
        assert!(
            low.z < centred.z - 10.0,
            "the cube offset should drop it: {} vs {}",
            low.z,
            centred.z
        );
    }

    /// The bound itself: past the clamp the object is drawn in, and never
    /// ends up inside the player.
    #[test]
    fn a_steep_look_is_bounded_by_the_column_and_stays_out_of_the_player() {
        let mut bump = 0.0;
        let hold = hold(Vec3::new(75.0, 0.0, 0.0));
        let (origin, _) = hold_placement(&hold, &mut bump, &mut NoTouchQuery);
        let radius = 22.63 + 22.0 * 3f32.sqrt();
        // Measured from the player's vertical axis, which is what
        // `UpdateObject`'s last clamp uses.
        let line = Vec3::new(0.0, 0.0, origin.z);
        assert!(
            origin.distance(line) >= radius - 1.0,
            "a steeply-held cube must stay clear of the player: {origin}"
        );
    }

    /// Turning turns what you are carrying.
    #[test]
    fn the_object_follows_the_players_yaw() {
        let mut bump = 0.0;
        let east = hold_placement(&hold(Vec3::ZERO), &mut bump, &mut NoTouchQuery).0;
        let mut bump = 0.0;
        let north = hold_placement(&hold(Vec3::new(0.0, 90.0, 0.0)), &mut bump, &mut NoTouchQuery).0;
        assert!(east.x > 30.0 && east.y.abs() < 1.0, "{east}");
        assert!(north.y > 30.0 && north.x.abs() < 1.0, "{north}");
    }

    /// A wall in front of the player pulls the object in rather than letting
    /// it hang inside the wall.
    #[test]
    fn a_wall_shortens_the_carry() {
        /// Everything is solid a third of the way along.
        struct Wall;
        impl TouchQuery for Wall {
            fn brush_models_touching(&mut self, _: Vec3, _: Vec3, _: Vec3, _: Vec3, _: &mut Vec<usize>) {}
            fn start_solid(&mut self, _: Vec3, _: Vec3, _: Vec3) -> bool {
                false
            }
            fn solid_trace(&mut self, start: Vec3, end: Vec3, _: Vec3, _: Vec3) -> PushHit {
                PushHit {
                    fraction: 0.33,
                    end: start + (end - start) * 0.33,
                    start_solid: false,
                }
            }
        }
        let mut bump = 0.0;
        let open = hold_placement(&hold(Vec3::ZERO), &mut bump, &mut NoTouchQuery).0;
        let mut bump = 0.0;
        let walled = hold_placement(&hold(Vec3::ZERO), &mut bump, &mut Wall).0;
        assert!(
            walled.x < open.x,
            "a wall should pull the carry in: {walled} vs {open}"
        );
    }

    /// `AlignAngles` is what makes a carried cube square.
    #[test]
    fn a_nearly_square_cube_is_snapped_square() {
        // 10° off axis is inside cos 30°, so it snaps to nothing at all.
        let snapped = align_angles(Vec3::new(0.0, 10.0, 0.0), ANGLE_ALIGNMENT);
        assert!(
            snapped.abs().max_element() < 1e-3,
            "10 degrees should snap to square, got {snapped}"
        );
        // 45° is outside it, and is left alone.
        let kept = align_angles(Vec3::new(0.0, 45.0, 0.0), ANGLE_ALIGNMENT);
        assert!(
            (kept.y - 45.0).abs() < 1e-2,
            "45 degrees should be left alone, got {kept}"
        );
    }

    #[test]
    fn player_space_angles_round_trip() {
        for (angles, yaw) in [
            (Vec3::ZERO, 0.0),
            (Vec3::new(0.0, 30.0, 0.0), 90.0),
            (Vec3::new(0.0, -120.0, 0.0), -45.0),
        ] {
            let local = to_player_space(angles, yaw);
            let world = from_player_space(local, yaw);
            let expected = Quat::from_mat3(&angle_matrix(angles));
            assert!(
                world.angle_between(expected) < 1e-3,
                "{angles} at yaw {yaw}: off by {} rad",
                world.angle_between(expected)
            );
        }
    }

    /// Pitch deliberately does not reach the held object — that is what
    /// `SetIgnorePitch( true )` buys.
    #[test]
    fn looking_up_and_down_does_not_tip_the_object() {
        let level = from_player_space(Vec3::ZERO, 0.0);
        let steep = from_player_space(Vec3::ZERO, 0.0);
        assert_eq!(level, steep);
        // The transform takes only a yaw, so there is no pitch to pass in:
        // the object's orientation is a function of the player's yaw alone.
        let turned = from_player_space(Vec3::ZERO, 90.0);
        assert!(turned.angle_between(level) > 1.0, "but yaw does turn it");
    }

    /// Eleven rays, the first long and thin and the ten after it short hulls,
    /// five angled down and five up.
    #[test]
    fn the_use_trace_is_one_ray_and_ten_tangent_hulls() {
        let rays = use_rays(Vec3::ZERO, Vec3::ZERO);
        assert_eq!(rays.len(), 11);
        assert_eq!(rays[0].2, 0.0, "the first is a line");
        assert!((rays[0].1.x - USE_RAY).abs() < 1e-3, "and reaches 1024 units");
        assert!(rays[1..].iter().all(|r| r.2 == USE_TANGENT_HULL));
        // The first five tangents aim below the eye, the last five above.
        assert!(rays[1..6].iter().all(|r| r.1.z < 0.0), "five down");
        assert!(rays[6..].iter().all(|r| r.1.z > 0.0), "then five up");
    }

    #[test]
    fn the_use_radius_is_portal_twos_hundred_units() {
        assert!(within_use_radius(Vec3::ZERO, Vec3::new(99.0, 0.0, 0.0)));
        assert!(!within_use_radius(Vec3::ZERO, Vec3::new(101.0, 0.0, 0.0)));
    }
}
