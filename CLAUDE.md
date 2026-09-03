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
  src/engine/      the engine; window/ (winit) so far
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

That is the entire build. **No CMake, no C++ toolchain, no `build.rs`, no FFI**, and five
direct dependencies: `thiserror`, `wgpu`, `winit`, `pollster`, `bytemuck` (each justified
in a comment in `Cargo.toml`). Release builds use full LTO and one codegen unit.

The CMake tree under `legacy/` is not part of this build and is not maintained — don't
invest in it and don't wire it back in. (`.github/workflows/kstrike-compile.yml` still
describes the old CMake build; it is `master`-gated and stale with respect to this
branch, where the top-level `CMakeLists.txt` has moved into `legacy/`.)

There is a unit test suite (`cargo test`, 193 tests), and the binary now **runs**: it
mounts the game filesystem, opens a window, clears it, and can draw a quad through a real
`.vmt` from the game's content — its shader, its textures and its pipeline state
(`-vmt <name>`). It is **not a runnable game** and won't be until the rest of the boot
path exists — engine host loop, map loading, game layer. Verification is still mostly against the reference: read
`legacy/`, compare behavior, reason it through. There is no hybrid binary to run.

To see it work you need a directory containing a mod directory with a `gameinfo.txt`:

```
cargo run -- -basedir /path/to/game -game portal2 -window -width 1280 -height 720
cargo run -- -basedir /path/to/game -game portal2 -window -vmt metal/metalwall048a
```

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
  ScaleformUI at once.
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
- **`src/materials/` — stages 1-3 of 8 ported.** `Renderer` owns `wgpu`'s
  instance/adapter/device/queue/surface and exposes one frame boundary
  (`begin_frame` → `clear` → `present`). The `IShaderDevice`/`IShaderAPI` tower is
  deleted, not ported, so `shaderapidx9`, `glmgr`, `ps3gcm`, `shaderapiempty` and `togl`
  have no counterpart. Stage 2 added the texture path: `Vtf` (`.vtf` 7.0-7.5),
  `ImageFormat` (Valve's format table, the mip-offset arithmetic, and the CPU conversions
  for formats `wgpu` lacks), and `TextureCache` (name → `Texture`, falling back to the
  error checkerboard). Stage 3 added the material path: `Vmt` (`.vmt` parsing, patch
  chains, conditional keys, flags), `MaterialVar` (the value grammar and Valve's
  coercions), `ShaderKind` (`UnlitGeneric`, rewritten in WGSL over the §7.4 constant ABI
  and the §7.5 prelude), `PipelineCache` (replacing `StateSnapshot_t` and deleting
  `TransitionTable.cpp`), and `MaterialCache` (name → `Material`, falling back to the
  error material). `-vmt <name>` draws one material full-screen; that switch and
  `preview.rs` are stage 3's verification path and are deleted when stage 4 lands.
  **API: `rustdocs/MATERIALS.md`** — read it before calling in, in particular for the two
  conventions that produce a plausible wrong picture rather than an error: **matrices are
  column-major and multiply on the left** (Valve's are the reverse on both counts), and
  **`ColorSpace` is a load-time decision the shader makes**, because Valve made it
  per-sampler in the shader. Plan: `portdocs/MATERIALSYSTEM.md`.
  Stages 4-8 (meshes and the render context, lightmaps, the rest of the shader set,
  paint maps) are not started. Still settled for those: the shaders are
  **rewritten in WGSL** from the `.fxc` HLSL in `stdshaders/`, and
  Valve's static/dynamic shader-combo system is deleted with them — `UnlitGeneric`
  needed no source-text variants at all, so §10's "how are variants expressed" question
  is still open and still unforced.
- **`src/engine/` — `window/` ported, the other 12 subsystems not started**
  (`portdocs/ENGINE.md`, `rustdocs/ENGINE.md`). Conclusion stands: don't port `engine` as
  one unit. Each of its 23 subsystems becomes its own Rust module under `src/engine/`
  (`audio/`, `net/`, `host/`, `world/`, `console/`, …) — 13 modules surviving, ~45,700
  lines deleted outright. `window/` is the `winit` event loop plus `VideoConfig`; input
  and the engine tick are not implemented, and frame pacing lives here rather than in the
  engine (see `rustdocs/ENGINE.md` gotcha #3).
- **Everything else is unported** and lives in `legacy/`.

Next on the boot path per `PORTING.md`: `materialsystem` stage 4 — typed vertex structs,
static and dynamic buffers, and the render state / matrix / render-target stacks, which
between them bring the depth buffer the stage-3 pipeline state is already carrying but
cannot apply. **Read `studiorender/` and `engine/`'s draw paths before fixing that API**
(`portdocs/MATERIALSYSTEM.md` §9); unlike stage 3 it is not strictly ahead of the engine
host loop, since it is the loop's requirements that should shape it.

### Known warts, and what triggers fixing them

Deliberate small compromises, recorded so nobody has to rediscover them and nobody
"fixes" one prematurely. Each names the condition that makes it worth doing. Both are
also commented at the site.

- **`CommandLine` lives in `src/launcher/` but is read from `src/engine/window/`.** The
  dependency direction is backwards — the launcher boots the engine, so the engine
  importing from it is upside down. It sits there because that is where it is built, but
  Valve kept `CommandLine()` in `tier0` precisely because *everything* reads it, and this
  port will end up the same way. **Move it to a crate-level `src/cmdline.rs` when a third
  subsystem needs it** — not before, since the only cost today is one odd-looking import,
  and the only benefit of moving early is churn in reviewed code.
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

- **Verify signatures against the source before writing them down.** Grep the `pub`
  items; do not transcribe from memory. A confidently wrong API doc is worse than none.
- **Record deliberate divergences from Valve's behavior**, with the switch or function
  that reverses them. Those are exactly what a future session cannot rediscover.

Rustdoc comments in the source stay the authority on individual items; `rustdocs/` carries
what doesn't fit on one item.

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
`query_graph`) over blind grep/Explore; the tree is too large to explore by hand. Fall
back to `search_code`/filesystem grep for literal text or when graph coverage looks thin.

**Check `check_index_coverage` before trusting any negative result.** Coverage on large
engine files is frequently partial (~6,300 files have unparsed ranges) — e.g. all of
`CEngine::Frame` and `FilterTime` at `legacy/engine/sys_engine.cpp:264-686` is unparsed.
Read flagged ranges from source and treat graph results there as under-reporting.

See `PORTING.md`'s "Using codebase-memory-mcp" section for how this applies specifically
to porting decisions.
