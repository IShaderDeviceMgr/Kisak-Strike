# CLIENT.md — `src/client/`

The game client: the local player, the command that moves it, and where the eye ends up.
Held keys and mouse motion in, a `UserCmd` and a player position out.

Porting plan and the C++ inventory: [`portdocs/CLIENT.md`](../portdocs/CLIENT.md).

| | |
|---|---|
| Module | `crate::client`, with `client::{button, movement, player, tonemap, usercmd, view}` |
| Replaces | `game/client/in_main.cpp`, `in_mouse.cpp`, `view.cpp`'s `SetUpView`/`GetZNear`/`GetZFar`, `game/shared/usercmd.h`, `in_buttons.h`, `FullNoClipMove`/`FullWalkMove` from `game/shared/gamemovement.cpp` (via `portal_gamemovement.cpp`), and `CTonemapSystem` from `viewpostprocess.cpp` |
| Lines | ~5,500 including tests |
| Tests | 107 (`cargo test client::`) |
| Dependencies | `std`, `glam`, and `crate::engine::console` for cvar handles. **Not `winit`, not `egui`, not `wgpu`, not `crate::engine::input`** |
| Status | **Stages 1-4 of 5 done** (`portdocs/CLIENT.md` §8), plus the tone mapper (`portdocs/CLIENT_TONEMAP.md`) and the dead player that `server/` stage 5 brought. `client/` stage 5 waits for `net/` |

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

    pub fn player(&self) -> &Player;
    /// The **server's** way in — see `Player::base_velocity`. `Engine::frame`'s
    /// `apply_player_state` is the only caller.
    pub fn player_mut(&mut self) -> &mut Player;
    pub fn tonemap(&self) -> &ToneMap;
    pub fn tonemap_mut(&mut self) -> &mut ToneMap;
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

/// `VEC_DEAD_VIEWHEIGHT` — **14, not the multiplayer table's 60**. Portal 2
/// single player is `CPortalGameRules : CHalfLife2`, which overrides no view
/// vectors, so it gets `g_DefaultViewVectors` like every other constant here.
pub const VEC_DEAD_VIEWHEIGHT: Vec3 = Vec3::new(0.0, 0.0, 14.0);

pub enum MoveType { Walk, Noclip, FlyGravity }

pub struct Player {
    pub origin: Vec3,        // the FEET
    pub velocity: Vec3,
    /// `m_vecBaseVelocity` — what is carrying the player. **The server owns
    /// it**; see the note below.
    pub base_velocity: Vec3,
    pub angles: ViewAngles,
    /// **The server owns this too, since `server/` stage 5** — `noclip` is a
    /// `game/server/` command and `Event_Killed` writes `MOVETYPE_FLYGRAVITY`.
    pub move_type: MoveType,
    /// `m_iHealth` — the server's. Read for one thing: `IsDead()`.
    pub health: i32,
    /// `GetFlags() & FL_FROZEN` — the server's. `player_loadsaved` sets it.
    pub frozen: bool,
    pub view_offset: Vec3,
    pub ground: Option<Vec3>,      // the normal underfoot, None when airborne
    pub surface_friction: f32,
    pub ducked: bool,
    pub ducking: bool,             // mid-transition, either direction
    pub duck_time_msecs: i32,
    pub old_buttons: ButtonBits,   // what the PREVIOUS command held
    /// `m_hPortalEnvironment` — the portal this player is inside the influence
    /// of, as the same opaque key `PortalState::id` is. Written at the end of
    /// each move by [the teleport](#the-teleport--handle_portalling) and read
    /// at the start of the next by `engine/`, to decide which carved wall to
    /// trace against. `None` is the ordinary case.
    pub portal_environment: Option<u64>,
}

impl Player {
    pub fn new(origin: Vec3, pitch: f32, yaw: f32) -> Player;  // MoveType::Walk
    pub fn eye(&self) -> Vec3;                                 // origin + view_offset
}
```

#### `base_velocity` is written by the server and read here

`src/server/` stage 4 landed `trigger_push`, and a push is not a velocity: the trigger
sets `m_vecBaseVelocity` and `FL_BASEVELOCITY` **every tick it is pushing**, the movement
adds it for the duration of a move and takes it back out again — so a player carried along
a conveyor still reports a velocity of zero — and the tick *after* the push stops,
`CPlayerMove::CheckMovingGround` converts the accumulated base velocity into real velocity
with a `1 + frametime/2` boost. That last step is the server's; the two here are
`walk_move`/`air_move`'s add-and-subtract and `start_gravity`'s, which spends the
*vertical* component once and zeroes it so an upward push is an impulse rather than a
permanent anti-gravity field.

It travels both ways through `server::PlayerState`, which `Engine::frame` copies in before
the server's ticks and out after them. **Nothing in this module writes it.**

#### `move_type`, `health` and `frozen` are the server's, and they only come *in*

`server/` stage 5 gave the server authority over three more fields. `noclip` is a
`game/server/` command in the original because the move type is server state that gets
networked down; `CBasePlayer::Event_Killed` writes `MOVETYPE_FLYGRAVITY` and the health
that got it there; `CRevertSaved::InputReload` sets `FL_FROZEN`. All three arrive through
`Engine::frame`'s `apply_player_state` and **nothing in this module writes any of them** —
`Client::toggle_noclip` is gone, and `Server::toggle_noclip` is what the console command
reaches now.

What this module does with them is one function each: `player_move` dispatches on the move
type, and `check_parameters` reads the other two.

#### The dead player

`MoveType::FlyGravity` is `CGameMovement::FullTossMove` — gravity, one swept move, and a
stop. There is no clip-and-retry and no stair stepping, which is what makes a corpse feel
like a dropped object rather than like a player, and `PerformFlyCollisionResolution` zeroes
the velocity outright when it lands because a player's move-collide is
`MOVECOLLIDE_DEFAULT` rather than `MOVECOLLIDE_FLY_BOUNCE`.

`check_parameters` is where being dead and being frozen are read, and they are **two
separate `if`s that happen to overlap**:

- `FL_FROZEN || FL_ONTRAIN || IsDead()` zeroes `forwardmove`/`sidemove`/`upmove` — and
  nothing else, so a corpse that was falling keeps falling.
- `IsDead()` alone pins `mv.angles` to the previous command's (the `old_angles`
  argument), and writes `VEC_DEAD_VIEWHEIGHT` into the view offset.

**`IsDead()` is `m_iHealth <= 0`** (`gamemovement.cpp:1091`), not the life state — see
`rustdocs/SERVER.md` gotcha 53 for why the two differ and why the *health* is what crosses
the seam.

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
                        view_offset, speed_cropped,
                        move_start, portal_environment, teleported */ }

/// What a teleport did, for the caller to finish — see "The teleport" below.
pub struct Teleport {
    pub matrix: Mat4,
    pub entered: u64,
    pub exit: u64,
    pub forced_duck: bool,
}
impl Teleport {
    /// `UTIL_Portal_AngleTransform`: an angle set taken through the portal.
    pub fn turn(&self, angles: ViewAngles) -> ViewAngles;
}

/// `ShouldPortalTransitionCrouch` — does this pair turn the up axis far enough
/// that an AABB cannot make the trip standing? `engine/` asks it to decide
/// whether to give the tracer `with_exit_hull`.
pub fn transition_crouches(matrix: Mat4) -> bool;

/// `'a` is the portals' lifetime: the teleport re-attaches the tracer's hole to
/// the **exit** portal before checking whether the player came out stuck.
pub fn player_move<'a>(mv: &mut MoveData, tracer: Option<&mut Tracer<'a>>,
                       portals: Option<&'a PortalHoles>,
                       vars: &MoveVars, dt: f32, old_angles: ViewAngles);
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
| `AirMove`'s acceleration | `sv_airaccelerate`, 12 | **`sv_paintairacceleration`, 5** |
| Steering against a fast fling | free | **cancelled per axis past `MIN_FLING_SPEED`** |
| Falling towards a floor portal | nothing | **funnelled onto its axis** |

Where Portal's override differs only by generalising world `+Z` to an arbitrary "stick
normal" — its paint-gel gravity reorientation — the two are the same function with no
paint, because `m_vGravityDirection = -stickNormal` and the stick normal is world up.
Those are ported in the world-`+Z` form.

**The last three rows arrived with `portdocs/PORTAL.md` stage 5 and the first of them is
the one to read twice.** `CGameMovement::AirMove` passes `sv_airaccelerate`
(`gamemovement.cpp:2043`); `CPortalGameMovement::AirMove` passes
`sv_paintairacceleration` (`:800`), **unconditionally, with no paint anywhere in the
branch**. The constant's name is the only thing about it that is about paint, and taking
the name at face value gives a Portal 2 player 2.4x the air control the shipped game gives
them. `SV_AIRACCELERATE` is still carried by `MoveVars` because `FullTossMove` and the
rest use it; `AirMove` alone uses the other one.

The fourth difference in that function is one the port does not need a special case for
and would break by "tidying": Valve leaves the view forward **unnormalised** when it is
steeper than 30 degrees from horizontal, *"to prevent the player from screwing up their
momentum after exiting floor portals or jumping off sticky ceilings while looking straight
up/down"*. Projecting onto the horizontal plane and not renormalising shortens the
movement basis as the player looks further up or down, which is the intended damping, so
the two branches are written out rather than collapsed.

### The funnel — `portal_funnel`

`CPortalGameMovement::PortalFunnel` (`portal_gamemovement.cpp:909`) and the three
functions under it, stage 5. A player falling towards a floor portal is pulled onto its
axis so the fall goes *in* rather than clipping the rim; it is what makes Portal 2's
long drops land, and without it every fling that starts as a fall is a coin toss.

It is also **`IsFloorPortal`'s one remaining consumer on the player's path**, which is
what put it in stage 5. The other three call sites are `TeleportTouchingEntity`'s
floor-to-floor special cases — the doubled `z` compensation, the pitch reorientation and
the Bowie manoeuvre — and the player never reaches them: `CPortal_Base2D::Touch`,
`StartTouch` and `EndTouch` all `return` immediately for one (`portal_base2d.cpp:709`,
`:885`, `:944`). The fourth is the punch guard, which is `server/`'s.

Five conditions, and each is a way the funnel would otherwise fight the player:

- it runs **only below `MIN_FLING_SPEED`** (300) horizontally — past that, `AirMove` is
  cancelling the steer instead and the funnel never runs at all;
- the player must not be steering hard: `|wishdir| > 64` on either horizontal axis kills
  it;
- they must be falling fast (`velocity.z < -165`) **and looking down** (`look.z < -0.7`),
  or rising fast at a ceiling portal — *"we are more liberal about funneling into a
  ceiling portal … we aren't going to be hitting these by accident"*;
- the portal must be within 1,024 units, on the side they are heading, and for a rising
  player within the height their rise can still reach;
- and they must be inside a cone that widens from 1.5x the portal's own size at 256 units
  to 3x at 1,024.

Three things about it that read as bugs until checked against the reference:

1. **`IsCeilingPortal` is not the mirror of `IsFloorPortal`.** Both compare
   `vForward.z` against the same default `0.8` — `>` for a floor portal and `<` for a
   ceiling one (`portal_base2d_shared.cpp:879`, `:884`) — so *every* portal that is not
   in the floor is a "ceiling portal", a wall portal included, where the comment above
   the test says *"make sure it's a floor or ceiling portal"*. Ported as written.
2. **It aims 32 units in front of the portal.** Every measurement is against
   `pPortal->WorldSpaceCenter()`, and a portal's collision box runs from its plane to 64
   units in front of it, so the target for a floor portal is a spot a yard *above* the
   hole. `PortalHole::world_center` is that point.
3. **A zero sideways velocity means "will not make it"**, not "is already there":
   `AirPortalFunnel`'s guard is `if( mv->m_vecVelocity[i] )`, so a player with no drift
   at all gets the pull rather than the decay.

**The ground half is deleted with a reason.** `speed_funnelling_enabled` gates it on
`player->MaxSpeed() > sv_speed_normal`, which in Portal 2 means speed gel; with no paint
`MaxSpeed()` is `SV_SPEED_NORMAL` exactly and the branch's first line returns `false`
every time.

### The teleport — `handle_portalling`

### The teleport — `handle_portalling`

`CPortalGameMovement::HandlePortalling` (`portal_gamemovement.cpp:2214`), which is
`portdocs/PORTAL.md` §6 and the second half of its stage 4. It runs at the end of every
move, for **every** move type including noclip, and it compares where the move *started*
with where it ended.

`MoveData` grows three fields for it, and `player_move` fills the first two in itself so
that no caller has to remember to:

| Field | Is |
|---|---|
| `move_start` | `m_vMoveStartPosition` — the feet before this move ran |
| `portal_environment` | `m_hPortalEnvironment` — the portal this player is being traced against, as an opaque key |
| `teleported` | `Some` when this move ended in one; cleared at the top of every move |

#### Selecting the portal

A swept hull against every **active linked** portal's trigger box, then three filters,
then nearest-centre-wins. The filters are the interesting part and each one is a bug
somebody had:

- the **old** centre must have been in front of the plane — unless this portal was already
  the player's environment, which is Valve's *"special exception if we were pushed past the
  plane but did not move past it"*;
- if the new centre is *behind* the plane it has to be over the quad, or walking into the
  wall beside a portal would count;
- if it is in *front*, the line from the centre to its most-penetrating extent has to pass
  through the quad — *"avoids case where you can butt up against a portal side on an
  angled panel"* — within `portal_player_interaction_quadtest_epsilon` (`-DIST_EPSILON`)
  and a one-unit quad margin.

**The sweep is approximated.** `CPortal_Base2D::TestCollision` is a box sweep against the
OBB; this is the union of the hull at both ends against the same box, which can only
answer `true` more often. Every filter then runs unchanged and the trigger is exact, so
the approximation cannot teleport anyone who should not be — it can only put the player in
a portal's environment a tick early, which is the direction that fails safe.

**The selection runs whether or not anyone goes through**, because its other job is to
write `portal_environment`, and that is what the *next* move is traced against. A player
walking up to a portal is in its environment for several ticks before they cross.

#### The trigger, and the frame split

**The centre crossing the plane**, `< -FLT_EPSILON` against `m_plane_Origin` — not the
hull's near face, and not the carve's shifted plane. The crossing happened part way
through the frame, so:

```text
crossed_at      = old_plane_dist / (old_plane_dist - plane_dist)     // 0.5 if that is 0
after_crossing  = (1 - crossed_at) * frametime
```

and the `0.5` fallback is Valve's, with the bug number attached: *"sometimes fOldPlaneDist
is too [negative], some kind of physics penetration seems to be the cause (bugbait
#61331)"*.

#### The velocity

Gravity is world-down on both sides of a portal, so the part applied *after* the crossing
is taken out before the rotation and added back to the result at **1.008×** — *"Apply
slightly more gravity on exit so that floor/floor portals trend towards decaying velocity.
1.008 is a magic number found through experimentation."* At 1.0 an infinite floor-to-floor
fall gains height every cycle.

Then the exit speed range, which asks the **exit** portal
(`prop_portal_shared.cpp:201`, `:267`):

| Situation | Minimum |
|---|---|
| Player, exit facing up past 30° | **300** |
| Player, exit not on the floor but `forward.z > 0.5` | solve a quadratic for the speed that perches the hull on the portal's bottom edge, capped at 300 |
| anything else | none |

Maximum is a flat 1000. **Below the minimum, speed is *added along the exit's forward***,
not scaled — scaling would turn a sideways exit into a faster sideways exit. Above the
maximum the whole vector is scaled. Then a per-axis `sv_maxvelocity` clamp, done quietly
rather than through `CheckVelocity`.

#### The forced duck, and the move

`ShouldPortalTransitionCrouch` is `|m_matrixThisToLinked.m[2][2]| < cos 30°` — *"how much
does zUp still look like zUp after going through this portal"*. An AABB cannot rotate, so
a wall-to-floor transition has to curl the player into the duck hull **immediately**:
`FinishDuck()` now, the duck timer set, and `vOriginToCenter` recomputed against the duck
hull afterwards.

That last recomputation is the point: **the transform preserves the box's *centre*, not
its origin.** `mv.origin = matrix * centre - origin_to_center`, and reading the two the
wrong way round drops the player by the difference between the hulls.
`a_transition_that_turns_the_up_axis_ducks_the_player_as_they_cross` asserts the centre
lands where the matrix says and nothing else.

`ShouldMaintainFlingAssistCrouch` keeps that duck when leaving a portal that faces partly
up at more than `PLAYER_FLING_HELPER_MIN_SPEED` (200), and there is a companion nudge that
moves the exit centre toward the portal's axis so a flung player does not stub the hull
corner on the exit lip — *"the real world equivalent of stubbing your toe on the exit hole
results in flinging straight up."*

#### Afterwards

The environment is reassigned to the **exit** portal *before* the post-teleport
`startsolid` check, not on the next frame's touch update, so the check runs against the
right carved geometry — which is what `Tracer::set_hole` exists for. If the player did
come out stuck, one recovery sweep is made in from the portal's own axis, which is the
direction with the most room. Valve's third attempt,
`UTIL_FindClosestPassableSpace_InPortal_CenterMustStayInFront`, is a 100-iteration search
and is **not ported**.

#### The angles are the caller's

`MoveData` has no view angles — `CPlayerMove::FinishMove` does not write `mv->m_vecAngles`
back either (`player_command.cpp:232`, commented out in the original) — so the transform
comes out as `Teleport::matrix` and `Client::run_move` composes it. That is
`portdocs/PORTAL.md` §6.5's *"the whole block of angle plumbing collapses to a single
compose"*: Valve transforms four angle sets (the engine's, the prediction's, `pl.v_angle`
and the entity's) and this port has one.

It is a **compose**, not a fix-up of yaw: a pair can turn all three angles at once, and
the only way to get that right is to go through a matrix and read it back with
`crate::math::matrix_angles`. No pitch clamp — `ApplyMouse` clamps on the next command,
which is Valve's order.

#### What is not ported

- **`GetImplicitVerticalStepSpeed`** — the vertical speed a player carries implicitly
  while walking up a slope, since ground velocity is xy-only. Added before the rotation in
  the original; nothing here tracks it and it is zero except on a slope.
- **`bSkipRemoteTubeCheck`**, which a fling sets. It needs content this port cannot reach
  yet. **The transition ramp is no longer here** — stage 5 landed it: the geometry is
  `trace/`'s `CarvedWall::ramp` and the flag is `Trace::portal_ramp`, which this module
  reads at four sites through `standable` and `Trace::hit_portal_ramp`. Measured, no
  shipped pair can reach it (`rustdocs/ENGINE.md`, "The transition ramp").
- **`UnrollPredictedTeleportations`, `ApplyPredictedPortalTeleportation` and the
  `EntityPortalled` user message** — all of them reconcile a predicting client with an
  authoritative server, and this port is one process.
- **The unstick pass when the environment goes back to `None`**, which Valve does because
  *"we can't wait for it to opportunistically find a non-stuck case"*. Not reached in
  practice; it is a floating-point recovery.

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

### `ToneMap` — auto exposure

`src/client/tonemap.rs`. `CTonemapSystem` (`game/client/viewpostprocess.cpp:702`).
Porting analysis: [`portdocs/CLIENT_TONEMAP.md`](../portdocs/CLIENT_TONEMAP.md).

```rust
pub const BUCKETS: usize = 16;
pub fn bucket_bounds() -> [f32; BUCKETS + 1];        // UpdateBucketRanges

/// What the map's `env_tonemap_controller` is asking for — the thirteen
/// file-scope globals `GetTonemapSettingsFromEnvTonemapController` writes.
/// `Default` is Valve's no-controller fallback.
pub struct TonemapSettings {
    pub use_custom_auto_exposure_min: bool, pub custom_auto_exposure_min: f32,
    pub use_custom_auto_exposure_max: bool, pub custom_auto_exposure_max: f32,
    pub use_custom_bloom_scale: bool, pub custom_bloom_scale: f32,
    pub custom_bloom_scale_minimum: f32,
    pub bloom_exponent: f32, pub bloom_saturation: f32,
    pub percent_target: f32, pub percent_bright_pixels: f32,
    pub min_avg_lum: f32, pub rate: f32,
}

pub struct ToneMap;
impl ToneMap {
    pub fn new(console: &mut Console<'_>) -> ToneMap;

    pub fn set_settings(&mut self, settings: TonemapSettings);   // once per frame
    pub fn settings(&self) -> &TonemapSettings;

    pub fn scale(&mut self) -> f32;                  // UpdateMaterialSystemTonemapScalar
    pub fn measured(&mut self, counts: &[u32], dt: f32);  // DoTonemapping
    pub fn reset(&mut self, scale: f32);             // ResetToneMapping
    pub fn measuring(&self) -> bool;                 // mat_dynamic_tonemapping
    pub fn exposure_region(&self) -> (f32, f32);     // mat_exposure_center_region_x/_y
    pub fn exposure_range(&self) -> (f32, f32);      // GetExposureRange

    pub fn current(&self) -> f32;
    pub fn target(&self) -> f32;
    pub fn histogram(&self) -> &[u32; BUCKETS];
    pub fn median_luminance(&self) -> Option<f32>;
    pub fn bright_end(&self) -> Option<(f32, f32)>;  // (where it is, where it wants to be)
}
```

**It names no GPU type.** The measurement is
[`materials::histogram`](MATERIALS.md#post-processing-and-exposure)'s; this is arithmetic
over the counts it hands back. The two meet in `Engine::render`, and that is the only
place either one is driven. `ToneMap` lives on `Client` because `ResetToneMapping( 1.0 )`
runs at level load and loading a level is what reaches a `Client` — `Client::spawn` does
it, for the same reason it drops the player's velocity.

The loop, which is one thing to get right and one thing to order right:

```text
frame N     scale()  ---> cLightScale.x ---> the scene is drawn exposed
                                                     |
                                              a histogram of *that* frame
                                                     |
frame N+2   measured(counts, dt) <-------------------+
```

**The measurement is of an already-exposed frame**, so `measured` treats its answer as a
*correction* to the scale currently in force and multiplies by it
(`ComputeTargetTonemapScalar`'s "Apply this against last frames scalar"). Reading it as an
absolute exposure makes the loop oscillate instead of converge.

The `tonemap` console command prints `current`, `target`, `exposure_range`, `bright_end`,
`median_luminance` and the buckets. It is this port's own, the way `trace` is;
`mat_show_histogram` and its 200-line bar chart are not ported.

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

`ToneMap::new` registers twelve more, all `FCVAR_CHEAT` and all Valve's
(`viewpostprocess.cpp:83-135`):

| Cvar | Default | What it does |
|---|---|---|
| `mat_dynamic_tonemapping` | 1 | 0 stops measuring; the exposure stays exactly where it was, which is not the same as forcing it to 1 |
| `mat_autoexposure_min` / `mat_autoexposure_max` | 0.5 / 2 | the range the exposure may settle in |
| `mat_autoexposure_max_multiplier` | 1.0 | scales the maximum |
| `mat_hdr_uncapexposure` | 0 | replaces both ends with `0..100` |
| `mat_force_tonemap_scale` | 0.0 | above zero, pins the exposure there — and *resets* the controller onto it, so clearing it resumes from the picture rather than from wherever the controller had drifted |
| `mat_accelerate_adjust_exposure_down` | 40.0 | how much faster to darken than to brighten. **Inert below ~128 fps** — see gotcha #16 |
| `mat_exposure_center_region_x` / `_y` | 0.9 / 0.85 | the fraction of the screen the exposure is measured over |
| `mat_force_tonemap_percent_target` | -1 | overrides the 65% target. Negative means no override, and **zero is an override** |
| `mat_force_tonemap_percent_bright_pixels` | -1 | overrides the 2% |
| `mat_force_tonemap_min_avglum` | -1 | overrides the 3% median floor |

Not registered, and each for a stated reason in `tonemap.rs`'s module docs:
`mat_tonemap_algorithm` (only one algorithm is ported, so the cvar could not change
anything), `mat_show_histogram` (the overlay is not ported), `mat_fullbright` (an
engine-wide unlit mode, not a tone-mapping switch).

Commands, registered by the engine alongside its own: the 22 `+`/`-` pairs from
`BUTTONS`, plus `noclip`, `impulse` and `tonemap`.

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

### The teleport's own, added by `portdocs/PORTAL.md` stage 4

- **`portal_environment` lags one move, on purpose.** The hole a move is traced against is
  the one the *previous* move ended touching, which is why the selection runs whether or
  not anyone teleported. A caller that recomputed it from the player's current position
  each tick would be tracing against the world the player has already left.

- **The teleport trigger is the box's *centre* crossing the plane.** The hull's near face
  is past the plane for several ticks first, and during those ticks the carve is what the
  player is walking through. Testing the near face teleports them a hull-depth early.

- **`FinishDuck` is called *during* the teleport, and `vOriginToCenter` is recomputed
  after it.** The transform preserves the box's **centre**; the origin is derived back from
  it afterwards. Conflating the two drops the player by the hull difference — 18 units
  standing-to-ducked.

- **`player_move` clears `portal_environment` when there are no portals**, rather than
  leaving it. A level change would otherwise leave a stale id behind and the next map's
  carve would be picked by it.

- **The angles do not come back in `MoveData`, and the teleport is the reason that
  matters.** Every other part of the move leaves them alone, so `Client::run_move`'s one
  write — composing `Teleport::matrix` — is the only place in the port where the view turns
  without the mouse. A caller that drops `mv.teleported` gets a player who comes out of the
  exit portal facing the way they went in, which looks like the matrix is wrong.

- **`select_portal` takes the nearest centre**, so on a map with two portals close together
  the one you walked at is not necessarily the one you go through. Measured on shipped
  content: it happens. That is Valve's rule, not a bug.

### Dying's own, added by `server/` stage 5

- **`move_type`, `health` and `frozen` come *in* and never go out.** They are the
  server's. Writing one here is undone by `apply_player_state` a fraction of a frame
  later, which reads as `noclip` not working rather than as a bug in this module.
  `Client::toggle_noclip` no longer exists; `Server::toggle_noclip` is what the console
  command reaches.

- **`IsDead()` is `m_iHealth <= 0`, not the life state.** They disagree for exactly one
  server dispatch, and `PlayerState` carries the health for this reason. Asking the life
  state instead leaves a corpse that can still walk for one frame.

- **`check_parameters` takes the *previous* command's angles**, which
  `Client::create_move` captures at its very top — before `adjust_angles` has moved them.
  Taking `self.player.angles` at `run_move` time instead gives the *current* angles and
  the dead-player pin becomes a no-op you cannot see.

- **The pin does not stick, and that is Valve's.** `CheckParameters` writes
  `mv->m_vecAngles`, and `CPlayerMove::FinishMove`'s
  `player->SetLocalAngles( move->m_vecAngles )` is **commented out**
  (`player_command.cpp:232`). So a dead Portal 2 player really can still turn the camera;
  what stops them looking at anything is the three-second fade to black. The pin changes
  the movement *basis* for that command and nothing else — and `run_move` deliberately
  does not copy `mv.angles` back for the same reason.

- **The dead view offset is written twice per command, and the second one is the
  load-bearing one.** `check_parameters` writes it before `Duck()` runs, and `player_move`
  writes it again after — because `Duck()` interpolates the eye and would otherwise lift
  it back out of the corpse over 400 ms if the player died mid-crouch.

- **`VEC_DEAD_VIEWHEIGHT` is 14, not 60.** The 60 is `portal_mp_gamerules.cpp`'s
  multiplayer table, annotated "previously 14". Single-player Portal 2 gets
  `g_DefaultViewVectors`, like every other view constant in this module.

- **There is no fall damage, and that is a measurement rather than a gap.**
  `CPortalGameRules::FlPlayerFallDamage` is `{ return 0.0f; } //no fall damage in portal`
  (`portal_gamerules.h:61`). Nothing in Portal 2 can be killed by landing, whatever the
  height — which is why 34 of the game's `trigger_hurt`s carry `DMG_FALL`: the pit does
  the killing, not the fall.

### The tone mapper's own

14. **The histogram measures an already-exposed frame, so `measured` produces a
    *correction* and not an exposure.** It multiplies by the scale currently in force.
    Treating the result as absolute makes the loop oscillate rather than converge, and it
    is one line (`ComputeTargetTonemapScalar`'s "Apply this against last frames scalar").
15. **`mat_dynamic_tonemapping 0` freezes the exposure where it is; it does not reset it
    to 1.** That is Valve's behaviour and it is the difference between "stop adapting" and
    "turn HDR off". `mat_force_tonemap_scale 1` is the second one.
16. **`mat_accelerate_adjust_exposure_down` does nothing below about 128 fps.** The step
    is capped at `(1/16) * 0.25` **per frame**, and the base rate is 2 per second, so
    `rate * dt` exceeds the cap whenever a frame is longer than 1/128 s — at which point
    darkening and brightening move by exactly the same amount. Measured, and tested both
    ways. The same cap makes **adaptation frame-rate dependent** above that threshold.
17. **The moving-average weights are `|i - 5| / 5`: the oldest sample counts most and the
    *middle* one counts for nothing.** Nobody would write that on purpose and it is what
    every Source game's exposure has been smoothed with. The buffer is also scrolled
    *before* it is weighted, so the sample that lands on the zero-weight slot is the one
    that was one place newer. Do not tidy it.
18. **`bucket_bounds` are *linear-light* luminances.** Valve's comment at
    `CHistogramBucket::IssueQuery` says "gamma-space" and is stale — `dev/lumcompare.vmt`
    reads the frame buffer through an sRGB sampler. Reading them as gamma values puts the
    65% target at 0.32 linear and halves every scene.
19. **`ToneMap::scale` takes `&mut self`.** `mat_force_tonemap_scale` does not merely
    report a different number, it *resets* the controller onto it, so that clearing the
    cvar resumes from where the picture actually is.
20. **A negative `mat_force_tonemap_*` means "no override", and zero is an override.** The
    test is `>= 0.0`, so `mat_force_tonemap_percent_target 0` really does aim at 0%.
21. **The map's exposure limits win over the cvars, and `server/` is what supplies
    them.** `ToneMap::set_settings` must be called **once per frame, before
    `scale()`** — `Engine::render` does it, from `Server::tonemap_settings()` — because
    a controller's values are set by map I/O and change whenever a map says so;
    `sp_a1_intro1` changes them 0.21 seconds in, to a ceiling of 1.5 against the cvar
    default of 2. Pass `TonemapSettings::default()` when no map is loaded: that is
    Valve's fallback branch, and it is **not** the same as leaving the previous map's
    values in place. A controller value of zero is ignored rather than obeyed (Valve's
    test is `> 0.0f`), and `mat_force_tonemap_*` overrides *the map*, not the constant.
    105 of Portal 2's 106 maps place a controller; `portdocs/CLIENT_TONEMAP.md` §6 has
    the census and `rustdocs/SERVER.md` the entity.

## Not implemented, and why

| | Why, and what unblocks it |
|---|---|
| `ExtraMouseSample` (`in_main.cpp:1246`) and the second mouse sample per frame | **Two independent reasons, and both would have to change.** It exists to recover latency: Valve builds the real command early in `_Host_RunFrame_Input`, then simulates, then samples the mouse again just before rendering (`host.cpp:4359`) so the picture uses the freshest angles. This port's `Engine::update_client` runs immediately before `Engine::render` in the same callback, so there is no staleness to recover. And it could not be done anyway: `winit` delivers one batch of events per frame and cannot be pumped re-entrantly from inside a handler, where Valve's `AccumulateMouse` re-polls the OS mid-frame — a second drain here would return `(0.0, 0.0)`. **Revisit when simulation lands between input and rendering.** |
| The view *tilt* round-trip in `AdjustAngles` | `CViewEffects` (shake, tilt, punch), which needs entities. **In scope for Portal 2**, which tilts the view; `AdjustAngles` is where it attaches, and it belongs there rather than in the renderer because tilt affects aim. |
| `view->StopPitchDrift()`, `DriftPitch` | Deleted with the pitch drift itself — it re-centres the view for keyboard-only play and `lookspring` defaults to 0. |
| Water — `CheckWater`, `WaterMove`, `WaterJump`, `CheckWaterJump`, water level and type | Needs a water level, which needs `CategorizePosition`'s water probes and the leaf water data the `.bsp` reader does not load. `full_walk_move` keeps the shape of the branch and takes the not-in-water side. Portal 2's goo is a `trigger_hurt` over a water brush, so this is a *drowning* feature more than a swimming one. |
| Ladders — `LadderMove`, `MOVETYPE_LADDER`, `OnLadder` | **Deleted, not deferred.** `CPortalGameMovement::GameHasLadders()` returns `false` (`portal_gamemovement.h:132`), so none of it is reachable in Portal 2. |
| The duck-jump machinery — `m_bInDuckJump`, `StartUnDuckJump`, `CanUnDuckJump`, `FinishUnDuckJump`, `UpdateDuckJumpEyeOffset`, `m_nJumpTimeMsecs` | **Unreachable in Portal 2**, and by Valve's choice: `CheckJumpButton` sets `bSetDuckJump = false` over a comment reading "temp fix for camera snapping when ducking in the air ( NO DUCKJUMP for now )". Nothing sets the timer, so every branch that reads it is dead. |
| ~~`env_tonemap_controller`~~ | **Done** — `src/server/` stage 2. `Server::tonemap_settings` produces a `TonemapSettings` and `Engine::render` hands it over once a frame. |
| `mat_tonemap_algorithm 0` — the 31-bucket log-spaced original | Selected by matching the game directory against `{"dod", "cstrike", "lostcoast"}`, so unreachable for Portal 2, and a different bucket count *and* a different target formula. Deleted rather than deferred. |
| `SetOverrideTonemapScale` | VScript and the commentary system call it; neither exists. `mat_force_tonemap_scale` covers it from a console. |
| `DisplayHistogram` / `mat_show_histogram` | 200 lines of `Viewport` + `ClearBuffers` used as a bar chart. The `tonemap` command prints the same numbers. |
| `CheckStuck`, `FixPlayerCrouchStuck`, `IsMovingPlayerStuck`, `UnblockPusher` | The unstick passes. They nudge a player out of geometry they should never have been in, and every path into that state needs entities — a door closing on you, a platform rising through you. |
| `CheckFalling`, `PlayerRoughLandingEffects`, `m_flFallVelocity` | The landing sound and the landing animation need sound and animation. **Fall damage is neither deferred nor missing: Portal has none.** `CPortalGameRules::FlPlayerFallDamage` is `{ return 0.0f; } //no fall damage in portal` (`portal_gamerules.h:61`), and the multiplayer rules agree in words. |
| ~~Base velocity~~ | **Done** — `src/server/` stage 4's `trigger_push` writes it and the walk adds and subtracts it; see [`Player`](#player-and-movedata). What is still missing is the *conveyor* half: `FL_CONVEYOR` and `SetGroundEntity`'s velocity exchange, which need a ground **entity** rather than a ground plane. No Portal 2 entity sets `FL_CONVEYOR` — `CFuncMoveLinear::Spawn` has the one call commented out, with a name and a reason. |
| `m_outWishVel`, `m_outJumpVel`, `m_outStepHeight` | Outputs for the view's step smoothing and the animation layer. Carrying fields nothing reads would be carrying fields nothing checks; `view.cpp`'s step smoothing is where `m_outStepHeight` attaches. |
| Speed paint, bounce gel, tractor beams, projected walls, `TBeamMove`, `GroundPortalFunnel` | Paint. They are why Portal's overrides generalise world `+Z` to a stick normal; that generalisation is the seam. **The air half of `PortalFunnel` has landed** — see [the funnel](#the-funnel--portal_funnel); the ground half is gated on `MaxSpeed() > sv_speed_normal`, which without speed gel is never true. **The teleport has landed too** — see [the teleport](#the-teleport--handle_portalling). |
| `GetImplicitVerticalStepSpeed`, `bSkipRemoteTubeCheck` | [The teleport](#the-teleport--handle_portalling)'s own deferrals; each is zero except on a slope or during a fling. **The transition ramp is no longer one of them** — stage 5 landed it, in `trace/`. |
| `player->m_surfaceFriction` from a real surface, `jumpFactor`, `maxSpeedFactor` | The physics surface-property database — `vphysics/`. Every surface reads as the default until then, and `surface_friction` still carries `CategorizePosition`'s 0.25. |
| `env_fog_controller`'s `farz`, which overrides `GetZFar` when positive | Entities. |
| `r_aspectratio`, and `AspectRatioInfo_t`'s non-square-pixel scalar | `r_aspectratio` is a *renderer* cvar (`gl_rmain.cpp:46`); registering it from the game client to read it in `screen_aspect` would put it in the wrong module. The pixel-shape scalar is the material system's. Both coincide with `width / height` on every square-pixel display, which is the only case this port supports. |
| `fovViewmodel`, `zNearViewmodel`, the ortho box, custom view/projection matrices, depth of field, motion blur | A viewmodel, portals, monitors and post-processing. `ViewSetup` carries eight fields where `CViewSetup` carries fifty. |
| `r_nearz` | `#ifdef _DEBUG` in the original. |
| Prediction, `MULTIPLAYER_BACKUP`, `CVerifiedUserCmd`, the command ring | Stage 5. Needs `net/` and `server/`. Keep `run_move`'s shape and it wraps rather than rewrites. |
| **The movement moving to the server** | `portdocs/SERVER.md` stage 5 lists it and stage 5 deliberately did not do it. `CPlayerMove::RunCommand` runs on the fixed tick and `CPrediction` re-runs *this same code* on the client, so a one-process port with no `net/` already has the client half; moving it would buy a 64 Hz camera and nothing else. What stage 5 moved is the **authority** — the move type, the health, the life state — and that is the part that was actually wrong. |
| `player_speedmod`'s `SetLaggedMovementValue` and `DisableButtons` | Two more `PlayerState` fields and a multiplier on this module's `dt`, for the four `player_speedmod`s in the game. Ordinary follow-on work. |
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
- **Anything in the tone mapper**: keep `wgpu` out of `tonemap.rs`. If the measurement
  needs to change shape, change `materials::histogram` and pass the result through
  `measured`; if the *policy* needs a new input, it is a cvar or a parameter, not a
  texture. The one thing the two modules share is the bucket count, and
  `engine::tests::the_tone_mapper_s_buckets_fit_the_histogram_shader` is where that is
  checked.

## Which tests guard what

`cargo test client::` — 115 tests, no window, no GPU, no game content, plus one
depot-gated. Stage 4's build a collision model with `engine::trace::fixture::Fixture`
rather than loading a `.bsp`: a room with a floor, a 16-unit step, a wall nothing can climb
and a ceiling only a crouched player fits under. The teleport's use
`fixture::portal_rooms`, which is two rooms a thousand units apart with a linked pair
between them — see `rustdocs/ENGINE.md`.

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
| `walking_into_a_portal_comes_out_of_the_other_one` | **the test that says stage 4 works**: one teleport, out of the partner, standing on the far room's floor, still walking, and looking the way that room faces |
| `standing_in_a_portals_trigger_box_is_not_going_through_it` | the trigger being the *centre* crossing, with the hull's near face already past the plane and the environment set |
| `a_portal_whose_far_side_is_blocked_cannot_be_walked_into` | the remote trace, as the property that matters: a barrier eight units in front of the exit stops the player eight units in front of the entrance. With the control — remove the barrier and the same walk goes through |
| `a_transition_that_turns_the_up_axis_ducks_the_player_as_they_cross` | the forced duck, the duck timer, the environment handover, and that the transform preserves the box's **centre** |
| `a_player_leaves_a_floor_portal_at_three_hundred_units_a_second` | `GetExitSpeedRange`'s four answers, including the perch quadratic and the `forward.z > 0.5` gate |
| `the_quadratic_keeps_valves_degenerate_answers` | `SolveQuadratic`'s linear, all-zero and imaginary cases, which `perch_speed` relies on rather than guards against |
| `falling_towards_a_floor_portal_pulls_the_player_onto_its_axis` | **the funnel**, with the control that makes it mean something: the same fall with no portal in the level does not drift at all |
| `the_funnel_refuses_a_fling_a_steer_and_a_player_looking_up` | its three live refusals, each on its own |
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
| `client::tonemap::a_dark_frame_brightens_and_a_bright_frame_darkens` | the loop closing at all, in both directions |
| `bucket_bounds_are_valve_s_power_distribution`, `bucket_bounds_tile_zero_to_one_and_ascend` | `(i/16)^2.5` against spot values, and that the buckets tile `[0, 1]` — which is what makes the percentile's telescoped range sum exact |
| `the_target_is_a_correction_to_the_current_scale_and_not_a_replacement` | gotcha 14, as arithmetic: doubling the current scale doubles the answer |
| `the_sticky_bin_reports_the_target_exactly` | the deadband, and that it makes the correction exactly 1 |
| `the_percentile_is_linear_inside_the_bucket_it_lands_in` | the interpolation, which is the only part of `FindLocationOfPercentBrightPixels` with a wrong answer that looks plausible |
| `the_median_floor_only_ever_brightens` | the secondary target, on a scene that is on target at the bright end and dark in the middle |
| `the_exposure_range_bounds_where_it_can_settle`, `mat_hdr_uncapexposure_replaces_both_ends`, `a_minimum_above_the_maximum_widens_the_maximum` | `GetExposureRange`, all three branches |
| `mat_dynamic_tonemapping_zero_freezes_the_exposure_where_it_is` | gotcha 15 — frozen, not reset |
| `mat_force_tonemap_scale_pins_the_exposure` | that forcing is not clamped into the auto-exposure range |
| `darkening_is_faster_than_brightening`, `the_per_frame_cap_hides_the_accelerated_darkening_below_128_fps` | gotcha 16, both sides of the threshold |
| `the_moving_average_weights_are_valve_s_v_shape` | gotcha 17, including that the buffer is scrolled before it is weighted |
| `one_step_is_capped_at_a_quarter_of_a_bucket` | the per-frame cap, against a ten-second frame |
| `an_empty_histogram_leaves_the_exposure_alone`, `reset_forgets_the_history`, `reset_with_a_non_positive_scale_takes_the_middle_of_the_range` | the first frames of a level, and both arms of `ResetTonemappingScale` |
| `a_negative_force_cvar_means_no_override` | gotcha 20 — including that zero *is* an override |
| `engine::tests::the_tone_mapper_s_buckets_fit_the_histogram_shader` | the one thing `client/` and `materials/` must agree on while naming none of each other's types |
| `engine::exposure::exposure_settles_on_a_real_map` (depot-gated) | the whole loop against real content: `sp_a1_intro1` drawn, measured and corrected for 120 frames, with the histogram printed |

Added by `server/` stage 5:

| Test | Guards |
|---|---|
| `movement::a_dead_player_falls_under_gravity_and_stops_on_the_floor` | `FullTossMove` and `PerformFlyCollisionResolution` |
| `movement::a_dead_player_cannot_walk` | `CheckParameters`' `IsDead()` branch |
| `movement::a_frozen_player_cannot_walk_and_is_still_alive` | the same branch reached by `FL_FROZEN` instead, and that the two are distinct states |
| `movement::the_dead_view_drops_to_the_floor_and_duck_does_not_lift_it_back` | `VEC_DEAD_VIEWHEIGHT`, and the write order against `Duck()` |
| `movement::a_dead_players_movement_basis_is_the_previous_commands` | the `m_vecOldAngles` pin, and that a live player is unaffected |
| `server::tests::noclip_is_the_servers_and_survives_the_round_trip` | that the move type only travels one way |

**`a_player_walks_through_every_shipped_portal_pair`** is the depot-gated acceptance test
`portdocs/PORTAL.md` §11 asks for, and it is the strongest evidence stage 4 has:

```text
KISAK_GAME_DIR=/path/to/portal2 cargo test --release walks_through -- --ignored --nocapture
```

Every pair of `prop_portal`s the shipped maps place is linked with the real
`teleport_matrix`, carved with the real `PortalHoles`, and walked into by a real player
hull through the real `player_move` — with the portal plumbing `Engine::update_client`
does around it, so the tracer's hole comes from the environment the previous move ended in
and the view turns with the player.

The pairing is by entity order within a map rather than through the server's linker,
because what is under test is the movement; that every shipped pair spawns and links is
`server::tests::every_shipped_portal_spawns_and_its_map_can_link_a_pair`'s job, and running
the whole entity system here would make a failure ambiguous.

**Measured: nine pairs across the shipped maps, six walked through.** One
(`sp_a4_finale4`) is stopped short by the geometry at the far end and two
(`sp_a1_intro6`, `sp_a4_finale1`) have nowhere to stand in front of them — the second of
those being one of the four tractor-beam portals the map parks in mid-air. Nine pairs out
of twenty-one portals is the content's arithmetic: `sp_a1_intro5` and `sp_a1_intro7` place
a single `prop_portal` each and `sp_a1_intro4` places three. Between them the nine pairs
hold **590 carved pieces, 72 tube slabs and 292 remote pieces**, and carving a linked pair
takes **0.09 ms on average and 0.16 ms at worst**.

**Either portal may be the entrance**, which the test had to be taught: some maps put the
pair close enough together that a player walking at one is nearer the other, and the
selection takes the nearest centre. What it asserts is that they came out of the
*partner*, on that portal's room side of its plane.

---

## What has landed, and what each stage found

> Moved here from `CLAUDE.md`, which had grown to 2,126 lines by accumulating a
> paragraph per landed stage. This is the narrative history of the module: what
> was ported, in what order, what it cost and what the measurements said.
> `CLAUDE.md` keeps a one-line summary and points here. **The invariants and
> gotchas above are the normative part of this document**; this section is the
> record of how they were arrived at.

**`src/client/` — the game client, stages 1-4 of 5 ported, plus the dead
player `server/` stage 5 brought and the teleport `portdocs/PORTAL.md` stage 4
brought** (`portdocs/CLIENT.md`,
**`rustdocs/CLIENT.md`** — read that before calling in). The first *game* module in the
tree, and a sibling of `src/engine/` because `client.so` was a sibling of `engine.so`.
**It is not `ENGINE.md` §7.5**, which is the client *connection* (`CClientState`,
snapshot parsing), lands at `src/engine/client/` and is blocked on `net/`; the two share
a name and nothing else. Stage 1 is the input→command→movement→view spine: `UserCmd`,
`kbutton_t`'s two-holder set **with its fractional `KeyState`** (the half `input/`
deliberately refused to build against a camera), the 22 `+`/`-` buttons and their `IN_*`
bits, `FullNoClipMove` and `Accelerate`, a `Player` in `MOVETYPE_NOCLIP`, and ~19 cvars
with Valve's names, defaults, bounds and flags. **Valve's own
`// FIXME, move entirely to client .dll`** (`engine/cdll_engine_int.cpp:1048`) is taken:
the view angles are the client's and the engine never gets a copy.
Stage 2 is `CViewRender::SetUpView`: a `ViewSetup`, `GetZNear`'s mega-wide branch,
`GetZFar` from `r_farz`/`r_mapextents`, and `Engine::camera` reduced to a
`ViewSetup` → `Camera` conversion. **It also fixed a field of view that had been
quietly too narrow since the camera existed** — Source quotes FOV *horizontally at
4:3* and scales it by `aspect / (4/3)` before projecting (`view.cpp:1084`), which the
port was not doing, so 16:9 was showing a 46.7° vertical FOV where the shipped game
shows 59.8°.
Stage 3 is keyboard look — `AdjustAngles`/`AdjustYaw`/`AdjustPitch`, `cl_yawspeed`,
`cl_pitchspeed`, `cl_anglespeedkey`, `cl_mouselook` — plus `IN_SetSampleTime`'s budget.
**`ExtraMouseSample` is deliberately not ported**, and the plan was wrong to assume it
would be: the latency it recovers is not lost here (`update_client` runs immediately
before `render`, with nothing between), and `winit` gives one batch of events per frame
where Valve re-polls the OS mid-frame, so a second drain would return nothing. Revisit
when simulation lands between input and rendering.
**Stage 4 is walking**, and its headline finding is that the reference is
**`CPortalGameMovement`, not `CGameMovement`**: Portal 2 overrides two dozen of the base
class's methods and ten of the overrides change behaviour that has nothing to do with
portals. Jump height is **45 units, not 21**; the air-control cap is **60, not 30**;
ducking takes **400 ms, not CS:GO's 200**; gravity is **600, not 800**; jumping while
ducked is **refused** where the base class allows it; **edge friction** doubles friction
over a ledge and the base class has none; and walking into a standable slope **slides up
it** rather than stepping. Where Portal's override only generalises world `+Z` to a
paint-gel "stick normal", the two are the same function with no paint and the world-`+Z`
form is what is ported. Stage 4 also **found a live stage-1 bug**: a Portal 2 player's
max speed is `min(sv_maxspeed, MaxSpeed())` = **175**, not `sv_maxspeed`'s 320, so noclip
had been flying at 1600 where the shipped game flies at 875. Not ported and documented:
water, base velocity, the unstick passes — and **ladders, the duck-jump state
machine and fall damage are deleted rather than deferred**, because
`GameHasLadders()` is `false` for Portal, `CheckJumpButton` sets
`bSetDuckJump = false` over a Valve FIXME, and
`CPortalGameRules::FlPlayerFallDamage` is
`{ return 0.0f; } //no fall damage in portal` — so every branch that reads
them is unreachable and **nothing in Portal 2 can be killed by landing,
whatever the height**.
Seven rules that produce a plausible wrong answer rather than an error:
**`ViewSetup::fov` is horizontal and already width-ratio scaled**, so anything reading
`default_fov` for a projection is reintroducing that bug; **`set_sample_time` must be
called once per frame before `create_move`** or keyboard look silently does nothing for
ever; **`cl_mouselook 0` does not turn the mouse off** — it *adds* keyboard pitch, and
`cl_mouseenable 0` is the switch it gets mistaken for;
**`KeyState` is destructive and the read order matters** — the movement axes are
computed before the button bits, so a tap shorter than a frame reaches `forwardmove` and
*not* `IN_FORWARD`, and reversing them is a difference a server would see; **the first
frame after a press is worth half a frame**, so a movement number wrong by a factor of
two is usually this working correctly; **`Player::origin` is the feet** and `eye()` is
64 units higher, so conflating them reads as a level built slightly wrong; and **a `dt`
of 1.0 does not move the player at all**, because the friction bleed scales with the
frame time and a one-second step removes more speed than a second of acceleration adds.
Stage 4 adds four more: **`mv.max_speed` is 175 and not `sv_maxspeed`**, which bounds
noclip as well as walking; **`old_buttons` lives on the `Player`**, because jump and duck
both ask about the *previous* command and a `MoveData` built fresh each frame has to
round-trip it; **`speed_cropped` must start false every command** or a crouched player
moves at full speed; and **`full_walk_move` zeroes a grounded player's vertical velocity
before anything else**, so `CategorizePosition`'s "rising too fast to be on the ground"
test is only ever reachable from the air.

**The dead player landed with `server/` stage 5**, which is the one piece of
movement this module gained after stage 4. `MoveType::FlyGravity` is
`CGameMovement::FullTossMove` — gravity, one swept move and a stop, with no
clip-and-retry and no stair stepping, which is what makes a corpse feel like
a dropped object — and `check_parameters` grew the two `if`s that read the
server's state. They overlap and are **not** the same test:
`FL_FROZEN || IsDead()` zeroes the three move axes and nothing else, so a
corpse that was falling keeps falling, while `IsDead()` *alone* pins the
movement basis to the previous command's angles and drops the eye to
`VEC_DEAD_VIEWHEIGHT`.
Five rules there produce a plausible wrong answer rather than an error, and
the first two are the ones that decide whether death looks right.
**`IsDead()` is `m_iHealth <= 0`, not the life state** — they disagree for
exactly one server dispatch, which is why `PlayerState` carries the health.
**The dead view offset is written twice a command and the second one is
load-bearing**, because `Duck()` runs between them and would otherwise lift
the eye back out of the corpse over 400 ms.
**`VEC_DEAD_VIEWHEIGHT` is 14, not 60** — the 60 is the *multiplayer* table,
annotated "previously 14", and single-player Portal 2 overrides no view
vectors. **The angle pin does not stick**, and that is Valve's:
`CPlayerMove::FinishMove`'s `SetLocalAngles` line is commented out, so a dead
Portal 2 player really can still turn the camera and what stops them looking
at anything is the fade. And **`check_parameters` needs the *previous*
command's angles**, captured at the top of `create_move` before
`adjust_angles` has moved them — taking them at `run_move` time gives the
current ones and the pin becomes a no-op you cannot see.

**The teleport landed after stage 4, with `portdocs/PORTAL.md` stage 4**
(§6, and `handle_portalling` above is the reference). `CPortalGameMovement::HandlePortalling`
is 614 lines in the original and about 200 here, because four fifths of it is prediction
reconciliation and angle plumbing this port does not have: Valve transforms four angle sets
where this has one, and `UnrollPredictedTeleportations` and friends exist to make a
predicting client agree with an authoritative server. What is left is the part that is
actually geometry — select the portal, split the frame at the crossing, rotate and clamp
the velocity, force the duck, move the box's *centre* through the matrix — and it fits in
`player_move`'s tail because that is where Valve calls it from
(`portal_gamemovement.cpp:468`, between `PlayerMove` and `FinishMove`).

Three things the portdoc's §5 and §6 did not predict, all of them found by writing the
tests rather than by reading:

1. **The far side is not usually what holds the player up.** §5 says the remote trace is
   what keeps a player on the far room's floor while their box straddles the plane.
   Measured: the wall *below* the hole is still solid and a swept AABB is supported by any
   ledge it overlaps, so in the wall-to-wall case the near side catches them first. The
   fixture had to put a ledge at the far end *16 units above the hole's own lip* before the
   two answers differed at all. What the far side demonstrably does is **stop** you —
   a portal whose exit is blocked cannot be walked into — and that is the test that was
   written instead.
2. **The remote set has nothing to say until the player is nearly through.** A point `d`
   in front of the entrance images to `d` *behind* the exit, so while they approach, their
   remote box is buried in the exit's wall where the World set holds nothing. The window is
   about the hull's half-depth: one or two ticks.
3. **`select_portal` takes the nearest centre, so the portal you walked at is not always
   the one you go through.** Measured on shipped content, where a pair can be close enough
   together for it, and the depot test had to be taught to accept either as the entrance.
   Valve's rule, not a bug.

Plus one correction to the reference tree rather than to the portdoc:
**`CalculateExtentShift`'s comment does not describe its arithmetic.** It is ported as
written and `rustdocs/ENGINE.md` gotcha 28 has the measurement; it is zero for every
wall-to-wall transition anyway, which is almost all of them.

**`client/tonemap.rs` landed alongside the five stages rather than inside them**
(`portdocs/CLIENT_TONEMAP.md`, and it is `viewpostprocess.cpp`'s `CTonemapSystem`, not
the input-and-view layer `portdocs/CLIENT.md` plans). It is the **policy** half of auto
exposure — bucket boundaries, the percentile search, the moving average, the rate
limiting and twelve `mat_*` cvars — and **it names no GPU type**, the way
`materials/histogram.rs` names no cvar; the two meet only in `Engine::render`. The
finding that decides the whole calibration is that **the histogram measures linear
light, not gamma**: `dev/lumcompare.vmt` leaves `$LINEARREAD_BASETEXTURE` unset so
`screenspace_general` reads the frame buffer through an sRGB sampler, and Valve's own
comment at `IssueQuery` says the opposite and is stale — reading the boundaries as gamma
puts the 65% target at 0.32 linear and halves every scene. Four more that produce a
plausible wrong answer rather than an error: **the measurement is of an
already-exposed frame**, so the result is a *correction* to the current scale and
multiplying is what makes the loop converge rather than oscillate; **the moving-average
weights are `|i - 5| / 5`**, so the oldest sample counts most and the middle one counts
for nothing, which is absurd and is what every Source game has been smoothed with;
**the step is capped per frame and not per second**, which makes adaptation frame-rate
dependent above ~128 fps and renders `mat_accelerate_adjust_exposure_down` inert below
it; and **`mat_dynamic_tonemapping 0` freezes the exposure where it is** rather than
resetting it to 1. Deleted rather than deferred: `mat_tonemap_algorithm 0` (selected by
a game-directory match against `{dod, cstrike, lostcoast}`, so unreachable),
`SetOverrideTonemapScale`, and `DisplayHistogram`'s 200-line bar chart — the `tonemap`
console command prints the same numbers. **`env_tonemap_controller` was its one
measured gap and `server/` stage 2 closed it**: the thirteen file-scope globals
`GetTonemapSettingsFromEnvTonemapController` writes became
`client::tonemap::TonemapSettings`, which the server fills in and `Engine::render`
hands over once a frame — 105 of Portal 2's 106 maps place a controller, and
`sp_a1_intro1` now gets the ceiling of 1.5 it asks for. **One Valve bug deliberately
not reproduced**: the no-controller fallback resets every custom flag *except*
`g_bUseCustomAutoExposureMin`, so a custom minimum is sticky for the rest of the level;
`TonemapSettings::default` resets all of them.

---

## Warts that were resolved, and what resolved them

> Moved here from `CLAUDE.md`'s "Known warts" list, which is for *live*
> compromises. These three are closed: they are kept because each records a
> decision that would otherwise be re-litigated, and the last one records a real
> divergence that the move fixed. The third is `console/`'s rather than this
> module's and is kept with the other two because all three are the same story —
> a thing living in the wrong module until the right one existed.

**Resolved:** the **view angles and the free-fly camera** used to live in
`src/engine/input/view.rs`, to be moved "to `client/` when it exists". `client/` stage 1
is that, and the file is deleted rather than moved: `ViewAngles` is the client's,
`MoveButtons` became `Buttons` with the fractional `KeyState` the wart said not to build
against a camera, and `FlyCamera` became a `Player` in `MOVETYPE_NOCLIP` moved by
`FullNoClipMove`. **The one placeholder that outlived it is also gone**: `+jump` and
`+duck` used to drive the vertical axis, because `ComputeUpwardMove` reads
`+moveup`/`+movedown` and Portal 2 binds neither, so without the hack a noclip player
could not rise. Stage 4 made walking real, which makes jump and duck buttons; a noclip
player now flies up the way the shipped game does it, by looking up and holding forward.
`bind SPACE +moveup` brings the axis back.

**Resolved:** **`noclip` used to be registered by the game client and it is a *server*
command.** Move type is server state that gets networked down, so `ConCommand noclip`
lives in `game/server/` in the original; with one process and no server it had to live
somewhere, and `src/client/` was where the move type was. The condition this wart
recorded was exact — "`portdocs/SERVER.md` stage 5, where the move type becomes the
server's state rather than a field on `client::Player`" — and that is what happened.
`Server::toggle_noclip` is the command, `PlayerState::move_type` carries the answer back
*to* the client, and `Client::toggle_noclip` is deleted. `god`, `kill` and `hurtme` came
with it, because they are its neighbours in `game/server/client.cpp`. The other half of
the prediction — "where the movement itself moves" — deliberately did **not** happen; see
`portdocs/SERVER.md` stage 5 for why.

**Resolved:** `CommandLine` used to live in `src/launcher/` and be read from
`src/engine/window/`, to be moved "when a third subsystem needs it". `console/` was that
third subsystem — `stuffcmds` and the `+<cvar>` default seeding both read it — so it now
lives at `src/cmdline.rs`. The move also fixed a real divergence: `CCommandLine::ParmValue`
refuses a value beginning with `-` or `+` (`tier0/commandline.cpp:646`) and the port's
`value()` did not, which would have had `-window` swallow `+map`.
