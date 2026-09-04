//! The host: the frame clock and the state machine that owns level lifetime.
//!
//! Replaces `engine/host_state.cpp` (the state machine) and the timing half of
//! `engine/sys_engine.cpp`'s `CEngine::Frame`/`FilterTime` — `portdocs/ENGINE.md`
//! §7.2 and §6. Everything here is orchestration: the host decides *that* a map
//! should be loaded and *when* a frame should run, and knows nothing about how
//! either is done. [`Level`] is that seam, which is why this module compiles
//! without `wgpu`, `winit` or the material system, and why its state machine is
//! tested without a GPU.
//!
//! # What became of `CEngine`
//!
//! The original runs *two* state machines, one inside the other. `CEngine`
//! holds `m_nDLLState`/`m_nNextDLLState` (`DLL_ACTIVE`, `DLL_CLOSE`,
//! `DLL_RESTART`) and translates a change in them into
//! `SetQuitting(QUIT_TODESKTOP)`/`SetQuitting(QUIT_RESTART)`, which
//! `CEngineAPI::MainLoop` then polls with `GetQuitting()`
//! (`sys_engine.cpp:589-611`, `sys_dll2.cpp:1132`). `CHostState` holds the
//! other one and reaches the first through `eng->SetNextState()`.
//!
//! **The outer one is deleted.** It exists to carry a decision across the
//! `IEngine` interface boundary by polling, and there is no such boundary here:
//! [`Host::frame`] returns the decision as an [`Outcome`]. The
//! quit-versus-restart distinction that `m_nQuitting` carried survives exactly
//! as `PORTING.md` requires it to, in the return value.
//!
//! # Pacing
//!
//! `CEngine::Frame` sleeps inside itself when a frame is early
//! (`ThreadNanoSleep`, `sys_engine.cpp:498`), which is the collision
//! `portdocs/ENGINE.md` §6 warns about: `winit` wants to own that wait through
//! `ControlFlow`. The split here is that **this module owns the policy and
//! `src/engine/window/` owns the mechanism** — [`FrameClock::frame`] decides
//! whether a frame runs, [`FrameClock::deadline`] says when the next one may,
//! and the window turns that into `ControlFlow::WaitUntil`. Nothing here
//! sleeps, and nothing in the window decides.

use std::time::{Duration, Instant};

/// `DEFAULT_FPS_MAX` (`engine/sys_engine.cpp:60`) — the `fps_max` convar's
/// default.
pub const DEFAULT_FPS_MAX: f32 = 300.0;

/// `MAX_FPS` (`engine/host.h:185`) — the ceiling `FilterTime` clamps to.
const MAX_FPS: f32 = 1000.0;

/// `MAX_FRAMETIME`/`MIN_FRAMETIME` (`engine/host.h:187-188`).
///
/// `Host_RunFrame` clamps the frame time into this range before anything
/// simulates against it (`engine/host.cpp:2448`). The upper bound is what stops
/// a two-second hitch — alt-tabbing away, or loading a map — from being
/// simulated as two seconds of physics in one step and putting everything
/// through a wall.
const MAX_FRAMETIME: f32 = 0.1;
const MIN_FRAMETIME: f32 = 0.001;

/// How the frame loop should continue.
///
/// `CEngine`'s `m_nQuitting` (`QUIT_NOTQUITTING`/`QUIT_TODESKTOP`/
/// `QUIT_RESTART`), as a return value rather than a field to poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Keep running.
    Continue,
    /// `QUIT_TODESKTOP` — leave the process.
    Quit,
    /// `QUIT_RESTART` — tear down and start again. The launcher's restart loop
    /// is what must eventually distinguish this from [`Outcome::Quit`].
    Restart,
}

/// The states the host moves through.
///
/// `HOSTSTATES` (`engine/host_state.cpp:54`) minus the three that have nothing
/// to reach yet: `HS_LOAD_GAME` needs `save/`, and `HS_CHANGE_LEVEL_SP`/`_MP`
/// need level transitions and a server. They are omitted rather than stubbed —
/// an unreachable variant is scaffolding, and `PORTING.md` asks for the
/// knowledge without the encoding. **The knowledge worth keeping is the shape**:
/// every path from [`Run`](HostState::Run) to a new level goes *through*
/// [`GameShutdown`](HostState::GameShutdown), so a level is always torn down
/// before the next one is built. That is why `State_Run` funnels four different
/// requests into the same state instead of jumping straight to them, and it is
/// reproduced exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    /// `HS_RUN` — the steady state, and the only one that runs once per frame
    /// rather than looping until it reaches another.
    Run,
    /// `HS_NEW_GAME` — load [`Host::pending_map`] and start it.
    NewGame,
    /// `HS_GAME_SHUTDOWN` — unload the level, then go wherever `next` says.
    GameShutdown,
    /// `HS_SHUTDOWN` — leave.
    Shutdown,
    /// `HS_RESTART` — leave, and come back.
    Restart,
}

/// What the host drives when it changes level.
///
/// The state machine says *when* a level is loaded and unloaded; this says how.
/// Valve reached `modelloader`, `sv`, the client, the material system and six
/// other globals directly from inside `State_NewGame`; the whole of that from
/// the host's point of view is these two calls.
pub trait Level {
    /// Loads a map. The `Err` string is shown to the user and the host recovers
    /// by returning to [`HostState::Run`] with no level — `State_NewGame`'s
    /// "new game failed" path (`engine/host_state.cpp:437`), which is why a bad
    /// map name does not take the process down.
    fn load(&mut self, map: &str) -> Result<(), String>;

    /// Unloads whatever is loaded. Must tolerate being called with nothing
    /// loaded.
    fn unload(&mut self);
}

/// The frame clock: `CEngine`'s timing fields and `FilterTime`'s policy.
///
/// `CEngine::Frame` (`engine/sys_engine.cpp:418`) reads the clock, accumulates
/// `dt` into `m_flFrameTime`, and asks `FilterTime` whether enough has piled up
/// to be worth a frame. Time that is *not* worth a frame is not thrown away —
/// it accumulates, so a frame limiter never loses time, it only postpones it.
#[derive(Debug)]
pub struct FrameClock {
    /// `fps_max`. Zero means unlimited, as the convar does.
    fps_max: f32,
    /// `m_flPreviousTime`.
    previous: Option<Instant>,
    /// `m_flFrameTime` — time accumulated since the last frame that ran.
    accumulated: f32,
    /// `m_flFilteredTime` — time swallowed by frames that did not run. Reported
    /// so that "the limiter is working" and "the clock is broken" look
    /// different.
    filtered: f32,
    /// `m_flMinFrameTime`, as computed by the last `FilterTime`.
    min_frame_time: f32,
    /// When the next frame may run, if the last one was refused.
    deadline: Option<Instant>,
}

impl FrameClock {
    pub fn new(fps_max: f32) -> FrameClock {
        FrameClock {
            fps_max: fps_max.max(0.0),
            previous: None,
            accumulated: 0.0,
            filtered: 0.0,
            min_frame_time: 0.0,
            deadline: None,
        }
    }

    #[allow(dead_code)] // read by `fps_max` once `console/` can report a convar
    pub fn fps_max(&self) -> f32 {
        self.fps_max
    }

    #[allow(dead_code)] // written by the `fps_max` convar's callback
    pub fn set_fps_max(&mut self, fps_max: f32) {
        self.fps_max = fps_max.max(0.0);
    }

    /// Advances the clock. `Some(frame_time)` means run a frame; `None` means
    /// this one is early, and [`deadline`](FrameClock::deadline) says when to
    /// come back.
    ///
    /// The frame time is clamped into `MIN_FRAMETIME..MAX_FRAMETIME` on the way
    /// out, which `Host_RunFrame` does (`engine/host.cpp:2448`) rather than
    /// `CEngine::Frame`. It happens here because the clamp is a property of the
    /// number, not of who reads it, and every reader wants it clamped.
    pub fn frame(&mut self, now: Instant) -> Option<f32> {
        let dt = match self.previous.replace(now) {
            // A clock that went backwards would give a negative `dt`, which
            // `CEngine::Frame` guards against by returning early
            // (`sys_engine.cpp:475`). `Instant` is monotonic, so this saturates
            // at zero instead of needing the guard.
            Some(previous) => now.saturating_duration_since(previous).as_secs_f32(),
            // The first frame has no previous time to subtract.
            None => 0.0,
        };
        self.accumulated += dt;

        if !self.filter_time(self.accumulated) {
            self.filtered += dt;
            let remaining = (self.min_frame_time - self.accumulated).max(0.0);
            self.deadline = Some(now + Duration::from_secs_f32(remaining));
            return None;
        }

        let frame_time = self.accumulated;
        self.accumulated = 0.0;
        self.filtered = 0.0;
        self.deadline = None;
        Some(frame_time.clamp(MIN_FRAMETIME, MAX_FRAMETIME))
    }

    /// `CEngine::FilterTime` (`engine/sys_engine.cpp:264`), minus everything
    /// that needs a subsystem this port does not have.
    ///
    /// Dropped, each because its input does not exist yet rather than because
    /// it was judged unnecessary: the dedicated server's tick-rate lock
    /// (`sv.IsDedicated()`), the `fps_max < 30` cheat clamp (needs `sv_cheats`
    /// and a notion of a multiplayer game), `fps_max_splitscreen`,
    /// `fps_max_menu` (needs "is the client connected"), and the timedemo
    /// bypass. Each returns with its subsystem.
    fn filter_time(&mut self, dt: f32) -> bool {
        self.min_frame_time = 0.0;
        if self.fps_max <= 0.0 {
            // "Don't do anything if fps_max=0 (which means it's unlimited)."
            return true;
        }
        let min_frame_time = 1.0 / self.fps_max.min(MAX_FPS);
        self.min_frame_time = min_frame_time;
        dt >= min_frame_time
    }

    /// When the next frame may run, if the last call refused one.
    ///
    /// This is the sleep `CEngine::Frame` would have taken, handed to the
    /// caller as a deadline instead. See the module docs.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Time swallowed by refused frames since the last one that ran.
    #[allow(dead_code)] // `host_filtered_time_history`, once there is a console to show it
    pub fn filtered_time(&self) -> f32 {
        self.filtered
    }
}

/// The host state machine.
///
/// `CHostState` (`engine/host_state.cpp:66`), which was a file-static
/// singleton reached through eleven free functions. Here it is a value the
/// engine owns, and the free functions are its methods.
#[derive(Debug)]
pub struct Host {
    clock: FrameClock,
    current: HostState,
    next: HostState,
    /// `m_levelName` — the map [`HostState::NewGame`] will load.
    pending_map: Option<String>,
    /// `m_activeGame` — whether there is a level to tear down.
    active_game: bool,
    /// The frame time of the last frame that ran.
    frame_time: f32,
    /// Frames run since construction. `host_framecount`.
    frame_count: u64,
}

/// How many state transitions may happen in one frame before the machine is
/// declared broken.
///
/// `CHostState::FrameUpdate` calls `Host_Error("state crash!")` after eight in
/// a debug build and loops forever in a release one
/// (`engine/host_state.cpp:830`). Eight is kept; hanging is not. In the current
/// graph the longest real path is three (`Run` → `GameShutdown` → `NewGame` →
/// `Run`).
const MAX_TRANSITIONS: u32 = 8;

impl Host {
    pub fn new(fps_max: f32) -> Host {
        Host {
            clock: FrameClock::new(fps_max),
            current: HostState::Run,
            next: HostState::Run,
            pending_map: None,
            active_game: false,
            frame_time: 0.0,
            frame_count: 0,
        }
    }

    pub fn clock(&self) -> &FrameClock {
        &self.clock
    }

    #[allow(dead_code)] // the seam the `fps_max` convar writes through
    pub fn clock_mut(&mut self) -> &mut FrameClock {
        &mut self.clock
    }

    #[allow(dead_code)] // used by the tests; the console's `status` reports it
    pub fn state(&self) -> HostState {
        self.current
    }

    /// Whether a level is loaded.
    #[allow(dead_code)] // used by the tests; `sv.IsActive()`'s replacement
    pub fn has_level(&self) -> bool {
        self.active_game
    }

    pub fn frame_time(&self) -> f32 {
        self.frame_time
    }

    #[allow(dead_code)] // `host_framecount`, which the console and net code read
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// `HostState_NewGame` (`engine/host_state.cpp:130`) — load a map as soon
    /// as possible.
    ///
    /// The map is not loaded here; the state machine reaches it on the next
    /// frame, after tearing down whatever is loaded now. That deferral is the
    /// entire reason `CHostState` exists: `map` is a console command that can
    /// be typed from inside the level it is about to destroy.
    pub fn request_new_game(&mut self, map: &str) {
        // `Q_StripExtension` + `Q_FixSlashes` (`host_state.cpp:134-137`):
        // stored with forward slashes and no extension, so that two spellings
        // of one map compare equal.
        let map = map.replace('\\', "/");
        let map = map.strip_suffix(".bsp").unwrap_or(&map);
        self.pending_map = Some(map.to_owned());
        self.set_next_state(HostState::NewGame);
    }

    /// `HostState_Shutdown` — leave, unloading the level on the way.
    pub fn request_shutdown(&mut self) {
        self.set_next_state(HostState::Shutdown);
    }

    /// `HostState_Restart`.
    ///
    /// Nothing requests this yet — the `restart` console command is what does,
    /// and `console/` is unported. It exists because the *outcome* it produces
    /// has to be distinguishable from a quit all the way out to the launcher,
    /// and a path that cannot be exercised is a path that is wrong.
    #[allow(dead_code)]
    pub fn request_restart(&mut self) {
        self.set_next_state(HostState::Restart);
    }

    /// `CHostState::SetNextState` (`engine/host_state.cpp:374`).
    ///
    /// Valve asserts `m_currentState == HS_RUN` here, because a request made
    /// from inside a transition would be overwritten by that transition's own
    /// bookkeeping. Requests only ever arrive from a console command or a
    /// window event, both of which happen between frames, so the assertion
    /// holds; a request arriving mid-transition is dropped rather than
    /// corrupting the machine.
    fn set_next_state(&mut self, next: HostState) {
        if self.current == HostState::Run {
            self.next = next;
        }
    }

    fn set_state(&mut self, state: HostState, clear_next: bool) {
        self.current = state;
        if clear_next {
            self.next = state;
        }
    }

    /// Advances the clock and, if a frame is due, runs one.
    ///
    /// `None` means the frame was early — `FilterTime` said so — and
    /// [`FrameClock::deadline`] says when to try again. This is the whole of
    /// `CEngine::Frame`'s contract with `MainLoop`, minus the sleep.
    pub fn frame(&mut self, now: Instant, level: &mut dyn Level) -> Option<Outcome> {
        let frame_time = self.clock.frame(now)?;
        self.frame_time = frame_time;
        self.frame_count += 1;
        Some(self.run_states(level))
    }

    /// `CHostState::FrameUpdate` (`engine/host_state.cpp:751`).
    ///
    /// Every state except `Run` loops until it reaches one that does not, so a
    /// level change completes within the frame that asked for it rather than
    /// dribbling one transition per frame. `Run` executes once and stops.
    fn run_states(&mut self, level: &mut dyn Level) -> Outcome {
        for _ in 0..MAX_TRANSITIONS {
            let previous = self.current;

            match self.current {
                HostState::Run => self.state_run(),
                HostState::NewGame => self.state_new_game(level),
                HostState::GameShutdown => self.state_game_shutdown(level),
                // `State_Shutdown`/`State_Restart` set `DLL_CLOSE`/`DLL_RESTART`
                // on `CEngine` and let `MainLoop` notice. Returning is the same
                // decision without the round trip — see the module docs.
                HostState::Shutdown => return Outcome::Quit,
                HostState::Restart => return Outcome::Restart,
            }

            // "only do a single pass at HS_RUN per frame. All other states loop
            // until they reach HS_RUN" (`host_state.cpp:817`).
            if previous == HostState::Run {
                return Outcome::Continue;
            }
        }

        // Valve's `Host_Error("state crash!")`, which only fired in a debug
        // build; a release build looped here forever. Quitting loses a session,
        // hanging loses the process.
        eprintln!(
            "source-engine: host: state machine did not settle after {MAX_TRANSITIONS} \
             transitions (stuck in {:?} heading for {:?}); shutting down",
            self.current, self.next
        );
        Outcome::Quit
    }

    /// `CHostState::State_Run` (`engine/host_state.cpp:583`).
    ///
    /// Not ported from it: `Host_RunFrame` — the server tick, the client tick,
    /// sound and the whole simulation — because none of those subsystems
    /// exists. What *is* here is the transition table, which is the part that
    /// makes the rest safe to add later.
    ///
    /// Also not ported: `m_flShortFrameTime`, which runs a few deliberately
    /// short frames after a level load so that the first simulated step is not
    /// the whole load time. It clamps `frameTime` against
    /// `host_state.interval_per_tick`, and there is no tick interval yet
    /// because there is no simulation. It comes back with the server.
    fn state_run(&mut self) {
        match self.next {
            HostState::Run => {}

            // "The game must be shutdown before a new game can start, before a
            // game can load, and before the system can be shutdown. This is
            // done here instead of pathfinding through a state transition
            // graph." (`host_state.cpp:604`)
            //
            // `clear_next` is false on purpose: `GameShutdown` reads `next` to
            // learn where it is going afterwards.
            HostState::NewGame
            | HostState::Shutdown
            | HostState::Restart
            | HostState::GameShutdown => self.set_state(HostState::GameShutdown, false),
        }
    }

    /// `CHostState::State_NewGame` (`engine/host_state.cpp:409`).
    fn state_new_game(&mut self, level: &mut dyn Level) {
        let map = self.pending_map.clone().unwrap_or_default();

        match level.load(&map) {
            Ok(()) => {
                self.active_game = true;
                self.set_state(HostState::Run, true);
            }
            Err(err) => {
                // "new game failed": report it, tear down whatever was half
                // built, and go back to running with no level. The original
                // ends the loading plaque and runs the server at the console.
                eprintln!("source-engine: host: could not start {map}: {err}");
                self.game_shutdown(level);
                self.set_state(HostState::Run, true);
            }
        }
    }

    /// `CHostState::State_GameShutdown` (`engine/host_state.cpp:668`).
    fn state_game_shutdown(&mut self, level: &mut dyn Level) {
        self.game_shutdown(level);

        match self.next {
            HostState::NewGame | HostState::Shutdown | HostState::Restart => {
                self.set_state(self.next, true)
            }
            _ => self.set_state(HostState::Run, true),
        }
    }

    /// `CHostState::GameShutdown` (`engine/host_state.cpp:392`) — the guard
    /// that keeps a teardown from running against nothing.
    fn game_shutdown(&mut self, level: &mut dyn Level) {
        if self.active_game {
            level.unload();
            self.active_game = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`Level`] that records what it was asked to do.
    #[derive(Default)]
    struct Log {
        events: Vec<String>,
        fail: bool,
    }

    impl Level for Log {
        fn load(&mut self, map: &str) -> Result<(), String> {
            self.events.push(format!("load {map}"));
            if self.fail {
                Err("no such map".into())
            } else {
                Ok(())
            }
        }
        fn unload(&mut self) {
            self.events.push("unload".into());
        }
    }

    /// A host with no frame limiter, so every `frame` call runs one.
    fn host() -> Host {
        Host::new(0.0)
    }

    fn tick(host: &mut Host, level: &mut Log) -> Outcome {
        host.frame(Instant::now(), level)
            .expect("unlimited fps always runs a frame")
    }

    /// Runs frames until one asks the loop to stop, or until nothing is left to
    /// do. A request needs two: see
    /// [`a_request_takes_effect_on_the_frame_after_it_is_made`].
    fn settle(host: &mut Host, level: &mut Log) -> Outcome {
        let mut outcome = Outcome::Continue;
        for _ in 0..MAX_TRANSITIONS {
            outcome = tick(host, level);
            if outcome != Outcome::Continue {
                break;
            }
        }
        outcome
    }

    #[test]
    fn a_new_host_runs_without_a_level() {
        let mut level = Log::default();
        let mut host = host();
        assert_eq!(tick(&mut host, &mut level), Outcome::Continue);
        assert_eq!(host.state(), HostState::Run);
        assert!(!host.has_level());
        assert!(level.events.is_empty());
    }

    /// **A request is armed by one frame and carried out by the next**, and
    /// that is Valve's behavior rather than an accident of this port.
    /// `FrameUpdate` breaks out of its loop whenever the state it just ran was
    /// `HS_RUN` (`engine/host_state.cpp:817`), so `State_Run` only moves the
    /// machine to `HS_GAME_SHUTDOWN` — the work happens on the following
    /// frame. It is why `SCR_BeginLoadingPlaque` is called from `State_Run`:
    /// the loading screen goes up on the frame that arms the load, and is on
    /// screen for the frame that blocks doing it.
    #[test]
    fn a_request_takes_effect_on_the_frame_after_it_is_made() {
        let mut level = Log::default();
        let mut host = host();
        host.request_new_game("sp_a1_intro1");

        assert_eq!(tick(&mut host, &mut level), Outcome::Continue);
        assert!(
            level.events.is_empty(),
            "the first frame only arms the transition"
        );
        assert_eq!(host.state(), HostState::GameShutdown);

        assert_eq!(tick(&mut host, &mut level), Outcome::Continue);
        assert_eq!(level.events, ["load sp_a1_intro1"]);
        assert_eq!(host.state(), HostState::Run, "and it settles back in Run");
        assert!(host.has_level());
    }

    /// Once it does run, the whole chain — `GameShutdown` then `NewGame` then
    /// back to `Run` — happens inside that one frame rather than dribbling out
    /// one transition per frame.
    #[test]
    fn the_transition_chain_completes_within_a_single_frame() {
        let mut level = Log::default();
        let mut host = host();
        host.request_new_game("first");
        settle(&mut host, &mut level);
        level.events.clear();

        host.request_new_game("second");
        tick(&mut host, &mut level); // arms it
        tick(&mut host, &mut level); // unload and load, both here
        assert_eq!(level.events, ["unload", "load second"]);
        assert_eq!(host.state(), HostState::Run);
    }

    /// The load-bearing invariant of the whole machine: you cannot get from one
    /// level to the next without passing through `GameShutdown`, so a level is
    /// always torn down before the next is built.
    #[test]
    fn changing_level_unloads_the_old_one_first() {
        let mut level = Log::default();
        let mut host = host();
        host.request_new_game("first");
        settle(&mut host, &mut level);

        host.request_new_game("second");
        assert_eq!(settle(&mut host, &mut level), Outcome::Continue);
        assert_eq!(level.events, ["load first", "unload", "load second"]);
        assert!(host.has_level());
    }

    #[test]
    fn a_map_that_fails_to_load_leaves_the_host_running() {
        let mut level = Log {
            fail: true,
            ..Default::default()
        };
        let mut host = host();
        host.request_new_game("nonexistent");

        assert_eq!(settle(&mut host, &mut level), Outcome::Continue);
        assert_eq!(host.state(), HostState::Run);
        assert!(!host.has_level(), "a failed load leaves no level");

        // And the host still works afterwards.
        level.fail = false;
        host.request_new_game("good");
        assert_eq!(settle(&mut host, &mut level), Outcome::Continue);
        assert!(host.has_level());
    }

    #[test]
    fn quitting_unloads_the_level_on_the_way_out() {
        let mut level = Log::default();
        let mut host = host();
        host.request_new_game("sp_a1_intro1");
        settle(&mut host, &mut level);

        host.request_shutdown();
        assert_eq!(settle(&mut host, &mut level), Outcome::Quit);
        assert_eq!(level.events, ["load sp_a1_intro1", "unload"]);
    }

    /// The distinction `PORTING.md` requires to survive for the launcher's
    /// restart loop.
    #[test]
    fn restart_is_a_different_outcome_from_quit() {
        let mut level = Log::default();
        let mut restarting = host();
        restarting.request_restart();
        assert_eq!(settle(&mut restarting, &mut level), Outcome::Restart);

        let mut quitting = host();
        quitting.request_shutdown();
        assert_eq!(settle(&mut quitting, &mut level), Outcome::Quit);
    }

    #[test]
    fn quitting_with_no_level_loaded_does_not_unload_anything() {
        let mut level = Log::default();
        let mut host = host();
        host.request_shutdown();
        assert_eq!(settle(&mut host, &mut level), Outcome::Quit);
        assert!(level.events.is_empty());
    }

    #[test]
    fn a_map_name_is_stored_stripped() {
        let mut level = Log::default();
        let mut host = host();
        host.request_new_game("maps\\sp_a1_intro1.bsp");
        settle(&mut host, &mut level);
        assert_eq!(level.events, ["load maps/sp_a1_intro1"]);
    }

    // --- the clock ---------------------------------------------------------

    #[test]
    fn an_unlimited_clock_runs_every_frame() {
        let mut clock = FrameClock::new(0.0);
        let now = Instant::now();
        assert!(clock.frame(now).is_some());
        assert!(clock.frame(now).is_some(), "even with no time elapsed");
        assert_eq!(clock.deadline(), None);
    }

    #[test]
    fn a_limited_clock_refuses_early_frames_and_says_when_to_return() {
        let mut clock = FrameClock::new(100.0); // 10 ms a frame
        let start = Instant::now();
        assert!(clock.frame(start).is_none(), "the first frame has no dt");

        let deadline = clock.deadline().expect("refused frames set a deadline");
        assert!(deadline > start);
        assert!(deadline <= start + Duration::from_millis(10));

        // Early: still refused.
        assert!(clock.frame(start + Duration::from_millis(4)).is_none());
        // Late enough: allowed.
        let frame_time = clock
            .frame(start + Duration::from_millis(11))
            .expect("11 ms is more than 10");
        assert!((frame_time - 0.011).abs() < 1e-4, "{frame_time}");
        assert_eq!(clock.deadline(), None);
    }

    /// A limiter must postpone time, never discard it: if refused frames threw
    /// their `dt` away, a 300-fps cap on a 3000-fps machine would run the game
    /// at a tenth speed.
    #[test]
    fn refused_frames_accumulate_their_time_rather_than_losing_it() {
        let mut clock = FrameClock::new(100.0);
        let start = Instant::now();
        clock.frame(start);

        for ms in 1..=9 {
            assert!(clock.frame(start + Duration::from_millis(ms)).is_none());
        }
        assert!(clock.filtered_time() > 0.0, "swallowed time is recorded");

        let frame_time = clock
            .frame(start + Duration::from_millis(10))
            .expect("the tenth millisecond completes the frame");
        assert!(
            (frame_time - 0.010).abs() < 1e-4,
            "the whole 10 ms should arrive at once, got {frame_time}"
        );
        assert_eq!(clock.filtered_time(), 0.0, "and the swallowed time resets");
    }

    /// `MAX_FRAMETIME`. A long stall must not be handed to the simulation as
    /// one enormous step.
    #[test]
    fn a_long_stall_is_clamped() {
        let mut clock = FrameClock::new(0.0);
        let start = Instant::now();
        clock.frame(start);
        let frame_time = clock
            .frame(start + Duration::from_secs(5))
            .expect("unlimited");
        assert_eq!(frame_time, MAX_FRAMETIME);
    }

    #[test]
    fn fps_max_is_clamped_to_the_engines_ceiling() {
        // `fps = MIN( MAX_FPS, fps )` (`sys_engine.cpp:392`), so asking for a
        // million frames a second gives a 1 ms minimum, not a zero one.
        let mut clock = FrameClock::new(1_000_000.0);
        let start = Instant::now();
        clock.frame(start);
        assert!(clock.frame(start + Duration::from_micros(500)).is_none());
        assert!(clock.frame(start + Duration::from_millis(2)).is_some());
    }

    #[test]
    fn the_host_reports_the_frame_it_ran() {
        let mut level = Log::default();
        let mut host = Host::new(0.0);
        let start = Instant::now();
        host.frame(start, &mut level);
        host.frame(start + Duration::from_millis(16), &mut level);
        assert_eq!(host.frame_count(), 2);
        assert!((host.frame_time() - 0.016).abs() < 1e-4);
    }

    #[test]
    fn an_early_frame_is_not_counted_as_a_frame() {
        let mut level = Log::default();
        let mut host = Host::new(60.0);
        let start = Instant::now();
        assert!(host.frame(start, &mut level).is_none());
        assert_eq!(host.frame_count(), 0);
        assert!(host.clock().deadline().is_some());
    }
}
