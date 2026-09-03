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
  **Next module on the boot path.** Inventory, search-path/path-ID model, VPK format,
  the Rust `Vfs` design, and a staged plan. Written against the current architecture.
- [`LAUNCHER.md`](LAUNCHER.md) — `launcher`. **Partly superseded** by PORTING.md's
  move to a single-binary architecture (see the banner at the top of the doc); its
  module-behavior analysis is still the reference.
- [`ENGINE.md`](ENGINE.md) — `engine`. Design/scoping only, but rewritten against the
  current architecture. 23 subsystems enumerated with files and sizes, mapped onto the
  **13 Rust modules under `src/engine/`** they become, plus the frame-loop/`winit`
  analysis. Concludes `engine` should **not** be ported as one unit or ported next, and
  each surviving subsystem gets its own portdoc (`ENGINE_AUDIO.md`, `ENGINE_NET.md`,
  `ENGINE_HOST.md`) when it's scheduled.

`LAUNCHER.md` predates PORTING.md's architecture change and carries a note at the top
saying what that changed; its factual content (module behavior analysis) is unaffected.
`FILESYSTEM.md`, `MATERIALSYSTEM.md` and `ENGINE.md` are written against the current
architecture and need no such note.
