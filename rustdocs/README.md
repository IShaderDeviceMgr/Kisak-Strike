# rustdocs/

API references for the Rust subsystems that have actually been implemented under `src/`.

**These describe what exists. [`portdocs/`](../portdocs/) describes what to build.** The
two are deliberately separate:

| | `portdocs/<MODULE>.md` | `rustdocs/<MODULE>.md` |
|---|---|---|
| Written | *before* the port | *with* the port, updated as it changes |
| Subject | the C++ in `legacy/` | the Rust in `src/` |
| Contains | inventory, sizes, what to port/replace/delete, staged plan, open questions | public types and signatures, usage, invariants, gotchas, what's deferred |
| Answers | "how do I port this?" | "how do I *use* this?" |
| Lifetime | can go stale once the module lands | must stay accurate forever |

A module gets a `rustdocs/` entry once it has a public API other subsystems will call.
Naming matches `portdocs/`: `src/filesystem/` → `rustdocs/FILESYSTEM.md`.

## Current docs

- [`FILESYSTEM.md`](FILESYSTEM.md) — `src/filesystem/`. `Vfs`, `PathId`, mounts,
  `gameinfo.txt` parsing, the KeyValues reader, and VPK reading.
- [`MATERIALS.md`](MATERIALS.md) — `src/materials/`. `Renderer` and the frame boundary,
  the `wgpu` decisions the rest of the renderer inherits, the texture path (`Vtf`,
  `ImageFormat`, `Texture`, `TextureCache`), the material path (`Vmt`, `MaterialVar`,
  `ShaderKind`, `PipelineCache`, `Material`), meshes and the render context, and the
  lightmap atlas. Its porting doc is
  [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named after the C++
  module, while this one is named after the Rust module.
- [`ENGINE.md`](ENGINE.md) — `src/engine/`. `window` (the `winit` event loop,
  `VideoConfig`, frame pacing), `host` (the state machine and frame clock), `world` (the
  `.bsp` reader, lightmap packing, and the batches a map draws as), `input` (buttons,
  bindings, the key-up latch) and `console` (cvars, the command buffer, the dialog), plus
  `Engine` itself and how the frame is composed.
- [`CLIENT.md`](CLIENT.md) — `src/client/`. The **game client**, and the first game module
  in the tree: `Client`, `UserCmd`, `Buttons`/`KButton`'s fractional `KeyState`, `Player`
  and `MOVETYPE_NOCLIP`, `FullNoClipMove`, and `ViewAngles`. Not to be confused with
  `ENGINE.md` §7.5's *client connection*, which does not exist yet.

## Why these exist

Porting sessions lose context. A cold-started session that has to re-derive an API by
reading 3,500 lines of source will burn most of its budget doing so, and is likely to
miss the non-obvious rules — which lookups skip which mounts, why a path type carries two
spellings of the same string, which behaviors deliberately diverge from Valve's. Those
belong in prose, once, next to the code they describe.

Rustdoc comments in the source stay the authority on individual items; these files carry
the parts that don't fit on a single item — cross-cutting semantics, worked examples, and
the "why is it like this" that a `///` on one function can't hold.
