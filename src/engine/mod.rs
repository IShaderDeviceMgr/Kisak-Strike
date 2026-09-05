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
pub mod host;
pub mod input;
pub mod trace;
pub mod window;
pub mod world;

use std::sync::Arc;
use std::time::Instant;

use crate::client::player::{VEC_HULL_MAX, VEC_HULL_MIN};
use crate::client::{Client, MoveType, BUTTONS};
use crate::cmdline::CommandLine;
use crate::filesystem::{PathId, Vfs};
use crate::materials::context::{Camera, Load};
use crate::materials::renderer::Frame;
use crate::materials::{Material, MaterialCache, MaterialPreview, RenderContext, CLEAR_COLOR};

use console::{
    Command, CommandSpec, CommandTarget, ConfigFiles, Console, ConsoleUi, Cvar, CvarFlags,
    CvarRegistry, Dispatch, ExecContext, Source,
};
use host::{Host, Level, Outcome};
use input::Bindings;
use input::{Button, CommandSink, Consumer, Input, Key, MouseButton};
use world::World;
use self::trace::{Contents, Ray};

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
    /// Seconds of simulated time since startup — `gpGlobals->curtime`,
    /// accumulated from the host's frame times rather than read from the clock,
    /// so that it advances with the game and not with the wall.
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
            // The game client's. `noclip` is a *server* command in the
            // original (`game/server/`), because move type is server state
            // that gets networked down; with no server it lives on the client
            // and moves when there is one (`portdocs/CLIENT.md` §9.2).
            CommandSpec::new("noclip", "Toggle. Player becomes non-solid and flies."),
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
            scene: Scene {
                vfs,
                device: device.clone(),
                materials,
                context,
                world: None,
                preview,
                client,
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
        self.scene.client.run_move(&command, seconds);

        let mouse_look = mouse_look_after(self.input.mouse_look(), self.input.events());
        self.input.set_mouse_look(mouse_look);
    }

    /// Records the frame.
    ///
    /// One pass, clearing colour and depth, with the world in it. A frame with
    /// no map loaded still clears, so that the window is a window rather than
    /// whatever was behind it.
    ///
    /// `-vmt` draws its cubes *instead of* the world, and owns the frame when
    /// it is set: it is an inspector for one material, so anything else in the
    /// shot defeats the purpose.
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let camera = self.camera(frame.size());
        let curtime = self.scene.curtime;
        let Scene {
            context,
            materials,
            world,
            preview,
            ..
        } = &mut self.scene;

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

        // A drawing frame clears as part of its first pass instead, rather
        // than paying for two passes over the target.
        let mut pass = context.pass(
            frame,
            materials.pipelines(),
            &camera,
            Load::Clear(CLEAR_COLOR),
        );
        world.draw(&mut pass);
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
        self.world = Some(world);
        Ok(())
    }

    /// `Host_ShutdownServer` plus `modelloader->UnloadUnreferencedModels`.
    ///
    /// Dropping the [`World`] frees its GPU buffers, which is the whole of it —
    /// the hunk allocator that made this a subsystem in the original is exactly
    /// what `PORTING.md` says to delete rather than port.
    fn unload(&mut self) {
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

/// `MAX_TRACE_LENGTH` (`public/worldsize.h:32`) — `sqrt(3) * COORD_EXTENT`,
/// the diagonal of the largest legal map, and so the longest a trace can
/// usefully be.
const MAX_TRACE_LENGTH: f32 = 1.732_050_8 * 2.0 * 16384.0;

/// The `trace` command: fire a ray from the player's eye, or sweep the player
/// hull from their feet, and report what the collision model says.
///
/// This port's own, and the acceptance test for `portdocs/ENGINE_TRACE.md`
/// stage 1 — it asks the one question the module exists to answer, using only
/// what already existed (a console, a player, a view). `client/` stage 4 is
/// what turns the answer into movement.
fn trace_command(
    world: Option<&World>,
    client: &Client,
    cmd: &Command,
    cx: &mut ExecContext<'_>,
) {
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
        }
        false => cx.print(&format!(
            "  ground: nothing within {GROUND_PROBE} units below the feet"
        )),
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
            // `CON_COMMAND_F( noclip, ..., FCVAR_CHEAT )`. Walking is stage 4
            // of `portdocs/CLIENT.md`, so turning it off leaves a player who
            // cannot move rather than one who falls: there is no ground.
            "noclip" => match self.client.toggle_noclip() {
                MoveType::Noclip => cx.print("noclip ON"),
                MoveType::Walk => cx.print(
                    "noclip OFF - MOVETYPE_WALK is not implemented \
                     (portdocs/CLIENT.md stage 4); the player will not move",
                ),
            },
            // `IN_Impulse` (`game/client/in_main.cpp:757`). Latched onto the
            // next command and cleared; nothing consumes impulses yet.
            "impulse" => match cmd.arg(1).and_then(|arg| arg.trim().parse().ok()) {
                Some(impulse) => self.client.set_impulse(impulse),
                None => cx.print("impulse <number>"),
            },
            "trace" => trace_command(self.world, self.client, cmd, cx),
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
    fn config_console(
        store: &std::sync::Arc<MemoryConfigs>,
    ) -> (Console<'static>, Client, Cvar) {
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
            client: &mut client,
        });
        sensitivity.set_string("6");
        console.enqueue("host_writeconfig", Source::Code);
        console.run(&mut EngineCommands {
            host: &mut host,
            input: &mut input,
            ui: &mut ui,
            world: None,
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
}
