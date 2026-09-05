//! Turning a command into a position. `game/shared/gamemovement.cpp`.
//!
//! Stage 1 is `FullNoClipMove` (`:2525`) and the `Accelerate` (`:2075`) it
//! shares with walking. `FullWalkMove` (`:2287`) is stage 4 and needs `trace/`.
//!
//! # This file is shared code
//!
//! `gamemovement.cpp` compiles into both the client and the server binaries,
//! and the same command must produce the same position on both or prediction
//! mispredicts. So **nothing here may assume a client**: no cvar handles, no
//! console, no view. Everything it reads arrives in [`MoveData`], which is
//! `CMoveData` and is exactly the interface Valve chose for the same reason.
//!
//! Where that shared code eventually lives — a `src/game/` shared with
//! `src/server/`, or a `pub(crate)` module — is deliberately not decided yet
//! (`portdocs/CLIENT.md` §10).

use glam::Vec3;

use super::{ButtonBits, ViewAngles};

/// `sv_maxspeed` (`movevars_shared.cpp:29`). `FullNoClipMove` clamps the wish
/// velocity to this times the noclip factor.
pub const SV_MAXSPEED: f32 = 320.0;

/// `sv_friction` (`movevars_shared.cpp:44`) — 5.2, not the 4.0 older Source
/// branches ship.
pub const SV_FRICTION: f32 = 5.2;

/// `sv_stopspeed` (`movevars_shared.cpp:23`). Read by walking, not by noclip;
/// registered at stage 1 so that a `.cfg` setting it is not reported unknown.
pub const SV_STOPSPEED: f32 = 80.0;

/// `sv_noclipspeed` (`movevars_shared.cpp:25`): the multiplier
/// `CGameMovement::PlayerMove` hands `FullNoClipMove` (`:5093`).
pub const SV_NOCLIPSPEED: f32 = 5.0;

/// `sv_noclipaccelerate` (`movevars_shared.cpp:24`).
///
/// **Not zero**, which is the difference between this and the placeholder
/// camera it replaces: the shipped game accelerates and bleeds off speed with
/// friction. Set it to 0 for the camera's old instant-stop feel — that is what
/// the cvar is for.
pub const SV_NOCLIPACCELERATE: f32 = 5.0;

/// `CMoveData` (`game/shared/imovehelper.h`), reduced to what noclip reads.
///
/// In and out: [`full_noclip_move`] takes it by `&mut` and the caller copies
/// the results back onto the player, which is `ProcessMovement`'s
/// `SetupMove`/`FinishMove` bracket (`gamemovement.cpp:1325`) with the
/// bookkeeping that only matters across a network removed.
#[derive(Debug, Clone, Copy)]
pub struct MoveData {
    /// The player's **feet**, moved in place.
    pub origin: Vec3,
    /// Carried in and out; noclip's acceleration path is stateful.
    pub velocity: Vec3,
    /// `m_vecViewAngles` — the angles the command carried, which is what the
    /// movement basis comes from.
    pub angles: ViewAngles,
    pub forwardmove: f32,
    pub sidemove: f32,
    pub upmove: f32,
    pub buttons: ButtonBits,
    /// `sv_maxspeed`.
    pub max_speed: f32,
    /// `sv_friction`.
    pub friction: f32,
}

/// `CGameMovement::Accelerate` (`gamemovement.cpp:2075`).
///
/// **This branch's version, not the classic one.** Every older Source release
/// scales the acceleration by `wishspeed`; `cstrike15` scales it by
/// `MAX( 250, wishspeed )`, so a slow wish still accelerates at the rate of a
/// 250-unit one. Copying the older formula would make low-speed movement feel
/// sluggish in a way that is very hard to attribute afterwards.
///
/// `player->m_surfaceFriction` is a factor here in the original; it is 1.0
/// except on low-friction surfaces, and there are no surfaces yet.
pub fn accelerate(mv: &mut MoveData, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32) {
    // See if we are changing direction a bit.
    let currentspeed = mv.velocity.dot(wishdir);

    // Reduce wishspeed by the amount of veer.
    let addspeed = wishspeed - currentspeed;
    if addspeed <= 0.0 {
        return;
    }

    let acceleration_scale = wishspeed.max(250.0);
    let accelspeed = (accel * dt * acceleration_scale).min(addspeed);

    mv.velocity += wishdir * accelspeed;
}

/// `CGameMovement::FullNoClipMove` (`gamemovement.cpp:2525`).
///
/// `factor` is `sv_noclipspeed` and `max_acceleration` is
/// `sv_noclipaccelerate`, exactly as `PlayerMove` passes them (`:5093`).
///
/// Four details that look like mistakes and are not:
///
/// - **`max_speed` is computed from the unhalved factor**, before `+speed`
///   halves it, so walking never reaches the clamp.
/// - **`upmove` goes on world `+Z`**, added after the forward/right terms, so
///   looking down does not tilt which way "up" is.
/// - **A velocity under one unit per second stops the player and returns
///   early**, skipping the position update for that frame.
/// - **A negative `max_acceleration` zeroes the velocity after moving**, which
///   is the "no accel" mode; zero takes the straight-to-`wishvel` branch
///   instead. Three behaviours from one float.
pub fn full_noclip_move(mv: &mut MoveData, dt: f32, factor: f32, max_acceleration: f32) {
    let mut factor = factor;
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

    if max_acceleration > 0.0 {
        accelerate(mv, wishdir, wishspeed, max_acceleration, dt);

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
        let drop = control * mv.friction * dt;
        let newspeed = (speed - drop).max(0.0);
        mv.velocity *= newspeed / speed;
    } else {
        mv.velocity = wishvel;
    }

    // Just move — don't clip or anything.
    mv.origin += mv.velocity * dt;

    if max_acceleration < 0.0 {
        mv.velocity = Vec3::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving(forwardmove: f32, sidemove: f32, upmove: f32) -> MoveData {
        MoveData {
            origin: Vec3::ZERO,
            velocity: Vec3::ZERO,
            angles: ViewAngles::new(0.0, 0.0),
            forwardmove,
            sidemove,
            upmove,
            buttons: ButtonBits::NONE,
            max_speed: SV_MAXSPEED,
            friction: SV_FRICTION,
        }
    }

    /// Within a quarter of a unit, which is finer than anything visible.
    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 0.25
    }

    /// `sv_noclipaccelerate 0` is the straight-to-`wishvel` branch, and it is
    /// the one with an arithmetic answer: one second at 175 units of
    /// `forwardmove` times a factor of 5 is 875 units along the view.
    #[test]
    fn without_acceleration_the_wish_velocity_is_the_velocity() {
        let mut mv = moving(175.0, 0.0, 0.0);
        full_noclip_move(&mut mv, 1.0, SV_NOCLIPSPEED, 0.0);
        assert!(close(mv.origin, Vec3::X * 875.0), "{:?}", mv.origin);
    }

    #[test]
    fn strafing_right_moves_along_negative_y_when_facing_positive_x() {
        let mut mv = moving(0.0, 175.0, 0.0);
        full_noclip_move(&mut mv, 1.0, SV_NOCLIPSPEED, 0.0);
        assert!(mv.origin.y < 0.0, "{:?}", mv.origin);
        assert!(mv.origin.x.abs() < 0.25);
    }

    #[test]
    fn rising_is_along_world_up_whatever_the_view_is_doing() {
        let mut mv = moving(0.0, 0.0, 320.0);
        mv.angles = ViewAngles::new(60.0, 30.0);
        full_noclip_move(&mut mv, 1.0, SV_NOCLIPSPEED, 0.0);
        assert!(close(mv.origin, Vec3::Z * 1600.0), "{:?}", mv.origin);
    }

    #[test]
    fn the_wish_velocity_is_clamped_to_the_server_maximum() {
        // Forward and up together are 2,475 units a second unclamped.
        let mut mv = moving(175.0, 0.0, 320.0);
        full_noclip_move(&mut mv, 1.0, SV_NOCLIPSPEED, 0.0);
        let travelled = mv.origin.length();
        assert!(
            (travelled - SV_MAXSPEED * SV_NOCLIPSPEED).abs() < 0.25,
            "{travelled}"
        );
    }

    /// `+speed` halves the factor *after* the clamp is computed from the
    /// unhalved one, so walking never reaches the clamp — and the walk is
    /// exactly half the run.
    #[test]
    fn walking_halves_the_speed() {
        let mut fast = moving(175.0, 0.0, 0.0);
        full_noclip_move(&mut fast, 1.0, SV_NOCLIPSPEED, 0.0);

        let mut slow = moving(175.0, 0.0, 0.0);
        slow.buttons = ButtonBits::SPEED;
        full_noclip_move(&mut slow, 1.0, SV_NOCLIPSPEED, 0.0);

        assert!((slow.origin.length() * 2.0 - fast.origin.length()).abs() < 0.25);
    }

    #[test]
    fn nothing_held_leaves_the_player_where_it_was() {
        let mut mv = moving(0.0, 0.0, 0.0);
        mv.origin = Vec3::ONE;
        full_noclip_move(&mut mv, 1.0, SV_NOCLIPSPEED, 0.0);
        assert_eq!(mv.origin, Vec3::ONE, "and no NaN from normalising zero");
    }

    /// The shipped defaults: acceleration builds speed up over several frames
    /// rather than reaching it at once.
    #[test]
    fn with_acceleration_the_first_frame_is_slower_than_the_steady_state() {
        let mut mv = moving(175.0, 0.0, 0.0);
        full_noclip_move(&mut mv, 1.0 / 60.0, SV_NOCLIPSPEED, SV_NOCLIPACCELERATE);
        let first = mv.velocity.length();

        for _ in 0..600 {
            full_noclip_move(&mut mv, 1.0 / 60.0, SV_NOCLIPSPEED, SV_NOCLIPACCELERATE);
        }
        let settled = mv.velocity.length();

        assert!(first > 0.0, "it does start moving");
        assert!(settled > first * 2.0, "{first} -> {settled}");
        assert!(
            settled <= SV_MAXSPEED * SV_NOCLIPSPEED + 0.5,
            "and never exceeds the clamp: {settled}"
        );
    }

    /// Releasing everything coasts to a stop rather than stopping dead, and
    /// the stop is exact — `speed < 1.0` zeroes it rather than leaving the
    /// player drifting a fraction of a unit a second for ever.
    #[test]
    fn releasing_everything_coasts_to_an_exact_stop() {
        let mut mv = moving(175.0, 0.0, 0.0);
        for _ in 0..60 {
            full_noclip_move(&mut mv, 1.0 / 60.0, SV_NOCLIPSPEED, SV_NOCLIPACCELERATE);
        }
        assert!(mv.velocity.length() > 1.0);

        mv.forwardmove = 0.0;
        for _ in 0..600 {
            full_noclip_move(&mut mv, 1.0 / 60.0, SV_NOCLIPSPEED, SV_NOCLIPACCELERATE);
        }
        assert_eq!(mv.velocity, Vec3::ZERO);
    }

    /// `Accelerate` refuses to add speed in a direction the player is already
    /// moving in faster than it asked for — the "reduce wishspeed by the amount
    /// of veer" clause. Without it, holding forward down a slope compounds.
    #[test]
    fn acceleration_does_not_add_to_a_velocity_that_already_exceeds_the_wish() {
        let mut mv = moving(0.0, 0.0, 0.0);
        mv.velocity = Vec3::X * 1000.0;
        accelerate(&mut mv, Vec3::X, 500.0, SV_NOCLIPACCELERATE, 1.0 / 60.0);
        assert_eq!(mv.velocity, Vec3::X * 1000.0);
    }
}
