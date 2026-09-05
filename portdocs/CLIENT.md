# Porting the game client → `src/client/`

The local player: the thing that turns held keys and mouse motion into a `CUserCmd`,
runs that command through movement, and hands the resulting eye position and angles to
the renderer. `game/client/in_*.cpp`, `game/client/view.cpp`, `game/shared/usercmd.h`
and the movement half of `game/shared/gamemovement.cpp`.

Read `PORTING.md` first; this doc assumes its standing decisions. Read
`portdocs/ENGINE_INPUT.md` §4.4–§4.6 as well — this module is where that document's
three deliberate deferrals land, and it says why they were deferred rather than
attempted.

**Status: stages 1-4 of 5 are done** (§8). `src/client/` exists — 3,600 lines, 77 tests — and
`src/engine/input/view.rs` is deleted, which closes the view-angles wart `CLAUDE.md` and
`ENGINE_INPUT.md` §11.2 recorded. **The API reference is `rustdocs/CLIENT.md`; read that
to *use* the module.** Stage 5 waits for `net/`.

---

## 0. Headline decisions

1. **There are two "clients", and this is the other one.** `ENGINE.md` §7.5 maps
   `engine/cl_main.cpp`, `baseclientstate.cpp` and `client.cpp` onto `client/` — that is
   the *connection*: signon states, snapshot parsing, the netchannel's view of a server.
   It is blocked on `net/` and is not what the boot path wants next. This doc is the
   **game client** — Valve's `client.so`, the local player and the view — which is
   blocked on nothing. They become **`src/client/` (this) and `src/engine/client/` (that,
   later)**. §1.
2. **View angles move here and the engine-side copy is never created.** Valve stores them
   in `CClientState` and reaches them from the client DLL through
   `engine->GetViewAngles`/`SetViewAngles`, over a comment that reads
   `// FIXME, move entirely to client .dll` (`engine/cdll_engine_int.cpp:1048`). There is
   no DLL boundary here to force the split, so the FIXME is simply taken. §4.7.
3. **`CUserCmd` is this module's product, and stage 1 builds it for real** — not as
   scaffolding for a network layer that does not exist, but because it is the only thing
   that makes `kbutton_t`'s fractional `KeyState` mean anything. A command carries *how
   much of the frame* a key was held; a camera reading `is_down()` cannot use that, which
   is exactly why `ENGINE_INPUT.md` §4.4 refused to build it against one. §4.2, §4.3.
4. **The player starts as `MOVETYPE_NOCLIP`, not as a camera.** Noclip is a real Source
   movetype with real cvars and a real function (`FullNoClipMove`,
   `gamemovement.cpp:2525`); flying around a level you cannot collide with is not a
   placeholder for a player, it *is* a player in the one movetype that needs no collision.
   This deletes the placeholder rather than replacing it, and it makes stage 1 faithful
   instead of approximate. `FullWalkMove` waits for `trace/`. §4.5.
5. **No prediction, and no `IPrediction`-shaped seam built ahead of one.** Single-player
   Portal 2 still runs a listen server, so prediction is eventually real — but with no
   server there is nothing to reconcile against, so the command runs immediately and once.
   Keep the *ordering* (command, then movement, then view) so prediction can wrap it;
   do not build `CPrediction`'s ring buffer against nothing. §4.8.
6. **Deleted outright:** split-screen (~all `FOR_EACH_VALID_SPLITSCREEN_PLAYER` /
   `PerUser` indirection), the third-person orbit camera (`in_camera.cpp`), HLTV and
   Replay cameras, TrackIR, force feedback, Sixense, the IFM/tool view overrides and the
   demo view override. §5.
7. **The one number this module must not inherit from CS:GO:** `default_fov` is **75**
   for Portal (`clientmode_portal.cpp:32`) and 90 for CS:GO
   (`clientmode_csnormal.cpp:136`). `PORTING.md`'s standing warning about CS:GO-shaped
   defaults in shared systems applies here more than anywhere.

---

## 1. Scope: the two-clients problem, and which one this is

`ENGINE.md` §7.5 is titled "Client connection & state" and claims the module name
`client/`. Everything it lists is `engine/`-side: `CClientState` and `CBaseClientState`,
signon handshakes, entity delta parsing, the clock-drift manager, `IVEngineClient`'s
host. None of it can be written before `net/`.

Everything *this* doc lists is `game/client/`- and `game/shared/`-side: the local
player's buttons, view angles, command construction, movement and view setup. None of it
needs `net/` at all — in Valve's build it needs the engine only as a place to keep two
`QAngle`s and a tick number.

They are different modules with the same name, so:

| | Valve | Rust | Blocked on | Doc |
|---|---|---|---|---|
| the game client | `client.so` (`game/client`, `game/shared`) | **`src/client/`** | nothing | **this** |
| the client connection | `engine/cl_*.cpp`, `client.cpp` | `src/engine/client/` | `net/` | `ENGINE.md` §7.5 |

`src/client/` is top-level, a sibling of `src/engine/` and not a child of it, because
`client.so` was a sibling of `engine.so`. That is `PORTING.md`'s module rule applied
literally, and it is the first time a *game* module appears in the tree — `src/server/`
will follow it there, not into `src/engine/`.

**Naming discipline, because the collision is real:** in prose call them *the game
client* and *the client connection*. A bare "the client" in a commit message or a comment
will be read as the wrong one within a month.

### What this module is not

It is not the client *DLL* in the sense of `cdll_client_int.cpp`'s `CHLClient` — the
88-method `IBaseClientDLL` (`public/cdll_int.h:878`, 378 virtuals across the file) that
the engine calls into. That interface exists to cross a `dlopen` boundary that
`PORTING.md` deletes. Its methods become ordinary calls or disappear; none of them
becomes a trait method for its own sake.

It is also not the whole of `game/client/`. That directory is **443,852 lines** of
`.cpp` (340,305 excluding `cstrike15/`, of which `portal/` + `portal2/` are 111,303), and
`game/shared/` is another 212,841. Almost all of it is entities, weapons, HUD panels,
particle effects and the rest of a shipped game. This module is the ~4,000-line spine
that the boot path runs through, and the rest arrives with the systems that need it.

---

## 2. Inventory

Only the files this module ports. Line counts are `wc -l` at time of writing.

### `game/client/` — the input and view layer

| File | Lines | Disposition |
|---|---|---|
| `in_main.cpp` | 2,102 | **The core.** `kbutton_t`, `KeyState`, `AdjustAngles`, `Compute*Move`, `CreateMove`, `GetButtonBits`. Split-screen and third-person strip roughly half of it. |
| `view.cpp` | 1,453 | `CViewRender::SetUpView` (`:668`), `GetZFar`, the pitch drift (`:403`, `:415`). Stage 2. |
| `in_mouse.cpp` | 843 | Accumulate/scale/apply. **Partly ported already** into `input/view.rs`'s `ViewAngles::apply_mouse`; this module takes it back and finishes it. |
| `in_joystick.cpp` | 2,016 | Deferred with `input/` stage 5 (`gilrs`). |
| `in_camera.cpp` | 1,079 | **Deleted.** Third-person orbit camera. |
| `in_forcefeedback.cpp` | 833 | **Deleted.** X360 rumble. |
| `in_steamcontroller.cpp` | 282 | Deferred with Steam. |
| `in_trackir.cpp` | 226 | **Deleted.** |
| `viewrender.cpp` | 9,931 | **Not this module.** It is the render pipeline (`render/`, `ENGINE.md` §7.16), and `src/materials/` already owns its replacement. Only `CViewSetup`'s *contents* matter here. |

### `game/shared/` — the command and the movement

| File | Lines | Disposition |
|---|---|---|
| `usercmd.h` | 331 | `CUserCmd`. Ported as a struct; the delta encoder is not (§7). |
| `usercmd.cpp` | 582 | `ReadUsercmd`/`WriteUsercmd` — bit-packed delta for the wire. **Deferred to `net/`.** |
| `in_buttons.h` | 64 | The `IN_*` bit set. Ported as `bitflags`-shaped constants; **Portal 2 adds three bits**, §7. |
| `gamemovement.cpp` | 5,357 | Stage 1 takes `FullNoClipMove` (`:2525`) and the frame of `ProcessMovement` (`:1325`) / `CheckParameters` (`:1137`). `FullWalkMove` (`:2287`) is stage 4 and needs `trace/`. |
| `gamemovement.h`, `igamemovement.h` | 342 + 139 | `CMoveData` and the `IGameMovement` interface. The interface is deleted; `CMoveData` survives as the in/out struct, which is what it is. |
| `movevars_shared.cpp` | — | The `sv_*` movement cvars. Stage 1 registers the seven it uses. |

### The engine-side call sites this module replaces

Not ported *here*, but they are the frame ordering (§4.1) and must be read:
`engine/host.cpp:3272` (`_Host_RunFrame_Input`), `engine/cl_main.cpp:2648`
(`CL_ExtraMouseUpdate`) and `:2734` (`CL_Move`), `engine/cdll_engine_int.cpp:1049`
(`GetViewAngles`) and `:2931` (`ClientDLL_ProcessInput`).

**In scope for stages 1–3: roughly 3,000 lines of C++, landing as well under 1,500 lines
of Rust.** Stage 4 (walking) roughly doubles it and is gated on a module that does not
exist.

---

## 3. Dependency graph

**Inbound — who calls the game client:**

- `engine/mod.rs`'s frame, at exactly one point, replacing `Engine::update_view`. It
  hands over a frame time, the drained input, and gets back a view.
- `engine/mod.rs`'s `camera()`, which stops computing a view and starts asking for one
  (stage 2).
- `console/`, through the command registry: `+forward`, `-forward` and their eighteen
  siblings; `noclip`; the movement and view cvars.

**Outbound — what the game client needs:**

- `input/` — for `Button`, and for nothing else. The `+`/`-` commands arrive through the
  **command buffer**, not through the input queue, which is what keeps the two modules
  from naming each other (`ENGINE_INPUT.md` §8.3). The mouse delta arrives as two floats.
- `console/` — `Cvar` handles for the ~20 cvars in §8, and `Command` registration.
- `materials/` — `Camera`, at the seam where the view is handed to the renderer. Stage 2.
- `world/` — the spawn point at load; later, `trace/` for stage 4.
- **Not `net/`, not `server/`, not `host/`.**

This is a leaf. Nothing in the tree depends on it yet, which is what makes it safe to
build before the connection exists.

---

## 4. The architecture you need in your head

### 4.1 The frame: where the client sits in it

`_Host_RunFrame_Input` (`host.cpp:3272`), in order:

1. `ClientDLL_ProcessInput()` → `CHLClient::HudProcessInput` → the HUD and the client
   mode get first refusal on input.
2. `Cbuf_Execute()` — the command buffer runs, so `+forward` has set `in_forward` before
   anything reads it.
3. `CL_Move()` (`cl_main.cpp:2734`) → `g_ClientDLL->CreateMove( nextcommandnr,
   host_state.interval_per_tick - accumulated_extra_samples, !cl.IsPaused() )` → the
   command is built → `CL_SendMove()` if it is time to send a packet.

Then the server frame, then rendering.

**The port already has this ordering.** `Engine::frame` drains input, dispatches
bindings, runs `Console::run` (which is `Cbuf_Execute`), and only then calls
`update_view`. Stage 1 replaces the third step with `Client::create_move` plus
`Client::run_move` and changes nothing about the first two. That is not a coincidence —
it is why `ENGINE_CONSOLE.md` put `Cbuf_Execute` inside the frame instead of in the
window loop.

**The second sample point is the non-obvious part.** `CL_ExtraMouseUpdate`
(`cl_main.cpp:2648`) is called again later in the frame (`host.cpp:4359` and `:4494`),
after simulation and before rendering, and it calls `CHLClient::ExtraMouseSample`, which
builds *another* command from a fresh mouse delta. Valve's comment says why:

> The mouse is always simulated for the current frame's time. This makes updates smooth in
> every case. Continuous controllers affecting the view are also simulated this way but
> they have a cap applied by `IN_SetSampleTime()` so they are not also simulated during
> input gathering.

So the mouse is sampled at the **frame** rate for smoothness, while everything
*continuous* — keyboard look, controller look — is sampled once per tick and would
otherwise be applied twice. The cap is a small budget: `IN_SetSampleTime`
(`in_main.cpp:861`) sets `m_flKeyboardSampleTime` to the frame time, and
`DetermineKeySpeed` (`:877`) consumes it —

```c
frametime = MIN( user.m_flKeyboardSampleTime, frametime );
user.m_flKeyboardSampleTime -= frametime;
```

— returning 0 once it is spent, which makes `AdjustAngles` return early. **This is the
kind of odd-looking special case `PORTING.md` says to keep the knowledge of**: it is not
an optimisation, it is the correctness fix that stops a held `+left` from turning twice
per frame. It only matters once there is a second sample point, so it is stage 3; the
design must leave room for it rather than assuming one command per frame forever.

### 4.2 `CUserCmd` is the client's only output

`usercmd.h`. The fields that matter for a single player with no netcode:

| Field | What it is |
|---|---|
| `command_number`, `tick_count` | Identity and time. Meaningful to prediction and the wire; carried from stage 1 anyway, because they cost nothing and their absence is what makes a later prediction port a rewrite. |
| `viewangles` | Where the view points *for this command*. |
| `forwardmove`, `sidemove`, `upmove` | **Intended velocities in units/sec**, not axes in [-1, 1]. `cl_forwardspeed` is baked in here, on the client. |
| `buttons` | The `IN_*` bit set. |
| `impulse`, `weaponselect`, `weaponsubtype` | Latched and cleared per command. |
| `mousedx`, `mousedy` | The raw delta, `short`. Kept for the server's benefit; nothing local reads it. |
| `random_seed` | `MD5_PseudoRandom( sequence_number )` — the shared-random seed that makes client and server draw the same "random" numbers. |

Portal 2 adds four (`#if defined( PORTAL2 )`): `player_held_entity`,
`held_entity_was_grabbed_through_portal`, `command_acknowledgements_pending` and
`predictedPortalTeleportations`. They exist because **Portal 2's grab code lives on the
client** so co-op can predict it, and because a portal teleport changes the view angles
underneath a command that is already in flight. None are stage 1; all four are why
`CUserCmd` cannot be quietly narrowed to "what movement reads".

The Rust type is a plain `#[derive(Clone, Copy, Default)] struct UserCmd`. The virtual
destructor, the hand-written `operator=`, the CRC and the split-screen array are all
artefacts of the C++ and go.

### 4.3 `kbutton_t` and the fractional `KeyState` — the algorithm that has been waiting

`in_main.cpp:424` (`KeyDown`), `:460` (`KeyUp`), `:813` (`KeyState`).

A button is `down[2]` — the codes of up to two keys holding it — plus a three-bit
`state`: `1` down, `2` impulse-down (pressed since last read), `4` impulse-up (released
since last read). `KeyState` collapses those into **the fraction of the frame the button
was held**:

| impulse-down | impulse-up | down | value | meaning |
|---|---|---|---|---|
| yes | no | yes | 0.5 | pressed this frame, still held |
| yes | no | no | 0.0 | pressed and gone |
| no | yes | — | 0.0 | released this frame |
| no | no | yes | 1.0 | held throughout |
| yes | yes | yes | 0.75 | released and re-pressed |
| yes | yes | no | 0.25 | pressed and released within the frame |

and clears the impulse bits (`data.state &= 1`). `ComputeForwardMove` then multiplies:
`cmd->forwardmove += cl_forwardspeed.GetFloat() * KeyState( &in_forward )`.

**This is why a 30 fps frame does not swallow a tap**, and it is why the port's current
`KButton::is_down()` is a placeholder rather than a simplification. `input/`'s existing
`KButton` already has the `down[2]` half — with the improvement that `Option<i32>` gives
a real empty instead of Valve's `0` — and deliberately omits the impulse bits. Stage 1
adds them.

**The read is destructive.** `KeyState` clears impulses, so calling it twice in a frame
gives different answers. That is fine in Valve's design because `CreateMove` calls each
one once, and it is a trap in any design where two consumers ask. Keep the single
consumer.

`GetButtonBits( bool bResetState )` (`:1771`) is the same idea for the bitfield: a button
contributes its bit if `state & 3` (down *or* pressed-since-last-read, so a tap inside one
frame still registers), and `bResetState` clears the impulse bit afterwards.
`m_nClearInputState` (`:1864`, `ClearInputButton`) is the "must be re-pressed" mask, used
when the game takes input away and gives it back — the same family of problem as
`input/`'s key-up latch, and it should be ported with it in mind.

### 4.4 Movement is computed in three places, and only one of them is here

A common misreading of Source: "the client sends an axis, the server does the movement".
It does not.

1. **The client turns keys into velocities** (`ComputeForwardMove` etc.,
   `in_main.cpp:1051`–`:1160`) using `cl_forwardspeed`/`cl_sidespeed`/`cl_backspeed`
   (**175 under `PORTAL2`**, 450 for every other game — `in_main.cpp:56`/`:58`) and
   `cl_upspeed` (320). These are `FCVAR_CHEAT` client cvars that decide how fast you
   *ask* to move.
2. **`CheckParameters` clips the ask** (`gamemovement.cpp:1137`) against
   `mv->m_flMaxSpeed`, the player's actual max speed — and **skips the clip entirely for
   `MOVETYPE_NOCLIP`, `MOVETYPE_ISOMETRIC` and `MOVETYPE_OBSERVER`**, which is the first
   reason noclip is a clean stage 1.
3. **`FullWalkMove`/`FullNoClipMove` turn the ask into a position**, with acceleration,
   friction and (for walking) collision.

`ScaleMovements` (`in_main.cpp:1161`) looks like a fourth and is not: its body is
`return;` above a commented-out block, under a `// FIXME FIXME: This doesn't work`.
**Do not port it, and do not "fix" it** — the clip it was going to do is (2), which
already happens in the right place.

### 4.5 `FullNoClipMove`, faithfully

`gamemovement.cpp:2525`, reached from `PlayerMove` at `:5093` with
`FullNoClipMove( sv_noclipspeed.GetFloat(), sv_noclipaccelerate.GetFloat() )` — both
default **5** (`movevars_shared.cpp:24`, `:25`), with `sv_maxspeed` 320 (`:29`) and
`sv_friction` 5.2 (`:44`).

```
maxspeed = sv_maxspeed * factor           // computed from the UNHALVED factor
if buttons & IN_SPEED: factor /= 2
fmove = forwardmove * factor;  smove = sidemove * factor
wishvel = forward*fmove + right*smove;  wishvel.z += upmove * factor
clamp |wishvel| to maxspeed
if maxacceleration > 0: Accelerate(); then friction bleed
else: velocity = wishvel
```

Three details the port's current `FlyCamera::step` gets right and one it drops:

- **`maxspeed` is computed before `+speed` halves the factor**, so walking never reaches
  the clamp. Kept.
- **`upmove` goes on world `+Z`**, after the forward/right terms, so looking down does not
  tilt which way "up" is. Kept.
- **`AngleVectors` is the same basis the view uses.** Kept.
- **The acceleration branch is dropped.** `FlyCamera` takes the `maxacceleration <= 0`
  path unconditionally and documents the choice ("a camera with momentum is harder to aim
  a screenshot with"). Once this is a *player* rather than a camera that is no longer the
  call to make: `sv_noclipaccelerate` is 5, not 0, so the shipped game accelerates, and
  the friction bleed uses `sv_friction * player->m_surfaceFriction` and
  `gpGlobals->frametime`. **Stage 1 restores it**, keeps `sv_noclipaccelerate` as the
  cvar it is, and anyone who wants the old feel sets it to 0 — which is exactly what the
  cvar is for.

`Accelerate` is `gamemovement.cpp`'s shared helper and is needed by walking too; port it
once, in the stage where noclip needs it.

### 4.6 The view: `SetUpView`, and what a `CViewSetup` actually holds

`CViewRender::SetUpView` (`view.cpp:668`), called from `OnRenderStart` (`:488`) once per
frame per split-screen slot. Stripped of split-screen, HLTV, Replay, demo override, tool
framework and the CS:GO observer interpolation, it is:

```
view.zFar = GetZFar();  view.zFarViewmodel = zFar
view.zNear = GetZNear(); view.zNearViewmodel = 1
view.fov = default_fov.GetFloat()
pPlayer->CalcView( view.origin, view.angles, view.zNear, view.zFar, view.fov )
GetClientMode()->OverrideView( &view )
view.fovViewmodel = GetClientMode()->GetViewModelFOV() - (GetDefaultFOV() - view.fov)
ComputeCameraVariables( view.origin, view.angles, &forward, &right, &up, &matCamInverse )
```

The parts worth carrying:

- **`GetZFar` is map-derived, not constant.** If `r_farz < 1` (the default) the far plane
  is `r_mapextents * 1.73205080757` — the map's half-extent times √3, i.e. the diagonal of
  the cube — overridable per-player by the fog controller's `farz`. The port currently
  hard-codes `VIEW_FAR_Z = 16384.0 * 1.732_050_8`, which is that formula with
  `r_mapextents`' default substituted. Stage 2 makes it the formula.
- **`CalcView` is where the eye comes from**, and it is *not* the player's origin: the eye
  is origin + the view offset, which is `VEC_VIEW` = `(0, 0, 64)` standing and
  `VEC_DUCK_VIEW` = `(0, 0, 28)` ducked (`gamerules.cpp:38`, with hulls
  `(-16,-16,0)`–`(16,16,72)` and `(-16,-16,0)`–`(16,16,36)`). Then view bob, view roll,
  punch angle and aim punch are *added*. A stage-1 noclip player has no bob, no punch and
  no duck, so the eye is origin plus 64 — but the offset is the thing to model, not the
  constant.
- **`default_fov` is 75 in Portal** (`clientmode_portal.cpp:32`, `FCVAR_CHEAT`) and the
  port already uses 75. Keep the value; make it the cvar.
- **`ComputeCameraVariables`' output is what `src/materials/`'s `Camera` already is.**
  The matrix stack it feeds is deleted (`rustdocs/MATERIALS.md`); the basis is
  `AngleVectors`, which `input/view.rs` already ports.

`C_Portal_Player::CalcView` (`c_portal_player.cpp:2772`) is the Portal 2 override and is
**900 lines of portal-specific eye interpolation** — `UpdatePortalEyeInterpolation`,
`m_bEyePositionIsTransformedByPortal`, the taunt camera. It is in scope for the target
game and out of scope for every stage in §8. Read it before designing the eye as a plain
`origin + offset` field, so the seam it needs later is not closed.

### 4.7 Where the view angles live, and Valve's own answer

`CInput::AdjustAngles` (`in_main.cpp:1006`) reads and writes the angles through
`engine->GetViewAngles`/`SetViewAngles`. Those resolve to `GetLocalClient().viewangles`
(`cdll_engine_int.cpp:1049`, `:1054`) — `CClientState::viewangles`, `engine/client.h:193`
— i.e. the angles are stored in the *engine*, in the module this doc is explicitly not,
and only the client DLL ever mutates them.

The comment immediately above the getter is:

```c
// FIXME, move entirely to client .dll
void CEngineClient::GetViewAngles( QAngle& va )
```

**Take the FIXME.** The reason it was never taken is the DLL boundary: the engine needed
the angles for demo playback, HLTV and the `addangle` queue, all of which are either
deleted or later modules here. So `src/client/` owns `ViewAngles` outright, `input/` stops
owning them, and `src/engine/client/` — when it arrives with `net/` — asks the game client
rather than storing a second copy.

Two behaviours from the setter are worth keeping even without the interface:
`SetViewAngles` **normalizes each component** (`AngleNormalize`) and **rejects a
non-finite angle** with a warning, zeroing rather than propagating a NaN. The port's
`ViewAngles::clamp` already wraps yaw and documents that it is a divergence from Valve;
it is not — the wrap is Valve's, it just lives on the other side of this call. Fold the
two together and the divergence note goes away.

### 4.8 Prediction, and why it is not a seam yet

`CPrediction` (`game/client/prediction.cpp`) re-runs the last N commands against the last
server snapshot every frame so the local player moves without waiting for a round trip.
`CUserCmd::hasbeenpredicted`, `MULTIPLAYER_BACKUP`, `CVerifiedUserCmd` and its CRC, the
`random_seed`, and `CInput`'s per-slot command ring all exist to serve it.

With no server there is nothing to reconcile against, so a command is created and run
once, immediately. What stage 1 must preserve is the **ordering and the identity**:
commands are numbered, carry a tick, and are run through movement in a function that
takes `(&mut Player, &UserCmd, dt)` and touches nothing else. A prediction layer later
wraps that function; it does not rewrite it. Building the ring buffer now would be
scaffolding of exactly the kind `ENGINE.md` §10 warns about for the cvar registry.

**`tick_count` has no source yet.** `host/` paces frames, not ticks — there is no
`interval_per_tick` in the port because nothing simulates. Valve's PC tick is
`1.0 / 64.0` (`public/const.h:29`). Stage 1 should carry a tick counter derived from
accumulated frame time rather than inventing a fixed-step loop, and say in `rustdocs/`
that it is a count and not yet a simulation rate.

---

## 5. What is deleted, and why

- **Split-screen.** `MAX_SPLITSCREEN_PLAYERS`, `PerUserInput_t`, `GetPerUser( nSlot )`,
  `ACTIVE_SPLITSCREEN_PLAYER_GUARD`, `in_forceuser`, `ss_mimic` and
  `CheckSplitScreenMimic`. This is the single largest structural simplification available
  in `in_main.cpp` — most of its 2,102 lines are threading a slot index. Portal 2 *has*
  split-screen co-op, so this is a deferral rather than a refusal: **keep one player, keep
  the seam, do not add a slot field until co-op is scheduled** (`ENGINE_INPUT.md` §11.1
  reached the same answer for the same reason).
- **Third-person** (`in_camera.cpp`, `CAM_IsThirdPerson`, `thirdperson_platformer`,
  `thirdperson_screenspace`, `cam_idealyaw`). Roughly a third of `ComputeSideMove` and
  `ComputeForwardMove` is third-person branches for a game that has no third person.
- **HLTV and Replay cameras**, `g_bEngineIsHLTV`, `HLTVCamera()->CreateMove` — with
  `ENGINE.md` §7.12 and §7.13.
- **TrackIR** (`headangles`, `headoffset`) and **Sixense**, both `#ifdef`-walled.
- **Force feedback.**
- **`ToolFramework_SetupEngineView`, `CalcDemoViewOverride`, `s_DemoView`** — the tool
  framework is deleted (`ENGINE.md` §7.22) and demos are `demo/`, later.
- **`ScaleMovements`** — dead in the original (§4.4).
- **`IBaseClientDLL`, `CHLClient`, `g_ClientDLL`, `ClientDLL_ProcessInput`** — the
  interface and its dispatcher, per `PORTING.md`'s no-`CreateInterface` rule.
- **`m_customaccel` 1–4** and `m_mousespeed`/`m_mouseaccel1`/`m_mouseaccel2` — already
  refused in `input/view.rs` with the reason (per-user feel tuning; the latter three are
  Windows `SPI_SETMOUSE` overrides, inert on POSIX).
- **`lookspring`** and the pitch drift (`view.cpp:403`, `:415`) — `DriftPitch` re-centres
  the pitch when moving without a mouse, for keyboard-only play. `lookspring` defaults to
  0 and `cl_mouselook` to 1. Delete, and record that `StopPitchDrift`'s call sites in
  `AdjustPitch` are the only reason `AdjustPitch` touches `view` at all.

---

## 6. The Rust design

### 6.1 Module layout

```
src/client/
  mod.rs        Client — owns the player, the command, the frame entry points
  button.rs     KButton (full kbutton_t), Buttons, ButtonBits (the IN_* set), BUTTONS
  usercmd.rs    UserCmd
  view.rs       ViewAngles, scale_mouse; stage 2's ViewSetup
  movement.rs   MoveData, accelerate, full_noclip_move; stage 4's full_walk_move
  player.rs     Player: origin, velocity, angles, move type, view offset
```

**As built.** One name changed: `move.rs` is `movement.rs`, because `move` is a keyword
and `mod r#move;` is not worth it.

`src/engine/input/view.rs` **is deleted** by stage 1: `ViewAngles` moves to
`client/view.rs`, `KButton` to `client/button.rs`, `MoveButtons` becomes `Buttons`, and
`FlyCamera` becomes `Player` + `move::full_noclip_move`. Nothing is left in the file, so
the wart in `CLAUDE.md` and `ENGINE_INPUT.md` §11.2 is closed rather than moved.

### 6.2 Types

Sketches, not signatures — the point is the shape.

**As built** (`rustdocs/CLIENT.md` has the full signatures):

```rust
pub struct Client {
    player: Player,
    buttons: Buttons,
    cvars: Cvars,          // ~19 handles
    command_number: i32,
    tick_count: i32,
    impulse: u8,
}

impl Client {
    pub fn new(console: &mut Console<'_>) -> Client;
    pub fn create_move(&mut self, mouse: (f32, f32)) -> UserCmd;  // CInput::CreateMove
    pub fn run_move(&mut self, cmd: &UserCmd, dt: f32);           // ProcessMovement
    pub fn eye(&self) -> Vec3;                                    // stage 2 makes this a ViewSetup
}
```

Two deviations from the sketch this section originally carried, both from building it:

- **~~`create_move` takes no frame time.~~ It does again, as of stage 3** — `AdjustAngles`
  needs it. Leaving it out for two stages cost one signature change and no confusion,
  which is the trade an always-unused parameter loses.
- **`create_move` takes no `active` flag.** Valve's third argument is `!cl.IsPaused()`;
  there is no pause.

`Player` is a field of `Client` rather than of `Engine`'s `Scene` directly, and `Client`
itself lives in `Scene` — because `Level::load` is handed a `&mut Scene` and loading a map
is the only thing that positions a player.

`state`'s three bits become three `bool`s rather than a bitfield: nothing indexes them,
`state & 3` is one `||`, and the C++'s `data.state &= clearmask` trick in `CalcButtonBits`
(`:1738`) becomes an explicit branch that says what it is doing.

### 6.3 The seams

Three, and all three already exist in some form:

1. **`+command` → `KButton`.** The command buffer calls into `Client` the way it calls
   into `Host` and `Input` today, through `console/`'s `CommandTarget`/`EngineCommands`.
   The nineteen `+`/`-` pairs are one table, not nineteen registrations, and the button
   index argument that makes two keys work is already threaded by `input/`'s bindings.
2. **Mouse delta → `create_move`.** Two `f32`s, produced by `Input::frame`, gated by
   `Engine::wants_mouse_capture` exactly as `update_view` gates it today. The gate stays
   in `engine/mod.rs`: it is a question about the console and the window, not about the
   player.
3. **`ViewSetup` → `materials::Camera`.** Stage 2 moves the body of `Engine::camera` into
   `Client::view` and leaves `engine/mod.rs` converting a `ViewSetup` into a `Camera`.
   The conversion stays on the engine side because the aspect ratio comes from the frame.

### 6.4 The borrow that will bite

`create_move` needs `&mut self` (it clears impulses) and its result is consumed by
`run_move`, which also needs `&mut self`. That is fine sequentially. What is not fine is
the shape Valve uses — `CInput` holding a ring of commands that `CPrediction` and
`CL_SendMove` both read while `CInput` still owns them. When prediction arrives, the
commands must live somewhere that is not inside `Client`, or the same disjoint-field
dance `ENGINE_CONSOLE.md` §6.6 describes will be needed for three readers instead of one.
**Return the command by value from stage 1** and let the caller decide where it goes;
that is the decision that keeps the option open.

---

## 7. Fixed formats and external content

Almost nothing here is pinned, which is unusual for this tree — and it is worth stating
because it means the design really is free:

- **`CUserCmd`'s wire encoding is pinned** the moment `net/` exists (`ReadUsercmd`/
  `WriteUsercmd`, `usercmd.cpp`) — bit-packed, delta-against-a-previous-command, with
  each field's presence flagged. **It is not pinned now.** Per `PORTING.md`, the format
  becomes ours once both ends are Rust, and both ends will be. Design the struct for
  clarity; the encoder is `net/`'s problem and can be `deku`.
- **The `IN_*` bit values are pinned** while they cross the wire or reach a `.dem` file,
  and Portal 2's three additions are the trap: `IN_SLOWTIME` (1<<26, under
  `USE_SLOWTIME`), `IN_COOP_PING` (1<<27), `IN_REMOTE_VIEW` (1<<28) — and
  `INFESTED_DLL` reuses 1<<22 through 1<<31 for something else entirely. Port the
  Portal 2 set, not the base set, and not Infested's.
- **`+forward`, `-forward`, `+attack`, … are external content**: `cfg/config_default.cfg`
  and every user's `config.cfg` name them. The names are fixed; already handled by
  `console/` and `input/` stages 2–3.
- **The cvar names and defaults are external content** for the same reason — a shipped
  `.cfg` sets `sensitivity`, and Portal 2's own configs set movement cvars.
- **Everything else is a free redesign** (`ENGINE.md` §11.1's split: this module is on the
  "free" side).

---

## 8. Staged plan

Each stage compiles, passes `cargo test`, and leaves the binary in a better state than it
found it.

### Stage 1 — the player exists — **DONE** (2,114 lines, 49 tests)

`src/client/` is created; `src/engine/input/view.rs` is deleted. What actually landed,
against what this section asked for:

- `UserCmd`, `Buttons` (the `IN_*` set), `KButton` with the impulse bits and the
  destructive `key_state()`.
- The `+`/`-` command table, taking over the ones `input/` registers today, plus the ones
  it does not: `+attack`, `+attack2`, `+use`, `+jump`, `+duck`, `+reload`, `+walk`,
  `+speed`, `+left`, `+right`, `+lookup`, `+lookdown`, `+strafe`, `+moveup`, `+movedown`,
  `impulse`.
- `create_move`: `AdjustAngles` (mouse only — keyboard look is stage 3), `ComputeSideMove`
  / `ComputeUpwardMove` / `ComputeForwardMove` against `KeyState`, `GetButtonBits`,
  impulse latch.
- `Player` with `MoveType::Noclip`, and `full_noclip_move` **including** the
  `Accelerate` + friction branch (§4.5).
- The cvars, registered with Valve's names, defaults, bounds and flags:
  `cl_forwardspeed`/`cl_sidespeed`/`cl_backspeed` (175, `FCVAR_CHEAT`), `cl_upspeed`
  (320, `FCVAR_CHEAT`), `sensitivity` (2.5, `FCVAR_ARCHIVE`, **bounds [0.0001, 1000]** —
  see §9.1), `m_yaw` (0.022, `FCVAR_ARCHIVE`), `m_pitch` (0.022, `FCVAR_ARCHIVE|FCVAR_SS`,
  server-bounded), `cl_pitchdown`/`cl_pitchup` (89, `FCVAR_CHEAT`), `sv_noclipspeed`/
  `sv_noclipaccelerate` (5), `sv_maxspeed` (320), `sv_friction` (5.2), `sv_stopspeed`
  (80), `default_fov` (**75**, `FCVAR_CHEAT`).
- A `noclip` command that toggles the move type — which, until stage 4, can only turn
  itself off into a movetype that does not exist. Register it anyway and have it say so;
  §9.2.
- `engine/mod.rs`'s `update_view` becomes `create_move` + `run_move`. `Scene::view`
  becomes `Client::player`.

Two things the plan did not anticipate, both found by building it:

- **A tap shorter than a frame reaches the command but does not move the player.** It was
  supposed to be the headline demonstration of `KeyState`. `FullNoClipMove`'s friction
  bleed floors `control` at `maxspeed / 4`, so it removes ~34.7 units of speed per frame
  at 60 Hz while a quarter-speed wish only accelerates by ~20.8 — the tap is real, reaches
  `forwardmove`, and is then eaten. That is Valve's arithmetic, and it is only visible
  because stage 1 restored the acceleration branch. `sv_noclipaccelerate 0` shows the tap
  moving the player.
- **The steady-state noclip speed is ~768, not the 875 the wish asks for**, because the
  friction bleed and `Accelerate`'s `addspeed` cap balance below it.

**Behaviour after stage 1 is close to what came before** — fly around with WASD — with
three visible differences: movement has momentum and coasts (that is
`sv_noclipaccelerate` 5, which the placeholder camera hard-coded to 0), the eye is
computed as feet-plus-`VEC_VIEW` rather than handed over as an eye, and every cvar above
now works, including `cl_forwardspeed 400`.

### Stage 2 — the view is the client's — **DONE** (~200 lines, 7 tests)

- `ViewSetup` and `Client::view(width, height)`: eye = origin + view offset,
  `default_fov`, `GetZFar` from `r_mapextents`/`r_farz` rather than a constant, `zNear`
  from `GetZNear` including the mega-wide branch.
- `Engine::camera` shrinks to a `ViewSetup` → `materials::Camera` conversion.
- ~~`r_mapextents` comes from the map; `world/` supplies it.~~ **Wrong, and corrected by
  building it.** `r_mapextents` is a plain `FCVAR_CHEAT` cvar defaulting to 16384
  (`view.cpp:119`) and **nothing in the tree sets it from the `.bsp`** — the name suggests
  otherwise. It is a knob a mapper turns. So stage 2 has no `world/` dependency at all,
  and the far plane is `r_mapextents × √3` exactly as before, but now reachable.

**The thing this stage was actually for, and the plan did not mention it.**
`SetUpView` leaves `fov` at `default_fov`, and `CViewRender::Render` scales it by
`aspect / (4/3)` a few hundred lines later (`view.cpp:1084`, `ScaleFOVByWidthRatio` at
`:923`). **Source's FOV numbers are horizontal and quoted at 4:3**, and the composition is
classic Hor+: the *vertical* FOV comes out constant at `2·atan(tan(fov/2) · 0.75)` and the
horizontal grows with the screen. The port was handing 75 straight to a `PerspectiveX`,
which at 16:9 gives a **46.7-degree vertical FOV where the shipped game gives 59.8** — a
view that is not obviously wrong, just quietly too narrow. That is the visible change in
this stage, and it is why the scaling is applied inside `Client::view` rather than left
for a caller: a `ViewSetup` whose `fov` still needs scaling is a trap.

Two more details worth having found: `GetZNear` returns **1 rather than 7 on a mega-wide
viewport** (`width / (height + 1) > 2`), because a wide frustum's edges reach far enough
out that a 7-unit near plane clips what the player is standing beside; and the *same*
aspect ratio feeds both the FOV scaling and the projection, because `Render` sets
`m_flAspectRatio` from `GetScreenAspectRatio` two lines after scaling the FOV with it
(`view.cpp:1106`).

### Stage 3 — keyboard look and the sample-time budget — **DONE** (~350 lines, 9 tests)

- Keyboard look: `AdjustAngles`, `AdjustYaw`, `AdjustPitch`, `ClampAngles`,
  `cl_yawspeed` 210, `cl_pitchspeed` 225, `cl_anglespeedkey` 0.67, `cl_mouselook`.
- `IN_SetSampleTime` / `DetermineKeySpeed`'s budget, as `Client::set_sample_time` plus a
  private `determine_key_speed`. `create_move` takes the frame time again, exactly as
  stage 1 predicted it would.

**`ExtraMouseSample` is not ported, and this section was wrong to assume it would be.**
The plan said the budget "should land with the frame-rate mouse sample rather than before
it". There is no frame-rate mouse sample to land with, for two independent reasons:

1. **The latency it recovers is not lost here.** Valve builds the real command early in
   `_Host_RunFrame_Input`, simulates, and only then samples the mouse again just before
   rendering (`host.cpp:4359`), so the picture uses angles newer than the command's. This
   port's `Engine::update_client` runs immediately before `Engine::render`, in the same
   `winit` callback, with nothing between them.
2. **It could not be done anyway.** `AccumulateMouse` re-polls the OS mid-frame
   (`in_mouse.cpp:719`, "Sample mouse one more time"); `winit` delivers one batch of
   events per frame and cannot be pumped re-entrantly from inside a handler, so a second
   drain would return `(0.0, 0.0)`.

**Revisit when simulation lands between input and rendering** — which is when reason 1
stops holding, and when the budget stops being a no-op for the other reason too.

So the budget is currently a no-op: refilled once per frame with the frame time, drawn
down once per command by the same amount. It is implemented anyway, because it is the
shape `DetermineKeySpeed` has the moment there is more than one command per frame, and
because "turning is twice as fast at 30 fps" is an expensive bug report to work backwards
from. Its failure mode is silent and is documented at the site: **forget the refill and
keyboard look does nothing, for ever.**

**Two things found by building it**, both now in `rustdocs/CLIENT.md`:

- **`cl_mouselook 0` does not turn the mouse off.** `ControllerMove` gates `MouseMove` on
  `cl_mouseenable` and on the cursor being grabbed (`in_main.cpp:1199`), never on
  `cl_mouselook`. Turning it off *adds* keyboard pitch — it is the only thing that makes
  `+lookup`, `+lookdown` and `+klook` do anything.
- **The destructive `KeyState` reads never collide, and it is not luck.** `AdjustYaw`
  reads `+left`/`+right` only when `+strafe` is up and `ComputeSideMove` only when it is
  down; `AdjustPitch` reads `+forward`/`+back` only when `+klook` is down and
  `ComputeForwardMove` only when it is up. Each pair is mutually exclusive on its
  condition, which is what lets `CreateMove` call them in sequence.

**Nothing visible changed out of the box**, and that is expected:
`cfg/config_default.cfg` binds no key to `+left`, `+right`, `+lookup`, `+lookdown` or
`+klook`, and `cl_mouselook` defaults to 1. `bind LEFTARROW "+left"` reaches it.

### Stage 4 — walking — **DONE** (~1,500 lines, 20 tests)

`FullWalkMove`, gravity, friction, `CategorizePosition`, ducking, jumping, the hulls, and
`CheckParameters`' speed clip. **This is where the player stopped being a noclip camera**,
where the `+jump`/`+duck`-as-up/down placeholder mapping went away, and where
`MoveType::Walk` became reachable — and it is now what a player spawns as.

Verified on `sp_a1_intro1`: the player spawns 8.97 units above `MOTEL/HOTEL_CARPET001`,
**falls onto it**, walks at 175 along the view, is stopped by the `TOOLS/TOOLSPLAYERCLIP`
brush 127 units ahead and slides along its face, and leaves the ground when `+jump` is
sent.

#### The headline finding: this is `CPortalGameMovement`, not `CGameMovement`

`CPortalGameMovement` (`game/shared/portal/portal_gamemovement.cpp`, 5,245 lines)
overrides two dozen of the base class's methods, and **several of the overrides change
behaviour that has nothing to do with portals**. §4.4 and §4.5 were written against
`CGameMovement`, and porting that would have produced a player who moves plausibly and
wrongly:

| | `CGameMovement` | `CPortalGameMovement` |
|---|---|---|
| Jump height | 21 (`GAMEMOVEMENT_JUMP_HEIGHT`) | **45** (`:573`) |
| Bunny-hop speed boost on jump | yes, under `HL2_DLL` | **none** |
| Jump while ducked | allowed, at a fixed speed | **refused** (`:534`) |
| Air-control speed cap | 30 (`:1975`) | **60** (`:641`) |
| Duck transition | 200 ms under `CSTRIKE15` | **400 ms** (`shareddefs.h:100`) |
| Gravity | 800 | **600** (`movevars_shared.cpp:16`) |
| Edge friction | absent | **on** (`:3351`), doubling friction over a ledge |
| `ClipVelocity`'s re-push | at least `DIST_EPSILON` | cancels the residual only (`:4303`) |
| `StayOnGround`'s up-probe | 2 units | **1 unit** (`:3487`) |
| Walking into a standable slope | `StepMove` | **slides up the ramp** (`:3824`) |

Where Portal's override differs only by generalising world `+Z` to an arbitrary "stick
normal" — the paint-gel gravity reorientation — the two are the same function with no
paint, because `m_vGravityDirection = -stickNormal` (`:440`) and the stick normal is world
up. Those are ported in the world-`+Z` form.

#### Corrections to this plan, found while implementing

- **§4.4's "`sv_maxspeed` 320" is not the number that bounds a Portal 2 player.**
  `mv->m_flMaxSpeed` is `GetPlayerMaxSpeed()` = `min( sv_maxspeed, MaxSpeed() )`, and a
  Portal player's `MaxSpeed()` is `sv_speed_normal` = **175**
  (`portal_player_shared.cpp:1591`). **This was a live bug in stage 1**, which passed
  `sv_maxspeed` and flew noclip at 1600 where the shipped game flies at 875. Fixed with
  stage 4, and one stage-1 test changed with it — see `rustdocs/CLIENT.md`.
- **The duck-jump state machine is unreachable in Portal 2 and is not ported.**
  `CheckJumpButton` sets `bSetDuckJump = false` over a Valve comment reading "temp fix for
  camera snapping when ducking in the air ( NO DUCKJUMP for now )". Nothing sets
  `m_nJumpTimeMsecs`, so `m_bInDuckJump`, `StartUnDuckJump`, `CanUnDuckJump`,
  `FinishUnDuckJump` and `UpdateDuckJumpEyeOffset` are all dead. That is most of the
  length of `Duck()`.
- **Ladders are deleted, not deferred.** `GameHasLadders()` returns `false` for Portal
  (`portal_gamemovement.h:132`).
- **`MoveData` grew a sibling, `MoveVars`.** The `sv_*` set is read once per command into
  a struct rather than reached through cvar handles at each use, because this module
  compiles into a server too and a cvar handle is a client-side convenience. It is
  `movevars_shared.cpp`, which is a file of exactly these.
- **The `+jump`/`+duck` placeholder is gone and nothing replaced it.** With walking real,
  jump and duck are buttons; a noclip player flies up the way the shipped game does, by
  looking up and holding forward. `bind SPACE +moveup` brings the axis back.

### Stage 5 — prediction and the connection (blocked on `net/` + `server/`)

`CPrediction`, the command ring, `CVerifiedUserCmd`, and the handoff to
`src/engine/client/`. Not this doc's.

---

## 9. Open questions and risks

1. **~~The port's `sensitivity` bounds are wrong~~ — found writing this doc, and
   already fixed.** `src/engine/input/view.rs` declared `SENSITIVITY_MAX =
   10_000_000.0` and `src/engine/mod.rs` commented the cvar as
   `FCVAR_ARCHIVE | FCVAR_SS` with the same maximum. This tree has exactly one
   declaration — `ConVar sensitivity( "sensitivity","2.5", FCVAR_ARCHIVE, "Mouse
   sensitivity.", true, 0.0001f, true, 1000 )` (`in_mouse.cpp:100`) — so the maximum is
   **1000** and there is no `FCVAR_SS`; the 10,000,000 figure belongs to a different
   Source branch. Recorded here because the *class* of error survives the fix: it is
   exactly what `rustdocs/`'s "verify signatures against the source" rule exists to
   catch, and the other nineteen cvars in §8 are still transcribed rather than checked.
   **Check each one against `legacy/` as stage 1 registers it.**
2. **`noclip` is a server command in Valve, and there is no server.** `ConCommand noclip`
   lives in `game/server/`, because move type is server state that gets networked down.
   With one process and no server, stage 1 has to put it somewhere; putting it on the
   client is a divergence that stage 4 or `server/` will have to undo. **Recommendation:**
   register it in `client/`, flag it in `rustdocs/CLIENT.md` as owned by `server/`, and
   move it when `server/` exists — the same shape as the `gameinfo.txt` and `CommandLine`
   warts, which is a pattern this project already handles well.
3. **Does `client/` own the render view, or does `engine/`?** Stage 2 says the client
   produces a `ViewSetup` and the engine converts it. The alternative — the client holding
   a `materials::Camera` — makes `client/` depend on `materials/`, which is a dependency
   worth not taking for a struct of five numbers. Revisit only if the view needs render
   targets (it will, for portals — `CViewRender` renders the world once per portal
   recursion level, and that is `render/`'s problem, not this one's).
4. **Split-screen.** Same answer and same reasoning as `ENGINE_INPUT.md` §11.1: one
   player, keep the seam, do not add a slot field until co-op is scheduled. Note that
   Portal 2's co-op is *the* reason its `CUserCmd` carries the grab fields, so this may
   arrive sooner here than elsewhere.
5. **`tick_count` without ticks.** §4.8. Carrying a counter that is not a simulation rate
   is honest as long as `rustdocs/` says so; inventing a fixed-step loop to make it true
   would be building `host/` a second time.
6. **The Portal eye is not `origin + offset`.** `C_Portal_Player::CalcView` interpolates
   the eye *through* a portal for several frames after a teleport
   (`UpdatePortalEyeInterpolation`) and flags `m_bEyePositionIsTransformedByPortal`. A
   stage-2 `ViewSetup` that computes the eye inline, rather than asking the player for it,
   closes that seam. Ask the player.
7. **Is prediction ever actually needed for single-player Portal 2?** A listen server has
   no latency, and `CL_Move`'s `IsLoopback()` branch already skips most of the send path.
   It may be that the whole of stage 5 reduces to "run the command against the local
   server's player". Worth answering *before* `net/` is designed, because the answer
   changes how much of `MULTIPLAYER_BACKUP` matters.

---

## 10. Notes for whoever picks this up

- **Everything quoted here was read from source**, not from the graph.
  `check_index_coverage` should be run before trusting any negative result in
  `in_main.cpp`, `view.cpp` or `gamemovement.cpp`; `ENGINE_INPUT.md` §12 already records
  that `in_main.cpp` and `in_mouse.cpp` are `parse_partial`, and `gamemovement.cpp` is
  5,357 lines of exactly the shape that parses partially.
- Line numbers are from the tree at time of writing; re-verify before relying on them.
- **`legacy/game/client/` is where CS:GO defaults hide.** `cstrike15/` is 103,547 lines of
  the 443,852 total and is *not* excluded by any `#ifdef` you will notice while reading —
  `clientmode_csnormal.cpp` and `clientmode_portal.cpp` both define `default_fov` at file
  scope. Check which mod a definition belongs to before copying its number.
- Read `game/shared/` for anything the server also runs. `gamemovement.cpp` compiles into
  both binaries; a `#ifndef CLIENT_DLL` in it marks server-only code, and its absence
  marks code this module will eventually share with `src/server/`. That sharing is a
  future `src/game/` or a plain `pub(crate)` module — **do not decide it now**, but do not
  write anything in `move.rs` that assumes a client.
- Only POSIX paths, per `PORTING.md`. This corner of the tree is milder than
  `inputsystem/` — the `#ifdef`s here are mostly *game* (`PORTAL2`, `CSTRIKE15`,
  `INFESTED_DLL`, `HL2_CLIENT_DLL`) rather than platform, and those matter: keep `PORTAL2`
  and `PORTAL`, discard the rest.
