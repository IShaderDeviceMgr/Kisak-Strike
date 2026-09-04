//! The game window and the event loop that drives it.
//!
//! Replaces `engine/sys_mainwind.cpp` (`CGame`), `engine/sys_getmodes.cpp`
//! (`CVideoMode`), `appframework/sdlmgr.cpp` (`CSDLMgr`),
//! `appframework/cocoamgr.mm`, `inputsystem/` and the vendored
//! `thirdparty/SDL2` — the entire left column of `portdocs/ENGINE.md` §6's
//! input path, which existed only to normalize platform events into
//! `InputEvent_t`. `winit` delivers them normalized already.
//!
//! # The control-flow inversion
//!
//! `portdocs/ENGINE.md` §6 is the standing analysis; the short version is that
//! Source's loop pulls (`CEngineAPI::MainLoop` calls `PumpMessages()` then
//! `eng->Frame()`) and `winit` pushes (it calls `resumed`, `window_event`,
//! `about_to_wait`). Worse, `CEngine::Frame` paces *itself*, sleeping inside
//! the call when `FilterTime` says the frame is early — which would mean
//! sleeping inside a callback `winit` scheduled.
//!
//! Where that has landed:
//!
//! - `about_to_wait` decides *when* the next frame happens and asks for it;
//!   `RedrawRequested` runs one engine tick through
//!   [`Engine::frame`](crate::engine::Engine::frame).
//! - **`FilterTime` is split in two.** Its *policy* — should this frame run,
//!   and if not, when may the next one — lives in
//!   [`crate::engine::host::FrameClock`]; its *mechanism* is
//!   `ControlFlow::WaitUntil`, here. That is exactly the division §6 asks for,
//!   and it is why **nothing in this module sleeps** and nothing in the host
//!   knows what a control flow is. Two deadlines can be outstanding at once —
//!   the engine's `fps_max` and this module's [`SKIP_RETRY`] — and
//!   [`GameWindow::about_to_wait`] takes the later.
//! - **Quit vs. restart is modelled**: [`run`] returns a [`RunOutcome`], which
//!   is `CEngineAPI::MainLoop`'s `RUN_OK`/`RUN_RESTART`. Closing the window
//!   does not exit the loop directly — it asks the engine to shut down, so the
//!   host state machine still unloads the level on the way out.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::window::{Fullscreen, Window, WindowId};

// `CommandLine` sits under `launcher` because that is where it is built, but
// Valve kept `CommandLine()` in tier0 precisely because everything reads it.
// If a third subsystem needs it, move it to a crate-level `src/cmdline.rs`
// rather than growing more of these imports.
use crate::engine::host::Outcome;
use crate::engine::Engine;
use crate::filesystem::Vfs;
use crate::launcher::cmdline::CommandLine;
use crate::materials::{Renderer, RendererOptions};

/// Fallback window title, from `CGame::CreateGameWindow`
/// (`engine/sys_mainwind.cpp:1266`) — used when `gameinfo.txt` has no `game`
/// key.
const DEFAULT_TITLE: &str = "HALF-LIFE 2";

/// Default windowed size.
///
/// **Deliberate divergence.** `MaterialSystem_Config_t`'s constructor says
/// 640x480 (`public/materialsystem/materialsystem_config.h:160`), but that is
/// a placeholder: `ReadVideoConfig()` overwrites it from `videoconfig.cfg`
/// before a window is ever created. Reproducing the placeholder would not be
/// fidelity. Once the video config file is read, this constant stops being
/// reachable in normal startup.
const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;

/// How long to idle after the renderer skips a frame before trying again.
///
/// **This is load-bearing, not a nicety.** A window that is off screen — behind
/// another window, minimized, on another desktop — makes every frame acquisition
/// fail, and asking again immediately is a spin loop at 100% of a core that
/// renders nothing. Measured on macOS/Metal before this existed: ~75,000
/// failed acquisitions per second.
///
/// It cannot be driven off `WindowEvent::Occluded` instead, which was the first
/// attempt: on macOS a window covered by another application produces the
/// failed acquisitions without producing that event, so a handler waiting for
/// it waits forever. The surface itself is the only signal that is always
/// right, so the retry is what un-occlusion is detected *by* — which is also
/// why this backs off rather than idling until an event arrives.
///
/// 100 ms means ~10 wasted wake-ups a second while off screen, and up to a
/// 100 ms stall on the rare transient skip (`Timeout`) of a window that is
/// visible. Trading a visible stall against a background spin, the stall wins.
const SKIP_RETRY: Duration = Duration::from_millis(100);

/// Video settings resolved at startup — the video half of
/// `MaterialSystem_Config_t`, minus everything that describes 2004-era
/// hardware variety.
///
/// # Defaults that differ from Valve's, and why
///
/// Valve's defaults live in two places: a constructor
/// (`materialsystem_config.h:140`) and `videoconfig.cfg`, read by
/// `ReadVideoConfig()`. **We have the first and not the second**, and the
/// constructor's values are placeholders that never survive to window
/// creation. So the defaults here are the ones that make an unconfigured
/// developer build behave sensibly, and each one keeps Valve's own switch as
/// the override:
///
/// | Setting | Valve's constructor | Here | Override |
/// |---|---|---|---|
/// | Windowed | fullscreen | **windowed** | `-fullscreen` / `-full` |
/// | Size | 640x480 | **1280x720** | `-width`/`-w`, `-height`/`-h` |
/// | Vsync | off (`NO_WAIT_FOR_VSYNC` set) | **on** | `-mat_vsync 0` |
/// | Resizable | off | off | `-resizing` |
///
/// Vsync is the one worth arguing about: with no `fps_max` and no
/// `FilterTime` yet, vsync is the *only* thing pacing the frame loop, and
/// without it a cleared window spins the GPU as fast as it will go. Revisit
/// when the host loop owns pacing.
#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
    /// `-noborder` (`engine/sys_getmodes.cpp:1341`). Windowed mode only;
    /// fullscreen is borderless by definition.
    pub borderless: bool,
    pub resizable: bool,
    pub adapter_index: Option<usize>,
    pub vsync: bool,
}

impl VideoConfig {
    /// Applies the command line, in the original's order.
    ///
    /// Port of `OverrideMaterialSystemConfigFromCommandLine`
    /// (`engine/matsys_interface.cpp:356`) plus the title handling from
    /// `CGame::CreateGameWindow` (`engine/sys_mainwind.cpp:1253`).
    /// `game_title` is `gameinfo.txt`'s `game` key.
    ///
    /// Two behaviors carried across deliberately, both of which look like bugs
    /// and are not:
    ///
    /// - `-width` without a matching `-height` forces 4:3
    ///   (`height = width * 3 / 4`), rather than keeping the previous height.
    /// - When both `-width` and `-w` are given, `-w` wins, because the
    ///   original reads them in that order into the same field.
    ///
    /// Not carried across: the `" - OpenGL"` title suffix
    /// (`sys_mainwind.cpp:1271`), which advertised the `togl` path that no
    /// longer exists. The backend in use is logged by the renderer instead.
    pub fn from_command_line(cmdline: &CommandLine, game_title: Option<&str>) -> Self {
        let mut config = VideoConfig {
            title: window_title(cmdline, game_title),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            fullscreen: false,
            borderless: cmdline.has("-noborder"),
            resizable: cmdline.has("-resizing"),
            adapter_index: parm_value(cmdline, "-adapter"),
            vsync: true,
        };

        if cmdline.has("-sw")
            || cmdline.has("-startwindowed")
            || cmdline.has("-windowed")
            || cmdline.has("-window")
        {
            config.fullscreen = false;
        } else if cmdline.has("-full") || cmdline.has("-fullscreen") {
            config.fullscreen = true;
        }

        let has_height = cmdline.has("-height") || cmdline.has("-h");
        if cmdline.has("-width") || cmdline.has("-w") {
            config.width = parm_value(cmdline, "-width").unwrap_or(config.width);
            config.width = parm_value(cmdline, "-w").unwrap_or(config.width);
            if !has_height {
                config.height = (config.width * 3) / 4;
            }
        }
        if has_height {
            config.height = parm_value(cmdline, "-height").unwrap_or(config.height);
            config.height = parm_value(cmdline, "-h").unwrap_or(config.height);
        }

        if let Some(vsync) = parm_value::<u32>(cmdline, "-mat_vsync") {
            config.vsync = vsync != 0;
        }

        // `-safe` (`matsys_interface.cpp:440`). Anti-aliasing and refresh rate
        // are part of it in the original; neither is implemented yet.
        if cmdline.has("-safe") {
            config.fullscreen = false;
            config.width = 640;
            config.height = 480;
        }

        config
    }

    fn renderer_options(&self) -> RendererOptions {
        RendererOptions {
            adapter_index: self.adapter_index,
            vsync: self.vsync,
        }
    }

    fn window_attributes(&self) -> winit::window::WindowAttributes {
        let attributes = Window::default_attributes()
            .with_title(self.title.clone())
            .with_resizable(self.resizable);

        if self.fullscreen {
            // Borderless on the current monitor, not an exclusive video mode.
            // `CVideoMode_Common`'s mode enumeration and `AdjustWindow`'s
            // mode-switching (`sys_getmodes.cpp`) are not ported: on a modern
            // compositor an exclusive mode change buys nothing and costs a
            // display reconfiguration on every alt-tab.
            attributes.with_fullscreen(Some(Fullscreen::Borderless(None)))
        } else {
            attributes
                .with_inner_size(PhysicalSize::new(self.width, self.height))
                .with_decorations(!self.borderless)
        }
    }
}

/// Builds the window title.
///
/// `gameinfo.txt`'s `game` key, else [`DEFAULT_TITLE`], plus
/// `-window_name_suffix` (`sys_mainwind.cpp:1279`).
fn window_title(cmdline: &CommandLine, game_title: Option<&str>) -> String {
    let mut title = game_title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(DEFAULT_TITLE)
        .to_owned();

    if let Some(suffix) = cmdline.value("-window_name_suffix").map(str::trim) {
        if !suffix.is_empty() {
            title.push_str(" - ");
            title.push_str(suffix);
        }
    }
    title
}

/// `CCommandLine::ParmValue` for numbers: the switch's value if it is present
/// and parses, otherwise nothing.
fn parm_value<T: std::str::FromStr>(cmdline: &CommandLine, name: &str) -> Option<T> {
    cmdline.value(name)?.parse().ok()
}

/// Anything that can stop the window from existing.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("could not run the window event loop: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),

    #[error("could not create the game window: {0}")]
    Creation(#[from] winit::error::OsError),

    #[error(transparent)]
    Renderer(#[from] crate::materials::RendererError),
}

/// How the engine stopped.
///
/// `CEngineAPI::MainLoop`'s return value (`engine/sys_dll2.cpp:1132`), which
/// distinguished `RUN_OK` from `RUN_RESTART` so that the launcher's restart
/// loop could tell "the user quit" from "the engine wants to come back". That
/// distinction is what `PORTING.md` requires to survive the `winit` inversion,
/// and this is where it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// `QUIT_TODESKTOP`, or the window was closed.
    Quit,
    /// `QUIT_RESTART`.
    Restart,
}

/// What the engine needs at startup, beyond the video settings.
///
/// `run`'s signature grew a struct at the point predicted in this module's
/// first version: these are `CEngineAPI::Run`'s arguments, and they are a value
/// rather than four positional parameters because they are about to keep
/// growing — the client, the server and the console each add one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Boot<'a> {
    /// The mounted game content, or `None` if it failed to mount.
    pub vfs: Option<&'a Vfs>,
    /// `+map <name>` — the map to load at startup.
    pub map: Option<&'a str>,
    /// `-vmt <name>` — draw one material instead of the world.
    pub test_material: Option<&'a str>,
    /// `+fps_max <n>`.
    pub fps_max: f32,
}

/// Creates the game window and runs the frame loop until the game quits.
///
/// This is `CEngineAPI::MainLoop`'s place in the boot sequence
/// (`engine/sys_dll2.cpp:1132`), and on POSIX it must be called from the main
/// thread — a hard requirement of macOS's AppKit that `winit` enforces.
pub fn run(config: VideoConfig, boot: Boot<'_>) -> Result<RunOutcome, WindowError> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = GameWindow::new(config, event_loop.owned_display_handle(), boot);
    event_loop.run_app(&mut app)?;

    // A failure inside a `winit` callback cannot be returned from it, so it is
    // parked on the handler and re-raised here.
    match app.startup_error {
        Some(err) => Err(err),
        None => Ok(app.outcome),
    }
}

/// The `winit` application handler: everything Valve's `CGame` and `CSDLMgr`
/// were between them, minus the event normalization `winit` already does.
struct GameWindow<'a> {
    config: VideoConfig,
    /// The display connection, kept because the renderer is built later, in
    /// `resumed`, and `wgpu` wants it on the instance.
    display: OwnedDisplayHandle,
    boot: Boot<'a>,
    /// Declared before `window` so the surface is torn down before the window
    /// it points at.
    renderer: Option<Renderer>,
    window: Option<Arc<Window>>,
    /// Built in `resumed`, once there is a device to build it against.
    engine: Option<Engine<'a>>,
    /// When to try again after a skipped frame. See [`SKIP_RETRY`].
    retry_at: Option<Instant>,
    /// Whether the milestone line has been printed. See [`GameWindow::draw`].
    presented: bool,
    outcome: RunOutcome,
    startup_error: Option<WindowError>,
}

impl<'a> GameWindow<'a> {
    fn new(config: VideoConfig, display: OwnedDisplayHandle, boot: Boot<'a>) -> Self {
        GameWindow {
            config,
            display,
            boot,
            renderer: None,
            window: None,
            engine: None,
            retry_at: None,
            presented: false,
            outcome: RunOutcome::Quit,
            startup_error: None,
        }
    }

    fn create(&mut self, event_loop: &ActiveEventLoop) -> Result<(), WindowError> {
        let window = Arc::new(event_loop.create_window(self.config.window_attributes())?);

        // The physical size the window actually got, which is not necessarily
        // the size asked for: a compositor may have overridden it, and in
        // fullscreen nothing was asked for at all.
        let size = window.inner_size();
        let renderer = Renderer::new(
            window.clone(),
            self.display.clone(),
            (size.width, size.height),
            &self.config.renderer_options(),
        )?;

        let mut engine = Engine::new(
            renderer.device(),
            renderer.queue(),
            self.boot.vfs,
            self.boot.fps_max,
            self.boot.test_material,
        );

        // `+map` is the console command spelled as a command-line argument,
        // which is how Source has always started on a map. It is queued rather
        // than loaded: the host state machine loads it on the first frame, so
        // startup and a later `map` command take exactly the same path.
        if let Some(map) = self.boot.map {
            engine.request_new_game(map);
        }

        self.engine = Some(engine);
        self.window = Some(window);
        self.renderer = Some(renderer);
        Ok(())
    }

    /// One frame: the engine tick, then the picture it produced.
    ///
    /// The order here is the frame boundary `rustdocs/MATERIALS.md` describes,
    /// with the engine wrapped around it:
    ///
    /// 1. `Engine::frame` — the clock decides whether a frame runs at all, then
    ///    the host state machine runs it. **Before** the surface is acquired, so
    ///    a refused frame costs nothing and a map load does not hold a
    ///    swap-chain image across it.
    /// 2. `Renderer::begin_frame` — acquire, or back off.
    /// 3. `Engine::render` — one pass.
    /// 4. `present`.
    ///
    /// Prints one milestone line the first time a frame reaches the screen.
    /// Creating a device and creating a window both succeed on machines where
    /// nothing is ever presented, so "the window opened" is not evidence that
    /// the GPU path works; this line is. It is the startup-log equivalent of
    /// the `COM_TimestampedLog` calls that bracketed `CreateGameWindow`
    /// (`engine/sys_getmodes.cpp:537`).
    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(renderer), Some(engine)) =
            (&self.window, &mut self.renderer, &mut self.engine)
        else {
            return;
        };

        // `None` means this frame is early. `about_to_wait` reads the engine's
        // deadline and waits it out; nothing sleeps here.
        let Some(outcome) = engine.frame(Instant::now()) else {
            return;
        };

        match outcome {
            Outcome::Continue => {}
            Outcome::Quit | Outcome::Restart => {
                self.outcome = match outcome {
                    Outcome::Restart => RunOutcome::Restart,
                    _ => RunOutcome::Quit,
                };
                // The level has already been torn down by the state machine on
                // its way through `GameShutdown`.
                event_loop.exit();
                return;
            }
        }

        // `None` is the ordinary "not on screen right now" answer, not a
        // failure — see `Renderer::begin_frame`. Back off rather than asking
        // again immediately; see `about_to_wait`.
        let Some(mut frame) = renderer.begin_frame() else {
            self.retry_at = Some(Instant::now() + SKIP_RETRY);
            return;
        };

        engine.render(&mut frame);

        // Tells the compositor a frame is imminent, so it can schedule
        // accordingly. Must be immediately before the present.
        window.pre_present_notify();
        frame.present();
        self.retry_at = None;

        if !self.presented {
            self.presented = true;
            eprintln!("source-engine: renderer: first frame presented");
        }
    }
}

impl ApplicationHandler for GameWindow<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` fires again after a suspend on mobile targets, which are
        // out of scope; on desktop it fires once. Guard anyway, so that a
        // second call cannot silently replace a live device.
        if self.window.is_some() {
            return;
        }
        if let Err(err) = self.create(event_loop) {
            self.startup_error = Some(err);
            event_loop.exit();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            // Not an immediate exit: the engine is asked to shut down and the
            // state machine unloads the level on its way out, which is what
            // `HostState_Shutdown` does. The frame that carries it out is the
            // next one, so the backoff is cleared and a redraw asked for
            // rather than waiting for the retry deadline.
            WindowEvent::CloseRequested => match &mut self.engine {
                Some(engine) => {
                    engine.request_shutdown();
                    self.retry_at = None;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
                None => event_loop.exit(),
            },

            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }

            // A DPI change is followed by a `Resized`, so there is nothing to
            // do here; the surface is sized in physical pixels either way.
            WindowEvent::ScaleFactorChanged { .. } => {}

            WindowEvent::RedrawRequested => self.draw(event_loop),

            // Input arrives here. `portdocs/ENGINE.md` §6 has the chain it
            // replaces and the open question about UI event precedence, which
            // `egui` collapses into one "did the UI consume this" answer.
            _ => {}
        }
    }

    /// Decides when the next frame happens.
    ///
    /// Two deadlines can be outstanding, and the later one wins because both
    /// are "do not come back before":
    ///
    /// - **The renderer's**, when a frame was skipped ([`SKIP_RETRY`]). The
    ///   window owns this one; the engine cannot, because a surface that
    ///   refuses to hand over an image is not an engine concept.
    /// - **The engine's**, when `FilterTime` refused a frame as early
    ///   (`fps_max`). The engine owns the *policy* and this owns the
    ///   *mechanism*, which is the split `portdocs/ENGINE.md` §6 asks for:
    ///   `CEngine::Frame` slept inside itself, and a callback `winit`
    ///   scheduled must not sleep.
    ///
    /// **Nothing in this module sleeps.** Note also that no redraw is requested
    /// while waiting: a pending redraw request wakes the loop, which would
    /// defeat the deadline entirely.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let engine_deadline = self.engine.as_ref().and_then(Engine::deadline);

        if let Some(deadline) = self.retry_at {
            if now >= deadline {
                self.retry_at = None;
            }
        }

        let deadline = [self.retry_at, engine_deadline]
            .into_iter()
            .flatten()
            .filter(|&deadline| deadline > now)
            .max();

        if let Some(deadline) = deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }

        event_loop.set_control_flow(ControlFlow::Poll);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `game_title` is `gameinfo.txt`'s `game` key; most tests don't care.
    fn config(args: &[&str]) -> VideoConfig {
        let cmdline = CommandLine::from_args(std::iter::once("game").chain(args.iter().copied()));
        VideoConfig::from_command_line(&cmdline, Some("Portal 2"))
    }

    #[test]
    fn defaults_are_a_windowed_720p_window() {
        let config = config(&[]);
        assert_eq!((config.width, config.height), (1280, 720));
        assert!(!config.fullscreen);
        assert!(!config.borderless);
        assert!(!config.resizable, "MATSYS_VIDCFG_FLAGS_RESIZING is off");
        assert!(config.vsync, "the only frame limiter we have yet");
        assert_eq!(config.adapter_index, None);
    }

    #[test]
    fn every_windowed_switch_spelling_is_accepted() {
        for switch in ["-sw", "-startwindowed", "-windowed", "-window"] {
            assert!(!config(&[switch, "-fullscreen"]).fullscreen, "{switch}");
        }
        for switch in ["-full", "-fullscreen"] {
            assert!(config(&[switch]).fullscreen, "{switch}");
        }
    }

    #[test]
    fn width_without_height_forces_four_by_three() {
        // `matsys_interface.cpp:386`. Looks like a bug, is not.
        let config = config(&["-width", "800"]);
        assert_eq!((config.width, config.height), (800, 600));
    }

    #[test]
    fn width_with_height_leaves_the_height_alone() {
        let config = config(&["-width", "1920", "-height", "1080"]);
        assert_eq!((config.width, config.height), (1920, 1080));
    }

    #[test]
    fn short_forms_win_over_long_ones() {
        // Both are read into the same field, `-w`/`-h` second
        // (`matsys_interface.cpp:382-392`).
        let config = config(&[
            "-width", "1920", "-w", "1024", "-height", "1080", "-h", "768",
        ]);
        assert_eq!((config.width, config.height), (1024, 768));
    }

    #[test]
    fn short_height_alone_suppresses_the_four_by_three_rule() {
        let config = config(&["-width", "800", "-h", "1000"]);
        assert_eq!((config.width, config.height), (800, 1000));
    }

    #[test]
    fn unparsable_sizes_fall_back_rather_than_producing_a_zero_size_window() {
        // A zero dimension would panic `Surface::configure`.
        let config = config(&["-width", "wide"]);
        assert_eq!(config.width, 1280);
        assert_eq!(config.height, 960, "still 4:3 of the surviving width");
    }

    #[test]
    fn vsync_follows_mat_vsync() {
        assert!(!config(&["-mat_vsync", "0"]).vsync);
        assert!(config(&["-mat_vsync", "1"]).vsync);
        assert!(config(&["-mat_vsync"]).vsync, "no value, no change");
    }

    #[test]
    fn safe_mode_overrides_everything_before_it() {
        let config = config(&["-fullscreen", "-width", "1920", "-height", "1080", "-safe"]);
        assert!(!config.fullscreen);
        assert_eq!((config.width, config.height), (640, 480));
    }

    #[test]
    fn border_resizing_and_adapter_switches() {
        assert!(config(&["-noborder"]).borderless);
        assert!(config(&["-resizing"]).resizable);
        assert_eq!(config(&["-adapter", "1"]).adapter_index, Some(1));
        assert_eq!(config(&["-adapter", "gpu"]).adapter_index, None);
    }

    #[test]
    fn the_title_comes_from_gameinfo() {
        let cmdline = CommandLine::from_args(["game"]);
        assert_eq!(
            VideoConfig::from_command_line(&cmdline, Some("Portal 2")).title,
            "Portal 2"
        );
        assert_eq!(
            VideoConfig::from_command_line(&cmdline, None).title,
            DEFAULT_TITLE
        );
        assert_eq!(
            VideoConfig::from_command_line(&cmdline, Some("   ")).title,
            DEFAULT_TITLE,
            "a blank `game` key is no title at all"
        );
    }

    #[test]
    fn window_name_suffix_is_appended() {
        assert_eq!(
            config(&["-window_name_suffix", "dev"]).title,
            "Portal 2 - dev"
        );
        assert_eq!(
            config(&["-window_name_suffix"]).title,
            "Portal 2",
            "no value, no suffix"
        );
    }
}
