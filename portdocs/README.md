# portdocs/

Per-module design/porting docs for larger modules being rewritten in Rust. See
[`../PORTING.md`](../PORTING.md)'s "Per-module porting docs" section for what belongs
here and when a module warrants one.

Naming: `<module directory>/` → `portdocs/<MODULE>.md` in `SCREAMING_SNAKE_CASE`,
e.g. `engine/` → `ENGINE.md`, `materialsystem/` → `MATERIALSYSTEM.md`.

## Current docs

- [`MATERIALSYSTEM.md`](MATERIALSYSTEM.md) — `materialsystem` (+ `togl`, `public/shaderapi`).
  Inventory, the shadow/dynamic two-phase model and its mapping onto `wgpu` pipelines,
  the deletion of the `IShaderDevice`/`IShaderAPI` tower, the **WGSL shader port** (§7 —
  combo policy, the constant-register ABI, the prelude, the porting recipe), Portal 2
  paint maps, and a staged plan. Written against the current architecture. This is the
  "rendering" step of PORTING.md's boot path.
- [`FILESYSTEM.md`](FILESYSTEM.md) — `filesystem` (+ `vpklib`, `public/filesystem_init.cpp`).
  **Ported — see `src/filesystem/`.** Inventory, search-path/path-ID model, VPK format,
  the `Vfs` design, the decisions taken while implementing, and what stayed deferred.
  Written against the current architecture.
- [`LAUNCHER.md`](LAUNCHER.md) — `launcher`. **Partly superseded** by PORTING.md's
  move to a single-binary architecture (see the banner at the top of the doc); its
  module-behavior analysis is still the reference.
- [`ENGINE.md`](ENGINE.md) — `engine`. Design/scoping only, but rewritten against the
  current architecture. 23 subsystems enumerated with files and sizes, mapped onto the
  **14 Rust modules under `src/engine/`** they become, plus the frame-loop/`winit`
  analysis. Concludes `engine` should **not** be ported as one unit or ported next, and
  each surviving subsystem gets its own portdoc (`ENGINE_AUDIO.md`, `ENGINE_NET.md`,
  `ENGINE_HOST.md`) when it's scheduled.
- [`ENGINE_INPUT.md`](ENGINE_INPUT.md) — input: Valve's top-level `inputsystem/`, plus
  `engine/keys.cpp` and the `game/client/in_*.cpp` movement layer. Inventory across all
  three, the `winit` event mapping, the two platform traps (`CursorGrabMode` is
  unimplemented on X11 *or* macOS depending on the mode; raw motion is accelerated on
  macOS only), the key-up latch that must survive, and a five-stage plan whose first two
  stages depend on nothing unbuilt. Controllers (`gilrs`) are stage 5, deliberately
  deferred. Concludes input should be **its own module**, revising `ENGINE.md` §1.
- [`ENGINE_CONSOLE.md`](ENGINE_CONSOLE.md) — console, cvars and commands: `engine/cmd.cpp`
  and `cvar.cpp`, plus `tier1/convar.cpp`, `tier1/commandbuffer.cpp` and `vstdlib/cvar.cpp`,
  which is where the system actually lives. Inventory across all six, the command buffer's
  tick/`wait` model, the two tokenizers, the dispatch order, a flag-by-flag disposition of
  the 32 `FCVAR_*` bits, and the decision that **there is no global cvar registry** — a cvar
  is a shared cell, so the registry only serves name lookup. Five stages; stage 1 boots
  `sp_a1_intro1` through `exec valve.rc` → `stuffcmds` and deletes the launcher's `+map`
  block. Concludes cvar sets are handled inside `console/` and commands are handed back out
  through a `CommandTarget` trait, the way `host::Level` works.

- [`CLIENT.md`](CLIENT.md) — the **game client**. **Stages 1-4 of 5 done; see
  `rustdocs/CLIENT.md` for the API.** `game/client/in_*.cpp`, `view.cpp`,
  `game/shared/usercmd.h` and the movement half of `gamemovement.cpp`. Resolves the
  two-clients problem — this is `client.so`'s local player and view, landing at
  `src/client/`, while `ENGINE.md` §7.5's *client connection* (`CClientState`, blocked on
  `net/`) stays `src/engine/client/`. Inventory, the frame's two input sample points and
  the keyboard-sample-time budget that exists because of them, `CUserCmd` as the module's
  only output, `kbutton_t`'s fractional `KeyState`, the three places movement is computed,
  and `SetUpView`. Concludes the player starts as `MOVETYPE_NOCLIP` rather than as a
  camera, and **takes Valve's own `// FIXME, move entirely to client .dll`** on the view
  angles. Stage 4 is walking, and its headline finding is that the reference is
  **`CPortalGameMovement`, not `CGameMovement`** — Portal 2 jumps 45 units where the base
  class jumps 21, caps air control at 60 where it caps at 30, refuses to jump while
  ducked, has edge friction, and bounds the player at 175 rather than `sv_maxspeed`'s
  320. Five stages; stages 1-4 are done, and they delete `src/engine/input/view.rs` along
  with `CLAUDE.md`'s view-angles and `+jump`/`+duck` warts.

- [`ENGINE_TRACE.md`](ENGINE_TRACE.md) — collision and tracing. **Stage 1 of 5 done; see
  `rustdocs/ENGINE.md` for the API.** `engine/cmodel*.cpp` (the
  BSP brush trace), `enginetrace.cpp` (the dispatch over collideables),
  `spatialpartition.cpp` (the entity broadphase) and `public/dispcoll_common.*`. Inventory
  across all four, `Ray_t`'s centered box and the offset that comes with it, the
  `startsolid`/`allsolid`/`fractionleftsolid` trio, `DIST_EPSILON` as behavior rather
  than noise, and the two `IEngineTrace` methods that exist only so Portal can carve a
  hole in a wall. **Corrects `ENGINE.md` §7.14/§7.17**, which file the 5,967-line BSP
  collision core under `world/`. Contains a full evaluation of **Rapier/parry**:
  adopted for `vphysics/`, the `.phy` sweep and the broadphase; **not** for the world
  brush trace, with the six reasons and the conditions that would reverse it. Five
  stages; stage 1 has landed and is what `portdocs/CLIENT.md` stage 4 was waiting on.

- [`ENGINE_WORLD_DISP.md`](ENGINE_WORLD_DISP.md) — displacements, the **rendering** half.
  **Ported — see `src/engine/world/disp/` and `rustdocs/ENGINE.md`.** The collision half
  landed with `ENGINE_TRACE.md` stage 3; this is the other half of the same lumps.
  Inventory across `engine/disp*.cpp`, `public/builddisp.cpp`, `disp_powerinfo.cpp` and
  `disp_tesselate.h` (~9,100 lines, ~350 with a counterpart), the four things a render
  vertex needs beyond the grid `trace/` already builds, and Valve's quadtree tessellation
  — which is **not** the collision triangulation, because it honours a per-vertex
  `m_AllowedVerts` set that stops a power-4 patch cracking against a power-2 neighbour,
  and which nonetheless coincides with it exactly when nothing is disallowed. Concludes
  there is **no separate terrain draw path**: a displacement goes through the existing
  material-grouping and lightmap-packing pipeline with its grid in place of the face's
  winding. Its other finding is a shader: **937 of the game's 1,181 displacement faces
  name `WorldVertexTransition`**, which is `LightmappedGeneric` under a second name and
  landed with this. Two of its sections were **corrected by the port** and say so: §4.4's
  winding argument was backwards, and §6.2 under-stated `$ssbump`'s reach by two orders of
  magnitude.

- [`CLIENT_TONEMAP.md`](CLIENT_TONEMAP.md) — **auto exposure. Ported; see
  `src/client/tonemap.rs`, `src/materials/{post,histogram}.rs`, and both rustdocs.**
  `CTonemapSystem` and the head of `DoEnginePostProcessing` (~1,000 lines of
  `viewpostprocess.cpp`), plus `dev/lumcompare`, `luminance_compare_ps2x.fxc` and
  `SetToneMappingScaleLinear`. Written *after* the port rather than before it, because the
  tone mapper was not on `CLIENT.md`'s plan. Records why the exposure scalar needs no float
  render target (Valve applies it in the shader in both HDR modes), why the scene
  nonetheless has to stop going straight to the back buffer, and the finding that decides
  the whole calibration: **the histogram measures linear light, not gamma**, because
  `dev/lumcompare.vmt` leaves `$LINEARREAD_BASETEXTURE` unset — Valve's own comment says
  the opposite and is stale. Also records three things that look like bugs and are not
  (the V-shaped moving-average weights, the per-frame step cap, and the
  `mat_accelerate_adjust_exposure_down` it renders inert below 128 fps), and a census of
  what the shipped maps actually ask for: **105 of 106 place an `env_tonemap_controller`**,
  and none of them uses the cvar defaults this port falls back on.

- [`SERVER.md`](SERVER.md) — the **game server**: the entity system. **Stage 1 of 5 done;
  see `src/server/` and `rustdocs/SERVER.md`.** `game/server/`'s
  447,000 lines, of which the framework — `CBaseEntity`, `CGlobalEntityList`, the
  datadesc, entity I/O, the event queue, thinks, `MOVETYPE_PUSH` — is ~29,800 and is the
  module; the rest is entity classes and `ai_*`. Scoped by **measuring the shipped maps
  rather than the tree**: 106 maps place **60,925 entities of 200 classnames**, the top
  25 of which are 79.8% of them, while the whole 122,298-line `ai_*`/`nav_*` tree serves
  **293 `npc_*` instances of 6 classnames**. Two findings change what porting means here:
  **41 of the 200 classnames have no source in this tree at all** (`server_portal2.vpc`
  lists 63 `.cpp` and 59 are missing — `func_portal_bumper`, the turrets, the whole paint
  system), and **the shipped `portal2.fgd` and its includes define 494 classes covering
  199 of the 200**, so the interface of the deleted ones survives even though the
  behaviour does not. Concludes the inheritance tree becomes a `&'static ClassDef` plus a
  three-method trait, that networking and save/restore delete outright, and that **the
  fixed server tick is the one decision to take before stage 1**. Five stages; stage 1 has
  landed — 17,069 of the 60,925 entity blocks in the shipped game now spawn, and
  `CLight::Spawn` deletes 6,937 of them because an unnamed light is already baked — and
  stage 2 lands `env_tonemap_controller`, closing `CLIENT_TONEMAP.md`'s one measured gap.
  Three of its sections were **corrected by the port** and say so: §7.3's `parent` pointer
  (inheritance became composition), §7.3's FGD cross-check (the FGD is not a superset of
  the datadesc, so the check with teeth is against map data), and §4.4's wildcard rule.

- [`PORTAL.md`](PORTAL.md) — **`prop_portal`: teleportation and a drawn frame.** Written
  before the port; nothing has landed. ~26,600 lines of `game/{server,shared}/portal/` and `mathlib/`
  plus a 7,400-line client renderer that is entirely out of scope, of which roughly
  **1,900** have a counterpart. Scoped deliberately to the mechanism rather than the look — you will see the
  *wall* through a portal, with a coloured oval on it, and walking into it will put you
  out of the other one. Scoped by measuring the maps: a portal is overwhelmingly a thing
  the **gun** makes, and the gun is out of scope, so what the shipped content offers is
  **21 `prop_portal`s across 10 maps** — two of them on `sp_a1_intro1`, which makes the
  default map the test bed. Its central finding is that **the collision carve does not
  need a polyhedron library**: `CarveWallBrushes_Sub` is four clips of the same four
  planes at four distance sets, and `clip_box_to_brush` consumes *planes only*, so a
  carved piece is the original brush's planes plus four more and `mathlib/polyhedron.cpp`
  (3,895 lines) plus `staticcollisionpolyhedroncache.cpp` (586) delete outright. Records
  that the teleport lives in the **movement** and not the entity (Valve's own
  `Warning( "PORTALLING PLAYER SHOULD BE DONE IN GAMEMOVEMENT" )`), that `portal1.mdl` is
  a 4-vertex quad wearing a **depth-only** shader and is invisible on purpose, that
  linkage is by **group and size** rather than by colour, and that the whole of §7 is
  gated on a **blended pass** this port has never had — which is stage 1, is not portal
  work, and unblocks the five translucent brush entities too. Five stages.

`LAUNCHER.md` predates PORTING.md's architecture change and carries a note at the top
saying what that changed; its factual content (module behavior analysis) is unaffected.
`FILESYSTEM.md`, `MATERIALSYSTEM.md` and `ENGINE.md` are written against the current
architecture and need no such note.
