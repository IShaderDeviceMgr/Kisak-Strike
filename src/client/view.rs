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

/// `cl_yawspeed` (`in_main.cpp:47`) and `cl_pitchspeed` (`:48`): degrees per
/// second for **keyboard** look — `+left`/`+right` and `+lookup`/`+lookdown`.
pub const CL_YAWSPEED: f32 = 210.0;
pub const CL_PITCHSPEED: f32 = 225.0;

/// `cl_anglespeedkey` (`in_main.cpp:46`): what `+speed` multiplies keyboard
/// turn speed by. It is **0.67, not 0.5** — `+speed` walks at half speed but
/// turns at two thirds.
pub const CL_ANGLESPEEDKEY: f32 = 0.67;

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

/// `VIEW_NEARZ` (`game/client/view.h:27`).
pub const VIEW_NEARZ: f32 = 7.0;

/// `r_mapextents`' default (`view.cpp:119`), "the max dimension for the map".
///
/// **It is a plain cheat cvar and nothing sets it from the `.bsp`** — the name
/// suggests otherwise and the porting plan originally assumed otherwise. A
/// mapper who needs a bigger far plane sets it; the far plane is
/// `r_mapextents × √3`, the diagonal of a cube that size, which is the furthest
/// two points in a map that big can be apart.
pub const R_MAPEXTENTS: f32 = 16384.0;

/// √3 — `1.73205080757f` at `view.cpp:645`, spelled out there rather than
/// derived, so it is spelled out here.
pub const MAP_DIAGONAL: f32 = 1.732_050_8;

/// The aspect ratio Valve's field-of-view numbers are quoted at.
///
/// `default_fov` 75 is a **4:3 horizontal** FOV; `CViewRender::Render`
/// (`view.cpp:1084`) widens it for the real screen. See
/// [`scale_fov_by_width_ratio`].
pub const FOV_ASPECT: f32 = 4.0 / 3.0;

/// `ScaleFOVByWidthRatio` (`view.cpp:923`): `2·atan(tan(fov/2) · ratio)`.
///
/// **This is the function that decides how much of the world you see**, and
/// leaving it out is a mistake you can look straight at without noticing. FOV
/// numbers in Source are horizontal and quoted at 4:3; `Render` scales them by
/// `aspect / (4/3)` before the projection is built (`view.cpp:1084`). The
/// composition is the classic Hor+ behaviour — the *vertical* FOV comes out
/// constant at `2·atan(tan(fov/2) · 0.75)` and the horizontal grows with the
/// screen.
///
/// Skip it and 75 goes straight into a `PerspectiveX`, which at 16:9 gives a
/// 46.7° vertical FOV where the shipped game gives 59.9°. The picture is not
/// obviously wrong — it is a plausible, slightly-too-narrow view.
pub fn scale_fov_by_width_ratio(fov_degrees: f32, ratio: f32) -> f32 {
    let half_angle = fov_degrees.to_radians() * 0.5;
    (half_angle.tan() * ratio).atan().to_degrees() * 2.0
}

/// `GetScreenAspect` (`engine/gl_rmain.cpp:127`) — the **physical** aspect
/// ratio of the screen, which is what both the FOV scaling and the projection
/// use (`view.cpp:1084`, `:1106`).
///
/// Two of Valve's terms are deliberately missing, and they coincide with this
/// one on every square-pixel display:
///
/// - `AspectRatioInfo_t::m_flFrameBuffertoPhysicalScalar`, which corrects for
///   non-square pixels — 1280×1024 shown on a physically 4:3 monitor. It is the
///   material system's, and `src/materials/` has no counterpart yet.
/// - The `r_aspectratio` override (`gl_rmain.cpp:46`), which is a *renderer*
///   cvar; registering it from the game client to read it here would put it in
///   the wrong module. It arrives with `render/`.
pub fn screen_aspect(width: u32, height: u32) -> f32 {
    match height {
        0 => 1.0,
        height => width as f32 / height as f32,
    }
}

/// `CViewSetup` (`public/view_shared.h:44`), reduced to what a single
/// perspective view of a world needs.
///
/// Valve's carries about fifty fields. The ones left out are all attached to
/// something that does not exist: the viewmodel pair (`fovViewmodel`,
/// `zNearViewmodel`), the ortho box, the custom view and projection matrices
/// that portals and monitors set, the depth-of-field and motion-blur
/// parameters, and `x`/`y`, which are only non-zero for a split-screen inset.
///
/// **This is data, not a camera.** Turning it into a projection matrix is
/// `src/materials/`'s convention to choose — see
/// [`Engine::camera`](crate::engine) — and keeping that on the other side of
/// the boundary is what stops `client/` depending on `wgpu` for the sake of
/// five numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewSetup {
    /// The eye, in world space. `CViewSetup::origin`.
    pub origin: Vec3,
    pub angles: ViewAngles,
    /// **Horizontal**, in degrees, and **already scaled** by
    /// [`scale_fov_by_width_ratio`] — this is the number to hand a
    /// `PerspectiveX`, not `default_fov`.
    pub fov: f32,
    pub z_near: f32,
    pub z_far: f32,
    pub width: u32,
    pub height: u32,
    /// [`screen_aspect`], used for *both* the FOV scaling above and the
    /// projection — Valve sets `m_flAspectRatio` from the same call two lines
    /// after scaling the FOV with it (`view.cpp:1106`).
    pub aspect: f32,
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

    /// `CInput::ClampAngles` (`in_main.cpp:975`), plus the yaw wrap
    /// `SetViewAngles` applies on the way back.
    ///
    /// Called at the end of `AdjustAngles`. The *mouse* path does not go
    /// through here — [`apply_mouse_pitch`](ViewAngles::apply_mouse_pitch)
    /// clamps inline, exactly as `ApplyMouse` does (`in_mouse.cpp:577`) — and
    /// the two are kept separate so that the callers stay distinguishable.
    ///
    /// **Roll is deliberately not clamped.** Every other Source game limits it
    /// to ±50 degrees; Portal excludes itself with a comment that says why —
    /// *"Don't constrain Roll in Portal because the player can be upside down!
    /// -Jeep"*.
    pub fn clamp(&mut self, pitch_down: f32, pitch_up: f32) {
        self.clamp_pitch(pitch_down, pitch_up);
        self.normalize();
    }
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

    /// The vertical field of view a `PerspectiveX` would build from a
    /// horizontal one — `Camera::perspective`'s conversion, replicated so that
    /// the composition below can be asserted on.
    fn vertical_fov(fov_x_degrees: f32, aspect: f32) -> f32 {
        let half = fov_x_degrees.to_radians() * 0.5;
        (2.0 * (half.tan() / aspect).atan()).to_degrees()
    }

    #[test]
    fn a_four_by_three_screen_leaves_the_field_of_view_alone() {
        let aspect = 4.0 / 3.0;
        let fov = scale_fov_by_width_ratio(75.0, aspect / FOV_ASPECT);
        assert!((fov - 75.0).abs() < 1e-3, "{fov}");
    }

    #[test]
    fn a_wider_screen_widens_the_horizontal_field_of_view() {
        let aspect = 16.0 / 9.0;
        let fov = scale_fov_by_width_ratio(75.0, aspect / FOV_ASPECT);
        assert!(fov > 91.0 && fov < 91.6, "{fov}");
    }

    /// **The property the whole scaling exists for**, and the regression this
    /// guards: without it, `default_fov` goes straight into a `PerspectiveX`
    /// and the vertical field of view *shrinks* as the screen gets wider —
    /// 59.8 degrees at 4:3 but only 46.7 at 16:9. The picture is not obviously
    /// broken, just quietly too narrow.
    #[test]
    fn the_vertical_field_of_view_is_the_same_at_every_aspect_ratio() {
        let scaled: Vec<f32> = [4.0 / 3.0, 16.0 / 10.0, 16.0 / 9.0, 21.0 / 9.0]
            .into_iter()
            .map(|aspect| {
                vertical_fov(
                    scale_fov_by_width_ratio(75.0, aspect / FOV_ASPECT),
                    aspect,
                )
            })
            .collect();

        for fov_y in &scaled {
            assert!((fov_y - scaled[0]).abs() < 1e-3, "{scaled:?}");
        }
        assert!((scaled[0] - 59.84).abs() < 0.05, "{}", scaled[0]);

        // And what it would be without the scaling, at 16:9.
        assert!((vertical_fov(75.0, 16.0 / 9.0) - 46.7).abs() < 0.1);
    }

    #[test]
    fn a_zero_height_viewport_does_not_divide_by_it() {
        assert_eq!(screen_aspect(1280, 0), 1.0);
        assert!((screen_aspect(1280, 720) - 16.0 / 9.0).abs() < 1e-6);
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
