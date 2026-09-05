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

/// `sensitivity`'s declared default (`in_mouse.cpp:100`).
///
/// The value is a **cvar**, not a constant — `FCVAR_ARCHIVE`, so it persists to
/// `config.cfg` — and [`Engine`](crate::engine::Engine) holds the handle. This
/// is only the default it is registered with, and the bounds below are the ones
/// `ConVar`'s clamp is given.
pub const SENSITIVITY: f32 = 2.5;

/// `sensitivity`'s clamp (`in_mouse.cpp:100`): `true, 0.0001f, true, 1000`.
///
/// Every one of `in_mouse.cpp`'s mouse-factor cvars carries the same
/// `[0.0001, 1000]` pair — `m_yaw`, `m_pitch`, `m_side`, `m_forward` — so an
/// out-of-range `sensitivity` in a shipped `.cfg` clamps rather than applying.
pub const SENSITIVITY_MIN: f32 = 0.0001;
pub const SENSITIVITY_MAX: f32 = 1000.0;

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
        let right = Vec3::new(-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp);
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
/// The keys are no longer hard-coded: movement comes from [`MoveButtons`],
/// which is fed by the `+forward`/`-forward` commands that
/// [`Bindings::dispatch`](super::bind::Bindings::dispatch) produces. Which
/// physical key that is comes from `cfg/config_default.cfg`.
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
    pub fn look(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.angles.apply_mouse(dx, dy, sensitivity);
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
    pub fn step(&mut self, buttons: &MoveButtons, seconds: f32) {
        let (forward, right, _) = self.angles.vectors();

        let mut wish = Vec3::ZERO;
        if buttons.forward.is_down() {
            wish += forward * CL_FORWARDSPEED;
        }
        if buttons.back.is_down() {
            wish -= forward * CL_FORWARDSPEED;
        }
        if buttons.move_right.is_down() {
            wish += right * CL_SIDESPEED;
        }
        if buttons.move_left.is_down() {
            wish -= right * CL_SIDESPEED;
        }
        // Along world `+Z`, not along `up`: `FullNoClipMove` adds `m_flUpMove`
        // to `wishvel[2]` after the forward/right terms, so looking down does
        // not tilt which way "up" is.
        if buttons.up.is_down() {
            wish.z += CL_UPSPEED;
        }
        if buttons.down.is_down() {
            wish.z -= CL_UPSPEED;
        }

        let mut factor = SV_NOCLIPSPEED;
        let max_speed = SV_MAXSPEED * factor;
        if buttons.speed.is_down() {
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

/// One button a `+command` holds down. `kbutton_t` (`in_main.cpp:424`), minus
/// the half that belongs to `client/`.
///
/// **`down` is the point of it.** A `+command` carries the index of the button
/// that sent it, and this records up to two of them, so that two keys bound to
/// `+forward` do not cancel each other: releasing one leaves the other holding
/// the movement. Without it, `bind UPARROW +forward` alongside `bind w
/// +forward` makes tapping either one stop the other.
///
/// **Deliberately not ported:** `state`'s impulse bits and `KeyState`'s
/// fraction-of-a-frame (`in_main.cpp:813`), which is what stops a 30 Hz frame
/// from swallowing a fast tap. That is genuinely good design and it is
/// genuinely `client/`'s — it exists to fill in `CUserCmd`'s float move
/// values, and there is no `CUserCmd`. Building it against a camera would bake
/// in the wrong consumer (`portdocs/ENGINE_INPUT.md` §4.4).
///
/// One divergence from the C++, and it fixes a latent bug: Valve stores the
/// holders as `int` with **0 meaning empty**, so button code 0 could never
/// hold anything. This uses `Option`, and `None` is the only empty.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KButton {
    /// The indices currently holding this down. `Some(-1)` is the holder for a
    /// `+command` typed with no argument, which is what Valve's `k = -1` is.
    down: [Option<i32>; 2],
}

impl KButton {
    /// `KeyDown` (`in_main.cpp:424`).
    pub fn press(&mut self, index: Option<i32>) {
        let index = index.or(Some(-1));
        if self.down[0] == index || self.down[1] == index {
            return; // repeating key
        }
        if self.down[0].is_none() {
            self.down[0] = index;
        } else if self.down[1].is_none() {
            self.down[1] = index;
        }
        // Valve warns and drops a third holder; two is enough to make the
        // two-keys-one-command case work, which is all this is for.
    }

    /// `KeyUp` (`in_main.cpp:460`).
    ///
    /// A `-command` typed with **no argument** releases unconditionally —
    /// Valve's `if ( !c || !c[0] )` branch. That is what makes typing
    /// `-forward` at the console a way out of a stuck key.
    pub fn release(&mut self, index: Option<i32>) {
        let Some(index) = index else {
            self.down = [None; 2];
            return;
        };
        if self.down[0] == Some(index) {
            self.down[0] = None;
        } else if self.down[1] == Some(index) {
            self.down[1] = None;
        } else {
            return; // key up without a corresponding down
        }
    }

    pub fn is_down(&self) -> bool {
        self.down[0].is_some() || self.down[1].is_some()
    }
}

/// The `+command` buttons the placeholder camera reads.
///
/// `CInput`'s `in_forward`, `in_back`, `in_moveleft`, `in_moveright`, `in_up`,
/// `in_down` and `in_speed` — the seven that `FullNoClipMove` and
/// `ComputeUpwardMove` actually consume. **Moves to `client/` with
/// [`FlyCamera`]**, where it becomes the front of `CUserCmd`.
///
/// # What Portal 2 does and does not bind
///
/// `cfg/config_default.cfg` binds `+forward`, `+back`, `+moveleft` and
/// `+moveright`, and **not** `+moveup`, `+movedown` or `+speed` — vertical
/// movement is a noclip-only concept and the shipped game has no key for it.
/// So this also accepts **`+jump` and `+duck`** (SPACE and CTRL in the shipped
/// config) as the camera's up and down. That is a **placeholder divergence**,
/// not Valve's behaviour: `ComputeUpwardMove` (`in_main.cpp:1101`) reads
/// `in_up`/`in_down` only, and jump is a button on `CUserCmd`, not a movement
/// axis. It exists so that a camera standing in for a player flies with the
/// keys the player's config actually binds, and it **dies with `client/`**.
#[derive(Debug, Clone, Copy, Default)]
pub struct MoveButtons {
    pub forward: KButton,
    pub back: KButton,
    pub move_left: KButton,
    pub move_right: KButton,
    pub up: KButton,
    pub down: KButton,
    pub speed: KButton,
}

/// The `+command`/`-command` pairs [`MoveButtons`] answers to.
///
/// Both spellings are listed rather than derived, because a [`CommandSpec`]
/// name is `&'static str` and these are what gets registered.
///
/// `jump` and `duck` are the placeholder divergence documented on
/// [`MoveButtons`].
///
/// [`CommandSpec`]: crate::engine::console::CommandSpec
pub const MOVE_COMMANDS: &[(&str, &str)] = &[
    ("+forward", "-forward"),
    ("+back", "-back"),
    ("+moveleft", "-moveleft"),
    ("+moveright", "-moveright"),
    ("+moveup", "-moveup"),
    ("+movedown", "-movedown"),
    ("+speed", "-speed"),
    ("+jump", "-jump"),
    ("+duck", "-duck"),
];

impl MoveButtons {
    /// Applies one `+name`/`-name` command. True if `name` was one of ours.
    ///
    /// `name` is the command without its sign, and `index` is the button-index
    /// argument the binding carried — `None` when a bare `+forward` was typed.
    pub fn apply(&mut self, name: &str, down: bool, index: Option<i32>) -> bool {
        let button = match name.to_ascii_lowercase().as_str() {
            "forward" => &mut self.forward,
            "back" => &mut self.back,
            "moveleft" => &mut self.move_left,
            "moveright" => &mut self.move_right,
            "moveup" | "jump" => &mut self.up,
            "movedown" | "duck" => &mut self.down,
            "speed" => &mut self.speed,
            _ => return false,
        };
        match down {
            true => button.press(index),
            false => button.release(index),
        }
        true
    }

    /// `CInput::ClearStates` — focus loss must not leave the camera walking.
    pub fn clear(&mut self) {
        *self = MoveButtons::default();
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

    /// Holds the named `+command`s, as a binding would.
    fn hold(commands: &[&str]) -> MoveButtons {
        let mut buttons = MoveButtons::default();
        for (index, name) in commands.iter().enumerate() {
            assert!(
                buttons.apply(name, true, Some(index as i32)),
                "`{name}` is not a movement command"
            );
        }
        buttons
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
        camera.step(&hold(&["forward"]), 1.0);
        assert!(close(
            camera.origin,
            Vec3::X * CL_FORWARDSPEED * SV_NOCLIPSPEED
        ));
    }

    #[test]
    fn strafing_right_moves_along_negative_y_when_facing_positive_x() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        camera.step(&hold(&["moveright"]), 1.0);
        assert!(camera.origin.y < 0.0, "{:?}", camera.origin);
        assert!(camera.origin.x.abs() < 0.25);
    }

    #[test]
    fn rising_is_along_world_up_whatever_the_view_is_doing() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 60.0, 30.0);
        camera.step(&hold(&["moveup"]), 1.0);
        assert!(close(camera.origin, Vec3::Z * CL_UPSPEED * SV_NOCLIPSPEED));
    }

    #[test]
    fn walking_halves_the_speed() {
        let mut fast = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        fast.step(&hold(&["forward"]), 1.0);
        let mut slow = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        slow.step(&hold(&["forward", "speed"]), 1.0);
        assert!((slow.origin.length() * 2.0 - fast.origin.length()).abs() < 0.25);
    }

    #[test]
    fn opposite_keys_cancel() {
        let mut camera = FlyCamera::new(Vec3::ZERO, 0.0, 0.0);
        camera.step(&hold(&["forward", "back", "moveleft", "moveright"]), 1.0);
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
        camera.step(&hold(&["forward", "moveup"]), 1.0);
        let travelled = camera.origin.length();
        assert!(
            (travelled - SV_MAXSPEED * SV_NOCLIPSPEED).abs() < 0.25,
            "{travelled}"
        );
    }

    #[test]
    fn two_keys_bound_to_one_command_do_not_cancel_each_other() {
        // The whole reason a `+command` carries the index of the button that
        // sent it. `bind w +forward` and `bind UPARROW +forward`: hold both,
        // release one, keep walking.
        let mut buttons = MoveButtons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", true, Some(20));
        assert!(buttons.forward.is_down());

        buttons.apply("forward", false, Some(20));
        assert!(
            buttons.forward.is_down(),
            "the other key is still holding it"
        );

        buttons.apply("forward", false, Some(10));
        assert!(!buttons.forward.is_down());
    }

    #[test]
    fn a_release_for_a_button_that_never_pressed_is_ignored() {
        let mut buttons = MoveButtons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", false, Some(99));
        assert!(buttons.forward.is_down(), "key up without a matching down");
    }

    #[test]
    fn a_repeated_press_from_the_same_button_is_not_a_second_holder() {
        let mut buttons = MoveButtons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", false, Some(10));
        assert!(!buttons.forward.is_down(), "one press, one release");
    }

    /// Valve's `if ( !c || !c[0] )` branch: typing `-forward` at the console
    /// releases regardless of who was holding it, which is the way out of a
    /// stuck movement key.
    #[test]
    fn a_bare_minus_command_releases_unconditionally() {
        let mut buttons = MoveButtons::default();
        buttons.apply("forward", true, Some(10));
        buttons.apply("forward", true, Some(20));
        buttons.apply("forward", false, None);
        assert!(!buttons.forward.is_down());
    }

    #[test]
    fn jump_and_duck_drive_the_placeholder_camera_vertically() {
        // A documented divergence: `ComputeUpwardMove` reads `+moveup`/
        // `+movedown` only, and Portal 2 binds neither. See `MoveButtons`.
        let mut buttons = MoveButtons::default();
        buttons.apply("jump", true, Some(1));
        assert!(buttons.up.is_down());
        buttons.apply("duck", true, Some(2));
        assert!(buttons.down.is_down());
    }

    #[test]
    fn an_unknown_command_is_refused_rather_than_silently_dropped() {
        let mut buttons = MoveButtons::default();
        assert!(!buttons.apply("attack", true, Some(1)));
    }
}
