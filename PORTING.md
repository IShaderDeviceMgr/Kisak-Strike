# PORTING.md

Goal: gradually rewrite Kisak-Strike from C++ to Rust, module by module, without a
big-bang rewrite and without breaking the ability to build a working game at every
commit. This document is the standing design reference for that effort — read it
before porting any module, and update it when the plan changes. Every future
porting-related session (Claude or otherwise) should treat this file, not tribal
knowledge, as the source of truth.

FFI approach: [`cxx`](https://cxx.rs). Interfaces stay in their original Valve/Source
form (pure-virtual C++ abstract classes, `CreateInterface`-style factories, the
`IAppSystem` lifecycle) for as long as both Rust and C++ modules coexist. Only once a
whole dependency subtree has been ported do we replace that subtree's boundary with
something more idiomatic (see "Endgame" below). Don't jump to idiomatic Rust traits
across a still-mixed boundary — it defeats the incremental strategy.

## Supported platforms

The Rust port only needs to support POSIX platforms:

- **Linux** — the primary target today. Android is a plausible future POSIX target
  (it's Linux-derived) but is not in scope right now; don't design around it yet,
  just don't design anything that would gratuitously preclude it later.
- **Apple platforms** — macOS first. iOS, iPadOS, tvOS, and visionOS are plausible
  future targets (same POSIX/Darwin lineage as macOS) but, like Android, are not in
  scope right now.

**Windows and consoles (X360, PS3, etc.) are explicitly out of scope and will not be
supported**, full stop — not "not yet ported." Don't write conditional scaffolding for
them (`#[cfg(windows)]`, `cfg_if` branches, Windows-shaped CMake options, etc.) on the
assumption they'll be filled in later; there is no later for those platforms. Where
the original C++ had `WIN32`/`_X360`/`_PS3`/`SN_TARGET_PS3` branches, the Rust port
simply doesn't have that branch — see `launcher_main/src/main.rs` and
`launcher_main/CMakeLists.txt` for the pattern (POSIX-only code, no Windows fallback
path, and the CMake hard-errors via `message(FATAL_ERROR ...)` if it somehow gets
configured for anything other than `LINUXALL`/`OSXALL`).

This also simplifies the porting-order guidance below: fan-out into
`_X360`/`_PS3`/Windows-only branches in a module you're scouting is dead code from the
Rust port's perspective, not extra complexity to plan around — it just doesn't get
ported.

## Why the app system is the thing to preserve

This codebase already has a real plugin architecture — it isn't "one giant binary,"
it's ~30 shared libraries (`engine_client.so`, `materialsystem_client.so`,
`vphysics_client.so`, ...) that find each other at runtime by name. That's exactly the
seam a gradual rewrite needs: a module boundary that already tolerates independent
compilation, independent versioning, and "the other side doesn't know or care what
language implemented me." We don't need to invent a plugin system for the Rust port —
we need to keep participating in the one that's already there.

Three layers make this work, from outside in:

1. **`CAppSystemGroup` (public/appframework/IAppSystemGroup.h, appframework/AppSystemGroup.cpp)**
   — the orchestrator. Owns the staged lifecycle
   (`CREATION → DEPENDENCIES → CONNECTION → PREINIT → INIT → POSTINIT → RUNNING →
   PRESHUTDOWN → SHUTDOWN → POSTSHUTDOWN → DISCONNECTION → DESTRUCTION`), `dlopen`s
   modules by base name (`LoadModule("materialsystem.dll")`, extension resolved per
   platform — see `_DLL_EXT` in `cmake/detect_platform.cmake`), asks each for a named
   interface via its factory, and topologically sorts/init's/shuts-down systems based
   on `IAppSystem::GetDependencies()`. **This class must never need to know whether a
   module is C++ or Rust.** As long as a module still exports a working
   `CreateInterface` that hands back a vtable-compatible pointer, `CAppSystemGroup` is
   untouched, forever, even after every module underneath it is Rust.

2. **`CreateInterface` / `InterfaceReg` (public/tier1/interface.h, tier1/interface.cpp)**
   — the per-module factory. Each module keeps a linked list of
   `(version-string, InstantiateInterfaceFn)` pairs registered by static constructors
   (`EXPOSE_INTERFACE`/`EXPOSE_SINGLE_INTERFACE` macros), and exports one
   `CreateInterface(const char *pName, int *pReturnCode)` symbol that walks the list.
   Interface version strings and their global pointer variables (`g_pMaterialSystem`,
   `g_pFullFileSystem`, ...) are centralized in `public/interfaces/interfaces.h`. A
   ported module keeps registering under the *same* version string
   (`MATERIAL_SYSTEM_INTERFACE_VERSION` etc.) so nothing on the C++ side has to change
   to keep finding it.

3. **`IAppSystem` (public/appframework/iappsystem.h)** — the per-instance contract:
   `Connect(factory) → Init() → [ticks] → Shutdown() → Disconnect()`, plus
   `QueryInterface` (secondary interfaces on the same object), `GetDependencies`
   (drives the auto-load/topo-sort), `GetTier`, `Reconnect`, `IsSingleton`. A Rust
   module's root object needs to behave like one of these from the outside, even
   though nothing forces it to be *implemented* as a C++ vtable internally.

The key realization: only layers 2 and 3 need a Rust-shaped answer. Layer 1 (the
orchestrator) is dependency-free of language once layers 2/3 hold up their end of the
contract.

## The actual FFI problem, and why it's not solved by `cxx` alone

`cxx` is excellent at "Rust calls into C++" (opaque C++ types, methods, `UniquePtr`,
etc.) and "C++ calls plain Rust functions/structs." What it deliberately does **not**
support is the thing we need for layer 2/3: **a Rust type standing in for a C++
abstract class and being handed out through a vtable pointer that pre-existing,
unmodified C++ code will call through.** `cxx` has no notion of overriding a C++
virtual method from Rust — it can't synthesize an Itanium-ABI vtable.

So the plan uses a small, deliberate, hand-written seam at exactly that point and
nowhere else:

### Direction A — Rust module *provides* an interface (C++ calls into Rust)

Hand-write a thin C++ "vtable shim" per interface being served out of Rust:

```cpp
// materialsystem/rust_shim.cpp — the ONLY handwritten C++ in a ported module
class CRustMaterialSystem : public IMaterialSystem
{
public:
    bool Connect( CreateInterfaceFn factory ) override { return rust_materialsystem_connect( m_pState, factory ); }
    void Disconnect() override { rust_materialsystem_disconnect( m_pState ); }
    void *QueryInterface( const char *pName ) override { return rust_materialsystem_query_interface( m_pState, pName ); }
    InitReturnVal_t Init() override { return rust_materialsystem_init( m_pState ); }
    void Shutdown() override { rust_materialsystem_shutdown( m_pState ); }
    // ... every other IMaterialSystem virtual, one trampoline line each ...
private:
    RustMaterialSystemState *m_pState = rust_materialsystem_new();
};

EXPOSE_SINGLE_INTERFACE( CRustMaterialSystem, IMaterialSystem, MATERIAL_SYSTEM_INTERFACE_VERSION )
```

The `rust_materialsystem_*` functions are `extern "C"` (or a `#[cxx::bridge] extern
"Rust"` block) implemented in Rust. `m_pState` is an opaque pointer into a Rust struct
implementing an `AppSystem`-shaped trait (see below) plus whatever the real interface
needs. **This shim is boilerplate, not logic** — it should be mechanically generated
per interface where practical (a build script parsing the interface header, or just
diligent copy-paste for the handful of interfaces we actually port), never grown
organically. All real behavior lives in Rust.

Only the interfaces a module actually *exposes* need a shim. Internal types, helper
classes, anything not reachable through `CreateInterface` — pure Rust, no shim needed.

### Direction B — Rust module *consumes* an interface (Rust calls into C++)

This is `cxx`'s home turf, with one caveat: most Source interfaces take/return Valve
container types (`CUtlVector`, `KeyValues*`, `const char*` with manual lifetime rules)
that aren't `cxx`-representable directly. Don't fight this — write a thin C++ adapter
free-function per call site that translates to `cxx`-friendly shapes (raw slices,
`&CxxString`, `usize`, plain structs), the mirror image of Direction A's shim. Keep
adapters colocated with the consuming Rust module (e.g.
`<module>/rust_bridge.h`/`.cpp`), not scattered into the shared `public/` headers —
`public/` interface headers stay pristine, unmodified Valve/Source contracts.

Global interface pointers (`g_pFullFileSystem`, `materials`, ...) declared in
`interfaces.h`: never bind Rust directly to the raw `extern` global. Expose a small
accessor (`const IFileSystem *rust_get_filesystem()`) from the adapter layer instead —
keeps the unsafe extern-mutable-global reasoning in one obvious place per module.

### The `AppSystem` trait

To keep every ported module's shim mechanically identical (and to keep the door open
for dropping the shim later — see Endgame), define one Rust trait mirroring
`IAppSystem` 1:1:

```rust
pub trait AppSystem {
    fn connect(&mut self, factory: CreateInterfaceFn) -> bool;
    fn disconnect(&mut self);
    fn query_interface(&mut self, name: &str) -> *mut c_void;
    fn init(&mut self) -> InitReturnVal;
    fn shutdown(&mut self);
    fn dependencies(&self) -> &[AppSystemInfo] { &[] }
    fn tier(&self) -> AppSystemTier { AppSystemTier::Other }
    fn reconnect(&mut self, factory: CreateInterfaceFn, interface_name: &str) {}
    fn is_singleton(&self) -> bool { true }
}
```

A module's root struct implements this once; the C++ shim's overrides are 1:1
trampolines into it. Every additional interface-specific virtual (the actual
`IMaterialSystem` surface beyond `IAppSystem`) gets its own trampoline into inherent
methods on the same struct — no separate trait needed for those, since they're not
part of the shared lifecycle contract `CAppSystemGroup` depends on.

## Choosing porting order

Port **one whole `CreateInterface`-exposing shared-lib module at a time** — never
half a module. Partial-module ports don't have a clean seam (everything inside one
`.so` shares C++ statics/singletons freely; there's no `CreateInterface` boundary to
hang a shim off of internally).

Use the existing dependency signal instead of guessing:
- `public/tier1/interface.h`'s `DECLARE_TIER1/2/3_INTERFACE` groupings in
  `interfaces.h` are Valve's own difficulty/layering ranking.
- `CAppSystemGroup::ComputeDependencies`/`SortDependentLibraries` (in
  `appframework/AppSystemGroup.cpp`) computes this at runtime from
  `IAppSystem::GetDependencies()` — reading a module's `GetDependencies()` override
  (or its `Create()`'s `AppSystemInfo_t appSystems[]` list, e.g.
  `appframework/matsysapp.cpp`) is the fastest way to find its real fan-in/fan-out.

Rules of thumb:
- **Avoid `tier0`/`tier1` first.** They're static libraries linked into nearly every
  other target (`TIER1_STATIC_LIB`, no `CreateInterface` boundary at all in most of
  tier1) — porting them means touching everything at once, the opposite of gradual.
  They're a good *late* target once most dynamic modules around them are already Rust.
- **Start with a leaf dynamic module**: something that exposes exactly one or two
  interfaces, has few entries in its own `GetDependencies()`, and has few *other*
  modules depending on it (check via `search_graph`/`trace_path` against the module's
  interface version macro — see below). Candidates worth scouting first:
  `soundemittersystem`, `localize`, `resourcefile`, `scenefilecache` — small, mostly
  self-contained, not on the hot path of every frame.
  Don't port `materialsystem`, `engine`, or anything game/client/server until the
  shim pattern has been proven on something low-stakes.
- Prefer modules that are already mostly POSIX to begin with over anything with heavy
  `_X360`/`_PS3`/Windows-only branches — not because those platforms need supporting
  (they don't, see "Supported platforms" above), but because a module steeped in
  console/Windows special-casing is more work to even *read* — more branches to mentally
  discard while figuring out what the POSIX behavior actually is.

## Build integration

A ported module keeps its existing CMake target identity: same output name
(`OUTLIBNAME`/`OUTDLLNAME`), same `_client.so`/`.dylib`/`.dll` extension convention
from `cmake/detect_platform.cmake`, same base name string other modules pass to
`LoadModule("thatmodule.dll")` in their `Create()`. Nothing upstream should be able to
tell the module changed language.

Concretely: the module's `CMakeLists.txt` keeps `include(source_dll_base.cmake)` etc.,
adds the (few) hand-written shim `.cpp` files via `target_sources` as today, and links
a static library produced from the module's Rust crate (via `corrosion` — the
`corrosion-rs` CMake integration — or a `add_custom_command` wrapping `cargo build
--release` and `target_link_libraries`-ing the resulting `.a`/`.lib`). Pick whichever
adds less CMake ceremony once the first module is actually being ported;
`kisak-strike-build-options.cmake` is the natural place for a global
`USE_CARGO`/Rust-toolchain-path option if one becomes necessary.

## Endgame — once a subtree is fully Rust

`CreateInterface`/`InterfaceReg`/versioned-string lookup and the vtable shims are a
*transitional* tax paid only at a still-mixed-language boundary. Once every module in
a dependency subtree is Rust (e.g. some module and everything it calls into), the
shims between *those* modules can be deleted and replaced with a normal Rust
dependency (direct crate dependency, `dyn AppSystem` trait objects, real Cargo
workspace boundaries instead of `dlopen`) — `cxx` drops out entirely for that subtree.
Keep a shim only at the outer edge, wherever the subtree still hands an interface to
C++ code that hasn't been ported yet. `CAppSystemGroup` itself is the last thing to
go, if it ever does — it's cheap to keep even after everything under it is Rust, since
it costs nothing at the fully-ported end state beyond one `dlopen` per module.

## Using codebase-memory-mcp for this work

This repo is indexed in the `codebase-memory-mcp` knowledge graph
(project `Users-damienbrown-Documents-SourceEngineWork-Kisak-Strike`). Prefer its
graph tools over plain grep/Explore when scouting a module to port or checking a
shim's completeness — this codebase is too large (500k+ indexed nodes) to explore
reliably by hand:

- `search_graph` (BM25/`name_pattern`) to find an interface's declaration, every
  `EXPOSE_INTERFACE`/`EXPOSE_SINGLE_INTERFACE` registration, or every class
  implementing a given abstract interface.
- `trace_path` (mode `calls`, `direction: inbound`) on an interface's key virtuals to
  find real fan-in before committing to a porting order — this is more reliable than
  reading `GetDependencies()` alone, since plenty of code reaches an interface via
  `factory(VERSION_STRING, ...)` directly rather than through `AddSystem`.
  See [[claude-md]] for the general index/coverage-checking workflow.
- `get_architecture` (`aspects: ["structure","entry_points"]`, scoped via `path`) to
  get oriented in an unfamiliar module directory before touching it.
- `check_index_coverage` before trusting a "nothing found" result on a file you're
  about to port — large generated or third-party files sometimes aren't fully parsed.

## Status

- **`launcher_main` — fully ported, old C++ deleted.** Rewritten in Rust:
  `launcher_main/Cargo.toml` + `launcher_main/src/main.rs`, wired into
  `launcher_main/CMakeLists.txt` via a `cargo build` custom command. Per "Supported
  platforms" above, there's no Windows/X360/PS3 fallback to keep around — the old
  `main.cpp` (and its `_X360`/`SN_TARGET_PS3`/`WIN32` branches) has been deleted
  outright, and the CMakeLists hard-errors via `message(FATAL_ERROR ...)` if
  configured for anything other than `LINUXALL`/`OSXALL`. It turned out to need
  neither FFI direction from this document —
  it's pure process bootstrap (`dlopen("launcher_client.so", RTLD_NOW)`,
  `dlsym(..., "LauncherMain")`, call the resulting `extern "C" fn(argc, argv) -> c_int`
  function pointer, exit with its return value), so there's no `IAppSystem` interface
  to implement or consume and thus no vtable shim to write. Confirms it as the
  natural first target: zero fan-in (nothing links against `launcher_main`) and zero
  interface surface. One deliberate behavior change from the original: on a failed
  `dlopen`/`dlsym`, the C++ version hangs in `while(1);` before an unreachable
  `return 0;` (looks like a leftover debugging aid); the Rust version prints the
  `dlerror()` message and exits `1` instead.
- Everything else is still C++. Update this list as modules move, and keep the
  porting-order rules above current as real fan-in/fan-out data comes back from
  `trace_path`.
