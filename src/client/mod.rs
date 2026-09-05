//! The game client: the local player, and the command that moves it.
//!
//! Valve's `client.so` — `game/client/in_*.cpp`, `view.cpp` and the movement
//! half of `game/shared/gamemovement.cpp` — reduced to the spine the boot path
//! runs through. `portdocs/CLIENT.md` is the standing analysis;
//! `rustdocs/CLIENT.md` is how to call it.
//!
//! # This is not `ENGINE.md` §7.5
//!
//! There are two modules called "the client" and they share nothing but the
//! word. This is the **game client**: what turns held keys and mouse motion
//! into a [`UserCmd`], runs it through movement, and says where the eye is. The
//! **client connection** — `CClientState`, signon handshakes, snapshot parsing
//! — is `src/engine/client/`, is blocked on `net/`, and does not exist. In
//! prose, never say just "the client".
//!
//! # The frame
//!
//! `_Host_RunFrame_Input` (`engine/host.cpp:3272`) runs three things in order:
//! the client processes input, `Cbuf_Execute` runs the command buffer, and
//! `CL_Move` (`engine/cl_main.cpp:2734`) has the client build a command. This
//! port already had that ordering before this module existed —
//! [`Engine::frame`](crate::engine::Engine::frame) drains input, dispatches
//! bindings, runs the console, and only then moves the view — so stage 1
//! changed the third step and nothing else.
//!
//! ```text
//! +forward 30  --(command buffer)-->  Buttons::apply
//!                                     Client::create_move  -> UserCmd
//!                                     Client::run_move     -> Player::origin
//!                                     Client::eye/angles   -> the camera
//! ```
//!
//! **[`create_move`](Client::create_move) and [`run_move`](Client::run_move)
//! are two calls, not one**, and that is deliberate: in a game with a server
//! the command goes over the wire between them, and prediction is a layer that
//! wraps `run_move` without touching it (`portdocs/CLIENT.md` §4.8).

pub mod button;
pub mod movement;
pub mod player;
pub mod usercmd;
pub mod view;

pub use button::{ButtonBits, Buttons, MoveButton, BUTTONS};
pub use movement::MoveData;
pub use player::{MoveType, Player};
pub use usercmd::UserCmd;
pub use view::{ViewAngles, ViewSetup};

use glam::Vec3;

use crate::engine::console::{Console, Cvar, CvarFlags};

/// `default_fov` for Portal (`game/client/portal/clientmode_portal.cpp:32`).
///
/// **75, not CS:GO's 90** (`clientmode_csnormal.cpp:136`). Both are defined at
/// file scope in the same tree, under no `#ifdef` a reader would notice, and
/// picking the wrong one is the single easiest mistake to make in this corner
/// of `legacy/`.
pub const DEFAULT_FOV: f32 = 75.0;

/// `cl_forwardspeed`/`cl_sidespeed`/`cl_backspeed` (`in_main.cpp:61-63`), which
/// are `MAX_LINEAR_SPEED` — **175 under `PORTAL2`** (`:56`), where every other
/// Source game gets 450 (`:58`).
pub const CL_FORWARDSPEED: f32 = 175.0;
pub const CL_SIDESPEED: f32 = 175.0;
pub const CL_BACKSPEED: f32 = 175.0;

/// `cl_upspeed` (`in_main.cpp:51`).
pub const CL_UPSPEED: f32 = 320.0;

/// Everything the client reads out of the cvar system.
///
/// One handle per cvar, per `ENGINE_CONSOLE.md` §6.1: a subsystem holds the
/// ones it reads rather than a way to look one up. Grouped only so that
/// [`Client`]'s own fields stay readable.
struct Cvars {
    sensitivity: Cvar,
    m_yaw: Cvar,
    m_pitch: Cvar,
    m_side: Cvar,
    m_forward: Cvar,
    lookstrafe: Cvar,
    cl_mouseenable: Cvar,
    cl_pitchdown: Cvar,
    cl_pitchup: Cvar,
    cl_yawspeed: Cvar,
    cl_pitchspeed: Cvar,
    cl_anglespeedkey: Cvar,
    cl_mouselook: Cvar,
    in_usekeyboardsampletime: Cvar,
    cl_forwardspeed: Cvar,
    cl_backspeed: Cvar,
    cl_sidespeed: Cvar,
    cl_upspeed: Cvar,
    default_fov: Cvar,
    r_farz: Cvar,
    r_mapextents: Cvar,
    sv_maxspeed: Cvar,
    sv_friction: Cvar,
    sv_noclipspeed: Cvar,
    sv_noclipaccelerate: Cvar,
}

/// The game client.
///
/// Owns the player because loading a map is what positions one — the same
/// reason [`Scene`](crate::engine) owns the material cache — and owns the
/// button state because a `+command` is the only thing that sets it.
pub struct Client {
    player: Player,
    /// What the `+command`s have left held. `CInput`'s file-scope `kbutton_t`s
    /// (`in_main.cpp:136` onwards).
    buttons: Buttons,
    cvars: Cvars,
    /// `command_number` — incremented per command, never reset. Valve derives
    /// it from the netchannel's outgoing sequence (`cl_main.cpp:2777`); with no
    /// netchannel it is simply a count.
    command_number: i32,
    /// `gpGlobals->tickcount`. See [`UserCmd::tick_count`] — a count, not yet a
    /// rate.
    tick_count: i32,
    /// `in_impulse` (`in_main.cpp:44`): set by the `impulse` command, latched
    /// onto the next command and cleared.
    impulse: u8,
    /// `PerUserInput_t::m_flKeyboardSampleTime` (`in_main.cpp:861`, `:877`) —
    /// how much real time this frame still owes keyboard look.
    ///
    /// Refilled once per **frame** by
    /// [`set_sample_time`](Client::set_sample_time) and drawn down once per
    /// **command** by `DetermineKeySpeed`. See both for why the two are not the
    /// same thing.
    keyboard_sample_time: f32,
}

impl Client {
    /// Registers the client's cvars and brings up a player at the origin.
    ///
    /// The commands — the `+`/`-` pairs from [`BUTTONS`], `impulse` and
    /// `noclip` — are registered by the engine alongside its own, because
    /// `console/` hands a command back through one
    /// [`CommandTarget`](crate::engine::console::CommandTarget) and that
    /// implementation is the engine's.
    pub fn new(console: &mut Console<'_>) -> Client {
        // Names, defaults, bounds and flags are Valve's. `FCVAR_NOTIFY`,
        // `FCVAR_REPLICATED`, `FCVAR_RELEASE` and `FCVAR_SS` have no
        // counterpart here (`ENGINE_CONSOLE.md` §4.6) and are dropped rather
        // than approximated.
        let bounds = (Some(view::SENSITIVITY_MIN), Some(view::SENSITIVITY_MAX));
        let cvars = Cvars {
            sensitivity: console.cvar_bounded(
                "sensitivity",
                &view::SENSITIVITY.to_string(),
                CvarFlags::ARCHIVE,
                "Mouse sensitivity.",
                bounds.0,
                bounds.1,
            ),
            m_yaw: console.cvar_bounded(
                "m_yaw",
                &view::M_YAW.to_string(),
                CvarFlags::ARCHIVE,
                "Mouse yaw factor.",
                bounds.0,
                bounds.1,
            ),
            // **Deliberately unbounded**, where its four neighbours are not:
            // `m_pitch` is a `ConVar_ServerBounded` whose constructor takes no
            // bounds (`in_mouse.cpp:59`), because a *negative* value is how
            // "reverse mouse" is spelled. A `[0.0001, 1000]` clamp copied from
            // the line above would silently break that option.
            m_pitch: console.cvar(
                "m_pitch",
                &view::M_PITCH.to_string(),
                CvarFlags::ARCHIVE,
                "Mouse pitch factor.",
            ),
            m_side: console.cvar_bounded(
                "m_side",
                &view::M_SIDE.to_string(),
                CvarFlags::ARCHIVE,
                "Mouse side factor.",
                bounds.0,
                bounds.1,
            ),
            m_forward: console.cvar_bounded(
                "m_forward",
                &view::M_FORWARD.to_string(),
                CvarFlags::ARCHIVE,
                "Mouse forward factor.",
                bounds.0,
                bounds.1,
            ),
            lookstrafe: console.cvar(
                "lookstrafe",
                "0",
                CvarFlags::ARCHIVE,
                "Apply horizontal mouse movement as sidemove rather than yaw.",
            ),
            cl_mouseenable: console.cvar(
                "cl_mouseenable",
                "1",
                CvarFlags::NONE,
                "Whether the mouse drives the view at all.",
            ),
            cl_pitchdown: console.cvar(
                "cl_pitchdown",
                &view::CL_PITCHDOWN.to_string(),
                CvarFlags::CHEAT,
                "How far down the view may look.",
            ),
            cl_pitchup: console.cvar(
                "cl_pitchup",
                &view::CL_PITCHUP.to_string(),
                CvarFlags::CHEAT,
                "How far up the view may look.",
            ),
            cl_yawspeed: console.cvar(
                "cl_yawspeed",
                &view::CL_YAWSPEED.to_string(),
                CvarFlags::NONE,
                "Degrees per second `+left`/`+right` turn the view.",
            ),
            cl_pitchspeed: console.cvar(
                "cl_pitchspeed",
                &view::CL_PITCHSPEED.to_string(),
                CvarFlags::NONE,
                "Degrees per second `+lookup`/`+lookdown` turn the view.",
            ),
            cl_anglespeedkey: console.cvar(
                "cl_anglespeedkey",
                &view::CL_ANGLESPEEDKEY.to_string(),
                CvarFlags::NONE,
                "What `+speed` multiplies keyboard turn speed by.",
            ),
            // `FCVAR_NOT_CONNECTED` in the original — "Cannot be set while
            // connected to a server" — which this port has no flag for and no
            // server to be connected to.
            cl_mouselook: console.cvar(
                "cl_mouselook",
                "1",
                CvarFlags::ARCHIVE,
                "Set to 1 to use mouse for look, 0 for keyboard look.",
            ),
            in_usekeyboardsampletime: console.cvar(
                "in_usekeyboardsampletime",
                "1",
                CvarFlags::NONE,
                "Use keyboard sample time smoothing.",
            ),
            cl_forwardspeed: console.cvar(
                "cl_forwardspeed",
                &CL_FORWARDSPEED.to_string(),
                CvarFlags::CHEAT,
                "",
            ),
            cl_backspeed: console.cvar(
                "cl_backspeed",
                &CL_BACKSPEED.to_string(),
                CvarFlags::CHEAT,
                "",
            ),
            cl_sidespeed: console.cvar(
                "cl_sidespeed",
                &CL_SIDESPEED.to_string(),
                CvarFlags::CHEAT,
                "",
            ),
            cl_upspeed: console.cvar(
                "cl_upspeed",
                &CL_UPSPEED.to_string(),
                CvarFlags::CHEAT,
                "",
            ),
            default_fov: console.cvar(
                "default_fov",
                &DEFAULT_FOV.to_string(),
                CvarFlags::CHEAT,
                "",
            ),
            r_farz: console.cvar(
                "r_farz",
                "-1",
                CvarFlags::CHEAT,
                "Override the far clipping plane. -1 means to use the value in \
                 env_fog_controller.",
            ),
            r_mapextents: console.cvar(
                "r_mapextents",
                &view::R_MAPEXTENTS.to_string(),
                CvarFlags::CHEAT,
                "Set the max dimension for the map. This determines the far clipping plane",
            ),
            sv_maxspeed: console.cvar(
                "sv_maxspeed",
                &movement::SV_MAXSPEED.to_string(),
                CvarFlags::NONE,
                "",
            ),
            sv_friction: console.cvar(
                "sv_friction",
                &movement::SV_FRICTION.to_string(),
                CvarFlags::NONE,
                "World friction.",
            ),
            sv_noclipspeed: console.cvar(
                "sv_noclipspeed",
                &movement::SV_NOCLIPSPEED.to_string(),
                CvarFlags::ARCHIVE,
                "",
            ),
            sv_noclipaccelerate: console.cvar(
                "sv_noclipaccelerate",
                &movement::SV_NOCLIPACCELERATE.to_string(),
                CvarFlags::ARCHIVE,
                "",
            ),
        };

        // `sv_stopspeed` is walking's (`gamemovement.cpp`'s `Friction`) and is
        // registered without a handle so that a `.cfg` setting it is not
        // reported as an unknown command. Stage 4 takes the handle.
        console.cvar(
            "sv_stopspeed",
            &movement::SV_STOPSPEED.to_string(),
            CvarFlags::NONE,
            "Minimum stopping speed when on ground.",
        );

        Client {
            player: Player::new(Vec3::ZERO, 0.0, 0.0),
            buttons: Buttons::default(),
            cvars,
            command_number: 0,
            tick_count: 0,
            impulse: 0,
            keyboard_sample_time: 0.0,
        }
    }

    /// What a `+command`/`-command` reaches.
    pub fn buttons_mut(&mut self) -> &mut Buttons {
        &mut self.buttons
    }

    /// `CInput::ClearStates` (`in_mouse.cpp:828`), for focus loss.
    ///
    /// Alt-tabbing with `+forward` held and coming back to a player who has
    /// walked into a wall for thirty seconds is the failure this prevents. It
    /// is not enough to clear the *keyboard* state, because what is held is
    /// held by the **command**, not by the key.
    pub fn clear_buttons(&mut self) {
        self.buttons.clear();
    }

    /// `CInput::IN_SetSampleTime` (`in_main.cpp:861`): gives keyboard look a
    /// frame's worth of real time to spend.
    ///
    /// **Call this once per frame, before [`create_move`](Client::create_move),
    /// or keyboard look silently does nothing.** `DetermineKeySpeed` returns 0
    /// with an empty budget and `AdjustAngles` returns early on a 0, so the
    /// failure is a `+left` that does not turn rather than an error.
    ///
    /// # Why it is not just `create_move`'s frame time
    ///
    /// Valve refills once per frame with `host_frametime` (`host.cpp:4192`) and
    /// spends it in `CreateMove`, which runs once per **tick** — and a frame
    /// can hold several ticks. The budget is what stops a two-tick frame from
    /// turning the view twice as far as a one-tick frame covering the same real
    /// time, and what leaves nothing for the end-of-frame `ExtraMouseSample` to
    /// spend a third time.
    ///
    /// This port runs exactly one command per frame and has no
    /// `ExtraMouseSample`, so today the refill and the draw-down cancel exactly
    /// and this is a no-op. It is here because it is the shape the function has
    /// the moment either of those changes, and because rediscovering it from a
    /// "turning is twice as fast at 30 fps" bug report would be expensive.
    pub fn set_sample_time(&mut self, frametime: f32) {
        self.keyboard_sample_time = frametime;
    }

    /// Puts the player at a spawn point. `origin` is the **feet**.
    ///
    /// Velocity is dropped with it: arriving at a new level carrying the last
    /// one's momentum is a bug you would spend a while attributing.
    pub fn spawn(&mut self, origin: Vec3, pitch: f32, yaw: f32) {
        self.player = Player::new(origin, pitch, yaw);
    }

    /// `impulse <n>` (`in_main.cpp:757`). Latched until the next command.
    pub fn set_impulse(&mut self, impulse: u8) {
        self.impulse = impulse;
    }

    /// Flips between [`MoveType::Noclip`] and [`MoveType::Walk`], returning
    /// what it flipped to.
    ///
    /// **`noclip` is a server command in Valve** (`game/server/`), because move
    /// type is server state that gets networked down. With one process and no
    /// server it has to live somewhere; it lives here and moves to `server/`
    /// when there is one — `portdocs/CLIENT.md` §9.2.
    pub fn toggle_noclip(&mut self) -> MoveType {
        self.player.move_type = match self.player.move_type {
            MoveType::Noclip => MoveType::Walk,
            MoveType::Walk => MoveType::Noclip,
        };
        self.player.move_type
    }

    /// `CViewRender::SetUpView` (`game/client/view.cpp:668`) plus the field-of-
    /// view scaling `CViewRender::Render` applies straight afterwards
    /// (`view.cpp:1084`).
    ///
    /// `width`/`height` are the render target's, in pixels. The result is data:
    /// turning it into a projection matrix is the material system's convention
    /// to choose, and keeping that on the other side of the boundary is what
    /// stops this module depending on `wgpu` for the sake of five numbers.
    ///
    /// # The two halves are one call here, and are two in the original
    ///
    /// `SetUpView` leaves `fov` at `default_fov` — a **4:3** number — and
    /// `Render` scales it by `aspect / (4/3)` a few hundred lines later, once
    /// it knows the viewport. Splitting them bought Valve a place for the
    /// client mode and the tool framework to intervene between; there is
    /// nothing to intervene, and a `ViewSetup` whose `fov` still needs scaling
    /// is a trap. So the scaling happens here and
    /// [`ViewSetup::fov`](ViewSetup) is the number to hand a `PerspectiveX`.
    ///
    /// # Not here
    ///
    /// `CalcView`'s additions — view bob, view roll, punch and aim punch — need
    /// a player that can be shot, and `C_Portal_Player::CalcView`'s eye
    /// interpolation through a portal needs portals. They attach at
    /// [`Player::eye`], which is why this asks the player for its eye rather
    /// than adding `VEC_VIEW` itself.
    pub fn view(&self, width: u32, height: u32) -> ViewSetup {
        let aspect = view::screen_aspect(width, height);
        ViewSetup {
            origin: self.player.eye(),
            angles: self.player.angles,
            fov: view::scale_fov_by_width_ratio(
                self.cvars.default_fov.float(),
                aspect / view::FOV_ASPECT,
            ),
            z_near: self.z_near(width, height),
            z_far: self.z_far(),
            width,
            height,
            aspect,
        }
    }

    /// `CViewRender::GetZNear` (`view.cpp:620`).
    ///
    /// **The near plane moves to 1 on a mega-wide screen**, from `VIEW_NEARZ`'s
    /// 7. A very wide viewport pushes the left and right edges of the frustum
    /// far enough out that a 7-unit near plane clips geometry the player is
    /// standing next to. Valve's test is literally `width / (height + 1) > 2`;
    /// the `+ 1` is a divide-by-zero guard and is kept.
    ///
    /// `r_nearz`'s override is `#ifdef _DEBUG` only and is not ported. The
    /// secondary `r_aspectratio > 2` test is not either — see
    /// [`view::screen_aspect`].
    fn z_near(&self, width: u32, height: u32) -> f32 {
        let mega_wide = width as f32 / (height as f32 + 1.0) > 2.0;
        match mega_wide {
            true => 1.0,
            false => view::VIEW_NEARZ,
        }
    }

    /// `CViewRender::GetZFar` (`view.cpp:639`).
    ///
    /// `r_farz` under 1 — the default is -1 — means "use the map's", which is
    /// `r_mapextents × √3`: the diagonal of a cube that size, and so the
    /// furthest apart two points in such a map can be. **Nothing sets
    /// `r_mapextents` from the `.bsp`**; it is a cheat cvar a mapper sets.
    ///
    /// Missing: the `env_fog_controller`'s `farz`, which overrides this when
    /// positive and needs entities.
    fn z_far(&self) -> f32 {
        match self.cvars.r_farz.float() {
            far if far >= 1.0 => far,
            _ => self.cvars.r_mapextents.float() * view::MAP_DIAGONAL,
        }
    }

    /// `CInput::CreateMove` (`in_main.cpp:1350`).
    ///
    /// `mouse` is this tick's accumulated raw motion, in device units, already
    /// gated on whether the mouse is actually driving the view — that is a
    /// question about the window and the console, so the engine answers it.
    ///
    /// # The order is the algorithm
    ///
    /// The movement axes are computed *before* the button bits, and the two
    /// clear different halves of a button's impulse state. With this ordering a
    /// tap shorter than one frame contributes to `forwardmove` and **not** to
    /// `IN_FORWARD`; reversed, it would contribute to both, which is a
    /// difference a server would see. [`Buttons::bits`] has the detail.
    ///
    /// The mouse comes last, after the axes, because with `+strafe` held it
    /// *adds* to them (`ApplyMouse`, `in_mouse.cpp:534`).
    ///
    /// `AdjustAngles` runs **first**, and that is not arbitrary: it and the
    /// `Compute*Move` calls read the same `KeyState`s, which are destructive.
    /// The two never collide, because each pair is mutually exclusive —
    /// `AdjustYaw` reads `+left`/`+right` only when `+strafe` is *up* and
    /// `ComputeSideMove` reads them only when it is *down*; `AdjustPitch` reads
    /// `+forward`/`+back` only when `+klook` is *down* and `ComputeForwardMove`
    /// only when it is *up*. Reorder them and nothing breaks; change either
    /// condition and everything does.
    ///
    /// # Not here yet
    ///
    /// `ScaleMovements` (`:1161`) is **dead in the original** (its body is
    /// `return;` under a `// FIXME FIXME: This doesn't work`) and is not
    /// ported. Weapon selection, the client mode's `CreateMove` override and
    /// `CheckPaused` need systems that do not exist.
    pub fn create_move(&mut self, dt: f32, mouse: (f32, f32)) -> UserCmd {
        self.command_number += 1;
        self.tick_count += 1;
        let mut cmd = UserCmd::new(self.command_number, self.tick_count);

        self.adjust_angles(dt);
        self.compute_side_move(&mut cmd);
        self.compute_upward_move(&mut cmd);
        self.compute_forward_move(&mut cmd);
        self.mouse_move(&mut cmd, mouse);

        cmd.impulse = std::mem::take(&mut self.impulse);
        cmd.buttons = self.buttons.bits(true);
        // Last, because `mouse_move` has just turned the view.
        cmd.viewangles = self.player.angles;
        cmd
    }

    /// `CInput::DetermineKeySpeed` (`in_main.cpp:877`): how many seconds of
    /// keyboard turning this command may do, and the draw-down half of the
    /// budget [`set_sample_time`](Client::set_sample_time) fills.
    ///
    /// Returns 0 when the budget is spent, which is `AdjustAngles`' signal to
    /// do nothing at all. `in_usekeyboardsampletime 0` removes the budget and
    /// hands back the raw frame time.
    ///
    /// `+speed` scales it by `cl_anglespeedkey` — **0.67, not the 0.5 that
    /// halves movement**. Holding the walk key turns at two thirds speed and
    /// moves at one half.
    fn determine_key_speed(&mut self, frametime: f32) -> f32 {
        let mut frametime = frametime;
        if self.cvars.in_usekeyboardsampletime.bool() {
            if self.keyboard_sample_time <= 0.0 {
                return 0.0;
            }
            frametime = frametime.min(self.keyboard_sample_time);
            self.keyboard_sample_time -= frametime;
        }

        match self.buttons.is_down(MoveButton::Speed) {
            true => frametime * self.cvars.cl_anglespeedkey.float(),
            false => frametime,
        }
    }

    /// `CInput::AdjustAngles` (`in_main.cpp:1006`) — keyboard look.
    ///
    /// Deleted from it: the view *tilt* round-trip. Valve subtracts last
    /// frame's tilt, has `CViewEffects` recompute and reapply it, and stores
    /// the delta back — because tilt affects aim and so has to be inside the
    /// angles the command carries. `CViewEffects` is the shake/tilt/punch
    /// system, which needs entities. **In scope for Portal 2**, which tilts the
    /// view; this is where it attaches.
    fn adjust_angles(&mut self, frametime: f32) {
        let speed = self.determine_key_speed(frametime);
        if speed <= 0.0 {
            return;
        }

        self.adjust_yaw(speed);
        self.adjust_pitch(speed);

        self.player.angles.clamp(
            self.cvars.cl_pitchdown.float(),
            self.cvars.cl_pitchup.float(),
        );
    }

    /// `CInput::AdjustYaw` (`in_main.cpp:908`), minus third-person.
    ///
    /// **Not gated on `cl_mouselook`** — arrow-key turning works whether or not
    /// the mouse is looking, which is the Quake-lineage behaviour Valve kept.
    /// `+strafe` suppresses it, because that is what makes `+left`/`+right`
    /// strafe instead ([`compute_side_move`](Client::compute_side_move)).
    fn adjust_yaw(&mut self, speed: f32) {
        if self.buttons.is_down(MoveButton::Strafe) {
            return;
        }
        let yawspeed = speed * self.cvars.cl_yawspeed.float();
        let right = self.buttons.key_state(MoveButton::Right);
        let left = self.buttons.key_state(MoveButton::Left);
        self.player.angles.yaw -= yawspeed * right;
        self.player.angles.yaw += yawspeed * left;
    }

    /// `CInput::AdjustPitch` (`in_main.cpp:942`).
    ///
    /// **All of it is gated on `cl_mouselook` being off**, which defaults to on
    /// — so out of the shipped configuration `+lookup`, `+lookdown` and
    /// `+klook` do nothing, and that is correct. They are keyboard-look keys.
    ///
    /// The surprise worth knowing: **`cl_mouselook 0` does not disable the
    /// mouse.** `ControllerMove` gates `MouseMove` on `cl_mouseenable` and on
    /// the mouse being grabbed (`in_main.cpp:1199`), never on `cl_mouselook`.
    /// Setting it to 0 *adds* keyboard pitch; `cl_mouseenable 0` is what takes
    /// the mouse away.
    ///
    /// `view->StopPitchDrift()` is dropped with the pitch drift itself
    /// (`portdocs/CLIENT.md` §5) — it re-centres the view for keyboard-only
    /// play and defaults off.
    fn adjust_pitch(&mut self, speed: f32) {
        if self.cvars.cl_mouselook.bool() {
            return;
        }
        let pitchspeed = speed * self.cvars.cl_pitchspeed.float();

        // With `+klook` held, forward and back are pitch rather than movement —
        // and `ComputeForwardMove` returns early for the same reason, so the
        // two never read these key states in the same command.
        if self.buttons.is_down(MoveButton::KLook) {
            let forward = self.buttons.key_state(MoveButton::Forward);
            let back = self.buttons.key_state(MoveButton::Back);
            self.player.angles.pitch -= pitchspeed * forward;
            self.player.angles.pitch += pitchspeed * back;
        }

        let up = self.buttons.key_state(MoveButton::LookUp);
        let down = self.buttons.key_state(MoveButton::LookDown);
        self.player.angles.pitch -= pitchspeed * up;
        self.player.angles.pitch += pitchspeed * down;
    }

    /// `ComputeSideMove` (`in_main.cpp:1051`), minus third-person.
    ///
    /// **`+strafe` makes `+left`/`+right` strafe** instead of turning, which is
    /// what the button is for.
    fn compute_side_move(&mut self, cmd: &mut UserCmd) {
        let side = self.cvars.cl_sidespeed.float();
        if self.buttons.is_down(MoveButton::Strafe) {
            cmd.sidemove += side * self.buttons.key_state(MoveButton::Right);
            cmd.sidemove -= side * self.buttons.key_state(MoveButton::Left);
        }
        cmd.sidemove += side * self.buttons.key_state(MoveButton::MoveRight);
        cmd.sidemove -= side * self.buttons.key_state(MoveButton::MoveLeft);
    }

    /// `ComputeUpwardMove` (`in_main.cpp:1099`), plus one placeholder.
    ///
    /// **The placeholder:** `cfg/config_default.cfg` binds neither `+moveup`
    /// nor `+movedown` — vertical movement is a noclip-only concept and the
    /// shipped game has no key for it — so `+jump` and `+duck` (SPACE and CTRL
    /// in that config) also drive the axis. That is not Valve's behaviour:
    /// jump is a *button* on the command, not a movement axis, and in the real
    /// game you fly up by looking up. It dies at stage 4, with walking
    /// (`portdocs/CLIENT.md` §8).
    ///
    /// It reads `is_down` rather than `key_state` on purpose: `key_state`
    /// clears the impulse bits that `IN_JUMP` and `IN_DUCK` are about to be
    /// read from, so a placeholder written the obvious way would quietly change
    /// what the command says about jumping.
    fn compute_upward_move(&mut self, cmd: &mut UserCmd) {
        let up = self.cvars.cl_upspeed.float();
        cmd.upmove += up * self.buttons.key_state(MoveButton::MoveUp);
        cmd.upmove -= up * self.buttons.key_state(MoveButton::MoveDown);

        if self.buttons.is_down(MoveButton::Jump) {
            cmd.upmove += up;
        }
        if self.buttons.is_down(MoveButton::Duck) {
            cmd.upmove -= up;
        }
    }

    /// `ComputeForwardMove` (`in_main.cpp:1111`), minus third-person.
    ///
    /// `+klook` suppresses it entirely: with keyboard-look held, forward and
    /// back are *pitch*, not movement (`AdjustPitch`, `in_main.cpp:942`).
    fn compute_forward_move(&mut self, cmd: &mut UserCmd) {
        if self.buttons.is_down(MoveButton::KLook) {
            return;
        }
        let forward = self.cvars.cl_forwardspeed.float();
        let back = self.cvars.cl_backspeed.float();
        cmd.forwardmove += forward * self.buttons.key_state(MoveButton::Forward);
        cmd.forwardmove -= back * self.buttons.key_state(MoveButton::Back);
    }

    /// `CInput::MouseMove` (`in_mouse.cpp:698`) and `ApplyMouse` (`:470`).
    ///
    /// Scale by `sensitivity`, then either turn the view or move the player,
    /// depending on `+strafe` and `lookstrafe`. The three cases are Valve's and
    /// the asymmetry between them is too: the *horizontal* axis checks
    /// `+strafe` **or** `lookstrafe`, the *vertical* axis checks only
    /// `+strafe`.
    fn mouse_move(&mut self, cmd: &mut UserCmd, (dx, dy): (f32, f32)) {
        // `cl_mouseenable 0` is the "give me my cursor back" escape hatch, and
        // it must not merely stop the view turning: the accumulated delta is
        // dropped, not banked.
        if !self.cvars.cl_mouseenable.bool() {
            return;
        }

        let (mouse_x, mouse_y) = view::scale_mouse(dx, dy, self.cvars.sensitivity.float());

        let strafe = self.buttons.is_down(MoveButton::Strafe);
        let lookstrafe = self.cvars.lookstrafe.bool();

        if strafe || lookstrafe {
            cmd.sidemove += self.cvars.m_side.float() * mouse_x;
        } else {
            self.player
                .angles
                .apply_mouse_yaw(mouse_x, self.cvars.m_yaw.float());
        }

        if strafe {
            cmd.forwardmove -= self.cvars.m_forward.float() * mouse_y;
        } else {
            self.player.angles.apply_mouse_pitch(
                mouse_y,
                self.cvars.m_pitch.float(),
                self.cvars.cl_pitchdown.float(),
                self.cvars.cl_pitchup.float(),
            );
        }

        // `AngleNormalize`, which `SetViewAngles` applies on the way back
        // (`cdll_engine_int.cpp:1054`) — an `f32` yaw that grows all session
        // loses precision where nothing wraps it.
        self.player.angles.normalize();

        // `cmd->mousedx = (int)mouse_x` (`in_mouse.cpp:604`) — the **scaled**
        // delta, truncated. Rust's `as` saturates where the C++ wraps, which is
        // the better of the two and the difference only shows up for a mouse
        // that reported more than 32,767 units in one frame.
        cmd.mousedx = mouse_x as i16;
        cmd.mousedy = mouse_y as i16;
    }

    /// `CGameMovement::ProcessMovement` (`gamemovement.cpp:1325`).
    ///
    /// With no server this runs immediately, once, on the command that was just
    /// created. That is the whole difference from the original, and it is why
    /// this is a separate call from [`create_move`](Client::create_move):
    /// prediction wraps this function rather than rewriting it.
    pub fn run_move(&mut self, cmd: &UserCmd, dt: f32) {
        let mut mv = MoveData {
            origin: self.player.origin,
            velocity: self.player.velocity,
            angles: cmd.viewangles,
            forwardmove: cmd.forwardmove,
            sidemove: cmd.sidemove,
            upmove: cmd.upmove,
            buttons: cmd.buttons,
            max_speed: self.cvars.sv_maxspeed.float(),
            friction: self.cvars.sv_friction.float(),
        };

        // `CheckParameters` (`gamemovement.cpp:1137`), for a noclip player:
        // the max-speed clip is **skipped entirely** for `MOVETYPE_NOCLIP`,
        // `ISOMETRIC` and `OBSERVER` (`:1140`), and roll is forced to zero
        // (`:1219`) so that a rolled *view* does not roll the *movement*. The
        // punch-angle addition it also does needs a player who can be shot.
        mv.angles.roll = 0.0;

        match self.player.move_type {
            MoveType::Noclip => movement::full_noclip_move(
                &mut mv,
                dt,
                self.cvars.sv_noclipspeed.float(),
                self.cvars.sv_noclipaccelerate.float(),
            ),
            // `FullWalkMove` is stage 4 and needs `trace/`. Doing nothing is
            // the honest placeholder: there is no ground to stand on, so
            // anything else would be inventing physics.
            MoveType::Walk => {}
        }

        // `FinishMove` — the results go back on the player.
        self.player.origin = mv.origin;
        self.player.velocity = mv.velocity;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client with the cvars registered and nothing else. `Console::detached`
    /// is the console/`egui`-free constructor `console/` provides for exactly
    /// this: no window, no GPU, no game content.
    fn client() -> Client {
        let mut console = Console::detached();
        Client::new(&mut console)
    }

    /// One frame at 60 Hz: refill the keyboard-look budget, then build the
    /// command — the pair `Engine::update_client` makes, in that order.
    const TICK: f32 = 1.0 / 60.0;

    fn frame(client: &mut Client, mouse: (f32, f32)) -> UserCmd {
        client.set_sample_time(TICK);
        client.create_move(TICK, mouse)
    }

    fn hold(client: &mut Client, commands: &[&str]) {
        for (index, name) in commands.iter().enumerate() {
            assert!(
                client.buttons_mut().apply(name, true, Some(index as i32)),
                "`{name}` is not a client command"
            );
        }
    }

    #[test]
    fn a_command_carries_the_speed_cvars_rather_than_an_axis() {
        let mut client = client();
        hold(&mut client, &["forward"]);

        // The press happened *during* the frame, so `KeyState` is worth half
        // of it. This is the whole point of the fractional model and it is the
        // first thing to look at if a movement number looks wrong by a factor
        // of two.
        let first = frame(&mut client, (0.0, 0.0));
        assert_eq!(first.forwardmove, CL_FORWARDSPEED * 0.5);
        assert!(first.buttons.contains(ButtonBits::FORWARD));

        let second = frame(&mut client, (0.0, 0.0));
        assert_eq!(
            second.forwardmove, CL_FORWARDSPEED,
            "held for the whole of the next one, so the full speed"
        );
    }

    #[test]
    fn opposite_keys_cancel() {
        let mut client = client();
        hold(&mut client, &["forward", "back", "moveleft", "moveright"]);
        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(cmd.forwardmove, 0.0);
        assert_eq!(cmd.sidemove, 0.0);
    }

    /// The reason `kbutton_t` exists: a key pressed and released between two
    /// frames still reaches the command, at a quarter speed for that frame.
    /// Without it the tap would be lost entirely.
    #[test]
    fn a_tap_shorter_than_a_frame_still_reaches_the_command() {
        let mut client = client();
        client.buttons_mut().apply("forward", true, Some(1));
        client.buttons_mut().apply("forward", false, Some(1));

        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(cmd.forwardmove, CL_FORWARDSPEED * 0.25);
    }

    /// ...and what noclip then does with it, which is nothing.
    ///
    /// `FullNoClipMove`'s friction bleed floors `control` at `maxspeed / 4`, so
    /// at 60 Hz it removes 34.7 units of speed per frame whatever the player is
    /// doing, while a quarter-speed wish only accelerates by 20.8. **This is
    /// Valve's arithmetic, not a bug**: noclip has momentum, and it is why
    /// `sv_noclipaccelerate` exists. Set it to 0 for the instant-stop feel the
    /// placeholder camera had.
    #[test]
    fn a_tap_does_not_overcome_noclip_friction_but_does_with_no_acceleration() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        client.buttons_mut().apply("forward", true, Some(1));
        client.buttons_mut().apply("forward", false, Some(1));
        let cmd = frame(&mut client, (0.0, 0.0));
        client.run_move(&cmd, 1.0 / 60.0);
        assert_eq!(client.player.origin, Vec3::ZERO);

        console
            .cvars()
            .find("sv_noclipaccelerate")
            .expect("registered")
            .set_string("0");
        client.buttons_mut().apply("forward", true, Some(1));
        client.buttons_mut().apply("forward", false, Some(1));
        let cmd = frame(&mut client, (0.0, 0.0));
        client.run_move(&cmd, 1.0 / 60.0);
        assert!(client.player.origin.x > 0.0);
    }

    /// One second of holding forward at 60 Hz. **Not one frame of one second**:
    /// the friction bleed is per-frame and scales with the frame time, so a
    /// `dt` of 1.0 removes more speed than a second of acceleration adds and
    /// the player never leaves the ground. Valve never runs a frame that long.
    #[test]
    fn a_command_moves_the_player_along_the_view() {
        let mut client = client();
        hold(&mut client, &["forward"]);
        for _ in 0..60 {
            let cmd = frame(&mut client, (0.0, 0.0));
            client.run_move(&cmd, 1.0 / 60.0);
        }

        let origin = client.player.origin;
        assert!(origin.x > 0.0, "{origin:?}");
        assert!(origin.y.abs() < 0.25 && origin.z.abs() < 0.25, "{origin:?}");
    }

    #[test]
    fn the_view_is_the_players_eye_not_its_feet() {
        let mut client = client();
        client.spawn(Vec3::new(10.0, 20.0, 30.0), 0.0, 0.0);
        let view = client.view(1280, 720);
        assert_eq!(view.origin, Vec3::new(10.0, 20.0, 30.0) + player::VEC_VIEW);
        assert_eq!(view.angles, client.player.angles);
    }

    /// The regression `view::scale_fov_by_width_ratio` exists for: a 16:9
    /// window sees a **91-degree** horizontal field of view, not the 75 the
    /// cvar says, because 75 is a 4:3 number.
    #[test]
    fn a_widescreen_view_is_wider_than_default_fov_says() {
        let client = client();
        let wide = client.view(1280, 720);
        assert!(wide.fov > 91.0 && wide.fov < 91.6, "{}", wide.fov);

        let square = client.view(1024, 768);
        assert!((square.fov - DEFAULT_FOV).abs() < 1e-3, "{}", square.fov);
    }

    #[test]
    fn the_far_plane_is_the_maps_diagonal_and_r_farz_overrides_it() {
        let mut console = Console::detached();
        let client = Client::new(&mut console);
        assert_eq!(
            client.view(1280, 720).z_far,
            view::R_MAPEXTENTS * view::MAP_DIAGONAL
        );

        console
            .cvars()
            .find("r_mapextents")
            .expect("registered")
            .set_string("32768");
        assert_eq!(client.view(1280, 720).z_far, 32768.0 * view::MAP_DIAGONAL);

        console
            .cvars()
            .find("r_farz")
            .expect("registered")
            .set_string("5000");
        assert_eq!(client.view(1280, 720).z_far, 5000.0, "the override wins");
    }

    /// `GetZNear`: a very wide viewport pushes the frustum edges out far enough
    /// that a 7-unit near plane clips what the player is standing next to.
    #[test]
    fn the_near_plane_moves_in_on_a_mega_wide_screen() {
        let client = client();
        assert_eq!(client.view(1280, 720).z_near, view::VIEW_NEARZ);
        assert_eq!(client.view(3840, 1080).z_near, 1.0);
    }

    #[test]
    fn spawning_drops_the_last_levels_momentum() {
        let mut client = client();
        hold(&mut client, &["forward"]);
        for _ in 0..10 {
            let cmd = frame(&mut client, (0.0, 0.0));
            client.run_move(&cmd, 1.0 / 60.0);
        }
        assert!(client.player.velocity.length() > 0.0);

        client.spawn(Vec3::ZERO, 0.0, 0.0);
        assert_eq!(client.player.velocity, Vec3::ZERO);
    }

    #[test]
    fn the_mouse_turns_the_view_and_reaches_the_command() {
        let mut client = client();
        let cmd = frame(&mut client, (100.0, 0.0));
        assert!(cmd.viewangles.yaw < 0.0, "right turns are negative yaw");
        assert_eq!(cmd.viewangles, client.player.angles);
        assert_eq!(cmd.mousedx, (100.0 * view::SENSITIVITY) as i16);
    }

    /// `+strafe` turns horizontal mouse motion into sidemove instead of yaw.
    #[test]
    fn holding_strafe_moves_with_the_mouse_instead_of_turning() {
        let mut client = client();
        hold(&mut client, &["strafe"]);
        let cmd = frame(&mut client, (100.0, 50.0));

        assert_eq!(cmd.viewangles.yaw, 0.0, "the view did not turn");
        assert_eq!(cmd.viewangles.pitch, 0.0);
        assert!(cmd.sidemove > 0.0, "{}", cmd.sidemove);
        assert!(cmd.forwardmove < 0.0, "{}", cmd.forwardmove);
    }

    /// The asymmetry in `ApplyMouse`: `lookstrafe` redirects the horizontal
    /// axis only, so the mouse still looks up and down.
    #[test]
    fn lookstrafe_redirects_only_the_horizontal_axis() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        console
            .cvars()
            .find("lookstrafe")
            .expect("registered")
            .set_string("1");

        let cmd = frame(&mut client, (100.0, 50.0));
        assert_eq!(cmd.viewangles.yaw, 0.0, "the view did not turn");
        assert!(cmd.viewangles.pitch > 0.0, "but it still looked down");
        assert!(cmd.sidemove > 0.0);
    }

    #[test]
    fn cl_mouseenable_zero_drops_the_motion_rather_than_banking_it() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        console
            .cvars()
            .find("cl_mouseenable")
            .expect("registered")
            .set_string("0");

        frame(&mut client, (500.0, 0.0));
        console
            .cvars()
            .find("cl_mouseenable")
            .expect("registered")
            .set_string("1");
        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(cmd.viewangles.yaw, 0.0, "nothing arrived in one lump");
    }

    #[test]
    fn changing_a_speed_cvar_changes_the_command() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        console
            .cvars()
            .find("cl_forwardspeed")
            .expect("registered")
            .set_string("400");

        client.buttons_mut().apply("forward", true, Some(1));
        frame(&mut client, (0.0, 0.0)); // the frame the press landed in
        assert_eq!(frame(&mut client, (0.0, 0.0)).forwardmove, 400.0);
    }

    #[test]
    fn an_impulse_is_latched_onto_one_command_and_cleared() {
        let mut client = client();
        client.set_impulse(101);
        assert_eq!(frame(&mut client, (0.0, 0.0)).impulse, 101);
        assert_eq!(frame(&mut client, (0.0, 0.0)).impulse, 0);
    }

    #[test]
    fn commands_are_numbered_from_one_and_never_repeat() {
        let mut client = client();
        let first = frame(&mut client, (0.0, 0.0));
        let second = frame(&mut client, (0.0, 0.0));
        assert_eq!(first.command_number, 1);
        assert_eq!(second.command_number, 2);
    }

    /// Walking is stage 4, so turning noclip off freezes the player rather than
    /// dropping them through the floor.
    #[test]
    fn turning_noclip_off_leaves_a_player_that_cannot_move_yet() {
        let mut client = client();
        assert_eq!(client.toggle_noclip(), MoveType::Walk);

        hold(&mut client, &["forward"]);
        for _ in 0..60 {
            let cmd = frame(&mut client, (0.0, 0.0));
            client.run_move(&cmd, 1.0 / 60.0);
        }
        assert_eq!(client.player.origin, Vec3::ZERO);

        assert_eq!(client.toggle_noclip(), MoveType::Noclip);
        for _ in 0..60 {
            let cmd = frame(&mut client, (0.0, 0.0));
            client.run_move(&cmd, 1.0 / 60.0);
        }
        assert!(client.player.origin.length() > 0.0);
    }

    /// Focus loss must not leave the player walking: what is held is held by
    /// the command, not by the key.
    #[test]
    fn clearing_the_buttons_stops_the_player() {
        let mut client = client();
        hold(&mut client, &["forward"]);
        client.clear_buttons();
        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(cmd.forwardmove, 0.0);
        assert_eq!(cmd.buttons, ButtonBits::NONE);
    }

    /// `AdjustYaw`. `+left` turns *left*, which is increasing yaw — the mirror
    /// of the mouse, where right is decreasing yaw.
    #[test]
    fn the_arrow_keys_turn_the_view() {
        let mut client = client();
        hold(&mut client, &["left"]);

        // Pressed during this frame, so half of it: 210 / 60 / 2.
        frame(&mut client, (0.0, 0.0));
        assert!((client.player.angles.yaw - 1.75).abs() < 1e-4, "{}", client.player.angles.yaw);

        // Held throughout the next.
        frame(&mut client, (0.0, 0.0));
        assert!((client.player.angles.yaw - 5.25).abs() < 1e-4, "{}", client.player.angles.yaw);
    }

    /// `+strafe` suppresses `AdjustYaw` and hands `+left`/`+right` to
    /// `ComputeSideMove` instead — the two never read the same `KeyState`.
    #[test]
    fn holding_strafe_makes_the_arrow_keys_strafe_rather_than_turn() {
        let mut client = client();
        hold(&mut client, &["strafe", "left"]);

        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(client.player.angles.yaw, 0.0, "the view did not turn");
        assert_eq!(cmd.sidemove, -CL_SIDESPEED * 0.5, "it strafed instead");
    }

    /// `AdjustPitch` is gated on `cl_mouselook` being **off**, and it defaults
    /// on — so out of the shipped configuration these keys do nothing, which is
    /// correct rather than broken.
    #[test]
    fn keyboard_pitch_needs_cl_mouselook_off() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        client.buttons_mut().apply("lookdown", true, Some(1));

        frame(&mut client, (0.0, 0.0));
        assert_eq!(client.player.angles.pitch, 0.0, "mouselook is on");

        console
            .cvars()
            .find("cl_mouselook")
            .expect("registered")
            .set_string("0");
        frame(&mut client, (0.0, 0.0));
        // Held for the whole of that frame: 225 / 60. Positive is downwards.
        assert!(
            (client.player.angles.pitch - 3.75).abs() < 1e-4,
            "{}",
            client.player.angles.pitch
        );
    }

    /// The surprise in `cl_mouselook`: turning it off does **not** take the
    /// mouse away. `ControllerMove` gates the mouse on `cl_mouseenable`
    /// (`in_main.cpp:1199`), never on this.
    #[test]
    fn cl_mouselook_off_still_lets_the_mouse_look() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        console
            .cvars()
            .find("cl_mouselook")
            .expect("registered")
            .set_string("0");

        let cmd = frame(&mut client, (100.0, 0.0));
        assert!(cmd.viewangles.yaw < 0.0, "the mouse still turns the view");
    }

    /// `+klook` makes forward and back pitch instead of moving, and
    /// `ComputeForwardMove` steps aside for it.
    #[test]
    fn klook_turns_forward_and_back_into_pitch() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        console
            .cvars()
            .find("cl_mouselook")
            .expect("registered")
            .set_string("0");
        hold(&mut client, &["klook", "forward"]);

        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(cmd.forwardmove, 0.0, "it did not walk");
        assert!(
            client.player.angles.pitch < 0.0,
            "it looked up: {}",
            client.player.angles.pitch
        );
    }

    /// `cl_anglespeedkey` is **0.67**, where `+speed` halves *movement*.
    #[test]
    fn walking_turns_at_two_thirds_speed_and_moves_at_one_half() {
        let mut fast = client();
        hold(&mut fast, &["left"]);
        frame(&mut fast, (0.0, 0.0));

        let mut slow = client();
        hold(&mut slow, &["left", "speed"]);
        frame(&mut slow, (0.0, 0.0));

        let ratio = slow.player.angles.yaw / fast.player.angles.yaw;
        assert!((ratio - view::CL_ANGLESPEEDKEY).abs() < 1e-4, "{ratio}");
    }

    /// The budget: one frame's worth of real time, however many commands are
    /// built from it. Today that is one, so the second turns nothing.
    #[test]
    fn the_keyboard_budget_is_spent_once_per_frame() {
        let mut client = client();
        hold(&mut client, &["left"]);

        client.set_sample_time(TICK);
        client.create_move(TICK, (0.0, 0.0));
        let after_first = client.player.angles.yaw;
        assert!(after_first > 0.0);

        client.create_move(TICK, (0.0, 0.0));
        assert_eq!(
            client.player.angles.yaw, after_first,
            "the frame's budget was already spent"
        );
    }

    /// **The failure mode `set_sample_time` documents**: forget the refill and
    /// keyboard look silently does nothing, for ever.
    #[test]
    fn without_a_refill_keyboard_look_does_nothing() {
        let mut client = client();
        hold(&mut client, &["left"]);
        client.create_move(TICK, (0.0, 0.0));
        assert_eq!(client.player.angles.yaw, 0.0);
    }

    #[test]
    fn in_usekeyboardsampletime_zero_removes_the_budget() {
        let mut console = Console::detached();
        let mut client = Client::new(&mut console);
        console
            .cvars()
            .find("in_usekeyboardsampletime")
            .expect("registered")
            .set_string("0");
        client.buttons_mut().apply("left", true, Some(1));

        // No refill at all, and two commands both turn.
        client.create_move(TICK, (0.0, 0.0));
        let after_first = client.player.angles.yaw;
        assert!(after_first > 0.0);
        client.create_move(TICK, (0.0, 0.0));
        assert!(client.player.angles.yaw > after_first);
    }

    /// The documented placeholder: Portal 2 binds no key to `+moveup`, so
    /// `+jump` flies. Reading it must not disturb `IN_JUMP`.
    #[test]
    fn jump_and_duck_drive_the_placeholder_vertical_axis() {
        let mut client = client();
        hold(&mut client, &["jump"]);
        let cmd = frame(&mut client, (0.0, 0.0));
        assert_eq!(cmd.upmove, CL_UPSPEED);
        assert!(
            cmd.buttons.contains(ButtonBits::JUMP),
            "and the button bit survives the placeholder reading it"
        );
    }
}
