# CLIENT.md — `src/client/`

The game client: the local player, the command that moves it, and where the eye ends up.
Held keys and mouse motion in, a `UserCmd` and a player position out.

Porting plan and the C++ inventory: [`portdocs/CLIENT.md`](../portdocs/CLIENT.md).

| | |
|---|---|
| Module | `crate::client`, with `client::{button, movement, player, usercmd, view}` |
| Replaces | `game/client/in_main.cpp`, `in_mouse.cpp`, `view.cpp`'s `SetUpView`/`GetZNear`/`GetZFar`, `game/shared/usercmd.h`, `in_buttons.h`, and `FullNoClipMove`/`Accelerate` from `game/shared/gamemovement.cpp` |
| Lines | 2,750 including tests |
| Tests | 65 (`cargo test client::`) |
| Dependencies | `std`, `glam`, and `crate::engine::console` for cvar handles. **Not `winit`, not `egui`, not `wgpu`, not `crate::engine::input`** |
| Status | **Stages 1-4 of 5 done** (`portdocs/CLIENT.md` §8). Stage 5 waits for `net/` |

## This is not `src/engine/client/`

Two modules are called "the client" and they share nothing but the word.

| | Valve | Rust | Blocked on |
|---|---|---|---|
| **the game client** — this | `client.so` | `src/client/` | nothing |
| the client connection | `engine/cl_*.cpp`, `client.cpp` | `src/engine/client/` (does not exist) | `net/` |

`ENGINE.md` §7.5 is the second one. In prose, say *the game client* and *the client
connection*; a bare "the client" gets read as the wrong one.

## Quick start

```rust
use crate::client::Client;

// At startup — this is where the client's ~19 cvars get registered.
let mut client = Client::new(&mut console);

// When a map loads. `origin` is the player's FEET, not the eye.
client.spawn(spawn.origin, spawn.pitch, spawn.yaw);

// Once per frame, after the command buffer has run so that this tick's
// `+forward` is already held. The refill comes FIRST — without it keyboard
// look silently does nothing.
client.set_sample_time(seconds);                        // IN_SetSampleTime
let command = client.create_move(seconds, mouse_delta); // CInput::CreateMove
client.run_move(&command, seconds);                     // ProcessMovement

// For the renderer. `ViewSetup` is data; turning it into a projection matrix is
// the material system's convention to choose, so the engine does that bit.
let view = client.view(width, height);          // CViewRender::SetUpView
let (forward, _, up) = view.angles.vectors();
let camera = Camera::perspective(view.origin, look_at_mat4(view.origin, view.origin + forward, up),
                                 view.fov, view.aspect, view.z_near, view.z_far);
```

`+forward` and its eighteen siblings arrive through the **command buffer**, not through a
function call:

```rust
// in EngineCommands::execute, for a name starting with '+' or '-'
self.client.buttons_mut().apply(name, down, index);
```

## Where it sits in the frame

`_Host_RunFrame_Input` (`engine/host.cpp:3272`) does three things in order, and
`Engine::frame` does the same three:

```
Input::frame            drain the queue, sum the mouse delta   ClientDLL_ProcessInput
Input::dispatch_bindings + Console::run                        Cbuf_Execute
Engine::update_client                                          CL_Move (cl_main.cpp:2734)
  -> Client::create_move
  -> Client::run_move
```

The ordering is load-bearing: the command buffer runs **before** `create_move`, so a key
pressed this tick moves the player this tick rather than the next one.

## Core types

### `Client`

```rust
pub struct Client { /* player, buttons, ~19 Cvar handles, command_number, tick_count, impulse */ }

impl Client {
    pub fn new(console: &mut Console<'_>) -> Client;

    pub fn set_sample_time(&mut self, frametime: f32);            // IN_SetSampleTime
    pub fn create_move(&mut self, dt: f32, mouse: (f32, f32)) -> UserCmd;  // CreateMove
    pub fn run_move(&mut self, cmd: &UserCmd, dt: f32);           // ProcessMovement

    pub fn spawn(&mut self, origin: Vec3, pitch: f32, yaw: f32);
    pub fn buttons_mut(&mut self) -> &mut Buttons;
    pub fn clear_buttons(&mut self);                              // CInput::ClearStates
    pub fn set_impulse(&mut self, impulse: u8);
    pub fn toggle_noclip(&mut self) -> MoveType;

    pub fn view(&self, width: u32, height: u32) -> ViewSetup;     // CViewRender::SetUpView
}
```

**`create_move` and `run_move` are two calls on purpose.** In a game with a server the
command goes over the wire between them, and prediction is a layer that wraps `run_move`
without rewriting it. Do not merge them for convenience.

`Client` lives in `Engine`'s `Scene`, not beside `Host` — loading a map is the only thing
that positions a player, and `Level::load` is handed a `&mut Scene`.

### `UserCmd`

```rust
pub struct UserCmd {
    pub command_number: i32,
    pub tick_count: i32,
    pub viewangles: ViewAngles,
    pub forwardmove: f32,   // units per second, NOT an axis in [-1, 1]
    pub sidemove: f32,
    pub upmove: f32,
    pub buttons: ButtonBits,
    pub impulse: u8,
    pub mousedx: i16,       // the SCALED delta, truncated
    pub mousedy: i16,
    pub random_seed: i32,   // always 0 — see "Not implemented"
}

impl UserCmd { pub fn new(command_number: i32, tick_count: i32) -> UserCmd; }
```

### `Buttons`, `KButton`, `ButtonBits`, `MoveButton`

```rust
pub struct KButton { /* down: [Option<i32>; 2], held, pressed, released */ }

impl KButton {
    pub fn press(&mut self, index: Option<i32>);     // KeyDown  (in_main.cpp:424)
    pub fn release(&mut self, index: Option<i32>);   // KeyUp    (:460)
    pub fn is_down(&self) -> bool;
    pub fn key_state(&mut self) -> f32;              // KeyState (:813) — DESTRUCTIVE
}

pub struct Buttons { /* [KButton; 22] */ }

impl Buttons {
    pub fn apply(&mut self, name: &str, down: bool, index: Option<i32>) -> bool;
    pub fn is_down(&self, button: MoveButton) -> bool;
    pub fn key_state(&mut self, button: MoveButton) -> f32;
    pub fn bits(&mut self, reset: bool) -> ButtonBits;  // GetButtonBits (:1771)
    pub fn clear(&mut self);
}

pub struct ButtonSpec { pub down: &'static str, pub up: &'static str,
                        pub name: &'static str, pub button: MoveButton, pub bits: ButtonBits }
pub const BUTTONS: &[ButtonSpec];   // 22 rows, indexed by `MoveButton`
```

`BUTTONS` is what the engine iterates to register the `+`/`-` command pairs. Both
spellings are stored because `CommandSpec::name` is a `&'static str`.

The 22: `forward`, `back`, `moveleft`, `moveright`, `moveup`, `movedown`, `left`,
`right`, `lookup`, `lookdown`, `speed`, `walk`, `strafe`, `klook`, `attack`, `attack2`,
`use`, `jump`, `duck`, `reload`, `zoom`, `score`. **Six carry no `IN_*` bit** — `moveup`,
`movedown`, `lookup`, `lookdown`, `strafe`, `klook` — because they are client-side
modifiers that change how the *other* buttons are read, and `GetButtonBits` never
mentions them.

### `Player` and `MoveData`

```rust
pub const VEC_VIEW: Vec3 = Vec3::new(0.0, 0.0, 64.0);          // the standing eye
pub const VEC_DUCK_VIEW: Vec3 = Vec3::new(0.0, 0.0, 28.0);     // the crouched one
pub const VEC_HULL_MIN: Vec3 = Vec3::new(-16.0, -16.0, 0.0);   // 32 x 32 x 72
pub const VEC_HULL_MAX: Vec3 = Vec3::new(16.0, 16.0, 72.0);
pub const VEC_DUCK_HULL_MIN: Vec3 = Vec3::new(-16.0, -16.0, 0.0);
pub const VEC_DUCK_HULL_MAX: Vec3 = Vec3::new(16.0, 16.0, 36.0);

pub enum MoveType { Walk, Noclip }

pub struct Player {
    pub origin: Vec3,        // the FEET
    pub velocity: Vec3,
    pub angles: ViewAngles,
    pub move_type: MoveType,
    pub view_offset: Vec3,
    pub ground: Option<Vec3>,      // the normal underfoot, None when airborne
    pub surface_friction: f32,
    pub ducked: bool,
    pub ducking: bool,             // mid-transition, either direction
    pub duck_time_msecs: i32,
    pub old_buttons: ButtonBits,   // what the PREVIOUS command held
}

impl Player {
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> Player;  // MoveType::Walk
    pub fn eye(&self) -> Vec3;                                 // origin + view_offset
}
```

### `movement` — `MoveData`, `MoveVars` and the move itself

```rust
/// `movevars_shared.cpp`: the `sv_*` set, read once per command.
pub struct MoveVars { gravity, friction, stopspeed, accelerate, airaccelerate,
                      stepsize, maxvelocity, edgefriction, use_edgefriction,
                      noclipspeed, noclipaccelerate }
impl MoveVars { pub const PORTAL2: MoveVars; }

/// `CMoveData` plus the parts of `player->m_Local` movement writes.
pub struct MoveData { /* origin, velocity, angles, forwardmove, sidemove, upmove,
                        buttons, old_buttons, max_speed, move_type, ground,
                        surface_friction, ducked, ducking, duck_time_msecs,
                        view_offset, speed_cropped */ }

pub fn player_move(mv: &mut MoveData, tracer: Option<&mut Tracer<'_>>,
                   vars: &MoveVars, dt: f32);
pub fn full_walk_move(mv: &mut MoveData, tracer: &mut Tracer<'_>,
                      vars: &MoveVars, dt: f32);
pub fn full_noclip_move(mv: &mut MoveData, vars: &MoveVars, dt: f32);
pub fn accelerate(mv: &mut MoveData, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32);
pub fn air_accelerate(mv: &mut MoveData, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32);
pub fn check_parameters(mv: &mut MoveData);
pub fn check_velocity(mv: &mut MoveData, vars: &MoveVars);
pub fn player_mins(ducked: bool) -> Vec3;
pub fn player_maxs(ducked: bool) -> Vec3;
pub fn player_view_offset(ducked: bool) -> Vec3;
```

`movement.rs` is **shared code**: `gamemovement.cpp` compiles into both binaries, and the
same command must produce the same position on both or prediction mispredicts. Nothing in
it may name a cvar, a console or a view — everything arrives in `MoveData` and `MoveVars`.
That is why the `sv_*` values are read into a struct by `Client::move_vars` rather than
reached through cvar handles at each use, the way the C++ does.

### It is `CPortalGameMovement`, not `CGameMovement`

Portal 2 overrides two dozen of the base class's methods and **several of the overrides
change behaviour that has nothing to do with portals**. Porting the base class produces a
player who moves plausibly and wrongly. What survives into this port:

| | `CGameMovement` | `CPortalGameMovement` |
|---|---|---|
| Jump height | 21 units | **45** |
| Bunny-hop speed boost on jump | yes (HL2) | **none** |
| Jump while ducked | allowed, at a fixed speed | **refused** |
| Air-control speed cap | 30 | **60** |
| Duck transition | 200 ms (CS:GO) | **400 ms** |
| Gravity | 800 | **600** |
| Edge friction | absent | **on**, doubling friction over a ledge |
| `ClipVelocity`'s re-push | at least `DIST_EPSILON` | cancels the residual only |
| `StayOnGround`'s up-probe | 2 units | **1 unit** |
| Walking into a standable slope | `StepMove` | **slides up the ramp** |

Where Portal's override differs only by generalising world `+Z` to an arbitrary "stick
normal" — its paint-gel gravity reorientation — the two are the same function with no
paint, because `m_vGravityDirection = -stickNormal` and the stick normal is world up.
Those are ported in the world-`+Z` form.

### `ViewSetup`

```rust
pub struct ViewSetup {
    pub origin: Vec3,        // the eye
    pub angles: ViewAngles,
    pub fov: f32,            // HORIZONTAL degrees, ALREADY width-ratio scaled
    pub z_near: f32,
    pub z_far: f32,
    pub width: u32,
    pub height: u32,
    pub aspect: f32,         // used for both the FOV scaling and the projection
}

pub fn scale_fov_by_width_ratio(fov_degrees: f32, ratio: f32) -> f32;  // view.cpp:923
pub fn screen_aspect(width: u32, height: u32) -> f32;                  // gl_rmain.cpp:127

pub const VIEW_NEARZ: f32 = 7.0;
pub const R_MAPEXTENTS: f32 = 16384.0;
pub const MAP_DIAGONAL: f32 = 1.732_050_8;   // √3
pub const FOV_ASPECT: f32 = 4.0 / 3.0;
```

`CViewSetup` (`public/view_shared.h:44`) carries about fifty fields; this carries the
eight a single perspective view of a world needs. What is left out is attached to
something that does not exist — the viewmodel pair, the ortho box, the custom view and
projection matrices portals and monitors set, depth-of-field and motion blur, and `x`/`y`,
which are only non-zero for a split-screen inset.

**It is data, not a camera.** Building a projection matrix from it is a `wgpu`
convention — handedness, depth range, which way `y` points — so `Engine::camera` does it
and `client/` never names a `materials` type.

### `ViewAngles`

```rust
pub struct ViewAngles { pub pitch: f32, pub yaw: f32, pub roll: f32 }

impl ViewAngles {
    pub fn new(pitch: f32, yaw: f32) -> ViewAngles;   // normalizes; does not clamp pitch
    pub fn normalize(&mut self);                      // SetViewAngles' AngleNormalize
    pub fn apply_mouse_yaw(&mut self, mouse_x: f32, m_yaw: f32);
    pub fn apply_mouse_pitch(&mut self, mouse_y: f32, m_pitch: f32, down: f32, up: f32);
    pub fn vectors(&self) -> (Vec3, Vec3, Vec3);      // AngleVectors: forward, right, up
}

pub fn scale_mouse(dx: f32, dy: f32, sensitivity: f32) -> (f32, f32);   // ScaleMouse
```

**The angles live here, not in the engine.** Valve keeps them in `CClientState`
(`engine/client.h:193`) and reaches them through `engine->GetViewAngles`/`SetViewAngles`
— over a comment reading `// FIXME, move entirely to client .dll`
(`engine/cdll_engine_int.cpp:1048`). There is no DLL boundary here to force the split, so
the port takes the FIXME. `src/engine/client/`, when it arrives, asks rather than keeping
a second copy.

## The cvars

Registered by `Client::new`. Names, defaults, bounds and flags are Valve's; `FCVAR_NOTIFY`,
`FCVAR_REPLICATED`, `FCVAR_RELEASE` and `FCVAR_SS` have no counterpart here and are
dropped rather than approximated.

| Cvar | Default | Flags | Source |
|---|---|---|---|
| `sensitivity` | 2.5, `[0.0001, 1000]` | archive | `in_mouse.cpp:100` |
| `m_yaw` | 0.022, `[0.0001, 1000]` | archive | `in_mouse.cpp:103` |
| `m_pitch` | 0.022, **unbounded** | archive | `in_mouse.cpp:59` |
| `m_side` | 0.8, `[0.0001, 1000]` | archive | `in_mouse.cpp:102` |
| `m_forward` | 1, `[0.0001, 1000]` | archive | `in_mouse.cpp:104` |
| `lookstrafe` | 0 | archive | `in_main.cpp:53` |
| `cl_mouseenable` | 1 | — | `in_mouse.cpp:125` |
| `cl_pitchdown` / `cl_pitchup` | 89 | cheat | `in_main.cpp:49`, `:50` |
| `cl_yawspeed` | 210 | — | `in_main.cpp:47` |
| `cl_pitchspeed` | 225 | — | `in_main.cpp:48` |
| `cl_anglespeedkey` | **0.67**, where `+speed` halves movement | — | `in_main.cpp:46` |
| `cl_mouselook` | 1 | archive | `in_mouse.cpp:121` |
| `in_usekeyboardsampletime` | 1 | — | `in_main.cpp:875` |
| `cl_forwardspeed` / `cl_backspeed` / `cl_sidespeed` | 175 | cheat | `in_main.cpp:61-63` |
| `cl_upspeed` | 320 | cheat | `in_main.cpp:51` |
| `default_fov` | **75**, and it is a **4:3 horizontal** number | cheat | `clientmode_portal.cpp:32` |
| `r_farz` | -1 (meaning "use the map's") | cheat | `view.cpp:135` |
| `r_mapextents` | 16384 | cheat | `view.cpp:119` |
| `sv_maxspeed` | 320 | — | `movevars_shared.cpp:29` |
| `sv_friction` | 5.2 | — | `movevars_shared.cpp:44` |
| `sv_stopspeed` | 80 | — | `movevars_shared.cpp:23` |
| `sv_noclipspeed` / `sv_noclipaccelerate` | 5 | archive | `movevars_shared.cpp:25`, `:24` |

Commands, registered by the engine alongside its own: the 22 `+`/`-` pairs from
`BUTTONS`, plus `noclip` and `impulse`.

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **`ViewSetup::fov` is horizontal, is already width-ratio scaled, and `default_fov` is
   not.** Source's FOV numbers are quoted at **4:3**; `CViewRender::Render` scales them by
   `aspect / (4/3)` before the projection is built (`view.cpp:1084`). The composition is
   Hor+: the *vertical* FOV comes out constant at `2·atan(tan(fov/2) · 0.75)` — 59.8° for
   Portal's 75 — and the horizontal grows with the screen, reaching 91.3° at 16:9. Hand
   `default_fov` straight to a `PerspectiveX` and you get a 46.7° vertical FOV at 16:9: a
   view that is not obviously wrong, just quietly too narrow. `Client::view` does the
   scaling, so **use `view.fov` and never `default_fov`** — and pass `view.aspect` to the
   projection, because the same ratio has to appear on both sides for the vertical FOV to
   come out constant.

2. **`set_sample_time` must be called once per frame, before `create_move`, or
   keyboard look silently stops working.** `DetermineKeySpeed` returns 0 with an empty
   budget and `AdjustAngles` returns early on a 0, so the symptom is `+left` doing nothing
   — no error, no log line. Valve splits the refill (once per *frame*,
   `host.cpp:4192`) from the draw-down (once per *command*) because a frame can hold
   several ticks; this port has one command per frame, so the two cancel exactly and the
   budget is currently a no-op. It is here because it is the shape the function has the
   moment either of those changes.

3. **`cl_mouselook 0` does not turn the mouse off.** It is easy to read as a master
   switch and it is not: `ControllerMove` gates the mouse on `cl_mouseenable` and on the
   cursor being grabbed (`in_main.cpp:1199`), never on this. `cl_mouselook 0` *adds*
   keyboard pitch — it is the only thing that makes `+lookup`, `+lookdown` and `+klook`
   do anything at all. `cl_mouseenable 0` is what takes the mouse away.

4. **`KeyState` is destructive, and the read order changes what the command says.**
   `KButton::key_state` clears both impulse bits; `Buttons::bits` clears only the
   *pressed* bit. `create_move` computes the movement axes first and the bitfield second,
   which is Valve's order, and it means **a tap shorter than one frame contributes to
   `forwardmove` and not to `IN_FORWARD`**. Reverse the two and it contributes to both —
   a difference a server would see. Call `key_state` once per button per command.

5. **The first frame after a press is worth half.** `KeyState` returns 0.5 for
   "pressed this frame and still held", 1.0 only once the button has been held across a
   whole frame, 0.25 for a press-and-release inside one frame, 0.75 for a
   release-and-re-press. A movement value that looks wrong by a factor of two is almost
   always this, working correctly.

6. **`origin` is the feet; `eye()` is 64 units higher.** `Player::origin` is what
   movement moves and what `world::Spawn::origin` supplies. Conflating them is a 64-unit
   error that reads as a level built slightly wrong rather than as a bug. Ask
   `Client::view` for the eye — do not add `VEC_VIEW` at a call site, because
   `Player::eye` is the seam where view bob, punch angles and Portal's through-a-portal
   eye interpolation attach.

7. **Noclip has momentum, and a tap does not move it.** `sv_noclipaccelerate` defaults to
   **5, not 0**. `FullNoClipMove`'s friction bleed floors `control` at `maxspeed / 4`, so
   at 60 Hz it removes ~34.7 units of speed every frame whatever the player is doing,
   while a quarter-speed wish only accelerates by ~20.8. This is Valve's arithmetic. The
   consequences, for a held `+forward` at 60 Hz: it takes ~0.6 s to reach 90% of speed,
   releasing coasts to a stop rather than stopping dead, and the steady state settles at
   **~768 units/s rather than the 875 the wish asks for** — friction and the
   `addspeed` cap balance below the ask. Set `sv_noclipaccelerate 0` for the instant-stop
   feel the old placeholder camera had.

8. **A frame time of 1.0 does not move the player at all.** The friction bleed scales
   with `dt`, so a one-second step removes more speed than a second of acceleration adds.
   Tests must step at a realistic rate (`1.0 / 60.0`); a one-shot `run_move(&cmd, 1.0)`
   asserts nothing useful.

9. **Pitch is positive downwards.** `vectors()` negates it (`forward.z = -sin(pitch)`)
   and `apply_mouse_pitch` *adds* the mouse's Y. If the view looks at the ceiling when it
   should look at the floor, this is the sign.

10. **"Right" is `-Y` when facing `+X`.** Source is Z-up right-handed. Get it backwards
   and strafing goes the wrong way while everything else looks correct.

11. **`m_pitch` is deliberately unbounded** where its four neighbours are clamped to
   `[0.0001, 1000]`: a *negative* value is how "reverse mouse" is spelled. Copying the
   clamp from the line above would silently break that option. In the original it is a
   `ConVar_ServerBounded` that returns `±0.022` with `sv_cheats` off — an anti-cheat
   measure, and one that needs `sv_cheats`, which does not exist yet.

12. **Focus loss needs two calls, not one.** `Input::clear` releases the *keys*;
   `Client::clear_buttons` releases what the `+command`s are holding. A button is held by
   the command, not by the key, so alt-tabbing with `+forward` down leaves the player
   walking for ever if the second call is missed. `Engine::update_client` makes it when
   `Event::FocusLost` reaches the tick.

13. **`turning noclip off freezes the player`**, it does not drop them. `MOVETYPE_WALK`
    is stage 4 and needs `trace/`; there is no ground to stand on, so doing nothing is
    the honest placeholder. The `noclip` command says so when it is turned off.

14. **`+jump` and `+duck` also drive the vertical axis**, and that is a documented
    placeholder, not Valve's behaviour: `ComputeUpwardMove` reads `+moveup`/`+movedown`
    only, and Portal 2's shipped config binds neither. It reads `is_down` rather than
    `key_state` precisely so that reading it does not disturb `IN_JUMP`/`IN_DUCK`. It
    dies at stage 4.

15. **`ScaleMovements` is dead in the original** — its body is `return;` above a
    commented-out block, under a `// FIXME FIXME: This doesn't work`. It is not ported,
    and it should not be "fixed": the clip it was going to apply is `CheckParameters`',
    which happens in the right place already (and is skipped entirely for noclip).

### Walking's own, added by stage 4

Same ordering: most likely to bite first.

- **A Portal 2 player's max speed is 175, not `sv_maxspeed`'s 320.**
  `mv->m_flMaxSpeed` is `GetPlayerMaxSpeed()`, which is
  `min( sv_maxspeed, MaxSpeed() )`, and a Portal player's `MaxSpeed()` is
  `sv_speed_normal` = 175 (`portal_player_shared.cpp:1591`). It bounds walking *and*
  noclip, whose ceiling is therefore `175 * sv_noclipspeed` = 875 rather than 1600.
  **Stage 1 had this wrong** and flew at 1600; the fix also changed what a sub-frame tap
  does, because `FullNoClipMove`'s friction floor is `maxspeed / 4`.

- **`old_buttons` lives on the `Player`, not in the `UserCmd`.** Jump reads it to refuse a
  pogo stick and duck reads it for press and release edges — both are questions about the
  *previous* command, so a `MoveData` built fresh each frame has to carry it in and out.
  Drop the round-trip and jump fires every frame the key is held.

- **`speed_cropped` must be false at the start of every command.** It is
  `m_iSpeedCropped`, and it exists so the ducking speed crop applies once; leave it set
  and a crouched player moves at full speed, leave it perpetually clear and they move at
  a third of a third.

- **`ground` is `Option<Vec3>` and `None` is airborne** — not `Some(Vec3::ZERO)`. Valve
  stores an entity pointer and tests it against null; the normal is what a world-only port
  can carry instead, and `Vec3::ZERO` would be a plane that is standable by no test and
  grounded by every `is_some()`.

- **`full_walk_move` zeroes the vertical velocity of a grounded player before anything
  else runs.** That is why `CategorizePosition`'s `NON_JUMP_VELOCITY` test can only ever
  be reached from the air: the only way to arrive there rising is to have already left the
  ground, which is what the jump does one line before. A test that sets a rising velocity
  on a grounded player and expects to lose the ground is testing a state the function
  cannot be in.

- **`set_ducked_eye_offset` splines the fraction twice.** Both callers pass
  `SimpleSpline( fraction )` and it applies `SimpleSpline` again
  (`gamemovement.cpp:4710`). Ported as written: it is the shape of the shipped crouch.

- **The duck timer counts *down* from 1000 ms and the transition is 400.**
  `GAMEMOVEMENT_DUCK_TIME` is the timer's full value, not the duration;
  `TIME_TO_DUCK_MSECS` is the duration, and reading the wrong one gives a crouch two and a
  half times too slow. It is also decremented in **whole milliseconds**, so a 300 fps
  frame truncates to 3 ms and a crouch takes marginally longer than at 60 fps.

- **`player_move` takes `Option<&mut Tracer>` and a walking player without one does not
  move.** That is not a stub: with no map there is nothing to stand on. Noclip still flies.

- **Trace results are in the caller's frame and `Ray`'s start is the box centre.** This is
  `rustdocs/ENGINE.md`'s trace gotcha 1, and every one of this module's ~14 traces goes
  through `trace_player_bbox`, which is the only place that pairing is written down.

## Not implemented, and why

| | Why, and what unblocks it |
|---|---|
| `ExtraMouseSample` (`in_main.cpp:1246`) and the second mouse sample per frame | **Two independent reasons, and both would have to change.** It exists to recover latency: Valve builds the real command early in `_Host_RunFrame_Input`, then simulates, then samples the mouse again just before rendering (`host.cpp:4359`) so the picture uses the freshest angles. This port's `Engine::update_client` runs immediately before `Engine::render` in the same callback, so there is no staleness to recover. And it could not be done anyway: `winit` delivers one batch of events per frame and cannot be pumped re-entrantly from inside a handler, where Valve's `AccumulateMouse` re-polls the OS mid-frame — a second drain here would return `(0.0, 0.0)`. **Revisit when simulation lands between input and rendering.** |
| The view *tilt* round-trip in `AdjustAngles` | `CViewEffects` (shake, tilt, punch), which needs entities. **In scope for Portal 2**, which tilts the view; `AdjustAngles` is where it attaches, and it belongs there rather than in the renderer because tilt affects aim. |
| `view->StopPitchDrift()`, `DriftPitch` | Deleted with the pitch drift itself — it re-centres the view for keyboard-only play and `lookspring` defaults to 0. |
| Water — `CheckWater`, `WaterMove`, `WaterJump`, `CheckWaterJump`, water level and type | Needs a water level, which needs `CategorizePosition`'s water probes and the leaf water data the `.bsp` reader does not load. `full_walk_move` keeps the shape of the branch and takes the not-in-water side. Portal 2's goo is a `trigger_hurt` over a water brush, so this is a *drowning* feature more than a swimming one. |
| Ladders — `LadderMove`, `MOVETYPE_LADDER`, `OnLadder` | **Deleted, not deferred.** `CPortalGameMovement::GameHasLadders()` returns `false` (`portal_gamemovement.h:132`), so none of it is reachable in Portal 2. |
| The duck-jump machinery — `m_bInDuckJump`, `StartUnDuckJump`, `CanUnDuckJump`, `FinishUnDuckJump`, `UpdateDuckJumpEyeOffset`, `m_nJumpTimeMsecs` | **Unreachable in Portal 2**, and by Valve's choice: `CheckJumpButton` sets `bSetDuckJump = false` over a comment reading "temp fix for camera snapping when ducking in the air ( NO DUCKJUMP for now )". Nothing sets the timer, so every branch that reads it is dead. |
| `CheckStuck`, `FixPlayerCrouchStuck`, `IsMovingPlayerStuck`, `UnblockPusher` | The unstick passes. They nudge a player out of geometry they should never have been in, and every path into that state needs entities — a door closing on you, a platform rising through you. |
| `CheckFalling`, `PlayerRoughLandingEffects`, `m_flFallVelocity` | Fall damage, the landing sound and the landing animation. Needs health, sound and animation. |
| Base velocity — conveyors, moving platforms, `GetBaseVelocity` | Entities. `SetGroundEntity`'s velocity exchange goes with it, and it is the reason Valve adds and subtracts it around every move. |
| `m_outWishVel`, `m_outJumpVel`, `m_outStepHeight` | Outputs for the view's step smoothing and the animation layer. Carrying fields nothing reads would be carrying fields nothing checks; `view.cpp`'s step smoothing is where `m_outStepHeight` attaches. |
| Speed paint, bounce gel, tractor beams, portal funnelling, projected walls, `PortalFunnel`, `TBeamMove` | Paint and portals. They are why Portal's overrides generalise world `+Z` to a stick normal; that generalisation is the seam. |
| `player->m_surfaceFriction` from a real surface, `jumpFactor`, `maxSpeedFactor` | The physics surface-property database — `vphysics/`. Every surface reads as the default until then, and `surface_friction` still carries `CategorizePosition`'s 0.25. |
| `env_fog_controller`'s `farz`, which overrides `GetZFar` when positive | Entities. |
| `r_aspectratio`, and `AspectRatioInfo_t`'s non-square-pixel scalar | `r_aspectratio` is a *renderer* cvar (`gl_rmain.cpp:46`); registering it from the game client to read it in `screen_aspect` would put it in the wrong module. The pixel-shape scalar is the material system's. Both coincide with `width / height` on every square-pixel display, which is the only case this port supports. |
| `fovViewmodel`, `zNearViewmodel`, the ortho box, custom view/projection matrices, depth of field, motion blur | A viewmodel, portals, monitors and post-processing. `ViewSetup` carries eight fields where `CViewSetup` carries fifty. |
| `r_nearz` | `#ifdef _DEBUG` in the original. |
| Prediction, `MULTIPLAYER_BACKUP`, `CVerifiedUserCmd`, the command ring | Stage 5. Needs `net/` and `server/`. Keep `run_move`'s shape and it wraps rather than rewrites. |
| `UserCmd::random_seed` | It is `MD5_PseudoRandom(command_number) & 0x7fffffff`, and its only purpose is making two ends draw the same "random" numbers. A value that is not Valve's MD5 would look like it worked. Left 0 until there are two ends. |
| The wire encoding (`ReadUsercmd`/`WriteUsercmd`) | `net/`'s. The format is **not pinned yet** — per `PORTING.md` it becomes ours once both ends are Rust, and both ends will be. |
| `m_customaccel` 1-4, `m_mousespeed`, `m_mouseaccel1/2` | Per-user feel tuning with no default behaviour; the last three are Windows `SPI_SETMOUSE` overrides, inert on POSIX. |
| `cl_mouselook_roll_compensation` | Rotates the mouse delta by the inverse of the view roll so "mouse left" stays "screen left" upside down. **In scope for Portal 2**, which rolls constantly; needs something that rolls the view. `ViewAngles::roll` is where it attaches. |
| Split-screen, third-person (`in_camera.cpp`), HLTV/Replay cameras, TrackIR, force feedback, Sixense, the tool and demo view overrides | Deleted. `portdocs/CLIENT.md` §5. Portal 2 does have split-screen co-op, so that one is a deferral: one player, keep the seam, no slot field until co-op is scheduled. |


## Extending it

- **A new `+command`**: add a row to `BUTTONS` and a variant to `MoveButton`. The
  `spec()` helper's `match` is exhaustive, so the compiler asks for the two spellings; the
  engine's registration loop and `Buttons::bits` pick it up with no further change.
- **A new cvar**: add a field to `Cvars` and register it in `Client::new`, with the
  default taken from a named constant rather than a literal so the number lives in one
  place. **Verify the default, bounds and flags against `legacy/` before writing them
  down** — the `sensitivity` bound in this port was wrong for two stages because it was
  transcribed from a different Source branch.
- **A new movement mode**: add a `MoveType` variant and an arm in `run_move`. Keep the
  work in `movement.rs` and keep it reading only `MoveData` — it is shared with the
  server that does not exist yet.

## Which tests guard what

`cargo test client::` — 77 tests, no window, no GPU, no game content. Stage 4's build a
collision model with `engine::trace::fixture::Fixture` rather than loading a `.bsp`: a
room with a floor, a 16-unit step, a wall nothing can climb and a ceiling only a crouched
player fits under.

| Test | Guards |
|---|---|
| `client::button::the_table_is_indexed_by_its_own_enum` | `BUTTONS` stays in `MoveButton` order and both spellings match the name |
| `a_button_held_for_the_whole_frame_is_worth_one`, `a_tap_shorter_than_a_frame_is_worth_a_quarter`, `a_release_and_a_re_press_in_one_frame_is_worth_three_quarters`, `releasing_is_worth_nothing_for_the_frame_it_happens_in` | all four `KeyState` cases |
| `a_tap_that_key_state_already_read_does_not_reach_the_bitfield` | gotcha 4 — the read order |
| `two_keys_bound_to_one_command_do_not_cancel_each_other` | `down[2]`, and why `+command` carries an index |
| `a_bare_minus_command_releases_unconditionally` | the way out of a stuck key |
| `the_axis_only_buttons_contribute_no_bits` | the six modifiers that never reach the server |
| `client::movement::without_acceleration_the_wish_velocity_is_the_velocity` | the arithmetic, exactly: 175 × 5 = 875 units in one second |
| `walking_halves_the_speed` | `+speed` halves the factor *after* the clamp is computed from the unhalved one |
| `the_wish_velocity_is_clamped_to_the_server_maximum` | the noclip ceiling — `mv.max_speed × sv_noclipspeed` |
| `rising_is_along_world_up_whatever_the_view_is_doing` | `upmove` on world `+Z`, not along `up` |
| `with_acceleration_the_first_frame_is_slower_than_the_steady_state`, `releasing_everything_coasts_to_an_exact_stop` | gotcha 7 — the shipped defaults, and the `speed < 1.0` exact stop |
| `acceleration_does_not_add_to_a_velocity_that_already_exceeds_the_wish` | `Accelerate`'s veer clause |
| `client::view::positive_pitch_looks_down`, `a_zero_angle_looks_down_positive_x` | gotchas 9 and 10 |
| `the_vertical_field_of_view_is_the_same_at_every_aspect_ratio` | **gotcha 1**, as the property rather than a number: the composition is Hor+ at 4:3, 16:10, 16:9 and 21:9, and the same test pins what the unscaled value would have been (46.7°) |
| `a_widescreen_view_is_wider_than_default_fov_says`, `a_four_by_three_screen_leaves_the_field_of_view_alone` | that the scaling is applied, and that it is a no-op at the aspect the number is quoted at |
| `the_far_plane_is_the_maps_diagonal_and_r_farz_overrides_it` | `GetZFar`'s two branches |
| `the_near_plane_moves_in_on_a_mega_wide_screen` | `GetZNear`'s mega-wide branch |
| `the_view_is_the_players_eye_not_its_feet` | gotcha 6 |
| `a_non_finite_angle_is_refused_rather_than_stored` | `SetViewAngles`' `IsValid` check — a NaN in the view matrix is a black screen with no error |
| `a_command_carries_the_speed_cvars_rather_than_an_axis` | gotcha 5, both halves |
| `client::movement::a_player_falls_until_it_lands_on_the_floor` | gravity, `CategorizePosition` and the landing |
| `gravity_is_six_hundred_a_second_squared` | Portal 2's gravity, and that both halves are applied |
| `the_landing_is_the_same_at_any_frame_rate` | why gravity is split in half at all — 300 Hz and 20 Hz land together |
| `walking_forward_settles_at_the_ground_speed` | 175, not `sv_maxspeed`'s 320 — the first walking gotcha |
| `releasing_forward_stops_the_player` | `Friction`, and that the stop is exact |
| `a_step_shorter_than_sv_stepsize_is_walked_up`, `a_wall_taller_than_a_step_stops_the_player` | `StepMove`, both answers |
| `a_wall_hit_at_an_angle_is_slid_along` | `TryPlayerMove`'s whole purpose |
| `a_jump_reaches_forty_five_units` | Portal's jump height against the base class's 21 |
| `a_held_jump_button_does_not_bounce` | `old_buttons` round-tripping — the second walking gotcha |
| `ducking_lowers_the_hull_and_the_eye`, `the_eye_slides_down_through_a_crouch` | `FinishDuck`, and that the transition interpolates |
| `a_ducked_player_cannot_stand_up_under_a_low_ceiling` | `CanUnduck`'s trace, and the "reset the timer" branch |
| `a_ducked_player_moves_at_a_third_speed` | `HandleDuckingSpeedCrop`, and `speed_cropped` |
| `a_ducked_player_cannot_jump` | Portal's refusal, where the base class jumps |
| `rising_rapidly_loses_the_ground` | `NON_JUMP_VELOCITY`, at the only place it is reachable |
| `air_control_is_capped_at_sixty` | Portal's cap against the base class's 30 |
| `edge_friction_slows_a_player_near_a_ledge` | that it fires over a ledge and nowhere else |
| `walking_into_things_never_ends_inside_them` | eight directions × 60 frames, asserting the hull is never in solid |
| `a_walking_player_without_a_map_does_not_move` | the `Option<&mut Tracer>` contract |
| `a_tap_does_not_overcome_noclip_friction_but_does_with_no_acceleration` | gotcha 7, and that `sv_noclipaccelerate 0` restores the old feel |
| `holding_strafe_moves_with_the_mouse_instead_of_turning`, `lookstrafe_redirects_only_the_horizontal_axis` | `ApplyMouse`'s three cases and the asymmetry between the axes |
| `cl_mouseenable_zero_drops_the_motion_rather_than_banking_it` | nothing arrives in one lump when it is turned back on |
| `turning_noclip_off_leaves_a_player_that_cannot_move_yet` | gotcha 13 |
| `jump_and_duck_drive_the_placeholder_vertical_axis` | gotcha 14, including that the button bit survives |
| `clearing_the_buttons_stops_the_player` | gotcha 12's second half |
| `the_arrow_keys_turn_the_view` | `AdjustYaw`, at `cl_yawspeed / 60` per frame and half that on the frame the key went down |
| `holding_strafe_makes_the_arrow_keys_strafe_rather_than_turn` | that `AdjustYaw` and `ComputeSideMove` are mutually exclusive on `+left`/`+right`, which is what keeps the destructive `KeyState` reads from colliding |
| `keyboard_pitch_needs_cl_mouselook_off`, `cl_mouselook_off_still_lets_the_mouse_look` | gotcha 3, both directions |
| `klook_turns_forward_and_back_into_pitch` | the other mutually-exclusive pair, and that `ComputeForwardMove` steps aside |
| `walking_turns_at_two_thirds_speed_and_moves_at_one_half` | `cl_anglespeedkey` 0.67 against `+speed`'s 0.5 |
| `the_keyboard_budget_is_spent_once_per_frame`, `without_a_refill_keyboard_look_does_nothing`, `in_usekeyboardsampletime_zero_removes_the_budget` | gotcha 2 — the budget, its silent failure mode, and the cvar that removes it |
| `engine::tests::a_bound_key_moves_the_camera_through_the_command_buffer` | the whole chain with nothing mocked: `bind` → press → command text → console → `Buttons` → `UserCmd` |
