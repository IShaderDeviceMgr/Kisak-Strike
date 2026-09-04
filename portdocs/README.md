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

`LAUNCHER.md` predates PORTING.md's architecture change and carries a note at the top
saying what that changed; its factual content (module behavior analysis) is unaffected.
`FILESYSTEM.md`, `MATERIALSYSTEM.md` and `ENGINE.md` are written against the current
architecture and need no such note.
