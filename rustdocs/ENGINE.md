# `src/engine/` — API reference

The engine. [`portdocs/ENGINE.md`](../portdocs/ENGINE.md) breaks the original `engine/`
module into 23 subsystems, 14 of which become modules here. **Four exist so far.**

| Module | Subsystem | Status |
|---|---|---|
| [`host`](#engine-host) | `host_state.cpp`, `sys_engine.cpp` (§7.2) | state machine + frame clock done; no simulation |
| [`world`](#engine-world) | `modelloader.cpp`, `cmodel.cpp` (§7.14) | `.bsp` geometry and lightmaps done; no visibility, collision or props |
| [`input`](#engine-input) | `inputsystem/`, `keys.cpp`, `in_*.cpp` (§7.3/§7.4) | buttons, mouse look and a free-fly camera done; no bindings, UI precedence or controllers |
| [`window`](#engine-window) | `sys_mainwind.cpp`, `sys_getmodes.cpp`, `sdlmgr.cpp` (§7.3) | window, event loop and input translation done |
| `net/`, `console/`, `client/`, `server/`, `audio/`, … | the other 10 | not started |

The binary now loads a real Portal 2 `.bsp` and draws it **lit**: base textures multiplied
by the map's baked lightmaps, packed into an atlas at load. On `sp_a1_intro1` that is
5,512 of 5,638 faces over 77 batches, 58 of its 66 materials resolving, and 4,828 surfaces
with real lighting across 12 atlas pages, and **WASD and the mouse fly through it**.
What is still missing is listed under
[Known limits](#known-limits-of-what-is-drawn); the largest items are visibility (every
face is drawn every frame), displacements and props.

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
WindowEvent      -> Engine::push_input()    -> queued, not acted on
DeviceEvent      -> Engine::push_input()    -> raw motion, ditto
about_to_wait    -> Engine::deadline()      -> ControlFlow::WaitUntil
RedrawRequested  -> Engine::frame(now)      -> None: too early, return and wait
                                            -> Input::frame(), then the view
                                            -> Some(Quit | Restart): exit
                                            -> Some(Continue): carry on
                 -> apply_capture()         -> the cursor grab follows the engine
                 -> Renderer::begin_frame() -> None: back off SKIP_RETRY
                 -> Engine::render(&mut frame)
                 -> Frame::present()
```

Five orderings in there are load-bearing:

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
5. **Nothing sleeps.** See [Pacing](#pacing-is-split-in-two).

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
| Lines | ~2,000 including tests |
| Tests | 23 (`cargo test engine::world`) |
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
    pub lighting_is_hdr: bool,
    pub lightmaps: LightmapPages,
    pub stats: WorldStats,
}
```

`load` is `HostState_NewGame` → `Host_NewGame` → `modelloader->GetModelForName` collapsed
into the one step that currently has meaning. **A material that fails to load is not an
error** — `MaterialCache::load` cannot fail — so the only failures are a missing or
malformed `.bsp`.

`draw` binds each batch's lightmap page and then records the batch with an identity model
matrix: world geometry is already in world space, which is the whole difference between
the world model and the brush models that are not drawn yet.

**Materials are resolved before the geometry is built**, which is forced rather than
stylistic: a surface's vertex layout comes from the shader its material named, and how
wide a lightmap block it reserves comes from whether that material has a `$bumpmap`
(`RegisterLightmappedSurface`, `gl_matsysiface.cpp:216`). Neither is answerable from the
`.bsp`. `load` therefore groups faces by material name, loads every material, and only
then packs lightmaps and emits vertices.

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
emitted in page order. On `sp_a1_intro1` that is 77 batches for 66 materials over 12
pages.

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
    pub faces_displaced: usize,       // geometry is in the displacement lumps
    pub faces_with_primitives: usize, // fan-approximated; see below
    pub vertices: usize,
    pub triangles: usize,
    pub materials: usize,
    pub materials_missing: usize,     // resolved to the error checkerboard
    pub faces_lit: usize,             // got a real lightmap block
    pub faces_fullbright: usize,      // wanted one and could not have one
    pub faces_with_lightstyles: usize,// more than style 0; only style 0 is baked
    pub lightmap_pages: usize,        // including the 1x1 white page
}
```

`faces_lit + faces_fullbright` does not reach `faces_drawn`: the difference is the faces
whose *material* is not lit at all — tool textures, and anything that fell back to the
error material. Those never ask for a block, so neither counter moves.

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
    pub lighting: Vec<ColorRgbExp32>,
    pub lighting_is_hdr: bool,
    pub level_flags: u32,
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

## `src/engine/input/`

Buttons, the event queue, and where the view points. Replaces `inputsystem/` (10,649
lines), `engine/keys.cpp` (1,392) and `sys_mainwind.cpp`'s `DispatchInputEvent`, and
stands in for `game/client/in_*.cpp` (7,402) until `client/` exists.
[`portdocs/ENGINE_INPUT.md`](../portdocs/ENGINE_INPUT.md) is the plan; **stages 1 and 2
of its five are done** — translation, button state, mouse look and a free-fly camera.
Bindings (stage 3) want `console/`, UI precedence (stage 4) wants `egui`, and controllers
(stage 5) want `gilrs`.

| | |
|---|---|
| Module | `crate::engine::input`, with `input::button` and `input::view` |
| Lines | ~1,360 including tests |
| Tests | 34 (`cargo test engine::input`), plus 5 for the `winit` table (`engine::window::translate`) |
| Dependencies | `std` and `glam`. **Not `winit`** — see [the seam](#the-seam-window-translates-input-decides) |

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

### `Input`

```rust
impl Input {
    pub fn new() -> Input;
    pub fn push(&mut self, event: Event);           // between ticks, from window/
    pub fn frame(&mut self) -> (f32, f32);          // once a tick; returns summed motion
    pub fn events(&self) -> &[Event];               // this tick's, until the next frame()
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

### `ViewAngles` and `FlyCamera`

```rust
pub const SENSITIVITY: f32 = 2.5;                   // `sensitivity`

pub struct ViewAngles { pub pitch: f32, pub yaw: f32, pub roll: f32 }

impl ViewAngles {
    pub fn new(pitch: f32, yaw: f32) -> ViewAngles;
    pub fn apply_mouse(&mut self, dx: f32, dy: f32, sensitivity: f32);
    pub fn vectors(&self) -> (Vec3, Vec3, Vec3);    // AngleVectors: forward, right, up
}

pub struct FlyCamera { pub origin: Vec3, pub angles: ViewAngles }

impl FlyCamera {
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> FlyCamera;
    pub fn look(&mut self, dx: f32, dy: f32);
    pub fn step(&mut self, input: &Input, seconds: f32);
}
```

`ViewAngles` is faithful: `ScaleMouse`/`ApplyMouse` (`in_mouse.cpp:412`, `:470`),
`ClampAngles` (`in_main.cpp:975`) and `AngleVectors` (`mathlib_base.cpp:1027`), with
Valve's shipped constants — `sensitivity` 2.5, `m_yaw`/`m_pitch` 0.022,
`cl_pitchdown`/`cl_pitchup` 89. Roll is deliberately *not* clamped, because Portal
excludes itself from the ±50° limit every other Source game applies ("the player can be
upside down! -Jeep").

`FlyCamera` is not faithful and is not meant to be: it is `CUserCmd` + `CGameMovement` +
`CViewRender::SetUpView` collapsed to eight lines, moving at `FullNoClipMove`'s speeds
(`cl_forwardspeed` 175 — Portal 2's `MAX_LINEAR_SPEED`, where other Source games get 450
— times `sv_noclipspeed` 5, halved by `+speed`, clamped to `sv_maxspeed * 5`) with no
acceleration. **It replaced the turntable camera**, and it is replaced in turn by
`client/`.

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
   is translated and queued for the UI that does not exist yet, and nothing reads it.
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
5. **Escape is the only way to get the cursor back**, until there is a UI. A click takes
   it again. If that policy is ever moved or "simplified", a grabbed window becomes one
   the user cannot leave.
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
    `in_main.cpp:434`), so the consumer decides. Likewise `Text` is unfiltered, control
    characters included — what counts as typable is the console's question.
11. **Wheel notches are accumulated before becoming button presses.** A mouse reports
    lines and a trackpad reports pixels, continuously; `MWHEELUP`/`MWHEELDOWN` are
    discrete. `window/`'s `PIXELS_PER_NOTCH` (50, a chosen constant — Valve never saw a
    pixel delta) is the threshold, and the fractional remainder is kept, so a slow swipe
    still eventually clicks.

### Not implemented, and what each waits on

| Missing | Waits on |
|---|---|
| The binding table, `bind`/`unbind`/`unbindall`, the `+`/`-` convention with the button index as an argument | `console/` — stage 3. The seam is `CommandSink`, and it is deliberately **not** written yet: nothing would implement it. |
| UI event precedence, and the **key-up latch** (`FilterKey`, `keys.cpp:1189`) that delivers a key-up to whoever consumed the key-down | `egui` — stage 4. Every "stuck key" bug in a Source-like engine is that invariant being violated: press `mouse1`, open the console, release. |
| Controllers, hot-plug, analog axes | `gilrs` — stage 5. `in_joystick.cpp`'s response curves and deadzones are content-tuned client behavior and come with `client/`, not with the device layer. |
| `CUserCmd`, `kbutton_t`'s `down[2]`, the fractional `KeyState` model, `CreateMove`, prediction | `client/`. Correct and the right design; building them against a camera instead of a player would bake in the wrong consumer. |
| `m_customaccel` 1-4, `m_mousespeed`/`m_mouseaccel1`/`m_mouseaccel2` | Nothing — deliberately dropped. Per-user feel tuning with no default behavior, and the latter three are Windows `SPI_SETMOUSE` overrides, inert on POSIX. |
| `cl_mouselook_roll_compensation` — rotating the mouse delta by the inverse of the view roll | Something that rolls the view. **In scope for Portal 2**, which rolls constantly (gels, portals through non-vertical surfaces); `ViewAngles::roll` is where it attaches. |
| `Key_StartTrapMode` ("press a key to bind it") | An options UI. ~35 lines, trivially re-added. |
| Split-screen: per-player down-state and view angles | Co-op being scheduled. The binding table was global even in the original, so the cost is deferred as long as nothing bakes a player slot into `Event` or `Button` — nothing does. |
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

The cursor grab, the `winit` event arms and `device_event` need a window and cannot be
tested here; they are verified by running the binary.

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
| Materials patched into the `.bsp` | 8 of `sp_a1_intro1`'s 66 are `maps/<map>/…` cubemap patches that live in the `.bsp`'s embedded pak lump, which `Vfs` does not mount. They are the magenta checkerboard; the rest resolve. |
| Dynamic lights, and lightstyles past style 0 | The atlas bakes style 0 once at load. `R_BuildLightMap` rebuilt a page every frame from `LightStyleValue( style )` and the visible `dlight_t`s. `WorldStats::faces_with_lightstyles` counts the surfaces this understates — zero on `sp_a1_intro1`. |
| Tone mapping | HDR lightmaps arrive in `[0..16]` and reach the shader with `cLightScale` at 1.0, so a map is as bright as `vrad` left it rather than as bright as the shipped game, which auto-exposes. |
| Displacements | Geometry lives in `LUMP_DISPINFO`/`LUMP_DISP_VERTS`; `world/disp/` (§7.15). Counted in `WorldStats::faces_displaced`. |
| Brush entities (models 1..n) | Positioned by the entity that names them, so they need the entity system, not just the lump. |
| Static props, `.mdl` models | `staticpropmgr.cpp`, `studiorender`. |
| The 3D skybox | `worldspawn`'s `skyname` is read and recorded; drawing it is a second camera over a second set of geometry. |
| Visibility (PVS), area portals | `mod_vis.cpp`. **Every face in the map is drawn every frame.** Fine at 14.5k triangles; not fine on a real level. Now that the camera flies, it is also possible to fly *out* of the level and look back in, which nothing culls. |
| Faces with explicit primitives | `BuildIndicesForWorldSurface` reads an index list from `LUMP_PRIMINDICES`; these are fan-triangulated instead. Valve's own assert says the index *count* is identical, so only the arrangement differs — visible solely on the non-convex surfaces the list exists for (water). Counted in `WorldStats::faces_with_primitives`. |
| Collision, traces | `cmodel.cpp`, `enginetrace.cpp` — needed by gameplay, not by drawing. |
| Simulation, sound, netcode | Not started. `State_Run` has no `Host_RunFrame` to call. Input exists but moves a camera, not a player. |

### The camera is a placeholder

`Engine::camera` reads the [`FlyCamera`](#engine-input) the level put at
`info_player_start` and that input moves: **WASD, space and left control to rise and
fall, left shift to walk, mouse to look, Escape to release the cursor.** (The turntable
that stood here before input landed is gone.) What is faithful is the projection —
`VIEW_NEARZ` 7 (`game/client/view.h:27`), far = `r_mapextents` × √3 (`view.cpp:644`),
and Portal's `default_fov` 75 (`clientmode_portal.cpp:32`) — and the coordinate system:
Source is **Z-up right-handed**, so the view is built with `Z` as up and world geometry
needs no conversion. The basis comes from `AngleVectors`, so the direction the camera
looks and the direction it moves are the same arithmetic, and **pitch is positive
downwards**.

What is *not* faithful is that there is no player: no collision, no gravity, no
`CUserCmd`, no prediction. `CViewRender::SetUpView` and `client/` are the real
replacement, and `FlyCamera` goes with them.

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
| Lines | ~1,230 including tests (`mod.rs` + `translate.rs`) |
| Tests | 17 (`cargo test engine::window`) |
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

pub fn push_input(&mut self, event: input::Event);  // from window/, between ticks
pub fn wants_mouse_capture(&self) -> bool;          // window/ turns this into a grab
```

The renderer stays with the window because the surface is tied to the window handle, and
a live `Frame` borrows it; the engine takes device handles instead, which are cheap
refcounted clones.

Internally `Engine` is three fields: the `Host`, the `Input`, and a private `Scene`
holding the `Vfs`, the device, the `MaterialCache`, the `RenderContext`, the `World`,
the view and `curtime`. `Scene` is what implements [`Level`], so
`host.frame(&mut self.scene)` is a split borrow of two fields rather than `&mut self`
twice — that is the whole reason for the split. The view lives in `Scene` rather than
beside the `Input` because it is level state: loading a map puts it at that map's
`info_player_start`.

`Engine::update_view` is where input becomes movement, and `mouse_look_after` is the
whole of the UI-precedence policy until there is a UI: Escape frees the cursor, a click
takes it back, last event of the tick wins.

`-vmt` is owned here too: when set, `render` draws the material preview *instead of* the
world, because it is an inspector for one material and anything else in the shot defeats
the purpose. `portdocs/MATERIALSYSTEM.md` §9 calls for deleting it once there is a map to
draw, and there now is; it is kept because it remains the only way to inspect a single
material in isolation and because `src/materials/preview.rs` carries the material
system's GPU regression suite.

## Test coverage

93 tests across the four modules; 317 in the crate. **42 of those arrived with input** —
34 in `input/`, 5 for the `winit` key table and 3 for the cursor-release policy — and
have [their own table](#test-coverage-input).

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
