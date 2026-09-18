//! The engine.
//!
//! `portdocs/ENGINE.md` breaks the original `engine/` module into 23
//! subsystems and concludes it must not be ported as one unit: each subsystem
//! becomes its own module here, 13 of them surviving, with ~45,700 lines
//! deleted outright. Five exist so far — [`window`], [`host`], [`world`],
//! [`input`] and [`console`] — and this file is what §1 calls `mod.rs`: the thing that owns
//! them and hands out `&mut` where one needs another, in place of the ambient
//! `g_p*` globals the C++ used to find everything.
//!
//! # Where the app-system tower went
//!
//! `CEngineAPI::RunListenServer` built a *third* `CAppSystemGroup` nested
//! inside the two the launcher already had, purely so each layer could
//! `dlopen` the next (`portdocs/ENGINE.md` §3). All three are deleted. What
//! survives is the ordering they encoded, and it is now just the order of the
//! statements in [`Engine::new`].
//!
//! # The frame
//!
//! ```text
//! window: WindowEvent     -> egui first refusal  -> Consumer::{Ui, Game}
//!                         -> Engine::push_input  -> queued, not acted on
//! window: about_to_wait   -> Engine::deadline    -> ControlFlow::WaitUntil
//! window: RedrawRequested -> Engine::frame       -> host clock + state machine
//!                                                -> Console::run, then fps_max
//!                                                -> Input::frame, then the view
//!                         -> Renderer::begin_frame
//!                         -> Engine::render      -> one pass, the world in it
//!                         -> Engine::run_ui      -> the console, over the top
//!                         -> Frame::present
//! ```
//!
//! Two orderings in there are not stylistic, and `rustdocs/MATERIALS.md` states
//! why: [`RenderContext::begin_frame`] runs before anything allocates, and
//! every pass ends before the frame is presented. A third is
//! `portdocs/ENGINE_INPUT.md` §6.4's: input is drained **inside**
//! [`Engine::frame`], after the host has agreed a frame is happening, so that
//! events pile up rather than being sampled by a frame that never runs.
//! [`Console::run`] is drained in the same place and for the same reason —
//! one run is one tick, which is what makes `wait 1` mean "next frame".

pub mod console;
mod exposure;
pub mod host;
pub mod input;
pub mod trace;
pub mod window;
pub mod world;

use std::sync::Arc;
use std::time::Instant;

use crate::client::player::{VEC_HULL_MAX, VEC_HULL_MIN};
use crate::client::{tonemap, Client, BUTTONS};
use crate::cmdline::CommandLine;
use crate::filesystem::{PathId, Vfs};
use crate::materials::context::{Camera, Load};
use crate::materials::pipeline::TargetFormat;
use crate::materials::renderer::Frame;
use crate::materials::{
    Material, MaterialCache, MaterialPreview, PostProcess, RenderContext, CLEAR_COLOR,
};
use crate::server::think::ServerClock;
use crate::server::{self, Server};

use self::trace::{disp_surf, Contents, Ray, CARVE};
use console::{
    Command, CommandSpec, CommandTarget, ConfigFiles, Console, ConsoleUi, Cvar, CvarFlags,
    CvarRegistry, Dispatch, ExecContext, Source,
};
use host::{Host, Level, Outcome};
use input::Bindings;
use input::{Button, CommandSink, Consumer, Input, Key, MouseButton};
use world::World;

/// The engine.
///
/// The lifetime is the mounted game content's: the [`Vfs`] is built by the
/// launcher and outlives this. It is an `Option` because a failed mount is
/// survivable — see [`window::run`].
pub struct Engine<'a> {
    /// Cvars, commands, and the buffer that turns typed or scripted text into
    /// them. Drained once per frame by [`Engine::frame`].
    console: Console<'a>,
    /// The engine's own handle to `fps_max`, per `ENGINE_CONSOLE.md` §6.1: a
    /// subsystem holds the one cvar it reads rather than a way to look one up.
    fps_max: Cvar,
    /// `r_novis` — draw everything, PVS or no PVS.
    r_novis: Cvar,
    /// `r_portal_stencil_depth` — how many views within views a portal shows.
    r_portal_stencil_depth: Cvar,
    /// `r_lockpvs` — stop recomputing the visible set so the view can be flown
    /// around it.
    ///
    /// Valve freezes the whole answer by returning early from `Map_VisMark`,
    /// which leaves the *frustum* still tracking the camera because
    /// `R_SetupAreaBits` runs separately. Freezing the eye instead gives the
    /// same picture from the same code path, and it is the eye that the whole
    /// answer is a function of.
    r_lockpvs: Cvar,
    /// Where the eye was when `r_lockpvs` was turned on.
    locked_eye: Option<glam::Vec3>,
    /// What [`Cvar::changed`] was last told. `fps_max` had an
    /// `FnChangeCallback_t` in the original (`engine/sys_engine.cpp:78`); this
    /// counter is what replaces it.
    fps_max_generation: u32,
    /// Whether the startup config exec has been through the buffer yet.
    /// `Host_Init` runs `Cbuf_Execute` and only then calls
    /// `Host_SetConfigCfgExecuted` (`engine/host.cpp:2092`); this is that
    /// ordering, spread over the first frame instead of a blocking drain.
    booted: bool,
    /// `saveconfig` (`engine/host.cpp:2069`): startup found no `config.cfg` and
    /// fell back to the defaults, so one should be written out.
    save_config: bool,
    host: Host,
    /// Everything the host drives when it changes level. Separate from [`Host`]
    /// so that `host.frame(&mut self.scene)` is a split borrow of two fields
    /// rather than `&mut self` twice.
    scene: Scene<'a>,
    /// What the platform reported. Filled by [`window`] between ticks and
    /// drained by [`Engine::frame`]; see [`input`].
    input: Input,
    /// The developer console dialog: scrollback, entry line, history and
    /// completion.
    ///
    /// State, not output — the scrollback itself belongs to
    /// [`Console`]'s log, because output exists whether or not anything is
    /// displaying it. Kept here rather than inside [`Console`] so that
    /// `console/`'s machinery stays usable with no `egui` pass at all, and so
    /// that the borrow in [`Engine::run_ui`] is two disjoint fields.
    console_ui: ConsoleUi,
}

/// What a loaded level consists of, and what loading one needs.
///
/// This is the [`Level`] implementation the host calls through. It holds the
/// material system rather than the engine holding it directly, because loading
/// a map is the only thing that puts anything into it.
struct Scene<'a> {
    vfs: Option<&'a Vfs>,
    /// A cheap refcounted handle, not the device itself — see
    /// `rustdocs/MATERIALS.md` on `Renderer::device`.
    device: wgpu::Device,
    materials: MaterialCache,
    context: RenderContext,
    /// Where the scene is drawn, how bright it came out, and what puts it on
    /// the screen (`src/materials/post.rs`).
    ///
    /// In [`Scene`] rather than beside the renderer for the same reason the
    /// material cache is: it is sized to the window and reallocated from
    /// inside [`Engine::render`], which is the one place that has both a
    /// [`Frame`] and the exposure the client chose.
    post: PostProcess,
    world: Option<World>,
    /// `-vmt <name>`: one material on two cubes, drawn *instead of* the world.
    /// See [`Engine::render`].
    preview: Option<(MaterialPreview, Arc<Material>)>,
    /// The game client: the local player, its buttons, and the command that
    /// moves it (`src/client/`, `rustdocs/CLIENT.md`).
    ///
    /// Here rather than beside [`Host`] for the same reason the material cache
    /// is: **loading a map is the only thing that positions a player**, and
    /// [`Level::load`] is handed a `&mut Scene`. It is not level state — the
    /// cvar handles and the button state outlive any map — but its one
    /// level-scoped field is what decides where it has to be reachable from.
    client: Client,
    /// The game server: the map's entity list (`src/server/`,
    /// `portdocs/SERVER.md`).
    ///
    /// In [`Scene`] rather than beside [`Host`] because the entity list is
    /// level state — it is emptied and refilled by every map change — and
    /// [`Level::load`] is the call that has the map. It holds no GPU handle
    /// and names no material type, which is what keeps `server/` testable
    /// without a window.
    server: Server,
    /// Seconds of simulated time **since this level started** —
    /// `gpGlobals->curtime`, accumulated from the host's frame times rather
    /// than read from the clock, so that it advances with the game and not
    /// with the wall.
    ///
    /// > **Level-relative, not since startup**, and reset by
    /// > [`unload`](Level::unload) in step with `Server::level_shutdown`'s
    /// > `ServerClock::reset`. Valve's is `sv.GetTime()`
    /// > (`engine/baseserver.cpp:2836`), which is `m_nTickCount *
    /// > m_flTickInterval` and therefore restarts with the map; anything that
    /// > compares this against a time the *server* stamped — an entity's
    /// > `anim_time`, a portal's `opened_at` — needs both to share an origin.
    ///
    /// It is deliberately **not** derived from the server's clock, which would
    /// make the two agree exactly: `ServerClock::accumulate` drops the surplus
    /// of a clamped frame rather than banking it, so a clock read back out of
    /// it would step backwards after a stall, and this one has to be monotonic
    /// for material animation. The residue is one frame's worth — the load
    /// frame is charged here and not to the server — which is what Valve's
    /// unported `m_flShortFrameTime` exists to remove.
    curtime: f32,
}

impl<'a> Engine<'a> {
    /// Brings the engine up against an already-running renderer.
    ///
    /// The renderer comes first because the window owns it: the surface is tied
    /// to the window handle, and `rustdocs/MATERIALS.md` explains why a `Frame`
    /// borrowing it means `resize` cannot happen through the engine. So the
    /// engine takes device handles and leaves the surface where it is.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: TargetFormat,
        vfs: Option<&'a Vfs>,
        command_line: Option<&CommandLine>,
        test_material: Option<&str>,
    ) -> Engine<'a> {
        let mut console = Console::new(
            Box::new(VfsConfigFiles(vfs)),
            command_line.map(|c| c.args().to_vec()).unwrap_or_default(),
        );

        // `ConVar fps_max( "fps_max", "300", FCVAR_RELEASE, "Frame rate
        // limiter", fps_max_callback )` (`engine/sys_engine.cpp:78`).
        // `FCVAR_RELEASE` is deleted (§4.6, a CS:GO-era allowlist) and the
        // callback becomes [`Engine::fps_max_generation`].
        let fps_max = console.cvar(
            "fps_max",
            &host::DEFAULT_FPS_MAX.to_string(),
            CvarFlags::NONE,
            "Frame rate limiter.",
        );

        // Visibility's two cheats — `mod_vis.cpp:22`. Held here rather than in
        // `world/` because the *view* is what they are about and the view is
        // assembled here: `world/` is handed an eye and a matrix and does not
        // know where they came from.
        let r_novis = console.cvar("r_novis", "0", CvarFlags::CHEAT, "Turn off the PVS.");
        let r_lockpvs = console.cvar(
            "r_lockpvs",
            "0",
            CvarFlags::CHEAT,
            "Lock the PVS so you can fly around and inspect what is being drawn.",
        );
        // `portalrender.cpp:43`, default and flags both. Bounded here where
        // Valve bounds it at the call site: `MIN( r_portal_stencil_depth,
        // MIN( MAX_PORTAL_RECURSIVE_VIEWS, 1 << StencilBufferBits() ) - 1 )`.
        // Eight stencil bits put that second term far above the first, so the
        // limit that bites is `MAX_PORTAL_RECURSIVE_VIEWS` and it is a taste
        // judgement — *"5 is extremely choppy under best conditions and is
        // barely visible"*.
        let r_portal_stencil_depth = console.cvar_bounded(
            "r_portal_stencil_depth",
            &world::portalview::DEFAULT_RECURSION.to_string(),
            CvarFlags::ARCHIVE,
            "When using stencil views, this changes how many views within views we see.",
            Some(0.0),
            Some(f32::from(world::portalview::MAX_RECURSION)),
        );

        // The game client's cvars — `sensitivity`, the mouse factors, the
        // movement speeds — are registered by the client itself, because it is
        // what reads them (`ENGINE_CONSOLE.md` §6.1). This is the line where
        // the port's first *game* module comes up.
        let client = Client::new(&mut console);

        // The engine's commands. Declared here as data and run by
        // [`EngineCommands`]; `ENGINE_CONSOLE.md` §6.3 is why they are not
        // callbacks.
        for spec in [
            CommandSpec::new("map", "Load a map.").with_completion(console::Completion::Files {
                dir: "maps",
                ext: "bsp",
            }),
            CommandSpec::new("quit", "Exit the engine."),
            CommandSpec::new("restart", "Restart the engine."),
            CommandSpec::new("bind", "Bind a key."),
            CommandSpec::new("bind_osx", "Bind a key for OSX only."),
            CommandSpec::new("unbind", "Unbind a key."),
            CommandSpec::new("unbindall", "Unbind all keys."),
            CommandSpec::new("key_listboundkeys", "List bound keys with bindings."),
            CommandSpec::new(
                "key_findbinding",
                "Find key bound to specified command string.",
            ),
            // `engine/console.cpp:1642`. `FCVAR_DONTRECORD` is deferred with
            // `demo/` (`ENGINE_CONSOLE.md` §4.6), so all three are flagless.
            CommandSpec::new("toggleconsole", "Show/hide the console."),
            CommandSpec::new("showconsole", "Show the console."),
            CommandSpec::new("hideconsole", "Hide the console."),
            // The game server's player commands (`game/server/client.cpp`),
            // all `FCVAR_CHEAT` there — a flag `ENGINE_CONSOLE.md` §4.6
            // deletes. `noclip` used to be the client's, because move type had
            // nowhere else to live; `portdocs/SERVER.md` stage 5 gave it one.
            CommandSpec::new("noclip", "Toggle. Player becomes non-solid and flies."),
            CommandSpec::new("god", "Toggle. Player becomes invulnerable."),
            CommandSpec::new("kill", "Kills the player with generic damage."),
            // **This port's own.** Valve's `hurtme` is `#ifdef _DEBUG`; with
            // `trigger_hurt` the only damage source in the shipped maps, the
            // alternative to this is walking into goo to test arithmetic.
            CommandSpec::new("hurtme", "Usage: hurtme [damage] — hurt the player."),
            CommandSpec::new("impulse", "Issue an impulse command."),
            // **This port's, not Valve's.** The C++ has no `trace` command:
            // its equivalents are `debugrayenable` and the trace counter,
            // which exist to work around a DLL boundary this build does not
            // have (`portdocs/ENGINE_TRACE.md` §6). This is stage 1's
            // acceptance test — the only way to ask the collision model a
            // question before `client/` stage 4 can walk on it.
            CommandSpec::new(
                "trace",
                "Trace from the eye along the view. `trace hull` sweeps the player hull.",
            ),
            // Also this port's own, and for the same reason: it is the only
            // way to see what the exposure controller is doing. Valve's
            // equivalent is `mat_show_histogram`, 200 lines of `Viewport` and
            // `ClearBuffers` used as a bar chart (`viewpostprocess.cpp:1115`),
            // which is not worth rebuilding in `egui` to read six numbers.
            CommandSpec::new("tonemap", "Report what the exposure controller is doing."),
            // Also this port's own. Valve's nearest equivalents are
            // `r_ShowViewerArea`, `mat_leafvis` and `r_DrawPortals`, all of
            // which draw rather than print; this prints, because the numbers
            // are what tell you whether the PVS is doing anything.
            CommandSpec::new(
                "vis",
                "Report what the PVS, the areas and the frustum left standing.",
            ),
            // The game server's. `CON_COMMAND(report_entities, ...)`
            // (`game/server/entitylist.cpp:1944`) and
            // `ConCommand ent_dump(...)` (`game/server/baseentity.cpp:6103`),
            // both `FCVAR_CHEAT` there — a flag `ENGINE_CONSOLE.md` §4.6
            // deletes, because cheat protection needs a server telling a
            // client no.
            CommandSpec::new("report_entities", "List the map's entities by class."),
            CommandSpec::new("ent_dump", "Usage: ent_dump <entity name / index / class>"),
            // `ent_fire` is the only way to drive entity I/O by hand, which is
            // what makes a module of invisible bookkeeping inspectable at all.
            // `dumpeventqueue` is its companion (`cbase.cpp:1010`).
            CommandSpec::new(
                "ent_fire",
                "Usage: ent_fire <target> [input] [value] [delay]",
            ),
            CommandSpec::new("dumpeventqueue", "List the pending entity I/O events."),
            // **This port's own**, and `portdocs/PORTAL.md` §10 asks for it by
            // name: "21 scripted portals is not enough to develop against".
            // It is `CWeaponPortalgun::FirePortal` minus the gun and minus the
            // placement rules — see [`Server::place_portal`].
            CommandSpec::new(
                "portal",
                "Usage: portal <1|2|off> — place a portal where you are looking.",
            ),
        ] {
            console
                .register_command(spec)
                .expect("the engine's commands are unique");
        }

        // The `+command`s a binding sends. Both signs are registered, because
        // dispatch only consults the target for names it has been told about —
        // an unregistered `-forward` would fall through to "unknown" and the
        // player would never stop.
        for spec in BUTTONS {
            for name in [spec.down, spec.up] {
                console
                    .register_command(CommandSpec::new(name, "Button."))
                    .expect("the client's buttons are unique");
            }
        }

        let mut materials = MaterialCache::new(device, queue);
        let context = RenderContext::new(device, queue, materials.pipelines());

        let preview = test_material.map(|name| {
            let material = match vfs {
                Some(vfs) => materials.load(vfs, name),
                None => {
                    eprintln!("source-engine: materials: -vmt {name}: no game content is mounted");
                    materials.error_material()
                }
            };
            eprintln!(
                "source-engine: materials: -vmt {} -> {} ({}), flags {}",
                name,
                material.shader.name(),
                material.name,
                material.flags
            );
            (MaterialPreview::new(device), material)
        });

        Engine {
            fps_max_generation: fps_max.generation(),
            booted: false,
            save_config: false,
            host: Host::new(fps_max.float()),
            console,
            fps_max,
            r_novis,
            r_lockpvs,
            r_portal_stencil_depth,
            locked_eye: None,
            scene: Scene {
                vfs,
                device: device.clone(),
                materials,
                context,
                post: PostProcess::new(device, queue, target, &tonemap::bucket_bounds()),
                world: None,
                preview,
                client,
                // `CServerGameDLL::GetTickInterval` (`gameinterface.cpp:1015`).
                // The rate is a constant with one definition site
                // (`portdocs/SERVER.md` §5) and this is the only thing that
                // overrides it.
                server: Server::with_tick_interval(ServerClock::interval_from_tickrate(
                    command_line
                        .and_then(|line| line.value("-tickrate"))
                        .and_then(|rate| rate.parse().ok()),
                )),
                curtime: 0.0,
            },
            input: Input::new(),
            console_ui: ConsoleUi::new(),
        }
    }

    /// Queues the startup command line. `Host_Init`'s last act.
    ///
    /// Everything about how the engine starts is in `cfg/valve.rc`, which execs
    /// `joystick.cfg` and `autoexec.cfg` (Portal 2 ships neither, and both fail
    /// silently by design), runs `stuffcmds` — which is where `+map` takes
    /// effect — and then `startupmenu`, which is GameUI's and is not ported.
    ///
    /// **This replaces the launcher's `+map` block.** The map now loads the
    /// same way it does in the shipped game, through the config files, rather
    /// than from a command-line argument read directly.
    pub fn boot(&mut self) {
        // `Host_Init` (`engine/host.cpp:2055`) prefers a user's `config.cfg`
        // and falls back to `config_default.cfg`, *before* `valve.rc`. Valve
        // checks `//usrlocal/` before `//mod/`; `usrlocal` is a console-era
        // per-user path this port has no equivalent for, so the mod directory
        // is the only candidate.
        //
        // This is where WASD comes from: whichever of the two is read opens
        // with `unbindall` and then binds `+forward` and friends.
        match self.console.config_exists("cfg/config.cfg", Some("mod")) {
            true => self.console.enqueue("exec config.cfg mod", Source::Code),
            false => {
                self.console
                    .enqueue("exec config_default.cfg", Source::Code);
                // `saveconfig` (`:2069`): started from the shipped defaults, so
                // write the user a real config once startup is safely past.
                self.save_config = true;
            }
        }
        self.console.enqueue("exec valve.rc", Source::Code);
    }

    /// `Host_WriteConfiguration` (`engine/host.cpp:1559`), minus Steam Cloud,
    /// splitscreen and the map-editor case.
    ///
    /// The composition is Valve's and spans two modules on purpose: the
    /// bindings are `input/`'s and the archived cvars are `console/`'s, and
    /// this is the engine-level policy that joins them — which is exactly where
    /// `host.cpp` put it.
    ///
    /// **Both guards are load-bearing and neither is an optimization.**
    fn write_configuration(&mut self, file: &str) {
        // `Host_WasConfigCfgExecuted` (`:1587`). Writing before startup has
        // read a config overwrites a real user's settings with defaults — which
        // is what a crash during startup would otherwise cost them. Silent,
        // because it is the normal state for most of a launch.
        if !self.console.config_was_read() {
            return;
        }

        // `Key_CountBindings() <= 1` (`:1603`). A session that somehow bound
        // nothing must not be allowed to persist that over a real config.
        if self.input.bindings().count() <= 1 {
            eprintln!("source-engine: console: skipping {file} output, no keys bound");
            return;
        }

        let contents = build_configuration(self.input.bindings(), self.console.cvars());
        match self
            .console
            .write_config_file(&format!("cfg/{file}"), &contents)
        {
            Ok(()) => eprintln!("source-engine: console: wrote cfg/{file}"),
            Err(err) => eprintln!("source-engine: console: could not write cfg/{file}: {err}"),
        }
    }

    /// Queues a map. See [`Host::request_new_game`].
    #[allow(dead_code)] // reached through the `map` command; kept for tests
    pub fn request_new_game(&mut self, map: &str) {
        self.host.request_new_game(map);
    }

    /// The console, for the things that will drive it from outside the frame:
    /// `input/` stage 3 enqueues a binding's command text, and the `egui`
    /// console reads the log ring and the completion data.
    #[allow(dead_code)] // consumers arrive with `ENGINE_CONSOLE.md` stages 2 and 4
    pub fn console(&self) -> &Console<'a> {
        &self.console
    }

    #[allow(dead_code)] // as above
    pub fn console_mut(&mut self) -> &mut Console<'a> {
        &mut self.console
    }

    /// Asks the engine to shut down, unloading the level on the way out.
    ///
    /// This is what a window close becomes. It is deliberately *not* an
    /// immediate exit: the state machine still runs `GameShutdown`, so
    /// whatever teardown a loaded level needs happens on the way out rather
    /// than being skipped because the user clicked the close box.
    pub fn request_shutdown(&mut self) {
        self.host.request_shutdown();
    }

    #[allow(dead_code)] // the frame counter and host state, once there is a HUD
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Queues one input event, with the answer the UI gave for it.
    /// `CInputSystem::PostEvent`.
    ///
    /// Called from [`window`] as events arrive, which is **between** ticks:
    /// nothing here acts on it, and [`Engine::frame`] dispatches the queue
    /// once the host has agreed a frame is happening.
    ///
    /// `consumer` is `CGame::DispatchInputEvent`'s five-target precedence
    /// chain (`sys_mainwind.cpp:399`) collapsed to `egui`'s one answer. The
    /// key-up latch that makes it safe is [`Input::frame`]'s.
    pub fn push_input(&mut self, event: input::Event, consumer: Consumer) {
        self.input.push_from(event, consumer);
    }

    /// Whether the game wants the mouse.
    ///
    /// [`window`] turns this into a cursor grab, and holds the grab only while
    /// the window also has focus. Splitting it this way is what keeps
    /// "the game wants the mouse" (which survives an alt-tab) apart from
    /// "the cursor is held right now" (which must not).
    ///
    /// **The console takes the cursor back while it is up**, which is what
    /// Source does and is the only way to click in the dialog. It is a
    /// separate term from [`Input::mouse_look`] rather than a write to it, so
    /// that closing the console restores whatever the game had rather than
    /// deciding for it.
    pub fn wants_mouse_capture(&self) -> bool {
        self.input.mouse_look() && !self.console_ui.is_open()
    }

    /// Whether the UI is claiming input.
    ///
    /// `window/` folds this into `egui`'s own "did I consume this" answer,
    /// because that answer is per-widget: with the console up but the entry
    /// unfocused, `egui` would say no and `w` would walk the camera. A dialog
    /// that is up owns the keyboard, which is what VGui's modal input context
    /// meant.
    pub fn ui_has_focus(&self) -> bool {
        self.console_ui.is_open()
    }

    /// Whether this button must reach the game whatever the UI wants.
    ///
    /// `Key_Event` bypasses the whole VGui chain for a `KEY_BACKQUOTE` press
    /// (`engine/keys.cpp:1319`) so that the console key can always close the
    /// console it opened, and so that it is never typed into the entry.
    /// Generalised here from "the backquote" to "whatever is bound to
    /// `toggleconsole`", which is the same rule without the hard-coded key.
    pub fn ui_bypasses(&self, button: Button) -> bool {
        self.input.bindings().bypasses_ui(button)
    }

    /// Builds this frame's UI. `CEngineVGui::Paint`'s place in the frame.
    ///
    /// Called by [`window`] between [`Engine::render`] and the present, with
    /// the `egui` pass already open. The borrow is the same split
    /// [`Engine::frame`] uses: the dialog and the console it drives are two
    /// fields, not `&mut self` twice.
    pub fn run_ui(&mut self, ctx: &egui::Context) {
        let Engine {
            console,
            console_ui,
            ..
        } = self;
        console_ui.draw(ctx, console);
    }

    /// When the next frame may run, if the last one was refused.
    pub fn deadline(&self) -> Option<Instant> {
        self.host.clock().deadline()
    }

    /// Runs one engine frame, if one is due.
    ///
    /// `None` means the frame was early and [`deadline`](Engine::deadline) says
    /// when to come back — the caller must **not** render, and must **not**
    /// busy-wait. `Some(outcome)` means a frame ran and the caller should
    /// render it unless the outcome says to stop.
    ///
    /// This runs before the swap-chain image is acquired, on purpose: a frame
    /// the host refuses should not cost a surface acquisition, and a frame that
    /// loads a map should not hold one across the load.
    pub fn frame(&mut self, now: Instant) -> Option<Outcome> {
        let outcome = self.host.frame(now, &mut self.scene)?;
        let seconds = self.host.frame_time();
        self.scene.curtime += seconds;

        // `DispatchAllStoredGameMessages`' place in `MainLoop`
        // (`sys_mainwind.cpp:509`), and the accumulator reset with it. This
        // must precede the two steps below: bindings read *this* tick's
        // events, and the console executes what they produce.
        let (dx, dy) = self.input.frame();

        // `Key_Event`'s dispatch half (`engine/keys.cpp:1130`): a bound press
        // becomes `+forward <index>` in the command buffer.
        let Engine { console, input, .. } = self;
        input.dispatch_bindings(console);

        // `Cbuf_Execute`. **Inside the frame**, so that one run is one tick and
        // `wait 1` means "next frame" — running it per window event instead
        // would tick the command buffer at the display's rate. It runs *after*
        // the bindings, so a key pressed this tick moves the view this tick
        // rather than the next one.
        //
        // The borrow is `ENGINE_CONSOLE.md` §6.6: `self.console.run(&mut self)`
        // cannot compile, so a struct of disjoint field borrows is the target.
        // It is the same move `host.frame(&mut self.scene)` above already
        // makes.
        let Engine {
            console,
            host,
            input,
            console_ui,
            scene,
            ..
        } = self;
        console.run(&mut EngineCommands {
            host,
            input,
            ui: console_ui,
            world: scene.world.as_ref(),
            server: &mut scene.server,
            client: &mut scene.client,
        });

        // What `fps_max_callback` did. A poll rather than a callback, because a
        // callback would have to own `&mut Host` — §6.2.
        if self.fps_max.changed(&mut self.fps_max_generation) {
            self.host.clock_mut().set_fps_max(self.fps_max.float());
        }

        if !self.booted {
            self.booted = true;

            // `engine/host.cpp:2085`: if nothing bound the console key, bind
            // it. Same family as `unbindall` sparing it — there has to be a way
            // to reach the console.
            let backquote = Button::Key(Key::Backquote);
            if self.input.bindings().get(backquote).is_none() {
                self.input.bindings_mut().bind(backquote, "toggleconsole");
            }

            // Only now, after the startup execs have actually been through the
            // buffer, is writing a config safe.
            self.console.set_config_was_read(true);
            if std::mem::take(&mut self.save_config) {
                self.write_configuration("config.cfg");
            }
        }

        // A clean exit persists settings, which is the other half of stage 3:
        // `HostState_Shutdown` calls `Host_WriteConfiguration` on the way out.
        if matches!(outcome, Outcome::Quit | Outcome::Restart) {
            self.write_configuration("config.cfg");
        }

        // Configs name cvars from subsystems that do not exist yet, so an
        // unrecognised name from a file is counted rather than printed
        // (§9 open question 6). One line beats sixty, and zero hides typos.
        let unknown = self.console.take_unknown_count();
        if unknown > 0 {
            eprintln!(
                "source-engine: console: {unknown} command(s) in the startup configs \
                 are not implemented yet"
            );
        }

        // `CServerGameDLL::GameFrame` (`gameinterface.cpp:1383`), which
        // `_Host_RunFrame` runs once per *server tick* rather than once per
        // rendered frame — so this call runs zero or more of them, whatever
        // the elapsed time bought. `portdocs/SERVER.md` §5 is why the server
        // is quantised and the client is not.
        //
        // **Before `update_client`**, because Valve's order is
        // `SV_Frame` then `CL_Move`: an entity that moved this tick has moved
        // before the player is asked where it is standing.
        //
        // The player goes in first and comes back out afterwards. Both halves
        // are unconditional and the round trip is an identity for anything the
        // server did not touch — `crate::server::PlayerState` says why that is
        // the shape rather than a one-way push with a change flag.
        let Scene {
            server,
            client,
            world,
            ..
        } = &mut self.scene;
        server.set_player_state(player_state(client));
        match world.as_ref() {
            // The engine's half of the touch test borrows the world for the
            // whole call, because `frame` may run several ticks and each of
            // them asks. The fields are named separately so that the borrow of
            // `scene.world` and the one of `scene.server` stay disjoint —
            // `portdocs/CLIENT.md` §6.4's rule, again.
            Some(world) => server.frame(seconds, &mut WorldTouchQuery { world }),
            None => server.frame(seconds, &mut crate::server::NoTouchQuery),
        };
        if let Some(state) = server.player_state() {
            apply_player_state(client, state);
        }

        // `engine->ServerCommand( "reload\n" )` — the game asking for the
        // level to start again, which is what a dead player gets three seconds
        // after dying and what `player_loadsaved` does. **Queued rather than
        // done**, exactly like the `map` command: the host state machine loads
        // it on the next frame and goes through `GameShutdown` on the way, so
        // a respawn takes the same path as a fresh `map`.
        if let Some(map) = self.scene.server.take_level_restart() {
            self.host.request_new_game(&map);
        }

        // `R_DrawBrushModel`'s placement, refreshed from the entity that owns
        // it — **after the ticks and before anything reads it**, so the player
        // is traced against the doors where they are now and the renderer
        // draws the same thing later in this frame. The two modules are joined
        // here because neither names the other.
        if let Some(world) = self.scene.world.as_mut() {
            sync_brush_models(world, &self.scene.server);
            // …and the same join for the models entities place: where they are
            // and which sequence they are playing. Cheap — the whole game has
            // 65 of them and no map more than four — so it is unconditional
            // rather than dirty-flagged.
            world.sync_entity_models(&model_entities(&self.scene.server));
            // …and the third of the three, which is the cheapest: a portal
            // owns no uploaded geometry, so this replaces a list of at most
            // four rows.
            world.sync_portals(&portals(&self.scene.server, self.scene.curtime));
            // …and the fourth, which changes nothing most ticks: which
            // areaportals the map's `func_areaportal`s have opened.
            // `CM_SetAreaPortalStates` (`cmodel.cpp:3517`) — one call for all
            // of them, so the area graph is re-flooded once rather than per
            // entity.
            world
                .vis
                .set_area_portals(&self.scene.server.area_portals());
        }

        // `CL_Move` (`engine/cl_main.cpp:2734`), which is
        // `_Host_RunFrame_Input`'s third step — after the client processed
        // input and after `Cbuf_Execute`, so a key pressed this tick moves the
        // player this tick. It is *after* the host, so a frame that loaded a
        // level moves the player the level put there, not the previous one.
        self.update_client(seconds, dx, dy);

        // Reclaims the previous frame's uniform and geometry arenas. Must
        // happen before anything allocates out of them and after the previous
        // frame is done being recorded — `rustdocs/MATERIALS.md` gotcha #5.
        self.scene.context.begin_frame();

        Some(outcome)
    }

    /// Builds this tick's command and runs it. `CL_Move`
    /// (`engine/cl_main.cpp:2734`).
    ///
    /// Two calls rather than one, because in a game with a server the command
    /// goes over the wire between them — see
    /// [`Client::run_move`](crate::client::Client::run_move).
    ///
    /// Two orderings matter. The mouse is applied under the capture state the
    /// motion was *accumulated* under, before this tick's events can change it;
    /// and the player moves after the console has run, so a tap of a movement
    /// key on the same tick as a click is not lost.
    fn update_client(&mut self, seconds: f32, dx: f32, dy: f32) {
        // `CInput::ClearStates`' other half (`in_mouse.cpp:828`).
        // [`Input::clear`] released the *keys*; what is held is held by the
        // `+command`, so alt-tabbing with `+forward` down would otherwise leave
        // the player walking into a wall until focus came back.
        let focus_lost = self
            .input
            .events()
            .iter()
            .any(|event| matches!(event, input::Event::FocusLost));
        if focus_lost {
            self.scene.client.clear_buttons();
        }

        // **[`wants_mouse_capture`](Engine::wants_mouse_capture), not
        // `Input::mouse_look`.** They differ by exactly one term — the console
        // being up — and using the wrong one is a bug you see rather than one
        // you read: `DeviceEvent::MouseMotion` arrives from the *device*
        // whether or not the cursor is grabbed, so moving the mouse to click in
        // the console would spin the view underneath it.
        //
        // Discarding this tick's delta rather than suppressing it at `push` is
        // safe because `Input::frame` resets the accumulator every tick, so
        // nothing piles up to arrive in one lump when the console closes.
        let mouse = match self.wants_mouse_capture() {
            true => (dx, dy),
            false => (0.0, 0.0),
        };

        // `IN_SetSampleTime` (`host.cpp:4192`), which the engine calls once per
        // *frame* while the client spends it once per *command*. One frame is
        // one command here, so the two cancel — but the split is Valve's and
        // the ordering is load-bearing: without the refill,
        // `DetermineKeySpeed` returns 0 for ever and keyboard look silently
        // stops working.
        self.scene.client.set_sample_time(seconds);

        let command = self.scene.client.create_move(seconds, mouse);

        // The tracer borrows `scene.world` while `run_move` borrows
        // `scene.client` exclusively. **The fields have to be named
        // separately** — an accessor on `Scene` returning a `Tracer` would
        // borrow all of `Scene` and the next line would not compile. Same
        // shape as the destructuring `Engine::frame` already does for the
        // command target (`portdocs/CLIENT.md` §6.4).
        // **The clip chain is stage 4's**, and the two borrows of `w` are
        // both shared, so they nest: `collision` gives the world's BSP and
        // `clip_models` the brush entities the game has said are solid — which
        // is what makes a shut door a wall and, because a trigger is
        // `FSOLID_NOT_SOLID`, leaves every trigger in the map walk-through.
        // …and the portal's hole. **Substitutive rather than additive** — see
        // [`Tracer::with_hole`] — so the selection matters: attaching a hole
        // that is nowhere near lets the sweep pass through the world.
        //
        // The portal is the one the player's *previous* move ended touching —
        // `m_hPortalEnvironment`, decided by
        // `client::movement::handle_portalling`'s selection, which is the
        // swept hull against every active linked portal's trigger box plus the
        // three filters that say a teleport is plausible. That one-move lag is
        // Valve's: the trace has to agree with the environment the move before
        // it ended in.
        //
        // And the hull they would have on the far side, when the pair would
        // turn their up axis far enough to make them duck on the way through —
        // `trace/` cannot name the duck hull, so the decision is made here,
        // once per move, where both halves are in scope.
        let player = self.scene.client.player();
        let environment = player.portal_environment;
        let world = self.scene.world.as_ref();
        let mut tracer = world.map(|w| {
            let tracer = w.collision.tracer().with_entities(w.clip_models());
            let Some(wall) = environment.and_then(|id| w.portal_holes.get(id)) else {
                return tracer;
            };
            let tracer = tracer.with_hole(wall);
            match wall.link().map(|link| link.to_exit) {
                Some(matrix) if crate::client::movement::transition_crouches(matrix) => tracer
                    .with_exit_hull(
                        crate::client::movement::player_mins(true),
                        crate::client::movement::player_maxs(true),
                    ),
                _ => tracer,
            }
        });
        let portals = world.map(|w| &w.portal_holes);
        self.scene
            .client
            .run_move(&command, seconds, tracer.as_mut(), portals);

        let mouse_look = mouse_look_after(self.input.mouse_look(), self.input.events());
        self.input.set_mouse_look(mouse_look);
    }

    /// Records the frame.
    ///
    /// The world goes into an offscreen target, the exposure controller is
    /// given the measurement of the *previous* frame, and the result is put on
    /// the back buffer. That is `CViewRender::RenderView`
    /// (`viewrender.cpp:2989` onwards) reduced to the three steps this port
    /// has: `UpdateMaterialSystemTonemapScalar`, the scene, and
    /// `DoEnginePostProcessing`.
    ///
    /// A frame with no map loaded still clears, so that the window is a window
    /// rather than whatever was behind it.
    ///
    /// `-vmt` draws its cubes *instead of* the world, and owns the frame when
    /// it is set: it is an inspector for one material, so anything else in the
    /// shot defeats the purpose — including the exposure, which is why it
    /// draws straight to the back buffer and is never measured.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let camera = self.camera(frame.size());
        let size = frame.size();
        let curtime = self.scene.curtime;
        // Read here because `frame` is borrowed by the pass that wants it and
        // `self` by the scene that owns the world.
        let portal_depth = self
            .r_portal_stencil_depth
            .int()
            .clamp(0, i32::from(world::portalview::MAX_RECURSION)) as u8;
        // `gpGlobals->frametime`, which is what the exposure is smoothed
        // against. Read before the split borrow below, since it is the host's.
        let frametime = self.host.frame_time();
        let Scene {
            context,
            materials,
            post,
            world,
            preview,
            client,
            server,
            ..
        } = &mut self.scene;

        // `GetTonemapSettingsFromEnvTonemapController`
        // (`c_env_tonemap_controller.cpp:97`), which a running game calls once
        // per frame before the exposure is computed — and so must this, because
        // a controller's values are set by map I/O and can change on any tick.
        // With no map, or a map that places no controller, this is Valve's
        // no-controller fallback rather than the previous map's values.
        client.tonemap_mut().set_settings(server.tonemap_settings());

        // **Unconditionally, and before anything branches.** This is what
        // drains the readback slots, so skipping it on a frame that draws
        // nothing would leave a measurement recorded and never collected, and
        // eventually no slot free to record into. It is also
        // `DoTonemapping`'s first act.
        if let Some(counts) = post.measurement() {
            client.tonemap_mut().measured(counts.as_slice(), frametime);
        }

        if let Some((preview, material)) = preview {
            context.draw_preview(frame, materials.pipelines(), preview, material, curtime);
            return;
        }

        // Nothing loaded — no map, no `-vmt`. `Frame::clear` rather than an
        // empty pass, which is what `rustdocs/MATERIALS.md` reserves it for:
        // the window should be a window rather than whatever was behind it.
        let Some(world) = world else {
            frame.clear(CLEAR_COLOR);
            return;
        };

        // `UpdateMaterialSystemTonemapScalar` (`viewrender.cpp:2989`), which
        // runs **before** the scene is drawn and not after it: this is the
        // number the whole frame is multiplied by, and a pass that has already
        // opened has its constants written.
        let tonemap = client.tonemap_mut();
        context.set_exposure(tonemap.scale());
        let measure = tonemap.measuring().then(|| tonemap.exposure_region());

        // **Visibility, once, before anything is drawn.** `Map_VisSetup` runs
        // at the top of `CViewRender::RenderView` for the same reason: every
        // pass below draws the same frame from the same place, so they must
        // all be told the same thing about what is in it.
        //
        // `novis` is `r_novis` *or* the camera being outside the world with
        // noclip on — `g_bNoClipEnabled` in `Map_VisMark` (`mod_vis.cpp:287`).
        // Without that second term, flying out of a map through a wall would
        // black the whole thing out, because a leaf out there has no cluster
        // and a cluster of -1 sees nothing.
        match self.r_lockpvs.bool() {
            true => {
                self.locked_eye.get_or_insert(camera.eye);
            }
            false => self.locked_eye = None,
        }
        let eye = self.locked_eye.unwrap_or(camera.eye);
        let outside = world.vis.cluster_at(eye) < 0;
        let novis = self.r_novis.bool()
            || (outside && client.player().move_type == crate::client::player::MoveType::Noclip);
        let visible = world.visible(eye, camera.view_proj(), novis);

        // The block ends both borrows of `post` before `resolve` takes it
        // mutably.
        {
            // A drawing frame clears as part of its first pass instead, rather
            // than paying for two passes over the target.
            let scene = post.scene(frame.size());
            let mut pass = context.target_pass(
                frame,
                materials.pipelines(),
                scene,
                &camera,
                Load::Clear(CLEAR_COLOR),
            );
            world.draw(&mut pass, curtime, &visible);

            // `DrawRecursivePortalViews()`' own place in the frame
            // (`CBaseWorldView::DrawExecute`, `viewrender.cpp:8021`): after
            // the opaque world and its entities, before anything translucent
            // — and **in the same pass**, because a portal view is a stencil
            // state and a second camera rather than a second target. See
            // `portdocs/PORTAL_RENDER.md` §2.3.
            //
            // The pass is handed back with its stencil disabled, its scissor
            // reset and this camera bound again, so nothing below needs to
            // know it ran.
            world.draw_portal_views(
                &mut pass,
                &world::portalview::PortalViewSetup {
                    curtime,
                    max_depth: portal_depth,
                    viewport: size,
                    // The debug switch has to mean the same thing inside a
                    // portal as outside one, or `r_novis` turns every portal
                    // into a window on the whole map.
                    novis,
                },
                &camera,
                &visible,
            );
        }

        // `UpdateRefractTexture` and `DrawTranslucentRenderables`, in that
        // order and in that relationship: a refracting material reads a *copy*
        // of the scene, so the pass that drew the scene has to have ended
        // before the copy is taken, and the copy has to have been taken before
        // the pass that samples it opens. Skipped entirely on a map with
        // nothing that refracts, which is 35 of the game's 106.
        if world.needs_frame_buffer_copy() {
            {
                let scene = post.scene(frame.size());
                context.update_refract_texture(frame, scene);
            }
            let scene = post.scene(frame.size());
            let mut pass = context.target_pass(
                frame,
                materials.pipelines(),
                scene,
                &camera,
                // **`Keep`, not `Clear`.** This pass draws on top of the scene
                // the first one left, against the depth buffer it left.
                Load::Keep,
            );
            world.draw_refracting(&mut pass, curtime, &visible);
        }

        // `DrawTranslucentRenderables`' own place in the frame: last, on top of
        // everything opaque, sorted back to front. Built before the pass is
        // opened so that a map with nothing blended pays neither the pass nor
        // the tile load and store it costs.
        let translucent = world.translucent_list(camera.eye, camera.forward(), &visible);
        if !translucent.is_empty() {
            let scene = post.scene(frame.size());
            let mut pass =
                context.target_pass(frame, materials.pipelines(), scene, &camera, Load::Keep);
            world.draw_translucent(&mut pass, curtime, &translucent, &visible);
        }

        post.resolve(frame, measure);
    }

    /// Where the view is: [`ViewSetup`](crate::client::ViewSetup) turned into
    /// the material system's [`Camera`].
    ///
    /// **The client decides what the view is; this decides how to project it.**
    /// Everything above the conversion — the eye, the angles, the field of view
    /// and both clip planes — is `CViewRender::SetUpView`'s and lives in
    /// [`Client::view`](crate::client::Client::view). What is left here is
    /// `CViewSetup::ComputeViewMatrices` (`public/view_shared.h:186`), and it
    /// stays here because a projection matrix is a `wgpu` convention —
    /// handedness, depth range, which way `y` points — and `client/` has no
    /// business knowing any of it.
    ///
    /// Two things to know if the picture looks wrong rather than broken.
    /// **`fov` is horizontal and already width-ratio scaled**, so it goes
    /// straight to `Camera::perspective`, which does the horizontal-to-vertical
    /// conversion with the same aspect. And the basis comes from
    /// `AngleVectors`, so the direction the player looks and the direction it
    /// moves are the same arithmetic — Source is **Z-up right-handed** and
    /// **pitch is positive downwards**, which is the sign error to watch for if
    /// the view looks at the ceiling when it should look at the floor.
    fn camera(&self, size: (u32, u32)) -> Camera {
        let (width, height) = size;
        let view = self.scene.client.view(width.max(1), height.max(1));
        let (forward, _, up) = view.angles.vectors();

        Camera::perspective(
            view.origin,
            glam::camera::rh::view::look_at_mat4(view.origin, view.origin + forward, up),
            view.fov,
            view.aspect,
            view.z_near,
            view.z_far,
        )
    }
}

/// Whether the mouse should still be driving the view after this tick.
///
/// Escape gives the cursor back; a click takes it again. **Do not drop the
/// Escape half**: with no UI there is otherwise no way to get the cursor out
/// of a grabbed window.
///
/// **Only what the UI did not take reaches here.** `CGame::DispatchInputEvent`'s
/// precedence chain (`sys_mainwind.cpp:399`) is decided in `window/` and
/// applied by [`Input::frame`](input::Input::frame)'s key-up latch, so with the
/// console up neither key is in this list: Escape closes the dialog inside
/// `egui` (which is why it does not also hand the cursor back), and a click is
/// the dialog's. The cursor is given back for the console's benefit by
/// [`Engine::wants_mouse_capture`], which is a separate term rather than a
/// write to `mouse_look` — so closing the console restores whatever the game
/// had.
///
/// Last event wins, so a click and an Escape in the same tick resolve in the
/// order they arrived rather than by precedence.
/// Copies every brush entity's placement out of the server and into the
/// world's `BrushModel`s.
///
/// The join `portdocs/SERVER.md` §7.4 asks for, and it lives here rather than
/// in either module because `world/` names no server type and `server/` names
/// no `world/` type — the same arrangement `console/` and `input/` already
/// have. `Server::brush_entity` is a binary search over one entry per brush
/// entity the port has a class for, so this is a few dozen lookups a frame on
/// a real map: 26 on `sp_a1_intro1`.
fn sync_brush_models(world: &mut World, server: &Server) {
    use crate::server::movement::EF_NODRAW;

    world.sync_brush_models(|index| {
        let entity = server.brush_entity(index)?;
        Some(world::Placement {
            origin: entity.origin,
            angles: entity.angles,
            visible: entity.effects & EF_NODRAW == 0,
            // `IsSolid()` rather than the `FSOLID_NOT_SOLID` bit alone, which
            // is what stage 4 changed: a trigger is `SOLID_BSP` *and* not
            // solid, and a `func_button` with `SF_BUTTON_NOTSOLID` is
            // `SOLID_NONE`. Reading only the bit would have put every trigger
            // in the game into the player's clip chain as a wall.
            solid: entity.is_solid(),
        })
    });
}

/// Every model an entity places, as `world/` wants it.
///
/// The conversion neither module can do for itself: `server/` names no studio
/// type and `world/` names no server type, so `engine/` owns the two-field
/// translation. Same shape as [`sync_brush_models`] above.
fn model_entities(server: &Server) -> Vec<world::entities::ModelEntity> {
    server
        .model_entities()
        .into_iter()
        .map(|entity| world::entities::ModelEntity {
            id: entity.id,
            model: entity.model,
            origin: entity.origin,
            angles: entity.angles,
            skin: entity.skin,
            visible: entity.visible,
            sequence: entity.sequence,
            cycle: entity.cycle,
            anim_time: entity.anim_time,
            playback_rate: entity.playback_rate,
            modulation: entity.modulation,
        })
        .collect()
}

/// Every active portal, as `world/` wants it.
///
/// The same two-field translation [`model_entities`] is, plus the one
/// subtraction neither module can make for itself: the server says *when* a
/// portal opened, on its own tick clock, and the renderer wants *how long ago*,
/// against the scene's.
///
/// **The two clocks are not the same** — `rustdocs/SERVER.md` gotcha 1 — and
/// the difference is up to one tick, 15.6 ms. It cannot matter here: both
/// curves the difference feeds are clamped into `0..1` over half a second and a
/// second, so a tick of error moves the opening animation by three per cent of
/// one frame of it. The subtraction is still clamped at zero, because a level
/// that has just restarted can hand over a portal opened in the future.
fn portals(server: &Server, curtime: f32) -> Vec<world::portals::Portal> {
    server
        .portals()
        .into_iter()
        .map(|portal| world::portals::Portal {
            id: portal.id,
            origin: portal.origin,
            angles: portal.angles,
            half_width: portal.half_width,
            half_height: portal.half_height,
            is_portal2: portal.is_portal2,
            open_for: (curtime - portal.opened_at).max(0.0),
            linked: portal.linked,
            matrix: portal.matrix,
        })
        .collect()
}

/// [`SequenceRow`](world::entities::SequenceRow)s into one entry per model.
///
/// The flat iterator is what `world/` can produce without allocating a map of
/// its own; the grouping is what `server/`'s table wants. One place rather
/// than either side, because it is neither module's business — and so is the
/// row-to-[`SequenceInfo`](crate::server::sequences::SequenceInfo)
/// translation, which is the only line in the port that names both types.
fn group_sequences<'a>(
    rows: impl Iterator<Item = world::entities::SequenceRow<'a>>,
) -> Vec<(
    String,
    Vec<(String, crate::server::sequences::SequenceInfo)>,
)> {
    let mut out: Vec<(
        String,
        Vec<(String, crate::server::sequences::SequenceInfo)>,
    )> = Vec::new();
    for row in rows {
        let info = crate::server::sequences::SequenceInfo {
            duration: row.duration,
            loops: row.loops,
            fade_out_time: row.fade_out_time,
        };
        match out.iter_mut().find(|(name, _)| name == row.model) {
            Some((_, labels)) => labels.push((row.label.to_owned(), info)),
            None => out.push((row.model.to_owned(), vec![(row.label.to_owned(), info)])),
        }
    }
    out
}

/// `client::Player` as the server's copy of it. See
/// [`PlayerState`](crate::server::PlayerState) for why the copy exists.
fn player_state(client: &Client) -> server::PlayerState {
    let player = client.player();
    server::PlayerState {
        origin: player.origin,
        // The **view** angles, which for the player entity are its angles —
        // `PlayerState`'s docs say why there is one field and not two. Roll is
        // zero because `ViewAngles` has no roll: the port has no view punch
        // and no vehicles.
        angles: glam::Vec3::new(player.angles.pitch, player.angles.yaw, 0.0),
        velocity: player.velocity,
        base_velocity: player.base_velocity,
        on_ground: player.ground.is_some(),
        mins: crate::client::movement::player_mins(player.ducked),
        maxs: crate::client::movement::player_maxs(player.ducked),
        // **The four fields the server owns are filled in anyway, and
        // `Server::set_player_state` ignores all four.** That is deliberate:
        // the struct is one vocabulary rather than two, and a round trip that
        // *reads* as an identity is easier to reason about than one with holes
        // in it. `PlayerState`'s docs say which four and why. Three of them
        // are the client's own mirror of what the server last said; the
        // fourth, `life_state`, is the only one the client does not carry at
        // all, so it is the default and means nothing on the way in.
        move_type: match player.move_type {
            crate::client::MoveType::Walk => server::movement::MoveType::Walk,
            crate::client::MoveType::Noclip => server::movement::MoveType::Noclip,
            crate::client::MoveType::FlyGravity => server::movement::MoveType::FlyGravity,
        },
        health: player.health,
        life_state: crate::server::damage::LifeState::Alive,
        flags: match player.frozen {
            true => server::movement::FL_FROZEN,
            false => 0,
        },
        // The one field that is purely the client's.
        buttons: client.buttons_bits(),
    }
}

/// …and back, once the server's ticks have had their say.
///
/// **Every field round-trips unchanged unless the server moved it**, which is
/// what makes an unconditional copy-back safe: the state went in at the top of
/// the same `Engine::frame`, nothing but a `trigger_push` or a teleport writes
/// it, and the player has not moved in between.
fn apply_player_state(client: &mut Client, state: server::PlayerState) {
    let player = client.player_mut();
    player.origin = state.origin;
    player.velocity = state.velocity;
    player.base_velocity = state.base_velocity;
    player.angles.pitch = state.angles.x;
    player.angles.yaw = state.angles.y;
    // `SetGroundEntity( NULL )` is the only direction the server writes this:
    // it can take the player off the floor (a push, a teleport) and never puts
    // it back, because the ground *plane* is `client/`'s to find.
    if !state.on_ground {
        player.ground = None;
    }
    // The four the server owns, coming back. **This is the half of the seam
    // that stage 5 added**: before it, every field went out and came back
    // unchanged, and `noclip` lived on the client because nothing could tell
    // it otherwise.
    player.move_type = match state.move_type {
        server::movement::MoveType::Noclip => crate::client::MoveType::Noclip,
        server::movement::MoveType::FlyGravity => crate::client::MoveType::FlyGravity,
        // `MOVETYPE_NONE` and `MOVETYPE_PUSH` are an entity's and no player is
        // ever in one; walking is what a player that is not flying does.
        _ => crate::client::MoveType::Walk,
    };
    player.health = state.health;
    player.frozen = state.flags & server::movement::FL_FROZEN != 0;
}

/// The engine's half of the server's touch test — `engine->SolidMoved`.
///
/// A struct rather than a closure because it holds the world across a whole
/// `Server::frame`, which may run several ticks; see
/// [`TouchQuery`](crate::server::TouchQuery).
struct WorldTouchQuery<'a> {
    world: &'a World,
}

impl server::TouchQuery for WorldTouchQuery<'_> {
    fn brush_models_touching(
        &mut self,
        start: glam::Vec3,
        end: glam::Vec3,
        mins: glam::Vec3,
        maxs: glam::Vec3,
        out: &mut Vec<usize>,
    ) {
        self.world
            .brush_models_touching(start, end, mins, maxs, out);
    }
}

fn mouse_look_after(current: bool, events: &[input::Event]) -> bool {
    events.iter().fold(current, |look, event| match event {
        input::Event::Pressed {
            button: Button::Key(Key::Escape),
            ..
        } => false,
        input::Event::Pressed {
            button: Button::Mouse(MouseButton::Left),
            ..
        } => true,
        _ => look,
    })
}

impl Level for Scene<'_> {
    /// `Host_NewGame` (`engine/host_cmd.cpp`) reduced to the one step that
    /// currently has meaning: read the `.bsp` and upload its geometry.
    ///
    /// Not here, and each one is a subsystem rather than a line: spawning the
    /// server, running the entity list, precaching, `mod_vis`, and the client
    /// connecting to the listen server.
    fn load(&mut self, map: &str) -> Result<(), String> {
        let vfs = self
            .vfs
            .ok_or_else(|| "no game content is mounted".to_string())?;

        let started = Instant::now();
        let world = World::load(vfs, &mut self.materials, &self.device, map)
            .map_err(|err| err.to_string())?;

        // `info_player_start` is where the *engine* puts the player, and is as
        // close as anything gets to a spawn until entities exist. A map
        // without one is not an error: the middle of the map is a better place
        // to look from than the origin, which is usually outside the level.
        match world.spawn {
            Some(spawn) => self.client.spawn(spawn.origin, spawn.pitch, spawn.yaw),
            // The centre of the map is where a `Player` is *stood*, so the eye
            // ends up `VEC_VIEW` above it. Sixty-four units up from the middle
            // of a room is a better guess than the middle of the room.
            None => self.client.spawn(world.center(), 0.0, 0.0),
        }

        // `CServerGameDLL::LevelInit` plus `ServerActivate`
        // (`gameinterface.cpp:1167` and `:1305`), which in the original run
        // either side of the engine's own level load and here run after it —
        // the entity lump is the engine's to read and the game's to
        // interpret. A map whose entities fail to spawn is not a failed load:
        // there is nothing in stage 1 that can fail, and a level shell with no
        // entity list is exactly what the port had before this module.
        let entities = self.server.level_init(map, &world.entities, &world.models);

        // Valve bracketed the load with `COM_TimestampedLog`; the interesting
        // number now is how much of the map actually draws, which is what
        // `summary` reports.
        eprintln!(
            "source-engine: world: loaded {} in {:.2}s",
            world.summary(),
            started.elapsed().as_secs_f32()
        );
        let (mins, maxs) = world.bounds;
        eprintln!(
            "source-engine: world: bounds ({:.0} {:.0} {:.0}) .. ({:.0} {:.0} {:.0})",
            mins.x, mins.y, mins.z, maxs.x, maxs.y, maxs.z
        );
        match world.spawn {
            Some(spawn) => eprintln!(
                "source-engine: world: player at ({:.0} {:.0} {:.0}) pitch {:.0} yaw {:.0}",
                spawn.origin.x, spawn.origin.y, spawn.origin.z, spawn.pitch, spawn.yaw
            ),
            None => eprintln!(
                "source-engine: world: no info_player_start; \
                 the view starts at the centre of the map"
            ),
        }
        if let Some(sky) = &world.sky_name {
            eprintln!("source-engine: world: skybox {sky} (not drawn yet)");
        }
        eprintln!("source-engine: server: {}", entities.summary());

        // `ClientPutInServer` — the player joins the entity list, so that
        // `!player` resolves and a trigger has something to notice. It happens
        // *after* `level_init`, because the map's own entities have to exist
        // before the client connects to them, which is Valve's order too.
        self.server.spawn_player(player_state(&self.client));

        // A `Spawn` may already have moved something: 40 of the game's doors
        // carry `spawnpos 1` and stand open from the first frame, and 337
        // `func_brush`es are `StartDisabled`. So the placements are taken from
        // the entities once here as well as once per frame.
        let mut world = world;
        sync_brush_models(&mut world, &self.server);

        // The models the game's entities place, read and uploaded now that
        // there *are* entities. This cannot happen inside `World::load` — the
        // entity list is built from the lump that load just read — and it is
        // the one place the two halves of a level are both in hand.
        let placements = model_entities(&self.server);
        world.load_entity_models(vfs, &mut self.materials, &self.device, &placements);
        if !placements.is_empty() {
            eprintln!("source-engine: world: {}", world.entity_models.summary());
        }

        // …and the answer back the other way. `server/` names no studio type,
        // so what a `.mdl` says about its sequences is copied into a plain
        // table here — the one moment in a level's life when the entities and
        // their models are both in hand. Without it `CDynamicProp::AnimThink`
        // cannot tell when an animation has finished, which is 181
        // `OnAnimationDone` connections in the game and 10 on `sp_a1_intro1`.
        //
        // **After `level_init`, and that is forced**: the models are named by
        // the entities. `crate::server::sequences` has what every `Spawn` in
        // the game therefore has to cope with.
        let mut sequences = crate::server::sequences::SequenceTable::new();
        for (model, labels) in group_sequences(world.entity_models.sequences()) {
            sequences.insert_model(&model, labels);
        }
        if !sequences.is_empty() {
            eprintln!(
                "source-engine: server: {} model(s) with animation the map can ask for",
                sequences.len()
            );
        }
        self.server.set_sequences(sequences);

        self.world = Some(world);
        Ok(())
    }

    /// `Host_ShutdownServer` plus `modelloader->UnloadUnreferencedModels`.
    ///
    /// Dropping the [`World`] frees its GPU buffers, which is the whole of it —
    /// the hunk allocator that made this a subsystem in the original is exactly
    /// what `PORTING.md` says to delete rather than port.
    fn unload(&mut self) {
        // Before the world, because the entity list is built from its lump and
        // `LevelShutdownPreEntity` runs before the engine frees the model in
        // the original too.
        self.server.level_shutdown();

        // **And the scene clock with it**, because `level_shutdown` resets the
        // server's. The two are one quantity — `gpGlobals->curtime` — kept in
        // two places so that animation can be smooth where the tick is
        // stepped, and a reset of one without the other silently breaks every
        // *non-looping* animation in the level: a cycle derived from a
        // `curtime` in one frame of reference and an `anim_time` in the other
        // is offset by however long the previous level ran, and
        // `clamp( 0, 1 )` pins it at the far end on the first frame. A looping
        // sequence is immune, because `rem_euclid` turns a constant offset
        // into a phase shift — which is exactly why a spinning fan kept
        // working while every door and button in the game snapped between two
        // poses. See `EntityModels::cycle`.
        self.curtime = 0.0;
        if let Some(world) = self.world.take() {
            eprintln!("source-engine: world: unloaded {}", world.name);
        }
        // Materials outlive the level deliberately: `UncacheUnusedMaterials`
        // was called only under memory pressure on consoles, and a map change
        // between two Portal 2 chambers shares most of its content.
    }
}

/// What the console hands a command back to.
///
/// A struct of field borrows rather than `Engine` itself, because `Console` is
/// one of `Engine`'s fields — `ENGINE_CONSOLE.md` §6.6. That is not only a
/// borrow-checker workaround: it makes the set of state a command may touch
/// explicit, where the C++ answer was "all of it".
///
/// It grows a field per subsystem that gains commands: `map`, `quit` and
/// `restart` are [`Host`] requests, `bind` and friends are [`Input`]'s, the
/// `+`/`-` pairs and `noclip` are the [`Client`]'s.
struct EngineCommands<'e> {
    host: &'e mut Host,
    input: &'e mut Input,
    /// The game client. A field of a field of [`Engine`] — `scene.client` —
    /// which borrows disjointly from `console` and `host` just as the rest do.
    client: &'e mut Client,
    /// `toggleconsole`/`showconsole`/`hideconsole`. The dialog is the engine's
    /// state, not the console's — see [`Engine::console_ui`].
    ui: &'e mut ConsoleUi,
    /// The loaded map, for `trace`. A shared borrow of `scene.world`
    /// alongside the exclusive one of `scene.client`, which is disjoint
    /// because they are separate fields — the same move the destructuring at
    /// the call site already makes.
    world: Option<&'e World>,
    /// The entity list, for `report_entities` and `ent_dump`. Shared, like
    /// [`world`](EngineCommands::world): neither command changes anything.
    server: &'e mut Server,
}

/// `input/` defines [`CommandSink`] and `console/` provides the buffer, and
/// **neither may name the other** — that is what keeps both testable alone. So
/// the one line joining them lives here, in the module that already owns both.
impl CommandSink for Console<'_> {
    fn enqueue(&mut self, command: &str) {
        // `kCommandSrcUserInput`: this came from a key the user pressed, which
        // is the distinction `ENGINE_CONSOLE.md` §4.7 exists to preserve.
        Console::enqueue(self, command, Source::UserInput);
    }
}

/// The `tonemap` command: what the exposure controller is doing, in text.
///
/// `CTonemapSystem::DisplayHistogram` (`viewpostprocess.cpp:1115`) without the
/// bar chart. The three lines it prints are the three its `Con_NPrintf` calls
/// printed, plus the buckets themselves — which Valve only ever drew.
fn tonemap_command(client: &Client, cx: &mut ExecContext<'_>) {
    let tonemap = client.tonemap();
    let (min, max) = tonemap.exposure_range();
    cx.print(&format!(
        "exposure {:.3} -> {:.3}, allowed {min:.2}..{max:.2}{}",
        tonemap.current(),
        tonemap.target(),
        match tonemap.measuring() {
            true => "",
            false => " (mat_dynamic_tonemapping 0)",
        }
    ));

    // What the map's `env_tonemap_controller` is asking for, and which of the
    // limits above came from it rather than from a cvar. 105 of the game's 106
    // maps place one, so "none" here means either no map is loaded or
    // `logic_auto`'s 0.2-second bootstrap has not run yet.
    let settings = tonemap.settings();
    let source = |custom: bool| match custom {
        true => "map",
        false => "cvar",
    };
    cx.print(&format!(
        "map: min from {}, max from {}, rate {:.2}, target {:.0}% of the top {:.0}%",
        source(settings.use_custom_auto_exposure_min),
        source(settings.use_custom_auto_exposure_max),
        settings.rate,
        settings.percent_target,
        settings.percent_bright_pixels,
    ));

    let Some((actual, wanted)) = tonemap.bright_end() else {
        cx.print("no frame has been measured yet");
        return;
    };
    cx.print(&format!(
        "bright end at {:.1}% of range, wants {:.1}%; median {:.1}%",
        actual * 100.0,
        wanted * 100.0,
        tonemap.median_luminance().unwrap_or(0.0) * 100.0,
    ));

    // The buckets, darkest first, as a share of the pixels measured. A width
    // rather than a count, because what matters is the shape.
    let buckets = tonemap.histogram();
    let total: u32 = buckets.iter().sum();
    let bounds = tonemap::bucket_bounds();
    for (i, &count) in buckets.iter().enumerate() {
        let share = match total {
            0 => 0.0,
            total => count as f32 / total as f32,
        };
        cx.print(&format!(
            "{:5.3}..{:5.3} {:6.2}% {:7} {}",
            bounds[i],
            bounds[i + 1],
            share * 100.0,
            count,
            "#".repeat((share * 50.0).round() as usize),
        ));
    }
}

/// `MAX_TRACE_LENGTH` (`public/worldsize.h:32`) — `sqrt(3) * COORD_EXTENT`,
/// the diagonal of the largest legal map, and so the longest a trace can
/// usefully be.
const MAX_TRACE_LENGTH: f32 = 1.732_050_8 * 2.0 * 16384.0;

/// The `portal` command: put a portal on the surface the player is looking at.
///
/// **This port's own**, and `portdocs/PORTAL.md` §10 asks for it by name:
/// twenty-one scripted portals across ten maps is not enough to develop
/// against, and nineteen of them are at axis-aligned yaws.
///
/// It is `CWeaponPortalgun::TraceFirePortal` (`weapon_portalgun_shared.cpp:1213`)
/// with the gun, the multi-segment trace, the fizzle taxonomy and every
/// placement rule taken out — `portdocs/PORTAL.md` §8 is why. What is left is
/// exactly the three lines that decide *where*: trace, take the surface
/// normal as the new forward, and hand the pair to
/// [`Server::place_portal`](crate::server::Server::place_portal), which is
/// `FindPortal` plus `NewLocation`.
///
/// ```text
///   portal 1 | blue      place the blue portal
///   portal 2 | orange    place the orange portal
///   portal off           fizzle every portal in the map
/// ```
///
/// Placing both colours links them, because `NewLocation` activates the portal
/// it moves and an activating portal looks for a partner.
///
/// **Nothing stops the two ending up in the same place**, which is the most
/// visible consequence of deleting the rules: type `portal 1` and `portal 2`
/// without moving and you get two coincident ovals linked to each other, whose
/// transform is the half turn about their own shared up axis.
/// `VerifyPortalPlacementAndFizzleBlockingPortals` is what refuses that in the
/// shipped game, and it is the gun's.
///
/// # The one rule from the gun that *is* here
///
/// **A portal on the floor or the ceiling is rolled to face the player.**
/// `TraceFirePortal` builds a pseudo-up of world `+Z` and then, when the
/// surface normal is vertical to within a thousandth, replaces it with the
/// direction the shot travelled — "If we're upright, then the top of the
/// portal should be away from us" (`:1348`). Without it every floor portal in
/// the game comes out at yaw 0 regardless of where you stood, which is both
/// wrong and confusing to debug a teleport against.
///
/// The `m_StickNormal` branch beside it is for a player standing on a
/// paint-gel wall, and there is no paint.
fn portal_command(
    world: Option<&World>,
    client: &Client,
    server: &mut Server,
    cmd: &Command,
    cx: &mut ExecContext<'_>,
) {
    let usage = "portal <1|2|off> : place a portal where you are looking";
    let Some(argument) = cmd.arg(1) else {
        cx.print(usage);
        return;
    };
    let argument = argument.trim();

    if argument.eq_ignore_ascii_case("off") {
        let fizzled = server.fizzle_portals();
        cx.print(&format!("portal: fizzled {fizzled} portal(s)"));
        return;
    }

    let is_portal2 = match argument {
        "1" => false,
        "2" => true,
        a if a.eq_ignore_ascii_case("blue") => false,
        a if a.eq_ignore_ascii_case("orange") || a.eq_ignore_ascii_case("red") => true,
        _ => {
            cx.print(usage);
            return;
        }
    };

    let Some(world) = world else {
        cx.print("portal: no map is loaded");
        return;
    };
    if world.collision.is_empty() {
        cx.print(&format!("portal: {} has no collision tree", world.name));
        return;
    }

    let player = client.player();
    let (forward, _, _) = player.angles.vectors();
    let eye = player.eye();
    // World and brush models only, and deliberately: a portal on a door would
    // need `SetMobileState` and the parent tracking under it, which
    // `sv_allow_mobile_portals` turns off outside one map anyway.
    let ray = Ray::line(eye, eye + forward * MAX_TRACE_LENGTH);
    let hit = world
        .collision
        .tracer()
        .trace(&ray, Contents::MASK_SHOT_PORTAL);
    if !hit.did_hit() {
        cx.print("portal: nothing in front of you");
        return;
    }

    // `Vector vUp( 0.0f, 0.0f, 1.0f )`, replaced by the shot direction on a
    // floor or a ceiling — see the doc comment.
    let vertical = hit.normal.x.abs() < 0.001 && hit.normal.y.abs() < 0.001;
    let up = match vertical {
        true => forward,
        false => glam::Vec3::Z,
    };
    let angles = crate::math::vector_angles(hit.normal, up);

    if !server.place_portal(is_portal2, hit.end, angles) {
        cx.print("portal: no game is running");
        return;
    }
    let colour = if is_portal2 { "orange" } else { "blue" };
    let linked = server
        .portals()
        .iter()
        .filter(|portal| portal.linked.is_some())
        .count();
    cx.print(&format!(
        "portal: {colour} at ({:.1} {:.1} {:.1}) angles ({:.1} {:.1} {:.1}); \
         {} active, {linked} linked",
        hit.end.x,
        hit.end.y,
        hit.end.z,
        angles.x,
        angles.y,
        angles.z,
        server.portals().len(),
    ));
}

/// The `trace` command: fire a ray from the player's eye, or sweep the player
/// hull from their feet, and report what the collision model says.
///
/// This port's own, and the acceptance test for `portdocs/ENGINE_TRACE.md`
/// stage 1 — it asks the one question the module exists to answer, using only
/// what already existed (a console, a player, a view). `client/` stage 4 is
/// what turns the answer into movement.
/// `vis`: what the three filters left standing, from where the player is.
///
/// Recomputed here rather than kept from the last frame, because the console
/// is drained before the frame is drawn and holding one would report the
/// previous view. The numbers are the same either way — the answer is a
/// function of the eye and the matrix.
fn vis_command(world: Option<&World>, client: &Client, cx: &mut ExecContext<'_>) {
    let Some(world) = world else {
        cx.print("vis: no map is loaded");
        return;
    };
    if world.vis.is_empty() {
        cx.print(&format!("vis: {} has no visibility data", world.name));
        return;
    }

    let view = client.view(1, 1);
    let (forward, _, up) = view.angles.vectors();
    let camera = Camera::perspective(
        view.origin,
        glam::camera::rh::view::look_at_mat4(view.origin, view.origin + forward, up),
        view.fov,
        view.aspect,
        view.z_near,
        view.z_far,
    );
    let set = world.visible(camera.eye, camera.view_proj(), false);
    let s = set.stats;
    let percent = |part: usize, whole: usize| match whole {
        0 => 0.0,
        _ => 100.0 * part as f32 / whole as f32,
    };

    cx.print(&format!("vis: {}", world.vis.summary()));
    cx.print(&format!(
        "  eye at ({:.0} {:.0} {:.0}) in leaf {}, cluster {}, area {}",
        camera.eye.x,
        camera.eye.y,
        camera.eye.z,
        world.vis.leaf_at(camera.eye),
        s.cluster,
        world.vis.area_at(camera.eye),
    ));
    cx.print(&format!(
        "  sees {} of {} clusters ({:.1}%), {} leaves, {} areas, {} nodes walked",
        s.clusters,
        world.vis.cluster_count(),
        percent(s.clusters, world.vis.cluster_count()),
        s.leaves,
        s.areas,
        s.nodes,
    ));
    cx.print(&format!(
        "  {} of {} world faces ({:.1}%)",
        s.faces,
        world.stats.faces_total,
        percent(s.faces, world.stats.faces_total),
    ));

    let closed: Vec<u16> = (0..world.vis.area_portal_count() as u16)
        .filter(|&key| !world.vis.area_portal_is_open(key))
        .collect();
    cx.print(&match closed.is_empty() {
        true => "  every areaportal is open".to_owned(),
        false => format!("  areaportals closed: {closed:?}"),
    });
}

fn trace_command(world: Option<&World>, client: &Client, cmd: &Command, cx: &mut ExecContext<'_>) {
    let Some(world) = world else {
        cx.print("trace: no map is loaded");
        return;
    };
    let collision = &world.collision;
    if collision.is_empty() {
        cx.print(&format!("trace: {} has no collision tree", world.name));
        return;
    }

    let hull = matches!(cmd.arg(1), Some(arg) if arg.trim().eq_ignore_ascii_case("hull"));
    let player = client.player();
    let (forward, _, _) = player.angles.vectors();

    // The hull sweeps from the feet, because that is what `origin` is and what
    // stage 4 will sweep; the ray goes from the eye, because that is where a
    // player is pointing from.
    let (from, ray) = match hull {
        true => (
            player.origin,
            Ray::hull(
                player.origin,
                player.origin + forward * MAX_TRACE_LENGTH,
                VEC_HULL_MIN,
                VEC_HULL_MAX,
            ),
        ),
        false => {
            let eye = player.eye();
            (eye, Ray::line(eye, eye + forward * MAX_TRACE_LENGTH))
        }
    };

    let hit = collision.tracer().trace(&ray, Contents::MASK_PLAYERSOLID);
    let v = |v: glam::Vec3| format!("({:.1} {:.1} {:.1})", v.x, v.y, v.z);

    cx.print(&format!(
        "trace: {} from {} along {} (mask {})",
        if hull { "hull" } else { "ray" },
        v(from),
        v(forward),
        Contents::MASK_PLAYERSOLID,
    ));
    if !hit.did_hit() {
        cx.print(&format!("  nothing hit; end {}", v(hit.end)));
    } else {
        cx.print(&format!(
            "  fraction {:.6}  distance {:.2}  end {}",
            hit.fraction,
            (hit.end - from).length(),
            v(hit.end),
        ));
        cx.print(&format!(
            "  normal {}  plane dist {:.2}",
            v(hit.normal),
            hit.plane_dist
        ));
        cx.print(&format!(
            "  surface \"{}\"  surface flags {:#x}  contents {}",
            collision.surface_name(hit.surface),
            hit.surface_flags,
            hit.contents,
        ));
        // Terrain, when it is terrain. `DISPSURF_FLAG_SURFACE` is ORed onto
        // every displacement triangle, so a non-zero value here is the whole
        // of `CGameTrace::IsDispSurface`.
        if hit.disp_flags & disp_surf::SURFACE != 0 {
            cx.print(&format!(
                "  displacement: flags {:#x} — {}",
                hit.disp_flags,
                match hit.disp_flags & disp_surf::WALKABLE != 0 {
                    true => "walkable",
                    false => "not walkable",
                },
            ));
        }
    }
    if hit.start_solid || hit.all_solid {
        cx.print(&format!(
            "  startsolid {}  allsolid {}  fractionleftsolid {:.6}  start {}",
            hit.start_solid,
            hit.all_solid,
            hit.fraction_left_solid,
            v(hit.start),
        ));
    }
    cx.print(&format!(
        "  at the eye: contents {}, leaf {}",
        collision.point_contents(player.eye()),
        collision.leaf(player.eye()),
    ));

    // The ground probe, which is the question stage 4 asks more than any
    // other. `CategorizePosition` (`gamemovement.cpp:1714`) sweeps the hull
    // exactly two units down and calls what it finds the ground; this reports
    // a longer sweep and the two-unit verdict separately, because "no ground"
    // and "ground, 8 units down" are the same answer to Valve's question and
    // very different answers to "is this module working".
    const GROUND_PROBE: f32 = 128.0;
    let ground = collision.tracer().trace(
        &Ray::hull(
            player.origin,
            player.origin - glam::Vec3::Z * GROUND_PROBE,
            VEC_HULL_MIN,
            VEC_HULL_MAX,
        ),
        Contents::MASK_PLAYERSOLID,
    );
    match ground.did_hit() {
        true => {
            let drop = player.origin.z - ground.end.z;
            cx.print(&format!(
                "  ground: \"{}\" {:.2} below the feet, normal {} — {}, {}",
                collision.surface_name(ground.surface),
                drop,
                v(ground.normal),
                // 0.7 is Valve's, and it is a cosine: anything steeper than
                // ~45.6 degrees is a wall you slide down, not a floor.
                if ground.normal.z > 0.7 {
                    "standable"
                } else {
                    "too steep to stand on"
                },
                if drop <= 2.0 {
                    "on the ground"
                } else {
                    "in the air (CategorizePosition only looks 2 units down)"
                },
            ));
            // The two questions are not the same one: `normal.z > 0.7` is
            // asked at runtime about the triangle actually hit, and
            // `DISPSURF_FLAG_WALKABLE` is VBSP's compile-time verdict.
            if ground.disp_flags & disp_surf::SURFACE != 0 {
                cx.print(&format!(
                    "    a displacement; vbsp compiled it as {}",
                    match ground.disp_flags & disp_surf::WALKABLE != 0 {
                        true => "walkable",
                        false => "not walkable",
                    },
                ));
            }
        }
        false => cx.print(&format!(
            "  ground: nothing within {GROUND_PROBE} units below the feet"
        )),
    }

    trace_portal_hole(world, client, &ray, cx);
    trace_brush_models(world, &ray, from, cx);
}

/// The same ray again, against the **carved** geometry of whichever portal the
/// player is standing in — `portdocs/PORTAL.md` stage 3.
///
/// Silent on a map with no portal, which is 96 of 106. When there is one it is
/// the only way to see the carve from inside the running game: a hole is
/// invisible, so "did the wall get cut" is otherwise a question you can only
/// answer by walking into it.
///
/// The second trace is the carved pieces **alone** — `UTIL_Portal_TraceRay`
/// (`portal_util_shared.cpp:638`), which never touches the real world. What
/// [`Tracer::with_hole`] does with the two answers is the line above it.
fn trace_portal_hole(world: &World, client: &Client, ray: &Ray, cx: &mut ExecContext<'_>) {
    if world.portal_holes.is_empty() {
        return;
    }
    let v = |v: glam::Vec3| format!("({:.1} {:.1} {:.1})", v.x, v.y, v.z);

    let player = client.player();
    // **`m_hPortalEnvironment` first, `touches` second.** The environment is
    // what the player's own trace uses, and it is what a developer needs told;
    // the geometric test is the fallback so that the line says something
    // useful on the tick before the environment catches up.
    let wall = player
        .portal_environment
        .and_then(|id| world.portal_holes.get(id))
        .or_else(|| {
            world
                .portal_holes
                .touching(player.origin + VEC_HULL_MIN, player.origin + VEC_HULL_MAX)
        });
    let Some(wall) = wall else {
        let carved = world
            .portal_holes
            .iter()
            .map(|wall| format!("{} at {}", wall.id(), v(wall.hole().center)))
            .collect::<Vec<_>>()
            .join(", ");
        cx.print(&format!(
            "  portal holes: {carved} — the player is in none of them"
        ));
        return;
    };

    cx.print(&format!(
        "  portal hole {} at {} facing {} (carve mask {CARVE}): {}",
        wall.id(),
        v(wall.hole().center),
        v(wall.hole().forward),
        wall.summary(),
    ));
    match player.portal_environment == Some(wall.id()) {
        true => cx.print("    this is the player's portal environment"),
        false => cx.print("    the player's hull is in it, but their environment is not set"),
    }
    let hit = wall
        .collision()
        .tracer()
        .trace(ray, Contents::MASK_PLAYERSOLID);
    match hit.did_hit() {
        false => cx.print("    the carved geometry stops nothing along this ray"),
        true => cx.print(&format!(
            "    fraction {:.6}  end {}  normal {}  startsolid {}",
            hit.fraction,
            v(hit.end),
            v(hit.normal),
            hit.start_solid,
        )),
    }

    // The far side, which is the whole of stage 4: the same ray as the exit
    // portal sees it, swept against the exit's geometry and this portal's tube.
    let Some(link) = wall.link() else {
        cx.print("    unlinked, so there is no far side to trace");
        return;
    };
    let Some((far_ray, shift)) = wall.remote_ray(ray, None) else {
        return;
    };
    cx.print(&format!(
        "    exit {} at {} facing {}; the ray becomes {} -> {} (shift {})",
        link.exit_id,
        v(link.exit.center),
        v(link.exit.forward),
        v(far_ray.origin()),
        v(far_ray.end()),
        v(shift),
    ));
    let Some(remote) = wall.remote() else { return };
    let hit = remote.tracer().trace(&far_ray, Contents::MASK_PLAYERSOLID);
    match hit.did_hit() {
        false => cx.print("    the far side stops nothing along this ray"),
        true => cx.print(&format!(
            "    far fraction {:.6}  end {}  normal {}  startsolid {}",
            hit.fraction,
            v(hit.end),
            v(hit.normal),
            hit.start_solid,
        )),
    }
}

/// The same ray, against the brush models the map places —
/// `portdocs/ENGINE_TRACE.md` stage 2's acceptance test, extended by stage 4.
///
/// Two lines, and the difference between them is the stage-4 story. The first
/// is the **clip chain**: [`World::clip_models`], the models the game says are
/// solid, which is what the player's own trace is swept against. The second is
/// everything else the ray passes through — the triggers — which is what the
/// player would *walk into* and which is worth seeing precisely because it is
/// not in the first.
///
/// It asks each model at full length rather than shortening the ray to the
/// world hit first, which `CEngineTrace::TraceRay` does
/// (`enginetrace.cpp:2870`): a console command wants the whole list, not the
/// nearest.
fn trace_brush_models(world: &World, ray: &Ray, from: glam::Vec3, cx: &mut ExecContext<'_>) {
    if world.brush_models.is_empty() {
        cx.print("  brush models: the map places none");
        return;
    }

    let mut tracer = world.collision.tracer();
    let hits = |tracer: &mut crate::engine::trace::Tracer<'_>,
                solid: bool|
     -> Option<(usize, String, crate::engine::trace::Trace)> {
        world
            .brush_models
            .iter()
            // What the *game* says. `owned` is whether it said anything at
            // all — see [`PlacedBrushModel::owned`] for why a model nobody has
            // answered for is in neither list.
            .filter(|placed| placed.owned && placed.solid == solid)
            .map(|placed| {
                (
                    placed.index,
                    placed.classname.clone(),
                    tracer.trace_model(ray, &placed.model, Contents::MASK_PLAYERSOLID),
                )
            })
            .filter(|(.., hit)| hit.did_hit())
            // `f32` is not `Ord`, and a NaN fraction would be a bug worth
            // seeing rather than a panic: `total_cmp` orders it last instead.
            .min_by(|(.., a), (.., b)| a.fraction.total_cmp(&b.fraction))
    };

    let v = |v: glam::Vec3| format!("({:.1} {:.1} {:.1})", v.x, v.y, v.z);
    let clip = world.clip_models().len();
    match hits(&mut tracer, true) {
        Some((index, classname, hit)) => cx.print(&format!(
            "  brush models: {} placed, {clip} in the clip chain; nearest solid is \
             *{index} \"{classname}\" at {:.2} units, surface \"{}\", normal {}",
            world.brush_models.len(),
            (hit.end - from).length(),
            world.collision.surface_name(hit.surface),
            v(hit.normal),
        )),
        None => cx.print(&format!(
            "  brush models: {} placed, {clip} in the clip chain, none of them in the way",
            world.brush_models.len()
        )),
    }
    if let Some((index, classname, hit)) = hits(&mut tracer, false) {
        cx.print(&format!(
            "  …and a non-solid one first: *{index} \"{classname}\" at {:.2} units \
             — a trigger, walked through",
            (hit.end - from).length(),
        ));
    }
}

impl CommandTarget for EngineCommands<'_> {
    fn execute(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) -> Dispatch {
        // The `+command`/`-command` pair, which is most of what a binding
        // sends. The argument is the index of the button that sent it; a bare
        // `-forward` typed at the console has none and releases regardless.
        if let Some(name) = cmd.name().strip_prefix(['+', '-']) {
            let down = cmd.name().starts_with('+');
            let index = cmd.arg(1).and_then(|arg| arg.trim().parse().ok());
            return match self.client.buttons_mut().apply(name, down, index) {
                true => Dispatch::Handled,
                false => Dispatch::Unknown,
            };
        }

        match cmd.name().to_ascii_lowercase().as_str() {
            // `CON_COMMAND_F( map, ... )` (`engine/host_cmd.cpp`), reduced to
            // the argument that currently means anything. Queued rather than
            // loaded: the host state machine loads it on the next frame, so
            // startup and a later `map` take exactly the same path — including
            // going *through* `GameShutdown`, which is the invariant
            // `rustdocs/ENGINE.md` records for the host.
            "map" => match cmd.arg(1) {
                Some(name) => self.host.request_new_game(name),
                None => cx.print("map <mapname> : load a map"),
            },
            // `CON_COMMAND_F( quit, "Exit the engine.", FCVAR_NONE )`
            // (`engine/host_cmd.cpp:2750`).
            // `CON_COMMAND_F( noclip, ..., FCVAR_CHEAT )` — **a server command
            // since `portdocs/SERVER.md` stage 5**, which is where the move
            // type went. `god` and `kill` are its neighbours in
            // `game/server/client.cpp` and arrived with it.
            "noclip" => match self.server.toggle_noclip() {
                Some(true) => cx.print("noclip ON"),
                Some(false) => cx.print("noclip OFF"),
                None => cx.print("noclip: no player"),
            },
            "god" => match self.server.toggle_god() {
                Some(true) => cx.print("godmode ON"),
                Some(false) => cx.print("godmode OFF"),
                None => cx.print("god: no player"),
            },
            "kill" => {
                if !self.server.kill_player() {
                    cx.print("kill: already dead, or too soon after the last one");
                }
            }
            "hurtme" => {
                let amount = cmd.arg(1).map_or(10.0, crate::server::keyvalue::atof);
                if !self
                    .server
                    .hurt_player(amount, crate::server::damage::DMG_GENERIC)
                {
                    cx.print("hurtme: no player, or the damage was refused");
                }
            }
            // `IN_Impulse` (`game/client/in_main.cpp:757`). Latched onto the
            // next command and cleared; nothing consumes impulses yet.
            "impulse" => match cmd.arg(1).and_then(|arg| arg.trim().parse().ok()) {
                Some(impulse) => self.client.set_impulse(impulse),
                None => cx.print("impulse <number>"),
            },
            "trace" => trace_command(self.world, self.client, cmd, cx),
            "portal" => portal_command(self.world, self.client, self.server, cmd, cx),
            "report_entities" => self.server.report_entities(cx),
            "ent_dump" => self.server.ent_dump(cmd, cx),
            "ent_fire" => self.server.ent_fire(cmd, cx),
            "dumpeventqueue" => self.server.dump_event_queue(cx),
            "tonemap" => tonemap_command(self.client, cx),
            "vis" => vis_command(self.world, self.client, cx),
            "quit" => self.host.request_shutdown(),
            "restart" => self.host.request_restart(),

            // `Con_ToggleConsole_f` and friends (`engine/console.cpp:257`).
            // `toggleconsole` is what the backquote is bound to, and the one
            // command `Key_Event` lets through the UI chain whatever else is
            // on screen — see [`Engine::ui_bypasses`].
            "toggleconsole" => self.ui.toggle(),
            "showconsole" => self.ui.set_open(true),
            "hideconsole" => self.ui.set_open(false),

            // `BindHelper` (`engine/keys.cpp:280`). `bind_osx` is the same
            // command gated on the platform, and it is not a curiosity:
            // `config_default.cfg` ships `bind_osx "z" "+zoom"`, and macOS is
            // a supported target.
            "bind" => self.bind(cmd, cx),
            "bind_osx" => {
                if cfg!(target_os = "macos") {
                    self.bind(cmd, cx);
                }
            }
            "unbind" => match cmd.arg(1).and_then(Button::from_name) {
                Some(button) => {
                    if !self.input.bindings_mut().unbind(button) {
                        cx.print("Can't unbind ESCAPE key");
                    }
                }
                None => cx.print("unbind <key> : remove commands from a key"),
            },
            "unbindall" => self.input.bindings_mut().unbind_all(),
            "host_writeconfig" => {
                let file = cmd.arg(1).unwrap_or("config.cfg");
                if !cx.config_was_read() {
                    cx.print("skipping config output, startup has not read one yet");
                } else if self.input.bindings().count() <= 1 {
                    cx.print(&format!("skipping {file} output, no keys bound"));
                } else {
                    let contents = build_configuration(self.input.bindings(), cx.cvars());
                    let path = format!("cfg/{file}");
                    match cx.write_config(&path, &contents) {
                        Ok(()) => cx.print(&format!("wrote {path}")),
                        Err(err) => cx.error(&format!("could not write {path}: {err}")),
                    }
                }
            }
            "key_listboundkeys" => {
                let listing: Vec<String> = self
                    .input
                    .bindings()
                    .iter()
                    .map(|(button, command)| format!("\"{}\" = \"{command}\"", button.name()))
                    .collect();
                cx.print(&listing.join("\n"));
            }
            "key_findbinding" => match cmd.arg(1) {
                Some(wanted) => {
                    let listing: Vec<String> = self
                        .input
                        .bindings()
                        .find(wanted)
                        .map(|button| {
                            let command = self.input.bindings().get(button).unwrap_or_default();
                            format!("\"{}\" = \"{command}\"", button.name())
                        })
                        .collect();
                    cx.print(&listing.join("\n"));
                }
                None => cx.print("key_findbinding <command> : find key bound to a command"),
            },

            _ => return Dispatch::Unknown,
        }
        Dispatch::Handled
    }
}

impl EngineCommands<'_> {
    /// `BindHelper` (`engine/keys.cpp:280`).
    ///
    /// One argument prints the current binding; two or more join the rest with
    /// spaces, so `bind F6 save quick` binds `save quick` even though the
    /// tokenizer split it.
    fn bind(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) {
        let Some(name) = cmd.arg(1) else {
            cx.print("bind <key> [command] : attach a command to a key");
            return;
        };
        let Some(button) = Button::from_name(name) else {
            cx.print(&format!("\"{name}\" isn't a valid key"));
            return;
        };

        if cmd.argc() < 3 {
            match self.input.bindings().get(button) {
                Some(command) => cx.print(&format!("\"{name}\" = \"{command}\"")),
                None => cx.print(&format!("\"{name}\" is not bound")),
            }
            return;
        }

        self.input
            .bindings_mut()
            .bind(button, &cmd.args()[1..].join(" "));
    }
}

/// The text of a `config.cfg`.
///
/// `Host_WriteConfiguration`'s body (`engine/host.cpp:1624`): `unbindall`, then
/// every binding, then every archived cvar.
///
/// **`unbindall` first is what makes the file idempotent** — reading it back
/// throws away whatever was bound before rather than merging with it. It is
/// also why `Bindings::unbind_all` has to spare Escape and the backquote: this
/// file is exec'd at startup, and without those exceptions reading your own
/// config would take away the menu key and the console key.
///
/// **The format is not ours to change** even though we write it and read it
/// (`ENGINE_CONSOLE.md` §7): a user's existing `config.cfg` was written by the
/// shipped engine, and one we write has to stay readable by it.
fn build_configuration(bindings: &Bindings, cvars: &CvarRegistry) -> String {
    let mut out = String::from("unbindall\n");
    bindings.write(&mut out);
    console::write_archived_cvars(cvars, &mut out);
    out
}

/// `exec`'s window onto the mounted content.
///
/// The whole of why `console/` names no filesystem type: it declares
/// [`ConfigFiles`] and this implements it. A console built for a test uses an
/// in-memory one instead and needs no mount.
struct VfsConfigFiles<'a>(Option<&'a Vfs>);

impl ConfigFiles for VfsConfigFiles<'_> {
    fn read_config(&self, path: &str, path_id: Option<&str>) -> Option<Vec<u8>> {
        let vfs = self.0?;
        // `exec <file> [path id]`, where Valve spells the path ID as the
        // `//<pathid>/` prefix on the path and `*` means "any mount".
        match path_id.map(str::to_ascii_lowercase).as_deref() {
            None | Some("*") => vfs.read(path).ok(),
            Some("mod") => vfs.scoped(PathId::Mod).read(path).ok(),
            Some("game") => vfs.scoped(PathId::Game).read(path).ok(),
            Some("gamebin") => vfs.scoped(PathId::GameBin).read(path).ok(),
            Some("platform") => vfs.scoped(PathId::Platform).read(path).ok(),
            Some("executable_path") => vfs.scoped(PathId::ExecutablePath).read(path).ok(),
            // An unknown ID searches everything rather than nothing: a config
            // naming a path this port does not have should still be read.
            Some(_) => vfs.read(path).ok(),
        }
    }

    fn config_exists(&self, path: &str, path_id: Option<&str>) -> bool {
        let Some(vfs) = self.0 else {
            return false;
        };
        match path_id.map(str::to_ascii_lowercase).as_deref() {
            Some("mod") => vfs.scoped(PathId::Mod).exists(path),
            Some("game") => vfs.scoped(PathId::Game).exists(path),
            Some("gamebin") => vfs.scoped(PathId::GameBin).exists(path),
            Some("platform") => vfs.scoped(PathId::Platform).exists(path),
            Some("executable_path") => vfs.scoped(PathId::ExecutablePath).exists(path),
            _ => vfs.exists(path),
        }
    }

    /// There is exactly one place a write can go — [`Vfs::write_root`] — where
    /// a read searches every mount in order. That asymmetry is why
    /// `DEFAULT_WRITE_PATH` was not ported as a search path
    /// (`rustdocs/FILESYSTEM.md`), and it is why this does not take a path ID.
    /// `cfg/*.cfg` for `exec`, `maps/*.bsp` for `map` — the completion half of
    /// `CBaseAutoCompleteFileList`.
    ///
    /// Merged across every mount, which is what makes a map inside a VPK
    /// complete the same way one loose on disk does; `Sys_FindFirst` searched
    /// the same search paths for the same reason. Directories are skipped:
    /// `maps/` has subdirectories in a real install and neither command takes
    /// one.
    fn list_files(&self, dir: &str, ext: &str) -> Vec<String> {
        let Some(vfs) = self.0 else {
            return Vec::new();
        };
        let Ok(entries) = vfs.list(dir) else {
            return Vec::new();
        };

        let suffix = format!(".{}", ext.to_ascii_lowercase());
        entries
            .into_iter()
            .filter(|entry| !entry.is_dir)
            .filter_map(|entry| {
                let lowered = entry.name.to_ascii_lowercase();
                match lowered.ends_with(&suffix) {
                    true => Some(entry.name[..entry.name.len() - suffix.len()].to_string()),
                    false => None,
                }
            })
            .collect()
    }

    fn write_config(&self, path: &str, contents: &str) -> Result<(), String> {
        let vfs = self.0.ok_or("no game content is mounted")?;
        let target = vfs.write_path(path).map_err(|err| err.to_string())?;
        if let Some(dir) = target.parent() {
            // `CreateDirHierarchy( "cfg", ... )` (`engine/host.cpp:1618`).
            std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        }
        std::fs::write(&target, contents).map_err(|err| format!("{}: {err}", target.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed(button: Button) -> input::Event {
        input::Event::Pressed {
            button,
            repeat: false,
        }
    }

    /// The one thing joining `client/`'s exposure policy to `materials/`'s
    /// measurement is that they agree on how many buckets there are, and the
    /// two modules deliberately do not name each other. This is where they
    /// meet, so this is where the agreement is checked — [`Engine::new`] would
    /// otherwise panic inside `Histogram::new`, at startup, on a GPU.
    #[test]
    fn the_tone_mapper_s_buckets_fit_the_histogram_shader() {
        assert_eq!(
            tonemap::bucket_bounds().len() - 1,
            tonemap::BUCKETS,
            "one boundary more than there are buckets"
        );
        // A `const` block, so a bucket table that outgrew the shader's
        // workgroup scratch array would not compile rather than panic on a
        // machine with a GPU.
        const { assert!(tonemap::BUCKETS <= crate::materials::histogram::MAX_BUCKETS) };
    }

    #[test]
    fn escape_gives_the_cursor_back_and_a_click_takes_it_again() {
        let escape = pressed(Button::Key(Key::Escape));
        let click = pressed(Button::Mouse(MouseButton::Left));

        assert!(!mouse_look_after(true, &[escape]));
        assert!(mouse_look_after(false, &[click]));
    }

    #[test]
    fn an_unrelated_event_changes_nothing() {
        let events = [
            pressed(Button::Key(Key::W)),
            input::Event::Released(Button::Key(Key::Escape)),
            input::Event::MouseMotion { dx: 4.0, dy: 0.0 },
        ];
        assert!(mouse_look_after(true, &events));
        assert!(!mouse_look_after(false, &events));
    }

    #[test]
    fn the_last_event_of_the_tick_wins() {
        let escape = pressed(Button::Key(Key::Escape));
        let click = pressed(Button::Mouse(MouseButton::Left));
        assert!(mouse_look_after(true, &[escape, click]));
        assert!(!mouse_look_after(false, &[click, escape]));
    }

    /// Stage 2 of `portdocs/ENGINE_CONSOLE.md` end to end, without a GPU: a
    /// `bind` command puts a key in the table, pressing that key produces
    /// command text, the console executes it, and the movement button ends up
    /// held. Every seam in the chain is exercised and none of them is mocked.
    #[test]
    fn a_bound_key_moves_the_camera_through_the_command_buffer() {
        use console::{Console, Source};

        let mut console = Console::detached();
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();
        let mut client = Client::new(&mut console);

        for spec in [
            console::CommandSpec::new("bind", ""),
            console::CommandSpec::new("+forward", ""),
            console::CommandSpec::new("-forward", ""),
        ] {
            console.register_command(spec).expect("unique");
        }

        // `bind w +forward`, as `config_default.cfg` does.
        console.enqueue("bind w +forward", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert_eq!(input.bindings().get(Button::Key(Key::W)), Some("+forward"));

        // Press it. The binding turns the press into command text...
        input.push(input::Event::Pressed {
            button: Button::Key(Key::W),
            repeat: false,
        });
        input.frame();
        input.dispatch_bindings(&mut console);

        // ...and the console executing it holds the movement button down.
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert!(
            client.create_move(1.0 / 60.0, (0.0, 0.0)).forwardmove > 0.0,
            "the command the client builds now asks to move forward"
        );

        // Releasing the key stops it again.
        input.push(input::Event::Released(Button::Key(Key::W)));
        input.frame();
        input.dispatch_bindings(&mut console);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert_eq!(client.create_move(1.0 / 60.0, (0.0, 0.0)).forwardmove, 0.0);
    }

    /// Stage 4 end to end without a GPU: the console key is bound by the
    /// shipped config, pressing it becomes command text, the console executes
    /// it, and the dialog opens. Every seam in the chain is exercised —
    /// bindings, the command buffer, `EngineCommands` — and none is mocked.
    #[test]
    fn the_console_key_opens_the_dialog_through_the_command_buffer() {
        use console::{Console, Source};

        let mut console = Console::detached();
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();
        let mut client = Client::new(&mut console);
        for spec in [
            console::CommandSpec::new("bind", ""),
            console::CommandSpec::new("toggleconsole", ""),
        ] {
            console.register_command(spec).expect("unique");
        }

        // The line `config_default.cfg` ships.
        console.enqueue("bind \"`\" \"toggleconsole\"", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert!(!ui.is_open());

        let backquote = Button::Key(Key::Backquote);
        input.push(pressed(backquote));
        input.frame();
        input.dispatch_bindings(&mut console);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert!(ui.is_open(), "the console key opened the console");

        // And it closes again, which is the half that needs the key to bypass
        // the UI — see `Engine::ui_bypasses`.
        input.push(input::Event::Released(backquote));
        input.push(pressed(backquote));
        input.frame();
        input.dispatch_bindings(&mut console);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert!(!ui.is_open());
    }

    /// The rule `window/` reads to decide that a key is never the UI's.
    #[test]
    fn only_the_key_bound_to_toggleconsole_bypasses_the_ui() {
        let mut bindings = Bindings::new();
        bindings.bind(Button::Key(Key::Backquote), "toggleconsole");
        bindings.bind(Button::Key(Key::W), "+forward");

        assert!(bindings.bypasses_ui(Button::Key(Key::Backquote)));
        assert!(!bindings.bypasses_ui(Button::Key(Key::W)));
        assert!(
            !bindings.bypasses_ui(Button::Key(Key::F1)),
            "unbound keys are the UI's"
        );
    }

    #[test]
    fn unbindall_spares_escape_and_the_console_key() {
        use console::{Console, Source};

        let mut console = Console::detached();
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();
        let mut client = Client::new(&mut console);
        for spec in [
            console::CommandSpec::new("bind", ""),
            console::CommandSpec::new("unbindall", ""),
        ] {
            console.register_command(spec).expect("unique");
        }

        // The opening lines of `config_default.cfg`.
        console.enqueue(
            "bind \"ESCAPE\" \"cancelselect\"; bind \"`\" \"toggleconsole\"; bind \"w\" \"+forward\"",
            Source::Code,
        );
        console.enqueue("unbindall", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });

        assert_eq!(input.bindings().get(Button::Key(Key::W)), None);
        assert_eq!(
            input.bindings().get(Button::Key(Key::Escape)),
            Some("cancelselect")
        );
        assert_eq!(
            input.bindings().get(Button::Key(Key::Backquote)),
            Some("toggleconsole")
        );
    }

    // ---- config persistence (stage 3) --------------------------------------

    /// A `ConfigFiles` that reads and writes an in-memory map, shared with the
    /// test through an `Arc` so both consoles in a round trip see one store.
    #[derive(Default)]
    struct MemoryConfigs {
        files: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl ConfigFiles for std::sync::Arc<MemoryConfigs> {
        fn read_config(&self, path: &str, _path_id: Option<&str>) -> Option<Vec<u8>> {
            self.files
                .lock()
                .expect("not poisoned")
                .get(path)
                .map(|text| text.as_bytes().to_vec())
        }

        fn write_config(&self, path: &str, contents: &str) -> Result<(), String> {
            self.files
                .lock()
                .expect("not poisoned")
                .insert(path.to_string(), contents.to_string());
            Ok(())
        }
    }

    /// A console with the engine's persistence-related commands registered, and
    /// a client to supply the archived cvars that get carried across.
    ///
    /// The cvars are the client's real ones rather than a stand-in, so this
    /// exercises what a session actually persists.
    fn config_console(store: &std::sync::Arc<MemoryConfigs>) -> (Console<'static>, Client, Cvar) {
        let mut console = Console::new(Box::new(store.clone()), Vec::new());
        console.log_mut().set_echo_to_stderr(false);
        for spec in [
            CommandSpec::new("bind", ""),
            CommandSpec::new("unbindall", ""),
            CommandSpec::new("host_writeconfig", ""),
        ] {
            console.register_command(spec).expect("unique");
        }
        let client = Client::new(&mut console);
        let sensitivity = console
            .cvars()
            .find("sensitivity")
            .expect("the client registers it")
            .clone();
        (console, client, sensitivity)
    }

    /// The whole of stage 3: what the writer produces, the reader reproduces.
    /// Both halves are ours, but the format is Valve's — a user's existing
    /// `config.cfg` has to stay readable and one we write has to stay readable
    /// by the shipped engine.
    #[test]
    fn a_written_config_reads_back_as_the_same_bindings_and_cvars() {
        let store = std::sync::Arc::new(MemoryConfigs::default());

        // Session one: bind some keys, change a setting, write it out.
        let (mut console, mut client, sensitivity) = config_console(&store);
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();
        console.set_config_was_read(true);
        console.enqueue(
            "bind \"w\" \"+forward\"; bind \"MOUSE1\" \"+attack\"; bind \"F6\" \"save quick\"",
            Source::Code,
        );
        console.enqueue("host_writeconfig", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        sensitivity.set_string("6");
        console.enqueue("host_writeconfig", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });

        let written = store
            .files
            .lock()
            .expect("not poisoned")
            .get("cfg/config.cfg")
            .cloned()
            .expect("a config was written");
        assert!(
            written.starts_with("unbindall\n"),
            "reading it back must throw away what was bound before: {written}"
        );

        // Session two: a fresh console and a fresh binding table, reading it.
        let (mut console, mut client, sensitivity) = config_console(&store);
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();
        console.enqueue("exec config.cfg", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });

        assert_eq!(input.bindings().get(Button::Key(Key::W)), Some("+forward"));
        assert_eq!(
            input
                .bindings()
                .get(Button::Mouse(input::MouseButton::Left)),
            Some("+attack")
        );
        assert_eq!(
            input.bindings().get(Button::Key(Key::F6)),
            Some("save quick"),
            "a multi-word binding survives the quotes"
        );
        assert_eq!(sensitivity.float(), 6.0, "and the archived cvar came back");
    }

    /// `Host_WasConfigCfgExecuted`. Without this, a crash between startup and
    /// the config exec writes defaults over a real user's settings.
    #[test]
    fn writing_is_refused_until_startup_has_read_a_config() {
        let store = std::sync::Arc::new(MemoryConfigs::default());
        let (mut console, mut client, _) = config_console(&store);
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();

        console.enqueue(
            "bind \"w\" \"+forward\"; bind \"s\" \"+back\"",
            Source::Code,
        );
        console.enqueue("host_writeconfig", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });

        assert!(
            store.files.lock().expect("not poisoned").is_empty(),
            "nothing may be written before startup has read a config"
        );
    }

    /// `Key_CountBindings() <= 1`. A session that bound nothing must not
    /// persist that over a real config.
    #[test]
    fn writing_is_refused_when_almost_nothing_is_bound() {
        let store = std::sync::Arc::new(MemoryConfigs::default());
        let (mut console, mut client, _) = config_console(&store);
        let mut host = Host::new(host::DEFAULT_FPS_MAX);
        let mut input = Input::new();
        let mut ui = ConsoleUi::new();
        console.set_config_was_read(true);

        console.enqueue("bind \"w\" \"+forward\"", Source::Code);
        console.enqueue("host_writeconfig", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
            server: &mut Server::new(),
            client: &mut client,
        });
        assert!(store.files.lock().expect("not poisoned").is_empty());
    }

    #[test]
    fn the_config_opens_with_unbindall_then_bindings_then_cvars() {
        let mut console = Console::detached();
        console.cvar("sensitivity", "2.5", CvarFlags::ARCHIVE, "");
        let mut bindings = Bindings::new();
        bindings.bind(Button::Key(Key::W), "+forward");

        let text = build_configuration(&bindings, console.cvars());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "unbindall",
                "bind \"w\" \"+forward\"",
                "sensitivity \"2.5\""
            ]
        );
    }
    /// **The stage-5 seam, both ways, without a GPU.** `Engine::frame` copies
    /// the player into the entity list before the server's ticks and back out
    /// after them, and stage 5 made four of the fields travel in one direction
    /// only — so the property to pin is not "it round-trips" but "it round-trips
    /// *except* where the server owns it".
    ///
    /// This is the join `rustdocs/SERVER.md` gotcha 57 warns about: a
    /// server-owned field that the client writes back is undone a fraction of a
    /// frame after it is set, which reads as `noclip` not working rather than
    /// as a bug in either module.
    #[test]
    fn the_player_state_seam_carries_the_servers_four_fields_one_way() {
        use crate::client::MoveType;
        use crate::server::movement;

        let mut console = crate::engine::console::Console::detached();
        let mut client = Client::new(&mut console);
        client.spawn(glam::Vec3::new(10.0, 20.0, 30.0), 0.0, 90.0);

        let mut server = server::Server::new();
        server.level_init("test", &[], &[]);
        server.spawn_player(player_state(&client));

        // Everything the client owns arrives.
        let state = server.player_state().expect("a player");
        assert_eq!(state.origin, glam::Vec3::new(10.0, 20.0, 30.0));
        assert_eq!(state.angles.y, 90.0);

        // …and the four the server owns come back with the *server's* values,
        // not the ones that went in. `Player::spawn` set these; the client's
        // copy said nothing about health at all.
        assert_eq!(state.health, 100);
        assert_eq!(state.life_state, crate::server::damage::LifeState::Alive);
        assert_eq!(state.move_type, movement::MoveType::Walk);

        // The command a console would run, and the field it writes.
        assert_eq!(server.toggle_noclip(), Some(true));
        // A whole frame of `Engine::frame`'s copy, in both directions.
        server.set_player_state(player_state(&client));
        apply_player_state(&mut client, server.player_state().expect("a player"));
        assert_eq!(
            client.player().move_type,
            MoveType::Noclip,
            "noclip reached the client"
        );

        // …and again, which is the step that would undo it if
        // `set_player_state` wrote the move type.
        server.set_player_state(player_state(&client));
        apply_player_state(&mut client, server.player_state().expect("a player"));
        assert_eq!(client.player().move_type, MoveType::Noclip, "and stayed");

        // Death travels the same way: the client learns it is dead from the
        // health, which is what `CGameMovement::IsDead` asks about.
        assert!(server.kill_player());
        apply_player_state(&mut client, server.player_state().expect("a player"));
        assert_eq!(client.player().health, 0);
        assert_eq!(client.player().move_type, MoveType::FlyGravity);
    }
}
