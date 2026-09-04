# `src/engine/` — API reference

The engine. [`portdocs/ENGINE.md`](../portdocs/ENGINE.md) breaks the original `engine/`
module into 23 subsystems, 13 of which become modules here. **Three exist so far.**

| Module | Subsystem | Status |
|---|---|---|
| [`host`](#engine-host) | `host_state.cpp`, `sys_engine.cpp` (§7.2) | state machine + frame clock done; no simulation |
| [`world`](#engine-world) | `modelloader.cpp`, `cmodel.cpp` (§7.14) | `.bsp` geometry done; no visibility, collision or props |
| [`window`](#engine-window) | `sys_mainwind.cpp`, `sys_getmodes.cpp`, `sdlmgr.cpp` (§7.3) | window + event loop done; input not started |
| `net/`, `console/`, `client/`, `server/`, `audio/`, … | the other 10 | not started |

The binary now loads a real Portal 2 `.bsp` and draws it. Most of what it draws is the
magenta error checkerboard, because most Portal 2 world materials name
`LightmappedGeneric` and the shader set is still one deep — see
[Known limits](#known-limits-of-what-is-drawn).

---

## Quick start

```rust
use crate::engine::window::{self, Boot, RunOutcome, VideoConfig};

let video = VideoConfig::from_command_line(&cmdline, game_title.as_deref());
let boot = Boot {
    vfs: vfs.as_ref(),
    map: cmdline.value("+map"),
    test_material: cmdline.value("-vmt"),
    fps_max: engine::host::DEFAULT_FPS_MAX,
};
match window::run(video, boot)? {          // returns when the engine quits
    RunOutcome::Quit => 0,
    RunOutcome::Restart => { /* the launcher's restart loop */ }
}
```

`src/launcher/mod.rs` is the only caller. To see it work:

```
cargo run --release -- -basedir /path/to/game -game portal2 -window +map sp_a1_intro1
```

---

## The frame

This is the whole control flow, and the part worth holding in your head:

```
about_to_wait    -> Engine::deadline()      -> ControlFlow::WaitUntil
RedrawRequested  -> Engine::frame(now)      -> None: too early, return and wait
                                            -> Some(Quit | Restart): exit
                                            -> Some(Continue): carry on
                 -> Renderer::begin_frame() -> None: back off SKIP_RETRY
                 -> Engine::render(&mut frame)
                 -> Frame::present()
```

Four orderings in there are load-bearing:

1. **`Engine::frame` runs before the surface is acquired.** A frame the clock refuses
   costs no acquisition, and a frame that loads a map does not hold a swap-chain image
   across the load.
2. **`RenderContext::begin_frame` is inside `Engine::frame`**, and reclaims the previous
   frame's arenas before anything allocates ([`MATERIALS.md`](MATERIALS.md) gotcha #5).
3. **Every pass ends before `present`** — the borrow checker enforces it.
4. **Nothing sleeps.** See [Pacing](#pacing-is-split-in-two).

<a id="engine-host"></a>

## `src/engine/host/`

The frame clock and the state machine that owns level lifetime. Replaces
`engine/host_state.cpp` and the timing half of `engine/sys_engine.cpp`.

| | |
|---|---|
| Module | `crate::engine::host` |
| Lines | ~700 including tests |
| Tests | 17 (`cargo test engine::host`) |
| Dependencies | `std` only — no `wgpu`, no `winit`, no material system |

That last row is the design in one line: the host decides *that* a map should load and
*when* a frame should run, and knows nothing about how either is done. [`Level`] is that
seam, and it is why the state machine is tested without a GPU.

### `Host`

```rust
pub fn new(fps_max: f32) -> Host;
pub fn frame(&mut self, now: Instant, level: &mut dyn Level) -> Option<Outcome>;

pub fn request_new_game(&mut self, map: &str);   // HostState_NewGame
pub fn request_shutdown(&mut self);              // HostState_Shutdown
pub fn request_restart(&mut self);               // HostState_Restart

pub fn clock(&self) -> &FrameClock;
pub fn clock_mut(&mut self) -> &mut FrameClock;
pub fn state(&self) -> HostState;
pub fn has_level(&self) -> bool;
pub fn frame_time(&self) -> f32;
pub fn frame_count(&self) -> u64;
```

`frame` returning `None` means the clock refused this frame as early — the caller must
not render and must not busy-wait; [`FrameClock::deadline`] says when to return.

### `Level`

```rust
pub trait Level {
    fn load(&mut self, map: &str) -> Result<(), String>;
    fn unload(&mut self);
}
```

Valve reached `modelloader`, `sv`, the client and six other globals directly from inside
`State_NewGame`. The whole of that, from the host's point of view, is these two calls.
A failed `load` is recovered from, not propagated: the host reports it and returns to
`Run` with no level, which is `State_NewGame`'s "new game failed" path. **A bad map name
must never take the process down.**

### `HostState` and `Outcome`

```rust
pub enum HostState { Run, NewGame, GameShutdown, Shutdown, Restart }
pub enum Outcome { Continue, Quit, Restart }
```

`HOSTSTATES` (`host_state.cpp:54`) has eight. Three are omitted rather than stubbed:
`HS_LOAD_GAME` needs `save/`, and `HS_CHANGE_LEVEL_SP`/`_MP` need level transitions and a
server. **The knowledge kept is the shape**: every path from `Run` to a new level goes
*through* `GameShutdown`, so a level is always torn down before the next is built. That
is why `State_Run` funnels four different requests into the same state instead of jumping
straight to them, and it is reproduced exactly.

`Outcome` is `CEngine`'s `m_nQuitting` (`QUIT_TODESKTOP`/`QUIT_RESTART`) as a return value
rather than a field to poll. **The outer `CEngine` state machine is deleted**: `CEngine`
held `m_nDLLState`/`m_nNextDLLState` purely to carry a decision across the `IEngine`
interface boundary, `CHostState` reached it through `eng->SetNextState()`, and
`MainLoop` polled `GetQuitting()`. There is no such boundary here.

### `FrameClock`

```rust
pub fn new(fps_max: f32) -> FrameClock;
pub fn frame(&mut self, now: Instant) -> Option<f32>;   // Some(frame_time) = run one
pub fn deadline(&self) -> Option<Instant>;              // when the next may run
pub fn fps_max(&self) -> f32;
pub fn set_fps_max(&mut self, fps_max: f32);
pub fn filtered_time(&self) -> f32;                     // time swallowed since the last frame
```

`CEngine::Frame`'s timing fields plus `FilterTime`'s policy. Constants carried across:
`DEFAULT_FPS_MAX` 300 (`sys_engine.cpp:60`), `MAX_FPS` 1000 (`host.h:185`),
`MAX_FRAMETIME` 0.1 and `MIN_FRAMETIME` 0.001 (`host.h:187`).

Dropped from `FilterTime`, each because its input does not exist yet rather than because
it was judged unnecessary: the dedicated server's tick-rate lock, the `fps_max < 30`
cheat clamp, `fps_max_splitscreen`, `fps_max_menu`, and the timedemo bypass.

### Pacing is split in two

`CEngine::Frame` **sleeps inside itself** when a frame is early (`ThreadNanoSleep`,
`sys_engine.cpp:498`). That is the collision `portdocs/ENGINE.md` §6 warns about: `winit`
wants to own that wait through `ControlFlow`, and two systems both owning pacing — one
sleeping inside a callback the other scheduled — is the failure mode.

The resolution: **`host/` owns the policy, `window/` owns the mechanism.**
`FrameClock::frame` decides whether a frame runs, `FrameClock::deadline` says when the
next one may, and `about_to_wait` turns that into `ControlFlow::WaitUntil`. Nothing in
`host/` knows what a control flow is; nothing in `window/` decides.

<a id="engine-world"></a>

## `src/engine/world/`

A loaded map and the geometry it draws.

| | |
|---|---|
| Module | `crate::engine::world` |
| Lines | ~1,600 including tests |
| Tests | 20 (`cargo test engine::world`) |
| Dependencies | `bytemuck`, `glam`, `crate::filesystem`, `crate::materials` |

### `World`

```rust
pub fn load(vfs: &Vfs, materials: &mut MaterialCache, device: &wgpu::Device, name: &str)
    -> Result<World, WorldError>;
pub fn draw(&self, pass: &mut Pass<'_>);
pub fn center(&self) -> Vec3;
pub fn summary(&self) -> String;

pub struct World {
    pub name: String,
    pub bsp_version: i32,
    pub bsp_revision: i32,
    pub batches: Vec<Batch>,
    pub bounds: (Vec3, Vec3),
    pub spawn: Option<Spawn>,
    pub sky_name: Option<String>,
    pub stats: WorldStats,
}
```

`load` is `HostState_NewGame` → `Host_NewGame` → `modelloader->GetModelForName` collapsed
into the one step that currently has meaning. **A material that fails to load is not an
error** — `MaterialCache::load` cannot fail — so the only failures are a missing or
malformed `.bsp`.

`draw` records every batch with an identity model matrix: world geometry is already in
world space, which is the whole difference between the world model and the brush models
that are not drawn yet.

### `Batch`

```rust
pub struct Batch {
    pub material: Arc<Material>,
    // private: one VertexBuffer, one IndexBuffer
}
```

Every face sharing a material, up to 65,536 vertices. Both halves are **static**, which
is a deliberate difference from the engine: Valve keeps static vertices and gathers the
*visible* faces' indices into a dynamic buffer each frame from the PVS
(`gl_rsurf.cpp:1168`). There is no visibility here yet, so every face is drawn every
frame and there is nothing per-frame to gather. When `mod_vis` lands the vertex buffers
stay and the index buffers become dynamic — which is exactly why
[`MATERIALS.md`](MATERIALS.md) makes `VertexSlice` and `IndexSlice` separate arguments.

### `Spawn` and `WorldStats`

```rust
pub struct Spawn { pub eye: Vec3, pub pitch: f32, pub yaw: f32 }

pub struct WorldStats {
    pub faces_total: usize,
    pub faces_drawn: usize,
    pub faces_not_drawn: usize,       // a surf flag said so
    pub faces_displaced: usize,       // geometry is in the displacement lumps
    pub faces_with_primitives: usize, // fan-approximated; see below
    pub vertices: usize,
    pub triangles: usize,
    pub materials: usize,
    pub materials_missing: usize,     // resolved to the error checkerboard
}
```

`Spawn` is `info_player_start`'s origin raised by `VEC_VIEW` (64 units) — the entity's
origin is at the player's feet, and a camera placed there looks at the floor.

### `world::bsp`

The `.bsp` reader. `Bsp::load(vfs, name)` reads `maps/<name>.bsp`; `Bsp::parse(path,
bytes)` does it without a mounted game, which is how it is tested.

```rust
pub struct Bsp {
    pub path: String,
    pub version: i32,
    pub revision: i32,
    pub entity_lump: String,
    pub vertices: Vec<[f32; 3]>,
    pub edges: Vec<Edge>,
    pub surfedges: Vec<i32>,
    pub faces: Vec<Face>,
    pub texinfo: Vec<TexInfo>,
    pub texdata: Vec<TexData>,
    pub texdata_string_table: Vec<String>,
    pub models: Vec<Model>,
}

pub fn world_model(&self) -> &Model;
pub fn model_faces(&self, model: &Model) -> &[Face];
pub fn face_material(&self, face: &Face) -> Option<&str>;
pub fn face_vertices(&self, face: &Face) -> impl Iterator<Item = Vec3> + '_;
pub fn texture_coordinate(&self, face: &Face, position: Vec3) -> [f32; 2];
pub fn entities(&self) -> Vec<Entity>;
```

Versions 19–21 are accepted (`MINBSPVERSION`/`BSPVERSION`); Portal 2 ships 21. The record
structs are `#[repr(C)]` + `bytemuck::Pod` transcriptions of `public/bspfile.h`, and their
sizes are asserted by a test — a silent change reads every subsequent record at the wrong
offset.

**On the parser choice:** `Cargo.toml` records that `binrw`/`deku` were left out because
the formats read so far are not struct arrays, and names `.bsp` as the candidate that
might change that. It does not — these lumps are plain `Pod` arrays that `bytemuck`
(already a dependency) reads with no derive macro and no parser DSL. Revisit for `.mdl`,
which has real internal pointers.

---

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **World triangles are emitted with their winding reversed, and this is not optional.**
   In file order every world surface is back-facing here, and the map draws as an empty
   clear colour — measured on `sp_a1_intro1`. The chain: Valve sets
   `D3DRS_CULLMODE = D3DCULL_CCW` (`shaderapidx8.cpp:4067`) and its own D3D→GL layer
   translates that to `glFrontFace(GL_CCW)` with back-face culling
   (`dxabstract.cpp:4107`) — which *reads* identical to this port's
   `front_face: Ccw, cull_mode: Back` and is not. GL's framebuffer origin is bottom-left,
   WebGPU's is top-left, and facing is decided **after** the viewport transform that
   flips between them, so the same `Ccw` names the opposite triangles. Valve content is
   therefore `Cw`-front here. The reversal happens once, at the boundary where external
   content enters — the same treatment [`MATERIALS.md`](MATERIALS.md) gives Valve's
   row-major matrices. **See [the open question](#open-question-the-culling-convention).**
2. **A request takes effect on the frame *after* it is made.** `FrameUpdate` breaks out
   of its loop whenever the state it just ran was `HS_RUN` (`host_state.cpp:817`), so
   `State_Run` only *arms* the transition. This is Valve's behavior, not an artifact: it
   is why `SCR_BeginLoadingPlaque` is called from `State_Run`, so the loading screen goes
   up on the frame that arms the load and is on screen for the frame that blocks doing
   it. Once it does run, the whole chain completes inside that one frame.
3. **`Host::frame` returning `None` is normal and must not be logged per frame.** It is
   `fps_max` doing its job. The caller must back off to `deadline()`, not spin.
4. **Nothing in `host/` or `window/` may sleep.** See
   [Pacing](#pacing-is-split-in-two).
5. **Closing the window is not an exit.** It calls `Engine::request_shutdown`, and the
   state machine unloads the level on its way out. An immediate `event_loop.exit()` would
   skip every teardown a loaded level needs.
6. **Texture coordinates divide by `dtexdata_t`'s size, not the material's.** Valve
   divides by the live material's `GetMappingWidth()`/`GetMappingHeight()`. The
   compile-time record has the property the runtime one lacks: it stays correct when the
   material falls back to the error checkerboard, which most Portal 2 world materials
   currently do. Dividing by the checkerboard's size would rescale every surface in the
   map.
7. **A `.bsp` is untrusted input.** `Bsp::validate` checks every cross-lump reference the
   geometry builder follows — once, at load — so walking a face later cannot index out of
   bounds. Valve validated lump *counts* and trusted the indices.
8. **A batch splits at 65,536 vertices**, before the face that would overflow and never
   in the middle of one, because a face's vertices must be contiguous for its fan.
9. **LZMA-compressed lumps are refused, not decoded.** Console builds compress
   individual lumps and stash the uncompressed size in the unused `fourCC`
   (`bsplib.cpp:5513`). Consoles are out of scope, and the alternative is reading
   compressed bytes as geometry and drawing noise.

## Known limits of what is drawn

Not bugs; each names what it waits on.

| Not drawn | Why |
|---|---|
| **Most world materials** | 62 of `sp_a1_intro1`'s 66 name `LightmappedGeneric`, which is not written. They are the magenta checkerboard. This is the single biggest visual gap and it closes with `materialsystem` stage 5. |
| Lightmaps | `LUMP_LIGHTING` is read by stage 5. Everything is fullbright. |
| Displacements | Geometry lives in `LUMP_DISPINFO`/`LUMP_DISP_VERTS`; `world/disp/` (§7.15). Counted in `WorldStats::faces_displaced`. |
| Brush entities (models 1..n) | Positioned by the entity that names them, so they need the entity system, not just the lump. |
| Static props, `.mdl` models | `staticpropmgr.cpp`, `studiorender`. |
| The 3D skybox | `worldspawn`'s `skyname` is read and recorded; drawing it is a second camera over a second set of geometry. |
| Visibility (PVS), area portals | `mod_vis.cpp`. **Every face in the map is drawn every frame.** Fine at 14.5k triangles; not fine on a real level. |
| Faces with explicit primitives | `BuildIndicesForWorldSurface` reads an index list from `LUMP_PRIMINDICES`; these are fan-triangulated instead. Valve's own assert says the index *count* is identical, so only the arrangement differs — visible solely on the non-convex surfaces the list exists for (water). Counted in `WorldStats::faces_with_primitives`. |
| Collision, traces | `cmodel.cpp`, `enginetrace.cpp` — needed by gameplay, not by drawing. |
| Input, simulation, sound, netcode | Not started. `State_Run` has no `Host_RunFrame` to call. |

### The camera is a placeholder

There is no input, so a fixed camera would show one wall. `Engine::camera` places the
view at the spawn point and **turns it slowly on the spot** (12°/s) so that the geometry,
depth and materials are all visible. What is faithful is the projection — `VIEW_NEARZ` 7
(`game/client/view.h:27`), far = `r_mapextents` × √3 (`view.cpp:644`), and Portal's
`default_fov` 75 (`clientmode_portal.cpp:32`) — and the coordinate system: Source is
**Z-up right-handed**, so the view is built with `Z` as up and world geometry needs no
conversion. `angles` are Valve's `(pitch, yaw, roll)` and **pitch is positive downwards**.

The turntable goes away with the first commit that can move the view, and takes
`Engine::camera` with it — `CViewRender::SetUpView` is its real replacement.

## Open question: the culling convention

Gotcha #1 is handled at the content boundary. **The arguably more correct fix is to flip
`front_face` to `Cw` in `PipelineCache`**, which would let every future Valve-authored
mesh — `.mdl` is next — load in its natural file order instead of each loader
remembering to reverse.

It is not done here because `src/materials/` currently has no Valve-authored geometry:
every vertex it draws is hand-wound in `preview.rs` for the present convention. Flipping
it was tried and **fails 17 of the stage-4 GPU tests**, and would require re-winding the
preview cube, the ground quad and every test quad. That is a material-system decision
made against the material system's own test suite, not a map-loading one. Recorded here
so it is a decision someone makes rather than a trap someone finds.

<a id="engine-window"></a>

## `src/engine/window/`

The game window and the event loop that drives the frame. Replaces `CGame`
(`sys_mainwind.cpp`), `CVideoMode` (`sys_getmodes.cpp`), `CSDLMgr` (`sdlmgr.cpp`),
`cocoamgr.mm`, `inputsystem/` and the vendored `thirdparty/SDL2`.

| | |
|---|---|
| Module | `crate::engine::window` |
| Lines | ~740 including tests |
| Tests | 12 (`cargo test engine::window`) |
| Dependencies | `winit` 0.30, `crate::engine`, `crate::materials`, `crate::filesystem`, `crate::launcher::cmdline` |

### `run`, `Boot` and `RunOutcome`

```rust
pub fn run(config: VideoConfig, boot: Boot<'_>) -> Result<RunOutcome, WindowError>;

pub struct Boot<'a> {
    pub vfs: Option<&'a Vfs>,
    pub map: Option<&'a str>,           // +map <name>
    pub test_material: Option<&'a str>, // -vmt <name>
    pub fps_max: f32,                   // +fps_max <n>
}

pub enum RunOutcome { Quit, Restart }
```

**Must be called from the main thread** — a hard AppKit requirement on macOS that `winit`
enforces on every platform.

`vfs` is `Option` because a failed mount is survivable: the launcher reports it and boots
the window anyway, since a window that opens and says what is wrong beats a process that
exits.

`RunOutcome` is `CEngineAPI::MainLoop`'s `RUN_OK`/`RUN_RESTART`. Nothing requests
`Restart` yet — the `restart` console command is what will — but the path exists all the
way out to the launcher, because a path that cannot be exercised is a path that is wrong.

`+map` and `+fps_max` are console commands spelled as command-line arguments, which is
how Source has always taken them: `CCommandLine` copies every `+`-prefixed argument into
the command buffer at startup. There is no command buffer yet, so the two the engine
needs to boot are read directly; `console/` replaces this.

### `VideoConfig`

Unchanged by this work. `from_command_line` is a port of
`OverrideMaterialSystemConfigFromCommandLine` (`matsys_interface.cpp:356`) plus the title
handling from `CGame::CreateGameWindow`. Switches, divergences from Valve's defaults
(windowed 1280x720, vsync on) and the two carried-across quirks (`-width` without
`-height` forcing 4:3; `-w`/`-h` beating `-width`/`-height`) are unchanged — see the
tests, which are the specification.

### Two deadlines, and why the later wins

`about_to_wait` can have two outstanding "do not come back before" times:

- **`SKIP_RETRY` (100 ms)**, when `Renderer::begin_frame` returned `None`. The window owns
  this: a surface that refuses to hand over an image is not an engine concept. Without it,
  a window that is off screen spins at 100% of a core — measured at ~75,000 failed
  acquisitions a second on macOS/Metal. It cannot be driven off `WindowEvent::Occluded`,
  which macOS does not send when a window is covered by another application.
- **The engine's**, when `FilterTime` refused a frame as early.

It takes the maximum, and **requests no redraw while waiting** — a pending redraw request
wakes the loop and defeats the deadline.

<a id="engine-root"></a>

## `src/engine/mod.rs` — `Engine`

What `portdocs/ENGINE.md` §1 calls `mod.rs`: it owns the subsystems as real fields and
hands out `&mut` where one needs another, replacing the ambient `g_p*` globals. All three
`CAppSystemGroup` layers are deleted; what survives of them is the ordering they encoded,
which is now the order of the statements in `Engine::new`.

```rust
pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, vfs: Option<&'a Vfs>,
           fps_max: f32, test_material: Option<&str>) -> Engine<'a>;

pub fn frame(&mut self, now: Instant) -> Option<Outcome>;
pub fn render(&mut self, frame: &mut Frame<'_>);
pub fn deadline(&self) -> Option<Instant>;
pub fn request_new_game(&mut self, map: &str);
pub fn request_shutdown(&mut self);
pub fn host(&self) -> &Host;
```

The renderer stays with the window because the surface is tied to the window handle, and
a live `Frame` borrows it; the engine takes device handles instead, which are cheap
refcounted clones.

Internally `Engine` is two fields: the `Host`, and a private `Scene` holding the `Vfs`,
the device, the `MaterialCache`, the `RenderContext`, the `World` and `curtime`. `Scene`
is what implements [`Level`], so `host.frame(&mut self.scene)` is a split borrow of two
fields rather than `&mut self` twice — that is the whole reason for the split.

`-vmt` is owned here too: when set, `render` draws the material preview *instead of* the
world, because it is an inspector for one material and anything else in the shot defeats
the purpose. `portdocs/MATERIALSYSTEM.md` §9 calls for deleting it once there is a map to
draw, and there now is; it is kept because it remains the only way to inspect a single
material in isolation and because `src/materials/preview.rs` carries the material
system's GPU regression suite.

## Test coverage

49 tests across the three modules; 252 in the crate.

| Test | Guards |
|---|---|
| `a_request_takes_effect_on_the_frame_after_it_is_made` | gotcha #2, the two-frame transition |
| `the_transition_chain_completes_within_a_single_frame` | that it is two frames, not four |
| `changing_level_unloads_the_old_one_first` | the load-bearing invariant of the state machine |
| `a_map_that_fails_to_load_leaves_the_host_running` | a bad map name is survivable |
| `quitting_unloads_the_level_on_the_way_out` | gotcha #5 |
| `restart_is_a_different_outcome_from_quit` | the distinction the launcher needs |
| `refused_frames_accumulate_their_time_rather_than_losing_it` | that a limiter postpones time and never discards it |
| `a_long_stall_is_clamped` | `MAX_FRAMETIME`, so a hitch is not simulated in one step |
| `fps_max_is_clamped_to_the_engines_ceiling` | `MAX_FPS` |
| `a_quad_triangulates_as_a_reversed_fan_from_its_first_vertex` | gotcha #1 — the failure mode is an *empty screen*, not a wrong picture |
| `a_batch_splits_before_it_runs_out_of_16_bit_indices` | gotcha #8 |
| `faces_the_compiler_marked_undrawable_are_skipped` | all six `surf` flags |
| `record_sizes_match_the_file_format` | every `bspfile.h` struct size |
| `a_negative_surfedge_walks_its_edge_backwards` | `Mod_LoadSurfedges`' sign rule |
| `a_compressed_lump_is_reported_rather_than_read_as_geometry` | gotcha #9 |
| `a_face_naming_a_vertex_that_is_not_there_is_caught_at_load` | gotcha #7 |
| `texture_coordinates_are_divided_by_the_texture_size` | gotcha #6 |

Anything touching `winit` or `wgpu` needs a display and a GPU, so the frame loop and the
world draw are verified by running the binary — see [Quick start](#quick-start).
