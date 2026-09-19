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
  src/materials/   the GPU device and frame boundary (wgpu), textures, materials, post
  src/engine/      the engine; window/, host/, world/, trace/, input/, console/ (egui)
  src/client/      the game client — the player, CUserCmd, movement, the view, exposure
  src/server/      the game server — the entity list, the class table, spawn
  src/studio/      studio models — .mdl/.vvd/.vtx into geometry, and animation
  src/cmdline.rs   CommandLine(), at the root because everything reads it
  src/math.rs      the parts of mathlib that are a convention, not arithmetic
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
builds use full LTO and one codegen unit; **debug builds optimise the dependencies**
(`[profile.dev.package."*"] opt-level = 3`) while leaving this crate untouched, because
almost all of a frame's CPU time is inside `wgpu` and a debug build has to be playable —
it takes `sp_a1_intro1` from 37 ms a frame to 6.6 ms and costs nothing at the debugger.

The CMake tree under `legacy/` is not part of this build and is not maintained — don't
invest in it and don't wire it back in. (`.github/workflows/kstrike-compile.yml` still
describes the old CMake build; it is `master`-gated and stale with respect to this
branch, where the top-level `CMakeLists.txt` has moved into `legacy/`.)

`cargo test` is 1,044 tests. What the binary has grown into, stage by stage, and
the standing census of what `sp_a1_intro1` draws — the numbers to re-measure
after a change to the draw path — are in `rustdocs/ENGINE.md`, **"What the
binary does, and what `sp_a1_intro1` draws"**.

To see it work you need a directory containing a mod directory with a `gameinfo.txt`:

```
cargo run --release -- -basedir /path/to/game -game portal2 -window +map sp_a1_intro1
cargo run -- -basedir /path/to/game -game portal2 -window -vmt tools/toolsblack
cargo run -- -basedir /path/to/game -game portal2 -window -vmt models/props/box_dropper
```

`-vmt` previews any ported shader on a pair of cubes, in whichever vertex
layout the `.vmt`'s shader declared — a model material is drawn under a synthetic ambient
cube and one point light, which is not a real lighting environment and does not pretend to
be.

**Controls.** WASD to walk, space to jump, left control to crouch, left shift to
walk slowly, mouse to look, **Escape to release the cursor**. `` ` `` opens and
closes the console, which releases the cursor while it is up; Tab and the arrows
cycle completions, and an empty entry cycles history instead. `+toggleconsole` on
the command line opens it at startup, which is how it can be inspected without
touching the keyboard.

**Console commands worth knowing.** `noclip` toggles a real `MOVETYPE_NOCLIP`, so
it has momentum (`sv_noclipaccelerate` is 5; set it to 0 for an instant stop, and
fly up by looking up, because Portal 2 binds no key to `+moveup`). `trace`
reports what is under and in front of the player, `tonemap` what the exposure is
doing, and `report_entities`/`ent_dump`/`ent_fire`/`dumpeventqueue` inspect the
entity list. `portal 1` and `portal 2` place a blue and an orange oval on
whatever you are looking at and `portal off` fizzles them — the portal gun minus
the gun and minus every placement rule, so nothing refuses a surface.
`r_portal_stencil_depth` is how many levels of portal-in-portal are drawn: 2 by
default, 0 for a flat oval with no view through, 10 at most.

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

Every module's own `rustdocs/<MODULE>.md` carries the detail: the API, the
invariants, and — under **"What has landed, and what each stage found"** — the
narrative of what was ported and what the measurements said. **Read the rustdoc
before calling into a module.** This table is the index.

| Module | State | Read |
|---|---|---|
| `src/launcher/` | **ported** — command line, single-instance lock, startup, mounts the filesystem, hands off to `engine::window::run` | `portdocs/LAUNCHER.md` |
| `src/filesystem/` | **ported** — `Vfs` over an ordered mount list, `gameinfo.txt`, KeyValues, VPK (v1/v2/headerless), the `.bsp` pak lump at the head. Async and `sv_pure` deferred; deflate unimplemented because all 64,428 shipped pak entries are stored | `rustdocs/FILESYSTEM.md`, `portdocs/FILESYSTEM.md` |
| `src/materials/` | **stages 1-6 of 8**, plus 9 shaders — `UnlitGeneric`, `LightmappedGeneric`, `WorldVertexTransition`, `VertexLitGeneric`, `Phong`, `Refract`, `PortalRefract` and its `$Stage 1`, `BufferClearObeyStencil` — and the **stencil**. Paint maps and GPU morph not started | `rustdocs/MATERIALS.md`, `portdocs/MATERIALSYSTEM.md` |
| `src/engine/` | **6 of 14 modules** — `window/`, `host/`, `world/` (geometry, lightmaps, terrain, light cache, brush entities, entity models, portals, **visibility**, the **recursive portal view**), `trace/` (4 of 5, plus the portal carve, the far-side trace and the transition ramp), `input/` (4 of 5), `console/` (complete). No skybox, dynamic lights or simulation | `rustdocs/ENGINE.md`, `portdocs/ENGINE.md` |
| `src/client/` | **stages 1-4 of 5**, plus the teleport and the portal funnel — input→command→movement→view, `CPortalGameMovement`'s walk and `AirMove`, `HandlePortalling`, the view, auto-exposure policy. Stage 5 needs `net/` | `rustdocs/CLIENT.md`, `portdocs/CLIENT.md` |
| `src/studio/` | **stages 1-5 of 6**, plus animation and `$includemodel`. No LOD selection, no `.phy`, **no skinning**, and **135 models pose outside the box their own sequences declare** — the external `.ani` blocks | `rustdocs/STUDIO.md`, `portdocs/STUDIO.md` |
| `src/server/` | **all five stages**, plus `prop_floor_button`, `prop_dynamic`, `prop_testchamber_door`, `logic_branch_listener`, `prop_portal` and the two areaportals — **48 classnames, 35,232 of the game's 60,925 entity blocks** | `rustdocs/SERVER.md`, `portdocs/SERVER.md` |
| everything else | **unported**, and lives in `legacy/` | — |

**What that adds up to, on `sp_a1_intro1`:** the boot path is continuous from
`main` to a lit, self-starting level you can walk around, interact with and die
in. World geometry, terrain, brush entities, static props and entity-placed
models all draw, lit the way the shipped game lights them and auto-exposed to
the map's own limits. The entity logic runs on a 64 Hz tick: doors and panels
move, triggers fire, a floor button presses when you stand on it, a chamber
door opens as you approach and shuts behind you, and a `trigger_hurt` can kill
you. Two portals draw as coloured ovals — **and they work, and you can see
through them**. **And only what you can see is drawn**: the areas, the PVS and
the frustum between them took the frame from 1.76 ms to 0.28 ms, which is also
what makes a portal's second camera affordable.

**It is not a runnable game**: no sound, no netcode, no weapon, no skybox, and
a door moves *through* the player rather than shoving one.
**The portals work, and you can see through them.** `portdocs/PORTAL.md` stages
3 and 4 landed the teleport — you walk into one oval and come out of the other,
rotated, with your velocity rotated and clamped and your view turned with you;
measured over the nine pairs the shipped maps form, a player hull walked through
six. `portdocs/PORTAL_RENDER.md` then landed the **picture**: an oval is a hole
with the room behind its partner in it, two levels deep by default and up to ten
under `r_portal_stencil_depth`. Stage 5 then landed the *animation*: an oval opens
on `$PortalOpenAmount` and fades its noise out on `$PortalStatic`, each on its own
clock, restarted when a portal is switched on or moved. What is still missing is the
*warp* — a portal's surface does not refract what is behind it — which is
`PortalRefract`'s `$Stage 0`.

**Visibility has landed** — `portdocs/ENGINE_WORLD_VIS.md`. `mod_vis.cpp`,
`r_areaportal.cpp`, the areaportal half of `cmodel.cpp` and
`R_RecursiveWorldNode`'s pruning are `src/engine/world/vis.rs`; `func_areaportal`
and `func_areaportalwindow` drive it from the entity list;
`OcclusionSystem.cpp` (2,999 lines) is deleted outright because Portal 2 places
no `func_occluder` at all. Across the 103 shipped spawns the PVS leaves **6.9%
of world faces** standing. The finding that would have cost the game its
terrain: **`LUMP_LEAFFACES` names none of the 1,181 displacement faces**, so a
displacement's leaf list has to be rebuilt from its bounds the way the shipped
loader builds `mleaf_t::dispListStart`.

**Frame cost is measurable and has been measured**, by `engine::world::bench`
and `engine::exposure` (both depot-gated, `--ignored`). `sp_a1_intro1` records a
whole frame in **1.86 ms** release / 6.6 ms debug. **Reach for the benchmark
before and after any change to the draw path** — the running game cannot be
profiled from outside, because macOS stops delivering redraws to an occluded
window. Run the six sub-benchmarks *individually*: back to back they share
thermal state and read 2-3x high. Numbers and history:
`rustdocs/ENGINE.md`, "Frame cost, measured".

### What to do next

**`portdocs/SERVER.md` is finished** — all five stages — so the game layer's next
steps are individual classes and subsystems rather than a staged plan. `client/`
stage 5 and everything below it needs `net/`, which is a long way from here.
**With `portdocs/PORTAL.md` finished too, no staged plan is live**: everything below is
a discrete piece of work, not a stage of one.

**`portdocs/PORTAL.md` is finished** — **all five of its stages**: the blended pass, the
class drawn, **the hole**, **the teleport**, and the polish. The carve needed no
polyhedron library, exactly as predicted — `mathlib/polyhedron.cpp` (3,895 lines) and `staticcollisionpolyhedroncache.cpp` (586) are deleted outright — and it needed
one correction the portdoc did not foresee, which is that an *empty* carved piece has to be
detected rather than left to the clip loop, because a swept box expands every plane and
turns one into a solid slab across the hole. Stage 4 needed three more, all in
`portdocs/PORTAL.md` §5.1: the far side is not what holds the player up (the wall *below*
the hole is a ledge and a swept AABB is held by any ledge it overlaps), the remote trace's
window is one or two ticks rather than the whole approach, and
`CalculateExtentShift`'s comment contradicts its own arithmetic.

**Stage 5, the polish, landed last** — the transition ramp, `$PortalOpenAmount`'s open
animation, `IsFloorPortal`'s special cases and `PunchAllPenetratingPlayers` — in four
places: a fifth carved set (`Ramp`) that `Trace::portal_ramp` reports and
`hit_portal_ramp` turns into standable ground, `$PortalStatic` on its own clock beside
`$PortalOpenAmount` in `engine/world/portals.rs`, `CPortalGameMovement::AirMove`'s
funnel in `client/movement.rs` (which is where `IsFloorPortal`'s reachable cases are —
`CPortal_Base2D::Touch` early-returns for players, so the floor-to-floor teleport cases
are unreachable), and a deferred punch queue on `Context` that
`Server::flush_portal_punches` drains. The stage's honest measurement, recorded in
§10: **0 of the 9 shipped portal pairs is steep enough to reach the transition ramp**
(7 are flat, 2 force a crouch), so the ramp is implemented against the reference and
unit-tested but cannot be exercised by shipped content until the portal gun exists.

**The recursive view is done** — `portdocs/PORTAL_RENDER.md`, all four of its stages. The
stencil landed in `materials/` (`RenderState::stencil`, `write_color`,
`Pass::set_stencil`, stencil ops on every depth attachment), `Pass::set_camera` made the
view a mid-pass parameter, `vis::ViewPoint` measures a sub-scene from the exit portal's
corners, and `engine/world/portalview.rs` is the four-step loop with an oblique near
plane. **The whole recursion is one render pass** — no render target per level — and each
level costs one more world draw: 0.27 ms on `sp_a1_intro1`, where the same draw before
visibility landed was 1.81. That is why `portdocs/ENGINE_WORLD_VIS.md` went first.

What the portal path still does not draw is `PortalRefract`'s **`$Stage 0`** — the
opening warp, deferred for a concrete reason rather than for scope: it samples a copy of
the scene taken part way through the frame, which cannot happen inside a `wgpu` render
pass — and `c_portalghostrenderable.cpp` (980) for the half of an entity that sticks out
of the other portal, which nothing but the player passes through and the player is not
drawn.

- **`CPhysicsPushedEntities` — a door that shoves the player.** `trace/` stage 4
  is no longer in the way, so this is unblocked for the first time:
  `physics_main.cpp:130-1130`, ~1,000 lines of speculative push, blocker
  enumeration and rollback, and `EntityCore::local_time` is already the field
  its answer goes in. The condition is the first puzzle that cannot be solved
  without standing on something that moves.
- **The local/abs transform pair on `EntityCore`**, which is smaller than a stage and
  unblocks two things at once: `SetParent`/`ClearParent`/`SetParentAttachment*` —
  **1,103 of the 1,371 inputs the depot test reports as unhandled** — and parented
  movers, which currently move in world space where Valve moves them in the parent's
  frame (174 of the game's 1,164 movers name a parent). `prop_dynamic` raised the
  stakes: **2,355 of the game's 8,462 props name a `parentname`**, and 177 of the
  unhandled inputs are now theirs. The attachment forms also want
  `LookupAttachment` on a studio model, which would be `server/`'s first dependency on
  `studio/`.
- **Skinning in `src/studio/`**, which `$includemodel` promoted to the largest gap in
  the model path and which now gates the second largest. The per-bone draw split is
  exact only when every vertex answers to one bone, and **74 of the 591 readable models
  the game's props name share a vertex between two — 290 entities wear one**, drawn in
  their bind pose. Seven of the 74 are on `sp_a1_intro1`
  (`models/container_ride/finedebris_part*`), so it is visible on the default map. It
  gates **external `.ani` animation blocks**, because every `$includemodel` host but
  the two panel arms — eggbot, ballbot, both Chells, the s8 player, the Wheatley boss
  and the personality sphere — is a model this cannot pose anyway, so reading `.ani`
  before skinning buys nothing. **There is now a number on what the missing `.ani`
  costs**: 135 models pose outside the box their own sequences declare, the worst by
  23,029 units, and since `studiomdl` computes that box from the animated geometry the
  pose is what is wrong. `every_shipped_studio_model_parses` prints the list. `vvd::Vertex` grows a `bones` field, `vtx` stops
  discarding `StripHeader_t`'s bone plumbing, and the bone matrices move to the GPU.
- **`world/`'s 3D skybox** — now that terrain draws, the last structural reason
  `sp_a1_intro1` does not look like the shipped game. A second camera over a second set of
  geometry, plus `sky_camera`'s scale. **Visibility made it cheaper and the recursive view
  built the seam**: `Map_VisSetup` takes an *array* of origins and ORs their PVS rows
  together precisely so that a skybox camera and the world share one visible set, and
  `vis::ViewPoint` / `Visibility::mark_view` is now that array. A skybox camera is its
  second consumer.
- **Bloom**, now that there is a scene target and a presenting pass to put it between.
  `Generate8BitBloomTexture`'s downsample/blur chain plus `BloomAdd`, three quarter-size
  render targets. It is the most visible thing still missing from the post chain and
  Portal 2 leans on it. `portdocs/CLIENT_TONEMAP.md` §7 ranks the rest.
- **`LightmappedGeneric`'s second vertex layout**, if `sp_a3_*` matters — a world-space
  normal on `WorldVertex` is what `$seamless_scale` (553 displacement faces) and `$envmap`
  both want, and it is the open question `MATERIALSYSTEM.md` §10 has been holding.

### Known warts, and what triggers fixing them

Deliberate small compromises, recorded so nobody has to rediscover them and nobody
"fixes" one prematurely. Each names the condition that makes it worth doing, and each is
also commented at the site.

**Three earlier warts are now resolved** and their full records — including the
`CCommandLine::ParmValue` divergence one of them fixed — moved to
`rustdocs/CLIENT.md`, "Warts that were resolved": the view angles and free-fly
camera leaving `engine/input/view.rs` for `client/`; `noclip` moving from the
client to the server, where the move type lives; and `CommandLine` moving to
`src/cmdline.rs` once `console/` became its third consumer.

- **Every static prop is lit by the light cache, including the 76% that then throw the
  answer away.** `World::load` runs `Props::light` before `PropModels::load`, because the
  models an instance names are not read until the second — so the `bStaticLighting`
  answer, which decides whether a prop uses the light cache at all, is not known when the
  cache is asked. Valve skips the work instead: `ComputeStaticLightingForCacheEntry` runs
  only when `LIGHTCACHEFLAGS_STATIC` is asked for. Measured: **1.4 s for all 56,955 props
  in the game**, about 13 ms a map, of which roughly three quarters is discarded — against
  0.25 s for `sp_a1_intro1`'s collision model alone. **Fix it when a map's load time is
  actually a problem**, by moving `Props::light` after `PropModels::load` and passing it
  the per-instance answer; the seam already exists as `PropModels::light_ranges`.

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

`portdocs/CLIENT_TONEMAP.md` is the exception to "written before the port": the tone
mapper was not on `portdocs/CLIENT.md`'s five-stage plan, so its portdoc was written
afterwards and says so at the top. Read it as the analysis that justifies the shape rather
than as a plan to follow.

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
