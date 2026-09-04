//! Where the view points, and the placeholder that moves it.
//!
//! This is `portdocs/ENGINE_INPUT.md`'s third layer — `game/client/in_main.cpp`
//! and `in_mouse.cpp` — reduced to the part that can exist before there is a
//! player.
//!
//! # What is faithful, and what is a placeholder
//!
//! [`ViewAngles`] is the real thing: `ApplyMouse`'s scale-and-apply
//! (`in_mouse.cpp:470`), `ClampAngles`' pitch limits (`in_main.cpp:975`) and
//! `AngleVectors`' basis (`mathlib/mathlib_base.cpp:1027`), with Valve's
//! shipped constants.
//!
//! [`FlyCamera`] is not. A real Source client turns held buttons into a
//! `CUserCmd` through `kbutton_t`'s fractional `KeyState` model and hands it
//! to `CGameMovement`; there is no player, no entity and no prediction to hand
//! it to, so this integrates a velocity into a position and calls it a camera.
//! Its speeds are `FullNoClipMove`'s (`gamemovement.cpp:2525`) because noclip
//! is the movement mode it is imitating, and **it deletes the turntable** that
//! stood in for input before it.
//!
//! Do not grow this into `CUserCmd`. `kbutton_t`'s `down[2]` array and the
//! fractional key state are correct and are the right design — they are why a
//! 30 Hz frame does not swallow a fast tap — but they belong to `client/`, and
//! building them against a camera instead of a player would bake in the wrong
//! consumer (`portdocs/ENGINE_INPUT.md` §1, §4.4).

use glam::Vec3;

use super::{Button, Input, Key};

/// `sensitivity` (`in_mouse.cpp:100`), clamped there to `0.0001..1000`.
pub const SENSITIVITY: f32 = 2.5;

/// `m_yaw` (`in_mouse.cpp:103`): degrees of yaw per unit of scaled motion.
const M_YAW: f32 = 0.022;

/// `m_pitch` (`in_mouse.cpp:63`).
const M_PITCH: f32 = 0.022;

/// `cl_pitchdown` (`in_main.cpp:49`). Pitch is positive *downwards*, so this
/// is how far the view may look at the floor.
const CL_PITCHDOWN: f32 = 89.0;

/// `cl_pitchup` (`in_main.cpp:50`), applied as `-cl_pitchup`.
const CL_PITCHUP: f32 = 89.0;

/// `cl_forwardspeed`/`cl_sidespeed` (`in_main.cpp:61`), which are
/// `MAX_LINEAR_SPEED` — **175 under `PORTAL2`**, where every other Source game
/// gets 450.
const CL_FORWARDSPEED: f32 = 175.0;
const CL_SIDESPEED: f32 = 175.0;

/// `cl_upspeed` (`in_main.cpp:51`). Applied along world `+Z`, as
/// `FullNoClipMove` applies `m_flUpMove`.
const CL_UPSPEED: f32 = 320.0;

/// `sv_noclipspeed` (`movevars_shared.cpp:26`), the multiplier
/// `CGameMovement::FullNoClipMove` is handed.
const SV_NOCLIPSPEED: f32 = 5.0;

/// `sv_maxspeed` (`movevars_shared.cpp:30`); `FullNoClipMove` clamps the wish
/// velocity to `sv_maxspeed * factor`.
const SV_MAXSPEED: f32 = 320.0;

/// Valve's `QAngle`: `(pitch, yaw, roll)` in degrees.
///
/// **Pitch is positive downwards.** That is the sign error to watch for, and
/// it is why [`vectors`](ViewAngles::vectors) negates it and
/// [`apply_mouse`](ViewAngles::apply_mouse) *adds* the mouse's Y.
///
/// These live here as a **known wart**. They belong to `CClientState`
/// (`engine/cdll_engine_int.cpp:1050`) and should move to `client/` when it
/// exists; until then the only alternative is `engine/mod.rs`, which would
/// spread the same code over two modules instead of one
/// (`portdocs/ENGINE_INPUT.md` §11.2).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewAngles {
    pub pitch: f32,
    pub yaw: f32,
    /// Kept, and read by [`vectors`](ViewAngles::vectors), although nothing
    /// sets it yet.
    ///
    /// **Portal 2 rolls the view** — reorientation gels, portals through
    /// non-vertical surfaces — and `ApplyMouse` has a `#if defined PORTAL`
    /// branch, on by default (`cl_mouselook_roll_compensation`), that rotates
    /// the mouse delta by the inverse of the current roll so that "mouse left"
    /// stays "screen left" while upside down. That branch is in scope for the
    /// target game and cannot be exercised until something rolls the view;
    /// this field is where it will attach.
    pub roll: f32,
}

impl ViewAngles {
    pub fn new(pitch: f32, yaw: f32) -> ViewAngles {
        let mut angles = ViewAngles {
            pitch,
            yaw,
            roll: 0.0,
        };
        angles.clamp();
        angles
    }

    /// `ScaleMouse` then `ApplyMouse` (`in_mouse.cpp:412`, `:470`).
    ///
    /// Valve scales by `sensitivity` in the first and multiplies by
    /// `m_yaw`/`m_pitch` in the second; the two are one multiply here because
    /// the intermediate had no other reader. The four custom-acceleration
    /// curves (`m_customaccel` 1-4) are not ported: they are per-user feel
    /// tuning with no default behavior, and `m_mousespeed`/`m_mouseaccel1`/
    /// `m_mouseaccel2` are Windows `SPI_SETMOUSE` overrides that are inert on
    /// POSIX.
    ///
    /// `dx`/`dy` are raw device units, and **they are not equally raw across
    /// platforms**: X11 (XI2) and Wayland (`zwp_relative_pointer_v1`) deliver
    /// unaccelerated deltas, macOS delivers `NSEvent.deltaX` — already through
    /// the OS pointer-ballistics curve. The same `sensitivity` therefore feels
    /// different on macOS, and that is recorded rather than corrected: Valve
    /// hit the same thing and answered it with convars, not with an inverse
    /// curve.
    pub fn apply_mouse(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.yaw -= M_YAW * sensitivity * dx;
        self.pitch += M_PITCH * sensitivity * dy;
        self.clamp();
    }

    /// `CInput::ClampAngles` (`in_main.cpp:975`), plus a yaw wrap Valve does
    /// not do.
    ///
    /// **Roll is deliberately not clamped.** Every other Source game limits it
    /// to +-50 degrees; Portal excludes itself from that with a comment that
    /// says why — "Don't constrain Roll in Portal because the player can be
    /// upside down! -Jeep".
    ///
    /// The yaw wrap is this port's: Valve leaves yaw to grow without bound and
    /// lets `CUserCmd`'s 16-bit angle quantisation fold it, which is a network
    /// path that does not exist here. An `f32` yaw that grows all session
    /// loses precision where nothing ever wraps it.
    fn clamp(&mut self) {
        self.pitch = self.pitch.clamp(-CL_PITCHUP, CL_PITCHDOWN);
        self.yaw = (self.yaw + 180.0).rem_euclid(360.0) - 180.0;
    }

    /// `AngleVectors` (`mathlib/mathlib_base.cpp:1027`) — forward, right, up.
    ///
    /// Source is **Z-up right-handed**, and "right" is `-Y` when facing `+X`,
    /// which is what makes `+moveright` add `right * cl_sidespeed`.
    pub fn vectors(&self) -> (Vec3, Vec3, Vec3) {
        let (sp, cp) = self.pitch.to_radians().sin_cos();
        let (sy, cy) = self.yaw.to_radians().sin_cos();
        let (sr, cr) = self.roll.to_radians().sin_cos();

        let forward = Vec3::new(cp * cy, cp * sy, -sp);
        let right = Vec3::new(
            -sr * sp * cy + cr * sy,
            -sr * sp * sy - cr * cy,
            -sr * cp,
        );
        let up = Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp);

        (forward, right, up)
    }
}

/// A free-fly camera: the placeholder for the player that does not exist yet.
///
/// It is the smallest thing that makes input worth having — a noclip
/// fly-through — and it is `CViewRender::SetUpView` plus `CUserCmd` plus
/// `CGameMovement` collapsed into eight lines, all three of which arrive with
/// `client/` and the game DLL.
///
/// The keys are **hard-coded**, because bindings are stage 3 and want
/// `console/`: WASD, space and left control to rise and fall, left shift to
/// walk. Those are Portal 2's shipped defaults for `+forward`, `+back`,
/// `+moveleft`, `+moveright`, `+moveup`, `+movedown` and `+speed`; when the
/// binding table lands, this reads it instead.
#[derive(Debug, Clone, Copy)]
pub struct FlyCamera {
    /// The eye, in world units.
    pub origin: Vec3,
    pub angles: ViewAngles,
}

impl FlyCamera {
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> FlyCamera {
        FlyCamera {
            origin,
            angles: ViewAngles::new(pitch, yaw),
        }
    }

    /// Turns the view by one tick's accumulated raw motion.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.angles.apply_mouse(dx, dy, SENSITIVITY);
    }

    /// Moves the view by one tick, from what is held.
    ///
    /// `CGameMovement::FullNoClipMove` (`gamemovement.cpp:2525`) without the
    /// acceleration: `sv_noclipaccelerate` smooths a *player's* velocity
    /// through `Accelerate`, and a camera with momentum is harder to aim a
    /// screenshot with than one without. Everything else is Valve's, including
    /// the detail that `+speed` halves the move factor **after** the speed
    /// clamp is computed from the unhalved one — so walking never reaches the
    /// clamp.
    pub fn step(&mut self, input: &Input, seconds: f32) {
        let held = |key| input.is_down(Button::Key(key));
        let (forward, right, _) = self.angles.vectors();

        let mut wish = Vec3::ZERO;
        if held(Key::W) {
            wish += forward * CL_FORWARDSPEED;
        }
        if held(Key::S) {
            wish -= forward * CL_FORWARDSPEED;
        }
        if held(Key::D) {
            wish += right * CL_SIDESPEED;
        }
        if held(Key::A) {
            wish -= right * CL_SIDESPEED;
        }
        // Along world `+Z`, not along `up`: `FullNoClipMove` adds `m_flUpMove`
        // to `wishvel[2]` after the forward/right terms, so looking down does
        // not tilt which way "up" is.
        if held(Key::Space) {
            wish.z += CL_UPSPEED;
        }
        if held(Key::LeftControl) {
            wish.z -= CL_UPSPEED;
        }

        let mut factor = SV_NOCLIPSPEED;
        let max_speed = SV_MAXSPEED * factor;
        if held(Key::LeftShift) {
            factor /= 2.0;
        }

        let mut velocity = wish * factor;
        let speed = velocity.length();
        if speed > max_speed {
            velocity *= max_speed / speed;
        }
        self.origin += velocity * seconds;
    }
}

#[cfg(test)]
mod tests {
    use super::super::Event;
    use super::*;

    /// Within a quarter of a unit, which is finer than anything visible and
    /// coarser than the difference between `sin_cos` and Valve's `SinCos`.
    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 0.25
    }

    fn hold(keys: &[Key]) -> Input {
        let mut input = Input::new();
        for &key in keys {
            input.push(Event::Pressed {
                button: Button::Key(key),
                repeat: false,
            });
        }
        input.frame();
        input
    }

    #[test]
    fn a_zero_angle_looks_down_positive_x() {
        let (forward, right, up) = ViewAngles::new(0.0, 0.0).vectors();
        assert!(close(forward, Vec3::X));
        // Source's "right" is -Y when facing +X. Get this backwards and
        // strafing goes the wrong way while everything else looks correct.
        assert!(close(right, -Vec3::Y));
        assert!(close(up, Vec3::Z));
    }

    #[test]
    fn a_yaw_of_ninety_degrees_faces_positive_y() {
        let (forward, right, _) = ViewAngles::new(0.0, 90.0).vectors();
        assert!(close(forward, Vec3::Y));
        assert!(close(right, Vec3::X));
    }

    #[test]
    fn positive_pitch_looks_down() {
        // `mathlib`'s `forward.z = -sp`. If this ever reads `Vec3::Z`, the
        // view looks at the ceiling when it should look at the floor.
        let (forward, _, _) = ViewAngles::new(89.0, 0.0).vectors();
        assert!(forward.z < -0.999, "{forward:?}");
    }

    #[test]
    fn the_basis_stays_orthonormal_under_roll() {
        let angles = ViewAngles {
            pitch: 20.0,
            yaw: 35.0,
            roll: 45.0,
        };
        let (forward, right, up) = angles.vectors();
        for vector in [forward, right, up] {
            assert!((vector.length() - 1.0).abs() < 1e-4, "{vector:?}");
        }
        assert!(forward.dot(right).abs() < 1e-4);
        assert!(forward.dot(up).abs() < 1e-4);
        assert!(right.dot(up).abs() < 1e-4);
    }

    #[test]
    fn moving_the_mouse_right_turns_right() {
        // Yaw *decreases* to the right: facing +X at yaw 0, a right turn faces
        // -Y, which is yaw -90.
        let mut angles = ViewAngles::new(0.0, 0.0);
        angles.apply_mouse(100.0, 0.0, SENSITIVITY);
        assert!(angles.yaw < 0.0);
        assert!((angles.yaw + M_YAW * SENSITIVITY * 100.0).abs() < 1e-4);
    }

    #[test]
    fn moving_the_mouse_down_looks_down() {
        let mut angles = ViewAngles::new(0.0, 0.0);
        angles.apply_mouse(0.0, 40.0, SENSITIVITY);
        assert!(angles.pitch > 0.0, "pitch is positive downwards");
    }

    #[test]
    fn pitch_clamps_at_the_poles() {
        let mut angles = ViewAngles::new(0.0, 0.0);
        angles.apply_mouse(0.0, 100_000.0, SENSITIVITY);
        assert_eq!(angles.pitch, CL_PITCHDOWN);
        angles.apply_mouse(0.0, -1_000_000.0, SENSITIVITY);
        assert_eq!(angles.pitch, -CL_PITCHUP);
    }

    #[test]
    fn yaw_wraps_rather_than_growing_without_bound() {
        let mut angles = ViewAngles::new(0.0, 0.0);
        for _ in 0..100 {
            angles.apply_mouse(-1000.0, 0.0, SENSITIVITY);
        }
        assert!(angles.yaw > -180.0 && angles.yaw <= 180.0, "{}", angles.yaw);
    }

    #[test]
    fn holding_forward_moves_along_the_view() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        camera.step(&hold(&[Key::W]), 1.0);
        assert!(close(
            camera.origin,
            Vec3::X * CL_FORWARDSPEED * SV_NOCLIPSPEED
        ));
    }

    #[test]
    fn strafing_right_moves_along_negative_y_when_facing_positive_x() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        camera.step(&hold(&[Key::D]), 1.0);
        assert!(camera.origin.y < 0.0, "{:?}", camera.origin);
        assert!(camera.origin.x.abs() < 0.25);
    }

    #[test]
    fn rising_is_along_world_up_whatever_the_view_is_doing() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 60.0, 30.0);
        camera.step(&hold(&[Key::Space]), 1.0);
        assert!(close(camera.origin, Vec3::Z * CL_UPSPEED * SV_NOCLIPSPEED));
    }

    #[test]
    fn walking_halves_the_speed() {
        let mut fast = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        fast.step(&hold(&[Key::W]), 1.0);
        let mut slow = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        slow.step(&hold(&[Key::W, Key::LeftShift]), 1.0);
        assert!((slow.origin.length() * 2.0 - fast.origin.length()).abs() < 0.25);
    }

    #[test]
    fn opposite_keys_cancel() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        camera.step(&hold(&[Key::W, Key::S, Key::A, Key::D]), 1.0);
        assert_eq!(camera.origin, Vec3::ZERO);
    }

    #[test]
    fn nothing_held_is_a_camera_that_does_not_drift() {
        let mut camera = FlyCamera::new(Vec3::ONE, 10.0, 20.0);
        camera.step(&hold(&[]), 1.0);
        assert_eq!(camera.origin, Vec3::ONE, "and no NaN from normalising zero");
    }

    #[test]
    fn the_wish_velocity_is_clamped_to_the_server_maximum() {
        // Forward and up together are 2,022 units a second unclamped;
        // `FullNoClipMove` clamps to `sv_maxspeed * factor`.
        let mut camera = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        camera.step(&hold(&[Key::W, Key::Space]), 1.0);
        let travelled = camera.origin.length();
        assert!(
            (travelled - SV_MAXSPEED * SV_NOCLIPSPEED).abs() < 0.25,
            "{travelled}"
        );
    }
}
