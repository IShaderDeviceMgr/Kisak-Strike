# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Kisak-Strike ("Gentoo Offensive") is a source port of CS:GO's engine (Source engine, CSTRIKE15 branch) targeting Linux, built from Valve's leaked/derived source tree. It is a huge legacy C++ codebase (hundreds of subsystems: engine, materialsystem, vphysics, vgui2, tier0-3, etc.) organized the way Valve's original VPC-based build described it; this repo replaced VPC with a hand-written CMake build (see `cmake/` and the per-module `CMakeLists.txt` files, which are direct ports of the old `.vpc` project scripts — comments referencing `*.VPC` files are intentional history, not stale cruft).

Game assets (maps, models, original CS:GO binaries) are NOT in this repo — they come from Valve's depots via DepotDownloader and from a separate companion repo, `Kisak-Strike-Files`. This repo only contains engine/game source code.

## Build

Out-of-tree CMake build; the binary output lands in `../game/` (i.e. a sibling directory of the repo root), so the repo must be cloned inside its own parent folder.

```
mkdir build && cd build
cmake .. [options]
make -j<N>
```

Common option combinations (see `kisak-strike-build-options.cmake`):
- Default (no flags): base VGUI UI, client+server, not dedicated.
- `-DUSE_ROCKETUI=1`: build with the RmlUi-based custom UI (open source, what CI builds).
- `-DUSE_SCALEFORM=1`: build with the proprietary Scaleform UI blob (not recommended, mutually exclusive with RocketUI).
- `-DDEDICATED=1`: build the dedicated server instead of the client. **Not in-tree compatible** with a client build — do a clean `cmake ..` re-run (or separate build dir) when switching.
- `-DUSE_ASAN=1`: AddressSanitizer. Break on errors with gdb via `b __asan::ReportGenericError`.
- `-DUSE_TRACY=1` (+ `-DTRACY_STORE_LOGS=1`): Tracy profiler support.
- `-DRELEASE_ASSERTS=1`: keep asserts in a release build.
- `CMAKE_BUILD_TYPE=RELEASE|DEBUG` (see `cmake/source_posix_base.cmake` for the flags each implies).

CI (`.github/workflows/kstrike-compile.yml`) is the canonical reference build: Ubuntu, installs SDL2/SDL2_mixer/openal/curl/openssl/fontconfig deps, builds client with `-DUSE_ROCKETUI=1` then rebuilds dedicated with `-DDEDICATED=1`, and pulls in `Kisak-Strike-Files` for the non-code game assets before packaging.

There is no automated test suite; there's a `unittests/` project but validating changes in practice means building and running the game (or dedicated server) directly. `devtools/` and `utils/` contain standalone dev tools (also built via their own CMakeLists).

## Architecture notes

- **Module layout mirrors Valve's original VPC projects.** Each subsystem (e.g. `engine/`, `tier1/`, `materialsystem/`, `vphysics/`) is its own static lib, shared lib, or executable target with its own `CMakeLists.txt`. These all funnel through the shared includes in `cmake/`:
  - `source_base.cmake` — global defines applied everywhere (`CSTRIKE_REL_BUILD`, `ALLOW_DEVELOPMENT_CVARS`, Tracy/ASAN/RELEASE_ASSERTS toggles). This is the file most likely to need edits for a new global build option.
  - `detect_platform.cmake` — sets `WIN32`/`LINUXALL`/`OSXALL`, `LINUX64`/`LINUX32`, and the `_client.so`/`.dll`/`.dylib` extension convention.
  - `source_lib_base.cmake` / `source_dll_base.cmake` (+ their `*_posix_base.cmake` counterparts) — included by every static-lib or shared-lib module respectively; set up `OUTLIBDIR`/`OUTDLLEXT`/etc.
  - `common_functions.cmake` — helper macros like `MacroRequired`.
  - Per-module `CMakeLists.txt` files add sources one-by-one via `target_sources(${OUTLIBNAME} PRIVATE "file.cpp")` — this pattern (not glob) is used throughout, so new source files must be added explicitly to the relevant CMakeLists.
- **Top-level `CMakeLists.txt`** is the master list of `add_subdirectory()` calls; it's gated heavily on `DEDICATED` (skips UI/input/sound-output modules for dedicated server builds) and on `USE_ROCKETUI`/`USE_SCALEFORM` (mutually exclusive UI backends, added at the very end).
- **`game/client`, `game/server`, `game/shared`**: the actual CS:GO/CSGO gameplay code (weapons, entities, HUD, game rules). `client` and `server` are separate binary targets; `shared` holds code compiled into both.
- **`public/`**: shared interface headers used across module boundaries (the engine's public API surface) — check here first when tracing how two subsystems talk to each other.
- **`common/`**: shared non-engine utilities (GameUI, config management, etc.) used by launcher/engine/tools.
- **`ivp/`** is a git submodule (`kisak-physics`, a separate repo) providing the Havok/IVP physics backend consumed by `vphysics/`.
- **`external/` and `thirdparty/`**: vendored third-party libraries (crypto++, zlib, libpng, protobuf, RmlUi, quickhull, etc.), each built as its own CMake subdirectory rather than a system dependency.
- **`vpc_scripts/`**: the original Valve VPC build scripts, kept for reference/history — not part of the active CMake build.
- Platform support is effectively Linux-first (`LINUXALL`/`POSIX`) with Windows and macOS paths present but less exercised; expect `#ifdef WIN32`/`POSIX`/`OSXALL` branches throughout.
- Non-free/closed-source pieces are opt-in only (`USE_SCALEFORM`, optional Valve-original `vphysics_client.so`/`scaleformui_client.so`/`libphonon3d.so` binaries fetched post-build) — the default build path is fully open source.

## Rust port (in progress)

This branch's long-term goal is a gradual, module-by-module rewrite of the codebase
into Rust using `cxx`, while keeping the original Valve interfaces (`IAppSystem`,
`CreateInterface`, the versioned interface strings in `public/interfaces/interfaces.h`)
as the FFI contract until a whole dependency subtree has been ported. **Read
[`PORTING.md`](PORTING.md) before starting or reviewing any port-related work** — it
documents the `appframework`/`CAppSystemGroup` lifecycle this has to preserve, the
vtable-shim pattern used in both FFI directions, porting-order guidance, and the plan
for eventually dropping the shims once a subtree is Rust-only. Keep it updated as
modules actually get ported or the plan changes.

## Codebase knowledge graph (codebase-memory-mcp)

This repo (500k+ nodes) is indexed by the `codebase-memory-mcp` MCP server. For
structural questions — finding a symbol, tracing callers/callees, checking who
implements or registers a given interface, orienting in an unfamiliar module — prefer
its graph tools (`search_graph`, `trace_path`, `get_code_snippet`, `get_architecture`,
`query_graph`) over blind grep/Explore; fall back to `search_code`/filesystem grep for
literal text or when graph coverage looks thin (check with `check_index_coverage`
before relying on a negative result). See `PORTING.md`'s "Using codebase-memory-mcp
for this work" section for how this applies specifically to porting decisions.
