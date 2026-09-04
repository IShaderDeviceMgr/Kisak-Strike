//! The engine.
//!
//! `portdocs/ENGINE.md` breaks the original `engine/` module into 23
//! subsystems and concludes it must not be ported as one unit: each subsystem
//! becomes its own module here, 13 of them surviving, with ~45,700 lines
//! deleted outright. Three exist so far — [`window`], [`host`] and [`world`] —
//! and this file is what §1 calls `mod.rs`: the thing that owns them and hands
//! out `&mut` where one needs another, in place of the ambient `g_p*` globals
//! the C++ used to find everything.
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
//! window: about_to_wait   -> Engine::deadline    -> ControlFlow::WaitUntil
//! window: RedrawRequested -> Engine::frame       -> host clock + state machine
//!                         -> Renderer::begin_frame
//!                         -> Engine::render      -> one pass, the world in it
//!                         -> Frame::present
//! ```
//!
//! Two orderings in there are not stylistic, and `rustdocs/MATERIALS.md` states
//! why: [`RenderContext::begin_frame`] runs before anything allocates, and
//! every pass ends before the frame is presented.

pub mod host;
pub mod window;
pub mod world;

use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;

use crate::filesystem::Vfs;
use crate::materials::context::{Camera, Load};
use crate::materials::renderer::Frame;
use crate::materials::{Material, MaterialCache, MaterialPreview, RenderContext, CLEAR_COLOR};

use host::{Host, Level, Outcome};
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

/// How fast the placeholder camera turns, in degrees a second.
///
/// **Temporary.** There is no input yet — `src/engine/window/` drops keyboard
/// and mouse events on the floor — so a fixed camera would show one wall of a
/// map and nothing else. Turning slowly on the spot is the cheapest thing that
/// demonstrates real geometry, real depth and real materials. It goes away with
/// the first commit that can move the view, and takes [`Engine::camera`] with
/// it.
const TURN_RATE: f32 = 12.0;

/// The engine.
///
/// The lifetime is the mounted game content's: the [`Vfs`] is built by the
/// launcher and outlives this. It is an `Option` because a failed mount is
/// survivable — see [`window::run`].
pub struct Engine<'a> {
    host: Host,
    /// Everything the host drives when it changes level. Separate from [`Host`]
    /// so that `host.frame(&mut self.scene)` is a split borrow of two fields
    /// rather than `&mut self` twice.
    scene: Scene<'a>,
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
        fps_max: f32,
        test_material: Option<&str>,
    ) -> Engine<'a> {
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
            host: Host::new(fps_max),
            scene: Scene {
                vfs,
                device: device.clone(),
                materials,
                context,
                world: None,
                preview,
                curtime: 0.0,
            },
        }
    }

    /// Queues a map. See [`Host::request_new_game`].
    pub fn request_new_game(&mut self, map: &str) {
        self.host.request_new_game(map);
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
        self.scene.curtime += self.host.frame_time();

        // Reclaims the previous frame's uniform and geometry arenas. Must
        // happen before anything allocates out of them and after the previous
        // frame is done being recorded — `rustdocs/MATERIALS.md` gotcha #5.
        self.scene.context.begin_frame();

        Some(outcome)
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
    /// `angles` are Valve's `(pitch, yaw, roll)` in degrees, and the forward
    /// vector is `AngleVectors`' (`mathlib/mathlib_base.cpp`): yaw turns about
    /// `+Z`, and **pitch is positive downwards**, which is the sign error to
    /// watch for if the view ever looks at the ceiling when it should look at
    /// the floor.
    fn camera(&self, size: (u32, u32)) -> Camera {
        let (width, height) = size;
        let aspect = width.max(1) as f32 / height.max(1) as f32;

        let (eye, pitch, yaw) = match &self.scene.world {
            Some(world) => {
                let spawn = world.spawn;
                let eye = spawn.map(|s| s.eye).unwrap_or_else(|| world.center());
                let pitch = spawn.map(|s| s.pitch).unwrap_or(0.0);
                let yaw = spawn.map(|s| s.yaw).unwrap_or(0.0);
                (eye, pitch, yaw + self.scene.curtime * TURN_RATE)
            }
            None => (Vec3::ZERO, 0.0, 0.0),
        };

        let (pitch, yaw) = (pitch.to_radians(), yaw.to_radians());
        let forward = Vec3::new(
            yaw.cos() * pitch.cos(),
            yaw.sin() * pitch.cos(),
            -pitch.sin(),
        );

        Camera::perspective(
            eye,
            glam::camera::rh::view::look_at_mat4(eye, eye + forward, Vec3::Z),
            DEFAULT_FOV,
            aspect,
            VIEW_NEAR_Z,
            VIEW_FAR_Z,
        )
    }
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
