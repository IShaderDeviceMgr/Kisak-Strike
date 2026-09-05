//! Where the view points, and how the mouse moves it.
//!
//! `game/client/in_mouse.cpp`'s `ScaleMouse` (`:412`) and `ApplyMouse`
//! (`:470`), plus `CInput::ClampAngles` (`in_main.cpp:975`) and
//! `AngleVectors` (`mathlib/mathlib_base.cpp:1027`).
//!
//! # Why the angles are here and not in the engine
//!
//! Valve keeps them in `CClientState::viewangles` (`engine/client.h:193`) and
//! the client DLL reaches them through `engine->GetViewAngles`/`SetViewAngles`
//! (`engine/cdll_engine_int.cpp:1049`, `:1054`) — over a comment reading
//! `// FIXME, move entirely to client .dll`. The DLL boundary that forced the
//! split does not exist here, so `portdocs/CLIENT.md` §4.7 takes the FIXME:
//! the game client owns them and `src/engine/client/`, when it arrives with
//! `net/`, asks rather than keeping a second copy.
//!
//! What that call did on the way through is kept, though —
//! [`normalize`](ViewAngles::normalize) is `SetViewAngles`' `AngleNormalize`,
//! and refusing a non-finite angle is its `IsValid` check.

use glam::Vec3;

/// `sensitivity`'s declared default (`in_mouse.cpp:100`).
///
/// The value is a **cvar**, not a constant, and [`Client`](super::Client) holds
/// the handle. This is only what it is registered with.
pub const SENSITIVITY: f32 = 2.5;

/// `sensitivity`'s clamp (`in_mouse.cpp:100`): `true, 0.0001f, true, 1000`.
///
/// Every mouse-factor cvar in that file carries the same pair — `m_yaw`,
/// `m_pitch`, `m_side`, `m_forward` — so an out-of-range value in a shipped
/// `.cfg` clamps rather than applying.
pub const SENSITIVITY_MIN: f32 = 0.0001;
pub const SENSITIVITY_MAX: f32 = 1000.0;

/// `m_yaw` (`in_mouse.cpp:103`): degrees of yaw per unit of scaled motion.
pub const M_YAW: f32 = 0.022;

/// `m_pitch` (`in_mouse.cpp:59`).
///
/// **This one is a `ConVar_ServerBounded`**, not a plain cvar: with `sv_cheats`
/// off, `GetFloat` returns `±0.022` whatever the stored value is, preserving
/// only the sign so that "reverse mouse" keeps working (`in_mouse.cpp:86`).
/// That is an anti-cheat measure — a large `m_pitch` is a vertical aimbot — and
/// it needs `sv_cheats`, which does not exist yet. Registered as an ordinary
/// cvar for now; the bound is `portdocs/CLIENT.md` §8's stage-4 company.
pub const M_PITCH: f32 = 0.022;

/// `m_side` (`in_mouse.cpp:102`) and `m_forward` (`:104`) — the mouse-as-movement
/// factors, used only while `+strafe` is held. See [`ViewAngles::apply_mouse`].
pub const M_SIDE: f32 = 0.8;
pub const M_FORWARD: f32 = 1.0;

/// `cl_pitchdown` (`in_main.cpp:49`). Pitch is positive *downwards*, so this is
/// how far the view may look at the floor.
pub const CL_PITCHDOWN: f32 = 89.0;

/// `cl_pitchup` (`in_main.cpp:50`), applied as `-cl_pitchup`.
pub const CL_PITCHUP: f32 = 89.0;

/// `ScaleMouse`'s default path (`in_mouse.cpp:459`): the raw delta times
/// `sensitivity`.
///
/// The four `m_customaccel` curves are not ported — per-user feel tuning with
/// no default behaviour — and `m_mousespeed`/`m_mouseaccel1`/`m_mouseaccel2`
/// are Windows `SPI_SETMOUSE` overrides that are inert on POSIX. The HUD's
/// sensitivity override (`GetHud().GetSensitivity()`, used while a HUD element
/// wants slower aim) arrives with the HUD.
///
/// **`dx`/`dy` are raw device units, and they are not equally raw across
/// platforms**: X11 (XI2) and Wayland deliver unaccelerated deltas, macOS
/// delivers `NSEvent.deltaX`, already through the OS pointer-ballistics curve.
/// The same `sensitivity` therefore feels different on macOS. That is recorded
/// rather than corrected: Valve hit it too and answered with convars.
pub fn scale_mouse(dx: f32, dy: f32, sensitivity: f32) -> (f32, f32) {
    (dx * sensitivity, dy * sensitivity)
}

/// Valve's `QAngle`: `(pitch, yaw, roll)` in degrees.
///
/// **Pitch is positive downwards.** That is the sign error to watch for, and it
/// is why [`vectors`](ViewAngles::vectors) negates it and
/// [`apply_mouse`](ViewAngles::apply_mouse) *adds* the mouse's Y.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewAngles {
    pub pitch: f32,
    pub yaw: f32,
    /// Read by [`vectors`](ViewAngles::vectors), although nothing sets it yet.
    ///
    /// **Portal 2 rolls the view** — reorientation gels, portals through
    /// non-vertical surfaces — and `ApplyMouse` has a `#if defined PORTAL`
    /// branch, on by default (`cl_mouselook_roll_compensation`,
    /// `in_mouse.cpp:55`), that rotates the mouse delta by the inverse of the
    /// current roll so that "mouse left" stays "screen left" while upside down.
    /// That branch is in scope for the target game and cannot be exercised
    /// until something rolls the view; this field is where it will attach.
    pub roll: f32,
}

impl ViewAngles {
    /// Angles as some other subsystem states them — a spawn point, a config.
    ///
    /// Normalizes, because `SetViewAngles` does; does **not** clamp pitch,
    /// because clamping is `ApplyMouse`'s and needs cvars this has no business
    /// reading. The first command clamps it.
    pub fn new(pitch: f32, yaw: f32) -> ViewAngles {
        let mut angles = ViewAngles {
            pitch,
            yaw,
            roll: 0.0,
        };
        angles.normalize();
        angles
    }

    /// `CEngineClient::SetViewAngles` (`cdll_engine_int.cpp:1054`):
    /// `AngleNormalize` on each component, and a non-finite angle is refused
    /// rather than stored.
    ///
    /// Valve warns and zeroes the whole thing; this zeroes the offending
    /// component, because two thirds of a view direction is better than none
    /// and there is no way for a caller to notice either way.
    pub fn normalize(&mut self) {
        for angle in [&mut self.pitch, &mut self.yaw, &mut self.roll] {
            *angle = match angle.is_finite() {
                true => (*angle + 180.0).rem_euclid(360.0) - 180.0,
                false => 0.0,
            };
        }
    }

    /// `ApplyMouse` (`in_mouse.cpp:470`), split the way the original splits it.
    ///
    /// `mouse_x`/`mouse_y` have already been through
    /// [`scale_mouse`](scale_mouse). The two axes are separate `if` blocks in
    /// the original and are two methods here, because `+strafe` and
    /// `lookstrafe` split them: with `lookstrafe` set, horizontal motion
    /// becomes `sidemove` while vertical still turns the view, and with
    /// `+strafe` held neither axis turns anything. That half reads and writes a
    /// [`UserCmd`](super::UserCmd), so it lives in
    /// [`Client::mouse_move`](super::Client), where both are in scope.
    ///
    /// Yaw *decreases* to the right: facing `+X` at yaw 0, a right turn faces
    /// `-Y`, which is yaw -90.
    pub fn apply_mouse_yaw(&mut self, mouse_x: f32, m_yaw: f32) {
        self.yaw -= m_yaw * mouse_x;
    }

    /// Pitch *increases* downwards, and the clamp is inline here in the
    /// original (`in_mouse.cpp:577`) rather than in `ClampAngles` — kept there
    /// so the two callers stay distinguishable.
    pub fn apply_mouse_pitch(&mut self, mouse_y: f32, m_pitch: f32, down: f32, up: f32) {
        self.pitch += m_pitch * mouse_y;
        self.clamp_pitch(down, up);
    }

    /// `CInput::ClampAngles` (`in_main.cpp:975`) is not here yet: its callers
    /// are `AdjustAngles` and keyboard look, which are stage 3. What is
    /// reachable now is its pitch half, applied inline by
    /// [`apply_mouse_pitch`](ViewAngles::apply_mouse_pitch) exactly as
    /// `ApplyMouse` applies it (`in_mouse.cpp:577`), and
    /// [`normalize`](ViewAngles::normalize) for the yaw wrap.
    ///
    /// **Roll is deliberately not clamped when that arrives.** Every other
    /// Source game limits it to ±50 degrees; Portal excludes itself with a
    /// comment that says why — *"Don't constrain Roll in Portal because the
    /// player can be upside down! -Jeep"*.
    fn clamp_pitch(&mut self, pitch_down: f32, pitch_up: f32) {
        if self.pitch > pitch_down {
            self.pitch = pitch_down;
        }
        if self.pitch < -pitch_up {
            self.pitch = -pitch_up;
        }
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
        let right = Vec3::new(-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp);
        let up = Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp);

        (forward, right, up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Within a quarter of a unit, which is finer than anything visible and
    /// coarser than the difference between `sin_cos` and Valve's `SinCos`.
    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 0.25
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
        // `mathlib`'s `forward.z = -sp`. If this ever reads `Vec3::Z`, the view
        // looks at the ceiling when it should look at the floor.
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
        let (x, _) = scale_mouse(100.0, 0.0, SENSITIVITY);
        angles.apply_mouse_yaw(x, M_YAW);
        assert!(angles.yaw < 0.0);
        assert!((angles.yaw + M_YAW * SENSITIVITY * 100.0).abs() < 1e-4);
    }

    #[test]
    fn moving_the_mouse_down_looks_down() {
        let mut angles = ViewAngles::new(0.0, 0.0);
        let (_, y) = scale_mouse(0.0, 40.0, SENSITIVITY);
        angles.apply_mouse_pitch(y, M_PITCH, CL_PITCHDOWN, CL_PITCHUP);
        assert!(angles.pitch > 0.0, "pitch is positive downwards");
    }

    #[test]
    fn pitch_clamps_at_the_poles() {
        let mut angles = ViewAngles::new(0.0, 0.0);
        angles.apply_mouse_pitch(100_000.0, M_PITCH, CL_PITCHDOWN, CL_PITCHUP);
        assert_eq!(angles.pitch, CL_PITCHDOWN);
        angles.apply_mouse_pitch(-1_000_000.0, M_PITCH, CL_PITCHDOWN, CL_PITCHUP);
        assert_eq!(angles.pitch, -CL_PITCHUP);
    }

    #[test]
    fn yaw_wraps_rather_than_growing_without_bound() {
        let mut angles = ViewAngles::new(0.0, 0.0);
        for _ in 0..100 {
            angles.apply_mouse_yaw(-1000.0, M_YAW);
            angles.normalize();
        }
        assert!(angles.yaw > -180.0 && angles.yaw <= 180.0, "{}", angles.yaw);
    }

    /// `SetViewAngles`' `IsValid` check. A NaN reaching the view matrix is a
    /// black screen with no error, which is the worst kind.
    #[test]
    fn a_non_finite_angle_is_refused_rather_than_stored() {
        let angles = ViewAngles::new(f32::NAN, f32::INFINITY);
        assert_eq!(angles.pitch, 0.0);
        assert_eq!(angles.yaw, 0.0);
    }
}
