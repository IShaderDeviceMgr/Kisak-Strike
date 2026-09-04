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
//! window: WindowEvent     -> Engine::push_input  -> queued, not acted on
//! window: about_to_wait   -> Engine::deadline    -> ControlFlow::WaitUntil
//! window: RedrawRequested -> Engine::frame       -> host clock + state machine
//!                                                -> Console::run, then fps_max
//!                                                -> Input::frame, then the view
//!                         -> Renderer::begin_frame
//!                         -> Engine::render      -> one pass, the world in it
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
pub mod window;
pub mod world;

use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;

use crate::cmdline::CommandLine;
use crate::filesystem::{PathId, Vfs};
use crate::materials::context::{Camera, Load};
use crate::materials::renderer::Frame;
use crate::materials::{Material, MaterialCache, MaterialPreview, RenderContext, CLEAR_COLOR};

use console::{
    Command, CommandSpec, CommandTarget, ConfigFiles, Console, Cvar, CvarFlags, Dispatch,
    ExecContext, Source,
};
use host::{Host, Level, Outcome};
use input::view::FlyCamera;
use input::{Button, Input, Key, MouseButton};
use world::World;

/// `VIEW_NEARZ` (`game/client/view.h:27`).
const VIEW_NEAR_Z: f32 = 7.0;

/// `CViewRender::GetZFar` (`game/client/view.cpp:644`): the map's extents times
/// the diagonal of a cube, which is the furthest two points in a map can be
/// apart. `r_mapextents` defaults to 16384.
const VIEW_FAR_Z: f32 = 16384.0 * 1.732_050_8;

/// `default_fov` for Portal (`game/client/portal/clientmode_portal.cpp:32`).
/// Horizontal, which is how every Valve entry point spells a field of view.
const DEFAULT_FOV: f32 = 75.0;

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
    host: Host,
    /// Everything the host drives when it changes level. Separate from [`Host`]
    /// so that `host.frame(&mut self.scene)` is a split borrow of two fields
    /// rather than `&mut self` twice.
    scene: Scene<'a>,
    /// What the platform reported. Filled by [`window`] between ticks and
    /// drained by [`Engine::frame`]; see [`input`].
    input: Input,
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
    /// Where the view is. Level state, because loading a map puts it at the
    /// map's spawn point; see [`Scene::load`].
    view: FlyCamera,
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
        ] {
            console
                .register_command(spec)
                .expect("the engine's commands are unique");
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
                view: FlyCamera::new(Vec3::ZERO, 0.0, 0.0),
                curtime: 0.0,
            },
            input: Input::new(),
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
        self.console.enqueue("exec valve.rc", Source::Code);
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

    /// Queues one input event. `CInputSystem::PostEvent`.
    ///
    /// Called from [`window`] as events arrive, which is **between** ticks:
    /// nothing here acts on it, and [`Engine::frame`] dispatches the queue
    /// once the host has agreed a frame is happening.
    pub fn push_input(&mut self, event: input::Event) {
        self.input.push(event);
    }

    /// Whether the game wants the mouse.
    ///
    /// [`window`] turns this into a cursor grab, and holds the grab only while
    /// the window also has focus. Splitting it this way is what keeps
    /// "the game wants the mouse" (which survives an alt-tab) apart from
    /// "the cursor is held right now" (which must not).
    pub fn wants_mouse_capture(&self) -> bool {
        self.input.mouse_look()
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

        // `Cbuf_Execute`. **Inside the frame**, so that one run is one tick and
        // `wait 1` means "next frame" — running it per window event instead
        // would tick the command buffer at the display's rate.
        //
        // The borrow is `ENGINE_CONSOLE.md` §6.6: `self.console.run(&mut self)`
        // cannot compile, so a struct of disjoint field borrows is the target.
        // It is the same move `host.frame(&mut self.scene)` above already
        // makes.
        let Engine { console, host, .. } = self;
        console.run(&mut EngineCommands { host });

        // What `fps_max_callback` did. A poll rather than a callback, because a
        // callback would have to own `&mut Host` — §6.2.
        if self.fps_max.changed(&mut self.fps_max_generation) {
            self.host.clock_mut().set_fps_max(self.fps_max.float());
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

        // `DispatchAllStoredGameMessages`' place in `MainLoop`
        // (`sys_mainwind.cpp:509`): after the frame is agreed, before anything
        // uses what it says. It is *after* the host, so a frame that loaded a
        // level moves the view the level put it at, not the previous one's.
        self.update_view(seconds);

        // Reclaims the previous frame's uniform and geometry arenas. Must
        // happen before anything allocates out of them and after the previous
        // frame is done being recorded — `rustdocs/MATERIALS.md` gotcha #5.
        self.scene.context.begin_frame();

        Some(outcome)
    }

    /// Dispatches this tick's input and moves the view with it.
    ///
    /// The stand-in for `CInput::CreateMove` plus `CViewRender::SetUpView`,
    /// and it is a stand-in twice over: there is no player to move and no
    /// binding table to ask, so the camera flies and the keys are the ones
    /// Portal 2 ships defaults for. [`FlyCamera`] has the details.
    ///
    /// Two orderings matter. The mouse is applied under the capture state the
    /// motion was *accumulated* under, before this tick's events can change
    /// it; and the camera moves after, so a tap of a movement key on the same
    /// tick as a click is not lost.
    fn update_view(&mut self, seconds: f32) {
        let (dx, dy) = self.input.frame();
        if self.input.mouse_look() {
            self.scene.view.look(dx, dy);
        }
        self.scene.view.step(&self.input, seconds);

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

    /// Where the view is.
    ///
    /// **A placeholder for [`CViewRender::SetUpView`]**, which is
    /// `game/client/view.cpp` and arrives with the client. What is faithful
    /// here is the projection — Valve's near and far planes and Portal's field
    /// of view — and the coordinate system: Source is **Z-up, right-handed**,
    /// so the view is built with `Z` as the up axis and world geometry needs no
    /// conversion on the way to the GPU.
    ///
    /// The basis comes from `AngleVectors` (`mathlib/mathlib_base.cpp:1027`)
    /// rather than being rebuilt here, so that the direction the camera looks
    /// and the direction it moves are the same arithmetic — and so that a
    /// rolled view (Portal 2 rolls constantly) tilts the picture rather than
    /// only the movement. **Pitch is positive downwards**, which is the sign
    /// error to watch for if the view ever looks at the ceiling when it should
    /// look at the floor.
    fn camera(&self, size: (u32, u32)) -> Camera {
        let (width, height) = size;
        let aspect = width.max(1) as f32 / height.max(1) as f32;

        let eye = self.scene.view.origin;
        let (forward, _, up) = self.scene.view.angles.vectors();

        Camera::perspective(
            eye,
            glam::camera::rh::view::look_at_mat4(eye, eye + forward, up),
            DEFAULT_FOV,
            aspect,
            VIEW_NEAR_Z,
            VIEW_FAR_Z,
        )
    }
}

/// Whether the mouse should still be driving the view after this tick.
///
/// Escape gives the cursor back; a click takes it again. **Do not drop the
/// Escape half**: with no UI there is otherwise no way to get the cursor out
/// of a grabbed window.
///
/// This is the placeholder for `CGame::DispatchInputEvent`'s precedence chain
/// (`sys_mainwind.cpp:399`), which walked VGui, then RocketUI, then GameUI,
/// then the client, asking each whether it wanted the event. Under `egui` that
/// chain becomes one "did the UI consume this" answer
/// (`portdocs/ENGINE_INPUT.md` §8.3), and until there is a UI it is these two
/// keys.
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
        self.view = match world.spawn {
            Some(spawn) => FlyCamera::new(spawn.eye, spawn.pitch, spawn.yaw),
            None => FlyCamera::new(world.center(), 0.0, 0.0),
        };

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
                "source-engine: world: view at ({:.0} {:.0} {:.0}) pitch {:.0} yaw {:.0}",
                spawn.eye.x, spawn.eye.y, spawn.eye.z, spawn.pitch, spawn.yaw
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
/// It grows a field per subsystem that gains commands. Today `map`, `quit` and
/// `restart` are all [`Host`] requests, so it holds one.
struct EngineCommands<'e> {
    host: &'e mut Host,
}

impl CommandTarget for EngineCommands<'_> {
    fn execute(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) -> Dispatch {
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
            "quit" => self.host.request_shutdown(),
            "restart" => self.host.request_restart(),
            _ => return Dispatch::Unknown,
        }
        Dispatch::Handled
    }
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
}
