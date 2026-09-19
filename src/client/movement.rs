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

use glam::{Mat4, Vec3};

use super::player::{
    MoveType, VEC_DEAD_VIEWHEIGHT, VEC_DUCK_HULL_MAX, VEC_DUCK_HULL_MIN, VEC_DUCK_VIEW,
    VEC_HULL_MAX, VEC_HULL_MIN, VEC_VIEW,
};
use super::{ButtonBits, ViewAngles};
use crate::engine::trace::{CarvedWall, Contents, PortalHole, PortalHoles, Ray, Tracer};

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
/// `COS_PI_OVER_SIX` (`portal_gamemovement.cpp:105`) — cos 30°.
///
/// Two different questions use it and it is worth knowing they are the same
/// number: *is this portal on a floor* (`plane.normal.z > cos30`) and *does up
/// still look like up after going through this pair* (`|m[2][2]| < cos30`).
const COS_PI_OVER_SIX: f32 = 0.866_025_4;

/// `PLAYER_FLING_HELPER_MIN_SPEED` (`portal_gamemovement.cpp:103`) — how fast
/// you have to be leaving an upward-facing portal for the game to decide you
/// are being flung and keep you crouched.
const PLAYER_FLING_HELPER_MIN_SPEED: f32 = 200.0;

/// `portal_player_interaction_quadtest_epsilon`
/// (`portal_gamemovement.cpp:73`) — `-DIST_EPSILON`, and the comment above the
/// original says exactly that.
const QUADTEST_EPSILON: f32 = -DIST_EPSILON;

/// The minimum speed a **player** leaves a portal on the floor at —
/// `CProp_Portal::GetMinimumExitSpeed` (`prop_portal_shared.cpp:201`).
///
/// At zero every fling in the game dies on the exit, which is why this is one
/// of the numbers `portdocs/PORTAL.md` §9 calls out.
const EXIT_SPEED_MIN_FLOOR: f32 = 300.0;

/// `CProp_Portal::GetMaximumExitSpeed` (`prop_portal_shared.cpp:267`), which
/// is a flat 1000 and asks none of its four arguments.
const EXIT_SPEED_MAX: f32 = 1000.0;

/// `sv_paintairacceleration` (`portal_gamemovement.cpp:58`) — **the air
/// acceleration a Portal 2 player actually gets, paint or no paint.**
///
/// `CGameMovement::AirMove` passes `sv_airaccelerate`
/// (`gamemovement.cpp:2043`); `CPortalGameMovement::AirMove` passes *this*
/// (`:800`), unconditionally and with no paint anywhere in the branch. The
/// name is the only thing about it that is about paint, and taking the name at
/// face value gives a player 2.4x the air control the shipped game gives them.
///
/// [`SV_AIRACCELERATE`] is still the right number and is still what
/// [`MoveVars`] carries, because `FullTossMove` and anything else that
/// accelerates in air uses it; this one is `AirMove`'s alone.
pub const SV_PAINTAIRACCELERATION: f32 = 5.0;

/// `MIN_FLING_SPEED` (`portal_shareddefs.h:38`) — the horizontal speed above
/// which `AirMove` stops the player steering *against* their own momentum.
///
/// *"Don't let the player screw their fling because of adjusting into a floor
/// portal"*: past this, a wish direction that opposes the velocity on either
/// horizontal axis is zeroed on that axis. It is also the gate on the funnel,
/// which only runs *below* it.
const MIN_FLING_SPEED: f32 = 300.0;

/// `PORTAL_FUNNEL_AMOUNT` (`portal_gamemovement.cpp:81`) — how hard the funnel
/// pulls, as a multiplier on the distance still to cover.
const PORTAL_FUNNEL_AMOUNT: f32 = 6.0;

/// `sv_player_funnel_height_adjust` (`portal_gamemovement.cpp:50`) — how far
/// *above* a floor portal the funnel aims.
///
/// Subtracted from the drop before the time-to-impact is solved, so the funnel
/// finishes centring the player 128 units up rather than at the lip, which is
/// what stops it still correcting as they go through.
const FUNNEL_HEIGHT_ADJUST: f32 = 128.0;

/// `sv_player_funnel_speed_bonus` (`portal_gamemovement.cpp:48`) — the funnel
/// pulls up to this much harder the faster the player is falling.
const FUNNEL_SPEED_BONUS: f32 = 2.0;

/// `sv_player_funnel_snap_threshold` (`portal_gamemovement.cpp:47`) — below
/// this much horizontal speed, a player who is already going to arrive
/// centred is simply stopped on that axis instead of decayed.
const FUNNEL_SNAP_THRESHOLD: f32 = 10.0;

/// *"Apply slightly more gravity on exit so that floor/floor portals trend
/// towards decaying velocity. 1.008 is a magic number found through
/// experimentation."* (`portal_gamemovement.cpp:2441`)
///
/// At 1.0 an infinite floor-to-floor fall gains height every cycle.
const EXIT_GRAVITY_BOOST: f32 = 1.008;

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
    /// `player->m_iHealth` — see [`is_dead`].
    pub health: i32,
    /// `player->GetFlags() & FL_FROZEN`.
    pub frozen: bool,

    /// `player->GetGroundEntity()`, reduced to what a world-only port can say:
    /// the normal of the plane underfoot, or `None` for airborne.
    ///
    /// The entity itself is what Valve stores, and it is what conveyor and
    /// platform velocity would come from.
    pub ground: Option<Vec3>,
    /// `player->GetBaseVelocity()` — the velocity of whatever is carrying the
    /// player, added for the duration of a move and taken back out.
    ///
    /// **Written by the *server*, not by anything here**: a `trigger_push`
    /// sets it every tick it is pushing, and `CPlayerMove::CheckMovingGround`
    /// turns it into real velocity the tick after the push stops. It reaches
    /// this struct through [`PlayerState`](crate::server::PlayerState), which
    /// `Engine::frame` copies both ways.
    ///
    /// The one thing done to it here is
    /// [`start_gravity`]'s: gravity takes the vertical component and zeroes
    /// it, so a push straight up is spent once rather than fighting gravity
    /// for ever.
    pub base_velocity: Vec3,
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

    /// `m_vMoveStartPosition` (`portal_gamemovement.h:156`) — where the feet
    /// were before this move ran.
    ///
    /// Written by [`player_move`] on the way in, so no caller has to remember
    /// to. [`handle_portalling`] is the only reader: the teleport is decided
    /// by comparing where the move *started* with where it ended, not by where
    /// the player is now.
    pub move_start: Vec3,
    /// `m_hPortalEnvironment` — the portal whose carved geometry this player
    /// is being traced against.
    ///
    /// Decided at the end of each move by [`handle_portalling`] and consumed
    /// at the start of the next one, by the caller, to pick the
    /// [`CarvedWall`] it attaches to the tracer. That one-move lag is Valve's
    /// and is why the field is networked state rather than a local: the trace
    /// has to agree with the environment the *previous* move ended in, or a
    /// player crossing the plane is traced against the world they have already
    /// left.
    pub portal_environment: Option<u64>,
    /// Set when this move ended in a teleport — see [`Teleport`].
    ///
    /// Reset to `None` at the top of every [`player_move`], so a caller reads
    /// it after the call and never has to clear it.
    pub teleported: Option<Teleport>,
}

/// What a teleport did, for the caller to finish.
///
/// Everything `HandlePortalling` does to the *movement* it does in place —
/// the origin, the velocity, the hull. What it cannot do here is the
/// **angles**: `client::ViewAngles` is not in [`MoveData`], because
/// `CPlayerMove::FinishMove` does not write `mv->m_vecAngles` back either
/// (`player_command.cpp:232`, commented out in the original). So the transform
/// comes out and the caller composes it — which is `portdocs/PORTAL.md` §6.5's
/// "the whole block of angle plumbing collapses to a single compose", because
/// this port has one angle set where Valve has four.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Teleport {
    /// `m_matrixThisToLinked` of the portal that was entered. Compose an angle
    /// set with it through `crate::math::angle_matrix` and read the result
    /// back with `crate::math::matrix_angles`.
    pub matrix: Mat4,
    /// The portal that was entered.
    pub entered: u64,
    /// The portal that was left — the same value
    /// [`portal_environment`](MoveData::portal_environment) now holds.
    pub exit: u64,
    /// Whether the transition forced the player into the duck hull.
    pub forced_duck: bool,
}

impl Teleport {
    /// An angle set taken through the portal — `UTIL_Portal_AngleTransform`
    /// (`portal_util_shared.cpp:1516`).
    ///
    /// Compose the matrix with the angles' own rotation and read the result
    /// back out. Not a component-wise fix-up of yaw: a portal pair can turn
    /// all three at once, and the only way to get that right is to go through
    /// a matrix.
    ///
    /// **No pitch clamp.** The composed angles are wherever the pair put them
    /// and `ApplyMouse` clamps on the next command, which is Valve's order.
    pub fn turn(&self, angles: ViewAngles) -> ViewAngles {
        let rotation = glam::Mat3::from_mat4(self.matrix)
            * crate::math::angle_matrix(Vec3::new(angles.pitch, angles.yaw, angles.roll));
        let turned = crate::math::matrix_angles(rotation);
        let mut out = ViewAngles {
            pitch: turned.x,
            yaw: turned.y,
            roll: turned.z,
        };
        out.normalize();
        out
    }
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
        // *"can't hit a steep slope that we can't stand on anyway"* — unless
        // it is a portal transition ramp, which is exactly a slope too steep
        // to stand on that the player has to be able to walk out of.
        && (trace.normal.z >= CRITICAL_SLOPE || trace.hit_portal_ramp(Vec3::Z))
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
    if trace.normal.z < CRITICAL_SLOPE && !trace.hit_portal_ramp(Vec3::Z) {
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

    // **Everything from here to the end of the function moves at
    // `velocity + base_velocity`**, and every exit puts the base back out.
    // That is why a player standing still on a conveyor is carried and still
    // reports a velocity of zero.
    mv.velocity += mv.base_velocity;

    let spd = mv.velocity.length();
    if spd < 1.0 {
        // Valve zeroes the velocity and *then* subtracts, leaving
        // `-base_velocity` rather than zero (`portal_gamemovement.cpp:3785`).
        // Reproduced: with a base velocity this small the two differ by less
        // than a unit a second, and "fixed" it would be the only exit of the
        // seven that does not round-trip.
        mv.velocity = Vec3::ZERO;
        mv.velocity -= mv.base_velocity;
        return;
    }

    // First try just moving to the destination.
    let dest = mv.origin + mv.velocity * dt;
    let pm = trace_player_bbox(mv, tracer, mv.origin, dest);

    if pm.fraction == 1.0 {
        mv.origin = pm.end;
        mv.velocity -= mv.base_velocity;
        stay_on_ground(mv, tracer, vars);
        return;
    }

    // Don't walk up stairs if not on ground.
    if old_ground.is_none() {
        mv.velocity -= mv.base_velocity;
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

    mv.velocity -= mv.base_velocity;
    stay_on_ground(mv, tracer, vars);
}

/// `CPortalGameMovement::AirMove` (`portal_gamemovement.cpp:706`) — **not the
/// base class's**, which this used to be.
///
/// Three things Portal's override does that `CGameMovement::AirMove`
/// (`gamemovement.cpp:2006`) does not, and none of them needs paint:
///
/// 1. **It accelerates at [`SV_PAINTAIRACCELERATION`]**, 5.0 against
///    `sv_airaccelerate`'s 12.0. The constant's name is about paint; its use
///    is not conditional on anything.
/// 2. **A fling is not steerable against itself.** Above
///    [`MIN_FLING_SPEED`] horizontally, a wish direction opposing the velocity
///    on the x or y axis is zeroed on that axis — *"don't let the player screw
///    their fling because of adjusting into a floor portal"*.
/// 3. **Below that speed it funnels** ([`portal_funnel`]), which is what makes
///    a fall into a floor portal go in rather than clip the rim.
///
/// The fourth difference is one the port cannot reproduce and does not need
/// to: Valve leaves the view forward *unnormalised* when it is steeper than
/// 30 degrees from horizontal, *"to prevent the player from screwing up their
/// momentum after exiting floor portals or jumping off sticky ceilings while
/// looking straight up/down"*. That is exactly what this does — projecting
/// onto the horizontal plane and **not** renormalising shortens the movement
/// basis as the player looks further up or down, which is the intended
/// damping — so the two branches are written out rather than collapsed.
fn air_move(
    mv: &mut MoveData,
    tracer: &mut Tracer<'_>,
    holes: &PortalHoles,
    vars: &MoveVars,
    dt: f32,
) {
    let (forward, right, _) = mv.angles.vectors();
    // Looking mostly straight forward? Flatten and renormalise. Looking
    // steeply up or down? Flatten and **leave it short**.
    let forward = match forward.z < 0.5 && forward.z > -0.5 {
        true => Vec3::new(forward.x, forward.y, 0.0).normalize_or_zero(),
        false => Vec3::new(forward.x, forward.y, 0.0),
    };
    let right = Vec3::new(right.x, right.y, 0.0).normalize_or_zero();

    let mut wishdir = forward * mv.forwardmove + right * mv.sidemove;
    wishdir.z = 0.0;

    let mut funnel = Vec3::ZERO;
    let horizontal = mv.velocity.x * mv.velocity.x + mv.velocity.y * mv.velocity.y;
    if horizontal > MIN_FLING_SPEED * MIN_FLING_SPEED {
        // Cancel only the component that fights the fling, and only past half
        // the threshold — so a player drifting sideways at 100 can still
        // correct, and one committed at 200 cannot undo it.
        for axis in 0..2 {
            if mv.velocity[axis] > MIN_FLING_SPEED * 0.5 && wishdir[axis] < 0.0 {
                wishdir[axis] = 0.0;
            } else if mv.velocity[axis] < -MIN_FLING_SPEED * 0.5 && wishdir[axis] > 0.0 {
                wishdir[axis] = 0.0;
            }
        }
    } else {
        // `sv_player_funnel_into_portals` is 1.
        funnel = portal_funnel(mv, holes, wishdir, vars, dt);
    }

    // `IsSuppressingAirControl` — bounce and speed gel, and the tractor beam.
    // No paint, so the wish direction is never nuked; the funnel is added
    // *after* the point where it would have been, which is the whole reason
    // the two are separate vectors. Valve's comment: *"we still want to
    // funnel, even if the player isnt allowed to move themself"*.
    wishdir += funnel;

    let mut wishspeed = wishdir.length();
    let wishdir = wishdir.normalize_or_zero();
    // **`VectorScale( targetVel, … )` on the line above this in the original
    // writes to a vector nothing reads again** — `targetVel` is dead from the
    // moment `wishdir` is copied out of it. What the clamp actually does is
    // cap the speed, and that is all this does.
    if wishspeed != 0.0 && wishspeed > mv.max_speed {
        wishspeed = mv.max_speed;
    }

    air_accelerate(mv, wishdir, wishspeed, SV_PAINTAIRACCELERATION, dt);

    mv.velocity += mv.base_velocity;
    try_player_move(mv, tracer, dt, None);
    mv.velocity -= mv.base_velocity;
}

/// `RemapValClamped` (`public/mathlib/mathlib.h:1035`).
fn remap_clamped(value: f32, from: (f32, f32), to: (f32, f32)) -> f32 {
    if from.0 == from.1 {
        // `fsel( val - B, D, C )` — **zero takes `D`**, because `fsel` selects
        // on `>= 0`.
        return match value >= from.1 {
            true => to.1,
            false => to.0,
        };
    }
    let fraction = ((value - from.0) / (from.1 - from.0)).clamp(0.0, 1.0);
    to.0 + (to.1 - to.0) * fraction
}

/// `ExponentialDecay( halflife, dt )` (`public/mathlib/mathlib.h:1603`) — the
/// factor a value is multiplied by to lose half of itself every `halflife`
/// seconds.
fn exponential_decay(halflife: f32, dt: f32) -> f32 {
    (-0.693_147_2 / halflife * dt).exp()
}

/// `CPortalGameMovement::IsInPortalFunnelVolume`
/// (`portal_gamemovement.cpp:811`) — is the player inside the cone that opens
/// out of this portal?
///
/// The portal's own right and up, **re-orthogonalised against its plane
/// normal** and renormalised, and the player's offset measured against each.
/// For a rigid basis the re-orthogonalisation is a no-op; it is here because
/// it is there, and because a hand-built placement need not be rigid.
///
/// Valve compares `(offset · axis * axis).LengthSqr()` against `extent²`,
/// which is the square of the *scalar* projection by a longer route.
fn is_in_portal_funnel_volume(
    to_portal: Vec3,
    hole: &PortalHole,
    extent_x: f32,
    extent_y: f32,
) -> bool {
    let flatten = |axis: Vec3| (axis - axis.dot(hole.forward) * hole.forward).normalize_or_zero();
    let across = to_portal.dot(flatten(hole.right));
    if across * across > extent_x * extent_x {
        return false;
    }
    let along = to_portal.dot(flatten(hole.up));
    along * along <= extent_y * extent_y
}

/// `CPortalGameMovement::PlayerShouldFunnel` (`portal_gamemovement.cpp:839`) —
/// should the player be pulled towards the middle of this portal?
///
/// **This is `IsFloorPortal`'s one remaining consumer on the player's path.**
/// The other three are `TeleportTouchingEntity`'s floor-to-floor special
/// cases, and `CPortal_Base2D::Touch`, `StartTouch` and `EndTouch` all return
/// immediately for a player, so the player never reaches them; the fourth is
/// the punch guard, which is `server/`'s.
///
/// Five conditions in the air, in Valve's order:
///
/// - the player is not steering hard sideways (`|wishdir| > 64` on either
///   horizontal axis kills it), **and** is either rising fast at a ceiling
///   portal or falling fast while looking down at a floor one — *"we are more
///   liberal about funneling into a ceiling portal … we aren't going to be
///   hitting these by accident"*;
/// - the portal faces the right way for that direction;
/// - it is within 1,024 units and on the side the player is heading, and for
///   a ceiling portal within the height the player's rise can still reach;
/// - the player is inside a cone that widens from 1.5x the portal's own size
///   at 256 units to 3x at 1,024;
///
/// and the **ground** branch is deleted with a reason:
/// `speed_funnelling_enabled` gates it on `player->MaxSpeed() >
/// sv_speed_normal`, which in Portal 2 means speed gel. There is no paint, so
/// `MaxSpeed()` is [`SV_SPEED_NORMAL`] exactly and the branch's first line
/// returns `false` every time.
fn player_should_funnel(
    mv: &MoveData,
    hole: &PortalHole,
    look: Vec3,
    wishdir: Vec3,
    vars: &MoveVars,
) -> bool {
    if mv.ground.is_some() {
        return false;
    }
    let funnel_up = mv.velocity.z > 165.0;
    if (wishdir.x.abs() > 64.0 || wishdir.y.abs() > 64.0)
        || !(funnel_up || (look.z < -0.7 && mv.velocity.z < -165.0))
    {
        return false;
    }

    // **`IsCeilingPortal` is not the mirror of `IsFloorPortal`, and the
    // comment above this test in the original claims it is.** Both take the
    // same default threshold and both compare against it directly —
    // `vForward.z > 0.8` for a floor portal and `vForward.z < 0.8` for a
    // ceiling one (`portal_base2d_shared.cpp:879`, `:884`) — so *every*
    // portal that is not in the floor is a "ceiling portal", a wall portal
    // included. What that means here is that the rising case funnels into a
    // wall portal overhead as well as into one in the ceiling, where
    // Valve's comment says *"make sure it's a floor or ceiling portal"*.
    // Ported as written: the other four conditions still have to pass, and
    // "fix" it and a fling at a high wall portal stops being helped.
    let to_portal = hole.world_center() - player_center(mv);
    let is_floor = hole.forward.z > 0.8;
    let is_ceiling = hole.forward.z < 0.8;
    if (funnel_up && !is_ceiling) || (!funnel_up && !is_floor) {
        return false;
    }

    // How high the player can still rise, from where they are.
    let peak = (mv.velocity.z * mv.velocity.z) / (2.0 * vars.gravity);
    let out_of_reach = match funnel_up {
        true => to_portal.z > 1024.0 || to_portal.z <= 0.0 || to_portal.z > peak,
        false => to_portal.z < -1024.0 || to_portal.z >= 0.0,
    };
    if out_of_reach {
        return false;
    }

    let cone = remap_clamped(to_portal.z.abs(), (256.0, 1024.0), (1.5, 3.0));
    is_in_portal_funnel_volume(
        to_portal,
        hole,
        hole.half_width * cone,
        hole.half_height * cone,
    )
}

/// `CPortalGameMovement::PortalFunnel` (`portal_gamemovement.cpp:909`) — pick
/// the nearest portal worth funnelling into and ask
/// [`air_portal_funnel`] for the push.
///
/// Returns the force to add to the wish direction; [`air_portal_funnel`] also
/// damps the velocity directly, which is the half of the effect that makes the
/// player *stop* drifting once they are going to arrive centred.
fn portal_funnel(
    mv: &mut MoveData,
    holes: &PortalHoles,
    wishdir: Vec3,
    vars: &MoveVars,
    dt: f32,
) -> Vec3 {
    let look = mv.angles.vectors().0;
    let center = player_center(mv);

    let mut best: Option<(PortalHole, Vec3, f32)> = None;
    for wall in holes.iter() {
        // `IsActivedAndLinked`.
        if wall.link().is_none() {
            continue;
        }
        let hole = wall.hole();
        let to_portal = hole.world_center() - center;
        let distance = to_portal.length_squared();
        if !player_should_funnel(mv, hole, look, wishdir, vars) {
            continue;
        }
        if best.is_none_or(|(_, _, nearest)| distance < nearest) {
            best = Some((*hole, to_portal, distance));
        }
    }
    let Some((_, to_portal, _)) = best else {
        return Vec3::ZERO;
    };

    // Only the air branch — see [`player_should_funnel`] for why the ground
    // one is unreachable without paint.
    let height = -to_portal.z - FUNNEL_HEIGHT_ADJUST;
    let extra = remap_clamped(mv.velocity.z, (0.0, 1065.0), (1.0, FUNNEL_SPEED_BONUS));

    // When do we hit the portal? `-g t²/2 + v t + h = 0`, written as
    // `SolveQuadratic( -g, 2v, 2h )`.
    let roots = solve_quadratic(-vars.gravity, 2.0 * mv.velocity.z, 2.0 * height);
    let Some((first, second)) = roots else {
        return Vec3::ZERO;
    };
    // *"flRoot1 > 0 ? flRoot1 : flRoot2"*, then the earlier of the two when
    // both are ahead — which is not the same as `min`, because a negative
    // first root must not be replaced by a negative second one.
    let mut time = match first > 0.0 {
        true => first,
        false => second,
    };
    if second < first && second >= 0.0 && first >= 0.0 {
        time = second;
    }

    air_portal_funnel(mv, to_portal, extra, time, dt)
}

/// `CPortalGameMovement::AirPortalFunnel` (`portal_gamemovement.cpp:981`) —
/// per horizontal axis, either pull towards the portal or damp what is already
/// there.
///
/// The question asked per axis is *"will I make it to the centre in time"*,
/// and the two answers are opposites: if not, add a force proportional to the
/// distance left; if so, **bleed the speed off**, because arriving centred and
/// still moving sideways means leaving centred and still moving sideways.
///
/// The decay's half-life comes from how far away the portal is — 0.01 seconds
/// at 128 units, 0.15 at 1,024 — so a distant portal corrects gently and a
/// close one snaps.
fn air_portal_funnel(mv: &mut MoveData, to_portal: Vec3, extra: f32, time: f32, dt: f32) -> Vec3 {
    let halflife = remap_clamped(to_portal.z.abs(), (128.0, 1024.0), (0.01, 0.15));
    let decay = exponential_decay(halflife, dt);

    let mut force = Vec3::ZERO;
    for axis in 0..2 {
        let velocity = mv.velocity[axis];
        // **A zero velocity is "will not make it"**, not "is already there":
        // Valve's guard is `if( mv->m_vecVelocity[i] )`, so a player with no
        // sideways speed at all gets the pull rather than the decay.
        let in_time = velocity != 0.0 && (to_portal[axis] / velocity) < time;
        if !in_time {
            force[axis] = to_portal[axis] * extra * PORTAL_FUNNEL_AMOUNT - velocity;
        } else if velocity.abs() > FUNNEL_SNAP_THRESHOLD {
            mv.velocity[axis] = velocity * decay;
        } else {
            mv.velocity[axis] = 0.0;
        }
    }
    force
}

/// `player->WorldSpaceCenter()` — the middle of the player's hull, where
/// [`MoveData::origin`] is their feet.
fn player_center(mv: &MoveData) -> Vec3 {
    mv.origin + (player_mins(mv.ducked) + player_maxs(mv.ducked)) * 0.5
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
    // "yes, this 0.5 looks wrong, but it's not" — and the base-velocity line
    // below it takes the *vertical* component of the push, spends it as a
    // velocity change and clears it, so an upward `trigger_push` is a single
    // impulse rather than a permanent anti-gravity field. The horizontal
    // component is left alone and is added and removed around the move.
    mv.velocity.z += mv.base_velocity.z * dt;
    mv.base_velocity.z = 0.0;
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

/// Is this what a player stands on? — the test
/// `CPortalGameMovement::CategorizePosition`, its four-quadrant retry,
/// `StepMove`'s step-down and `StayOnGround` all spell out in place.
///
/// `DidHit() && normal.z >= CRITICAL_SLOPE`, **or the portal transition
/// ramp** — see [`Trace::hit_portal_ramp`](crate::engine::trace::Trace::hit_portal_ramp),
/// which is how a slightly-angled portal transition stops presenting as an
/// unclimbable step.
///
/// Valve writes the two halves the other way up — *"was on ground, but now
/// suddenly am not"* is `!pm.m_pEnt || ((traceNormalAngle < flStandableAngle)
/// && !pm.HitPortalRamp(stickNormal))` (`portal_gamemovement.cpp:1320`) — and
/// `!pm.m_pEnt` is this port's `!did_hit()`, because a trace that hit nothing
/// has no entity and a trace that hit the world has one.
fn standable(trace: &crate::engine::trace::Trace) -> bool {
    trace.did_hit() && (trace.normal.z >= CRITICAL_SLOPE || trace.hit_portal_ramp(Vec3::Z))
}

/// `TracePlayerBBoxForGround` (`gamemovement.cpp:4049`) — retry the ground
/// trace with each quadrant of the hull, looking for a shallower slope one
/// corner of the player is standing on.
///
/// The fraction and endpoint of the *original* trace are restored on the way
/// out, "so we don't try to move the player down to the new floor and get stuck
/// on a leaning wall that the original trace hit first".
///
/// **Portal's version is the same four quadrants with
/// [`standable`] in place of the slope test** — all four of
/// `PortalTracePlayerBBoxForGround`'s comparisons are
/// `(flNormalCos >= 0.7f) || pm.HitPortalRamp( Vector( 0, 0, 1 ) )`
/// (`portal_gamemovement.cpp:2065`, `:2099`, `:2133`, `:2167`), and the world
/// up they pass is a literal rather than the stick normal.
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
        if standable(pm) {
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
pub fn check_parameters(mv: &mut MoveData, old_angles: ViewAngles) {
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

    // `if ( player->GetFlags() & FL_FROZEN || player->GetFlags() & FL_ONTRAIN
    // || IsDead() )` (`portal_gamemovement.cpp:2986`) — the move is zeroed and
    // **nothing else is**: the velocity survives, so a corpse that was falling
    // keeps falling and a frozen player standing on a lift still rides it.
    // `FL_ONTRAIN` has no source in this port; `func_tracktrain` is not ported.
    if mv.frozen || is_dead(mv) {
        mv.forwardmove = 0.0;
        mv.sidemove = 0.0;
        mv.upmove = 0.0;
    }

    // `if ( !IsDead() ) { v_angle = mv->m_vecAngles; … } else { mv->m_vecAngles
    // = mv->m_vecOldAngles; }` (`:2998`). **A dead player cannot look around**,
    // which is what makes the death camera hold still while the body slides.
    if is_dead(mv) {
        mv.angles = old_angles;
    }

    // `CalcRoll` is `sv_rollangle`, which is 0 in this branch, so the roll is
    // zero either way — and it is forced to zero outright for noclip (`:1224`).
    // A rolled *view* must not roll the *movement* basis.
    mv.angles.roll = 0.0;

    // "Set dead player view_offset" (`:3023`) — after the angles, and
    // unconditionally every command, so it survives a duck transition that was
    // in progress when the player died.
    if is_dead(mv) {
        mv.view_offset = VEC_DEAD_VIEWHEIGHT;
    }
}

/// `CGameMovement::IsDead` (`gamemovement.cpp:1091`) — `m_iHealth <= 0`.
///
/// > **It is the health, not the life state.** `CBaseEntity::IsAlive` asks
/// > about `m_lifeState`, and the two disagree for the single dispatch between
/// > `OnTakeDamage` subtracting the last point and `Event_Killed` running. The
/// > movement wants this one, and getting them the wrong way round leaves a
/// > corpse that can still walk for one frame.
pub fn is_dead(mv: &MoveData) -> bool {
    mv.health <= 0
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
pub fn full_walk_move(
    mv: &mut MoveData,
    tracer: &mut Tracer<'_>,
    holes: &PortalHoles,
    vars: &MoveVars,
    dt: f32,
) {
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
        air_move(mv, tracer, holes, vars, dt);
    }

    categorize_position(mv, tracer, vars);
    check_velocity(mv, vars);
    finish_gravity(mv, vars, dt);

    if mv.ground.is_some() {
        mv.velocity.z = 0.0;
    }

    // `CheckFalling` is the landing sound, the landing animation and **fall
    // damage** — and the last of those is a measurement rather than a gap:
    // `CPortalGameRules::FlPlayerFallDamage` is `{ return 0.0f; } //no fall
    // damage in portal` (`portal_gamerules.h:61`), and the multiplayer rules
    // agree in words (`portal_mp_gamerules.cpp:1463`: "No fall damage in
    // Portal!"). Nothing in Portal 2 can be killed by landing, whatever the
    // height, which is why 34 of the game's `trigger_hurt`s carry `DMG_FALL`:
    // the pit does the killing, not the fall.
}

/// `CGameMovement::PushEntity` (`gamemovement.cpp:3861`) — sweep the hull by
/// `push` and take whatever fraction of it is free.
///
/// Not `TryPlayerMove`: there is no clip-and-retry and no plane list, so
/// hitting anything stops the move dead at the impact point. That is what
/// makes a `MOVETYPE_FLYGRAVITY` corpse feel like a dropped object rather than
/// like a player.
fn push_entity(
    mv: &mut MoveData,
    tracer: &mut Tracer<'_>,
    push: Vec3,
) -> crate::engine::trace::Trace {
    let end = mv.origin + push;
    let trace = trace_player_bbox(mv, tracer, mv.origin, end);
    mv.origin = trace.end;
    trace
}

/// `CGameMovement::PerformFlyCollisionResolution` (`gamemovement.cpp:5146`) —
/// what a flier does when it hits something.
///
/// **Three of Valve's four branches collapse for a player**, and each collapses
/// because of `MOVECOLLIDE_DEFAULT`:
///
/// - The backoff is **1** — a slide — rather than `2 - m_surfaceFriction`,
///   which is `MOVECOLLIDE_FLY_BOUNCE`'s.
/// - The `vel < 30*30 || GetMoveCollide() != MOVECOLLIDE_FLY_BOUNCE` test is
///   therefore **always true**, so the bounce-and-push-again alternative is
///   unreachable and a corpse that lands on anything flat stops there.
/// - Which in turn subsumes the "rolling on the ground, add static friction"
///   block above it: that one zeroes the *vertical* velocity below
///   `sv_gravity * frametime` and this one zeroes all three unconditionally.
///   Reproducing it would be writing a line that cannot be observed.
///
/// `MOVECOLLIDE_FLY_CUSTOM` is the fourth, and Valve's own comment on it is
/// "Should this ever occur for players!?" over an `Assert(0)`.
fn perform_fly_collision_resolution(mv: &mut MoveData, trace: &crate::engine::trace::Trace) {
    // `MOVECOLLIDE_DEFAULT` → `backoff = 1`.
    let (velocity, _) = clip_velocity(mv.velocity, trace.normal, 1.0);
    mv.velocity = velocity;

    // "stop if on ground" — and for a player that is the whole of it.
    if trace.normal.z > 0.7 {
        set_ground(mv, Some(trace.normal));
        mv.velocity = Vec3::ZERO;
    }
}

/// `CGameMovement::FullTossMove` (`gamemovement.cpp:5198`) — the dead player.
///
/// Gravity, one swept move, and a stop. The opening wish-velocity block is
/// reproduced and is **unreachable for a corpse**, because
/// [`check_parameters`] has already zeroed all three move axes for a dead or
/// frozen player and `MOVETYPE_FLYGRAVITY` is only ever reached by dying — it
/// is kept because it is the difference between this function and "apply
/// gravity", and because `MOVETYPE_FLY` would enter it.
///
/// `CheckWater` is absent along with water.
fn full_toss_move(mv: &mut MoveData, tracer: &mut Tracer<'_>, vars: &MoveVars, dt: f32) {
    if mv.forwardmove != 0.0 || mv.sidemove != 0.0 || mv.upmove != 0.0 {
        let (forward, right, _) = mv.angles.vectors();
        let mut wishvel =
            forward.normalize_or_zero() * mv.forwardmove + right.normalize_or_zero() * mv.sidemove;
        wishvel.z += mv.upmove;

        let wishdir = wishvel.normalize_or_zero();
        let mut wishspeed = wishvel.length();
        if wishspeed > mv.max_speed {
            wishspeed = mv.max_speed;
        }
        accelerate(mv, wishdir, wishspeed, vars.accelerate, dt);
    }

    if mv.velocity.z > 0.0 {
        set_ground(mv, None);
    }

    // "If on ground and not moving, return." — a corpse at rest costs one
    // comparison a frame and no trace.
    if mv.ground.is_some() && mv.base_velocity == Vec3::ZERO && mv.velocity == Vec3::ZERO {
        return;
    }

    check_velocity(mv, vars);

    // `if ( player->GetMoveType() == MOVETYPE_FLYGRAVITY ) AddGravity();` —
    // the **whole** frame's worth, not the half that `FullWalkMove` splits
    // either side of its move. `AddGravity` also spends the vertical base
    // velocity, exactly as [`start_gravity`] does.
    if mv.move_type == MoveType::FlyGravity {
        mv.velocity.z -= vars.gravity * dt;
        mv.velocity.z += mv.base_velocity.z * dt;
        mv.base_velocity.z = 0.0;
        check_velocity(mv, vars);
    }

    // "Base velocity is not properly accounted for since this entity will move
    // again after the bounce without taking it into account" — Valve's own
    // comment on the add/scale/subtract below.
    mv.velocity += mv.base_velocity;
    check_velocity(mv, vars);
    let push = mv.velocity * dt;
    mv.velocity -= mv.base_velocity;

    let trace = push_entity(mv, tracer, push);
    check_velocity(mv, vars);

    if trace.all_solid {
        // "entity is trapped in another solid" — `SetGroundEntity( &pm )` with
        // a trace that has no usable plane, because nothing was hit on the way
        // in. World `+Z` is this port's substitute for Valve's ground *entity*,
        // which carries no normal at all; either way what it means is "stop".
        set_ground(mv, Some(Vec3::Z));
        mv.velocity = Vec3::ZERO;
        return;
    }
    if trace.fraction != 1.0 {
        perform_fly_collision_resolution(mv, &trace);
    }
}

// ---------------------------------------------------------------------------
// The teleport — `CPortalGameMovement::HandlePortalling`
// ---------------------------------------------------------------------------

/// `ShouldPortalTransitionCrouch` (`portal_gamemovement.cpp:244`) — does this
/// pair turn the player's up axis far enough that an AABB cannot make the trip
/// standing?
///
/// Valve's whole test is `fabs( m_matrixThisToLinked.m[2][2] ) < COS_PI_OVER_SIX`,
/// with its own comment: *"how much does zUp still look like zUp after going
/// through this portal"*. `m[2][2]` is the z of the image of the z axis, which
/// is the one element a row-major matrix and a column-major one agree on
/// without any transposing.
pub fn transition_crouches(matrix: Mat4) -> bool {
    matrix.z_axis.z.abs() < COS_PI_OVER_SIX
}

/// `ShouldMaintainFlingAssistCrouch` (`portal_gamemovement.cpp:252`) — a
/// player leaving a partly-upward portal fast stays crouched.
///
/// *"If player is already crouched, do NOT automatically uncrouch. You don't
/// actually have to check that the player is exiting the portal, but we assume
/// that's the intent."*
fn should_maintain_fling_crouch(exit: &PortalHole, velocity: Vec3) -> bool {
    (exit.forward.z > 0.1 && exit.forward.z < 0.9)
        && velocity.z > 1.0
        && velocity.dot(exit.forward) > PLAYER_FLING_HELPER_MIN_SPEED
}

/// `SolveQuadratic` (`mathlib/mathlib_base.cpp:1445`) — `a x² + b x + c = 0`,
/// with Valve's degenerate cases kept because the caller relies on them.
fn solve_quadratic(a: f32, b: f32, c: f32) -> Option<(f32, f32)> {
    if a == 0.0 {
        // No x² term: linear, or all zeroes, or no solution at all.
        if b != 0.0 {
            return Some((-c / b, -c / b));
        }
        return (c == 0.0).then_some((0.0, 0.0));
    }
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    Some(((-b + root) / (2.0 * a), (-b - root) / (2.0 * a)))
}

/// The speed range a player may leave `exit` at —
/// `CPortal_Base2D::GetExitSpeedRange` (`portal_base2d_shared.cpp:977`) with
/// `CProp_Portal`'s overrides (`prop_portal_shared.cpp:201`, `:267`) folded in.
///
/// **It asks the exit portal, not the entrance.** Valve computes whether the
/// *entrance* is on a floor as well, and then uses it only in the two branches
/// that are not about players (225 and 50 for a physics object); with a player
/// on the line the answer never depends on it, so it is not computed here.
///
/// The maximum is a flat [`EXIT_SPEED_MAX`]. Below the minimum, speed is
/// *added along the exit's forward*, which is the caller's job; above the
/// maximum the whole vector is scaled.
fn exit_speed_range(
    exit: &PortalHole,
    center_at_exit: Vec3,
    extents: Vec3,
    gravity: f32,
) -> (f32, f32) {
    let minimum = if exit.forward.z > COS_PI_OVER_SIX {
        // Out of the floor: the number that keeps every fling in the game
        // alive.
        EXIT_SPEED_MIN_FLOOR
    } else if exit.forward.z > 0.5 {
        // *"bExitOnFloor means the portal is facing almost entirely up, just
        // because it's false doesn't mean the portal isn't facing
        // significantly up."*
        perch_speed(exit, center_at_exit, extents, gravity).unwrap_or(f32::NEG_INFINITY)
    } else {
        f32::NEG_INFINITY
    };
    (minimum, EXIT_SPEED_MAX)
}

/// The slowest a player can leave an upward-slanted portal and still land on
/// its bottom edge rather than falling back in
/// (`prop_portal_shared.cpp:217-260`).
///
/// *"Assuming our current velocity is zero. What's the minimum portal-forward
/// velocity to perch the player on the bottom edge of the portal?"* — a
/// projectile problem in the vertical plane through the portal's up axis,
/// solved for the launch speed along `forward`, and capped at the floor
/// portal's 300 so that a nearly-vertical portal does not ask for more than a
/// vertical one.
///
/// `None` when there is no gravity or the quadratic has no positive root, both
/// of which mean "do not touch the speed".
fn perch_speed(exit: &PortalHole, center: Vec3, extents: Vec3, gravity: f32) -> Option<f32> {
    if gravity == 0.0 {
        return None;
    }
    // A point along the bottom edge of the portal, horizontally centred, and
    // the bottom of the player's box at the exit.
    let perch = exit.center - exit.up * exit.half_height;
    let mut to_perch = perch - (center - Vec3::Z * extents.z);
    // Projected onto the portal's vertical centre line, so that all of the
    // horizontal distance is distance to the perch *line* rather than to one
    // point on it.
    to_perch -= to_perch.dot(exit.right) * exit.right;

    let horizontal = to_perch.truncate().length();
    let forward_horizontal = exit.forward.truncate().length();
    let a = (exit.forward.z * -2.0)
        * ((horizontal * forward_horizontal) - (to_perch.z * exit.forward.z));
    let (first, second) = solve_quadratic(a, 0.0, horizontal * horizontal * gravity)?;

    let best = first.max(second);
    (best > 0.0).then(|| best.min(EXIT_SPEED_MIN_FLOOR))
}

/// `Sign` (`public/mathlib/mathlib.h:1154`) — **zero is positive**, which the
/// two callers both depend on.
fn sign(value: f32) -> f32 {
    match value >= 0.0 {
        true => 1.0,
        false => -1.0,
    }
}

/// Which portal, if any, the player is interacting with at the end of this
/// move — `HandlePortalling`'s opening loop (`portal_gamemovement.cpp:2249`).
///
/// A swept hull against every active linked portal's trigger box, then three
/// filters, then nearest-centre-wins. The filters are the interesting part and
/// each one is a bug someone had:
///
/// - the **old** centre must have been in front of the plane — unless this
///   portal was already the player's environment, which is Valve's *"special
///   exception if we were pushed past the plane but did not move past it"*;
/// - if the new centre is *behind* the plane it has to be over the quad, or
///   walking into the wall beside a portal would count;
/// - if it is in *front*, the line from the centre to its most-penetrating
///   extent has to pass through the quad — *"avoids case where you can butt up
///   against a portal side on an angled panel"*.
///
/// **The sweep is approximated.** `CPortal_Base2D::TestCollision` is a box
/// sweep against the OBB; this is the union of the hull at both ends tested
/// against the same box, which can only ever answer `true` more often. Every
/// filter below then runs unchanged, and the trigger — the centre crossing the
/// plane — is exact, so the approximation cannot teleport anyone who should
/// not be; it can only put the player in a portal's environment a tick early,
/// which is the direction that fails safe.
fn select_portal<'a>(
    holes: &'a PortalHoles,
    environment: Option<u64>,
    start: Vec3,
    end: Vec3,
    mins: Vec3,
    maxs: Vec3,
) -> Option<&'a CarvedWall> {
    let origin_to_center = (mins + maxs) * 0.5;
    let center = end + origin_to_center;
    let previous = start + origin_to_center;
    let extents = (maxs - mins) * 0.5;
    let (lo, hi) = (
        (start + mins).min(end + mins),
        (start + maxs).max(end + maxs),
    );

    let mut best: Option<(&CarvedWall, f32)> = None;
    for wall in holes.iter() {
        // `IsActivedAndLinked`. An unlinked portal has a hole and nowhere to
        // go, so it can never be a teleport and is never an environment.
        if wall.link().is_none() {
            continue;
        }
        let hole = wall.hole();
        if !hole.touches(lo, hi) {
            continue;
        }

        let dist = hole.forward.dot(hole.center);
        let was_in_front = hole.forward.dot(previous) - dist > 0.0;
        if !was_in_front && Some(wall.id()) != environment {
            continue;
        }

        let ahead = hole.forward.dot(center) - dist;
        let over_the_quad = |point: Vec3, margin: f32| {
            let offset = point - hole.center;
            let offset = offset - offset.dot(hole.forward) * hole.forward;
            offset.dot(hole.right).abs() <= hole.half_width + margin
                && offset.dot(hole.up).abs() <= hole.half_height + margin
        };

        let accepted = match ahead < 0.0 {
            true => over_the_quad(center, 0.0),
            false => {
                // The most-penetrating corner of the box, which is the corner
                // furthest *behind* the plane.
                let test = center
                    - Vec3::new(
                        sign(hole.forward.x) * extents.x,
                        sign(hole.forward.y) * extents.y,
                        sign(hole.forward.z) * extents.z,
                    );
                let test_dist = hole.forward.dot(test) - dist;
                let total = ahead - test_dist;
                // Not penetrating at all, or the two distances are equal and
                // there is no line to intersect: nothing to reject.
                test_dist >= QUADTEST_EPSILON
                    || total == 0.0
                    || over_the_quad(test * (ahead / total) - center * (test_dist / total), 1.0)
            }
        };
        if !accepted {
            continue;
        }

        let distance = (hole.center - center).length_squared();
        if best.is_none_or(|(_, nearest)| distance < nearest) {
            best = Some((wall, distance));
        }
    }
    best.map(|(wall, _)| wall)
}

/// *"The real world equivalent of stubbing your toe on the exit hole results
/// in flinging straight up"* (`portal_gamemovement.cpp:2529`) — move a flung
/// player's centre back towards the portal's axis so their hull corner clears
/// the lip.
///
/// The corner tested is the one furthest from the axis, and the margin is five
/// units inside the portal's own edge.
fn fling_nudge(exit: &PortalHole, center: Vec3, extents: Vec3) -> Vec3 {
    let to_center = center - exit.center;
    let off_axis = to_center - to_center.dot(exit.forward) * exit.forward;
    let corner = center
        + Vec3::new(
            extents.x * sign(off_axis.x),
            extents.y * sign(off_axis.y),
            extents.z * sign(off_axis.z),
        );

    let to_corner = corner - exit.center;
    let (across, up) = (to_corner.dot(exit.right), to_corner.dot(exit.up));
    let (width, height) = (exit.half_width - 5.0, exit.half_height - 5.0);

    let pull = |along: f32, limit: f32, axis: Vec3| match along {
        _ if along > limit => -axis * (along - limit),
        _ if along < -limit => -axis * (along + limit),
        _ => Vec3::ZERO,
    };
    pull(across, width, exit.right) + pull(up, height, exit.up)
}

/// `CPortalGameMovement::HandlePortalling` (`portal_gamemovement.cpp:2214`) —
/// the teleport, run at the end of every move.
///
/// It compares where the move *started* with where it ended: if the player's
/// box centre crossed an active linked portal's plane during this move, they
/// come out of the other one. Everything else in the function is about making
/// that survive an axis-aligned box that cannot rotate.
///
/// In order, and each is `portdocs/PORTAL.md` §6's numbered part:
///
/// 1. **Select the portal** ([`select_portal`]) — and record it as the
///    player's environment whether or not they go through, because that is
///    what the *next* move is traced against.
/// 2. **The frame split**: the crossing happened part way through the frame,
///    so the gravity applied after it is unwound before the rotation and put
///    back after it at [`EXIT_GRAVITY_BOOST`].
/// 3. **The velocity**: rotated, then clamped into
///    [`exit_speed_range`], then clamped per axis the way `CheckVelocity`
///    would — *"but be quiet about it"*.
/// 4. **The forced duck**, when the transition turns the player's up axis.
/// 5. **The move itself**, which preserves the box's **centre** and not its
///    origin — conflating the two drops the player 18 units.
///
/// The angles are not touched here; see [`Teleport`].
fn handle_portalling<'a>(
    mv: &mut MoveData,
    tracer: &mut Tracer<'a>,
    holes: &'a PortalHoles,
    vars: &MoveVars,
    dt: f32,
) {
    let (mins, maxs) = (player_mins(mv.ducked), player_maxs(mv.ducked));
    let mut origin_to_center = (mins + maxs) * 0.5;
    let center = mv.origin + origin_to_center;
    let previous = mv.move_start + origin_to_center;
    let extents = (maxs - mins) * 0.5;

    let selected = select_portal(
        holes,
        mv.portal_environment,
        mv.move_start,
        mv.origin,
        mins,
        maxs,
    );
    let Some(wall) = selected else {
        mv.portal_environment = None;
        return;
    };
    let (id, hole) = (wall.id(), *wall.hole());
    let link = *wall
        .link()
        .expect("select_portal keeps only linked portals");
    mv.portal_environment = Some(id);

    // **The trigger is the centre crossing the plane** — `m_plane_Origin` and
    // `< -FLT_EPSILON`, not the hull's near face and not the simulator's
    // shifted plane. `IsMobile` is the other way in and this port has no
    // moving portals.
    let dist = hole.forward.dot(hole.center);
    let plane_dist = hole.forward.dot(center) - dist;
    if plane_dist >= -f32::EPSILON {
        return;
    }

    let exit = link.exit;
    let matrix = link.to_exit;

    // §6.2 — when in this frame the crossing happened. `fOldPlaneDist` is
    // *meant* to be positive and sometimes is not: *"some kind of physics
    // penetration seems to be the cause (bugbait #61331)"*, and Valve's answer
    // is to call it half way and move on.
    let old_plane_dist = hole.forward.dot(previous) - dist;
    let total = old_plane_dist - plane_dist;
    let crossed_at = match total != 0.0 {
        true => old_plane_dist / total,
        false => 0.5,
    };
    let after_crossing = (1.0 - crossed_at) * dt;

    let was_on_ground = mv.ground.is_some();
    set_ground(mv, None);

    // §6.3 — the velocity.
    {
        // Gravity is world-down on both sides of a portal, so it is taken out
        // of the velocity *before* the rotation and added back to the result
        // rather than rotated with it. A player who was on the ground had none
        // applied to begin with.
        //
        // `GetImplicitVerticalStepSpeed` — the vertical speed a player carries
        // implicitly while walking up a slope, since ground velocity is
        // xy-only — is added before the rotation in the original. It is not
        // ported: nothing in this port tracks it, and it is zero except on a
        // slope.
        let gravity = match was_on_ground {
            true => Vec3::ZERO,
            false => Vec3::new(0.0, 0.0, -vars.gravity * after_crossing),
        };
        let mut velocity =
            matrix.transform_vector3(mv.velocity - gravity) + gravity * EXIT_GRAVITY_BOOST;

        let (minimum, maximum) = exit_speed_range(
            &exit,
            matrix.transform_point3(center),
            extents,
            vars.gravity,
        );
        let along_exit = velocity.dot(exit.forward);
        if along_exit < minimum {
            // **Added along the exit forward, not scaled.** Scaling would turn
            // a sideways exit into a faster sideways exit.
            velocity += exit.forward * (minimum - along_exit);
        } else {
            let speed = velocity.length();
            if speed > maximum && speed != 0.0 {
                velocity *= maximum / speed;
            }
        }

        // `CheckVelocity`'s per-axis clamp, done quietly.
        mv.velocity = velocity.clamp(
            Vec3::splat(-vars.maxvelocity),
            Vec3::splat(vars.maxvelocity),
        );
    }

    // §6.4 — the forced duck. An AABB cannot rotate, so a transition that
    // turns the up axis has to curl the player into the duck hull *now*.
    let duck_to_fit = transition_crouches(matrix);
    let duck_to_fling = should_maintain_fling_crouch(&exit, mv.velocity);
    let forced_duck = duck_to_fit || duck_to_fling;
    if forced_duck && !mv.ducked {
        // `m_bInDuckJump` has no field here — it exists to keep the duck-jump
        // eye offset going, and the timer is what makes the duck a duck.
        mv.duck_time_msecs = DUCK_TIME_MSECS;
        finish_duck(mv, tracer, vars);
        // **Recomputed against the duck hull**, so that the transform below
        // preserves the *centre* of the box the player now has.
        origin_to_center = (player_mins(true) + player_maxs(true)) * 0.5;
    }

    // §6.5 — the move. The centre goes through the matrix and the origin is
    // derived back from it.
    let mut exit_center = matrix.transform_point3(center);
    if duck_to_fling
        || (duck_to_fit && mv.velocity.dot(exit.forward) > PLAYER_FLING_HELPER_MIN_SPEED)
    {
        let duck_extents = (player_maxs(true) - player_mins(true)) * 0.5;
        exit_center += fling_nudge(&exit, exit_center, duck_extents);
    }
    mv.origin = exit_center - origin_to_center;

    // *"We need to trace against the new environment now instead of waiting
    // for it to update naturally"* — the player is at the exit, so the carved
    // geometry they are inside is the exit's.
    tracer.set_hole(holes.get(link.exit_id));
    if trace_player_bbox(mv, tracer, mv.origin, mv.origin).start_solid {
        // *"AABB's going through portals are likely to cause weird collision
        // bugs. Just try to get them close"*: sweep in from the portal's own
        // axis, which is the direction with the most room.
        let to_center = (mv.origin + origin_to_center) - exit.center;
        let off_axis = to_center - to_center.dot(exit.forward) * exit.forward;
        let pulled = trace_player_bbox(mv, tracer, mv.origin - off_axis, mv.origin);
        if !pulled.start_solid {
            mv.origin = pulled.end;
        }
        // Valve's third attempt,
        // `UTIL_FindClosestPassableSpace_InPortal_CenterMustStayInFront`, is a
        // 100-iteration search for a free spot and is **not ported**: it needs
        // `UTIL_FindClosestPassableSpace`, which nothing else here wants yet.
    }

    mv.portal_environment = Some(link.exit_id);
    mv.teleported = Some(Teleport {
        matrix,
        entered: id,
        exit: link.exit_id,
        forced_duck,
    });
}

/// `CGameMovement::PlayerMove` (`gamemovement.cpp:4994`) — the per-command
/// entry point, and the order everything else runs in.
pub fn player_move<'a>(
    mv: &mut MoveData,
    tracer: Option<&mut Tracer<'a>>,
    portals: Option<&'a PortalHoles>,
    vars: &MoveVars,
    dt: f32,
    old_angles: ViewAngles,
) {
    // `m_vMoveStartPosition = mv->GetAbsOrigin()`
    // (`portal_gamemovement.cpp:393`), which the original does in
    // `ProcessMovement` just before this. Here rather than in the caller so
    // that nothing can forget it, and so that
    // [`handle_portalling`]'s comparison is always against the move that just
    // ran.
    mv.move_start = mv.origin;
    mv.teleported = None;

    check_parameters(mv, old_angles);
    reduce_timers(mv, dt);

    // `CheckStuck` is skipped for noclip anyway, and is not ported.

    let Some(tracer) = tracer else {
        // No map loaded: noclip still flies, walking has nothing to stand on
        // and a corpse has nothing to land on.
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

    // `UpdateDuckJumpEyeOffset(); Duck();` — and it runs for a dead player
    // too, which is why [`check_parameters`] writes the dead view offset
    // *before* this rather than after: `Duck` would otherwise interpolate the
    // eye back up out of the corpse over the next 400 ms.
    duck(mv, tracer, vars);
    if is_dead(mv) {
        mv.view_offset = VEC_DEAD_VIEWHEIGHT;
    }

    // A map with no portals still walks, and `air_move` still asks the list —
    // it just has nothing in it. Built here rather than threaded as an
    // `Option` because the funnel's loop over an empty list is one branch and
    // an `Option` at every use is not.
    let no_portals = PortalHoles::default();
    match mv.move_type {
        MoveType::Noclip => full_noclip_move(mv, vars, dt),
        MoveType::Walk => full_walk_move(mv, tracer, portals.unwrap_or(&no_portals), vars, dt),
        MoveType::FlyGravity => full_toss_move(mv, tracer, vars, dt),
    }

    // `HandlePortalling()` (`portal_gamemovement.cpp:468`), which the original
    // calls between `PlayerMove` and `FinishMove` for **every** move type —
    // including noclip, which is how you fly through a portal.
    //
    // With no portals in the level there is nothing to select and the field is
    // cleared, which matters: a level change leaves a stale id behind
    // otherwise, and the next map's carve would be picked by it.
    match portals {
        Some(portals) if !portals.is_empty() => handle_portalling(mv, tracer, portals, vars, dt),
        _ => mv.portal_environment = None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::trace::fixture::{self, Fixture};
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
            health: 100,
            frozen: false,
            ground: None,
            base_velocity: Vec3::ZERO,
            surface_friction: 1.0,
            ducked: false,
            ducking: false,
            duck_time_msecs: 0,
            view_offset: VEC_VIEW,
            speed_cropped: false,
            move_start: origin,
            portal_environment: None,
            teleported: None,
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
            let angles = mv.angles;
            player_move(mv, Some(&mut tracer), None, &MoveVars::PORTAL2, dt, angles);
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
                let angles = mv.angles;
                player_move(
                    &mut mv,
                    Some(&mut tracer),
                    None,
                    &MoveVars::PORTAL2,
                    TICK,
                    angles,
                );

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
            let angles = mv.angles;
            player_move(&mut mv, None, None, &MoveVars::PORTAL2, TICK, angles);
        }
        assert_eq!(mv.origin, Vec3::new(0.0, 0.0, 100.0));
    }

    // -----------------------------------------------------------------------
    // the dead player — `server/` stage 5
    // -----------------------------------------------------------------------

    /// A corpse falls, lands, and stops. `MOVETYPE_FLYGRAVITY` and
    /// `FullTossMove`.
    #[test]
    fn a_dead_player_falls_under_gravity_and_stops_on_the_floor() {
        let world = room();
        let mut mv = walker(Vec3::new(0.0, 0.0, 200.0));
        mv.move_type = MoveType::FlyGravity;
        mv.health = 0;

        run(&mut mv, &world, 120, TICK, |_| {});

        assert!(
            (mv.origin.z - 0.0).abs() < 0.1,
            "landed on the floor, at {}",
            mv.origin.z
        );
        // `PerformFlyCollisionResolution`'s "stop if on ground" — the velocity
        // is zeroed outright rather than bled off, because a player's
        // move-collide is `MOVECOLLIDE_DEFAULT`.
        assert_eq!(mv.velocity, Vec3::ZERO);
        assert!(mv.ground.is_some());
    }

    /// A dead player takes no input at all: `CheckParameters` zeroes all three
    /// move axes when `IsDead()`, so holding forward does nothing.
    #[test]
    fn a_dead_player_cannot_walk() {
        let world = room();
        let mut mv = walker(Vec3::new(0.0, 0.0, 0.0));
        mv.health = 0;
        mv.move_type = MoveType::FlyGravity;

        run(&mut mv, &world, 60, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
            mv.buttons = ButtonBits::FORWARD;
        });

        assert!(
            mv.origin.x.abs() < 1e-3,
            "a corpse walked to {}",
            mv.origin.x
        );
    }

    /// A **frozen** player is the same test with health left alone: the move
    /// is zeroed and the player is still `MOVETYPE_WALK`.
    ///
    /// `CRevertSaved::InputReload` is the one thing in this port that sets the
    /// flag — 11 connections at the nine `player_loadsaved` entities.
    #[test]
    fn a_frozen_player_cannot_walk_and_is_still_alive() {
        let world = room();
        let mut mv = walker(Vec3::new(0.0, 0.0, 0.0));
        mv.frozen = true;

        run(&mut mv, &world, 60, TICK, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
            mv.buttons = ButtonBits::FORWARD;
        });

        assert!(mv.origin.x.abs() < 1e-3, "a frozen player walked");
        assert!(!is_dead(&mv), "frozen is not dead");
        assert_eq!(mv.move_type, MoveType::Walk);
    }

    /// The eye drops from 64 to [`VEC_DEAD_VIEWHEIGHT`], and it is 14 rather
    /// than the multiplayer table's 60 — see that constant.
    ///
    /// Also pins the *ordering*: the dead offset is written after `Duck()`,
    /// which would otherwise interpolate the eye back up out of the corpse
    /// over the 400 ms of an un-duck.
    #[test]
    fn the_dead_view_drops_to_the_floor_and_duck_does_not_lift_it_back() {
        let world = room();
        let mut mv = walker(Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(mv.view_offset, VEC_VIEW);

        // Crouch first, so that a duck transition is in flight when the player
        // dies — the shape that would otherwise fight the dead offset.
        run(&mut mv, &world, 40, TICK, |mv| {
            mv.buttons = ButtonBits::DUCK;
        });
        assert!(mv.ducked);

        mv.health = 0;
        mv.move_type = MoveType::FlyGravity;
        run(&mut mv, &world, 60, TICK, |_| {});
        assert_eq!(mv.view_offset, VEC_DEAD_VIEWHEIGHT);
    }

    /// A dead player's *movement basis* is pinned to the previous command's
    /// angles (`portal_gamemovement.cpp:3020`), which is a distinct `if` from
    /// the one that zeroes the move.
    #[test]
    fn a_dead_players_movement_basis_is_the_previous_commands() {
        let mut mv = walker(Vec3::ZERO);
        mv.health = 0;
        mv.move_type = MoveType::FlyGravity;
        mv.angles = ViewAngles::new(10.0, 90.0);

        let old = ViewAngles::new(0.0, 0.0);
        player_move(&mut mv, None, None, &MoveVars::PORTAL2, TICK, old);
        assert_eq!(mv.angles.yaw, 0.0, "pinned to m_vecOldAngles");
        assert_eq!(mv.angles.pitch, 0.0);

        // …and a *live* player takes the command's angles unchanged.
        let mut mv = walker(Vec3::ZERO);
        mv.angles = ViewAngles::new(10.0, 90.0);
        player_move(&mut mv, None, None, &MoveVars::PORTAL2, TICK, old);
        assert_eq!(mv.angles.yaw, 90.0);
    }

    // -----------------------------------------------------------------------
    // Portals — `portdocs/PORTAL.md` stage 4
    // -----------------------------------------------------------------------

    /// [`run`], with the portal plumbing `Engine::update_client` does around it.
    ///
    /// Two things happen per frame that the plain runner has no reason to do:
    /// the tracer's hole is chosen from the environment the **previous** move
    /// ended in, and the view turns with the player when one of them teleports.
    /// Both are the engine's job in the real thing, and doing them here is what
    /// makes these tests about the movement rather than about a harness.
    ///
    /// Returns every teleport that happened, because `player_move` clears the
    /// field at the top of each command.
    fn run_portals(
        mv: &mut MoveData,
        collision: &CollisionBsp,
        holes: &PortalHoles,
        frames: usize,
        mut fill: impl FnMut(&mut MoveData),
    ) -> Vec<Teleport> {
        let mut teleports = Vec::new();
        for _ in 0..frames {
            mv.forwardmove = 0.0;
            mv.sidemove = 0.0;
            mv.upmove = 0.0;
            mv.buttons = ButtonBits::NONE;
            mv.speed_cropped = false;
            fill(mv);

            let mut tracer = collision.tracer();
            if let Some(wall) = mv.portal_environment.and_then(|id| holes.get(id)) {
                tracer = tracer.with_hole(wall);
                let crouches = wall
                    .link()
                    .is_some_and(|link| transition_crouches(link.to_exit));
                if crouches {
                    tracer = tracer.with_exit_hull(player_mins(true), player_maxs(true));
                }
            }

            let angles = mv.angles;
            player_move(
                mv,
                Some(&mut tracer),
                Some(holes),
                &MoveVars::PORTAL2,
                TICK,
                angles,
            );
            if let Some(teleport) = mv.teleported {
                mv.angles = teleport.turn(mv.angles);
                teleports.push(teleport);
            }
        }
        teleports
    }

    /// Blue's room, its portal, and a carved store for both of them.
    fn rooms_and_holes() -> (fixture::PortalRooms, PortalHoles) {
        let rooms = fixture::portal_rooms();
        let mut holes = PortalHoles::default();
        holes.sync(&rooms.collision, &rooms.live());
        (rooms, holes)
    }

    /// A player standing on blue's floor, sixty units in front of the portal,
    /// facing it.
    fn at_the_blue_portal(rooms: &fixture::PortalRooms, holes: &PortalHoles) -> MoveData {
        // Dropped rather than placed, so that the same `CategorizePosition`
        // that runs during the walk is what put them on the ground.
        let mut mv = walker(Vec3::new(60.0, 0.0, fixture::PORTAL_ROOM_FLOOR + 20.0));
        // Blue faces `+X` and the player walks into it, so they are looking
        // along `-X`.
        mv.angles = ViewAngles::new(0.0, 180.0);
        let teleports = run_portals(&mut mv, &rooms.collision, holes, 40, |_| {});
        assert!(teleports.is_empty(), "teleported while standing still");
        assert!(mv.ground.is_some(), "not standing on the floor: {mv:#?}");
        assert!(
            (mv.origin.z - fixture::PORTAL_ROOM_FLOOR).abs() < 0.1,
            "{}",
            mv.origin
        );
        mv
    }

    /// **The test that says stage 4 works.** A player walks into one portal and
    /// comes out of the other, standing on the far room's floor and facing the
    /// way that room faces.
    #[test]
    fn walking_into_a_portal_comes_out_of_the_other_one() {
        let (rooms, holes) = rooms_and_holes();
        let mut mv = at_the_blue_portal(&rooms, &holes);

        let teleports = run_portals(&mut mv, &rooms.collision, &holes, 60, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        assert_eq!(teleports.len(), 1, "{teleports:#?}");
        let teleport = teleports[0];
        assert_eq!(teleport.entered, fixture::PortalRooms::BLUE_ID);
        assert_eq!(teleport.exit, fixture::PortalRooms::ORANGE_ID);
        assert!(!teleport.forced_duck, "both portals are on walls");

        // Orange is at `(1000, 0, 0)` facing `+Y`, so its room is the `+Y` side
        // and a player walking straight through comes out on its axis.
        assert!(
            (mv.origin.x - 1000.0).abs() < 1.0,
            "came out at {}",
            mv.origin
        );
        assert!(
            mv.origin.y > 40.0,
            "did not walk away from orange: {}",
            mv.origin
        );
        assert!(
            (mv.origin.z - fixture::PORTAL_ROOM_FLOOR).abs() < 0.1,
            "not on the far room's floor: {}",
            mv.origin
        );
        assert!(mv.ground.is_some(), "airborne in the far room");

        // Still walking forward, which is now `+Y`.
        assert!(mv.velocity.y > 100.0, "{}", mv.velocity);
        // And looking that way: blue's `-X` became orange's `+Y`.
        assert!((mv.angles.yaw - 90.0).abs() < 1e-2, "{}", mv.angles.yaw);
        assert!(mv.angles.pitch.abs() < 1e-2 && mv.angles.roll.abs() < 1e-2);
    }

    /// A floor portal and its partner, in an otherwise empty world, with
    /// nothing to land on — the funnel's fixture.
    ///
    /// The portal faces straight up (`pitch -90` puts `forward` on `+Z`) and
    /// sits at the origin; the partner is a thousand units away, because
    /// `player_should_funnel` only asks about linked portals and never looks
    /// at where the partner is.
    fn a_floor_portal() -> (CollisionBsp, PortalHoles) {
        use crate::engine::trace::{LivePortal, PortalLink};
        use crate::server::classes::portal::teleport_matrix;

        let floor_at = (Vec3::ZERO, Vec3::new(-90.0, 0.0, 0.0));
        let exit_at = (Vec3::new(1000.0, 0.0, 0.0), Vec3::ZERO);
        let hole = |(origin, angles): (Vec3, Vec3)| PortalHole::new(origin, angles, 32.0, 56.0);

        // One brush, far below and far to the side: the fall has to be free,
        // but `PortalHoles::sync` wants a collision model to carve against.
        let mut fixture = fixture::Fixture::default();
        fixture.add_box(
            Vec3::new(-2000.0, -2000.0, -2000.0),
            Vec3::new(2000.0, 2000.0, -1900.0),
            Contents::SOLID,
            true,
        );
        let collision = fixture.single_leaf();

        let live = [
            LivePortal {
                id: 1,
                hole: hole(floor_at),
                link: Some(PortalLink {
                    exit_id: 2,
                    exit: hole(exit_at),
                    to_exit: teleport_matrix(floor_at, exit_at),
                    to_entrance: teleport_matrix(exit_at, floor_at),
                }),
            },
            LivePortal {
                id: 2,
                hole: hole(exit_at),
                link: Some(PortalLink {
                    exit_id: 1,
                    exit: hole(floor_at),
                    to_exit: teleport_matrix(exit_at, floor_at),
                    to_entrance: teleport_matrix(floor_at, exit_at),
                }),
            },
        ];
        let mut holes = PortalHoles::default();
        holes.sync(&collision, &live);
        (collision, holes)
    }

    /// **The funnel.** A player falling past a floor portal, off its axis and
    /// looking down, is pulled onto it — and the same fall with no portal in
    /// the level is not.
    ///
    /// This is `IsFloorPortal`'s one remaining consumer on the player's path;
    /// see [`player_should_funnel`] for where the other three went.
    #[test]
    fn falling_towards_a_floor_portal_pulls_the_player_onto_its_axis() {
        let (collision, holes) = a_floor_portal();

        // Off the axis, well above it, already falling fast enough to qualify
        // (`velocity.z < -165`) and looking down (`pitch 60` puts the view
        // forward's `z` at `-sin 60`, which clears the `-0.7` threshold).
        let start = Vec3::new(40.0, 0.0, 400.0);
        let drop = |holes: &PortalHoles| {
            let mut mv = walker(start);
            mv.angles = ViewAngles::new(60.0, 0.0);
            mv.velocity = Vec3::new(0.0, 0.0, -300.0);
            run_portals(&mut mv, &collision, holes, 30, |_| {});
            mv
        };

        let funnelled = drop(&holes);
        let control = drop(&PortalHoles::default());

        assert!(
            control.origin.x == start.x && control.velocity.x == 0.0,
            "the control drifted with no portal in the level: {}",
            control.origin
        );
        assert!(
            funnelled.velocity.x < -10.0,
            "the funnel did not pull the player towards the axis: {}",
            funnelled.velocity
        );
        assert!(
            funnelled.origin.x < start.x - 5.0,
            "…and did not move them: {} from {start}",
            funnelled.origin
        );
        assert!(
            funnelled.origin.x > 0.0,
            "the funnel overshot the portal's axis: {}",
            funnelled.origin
        );
    }

    /// The funnel's own three refusals, each one alone: a fling is not
    /// funnelled, a player steering hard is not funnelled, and a player who is
    /// not looking down is not funnelled into a portal below them.
    #[test]
    fn the_funnel_refuses_a_fling_a_steer_and_a_player_looking_up() {
        let (collision, holes) = a_floor_portal();
        let start = Vec3::new(40.0, 0.0, 400.0);

        let drift = |velocity: Vec3, pitch: f32, sidemove: f32| {
            let mut mv = walker(start);
            mv.angles = ViewAngles::new(pitch, 0.0);
            mv.velocity = velocity;
            run_portals(&mut mv, &collision, &holes, 30, |mv| {
                mv.sidemove = sidemove;
            });
            mv.origin.x - start.x
        };

        let falling = Vec3::new(0.0, 0.0, -300.0);
        assert!(drift(falling, 60.0, 0.0) < -5.0, "the control funnels");
        // Past `MIN_FLING_SPEED` horizontally the branch is the fling
        // cancellation instead, and the funnel never runs.
        let flung = Vec3::new(0.0, 400.0, -300.0);
        assert!(drift(flung, 60.0, 0.0).abs() < 1.0, "a fling was funnelled");
        // Looking ahead rather than down.
        assert!(
            drift(falling, 0.0, 0.0).abs() < 1.0,
            "a player not looking into the portal was funnelled"
        );
        // Steering hard sideways — `|wishdir| > 64` on a horizontal axis. At
        // yaw 0 `right` is `-Y`, so the steer itself moves the player along
        // `y` and leaves `x` to say whether the funnel ran.
        assert!(
            drift(falling, 60.0, SV_SPEED_NORMAL).abs() < 1.0,
            "a player steering hard sideways was still pulled in"
        );
    }

    /// The trigger is the box's **centre** crossing the plane, not its near
    /// face touching it.""
    ///
    /// A player walked up to the portal is in its environment — which is what
    /// makes the next move traced against the carve — and has not gone
    /// anywhere.
    #[test]
    fn standing_in_a_portals_trigger_box_is_not_going_through_it() {
        let (rooms, holes) = rooms_and_holes();
        let mut mv = at_the_blue_portal(&rooms, &holes);
        assert_eq!(
            mv.portal_environment,
            Some(fixture::PortalRooms::BLUE_ID),
            "sixty units out is inside the sixty-four-unit trigger box"
        );

        // Walk until the hull's near face is past the plane and stop there.
        // A tick at a time rather than a fixed count, because the number of
        // ticks the acceleration ramp takes is not what this test is about —
        // the hull is sixteen units deep, so there are always several ticks
        // between the face crossing and the centre.
        let mut teleports = Vec::new();
        for _ in 0..60 {
            teleports.extend(run_portals(&mut mv, &rooms.collision, &holes, 1, |mv| {
                mv.forwardmove = SV_SPEED_NORMAL;
            }));
            if mv.origin.x + player_mins(false).x < 0.0 {
                break;
            }
        }
        assert!(
            mv.origin.x + player_mins(false).x < 0.0,
            "the hull never reached the plane, so the test proves nothing: {}",
            mv.origin
        );
        assert!(teleports.is_empty(), "teleported without crossing");
        assert!(
            mv.origin.x > 0.0,
            "the centre crossed after all: {}",
            mv.origin
        );
        assert_eq!(mv.portal_environment, Some(fixture::PortalRooms::BLUE_ID));
    }

    /// **A portal you cannot come out of is a portal you cannot walk into.**
    ///
    /// The whole point of the remote trace: a barrier eight units in front of
    /// the *exit* stops the player eight units in front of the *entrance*, and
    /// the surface they are stopped by faces back out of the portal at them.
    #[test]
    fn a_portal_whose_far_side_is_blocked_cannot_be_walked_into() {
        // Across orange's opening, eight units out from its plane.
        let barrier = (
            Vec3::new(960.0, 8.0, fixture::PORTAL_ROOM_FLOOR),
            Vec3::new(1040.0, 16.0, 56.0),
        );
        let rooms = fixture::portal_rooms_with(&[barrier]);
        let mut holes = PortalHoles::default();
        holes.sync(&rooms.collision, &rooms.live());

        let mut mv = at_the_blue_portal(&rooms, &holes);
        let teleports = run_portals(&mut mv, &rooms.collision, &holes, 60, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });

        assert!(teleports.is_empty(), "walked through a blocked portal");
        assert!(
            mv.origin.x > 4.0 && mv.origin.x < 12.0,
            "stopped at {} rather than eight units short",
            mv.origin
        );
        // The control: without the barrier the same walk goes through.
        let (rooms, holes) = rooms_and_holes();
        let mut mv = at_the_blue_portal(&rooms, &holes);
        let teleports = run_portals(&mut mv, &rooms.collision, &holes, 60, |mv| {
            mv.forwardmove = SV_SPEED_NORMAL;
        });
        assert_eq!(teleports.len(), 1, "the barrier was not what stopped them");
    }

    /// A transition that turns the player's up axis curls them into the duck
    /// hull as they cross, and puts the *centre* of the new hull where the
    /// centre of the old one was.
    #[test]
    fn a_transition_that_turns_the_up_axis_ducks_the_player_as_they_cross() {
        use crate::engine::trace::{LivePortal, PortalLink};
        use crate::server::classes::portal::teleport_matrix;

        let rooms = fixture::portal_rooms();
        let hole = |(origin, angles): (Vec3, Vec3)| PortalHole::new(origin, angles, 32.0, 56.0);
        // Blue where the fixture puts it, and a partner in the **floor** far
        // away: pitch -90 faces a portal straight up.
        let blue_at = (Vec3::ZERO, Vec3::ZERO);
        let floor_at = (Vec3::new(1000.0, 0.0, 0.0), Vec3::new(-90.0, 0.0, 0.0));
        let pair = |a: (Vec3, Vec3), b: (Vec3, Vec3), exit_id: u64| {
            Some(PortalLink {
                exit_id,
                exit: hole(b),
                to_exit: teleport_matrix(a, b),
                to_entrance: teleport_matrix(b, a),
            })
        };
        let live = [
            LivePortal {
                id: 1,
                hole: hole(blue_at),
                link: pair(blue_at, floor_at, 2),
            },
            LivePortal {
                id: 2,
                hole: hole(floor_at),
                link: pair(floor_at, blue_at, 1),
            },
        ];
        assert!(
            transition_crouches(live[0].link.unwrap().to_exit),
            "a wall to a floor is the transition that needs the duck"
        );

        let mut holes = PortalHoles::default();
        holes.sync(&rooms.collision, &live);

        // Straddling blue's plane, standing: the move ended with the centre a
        // unit past it.
        let mut mv = walker(Vec3::new(-1.0, 0.0, fixture::PORTAL_ROOM_FLOOR));
        mv.move_start = Vec3::new(2.0, 0.0, fixture::PORTAL_ROOM_FLOOR);
        mv.portal_environment = Some(1);
        mv.ground = Some(Vec3::Z);
        let centre_before = mv.origin + (player_mins(false) + player_maxs(false)) * 0.5;

        let mut tracer = rooms.collision.tracer();
        handle_portalling(&mut mv, &mut tracer, &holes, &MoveVars::PORTAL2, TICK);

        let teleport = mv.teleported.expect("the centre crossed the plane");
        assert!(teleport.forced_duck);
        assert!(mv.ducked, "the hull is still the standing one");
        assert_eq!(mv.duck_time_msecs, DUCK_TIME_MSECS);
        assert_eq!(mv.portal_environment, Some(2));

        // **The centre is what the transform preserves**, not the origin:
        // reading the two the wrong way round drops the player by the
        // difference between the hulls.
        let centre_after = mv.origin + (player_mins(true) + player_maxs(true)) * 0.5;
        let expected = teleport.matrix.transform_point3(centre_before);
        assert!(
            (centre_after - expected).length() < 0.1,
            "the centre moved to {centre_after} rather than {expected}"
        );
    }

    /// `GetExitSpeedRange`: 300 out of a floor portal for a player, nothing
    /// imposed out of a wall, and the perched solution in between.
    #[test]
    fn a_player_leaves_a_floor_portal_at_three_hundred_units_a_second() {
        let extents = (player_maxs(false) - player_mins(false)) * 0.5;
        let centre = Vec3::new(0.0, 0.0, 36.0);
        let at = |pitch: f32| PortalHole::new(Vec3::ZERO, Vec3::new(pitch, 0.0, 0.0), 32.0, 56.0);

        // Straight up: the number that keeps every fling in the game alive.
        let (minimum, maximum) = exit_speed_range(&at(-90.0), centre, extents, SV_GRAVITY);
        assert_eq!((minimum, maximum), (EXIT_SPEED_MIN_FLOOR, EXIT_SPEED_MAX));

        // A wall imposes no minimum at all.
        let (minimum, _) = exit_speed_range(&at(0.0), centre, extents, SV_GRAVITY);
        assert_eq!(minimum, f32::NEG_INFINITY);

        // Tilted 45° up: `bExitOnFloor` is false and `forward.z > 0.5` is true,
        // so the speed comes from the quadratic and is capped at the floor's.
        let (minimum, _) = exit_speed_range(&at(-45.0), centre, extents, SV_GRAVITY);
        assert!(
            minimum > 0.0 && minimum <= EXIT_SPEED_MIN_FLOOR,
            "the perch solution came out {minimum}"
        );

        // …and 30° up is below the `forward.z > 0.5` gate, so nothing applies.
        let (minimum, _) = exit_speed_range(&at(-29.0), centre, extents, SV_GRAVITY);
        assert_eq!(minimum, f32::NEG_INFINITY);
    }

    /// `SolveQuadratic`'s degenerate cases, which the perch calculation relies
    /// on rather than guarding against.
    #[test]
    fn the_quadratic_keeps_valves_degenerate_answers() {
        assert_eq!(solve_quadratic(1.0, 0.0, -4.0), Some((2.0, -2.0)));
        // No square term: one root, twice.
        assert_eq!(solve_quadratic(0.0, 2.0, -6.0), Some((3.0, 3.0)));
        // Nothing at all is a solution of nothing.
        assert_eq!(solve_quadratic(0.0, 0.0, 0.0), Some((0.0, 0.0)));
        assert_eq!(solve_quadratic(0.0, 0.0, 1.0), None);
        // Imaginary.
        assert_eq!(solve_quadratic(1.0, 0.0, 4.0), None);
    }

    /// **The acceptance test `portdocs/PORTAL.md` §11 asks for, on the maps
    /// that ship.** Every pair of `prop_portal`s the game places is linked,
    /// carved, and walked into by a player-sized hull; the assertion is that
    /// the player comes out of the other one.
    ///
    /// The pairing is by entity order within a map rather than through the
    /// server's linker, because what is under test is the *movement* — that
    /// every shipped pair spawns and links is
    /// `every_shipped_portal_spawns_and_its_map_can_link_a_pair`'s job, and
    /// running the whole entity system here would make a failure ambiguous.
    ///
    /// A pair is **skipped** when the entrance has no floor within 300 units,
    /// or when the player cannot stand 40 units in front of it — four of the
    /// game's twenty-one portals are parked in mid-air by their map and moved
    /// from script, and some of the rest face across a gap.
    ///
    /// ```text
    /// KISAK_GAME_DIR=/path/to/portal2 cargo test --release walks_through -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a Portal 2 install; set KISAK_GAME_DIR"]
    fn a_player_walks_through_every_shipped_portal_pair() {
        use crate::engine::trace::{LivePortal, PortalLink, Ray};
        use crate::engine::world::bsp::Bsp;
        use crate::server::classes::portal::teleport_matrix;

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

        let (mut pairs, mut walked, mut skipped, mut blocked) = (0usize, 0usize, 0usize, 0usize);
        let mut mismatched = 0usize;
        // How each pair tilts the player's up axis — the number the transition
        // ramp exists for. `|m[2][2]|` is `ShouldPortalTransitionCrouch`'s own
        // quantity: 1 means up stays up, below `cos 30°` means an AABB cannot
        // make the trip standing, and *between* the two is the case Valve
        // calls "slightly angled" and builds `pAABBAngleTransformCollideable`
        // to rescue.
        let (mut flat, mut angled, mut crouching) = (0usize, 0usize, 0usize);
        let (mut carved, mut worst) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
        let (mut pieces, mut tube, mut remote) = (0usize, 0usize, 0usize);
        let mut report: Vec<String> = Vec::new();

        for name in &names {
            let bsp = Bsp::load(&vfs, name).expect("a shipped map parses");
            let entities = bsp.entities();
            let placed: Vec<_> = entities
                .iter()
                .filter(|e| e.classname() == Some("prop_portal"))
                .collect();
            if placed.len() < 2 {
                continue;
            }
            let collision = CollisionBsp::build(&bsp);

            for two in placed.chunks(2) {
                let [entrance, exit] = two else { continue };
                let place = |e: &crate::engine::world::bsp::Entity| {
                    (vector(e.get("origin")), vector(e.get("angles")))
                };
                let (a, b) = (place(entrance), place(exit));
                let size = |e: &crate::engine::world::bsp::Entity| {
                    (
                        number(e.get("HalfWidth"), 32.0),
                        number(e.get("HalfHeight"), 56.0),
                    )
                };
                if size(entrance) != size(exit) {
                    // `UpdatePortalLinkage` will not pair two portals of
                    // different sizes, so neither does this.
                    mismatched += 1;
                    continue;
                }
                let (half_width, half_height) = size(entrance);
                let hole = |(origin, angles): (Vec3, Vec3)| {
                    PortalHole::new(origin, angles, half_width, half_height)
                };
                let link = |from: (Vec3, Vec3), to: (Vec3, Vec3), exit_id: u64| {
                    Some(PortalLink {
                        exit_id,
                        exit: hole(to),
                        to_exit: teleport_matrix(from, to),
                        to_entrance: teleport_matrix(to, from),
                    })
                };
                pairs += 1;
                let tilt = teleport_matrix(a, b).z_axis.z.abs();
                match tilt {
                    _ if tilt > 0.9999 => flat += 1,
                    _ if tilt < COS_PI_OVER_SIX => crouching += 1,
                    _ => angled += 1,
                }

                let live = [
                    LivePortal {
                        id: 1,
                        hole: hole(a),
                        link: link(a, b, 2),
                    },
                    LivePortal {
                        id: 2,
                        hole: hole(b),
                        link: link(b, a, 1),
                    },
                ];
                let mut holes = PortalHoles::default();
                let started = std::time::Instant::now();
                holes.sync(&collision, &live);
                let took = started.elapsed();
                carved += took;
                worst = worst.max(took);
                for wall in holes.iter() {
                    pieces += wall.pieces();
                    tube += wall.tube_slabs();
                    remote += wall.remote_pieces();
                }
                let blue = *holes.get(1).expect("carved").hole();

                // Somewhere to stand: 40 units out in front, dropped onto
                // whatever is below.
                let from = blue.center + blue.forward * 40.0;
                let mins = player_mins(false);
                let maxs = player_maxs(false);
                let down = Ray::hull(from, from - Vec3::Z * 300.0, mins, maxs);
                let ground = collision.tracer().trace(&down, Contents::MASK_PLAYERSOLID);
                if ground.start_solid || !ground.did_hit() {
                    skipped += 1;
                    report.push(format!("  {name}: nowhere to stand in front of the portal"));
                    continue;
                }

                let mut mv = walker(ground.end + Vec3::Z * 2.0);
                // Looking into the portal, which is the way its forward points
                // back.
                let into = -blue.forward;
                mv.angles = ViewAngles::new(0.0, into.y.atan2(into.x).to_degrees());
                let settle = run_portals(&mut mv, &collision, &holes, 30, |_| {});
                if !settle.is_empty() || mv.ground.is_none() {
                    skipped += 1;
                    report.push(format!(
                        "  {name}: the player would not settle in front of it"
                    ));
                    continue;
                }

                let teleports = run_portals(&mut mv, &collision, &holes, 180, |mv| {
                    mv.forwardmove = SV_SPEED_NORMAL;
                });
                match teleports.first() {
                    Some(teleport) => {
                        walked += 1;
                        // **Either portal may be the entrance.** Some maps put
                        // the pair close enough together that a player walking
                        // at one is nearer the other, and `select_portal` takes
                        // the nearest centre — which is Valve's rule. What has
                        // to hold is that they came out of the *partner*.
                        assert_ne!(
                            teleport.entered, teleport.exit,
                            "{name}: a portal teleported into itself"
                        );
                        let exit = hole(match teleport.exit {
                            1 => a,
                            _ => b,
                        });
                        let centre = mv.origin + (mins + maxs) * 0.5;
                        let ahead = exit.forward.dot(centre - exit.center);
                        assert!(
                            ahead > 0.0,
                            "{name}: came out {ahead} units behind the exit plane"
                        );
                    }
                    None => {
                        blocked += 1;
                        report.push(format!(
                            "  {name}: walked {:.1} units and did not go through",
                            (mv.origin - (ground.end + Vec3::Z * 2.0)).length()
                        ));
                    }
                }
            }
        }

        for line in &report {
            println!("{line}");
        }
        println!(
            "{pairs} portal pairs across the shipped maps: {walked} walked through, \
             {blocked} stopped short, {skipped} with nowhere to stand; \
             {mismatched} adjacent pairs were different sizes.\n  \
             {pieces} carved pieces, {tube} tube slabs and {remote} remote pieces \
             between them; carving a linked pair took {:.2} ms on average and \
             {:.2} ms at worst.\n  \
             {flat} keep the up axis, {crouching} turn it far enough to force a \
             crouch, {angled} land in between — the transition ramp's case.",
            carved.as_secs_f32() * 1000.0 / pairs.max(1) as f32,
            worst.as_secs_f32() * 1000.0,
        );
        // Nine pairs out of the game's twenty-one portals, and the arithmetic
        // is the content's: `sp_a1_intro5` and `sp_a1_intro7` place a single
        // `prop_portal` each and `sp_a1_intro4` places three, so three portals
        // have no neighbour to pair with. No adjacent pair is mismatched in
        // size, which is worth knowing because `UpdatePortalLinkage` would not
        // have linked one that was.
        assert_eq!(pairs, 9, "the pairing changed");
        assert!(
            walked >= 6,
            "only {walked} of {pairs} shipped pairs could be walked through"
        );
    }
}
