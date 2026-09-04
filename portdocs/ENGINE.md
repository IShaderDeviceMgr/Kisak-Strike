# ENGINE.md

Porting design doc for `engine/` → `src/engine/`.

Read [`../PORTING.md`](../PORTING.md) first. Paths here are relative to the original
tree; prefix them with `legacy/` to open them.

**Status: three of thirteen modules landed** — `window/` (§7.3), `host/` (§7.2) and the
geometry slice of `world/` (§7.14). The binary loads a real Portal 2 `.bsp` and draws it.
**What exists is documented in [`../rustdocs/ENGINE.md`](../rustdocs/ENGINE.md)**, which
is the doc to read before calling in; this one remains the map of what is still ahead.
§6's control-flow inversion is resolved — see the note at the end of it.

Two headline decisions:

1. **`engine` is not one port.** It is ~361k lines across 23 recognizable subsystems that
   happen to share a `.so`. Porting it as a unit is a category error; see §5.
2. **Each subsystem becomes its own Rust module under `src/engine/`.** `sound` →
   `src/engine/audio/`, networking → `src/engine/net/`, and so on — ordinary Rust modules
   in the single binary, with ordinary Rust visibility rules doing the job the `.so`
   boundary never did. §1 is the map.

---

## 1. Target module layout

The subsystems in §7 are not just a reading aid — they are the module boundaries. Under
PORTING.md's one-crate architecture there is no `.so` seam between them, so the seams that
matter are Rust ones: `mod`, `pub(crate)`, and real types crossing between them.

Modules marked **done** exist in `src/`; the rest are the plan.

```
src/engine/
  mod.rs          Engine construction + ownership of the subsystems below    **done**
  host/           frame loop, host state machine, level/map lifecycle        (§7.2) **done**
  window/         winit integration, video mode, input translation           (§7.3) **done**
  input/          button state, bindings, mouse look, controllers            (§7.3/§7.4) **stages 1-2 done**
  console/        cvars, command buffer, dev console                         (§7.4)
  client/         client connection lifecycle, entity parsing, prediction    (§7.5)
  server/         server lifecycle, connected clients, snapshot writing      (§7.6)
  net/            UDP transport, netchannel, framing, protobuf messages      (§7.7)
    datatable/    SendTable/RecvTable entity delta encoding                  (§7.8)
    stringtable/  replicated string↔index tables                             (§7.9)
  events/         the named game-event bus                                   (§7.10)
  world/          BSP + model loading, visibility, collision hulls           (§7.14) **geometry only**
    disp/         displacement (terrain) surfaces                            (§7.15)
  trace/          ray/hull traces, spatial partition                         (§7.17)
  render/         thin front-end over src/materials — view setup, draw lists (§7.16)
  paint/          Portal 2 paint/gel maps                                    (§7.16)
  audio/          mixing, DSP, streaming, spatialization, voice              (§7.18)
  save/           savegame serialization                                     (§7.21)
```

**Fourteen modules from twenty-three subsystems.** (Thirteen until `input/` was split
out of `window/` and `console/` — see `portdocs/ENGINE_INPUT.md` §8.1 for why.) Six
subsystems are deleted outright rather than ported (§7.12, §7.13, §7.19, §7.20, §7.22,
§7.23 — ~45,700 lines), one dissolves into the launcher and `mod.rs` (§7.1), and the
renderer front-end (§7.16, ~40k) mostly folds into the `src/materials/` work rather than
being ported in place.

Notes on specific choices:

- **`net/` owns `datatable/` and `stringtable/`** because both exist only to put entity
  and string state on the wire; they have no meaning apart from the protocol. Their
  formats are fixed together, they version together, and they should fail to compile
  together when the protocol changes.
- **`render/` stays thin.** Everything in §7.16 that drives the material system is being
  replaced by `wgpu` (see [`MATERIALSYSTEM.md`](MATERIALSYSTEM.md)). What survives in
  `src/engine/render/` is the part that is genuinely *engine* logic — deciding what to
  draw, in what order, from what view — not how to draw it.
- **`paint/` is a sibling, not a child of `render/`.** `engine/paint.cpp` (1,656 lines)
  pairs directly with `cmatpaintmaps.cpp` on the material-system side, and per
  MATERIALSYSTEM.md §8 the two halves must be ported together. Giving it its own module
  keeps that pairing visible instead of burying it in the renderer.
- **`window/` will end up small.** Much of §7.3's 5,700 lines is mode enumeration and
  OS event pumping that `winit` provides outright; what remains is the engine's own
  event translation and the frame-pacing policy of §6.

`src/engine/mod.rs` owns the subsystems as real fields and hands out `&mut` where a
subsystem needs another. That replaces the ambient `g_p*` globals the C++ uses to find
everything — which is the single biggest structural difference between the two trees, and
the one most likely to force honest thinking about who actually owns what.

## 2. Scale

| | `launcher` (ported) | **`engine`** |
|---|---|---|
| Source files (`.cpp`+`.h`) | 4 | **665** |
| Lines | ~2,500 | **361,293** |
| Root `.cpp` files | — | **232** |
| Indexed graph nodes | 132 | **24,122** |
| `EXPOSE_SINGLE_INTERFACE` registrations | 0 | **20** |
| Internal call-graph clusters | — | **12** |

`engine` is roughly **140× the size of `launcher`**, the module that took a full session.
It is also the module every other ported module eventually has to interoperate with.

## 3. What `engine` is, and what survives of its startup

`engine_client.so` is the Source engine proper: frame loop, client and server game-state
machinery, renderer front-end, sound, network protocol, console/cvar system, BSP/model/
material glue, and the VGui host.

Structurally it is *itself* an app-system host. `CEngineAPI::RunListenServer()`
(`sys_dll2.cpp:1415`) constructs a **`CModAppSystemGroup`** (`sys_dll2.cpp:414`) — a third
`CAppSystemGroup` nested inside the two `launcher` already builds — which loads the
mod-dependent modules and whose `Main()` (`sys_dll2.cpp:2374`) finally calls
`CEngineAPI::MainLoop()`.

```
launcher (Rust, ported)
  └─ CSteamApplication ─┐
      └─ CSourceAppSystemGroup       loads engine, materialsystem, …
          └─ IEngineAPI::Run()       sys_dll2.cpp:1742
              └─ RunListenServer()   sys_dll2.cpp:1415
                  ├─ ModInit()       creates window + video mode
                  └─ CModAppSystemGroup   sys_dll2.cpp:414   (3rd app-system group)
                      └─ ::Main()    sys_dll2.cpp:2374
                          ├─ eng->Load()
                          └─ CEngineAPI::MainLoop()   sys_dll2.cpp:1132  ← THE frame loop
```

**All three `CAppSystemGroup` layers are mechanism to delete.** What survives is the
*sequence* they encode — the order in which subsystems must become usable, and what has
to exist before a map can load. Read `CModAppSystemGroup::Create`/`Main` for that ordering
and then discard the machinery. In Rust this is `Engine::new(deps) -> Result<Engine>`
followed by a run loop; the three-layer nesting exists only because each layer `dlopen`ed
the next.

## 4. The public surface

`engine` registers **20** interfaces via `EXPOSE_SINGLE_INTERFACE*`. Under the current
architecture these are **not** shim obligations — nothing links the C++, and each becomes
an ordinary Rust API or disappears. The table is still worth keeping as a map of what the
module exposes and to whom:

| Interface | File | Becomes |
|---|---|---|
| `IVEngineClient` | `cdll_engine_int.cpp:736` | The `client`↔game API. Ported with `game/client` |
| `IVEngineServer` | `vengineserver_impl.cpp:2061` | The `server`↔game API. Ported with `game/server` |
| `IEngineAPI` | `sys_dll2.cpp:530` | Dissolves — `launcher` calls `engine::run()` directly |
| `IVRenderView` | `view.cpp:713` | `src/engine/render/` internals |
| `IVModelRender` | `l_studio.cpp:971` | `src/engine/render/` ↔ `studiorender` |
| `IVModelInfo` / `IVModelInfoClient` | `ModelInfo.cpp:1024,1450` | `src/engine/world/` API |
| `IGameEventManager2` | `GameEventManager.cpp:62` | `src/engine/events/` API |
| `IStaticPropMgrClient` / `Server` | `staticpropmgr.cpp:459-460` | `src/engine/render/` internals |
| `IShadowMgr` | `shadowmgr.cpp:555` | `src/engine/render/` internals |
| `ICvarQuery` | `cvar.cpp:232` | `src/engine/console/` internals |
| `INetSupport` | `net_support.cpp:39` | `src/engine/net/` API |
| `IEngineVGui` | `vgui_baseui_interface.cpp:552` | **Deleted** — `egui` |
| `IGameUIFuncs` | `sys_dll2.cpp:2982` | **Deleted** — `egui` |
| `IDedicatedServerAPI` | `sys_dll2.cpp:2564` | Deferred — see §11 |
| `IUploadGameStats` | `sv_uploadgamestats.cpp:527` | **Deleted** |
| `IFileLoggingListener` | `sys_dll.cpp:1001` | **Deleted** — `tracing` |
| `IBugReporterDefaultUsername` | `bugreporter.cpp:2916` | **Deleted** |
| replay history manager | `replayhistorymanager.cpp:523` | **Deleted** — §7.13 |

Seven of twenty are deleted outright. `IVEngineClient`/`IVEngineServer` are the wide ones
— roughly a hundred virtuals each, and the entire surface `game/client` and `game/server`
are written against. **Under the old FFI plan these were the single largest blocker in the
project.** They aren't any more: the game DLLs get ported too, and at that point the
interface is just the boundary between two Rust modules, free to be redesigned per
PORTING.md's polarity rule.

**Consumed** interfaces (`CEngineAPI::Connect`, `sys_dll2.cpp:536-613`): `IFileSystem`,
`IPhysics`, `ISoundEmitterSystemBase`, `IStudioRender`, `IDataCache`, `IMDLCache`,
`IMatSystemSurface`, `IInputSystem`, `ILauncherMgr`, `IRocketUI`, plus the shader API via
`Shader_Connect()`. In Rust these are constructor arguments to `Engine::new`, and three
of them (`IMatSystemSurface`, `ILauncherMgr`, `IRocketUI`) don't survive at all.

## 5. Why this can't be one port

**1. It's the frame loop, and the frame loop is changing.** The `winit` control-flow
inversion PORTING.md describes lands *here*, in `CEngineAPI::MainLoop`/`PumpMessages` and
`CEngine::Frame` (§6). That's a structural redesign on top of a translation, in the same
code.

**2. It's the renderer front-end, and the renderer is being replaced.** The `gl_*.cpp`
family drives `materialsystem`/`shaderapidx9`, and that whole tower is `wgpu` now. Porting
engine's renderer front-end faithfully *and then* replacing what it talks to is doing the
work twice.

**3. It's 23 subsystems that should be 23 Rust modules.** The graph's own community
detection finds 12 clusters with cohesion 0.57–0.89, corresponding closely to recognizable
subsystems. **Under the old FFI plan this was an obstacle** — the seams were all internal
to one `.so`, so there was no `CreateInterface` boundary to hang a shim off. Under the
current architecture it inverts into the plan: those seams become the module boundaries in
§1, and no shim is needed at any of them.

**4. The `tier0`/`tier1` rule bites hardest here.** `engine` is the codebase's heaviest
user of `CUtlVector`/`KeyValues`/`ConVar`/`bitbuf`, and `bitbuf` is load-bearing for the
network wire format — "reimplement it in Rust" means "reimplement a bit-exact wire
encoder."

**5. Nothing here can be validated until the boot path is complete.** Per PORTING.md's
"no incremental scaffolding," there is no hybrid binary to test a half-ported engine
against. That raises the cost of porting subsystems out of dependency order, and is the
main argument for the sequencing in §10.

## 6. Control flow: where the `winit` inversion lands

Re-read this section when the windowing work starts.

### Today (pull-based)

`CEngineAPI::MainLoop()` (`sys_dll2.cpp:1132-1173`) is a bare `while (true)` that, per
iteration:

1. Checks `eng->GetQuitting()` and returns if quitting — the return value distinguishes
   restart from exit, becoming `RUN_RESTART`, which propagates back to `launcher`'s
   restart loop.
2. Calls `PumpMessages()` (`sys_dll2.cpp:988`) → `g_pLauncherMgr->PumpWindowsMessageLoop()`
   → `g_pInputSystem->PollInputState()` → `game->DispatchAllStoredGameMessages()`.
3. Calls `eng->Frame()`.

### The input path, in full

Every hop here is either reproduced or replaced by the `winit` port:

```
SDL2 (or Cocoa)
  └─ CSDLMgr::PumpWindowsMessageLoop()      appframework/sdlmgr.cpp:1547
      └─ internal CCocoaEvent queue          public/appframework/ilaunchermgr.h:170
          └─ CInputSystem::PollInputState()  drains via ILauncherMgr::GetEvents()
              └─ InputEvent_t queue          g_pInputSystem->GetEventData()
                  └─ CGame::DispatchAllStoredGameMessages()  engine/sys_mainwind.cpp:509
                      └─ CGame::DispatchInputEvent()          sys_mainwind.cpp:399
                          ├─ Key_Event()                       (keyboard/buttons)
                          ├─ g_pMatSystemSurface->HandleInputEvent()   (VGui first)
                          ├─ g_pRocketUI->HandleInputEvent()           (then RocketUI)
                          └─ g_ClientDLL->HandleGameUIEvent()          (then GameUI)
```

The ordering is load-bearing: VGui gets first refusal, RocketUI second (and only when the
VGui console isn't up — `cv_vguipanel_active`), GameUI last. Since `egui` replaces all
three, this chain collapses into `egui`'s "did the UI consume this event" answer — a real
behavioral design question, not a translation.

**The whole left column collapses.** SDL2 → `CCocoaEvent` → `inputsystem` →
`InputEvent_t` → `CGame` exists to normalize platform events; `winit` already delivers
normalized events. `src/engine/window/` translates `WindowEvent` straight into the
engine's own input events.

### The hard part: `CEngine::Frame` does its own pacing

`CEngine::Frame()` (`sys_engine.cpp:418-614`) is **not** a "render one frame" function. It:

- Reads the clock itself (`Sys_FloatTime()`), accumulates `dt`, maintains
  `m_flFrameTime`/`m_flFilteredTime`.
- Calls `FilterTime()` (`sys_engine.cpp:264-411`) to decide whether to run a frame at all.
- **If not, it sleeps** — `ThreadNanoSleep()` on POSIX, gated on a
  `sleep_when_meeting_framerate` convar — and returns without doing work.
- Only then dispatches to `HostState_Frame()` on a `DLL_ACTIVE`/`DLL_PAUSED`/`DLL_CLOSE`/
  `DLL_RESTART` state machine, translating the latter two into
  `SetQuitting(QUIT_TODESKTOP)`/`QUIT_RESTART`.

`winit` wants to own exactly this: `ControlFlow::Poll`/`Wait`/`WaitUntil` *is* the pacing
mechanism. Two systems both owning pacing — one sleeping inside a callback the other
scheduled — is the specific failure mode to design against.

**Target shape** (to be validated, not settled):

- `window_event` translates `WindowEvent` into the engine's input events directly.
- `about_to_wait` drives one engine tick.
- `FilterTime`'s *policy* survives (respect `fps_max` and the convars that tune it); the
  *mechanism* becomes `ControlFlow::WaitUntil(next_frame_deadline)`.
- `SetQuitting` maps to `ControlFlow::Exit`, but **the restart-vs-exit distinction must
  survive** — `src/launcher/`'s restart loop depends on it. Check against
  `src/launcher/mod.rs` when building this.

Note the frame loop also constrains `src/materials/`: surface acquire and present have to
sit at a well-defined point in the tick. MATERIALSYSTEM.md stage 1 establishes that
boundary, which is why it lands first.

### Resolved

All four target-shape bullets above landed as written, with one refinement worth
recording: **`FilterTime` split into a policy half and a mechanism half.**
`host::FrameClock` decides whether a frame runs and when the next may
(`fps_max`, `MAX_FPS`, the frame-time clamps); `window::about_to_wait` turns that answer
into `ControlFlow::WaitUntil`. Neither half can sleep, which is what makes the
two-systems-both-pacing failure structurally impossible rather than merely avoided.

`SetQuitting`'s restart-vs-exit distinction survives as `host::Outcome` →
`window::RunOutcome`, out to the launcher. The *outer* `CEngine` state machine
(`m_nDLLState`/`m_nQuitting`) is deleted outright: it existed only to carry that decision
across the `IEngine` boundary by polling, and there is no such boundary.

The input path (the left column above) is **now done as far as it can go without
`console/` and `egui`**: `window/translate.rs` plus one `match` arm per event turns
`WindowEvent` and `DeviceEvent::MouseMotion` into `input::Event`, and `Engine::frame`
drains the queue once a tick. What is left of the chain is the two ends —
bindings (`portdocs/ENGINE_INPUT.md` stage 3, wants the command buffer) and the `egui`
precedence question with the key-up latch it carries (stage 4). **API:
`rustdocs/ENGINE.md`.**

## 7. Subsystem breakdown

The 232 root `.cpp` files plus subdirectories, grouped into 23 subsystems. Line counts are
`wc -l`.

**Caveat on the grouping:** Valve left most `// Purpose:` headers blank, so these are
inferred from filenames, contained symbols, and cluster analysis — not authoritative
documentation. Boundaries between adjacent subsystems (especially client/server/netcode
and renderer/world) are genuinely fuzzy in the source. Treat this as a map, not a
partition; several files legitimately belong to two groups.

### 7.1 Bootstrap & module hosting — ~5,700 → dissolves
`sys_dll2.cpp` (2,984 — `CEngineAPI`, `CModAppSystemGroup`, `CDedicatedServerAPI`,
`CGameUIFuncs`), `sys_dll.cpp` (1,740), `sys_engine.cpp` (686 — `CEngine`, the frame tick
+ pacing), `traceinit.cpp` (177), `buildnum.cpp` (79), `initmathlib.cpp` (56),
`quakedef.cpp` (13), `*_pch.cpp` (10 each).
**Mostly deleted** (§3). The frame tick from `sys_engine.cpp` goes to `host/`; the
startup ordering informs `mod.rs`.

### 7.2 Host / frame orchestration — ~15,000 → `host/`
`host.cpp` (6,654), `host_saverestore.cpp` (3,538), `host_cmd.cpp` (3,049),
`host_state.cpp` (958 — a clean `HS_RUN`/`HS_LOAD_GAME`/`HS_NEW_GAME`/
`HS_CHANGE_LEVEL_SP|MP`/`HS_GAME_SHUTDOWN`/`HS_SHUTDOWN`/`HS_RESTART` state machine),
`host_listmaps.cpp` (812).
`host_state.cpp` is the cleanest file in the module and maps almost directly onto a Rust
enum state machine — a good place to start.

### 7.3 Windowing & video mode — ~5,700 → `window/`
`sys_mainwind.cpp` (2,744 — `CGame`, `DispatchInputEvent`, the UI precedence chain),
`sys_getmodes.cpp` (2,712 — `CVideoMode`), `sys_linuxwind.cpp` (219 — dedicated-server
`CGame` stub), `igame.h` (17-method window abstraction).
**Primary `winit` target.** Expect this to shrink dramatically (§1).

### 7.4 Console, cvars & commands — ~5,600 → `console/` (+ `input/`)
`console.cpp` (1,652), `cvar.cpp` (1,425), `keys.cpp` (1,392 — **now `input/`**, see
`portdocs/ENGINE_INPUT.md`), `cmd.cpp` (1,171),
`ipc_console.cpp` (294), `netconsole.cpp` (258), `cl_bounded_cvars.cpp` (163),
`cheatcodes.cpp` (162), `baseautocompletefilelist.cpp` (97).
**Port early — everything depends on it.** **Stage 1 is done**; planned in
`portdocs/ENGINE_CONSOLE.md`, which corrects this entry in two ways. First the sizing:
only ~5,500 of the system is in `engine/`, and the objects, the registry and the command
buffer are in `tier1/convar.cpp` (1,531), `vstdlib/cvar.cpp` (1,317) and
`tier1/commandbuffer.cpp` (407) — ~12,200 lines in scope, not 5,600. Second the design:
the claim that the cvar registry "is the one piece of ambient global state that is
genuinely process-global and justifies a `OnceLock`" **is reversed there**. What is shared
is each cvar's *value*, not the registry — a cvar becomes an `Arc`-held cell with atomics
inside, so a subsystem holds a handle to the one cvar it wants and the registry is left
serving only name lookup, for one caller. No global, and `console/` depends on `std`
alone.

### 7.5 Client connection & state — ~13,600 → `client/`
`cl_main.cpp` (3,919), `baseclientstate.cpp` (3,819), `cdll_engine_int.cpp` (3,059 — hosts
`IVEngineClient`), `client.cpp` (2,349 — `CClientState`), `servermsghandler.cpp` (997),
`cl_ents_parse.cpp` (763), `cl_pluginhelpers.cpp` (641), `cl_entityreport.cpp` (614),
`cl_splitscreen.cpp` (418), `LocalNetworkBackdoor.cpp` (414), `cl_steamauth.cpp` (336),
`clientframe.cpp` (281), `clockdriftmgr.cpp` (238), `cl_null.cpp` (203),
`cl_parse_event.cpp` (135), `cl_pred.cpp` (75), `cl_localnetworkbackdoor.cpp` (43).

### 7.6 Server & game boundary — ~22,500 → `server/`
`baseserver.cpp` (4,551), `sv_main.cpp` (3,541), `sv_client.cpp` (2,534),
`vengineserver_impl.cpp` (2,550 — hosts `IVEngineServer`), `baseclient.cpp` (2,300 — the
*server's* view of a client), `sv_uploadgamestats.cpp` (1,219), `sv_steamauth.cpp`
(1,167), `pure_server.cpp` (977), `sv_ents_write.cpp` (976), `sv_remoteaccess.cpp` (967),
`sv_packedentities.cpp` (963), `sv_filter.cpp` (943), `sv_plugin.cpp` (847), `sv_log.cpp`
(833), `sv_precache.cpp` (672), `sv_rcon.cpp` (641), `sv_framesnapshot.cpp` (578),
`sv_logofile.cpp` (352), `pr_edict.cpp` (297), `sv_ipratelimit.cpp` (243),
`sv_uploaddata.cpp` (179), `enginesingleuserfilter.cpp` (152), `sv_redirect.cpp` (97),
`voiceserver_impl.cpp` (70), `sv_master.cpp` (60).
A single-player Portal 2 still runs a listen server, so this is **not** optional — but
`pure_server`, RCON, plugins, master-server registration, ratelimiting and stats upload
(~5,500 lines) all are.

### 7.7 Network transport — ~13,700 → `net/`
`net_ws.cpp` (5,338), `net_chan.cpp` (3,797), `net_steamsocketmgr.cpp` (1,177),
`download.cpp` (1,005), `downloadthread.cpp` (919), `cl_rcon.cpp` (865),
`DownloadListGenerator.cpp` (415), `socketcreator.cpp` (358), `net_support.cpp` (301),
`status.cpp` (271), `net_ws_queued_packet_sender.cpp` (268), `filetransfermgr.cpp` (68),
`bitbuf_errorhandler.cpp` (64), `net_synctags.cpp` (19).
Messages come from `common/netmessages.proto` (`prost`); the bit-packed framing around
them is hand-rolled (§8).

### 7.8 Entity serialization / datatables — ~8,200 → `net/datatable/`
`dt_send_eng.cpp` (1,850), `dt_encode.cpp` (1,464), `dt_test.cpp` (1,044), `dt.cpp` (922),
`dt_recv_eng.cpp` (912), `dt_localtransfer.cpp` (707), `dt_common_eng.cpp` (488),
`serializedentity.cpp` (402), `dt_instrumentation_server.cpp` (357),
`dt_instrumentation.cpp` (246), `packed_entity.cpp` (107), `changeframelist.cpp` (63),
`dt_recv_decoder.cpp` (53), `dt_stack.cpp` (51).
Source's `SendTable`/`RecvTable` delta compression. PORTING.md pins this format only while
`game/{client,server}` are C++ — **once they're ported too, the format becomes ours**, so
this is one of the few places where the constraint expires rather than being permanent.
Modernize the mechanism (`deku`/`bitvec`) regardless.

### 7.9 Network string tables — ~2,600 → `net/stringtable/`
`networkstringtable.cpp` (2,259), `NetworkStringTableItem.cpp` (232),
`networkstringtableserver.cpp` (67), `networkstringtableclient.cpp` (34).

### 7.10 Game events — ~1,500 → `events/`
`GameEventManager.cpp` (1,234), `GameEventManagerOld.cpp` (144),
`gameeventtransmitter.cpp` (122).
A named-event pub/sub bus with a `KeyValues`-shaped payload. Prime candidate for a real
Rust enum instead of stringly-typed fields — but the event *names and fields* are shared
with the game DLLs and with `.dem` files, so change them deliberately.

### 7.11 Demo recording & playback — ~14,700 → `demo/`
`cl_demo.cpp` (4,055), `cl_demosmootherpanel.cpp` (2,772), `cl_demoactioneditors.cpp`
(1,442), `cl_demouipanel.cpp` (1,168), `cl_demoaction_types.cpp` (1,017),
`cl_broadcast.cpp` (742), `demobuffer.cpp` (679), `demostreamhttp.cpp` (657),
`cl_demoaction.cpp` (645), `demofile.cpp` (641), `cl_demoactionmanager.cpp` (484),
`cl_demoeditorpanel.cpp` (386), `demostream.cpp` (7).
The four `*panel.cpp` files (~5,800) are VGui demo-editor UI and **delete with §7.19** —
leaving ~8,900 of actual record/playback.

### 7.12 HLTV / SourceTV — ~7,300 → **deleted**
`hltvserver.cpp` (4,059), `hltvclientstate.cpp` (1,154), `hltvbroadcast.cpp` (741),
`hltvclient.cpp` (615), `hltvdemo.cpp` (517), `hltvtest.cpp` (169).
Spectator relay for competitive multiplayer. Not a Portal 2 concern.

### 7.13 Replay — ~3,300 → **deleted**
`replayserver.cpp` (1,583), `replayhistorymanager.cpp` (532), `replayclient.cpp` (509),
`replaydemo.cpp` (446), `replay.cpp` (211).

### 7.14 World / BSP / model loading — ~16,300 → `world/`
`modelloader.cpp` (7,587 — **largest non-audio file in the module**), `cmodel.cpp` (4,067 —
BSP collision), `ModelInfo.cpp` (1,454), `cmodel_bsp.cpp` (1,297), `cmodel_disp.cpp` (603),
`world.cpp` (589 — entity/trigger spatial linking), `mod_vis.cpp` (467), `zone.cpp` (277),
`precache.cpp` (260), `bsplog.cpp` (181), `mem_fgets.cpp` (65), `mem.cpp` (52).
Largest graph cluster (433 members, cohesion 0.87). **`.bsp`/`.mdl` formats are fixed** —
parse with `binrw`/`nom`. `zone.cpp`/`mem.cpp`/`mem_fgets.cpp` are the hunk/zone
allocators and **delete outright** — that's what `std` allocation is for.

### 7.15 Displacements (terrain) — ~3,700 → `world/disp/`
`disp_interface.cpp` (1,461), `disp.cpp` (1,203), `disp_mapload.cpp` (1,009),
`disp_defs.cpp` (40), `disp_helpers.cpp` (20).

### 7.16 Renderer front-end — ~40,000 → `render/` + `paint/`, mostly folded into `src/materials/`
`gl_rsurf.cpp` (6,465), `l_studio.cpp` (5,659), `shadowmgr.cpp` (4,747),
`lightcache.cpp` (3,128), `Overlay.cpp` (3,107), `OcclusionSystem.cpp` (2,999),
`r_decal.cpp` (2,662), `staticpropmgr.cpp` (2,449), `gl_lightmap.cpp` (2,216),
`matsys_interface.cpp` (2,024), **`paint.cpp` (1,656 — essential for Portal 2)**,
`brushbatchrender.cpp` (1,430), `gl_rmain.cpp` (1,255), `buildcubemaps.cpp` (1,241),
`buildmodelforworld.cpp` (1,135), `gl_matsysiface.cpp` (1,085), `gl_rlight.cpp` (841),
`view.cpp` (716), `debug_leafvis.cpp` (701), `r_areaportal.cpp` (623),
`gl_drawlights.cpp` (415), `gl_screen.cpp` (394), `decal_clip.cpp` (341), `gl_warp.cpp`
(342), `gl_rmisc.cpp` (332), `LoadScreenUpdate.cpp` (284), `decals.cpp` (183),
`imagepacker.cpp` (167), `r_efx.cpp` (166), `gl_shader.cpp` (115), `gl_draw.cpp` (101),
`r_linefile.cpp` (74), `materialproxyfactory.cpp` (63).
**Do not port faithfully.** Split it: *what to draw* (visibility, leaf/PVS traversal,
draw-list construction, view setup, decal placement, light cache) is engine logic that
survives into `render/`; *how to draw it* (everything calling `IMatRenderContext` or
`IShaderAPI`) goes to `src/materials/`. Note `imagepacker.cpp` here is a **duplicate** of
the material system's — port one.

### 7.17 Collision, tracing & spatial queries — ~6,400 → `trace/`
`spatialpartition.cpp` (3,219), `enginetrace.cpp` (3,177), `gametrace_engine.cpp` (24).
Port faithfully — gameplay-visible behavior, and Portal 2's portal placement depends on
exact trace semantics.

### 7.18 Sound — ~97,200 → `audio/`
**The largest subsystem in the module by a wide margin**, and larger than this doc
previously recorded. Breakdown:

| Part | Lines |
|---|---:|
| `audio/private/` `.cpp` | 72,626 |
| `audio/` headers | 6,701 |
| `voice_codecs/` (`speex` 11,865, `minimp3` 2,204, `miles` 836, `celt` 237, `frame_encoder` 194) | 15,336 |
| Root (`EngineSoundClient.cpp` 601, `engsoundservice.cpp` 572, `EngineSoundServer.cpp` 517, `tmessage.cpp` 638, `sound_shared.cpp` 128, `snd_io.cpp` 94) | 2,550 |
| **Total** | **~97,200** |

Largest files: `snd_dsp.cpp` (12,358 — DSP/reverb, the biggest file in the whole engine),
`snd_dma.cpp` (10,033 — mixer core), `snd_mix.cpp` (5,440), `snd_wave_data.cpp` (4,154),
`snd_wave_source.cpp` (3,497), `vox.cpp` (2,860 — sentence/vocal synthesis),
`snd_mixgroups.cpp` (2,756), `voice.cpp` (2,090).
`snd_op_sys/` (**11,807**) is the data-driven "sound operator" stack — a graph of
`sos_op_*` operators driving mixing from script. **Portal 2-era and actively used**; do
not mistake it for dead CS:GO content.

What comes off the top before porting:
- **~15,300 vendored codecs → crates.** `speex`, `minimp3`, `celt` all have Rust
  equivalents or bindings; `miles` (836) is proprietary and out of scope.
- **~9,300 out-of-scope backends**: `snd_dev_direct` (2,093), `snd_dev_ps3audio` (2,022),
  `snd_wave_mixer_ps3_mp3` (1,947), `snd_wave_mixer_xma` (1,116), `snd_dev_xaudio`
  (1,067), `snd_dev_wave` (563), `snd_ps3_mp3dec` (481).
- POSIX backends that stay: `snd_dev_openal` (403), `snd_dev_sdl` (751),
  `snd_dev_mac_audioqueue` (481), `snd_dev_common` (1,250), `snd_posix` (15).

That leaves **~72,000 lines of real sound code**. The subsystem already has a **pluggable
device-backend abstraction**, which is the cleanest internal seam in the whole module and
the natural place to substitute a Rust audio crate (§11).

### 7.19 VGui host & debug panels — ~19,000 → **deleted**
`colorcorrectionpanel.cpp` (6,161), `vgui_baseui_interface.cpp` (3,013),
`cl_texturelistpanel.cpp` (3,060), `debugoverlay.cpp` (1,411), `vgui_vprofpanel.cpp`
(1,153), `perfuipanel.cpp` (713), `cl_foguipanel.cpp` (598), `vgui_drawtreepanel.cpp`
(564), `vgui_vprofgraphpanel.cpp` (445), `vgui_DebugSystemPanel.cpp` (382),
`vgui_texturebudgetpanel.cpp` (372), `vgui_askconnectpanel.cpp` (276),
`vgui_budgetfpspanel.cpp` (225), `vgui_budgetpanel.cpp` (182), `vgui_helpers.cpp` (174),
`perfwizard.cpp` (158), `vgui_watermark.cpp` (125), `cl_txviewpanel.cpp` (120),
`vgui_basepanel.cpp` (113).
`egui` replaces the host; the panels are developer tools that should be **rewritten small
in `egui` if wanted at all**, not ported. `debugoverlay.cpp` (1,411) is the exception —
in-world debug drawing is genuinely useful during a port and is cheap to reimplement.

### 7.20 Profiling, stats & dev tooling — ~8,300 → **mostly deleted**
`bugreporter.cpp` (3,603), `vprof_engine.cpp` (1,271), `vprof_record.cpp` (1,226),
`MapReslistGenerator.cpp` (1,036), `testscriptmgr.cpp` (527), `rpt_engine.cpp` (416),
`checksum_engine.cpp` (359), `blackbox.cpp` (229), `cbenchmark.cpp` (168),
`enginestats.cpp` (156), `DevShotGenerator.cpp` (149).
VProf is replaced by `tracing` + `tracy`/`puffin`. The bug reporter and stats upload are
Valve infrastructure that doesn't exist for us.

### 7.21 Save / restore — ~1,600 → `save/`
`saverestore_filesystem.cpp` (1,153), `saverestore_filesystem_passthrough.cpp` (304),
`singleplayersharedmemory.cpp` (126). (`host_saverestore.cpp` is counted in §7.2.)
**Portal 2 is single-player, so this is load-bearing**, more so than it would be for the
CS:GO base. Savegame format is ours to change (PORTING.md: "Format is ours to change") —
but only if we accept that existing saves don't load, which for a fresh port is fine.

### 7.22 Tool framework — ~2,900 → **deleted**
`toolframework.cpp` (1,803), `enginetool.cpp` (1,127). Hooks for Hammer and SFM-lineage
tooling to drive the engine. Related to the `-edit` path already dropped in `launcher`.

### 7.23 Platform, console & misc — ~4,900 → **mostly deleted**
`xboxsystem.cpp` (2,557 — X360, out of scope), `common.cpp` (1,186 — misc utilities, some
of which survives into whichever module uses it), `filesystem_engine.cpp` (500),
`info.cpp` (417 — userinfo key/value strings), `logofile_shared.cpp` (199),
`movieplayer_matchframework.cpp` (136), `randomstream.cpp` (48). Plus out-of-scope
console files: `engine_helper_ps3.cpp` (378), `buildworldlists_PS3.cpp` (173),
`buildindices_PS3.cpp` (115), `ps3/` (7 files), `xbox/` (headers only).

### Size ranking, for sequencing

| §7 | Subsystem | ~Lines | Module | Disposition |
|---|---|---:|---|---|
| 18 | Sound | 97,200 | `audio/` | ~72k real; self-contained, clean backend seam |
| 16 | Renderer front-end | 40,000 | `render/` + `paint/` | Split; most folds into `src/materials/` |
| 6 | Server & game boundary | 22,500 | `server/` | ~5.5k droppable (RCON, plugins, pure) |
| 19 | VGui host & debug panels | 19,000 | — | **Delete** (`egui`) |
| 14 | World / BSP / model loading | 16,300 | `world/` | Port faithfully (format fixed) |
| 2 | Host / frame orchestration | 15,000 | `host/` | The `winit` work |
| 11 | Demo record & playback | 14,700 | `demo/` | ~8.9k after deleting editor panels |
| 7 | Network transport | 13,700 | `net/` | Port faithfully |
| 5 | Client connection & state | 13,600 | `client/` | Ports with `game/client` |
| 20 | Profiling & dev tooling | 8,300 | — | **Mostly delete** (`tracing`) |
| 8 | Entity serialization | 8,200 | `net/datatable/` | Format fixed until game DLLs land |
| 12 | HLTV / SourceTV | 7,300 | — | **Delete** |
| 17 | Collision / tracing | 6,400 | `trace/` | Port faithfully |
| 3 | Windowing & video mode | 5,700 | `window/` | The `winit` work; shrinks a lot |
| 1 | Bootstrap & module hosting | 5,700 | — | **Dissolves** into `mod.rs` |
| 4 | Console, cvars & commands | 5,600 | `console/` | Port early |
| 23 | Platform / console / misc | 4,900 | — | **Mostly delete** |
| 15 | Displacements | 3,700 | `world/disp/` | Port faithfully |
| 13 | Replay | 3,300 | — | **Delete** |
| 22 | Tool framework | 2,900 | — | **Delete** |
| 9 | Network string tables | 2,600 | `net/stringtable/` | Port faithfully |
| 21 | Save / restore | 1,600 | `save/` | Port faithfully |
| 10 | Game events | 1,500 | `events/` | Port faithfully |

Counts mix `.cpp`-only figures (root subsystems) with whole-subtree figures (sound), so
the column doesn't sum cleanly to 361,293 — treat it as a scoping signal, not an audit.

**Roughly 45,700 lines are deleted outright** (§7.12, §7.13, §7.19, §7.20, §7.22, §7.23),
before counting the renderer front-end that folds into `src/materials/`, the ~15,300
vendored audio codecs replaced by crates, and the ~9,300 out-of-scope audio backends.

## 8. Networking and protobuf

`engine` compiles three `.proto` files into `engine/generated_proto/`
(`engine_inc.cmake:12-14`): `common/netmessages.proto`, `common/network_connection.proto`,
`common/engine_gcmessages.proto`, linking vendored `libprotobuf`
(`thirdparty/protobuf-3.5.1`).

This is the best news in the document: protobuf is cross-language by design, so `prost`
consumes the same `.proto` files and the message definitions need no hand-translation.
Two caveats to verify before relying on it:

- The vendored protobuf is **3.5.1**, old enough that generated-code details may differ
  from modern Rust codegen. Check for `proto2` syntax and required-field semantics, which
  Source protocols commonly use and `prost` handles differently.
- Protobuf covers the *message* layer, not Source's bit-packed `bitbuf` framing around it,
  which is hand-rolled. Reimplement that with `deku`/`bitvec` rather than transliterating
  `bf_read`/`bf_write` sequences.

Whether the framing keeps its exact byte layout is a **flag-day decision**, and per §7.8
the constraint expires once `game/{client,server}` are ported — at that point we own both
ends of the wire and the format becomes ours.

## 9. Dependencies

The C++ links (`engine_inc.cmake:455-485`): `appframework`, `bitmap`, `dmxloader`,
`mathlib`, `matsys_controls`, `soundsystem_lowlevel`, `tier0`, `tier2`, `tier3`,
`vstdlib`, `vtf`, `vgui_controls`, `videocfg`, `bzip2`, `jpeglib`, `libprotobuf`,
`quickhull`, `cryptopp`, plus system `SDL2`, `rt`, `openal`, `curl`, `ssl`, `z`, `crypto`,
and optionally the proprietary `steamdatagramlib`/`libphonon3d`.

What each becomes: tier libs → `std`; `mathlib` → `glam`; `bzip2`/`z` → `bzip2`/`flate2`;
`jpeglib` → `image`; `cryptopp`/`ssl`/`crypto` → `ring`/`rustls`; `curl` → `reqwest`;
`libprotobuf` → `prost`; `quickhull` → a crate; `vgui_controls`/`matsys_controls` →
deleted with `egui`; `bitmap`/`vtf` → `src/materials/`.

Notable: **`engine` links `SDL2` and `openal` directly**, not only through `appframework`
— so the audio backend decision is genuinely independent of the windowing one, and
removing SDL2 touches this module in two unrelated places.

## 10. Sequencing

Done: `filesystem`, `materialsystem` stages 1-4, and then — out of the order below, and
deliberately — `host/` + `world/` geometry ahead of `console/`. The reason: `console/`'s
value is that everything registers cvars, and nothing did yet. Two cvars were actually
needed (`map`, `fps_max`), and both are reachable as `+map`/`+fps_max` command-line
arguments, which is how Source spells them anyway. Porting a cvar registry to serve two
readers would have been scaffolding; it is worth doing when the third and fourth
subsystems want it.

**§10.3's "first point at which something recognizable appears on screen" has been
reached.** Remaining, in dependency order:

1. **Input** (§7.3's remainder) — the largest gap in `window/`, and the thing that makes
   the placeholder camera in `rustdocs/ENGINE.md` unnecessary. Needs the `egui`
   precedence decision in §6. **Planned in `portdocs/ENGINE_INPUT.md`**, which lands it as
   its own module: stages 1-2 (keyboard, mouse, mouse look) need nothing that does not
   exist; bindings want `console/`, and controllers (`gilrs`) are deferred to stage 5.
2. **`console/`** (§7.4) — **stage 1 done.** Cvars, the command buffer, both
   tokenizers, dispatch, aliases, `exec` and `stuffcmds`, with `map`/`quit`/`restart`
   reached through a `CommandTarget`; `sp_a1_intro1` now boots through the shipped
   `cfg/valve.rc` rather than from a launcher branch. Stages 2-5 remain:
   bindings (which is `input/` stage 3), config persistence, the `egui` dialog, and the
   list commands. See `portdocs/ENGINE_CONSOLE.md` and `rustdocs/ENGINE.md`.
3. **`materialsystem` stage 5** (lightmaps) — no longer blocked; there is a `.bsp` to
   pack from, and `LightmappedGeneric` is what turns 62 of `sp_a1_intro1`'s 66 materials
   from checkerboard into content. **Highest visual return of anything on this list.**
4. **The rest of `world/`** (§7.14, §7.15) — visibility (every face is drawn every frame
   today), displacements, brush entities, static props — plus `trace/` (§7.17).
   - `render/` (§7.16) + `paint/` — as the consumer side of the `src/materials/` work,
     not as a separate port. `Engine::camera` and `World::draw` are its seed.
   - `audio/` (§7.18) — large but self-contained, no `winit`/`wgpu` entanglement, and the
     existing backend abstraction makes it substitutable. Can proceed in parallel with the
     above once `filesystem` lands.
   - `net/` (§7.7–7.9) + `client/` (§7.5) + `server/` (§7.6) — multiplayer/listen-server
     machinery. A single-player Portal 2 still needs a listen server, but this is the last
     of the core path.
   - `save/` (§7.21), `events/` (§7.10), `demo/` (§7.11) — as needed.

Each module above deserves its own portdoc once it's actually scheduled —
`portdocs/ENGINE_AUDIO.md`, `ENGINE_NET.md`, `ENGINE_HOST.md` — following
PORTING.md's per-module rule. This doc stays the index over them.

## 11. Open questions

1. **Which subsystems are ported vs. redesigned?** PORTING.md's polarity rule means
   nothing is a transliteration — but there's still a real split between subsystems whose
   *external behavior* is pinned (BSP/MDL parsing, wire format while game DLLs are C++,
   `.dem` if recordings must stay playable, trace semantics that gameplay depends on) and
   those that are free redesigns (audio, console, save format, game events). Decide per
   module, in its own portdoc, and say which one it is up front.
2. **Audio backend.** Keep OpenAL via a Rust binding, or go Rust-native (`cpal` +
   `rodio`/custom mixer)? PORTING.md picked Rust-native for windowing/rendering/UI but is
   silent on audio. The existing device abstraction (§7.18) makes this genuinely
   swappable, and ~72k lines of mixer/DSP is enough that the answer matters. Note the DSP
   stack (`snd_dsp.cpp`, 12,358) and the operator system (`snd_op_sys/`, 11,807) are
   *content-driven* — Portal 2's audio scripts target them, so they're closer to "pinned"
   than the backend is.
3. **`fps_max` / frame-pacing fidelity.** Is exactly reproducing `FilterTime` a
   requirement, or is equivalent-feeling pacing via `ControlFlow::WaitUntil` acceptable?
   Affects how mechanical the §6 port can be.
4. **Dedicated server.** `IDedicatedServerAPI`, `engine_ds`, `-DDEDICATED`. PORTING.md
   describes a "single-player-focused Portal 2 build," which points at *no* — but it also
   assumes the tree builds both when discussing wire-format flag days. **Resolve this
   explicitly**, because the answer determines how much `#ifdef DEDICATED` branching needs
   preserving across §7.5–7.7 and whether `cl_null.cpp`/`sys_linuxwind.cpp` matter.
5. **Steam integration.** `steam_api`, `SteamAPI_RestartAppIfNecessary` in
   `CModAppSystemGroup::Create`, the `steamdatagramlib` blob. The port inherits a
   closed-source C ABI dependency here regardless of language — PORTING.md's one
   acknowledged `dlopen`.
6. **Where does the `mod.rs` ownership graph actually cut?** §1 assumes subsystems can be
   owned as fields with `&mut` passed where needed. The C++ resolves this with ambient
   globals, so the real aliasing requirements are undocumented. Expect at least one
   subsystem pair (likely `client`↔`net` or `render`↔`world`) to need a deliberate
   answer — split borrows, an id/handle indirection, or interior mutability.

## 12. Notes for whoever picks this up

- **Graph coverage on this module is partial.** `check_index_coverage` reports
  `parse_partial` for most large engine files. Most consequentially,
  `sys_engine.cpp:264-686` is unparsed — that's *all* of `FilterTime` and `CEngine::Frame`,
  the two functions §6 hinges on. Also partial: `sys_dll2.cpp`, `sys_mainwind.cpp`,
  `host.cpp` (~58 flagged ranges), `cl_main.cpp`, `sys_getmodes.cpp`. Clean:
  `host_state.cpp`, `igame.h`. Everything quoted here was read from source directly —
  do the same, and treat `search_graph`/`trace_path` results in this module as
  under-reporting.
- Line numbers are from the tree at time of writing; re-verify before relying on them.
  File sizes and counts in §2 and §7 were re-verified against the current tree.
- Only POSIX paths are documented, per PORTING.md's "Supported platforms." `engine` is
  dense with `_X360`/`_PS3`/`WIN32` branches — skim for `#elif defined(POSIX)`/`LINUX`/
  `OSX` and unconditional code and disregard the rest.
