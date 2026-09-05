//! Turning a command into a position. `game/shared/gamemovement.cpp`.
//!
//! Stage 1 was `FullNoClipMove` (`:2525`) and the `Accelerate` (`:2075`) it
//! shares with walking. **Stage 4 is the rest**: gravity, friction, `WalkMove`,
//! `AirMove`, `TryPlayerMove`, stair stepping, `CategorizePosition`, jumping
//! and ducking — everything that needs [`trace`](crate::engine::trace).
//!
//! # Which `gamemovement.cpp` this is
//!
//! **`CPortalGameMovement`, not `CGameMovement`.** Portal 2 overrides two
//! dozen of the base class's methods, and several of the overrides change
//! behaviour that has nothing to do with portals. Taking the base class would
//! produce a player who moves *plausibly* and wrongly. The differences that
//! survive into this port, each cited at the site:
//!
//! | | `CGameMovement` | `CPortalGameMovement` |
//! |---|---|---|
//! | Jump height | 21 units | **45** |
//! | Bunny-hop speed boost on jump | yes (HL2) | **none** |
//! | Jump while ducked | allowed, at a fixed speed | **refused** |
//! | Air-control speed cap | 30 | **60** |
//! | Duck transition | 200 ms (CS:GO) | **400 ms** |
//! | Gravity | 800 | **600** |
//! | Edge friction | off | **on**, doubling friction near a ledge |
//! | `ClipVelocity`'s re-push | at least `DIST_EPSILON` | just cancels the residual |
//! | `StayOnGround`'s up-probe | 2 units | **1 unit** |
//! | Walking into a standable slope | `StepMove` | **slides up the ramp** |
//!
//! Where Portal's override differs only by generalising world `+Z` to an
//! arbitrary "stick normal" — its paint-gel gravity reorientation — the two are
//! identical with no paint, because `m_vGravityDirection = -stickNormal`
//! (`portal_gamemovement.cpp:440`) and the stick normal is world up. Those are
//! ported in the world-`+Z` form, and the generalisation is noted where it
//! would matter to `paint/`.
//!
//! # This file is shared code
//!
//! `gamemovement.cpp` compiles into both the client and the server binaries,
//! and the same command must produce the same position on both or prediction
//! mispredicts. So **nothing here may assume a client**: no cvar handles, no
//! console, no view. Everything it reads arrives in [`MoveData`] and
//! [`MoveVars`], which are `CMoveData` and `movevars_shared.cpp` and are
//! exactly the interfaces Valve chose for the same reason.
//!
//! Where that shared code eventually lives — a `src/game/` shared with
//! `src/server/`, or a `pub(crate)` module — is deliberately not decided yet
//! (`portdocs/CLIENT.md` §10).

use glam::Vec3;

use super::player::{
    MoveType, VEC_DUCK_HULL_MAX, VEC_DUCK_HULL_MIN, VEC_DUCK_VIEW, VEC_HULL_MAX, VEC_HULL_MIN,
    VEC_VIEW,
};
use super::{ButtonBits, ViewAngles};
use crate::engine::trace::{Contents, Ray, Tracer};

/// `sv_maxspeed` (`movevars_shared.cpp:29`) — the server's ceiling on any
/// player's speed, not the speed a Portal 2 player walks at. See
/// [`SV_SPEED_NORMAL`].
pub const SV_MAXSPEED: f32 = 320.0;

/// `sv_speed_normal` (`portal_gamemovement.cpp:54`) — **the Portal 2 player's
/// max speed**, and the number `CheckParameters` and `WalkMove` clamp against.
///
/// `CBasePlayer::GetPlayerMaxSpeed` (`baseplayer_shared.cpp:212`) is
/// `min(sv_maxspeed, MaxSpeed())`, and a Portal player's `MaxSpeed()` is set
/// to this (`portal_player_shared.cpp:1591`). So a Portal 2 player's
/// `mv->m_flMaxSpeed` is **175, not 320** — including in noclip, whose ceiling
/// is therefore `175 * sv_noclipspeed`.
pub const SV_SPEED_NORMAL: f32 = 175.0;

/// `sv_gravity` (`movevars_shared.cpp:21`). **600 for Portal 2**, 800 for
/// CS:GO — `DEFAULT_GRAVITY_STRING` is `#if defined(HL2_DLL) || ... ||
/// defined(PORTAL2)`. Exactly the CS:GO-shaped default `PORTING.md` warns
/// about.
pub const SV_GRAVITY: f32 = 600.0;

/// `sv_friction` (`movevars_shared.cpp:44`) — 5.2, not the 4.0 older Source
/// branches ship.
pub const SV_FRICTION: f32 = 5.2;

/// `sv_stopspeed` (`movevars_shared.cpp:23`): below this, friction bleeds as
/// if the player were moving at it, which is what stops a walk dead rather
/// than asymptotically.
pub const SV_STOPSPEED: f32 = 80.0;

/// `sv_accelerate` (`movevars_shared.cpp:31`) — ground acceleration.
pub const SV_ACCELERATE: f32 = 5.5;

/// `sv_airaccelerate` (`movevars_shared.cpp:37`) — air control.
pub const SV_AIRACCELERATE: f32 = 12.0;

/// `sv_stepsize` (`movevars_shared.cpp:52`) — how high a step can be walked up
/// without jumping.
pub const SV_STEPSIZE: f32 = 18.0;

/// `sv_maxvelocity` (`movevars_shared.cpp:47`) — a **per-axis** clamp, not a
/// clamp on the magnitude.
pub const SV_MAXVELOCITY: f32 = 3500.0;

/// `sv_edgefriction` (`portal_gamemovement.cpp:3350`) — the multiplier applied
/// to friction when the player is walking off a ledge.
pub const SV_EDGEFRICTION: f32 = 2.0;

/// `sv_use_edgefriction` (`portal_gamemovement.cpp:3351`). **On in Portal 2**,
/// which the base `CGameMovement::Friction` has no equivalent of at all.
pub const SV_USE_EDGEFRICTION: bool = true;

/// `sv_noclipspeed` (`movevars_shared.cpp:25`): the multiplier
/// `CGameMovement::PlayerMove` hands `FullNoClipMove` (`:5093`).
pub const SV_NOCLIPSPEED: f32 = 5.0;

/// `sv_noclipaccelerate` (`movevars_shared.cpp:24`).
///
/// **Not zero**, which is the difference between this and the placeholder
/// camera it replaced: the shipped game accelerates and bleeds off speed with
/// friction. Set it to 0 for the camera's old instant-stop feel.
pub const SV_NOCLIPACCELERATE: f32 = 5.0;

/// The height Portal 2 jumps: `flMul = sqrt( 2 * sv_gravity * 45.f )`
/// (`portal_gamemovement.cpp:573`).
///
/// **The base class uses `GAMEMOVEMENT_JUMP_HEIGHT`, which is 21**
/// (`gamemovement.h:24`). Porting the base gives a jump less than half as
/// high — 158 units/s of launch velocity against Portal's 232 — which reads as
/// "the gravity is wrong" and is not.
pub const JUMP_HEIGHT: f32 = 45.0;

/// The cosine of the steepest slope that can be stood on — `CRITICAL_SLOPE`
/// (`portal_gamemovement.cpp:102`), and the bare `0.7` littered through the
/// base class. About 45.6 degrees.
pub const CRITICAL_SLOPE: f32 = 0.7;

/// `NON_JUMP_VELOCITY` (`gamemovement.cpp:4184`): rising faster than this means
/// the player is definitely not on the ground. A jump is about 145.
const NON_JUMP_VELOCITY: f32 = 140.0;

/// `MAX_CLIP_PLANES` (`gamemovement.cpp:33`) — how many surfaces one move may
/// slide along before giving up and stopping.
const MAX_CLIP_PLANES: usize = 5;

/// `MINIMUM_MOVE_FRACTION` (`gamemovement.cpp:86`). Valve's comment: "extremely
/// tiny move fractions cause problems in later computations that determine
/// values using portions of distance moved."
const MINIMUM_MOVE_FRACTION: f32 = 0.0001;

/// `EFFECTIVELY_HORIZONTAL_NORMAL_Z` (`gamemovement.cpp:87`) — a plane this
/// close to vertical is *made* vertical before the velocity is clipped to it,
/// so that walking into a wall does not creep the player up or down it.
const EFFECTIVELY_HORIZONTAL_NORMAL_Z: f32 = 0.0001;

/// `DIST_EPSILON` (`public/coordsize.h:35`), the same 1/32 unit
/// [`trace`](crate::engine::trace) stops short by. Movement adds it back in the
/// two places it needs to clear a surface it is standing on.
const DIST_EPSILON: f32 = 0.03125;

/// `GAMEMOVEMENT_DUCK_TIME` (`gamemovement.h:22`) — the duck timer's full
/// value, in milliseconds. Not the duration of the transition; see
/// [`TIME_TO_DUCK_MSECS`].
const DUCK_TIME_MSECS: i32 = 1000;

/// `TIME_TO_DUCK_MSECS` (`shareddefs.h:100`) — **400 for Portal 2.** The 200 at
/// `:96` is `#if defined(TF_DLL) || ... || defined( CSTRIKE15 )`, so reading
/// the first branch of that `#if` gives a crouch twice as fast as the shipped
/// game's.
const TIME_TO_DUCK_MSECS: i32 = 400;

/// `TIME_TO_UNDUCK_MSECS` (`shareddefs.h:104`) — 200 for every game, so
/// standing up is twice as fast as crouching.
const TIME_TO_UNDUCK_MSECS: i32 = 200;

/// `HandleDuckingSpeedCrop`'s factor (`gamemovement.cpp:4736`) — a ducked
/// player on the ground moves at a third speed.
const DUCK_SPEED_CROP: f32 = 1.0 / 3.0;

/// The `sv_*` movement variables — `game/shared/movevars_shared.cpp`, which is
/// a file of exactly these.
///
/// Read once per command and passed down, rather than reached through cvar
/// handles: this module compiles into a server too, and a cvar handle is a
/// client-side convenience the shared code may not have. [`MoveVars::PORTAL2`]
/// is the shipped set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveVars {
    pub gravity: f32,
    pub friction: f32,
    pub stopspeed: f32,
    pub accelerate: f32,
    pub airaccelerate: f32,
    pub stepsize: f32,
    pub maxvelocity: f32,
    pub edgefriction: f32,
    pub use_edgefriction: bool,
    pub noclipspeed: f32,
    pub noclipaccelerate: f32,
}

impl MoveVars {
    /// The shipped Portal 2 defaults.
    ///
    /// The engine builds its set from the cvars instead
    /// (`Client::move_vars`), so nothing outside tests reads this yet — but it
    /// is the one place the shipped numbers appear together, and `server/` will
    /// want exactly it.
    #[allow(dead_code)]
    pub const PORTAL2: MoveVars = MoveVars {
        gravity: SV_GRAVITY,
        friction: SV_FRICTION,
        stopspeed: SV_STOPSPEED,
        accelerate: SV_ACCELERATE,
        airaccelerate: SV_AIRACCELERATE,
        stepsize: SV_STEPSIZE,
        maxvelocity: SV_MAXVELOCITY,
        edgefriction: SV_EDGEFRICTION,
        use_edgefriction: SV_USE_EDGEFRICTION,
        noclipspeed: SV_NOCLIPSPEED,
        noclipaccelerate: SV_NOCLIPACCELERATE,
    };
}

/// `CMoveData` (`game/shared/imovehelper.h`) plus the parts of
/// `player->m_Local` that movement reads and writes.
///
/// Valve splits these across `mv` and `player`; there is no such split here
/// because there is no entity, so the whole per-command state travels in one
/// struct and the caller copies it back onto the [`Player`](super::Player).
/// That is `ProcessMovement`'s `SetupMove`/`FinishMove` bracket
/// (`gamemovement.cpp:1325`) with the networking bookkeeping removed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveData {
    /// The player's **feet**, moved in place.
    pub origin: Vec3,
    pub velocity: Vec3,
    /// `m_vecViewAngles` — the angles the command carried, which is what the
    /// movement basis comes from.
    pub angles: ViewAngles,
    pub forwardmove: f32,
    pub sidemove: f32,
    pub upmove: f32,
    pub buttons: ButtonBits,
    /// `m_nOldButtons` — what was held on the *previous* command. Jump reads it
    /// to refuse a pogo stick and duck reads it for press/release edges, so it
    /// has to outlive the command.
    pub old_buttons: ButtonBits,
    /// `mv->m_flMaxSpeed`, which for Portal 2 is [`SV_SPEED_NORMAL`].
    pub max_speed: f32,
    pub move_type: MoveType,

    /// `player->GetGroundEntity()`, reduced to what a world-only port can say:
    /// the normal of the plane underfoot, or `None` for airborne.
    ///
    /// The entity itself is what Valve stores, and it is what conveyor and
    /// platform velocity would come from — `server/`'s, along with
    /// `GetBaseVelocity`.
    pub ground: Option<Vec3>,
    /// `player->m_surfaceFriction`. 1.0 except after losing the ground while
    /// rising, where `CategorizePosition` drops it to 0.25.
    pub surface_friction: f32,

    /// `player->m_Local.m_bDucked` — the hull *is* the ducked one.
    pub ducked: bool,
    /// `player->m_Local.m_bDucking` — mid-transition, either way.
    pub ducking: bool,
    /// `player->m_Local.m_nDuckTimeMsecs`, counted **down** by
    /// [`reduce_timers`] from [`DUCK_TIME_MSECS`].
    pub duck_time_msecs: i32,
    /// `player->GetViewOffset()` — the eye above [`origin`](MoveData::origin),
    /// interpolated through a duck.
    pub view_offset: Vec3,
    /// `m_iSpeedCropped`'s `SPEED_CROPPED_DUCK` bit: the duck speed crop is
    /// applied at most once per command.
    pub speed_cropped: bool,
}

/// `GetPlayerMins`/`GetPlayerMaxs` (`gamemovement.cpp`) — the hull for the
/// player's current stance.
pub fn player_mins(ducked: bool) -> Vec3 {
    // The two are equal for Portal 2 — the origin is on the floor between the
    // feet either way, which is why `FinishDuck` moves the origin only when the
    // player is airborne. Written as the branch it is anyway: a game whose duck
    // hull sinks the origin would break silently otherwise, and Valve's
    // `GetPlayerMins` really is a branch.
    match ducked {
        true => VEC_DUCK_HULL_MIN,
        false => VEC_HULL_MIN,
    }
}

/// See [`player_mins`].
pub fn player_maxs(ducked: bool) -> Vec3 {
    match ducked {
        true => VEC_DUCK_HULL_MAX,
        false => VEC_HULL_MAX,
    }
}

/// `GetPlayerViewOffset` — where the eye sits above the feet, standing or
/// ducked.
pub fn player_view_offset(ducked: bool) -> Vec3 {
    match ducked {
        true => VEC_DUCK_VIEW,
        false => VEC_VIEW,
    }
}

/// `SimpleSpline` (`public/mathlib/mathlib.h:1626`) — ease in, ease out.
fn simple_spline(value: f32) -> f32 {
    let squared = value * value;
    3.0 * squared - 2.0 * squared * value
}

/// `CGameMovement::TracePlayerBBox` (`gamemovement.h:308`) — sweep the player's
/// current hull, against the world only.
fn trace_player_bbox(
    mv: &MoveData,
    tracer: &mut Tracer<'_>,
    start: Vec3,
    end: Vec3,
) -> crate::engine::trace::Trace {
    trace_hull(tracer, start, end, mv.ducked)
}

/// The same, for the one caller that needs a hull other than the current one.
fn trace_hull(
    tracer: &mut Tracer<'_>,
    start: Vec3,
    end: Vec3,
    ducked: bool,
) -> crate::engine::trace::Trace {
    let ray = Ray::hull(start, end, player_mins(ducked), player_maxs(ducked));
    tracer.trace(&ray, Contents::MASK_PLAYERSOLID)
}

/// `CGameMovement::CheckVelocity` (`gamemovement.cpp:3410`).
///
/// The clamp is **per axis**, not on the magnitude — a diagonal can legally
/// exceed `maxvelocity` by a factor of root three.
pub fn check_velocity(mv: &mut MoveData, vars: &MoveVars) {
    for axis in 0..3 {
        if mv.velocity[axis].is_nan() {
            mv.velocity[axis] = 0.0;
        }
        if mv.origin[axis].is_nan() {
            mv.origin[axis] = 0.0;
        }
        mv.velocity[axis] = mv.velocity[axis].clamp(-vars.maxvelocity, vars.maxvelocity);
    }
}

/// `CGameMovement::Accelerate` (`gamemovement.cpp:2075`).
///
/// **This branch's version, not the classic one.** Every older Source release
/// scales the acceleration by `wishspeed`; `cstrike15` scales it by
/// `MAX( 250, wishspeed )`, so a slow wish still accelerates at the rate of a
/// 250-unit one. Copying the older formula would make low-speed movement feel
/// sluggish in a way that is very hard to attribute afterwards.
pub fn accelerate(mv: &mut MoveData, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32) {
    // See if we are changing direction a bit.
    let currentspeed = mv.velocity.dot(wishdir);

    // Reduce wishspeed by the amount of veer.
    let addspeed = wishspeed - currentspeed;
    if addspeed <= 0.0 {
        return;
    }

    let acceleration_scale = wishspeed.max(250.0);
    let accelspeed = (accel * dt * acceleration_scale * mv.surface_friction).min(addspeed);

    mv.velocity += wishdir * accelspeed;
}

/// `CPortalGameMovement::AirAccelerate` (`portal_gamemovement.cpp:626`).
///
/// **The wish speed is capped at 60, where the base class caps it at 30**
/// (`gamemovement.cpp:1975`). That cap is the whole of air control: it is how
/// much of the asked-for speed can be gained per second while airborne, and
/// doubling it is why a Portal 2 player can steer a fling and a CS:GO player
/// cannot.
///
/// Portal also scales the acceleration by `m_flAirInputScale`, which is 1.0
/// except while bounce or speed gel is damping the player's control
/// (`portal_player_shared.cpp:1732`). No paint, so no scale.
pub fn air_accelerate(mv: &mut MoveData, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32) {
    let wishspd = wishspeed.min(60.0);

    let currentspeed = mv.velocity.dot(wishdir);
    let addspeed = wishspd - currentspeed;
    if addspeed <= 0.0 {
        return;
    }

    // Note `wishspeed`, uncapped, here — only the *target* is capped, not the
    // rate of approach to it.
    let accelspeed = (accel * wishspeed * dt * mv.surface_friction).min(addspeed);
    mv.velocity += wishdir * accelspeed;
}

/// `CPortalGameMovement::ClipVelocity` (`portal_gamemovement.cpp:4303`) — slide
/// `velocity` along a plane.
///
/// **Portal drops the base class's `MIN( adjust, -DIST_EPSILON )`**
/// (`gamemovement.cpp:3535`), which pushed the result a fixed distance clear of
/// the plane. Portal only cancels the residual component, so a velocity that
/// ends up exactly parallel stays exactly parallel.
///
/// Returns Valve's blocked flags: 1 for a floor, 2 for a wall.
fn clip_velocity(input: Vec3, normal: Vec3, overbounce: f32) -> (Vec3, u32) {
    let angle = normal.z;

    let mut blocked = 0u32;
    if angle > 0.0 {
        blocked |= 0x01; // floor
    }
    if angle == 0.0 {
        blocked |= 0x02; // wall or step
    }

    let backoff = input.dot(normal) * overbounce;
    let mut out = input - normal * backoff;

    // Iterate once to make sure we are not still moving through the plane.
    let adjust = out.dot(normal);
    if adjust < 0.0 {
        out -= normal * adjust;
    }
    (out, blocked)
}

/// `CGameMovement::TryPlayerMove` (`gamemovement.cpp:2850`) — move along the
/// velocity, sliding along whatever is hit, for up to four bumps.
///
/// This is the function every other movement path ends in, and the one that
/// decides what a wall feels like. Returns the blocked flags.
fn try_player_move(
    mv: &mut MoveData,
    tracer: &mut Tracer<'_>,
    dt: f32,
    first_dest: Option<(Vec3, crate::engine::trace::Trace)>,
) -> u32 {
    const NUMBUMPS: usize = 4;

    let mut blocked = 0u32;
    let mut planes: Vec<Vec3> = Vec::with_capacity(MAX_CLIP_PLANES);

    let primal_velocity = mv.velocity;
    let mut original_velocity = mv.velocity;
    let mut new_velocity = Vec3::ZERO;

    let mut all_fraction = 0.0f32;
    let mut time_left = dt;

    for _ in 0..NUMBUMPS {
        if mv.velocity.length() == 0.0 {
            break;
        }

        let end = mv.origin + mv.velocity * time_left;

        // `WalkMove` has already traced to this exact point; reusing its result
        // is `g_bMovementOptimizations`' one visible effect.
        let mut pm = match &first_dest {
            Some((dest, trace)) if *dest == end => *trace,
            _ => trace_player_bbox(mv, tracer, mv.origin, end),
        };

        if pm.fraction > 0.0 && pm.fraction < MINIMUM_MOVE_FRACTION {
            pm.fraction = 0.0;
        }
        all_fraction += pm.fraction;

        // Started in a solid, or was in solid the whole way.
        if pm.all_solid {
            mv.velocity = Vec3::ZERO;
            return 4;
        }

        if pm.fraction > 0.0 {
            if pm.fraction == 1.0 {
                // "There's a precision issue with terrain tracing that can
                // cause a swept box to successfully trace when the end position
                // is stuck in the triangle." Re-test unswept before committing.
                let stuck = trace_player_bbox(mv, tracer, pm.end, pm.end);
                if stuck.start_solid || stuck.fraction != 1.0 {
                    mv.velocity = Vec3::ZERO;
                    break;
                }
            }
            mv.origin = pm.end;
            original_velocity = mv.velocity;
            planes.clear();
        }

        if pm.fraction == 1.0 {
            break; // moved the entire distance
        }

        if pm.normal.z > CRITICAL_SLOPE {
            blocked |= 1; // floor
        }
        if pm.normal.z.abs() < EFFECTIVELY_HORIZONTAL_NORMAL_Z {
            pm.normal.z = 0.0;
            blocked |= 2; // step or wall
        }

        time_left -= time_left * pm.fraction;

        if planes.len() >= MAX_CLIP_PLANES {
            // Should not happen; stop rather than slide through something.
            mv.velocity = Vec3::ZERO;
            break;
        }
        planes.push(pm.normal);

        // Only the first impact plane gets the reflection treatment: "you can
        // get yourself stuck in an acute corner by jumping in place and
        // pressing forward and nobody was really using this bounce/reflection
        // feature anyway".
        if planes.len() == 1 && mv.move_type == MoveType::Walk && mv.ground.is_none() {
            for plane in &planes {
                // `sv_bounce` is 0 in Portal 2, so both branches of Valve's
                // overbounce are 1 and the wall case collapses into the floor
                // case. Kept as one call rather than two identical ones.
                (new_velocity, _) = clip_velocity(original_velocity, *plane, 1.0);
                original_velocity = new_velocity;
            }
            mv.velocity = new_velocity;
            original_velocity = new_velocity;
        } else {
            let mut i = 0;
            while i < planes.len() {
                let (clipped, _) = clip_velocity(original_velocity, planes[i], 1.0);
                mv.velocity = clipped;

                // Are we now moving against any of the other planes?
                if planes
                    .iter()
                    .enumerate()
                    .all(|(j, other)| j == i || mv.velocity.dot(*other) >= 0.0)
                {
                    break; // didn't have to clip, so we're ok
                }
                i += 1;
            }

            if i == planes.len() {
                // Went all the way through the plane set: go along the crease.
                if planes.len() != 2 {
                    mv.velocity = Vec3::ZERO;
                    break;
                }
                let dir = planes[0].cross(planes[1]).normalize_or_zero();
                mv.velocity = dir * dir.dot(mv.velocity);
            }

            // If the new velocity opposes the original, stop dead rather than
            // oscillate in a sloping corner.
            if mv.velocity.dot(primal_velocity) <= 0.0 {
                mv.velocity = Vec3::ZERO;
                break;
            }
        }
    }

    if all_fraction == 0.0 {
        mv.velocity = Vec3::ZERO;
    }
    blocked
}

/// `CPortalGameMovement::StayOnGround` (`portal_gamemovement.cpp:3485`) — stop
/// a walking player bouncing off the tops of stairs and slopes.
///
/// **Portal's up-probe is 1 unit, the base class's is 2**
/// (`gamemovement.cpp:2119`).
fn stay_on_ground(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars) {
    let up = mv.origin + Vec3::Z;
    let down = mv.origin - Vec3::Z * vars.stepsize;

    // See how far up we can go without getting stuck.
    let trace = trace_player_bbox(mv, tracer, mv.origin, up);
    let start = trace.end;

    // Now trace down from a known safe position. `start_solid` is unreliable
    // here — Valve's comment: "it doesn't get set when tracing bounding box
    // vs. terrain".
    let trace = trace_player_bbox(mv, tracer, start, down);
    if trace.fraction > 0.0
        && trace.fraction < 1.0
        && !trace.start_solid
        && trace.normal.z >= CRITICAL_SLOPE
    {
        let delta = (mv.origin.z - trace.end.z).abs();
        // "This is incredibly hacky. The real problem is that trace returning
        // that strange value we can't network over." `COORD_RESOLUTION` is
        // 1/32.
        if delta > 0.5 * DIST_EPSILON {
            mv.origin = trace.end;
        }
    }
}

/// `CGameMovement::StepMove` (`gamemovement.cpp:1758`) — try the move at foot
/// height, then again from a step higher, and keep whichever got further.
fn step_move(
    mv: &mut MoveData,
    tracer: &mut Tracer<'_>,
    vars: &MoveVars,
    dt: f32,
    destination: Vec3,
    trace: crate::engine::trace::Trace,
) {
    let start_pos = mv.origin;
    let start_vel = mv.velocity;

    // First try walking straight to where they want to go.
    try_player_move(mv, tracer, dt, Some((destination, trace)));
    let down_pos = mv.origin;
    let down_vel = mv.velocity;

    // Reset and try again from a step higher.
    mv.origin = start_pos;
    mv.velocity = start_vel;

    // `m_bAllowAutoMovement` is true except inside a `trigger_no_automovement`,
    // which needs entities.
    let up = mv.origin + Vec3::Z * (vars.stepsize + DIST_EPSILON);
    let trace = trace_player_bbox(mv, tracer, mv.origin, up);
    if !trace.start_solid && !trace.all_solid {
        mv.origin = trace.end;
    }
    try_player_move(mv, tracer, dt, None);

    // Move back down a step (attempt to).
    let down = mv.origin - Vec3::Z * (vars.stepsize + DIST_EPSILON);
    let trace = trace_player_bbox(mv, tracer, mv.origin, down);

    // If we are not on the ground any more then use the original attempt.
    if trace.normal.z < CRITICAL_SLOPE {
        mv.origin = down_pos;
        mv.velocity = down_vel;
        return;
    }

    if !trace.start_solid && !trace.all_solid {
        mv.origin = trace.end;
    }
    let up_pos = mv.origin;

    // Decide which one went further, horizontally.
    let flat = |v: Vec3| (v.x - start_pos.x).powi(2) + (v.y - start_pos.y).powi(2);
    if flat(down_pos) > flat(up_pos) {
        mv.origin = down_pos;
        mv.velocity = down_vel;
    } else {
        // Keep the stepped-up position, but take the vertical velocity from the
        // slide: stepping up must not also cancel a fall.
        mv.velocity.z = down_vel.z;
    }
}

/// `CPortalGameMovement::WalkMove` (`portal_gamemovement.cpp:3688`).
fn walk_move(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars, dt: f32) {
    let old_ground = mv.ground;
    let (forward, right, _) = mv.angles.vectors();

    // Keep the movement basis in the plane of movement. With no paint the
    // gravity direction is world down, so this is "flatten and renormalise".
    let forward = Vec3::new(forward.x, forward.y, 0.0).normalize_or_zero();
    let right = Vec3::new(right.x, right.y, 0.0).normalize_or_zero();

    let mut wishvel = forward * mv.forwardmove + right * mv.sidemove;
    wishvel.z = 0.0;

    let mut wishspeed = wishvel.length();
    let wishdir = wishvel.normalize_or_zero();

    // Clamp to the server-defined max speed.
    if wishspeed != 0.0 && wishspeed > mv.max_speed {
        wishvel *= mv.max_speed / wishspeed;
        wishspeed = mv.max_speed;
    }

    // **Portal does not bracket this with `velocity.z = 0`, and does not apply
    // the base class's extra "keep us from going faster than allowed while
    // turning" clamp** (`gamemovement.cpp:2216`). Neither matters here — the
    // vertical velocity was already zeroed by `full_walk_move` and `wishdir`
    // is horizontal — but the second is a real behavioural difference the day
    // something adds vertical speed on the ground.
    accelerate(mv, wishdir, wishspeed, vars.accelerate, dt);

    let spd = mv.velocity.length();
    if spd < 1.0 {
        mv.velocity = Vec3::ZERO;
        return;
    }

    // First try just moving to the destination.
    let dest = mv.origin + mv.velocity * dt;
    let pm = trace_player_bbox(mv, tracer, mv.origin, dest);

    if pm.fraction == 1.0 {
        mv.origin = pm.end;
        stay_on_ground(mv, tracer, vars);
        return;
    }

    // Don't walk up stairs if not on ground.
    if old_ground.is_none() {
        return;
    }

    // **Portal's ramp slide** (`portal_gamemovement.cpp:3824`): walking into a
    // surface shallow enough to stand on redirects the velocity up the slope
    // instead of stepping. This is reachable without a portal in sight — it is
    // what walking up any ramp does.
    if pm.normal.z > CRITICAL_SLOPE {
        let wish_direction = mv.velocity.normalize_or_zero();
        let tangent_right = wish_direction.cross(pm.normal);
        let tangent_forward = pm.normal.cross(tangent_right).normalize_or_zero();

        let speed = mv.velocity.length();
        let end = mv.origin
            + (mv.velocity * pm.fraction + tangent_forward * (1.0 - pm.fraction) * speed) * dt;

        // "above code has the distinct possibility of placing the player inside
        // a wall. Not quite sure why it works so well most of the time."
        // `sv_portal_new_player_trace` is 1, so the check is on.
        let ramp = trace_player_bbox(mv, tracer, end, end);
        if !ramp.start_solid {
            mv.origin = end;
        } else {
            step_move(mv, tracer, vars, dt, dest, pm);
        }
    } else {
        step_move(mv, tracer, vars, dt, dest, pm);
    }

    stay_on_ground(mv, tracer, vars);
}

/// `CGameMovement::AirMove` (`gamemovement.cpp:2006`).
///
/// Portal's override (`:706`) adds portal funnelling and the gravity-direction
/// generalisation; without portals or paint the two are the same function.
fn air_move(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars, dt: f32) {
    let (forward, right, _) = mv.angles.vectors();
    let forward = Vec3::new(forward.x, forward.y, 0.0).normalize_or_zero();
    let right = Vec3::new(right.x, right.y, 0.0).normalize_or_zero();

    let mut wishvel = forward * mv.forwardmove + right * mv.sidemove;
    wishvel.z = 0.0;

    let mut wishspeed = wishvel.length();
    let wishdir = wishvel.normalize_or_zero();

    if wishspeed != 0.0 && wishspeed > mv.max_speed {
        wishvel *= mv.max_speed / wishspeed;
        wishspeed = mv.max_speed;
    }

    air_accelerate(mv, wishdir, wishspeed, vars.airaccelerate, dt);
    try_player_move(mv, tracer, dt, None);
}

/// `CPortalGameMovement::Friction` (`portal_gamemovement.cpp:3356`).
///
/// **Edge friction is Portal's and is on by default.** When the player is
/// walking towards a drop, friction doubles — which is what stops a Portal 2
/// player skating off every ledge they approach. The base `CGameMovement`
/// has no equivalent at all.
fn friction(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars, dt: f32) {
    let speed = mv.velocity.length();
    if speed < 0.1 {
        return;
    }

    let mut drop = 0.0;
    if mv.ground.is_some() {
        let mut friction = vars.friction * mv.surface_friction;

        if vars.use_edgefriction {
            // Valve's expression here is
            // `dir -= DotProduct( dir, gravityDir ) * dir`, which multiplies by
            // `dir` where it means `gravityDir` — a typo. It is unreachable:
            // `full_walk_move` zeroes the vertical velocity before calling this
            // on the ground, so the dot product is 0 and the statement is a
            // no-op either way. Written as the projection it means.
            let direction = Vec3::new(mv.velocity.x, mv.velocity.y, 0.0).normalize_or_zero();
            // 16 units ahead, 1 unit up, then 49 down — 1 for the bump plus the
            // 48 a player can fall and still jump back up.
            let start = mv.origin + direction * 16.0 + Vec3::Z;
            let stop = start - Vec3::Z * 49.0;

            let pm = trace_player_bbox(mv, tracer, start, stop);
            if pm.fraction == 1.0 {
                friction *= vars.edgefriction;
            }
        }

        // Bleed off some speed, but if we have less than the bleed threshold,
        // bleed the threshold amount.
        let control = match speed < vars.stopspeed {
            true => vars.stopspeed,
            false => speed,
        };
        drop += control * friction * dt;
    }

    let newspeed = (speed - drop).max(0.0);
    if newspeed != speed {
        mv.velocity *= newspeed / speed;
    }
}

/// `CPortalGameMovement::StartGravity` (`portal_gamemovement.cpp:3078`).
///
/// Half the frame's gravity before the move. Valve's comment: "yes, this 0.5
/// looks wrong, but it's not" — the other half is [`finish_gravity`], and
/// splitting it either side of the move is what makes a fall land in the same
/// place at any frame rate.
fn start_gravity(mv: &mut MoveData, vars: &MoveVars, dt: f32) {
    mv.velocity.z -= vars.gravity * 0.5 * dt;
    check_velocity(mv, vars);
}

/// `CPortalGameMovement::FinishGravity` (`portal_gamemovement.cpp:3128`) — the
/// other half.
fn finish_gravity(mv: &mut MoveData, vars: &MoveVars, dt: f32) {
    mv.velocity.z -= vars.gravity * 0.5 * dt;
    check_velocity(mv, vars);
}

/// `CGameMovement::SetGroundEntity` (`gamemovement.cpp:3985`), reduced to a
/// world without entities.
///
/// The base-velocity exchange it also does — adding and subtracting the ground
/// object's velocity as the player steps on and off it — is `server/`'s, and
/// is what makes conveyors and moving platforms work.
fn set_ground(mv: &mut MoveData, ground: Option<Vec3>) {
    mv.ground = ground;
    if ground.is_some() && mv.move_type != MoveType::Noclip {
        mv.velocity.z = 0.0;
    }
}

/// `TracePlayerBBoxForGround` (`gamemovement.cpp:4049`) — retry the ground
/// trace with each quadrant of the hull, looking for a shallower slope one
/// corner of the player is standing on.
///
/// The fraction and endpoint of the *original* trace are restored on the way
/// out, "so we don't try to move the player down to the new floor and get stuck
/// on a leaning wall that the original trace hit first".
fn trace_player_bbox_for_ground(
    mv: &MoveData,
    tracer: &mut Tracer<'_>,
    start: Vec3,
    end: Vec3,
    pm: &mut crate::engine::trace::Trace,
) {
    let fraction = pm.fraction;
    let endpos = pm.end;

    let mins_src = player_mins(mv.ducked);
    let maxs_src = player_maxs(mv.ducked);

    let quadrants = [
        // -x, -y
        (
            mins_src,
            Vec3::new(maxs_src.x.min(0.0), maxs_src.y.min(0.0), maxs_src.z),
        ),
        // +x, +y
        (
            Vec3::new(mins_src.x.max(0.0), mins_src.y.max(0.0), mins_src.z),
            maxs_src,
        ),
        // -x, +y
        (
            Vec3::new(mins_src.x, mins_src.y.max(0.0), mins_src.z),
            Vec3::new(maxs_src.x.min(0.0), maxs_src.y, maxs_src.z),
        ),
        // +x, -y
        (
            Vec3::new(mins_src.x.max(0.0), mins_src.y, mins_src.z),
            Vec3::new(maxs_src.x, maxs_src.y.min(0.0), maxs_src.z),
        ),
    ];

    for (mins, maxs) in quadrants {
        let ray = Ray::hull(start, end, mins, maxs);
        *pm = tracer.trace(&ray, Contents::MASK_PLAYERSOLID);
        if pm.did_hit() && pm.normal.z >= CRITICAL_SLOPE {
            break;
        }
    }

    pm.fraction = fraction;
    pm.end = endpos;
}

/// `CPortalGameMovement::CategorizePosition` (`portal_gamemovement.cpp:1202`) —
/// decide whether the player is on the ground, and snap them to it.
///
/// The speed-paint ramp launching and the portal-ramp tests in Portal's version
/// need paint and portals; what is left is the base class's shape with Portal's
/// constants. Note that this *moves the player* as well as classifying them:
/// `bMoveToEndPos` is `StayOnGround`'s stair debouncing folded into the trace
/// that is happening anyway.
fn categorize_position(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars) {
    // Reset each time, "otherwise we have bogus friction when we jump into
    // water and plunge downward really quickly".
    mv.surface_friction = 1.0;

    const GROUND_OFFSET: f32 = 2.0;
    let mut point = mv.origin - Vec3::Z * GROUND_OFFSET;
    let bump_origin = mv.origin;

    let zvel = mv.velocity.z;
    let moving_up_rapidly = zvel > NON_JUMP_VELOCITY;

    let mut move_to_end_pos = false;
    if mv.move_type == MoveType::Walk && mv.ground.is_some() {
        // Extend the trace down by a step so we don't bounce down slopes. The
        // ratio Portal scales this by is `MaxSpeed() / sv_speed_normal`, which
        // is 1 without speed gel.
        move_to_end_pos = true;
        point.z -= vars.stepsize;
    }

    // Valve leaves `pm` uninitialised on the rapid-rise path and then reads it
    // in the `bMoveToEndPos` block below; that is safe there only because the
    // same branch clears `bMoveToEndPos`. `Option` says so out loud.
    let mut ground_trace = None;

    if moving_up_rapidly {
        // Was on ground, but now suddenly am not.
        set_ground(mv, None);
        move_to_end_pos = false;
    } else {
        let mut pm = trace_player_bbox(mv, tracer, bump_origin, point);

        let standable =
            |t: &crate::engine::trace::Trace| t.did_hit() && t.normal.z >= CRITICAL_SLOPE;

        if !standable(&pm) {
            // Test four sub-boxes for a shallower slope we could stand on.
            trace_player_bbox_for_ground(mv, tracer, bump_origin, point, &mut pm);
            if !standable(&pm) {
                set_ground(mv, None);
                if mv.velocity.z > 0.0 && mv.move_type != MoveType::Noclip {
                    mv.surface_friction = 0.25;
                }
                move_to_end_pos = false;
            } else {
                set_ground(mv, Some(pm.normal));
            }
        } else {
            set_ground(mv, Some(pm.normal));
        }
        ground_trace = Some(pm);
    }

    // "This logic block essentially lifted from StayOnGround implementation."
    if let (true, Some(pm)) = (move_to_end_pos, ground_trace) {
        if !pm.start_solid && pm.fraction > 0.0 && pm.fraction < 1.0 {
            mv.origin = pm.end;
        }
    }
}

/// `CPortalGameMovement::CheckJumpButton` (`portal_gamemovement.cpp:528`).
///
/// **Three differences from the base class that a player would notice
/// immediately.** Portal jumps to 45 units where the base jumps to 21; Portal
/// refuses to jump at all while ducked where the base jumps at a fixed speed;
/// and Portal has no bunny-hop forward-speed bonus, which the base adds under
/// `HL2_DLL` and which Valve's `#ifdef PORTAL` explicitly enables for *Portal
/// 1*.
///
/// Returns whether the jump happened.
fn check_jump_button(mv: &mut MoveData, vars: &MoveVars, dt: f32) -> bool {
    // Cannot jump while ducked.
    if mv.ducked {
        return false;
    }

    // In the air, so no effect.
    if mv.ground.is_none() {
        mv.old_buttons = mv.old_buttons.insert(ButtonBits::JUMP);
        return false;
    }

    // Don't pogo stick: the button has to be released and pressed again.
    if mv.old_buttons.contains(ButtonBits::JUMP) {
        return false;
    }

    // Cannot jump in the unduck transition.
    if mv.ducking && mv.ducked {
        return false;
    }

    // In the air now.
    set_ground(mv, None);

    // `flGroundFactor` is the surface's `jumpFactor`, which needs the physics
    // surface-property database — `vphysics/`'s, and 1.0 for every surface
    // that has not overridden it.
    let mul = (2.0 * vars.gravity * JUMP_HEIGHT).sqrt();
    mv.velocity.z += mul;

    finish_gravity(mv, vars, dt);

    // Portal 2 sets `bSetDuckJump = false`, over a Valve comment reading "This
    // is set to false as a temp fix for camera snapping when ducking in the air
    // ( NO DUCKJUMP for now )". That one constant deletes the whole duck-jump
    // state machine — `m_nJumpTimeMsecs`, `m_bInDuckJump`, `StartUnDuckJump`,
    // `CanUnDuckJump`, `FinishUnDuckJump` and `UpdateDuckJumpEyeOffset` are all
    // unreachable in Portal 2, which is why none of them are here.

    // Don't jump again until released.
    mv.old_buttons = mv.old_buttons.insert(ButtonBits::JUMP);
    true
}

/// `CGameMovement::HandleDuckingSpeedCrop` (`gamemovement.cpp:4731`) — a ducked
/// player on the ground moves at a third speed, once per command.
fn handle_ducking_speed_crop(mv: &mut MoveData) {
    if !mv.speed_cropped && mv.ducked && mv.ground.is_some() {
        mv.forwardmove *= DUCK_SPEED_CROP;
        mv.sidemove *= DUCK_SPEED_CROP;
        mv.upmove *= DUCK_SPEED_CROP;
        mv.speed_cropped = true;
    }
}

/// `CGameMovement::SetDuckedEyeOffset` (`gamemovement.cpp:4707`).
///
/// **The fraction is splined twice.** Both callers pass
/// `SimpleSpline( fraction )` and this applies `SimpleSpline` again
/// (`:4710`). Ported as written: it is the shape of the shipped crouch, and
/// "fixing" it would change how a crouch looks for no stated reason.
fn set_ducked_eye_offset(mv: &mut MoveData, duck_fraction: f32) {
    let duck_fraction = simple_spline(duck_fraction);

    // `fMore` is the difference between the two hulls' minima, which is zero
    // for Portal 2's — both sit on the floor.
    let more = player_mins(true).z - player_mins(false).z;

    let ducked = player_view_offset(true).z - more;
    let standing = player_view_offset(false).z;
    mv.view_offset.z = ducked * duck_fraction + standing * (1.0 - duck_fraction);
}

/// The origin shift that keeps a player in the same place through a hull
/// change — `FinishDuck`/`FinishUnDuck`'s "HACKHACK - Fudge for collision bug".
///
/// On the ground the feet stay put and only the top of the box moves, so the
/// shift is zero (both hulls share their minimum). In the air the *head* stays
/// put instead, so crouching lifts the origin by the height difference.
fn duck_origin_shift(on_ground: bool) -> Vec3 {
    match on_ground {
        true => player_mins(true) - player_mins(false),
        false => {
            (player_maxs(false) - player_mins(false)) - (player_maxs(true) - player_mins(true))
        }
    }
}

/// `CGameMovement::CanUnduck` (`gamemovement.cpp:4493`) — is there room to
/// stand up?
fn can_unduck(mv: &MoveData, tracer: &mut Tracer<'_>) -> bool {
    let new_origin = mv.origin - duck_origin_shift(mv.ground.is_some());
    // Traced with the *standing* hull, which is the whole question.
    let trace = trace_hull(tracer, mv.origin, new_origin, false);
    !trace.start_solid && trace.fraction == 1.0
}

/// `CGameMovement::FinishDuck` (`gamemovement.cpp:4635`).
fn finish_duck(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars) {
    if mv.ducked {
        return;
    }
    mv.ducked = true;
    mv.ducking = false;
    mv.view_offset = player_view_offset(true);
    mv.origin += duck_origin_shift(mv.ground.is_some());

    // `FixPlayerCrouchStuck` is the nudge-out-of-a-wall pass; it needs
    // `CheckStuck`, which is not ported (§ "Not implemented").

    // Ducking can change the origin, so re-classify.
    categorize_position(mv, tracer, vars);
}

/// `CGameMovement::FinishUnDuck` (`gamemovement.cpp:4532`).
fn finish_unduck(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars) {
    mv.origin -= duck_origin_shift(mv.ground.is_some());
    mv.ducked = false;
    mv.ducking = false;
    mv.view_offset = player_view_offset(false);
    mv.duck_time_msecs = 0;

    categorize_position(mv, tracer, vars);
}

/// `CGameMovement::ReduceTimers` (`gamemovement.cpp:1244`) — the duck timer
/// counts **down** in whole milliseconds.
///
/// Whole milliseconds, from `(int)( 1000 * frametime )`: at 300 fps that
/// truncates to 3 and the crouch takes slightly longer in wall-clock time than
/// at 60 fps. Valve's, and visible only if you go looking.
fn reduce_timers(mv: &mut MoveData, dt: f32) {
    let frame_msec = (1000.0 * dt) as i32;
    if mv.duck_time_msecs > 0 {
        mv.duck_time_msecs = (mv.duck_time_msecs - frame_msec).max(0);
    }
}

/// `CGameMovement::Duck` (`gamemovement.cpp:4773`), with the duck-jump branches
/// removed because Portal 2 cannot reach them (see [`check_jump_button`]).
fn duck(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars) {
    let changed = mv.old_buttons.changed(mv.buttons);
    let pressed = changed.intersection(mv.buttons);
    let released = changed.intersection(mv.old_buttons);

    let in_air = mv.ground.is_none();
    let in_duck = mv.ducked;

    if mv.buttons.contains(ButtonBits::DUCK) {
        mv.old_buttons = mv.old_buttons.insert(ButtonBits::DUCK);
    } else {
        mv.old_buttons = mv.old_buttons.remove(ButtonBits::DUCK);
    }

    handle_ducking_speed_crop(mv);

    if !(mv.buttons.contains(ButtonBits::DUCK) || mv.ducking || in_duck) {
        // The eye-height restore hack (`:4963`) guards against a bug Valve
        // never reproduced; with no duck-jump there is nothing to leave the
        // eye stranded, so it is not ported.
        return;
    }

    if mv.buttons.contains(ButtonBits::DUCK) {
        // Duck button held but not yet ducked: start the transition.
        if pressed.contains(ButtonBits::DUCK) && !in_duck {
            mv.duck_time_msecs = DUCK_TIME_MSECS;
            mv.ducking = true;
        }

        if mv.ducking {
            let elapsed = (DUCK_TIME_MSECS - mv.duck_time_msecs).max(0);
            // Finish when the transition time is over, already ducked, or in
            // the air — a crouch in mid-air is instant.
            if elapsed > TIME_TO_DUCK_MSECS || in_duck || in_air {
                finish_duck(mv, tracer, vars);
            } else {
                let fraction = simple_spline(fraction_ducked(elapsed));
                set_ducked_eye_offset(mv, fraction);
            }
        }
        return;
    }

    // Unduck, or attempt to.
    if released.contains(ButtonBits::DUCK) {
        if in_duck {
            mv.duck_time_msecs = DUCK_TIME_MSECS;
        } else if mv.ducking && !mv.ducked {
            // Invert the time if released before fully ducked, so standing back
            // up takes as long as the part of the crouch that happened.
            let elapsed = DUCK_TIME_MSECS - mv.duck_time_msecs;
            let remaining = (fraction_ducked(elapsed) * TIME_TO_UNDUCK_MSECS as f32) as i32;
            mv.duck_time_msecs = DUCK_TIME_MSECS - TIME_TO_UNDUCK_MSECS + remaining;
        }
    }

    if can_unduck(mv, tracer) {
        if mv.ducking || mv.ducked {
            let elapsed = (DUCK_TIME_MSECS - mv.duck_time_msecs).max(0);
            if elapsed > TIME_TO_UNDUCK_MSECS || in_air {
                finish_unduck(mv, tracer, vars);
            } else {
                let fraction = simple_spline(1.0 - fraction_unducked(elapsed));
                set_ducked_eye_offset(mv, fraction);
                mv.ducking = true;
            }
        }
    } else if mv.duck_time_msecs != DUCK_TIME_MSECS {
        // Still under something. Reset the timer so we stand up the moment we
        // leave the tunnel rather than part-way through it.
        set_ducked_eye_offset(mv, 1.0);
        mv.duck_time_msecs = DUCK_TIME_MSECS;
        mv.ducked = true;
        mv.ducking = false;
    }
}

/// `FractionDucked` (`shareddefs.h:106`).
fn fraction_ducked(msecs: i32) -> f32 {
    (msecs as f32 / TIME_TO_DUCK_MSECS as f32).clamp(0.0, 1.0)
}

/// `FractionUnDucked` (`shareddefs.h:111`).
fn fraction_unducked(msecs: i32) -> f32 {
    (msecs as f32 / TIME_TO_UNDUCK_MSECS as f32).clamp(0.0, 1.0)
}

/// `CGameMovement::CheckParameters` (`gamemovement.cpp:1137`), for the parts
/// that have meaning without weapons, vehicles, constraints or death.
///
/// The speed clip is **skipped entirely for `MOVETYPE_NOCLIP`** (`:1140`),
/// which is the first reason noclip was a clean stage 1.
pub fn check_parameters(mv: &mut MoveData) {
    if mv.move_type != MoveType::Noclip {
        let spd =
            mv.forwardmove * mv.forwardmove + mv.sidemove * mv.sidemove + mv.upmove * mv.upmove;
        if spd != 0.0 && spd > mv.max_speed * mv.max_speed {
            let ratio = mv.max_speed / spd.sqrt();
            mv.forwardmove *= ratio;
            mv.sidemove *= ratio;
            mv.upmove *= ratio;
        }
    }

    // `CalcRoll` is `sv_rollangle`, which is 0 in this branch, so the roll is
    // zero either way — and it is forced to zero outright for noclip (`:1224`).
    // A rolled *view* must not roll the *movement* basis.
    mv.angles.roll = 0.0;
}

/// `CGameMovement::FullNoClipMove` (`gamemovement.cpp:2525`).
///
/// Four details that look like mistakes and are not:
///
/// - **`max_speed` is computed from the unhalved factor**, before `+speed`
///   halves it, so walking never reaches the clamp.
/// - **`upmove` goes on world `+Z`**, added after the forward/right terms, so
///   looking down does not tilt which way "up" is.
/// - **A velocity under one unit per second stops the player and returns
///   early**, skipping the position update for that frame.
/// - **A negative `sv_noclipaccelerate` zeroes the velocity after moving**,
///   which is the "no accel" mode; zero takes the straight-to-`wishvel` branch
///   instead. Three behaviours from one float.
pub fn full_noclip_move(mv: &mut MoveData, vars: &MoveVars, dt: f32) {
    let mut factor = vars.noclipspeed;
    let max_speed = mv.max_speed * factor;

    let (forward, right, _) = mv.angles.vectors();

    if mv.buttons.contains(ButtonBits::SPEED) {
        factor /= 2.0;
    }

    let fmove = mv.forwardmove * factor;
    let smove = mv.sidemove * factor;

    // `AngleVectors` already returns unit vectors; Valve normalizes anyway and
    // so does this, because the day something hands over a scaled basis is the
    // day the movement speed changes for no visible reason.
    let forward = forward.normalize_or_zero();
    let right = right.normalize_or_zero();

    let mut wishvel = forward * fmove + right * smove;
    wishvel.z += mv.upmove * factor;

    let mut wishspeed = wishvel.length();
    let wishdir = wishvel.normalize_or_zero();

    // Clamp to the server-defined max speed.
    if wishspeed > max_speed {
        wishvel *= max_speed / wishspeed;
        wishspeed = max_speed;
    }

    if vars.noclipaccelerate > 0.0 {
        accelerate(mv, wishdir, wishspeed, vars.noclipaccelerate, dt);

        let speed = mv.velocity.length();
        if speed < 1.0 {
            mv.velocity = Vec3::ZERO;
            return;
        }

        // Bleed off some speed, but if we have less than the bleed threshold,
        // bleed the threshold amount.
        let control = match speed < max_speed / 4.0 {
            true => max_speed / 4.0,
            false => speed,
        };
        let drop = control * vars.friction * dt;
        let newspeed = (speed - drop).max(0.0);
        mv.velocity *= newspeed / speed;
    } else {
        mv.velocity = wishvel;
    }

    // Just move — don't clip or anything.
    mv.origin += mv.velocity * dt;

    if vars.noclipaccelerate < 0.0 {
        mv.velocity = Vec3::ZERO;
    }
}

/// `CPortalGameMovement::FullWalkMove` (`portal_gamemovement.cpp:3877`).
///
/// The order is the whole function, and every line of it is load-bearing:
/// gravity is applied in two halves either side of the move; friction runs
/// *before* the move so that a player standing still on a conveyor does not
/// slow relative to it; and `CategorizePosition` runs after the move so the
/// next frame knows whether there is ground.
pub fn full_walk_move(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars, dt: f32) {
    start_gravity(mv, vars, dt);

    // The water branch (`CheckWater`, `WaterMove`, `WaterJump`) is not ported —
    // see the module's "Not implemented". A Portal 2 player is never in water
    // without also being dead, and the goo is a trigger rather than a fluid.

    if mv.buttons.contains(ButtonBits::JUMP) {
        check_jump_button(mv, vars, dt);
    } else {
        mv.old_buttons = mv.old_buttons.remove(ButtonBits::JUMP);
    }

    // Friction is handled before we add in any base velocity, so that a player
    // standing still on a conveyor does not slow relative to it.
    if mv.ground.is_some() {
        mv.velocity.z = 0.0;
        friction(mv, tracer, vars, dt);
    }

    check_velocity(mv, vars);

    if mv.ground.is_some() {
        walk_move(mv, tracer, vars, dt);
    } else {
        air_move(mv, tracer, vars, dt);
    }

    categorize_position(mv, tracer, vars);
    check_velocity(mv, vars);
    finish_gravity(mv, vars, dt);

    if mv.ground.is_some() {
        mv.velocity.z = 0.0;
    }

    // `CheckFalling` is fall damage, the landing sound and the landing
    // animation — none of which exist.
}

/// `CGameMovement::PlayerMove` (`gamemovement.cpp:4994`) — the per-command
/// entry point, and the order everything else runs in.
pub fn player_move(mv: &mut MoveData, tracer: Option<&mut Tracer<'_>>, vars: &MoveVars, dt: f32) {
    check_parameters(mv);
    reduce_timers(mv, dt);

    // `CheckStuck` is skipped for noclip anyway, and is not ported.

    let Some(tracer) = tracer else {
        // No map loaded: noclip still flies, walking has nothing to stand on.
        if mv.move_type == MoveType::Noclip {
            full_noclip_move(mv, vars, dt);
        }
        return;
    };

    // `sv_optimizedmovement` is 1, so a walking player skips the opening
    // `CategorizePosition` and gets this cheap test instead — the first real
    // classification of the frame happens inside `full_walk_move`.
    if mv.move_type != MoveType::Walk {
        categorize_position(mv, tracer, vars);
    } else if mv.velocity.z > 250.0 {
        set_ground(mv, None);
    }

    duck(mv, tracer, vars);

    match mv.move_type {
        MoveType::Noclip => full_noclip_move(mv, vars, dt),
        MoveType::Walk => full_walk_move(mv, tracer, vars, dt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::trace::fixture::Fixture;
    use crate::engine::trace::CollisionBsp;

    const TICK: f32 = 1.0 / 60.0;

    /// A room to walk in: a floor at `z = 0`, a 16-unit step at `x >= 200`
    /// (under [`SV_STEPSIZE`], so walkable), a tall wall at `x >= 600`, and a
    /// ceiling 40 units up over `x < -500` — high enough for a ducked player
    /// (36) and not a standing one (72).
    fn room() -> CollisionBsp {
        let mut fixture = Fixture::default();
        let mut solid = |mins: Vec3, maxs: Vec3| {
            fixture.add_box(mins, maxs, Contents::SOLID, true);
        };
        // Floor.
        solid(
            Vec3::new(-1000.0, -1000.0, -100.0),
            Vec3::new(1000.0, 1000.0, 0.0),
        );
        // A step up.
        solid(
            Vec3::new(200.0, -1000.0, 0.0),
            Vec3::new(1000.0, 1000.0, 16.0),
        );
        // A wall nothing can climb.
        solid(
            Vec3::new(600.0, -1000.0, 0.0),
            Vec3::new(700.0, 1000.0, 500.0),
        );
        // A low ceiling to crouch under.
        solid(
            Vec3::new(-1000.0, -1000.0, 40.0),
            Vec3::new(-500.0, 1000.0, 140.0),
        );
        fixture.single_leaf()
    }

    /// A walking player at `origin`, facing `+X`, holding nothing.
    fn walker(origin: Vec3) -> MoveData {
        MoveData {
            origin,
            velocity: Vec3::ZERO,
            angles: ViewAngles::new(0.0, 0.0),
            forwardmove: 0.0,
            sidemove: 0.0,
            upmove: 0.0,
            buttons: ButtonBits::NONE,
            old_buttons: ButtonBits::NONE,
            max_speed: SV_SPEED_NORMAL,
            move_type: MoveType::Walk,
            ground: None,
            surface_friction: 1.0,
            ducked: false,
            ducking: false,
            duck_time_msecs: 0,
            view_offset: VEC_VIEW,
            speed_cropped: false,
        }
    }

    /// Runs `frames` commands, letting the caller fill each one in.
    ///
    /// The per-command fields are **cleared before every frame**, because
    /// `Client::run_move` builds a fresh `MoveData` from each `UserCmd` and a
    /// harness that let `forwardmove` persist would be testing a player holding
    /// a key they had released.
    fn run(
        mv: &mut MoveData,
        world: &CollisionBsp,
        frames: usize,
        dt: f32,
        mut fill: impl FnMut(&mut MoveData),
    ) {
        let mut tracer = world.tracer();
        for _ in 0..frames {
            mv.forwardmove = 0.0;
            mv.sidemove = 0.0;
            mv.upmove = 0.0;
            mv.buttons = ButtonBits::NONE;
            mv.speed_cropped = false;
            fill(mv);
            player_move(mv, Some(&mut tracer), &MoveVars::PORTAL2, dt);
        }
    }

    /// Drops the player onto the floor and leaves them standing on it.
    fn settled(world: &CollisionBsp) -> MoveData {
        let mut mv = walker(Vec3::new(0.0, 0.0, 20.0));
        run(&mut mv, world, 40, TICK, |_| {});
        assert!(mv.ground.is_some(), "the fixture starts on the ground");
        mv
    }

    #[test]
    fn a_player_falls_until_it_lands_on_the_floor() {
        let world = room();
        let mut mv = walker(Vec3::new(0.0, 0.0, 200.0));
        assert!(mv.ground.is_none());

        run(&mut mv, &world, 120, TICK, |_| {});

        assert!(mv.ground.is_some(), "landed: {mv:?}");
        assert_eq!(mv.ground, Some(Vec3::Z), "on a flat floor");
        assert!(mv.origin.z.abs() < 0.1, "at the floor: {}", mv.origin.z);
        assert_eq!(mv.velocity.z, 0.0, "and not still falling");
    }

    /// Gravity is applied in two halves either side of the move, so one frame
    /// of free fall from rest is a whole frame's worth of acceleration.
    #[test]
    fn gravity_is_six_hundred_a_second_squared() {
        let world = room();
        let mut mv = walker(Vec3::new(0.0, 0.0, 500.0));
        run(&mut mv, &world, 1, TICK, |_| {});
        assert!(
            (mv.velocity.z + SV_GRAVITY * TICK).abs() < 0.01,
            "{}",
            mv.velocity.z
        );
    }

    /// A fall lands in the same place whatever the frame rate — which is the
    /// entire reason gravity is split into halves.
    #[test]
    fn the_landing_is_the_same_at_any_frame_rate() {
        let world = room();
        let mut fast = walker(Vec3::new(0.0, 0.0, 200.0));
        run(&mut fast, &world, 600, 1.0 / 300.0, |_| {});

        let mut slow = walker(Vec3::new(0.0, 0.0, 200.0));
        run(&mut slow, &world, 40, 1.0 / 20.0, |_| {});

        assert!(fast.ground.is_some() && slow.ground.is_some());
        assert!(fast.origin.z.abs() < 0.1 && slow.origin.z.abs() < 0.1);
    }

    #[test]
    fn walking_forward_settles_at_the_ground_speed() {
        let world = room();
        let mut mv = settled(&world);
        run(&mut mv, &world, 120, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        let speed = mv.velocity.truncate().length();
        assert!(
            (speed - SV_SPEED_NORMAL).abs() < 1.0,
            "walks at sv_speed_normal, not sv_maxspeed: {speed}"
        );
        assert!(
            mv.origin.x > 100.0,
            "and actually travelled: {}",
            mv.origin.x
        );
    }

    /// Friction stops a walk rather than letting it coast for ever, and the
    /// stop is exact.
    #[test]
    fn releasing_forward_stops_the_player() {
        let world = room();
        let mut mv = settled(&world);
        run(&mut mv, &world, 60, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });
        assert!(mv.velocity.length() > 100.0);

        run(&mut mv, &world, 120, TICK, |_| {});
        assert!(
            mv.velocity.length() < 1.0,
            "coasted to a stop: {}",
            mv.velocity.length()
        );
    }

    /// A 16-unit step is under `sv_stepsize`, so walking into it climbs it —
    /// this is `StepMove`, and it is the difference between a staircase and a
    /// wall.
    #[test]
    fn a_step_shorter_than_sv_stepsize_is_walked_up() {
        let world = room();
        let mut mv = settled(&world);
        mv.origin.x = 150.0;

        run(&mut mv, &world, 90, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        assert!(mv.origin.x > 210.0, "got past the step: {}", mv.origin.x);
        assert!(
            (mv.origin.z - 16.0).abs() < 0.2,
            "and is standing on top of it: {}",
            mv.origin.z
        );
        assert!(mv.ground.is_some());
    }

    /// ...and a 500-unit one is not.
    #[test]
    fn a_wall_taller_than_a_step_stops_the_player() {
        let world = room();
        let mut mv = settled(&world);
        mv.origin.x = 400.0;
        mv.origin.z = 16.0;

        run(&mut mv, &world, 180, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        assert!(mv.origin.x < 600.0, "stopped at the wall: {}", mv.origin.x);
        assert!(mv.origin.x > 550.0, "but did reach it: {}", mv.origin.x);
        assert!(
            (mv.origin.z - 16.0).abs() < 0.2,
            "and did not climb it: {}",
            mv.origin.z
        );
    }

    /// Walking into a wall at an angle slides along it rather than stopping
    /// dead — `TryPlayerMove`'s whole purpose.
    #[test]
    fn a_wall_hit_at_an_angle_is_slid_along() {
        let world = room();
        let mut mv = settled(&world);
        mv.origin = Vec3::new(400.0, 0.0, 16.0);
        // 45 degrees into the wall's face.
        mv.angles = ViewAngles::new(0.0, 45.0);

        run(&mut mv, &world, 120, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        assert!(mv.origin.x < 600.0, "did not pass the wall");
        assert!(
            mv.origin.y > 100.0,
            "and slid along it rather than stopping: {}",
            mv.origin.y
        );
    }

    /// Portal 2 jumps 45 units, not the base class's 21.
    #[test]
    fn a_jump_reaches_forty_five_units() {
        let world = room();
        let mut mv = settled(&world);

        let launch = (2.0 * SV_GRAVITY * JUMP_HEIGHT).sqrt();
        assert!((launch - 232.379).abs() < 0.01, "sqrt(2*600*45): {launch}");

        let mut peak: f32 = 0.0;
        for _ in 0..120 {
            run(&mut mv, &world, 1, TICK, |mv| {
                mv.buttons = ButtonBits::JUMP;
            });
            peak = peak.max(mv.origin.z);
        }

        // The half-frame of gravity `check_jump_button` applies on the way out
        // costs a little height, so the peak is just under the ideal 45.
        assert!(
            (43.0..45.5).contains(&peak),
            "peaked at {peak}, not the base class's ~21"
        );
    }

    /// Holding jump does not pogo: the button has to be released first.
    #[test]
    fn a_held_jump_button_does_not_bounce() {
        let world = room();
        let mut mv = settled(&world);

        run(&mut mv, &world, 1, TICK, |mv| mv.buttons = ButtonBits::JUMP);
        assert!(mv.velocity.z > 200.0, "jumped once");

        // Land, still holding.
        run(&mut mv, &world, 240, TICK, |mv| {
            mv.buttons = ButtonBits::JUMP
        });
        assert!(mv.ground.is_some(), "landed");
        assert_eq!(mv.velocity.z, 0.0, "and stayed down");

        // Release and press again.
        run(&mut mv, &world, 1, TICK, |mv| mv.buttons = ButtonBits::NONE);
        run(&mut mv, &world, 1, TICK, |mv| mv.buttons = ButtonBits::JUMP);
        assert!(mv.velocity.z > 200.0, "jumped again once released");
    }

    #[test]
    fn ducking_lowers_the_hull_and_the_eye() {
        let world = room();
        let mut mv = settled(&world);
        assert_eq!(mv.view_offset, VEC_VIEW);

        run(&mut mv, &world, 60, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK
        });

        assert!(mv.ducked, "{mv:?}");
        assert_eq!(mv.view_offset, VEC_DUCK_VIEW);
        assert!(
            mv.origin.z.abs() < 0.1,
            "and the feet stay on the floor: {}",
            mv.origin.z
        );

        run(&mut mv, &world, 60, TICK, |mv| {
            mv.buttons = ButtonBits::NONE
        });
        assert!(!mv.ducked, "stood back up");
        assert_eq!(mv.view_offset, VEC_VIEW);
    }

    /// The eye moves through the transition rather than snapping.
    #[test]
    fn the_eye_slides_down_through_a_crouch() {
        let world = room();
        let mut mv = settled(&world);
        run(&mut mv, &world, 6, TICK, |mv| mv.buttons = ButtonBits::DUCK);

        assert!(mv.ducking && !mv.ducked, "mid-transition: {mv:?}");
        let eye = mv.view_offset.z;
        assert!(
            eye < VEC_VIEW.z && eye > VEC_DUCK_VIEW.z,
            "between the two heights: {eye}"
        );
    }

    /// A ducked player fits under a 40-unit ceiling and cannot stand back up
    /// while under it — the `CanUnduck` trace.
    #[test]
    fn a_ducked_player_cannot_stand_up_under_a_low_ceiling() {
        let world = room();
        let mut mv = settled(&world);
        run(&mut mv, &world, 60, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK
        });
        assert!(mv.ducked);

        // Walk under the ceiling, still crouched. A ducked player moves at a
        // third speed, so this needs a running start rather than a long walk.
        mv.origin.x = -450.0;
        mv.angles = ViewAngles::new(0.0, 180.0);
        run(&mut mv, &world, 180, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK;
            mv.forwardmove = SV_SPEED_NORMAL;
        });
        assert!(mv.origin.x < -540.0, "got under it: {}", mv.origin.x);

        // Release duck: there is no room, so nothing happens.
        run(&mut mv, &world, 60, TICK, |_| {});
        assert!(mv.ducked, "still crouched under the ceiling");
        assert_eq!(mv.view_offset, VEC_DUCK_VIEW);
    }

    /// `HandleDuckingSpeedCrop` — a third speed, and only once per command.
    #[test]
    fn a_ducked_player_moves_at_a_third_speed() {
        let world = room();

        let mut standing = settled(&world);
        run(&mut standing, &world, 120, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        let mut ducked = settled(&world);
        run(&mut ducked, &world, 60, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK
        });
        assert!(ducked.ducked);
        run(&mut ducked, &world, 120, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK;
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        let fast = standing.velocity.truncate().length();
        let slow = ducked.velocity.truncate().length();
        assert!(
            (slow - fast / 3.0).abs() < 2.0,
            "a third of {fast}, got {slow}"
        );
    }

    /// Portal 2 refuses to jump while ducked; the base class jumps at a fixed
    /// speed instead.
    #[test]
    fn a_ducked_player_cannot_jump() {
        let world = room();
        let mut mv = settled(&world);
        run(&mut mv, &world, 60, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK
        });
        assert!(mv.ducked);

        run(&mut mv, &world, 1, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK.insert(ButtonBits::JUMP);
        });
        assert_eq!(mv.velocity.z, 0.0, "did not leave the ground");
        assert!(mv.ground.is_some());
    }

    /// Rising faster than `NON_JUMP_VELOCITY` means the player is not on the
    /// ground, whatever the trace under their feet says.
    #[test]
    fn rising_rapidly_loses_the_ground() {
        let world = room();
        let mut mv = settled(&world);
        let mut tracer = world.tracer();
        assert!(mv.ground.is_some());

        // Straight at `categorize_position`, which is where the test lives:
        // `full_walk_move` zeroes the vertical velocity of a grounded player
        // before anything else can see it, so the only way to arrive here
        // rising is to have already left the ground — which is what a jump
        // does one line before.
        mv.velocity.z = NON_JUMP_VELOCITY + 10.0;
        categorize_position(&mut mv, &mut tracer, &MoveVars::PORTAL2);
        assert!(mv.ground.is_none(), "{mv:?}");

        // Just under the threshold and the floor still counts.
        mv.velocity.z = NON_JUMP_VELOCITY - 10.0;
        categorize_position(&mut mv, &mut tracer, &MoveVars::PORTAL2);
        assert!(mv.ground.is_some(), "{mv:?}");
    }

    /// Air control is capped, so a player cannot turn a fling into a full-speed
    /// walk mid-air — but Portal's cap is 60, double the base class's 30.
    #[test]
    fn air_control_is_capped_at_sixty() {
        let mut mv = walker(Vec3::new(0.0, 0.0, 500.0));
        mv.velocity = Vec3::new(0.0, 0.0, 0.0);

        // One very long airborne frame, asking for full speed sideways.
        air_accelerate(&mut mv, Vec3::X, SV_SPEED_NORMAL, SV_AIRACCELERATE, 1.0);
        assert!(
            (mv.velocity.x - 60.0).abs() < 0.01,
            "capped at the wish speed, not the acceleration: {}",
            mv.velocity.x
        );
    }

    /// Edge friction doubles the friction when the player is walking towards a
    /// drop, which is what keeps a Portal 2 player from skating off ledges.
    #[test]
    fn edge_friction_slows_a_player_near_a_ledge() {
        // A floor that ends at x = 0, so walking towards +x is walking off it.
        let mut fixture = Fixture::default();
        fixture.add_box(
            Vec3::new(-1000.0, -1000.0, -100.0),
            Vec3::new(0.0, 1000.0, 0.0),
            Contents::SOLID,
            true,
        );
        let world = fixture.single_leaf();

        // The probe looks 16 units ahead with the player's own 32-wide hull, so
        // it clears the floor only once the *origin* is past the edge — which
        // is still standable, because the hull behind it overlaps the floor.
        // That 16-unit band is the whole of "walking off a ledge".
        let braked = |x: f32, use_edge: bool| {
            let mut mv = walker(Vec3::new(x, 0.0, 0.0));
            mv.ground = Some(Vec3::Z);
            mv.velocity = Vec3::X * 150.0;
            let mut vars = MoveVars::PORTAL2;
            vars.use_edgefriction = use_edge;
            let mut tracer = world.tracer();
            friction(&mut mv, &mut tracer, &vars, TICK);
            mv.velocity.length()
        };

        let over_the_edge = braked(8.0, true);
        let same_spot_disabled = braked(8.0, false);
        let well_inside = braked(-500.0, true);

        assert!(
            over_the_edge < same_spot_disabled - 5.0,
            "edge friction bit: {over_the_edge} against {same_spot_disabled}"
        );
        assert!(
            (well_inside - same_spot_disabled).abs() < 0.01,
            "and does nothing in the middle of the floor: {well_inside}"
        );
    }

    /// A walking player never ends a frame inside the world.
    #[test]
    fn walking_into_things_never_ends_inside_them() {
        let world = room();
        let mut mv = settled(&world);
        let mut tracer = world.tracer();

        for yaw in [0.0, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0] {
            mv.angles = ViewAngles::new(0.0, yaw);
            for _ in 0..60 {
                mv.forwardmove = SV_SPEED_NORMAL;
                mv.speed_cropped = false;
                player_move(&mut mv, Some(&mut tracer), &MoveVars::PORTAL2, TICK);

                let stuck = trace_player_bbox(&mv, &mut tracer, mv.origin, mv.origin);
                assert!(!stuck.start_solid, "stuck at {:?} facing {yaw}", mv.origin);
            }
        }
    }

    /// With no map there is nothing to stand on, and a walking player stays
    /// put rather than falling for ever.
    #[test]
    fn a_walking_player_without_a_map_does_not_move() {
        let mut mv = walker(Vec3::new(0.0, 0.0, 100.0));
        for _ in 0..60 {
            player_move(&mut mv, None, &MoveVars::PORTAL2, TICK);
        }
        assert_eq!(mv.origin, Vec3::new(0.0, 0.0, 100.0));
    }
}
