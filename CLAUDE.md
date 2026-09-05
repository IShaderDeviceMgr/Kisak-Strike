# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A **rewrite of the Source engine in Rust**, targeting **Portal 2**, using Valve's
`cstrike15` tree as the reference implementation.

The starting point was Kisak-Strike ("Gentoo Offensive"), a Linux source port of CS:GO's
engine built from Valve's leaked/derived tree with a hand-written CMake build replacing
VPC. That entire C++ tree now lives in `legacy/` and is **reference material only** — it
is not compiled, not linked, and not edited.

```
/                  Rust crate root — Cargo.toml, src/
  src/main.rs      entry point
  src/launcher/    process bootstrap
  src/filesystem/  search paths, gameinfo.txt, VPK reading
  src/materials/   the GPU device and frame boundary (wgpu), textures, materials
  src/engine/      the engine; window/ (winit), host/, world/, input/, console/ (egui)
  legacy/          the original C++ tree, verbatim; read-only reference
  portdocs/        per-module porting design docs (what to build)
  rustdocs/        per-module API references (what exists)
  PORTING.md       standing design reference — read before any port work
```

Game assets (maps, models, original binaries) are not in this repo and never were —
they come from Valve's depots via DepotDownloader and the companion `Kisak-Strike-Files`
repo.

**Read [`PORTING.md`](PORTING.md) before starting or reviewing any port-related work**,
and keep it updated as modules land or the plan changes. It is the source of truth; this
file is a summary of it.

## Build

```
cargo build
cargo test
```

That is the entire build. **No CMake, no C++ toolchain, no `build.rs`, no FFI**, and nine
direct dependencies: `thiserror`, `wgpu`, `winit`, `pollster`, `bytemuck`, `glam`, and
`egui`/`egui-winit`/`egui-wgpu` (each justified in a comment in `Cargo.toml`). Release
builds use full LTO and one codegen unit.

The CMake tree under `legacy/` is not part of this build and is not maintained — don't
invest in it and don't wire it back in. (`.github/workflows/kstrike-compile.yml` still
describes the old CMake build; it is `master`-gated and stale with respect to this
branch, where the top-level `CMakeLists.txt` has moved into `legacy/`.)

There is a unit test suite (`cargo test`, 456 tests), and the binary now **runs, loads a
map, lets you fly around it and has a working developer console**: it mounts the game
filesystem, opens a window, runs an
engine frame loop with a real host state machine, **reads the shipped `cfg/config_default.cfg` and
`cfg/valve.rc` and boots through them**, reads a Portal 2 `.bsp`, packs its baked lightmaps into an atlas,
draws its world geometry **lit**, moves the view with WASD and the mouse, and drops an
`egui` console over the top of it on `` ` `` — scrollback, history, tab completion, the
list commands (`cvarlist`, `help`, `find`, `differences`, `toggle`, `incrementvar`), and
every cvar and command the port has registered. It is **not a runnable game** — there is no simulation, sound or
netcode, and nothing that moves is a player — but the boot path is continuous from
`main` to a rendered, lit level you can look around.

To see it work you need a directory containing a mod directory with a `gameinfo.txt`:

```
cargo run --release -- -basedir /path/to/game -game portal2 -window +map sp_a1_intro1
cargo run -- -basedir /path/to/game -game portal2 -window -vmt tools/toolsblack
```

**`sp_a1_intro1` draws lit**: 5,512 of 5,638 faces, 58 of its 66 materials resolving,
4,828 surfaces with real baked lighting over 12 atlas pages. The 8 materials that still
draw as the magenta error checkerboard are `maps/<map>/…` cubemap patches living in the
`.bsp`'s embedded pak lump, which the `Vfs` does not mount yet. The scene is **dimmer than
the shipped game** because there is no tone mapper: HDR lightmaps reach the shader
unexposed. The camera is a **free-fly noclip camera** — WASD, space and left control,
left shift to walk, mouse to look, **Escape to release the cursor** — standing in for a
player that does not exist yet; it is a placeholder and is documented as one.

`` ` `` opens the console (Escape or `` ` `` closes it), which releases the cursor for as
long as it is up. Tab and the arrow keys cycle completions; an empty entry cycles history
instead. `+toggleconsole` on the command line opens it at startup, which is how it can be
inspected without touching the keyboard.

Verification is otherwise still mostly against the reference: read `legacy/`, compare
behavior, reason it through. There is no hybrid binary to run.

## The port: standing decisions

Full rationale for each of these is in `PORTING.md`; this is the short form.

- **One crate, one binary.** Every former Valve module becomes a module under `src/`
  (`src/engine/`, `src/filesystem/`, …) — not a separate crate, not a separate `.so`.
  No `dlopen` app system, no `CreateInterface`, no `IAppSystem` lifecycle, no interface
  version strings. Ordinary Rust calls between modules. The one possible exception is
  `libsteam_api.so`, a closed-source C-ABI blob, if Steam integration is added.
- **`legacy/` is fully decoupled.** No `cxx`, no `extern "C"` bridge, no vtable shims,
  no adapters. Nothing links the C++. This is why there's no incremental scaffolding and
  why ordering matters: **follow the boot path depth-first** rather than going broad.
- **The Rust interface is the contract.** Read `legacy/` to learn *what* a subsystem
  does and *why*, then design the Rust API from scratch. Never transliterate a Valve
  signature, never preserve an interface shape "for now". The test: if a Rust signature
  only makes sense once you've read the C++ it came from, it's wrong.
  What *does* carry across: algorithms, externally-fixed data layouts, protocol state
  machines, physics and math, frame-ordering constraints, and the bug fixes encoded in
  odd-looking special cases. Keep the knowledge, discard the encoding.
- **POSIX only** — Linux primary, macOS second. Windows and consoles (X360, PS3) are
  permanently out of scope, not "not yet"; don't write `#[cfg(windows)]` scaffolding.
  When reading `legacy/`, skim for `POSIX`/`LINUX`/`OSX` and unconditional code and
  disregard the rest.
- **Replacements, all decided:** `wgpu` replaces `materialsystem/shaderapidx9` + `togl`;
  `winit` replaces `ILauncherMgr`/SDL2/Cocoa; `egui` replaces vgui2, RocketUI, and
  ScaleformUI at once — **and has now landed**, as three crates split across three layers:
  `window/` is the `winit` boundary, `console/ui.rs` is the widgets, `materials/ui.rs` is
  the `wgpu` boundary.
- **`tier0`–`tier3` are not tasks.** They're replaced by `std` and crates as a side
  effect of porting everything else, never translated. Same for anything else the Rust
  ecosystem does better — compression, hashing, thread pools, HTTP, serialization.
- **Separate serialization *mechanism* from *format*.** Always modernize the mechanism
  (`binrw`/`nom`/`deku` over hand-rolled `bf_read` calls; `prost` against the existing
  `.proto` files). The format is only ours to change where we own both ends — Valve
  asset formats (`.bsp`, `.mdl`, `.vtf`, `.vmt`, `.vpk`), demo files, and Steam-facing
  protocols are fixed regardless.
- **Target is Portal 2 from a cstrike15 base**, so `legacy/game/{client,server,shared}/portal{,2}`
  is in scope and CS:GO-specific content generally isn't. Watch for CS:GO-shaped
  defaults in shared systems (e.g. `DEFAULT_HL2_GAMEDIR` is `"csgo"`) that need
  retargeting. `legacy/engine/paint.cpp` is essential for Portal 2, not vestigial.

### Status

- **`src/launcher/` — ported.** Command line, single-instance lock, early-error
  reporting, startup sequence. Mounts the filesystem, then hands off to
  `engine::window::run`.
- **`src/filesystem/` — ported.** `Vfs` over an ordered mount list: `gameinfo.txt` ->
  search paths, KeyValues reader, case-folded directory mounts, and VPK reading
  (v1/v2/headerless, multi-archive, embedded chunks). Async, `.bsp` pak lumps and
  `sv_pure` are deferred. **API: `rustdocs/FILESYSTEM.md`** (read this before calling it);
  porting decisions and the C++ inventory: `portdocs/FILESYSTEM.md`.
- **`src/materials/` — stages 1-5 of 8 ported.** `Renderer` owns `wgpu`'s
  instance/adapter/device/queue/surface and exposes one frame boundary
  (`begin_frame` → record passes → `present`). The `IShaderDevice`/`IShaderAPI` tower is
  deleted, not ported, so `shaderapidx9`, `glmgr`, `ps3gcm`, `shaderapiempty` and `togl`
  have no counterpart. Stage 2 added the texture path: `Vtf` (`.vtf` 7.0-7.5),
  `ImageFormat` (Valve's format table, the mip-offset arithmetic, and the CPU conversions
  for formats `wgpu` lacks), and `TextureCache` (name → `Texture`, falling back to the
  error checkerboard). Stage 3 added the material path: `Vmt` (`.vmt` parsing, patch
  chains, conditional keys, flags), `MaterialVar` (the value grammar and Valve's
  coercions), `ShaderKind` (`UnlitGeneric`, rewritten in WGSL over the §7.4 constant ABI
  and the §7.5 prelude), `PipelineCache` (replacing `StateSnapshot_t` and deleting
  `TransitionTable.cpp`), and `MaterialCache` (name → `Material`, falling back to the
  error material). Stage 4 added geometry and the render context: `mesh` (typed
  `#[repr(C)]` vertex structs replacing `CMeshBuilder`, static buffers, and a per-frame
  bump arena for dynamic ones), `target` (`DepthBuffer`, `RenderTarget`) and `context`
  (`RenderContext`, `Pass`, `Camera`). **Valve's matrix, render-target and scissor stacks
  are deleted rather than ported** — a `wgpu` render pass already *is* the state they
  saved and restored, so a target, a camera and a viewport are the arguments to opening
  one, and nesting becomes sequencing. `glam` arrives here as the `mathlib` substitution.
  `-vmt <name>` draws two cubes and a ground quad; that switch and
  `preview.rs` hold the nineteen GPU tests and stay until something else can run them
  against real pixels. Stage 5 added lightmaps: `lightmap` (a faithful `CImagePacker`
  port, `Rgba16Float` atlas pages of linear radiance, the `ColorRGBExp32` decode and the
  bumped-lightmap correction), `mesh::WorldVertex`, a fourth bind group holding the atlas
  page, and `LightmappedGeneric` in WGSL — flat and radiosity-normal-mapped.
  **API: `rustdocs/MATERIALS.md`** — read it before calling in, in particular for the five
  conventions that produce a plausible wrong picture rather than an error: **matrices are
  column-major and multiply on the left** (Valve's are the reverse on both counts); **a
  lightmap sample decodes with `TexLightToLinear`, not `ColorRGBExp32ToVector`**, which is
  the same thing times 255 and gives a uniformly white screen; **`ColorSpace` is a
  load-time decision the shader makes**, because Valve made it per-sampler in the shader;
  **per-draw constants need distinct arena slots**, because `Queue::write_buffer` stages
  its copy ahead of the whole command buffer and a rewritten uniform would reach every
  draw in the frame; and **`glam`'s `near`/`far` are distances along `-z`**, so a
  hand-built projection can silently invert the depth comparison.
  Plan: `portdocs/MATERIALSYSTEM.md`.
  Stage 5 also brought `src/materials/ui.rs` with the console — `UiRenderer`, an
  `egui_wgpu::Renderer` over the frame's encoder. **It is not part of the material
  system**: `egui` owns its own pipeline, font atlas and vertex format, and it lives in
  `materials/` only because `Frame::parts` is `pub(super)` and opening a pass belongs on
  this side of that boundary.
  Stages 6-8 (the rest of the shader set, paint maps, GPU morph) are not started. Still
  settled for those: the shaders are **rewritten in WGSL** from the `.fxc` HLSL in
  `stdshaders/`, and Valve's static/dynamic shader-combo system is deleted with them —
  two shaders in, neither needed a source-text variant, so §10's "how are variants
  expressed" question is still open and still unforced. **`LightmappedGeneric` was
  expected to force it and did not**: bumped and unbumped share one vertex layout, because
  the bumped diffuse path never leaves tangent space.
- **`src/engine/` — 5 of 14 modules ported: `window/`, `host/`, `world/`'s geometry and
  lightmaps, `input/` (stages 1-4 of 5), and `console/` (all five stages, complete)**
  (`portdocs/ENGINE.md`, **`rustdocs/ENGINE.md`** — read that before calling in).
  Conclusion stands: don't port `engine` as one unit; each of its 23 subsystems becomes
  its own module, 14 surviving, ~45,700 lines deleted outright.
  `host/` is `CHostState`'s state machine (eight states become five, keeping the
  invariant that every path to a new level goes *through* `GameShutdown`) plus
  `FilterTime`'s policy; it depends on `std` alone, because loading a level is a `Level`
  trait, and is tested without a GPU. **`CEngine`'s outer `m_nDLLState`/`m_nQuitting`
  machine is deleted** — quit-vs-restart is a return value that reaches the launcher as
  `window::RunOutcome`. `world/` reads the `.bsp` lumps the renderer walks, packs each
  surface's baked light into the material system's lightmap atlas, and groups faces into
  per-(material, page) batches at load — which is exactly what Valve's *sort ID* was.
  **Materials are resolved before the geometry**, because a surface's vertex layout comes
  from its shader and how wide a lightmap block it reserves comes from whether its
  material has a `$bumpmap`; neither is answerable from the `.bsp`. The **`winit` control-flow inversion is
  resolved**: `FilterTime` split into policy (`host::FrameClock`) and mechanism
  (`window`'s `ControlFlow::WaitUntil`), and neither half may sleep.
  `input/` is stages 1-4 of `portdocs/ENGINE_INPUT.md`'s five: `Button`'s flat dense
  space with Valve's shipped key names, an event queue **pushed between ticks and
  drained once per tick** inside `Engine::frame`, `ViewAngles`' faithful
  `ApplyMouse`/`ClampAngles`/`AngleVectors`, a free-fly camera at
  `FullNoClipMove`'s speeds that **deleted the turntable**, bindings, and UI precedence.
  It names no `winit` type and no `egui` type —
  `window/` translates, `input/` decides — so it is tested without a window, which is
  also what leaves room for `gilrs` at stage 5. **Stage 4 is `FilterKey`'s key-up latch**
  (`keys.cpp:1189`): the target that consumed a *press* is recorded per button and the
  matching *release* goes there and nowhere else, whoever wants it by then. That is a
  correctness fix rather than polish — without it, clicking and then opening the console
  leaves `+attack` held forever, which is what every stuck-key bug in a Source-like
  engine is. `console/` stage 4 is the `egui` dialog it pairs with: `Console::complete`
  is `RebuildCompletionList` (a question about the registry, not about a widget) and
  `ConsoleUi` is the dialog, naming `egui` and nothing else, so it is unit-tested against
  a headless `egui::Context` with no window and no GPU. **`console/` stage 5 finishes the
  module**: the six list commands, all built-ins because they need the registry and the
  log and nothing else, plus `console/describe.rs` — the one implementation of
  `ConVar_PrintDescription`, replacing the shortened copy stage 1 had inlined and
  collapsing the *three* tables the C++ spells the same six flags in. One rule there
  produces a plausible wrong answer rather than an error: **`Cvar::string` is stale for an
  `FCVAR_NEVER_AS_STRING` cvar**, so anything comparing or displaying a value goes through
  `describe::value`/`describe::is_at_default` — otherwise `differences` reports every such
  cvar as unchanged for ever.
  Not implemented: simulation, visibility, collision, displacements, brush
  entities, static props, the skybox, dynamic lights and lightstyle animation.
  **One `egui` rule that produces a plausible wrong behavior rather than an error:** the
  key bound to `toggleconsole` is never shown to `egui` at all, on either edge
  (`keys.cpp:1319`'s `KEY_BACKQUOTE` bypass). Drop it and the key that opens the console
  cannot close it, and types a backquote into the entry on the way.
  **The one divergence that will bite:** world triangles are emitted with their **winding
  reversed**, because Valve's `D3DCULL_CCW` and this port's `front_face: Ccw` read
  identically and are not the same thing (GL's framebuffer is Y-up, WebGPU's is Y-down,
  and facing is decided after the flip). In file order a map draws as an empty clear
  colour. `rustdocs/ENGINE.md` gotcha #1 has the evidence and the open question about
  fixing it in `PipelineCache` instead.
- **Everything else is unported** and lives in `legacy/`.

Next: **`console/` is finished and nothing in `input/` blocks the boot path any more.**
The candidates, in the order they are worth doing:

- **`materialsystem` stage 6** (`VertexLitGeneric` and the rest of the shader set). A
  breadth move: unblocked, needed by every model, not on the boot path.
- **`client/`** (`ENGINE.md` §7.5) is what the boot path itself wants next, and it is the
  module that finally takes `ViewAngles`, `FlyCamera` and `MoveButtons` out of `input/` —
  see the wart below.

### Known warts, and what triggers fixing them

Deliberate small compromises, recorded so nobody has to rediscover them and nobody
"fixes" one prematurely. Each names the condition that makes it worth doing. Both are
also commented at the site.

**Resolved:** `CommandLine` used to live in `src/launcher/` and be read from
`src/engine/window/`, to be moved "when a third subsystem needs it". `console/` was that
third subsystem — `stuffcmds` and the `+<cvar>` default seeding both read it — so it now
lives at `src/cmdline.rs`. The move also fixed a real divergence: `CCommandLine::ParmValue`
refuses a value beginning with `-` or `+` (`tier0/commandline.cpp:646`) and the port's
`value()` did not, which would have had `-window` swallow `+map`.

- **The view angles and the free-fly camera live in `src/engine/input/view.rs`.** They
  belong to the client: `CInput::AdjustAngles` reads and writes them through
  `engine->GetViewAngles`/`SetViewAngles`, which resolve to `CClientState::viewangles`
  (`engine/cdll_engine_int.cpp:1050`), and only the client DLL mutates them. They sit in
  `input/` because there is nowhere else — the alternative, `engine/mod.rs`, spreads the
  same code over two modules instead of one. **Move them to `client/` when it exists**,
  along with `FlyCamera`, whose real replacements are `CUserCmd`, `CGameMovement` and
  `CViewRender::SetUpView`. Until then, do not grow `FlyCamera` towards `CUserCmd`:
  `kbutton_t`'s `down[2]` and its fractional `KeyState` are correct and are the right
  design, but building them against a camera instead of a player bakes in the wrong
  consumer.
- **`gameinfo.txt` is parsed twice at startup.** `src/launcher/mod.rs` reads it for the
  window title (`gameinfo.txt`'s `game` key, `engine/sys_mainwind.cpp:1261`), and
  `Vfs::mount_game` reads it again to build the search paths. A few kilobytes, once. The
  alternative — threading a `GameInfo` back out of a `Vfs` that has no other use for one —
  is worse for one consumer. **When a second subsystem wants gameinfo, load it once in the
  launcher and pass it down**; the Steam app ID (`SteamAppId`, already parsed into
  `GameInfo::steam_app_id`) is the obvious next consumer.

### Per-module porting docs

For any module with real internal complexity (not a small leaf like `launcher`), a
design/porting doc belongs in `portdocs/<MODULE>.md`, named after the module directory in
`SCREAMING_SNAKE_CASE` (`engine/` → `portdocs/ENGINE.md`). Write and consult it before
doing that module's port; see `PORTING.md`'s "Per-module porting docs" section for what
goes in one.

`portdocs/LAUNCHER.md` predates the current architecture and carries a banner saying what
changed for it. Its *plan* assumes the old FFI-bridged model; its factual content — module
behavior analysis — remains accurate and is the reason to keep it. `portdocs/ENGINE.md`
used to carry the same banner and has been rewritten against the current architecture.

### Per-module API docs (`rustdocs/`) — required for every subsystem you implement

**When you finish implementing a subsystem under `src/`, write its API reference in
`rustdocs/<MODULE>.md`** (same `SCREAMING_SNAKE_CASE` naming as `portdocs/`), and update
it whenever the API changes. This is not optional polish — porting sessions get their
context cleared, and a cold-started session that has to re-derive an API from source
burns most of its budget doing so and still misses the non-obvious rules.

`portdocs/` and `rustdocs/` are deliberately different documents:

| | `portdocs/<MODULE>.md` | `rustdocs/<MODULE>.md` |
|---|---|---|
| Written | *before* the port | *with* the port |
| Subject | the C++ in `legacy/` | the Rust in `src/` |
| Answers | "how do I port this?" | "how do I *use* this?" |
| Lifetime | can go stale once the module lands | must stay accurate forever |

An API doc should cover, roughly in this order: a one-line summary and status table; a
quick-start example that actually compiles; the core public types with real signatures;
cross-cutting semantics that no single `///` can hold (search order, scoping rules,
lifecycle); an **invariants-and-gotchas** list ordered by how likely each is to bite; what
is deliberately *not* implemented and why; how to extend it; and which tests guard which
behavior. `rustdocs/FILESYSTEM.md` is the worked example.

Two rules that keep these trustworthy:

- **Verify signatures against the source before writing them down.** Search the `pub`
  items with the `Grep` tool; do not transcribe from memory. A confidently wrong API doc
  is worse than none.
- **Record deliberate divergences from Valve's behavior**, with the switch or function
  that reverses them. Those are exactly what a future session cannot rediscover.

Rustdoc comments in the source stay the authority on individual items; `rustdocs/` carries
what doesn't fit on one item.

## Searching: use the `Grep` tool, never shell `grep`

**Always search with the `Grep` tool. Do not call `grep`, `rg` or `ag` through `Bash`.**
This is not a style preference — shell `grep` gives *wrong answers* on this repo:

- **`legacy/` is ISO-8859 (latin-1), not UTF-8.** GNU/BSD `grep` classifies those files as
  binary and prints `Binary file … matches` or, piped, nothing at all. A search for a
  symbol that is sitting right there comes back empty, and the natural conclusion — "that
  doesn't exist, it must have been deleted" — is wrong. This has already cost one session:
  `SurfaceCtx_t` and `SurfComputeLightmapCoordinate` both read as absent from every header
  in the tree until the encoding was noticed.
- The `Grep` tool also handles ignore rules, multiline mode and output modes
  (`files_with_matches`, `content`, `count`) without a pipeline, and does not blow up
  context on a large hit.

**If the `Grep` tool is not available** — some session configurations disable it and route
everything through `Bash` — then shell `grep` is the fallback, and **`-a` is mandatory on
anything under `legacy/`**: `grep -arn "Symbol" legacy/`. Without it a negative result
means nothing.

Either way: when a search comes back empty, **suspect the encoding before you conclude the
symbol is gone.**

## Reading the reference tree (`legacy/`)

Paths throughout `PORTING.md` and `portdocs/` are given relative to the original tree
(`engine/sys_dll2.cpp`, `public/tier1/interface.h`, …) — prefix them with `legacy/` to
open them. `legacy/` shrinks as subsystems land in `src/`, and directories can be deleted
once nothing needs reading from them any more.

How the C++ tree is organized, which is what you need to navigate it (not to build it):

- **Module layout mirrors Valve's original VPC projects.** Each subsystem (`engine/`,
  `tier1/`, `materialsystem/`, `vphysics/`, …) was its own static lib, shared lib, or
  executable with its own `CMakeLists.txt` — direct ports of the old `.vpc` scripts, so
  comments referencing `*.VPC` files are intentional history. The per-module
  `CMakeLists.txt` files remain the fastest way to see exactly which sources composed a
  given module, since they list files one-by-one rather than globbing.
- **`legacy/CMakeLists.txt`** is the master `add_subdirectory()` list, gated on
  `DEDICATED` and on `USE_ROCKETUI`/`USE_SCALEFORM` — useful for seeing which modules a
  client vs. dedicated build actually pulled in.
- **`legacy/public/`** — shared interface headers used across module boundaries. Check
  here first when tracing how two subsystems talk to each other.
- **`legacy/game/{client,server,shared}`** — the gameplay code. `client`/`server` were
  separate binaries; `shared` compiled into both.
- **`legacy/common/`** — shared non-engine utilities (GameUI, config management) used by
  launcher/engine/tools.
- **`legacy/ivp/`** is a git submodule (`kisak-physics`) providing the Havok/IVP physics
  backend consumed by `vphysics/`.
- **`legacy/external/`, `legacy/thirdparty/`** — vendored third-party libs (crypto++,
  zlib, libpng, protobuf, RmlUi, SDL2, quickhull, …). Most are slated for replacement by
  crates; `common/netmessages.proto` and friends are the exception worth keeping.
- **`legacy/vpc_scripts/`** — Valve's original VPC build scripts, kept for history.
- **`legacy/devtools/`, `legacy/utils/`** — standalone dev tools.

## Codebase knowledge graph (codebase-memory-mcp)

This repo (~547k nodes) is indexed by the `codebase-memory-mcp` MCP server, project
`Users-damienbrown-Documents-SourceEngineWork-Kisak-Strike`. The index reflects the
current layout, so **graph paths are `legacy/`-prefixed**.

For structural questions — finding a symbol, tracing callers/callees, checking who
implements or registers a given interface, orienting in an unfamiliar module — prefer its
graph tools (`search_graph`, `trace_path`, `get_code_snippet`, `get_architecture`,
`query_graph`) over blind text search; the tree is too large to explore by hand. Fall
back to `search_code` or the `Grep` tool for literal text, or when graph coverage looks
thin — see "Searching" above for why that fallback must not be shell `grep`.

**Check `check_index_coverage` before trusting any negative result.** Coverage on large
engine files is frequently partial (~6,300 files have unparsed ranges) — e.g. all of
`CEngine::Frame` and `FilterTime` at `legacy/engine/sys_engine.cpp:264-686` is unparsed.
Read flagged ranges from source and treat graph results there as under-reporting.

See `PORTING.md`'s "Using codebase-memory-mcp" section for how this applies specifically
to porting decisions.
