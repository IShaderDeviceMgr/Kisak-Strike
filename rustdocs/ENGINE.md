# `src/engine/` — API reference

The engine. [`portdocs/ENGINE.md`](../portdocs/ENGINE.md) breaks the original `engine/`
module into 23 subsystems, 14 of which become modules here. **Five exist so far.**

| Module | Subsystem | Status |
|---|---|---|
| [`host`](#engine-host) | `host_state.cpp`, `sys_engine.cpp` (§7.2) | state machine + frame clock done; no simulation |
| [`world`](#engine-world) | `modelloader.cpp`, `cmodel.cpp` (§7.14) | `.bsp` geometry, lightmaps, brush models, terrain and props done; no visibility, no 3D skybox |
| [`input`](#engine-input) | `inputsystem/`, `keys.cpp`, `in_*.cpp` (§7.3/§7.4) | buttons, mouse look, bindings, UI precedence and a free-fly camera done; no controllers |
| [`console`](#srcengineconsole) | `convar.cpp`, `commandbuffer.cpp`, `cmd.cpp`, `cvar.cpp`, `console.cpp`, `consoledialog.cpp` (§7.4) | complete — cvars, commands, buffer, `exec`, `stuffcmds`, `bind`, `config.cfg`, the list commands and the `egui` dialog |
| [`window`](#engine-window) | `sys_mainwind.cpp`, `sys_getmodes.cpp`, `sdlmgr.cpp` (§7.3) | window, event loop, input translation and the `egui` boundary done |
| `net/`, `client/`, `server/`, `audio/`, … | the other 9 | not started |

The binary now loads a real Portal 2 `.bsp` and draws it **lit**: base textures multiplied
by the map's baked lightmaps, packed into an atlas at load. On `sp_a1_intro1` that is
5,512 of 5,638 world faces over 77 batches, 71 of its 74 materials resolving, and 4,846
surfaces with real lighting across 13 atlas pages — plus 26 of its 78 brush entities and
1,080 static props — and **WASD and the mouse walk through it**.
What is still missing is listed under
[Known limits](#known-limits-of-what-is-drawn); the largest items are visibility (every
face is drawn every frame), displacements and the 3D skybox.

---

## Quick start

```rust
use crate::engine::window::{self, Boot, RunOutcome, VideoConfig};

let video = VideoConfig::from_command_line(&cmdline, game_title.as_deref());
let boot = Boot {
    vfs: vfs.as_ref(),
    command_line: Some(&cmdline),        // stuffcmds and +<cvar> seeding read this
    test_material: cmdline.value("-vmt"),
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
WindowEvent      -> GameWindow::offer_to_ui()  -> Consumer::{Ui, Game}
                 -> Engine::push_input(e, c)   -> queued, not acted on
DeviceEvent      -> Engine::push_input()       -> raw motion, always Game
about_to_wait    -> Engine::deadline()         -> ControlFlow::WaitUntil
RedrawRequested  -> Engine::frame(now)         -> None: too early, return and wait
                                               -> Input::frame()   [key-up latch]
                                               -> Input::dispatch_bindings(console)
                                               -> Console::run(EngineCommands)
                                               -> fps_max, then the view
                                               -> Some(Quit | Restart): exit
                                               -> Some(Continue): carry on
                 -> apply_capture()            -> the cursor grab follows the engine
                 -> Renderer::begin_frame()    -> None: back off SKIP_RETRY
                 -> Engine::render(&mut frame) -> the world, then the refractors
                 -> Context::run_ui(|| Engine::run_ui())
                 -> UiRenderer::draw(&mut frame, …)  -> the console, over the top
                 -> Frame::present()
```

Eight orderings in there are load-bearing:

1. **`Engine::frame` runs before the surface is acquired.** A frame the clock refuses
   costs no acquisition, and a frame that loads a map does not hold a swap-chain image
   across the load.
2. **`RenderContext::begin_frame` is inside `Engine::frame`**, and reclaims the previous
   frame's arenas before anything allocates ([`MATERIALS.md`](MATERIALS.md) gotcha #5).
3. **Every pass ends before `present`** — the borrow checker enforces it.
4. **Input is drained inside `Engine::frame`**, after the host has agreed a frame is
   happening — `DispatchAllStoredGameMessages`' place in `MainLoop`. Events pile up
   between ticks rather than being sampled by a frame that never runs; see
   [`input`](#engine-input) gotcha #2.
5. **Input, then bindings, then the console, then the view** — in that order, all inside
   one frame. A key pressed this tick therefore moves the view *this* tick rather than the
   next one. It also means one `Console::run` is one command-buffer tick, which is what
   makes `wait 1` mean "next frame"; running the console per window event would tick it at
   the display's rate instead.
6. **Nothing sleeps.** See [Pacing](#pacing-is-split-in-two).
7. **A refracting draw is a second pass with a copy between it and the first.**
   `World::draw` records the opaque scene, that pass *ends*,
   `RenderContext::update_refract_texture` copies the scene target, and a second
   `Load::Keep` pass records `World::draw_refracting`. The middle step is why the first
   pass has to end: a pass cannot sample its own attachment. Skipped entirely when
   `World::needs_frame_buffer_copy` is false.
8. **The UI is built and drawn inside the acquired frame**, after the world. `egui`'s
   `TexturesDelta` must be applied by whoever built it (`epaint` asserts on drop that it
   was), so a pass built for a frame that then found no swap-chain image would leave an
   upload owed to nobody. A skipped frame therefore skips `egui` entirely and its events
   stay queued for the next one.

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
| Lines | ~7,200 including tests |
| Tests | 64 (`cargo test engine::world`), plus four depot-gated |
| Dependencies | `bytemuck`, `glam`, `crate::filesystem`, `crate::materials` |

### `World`

```rust
pub fn load(vfs: &Vfs, materials: &mut MaterialCache, device: &wgpu::Device, name: &str)
    -> Result<World, WorldError>;
pub fn draw(&self, pass: &mut Pass<'_>, curtime: f32);
pub fn needs_frame_buffer_copy(&self) -> bool;
pub fn draw_refracting(&self, pass: &mut Pass<'_>, curtime: f32);
pub fn sync_brush_models(&mut self, placement: impl Fn(usize) -> Option<Placement>);
/// The models the game's entities place. Cannot run inside `load` — the entity
/// list is built from the lump `load` just read, so `Level::load` is where the
/// two halves meet.
pub fn load_entity_models(
    &mut self, vfs: &Vfs, materials: &mut MaterialCache, device: &wgpu::Device,
    entities: &[entities::ModelEntity],
);
pub fn sync_entity_models(&mut self, entities: &[entities::ModelEntity]);
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
    pub lighting_is_hdr: bool,
    pub lightmaps: LightmapPages,
    /// The `.bsp`'s model lump — bounding boxes, for `Server::level_init`.
    pub models: Vec<bsp::Model>,
    /// Every brush entity the map places — `ENGINE_TRACE.md` stage 2.
    pub brush_models: Vec<PlacedBrushModel>,
    /// The drawable ones among them, with their geometry.
    pub brush_model_geometry: Vec<BrushModelGeometry>,
    pub stats: WorldStats,
}
```

`load` is `HostState_NewGame` → `Host_NewGame` → `modelloader->GetModelForName` collapsed
into the one step that currently has meaning. **A material that fails to load is not an
error** — `MaterialCache::load` cannot fail — so the only failures are a missing or
malformed `.bsp`.

`draw` records the world's batches, then the brush entities', then the static props. The
world's go under an identity model matrix — world geometry is already in world space —
and each brush entity's go under its own placement. **Terrain is in the world's batches**:
a displacement is world geometry with a different way of generating its vertices, not a
separate pass. See [`world::disp`](#worlddisp--the-terrain).

**`draw` deliberately holds back the geometry that has to read the frame it is drawn
into**, and `draw_refracting` is the rest of it. Today that means the static props wearing
a `Refract` material with no `$basetexture` of its own — glass that warps the scene behind
it. A render pass cannot sample its own colour attachment, so the caller has to sequence
three things:

```rust
{ let mut pass = …; world.draw(&mut pass); }          // the opaque scene; the pass ends
if world.needs_frame_buffer_copy() {
    context.update_refract_texture(frame, scene);       // UpdateRefractTexture
    let mut pass = … Load::Keep …;                      // Keep, not Clear
    world.draw_refracting(&mut pass);
}
```

`Engine::render` is that, and `MATERIALS.md`'s
[frame-buffer copy](MATERIALS.md#the-frame-buffer-copy) is the other half. It is Valve's
own ordering — the opaque list, `UpdateRefractTexture`, then the translucent list
(`viewrender.cpp:6195`) — made explicit because `wgpu` enforces what D3D9 left undefined.

**`needs_frame_buffer_copy` is asked once a frame and fixed at load**, so a map with
nothing that refracts pays neither the copy nor the pass. Measured over the depot:
**71 of the game's 106 maps answer yes**; on `sp_a1_intro1` it is one model, the
container's observation window. `draw_refracting` is safe to call either way — it draws
nothing when the answer is no.

**Only static props are offered there**, not world faces or brush entities, and that is a
measurement rather than a simplification: all 29 of the game's `$model 1` `Refract`
materials are on models, and **no brush face or displacement in the shipped game names
that shader**. The other inhabitants of Valve's translucent list — particles, sprites,
the water surface — are not ported.

`models` is the `.bsp`'s model lump kept for somebody else: the *server* needs it, because
a `func_door` computes how far it slides from the size of its own brushes and that number
is in the file rather than in the entity lump (`UTIL_SetModel`, `game/server/util.cpp:1426`).
`Level::load` hands it to `Server::level_init` beside `entities`, for the same reason and
by the same caller. 32 bytes each; the largest shipped map has 258.

**Materials are resolved before the geometry is built**, which is forced rather than
stylistic: a surface's vertex layout comes from the shader its material named, and how
wide a lightmap block it reserves comes from whether that material has a `$bumpmap`
(`RegisterLightmappedSurface`, `gl_matsysiface.cpp:216`). Neither is answerable from the
`.bsp`. `load` therefore groups faces by material name, loads every material, and only
then packs lightmaps and emits vertices.

### Brush models — `PlacedBrushModel` and `BrushModelGeometry`

```rust
pub struct PlacedBrushModel {
    pub classname: String,   // func_door, trigger_multiple, func_brush
    pub index: usize,        // the model the entity named: "*12" is 12
    pub model: BrushModel,   // the placement, shared with trace/
    pub render_mode: i32,    // RenderMode_t, 0 when the key is absent
    pub visible: bool,       // EF_NODRAW clear — live, from the server
    pub solid: bool,         // IsSolid() — ditto
    pub owned: bool,         // …and whether the server answered at all
}

pub struct BrushModelGeometry {
    pub placement: usize,    // which entry of World::brush_models
    pub batches: Vec<Batch>,
}

/// Where a brush entity is, as the game server sees it.
pub struct Placement {
    pub origin: Vec3,
    pub angles: Vec3,
    pub visible: bool,
    pub solid: bool,
}

impl World {
    /// Take every placement from whoever owns it. Once a frame.
    pub fn sync_brush_models(&mut self, placement: impl Fn(usize) -> Option<Placement>);
    /// The ones the player's trace is clipped against — `owned && solid`.
    pub fn clip_models(&self) -> &[BrushModel];
    /// `engine->SolidMoved` — every model a swept box meets, by "*N" index.
    pub fn brush_models_touching(
        &self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, out: &mut Vec<usize>,
    );
}

pub const RENDER_NONE: i32 = 10;   // kRenderNone
```

Model 0 of a `.bsp` is the world; models 1.. are the **brush entities** — doors,
platforms, the moving parts of a test chamber. They are drawn by `R_DrawBrushModel`
(`engine/gl_rsurf.cpp`), which is the ordinary world-surface draw with the entity's
matrix in place of the identity, and that is exactly what this is.

**Three facts make this small, and all three were measured rather than assumed:**

1. **A brush model's faces are in the model's own frame**, like a static prop's — so
   vertices and texture coordinates are built unchanged and the placement is a matrix.
   Checked over the whole game: of 4,309 displaced, unrotated brush models, 4,088 have
   face bounds matching their model box exactly and **none** matches it offset by the
   entity origin.
2. **The `SURF_*` filter is the whole of the visibility question.** Every `trigger_*`
   class in Portal 2 compiles to `SURF_NODRAW`/`SURF_TRIGGER` faces and drops out of
   `group_faces` with no per-classname rule — 11,635 brush entities across 106 maps, of
   which only 2,697 keep a drawable face. `trigger_portal_cleanser` is the instructive
   exception: it keeps 1,174 of its faces because a fizzler field really is visible.
3. **Where a brush model is comes from the entity, not the model lump.** `Model::origin`
   is "for sounds and lights, not a render transform"; the placement is the naming
   entity's `"origin"` and `"angles"`.

**The transform is `BrushModel::model_to_world`, recomputed per draw and never cached** —
deliberately, so that what is drawn and what `Tracer::trace_model` collides with cannot
drift apart. It is a handful of `Mat4` products for a map's few dozen brush models.

#### The placement is *live* — `sync_brush_models`

Until `src/server/` stage 3 the placement was read out of the entity lump once at
load and never written again, which is precisely why nothing in a map moved.
Now `Engine::frame` copies it out of the game server once a frame, after the
server's ticks and before the player is traced against anything:

```text
server.frame( dt )                    a door integrates its velocity
sync_brush_models( world, server )    engine/mod.rs — the joining layer
  world.sync_brush_models(|index| …)  asks by "*N" index
    BrushModel::set_placement(…)      the one transform both consumers read
update_client( … )                    …the player is traced against it
```

Four things about it:

- **The `"*N"` index is the key**, and it is usable because it is unique: across
  all 106 shipped maps there are 11,635 `(map, "*N")` pairs and **not one** is
  named by two entities.
- **`None` means "leave it where the lump put it"**, which is the answer for
  8,225 of those 11,635 — the brush entities whose classname the server has no
  class for. Nothing regresses for them.
- **`visible` and `solid` are live state, not map keys.** A `func_brush` is
  switched on and off by `Enable`/`Disable` all through a level and 337 of the
  game's 2,502 start switched off; `draw_brush_models` skips an invisible one
  and the `trace` command skips a non-solid one. `render_mode` stays a load-time
  decision beside them because it cannot change.
- **Neither module names the other.** `world/` defines `Placement` and
  `src/server/` answers with an `EntityCore`; `engine/mod.rs` converts, the same
  arrangement `console/` and `input/` already have.

`render_mode` is stored as the file's number rather than interpreted, because consumers
differ: `World` acts only on `RENDER_NONE`, which is the only render mode
`C_BaseEntity::ShouldDraw` (`c_baseentity.cpp:1884`) refuses, and collision ignores it
entirely — a `rendermode 10` brush is invisible and still solid. 94 brush entities in the
shipped game set it.

On `sp_a1_intro1`: **26 of 78 brush models draw**, 148 faces and 308 triangles, 117 of
them lit. Across all 106 maps, 2,608 draw with 22,502 faces and 47,866 triangles.

#### `owned` is the rule that keeps the clip chain honest

Stage 4 put brush entities in the player's trace, and **the map's 11,635 of
them are not all solid**: a `trigger_portal_cleanser` is a fizzler you walk
through, and `func_portal_bumper` — 2,383 of them, the ninth commonest
classname in the game — exists only to stop a portal landing on a wall.
Whether one is solid is `FSOLID_NOT_SOLID`, which is *game* state, and
`src/server/` has classes for 6,302 of the 11,635.

So `clip_models` is `owned && solid`: **collide with what the game has told us
about**, rather than assume everything is a wall. The alternative fills every
Portal 2 chamber with invisible walls, and would do it silently.

`brush_models_touching` is the other direction — the engine's half of the
server's touch test (`engine->SolidMoved`, `engine/world.cpp`'s `CTouchLinks`).
Two things it is *not*: not a bounding-box overlap (Valve's enumerator ends in
`ClipRayToCollideable( ray, MASK_SOLID, pTrigger, &tr )`, the swept box against
the trigger's actual brushes) and not filtered to triggers (which of them is
one is `FSOLID_TRIGGER`, the server's live state; a copy here would be a frame
stale every time something was enabled). It *is* filtered to `owned`, for the
same reason `clip_models` is.

### `Batch`

```rust
pub struct Batch {
    pub material: Arc<Material>,
    pub lightmap_page: u32,
    // private: one VertexBuffer, one IndexBuffer
}
```

**A batch is a (material, lightmap page) pair**, which is exactly Valve's *sort ID*:
`AllocateLightmap` returns one and increments it whenever either half changes
(`cmatlightmaps.cpp:306`), because the page is one texture binding and cannot vary within
a draw. A material whose surfaces did not all fit on one atlas page is several batches,
emitted in page order. On `sp_a1_intro1` that is 77 batches over 13 pages for the world;
each brush entity's batches are its own and live in
[`BrushModelGeometry`](#brush-models--placedbrushmodel-and-brushmodelgeometry), because
they are drawn under a different matrix.

Every face sharing a material and a page, up to 65,536 vertices. Both halves are **static**, which
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
    pub faces_displaced: usize,       // terrain: a SUBSET of faces_drawn
    pub triangles_displaced: usize,   // how many of `triangles` were terrain
    pub faces_with_primitives: usize, // fan-approximated; see below
    pub vertices: usize,
    pub triangles: usize,
    pub materials: usize,
    pub materials_missing: usize,     // resolved to the error checkerboard
    pub faces_lit: usize,             // got a real lightmap block
    pub faces_fullbright: usize,      // wanted one and could not have one
    pub faces_with_lightstyles: usize,// more than style 0; only style 0 is baked
    pub lightmap_pages: usize,        // including the 1x1 white page

    // Brush entities, kept apart from the face counters above: those answer
    // "how much of the level shell is on screen", and mixing a map's doors
    // into them makes both numbers harder to read.
    pub brush_models_drawn: usize,    // out of World::brush_models.len()
    pub brush_model_faces: usize,
    pub brush_model_triangles: usize,
    pub brush_model_faces_lit: usize,
}
```

`faces_lit + faces_fullbright` does not reach `faces_drawn`: the difference is the faces
whose *material* is not lit at all — tool textures, and anything that fell back to the
error material. Those never ask for a block, so neither counter moves.

**`faces_displaced` is a subset of `faces_drawn`, not a sibling of it.** It used to count
faces that were *skipped* because their geometry lived in the displacement lumps; terrain
draws now, so it counts terrain. `triangles_displaced` is the matching share of
`triangles`, and it is worth having separately because a power-4 patch is 512 triangles —
a map with a lot of terrain has a triangle count that says nothing about how big its level
shell is. On `sp_a1_intro1`: 11 displacements, 1,408 of 15,954 triangles.

`Spawn` is `info_player_start`'s origin raised by `VEC_VIEW` (64 units) — the entity's
origin is at the player's feet, and a camera placed there looks at the floor.

### `world::entities` — the models a *game entity* places

```rust
pub struct ModelEntity {
    pub id: u64,              // opaque and stable — what `sync` matches on
    pub model: String,        // models/props/portal_button.mdl
    pub origin: Vec3,
    pub angles: Vec3,         // pitch, yaw, roll
    pub skin: i32,
    pub visible: bool,        // ShouldDraw; an invisible one is still uploaded
    pub sequence: String,     // the LABEL — "up", "down"; "" is sequence 0
    pub cycle: f32,           // where in the sequence it was at anim_time
    pub anim_time: f32,       // when that was
    pub playback_rate: f32,   // signed; 0 holds the pose
}

pub struct SequenceRow<'a> {
    pub model: &'a str, pub label: &'a str,
    pub duration: f32, pub loops: bool, pub fade_out_time: f32,
}

pub struct EntityModels { pub stats: EntityModelStats, /* private */ }

impl EntityModels {
    pub fn load(
        vfs: &Vfs, materials: &mut MaterialCache, device: &wgpu::Device,
        entities: &[ModelEntity], ambient: &AmbientLighting, collision: &CollisionBsp,
    ) -> EntityModels;
    pub fn sync(&mut self, entities: &[ModelEntity]);
    /// What each loaded model says about each of its sequences — the answer
    /// back, for `crate::server::sequences::SequenceTable`.
    pub fn sequences(&self) -> impl Iterator<Item = SequenceRow<'_>> + '_;
    pub fn draw(&self, pass: &mut Pass<'_>, curtime: f32);
    pub fn draw_refracting(&self, pass: &mut Pass<'_>, curtime: f32);
    pub fn refracts(&self) -> bool;
    pub fn summary(&self) -> String;
}
```

The third kind of geometry in a level shell. World faces and brush entities are
`.bsp` geometry drawn with a matrix; static props are `.mdl` geometry the *map
compiler* placed, never moving and lit once. This is `.mdl` geometry the
**game** places, and where it is and what it is doing can change every tick.

**`prop_dynamic` is what this module is for.** It was written for a
`prop_floor_button` — 65 in the game, 1 on `sp_a1_intro1` — and the class that
followed places **8,462 entities across 105 of the 106 maps, 90 of them from 52
models on `sp_a1_intro1` alone**. Everything below either dates from the button
or was changed by the prop. `prop_testchamber_door` then added 2 more instances
and 1 more model to that map, for **93 entity models from 54 models**, and
changed nothing here at all — which is the point of the list below.

> **The chamber door is what says the per-bone draw split generalises.** A
> floor button is two bone runs, one of which moves; a door is **five**, of
> which three move, and they move in two separate acts — the two spinner rings
> turn about their own axes over the first 62% of `open`, *inside* the door's
> own thickness, and only then do the two leaves slide 53 units apart. So the
> first two thirds of the animation draw pixel-identically from outside and the
> doorway clears all at once, which is the model rather than the port;
> `entities::tests::the_chamber_door_draws_and_opens` measures both acts
> geometrically for that reason, and checks the pixels only where they can say
> anything.

Six things about it are worth knowing.

- **The join with the game is a sequence *name*, and the cycle is the engine's.**
  The server says which sequence, where in it the entity was, when that was, and
  how fast it is playing — exactly `DT_BaseAnimating`'s five networked fields —
  and the engine looks the label up in the model and solves for now:
  `cycle + elapsed * playback_rate / duration`, wrapped for a looping sequence
  and clamped otherwise. That is Valve's own split, with
  `C_BaseAnimating::FrameAdvance` on the *client* turning the networked state
  into a pose, and it is what keeps `server/`'s promise to name no studio type.
  It also makes the animation smooth where the server's 64 Hz ticks would step
  it.

- **A label that resolves to nothing is sequence 0, not the bind pose.** This
  is the one place the label seam does not say what Valve's seam says, and it
  has to be closed by hand. `m_nSequence` is an `int` that starts at zero, and
  `CDynamicProp` only ever moves it off zero deliberately: `Spawn` calls
  `PropSetAnim` only for a prop that has a `DefaultAnim` (`props.cpp:2036`),
  and `PropSetAnim` answers a name the model does not have with an explicit
  `SetSequence( 0 )` (`props.cpp:2422`). So **every prop in the game is posed
  by some sequence** — 6,046 of the 8,462 have no `DefaultAnim` at all and 183
  more name a sequence that is in no model — and the bind pose is reachable
  only by a model with no sequences whatsoever.

  The reason to care is that **a bind pose is not a pose anybody ever looked
  at**. For most models it happens to equal sequence 0 frame 0 and the
  difference cannot be seen; for 20 of `sp_a1_intro1`'s 91 entity models it
  cannot. `props_motel/hotel_container_furniture01`-`03` bind at `rot_x(+90)`
  against a `poseToBone` of `rot_x(-90)`, whose product is the identity, while
  their `idle` holds `Quaternion64(0.5, 0.5, 0.5, 0.5)` — a 120° turn about
  `(1,1,1)` — and *that* against the same `poseToBone` is `rot_z(+90)`. Posed
  from the bind pose the intro room's dresser, wardrobe and desk come out a
  quarter turn wrong and standing inside the bed, which is
  `hotel_container_furniture04` and a **static prop**: the static path never
  looks at a sequence, so the bed stays where it belongs and the three around
  it do not.
  `entities::tests::a_prop_with_no_default_anim_is_posed_by_sequence_zero`
  pins it.

- **`sequences()` is the same seam pointing the other way.** The game needs to
  know when an animation has *ended* — `CDynamicProp::AnimThink` fires
  `OnAnimationDone`, and 181 shipped connections listen for it — and it cannot
  read a `.mdl`. So `Engine::load_level` walks this iterator once and fills in
  `crate::server::sequences::SequenceTable`. `DynamicProp::cycle_now` then
  computes the *same* expression as `EntityModels::cycle` from the same five
  numbers; the two must not drift.

  It is a `SequenceRow` rather than a tuple because `prop_testchamber_door`
  added a fourth number to it: `fade_out_time`, which is what
  `CBaseAnimating::GetLastVisibleCycle` subtracts to decide that a non-looping
  sequence has *finished* before it has *ended*. `world/` still names no server
  type — the row-to-`SequenceInfo` translation is `engine::group_sequences`,
  the one function in the port that names both.

- **The list is keyed on `id`, and it used to be positional.** The note here
  said the condition for a real key would be "the first class that creates or
  destroys a model entity after the spawn pass". `prop_dynamic` is it: 556
  shipped connections fire `Kill` at one and 51 fire `FadeAndKill`. An instance
  whose id is missing from a frame's list is made **invisible and kept**, so
  that the model it uploaded — very likely shared with its neighbours — stays
  valid.

- **`visible` is carried, not filtered.** 1,000 props in the game are
  `StartDisabled` and 206 connections toggle one, so an invisible instance is
  one that may be drawn next tick. It is loaded, uploaded and skipped at record
  time.

- **Lighting is sampled once, at load.** An entity model has no `.vhv` —
  `vrad` bakes per-vertex light for static props and nothing else — so each one
  is lit by the leaf ambient cube where it stands, through
  [`AmbientLighting`](#ambientlighting). The condition for resampling per frame
  is the first entity model that **travels**, and `prop_dynamic` is not quite
  it: a prop that *animates* is re-posed every frame but its origin does not
  move, and 2,355 of them name a `parentname` whose transform this port does not
  apply anyway (`rustdocs/SERVER.md` gotcha 34).

### `AmbientLighting`

```rust
pub struct AmbientLighting { /* private */ }
impl AmbientLighting {
    pub fn from_bsp(bsp: &Bsp) -> AmbientLighting;
    pub fn ambient_at(&self, collision: &CollisionBsp, position: Vec3) -> AmbientCube;
    pub fn lighting_at(&self, collision: &CollisionBsp, position: Vec3) -> ModelLighting;
}
```

The three lumps that answer "how bright is it here", kept after the rest of the
`.bsp` is dropped. `World::load` reads a map, uploads it and lets the `Bsp` go;
a *static* prop is lit before that happens and needs nothing, but an entity's
model is placed later — the entities do not exist until the game server has
spawned them — and does. A few tens of kilobytes against the map's 12 MB of
lightmaps.

### `world::disp` — the terrain

```rust
// Private to `world/`; this is what the module does, not an API to call.
pub(super) struct Displacement { vertices: Vec<DispVertex>, indices: Vec<u16> }
pub(super) struct DispVertex { position: Vec3, texcoord: [f32; 2], luxel: [f32; 2], alpha: f32 }
pub(super) fn Displacement::build(bsp: &Bsp, face: &Face) -> Option<Displacement>;
```

A displacement replaces one four-sided world face with a `(2^power + 1)²` grid.
`portdocs/ENGINE_WORLD_DISP.md` is the porting doc; what matters at this level is that
**there is no separate terrain draw path**. A displacement is selected by
[`group_faces`](#batch), resolved to a material, packed into the same lightmap atlas,
grouped into the same `(material, page)` [`Batch`](#batch) and split by the same 16-bit
rule as an ordinary face — the only difference is that `build_page_meshes` asks the patch
for its vertices instead of fanning the face's winding. That is also what Valve does:
`DispInfo_CreateMaterialGroups` groups by `(lightmapPageID, material)`, which is what a
sort ID already was.

**Every one of Portal 2's 1,181 displacements is in model 0**, so nothing about brush
entities changes. Measured, not assumed.

Four rules here produce a plausible wrong picture rather than an error:

- **A displacement's texture coordinates are bilinear over the base face's four *flat*
  corner coordinates**, not the planar projection evaluated at the displaced position.
  The two agree on a flat patch and diverge with the displacement, so the wrong one looks
  correct until you stand next to a cliff.
- **Its lightmap coordinates are not the base face's at all.** `vrad` bakes a
  displacement against the *grid*, so `BuildDispSurfInit` computes the face's luxel
  corners and then overwrites them with a canonical square — collapsing to
  `luxel(i, j) = (0.5 + width * j/n, 0.5 + height * i/n)`, where `width`/`height` are
  `dface_t::lightmap_size`, the **extents**, one less than the block dimensions
  `Bsp::face_lightmap_size` returns. Swapping the two axes mirrors the lighting about the
  patch's diagonal, which on gentle terrain looks like nothing at all.
- **The render tessellation is not the collision one** — it is a quadtree walk that fans
  each node and skips any vertex `vbsp` disallowed (`DispInfo::allowed_verts`), which is
  what stops a power-4 patch cracking against a power-2 neighbour. 100 of the game's
  1,181 have at least one bit cleared, and **`sp_a1_intro1` has none**, so only the depot
  test exercises it. For a patch with every bit set the two coincide exactly, which is
  what `tessellation_matches_the_collision_surface` asserts — see the note on
  `world::disp::tessellate`.
- **Terrain triangles are reversed like everything else Valve authored** (gotcha 1). The
  reversal is in `Displacement::build`, not in the tessellation walk, so that the walk's
  output can be compared against `trace::disp`'s list winding and all.

The grid *positions* are `Bsp::disp_base_quad` + `Bsp::disp_grid`, **shared with
`trace::disp`** rather than derived twice — if the two ever disagreed the map would be
solid somewhere it is not drawn.

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
    pub lighting: Vec<ColorRgbExp32>,
    pub lighting_is_hdr: bool,
    pub level_flags: u32,
    // Collision, read here and given meaning by `trace/` — see
    // "Where the data comes from" below.
    pub planes: Vec<Plane>,
    pub nodes: Vec<Node>,
    pub leaves: Vec<Leaf>,
    pub leaf_brushes: Vec<u16>,
    pub brushes: Vec<Brush>,
    pub brush_sides: Vec<BrushSide>,
    pub disp_info: Vec<DispInfo>,
    pub disp_verts: Vec<DispVert>,
    pub disp_tris: Vec<DispTri>,
    // ...plus the game lumps, leaf ambient lighting and the pak lump.
}

pub fn world_model(&self) -> &Model;
pub fn model_faces(&self, model: &Model) -> &[Face];
pub fn face_material(&self, face: &Face) -> Option<&str>;
pub fn face_vertices(&self, face: &Face) -> impl Iterator<Item = Vec3> + '_;
pub fn texture_coordinate(&self, face: &Face, position: Vec3) -> [f32; 2];
pub fn lightmap_coordinate(&self, face: &Face, position: Vec3) -> [f32; 2];  // in luxels
pub fn face_lightmap_samples(&self, face: &Face) -> Option<&[ColorRgbExp32]>;
pub fn face_lightmap_blocks(&self, face: &Face) -> u32;   // 1, or 4 for SURF_BUMPLIGHT
pub fn face_lightmap_size(face: &Face) -> (u32, u32);     // extents + 1, in luxels
pub fn face_lightstyle_count(face: &Face) -> usize;
pub fn entities(&self) -> Vec<Entity>;

// A displacement's two counts, which the file does not record.
pub fn DispInfo::vert_count(power: i32) -> usize;   // (2^power + 1)^2
pub fn DispInfo::tri_count(power: i32) -> usize;    // 2^power * 2^power * 2
pub fn DispInfo::disp_flags(&self) -> u32;          // minTess, decoded

// A displacement's geometry, shared by `trace/` (collision) and `world/disp/`
// (drawing) so that the two cannot describe different surfaces.
pub fn disp_base_quad(&self, face: &Face, info: &DispInfo) -> Option<[Vec3; 4]>;
pub fn disp_grid(&self, info: &DispInfo, points: &[Vec3; 4]) -> Vec<Vec3>;
```

**Which lighting lump, and which faces lump, are one decision.** `LUMP_LIGHTING_HDR` wins
whenever it is non-empty, and `LUMP_FACES_HDR` comes with it — the HDR faces carry
different `light_ofs` values, and in an HDR-only map the LDR ones are meaningless.
`sp_a1_intro1` is exactly that: `LUMP_LIGHTING` is empty, and every face in `LUMP_FACES`
has `light_ofs` 0. `Mod_LoadFaces` (`modelloader.cpp:2188`) makes the same choice.

`face_lightmap_samples` returns **lightstyle 0 only**, and `light_ofs` needs no adjusting
to find it: `vrad` writes one average colour per style *ahead* of the samples and points
`light_ofs` past them. Verified against `sp_a1_intro1`, where consecutive faces' offsets
differ by exactly the sample bytes plus the next face's average colours.

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

<a id="engine-input"></a>

## `src/engine/trace/`

Ray and swept-box traces against the world's brushes, against the brush models built out
of them, and against its terrain. Stages 1-3 of
[`portdocs/ENGINE_TRACE.md`](../portdocs/ENGINE_TRACE.md), and what `src/client/` stage 4
walks on — [`rustdocs/CLIENT.md`](CLIENT.md) is its one real consumer.

| | |
|---|---|
| Replaces | `engine/cmodel.cpp`'s trace, `engine/cmodel_disp.cpp`, `public/dispcoll_common.cpp`, `engine/cmodel_bsp.cpp`'s load, `CCollisionBSPData` |
| Depends on | `world::bsp` (the lumps), `glam`, `crate::math`. **No GPU, no window, no I/O** |
| Status | world brushes, brush models, displacements **and the clip chain** — no static props, no vcollide |

### Quick start

```rust
use crate::engine::trace::{disp_surf, Contents, Ray};

// `World::load` builds one; it is `world.collision`.
let collision = &world.collision;

// A ray from the eye.
let hit = collision.tracer().trace(
    &Ray::line(eye, eye + forward * 8192.0),
    Contents::MASK_SOLID,
);
if hit.did_hit() {
    println!("{} at {:?}", collision.surface_name(hit.surface), hit.end);
}

// A player hull swept from the feet. Reuse one `Tracer` across a frame's
// traces: it owns the per-trace scratch, and a fresh one allocates a stamp
// per brush.
let mut tracer = collision.tracer();
let ground = tracer.trace(
    &Ray::hull(feet, feet - Vec3::Z * 2.0, VEC_HULL_MIN, VEC_HULL_MAX),
    Contents::MASK_PLAYERSOLID,
);
let standing = ground.did_hit() && ground.normal.z > 0.7;
// Terrain is part of the world and needs no separate call; it only announces
// itself in `disp_flags`.
let on_terrain = ground.disp_flags & disp_surf::SURFACE != 0;

// A brush model — a door, a platform. `World::brush_models` has the map's,
// resolved from the entity lump; `collision.brush_model(i, origin, angles)`
// builds one directly.
for placed in &world.brush_models {
    let hit = tracer.trace_model(&ray, &placed.model, Contents::MASK_PLAYERSOLID);
    if hit.did_hit() {
        println!("{} *{} at {:?}", placed.classname, placed.index, hit.end);
    }
}
```

**Nothing combines the world trace and the brush models for you.** A caller that wants
"what is in the way" asks both and keeps the smaller `fraction`. Doing that *for* the
caller is `ClipRayToCollideable`'s job and needs a filter and a broadphase, which need
entities — stage 4.

### `Ray` — the query

```rust
pub fn line(start: Vec3, end: Vec3) -> Ray;
pub fn hull(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Ray;
pub fn origin(&self) -> Vec3;
```

`mins`/`maxs` are relative to `start`, so a standing player is
`Ray::hull(feet, target, VEC_HULL_MIN, VEC_HULL_MAX)` with the constants from
[`client::player`](CLIENT.md).

**Internally the ray's start is the *centre* of the box** (`Ray_t`, `public/cmodel.h`),
which is what lets the brush clip push a plane out by one `|normal · extents|` instead of
picking a corner per plane. [`Ray::origin`] undoes the centring, and every field of
[`Trace`] is already in the caller's frame — see gotcha 1.

### `Contents` — the mask

A newtype over Valve's 32-bit `CONTENTS_*` set (`public/bspflags.h`), with the `MASK_*`
combinations under Valve's own names so the C++ stays greppable. `Contents::MASK_PLAYERSOLID`
is `PlayerSolidMask()` and is what every one of `client/`'s ~14 movement traces passes.

```rust
pub fn intersects(self, other: Contents) -> bool;  // the test the traversal runs
pub fn and(self, other: Contents) -> Contents;
pub fn or(self, other: Contents) -> Contents;
pub fn is_empty(self) -> bool;
```

`Display` prints hex, which is how every Valve tool writes it.

### `Trace` — the answer

```rust
pub struct Trace {
    pub start: Vec3,               // caller's frame; see gotcha 3
    pub end: Vec3,
    pub normal: Vec3,
    pub plane_dist: f32,
    pub fraction: f32,
    pub fraction_left_solid: f32,  // rays only; see gotcha 4
    pub contents: Contents,
    pub surface: Option<u16>,      // index; resolve with `surface_name`
    pub disp_flags: u16,           // DISPSURF_FLAG_*; 0 unless terrain was hit
    pub surface_flags: i32,
    pub all_solid: bool,
    pub start_solid: bool,
}

pub fn did_hit(&self) -> bool;     // fraction < 1 || all_solid || start_solid
```

Absent, and what each waits on: `m_pEnt`, `hitgroup`, `physicsbone`, `hitbox` (entities
and `.mdl`, stages 4-5), `worldSurfaceIndex` (decals and paint, `render/`'s), and
`csurface_t::surfaceProps` (the physics surface-property database, which arrives with
`vphysics/` — a field that was always zero would read as "this surface has the default
properties", which is a different claim).

### `disp_surf` — was this terrain, and is it walkable

```rust
use crate::engine::trace::disp_surf;

if hit.disp_flags & disp_surf::SURFACE != 0 {
    let vbsp_says_walkable = hit.disp_flags & disp_surf::WALKABLE != 0;
}
```

`DISPSURF_FLAG_*` (`public/trace.h:25`), the per-triangle tags VBSP bakes into
`LUMP_DISP_TRIS` — `SURFACE`, `WALKABLE`, `BUILDABLE` and four `SURFPROP` slots. The
engine ORs `SURFACE` onto every displacement triangle, so a non-zero `disp_flags` is
exactly `CGameTrace::IsDispSurface()`: *this was terrain*.

`WALKABLE` is **not** the same question as `normal.z > 0.7`. The first is VBSP's
compile-time verdict on the triangle's slope; the second is `CategorizePosition` asking at
runtime about the triangle actually hit. Gameplay reads both, for different reasons.

The `SURFPROP` bits pick which of a material's `$surfaceprop`…`$surfaceprop4` applies.
They are carried and go no further: resolving one needs the physics surface-property
database, which arrives with `vphysics/`.

### `CollisionBsp` and `Tracer`

```rust
impl CollisionBsp {
    pub fn build(bsp: &Bsp) -> CollisionBsp;          // infallible
    pub fn tracer(&self) -> Tracer<'_>;
    pub fn point_contents(&self, point: Vec3) -> Contents;
    pub fn leaf(&self, point: Vec3) -> usize;
    pub fn surface_name(&self, surface: Option<u16>) -> &str;
    pub fn is_empty(&self) -> bool;
    pub fn summary(&self) -> String;
    /// How many displacements built collision geometry — stage 3.
    pub fn disp_count(&self) -> usize;
    /// One displacement's world bounds, bloated by a unit. `None` if it built none.
    pub fn disp_bounds(&self, index: usize) -> Option<(Vec3, Vec3)>;
    /// Model `index` of the `.bsp`'s model lump, placed. `None` if there is no
    /// such model. `angles` is a `QAngle`: pitch, yaw, roll, in degrees.
    pub fn brush_model(&self, index: usize, origin: Vec3, angles: Vec3)
        -> Option<BrushModel>;
}

impl BrushModel {
    pub fn model_to_world(&self) -> Mat4;
    /// Move it. `UTIL_SetOrigin` plus `SetLocalAngles`, from the entity.
    pub fn set_placement(&mut self, origin: Vec3, angles: Vec3);
}

impl Tracer<'_> {
    /// `CEngineTrace::TraceRay` — the world, then the clip chain.
    pub fn trace(&mut self, ray: &Ray, mask: Contents) -> Trace;
    /// The world's subtree alone. What `trace` is when the chain is empty.
    pub fn trace_world(&mut self, ray: &Ray, mask: Contents) -> Trace;
    pub fn trace_model(&mut self, ray: &Ray, model: &BrushModel, mask: Contents) -> Trace;
    /// Stage 4: put brush entities in the clip chain. See below.
    pub fn with_entities(self, entities: &[BrushModel]) -> Tracer<'_>;
}
```

### The clip chain — `with_entities`, and who decides what is in it

`CEngineTrace::TraceRay` (`engine/enginetrace.cpp:2786`) is stage 4, and it is what makes
a door a wall. A `Tracer` starts with an empty chain, so `trace` is a world-only sweep for
every caller that has not asked for more; `with_entities` adds the brush models, and
`World::clip_models` is where the list comes from.

**Which entities is the *game's* decision, not this module's.** Whether a brush entity is
solid is `FSOLID_NOT_SOLID` and whether it is a trigger is `FSOLID_TRIGGER`, and both live
in `src/server/`. `world/` carries the answer across as
[`PlacedBrushModel::owned`](#brush-models--placedbrushmodel-and-brushmodelgeometry) and
`solid`, and **a model nobody has answered for is left out** — this port has classes for
6,302 of the game's 11,635 brush entities, and among the rest are 2,383
`func_portal_bumper`s, none of which is solid to a player.

Three details inside it are load-bearing:

- **The world is traced first and the ray is then shortened to the hit**, so a door behind
  a wall costs a rejected descent rather than a full sweep. The shortening recomputes the
  end and *subtracts* to get the delta rather than scaling it — Valve's comment says
  scaling "would miss intersections we would get by feeding these results back in to the
  tracer".
- **The fractions come back rescaled onto the original ray.** Inside the loop they are
  fractions of the shortened one.
- **A trace that starts inside the world never looks at an entity** — "inside world, no
  need to check being inside anything else".

`ClipTraceToTrace`'s merge is not "take the smaller fraction": a trace that started inside
something has a fraction of 1 and matters anyway, and when *both* started solid the
surviving `start`/`fraction_left_solid` is the pair from whichever left solid **later**.

The chain is walked linearly — Valve's spatial partition replaced by nothing, deliberately.
A Portal 2 map has a few hundred brush entities (78 on `sp_a1_intro1`), each rejected by a
bounding-box test at the top of its own BSP descent, and §5 of the portdoc already records
that `parry`'s `Qbvh` is where a broadphase comes from when one is needed.

### `BrushModel` — a door, a platform, a piston

`CM_TransformedBoxTrace` (`engine/cmodel.cpp:3253`) is the whole of stage 2, and
`ClipRayToBSP` (`engine/enginetrace.cpp:1203`) is nothing but a call to it: move the ray
into the model's frame, run the ordinary sweep against the model's *own* head node, turn
the normal back out.

A `.bsp` stores model 0 (the world) and models 1.. (the brush entities), each with its own
subtree of the one shared BSP. **Where a brush model is does not come from the model
lump** — `Model::origin` is "for sounds and lights, not a render transform". It comes from
the entity that names it: `"model" "*12"` with an `"origin"` and an `"angles"`.
`World::brush_models` is that resolution done once at load, as
`PlacedBrushModel { classname, index, model }`.

`brush_model` resolves the index once so the trace does not have to. The placement is
**mutable**, through `BrushModel::set_placement( origin, angles )`, and that is the one
change `src/server/` stage 3 needed from this module: a door has a velocity now, and this
is where the result of integrating it lands. The two fields it writes are the only ones
`model_to_world` and `local_ray` read, so the drawn door and the collided door cannot be
in different places. Note that it re-evaluates `CM_TransformedBoxTrace`'s `rotated` flag
every call rather than keeping it: a door that starts unrotated and swings is rotated from
its first tick onwards, and a stale flag would trace it as though it never turned.

**Placements, not policy.** `World::brush_models` holds *every* entity naming a `"*N"`
model, triggers included. A `trigger_multiple`'s brushes are `CONTENTS_SOLID` in the file
and not solid in the game; what makes the difference is `FSOLID_TRIGGER` on the entity,
which `server/` stage 4 now sets — so `PlacedBrushModel::solid` is finally the whole
answer for a class the game knows, and `owned` says whether it knows one. **Do not filter
on `classname`**: that was the stopgap, and `clip_models` is the rule now.

`build` is infallible because [`Bsp::parse`](#worldbsp) has already checked every
cross-lump reference the trace walks — that check is what buys the right to index without
bounds tests in the inner loop, and it is why the collision lumps are validated in
`world/bsp.rs` rather than here.

`Tracer` is Valve's `BeginTrace`/`EndTrace` pair (`engine/cmodel.cpp:66`, `:111`)
expressed as a borrow. Those existed to hand out one of a pool of `TraceInfo_t`s and to
push/pop a depth counter for the one re-entrant case; here the scratch is owned by the
`Tracer` and re-entrancy is a second `Tracer`, which the borrow checker enforces for free.

`trace` takes `&mut self` because it stamps a visited-brush table — a brush belongs to
every leaf it touches, so without deduplication a wall spanning eight leaves is clipped
eight times, which is wasted work *and* wrong for `fraction_left_solid`.

`point_contents` and `leaf` take `&self`: neither needs the scratch, because a point is in
exactly one leaf and there is nothing to deduplicate.

### Displacements — the terrain

There is **no API for these**. A displacement is not something a caller names: it is part
of the world, `trace` finds it the way it finds a brush, and the only visible difference
is that `Trace::disp_flags` comes back non-zero. `CollisionBsp::disp_count` and
`disp_bounds` exist for reporting and for the depot test, not for tracing.

What is behind that, because it explains every gotcha below: a displacement replaces one
**four-sided world face** with a `(2^power + 1)²` grid of vertices, each pushed off the
flat quad along its own direction, triangulated two per cell and indexed by an AABB
quadtree. Portal 2 ships 1,181 of them across 29 of its 106 maps — 904 at power 2 (32
triangles), 202 at power 3 (128) and 75 at power 4 (512).

Three things follow from "a surface, not a volume":

- **Every test is one-sided.** A ray travelling *along* a triangle's normal is rejected
  before anything else happens, and a sweep the same way with `DIST_EPSILON` of slack.
  Terrain is solid from the front and transparent from the back — walk under a hillside
  and nothing stops you coming back out through it.
- **There is no `fraction_left_solid` and no plane set.** The sweep builds its Minkowski
  sum out of 3 axis planes, 9 edge-cross planes and the face plane — the separating-axis
  theorem with a direction of travel — rather than clipping against sides the way a brush
  does.
- **"Am I inside terrain" is a box-versus-triangle overlap test**, and a *point* has no
  box, so a point inside terrain is reported as not solid. See gotcha 16.

Two per-displacement switches are live in Portal 2 and are easy to miss because they are
smuggled through a field called `minTess`:

| Switch | What it hides the patch from | Portal 2 |
|---|---|---|
| `SURF_NOHULL_COLL` | swept boxes | 51 patches |
| `SURF_NORAY_COLL` | rays | 44 patches |
| contents without `MASK_OPAQUE` | rays | 51 patches (`WINDOW \| TRANSLUCENT`) |

### Where the data comes from

**`world/bsp.rs` reads the collision lumps; `trace/` derives from them.** Valve has two
`.bsp` readers (`modelloader.cpp` and `cmodel_bsp.cpp`) because rendering and collision
lived in code that could not see each other's allocations; one crate has no such excuse.
`Bsp` gained `planes`, `nodes`, `leaves`, `leaf_brushes`, `brushes` and `brush_sides`
(lumps 1, 5, 10, 17, 18, 19) at stage 1, and `disp_info`, `disp_verts` and `disp_tris`
(lumps 26, 33, 48) at stage 3 — all `Pod` struct arrays read by the same bounds-checked
reader as everything else. What `trace/` builds is the part that is *derived*: the surface
table, the box brushes, the contents summary, each displacement's vertex grid and AABB
tree, and the per-leaf displacement lists.

Four format notes:

- **`LUMP_LEAFS` has two versions and only the directory says which.** Version 0 carries
  a `CompressedLightCube` inline at 56 bytes a leaf; version 1 is 32. Portal 2 ships
  version 1, and a version 0 map is refused with `BspError::UnsupportedLeafVersion` rather
  than read at the wrong stride — which would produce a plausible tree of nonsense.
- **`dnode_t` and `dleaf_t` carry an explicit `_pad: u16`.** Their fields sum to 30 bytes
  at 4-byte alignment, so the compiler that wrote the file put two bytes there.
  `bytemuck::Pod` refuses a type with *implicit* padding, so naming it is what proves the
  stride is 32. `collision_lump_strides_match_the_file` asserts all eight.
- **Nothing in the file says how big a displacement is.** `LUMP_DISP_VERTS` and
  `LUMP_DISP_TRIS` are one run per displacement with no lengths; both counts follow from
  `DispInfo::power`, so a power outside 2..=4 does not overrun *that* record, it silently
  slides every later one's slice. `Bsp::validate` refuses it. Confirmed against the depot:
  every shipped displacement's `disp_vert_start` and `disp_tri_start` are exactly the
  running totals implied by the powers before it.
- **`ddispinfo_t::minTess` is a flags field, not a tessellation level** — the top bit is
  set on all 1,181 shipped displacements, and the rest carries `SURF_NOPHYSICS_COLL`,
  `SURF_NOHULL_COLL` and `SURF_NORAY_COLL`. `DispInfo::disp_flags()` decodes it. Reading
  it as a number gives 0x80000000 and no flags at all.

### Invariants and gotchas (trace)

Ordered by how likely each is to bite.

1. **`Ray`'s start is the centre of the box; `Trace`'s is not.** A Portal 2 player hull is
   72 units tall, so the two differ by 36. Everything on `Trace` is already in the
   caller's frame — do not add the offset back yourself, and do not feed `Trace::end`
   into something expecting a box centre. A player who ends up a hull-height above the
   floor is this.

2. **`fraction` stops `DIST_EPSILON` — 1/32 unit — short of the surface, deliberately.**
   `public/coordsize.h:35`. It is not a tolerance to tune: it is why a player does not
   fuse to a wall, why `TryPlayerMove`'s clip-and-retry finds room to move, and why stair
   stepping terminates. The same epsilon overlaps the two halves of a node split, so a
   brush sitting on a node plane is reached from both sides rather than missed by both.
   If a number here disagrees with the C++ by about 0.03, this is why.

3. **`Trace::start` is not the ray's start when the trace began in solid.** It is where
   the sweep *left* solid — `start + delta * fraction_left_solid`. For a sweep that never
   left, `compute_trace_endpoints` forces `all_solid`, `fraction = 0` and `end == start`.

4. **`fraction_left_solid` is meaningful for rays only.** Computing it for a box sweep
   needs, in Valve's words, "*a lot* more computation", so a hull trace gets zero and its
   `start` is the ray's origin (`CEngineTrace::TraceRay`, `engine/enginetrace.cpp:2958`).
   Reading it after a hull sweep is reading a value nothing produced.

5. **A leaf's `contents` is what its *volume* is made of, not the OR of its brush list.**
   An empty leaf beside a wall references that wall in `leaf_brushes` while its own
   contents stay 0. Conflating them makes every position test in open air report
   `all_solid`, because the unswept path reads leaf contents to decide whether the box is
   buried in the void outside the map. This cost a test failure while stage 1 was being
   written, and it is why the test fixtures set leaf contents explicitly.

6. **`surface_flags` is per *material*, not per side.** Valve ORs every texinfo's flags
   into the one surface entry its texdata shares, under a comment reading `HACKHACK: Copy
   this over for the whole material!!!` (`engine/cmodel_bsp.cpp:381`). Ported as written:
   a surface can therefore report a flag set by a different face using the same material.

7. **Bevel planes are skipped by rays and used by hulls.** VBSP emits redundant planes so
   that "push the plane out by `|normal · extents|`" is *exact* for an AABB. A brush's
   plane set is therefore not its hull — it is its hull plus the planes that make box
   sweeps correct. Clipping a ray against them reports impacts on planes that are not
   surfaces.

8. **`point_contents` returns a solid leaf's own contents without testing brushes**, on
   `cluster < 0` — not on the leaf having no brushes. The two look equivalent and are not:
   an *empty* leaf with an empty brush list would then report the leaf's contents rather
   than nothing.

9. **The box-brush and plane-brush paths differ in one place Valve did not make
   symmetric.** In the all-solid case the plane path sets `fraction_left_solid = 1` and
   the box path does not, which changes what `compute_trace_endpoints` reports as `start`
   for a sweep that begins inside an axial brush. Ported as written; inventing symmetry
   would be a silent divergence rather than a fix.

10. **Most brushes in a Source map are box brushes** — 1,010 of `sp_a1_intro1`'s 1,681 —
    so the slab test is the path most traces take, not a rarely-hit optimization. The two
    paths are asserted to agree in
    `a_box_brush_answers_the_same_as_its_six_planes`.

11. **A brush model's `normal` comes back in world space; its `plane_dist` does not.**
    `CM_TransformedBoxTrace` rotates `plane.normal` out of the model's frame and says
    nothing about `plane.dist`, so the distance describes a plane in the *model's* frame
    and pairs with the world-space normal to describe nothing at all. Ported as written —
    every consumer reads the normal and none reads the distance — and guarded by
    `a_brush_models_plane_dist_stays_in_its_own_frame` so that nobody "fixes" it by
    accident.

12. **A swept box is not rotated into a brush model's frame.** Valve copies `m_Extents`
    across untouched, so a hull meeting a door turned 45° sweeps a box that is axis
    aligned in *the door's* space rather than the world's. For the square-in-`x`/`y` hulls
    Source actually sweeps the difference is small, and it is the behaviour every line of
    movement code was tuned against. It also means the obvious symmetry test — rotate the
    model and the query together, expect the answer to rotate — holds for a **ray** and
    not for a hull.

13. **`trace` and `trace_model` are separate questions and neither includes the other.**
    A door is not in the world's subtree, so `trace` sweeps straight through it; model 0
    *is* the world, so `trace_model` with it is the world trace field for field. Combining
    them is the caller's job until stage 4.

14. **A ray stops *on* a displacement and `DIST_EPSILON` short of a brush.** The
    displacement ray path is `IntersectRayWithTriangle`, which has no epsilon pullback,
    where `clip_box_to_brush` subtracts one. A **hull** sweep stops short of both, because
    the sweep path resolves its planes through `ResolveRayPlaneIntersect`, which does have
    it. So an impact point on terrain and one on a wall are not directly comparable, and
    a ray that ends exactly at the surface is the degenerate case rather than the normal
    one.

15. **Terrain is one-sided.** A ray or sweep travelling along a triangle's normal is
    rejected outright, so a query that starts under a hillside passes straight out
    through it. The normal that decides this is `(v2 - v0) × (v1 - v0)` — the reverse of
    the obvious order — built from a base quad whose own normal is `(p3 - p0) × (p1 -
    p0)`, likewise reversed. Both point out of the terrain, measured on the depot 60 to 8
    against the leaves either side of a base face. Wind a displacement the other way and
    you get terrain you fall through.

16. **A *point* inside terrain is reported as not solid, and that is Valve's answer.**
    `CM_TestInDispTree`'s box-versus-triangle test is what decides "inside", and a point
    has no box; what is left is the **stab**, which fires along the surface's own outward
    normal — the one direction gotcha 15 says nothing can be hit in — so `CM_PostStab`
    takes its clearing branch. A *box* inside terrain is correctly `all_solid`. Pinned by
    `a_point_inside_terrain_is_reported_as_not_solid` so that nobody "fixes" the stab
    without meaning to. `portdocs/ENGINE_TRACE.md` §4.8 has the full reading.

17. **`disp_flags` is cleared when a brush beats a displacement, and in Valve it is
    not.** This is the module's **one deliberate divergence**. `dispFlags` is written in
    two places (`dispcoll_common.cpp:696`, `:1416`) and cleared in none, so in the
    original a nearer brush keeps the displacement's flags and `IsDispSurface()` calls a
    wall terrain — measured at 45 of 2,362 depot traces. `m_bDispHit`, cleared on the
    adjacent line, is Valve's own evidence the pairing was intended. **Delete the two
    `work.trace.disp_flags = 0;` lines in `brush.rs` to get Valve's behaviour back**;
    `a_brush_hit_after_a_displacement_does_not_report_terrain` is what fails when you do.

18. **Two per-displacement switches hide a patch from half the queries**, and Portal 2
    uses both — see the table under "Displacements" above. A patch with
    `SURF_NOHULL_COLL` is decoration; making it solid puts invisible walls in the ruins.

### Three more, from stage 4's clip chain

19. **`Tracer::trace` is world-only *until* someone calls `with_entities`.** Every caller
    written before stage 4 still gets exactly what it got, because
    `CollisionBsp::tracer()` hands out an empty chain. If a door is not stopping the
    player, the missing call is at the `Tracer` construction site, not in the sweep.

20. **A trigger must not be in the chain, and the thing that keeps it out is
    `FSOLID_NOT_SOLID`** — set by `CBaseTrigger::InitTrigger`, carried across as
    `Placement::solid`, filtered by `World::clip_models`. Break any link in that and every
    trigger in the game becomes an invisible wall, which is a bug you walk into rather
    than one you read.

21. **`brush_models_touching` and `clip_models` disagree on purpose.** The first reports
    triggers and the second excludes them; the first is asked "what is the player
    overlapping" and the second "what stops the player". They share the `owned` filter and
    nothing else.

### One place this is stricter than Valve

`IsBoxBrush` (`engine/cmodel_bsp.cpp:667`) checks only that a six-sided brush's planes
have axial *types*, then asserts in `ExtractBoxBrush` that each normal component is
exactly ±1 — an assert compiled out of release builds, leaving a box brush with
uninitialised bounds. `extract_box` checks the normals and that all six faces are
distinct, and keeps the brush on the plane path if either fails. Valid maps take the same
path either way.

### Not implemented, and what each waits on

| Missing | Waits on |
|---|---|
| Brush entities in the clip chain, `ClipTraceToTrace`, the fraction rescaling | **done** — stage 4, above |
| A trace *filter* (`ITraceFilter`, `TRACE_WORLD_ONLY`, collision groups) | nothing needs one: the caller chooses the chain, which is the same decision one step earlier |
| A broadphase | nothing yet — a linear scan over a few hundred models. `parry`'s `Qbvh` at stage 5 |
| Static props and `.phy`/vcollide | stage 5, where `parry` enters |
| Displacement *rendering* | done — `world/disp/` |
| `LUMP_PHYSDISP`, `CM_CreateDispPhysCollide` | `vphysics/` — the displacement's *physics* mesh, not its trace |
| Displacement multiblend (`LUMP_DISP_MULTIBLEND`) | nothing — no Portal 2 displacement sets `DISP_INFO_FLAG_HAS_MULTIBLEND` |
| PVS, areas, areaportals | `world/`'s visibility work, not this module's |
| `surfaceProps`, hitboxes, occlusion queries | `vphysics/`, `.mdl`, and never |

### The `trace` command

`trace` fires a ray from the player's eye along the view; `trace hull` sweeps the player
hull from the feet. Both print the fraction, distance, endpoint, normal, surface, contents
and solid flags, then — stage 3 — whether what was hit was terrain and whether VBSP
compiled it as walkable, then the contents and leaf at the eye, a ground probe with
`CategorizePosition`'s 0.7 standable test (and the same walkable contrast), and — stage
2 — the same ray against every brush model the map places, reporting the nearest by
classname and model index.

**This port's own, not Valve's** — the C++ equivalents (`debugrayenable`, the trace
counter) exist to work around a DLL boundary this build does not have. It is stages 1
and 2's acceptance test: it asks the one question the module exists to answer using only a
console, a player and a view, all of which already existed.

Since `server/` stage 3 the brush-model pass **skips anything the game says is not
solid**, which today is a switched-off `func_brush` and nothing else. It is still not the
whole solidity question — `FSOLID_TRIGGER` and the entity clip chain are stage 4's — so
the classname is still printed and the judgement is still left to the reader.

The brush-model pass asks every model at full length, where `CEngineTrace::TraceRay`
shortens the ray to the world hit first and lets the spatial partition pick candidates
(`enginetrace.cpp:2870`). Both of those are stage 4's and neither exists; asking all of
them is the same answer more slowly, because `ClipTraceToTrace` keeps the minimum
fraction and enumeration order is not observable.

On `sp_a1_intro1` it reports the spawn standing on `MOTEL/HOTEL_CARPET001` with a
`(0, 0, 1)` normal (8.97 units above it at the spawn instant, 0.00 once the player has
fallen), and a `TOOLS/TOOLSPLAYERCLIP` brush 127 units ahead with contents `0x8010000`
(`PLAYERCLIP | DETAIL`) and surface flags `0x480` (`NODRAW | NOLIGHT`). The map places
**78 brush models**, and the nearest along that same ray is `*38 "func_illusionary"` at
284 units on `LIGHTS/WHITE001` — which is also the caveat in one line: a
`func_illusionary` is not solid in the shipped game, and nothing here knows that yet.

### Test coverage (trace)

41 tests, none of which need a map, a GPU or a window — the fixtures build a
`CollisionBsp` through `CollisionBsp::build` from a hand-written `Bsp`, so the box
extraction, the displacement build and the surface table are under test too — plus two
depot-gated tests that need a Portal 2 install.

| Test | Guards |
|---|---|
| `a_ray_stops_dist_epsilon_short_of_the_surface` | gotcha 2, and the normal and contents |
| `a_box_brush_answers_the_same_as_its_six_planes` | gotcha 10 — three rays including a diagonal |
| `a_hull_sweep_stops_a_half_width_early` | the plane offset, and gotcha 1 |
| `a_hull_reports_no_fraction_left_solid` | gotcha 4 |
| `a_ray_starting_inside_reports_where_it_left` | gotcha 3 |
| `a_ray_that_never_leaves_a_brush_is_all_solid` | the `all_solid` branch |
| `a_mask_that_excludes_the_brush_hits_nothing` | contents filtering |
| `playerclip_stops_a_player_and_not_a_shot` | `MASK_PLAYERSOLID` vs `MASK_SOLID` |
| `a_zero_length_sweep_is_a_position_test` | the unswept path, and gotcha 5 |
| `the_tree_descent_keeps_the_nearer_hit` | the descent and the split, both directions |
| `a_downward_hull_sweep_finds_the_floor_normal` | `CategorizePosition`'s question |
| `bevel_planes_bind_a_hull_and_are_invisible_to_a_ray` | gotcha 7 |
| `point_contents_and_leaf_lookup` | gotcha 8 |
| `a_map_with_no_brushes_traces_as_a_clean_miss` | the empty-model early-out |
| `a_tracer_gives_the_same_answer_twice` | the visit stamps not leaking between traces |

Stage 2, all on a fixture whose world subtree and model subtree hold *different* brushes,
so a trace against the wrong head node is caught rather than merely suspected:

| Test | Guards |
|---|---|
| `a_brush_model_is_traced_against_its_own_subtree` | the head node — the one thing stage 2 is |
| `an_unrotated_brush_model_moves_with_its_origin` | the cheap branch, at three offsets |
| `model_zero_is_the_world` | gotcha 13, field for field |
| `a_model_the_map_does_not_have_is_none` | the index resolution |
| `a_rotated_brush_model_reports_a_world_space_normal` | the rotate-back, and the wall having moved |
| `rotating_the_model_and_the_query_together_rotates_the_answer` | `VectorITransform`'s transposes and translation sign |
| `a_rotated_model_keeps_the_hulls_centring` | the `- offset` re-application — 36 units if dropped |
| `a_hull_is_not_rotated_into_the_models_frame` | gotcha 12, with an oblong box that makes it visible |
| `a_brush_models_plane_dist_stays_in_its_own_frame` | gotcha 11 |
| `a_position_test_against_a_brush_model_finds_it` | the unswept path reaching the model's head node |

Stage 3, on a fixture that builds a real displacement — base face, vertex grid, triangle
tags and all — over a quad wound so its normals point `+Z`:

| Test | Guards |
|---|---|
| `a_displacement_is_built_and_reachable_from_its_leaf` | the build, and the per-leaf lists |
| `a_ray_stops_on_a_displacement` | gotcha 14's ray half, the normal, the contents, `disp_flags` and the surface name |
| `a_hull_stops_a_hair_above_a_displacement` | gotcha 14's hull half, and `CategorizePosition`'s test on terrain |
| `a_displacement_is_transparent_from_behind` | gotcha 15, for both a ray and a hull |
| `a_displaced_vertex_raises_the_surface_under_it` | the bilinear grid and the offsets — a ramp, sampled at three points |
| `the_start_position_rotates_the_grid` | `FindSurfPointStartIndex`/`AdjustSurfPointData`, which have no geometric tell |
| `the_collision_flags_hide_a_displacement_from_one_kind_of_query` | gotcha 18, both flags both ways |
| `a_ray_passes_through_a_displacement_that_blocks_a_hull` | the `MASK_OPAQUE` condition on rays |
| `a_mask_that_excludes_the_displacement_hits_nothing` | contents filtering |
| `a_box_straddling_a_displacement_is_all_solid` | the box-versus-triangle position test |
| `a_box_above_a_displacement_is_not_solid` | the stab's clearing branch, the case it gets right |
| `a_point_inside_terrain_is_reported_as_not_solid` | gotcha 16 |
| `a_brush_and_a_displacement_compete_on_distance` | the nearer of the two wins, in both orders |
| `a_brush_hit_after_a_displacement_does_not_report_terrain` | gotcha 17 — **confirmed to fail without the fix** |
| `a_tracer_gives_the_same_displacement_answer_twice` | the displacement visit stamps not leaking between traces |

Plus `collision_lump_strides_match_the_file`, `displacement_sizes_follow_from_the_power`,
`min_tess_decodes_as_flags_only_when_the_top_bit_is_set` and
`leaf_area_and_flags_unpack` in
`world::bsp`, and `valves_angle_order_is_yaw_then_pitch_then_roll` /
`the_transpose_undoes_the_rotation` in `crate::math` — see
[the root-module note in `rustdocs/README.md`](README.md#root-modules).

**`every_shipped_map_traces_its_brush_models`** is `--ignored` and gated on
`KISAK_GAME_DIR`, like the `studio/` and `world/props/` depot tests:

```text
KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_brush_models -- --ignored --nocapture
```

It loads all 106 shipped maps, resolves their brush entities, and sweeps each model with a
ray down the middle of its own box — then **requires the hit to land inside that box,
carried out to world space through the placement**. That is the assertion that earns the
runtime: a wrong head node reports another model's geometry, a dropped origin reports it
in the wrong place, and an inverted rotation reports it turned the wrong way, and all
three land outside. Both of the last two were confirmed to fail the test before it was
trusted.

Measured: **11,635 brush models across 106 maps, 5,115 rotated, 10,550 off the origin, 39
classnames** — `func_brush` (2,502), `func_portal_bumper` (2,383), `trigger_once` (1,476),
`trigger_multiple` (899) lead. 9,372 of the 11,635 centre rays hit; the rest are models
whose geometry does not span their own bounding-box centre, which an L-shaped or hollow
brush entity does not.

**`every_shipped_map_traces_its_displacements`** is the same shape for stage 3:

```text
KISAK_GAME_DIR=/path/to/portal2 cargo test --release shipped_map_displacements -- --ignored --nocapture
```

It builds every map's collision model, requires **every** displacement to build and to be
named by at least one leaf, and then fires two traces at each: one down the middle of its
own box, and one **head-on along the base face's normal — derived from the `.bsp`'s own
face lump rather than asked of the module**, so the check is against the file rather than
against itself. A hit attributed to terrain has to land inside a displacement's bounds.

Measured: **1,181 displacements across 29 of 106 maps, all 1,181 built**, 14,190 leaf
references, 904 at power 2 / 202 at 3 / 75 at 4. Of the 1,106 that a ray may hit at all,
**897 are hit head-on inside their own box**, 27 hit a neighbouring patch first (the ray
starts as far out as the vertex offsets can reach, which on a deep patch is far enough to
cross another), and 182 meet a brush on the way. If the winding convention were inverted,
that first number would be zero — which is what the test asserts on.

Stage 4's clip chain is covered by three unit tests here
(`the_clip_chain_keeps_the_nearest_of_the_world_and_the_entities`,
`a_tracer_with_no_entities_is_a_world_trace`,
`a_trace_that_starts_in_the_world_ignores_the_chain`) and, end to end against all 106
shipped maps, by `server::tests::every_shipped_maps_triggers_notice_the_player` — which
walks a real player hull into **every one of the game's 2,255 live triggers** through the
same sweep. See `rustdocs/SERVER.md`.

## `src/engine/input/`

Buttons, the event queue, and where the view points. Replaces `inputsystem/` (10,649
lines), `engine/keys.cpp` (1,392) and `sys_mainwind.cpp`'s `DispatchInputEvent`.
[`portdocs/ENGINE_INPUT.md`](../portdocs/ENGINE_INPUT.md) is the plan; **stages 1-4 of its
five are done** — translation, button state, mouse look, bindings and UI precedence. Only
controllers (stage 5, `gilrs`) are left.

**The movement layer is no longer here.** `game/client/in_*.cpp`'s view angles,
`kbutton_t`s and the free-fly camera lived in `input::view` as a placeholder until
`src/client/` existed; they are now [`rustdocs/CLIENT.md`](CLIENT.md)'s, where the
`CUserCmd` that gives them meaning is. What crosses the boundary is a `+command` in the
command buffer and two floats of mouse delta — **this module names no client type and the
client names no input type**.

| | |
|---|---|
| Module | `crate::engine::input`, with `input::bind` and `input::button` |
| Lines | ~1,500 including tests |
| Tests | 58 (`cargo test engine::input`), plus 5 for the `winit` table (`engine::window::translate`) |
| Dependencies | `std` and `glam`. **Not `winit`, and not `egui`** — see [the seam](#the-seam-window-translates-input-decides) |

### Quick start

```rust
use crate::engine::input::{Button, Event, Input, Key};

let mut input = Input::new();

// From `window/`, as events arrive, between ticks:
input.push(Event::Pressed { button: Button::Key(Key::W), repeat: false });
input.push(Event::MouseMotion { dx: 4.0, dy: -1.0 });

// From `Engine::frame`, once per tick:
let (dx, dy) = input.frame();          // dispatches the queue, sums the motion
for event in input.events() { /* … */ }
if input.is_down(Button::Key(Key::W)) { /* … */ }
```

### `Button`, `Key`, `MouseButton`

```rust
pub enum Button { Key(Key), Mouse(MouseButton) }
pub enum MouseButton { Left, Right, Middle, Mouse4, Mouse5, WheelUp, WheelDown }
pub enum Key { Num0, …, A, …, Pad0, …, Escape, …, F12 }   // 103 variants

impl Button {
    pub const COUNT: usize;                                // 110
    pub fn index(self) -> usize;                           // dense, 0..COUNT
    pub fn from_index(index: usize) -> Option<Button>;
    pub fn name(self) -> &'static str;                     // "w", "MOUSE1", "MWHEELUP"
    pub fn from_name(name: &str) -> Option<Button>;        // case-insensitive
    pub fn all() -> impl Iterator<Item = Button>;
}
```

`Key::COUNT` is 103 and `MouseButton::COUNT` is 7; both also have `index` and `name`.
The flat dense space is the one thing kept from `ButtonCode_t` — it is what lets stage
3's binding table be an array and the down-state be a bitset, and why a controller button
will bind to `+forward` with no special case. The macro arithmetic
(`JOYSTICK_BUTTON( joy, button )`) and `JOYSTICK_AXIS_BUTTON` are not kept; `gilrs`
reports axes as axes.

**The names are external content**, transcribed verbatim from `s_pButtonCodeName`
(`key_translation.cpp:357`), because `bind "w" "+forward"` lives in shipped `.cfg` files
and `scripts/kb_def.lst`. Divergences from Valve's table, all in `button.rs`'s module
docs: `KEY_NONE` is `Option::None`; the three `KEY_*TOGGLE` pseudo-keys are gone (vgui
toggle *state*, not keys — Valve's own table asks what they are for); and `LWIN`/`RWIN`
keep those names on every platform instead of both becoming `"COMMAND"` on macOS, which
could not round-trip. `"COMMAND"` is still accepted by `from_name`, as it is on Valve's
non-OSX path.

### `Event`

```rust
pub enum Event {
    Pressed { button: Button, repeat: bool },
    Released(Button),
    Text(char),
    MouseMotion { dx: f32, dy: f32 },   // raw, look only
    CursorMoved { x: f32, y: f32 },     // absolute, UI only
    Wheel(f32),                         // notches, positive away from the user
    FocusLost,
    FocusGained,
}
```

`InputEvent_t` minus the three-events-per-keypress it was built on. Valve posted
`IE_ButtonPressed` (carrying a scan code *and* a virtual code), `IE_KeyCodeTyped` and
`IE_KeyTyped` for one key press, then spent a hundred lines undoing SDL's double
reporting of the same fact; `winit`'s `KeyEvent` carries all three facts in one struct.
`FocusGained` is not in Valve's set — this port needs it because X11 delivers raw motion
whether or not the window is focused.

### `Consumer` — who an event was given to

```rust
pub enum Consumer { Ui, Game }
```

`KeyUpTarget_t` (`engine/keys.cpp:41`), collapsed from five targets to two. Valve asked
tools, VGui, RocketUI/Scaleform, GameUI and the client in turn; under `egui` there is one
UI, so the chain is one answer. `window/` decides it per event, at the moment `egui` saw
the event, and it travels into the queue with it.

**`None` in the latch is `KEY_UP_ANYTARGET`, which is not `Consumer::Game`**: it means
nothing claimed the press. See the latch below.

### `Input`

```rust
impl Input {
    pub fn new() -> Input;
    pub fn push(&mut self, event: Event);                          // == push_from(_, Game)
    pub fn push_from(&mut self, event: Event, consumer: Consumer); // between ticks, from window/
    pub fn frame(&mut self) -> (f32, f32);          // once a tick; returns summed motion
    pub fn events(&self) -> &[Event];               // this tick's, for the *game*
    pub fn is_down(&self, button: Button) -> bool;
    pub fn mouse_look(&self) -> bool;
    pub fn set_mouse_look(&mut self, on: bool);
    pub fn clear(&mut self);                        // ClearStates
}
```

`push`/`frame` is `PostEvent`/`DispatchAllStoredGameMessages`, and the split is what
makes the sampling rule structural rather than remembered. `frame` is also
`GetAccumulatedMouseDeltasAndResetAccumulators`: it is the **single** point at which the
motion accumulator resets, and a second one would silently halve the turn rate.

`events()` returns **only what the game gets**. Everything the UI claimed is gone, which
is what stops typing `w` in the console from walking the camera; `is_down` is unaffected,
because `m_bKeyDown` is set before the chain runs (`keys.cpp:1288`) and records what is
physically held.

### The key-up latch

`FilterKey` (`engine/keys.cpp:1189`), and the one algorithm in `keys.cpp` that has to
survive intact. `Input::frame` records which target consumed a **press**, per button, and
delivers the matching **release to that target and no other** — whatever anyone wants by
the time it arrives. Valve's comment is the rule: *"It is illegal to trap up key events.
The system will do it for us."*

The failure it prevents, and the reason stage 4 is a correctness fix rather than polish:

```
bind mouse1 +attack ; click in game ; open the console ; let go
```

Without the latch the console eats the release, `-attack` never runs, and the player
fires forever. Every stuck-key bug in a Source-like engine is this invariant violated.
So: **a release reaches the game unless the press that matched it was taken by the UI —
not unless the UI wants it now.** `Input::clear` resets the latch with the down-state,
because a claim that outlived the window it was made in would misroute the next release.

### The view angles and the camera are gone from here

`ViewAngles`, `KButton`, `MoveButtons` and `FlyCamera` were `input::view`, and are now
`crate::client` — see [`rustdocs/CLIENT.md`](CLIENT.md). `FlyCamera` in particular no
longer exists in any form: a real `Player` in `MOVETYPE_NOCLIP`, moved by
`FullNoClipMove` from a `UserCmd`, replaced it rather than being renamed into it.

The two facts that used to live here and still matter to anyone reading `input/`:

- **The mouse delta this module returns is raw device units**, summed since the last
  tick. Scaling by `sensitivity` and turning it into angles is the client's
  (`ScaleMouse`/`ApplyMouse`), and the client is also what decides whether `+strafe`
  makes it movement instead.
- **Whether the delta should be applied at all is the engine's question**, not this
  module's: `Engine::wants_mouse_capture` (which includes "the console is not up") gates
  it, and `Input::mouse_look` alone is the wrong term — see the gotcha in
  `Engine::update_client`.

### The seam: `window/` translates, `input/` decides

| Stage | Where | What |
|---|---|---|
| `WindowEvent` → `Event` | `window/translate.rs` + `window_event` | a lookup and one `match` arm each |
| `DeviceEvent::MouseMotion` → `Event` | `window/`'s `device_event` | the only source of look input |
| queue | `Engine::push_input` → `Input::push` | between ticks; nothing acts on it |
| dispatch | `Engine::frame` → `Input::frame` | once a tick, after the host agrees |
| policy | `Engine::update_view`, `mouse_look_after` | Escape frees the cursor, a click takes it |
| the grab | `window/`'s `apply_capture`/`warp_cursor` | `mouse_look && focused` |

`input/` names no `winit` type on purpose. That is what makes the guard, the accumulator
and the angle math testable without a window, and it is what leaves room for `gilrs`,
which is *polled* rather than pushed — stage 5 drains it into the same queue before
`Input::frame` runs.

### Invariants and gotchas (input)

Ordered by how likely each is to bite.

1. **View look comes from `DeviceEvent::MouseMotion`, never from `CursorMoved`.**
   `CursorMoved` is clamped to the window and quantised to pixels, so a view driven from
   it stalls at the screen edge — the classic "cannot turn past 180°" bug. `CursorMoved`
   is translated and queued, and only `egui` reads it — through its own `winit`
   integration, not through this queue.
2. **Input is sampled at the frame rate, not the event rate.** The queue is drained
   inside `Engine::frame`, *after* `Host::frame` agrees a frame is happening, so a frame
   `fps_max` refuses samples nothing and **a lower `fps_max` is a higher input latency**.
   That is faithful. It is also why mouse motion accumulates as a **sum**: applying it
   per event would make turn speed depend on event rate, and keeping only the last delta
   would discard motion on every refused frame. `m_flAccumulatedMouseXMovement` is the
   field this is, and it looks like sampling cruft that could be dropped. It cannot.
3. **Bindings are by physical key.** `Key::W` is the key *where* W is on a US layout,
   which is where Z is printed on AZERTY. Valve's POSIX path collapsed scancode and
   virtual code, so `bind w +forward` there binds the key labelled W and WASD stops being
   a square. Consequence to document for users: a `bind` listing can name a key whose
   keycap says something else. Text entry and key *display* will use the logical key;
   neither exists yet.
4. **Raw motion is not equally raw.** X11 (XI2) and Wayland
   (`zwp_relative_pointer_v1`) deliver unaccelerated device deltas; **macOS delivers
   `NSEvent.deltaX`, already through the OS ballistics curve**. The same `sensitivity`
   therefore feels different on macOS, and that is recorded rather than corrected —
   Valve answered the same problem with convars, not by inverting the curve.
5. **Escape gets the cursor back and a click takes it again — but only when the console
   is closed.** With the console up, both events are the UI's and never reach
   `Input::events`: Escape closes the dialog inside `egui`, and a click is the dialog's.
   The cursor is instead given back by `Engine::wants_mouse_capture`, which is
   `mouse_look && !console_open`. That is deliberately a **separate term** rather than a
   write to `mouse_look`, so closing the console restores whatever the game had rather
   than deciding for it. If either half is ever "simplified", a grabbed window becomes
   one the user cannot leave.
6. **Neither cursor grab mode works on both platforms.** `CursorGrabMode::Locked` is
   unimplemented on X11, `Confined` is unimplemented on macOS
   (`winit-0.30.13/src/window.rs:1682`), and which applies is a *runtime* property of the
   session — X11-versus-Wayland is not a compile-time fact. `window/` tries `Locked`,
   falls back to `Confined` plus a per-frame warp to the window centre (which is what
   `CInput::ResetMouse` was), and if both fail says so once and leaves the cursor visible.
   **Re-check this on any `winit` upgrade.**
7. **The redundant-transition guard is load-bearing.** `Input::frame` drops any press or
   release that does not change the down-state (`keys.cpp:1284`). Valve needed it because
   several paths reported the same transition; this port needs it because `winit` emits
   **synthetic key events on focus change** to report keys already held. With the guard
   they cost nothing; without it they double-count, and a `+attack` sent twice is stopped
   once.
8. **Losing focus releases every held button, but does not surrender the mouse.**
   `Input::clear` is `CInput::ClearStates` — alt-tabbing with `+forward` held and coming
   back to a player who walked into a wall for thirty seconds is the failure it prevents.
   `mouse_look` deliberately survives, because the grab is suspended by `window/`
   (`mouse_look && focused`) rather than given up; otherwise coming back from an alt-tab
   would leave the cursor loose in a game that thinks it has it.
9. **Motion is dropped at `push` when the mouse is not driving the view**, rather than
   accumulated and ignored later. X11's raw events arrive from the *device*, so an
   alt-tabbed window would otherwise spin the view while the user works elsewhere, and
   the accumulator would deliver one enormous delta on the frame the grab returns.
10. **`repeat` is passed through, not filtered.** The console wants auto-repeat and
    bindings must not have it (`kbutton_t`'s `KeyDown` returns early on one,
    `in_main.cpp:434`), so the consumer decides. `Text` reaches this queue only when the
    UI did not claim it; the console gets its own text from `egui_winit`, built from the
    same `KeyEvent`, so the two paths never both see a character.
12. **`egui` sees every event that is not bypassed, whoever ends up owning it.** The
    latch decides what the *game* sees, not what `egui` sees — those are two different
    questions here, where Valve's one chain answered both at once. A consequence: after
    clicking in the world and opening the console, `egui` receives a mouse release for a
    press it never got. That is harmless (it just clears pointer state), and it is the
    price of the console being able to answer "did you want this" from its own state.
13. **View look is gated on `Engine::wants_mouse_capture`, not on `Input::mouse_look`.**
    They differ by exactly one term — the console being up — and using the wrong one is a
    bug you see rather than one you read: `DeviceEvent::MouseMotion` arrives from the
    *device* whether or not the cursor is grabbed, so moving the mouse to click in the
    console would spin the view underneath it. Discarding the tick's delta rather than
    suppressing it at `push` is safe, because `Input::frame` resets the accumulator every
    tick and nothing piles up to arrive in one lump when the console closes.
14. **A movement key held when the console opens stays held, and the camera keeps
    moving.** That is faithful — Source does the same, because the press already went to
    the game and only its *release* is latched — and the release, when it comes, still
    reaches the game and stops it. It is listed here because it looks like a stuck-key
    bug and is the opposite: it is the latch working.
15. **The key bound to `toggleconsole` never reaches the UI at all.** `Key_Event`
    bypasses the whole VGui chain for a `KEY_BACKQUOTE` press (`keys.cpp:1319`), because
    otherwise the key that opens the console cannot close it and types a backquote into
    the entry on the way. `Bindings::bypasses_ui` generalises that from the backquote to
    whatever is bound to the command, and `window/` skips `egui` for both edges — so
    `egui` never has a half-open key state to go with a release it will not see either.
11. **Wheel notches are accumulated before becoming button presses.** A mouse reports
    lines and a trackpad reports pixels, continuously; `MWHEELUP`/`MWHEELDOWN` are
    discrete. `window/`'s `PIXELS_PER_NOTCH` (50, a chosen constant — Valve never saw a
    pixel delta) is the threshold, and the fractional remainder is kept, so a slow swipe
    still eventually clicks.

### `Bindings` and `CommandSink`

```rust
pub trait CommandSink { fn enqueue(&mut self, command: &str); }

pub struct Bindings;
pub fn bind(&mut self, button: Button, command: &str);
pub fn unbind(&mut self, button: Button) -> bool;   // false: Escape is refused
pub fn unbind_all(&mut self);
pub fn get(&self, button: Button) -> Option<&str>;
pub fn iter(&self) -> impl Iterator<Item = (Button, &str)>;
pub fn find(&self, command: &str) -> impl Iterator<Item = Button> + '_;
pub fn count(&self) -> usize;                       // Key_CountBindings
pub fn write(&self, out: &mut String);              // Key_WriteBindings
pub fn dispatch(&self, button: Button, down: bool, modifier_down: bool,
                sink: &mut dyn CommandSink) -> bool;

// on Input:
pub fn bindings(&self) -> &Bindings;
pub fn bindings_mut(&mut self) -> &mut Bindings;
pub fn dispatch_bindings(&self, sink: &mut dyn CommandSink);
```

`Key_SetBinding` (`keys.cpp:117`) and `Key_Event`'s dispatch tail (`:1130`).
`CommandSink` is the seam: **`input/` names no console type and `console/` names no input
type**, so `impl CommandSink for Console` lives in `src/engine/mod.rs`, which already owns
both. `Console::enqueue` with `Source::UserInput` is the whole implementation.

**The `+`/`-` convention is asymmetric.** A binding starting with `+` sends
`+forward <index>` on press and `-forward <index>` on release; **any other binding fires
on press only**, because `bind F5 jpeg` must not take two screenshots.

**The index argument is load-bearing**, not decoration: `KButton` records up to *two*
holders, so two keys bound to `+forward` do not cancel each other. Valve says it outright
— "*Button commands include the kenum as a parameter, so multiple downs can be matched
with ups*" (`keys.cpp:1132`). What consumes it is [`Buttons`](CLIENT.md).

Three of Valve's special cases are kept and each one is a usability guarantee rather than
a quirk:

- `bind ESCAPE <anything>` stores `cancelselect` regardless (`keys.cpp:310`). There must
  always be a way out of a menu — and in this port Escape is currently the only way to
  release the captured cursor.
- `unbind ESCAPE` is refused (`keys.cpp:183`).
- `unbindall` **spares Escape and the backquote** (`keys.cpp:199`). `config_default.cfg`
  opens with `unbindall`, so without the exceptions exec'ing it would take away the menu
  key and the console key at once, with no way to get either back.

And one more: `toggleconsole` is **swallowed while a shift, control or alt is held**
(`keys.cpp:1170`), so a chord passing through the console key does not open it.

`bind_osx` is not a curiosity — `config_default.cfg` ships `bind_osx "z" "+zoom"` and
macOS is a supported target. It is `bind` gated on `cfg!(target_os = "macos")`.

### Not implemented, and what each waits on

| Missing | Waits on |
|---|---|
| Controllers, hot-plug, analog axes | `gilrs` — stage 5. `in_joystick.cpp`'s response curves and deadzones are content-tuned client behavior and come with `client/`, not with the device layer. |
| `CUserCmd`, `kbutton_t`, the fractional `KeyState` model, `CreateMove`, prediction | **Done, in [`crate::client`](CLIENT.md)** — not here. `input/` produces `+forward <index>` text and a raw mouse delta; everything downstream of that is the game client's. |
| `unbindalljoystick`, `unbindallmousekeyboard`, `Key_SetBinding`'s splitscreen joystick remap | controllers (stage 5) and co-op. |
| The guard refusing every binding except `toggleconsole` while not connected (`keys.cpp:1139`) | `client/` — it needs `engineClient->IsConnected()`. |
| `m_customaccel`, `cl_mouselook_roll_compensation`, and the rest of the mouse-shaping cvars | The client's, not this module's — [`CLIENT.md`](CLIENT.md) records which are dropped and which are deferred. |
| `Key_StartTrapMode` ("press a key to bind it") | An options UI. ~35 lines, trivially re-added. |
| Split-screen: per-player down-state | Co-op being scheduled. The binding table was global even in the original, so the cost is deferred as long as nothing bakes a player slot into `Event` or `Button` — nothing does. |
| IME, cursor icons, `IInputStackSystem`, X360/PS3/TrackIR/Novint hardware | Deleted, not deferred. See `portdocs/ENGINE_INPUT.md` §5. |

### Test coverage (input)

| Test | Guards |
|---|---|
| `every_name_round_trips`, `no_two_buttons_share_a_name` | the external name format — a name that does not survive `bind` is a binding that vanishes from a `.cfg` |
| `names_are_in_button_code_order`, `indices_are_dense_and_round_trip` | that the name table and the discriminant have not drifted apart |
| `every_key_has_exactly_one_position` | that no `Key` is missing a `winit` code, which would be a key that silently does nothing |
| `a_transition_that_changes_nothing_is_dropped` | gotcha #7, the guard |
| `motion_accumulates_across_refused_frames` | gotcha #2, the accumulator |
| `motion_is_dropped_while_the_mouse_is_not_driving_the_view`, `motion_while_unfocused_never_reaches_the_view` | gotcha #9 |
| `losing_focus_releases_everything_held`, `losing_focus_does_not_give_up_the_mouse` | gotcha #8, both halves |
| `escape_gives_the_cursor_back_and_a_click_takes_it_again` | gotcha #5 |
| `a_zero_angle_looks_down_positive_x`, `positive_pitch_looks_down` | `AngleVectors`' signs — "right" is `-Y` facing `+X`, and pitch is positive downwards |
| `moving_the_mouse_right_turns_right`, `pitch_clamps_at_the_poles` | `ApplyMouse` and `ClampAngles` |
| `the_wish_velocity_is_clamped_to_the_server_maximum`, `walking_halves_the_speed` | `FullNoClipMove`'s arithmetic, including that the clamp uses the *unhalved* factor |
| `a_plus_binding_sends_both_edges_with_the_button_index`, `a_plain_binding_fires_on_the_way_down_only` | the `+`/`-` convention, both halves of its asymmetry |
| `escape_always_binds_to_cancelselect`, `escape_cannot_be_unbound_and_unbindall_spares_the_console_key` | the three Valve special cases that guarantee a way out |
| `toggleconsole_is_swallowed_under_a_modifier`, `a_modifier_held_swallows_toggleconsole` | `keys.cpp:1170`, at both layers |
| `auto_repeat_never_reaches_the_binding` | that the transition guard already covers `KeyDown`'s repeat check |
| `focus_loss_releases_the_keys_and_is_still_reported` | that clearing the key down-state is *not* enough — the command holds the button, so the event has to reach the tick for the engine to clear the client's too |
| `a_bound_key_moves_the_camera_through_the_command_buffer` | the whole chain, `bind` → press → command text → console → `Buttons` → `UserCmd`, with nothing mocked |
| `a_release_goes_to_whoever_took_the_press` | **the latch, and the bug it exists for**: click, open the console, let go, and `-attack` still runs |
| `a_press_the_ui_took_never_reaches_the_game` | the other direction — clicking in the console does not send a bare `-attack` on the way out |
| `the_down_state_records_what_is_held_whoever_took_it` | that `m_bKeyDown` is set before the chain runs |
| `text_and_wheel_the_ui_took_are_dropped` | that typing `w` in the console does not walk the player |
| `losing_focus_forgets_who_was_owed_a_release` | a claim outliving the window it was made in |
| `only_the_key_bound_to_toggleconsole_bypasses_the_ui` | `Bindings::bypasses_ui`, gotcha #15 |
| `the_console_key_opens_the_dialog_through_the_command_buffer` | stage 4 end to end: binding → command text → console → dialog, and closing again |

The cursor grab, the `winit` event arms and `device_event` need a window and cannot be
tested here; they are verified by running the binary.

## `src/engine/console/`

Cvars, commands, the buffer that turns typed or scripted text into them, and the output
they print to. Replaces `tier1/convar.cpp` + `tier1/commandbuffer.cpp` (the objects and
the queue), `vstdlib/cvar.cpp` (the registry), `engine/cmd.cpp` + `engine/cvar.cpp` (the
policy) and the print half of `engine/console.cpp`. The design is
[`portdocs/ENGINE_CONSOLE.md`](../portdocs/ENGINE_CONSOLE.md), and **all five stages of
its §8 have landed** — stage 2 being bindings, which is the same work as `input/` stage 3
and is documented [there](#bindings-and-commandsink), stage 3 being
[config persistence](#config-persistence), stage 4 the
[`egui` dialog](#consoleui--the-dialog), and stage 5
[the list commands](#the-list-commands).

| | |
|---|---|
| Module | `crate::engine::console`, with `console::{buffer, cvar, log, token, ui}` and the private `console::describe` |
| Lines | ~5,690 including tests |
| Tests | 104 (`cargo test engine::console`), 8 of them the dialog's |
| Dependencies | **`std`, plus `egui` in `console::ui` alone** — no `wgpu`, no `winit`, no `crate::filesystem` |

**It names no engine type and no filesystem type**, which is what lets it be constructed,
driven and asserted on with no window, no GPU and no mount. Two traits buy that:
[`CommandTarget`](#commandtarget-and-execcontext) for commands it does not own, and
[`ConfigFiles`](#configfiles) for the files `exec` reads.

`console::ui` is the one submodule with a dependency, and it is `egui` and nothing else —
no `winit`, no `wgpu`, no engine type. That keeps the property, rather than spending it:
a headless `egui::Context` is a complete `egui`, so the dialog is driven and asserted on
in unit tests exactly as the rest of the module is.

### Quick start

```rust
use crate::engine::console::{Console, CommandSpec, CvarFlags, Source, NoTarget};

let mut console = Console::new(Box::new(VfsConfigFiles(vfs)), cmdline.args().to_vec());

// Registration hands back a handle. Keep the handle, not a way to look one up.
let fps_max = console.cvar("fps_max", "300", CvarFlags::NONE, "Frame rate limiter.");
console.register_command(CommandSpec::new("map", "Load a map."))?;

console.enqueue("exec valve.rc", Source::Code);
console.run(&mut target);        // once per frame; one run is one tick

if fps_max.changed(&mut generation) { clock.set_fps_max(fps_max.float()); }
```

### `Cvar` — a handle, not a lookup

```rust
pub struct Cvar(Arc<CvarCell>);          // Clone, Send, Sync

pub fn name(&self) -> &str;
pub fn help(&self) -> &str;
pub fn default_value(&self) -> &str;
pub fn flags(&self) -> CvarFlags;
pub fn bounds(&self) -> (Option<f32>, Option<f32>);

pub fn float(&self) -> f32;              // atomic load
pub fn int(&self) -> i32;
pub fn bool(&self) -> bool;              // GetInt() != 0
pub fn string(&self) -> Arc<str>;

pub fn set_string(&self, value: &str);   // InternalSetValue
pub fn set_float(&self, value: f32);
pub fn set_int(&self, value: i32);
pub fn set_bool(&self, value: bool);
pub fn revert(&self);                    // back to default_value()

pub fn generation(&self) -> u32;         // bumped on every change
pub fn changed(&self, last: &mut u32) -> bool;
```

**This is the headline decision and it reverses `portdocs/ENGINE.md` §7.4.** That document
called the cvar registry "the one piece of ambient global state that is genuinely
process-global"; `ENGINE_CONSOLE.md` §6.1 reverses it. What is shared is each cvar's
*value*, not the registry, so a subsystem holds the one cvar it reads and reading it is an
atomic load through its own handle — no lock, no hash probe, **no `&Console` in the
reader's signature**, callable from any thread. The registry is left serving name lookup
for exactly one caller, the dispatcher.

Two consequences worth knowing:

- **`FCVAR_MATERIAL_SYSTEM_THREAD` has nothing to solve.** Its whole purpose was
  `CCvar::QueueMaterialThreadSetValue` (`vstdlib/cvar.cpp:774`), a deferred-write queue for
  a cvar read off-thread. Deleted, with `FCVAR_ACCESSIBLE_FROM_THREADS`.
- **`generation` replaces change callbacks.** `FnChangeCallback_t` is not ported: a
  callback that must touch `&mut` engine state cannot be owned by a registry the engine
  owns. `fps_max` is the worked example — it had a real callback
  (`engine/sys_engine.cpp:78`) and is now a poll in `Engine::frame`.

**Keep the invariant: callers hold `Cvar`, never `&CvarCell`.** §9 open question 1 records
that this is what keeps the fallback design (a console-owned registry with index handles)
a mechanical change rather than a rewrite of every caller.

### `CvarFlags` and `CommandFlags`

Only the six flags §4.6 marks "Keep" exist: `DEVELOPMENTONLY`, `HIDDEN`, `ARCHIVE`,
`NEVER_AS_STRING`, `CHEAT`, `SPONLY`. The untrusted-source set (`REPLICATED`,
`SERVER_CAN_EXECUTE`, `USERINFO`, …) is deliberately **absent rather than
present-and-ignored** — those are a security model, and a flag that exists but is never
checked reads as though it were enforced.

**The bit values are ours** and are packed densely from zero, not copied from
`public/tier1/iconvar.h`. Nothing in shipped content spells a flag numerically, so only
the meanings are fixed.

They are **two types on purpose**. In the original, bit 10 is `FCVAR_PRINTABLEONLY` on a
`ConVar` and `FCVAR_GAMEDLL_FOR_REMOTE_CLIENTS` on a `ConCommand` — one bit meaning two
things depending on what holds it, which is exactly what a faithful transliteration
reproduces by accident. Separate types make the collision unrepresentable.

### `Console`

```rust
pub struct Console<'a>;                  // 'a is the mounted content's

pub fn new(files: Box<dyn ConfigFiles + 'a>, command_line: Vec<String>) -> Console<'a>;
pub fn detached() -> Console<'static>;   // no files, no command line; for tests

pub fn cvar(&mut self, name: &str, default: &str, flags: CvarFlags, help: &str) -> Cvar;
pub fn cvar_bounded(&mut self, name: &str, default: &str, flags: CvarFlags, help: &str,
                    min: Option<f32>, max: Option<f32>) -> Cvar;
pub fn try_cvar(..) -> Result<Cvar, RegisterError>;         // as above, recoverable
pub fn try_cvar_bounded(..) -> Result<Cvar, RegisterError>;
pub fn register_command(&mut self, spec: CommandSpec) -> Result<(), RegisterError>;

pub fn find_cvar(&self, name: &str) -> Option<&Cvar>;       // case-insensitive
pub fn find_command(&self, name: &str) -> Option<&CommandSpec>;
pub fn cvars(&self) -> &CvarRegistry;
pub fn commands(&self) -> impl Iterator<Item = &CommandSpec>;
pub fn log(&self) -> &Log;
pub fn log_mut(&mut self) -> &mut Log;
pub fn buffer(&self) -> &CommandBuffer;
pub fn can_cheat(&self) -> bool;                            // sv_cheats

pub fn enqueue(&mut self, text: &str, source: Source);      // Cbuf_AddText
pub fn run(&mut self, target: &mut dyn CommandTarget);      // Cbuf_Execute
pub fn take_unknown_count(&mut self) -> u32;
```

`cvar`/`cvar_bounded` panic on a duplicate name; `try_cvar`/`try_cvar_bounded` return it.
**A duplicate is a bug**, not a runtime condition: `vstdlib/cvar.cpp:361-450` linked a
same-named newcomer as a *child* of the incumbent so that `sv_cheats`, declared separately
in `engine`, `client.so` and `server.so`, resolved to one value. One binary, one
declaration — that machinery is the single largest deletion in the module, and takes
`ConVarRef`, `CVarDLLIdentifier_t` and `IConCommandBaseAccessor` with it.

### Dispatch order

`Cmd_ExecuteCommand` (`engine/cmd.cpp:929`) minus the deleted steps:

```
alias -> command -> cvar -> unknown
```

- **Alias before command**, so an alias shadows a command of the same name. An alias is
  *text substitution re-entering the whole of dispatch*, not a call, so it can expand to
  further aliases.
- **A cvar set never reaches the target.** `fps_max 60` needs nothing but the cvar;
  `map sp_a1_intro1` needs `&mut Host`. That split is the reason `CommandTarget` exists.
- **Only registered names reach the target.** An unregistered name falls through to the
  cvar step and then to "unknown", so a target's `Dispatch::Unknown` means "I registered
  this and then did not handle it", which is a bug in the target.

Deleted from the original: execution markers (`CMDSTR_ADD_EXECUTION_MARKER`, which serves
`ClientCmd_Unrestricted`), `FCVAR_GAMEDLL` forwarding to the server, and the
forward-to-server fallback. All three return with `net/` and `client/`.

### `CommandTarget` and `ExecContext`

```rust
pub trait CommandTarget {
    fn execute(&mut self, cmd: &Command, cx: &mut ExecContext<'_>) -> Dispatch;
}
pub enum Dispatch { Handled, Unknown }
pub struct NoTarget;                     // handles nothing

impl ExecContext<'_> {
    pub fn enqueue(&mut self, text: &str);                  // with the command's source
    pub fn enqueue_delayed(&mut self, text: &str, ticks: i32);
    pub fn print(&mut self, text: &str);
    pub fn warn(&mut self, text: &str);
    pub fn error(&mut self, text: &str);
    pub fn developer_print(&mut self, level: i32, text: &str);
    pub fn source(&self) -> Source;
}
```

**A command is not a callback.** `ConCommand` holds an `FnCommandCallback_t` — a bare
function pointer reaching its state through globals. There are no globals here, and a
closure capturing `&mut Engine` cannot be stored in a registry `Engine` owns. So commands
are declared as data ([`CommandSpec`]) and executed by whoever owns the state.

`Console` is a field of `Engine`, so `self.console.run(&mut self)` cannot compile. The
target is **a struct of disjoint field borrows**, constructed per call — the same move
`host.frame(&mut self.scene)` already makes. In `src/engine/mod.rs` that is
`EngineCommands { host }`, and it grows a field per subsystem that gains commands. That is
a genuine improvement on the C++, where the answer to "what state may a command touch" was
"all of it".

`ExecContext` exists for re-entrancy: a command that queues more text writes into field
borrows of the console the dispatcher is already inside.

### `ConfigFiles`

```rust
pub trait ConfigFiles {
    fn read_config(&self, path: &str, path_id: Option<&str>) -> Option<Vec<u8>>;
    fn config_exists(&self, path: &str, path_id: Option<&str>) -> bool;      // defaulted
    fn write_config(&self, path: &str, contents: &str) -> Result<(), String>; // defaulted
    fn list_files(&self, dir: &str, ext: &str) -> Vec<String>;                // defaulted
}
pub struct NoConfigFiles;                // reads nothing, writes nothing
```

Reading and writing are **not symmetric**, which is why they are separate methods and why
only one takes a path ID: a read searches every mount in order, and there is exactly one
place a write can go (`Vfs::write_root`, the mod directory). That asymmetry is also why
`DEFAULT_WRITE_PATH` was not ported as a search path — see
[`FILESYSTEM.md`](FILESYSTEM.md).

The seam that keeps `console/` off `crate::filesystem`. `path` arrives assembled
(`cfg/valve.rc`); `path_id` is `exec`'s optional second argument, which Valve spells as the
`//<pathid>/` prefix and defaults to `*` (any mount). `VfsConfigFiles` in
`src/engine/mod.rs` is the real implementation; tests use an in-memory map.

`list_files` is the completion half — `CBaseAutoCompleteFileList::AutoCompletionFunc`
(`engine/baseautocompletefilelist.cpp:23`), which walks `<subdir>/*.<ext>` and chops four
characters off each name to remove the extension (which is why every extension Valve
completes happens to be three letters long). It returns names **with the extension
stripped**, merged across every mount, and defaults to nothing — so a console with no
content mounted completes commands and cvars and simply offers no filenames.

> **This resolves a contradiction in the plan.** `ENGINE_CONSOLE.md` §0.1 asks for a module
> testable "without a mounted filesystem" while §3 and §4.5 have `exec` reading
> `cfg/*.cfg`. Both hold only if the read goes through a trait, so it does.

### Config persistence

```rust
// free function -- both callers hold different things
pub fn write_archived_cvars(cvars: &CvarRegistry, out: &mut String);

// on Console:
pub fn config_exists(&self, path: &str, path_id: Option<&str>) -> bool;
pub fn config_was_read(&self) -> bool;
pub fn set_config_was_read(&mut self, read: bool);
pub fn write_config_file(&self, path: &str, contents: &str) -> Result<(), String>;

// on ExecContext, for a command that persists state:
pub fn cvars(&self) -> &CvarRegistry;
pub fn config_was_read(&self) -> bool;
pub fn write_config(&mut self, path: &str, contents: &str) -> Result<(), String>;
```

`Host_WriteConfiguration` (`engine/host.cpp:1559`) is **engine policy that pulls from two
modules** — `Key_WriteBindings` from `keys.cpp` and `WriteVariables` from `cvar.cpp` — so
the composition lives in `src/engine/mod.rs` as `build_configuration`, exactly where
`host.cpp` put it. `Engine::write_configuration` and the `host_writeconfig` command are
its two callers.

The file is `unbindall`, then every binding, then every `FCVAR_ARCHIVE` cvar as
`<name> "<value>"`:

```
unbindall
bind "w" "+forward"
bind "MOUSE1" "+attack"
sensitivity "6"
```

- **`unbindall` first is what makes the file idempotent** — reading it back throws away
  what was bound rather than merging. It is also why
  [`unbind_all` spares Escape and the backquote](#bindings-and-commandsink): this file is
  exec'd at startup, and without those exceptions reading your own config would take away
  the menu key and the console key.
- **Cvars are sorted case-insensitively by name** (`CVarSortFunc`, `cvar.cpp:629`). Not
  cosmetic: the file is rewritten on every clean exit, and an unstable order would churn
  against version control and against any diff a user takes.
- **The value is quoted**, which is what makes `strip_set_value` the reader — a cvar
  holding spaces survives the round trip.
- **The format is fixed** (§7). We write it *and* read it, but a user's existing
  `config.cfg` was written by the shipped engine, and one we write must stay readable by
  it.

**Two guards, both load-bearing, neither an optimization:**

1. **`config_was_read`** (`Host_WasConfigCfgExecuted`, `:1587`). Nothing may be written
   until startup's config exec has been through the buffer. Without it, a crash between
   startup and that exec overwrites a real user's settings with defaults. `Engine` sets it
   after the first `Console::run`, which is where `Host_Init` calls `Cbuf_Execute` and then
   `Host_SetConfigCfgExecuted` (`:2092`).
2. **`Bindings::count() <= 1`** (`:1603`). A session that somehow bound nothing must not
   persist that over a real config.

**Startup** (`Engine::boot`, `host.cpp:2058`) prefers `//mod/cfg/config.cfg` and falls
back to `config_default.cfg`, setting `save_config` so the user gets a real config written
on that first launch. Valve checks `//usrlocal/` first; that is a console-era per-user path
this port has no equivalent for.

`execifexists` (`cmd.cpp:798`) is `exec` with `bOnlyIfExists` — silent about a missing
file, where `exec` complains.

<a id="the-list-commands"></a>

### The list commands

Six commands, all of them console **built-ins** rather than the target's, for the same
reason `exec` is: they need the registry and the log and nothing else. Nothing here is new
public API — they are registered in `register_builtins` and run in `run_builtin`, and the
only new module is the private `console::describe`, which holds the three formatters they
share.

| Command | Original | Does |
|---|---|---|
| `cvarlist [log <file>] [partial]` | `engine/cvar.cpp:952` | every cvar and command, one per line, optionally also as a CSV |
| `help <name>` | `:1109` | one description |
| `find <string> [<string>...]` | `vstdlib/cvar.cpp:1052` | substring search over names **and** help text |
| `differences` | `engine/cvar.cpp:1139` | every cvar not on its declared default |
| `toggle <cvar> [values...]` | `:1161` | flip a flag, or cycle a list |
| `incrementvar <cvar> <min> <max> <delta>` | `engine/host_cmd.cpp:2638` | step a number, wrapping at both ends |

**`DEVELOPMENTONLY` and `HIDDEN` are filtered in one place** — `Console::listable` — rather
than at each call site, because every listing does it and it is the one filter that must
not be forgotten: a listing is precisely what those two flags exist to hide from. `help` is
deliberately *not* a caller, and that is the difference between the two flags: `HIDDEN`
means not discoverable, not unusable.

Things worth knowing before touching any of them:

- **`describe::value`, never `Cvar::string`, wherever a value is compared or displayed.**
  The two differ for exactly the `FCVAR_NEVER_AS_STRING` cvars, and there the string is a
  stale copy that no set ever updates — so `differences` would report every one of them as
  unchanged for ever, and `toggle` could never find one in its value list. Valve lands on
  the other side of the same split: `ConVar::GetString` returns the *literal string*
  `"FCVAR_NEVER_AS_STRING"` (`public/tier1/convar.h:620`), so its `differences` lists every
  one of them, always. `describe::is_at_default` is the matching comparison, and it is the
  same predicate that decides whether a description carries its `( def. "…" )` clause — one
  function, so a cvar can never be listed as differing and then shown without the clause
  saying how.
- **`incrementvar` sets through the command buffer rather than writing the cvar.** Valve
  explains this as avoiding "any problems with state in a demo loop": what a recording then
  contains is the set, not the increment. Kept, and it costs nothing — an insert during
  processing goes to the head. It also means everything dispatch does still applies, which
  is why `incrementvar` on a cheat cvar is refused by the cheat gate rather than by
  `incrementvar` itself.
- **`cvarlist` prints a *number* in its value column, even for a string cvar.** `PrintCvar`
  only ever formats `GetInt()`/`GetFloat()`, so `con_filter_text` reads `0`. That is the
  shipped engine's output, not a port bug; `help con_filter_text` shows the string.
- **`toggle` compares case-sensitively** (`Q_strcmp`), and it looks for the *current*
  value, so a cvar sitting on something not in the list starts the cycle at the beginning.
- **`cvarlist test_` will not list `+test_forward`.** The prefix filter compares from the
  first character, `+` included — Valve's does too. The *sort* is the one that ignores
  leading `+`/`-` (`ConCommandBaseLessFunc`), so the two halves of a button appear together.

Deliberate divergences, with what reverses each:

| Divergence | Why |
|---|---|
| `find` takes any number of terms; **every** one must match, each against the name or the help | Valve's takes one, the reference tree two (a Kisak addition) while printing a usage line promising `[<string>...]`. Taking as many as are given is less code than either. |
| `differences` is **sorted** | Valve walks its hash table in whatever order it is in; a `HashMap` here is seeded per process, so unsorted means a different order every launch. |
| `cvarlist` fills the flag column **for commands too** | Valve leaves it empty while its own `help` prints a command's flags — an oversight, and seeing which commands are cheat-gated is the point of the listing. |
| `cvarlist log` applies `exec`'s **extension blocklist** to the log path | The path comes from the same places `exec`'s does. `Vfs::write_path` already confines it to the write root; nothing legitimately logs cvars to a `.dll`. |
| The CSV has no stray empty column | `PrintListHeader` emits one, because `csvflagstr` already ends in a comma and the format string adds another. The rows carry the same extra comma so the bug is invisible — but nothing reads this file back. |
| One flag table, not three | `g_ConVarFlags` spells the same six flags twice and `g_PrintConVarFlags` a third time, **listing a different subset**. `describe::FLAGS` is one table with a long name and a short one; the union of the subsets is kept, so `help` on a hidden cvar says it is hidden. |
| A description with no help text has no trailing padding | `%-80s` pads regardless; nothing follows it. |

`findflags` is **not** ported: it searches the twenty-two flags this port does not have, and
`find` covers what is left of it. Neither are `multvar` (`incrementvar` with a different
operator and no caller), `reset_gameconvars` or `getcvars` (`server/`, and the Steam
overlay).

### `CommandBuffer` and the tick model

```rust
pub fn add_text(&mut self, text: &str, source: Source, tick_delay: i32) -> bool;
pub fn begin_processing(&mut self, delta_ticks: i32);
pub fn dequeue(&mut self) -> Option<Command>;
pub fn end_processing(&mut self);
pub fn delay_all(&mut self, delay: i32);
pub fn set_wait_enabled(&mut self, enabled: bool);
pub fn take_overflow(&mut self) -> bool;
pub fn clear(&mut self);
```

**One `Console::run` is one tick.** That is the spoof `engine/cmd.cpp:288` performs by
passing 1 to `BeginProcessingCommands` every time, and it is what makes `wait 1` mean
"next frame". The shipped `.cfg` files assume it, so it is kept — and it is why `run` is
called *inside* `Engine::frame` rather than per window event.

### Completion

```rust
pub struct Suggestion {
    pub text: String,          // what replaces the input line
    pub value: Option<String>, // a cvar's current value; None for a command
}

impl Console<'_> {
    pub fn complete(&self, partial: &str) -> Vec<Suggestion>;
}
```

`CConsolePanel::RebuildCompletionList` (`consoledialog.cpp:510`), moved out of the widget
because it is a question about the registry rather than about a dialog. The rules, all of
which are Valve's:

- **Empty input returns nothing** — empty input lists *history*, and history belongs to
  [`ConsoleUi`](#consoleui--the-dialog).
- **Input containing a space** first asks whether the command named by the first token
  completes its own arguments (`Completion::Files`/`Values` on its `CommandSpec`).
  `exec ` listing `cfg/*.cfg` and `map ` listing `maps/*.bsp` are that path, and the
  suggestion is the **whole line** (`exec config_default`), which is what
  `AutoCompletionFunc` builds.
- **Otherwise prefix match**; and if there *was* a space and no command claimed it, fall
  back to space-separated substring matching (`CommandMatchesText`, `:451`), so
  `draw wire` finds `mat_wireframe`. Non-obvious and pleasant.
- `DEVELOPMENTONLY` and `HIDDEN` are excluded from both the command and the cvar list.
- Sorted by name, capped at 64 (`COMMAND_COMPLETION_MAXITEMS`).

**Completion is data on the `CommandSpec`, not a callback.**
`FnCommandCompletionCallback` reached the filesystem through a global; there is none here,
so `Completion::Files { dir, ext }` names what to list and
[`ConfigFiles::list_files`](#configfiles) does the listing.

Two divergences worth knowing: the argument list is **sorted and deduplicated** where
Valve's is in filesystem order (two mounts can serve the same `cfg/*.cfg`, and `Vfs::list`
merges rather than deduplicating); and a cvar's displayed value uses an integer format for
an integral `FCVAR_NEVER_AS_STRING` number rather than `1.000000`, which is Valve's own
rule at `:594`.

<a id="consoleui--the-dialog"></a>

### `ConsoleUi` — the dialog

```rust
pub struct ConsoleUi;
impl ConsoleUi {
    pub fn new() -> ConsoleUi;
    pub fn is_open(&self) -> bool;
    pub fn set_open(&mut self, open: bool);   // showconsole / hideconsole
    pub fn toggle(&mut self);                 // toggleconsole
    pub fn draw(&mut self, ctx: &egui::Context, console: &mut Console<'_>);
    pub fn input(&self) -> &str;
    pub fn history(&self) -> &[String];
    pub fn completion(&self) -> &[Suggestion];
}
```

`vgui2/vgui_controls/consoledialog.cpp` (1,371 lines) plus its game-side wrappers. **The
widget tree does not survive; three algorithms do**: `RebuildCompletionList` (above),
`AddToHistory` (`:1075`) and `OnAutoComplete` (`:648`).

Owned by `Engine`, not by `Console` — the scrollback is the console's (output exists
whether or not anything displays it), the dialog's own state is the engine's, and keeping
them apart is what makes `Engine::run_ui`'s borrow two disjoint fields.

Keys, all of them Valve's:

| Key | Does |
|---|---|
| the key bound to `toggleconsole` | opens and closes; **never reaches `egui`** (gotcha #15 in [input](#invariants-and-gotchas-input)) |
| Enter, or Submit | runs the line as `Source::UserInput`, echoes it as `] <line>`, adds it to history |
| Tab / ↓ | next completion; Shift-Tab / ↑ the previous. Both wrap |
| Escape | closes the dialog. Taken with `consume_key` *before* the entry is built, because a focused `TextEdit` surrenders focus on Escape and the dialog would stay up with nothing focused |

Three behaviours that look like details and are the design:

1. **Empty input cycles history, and that is why there is no separate history key.**
   `RebuildCompletionList` fills the completion list from history when the entry is empty,
   so ↑ on an empty line is already the most recent command. History is offered **oldest
   first**, which looks backwards and is not: `OnAutoComplete`'s reverse case starts at the
   *end* of the list, so "up from nothing" lands on the newest and the behaviour falls out
   of the ordering rather than being special-cased.
2. **Cycling must not rebuild the list.** The entry text changes while cycling, and a naive
   "rebuild when the text changed" narrows the list to one item and stops. `ConsoleUi`
   keeps what the *user* typed (`m_szPartialText`) separately from what it last wrote
   (`m_bAutoCompleteMode` under another name), and only a user edit rebuilds.
3. **A completion gets a trailing space unless it already contains one** (`:715`), so
   `mat_wireframe ` is ready for a value while `exec valve.rc` is ready to run.

Deliberately not here: the **notify area** (`CConPanel`, the fading lines at the top of
the screen — a HUD element), `m_bStatusVersion`'s one-line layout (the dedicated server's),
and `DumpConsoleTextToFile`, which arrives with `con_logfile`.

### Invariants and gotchas (console)

Ordered by how likely each is to bite.

1. **There are two splitters and they disagree.** `buffer::split_commands` divides text
   into *commands* on `;` and newlines; `token`'s tokenizer divides one command into
   *argv* with the break set `` {}()': ``. A `;` inside quotes does **not** split a
   command, but **a newline does, even inside quotes** — Valve flags that in its own
   source as legacy (`commandbuffer.cpp:194`) and shipped configs were written against it.
   `;` is an ordinary word character to the argv tokenizer, because splitting already
   happened.
2. **An alias body must be quoted to contain a `;`.** `alias pair a; b` sets `pair` to
   `a` and runs `b` immediately, because `add_text` splits before the `alias` command
   ever sees the text. `alias pair "a; b"` is what you meant. This is Valve's behaviour
   and the reason every multi-command alias in a shipped `.cfg` is quoted.
3. **`Command::tail` is not `args().join(" ")`.** It is `ArgS()`: the raw remainder after
   argv[0], *as typed*, with quotes intact. The tokenizer strips quotes; the tail does
   not, which is the whole reason both exist — `hostname "  a b  "` keeps its interior
   spaces only because the set path reads the tail and then strips quotes in Valve's
   order (unquote, trim, unquote: `strip_set_value`). Rebuilding it from tokens loses
   exactly the information it carries. `"foo"bar` parses as two args with a tail of `bar`.
4. **Insertion during processing goes to the head, and repeated inserts keep their
   order.** Valve's `InsertImmediateCommand` links before `m_hNextCommand`, which is
   re-pointed only by `BeginProcessingCommands` and `DequeueNextCommand` — *not by the
   insert*. So a three-command alias body runs in the order written. Pushing each to the
   front instead reverses them, which is the plausible-but-wrong ordering §4.2 warns
   about; it is guarded by
   `several_immediate_inserts_keep_their_order`.
5. **`wait` is handled at insert time and the command is dropped.** It adds its delay to a
   running tick that the *rest of the same text* inherits — a scheduling primitive, not a
   sleep. It therefore never reaches dispatch.
6. **A clamped set stores the reformatted number, not the text typed.** `ClampValue` runs
   before the string is decided, so `fps_max -1` against a minimum of 0 reads back as
   `"0.000000"` (`printf("%f")`), not `"-1"`. An unclamped set keeps the text exactly,
   which is what lets a string cvar hold something non-numeric.
7. **Cvar values parse like `atof`, not like `str::parse`.** The longest numeric prefix
   wins and anything else yields 0. Strictness would be wrong: this reads shipped `.cfg`
   files where a trailing unit or comment must not turn a real value into a failure.
8. **An unknown name is counted every time and printed once.** Two pressures, one rule.
   Shipped configs name commands from subsystems that do not exist yet, so a printed error
   per *line* is a wall at every launch (§9 open question 6) — hence `Source::Code` is
   quiet and `Source::UserInput` prints. But bindings send `Source::UserInput` too, and
   `config_default.cfg` binds `+attack` to MOUSE1 and `cancelselect` to Escape, so without
   the once-per-name rule every click and every Escape would print. **Valve prints every
   time and can afford to**; it implements all of its commands. `take_unknown_count` still
   counts every occurrence.
9. **`exec` is line-at-a-time and immediate**, not "append the file to the buffer". Each
   line is drained completely before the next is read, which is why a nested `exec`
   finishes before the outer file continues (`valve.rc` depends on this) and why a bad
   line does not stop the lines after it.
10. **`autoexec.cfg`, `joystick.cfg` and `game.cfg` fail silently.** This looks like a
    hack and is exactly right: Portal 2 ships none of them and `valve.rc` execs two of
    them, so without the special case every launch prints two errors. Verified against
    the depot.
11. **`exec`'s extension check is a blocklist, not an allowlist** — see the plan
    corrections below.
12. **`Command` is owned where Valve's `CCommand` points into the buffer.** Forced, and
    Valve hit the same problem from the other side: its `memcpy` at `tier1/convar.cpp:421`
    exists "to avoid the pointers returned by `DequeueNextCommand` to become invalid by
    calling `AddText`". Dispatching can insert text, so the borrow could not survive
    dispatch. `ENGINE_CONSOLE.md` §6.4 sketches `Command<'a>`; owning it is the
    correction.
13. **`Cvar::string` is stale for an `FCVAR_NEVER_AS_STRING` cvar** — the set path does not
    maintain it. Use `describe::value` and `describe::is_at_default` for anything that
    compares or displays a value; see [the list commands](#the-list-commands).

### Two guards Valve does not have

Both are runaway protection, and they catch different shapes:

- **`MAX_QUEUED_COMMANDS` (1,024)** catches an alias that expands to *many* commands.
- **`MAX_COMMANDS_PER_ROUND` (10,000)** catches one that expands to **itself**. The queue
  cap cannot: each round removes one command and inserts one, so the length never grows
  and the loop runs forever at one. `alias x x; x` hangs the shipped engine. When the
  budget trips, the rest of the queue is dropped so the loop does not resume next frame.
- **`MAX_EXEC_DEPTH` (16)** catches a `.cfg` that execs itself, which recurses through
  Rust's stack rather than through the queue.

### Not implemented, and what each waits on

| Missing | Waits on |
|---|---|
| `findflags`, `multvar`, `reset_gameconvars`, `getcvars` | nothing — [deliberately not ported](#the-list-commands) |
| The flag-versus-source permission matrix | `net/` — the check is already a function returning "allowed" (§9 q4) |
| `con_logfile`, `Con_NPrintf`, colour cvars | later; the notify area is the HUD's |
| Splitscreen: per-target buffers, `FCVAR_SS`, `cmd1`…`cmd4` | deliberately deleted (§5) |

### Corrections to `portdocs/ENGINE_CONSOLE.md`

Found while implementing, and recorded because the plan is otherwise the reference:

- **§4.5 says non-`.cfg`/`.rc` extensions are refused. They are not.**
  `IsValidFileExtension` (`engine/cmd.cpp:438`) is a **blocklist** of `.exe`, `.vbs`,
  `.com`, `.bat`, `.dll`, `.ini`, `.gcf`, `.sys`, `.blob`. An allowlist would reject
  `valve.rc`, which is exec'd by name. The port keeps the blocklist and matches it
  case-insensitively, where Valve's `Q_strstr` is case-sensitive — deliberate, since this
  is a trust check and `FOO.EXE` should not pass one.
- **§6.4 sketches `Command<'a>` borrowing from the buffer.** It cannot; see gotcha 12.
- **§4.2's queue cap does not catch a self-recursive alias.** See "Two guards" above.
- **`CCommandLine::ParmValue` refuses a value starting with `-` or `+`**
  (`tier0/commandline.cpp:646`), and `src/cmdline.rs` did not. `stuffcmds` skips each
  `-switch` *and its value*, so without that clause `-window +map foo` has `-window`
  swallow `+map` and the map never loads. Fixed with the move, and guarded by
  `a_switch_is_never_read_as_another_switch_s_value` and
  `a_valueless_option_does_not_eat_the_next_command`.
- **One deliberate behavioural divergence:** `alias <name>` with no body **prints the
  current definition** where Valve sets an *empty* one. Valve's reading means a typo at
  the console silently shadows a command with nothing until you restart.

### Test coverage (console)

104 tests, `cargo test engine::console`.

| Test | Guards |
|---|---|
| `splits_on_semicolons_and_newlines`, `a_semicolon_inside_quotes_does_not_split`, `a_newline_splits_even_inside_quotes` | the command splitter, including Valve's legacy newline quirk |
| `comments_are_trimmed_off_the_command_not_the_line`, `a_comment_inside_quotes_is_not_a_comment` | `//` handling in the splitter |
| `a_quoted_argument_is_one_token_without_its_quotes`, `break_characters_are_their_own_tokens`, `a_semicolon_is_an_ordinary_character_here` | the argv tokenizer, and that the two splitters differ |
| `quoted_argv0_followed_immediately_by_a_word`, `tail_is_the_raw_remainder_not_the_rejoined_tokens` | `ArgS`/`m_nArgv0Size` arithmetic |
| `wait_defers_the_rest_of_the_same_text`, `wait_takes_an_explicit_count`, `wait_can_be_disabled` | `wait` scheduling |
| `insertion_during_processing_goes_to_the_head`, `several_immediate_inserts_keep_their_order` | alias ordering, both halves |
| `an_alias_shadows_a_command_of_the_same_name`, `an_alias_is_text_substitution_and_re_enters_dispatch` | dispatch order |
| `an_alias_body_is_split_on_semicolons_unless_it_is_quoted` | gotcha 2 |
| `an_alias_that_expands_to_itself_stops_the_round`, `an_alias_that_expands_without_end_overflows_the_queue` | both runaway guards |
| `a_nested_exec_completes_before_the_outer_file_continues`, `a_bad_line_does_not_stop_the_lines_after_it` | `exec` line-at-a-time |
| `the_three_optional_configs_fail_silently_and_others_do_not` | `engine/cmd.cpp:572` |
| `dangerous_extensions_are_refused`, `exec_refuses_to_recurse_without_end`, `exec_refuses_a_file_over_a_megabyte` | `exec`'s trust and resource limits |
| `the_set_path_strips_surrounding_quotes_but_keeps_interior_spaces` | `strip_set_value`'s ordering |
| `bounds_clamp_on_every_set_including_the_default` | `ClampValue`, and the reformat on clamp |
| `the_generation_counter_reports_changes` | the change-callback replacement |
| `a_duplicate_registration_is_refused` | §4.8, the deleted parent/child linkage |
| `cheat_cvars_need_sv_cheats_and_revert_when_it_goes_off` | `CHEAT` and `RevertFlaggedConVars` |
| `unknown_names_from_a_config_are_counted_quietly_and_typed_ones_are_not` | §9 q6 |
| `stuffcmds_turns_plus_arguments_into_commands`, `a_valueless_option_does_not_eat_the_next_command`, `map_takes_a_second_argument_from_the_command_line` | `stuffcmds` |
| `a_plus_argument_seeds_a_cvar_at_registration` | `GetCommandLineValue`, distinct from `stuffcmds` |
| `valve_rc_boots_a_map_through_stuffcmds` | stage 1's deliverable, end to end |
| `filter_mode_one_drops_and_mode_two_dims`, `developer_is_a_level_not_a_bool`, `the_ring_is_bounded` | the log sink |
| `only_archived_cvars_are_written_and_they_are_sorted` | `WriteVariables`' filter and `CVarSortFunc` |
| `a_written_cvar_reads_back_through_the_set_path` | that the writer's quoting and `strip_set_value` agree |
| `execifexists_is_silent_about_a_missing_file` | the difference from `exec` |
| `a_written_config_reads_back_as_the_same_bindings_and_cvars` | the whole round trip across two consoles and two binding tables |
| `writing_is_refused_until_startup_has_read_a_config`, `writing_is_refused_when_almost_nothing_is_bound` | both guards |
| `the_config_opens_with_unbindall_then_bindings_then_cvars` | the file's shape |
| `the_written_config_reads_back_as_the_same_table` | `Key_WriteBindings`' exact lines |
| `completion_prefix_matches_commands_and_cvars_and_sorts_them`, `completion_hides_developmentonly_and_hidden` | `RebuildCompletionList`'s filter and sort |
| `a_space_with_no_claiming_command_matches_substrings` | `CommandMatchesText`'s second mode — every piece must match, not any |
| `a_command_that_claims_its_arguments_completes_files`, `a_claiming_command_with_no_files_offers_nothing` | `AutoCompletionFunc`, including that a claiming command does not fall back to the name search |
| `an_empty_line_completes_to_nothing_because_history_is_the_uis` | the split between the registry's job and the dialog's |
| `the_completion_list_is_capped`, `never_as_string_cvars_show_a_number_rather_than_their_string` | `COMMAND_COMPLETION_MAXITEMS`, and `:594`'s number format |
| `a_typed_line_reaches_the_command_buffer` | the dialog end to end with no window and no GPU: type, Enter, and find it queued, echoed and in history |
| `tab_cycles_the_completions` | that cycling wraps and does **not** rebuild the list from the completed text |
| `an_empty_line_cycles_history_instead` | `RebuildCompletionList`'s empty-input rule, and that ↑ lands on the newest |
| `history_keeps_the_newest_copy_of_a_repeated_command`, `history_is_bounded` | `AddToHistory` |
| `escape_closes_the_console`, `closing_forgets_the_completion_state` | the Escape path and `CConsolePanel::Hide` |
| `cvarlist_prints_a_banner_rows_and_a_count`, `cvarlist_with_no_argument_lists_everything`, `cvarlist_help_is_a_usage_line` | `CvarList`'s shape, its prefix filter, and that `DEVELOPMENTONLY`/`HIDDEN` reach no listing |
| `cvarlist_sorts_the_two_halves_of_a_button_together` | `ConCommandBaseLessFunc` — the sort ignores a leading `+`/`-` where the filter does not |
| `cvarlist_log_writes_a_csv_beside_the_listing`, `cvarlist_log_that_cannot_be_written_prints_nothing_else`, `cvarlist_log_refuses_a_dangerous_extension` | the CSV, that a refused file aborts the command, and the added blocklist |
| `help_describes_a_cvar_a_command_and_neither`, `help_finds_a_hidden_cvar_that_the_listings_hide` | `ConVar_PrintDescription` on both kinds, and the `HIDDEN`-vs-`DEVELOPMENTONLY` difference |
| `help_shows_the_default_and_the_bounds_once_the_value_moves` | the `( def. )` clause and `%f` bounds — what makes a `help` line comparable with the shipped engine's |
| `find_requires_every_term_and_looks_in_the_help_text`, `find_searches_commands_as_well_as_cvars` | `CCvar::Find`, generalised to N terms |
| `differences_lists_only_what_has_moved`, `differences_sees_a_never_as_string_cvar_move` | `CvarDifferences`, and gotcha 13 — the reason `describe::value` exists |
| `toggle_with_no_values_flips_between_zero_and_one`, `toggle_cycles_a_value_list_and_wraps`, `toggle_is_refused_for_a_cheat_cvar_and_for_an_unknown_name` | `CvarToggle`'s two modes, its wrap, and `IsValidToggleCommand` |
| `incrementvar_wraps_at_both_ends_and_sets_through_the_buffer`, `incrementvar_goes_through_dispatch_and_so_through_the_cheat_gate` | the wrap at either end, and that the set re-enters dispatch rather than being written |

---

## Invariants and gotchas

Ordered by how likely each is to bite. Input has its own list, with the module:
[`input`'s invariants](#invariants-and-gotchas-input).

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
   row-major matrices. **Terrain is reversed at the same boundary**, in
   `disp::Displacement::build` rather than in the fan loop, and for the same reason —
   there is no shader or geometry kind that escapes this. **See
   [the open question](#open-question-the-culling-convention).**
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
9. **A face's lightmap stride comes from `SURF_BUMPLIGHT`, not from its material.** The
   flag is the file describing its own layout; Valve re-derives the same answer from the
   live material and reads the lump at the wrong stride if a `.vmt` changed after the map
   was compiled. Checked against `sp_a1_intro1`: the flag agrees with the byte spacing
   between consecutive light offsets on all 4,982 lit faces, with zero disagreements. How
   wide a block to *reserve* still comes from the material, because that is what keeps one
   material's surfaces sampling the same way — `LightmapAtlas::write` reconciles the two.
10. **A map with `LVLFLAGS_LIGHTMAP_ALPHA` is refused, not misread.** That CS:GO-era flag
   interleaves a cascaded-shadow term between every face's samples, changing the stride
   for the whole lump. Portal 2 does not set it (`sp_a1_intro1`'s `LUMP_MAP_FLAGS` is 2,
   the baked-static-prop-lighting bit alone), and reading past it would draw noise.
11. **LZMA-compressed lumps are refused, not decoded.** Console builds compress
   individual lumps and stash the uncompressed size in the unused `fourCC`
   (`bsplib.cpp:5513`). Consoles are out of scope, and the alternative is reading
   compressed bytes as geometry and drawing noise.

## Known limits of what is drawn

Not bugs; each names what it waits on.

| Not drawn | Why |
|---|---|
| Shaders this port has not ported | 3 of `sp_a1_intro1`'s 74 materials name one — `SolidEnergy` (the fizzler field), `Refract` and `Black`. They are the magenta checkerboard; the rest resolve, the `maps/<map>/…` cubemap patches included, since the `.bsp`'s pak lump is mounted. |
| Dynamic lights, and lightstyles past style 0 | The atlas bakes style 0 once at load. `R_BuildLightMap` rebuilt a page every frame from `LightStyleValue( style )` and the visible `dlight_t`s. `WorldStats::faces_with_lightstyles` counts the surfaces this understates — zero on `sp_a1_intro1`. |
| Tone mapping | HDR lightmaps arrive in `[0..16]` and reach the shader with `cLightScale` at 1.0, so a map is as bright as `vrad` left it rather than as bright as the shipped game, which auto-exposes. |
| Displacement `$seamless_scale` | Terrain **draws** now (`world/disp/`), but seamless mapping is a triplanar projection blended by the world normal, and a `WorldVertex` has none. 553 of the game's 1,181 displacement faces set it, all in the `sp_a3_*` underground maps and **none in `sp_a1_intro1`**; they draw with the texinfo's ordinary planar mapping — the right texture at the wrong scale. It is the feature that will force `LightmappedGeneric`'s second vertex layout. |
| Translucent brush entities | Render modes 1-5 and 7-9 need a sorted blended pass and draw opaque instead; only `kRenderNone` is honoured. Five entities in the shipped game set one. |
| Brush entities *moving*, and the game state that hides one | They are drawn and solid where the map placed them. Nothing runs `func_door`'s movement or reads `StartDisabled` — that is `server/`. 86 of the game's 2,608 drawable brush entities start disabled. |
| The 3D skybox | `worldspawn`'s `skyname` is read and recorded; drawing it is a second camera over a second set of geometry. |
| Visibility (PVS), area portals | `mod_vis.cpp`. **Every face in the map is drawn every frame.** Fine at 14.5k triangles; not fine on a real level. It is also possible to noclip *out* of the level and look back in, which nothing culls. |
| Faces with explicit primitives | `BuildIndicesForWorldSurface` reads an index list from `LUMP_PRIMINDICES`; these are fan-triangulated instead. Valve's own assert says the index *count* is identical, so only the arrangement differs — visible solely on the non-convex surfaces the list exists for (water). Counted in `WorldStats::faces_with_primitives`. |
| Prop collision | `trace/` covers the world's brushes, the brush models and the displacements; `.phy`/vcollide is its stage 5. |
| Simulation, sound, netcode | Not started. `State_Run` has no `Host_RunFrame` to call. There is a player who walks, falls and is stopped by the world, and nothing else is simulated at all. |

### The camera is the player's eye

`Engine::camera` is now only the conversion: it asks [`Client::view`](CLIENT.md) for a
`ViewSetup` and turns it into a `materials::Camera`. Everything above that — the eye, the
angles, the field of view and both clip planes — is `CViewRender::SetUpView`'s and lives
in `src/client/`. The conversion stays here because a projection matrix is a `wgpu`
convention (handedness, depth range, which way `y` points) and `client/` has no business
knowing any of it.

The player it reads is a real one — a `Player` in `MOVETYPE_NOCLIP`, positioned at
`info_player_start` and moved by `FullNoClipMove` from a `UserCmd` — rather than the
free-fly camera that stood here before `src/client/` landed.

**Which keys move it comes from `cfg/config_default.cfg`**, not from this file: WASD is
`+forward`/`+back`/`+moveleft`/`+moveright`, SPACE and CTRL are `+jump`/`+duck` and drive
the vertical axis (a placeholder divergence — see `CLIENT.md` gotcha 12), the mouse looks,
and Escape releases the cursor.

**Noclip has momentum**, which it did not when this was a camera: `sv_noclipaccelerate`
defaults to 5, so movement accelerates over ~0.6 s and coasts on release. `CLIENT.md`
gotcha 5 has the arithmetic; `sv_noclipaccelerate 0` restores the old instant-stop feel.

**The field of view is wider than `default_fov` says**, and that is correct:
`ViewSetup::fov` has been through `ScaleFOVByWidthRatio`, because Source quotes FOV
horizontally at 4:3. At 16:9 Portal's 75 becomes 91.3 horizontal and 59.8 vertical.
`CLIENT.md` gotcha 1 — and passing `view.aspect` rather than a locally computed one is
part of it.

What is faithful here is the coordinate system: Source is **Z-up right-handed**, so the
view is built with `Z` as up and world geometry needs no conversion. The basis comes from
`AngleVectors`, so the direction the player looks and the direction it moves are the same
arithmetic, and **pitch is positive downwards**.

What is *not*: there is still no collision, no gravity and no prediction —
`MOVETYPE_WALK` is `portdocs/CLIENT.md` stage 4 and waits for `trace/`.

**A black screen on some maps is this, not a lighting bug.** `info_player_start` is only
where the *engine* puts the player; several Portal 2 maps spawn inside a sealed box in the
void and rely on a VScript to teleport the player into the level. `sp_a2_laser_intro` is
one. Until entities and scripting exist there is nothing to run that teleport, so the
camera sits in the box and sees its inside faces. `sp_a1_intro1` spawns in the room it
draws and is the map to check a rendering change against.

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
| Module | `crate::engine::window`, with `window::translate` |
| Lines | ~1,350 including tests (`mod.rs` + `translate.rs`) |
| Tests | 17 (`cargo test engine::window`) |
| Dependencies | `winit` 0.30, `egui-winit`, `crate::engine`, `crate::materials`, `crate::filesystem`, `crate::cmdline` |

### `run`, `Boot` and `RunOutcome`

```rust
pub fn run(config: VideoConfig, boot: Boot<'_>) -> Result<RunOutcome, WindowError>;

pub struct Boot<'a> {
    pub vfs: Option<&'a Vfs>,
    pub command_line: Option<&'a CommandLine>,
    pub test_material: Option<&'a str>, // -vmt <name>
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

`command_line` replaced the separate `map` and `fps_max` fields, which were two
open-codings of one thing. Every `+`-prefixed argument now reaches the engine as command
text the way `CCommandLine` has always fed them to the command buffer: `Engine::boot`
queues `exec valve.rc`, and the shipped file's `stuffcmds` is what turns `+map foo` into a
`map` command. Cvar registration separately seeds `+<name> <value>` defaults, which is why
`+fps_max 60` is in effect before `valve.rc` runs. See
[`console`](#srcengineconsole).

### `VideoConfig`

Unchanged by this work. `from_command_line` is a port of
`OverrideMaterialSystemConfigFromCommandLine` (`matsys_interface.cpp:356`) plus the title
handling from `CGame::CreateGameWindow`. Switches, divergences from Valve's defaults
(windowed 1280x720, vsync on) and the two carried-across quirks (`-width` without
`-height` forcing 4:3; `-w`/`-h` beating `-width`/`-height`) are unchanged — see the
tests, which are the specification.

### Input translation and the cursor grab

`window/` translates and nothing else: `window::translate` is a `KeyCode` → `Key` table
and a mouse-button `match`, and each `winit` event arm builds one
[`input::Event`](#engine-input) and pushes it. `device_event` is implemented for
`DeviceEvent::MouseMotion`, which is the only source of view look.

The one piece of *state* here is the cursor grab, because that is a `winit` call. It
follows `engine.wants_mouse_capture() && focused`, is reconciled after each engine tick
and on every focus change, and picks its mode at runtime — see
[`input`](#engine-input) gotchas #5, #6 and #8, which are the ones that bite. A grab the
platform refuses is reported once and not retried every frame.

### The `egui` boundary

`window/` is also `egui`'s `winit` side, and it owns three things: one `egui::Context`
(cheap to clone; it is an `Arc` inside), an `egui_winit::State`, and a
[`UiRenderer`](MATERIALS.md#uirenderer).

```rust
fn offer_to_ui(&mut self, event: &WindowEvent) -> Consumer;
```

is the whole of `CGame::DispatchInputEvent`'s precedence chain (`sys_mainwind.cpp:399`) —
VGui, then Scaleform or RocketUI, then GameUI, then the client, each asked in turn —
collapsed to one answer. Every window event goes through it before it is translated, and
two things are folded into `egui`'s own `consumed`:

- **The key bound to `toggleconsole` is never the UI's**, and is not shown to `egui` at
  all. See [`input`](#engine-input) gotcha #15.
- **A dialog that is up owns the keyboard** (`Engine::ui_has_focus`). `egui`'s `consumed`
  is per-widget — it is "something has focus" — so with the console open but the entry
  unfocused it would say no and `w` would walk the camera. VGui's answer was a modal
  input context; this is that.

That the answer reflects the *previous* frame's UI state is normal: it is what every
`egui` integration does, and it is what Valve's chain did too, since each target answered
from the state its last frame left. The [key-up latch](#the-key-up-latch) is what makes
one boolean safe.

**`handle_platform_output` is skipped while the game holds the cursor.** It is what sets
the cursor icon, and `egui_winit` calls `set_cursor_visible(true)` alongside
(`egui-winit/src/lib.rs:1286`), which would undo the hide that goes with a grab. Nothing
is lost: while the cursor is captured there is by construction no UI to interact with,
because the console gives the cursor back before it can be clicked in.

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
pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, target: TargetFormat,
           vfs: Option<&'a Vfs>, command_line: Option<&CommandLine>,
           test_material: Option<&str>) -> Engine<'a>;

pub fn boot(&mut self);                 // queues `exec valve.rc`; Host_Init's last act
pub fn frame(&mut self, now: Instant) -> Option<Outcome>;
pub fn render(&mut self, frame: &mut Frame<'_>);
pub fn deadline(&self) -> Option<Instant>;
pub fn request_new_game(&mut self, map: &str);
pub fn request_shutdown(&mut self);
pub fn host(&self) -> &Host;
pub fn console(&self) -> &Console<'a>;
pub fn console_mut(&mut self) -> &mut Console<'a>;

pub fn push_input(&mut self, event: input::Event, consumer: Consumer); // from window/
pub fn wants_mouse_capture(&self) -> bool;          // window/ turns this into a grab

pub fn run_ui(&mut self, ctx: &egui::Context);      // builds this frame's UI
pub fn ui_has_focus(&self) -> bool;                 // the console is up
pub fn ui_bypasses(&self, button: Button) -> bool;  // the toggleconsole key
```

The renderer stays with the window because the surface is tied to the window handle, and
a live `Frame` borrows it; the engine takes device handles instead, which are cheap
refcounted clones. `target` is `Renderer::target_format()`, and it is a parameter rather
than something the engine discovers because the scene render target
([`PostProcess`](MATERIALS.md#post-processing-and-exposure)) must be allocated in the back
buffer's exact format — a different one would double the pipeline count.

Internally `Engine` is seven fields: the `Console`, the `ConsoleUi` that draws it, the
`Host`, the `Input` (which owns the binding table), the engine's own `fps_max` handle with
the generation it last saw, the two booleans that sequence startup, and a private `Scene`
holding the `Vfs`, the device, the `MaterialCache`, the `RenderContext`, the
`PostProcess`, the `World`, the [`Client`](CLIENT.md) and `curtime`. `Scene` is what implements [`Level`], so
`host.frame(&mut self.scene)` is a split borrow of two fields rather than `&mut self`
twice — that is the whole reason for the split.

**The `Client` lives in `Scene` for that same reason**: loading a map is the only thing
that positions a player, and `Level::load` is handed a `&mut Scene`. It is not level state
— its ~19 cvar handles and its button state outlive any map — but its one level-scoped
field decides where it has to be reachable from. `sensitivity` and the rest of the mouse
and movement cvars are registered by `Client::new` and are no longer the engine's.

`Engine::frame` runs the console **inside** the frame, after the host has agreed one is
happening — one `Console::run` is one command-buffer tick, so running it per window event
would tick `wait` at the display's rate. A command queued this frame is therefore acted on
by the next frame's state machine, which is one frame of latency at startup and is why
`map` goes through `Host::request_new_game` rather than loading in place. `EngineCommands`
is the `CommandTarget`: a struct of field borrows, holding `&mut Host`, `&mut Input`,
`&mut ConsoleUi` and `&mut Client` (which is `scene.client` — a field of a field, and
disjoint from the rest).
It owns `map`/`quit`/`restart`, the four `bind` commands, `key_listboundkeys`/
`key_findbinding`, `toggleconsole`/`showconsole`/`hideconsole`, `noclip`, `impulse`,
`trace`, `tonemap`, and the 22 `+`/`-` button pairs from `client::BUTTONS`.

### `Engine::render` — the frame, in four steps

`CViewRender::RenderView` (`viewrender.cpp:2989` onwards), reduced to what this port has:

```text
post.measurement()          drain the readback, feed client.tonemap   DoTonemapping
context.set_exposure(...)   BEFORE the scene, never after             UpdateMaterialSystemTonemapScalar
world.draw(into post.scene) the scene, into an offscreen target       the 3D view
post.resolve(frame, ...)    measure it, then put it on the screen     DoEnginePostProcessing
```

Three rules, each of which fails quietly rather than loudly:

- **`post.measurement()` runs unconditionally, before anything branches.** It arms the
  previous frame's readback as well as returning it, so a frame that returns early
  (no map, `-vmt`) without calling it strands a staging buffer — and after two such frames
  the exposure silently stops adapting for ever.
- **The exposure is set before the scene pass opens.** A pass writes its frame constants
  when it opens, so setting it afterwards affects the *next* pass.
- **`-vmt` and the no-map clear draw straight to the back buffer** and are never measured
  or resolved. A material inspector with an auto-exposing background is not an inspector.

`src/engine/exposure.rs` is the depot-gated measurement of the whole loop against a real
map — the sibling of `world/bench.rs`, and the same shape:

```text
KISAK_GAME_DIR=/path/to/portal2 cargo test --release exposure -- --ignored --nocapture
```

`KISAK_MAP` picks the map and `KISAK_AUTOEXPOSURE_MAX` raises the ceiling, which is how to
ask what a map looks like under the limit its own `env_tonemap_controller` sets. On
`sp_a2_bts2` — a dark maintenance area — the default ceiling of 2 binds immediately and
the map's own 5 takes the exposure to 4.4.

`Engine::boot` prefers `//mod/cfg/config.cfg` and falls back to `config_default.cfg`, then
queues `exec valve.rc` — see [config persistence](#config-persistence). `Engine::frame`
completes the handshake on its first pass: it binds the backquote to `toggleconsole` if
nothing else did (`host.cpp:2085`), sets `config_was_read`, and writes a config if startup
fell back to the defaults. A clean `Outcome::Quit` or `Restart` writes one too.

`Engine::update_client` is where input becomes movement: it is `CL_Move`
(`cl_main.cpp:2734`), and it calls `Client::set_sample_time`, then `Client::create_move`,
then `Client::run_move`. **The refill comes first and is not optional** — Valve makes that
call from the host once per frame (`host.cpp:4192`) because a frame can hold several
ticks; drop it here and keyboard look silently stops working. It also
makes the second half of `CInput::ClearStates` — `Input::clear` released the *keys*, and
`Client::clear_buttons` releases what the `+command`s are holding, which is why
`Event::FocusLost` has to survive into `Input::events`.

`mouse_look_after` now sees **only
what the UI did not take**: with the console up, Escape closes the dialog inside `egui`
and a click is the dialog's, so neither reaches it. With the console closed it is
unchanged — Escape frees the cursor, a click takes it back, last event of the tick wins.
The cursor is given back *for* the console by `wants_mouse_capture`, which is
`mouse_look && !console_open`; keeping that a separate term rather than a write to
`mouse_look` is what makes closing the console restore whatever the game had.

`Engine::run_ui` is called by `window/` between `render` and the present, with the `egui`
pass already open. It is one line — the dialog and the console it drives are two disjoint
fields, which is the same split `host.frame(&mut self.scene)` makes.

`-vmt` is owned here too: when set, `render` draws the material preview *instead of* the
world, because it is an inspector for one material and anything else in the shot defeats
the purpose. `portdocs/MATERIALSYSTEM.md` §9 calls for deleting it once there is a map to
draw, and there now is; it is kept because it remains the only way to inspect a single
material in isolation and because `src/materials/preview.rs` carries the material
system's GPU regression suite.

## Test coverage

229 tests across the five modules; 456 in the crate. **104 are `console/`'s** and have
[their own table](#test-coverage-console); the input tests, now 58, have
[theirs](#test-coverage-input). The tests that arrived with bindings, and those that
arrived with UI precedence, are split across both — because both features are.

The `egui` path itself is tested in two places and neither needs a window: the dialog's
behaviour against a headless `egui::Context` (`engine::console::ui`), and the
`egui`-to-`wgpu` half against a real device and an offscreen target
(`materials::ui` — see [`MATERIALS.md`](MATERIALS.md#uirenderer)).

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
