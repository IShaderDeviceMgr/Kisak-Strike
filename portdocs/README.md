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

- [`CLIENT.md`](CLIENT.md) — the **game client**. **Stages 1-3 of 5 done; see
  `rustdocs/CLIENT.md` for the API.** `game/client/in_*.cpp`, `view.cpp`,
  `game/shared/usercmd.h` and the movement half of `gamemovement.cpp`. Resolves the
  two-clients problem — this is `client.so`'s local player and view, landing at
  `src/client/`, while `ENGINE.md` §7.5's *client connection* (`CClientState`, blocked on
  `net/`) stays `src/engine/client/`. Inventory, the frame's two input sample points and
  the keyboard-sample-time budget that exists because of them, `CUserCmd` as the module's
  only output, `kbutton_t`'s fractional `KeyState`, the three places movement is computed,
  and `SetUpView`. Concludes the player starts as `MOVETYPE_NOCLIP` rather than as a
  camera, and **takes Valve's own `// FIXME, move entirely to client .dll`** on the view
  angles. Five stages; stages 1-3 are unblocked and delete `src/engine/input/view.rs`
  along with `CLAUDE.md`'s view-angles wart.

- [`ENGINE_TRACE.md`](ENGINE_TRACE.md) — collision and tracing: `engine/cmodel*.cpp` (the
  BSP brush trace), `enginetrace.cpp` (the dispatch over collideables),
  `spatialpartition.cpp` (the entity broadphase) and `public/dispcoll_common.*`. Inventory
  across all four, `Ray_t`'s centered box and the offset that comes with it, the
  `startsolid`/`allsolid`/`fractionleftsolid` trio, `DIST_EPSILON` as behavior rather
  than noise, and the two `IEngineTrace` methods that exist only so Portal can carve a
  hole in a wall. **Corrects `ENGINE.md` §7.14/§7.17**, which file the 5,967-line BSP
  collision core under `world/`. Contains a full evaluation of **Rapier/parry**:
  adopted for `vphysics/`, the `.phy` sweep and the broadphase; **not** for the world
  brush trace, with the six reasons and the conditions that would reverse it. Five
  stages; stage 1 is unblocked and is what `portdocs/CLIENT.md` stage 4 waits on.

`LAUNCHER.md` predates PORTING.md's architecture change and carries a note at the top
saying what that changed; its factual content (module behavior analysis) is unaffected.
`FILESYSTEM.md`, `MATERIALSYSTEM.md` and `ENGINE.md` are written against the current
architecture and need no such note.
