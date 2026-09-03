# PORTING.md

Goal: rewrite the Source engine in Rust as **one Rust project** — a single crate at
the repository root producing a single statically-linked binary, with no
`dlopen`-based app system and interfaces designed for Rust rather than inherited from
Valve's C++.

**Target game: Portal 2.** The original tree is the `cstrike15` branch, which is what
we're working from, but the goal is Portal 2 — so `portal`/`portal2` content is in
scope and CS:GO-specific content generally isn't (see "Game scope" below).

This document is the standing design reference. Read it before porting anything, and
update it when the plan changes. Every future porting session (Claude or otherwise)
should treat this file, not tribal knowledge, as the source of truth. Per-module
detail lives in `portdocs/<MODULE>.md` — see "Per-module porting docs".

## Repository layout

```
/                  Rust project root — Cargo.toml, src/
  src/main.rs      entry point
  src/launcher/    process bootstrap (first module ported)
  src/filesystem/  search paths, gameinfo.txt, VPK reading
  src/materials/   the GPU device and the frame boundary (wgpu)
  src/engine/      the engine; window/ (winit) so far
  legacy/          the original C++ tree, verbatim
  portdocs/        per-module porting design docs (what to build)
  rustdocs/        per-module API references (what exists)
  PORTING.md       this file
```

**`legacy/` is the original C++ source, moved wholesale out of the root.** It is the
*reference implementation* — read it, measure it, port from it — but it is **not a
build dependency and not linked into anything.** The Rust project does not use CMake,
does not compile any C++, and has no FFI to `legacy/`. Nothing in `legacy/` should be
edited.

Note that all file paths cited throughout this document and in `portdocs/` are given
relative to the original tree (`engine/sys_dll2.cpp`, `public/tier1/interface.h`, …);
prefix them with `legacy/` to open them.

`legacy/` shrinks as subsystems land in `src/` and can be deleted directory by
directory once nothing needs reading from it any more.

### Consequence: no incremental scaffolding

An earlier revision of this plan kept the C++ linked in as static-library scaffolding
so the game stayed runnable at every commit. **That is no longer the approach.** With
`legacy/` fully decoupled, the Rust binary is not a playable game until enough
subsystems exist to boot one — bootstrap, filesystem, the engine host loop, rendering,
and enough of the game layer. Two things follow:

- **Verification is against the reference, not against a running hybrid.** Compare
  behavior by reading `legacy/`, and by building the C++ tree separately if a live
  reference is wanted (see the caveat in "Status" — the C++ launcher was removed, so
  that tree no longer links a game binary as-is).
- **Ordering matters more than it would otherwise.** Nothing is exercised end-to-end
  until the boot path is complete, so prefer depth on the startup path over breadth
  across subsystems. See "Porting order".

---

## Architecture: one crate, one binary, no app system

The original engine is ~30 shared libraries that find each other at runtime by name
through `CreateInterface` and a versioned-string registry (`public/tier1/interface.h`,
`appframework/AppSystemGroup.cpp`). **The Rust port does not reproduce this.** Instead:

- **One crate, one `Cargo.toml`**, at the repository root. Each former Valve module
  becomes a **module** under `src/` (`src/engine/`, `src/filesystem/`,
  `src/materials/`, …) — not a separate crate, not a separate library.
- **One statically-linked executable.** No `.so`s of our own, no `dlopen` of our own
  code, no `CreateInterface`, no `IAppSystem` lifecycle, no interface version strings.
- **Ordinary Rust calls between modules** — real types, real generics, real trait
  objects where dynamism is genuinely wanted, and full inlining/LTO throughout.

**Why not keep the dynamic app system:** Rust has no stable ABI. Dynamically linking
Rust-to-Rust either pins every module to one exact compiler version (`dylib`) or
forces everything through `extern "C"` with C-representable types only — which throws
away the idiomatic interfaces that are the whole point of this rewrite. The plugin
architecture was load-bearing for C++ and is pure cost for Rust.

**What we give up, acknowledged:** runtime module swapping (`CAppSystemGroup::ReloadModule`)
and third-party binary game modules. Neither matters for a single-player-focused
Portal 2 build.

**One asterisk:** `libsteam_api.so` is a closed-source shared object. If Steam
integration is kept, that one `dlopen` remains regardless — it's a plain C ABI, so
it's not a problem, but "one pure static binary" isn't literally reachable.

## No FFI, no C++, no CMake

There is **no C++ in the Rust project and no build script** — no `cxx`, no
`extern "C"` bridge to `legacy/`, no vtable shims, no `build.rs`, no CMake. `cargo
build` is the entire build.

This is a deliberate simplification over an FFI-bridged port. It costs the ability to
run a hybrid binary mid-port (see "Consequence: no incremental scaffolding" above) and
buys: no ABI-compatibility surface to get wrong, no C++ toolchain in the build, no
adapter code written only to be deleted, and no risk of Valve's interface shapes
leaking into the Rust design through the FFI boundary.

**The only `dlopen` in the design is `libsteam_api.so`**, a closed-source C-ABI blob,
if and when Steam integration is added.

### Polarity: the Rust interface is the contract

The most important design rule here, and the reason the FFI approach was dropped.

**Define the interface Rust wants. Never derive it from a Valve one.** When porting a
subsystem, read `legacy/` to learn *what it does* and *why* — the domain knowledge is
real and hard-won — then design the Rust API from scratch. Do not transliterate a
Valve signature, do not preserve an interface shape "for now", and do not let a C++
type dictate a Rust type.

The practical test when porting something: if a Rust signature only makes sense once
you've read the C++ it came from, it's wrong. Rewrite it.

What *does* carry across from `legacy/`: algorithms, data layouts that are fixed by an
external format (see "Format is fixed" below), protocol state machines, physics and
math, frame-ordering constraints, and the accumulated bug fixes encoded in odd-looking
special cases. Read those carefully and preserve the behavior — just not the shape.

### What "idiomatic" means concretely

Do not carry these Valve patterns into Rust, even where mechanically translating them
would be easier:

| Valve C++ pattern | Rust replacement |
|---|---|
| `Connect(factory)` / `Init()` / `Shutdown()` / `Disconnect()` four-phase lifecycle | Construction returns a fully-initialized value (`fn new(deps) -> Result<Self>`); `Drop` handles teardown |
| `QueryInterface(const char *name)` string lookup | Real types. If dynamism is genuinely needed, an enum or `dyn Trait`, resolved at compile time |
| `CreateInterfaceFn` + version strings (`"VMaterialSystem080"`) | Cargo dependency versions |
| `bool` returns with out-params for errors | `Result<T, E>` with a real error enum (`thiserror`) |
| Raw `T*` with documented-but-unenforced ownership | `&T`, `&mut T`, `Box<T>`, `Arc<T>`, lifetimes |
| `g_pGlobalSingleton` mutable globals | Explicit dependency passing; `OnceLock`/`Arc` only where genuinely process-global |
| `CUtlVector`, `CUtlString`, `KeyValues` | `Vec`, `String`, `HashMap`, `serde` |
| Manual `new`/`delete`, no move semantics | Ownership, RAII, moves |
| `virtual` everything, deep inheritance | Traits, composition, generics; `dyn` only where runtime polymorphism is real |
| `#ifdef` platform branching | `#[cfg]`, or better, no branch at all (see "Supported platforms") |

Where a Valve interface encodes real domain knowledge (the shape of a BSP node, the
netchannel state machine, the phases of a frame), **keep the knowledge and discard the
encoding.**

---

## Supported platforms

POSIX only:

- **Linux** — primary target. Android is a plausible future POSIX target but not in
  scope; don't design around it, don't gratuitously preclude it.
- **Apple** — macOS first. iOS/iPadOS/tvOS/visionOS are plausible later targets, also
  not in scope now.

**Windows and consoles (X360, PS3) are permanently out of scope** — not "not yet."
Don't write `#[cfg(windows)]` scaffolding on the assumption someone fills it in later.
Where the original had `WIN32`/`_X360`/`_PS3`/`SN_TARGET_PS3` branches, the Rust port
simply has no branch. When reading Source files, skim for `#elif defined(POSIX)` /
`LINUX` / `OSX` and unconditional code, and disregard the rest.

This also simplifies scoping: console/Windows-only code in a module you're sizing up
is dead weight from the port's perspective, not complexity to plan around.

## Rendering, windowing, and UI

Three replacements, all decided:

- **`wgpu` replaces `materialsystem/shaderapidx9` + `togl`.** The current renderer is
  written against a D3D9-shaped API and runs on POSIX through `togl`
  (`togl/glmgr.cpp`, gated on `DX_TO_GL_ABSTRACTION`), a hand-written D3D9→OpenGL
  translation layer. That whole tower goes. `wgpu` targets Metal on macOS and Vulkan
  (falling back to GL) on Linux, so there's no translation shim to maintain.
  **The shaders are rewritten in WGSL**, translated from the `.fxc` HLSL sources in
  `materialsystem/stdshaders/`. The shipped `.vcs` files are D3D9 bytecode and are
  unusable — reusing them would mean writing a new `dx9asmtogl`, which is the tower being
  deleted. Valve's static/dynamic **combo system goes with it**: one `.fxc` there declares
  ~15.3M static variants, and `utils/shadercompile` was a distributed farm built to
  compile them. In the port, most combo axes are pinned at compile time, most of the rest
  become uniform branches, and only vertex-layout/pipeline-state changes stay as real
  pipeline variants. See `portdocs/MATERIALSYSTEM.md` §7.
- **`winit` replaces `ILauncherMgr`/SDL2/Cocoa windowing** — i.e.
  `public/appframework/ilaunchermgr.h`, `appframework/sdlmgr.cpp` (`CSDLMgr`),
  `appframework/cocoamgr.mm`, and the vendored `thirdparty/SDL2`.
- **`egui` replaces vgui2, RocketUI, and ScaleformUI** — every UI backend at once.
  Picked because it's Rust-native with first-class `egui-wgpu`/`egui-winit` backends,
  making it the natural third leg of this stack.

### The control-flow inversion

Source's frame loop is **pull-based**: `CEngineAPI::MainLoop()`
(`engine/sys_dll2.cpp:1132`) is a bare `while(true)` that calls `PumpMessages()` then
`eng->Frame()`. `winit` is **push-based**: you hand an `EventLoop` an
`ApplicationHandler` and it calls *you* (`resumed`, `window_event`, `about_to_wait`),
with `ControlFlow` governing pacing.

The specific collision: **`CEngine::Frame()` (`engine/sys_engine.cpp:418-614`) owns
its own frame pacing and sleeps inside itself** — it calls `FilterTime()`, and if not
enough time has passed it `ThreadNanoSleep()`s and returns without doing work. Two
systems both trying to own pacing, one sleeping inside a callback the other scheduled,
is the failure mode to design against.

Target shape (to be validated, not settled):

- `window_event` translates winit events into the engine's input events directly.
- `about_to_wait` drives one engine tick.
- `FilterTime`'s *policy* survives (respect `fps_max` and friends) but becomes
  `ControlFlow::WaitUntil(deadline)` instead of a sleep.
- Quit/restart signalling maps to `ControlFlow::Exit` while preserving the
  restart-vs-exit distinction the launcher's restart loop depends on.

Also note: the UI event precedence chain in `CGame::DispatchInputEvent`
(`engine/sys_mainwind.cpp:399`) currently runs VGui → RocketUI → GameUI, with VGui
getting first refusal. Since `egui` replaces all three, that chain collapses into
`egui`'s "did the UI consume this event" answer — a real design question, not a
translation. Details in `portdocs/ENGINE.md`.

## Tier libraries (`tier0`–`tier3`): not used by Rust code

Rust code does not depend on `tier0`/`tier1`/`tier2`/`tier3` — not even the
"obviously portable" utility bits. Use `std` and ordinary crates:

- **`tier0`** (`libtier0_client`) is ambient infrastructure: a custom allocator
  (`tier0/mem.cpp`, `tier0/dlmalloc/`), memory debugging, threading
  (`tier0/threadtools.cpp`), assert/logging (`tier0/dbg.cpp`, `tier0/logging.cpp`),
  CPU detection, and the `platform.h` macro layer. All of that is `std` (allocation,
  `std::thread`/`std::sync`, `panic!`/`Result`) plus a logging crate (`tracing`/`log`).
- **`tier1`** is Valve's pre-STL container layer — `CUtlVector`, `CUtlString`,
  `KeyValues`, `bitbuf`, checksums, `ConVar`/`CCommand`. Replaced by `Vec`/`String`/
  `HashMap`/`serde` and, for bit-level work, `deku`/`bitvec` (see below).
- **`tier2`/`tier3`** are helpers built on `tier1`'s shapes; same reasoning.

Since nothing links `legacy/`, this rule is now automatic rather than a discipline to
maintain — there is no tier0/tier1 in the process at all. It's recorded here mainly so
that "port `tier1`" never gets mistaken for a task: those modules are replaced by
`std` and crates, not translated.

## Prefer modern Rust crates over porting old in-engine code

Where the engine hand-rolled something the Rust ecosystem does better, **use the crate
— don't transliterate the C++.** Same reasoning as the tier rule, one level up.
Applies to compression, hashing, thread pools, HTTP, string formatting, and above all
serialization.

**But separate the *mechanism* from the *format*** — conflating them is the expensive
mistake:

- **Mechanism** (how the code is written): always modernize. A derive macro or parser
  combinator beats hand-written `bf_read::ReadUBitLong()` calls, with zero
  compatibility consequence.
- **Format** (what bytes come out): only free to change where **we own both ends**.

### Format is ours to change

Internal-only data; savegames; anything new; and the client↔server wire format *if* we
accept a flag day (this tree builds both client and dedicated server, so we can change
both at once — the cost is losing the ported-client-vs-unported-server test
configuration, and breaking stock third-party servers).

### Format is fixed regardless of crate choice

- **Valve asset formats — `.bsp`, `.mdl`, `.vtf`, `.vmt`, `.vpk`.** Content comes from
  Valve's depots; we don't own the producer and never will. Parse them with
  `binrw`/`nom`/`deku` instead of hand-rolled readers, but the byte layout is
  immovable.
- **`SendTable`/`RecvTable` entity delta encoding**, while `game/client`/`game/server`
  are still C++ — those define the props via `SendPropInt`/`RecvPropInt` macros in
  `game/shared/`, so the engine must speak exactly the format they describe. Becomes
  negotiable once they're ported.
- **`.dem` demo files**, if existing recordings should stay playable — decide
  explicitly rather than breaking it by accident.
- Steam-facing protocols (auth, matchmaking, GC messages, datagram relay).

### Netchannel specifically

It's already half schema-driven: `common/netmessages.proto` (674 lines) defines the
messages, and `engine/net_chan.cpp` is a roughly even mix of protobuf and `bitbuf`
calls.

- **Messages** → feed the existing `.proto` files to `prost`. No hand-porting, format
  preserved free. (Caveat: vendored runtime is protobuf 3.5.1; watch `proto2`
  semantics, which `prost` handles differently.)
- **Framing** (fragments, subchannels, reliability, bit-packed headers) → the
  hand-rolled part. Describe the layout declaratively with `deku`/`bitvec` rather than
  transliterating `bf_read`/`bf_write` sequences.

**On `serde` specifically:** right tool when we control both ends and want a compact
format (`postcard`, `bincode`). Wrong tool for reading fixed legacy binary layouts —
use `deku` (bit-level, derive-based), `binrw` (byte-level), or `nom`. Choosing `serde`
for `.bsp` parsing would be a category error.

---

## Game scope: Portal 2 from a cstrike15 base

The tree is the `cstrike15` branch but the target is Portal 2, so scope is neither
"everything in `game/`" nor "what the CS:GO build compiles."

Measured sizes (whole subtree; not all of it is compiled today):

| Path | Lines | Disposition |
|---|---|---|
| `game/shared/portal` | 41,642 | **In scope** — core Portal gameplay (portals, physics, grab controller) |
| `game/client/portal` | 15,069 | In scope |
| `game/server/portal` | 12,841 | In scope |
| `game/server/portal2` | 4,655 | In scope |
| `game/shared/portal2` | 3,785 | In scope |
| `game/client/portal2` | 111,384 | **~90k is `gameui/portal2/` → replaced by `egui`.** Real entity code (`c_prop_weightedcube`, `c_prop_floor_button`, `radialmenu`) is small |
| `game/*/cstrike15` | — | Out of scope except where it's the only implementation of something generic |

Portal-2-specific engine features already identified in the tree: `engine/paint.cpp`
(1,656 lines — paint/gel maps) is **essential**, not vestigial as it would be for a
CS:GO build. `vscript/` (Squirrel VM, ~55k) matters because Portal 2 puzzle logic is
scripted — worth evaluating a Rust Squirrel binding or an alternative VM rather than
porting the vendored one.

Beware: the `cstrike15` base means some shared systems are CS:GO-shaped (e.g.
`DEFAULT_HL2_GAMEDIR` is `"csgo"` in the launcher, game rules default to CS:GO). Those
need retargeting as part of making Portal 2 boot, and each is worth calling out in the
relevant `portdocs/` entry when hit.

## Build

`cargo build`. That's the whole thing — one crate, no build script, no C++ toolchain,
no CMake. Release builds use full LTO and a single codegen unit (see `Cargo.toml`).

The CMake tree under `legacy/` is not part of this build and is not maintained. Don't
invest in it, and don't wire it back in.

## Porting order

Use the dependency signal rather than guessing — `IAppSystem::GetDependencies()`
overrides and `Create()`'s `AppSystemInfo_t` lists (e.g. `appframework/matsysapp.cpp`)
still document the real graph even though we're discarding the mechanism.

Rules of thumb:

- **Follow the boot path, depth-first.** Since nothing runs until the whole startup
  chain exists, the ordering that produces a running artifact soonest is: bootstrap →
  filesystem (enough to read `gameinfo.txt` and mount VPKs) → windowing (`winit`) →
  rendering (`wgpu`, enough to clear a frame) → engine host loop → map loading →
  the game layer. Breadth across unrelated subsystems produces nothing runnable.
- **`tier0`/`tier1` are not tasks.** They're replaced by `std` and crates as a side
  effect of porting everything else, not ported in their own right.
- **Do the `winit`/`wgpu` groundwork deliberately**, before the engine's frame loop —
  it constrains how that loop can be structured. See `portdocs/ENGINE.md`. *Stage 1 of
  that groundwork (a cleared window, and the frame boundary the host loop has to fit) is
  done; stages 2-3, textures and materials, are next.*
- **Don't port anything slated for replacement.** Renderer front-end (→ `wgpu`), UI
  (→ `egui`), tier libs (→ `std`), zip/compression/etc. (→ crates). A large fraction
  of the raw line count evaporates this way — roughly half of the compiled
  `game/client`, for instance.
- Prefer modules already mostly POSIX over ones steeped in console/Windows branching —
  not because those platforms need support, but because there's less to mentally
  discard while reading.

## Per-module porting docs (`portdocs/`)

This file is the cross-cutting strategy. Per-module detail goes in
`portdocs/<MODULE>.md`, named after the module directory in `SCREAMING_SNAKE_CASE`
(`engine/` → `portdocs/ENGINE.md`). Write one for any module with real internal
complexity — multiple subsystems, meaningful fan-in/fan-out, or a structural change —
covering:

- The module's real dependency graph in both directions.
- Internal architecture a porter needs to hold in their head.
- What to port faithfully vs. replace vs. delete.
- Structural/behavioral changes specific to it, staged as concrete steps.
- Open questions and known-risky spots.

Read it before touching that module; update it as the port proceeds and reality
diverges. Small leaf modules don't need one — a Status entry here is enough.

Once a module is **implemented**, it also gets an API reference in `rustdocs/<MODULE>.md`
describing what now exists in `src/` — public types, usage, invariants, gotchas, and what
stayed deferred. `portdocs/` says how to build it; `rustdocs/` says how to use it. See
`CLAUDE.md`'s "Per-module API docs" for what belongs in one and `rustdocs/FILESYSTEM.md`
for the worked example.

## Using codebase-memory-mcp

This repo is indexed in the `codebase-memory-mcp` knowledge graph (project
`Users-damienbrown-Documents-SourceEngineWork-Kisak-Strike`). Prefer its graph tools
over blind grep when scouting — the tree is too large to explore by hand:

- `search_graph` — find declarations, registrations, implementors.
- `trace_path` (`direction: inbound`) — real fan-in before committing to an order.
- `get_architecture` (`aspects: ["structure","clusters"]`, scoped by `path`) — orient
  in an unfamiliar module; `clusters` finds de-facto subsystems that cut across the
  folder layout.
- `check_index_coverage` — **before trusting any negative result.** Coverage on large
  engine files is frequently partial; `engine/sys_engine.cpp:264-686` (all of
  `CEngine::Frame` and `FilterTime`) is unparsed, for instance. Read flagged ranges
  from source and treat graph results there as under-reporting.

## Status

**Repository restructured**: the entire original C++ tree was moved to `legacy/`, and
the repo root is now the Rust crate (`Cargo.toml`, `src/`). The `ivp` submodule moved
with it and `.gitmodules` was updated to `legacy/ivp`.

- **`src/launcher/` — process bootstrap, ported.** Command-line handling
  (`cmdline.rs`), single-instance locking (`single_instance.rs`), early-error
  reporting (`dialog.rs`), and the startup sequence (`mod.rs`). Replaces both
  `launcher_main` and `launcher` from the original tree, which existed only to
  `dlopen` their way up a chain of shared libraries — meaningless in a single binary.
  Notable design choices, all deliberate departures from the original: `CommandLine`
  is an owned struct passed explicitly rather than a `CommandLine()` singleton behind
  a pure-virtual interface; the `GrabSourceMutex`/`ReleaseSourceMutex` pair became an
  RAII guard released on `Drop`; the native error dialogs are stderr for now
  (see `dialog.rs`). The startup sequence runs to completion and then stops — there's
  no engine to hand off to yet.
- **`src/filesystem/` — ported.** `Vfs` over an ordered list of mounts: `gameinfo.txt`
  parsing and search-path construction, a KeyValues reader (with the `[$COND]`
  evaluator), `RelPath` normalization, `DirMount` with a cached case-folded index, and a
  `VpkMount` covering v1/v2/headerless directories, multi-archive files and embedded
  chunks. 70 tests; the launcher mounts the game at startup and prints the search paths.
  Deferred deliberately: async, `.bsp` pak lump mounts, `sv_pure` tracking,
  `QueuedLoader`. Two subsystems confirmed dead and excluded (`IAsyncFileSystem`, ~3,700
  lines with no callers; `filesystem_steam.cpp`, not in the build).
  **Behavior change, unverified against a real install:** VPKs are ordinary ordered
  mounts rather than a global list that always wins over loose files, so a loose file
  overrides the VPK beside it. `VPKS_AFTER_LOOSE_FILES` reverts it.
  **Trap recorded in `portdocs/FILESYSTEM.md`:** KeyValues' `$WIN32` resolves to
  `IsPC()`, so `[$WIN32]` is *true* on POSIX; reading it as "is Windows" silently drops
  search paths.
- **`src/materials/` — stage 1 of 8 ported; `src/engine/window/` with it.** The
  `wgpu`/`winit` groundwork this file calls for below: `Renderer` owns the `wgpu`
  instance/adapter/device/queue/surface and exposes one frame boundary
  (`begin_frame` → `clear` → `present`), and `src/engine/window/` owns the `winit` event
  loop, the window, and `VideoConfig` (the port of
  `OverrideMaterialSystemConfigFromCommandLine`). The launcher now boots into a window
  instead of stopping after the filesystem mount. 12 new tests, 91 total; verified on
  macOS/Metal. **APIs: `rustdocs/MATERIALS.md` and `rustdocs/ENGINE.md`** — read those
  before calling in. Deliberate divergences, each with its reversing switch, are recorded
  there: windowed 1280x720 with vsync on by default (Valve's real defaults come from
  `videoconfig.cfg`, which isn't ported), SDR only, and `wgpu::Limits::default()` as the
  single capability tier. Not yet: input, the engine tick, MSAA, exclusive fullscreen
  modes, and any content loading at all — the renderer has no `Vfs` connection yet.
- **`materialsystem` (stages 2-8) — documented, not started.** `portdocs/MATERIALSYSTEM.md`: inventory,
  the shadow/dynamic two-phase model and how it maps onto `wgpu` pipelines, the shader
  (`.vcs`/`.fxc`) problem, Portal 2 paint maps, and a staged plan. **This module *is* the
  "rendering" step of the boot path below** — there is no separate renderer. Settled: the
  `IShaderDeviceMgr`/`IShaderDevice`/`IShaderAPI`/`IShaderShadow` tower is deleted and
  `wgpu` is used directly from inside the material system. That deletes `shaderapidx9`,
  `shaderapiempty`, `glmgr`, `ps3gcm` and `togl` outright — ~148k of the module's ~262k
  lines — leaving a real port target around 30–35k. Also settled: **the shaders are
  rewritten in WGSL** from the `.fxc` HLSL, and the static/dynamic combo system is deleted
  (see below).
- **`engine` — documented, not started.** `portdocs/ENGINE.md`: 23 subsystems enumerated
  with files and sizes, plus the frame-loop/`winit` analysis. Conclusion stands: don't
  port it as one unit. **Each subsystem becomes its own Rust module under `src/engine/`**
  (`audio/`, `net/`, `host/`, `world/`, `console/`, …) — 13 modules from 23 subsystems,
  with ~45,700 lines deleted outright (HLTV, replay, VGui panels, dev tooling, tool
  framework, console platform code). Its `paint.cpp` is essential for Portal 2. Corrected
  while rewriting: **sound is ~97,200 lines, not the ~48,000 previously recorded** — the
  largest subsystem in the module by a wide margin.
- **Everything else is unported** and lives in `legacy/`.

**Caveat on `legacy/` as a runnable reference:** the original C++ `launcher` and
`launcher_main` were deleted before the restructure, so `legacy/` no longer links a
game binary as-is. Everything else is intact and readable. If a *running* reference is
wanted, recover those two directories from git history
(`git log --diff-filter=D --name-only -- launcher launcher_main` to find the commit)
and restore them under `legacy/`.

**Docs that predate the current architecture:** `portdocs/LAUNCHER.md` carries a banner
noting what changed for it. Its factual content — module behavior analysis — remains
accurate and is the reason to keep it; its *plan* assumed the earlier FFI-bridged model.
`portdocs/ENGINE.md` used to carry the same banner and has since been rewritten against
the current architecture.
