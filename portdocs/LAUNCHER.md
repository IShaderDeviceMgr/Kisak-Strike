# Porting `launcher`

> ## ⚠️ SUPERSEDED IN PART — read this first
>
> This doc was written under the previous architecture (incremental port, `cxx` shims,
> preserved `dlopen` app system, Valve interfaces kept as the FFI contract).
> `../PORTING.md` has since moved to a **single-binary, single-workspace, Rust-first**
> model. What changed for this module:
>
> - **There is no `launcher_client.so` any more.** With one statically-linked binary
>   there's nothing to `dlopen`, so `launcher`'s core job — load modules by name, build
>   three nested `CAppSystemGroup` layers, hand off via `IEngineAPI` — largely
>   evaporates. It should collapse into ordinary `main()`-time initialization.
> - **The shim's polarity is now backwards.** `CRustSourceAppSystemGroup` conforms Rust
>   to `CSteamAppSystemGroup`; the new rule is the Rust interface is the contract and
>   C++ adapts to it. See PORTING.md's "Polarity" section.
> - **Still accurate and worth keeping:** the POSIX-only scoping, the analysis of what
>   to port vs. drop (Hammer, reslists, `CLogAllFiles`), the single-instance-lock
>   behavior, the `filesystem_init.cpp` shared-source finding, and the open questions.
>   The Rust in `launcher/src/{cmdline,mutex}.rs` is model-independent and survives.
>
> Treat the sections below as reference for *what this module does*, not as the current
> implementation plan.

Original status: **implemented, not build-verified.** The Rust crate compiles clean;
the C++ shim (`launcher/cpp/rust_shim.cpp`) was never compiled against the real build.
Read [`../PORTING.md`](../PORTING.md)
first; this doc assumes everything there (the shim pattern, POSIX-only scope,
`tier0`–`tier3` decision, `wgpu`/`winit`/`egui` decisions) and only adds what's
specific to this module. `launcher_main` (already ported, see PORTING.md's Status
section) is a prerequisite and stays as-is — this doc is about `launcher_client.so`,
the library it `dlopen`s and calls `LauncherMain` in.

## What `launcher` actually is

Despite the name, `launcher` isn't "the executable" (that's `launcher_main`, already
ported) — it's the module that **boots the entire game**: it loads every other engine
module (`engine`, `materialsystem`, `inputsystem`, `vphysics`, `datacache`,
`studiorender`, `soundemittersystem`, `vscript`, `vguimatsurface`, `vgui2`/`rocketui`,
`filesystem_stdio`), wires the Steam/`gameinfo.txt` filesystem environment, does
single-instance locking, then hands control to `engine.so` and doesn't get it back
until the process is exiting. It is, in effect, the concrete "main()" of the whole
`CAppSystemGroup` machinery described in PORTING.md.

**Where it hands off:** `CSourceAppSystemGroup::Main()` is one line —
`return g_pEngineAPI->Run();` (`launcher/launcher.cpp:966-969`). Everything after that
point — the actual per-frame loop, and eventually the `winit` event-loop integration
PORTING.md's "Rendering & windowing" section describes — lives inside `engine`, not
here. **This means the `winit` control-flow inversion is explicitly out of scope for
this port** — it happens when `engine` (and `appframework`'s `ILauncherMgr` pieces)
get ported, not now. This doc only covers getting `launcher` itself to Rust while
everything it loads, including the window/event-pump layer, stays exactly as it is
today.

## The object graph `launcher` builds and drives

```
LauncherMain(argc, argv)
  GrabSourceMutex()                          -- single-instance file lock
  CommandLine()->CreateCmdLine(argc, argv)
  UTIL_ComputeBaseDir()
  ... locale / cmdline cleanup ...
  loop (bRestart):
    CSourceAppSystemGroup sourceSystems;      -- : CSteamAppSystemGroup : CAppSystemGroup
    CSteamApplication steamApplication(&sourceSystems);   -- : CAppSystemGroup
    steamApplication.Run()
      -> CSteamApplication::Create()          -- loads filesystem_stdio, gets IFileSystem
      -> CSteamApplication::Main()
           sourceSystems.Setup(m_pFileSystem, this)
           return sourceSystems.Run()
             -> CSourceAppSystemGroup::Create()    -- AddSystems: engine, inputsystem,
                                                       vphysics, materialsystem, datacache x3,
                                                       studiorender, soundemittersystem,
                                                       vscript, vguimatsurface, vgui2,
                                                       engine (again, for IEngineAPI);
                                                       also directly constructs the
                                                       ILauncherMgr (CreateSDLMgr()/
                                                       CreateCCocoaMgr())
             -> CSourceAppSystemGroup::PreInit()    -- ConnectTier1/2/3Libraries, Steam/
                                                       gameinfo.txt mount, reslistgenerator
                                                       init, builds StartupInfo_t, calls
                                                       g_pEngineAPI->SetStartupInfo(info)
             -> CSourceAppSystemGroup::Main()       -- return g_pEngineAPI->Run();  <-- HANDOFF
             -> CSourceAppSystemGroup::PostShutdown()/Destroy()
    bRestart = (INIT_RESTART from Init stage) || (RUN_RESTART from Main) || reslistgenerator->ShouldContinue()
  ReleaseSourceMutex()
```

`CSteamApplication` and `CAppSystemGroup` themselves are **not** part of this port —
they're compiled into `appframework_client` (`appframework/AppSystemGroup.cpp`,
`appframework/posixapp.cpp`; see `public/appframework/AppFramework.h` for
`CSteamApplication`'s shape). `launcher`'s own sources
(`launcher/launcher.cpp`, `launcher/reslistgenerator.cpp`, and its own compiled copy
of `public/filesystem_init.cpp` — see "A wrinkle: shared source, not a shared
library" below) are what get ported here; `appframework` stays untouched C++ that the
Rust `launcher` links against and subclasses, exactly as `launcher.cpp` does today.

## The FFI shape: this needs more than a pure-interface shim

Every shim in PORTING.md so far assumes the C++ side is a **pure abstract interface**
(`tier1/interface.h`'s own rule: "must be ALL pure virtuals, and have no data
members"). `CSourceAppSystemGroup : public CSteamAppSystemGroup` breaks that
assumption: `CAppSystemGroup` is a real class with real data members
(`m_Modules`, `m_Systems`, `m_SystemDict`, ...) and non-virtual protected methods
(`LoadModule`, `AddSystem`, `AddSystems`, `FindSystem`, `GetFactory`, ...) that a
subclass is expected to call, not just override. `CAppSystemGroup::Run()` /
`OnStartup()` / `OnShutdown()` call `Create()` / `PreInit()` / `Main()` /
`PostShutdown()` / `Destroy()` **virtually** on `this`, exactly like an
`IAppSystemGroup`-shaped Direction-A case — but the object being subclassed carries
state and inherited behavior a pure-interface shim never had to think about.

The shim still works, it just has two jobs instead of one:

1. **Override the virtuals** (`Create`, `PreInit`, `Main`, `PostShutdown`, `Destroy` —
   `PostInit`/`PreShutdown` have usable default impls and `CSourceAppSystemGroup`
   doesn't override them, so the shim doesn't need to either), each trampolining into
   Rust — same mechanics as every other Direction-A shim in PORTING.md.
2. **Expose the inherited protected members Rust needs to call** — `LoadModule`,
   `AddSystem`/`AddSystems`, `FindSystem`, `GetFactory` — as additional public
   wrapper methods on the same shim class (or `extern "C"` free functions taking the
   shim pointer as `this`). This is ordinary Direction B, just reached through the
   shim object instead of a `g_pWhatever` global.

```cpp
// launcher/rust_shim.cpp (sketch)
class CRustSourceAppSystemGroup : public CSteamAppSystemGroup
{
public:
    // IAppSystemGroup overrides -> Direction A trampolines
    bool Create() override    { return rust_launcher_create( m_pState ); }
    bool PreInit() override   { return rust_launcher_preinit( m_pState ); }
    int  Main() override      { return rust_launcher_main( m_pState ); }
    void PostShutdown() override { rust_launcher_post_shutdown( m_pState ); }
    void Destroy() override   { rust_launcher_destroy( m_pState ); }

    // Direction B: exposes otherwise-`protected` CAppSystemGroup members to Rust
    AppModule_t LoadModule_(const char *dll) { return LoadModule(dll); }
    IAppSystem *AddSystem_(AppModule_t m, const char *iface) { return AddSystem(m, iface); }
    void *FindSystem_(const char *iface) { return FindSystem(iface); }
    CreateInterfaceFn GetFactory_() { return GetFactory(); }

private:
    RustLauncherState *m_pState = rust_launcher_new(this);
};
```

**Suggest folding this pattern back into `PORTING.md`'s "Direction A" section** once
it's proven here — the doc currently only describes the pure-interface case. Flagging
it in this doc for now rather than editing `PORTING.md` preemptively, since it hasn't
been built yet.

## A wrinkle: shared source, not a shared library

`public/filesystem_init.cpp` (Steam environment detection, `gameinfo.txt` parsing,
search-path mounting — `FileSystem_SetupSteamEnvironment`, `FileSystem_MountContent`,
`FileSystem_LoadSearchPaths`, etc., declared in `public/filesystem_init.h`) is **not**
a module — it's a 1600-line `.cpp` file compiled independently into five different
targets: `launcher`, `appframework_client`, `vgui2/src`, `game/client`, and
`dedicated` (each `CMakeLists.txt` lists it directly via `target_sources`). Each
target gets its own copy of the compiled code and its own independent globals — this
is the same "duplicate small foundational source across binaries" pattern `tier1`
uses, just at the `.cpp` level instead of a static-lib level.

Concretely, this means:
- `appframework_client`'s copy is what `CSteamApplication::Create()` uses to load
  `filesystem_stdio` and get the first `IFileSystem*` — **not** launcher's copy, and
  not something this port touches.
- `launcher`'s **own** copy is what `CSourceAppSystemGroup::PreInit()` calls
  (`FileSystem_SetupSteamEnvironment`, `FileSystem_MountContent`) to mount the actual
  game content search paths — this genuinely is part of porting `launcher`, since it's
  `launcher_client.so`'s own compiled code, not a dependency on another module.

**Recommendation:** don't reimplement `filesystem_init.cpp`'s logic in Rust in this
first pass. It's substantial (KeyValues-based `gameinfo.txt` parsing, Steam env-var
detection, search-path construction) and security/correctness-sensitive (it decides
which directories the game reads content from) — treat it as unmodified C++ launcher
calls into via Direction B for now (compile launcher's copy of `filesystem_init.cpp`
into the same `rust_shim.cpp` translation unit, call its free functions with thin
`cxx`-friendly wrappers around `CFSSteamSetupInfo`/`CFSMountContentInfo`). Revisit as
a separate, focused follow-up once more of the filesystem side of things is ported —
possibly worth a `portdocs/FILESYSTEM_INIT.md` of its own someday, since it's shared
by five targets and porting it properly means understanding all five call sites, not
just launcher's.

## What to actually port vs. what to drop

`launcher/launcher.cpp` is 1972 lines, but a lot of it is dead weight for this port
specifically because of decisions already made in `PORTING.md`:

**Genuinely needs a Rust implementation:**
- `LauncherMain` itself — the restart loop, cmdline setup, locale fix
  (`launcher.cpp:1485-1972`, the `#elif defined(POSIX)` branch of that function —
  the only branch that's in scope per PORTING.md's "Supported platforms").
- `GrabSourceMutex` / `ReleaseSourceMutex` (`launcher.cpp:1082-1211`) — real,
  platform-specific single-instance locking: `fcntl(F_SETLK)` on Linux, `open(...,
  O_EXLOCK)` on macOS, both keyed on a CRC32 of the `-game` argument in a
  `$TMPDIR`/`/tmp` lock file. Faithful, needed behavior.
- `UTIL_ComputeBaseDir` / `GetBaseDirectory` / `GetGameDirectory` /
  `RemoveSpuriousGameParameters` (`-game` argument dedup for Steam's `applaunch`
  quirk) — needed, platform-agnostic-ish logic.
- `CSourceAppSystemGroup::Create`/`PreInit`/`Main`/`PostShutdown`/`Destroy`
  (`launcher.cpp:709-996`) — the actual module list and lifecycle. This is the core of
  the port.
- `CLauncherLoggingListener` (`launcher.cpp:183-225`) — routes `LS_WARNING`/
  `LS_ASSERT`/`LS_ERROR` log messages to a native message box. Needs a decision (see
  "Open questions" below), not just a port.
- `launcher`'s own copy of the `filesystem_init.cpp` call sites (see above — kept as
  C++ for now, but the *calls* into it from `PreInit()` are part of this port).

**Not part of this port at all:** a fair chunk of `launcher.cpp` only exists inside
platform-conditional bodies that are empty or absent for POSIX already. None of that
needs a Rust equivalent — per PORTING.md's "Supported platforms," it was never in
scope, not "not yet ported." This doc doesn't enumerate it; skim `launcher.cpp` for
the `#elif defined(POSIX)`/unconditional code when implementing and disregard the
rest.
- `CThreadWatchdog` / `CLoadMemoryWatchdog` (`launcher.cpp:1351-1443`) — gated behind
  `#if LOADING_MEMORY_WATCHDOG`, which is `#define`d out (`// #define
  LOADING_MEMORY_WATCHDOG 100`, commented). Not compiled today. Skip.
- The `-edit` / Hammer code path (`m_bEditMode`, `g_pHammer`, `IHammer`,
  `DetermineDefaultMod`/`DetermineDefaultGame`'s Hammer branches). **Recommend
  treating as unsupported** in the Rust port — Hammer is level-editor tooling, not
  part of running the game, and isn't part of what a Linux/macOS CS:GO client needs.
  Flagged under "Open questions" for confirmation rather than assumed.
- The `INCLUDE_SCALEFORM` branch in `Create()` (`launcher.cpp:784-809`) — per
  PORTING.md's UI decision, ScaleformUI isn't part of this port's future regardless;
  no reason to carry it. Keep only the `INCLUDE_ROCKETUI` branch (what CI actually
  builds with today), matching current default.

**Legacy dev tooling, recommend stubbing rather than porting:**
- `CLogAllFiles` (`launcher.cpp:411-590`) and `CResListGenerator`
  (`launcher/reslistgenerator.cpp`, `IResListGenerator` in `reslistgenerator.h`) — the
  "reslist" generation system (dumps which files got touched during a level, for old
  console-port preload optimization). Both are inert unless `-makereslists` is passed
  with a command file (`CResListGenerator::Init`, `reslistgenerator.cpp:199-231`;
  `CLogAllFiles::Init`, `launcher.cpp:447-463` — both bail immediately without that
  flag). **`CLogAllFiles` also hardcodes `\\` path separators even in its POSIX code
  paths** (`launcher.cpp:494`, `502`, `528`, etc. — `CFmtStr("%s\\%s\\%s", ...)`),
  meaning it looks already broken on Linux/macOS as shipped, not just unused.
  Recommend: implement `IResListGenerator`'s five methods as inert no-ops in Rust
  (`IsActive()` → `false`, `ShouldContinue()` → `false`, matching the already-inert
  common case) and drop `CLogAllFiles` entirely, rather than porting either
  faithfully. `reslistgenerator->ShouldContinue()` still needs to exist and return
  `false` since `LauncherMain`'s restart loop reads it.
- `IResourceAccessControl` connection (`launcher.cpp:830-837`, gated on `-dev`) — a
  real but minor dev-mode consistency check. Low priority; fine to defer or drop.

## Open questions (need your input before/while implementing)

1. **Message boxes.** `CLauncherLoggingListener::Log` and `GrabSourceMutex`'s
   single-instance failure currently pop a native OS dialog (`SDL_ShowSimpleMessageBox`
   on Linux, `CFUserNotificationDisplayAlert` on macOS) for early fatal errors —
   before any window/render loop exists. Since we're dropping SDL2 for `winit` (and
   `winit` has no message-box primitive), options: (a) a small dedicated crate like
   [`rfd`](https://docs.rs/rfd) for native message boxes, (b) drop the dialog and rely
   on stderr/the terminal, accepting that a user launching by double-clicking won't see
   the error. Recommend (a), since these are exactly the "can't even get a window up"
   errors where a terminal isn't guaranteed to be visible — but this is a product
   decision, not just an engineering one.
2. **`-edit`/Hammer support** — confirm dropping it (see above) rather than porting.
3. **Reslist generation** — confirm stubbing to inert no-ops (see above) rather than
   porting `CResListGenerator` faithfully.
4. **`GetExecutableName`/`GetExecutableFilename` are already no-ops on POSIX today**
   (`launcher.cpp:251-262`, `268-296` — both just `return false;` / return an empty
   string unconditionally in the current code). One consequence:
   `UTIL_ComputeBaseDir`'s `GetExecutableName(...)` call always fails on POSIX, so
   `g_szBasedir` is **only** ever populated from an explicit `-basedir` argument — if
   none is passed, base dir is `""` (relying on the process's CWD already being the
   game directory, since `csgo.sh`/`launcher_main` are expected to be run from
   `../game`). This is existing, shipping behavior, not a bug introduced by this port
   — the Rust version should replicate it as-is unless you'd rather have it actually
   compute the real executable path (which Rust's `std::env::current_exe()` could do
   trivially, unlike the C++ version). Flagging rather than silently "fixing," since
   it changes observable behavior (e.g. running the binary via a relative path from
   somewhere other than the game directory would start working).

## Suggested staged plan

1. Land the `CRustSourceAppSystemGroup` shim + `rust_launcher_*` trampolines with
   every method initially just forwarding to a literal, unmodified transliteration of
   today's `CSourceAppSystemGroup`/`LauncherMain` logic in Rust — prove the "subclass
   a concrete C++ base" shim shape works end to end (game still boots) before changing
   any behavior.
2. Port `GrabSourceMutex`/`ReleaseSourceMutex` and `RemoveSpuriousGameParameters` as
   free-standing, easily unit-testable Rust functions — no C++ shim dependency, pure
   logic, good early confidence check.
3. Resolve the "Open questions" above (at least #1 and #2) before writing
   `CLauncherLoggingListener`'s replacement and the `-edit` branch's removal.
4. Wire `Create()`'s `AppSystemInfo_t` list and the `ILauncherMgr` construction
   (`CreateSDLMgr()`/`CreateCCocoaMgr()`) — straight Direction-B calls, no behavior
   change; this is the one place this port touches the windowing layer, and it should
   stay a call into unmodified C++ (see "Where it hands off" above — the `winit` swap
   is `appframework`'s job later, not this port's).
5. Leave `filesystem_init.cpp` and `reslistgenerator`'s inert-stub as the last things
   touched, since they're the most likely to hide subtle behavior differences.
