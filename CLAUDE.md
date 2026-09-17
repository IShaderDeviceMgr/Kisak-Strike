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

There is a unit test suite (`cargo test`, 938 tests), and the binary now **runs, loads a
map, lets you fly around it and has a working developer console**: it mounts the game
filesystem, opens a window, runs an
engine frame loop with a real host state machine, **reads the shipped `cfg/config_default.cfg` and
`cfg/valve.rc` and boots through them**, reads a Portal 2 `.bsp`, packs its baked lightmaps into an atlas,
draws its world geometry **lit**, moves the view with WASD and the mouse, and drops an
`egui` console over the top of it on `` ` `` — scrollback, history, tab completion, the
list commands (`cvarlist`, `help`, `find`, `differences`, `toggle`, `incrementvar`), the
entity commands (`report_entities`, `ent_dump`, `ent_fire`, `dumpeventqueue`), and
every cvar and command the port has registered. **There is now a player who walks.** A real one, in
`MOVETYPE_WALK`, built from a `CUserCmd` and moved by `FullWalkMove`: it falls under
gravity, stands on the floor, is stopped by walls and slides along them, climbs stairs
under `sv_stepsize`, jumps 45 units, and crouches under things it does not fit past.
`noclip` still flies. **The map's entity logic now runs**: entities spawn, fire outputs
at each other through one event queue and think on a fixed 64 Hz server tick, so a map
bootstraps itself the way the shipped game does — **and the brush entities move**, so
doors open and shut, panels slide, buttons press in and come back out and fans spin up.
**And the map notices you.** Triggers fire when you walk into them, filters
decide who counts, `trigger_push` blows you across a room, `trigger_teleport`
and `point_teleport` move you, and **doors are walls** — brush entities are in
the player's clip chain now, so a shut door stops you and a trigger does not.
**Standing on a floor button presses it** — and **you can see it happen**: the
button's model draws and its plate animates down under you, which is the first
studio *animation* and the first entity-placed model in the port.
**And the map can kill you.** The player has health, a `trigger_hurt` takes it
away at the rate the map asked for, and at zero the body drops, the camera
falls to fourteen units off the floor, and three seconds later the level starts
again — which is what single-player Portal 2 does, minus the save.
**And the map is furnished**: `prop_dynamic` is 8,462 entities across 105 of
the game's 106 maps — the second commonest classname in Portal 2 — so the
signs, pipes, panel arms and machinery a chamber is built out of now draw,
and they *animate*, on the sequence the map names and for as long as the map
says.
**And the chamber doors open, and shut behind you.**
`prop_testchamber_door` is 138 entities
across 71 maps, two of them on `sp_a1_intro1`: the big round door draws,
its two rings spin and then its two halves part, and 130 of the 138 are
driven by a chain — `trigger_once` → `func_instance_io_proxy` →
`logic_relay` → the door — that is now ported end to end, so walking into
a chamber opens its door. The way *back* out runs through
`logic_branch_listener` (158 across 46 maps), an AND gate over a pair of
`logic_branch`es that says "the map wants this shut" and "the player is
not in the doorway" — so the door waits for you to be clear of it and
then closes.
It is **still not a runnable game** — no sound, no netcode, no weapon, and
a door moves *through* the player rather than shoving it (a chamber door is
walked through for the same reason) — but the boot path is
continuous from `main` to a rendered, lit, self-starting level you can walk
around, interact with and die in.

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

**`sp_a1_intro1` draws lit**: 5,523 of 5,638 world faces, 73 of its 76 materials
resolving, 4,857 surfaces with real baked lighting over 13 atlas pages, and **1,080 static
props from 136 models** on top of that. **Its terrain draws too** — 11 displacements,
1,408 triangles — which is the last of the big absences in the level shell. The `maps/<map>/…` cubemap patches that used to draw as the magenta error checkerboard
now resolve, because the `.bsp`'s embedded pak lump is mounted (`portdocs/STUDIO.md`
stage 4); 3 of its 76 world materials still do not. Two name shaders this port has not
ported — `SolidEnergy` (the fizzler field) and `Black` (`tools/toolsblack_noportal_skybox`)
— and the third is a **missing file**: `models/props_trainstation/trainstation_clock_glass001`,
which exists in none of `portal2`, `portal2_dlc1` or `portal2_dlc2`, so the map ships a
dangling reference. (An earlier draft of this file named `Refract` as the third. That was
wrong: nothing in the map's world materials names it. `Refract` *is* in the map, on three
static props, and it is ported now — so the container's observation window and its two
light covers refract instead of drawing as checkerboards, and that window is why the map
takes the second, frame-buffer-copy pass.) **26 of its 78 brush
entities draw too**, on top of the world: doors, panels and fizzlers, 148 faces and 308
triangles, each under the placement its entity gives it — and since `prop_dynamic`
landed, **so do 90 entity-placed models from 52 more `.mdl`s**, which is the furniture
the map is made of rather than the shell it sits in.
**Every model in it is lit the way the shipped game lights it**, now that the light
cache has its local half: **39 of the map's 43 world lights** reach props through
`AddStaticLighting`, and the 246 props whose materials are bumped or phong are lit per
pixel by them instead of by `vrad`'s per-vertex bake — which is what gives a phong prop a
specular highlight at all.
**The scene is auto-exposed**: it is drawn into an
offscreen target, a compute pass bins its pixels by luminance, and a port of
`CTonemapSystem` picks the scalar the lit shaders multiply by — `tonemap` in the console
reports what it is doing. **The map's own exposure limits apply too**, now that entities run: 105 of the game's
106 maps place an `env_tonemap_controller`, and `sp_a1_intro1` asks for — and gets — a
ceiling of 1.5 where the cvar default is 2.
The view is the **player's eye**: WASD to walk, space to jump, left control to
crouch, left shift to walk slowly, mouse to look, **Escape to release the cursor**.
`noclip` toggles a real `MOVETYPE_NOCLIP` rather than a camera pretending to be one — so
**it has momentum**, because `sv_noclipaccelerate` is 5 and not 0; set
`sv_noclipaccelerate 0` for an instant stop, and fly up by looking up, because Portal 2
binds no key to `+moveup`. `trace` in the console reports what is under and in front of
the player.

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
  search paths, KeyValues reader, case-folded directory mounts, VPK reading
  (v1/v2/headerless, multi-archive, embedded chunks), and the `.bsp`'s embedded pak lump
  (`PakMount`, a stored-entry ZIP reader) mounted **at the head** for a map's lifetime by
  `Vfs::set_map_pak` — `AddSearchPath( <map>.bsp, "GAME", PATH_ADD_TO_HEAD )`. Deflate is
  not implemented and that is a measurement: all 64,428 pak entries in Portal 2's 106 maps
  are stored. Async and `sv_pure` are deferred. **API: `rustdocs/FILESYSTEM.md`** (read this before calling it);
  porting decisions and the C++ inventory: `portdocs/FILESYSTEM.md`.
- **`src/materials/` — stages 1-6 of 8 ported, plus the first two shaders of §7.8's remainder.** `Renderer` owns `wgpu`'s
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
  **Stage 6 is `VertexLitGeneric`, and it is done** — the shader every model wears, and
  the largest in the shipped game: 1,135 of the mounted game's 3,555 materials name it,
  1,012 of them under `materials/models/`. Landed as `ShaderKind::VertexLitGeneric` with
  `shaders/vertexlitgeneric.wgsl`, `mesh::ModelVertex`, `uniforms::{Light, ModelLighting}`
  — the ambient cube and up to four local lights that `engine/lightcache.cpp`'s
  `LightcacheGetStatic`/`Mod_LeafAmbientColorAtPos` fill (**there is no
  `R_StudioSetupLighting` in this tree**; earlier drafts of this file named one) — a
  second shape for bind group 3, and the cubemap half of the texture path. **Every one of
  those materials loads and builds a pipeline against the real game** — 818 here in 14
  pipelines and the other 317 in `Phong`'s 7 — which answers §10's "how many variants
  survive" with a measurement.
  **`Refract` and `Phong` are the first two of §7.8's remaining set and have landed**
  (below); the rest of it and stages 7-8 (paint maps, GPU morph) are not started.

  Six things about it that a reader will otherwise rediscover the hard way:
  **a `.vmt` naming `VertexLitGeneric` does not always reach it** — `WantsPhongShader`
  sends 317 of the 1,135 to `DrawPhong_DX9`, which **is ported now** (below), so the
  resolution from a `.vmt` to a shader is `ShaderKind::resolve` and not
  `ShaderKind::from_name`; **group 3 is now "where this
  shader's lighting comes from"**, a lightmap page for brushes or a `ModelLighting` block
  for models, so `reads_lightmap()` became `lighting_binding()`; **the unbumped path
  lights per vertex and the bumped path per pixel**, because they are two files in the
  original and unifying them would silently reshade every prop; **a bumped model gets no
  baked vertex light at all**, which is Valve's asymmetry and not an omission; and
  **`$envmap "env_cubemap"` names no file** — it is a request for the render instance's
  local cubemap, so those 78 materials reflect a black cube; the pak lump is mounted now,
  but the per-instance cubemap lookup that would resolve them is not written; and
  **`$envmaptint` is gamma-decoded on the CPU, with `GammaToLinearFullRange` and not the
  table** — `pow( x, 2.2 )` flat, where every other tint in the module takes
  `GammaToLinear`'s 256-entry table with its `>= 0.95` clamp and its `> 1` passthrough.
  That is `SetEnvMapTintPixelShaderDynamicStateGammaToLinear` and it is
  `VertexLitGeneric`'s alone: `LightmappedGeneric` calls the *other* overload and sends
  its tint through in gamma space, which is Valve's asymmetry and not a bug to fix. It
  matters because **every one of the 57 materials in the game with a resolvable `$envmap`
  writes a dark tint** — `[0.05 0.05 0.05]` on twenty of them, which is 0.0014 linear, a
  factor of 36, and `[0.01 0.01 0.01]` on five more, which is 4.0e-5 and means "no
  reflection" rather than "a faint one". Skipping the decode does not error; it makes
  every reflective prop in the game shine. 57 of the 106 maps place a static prop wearing
  one. **Two CS:GO-shaped defaults were found here and reversed**: `bHalfLambert` is
  hard-coded `false` in the CS:GO tree over a commented-out read of the material flag, and
  `SoftenCosineTerm` (`// For CS:GO`) changes the diffuse falloff of every lit surface.
  Portal 2 has neither. (`Phong` brought a third of the same kind — see below.)

  **`post.rs` and `histogram.rs` landed with the tone mapper, and they close half of §10's
  HDR question.** `PostProcess` is `_rt_FullFrameFB` plus the final pass of
  `DoEnginePostProcessing`: the scene is drawn into an offscreen target in the back
  buffer's exact format, measured, and blitted forward. `Histogram` is a compute dispatch
  that bins a frame's pixels by luminance and reads the counts back without ever blocking
  — replacing sixteen occlusion queries issued one per frame. **No float render target was
  needed**, because Valve applies the exposure scalar in `FinalOutput` before the sRGB
  write in *both* HDR modes, so this port's 8-bit sRGB frame buffer already is
  `HDR_TYPE_INTEGER`'s; what was needed was for the scene to stop going straight to the
  back buffer, since a swap-chain image cannot be sampled. Two rules there produce a
  plausible wrong answer rather than an error: **`RenderContext::set_exposure` takes effect
  on the next pass *opened*, not one already open**, and **`PostProcess::measurement` must
  be called once a frame unconditionally** — it arms the previous frame's readback as well
  as returning it, so a frame that returns early strands a staging buffer and the exposure
  silently stops adapting for ever.

  **`Refract` has landed too, and it is the first of §7.8's *remaining* shader set** — the
  screen-space refraction shader, which is what every pane of glass in Portal 2 wears. 37
  materials name it, 29 of them on models, and **all 37 load and build a pipeline** (7 of
  them). Landed as `ShaderKind::Refract`, `shaders/refract.wgsl`, `shader::RefractUniforms`
  and a **third shape for bind group 3** — `ContextBinding::FrameBufferCopy`, which is why
  `ShaderKind::lighting_binding` is now `context_binding`: group 3 was "where this shader's
  lighting comes from" and is now "whichever piece of render-context state this shader
  reads", because a copy of the frame buffer belongs in the same slot for the same reason
  a lightmap page does.

  **It is the first shader in the port that reads the frame it is being drawn into**, and
  that cost a structural change rather than a shader. `RenderContext::update_refract_texture`
  is `UpdateRefractTexture` plus `SetFrameBufferCopyTexture` — a `copy_texture_to_texture`
  off the scene target and the group-3 bind group over the copy — and because a render pass
  cannot sample its own attachment, `Engine::render` now runs **two passes**: the opaque
  scene, the copy, then `World::draw_refracting` under `Load::Keep`. That is Valve's own
  opaque-list / `UpdateRefractTexture` / translucent-list ordering, made explicit because
  `wgpu` enforces what D3D9 left undefined. 71 of the game's 106 maps need the second pass;
  the other 35 pay neither it nor the copy.

  Five things about it a reader will otherwise rediscover the hard way.
  **A `Refract` material's `$basetexture` is not a surface texture** — it is an alternative
  *image to warp*, bound to the sampler the frame-buffer copy would occupy, and
  `$localrefract` is what says a material has one; six of the 37 take that branch and need
  no copy of the frame at all, including five of the six in `sp_a1_intro1`. **It has no
  lighting**, so `lighting()` is `None` and group 3 is free for the frame copy. **Its
  blending comes from the `$normalmap` and only when there is no `$envmap`** — and all 29
  `$model 1` materials have an envmap, so every piece of glass in the game draws *opaque*,
  showing the copy it sampled rather than blending with the frame it is writing.
  **`$bluramount` is declared an integer and content writes fractions into it** (sixteen
  materials say `".3"`), so `GetIntValue()`'s truncation is what decides whether the blur
  runs — 26 of the 37 get none. And **the "aspect fixup" is an integer division**:
  `float( nHeight / nWidth )` with both operands `int`, so `glass/refract_light_color`
  being 128x512 makes it **4** for five glass materials, and a wider-than-tall source
  would make it 0.
  **One Valve bug is reproduced deliberately**: `refract_ps2x.fxc:338` reflects the
  environment map along a *tangent-space* eye vector beside a world-space normal, which is
  what decides what every pane of glass in the game reflects; the line is marked in the
  WGSL.
  Four of the pixel shader's static axes are pinned off **on a content measurement rather
  than a capability** — `SECONDARY_NORMAL`, `MASKED`, `MAGNIFY` and `COLORMODULATE`, which
  no Portal 2 material sets — and the first of those is broken in the original anyway
  (it binds sampler 1 and samples sampler 3).

  **`Refract` also brought the census that says a shader is finished**:
  `materials::material::tests::every_shipped_material_of_a_ported_shader_builds_a_pipeline`
  is depot-gated (`KISAK_GAME_DIR`), walks every `.vmt` in the mounted game, loads it
  through the real `MaterialCache` and asks the real `PipelineCache` for a pipeline with
  `on_uncaptured_error` latching any validation failure. Running it prints the whole
  picture: **2,947 of the mounted game's 3,555 materials draw with a real shader, in 57
  pipelines** — 956 `UnlitGeneric`, 818 `VertexLitGeneric`, 801 `LightmappedGeneric`,
  317 `Phong`, 37 `Refract`, 18 `WorldVertexTransition`, and 608 on the error material.
  That is the standing answer to §10's "how many variants actually survive".

  **`Phong` has landed too, and it is the second of §7.8's *remaining* set** — the model
  shader with a specular highlight, and **the first shader in the port that no `.vmt`
  names**. There is no `SHADER( Phong )` in `stdshaders/` at all:
  `phong_dx9_helper.cpp` is reached only from `DrawVertexLitGeneric_DX9`, which hands the
  material over when `WantsPhongShader` says so — so `ShaderKind::from_name( "Phong" )`
  is `None` and the redirect lives in a new **`ShaderKind::resolve( vmt )`**, which is
  what `Material::new` calls and what anything holding a `.vmt` should call.
  **317 materials reach it**, 301 under `materials/models/`, all 317 build a pipeline,
  and the whole set needs 7. **104 of the game's 106 maps place a static prop wearing
  one**, including 11 in `sp_a1_intro1` and 20 in `sp_a3_portal_intro` — so unlike the
  `$envmaptint` fix this one is visible on the default map.

  Six things about it that read as bugs until you check the reference.
  **Half-Lambert is on by default and `$halflambert` does nothing** — the switch is
  `$phongdisablehalflambert`, and this is the **third** CS:GO-shaped default of the kind
  above: `bPhongHalfLambert` is hard-coded `false` over a commented-out read of the
  parameter, whose own declaration says half-Lambert "has always been forced on in
  phong". The content settles it: 26 of the 317 write the parameter and **20 write `1`**,
  a no-op against an off-by-default.
  **`$envmaptint` is *not* gamma-decoded here**, which makes three shaders with three
  answers — `VertexLitGeneric` uses `GammaToLinearFullRange`, `Refract` the 256-entry
  table, `Phong` and `LightmappedGeneric` none — so making them agree would be a
  divergence and a factor of 36.
  **A Phong model gets no baked vertex light**, because it is always a per-pixel shader
  (`bStaticLight = false`); the CS:GO `STATICLIGHT3` work exists because of it. That same
  fact is what `DrawModelExStaticProp` calls `bStaticLighting`, and **`world/light.rs`
  now acts on it**: a phong or bumped prop's `.vhv` is never opened and the light cache
  is its whole lighting, ambient cube *and* local lights, so the specular term has
  something to work with. Until that landed a Phong prop had no highlight at all,
  because the term needs a light.
  **The envmap mask is base alpha whether the material asked or not**, so
  `$basealphaenvmapmask` is inert and `$envmapmask` is not even sampled.
  **`$phongexponent` unset is a sentinel**, not a default: zero means "read the exponent
  from `$phongexponenttexture`'s red channel", remapped onto 1..150, and 71 of the 317
  take that path.
  And **one register is the detail blend factor *or* `$phongalbedoboost`** — Valve's own
  name for it is `flBlendFactorOrPhongAlbedoBoost`.
  Pinned off on *content* rather than capability: wrinkle maps, `$decaltexture`,
  `$tintmasktexture` and `$rimmask`, none of which any Portal 2 material sets.
  **Also found here and deliberately left alone**: the pixel shaders' modulation colour
  should be *linear* for the two model shaders and gamma for the world one
  (`shaderapidx8.cpp:8664`), and `modulation_color` returns gamma for all of them. It
  moves every tinted model, so it wants its own change; 48 materials set
  `$color`/`$color2`, 8 of them on a model shader.

  §10's "how are variants expressed" question is **closed**: six shaders in, none
  needed a source-text variant — `VertexLitGeneric` merges two Valve *files* into one
  module with a uniform branch, `WorldVertexTransition` is a second *name* on
  `LightmappedGeneric`'s module rather than a variant of it, and `Phong`'s nineteen
  static axes produced none at all — so the prelude is prepended by string concatenation
  and `naga_oil`, `override` constants and a build-time preprocessor are all declined on
  evidence. **`Phong` did add one degree of freedom without changing the mechanism**: it
  shares `VertexLitGeneric`'s group 3, so the `@group(3)` declaration and the two
  lighting terms both shaders compute identically moved into
  `shaders/modellighting.wgsl`, prepended for exactly the shaders whose
  `ContextBinding` is `ModelLighting`. The *diffuse* term deliberately did not move —
  `DiffuseTerm` and `CosineTermInternal` are two different functions in the original.
  **`LightmappedGeneric` was expected to force the second vertex layout and did not**
  (bumped and unbumped share one, because the bumped diffuse path never leaves tangent
  space); `VertexLitGeneric` genuinely has two in Valve's engine and this port still keeps
  one, because the tangent is in the `.vvd` either way — and `Phong` asks for the tangent
  unconditionally, so for its 317 one layout is not even a simplification.
- **`src/engine/` — 6 of 14 modules ported: `window/`, `host/`, `world/`'s geometry,
  lightmaps, terrain and light cache, `trace/` (stages 1-4 of 5), `input/` (stages 1-4
  of 5), and `console/` (all five stages, complete)**
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
  **`World::draw` is now two calls and not one**: `draw` records the opaque scene and
  `draw_refracting` records the geometry whose material samples a copy of it, with
  `RenderContext::update_refract_texture` between them and the first pass *ended* — a
  render pass cannot read its own attachment. `needs_frame_buffer_copy` says whether the
  second pass is needed at all, and 71 of the game's 106 maps say yes. The split is per
  *batch* rather than per prop, which diverges from Valve on purpose: 60 of the 66 models
  in the game that wear a refracting material also wear an opaque one, and without a
  translucency sort the opaque half belongs in the opaque pass.
  **Brush entities draw**, which closed the one place this port had collision ahead of
  rendering: model 0 is the world and models 1.. are the doors, panels and platforms, each
  built by the *same* face-grouping and lightmap-packing path and drawn with the entity's
  matrix in place of the identity (`R_DrawBrushModel`). Three measurements made that
  small: a brush model's faces are in its own frame like a static prop's (4,088 of 4,309
  displaced models match their model box exactly, **none** matches it offset); the
  existing `SURF_*` filter is the whole of the visibility question, so every `trigger_*`
  class drops out with no per-classname rule (11,635 brush entities in the game, 2,697
  with a drawable face — `trigger_portal_cleanser` keeps its, because a fizzler really is
  visible); and where a brush model *is* comes from the entity, never from
  `Model::origin`. **The transform is `BrushModel::model_to_world` and is not cached**, so
  what is drawn and what `trace_model` collides with cannot drift apart — and **the
  placement is live now**: `World::sync_brush_models` takes it from the game server once a
  frame, keyed by the `"*N"` model index, which is why doors open. Not honoured, and each
  measured rather than guessed: the translucent render modes (five entities in the game;
  they need a blended pass) and `renderamt`. `rendermode 10` **is** honoured, because it
  is the one mode `C_BaseEntity::ShouldDraw` refuses — 94 entities; and `StartDisabled`
  **is** honoured for `func_brush`, which is the class whose `Spawn` reads it — 337 of
  the game's 2,502 start invisible and non-solid, where before `server/` stage 3 all of
  them drew.
  **Materials are resolved before the geometry**, because a surface's vertex layout comes
  from its shader and how wide a lightmap block it reserves comes from whether its
  material has a `$bumpmap`; neither is answerable from the `.bsp`. The **`winit` control-flow inversion is
  resolved**: `FilterTime` split into policy (`host::FrameClock`) and mechanism
  (`window`'s `ControlFlow::WaitUntil`), and neither half may sleep.
  `input/` is stages 1-4 of `portdocs/ENGINE_INPUT.md`'s five: `Button`'s flat dense
  space with Valve's shipped key names, an event queue **pushed between ticks and
  drained once per tick** inside `Engine::frame`, bindings, and UI precedence. **The
  movement layer that lived here as a placeholder is gone** — `input::view` is deleted and
  its contents are `src/client/`'s. It names no `winit` type, no `egui` type and no client
  type —
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
  `trace/` is stages 1-3 of `portdocs/ENGINE_TRACE.md`: `CM_BoxTrace` and everything under
  it — the recursive hull check, the brush clip, box brushes, the position test,
  `point_contents` and the leaf lookup — over six new collision lumps read by the
  *existing* `bsp.rs` rather than by a second reader, which is a duplication Valve only had
  because collision could not see `modelloader.cpp`'s allocations. **Stage 2 adds brush
  models** — `CM_TransformedBoxTrace`, which is the whole of `ClipRayToBSP`: the ray moves
  into the model's frame, the ordinary sweep runs against the model's *own* head node, and
  the normal turns back out. Doors, platforms and the moving parts of a test chamber are
  now solid, `world/` draws them, and since `server/` stage 3 **they move** — the
  placement is `BrushModel::set_placement`, written once a frame from the entity.
  Where a brush model *is* does not come from the model lump (`Model::origin`
  is "for sounds and lights, not a render transform") but from the entity that names it as
  `"*N"`, so `World::brush_models` resolves the entity lump at load — **placements, not
  policy**: triggers are carried too, because a trigger's brushes are `CONTENTS_SOLID` in
  the file and what makes them non-solid is `FSOLID_TRIGGER`, set by a game DLL that does
  not exist. Measured on the depot: **106 maps place 11,635 brush models, 5,115 of them
  rotated** — the rotated path is 44% of them and not a corner case — and `sp_a1_intro1`
  has 78. **Stage 3 is displacements, and terrain is now solid**: `CDispCollTree` and the
  parts of `builddisp.cpp` that turn a `ddispinfo_t` into geometry, plus the per-leaf
  displacement lists, `CM_TraceToDispList`, the box-versus-triangle position test and the
  stab. Three more lumps in the *same* `bsp.rs`, and a `Trace::disp_flags` carrying VBSP's
  per-triangle `DISPSURF_*` tags — a non-zero value is `IsDispSurface()`, and
  `disp_surf::WALKABLE` is VBSP's compile-time verdict rather than `CategorizePosition`'s
  runtime one. Measured: **1,181 displacements across 29 of Portal 2's 106 maps, all
  1,181 built**, 904 at power 2 / 202 at 3 / 75 at 4, over 14,190 leaf references;
  building `sp_a3_end`'s 201 costs 1-3 ms and a ground probe on it 0.2 µs.
  **`parry` was reconsidered here, as `ENGINE_TRACE.md` §5.5 said to, and declined** —
  of `CDispCollTree`'s 1,565 lines about 120 are the tree walk a `Qbvh` would replace and
  the rest is displacement semantics. Entities and props are stages 4-5.
  `spatialpartition.cpp` is not ported and will not be — `parry`'s `Qbvh` replaces it when
  entities land, and `rapier` replaces `vphysics/`; `ENGINE_TRACE.md` §5 is the full
  evaluation of where those two crates do and do not fit, and the world brush trace is one
  of the places they do not. Three rules there produce a plausible wrong answer rather than
  an error: **`Ray`'s start is the centre of the box and `Trace`'s is not** — 36 units
  apart for a player, so conflating them floats them a hull-height up; **`fraction` stops
  `DIST_EPSILON`, 1/32 unit, short of the surface on purpose**, and stair stepping, ground
  probes and `TryPlayerMove`'s clip-and-retry are all written around that gap; and **a
  leaf's `contents` describes its own volume, not the OR of its brush list**, so an empty
  leaf beside a wall has contents 0 — reading it the other way makes every position test in
  open air report `all_solid`. Stage 2 adds three more: **`trace` and `trace_model` are
  separate questions and neither includes the other**, so a door is invisible to a world
  trace and combining them is the caller's job until stage 4; **a brush model's `normal`
  comes back in world space and its `plane_dist` does not**, which is Valve's asymmetry and
  is pinned by a test so nobody "fixes" it; and **the swept box is not rotated into the
  model's frame**, so the obvious symmetry test — turn the model and the query together,
  expect the answer to turn — holds for a ray and not for a hull.
  Stage 3 adds four, and the first two are the ones that make terrain terrain:
  **every displacement test is one-sided** — a query travelling along the triangle's
  normal is rejected, so walk under a hillside and nothing stops you coming back out
  through it — and the normal is `(v2 - v0) × (v1 - v0)` over a base quad whose own is
  `(p3 - p0) × (p1 - p0)`, both the reverse of the obvious order and both pointing *out*
  of the solid; **a ray stops *on* a displacement and `DIST_EPSILON` short of a brush**,
  because the displacement ray path has no epsilon pullback (a hull sweep stops short of
  both); **a *point* inside terrain is reported as not solid**, because the box-versus-
  triangle test is what decides "inside" and the stab, which is all that is left for a
  point, fires along the one direction nothing can be hit in — Valve's, pinned by a test;
  and **two switches hidden in `ddispinfo_t::minTess` take a patch out of half the
  queries**, which 51 of Portal 2's use for hulls and 44 for rays.
  **The module's one deliberate divergence is also stage 3's:** Valve writes
  `dispFlags` in two places and clears it in none, so a brush that beats a displacement
  keeps the displacement's flags and `IsDispSurface()` calls a wall terrain — 45 of 2,362
  depot traces. This port clears them where `m_bDispHit` is cleared; `rustdocs/ENGINE.md`
  gotcha 17 names the two lines to delete to get Valve's behaviour back.
  **Stage 4 is the clip chain, and it landed with `server/` stage 4**:
  `Tracer::trace` is `CEngineTrace::TraceRay` now — the world, then every brush
  model a `Tracer::with_entities` was handed, nearest wins, fractions rescaled
  onto the original ray — so a shut door is a wall and, because a trigger is
  `FSOLID_NOT_SOLID`, a trigger is not. Two things the plan asked for turned out
  not to be needed: **the trace filter**, because the candidates arrive as a list
  the caller assembles and `ITraceFilter`'s decision has therefore already been
  made one step earlier; and **the broadphase**, because a map has a few hundred
  brush entities (78 on `sp_a1_intro1`) and each is rejected by the
  bounding-box test at the top of its own BSP descent. What *is* the whole
  difficulty is **which entities are in the chain**: the game's 11,635 brush
  entities include 2,383 `func_portal_bumper`s you walk straight through and
  this port has classes for 6,302 of them, so `World::clip_models` requires
  `PlacedBrushModel::owned` **and** `solid` and a model the game has not
  answered for is left *out* rather than assumed in. Defaulting the other way
  fills every chamber with invisible walls, silently.
  Not implemented: simulation, visibility, the skybox, dynamic lights and
  lightstyle animation. **The static world lights are** — `world/light.rs`,
  below. Brush entities are solid, drawn, **moved and collided
  with**. **Displacements are solid and drawn** —
  `world/disp/` has landed, below.
  **`world/disp/` is the rendering half of `trace/` stage 3's lumps, and terrain now
  draws** (`portdocs/ENGINE_WORLD_DISP.md`, `rustdocs/ENGINE.md`). ~9,100 lines of C++
  across `engine/disp*.cpp`, `public/builddisp.cpp`, `disp_powerinfo.cpp` and
  `disp_tesselate.h`; about 350 have a counterpart, because the LOD tree, decals,
  neighbour stitching and `SetupAllowedVerts` all delete — the last because `vbsp` already
  wrote its answer into the lump. **There is no separate terrain draw path**: a
  displacement is selected, materialed, lightmapped, batched by `(material, page)` and
  split at 65,536 vertices by the *same* pipeline an ordinary face is, and the only
  difference is that `build_page_meshes` asks the patch for its grid instead of fanning the
  face's winding. That is also Valve's `DispInfo_CreateMaterialGroups`. Measured rather
  than assumed: **all 1,181 shipped displacements are in model 0**, so brush entities need
  no change, and all 1,181 have a four-cornered base face.
  Four rules here produce a plausible wrong picture rather than an error. **Texture
  coordinates are bilinear over the base face's four *flat* corners**, not the projection
  evaluated at the displaced position — the two agree on a flat patch and diverge with the
  displacement, so the wrong one looks right until you stand next to a cliff. **Lightmap
  coordinates are not the base face's at all**: `vrad` bakes against the *grid*, so
  `BuildDispSurfInit` computes the face's luxel corners and then overwrites them with a
  canonical square, collapsing to `(0.5 + w·j/n, 0.5 + h·i/n)` where `w`,`h` are the
  *extents* and not the block size. **The render tessellation is not the collision one** —
  it is a quadtree walk that skips any vertex `vbsp` disallowed, which is what stops a
  power-4 patch cracking against a power-2 neighbour (100 of the 1,181, and **none in
  `sp_a1_intro1`**, so only the depot test reaches it) — and yet for a patch with nothing
  disallowed the two coincide **exactly**, which is the unit test that makes the walk
  checkable at all. And **terrain triangles are reversed like every other piece of
  Valve-authored geometry**; this port's own portdoc argued they should not be, and the
  depot test caught it on its first run. That anchor is worth copying: the sum of a
  patch's rendered triangle normals must agree in sign with the *rendered* normal of the
  base face it was carved from — 1,181 of 1,181 agree, 1,181 of 1,181 disagree without the
  reversal, and it is taken per patch rather than per triangle because 131 of 92,622
  individual triangles genuinely overhang.
  **`WorldVertexTransition` landed with it, and it is `LightmappedGeneric`.** 937 of the
  game's 1,181 displacement faces name it — including all 11 of `sp_a1_intro1`'s — so
  without it this module's output was eleven magenta checkerboards.
  `worldvertextransition.cpp` forwards to `DrawLightmappedGeneric_DX9` and nothing else, so
  the WGSL, the vertex layout, the lighting binding and the bind group layout are shared;
  only `name()` and the parameter table differ. Measured: **zero** non-displacement faces
  in the game name it. What it added to the shader is `$basetexture2` blended by the vertex
  alpha, `$blendmodulatetexture`, `$bumpmap2` — and **`$ssbump`, which was a live bug in
  the world path all along**: a self-shadowed bump map is three positive coefficients, not
  a signed normal, so both the `2t-1` decode and the `saturate(dot(n,basis))²` weighting
  are wrong for it, and **128,139 of Portal 2's 288,250 drawable world faces** wear one.
  Deferred and measured: `$seamless_scale` (553 displacement faces, all in `sp_a3_*`, none
  in `sp_a1_intro1`) and `$envmap` — which are now **the** reason `LightmappedGeneric` will
  eventually need a second vertex layout, since both want a world-space normal that a
  `WorldVertex` does not carry. `MATERIALSYSTEM.md` §10 expected bumpedness to force that
  and it did not.
  A gap closed on the way past: **nothing in `cargo test` had ever compiled
  `lightmappedgeneric.wgsl`**, because `preview.rs`'s GPU tests draw `UnlitGeneric` and
  `VertexLitGeneric` only. `materials::pipeline`'s
  `every_shader_compiles_and_builds_a_pipeline` now builds a real pipeline for every
  `ShaderKind`, which also checks the thing a WGSL author gets wrong most often — that the
  bind group layout and the `@group`/`@binding` declarations agree.
  **`world/light.rs` is the light cache, and with it every model in a level is lit the
  way the shipped game lights it.** `engine/lightcache.cpp`'s static half —
  `LightcacheGetStatic` and everything under it — plus the
  `dworldlight_t`-to-hardware-light conversion out of `engine/l_studio.cpp`. It absorbed
  `props/light.rs`, which held the ambient-cube half and was in the wrong directory once
  entity models started using it too. `bsp.rs` reads `LUMP_WORLDLIGHTS_HDR` (version 1 on
  all 106 maps, so version 0 is refused rather than widened) and the module does the
  selection: the grid-cell cull, the falloff, the angular term, **one trace per light**,
  the `MIN( MaxNumLights(), r_worldlights )` slots and the fold-the-rest-into-the-cube
  step. Measured: **14,246 world lights across the game, 7,302 surviving the load-time
  filters, and 1.4 s to light all 56,955 static props** — 13 ms a map, against 0.25 s for
  `sp_a1_intro1`'s collision model, which is why `FastRejectLightSource`'s PVS test is
  left out. The frame cost did not move; all of this is load-time.
  **The finding that mattered most was not about lights at all: the two lighting terms
  do not add together.** `StudioSetupLighting` asks `LightcacheGetStatic` *without*
  `LIGHTCACHEFLAGS_STATIC` for a prop that wears `vrad`'s per-vertex bake, so that prop's
  ambient cube and local lights come back **zeroed** and the bake is everything; a prop
  that is bumped or phong is lit per pixel, never has its `.vhv` opened at all, and gets
  the cache instead. This port had been adding the leaf ambient cube on top of the bake
  since `studio/` stage 5, which double-counts a prop's indirect light. The predicate is
  `STUDIOHDR_FLAGS_USES_BUMPMAPPING` — `$bumpmap`, **or `$phong` non-zero on its own**,
  which is a wider net than `WantsPhongShader` casts — and it now lives on
  `Material::uses_bumpmapping`. On `sp_a1_intro1`: **816 baked, 246 per-pixel, 18 with no
  file at all**, where before all 1,062 with a file took both.
  Three more rules produce a plausible wrong picture rather than an error, and the first
  decides whether half the lights in Portal 2 are counted twice.
  **The ambient cube this port uses is not the one a static prop gets.** Valve gathers a
  static prop's by firing 162 rays at the lightmaps and everything else's from `vrad`'s
  baked leaf cubes — and the two carry the same energy by different routes, because
  `vrad`'s cube already contains the dim `emit_surface` lights and the runtime gather does
  not. That is `bAddedLeafAmbientCube`, and it is what `AddStaticLighting`'s first
  `continue` reads. This port uses the leaf cube for everything, so the flag is *true*
  here and the `DWL_FLAGS_INAMBIENTCUBE` lights are dropped at load: **6,731 of the
  14,246**.
  **`1 / (thetaDot - phiDot)` is 1 when the two cone cosines are equal, not 0** — "hard
  falloff instead of divide by zero". `WorldLightToMaterialLight` turns every
  `emit_surface` light, 7,073 of the game's 14,246, into a 180-degree spotlight with both
  cosines 0, and the shader reads a 0 there as "this light is off". `uniforms::Light::spot`
  had the other answer and no callers; it has Valve's now.
  And **`r_worldlights` is 2 because this port is POSIX** — the tree offers 4 as designed,
  3 "Changed from 4 to 3 for L4D!", and 2 under `#ifdef POSIX` with "JasonM GL - capping
  at 2 world lights at the moment". It is the least certain constant in the module, it
  binds almost everywhere (**44,421 of the game's 56,955 props fill both slots**), and
  raising it is one edit because everything downstream already carries four. What it buys
  is directionality rather than brightness: a light that misses a slot is folded into the
  ambient cube rather than discarded.
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
- **`src/client/` — the game client, stages 1-4 of 5 ported, plus the dead
  player `server/` stage 5 brought** (`portdocs/CLIENT.md`,
  **`rustdocs/CLIENT.md`** — read that before calling in). The first *game* module in the
  tree, and a sibling of `src/engine/` because `client.so` was a sibling of `engine.so`.
  **It is not `ENGINE.md` §7.5**, which is the client *connection* (`CClientState`,
  snapshot parsing), lands at `src/engine/client/` and is blocked on `net/`; the two share
  a name and nothing else. Stage 1 is the input→command→movement→view spine: `UserCmd`,
  `kbutton_t`'s two-holder set **with its fractional `KeyState`** (the half `input/`
  deliberately refused to build against a camera), the 22 `+`/`-` buttons and their `IN_*`
  bits, `FullNoClipMove` and `Accelerate`, a `Player` in `MOVETYPE_NOCLIP`, and ~19 cvars
  with Valve's names, defaults, bounds and flags. **Valve's own
  `// FIXME, move entirely to client .dll`** (`engine/cdll_engine_int.cpp:1048`) is taken:
  the view angles are the client's and the engine never gets a copy.
  Stage 2 is `CViewRender::SetUpView`: a `ViewSetup`, `GetZNear`'s mega-wide branch,
  `GetZFar` from `r_farz`/`r_mapextents`, and `Engine::camera` reduced to a
  `ViewSetup` → `Camera` conversion. **It also fixed a field of view that had been
  quietly too narrow since the camera existed** — Source quotes FOV *horizontally at
  4:3* and scales it by `aspect / (4/3)` before projecting (`view.cpp:1084`), which the
  port was not doing, so 16:9 was showing a 46.7° vertical FOV where the shipped game
  shows 59.8°.
  Stage 3 is keyboard look — `AdjustAngles`/`AdjustYaw`/`AdjustPitch`, `cl_yawspeed`,
  `cl_pitchspeed`, `cl_anglespeedkey`, `cl_mouselook` — plus `IN_SetSampleTime`'s budget.
  **`ExtraMouseSample` is deliberately not ported**, and the plan was wrong to assume it
  would be: the latency it recovers is not lost here (`update_client` runs immediately
  before `render`, with nothing between), and `winit` gives one batch of events per frame
  where Valve re-polls the OS mid-frame, so a second drain would return nothing. Revisit
  when simulation lands between input and rendering.
  **Stage 4 is walking**, and its headline finding is that the reference is
  **`CPortalGameMovement`, not `CGameMovement`**: Portal 2 overrides two dozen of the base
  class's methods and ten of the overrides change behaviour that has nothing to do with
  portals. Jump height is **45 units, not 21**; the air-control cap is **60, not 30**;
  ducking takes **400 ms, not CS:GO's 200**; gravity is **600, not 800**; jumping while
  ducked is **refused** where the base class allows it; **edge friction** doubles friction
  over a ledge and the base class has none; and walking into a standable slope **slides up
  it** rather than stepping. Where Portal's override only generalises world `+Z` to a
  paint-gel "stick normal", the two are the same function with no paint and the world-`+Z`
  form is what is ported. Stage 4 also **found a live stage-1 bug**: a Portal 2 player's
  max speed is `min(sv_maxspeed, MaxSpeed())` = **175**, not `sv_maxspeed`'s 320, so noclip
  had been flying at 1600 where the shipped game flies at 875. Not ported and documented:
  water, base velocity, the unstick passes — and **ladders, the duck-jump state
  machine and fall damage are deleted rather than deferred**, because
  `GameHasLadders()` is `false` for Portal, `CheckJumpButton` sets
  `bSetDuckJump = false` over a Valve FIXME, and
  `CPortalGameRules::FlPlayerFallDamage` is
  `{ return 0.0f; } //no fall damage in portal` — so every branch that reads
  them is unreachable and **nothing in Portal 2 can be killed by landing,
  whatever the height**.
  Seven rules that produce a plausible wrong answer rather than an error:
  **`ViewSetup::fov` is horizontal and already width-ratio scaled**, so anything reading
  `default_fov` for a projection is reintroducing that bug; **`set_sample_time` must be
  called once per frame before `create_move`** or keyboard look silently does nothing for
  ever; **`cl_mouselook 0` does not turn the mouse off** — it *adds* keyboard pitch, and
  `cl_mouseenable 0` is the switch it gets mistaken for;
  **`KeyState` is destructive and the read order matters** — the movement axes are
  computed before the button bits, so a tap shorter than a frame reaches `forwardmove` and
  *not* `IN_FORWARD`, and reversing them is a difference a server would see; **the first
  frame after a press is worth half a frame**, so a movement number wrong by a factor of
  two is usually this working correctly; **`Player::origin` is the feet** and `eye()` is
  64 units higher, so conflating them reads as a level built slightly wrong; and **a `dt`
  of 1.0 does not move the player at all**, because the friction bleed scales with the
  frame time and a one-second step removes more speed than a second of acceleration adds.
  Stage 4 adds four more: **`mv.max_speed` is 175 and not `sv_maxspeed`**, which bounds
  noclip as well as walking; **`old_buttons` lives on the `Player`**, because jump and duck
  both ask about the *previous* command and a `MoveData` built fresh each frame has to
  round-trip it; **`speed_cropped` must start false every command** or a crouched player
  moves at full speed; and **`full_walk_move` zeroes a grounded player's vertical velocity
  before anything else**, so `CategorizePosition`'s "rising too fast to be on the ground"
  test is only ever reachable from the air.

  **The dead player landed with `server/` stage 5**, which is the one piece of
  movement this module gained after stage 4. `MoveType::FlyGravity` is
  `CGameMovement::FullTossMove` — gravity, one swept move and a stop, with no
  clip-and-retry and no stair stepping, which is what makes a corpse feel like
  a dropped object — and `check_parameters` grew the two `if`s that read the
  server's state. They overlap and are **not** the same test:
  `FL_FROZEN || IsDead()` zeroes the three move axes and nothing else, so a
  corpse that was falling keeps falling, while `IsDead()` *alone* pins the
  movement basis to the previous command's angles and drops the eye to
  `VEC_DEAD_VIEWHEIGHT`.
  Five rules there produce a plausible wrong answer rather than an error, and
  the first two are the ones that decide whether death looks right.
  **`IsDead()` is `m_iHealth <= 0`, not the life state** — they disagree for
  exactly one server dispatch, which is why `PlayerState` carries the health.
  **The dead view offset is written twice a command and the second one is
  load-bearing**, because `Duck()` runs between them and would otherwise lift
  the eye back out of the corpse over 400 ms.
  **`VEC_DEAD_VIEWHEIGHT` is 14, not 60** — the 60 is the *multiplayer* table,
  annotated "previously 14", and single-player Portal 2 overrides no view
  vectors. **The angle pin does not stick**, and that is Valve's:
  `CPlayerMove::FinishMove`'s `SetLocalAngles` line is commented out, so a dead
  Portal 2 player really can still turn the camera and what stops them looking
  at anything is the fade. And **`check_parameters` needs the *previous*
  command's angles**, captured at the top of `create_move` before
  `adjust_angles` has moved them — taking them at `run_move` time gives the
  current ones and the pin becomes a no-op you cannot see.

  **`client/tonemap.rs` landed alongside the five stages rather than inside them**
  (`portdocs/CLIENT_TONEMAP.md`, and it is `viewpostprocess.cpp`'s `CTonemapSystem`, not
  the input-and-view layer `portdocs/CLIENT.md` plans). It is the **policy** half of auto
  exposure — bucket boundaries, the percentile search, the moving average, the rate
  limiting and twelve `mat_*` cvars — and **it names no GPU type**, the way
  `materials/histogram.rs` names no cvar; the two meet only in `Engine::render`. The
  finding that decides the whole calibration is that **the histogram measures linear
  light, not gamma**: `dev/lumcompare.vmt` leaves `$LINEARREAD_BASETEXTURE` unset so
  `screenspace_general` reads the frame buffer through an sRGB sampler, and Valve's own
  comment at `IssueQuery` says the opposite and is stale — reading the boundaries as gamma
  puts the 65% target at 0.32 linear and halves every scene. Four more that produce a
  plausible wrong answer rather than an error: **the measurement is of an
  already-exposed frame**, so the result is a *correction* to the current scale and
  multiplying is what makes the loop converge rather than oscillate; **the moving-average
  weights are `|i - 5| / 5`**, so the oldest sample counts most and the middle one counts
  for nothing, which is absurd and is what every Source game has been smoothed with;
  **the step is capped per frame and not per second**, which makes adaptation frame-rate
  dependent above ~128 fps and renders `mat_accelerate_adjust_exposure_down` inert below
  it; and **`mat_dynamic_tonemapping 0` freezes the exposure where it is** rather than
  resetting it to 1. Deleted rather than deferred: `mat_tonemap_algorithm 0` (selected by
  a game-directory match against `{dod, cstrike, lostcoast}`, so unreachable),
  `SetOverrideTonemapScale`, and `DisplayHistogram`'s 200-line bar chart — the `tonemap`
  console command prints the same numbers. **`env_tonemap_controller` was its one
  measured gap and `server/` stage 2 closed it**: the thirteen file-scope globals
  `GetTonemapSettingsFromEnvTonemapController` writes became
  `client::tonemap::TonemapSettings`, which the server fills in and `Engine::render`
  hands over once a frame — 105 of Portal 2's 106 maps place a controller, and
  `sp_a1_intro1` now gets the ceiling of 1.5 it asks for. **One Valve bug deliberately
  not reproduced**: the no-controller fallback resets every custom flag *except*
  `g_bUseCustomAutoExposureMin`, so a custom minimum is sticky for the rest of the level;
  `TonemapSettings::default` resets all of them.
- **`src/studio/` — stages 1-5 of `portdocs/STUDIO.md`'s six ported, plus animation**,
  and with them **static props draw, lit the way the shipped game lights them, and an
  entity's model animates**. `.mdl`/`.vvd`/`.dx90.vtx` become a `StudioModel`: one vertex
  buffer, one index buffer, per-material `Batch`es. The instances are
  `src/engine/world/props/` — the `sprp` game lump, `AngleMatrix` transforms, one upload
  per distinct model and one draw per instance, lit by `world/light.rs`'s light cache.
  `sp_a1_intro1` now draws **1,080 props from 136 models, 224,924 triangles** on top of
  the world's 14,546, **816 of them wearing `vrad`'s per-vertex bake and 246 lit per
  pixel by the world lights instead** (18 have neither and take the cache too). Stage 4
  also mounted the `.bsp`'s `LUMP_PAKFILE` as a search path, which is what the `.vhv`
  files live in and **which also fixed the 8 `maps/<map>/…` cubemap materials** that used
  to draw as checkerboards — one change, two subsystems, as predicted. Not done: LOD
  selection (stage 6) and `.phy` collision (that is `ENGINE_TRACE.md`'s). **`studio/anim.rs` landed later, with `prop_floor_button`** — bones,
  sequences and the RLE animation blocks, plus the `R_StudioSetupBones` slice that poses
  them; skinning is *replaced* by a per-bone draw split rather than deferred, which is
  exact for every model the port draws. See `src/server/`, below.
  **`prop_dynamic` measured two gaps in that half, and the larger one is now closed.**
  **`studio/include.rs` is `$includemodel`** — `CStudioHdr::ResolveIncludedModels` and
  the `virtualmodel_t` under it: 9 of the 606 models the game's props name keep their
  sequences in a companion `*_animation.mdl`, **926 entities wear one**, and until it
  landed those 926 stood in their bind pose. Valve keeps the included headers separate
  and hops through a per-group remap table on every access, because they are cache
  entries that can be evicted; a `StudioModel` is an owned value, so the merge happens
  **once, at load**, and `masterSeq`, `boneMap`, the attachment/pose/node tables and
  `CModelLookupContext` all delete. What does not delete is `masterBone`: an included
  animation's track names a bone of the *included* model, and the two skeletons are the
  same bones **in a different order** in eight of the nine — so a merge without the
  remap bends a panel arm at the wrong joint, which is a wrong picture and not an
  error. Measured on the running maps: of the 2,738 props playing a sequence two
  seconds into their level, the labels that resolve went from 1,666 to **2,556** and
  the ones that do not from 897 to **182** — and that remainder is Valve's own map
  errors rather than a gap. `portdocs/STUDIO.md` §12 has the anatomy; the rest of the
  measurements are there too, including the one that made it simple (**host and include
  bind poses agree to 4e-6**, so `boneMap` buys nothing) and the one that bounds it
  (**nothing nests**, and `STUDIO_OVERRIDE` is set on 0 of the game's 7,885 sequences).
  **`.ani` animation blocks are the binding constraint now**: the nine models name
  eight distinct companions (both panel arms share one) and only two are inline —
  `arm64x64_interior_animation`, all 1,350 of them, and `personality_sphere_animation`,
  313 of 318 — while the other six keep almost all of theirs in a companion `.ani`, so
  their labels resolve and their poses are empty. Every model but the two panel arms is
  one this port draws in its bind pose for want of skinning anyway, so skinning comes
  first and the arms are the whole visible payoff: **898 of the 926**.
  And **flex deltas stop being academic**: the 16 models the reader
  refuses are `models/props_destruction/toxin*`, 15 of them are placed as
  `prop_dynamic`s by **41 entities**, and those 41 draw nothing. The "absent from the
  data" claim below is about *static props* and is still exactly true; `prop_dynamic`
  is the first thing in the port that places a model that is not one. `CMDLCache`'s eviction, budgets and async queues are
  **deleted rather than deferred**, and so are skinning, flexes and sub-d — which are
  absent from the *data*: all 968 models Portal 2 places as static props have one bone,
  trilist strips and no flex deltas. **API: `rustdocs/STUDIO.md`** — read it before
  calling in, in particular for the gotchas that produce a plausible wrong picture rather
  than an error: **`sizeof(StaticPropLumpV9_t)` is 72 and not 69**, because Valve's prop
  structs are the only ones on this path not `#pragma pack(1)`, and at 69 every prop
  after the first drifts; **the ambient cube decodes with `ColorRGBExp32ToVector` and the
  lightmap with `TexLightToLinear`**, which is the *opposite* of `rustdocs/MATERIALS.md`'s
  rule and 255× either way (measured: 0.0249 against 0.0002 mean luminance on
  `sp_a1_intro1`); **a `QAngle` is pitch, yaw, roll** composed `Rz·Ry·Rx`, so props with
  only a yaw look right under any other reading and tilted ones do not; and **a leaf with
  zero ambient samples and a non-zero `first_sample` is a solid leaf whose `first_sample`
  is a *leaf* index**, which is what keeps a prop embedded in geometry lit.
  Two more gotchas arrived with stage 4, and the first is the worst in the module:
  **a `.vhv` is in *hardware* vertex order, not `.vvd` pool order** — Valve's runtime
  compacts a model's vertices per LOD and bakes against that numbering, this port does
  not compact, and reading the block as a run over the pool mislights 125 of
  `sp_a1_intro1`'s 1,080 props **while appearing to work for the other 955**
  (`HardwareMesh` carries the mapping); and **`vrad` writes no block for an empty mesh**,
  so a model's empty meshes must be dropped before the lists are matched. The `.vhv`
  checksum is **counted, not enforced**, because `r_ignoreStaticColorChecksum` defaults to
  1 and 24 of the game's 56,801 files need it to.
  Two verifications run against the real depot behind `KISAK_GAME_DIR` and `--ignored`:
  **2,017 of the 2,041 shipped models parse** (all 1,444 flagged `STATIC_PROP`; the 16
  refusals are animated flex-delta models and are correct), and **all 106 shipped maps
  place their props — 56,955 of them — with all 56,801 `.vhv` files describing the model
  they are for**. The first of those found two wrong `.vtx` field offsets that **every
  synthetic test had passed**, because the fixture had been written from the reader
  instead of from `optimize.h`; the second found the hardware-order rule.
  `portdocs/STUDIO.md` §11 has both.
- **`src/server/` — all five stages of `portdocs/SERVER.md` ported, plus
  `prop_floor_button` and `prop_dynamic`**, and with them the map's **entity logic
  runs, its brush entities move, it notices the player, a pad you stand on
  presses, the models it places draw and animate — and the player can be hurt,
  and can die**. Valve's `server.so` — 446,861 lines, of which the framework is
  ~29,800 and is the module. `Server::level_init` turns the `.bsp`'s entity lump into
  entities: `ClassDef` chooses the class, `CBaseEntity::KeyValue`'s ladder and the class's
  own `key_value` parse the keys, and the three-pass spawn runs — hierarchy depth, then
  `SortSpawnListByHierarchy`, then `Spawn` and `Activate` over the whole list — with
  `UTIL_Remove`'s deferred deletion under it. `EntityId` is `CBaseHandle` as a generational
  index. **API: `rustdocs/SERVER.md`** — read it before calling in.
  Scoped by **measuring the shipped maps rather than the tree**: 106 maps place
  **60,925 entities of exactly 200 classnames**, the top 25 of which are 79.8% of them,
  while the whole 122,298-line `ai_*`/`nav_*` tree serves **293 `npc_*` instances of 6
  classnames**. Sixteen classnames are implemented and they cover **19,229 of the 60,925
  blocks**: `logic_relay` (8,082, the commonest entity in the game), the light family
  (7,150), `func_instance_io_proxy` (1,184), `logic_auto` (1,112), `logic_branch` (601),
  `info_target`, `logic_timer`, `info_player_start`, `env_tonemap_controller`,
  `worldspawn`, `math_counter` and `logic_case`. `sp_a1_intro1` spawns 163 entities from
  598 blocks.
  The one piece of stage-1 *behaviour* is `CLight::Spawn`, and it is load-bearing:
  **an unnamed light deletes itself**, which is 6,937 of the game's 7,150, because `vrad`
  has already baked its whole contribution — a port that skipped that one `if` would carry
  eleven per cent of the entity list as garbage and every other number would still look
  right. 213 named lights survive.
  Four findings from writing it. **Inheritance became composition**: `SERVER.md` §7.3
  planned a `parent` pointer so `KeyValue` could walk the `baseMap` chain, and there is
  nothing for the walk to do — `CEnvLight : public CLight` is an `EnvLight` that *holds* a
  `Light` and ends its `key_value` by calling the contained one's. **The FGD is not an
  oracle**: the four shipped `.fgd` files describe 199 of the 200 classnames and are the
  best reference for what a key is called, but they are not a superset of the datadesc
  (`world_mins` is `vbsp`'s, `defaultstyle` is internal, the FGD's `OnProxyRelay` is the
  server's `OnProxyRelay1`-`30`), so the check with teeth is against **map data** — every
  declared key must be consumed, and the 106-map depot test pins the exact set of key
  names nothing consumes. **`names_match`'s `*` does not have to be trailing** whatever
  `baseentity.cpp`'s comment says — `"*door"` matches everything — though all 234 wildcard
  targets in the shipped maps are plain trailing ones. And **"unhandled key" is not
  "unimplemented"**: of the 29 key names in the whole game that nothing consumes, 17 are
  the *map compiler's* (`_light`, `_quadratic_attn` and the falloff family are `vrad`'s and
  have no run-time consumer in Valve's engine either) and 6 are mapper mistakes shipped in
  the game.
  **The module names no GPU type** — not `wgpu`, not `materials`, not `studio` — so all 78
  of its unit tests run without a window, the way `host/`, `trace/` and `input/` do. An
  entity holds a model *name*.

  **Stage 2 is entity I/O, the event queue and thinks, and it is what makes a map do
  anything.** `CEventAction`/`CBaseEntityOutput`/`CEventQueue`, `AcceptInput` with
  `variant_t`'s coercion table, the think schedule and the `SimThink` list, the frame
  order from `CServerGameDLL::GameFrame`, six more classes, and `ent_fire`/`dumpeventqueue`
  beside the two stage-1 commands. **The visible outcome is the exposure**:
  `sp_a1_intro1` asks for a ceiling of 1.5 against the cvar default of 2, through
  `logic_relay`'s `OnSpawn` → two exposure relays → `env_tonemap_controller`, and now gets
  it. Measured over the depot: **two seconds of server time on each of the 106 maps is
  5,763 events dispatched, 2,070 inputs accepted and 1,197 thinks**, with zero values
  that would not convert and a peak of 43 entities thinking at once.

  **The tick decision (`portdocs/SERVER.md` §5) is taken: the server is fixed-tick and the
  client is not.** `ServerClock` accumulates the rendered frame's time and runs zero or
  more 1/64 s ticks inside it; the client keeps moving the player on the rendered frame,
  which is Valve's own split. It is forced rather than chosen — `SetNextThink` quantises
  to ticks, so a schedule built on a variable `dt` is a different schedule at every frame
  rate. **The rate is one constant and is still unverified**: 1/64 is
  `DEFAULT_TICK_INTERVAL_PC`, which is CS:GO's number, and Portal 2's real
  `interval_per_tick` is not in the tree, the maps, or the depot, which ships only
  `vbsp`/`vvis`/`vrad`. `-tickrate` overrides it, quantised to `N/512` and clamped to
  20.48–128 Hz as `GetTickInterval` does.

  Seven rules here produce a plausible wrong answer rather than an error, and the first
  two are the ones that decide whether a map runs at all. **The server's `curtime` is not
  `Scene::curtime`** — the server's is `tick * interval` and moves in steps, the scene's
  is the accumulated wall clock. **`SetNextThink` rounds to the nearest tick and a think
  tick of zero never runs**, so `curtime + 0.01` is next tick at 64 Hz and *never* at
  30 Hz — and `logic_auto`'s 0.2-second bootstrap, which every map in the game starts
  through, lives on that edge. **The event queue restarts from the head after every
  event**, so a chain of eight zero-delay relays completes in one tick and not eight; get
  it wrong and every map runs its logic in slow motion. **An output's connections fire in
  *reverse* lump order**, because `AddEventAction` prepends. **A `variant_t` accessor
  returns zero unless the value already is that type**, so a handler is only safe because
  `AcceptInput` converted against the type `ClassDef::inputs` declared — a wrong
  declaration there is a silent zero, not a compile error. **The think schedule is
  cleared before the think runs**, so anything recurring re-arms on the way out. And
  **a per-action parameter override silently discards the caller's extra delay**, which
  is Valve's bug at `cbase.cpp:280` against `:289` and is reproduced.

  Two findings worth carrying to stage 3. **`SERVER.md` §10.3's borrow risk is not one**:
  `FireOutput` appends to the queue rather than calling the target, so nothing in the
  subsystem is re-entrant and a behaviour's `Context` does not hold the entity list at
  all — the condition that changes that is `logic_branch_listener`, the first class in
  the game that must read *another* entity during dispatch. **It landed, and it did not
  change the shape**: stage 4's `Server::dispatch` lifting the dispatched entity out of
  the list was already enough. And **`CUniformRandomStream`
  was ported rather than replaced by a crate**, one of the few places `PORTING.md`'s
  "prefer the crate" rule points the other way: `ran1`'s rejection sampling and its lossy
  seed convention (0, 1 and -1 are one stream) are behaviour a dependency would silently
  replace, and `logic_case` is what would change.

  **Stage 3 is `MOVETYPE_PUSH`, and it is the first time anything in a map moves.**
  `src/server/movement.rs` is `CBaseToggle`'s two moves — set a velocity, set an
  arrival alarm — plus `PerformPush` with the blocker always null, and
  `src/server/classes/brush.rs` is six classes: `func_brush` (2,502), `func_door_rotating`
  (346), `func_door` (275), `func_movelinear` (196), `func_button` (64) and
  `func_rotating` (27), **3,410 entities**, taking the port to 22 classnames and 22,639
  of the game's 60,925 blocks. Pushing the player is deliberately absent —
  `CPhysicsPushedEntities` is ~1,000 lines of speculative push and rollback that want
  `ENGINE_TRACE.md` stage 4 underneath them — so a door moves *through* a player rather
  than shoving one.
  Measured over the depot: two seconds of each of the 106 maps now moves **67 brush
  entities off their spawn placement, 34 of them still travelling** when the clock stops.
  Before stage 3 that number was zero. **To see it, load a co-op map**: no
  single-player map moves a brush entity in its first twenty seconds — a Portal 2
  chamber starts shut and waits for the player, which is why `sp_a1_intro1` looks
  identical to before — while `mp_coop_fan` spins `brush_fan` and opens two doors at map
  spawn and `mp_coop_lobby_2` slides eleven `func_movelinear` screen panels.

  Four findings. **The scope of "brush entities" is not the mover census**: §4.7 counted
  1,164 movers and the six classes are 3,410 entities, because `func_brush` is 2,502 of
  them, does not move at all, and is where **`StartDisabled` finally comes home** — 337
  of them start switched off and until this stage `world/` drew every one. **The join
  between the game and the renderer is the `"*N"` model index**, and that is a
  measurement rather than a convention: 106 maps place 11,635 `(map, "*N")` pairs and
  **not one** is claimed by two entities, so `BrushModel`'s placement is refreshed once a
  frame by index — `Engine::frame` does it between the server's ticks and the player's
  trace — and `world/` and `server/` still name no type of each other's.
  **`CSimThinkManager` is two questions in one list** and stage 2 only saw one: an entity
  is in it when it will think *or* when it is a mover with a live alarm, and a mover is
  stored with a tick of **zero** so that it is handed out every tick and refuses its own
  think — which makes `PhysicsRunSpecificThink`'s tick guard load-bearing rather than
  defensive. And **a mover needs one number that is not in the entity lump**: a door's
  travel is the size of its own brushes, which `SetModel` reads out of the `.bsp`'s model
  lump, so `level_init` grew a `&[bsp::Model]` argument.

  Nine more rules produce a plausible wrong answer rather than an error, and the first
  three are the ones that decide whether a door works at all. **The arrival alarm is not
  the think schedule** — it is a second timer with its own field, it is *not* quantised
  to a tick, and it runs on the entity's own `local_time`; a `func_door` uses it for both
  the travel and the wait. **`SetMoveDoneTime(0)` arms an alarm that can never fire**,
  because `PerformPush` tests the absolute alarm with `> 0` and `WillSimulateGamePhysics`
  then drops the entity out of the simulation list entirely — which is why the four
  `func_door_rotating`s in the shipped game with `wait 0` stand open for ever, and why
  `CBaseButton::Spawn`'s apparently pointless substitution of `wait 1` for `wait 0` is
  load-bearing for 14 of the game's 64 buttons. **A class holding a `Toggle` must call
  `Toggle::move_done` first**, because that call *is* `CBaseToggle::MoveDone` and it is
  what snaps the mover onto its exact destination. **`linear_move` returning `false` means
  "already there" and the caller must run `move_done` itself, *before* firing any
  output** — in the C++ that call happens inside `LinearMove`, so a zero-length open
  queues `OnFullyOpen` before `OnOpen`. **`speed` is `CBaseEntity`'s**, not the mover's,
  because `func_rotating` uses it as its current rotation rate. **`DotProductAbs` is not
  `|a·b|`**, and a door's travel subtracts two units for the bbox expansion before the
  lip. **`AngleVectors` of a right angle is not exact**, so a door travelling 64 units
  straight up also travels 2.8 millionths of a unit sideways — Valve's residue too.
  **The `Use` input's *type* is the connection's serial number, cast**
  (`InputUse` passes `(USE_TYPE)inputdata.nOutputID`), which is why an I/O `Use` does
  nothing on a `func_movelinear` and works on a `func_button`. And **a parented mover
  moves in world space**, where Valve moves it in the parent's frame — 174 of the game's
  1,164 movers name a parent, and the missing local/abs pair is the same thing that keeps
  the `SetParent` family unimplemented (**1,078 of the depot's 1,081 unhandled inputs**).

  One find worth keeping for its own sake: **`inputfilter` is declared by `base.fgd` for
  `func_brush`, written onto 2,497 of them by Hammer, and consumed by nothing anywhere in
  `legacy/`** — not in `game/server/`, not in the engine, not in the tools. The sharpest
  example yet of `portdocs/SERVER.md` §1.4's "the FGD is a reference, not an oracle".

  **Stage 4 is triggers and touch, and it is the first time the map responds to
  the player.** `src/server/touch.rs` is `touchlink_t` and the four
  `CBaseEntity::Physics*Touch*` functions; `classes/trigger.rs` is `CBaseTrigger`
  plus `trigger_once` (1,476), `trigger_multiple` (899), `trigger_hurt` (215),
  `trigger_push` (192) and `trigger_teleport` (110); `classes/filter.rs` is the
  six `filter_*` classes (302) they consult; `classes/point.rs` is
  `point_teleport` (128); and `classes/player.rs` is the player. **Twelve
  classnames, taking the port to 34 and to 25,961 of the game's 60,925 entity
  blocks.** On the engine side it brought `trace/` stage 4's clip chain,
  `World::clip_models`, `World::brush_models_touching` and base velocity in
  `client/`'s walk.

  **The player had to become an entity here, not at stage 5, and that contradicts
  the plan.** A touch is a fact about *two* entities:
  `PassesTriggerFilters` tests `FL_CLIENT` on the toucher, `CTriggerHurt` picks
  its output by `IsPlayer()`, `CFilterName` special-cases the literal string
  `!player`, and **121 of the game's 128 `point_teleport`s target `!player`**.
  So `classes::Player` is sixty lines — a box with `FL_CLIENT` set, holding no
  `client/` type and moving under nobody's power — and the two halves exchange a
  plain `server::PlayerState` that `Engine::frame` copies in before the ticks
  and out after them, the same seam `world/` already had for brush placements
  pointing the other way. Stage 5 is still most of `CBasePlayer`: the movement,
  `noclip`'s home, health, death, the weapon, the view.

  Three more findings. **§10.3's borrow question reopened exactly where stage 2
  said it would** — "a handler that must *read* another entity during dispatch",
  and stage 4 has three of them — **and the answer stage 2 wrote down was
  right**: `Server::dispatch` lifts the entity it is about to run *out* of the
  list, so `Context` can carry the rest of it. No `RefCell`, no `unsafe`, one new
  rule (`cx.entity(self.id())` is `None` inside your own handler). **The
  engine/game split at `SolidMoved` is worth keeping**: the engine answers "what
  does this swept box overlap" and the game decides what it means, which here is
  one trait (`TouchQuery`) and is what lets the whole touch system be tested with
  no map — and two properties of the answer are Valve's and load-bearing, that it
  sweeps the trigger's **real brushes** rather than its bounding box and that it
  is **not** filtered to triggers, because `FSOLID_TRIGGER` is the game's live
  state and an engine-side copy would be a frame stale. And **`trigger_hurt` arrived with
  complete timing and no damage**, which was the honest shape for a stage with
  no health anywhere: it fired `OnHurt`/`OnHurtPlayer` on exactly the schedule
  the shipped game does and took nothing away. Stage 5 supplied the missing
  line.

  Twelve more rules produce a plausible wrong answer rather than an error
  (`rustdocs/SERVER.md` gotchas 35-46), and three decide whether a trigger works
  at all: **a trigger is `SOLID_BSP` *and* `FSOLID_NOT_SOLID` *and*
  `FSOLID_TRIGGER`**, and reading only the bit makes every trigger a wall while
  reading only the type makes every point entity one; **only one side of a touch
  owes an `EndTouch`**, and it is the trigger's; and **an entity that deletes
  itself fires no `EndTouch` of its own**, which is why none of the game's 1,476
  `trigger_once`s ever does. Two more are worth having in hand: **a Portal 2
  single-player `trigger_push` is twice as strong as the map says**
  (`CTriggerPush::Activate`'s `DIRTY HACK TO FOLLOW` — the game was tuned with
  `sv_alternateticks` on and ships with it off), and **a teleport discards the
  swept-from point**, without which a teleport fires every trigger between the
  two ends.

  **One bug the tests could not have found, and it is worth the paragraph.**
  `EntityCore::solid` arrived at stage 4 with a `SOLID_NONE` default and the
  five stage-3 brush classes were never given one, so `is_solid()` was false
  for every door in the game and `World::clip_models` came back empty — the
  clip chain silently collided with nothing, with every unit test passing,
  because they all build a `PlacedBrushModel` by hand. What found it was
  loading the game and reading one number the `trace` command prints:
  *"78 placed, **0** in the clip chain"*. **Adding a field with a `Default` is
  the same class of change as adding an enum variant and the compiler does not
  help**; `tests::every_brush_class_is_solid_unless_it_says_otherwise` now
  spawns each class through `Server::level_init` and asserts on the entity.

  The measurement that says it works is
  `server::tests::every_shipped_maps_triggers_notice_the_player`: for **every one
  of the game's 2,255 live triggers** it reloads the level, finds a point inside
  the trigger's *actual brushes* that a 32×32×72 hull fits in, puts a player
  there and runs two ticks through the same `ClipRayToCollideable` sweep the
  running game uses. **2,246 notice, 1,888 dispatch something, 3 have no point a
  standing player fits in, 6 are switched off or deleted by the map's own
  bootstrap.** (Nine of that 1,888 are a floor button, below: 21 of the probe
  points also stand on a pad, and twelve of those were already firing.)

  **`prop_floor_button` landed after stage 4 rather than inside it, and it is
  the first class in the port from `game/server/portal2/`.** The big red pad you
  stand on — **65 across 47 of the 106 maps, one of them on `sp_a1_intro1`** —
  with 227 output connections on them. `src/server/classes/prop.rs` is
  `CPropFloorButton` and `CPortalButtonTrigger`, taking the port to **36
  classnames and 26,026 of the game's 60,925 entity blocks**.

  It is small and what it *cost* is not, because a button is **two entities**:
  the prop collides with nothing, and what notices the player is a second entity
  the prop creates in its own `Spawn` — a `trigger_portal_button`, 40×40×14
  units, centred on the pad and turned to match it. Three pieces of framework
  came with that, and each is reusable:

  - **`Context::create_entity`** — `CreateEntityByName` + `DispatchSpawn`, the
    first entities in this port that are not in a `.bsp`. The spawn is
    **deferred by one dispatch**, exactly the way `UTIL_Remove` defers a
    deletion, because `Server::dispatch` has lifted the *creator* out of the
    entity list and nothing can dispatch into it while a handler runs.
    `LevelStats::created` is the new term that makes `spawned +
    removed_on_spawn` differ from `matched`.
  - **`Solid::Obb` and `src/server/obb.rs`** — `IntersectRayWithOBB`, the first
    trigger in the port whose shape is a *box* rather than a brush model. It
    lives in `server/` and not in `engine/trace/` because there is no map data
    in it to ask the engine about — and that is where Valve keeps it too, in
    `public/collisionutils.cpp`, compiled into both game DLLs. Two paths, chosen
    by an **exact** comparison against zero angles: a slab clip for the 42
    buttons at `angles "0 0 0"`, and a fifteen-plane separating-axis sweep for
    the 23 that are turned — including `sp_a1_intro1`'s, which is at yaw 90.
  - **`Touched`** — `OnStartTouchAll` and `OnEndTouchAll` are virtuals and until
    now no class overrode either, so `BaseTrigger::start_touch` reports them
    back to whatever contains it.

  **The model draws and the plate animates**, which took the two pieces the
  port did not have. **`src/studio/anim.rs`** is the bone list, the sequence
  table and the RLE animation blocks — `bone_decode.cpp`'s `ExtractAnimValue`,
  `CalcBoneQuaternion`, `CalcBonePosition`, the `Quaternion48`/`Quaternion64`/
  `Vector48` compressed types, and the slice of `R_StudioSetupBones` that turns
  a (sequence, cycle) into one matrix per bone. **`src/engine/world/entities.rs`**
  is the third kind of geometry in a level shell: `.mdl` geometry the *game*
  places, where world faces are `.bsp` geometry with a matrix and static props
  are `.mdl` geometry the *compiler* placed.
  Measured on the real file: `portal_button.mdl` is **3 bones, 4 sequences
  (`BindPose`, `up`, `idledown`, `down`), 11 frames at 24 fps**, and the plate
  travels **7.29 units**, with `up` retracing `down` exactly.

  Four decisions there are worth knowing.
  **There is no skinning, and that is a substitution rather than a gap — with a
  measured expiry date.** Every vertex of every model the port *draws* answers
  to exactly one bone — a button's 7,929 split 7,263 on the body and 666 on the
  plate — so each batch's triangles are sorted by bone at load and each bone's
  contiguous run is drawn under its own matrix. That needs no change to the
  vertex format, the shaders or the bind groups, and it is **exact** for this
  data. It does **not** generalise: across the game 420 of 2,017 models have
  more than one bone and **141 of those share a vertex between two** (the
  `a4_destruction` set), so `StudioModel::rigid_bones` checks the precondition
  rather than assuming it, a model that fails it is drawn in its bind pose and
  counted, and those 141 are the condition that makes real skinning worth
  writing. **`prop_dynamic` cashed that condition in.** It places models rather
  than static props, so it reaches them: **74 of the 591 readable models the
  game's props name share a vertex between bones, and 290 entities wear one**,
  drawn in their bind pose instead of animating. Seven of those models are on
  `sp_a1_intro1` — the `models/container_ride/finedebris_part*` set — so it is
  visible on the map this port loads by default rather than only in a census.
  With `$includemodel` merged, skinning is now the **largest** gap in the model
  path — and it gates the next one, because the six include hosts whose
  animation is in an `.ani` are all models it cannot pose anyway.
  **The RLE stream is expanded at load, not walked at draw**, because a whole
  button model's animation is a few hundred bytes — the game's longest is
  **4,050 frames**, which is the matching bound on that decision.
  **The join with the game is a sequence *name***: the server says `"down"` and
  when it started, and the engine looks the label up and computes the cycle from
  the scene clock — Valve's own server/client split, and what keeps the
  animation smooth where a 64 Hz tick would step it.
  And **`AnimateThink` is still not scheduled**, which is now a saving rather
  than an absence: its body is `StudioFrameAdvance`, which the renderer does for
  itself.

  **One bug this found that nothing else would have.** Bone **255 terminates**
  an animation's bone chain — `studiomdl` writes it (`write.cpp:1182`) and the
  decoder reads `while (panim && panim->bone < 255)` (`bone_decode.cpp:1395`).
  Reading it as a bone index refused 15 of the models `sp_a1_intro1` places, and
  **every one of them still parsed as a file**; only loading the real game
  showed it.

  Six things about it that read as bugs until you check the reference.
  **A button is pressed by an *input*, not by a call**: the trigger posts
  `PressIn` at its owner where Valve calls `m_pOwnerButton->TriggerStartTouch`
  directly, because a handler cannot dispatch into another class. It costs one
  extra event and **no tick** — the queue restarts from the head, so the chain
  lands inside the tick the touch happened in. **The rest of `CDynamicProp`
  is still absent**: bone followers, `VPhysicsInitStatic`, prop data, LOS
  blocking and fade distances, none of which has anything here to drive it —
  and `m_nSkin`, which is parsed and printed by `ent_dump` and not drawn,
  because skin families are `portdocs/STUDIO.md` stage 6's.
  **`SetSkin( button_off_skin )` runs after
  the `skin` key is read**, so a map cannot choose the starting skin — which is
  why all 18 shipped `skin` keys are `0`. **`SetParent` on the trigger is
  skipped** and nothing is lost: not one of the 65 has a `parentname` and none
  is a mover. **`UpdateOnRemove` is skipped too**, so a killed button orphans
  its trigger — and no connection in any shipped map fires `Kill` at one; an
  orphan does nothing, because its owner handle stops resolving and its filter
  then refuses everything. And **the three sibling classes are deliberately not
  here**: `prop_floor_cube_button` and `prop_floor_ball_button` accept *only*
  cubes and balls, and `prop_weighted_cube` is not ported, so in this port they
  would be furniture nothing could ever press.

  The measurement that says *this* works is
  `server::tests::every_shipped_floor_button_presses_when_stood_on` — the
  `SOLID_OBB` half of the trigger test, needing no collision data at all. For
  every button in the game it reloads the level, puts the player's hull centre
  on the pad's box centre (which is inside it whichever way the pad faces, and
  some are on walls) and then walks away. **65 press, 65 release.**
  The measurement that says the *model* works is
  `engine::world::entities::tests::the_button_draws_and_moves_as_it_presses`,
  which renders `sp_a1_intro1`'s button headlessly from four feet away: 24,889
  of 65,536 pixels drawn, **11,467 of them different between the two ends of
  `down`**, and the held-`up` image pixel-identical to `down` at cycle 0 —
  which is what says the pose reaches the right geometry rather than just some
  geometry. To watch one
  do something, load **`sp_a1_intro5`**, where `button_1-button` drives a
  `func_door` (`stair_ramp_door`) open and shut through a
  `func_instance_io_proxy` and a pair of `logic_relay`s; `sp_a1_intro1`'s drives
  an `env_texturetoggle`, which is not ported, so there the chain runs and
  nothing moves.

  **Stage 5 is the player as a whole entity, and its headline is that
  `trigger_hurt` kills.** `src/server/damage.rs` is `CTakeDamageInfo`, the
  `DMG_*` table, `m_takedamage`, `m_lifeState` and the health arithmetic;
  `classes/player.rs` is `CBasePlayer`'s damage and death path plus
  `logic_playerproxy` (9) and `player_loadsaved` (9). **38 classnames, 26,044
  of the game's 60,925 entity blocks** — continuing the count the stages above
  use, which is every class registered bar `player`; **36 of them are among the
  200 classnames the shipped maps actually place**, the other two being
  `trigger_portal_button` (created by a `prop_floor_button`) and `light_glspot`
  (registered because Valve registers it). `noclip` moved here from `src/client/`
  and brought `god`, `kill` and `hurtme` with it — they are its neighbours in
  `game/server/client.cpp` — and `client/` gained the dead player's movement:
  `MOVETYPE_FLYGRAVITY`, `FullTossMove`, and an eye that drops from 64 units to
  14.

  The measurement is
  `server::tests::every_shipped_trigger_hurt_kills_the_player_standing_in_it`:
  for **every one of the game's 215 `trigger_hurt`s** it reloads the level,
  finds a point inside the trigger's actual brushes that a 32×32×72 hull fits
  in, puts a player there and runs sixteen seconds without moving it. **138
  kill**, the fastest on the first tick and the slowest after 9.78 seconds —
  the one `damage 10` trigger in the game — and all 138 reach `RespawnPlayer`
  and ask the engine for the level back. The other 77 are each accounted for:
  72 are never touched (73 carry `StartDisabled 1`), 4 are touched and refused
  (no `SF_TRIGGER_ALLOW_CLIENTS`, or a filter), and 1 has nowhere to stand.

  **The two-clocks question is answered and the answer is no.** The plan lists
  "the movement moving to the server"; §5 of the same document already contains
  the argument against it — `CPlayerMove::RunCommand` runs the movement on the
  fixed tick and `CPrediction` re-runs *the same code* on the client, so a
  one-process port with no `net/` already has the client half, and moving it
  would buy a 64 Hz camera with no interpolation and nothing else. **What was
  wrong was the *authority*.** Four fields of `PlayerState` became the server's
  — `move_type`, `health`, `life_state` and `flags` — and `set_player_state`
  now ignores what arrives in them, which is what makes `noclip`, damage and
  death possible at all: each is a value the client would otherwise overwrite
  on the next rendered frame.

  Six findings. **Damage had to be deferred by one dispatch**, and the shape
  was already in the module: `Context::take_damage` queues exactly the way
  `create_entity` queues a spawn and `EntityCore::remove` queues a deletion,
  because applying damage runs the *victim's* virtuals and the hurter has been
  lifted out of the entity list. It costs no tick, and the two gates a caller
  branches on are still evaluated synchronously — three deferral mechanisms now
  share one shape.
  **`logic_playerproxy` is the payoff and all of it is on the default map**:
  nine in the game, and **every one of the five output connections in the
  entire game is on `sp_a1_intro1`** — three `OnJump`, one `OnDuck`, one
  `OnUnDuck` — so jumping there now fires three relays. Two things about it are
  measurements rather than omissions: every input it has in Portal 2 is a
  portal-gun or grab-controller input (`RequestPlayerHealth`/`SetPlayerHealth`
  are `#if defined HL2_EPISODIC && !defined( PORTAL2 )`), so the class accepts
  **none** and its `PlayerHealth` output cannot fire at all — and **`PlayerDied`
  is declared and fired by nothing** anywhere in the tree; the one textual hit
  is a Squirrel function name.
  **Portal 2 has two ways of dying and only one is damage**: `player_loadsaved`
  is what happens when you fall into the abyss in `sp_a3_portal_intro`, where
  there is no `trigger_hurt` at all — the map freezes the player, fades the
  screen and reloads. Nine entities, 11 `Reload` connections, seven named some
  variation of `fade_to_death`.
  **Fall damage is deleted rather than deferred, and the tree says so in
  words**: `CPortalGameRules::FlPlayerFallDamage` is
  `{ return 0.0f; } //no fall damage in portal` (`portal_gamerules.h:61`).
  Nothing in Portal 2 can be killed by landing, which is exactly why 34 of the
  215 `trigger_hurt`s carry `DMG_FALL` — the pit does the killing and the
  damage type is a label.
  **The `health` key is carried by 682 entities and every one writes `0`** —
  346 `func_door_rotating`, 272 `func_door`, 64 `func_button`, the three ported
  classes whose `Spawn` makes them shootable above zero — so the whole
  shootable-brush path is dead in Portal 2 and the key leaves the depot test's
  unhandled table by being *consumed* rather than by being implemented.
  And **one number in the damage path is not recoverable**:
  `CPortal_Player::OnTakeDamage` multiplies every hit by `sk_dmg_take_scale1`,
  which is declared `extern` here, defined in an `hl2_gamerules.cpp` this tree
  does not contain, and set by no `.cfg` and no VPK in the depot. It is 1, with
  one definition site — and it barely matters, because the weakest
  `trigger_hurt` in the game deals 10 a second against 100 health and 202 of
  the 215 deal 100 or more, so any scale between about 0.1 and 10 kills the
  player in the same place.

  Eight more rules produce a plausible wrong answer rather than an error
  (`rustdocs/SERVER.md` gotchas 52-59), and three decide whether anything dies:
  **`Context::take_damage` aimed at yourself is silently dropped**, because the
  dispatched entity is not in the list the queue resolves against — hurting
  yourself calls `self.on_take_damage` directly, which is what the C++
  compiles to anyway; **`EntityCore::is_alive` is the life state and
  `CGameMovement::IsDead` is the health**, and they disagree for exactly the one
  dispatch between the subtraction and `Event_Killed`, which is why
  `PlayerState` carries the health; and **`m_flDamage` is per second and a dose
  is per think** (`m_flDamage * dt`, `dt` 0.5), so dealing the key's value per
  dose doubles the lethality of every `trigger_hurt` in the game.

  **One behaviour that reads as a bug and is Valve's:** a `trigger_hurt` keeps
  firing `OnHurtPlayer` at a corpse, every half second until the level reloads —
  six more times. The dead player going `FSOLID_NOT_SOLID` only stops the
  *player* testing triggers and a stationary trigger never re-tests its own, so
  the link survives; `m_takedamage` stays `DAMAGE_YES`, because
  `CBaseCombatCharacter::Event_Killed` does **not** chain to
  `CBaseEntity::Event_Killed`; and `TakeDamage` returns `void`, so `HurtEntity`
  cannot see the refusal. It is bounded, and it is also why `god` mode leaves a
  scripted chamber usable rather than wedged.

  **`sp_a1_intro1` places no `trigger_hurt` at all**, so the default map cannot
  kill you — `sp_a1_intro5` is the nearest that can, and is already the map to
  load for the floor button. What `sp_a1_intro1` does have is the
  `logic_playerproxy`.

  Not implemented, and each is a class or a subsystem: the weapon
  (`weapon_portalgun`, 3 placed, and it needs the portal system), the armour
  (Portal has none), drowning, the HEV suit, and everything else that can hurt
  you — turrets, crushers, `prop_physics`. `trigger_hurt` is the whole damage
  surface the shipped maps reach.

  **`prop_dynamic` landed after stage 5, and it is the biggest class in the
  game.** `CDynamicProp` across its four classnames — `prop_dynamic` (8,072),
  `prop_dynamic_override` (390), and `dynamic_prop`/`prop_dynamic_glow`, which
  Valve registers and no map places — is **8,462 entities across 105 of the 106
  maps**, ten short of `logic_relay` and ahead of everything else. It carries
  **5,311 `SetAnimation` connections**, more than any other input in the game
  reaches an implemented class, and 1,141 distinct sequence names. That takes
  the port to **43 registered classnames and 34,506 of the game's 60,925 entity
  blocks** — 38 of them among the 200 the maps place. **`sp_a1_intro1` gains 90
  of them from 52 models**, so the default map's signage, pipework and panels
  draw for the first time.

  The plan's "Beyond" list had it as *"needs `studio/` stage 6 and skin
  families"* and **that was wrong**: skin families decide which texture a model
  wears, `m_nSkin` is `0` on 7,808 of the 8,462, and what the class needed was
  what `studio/` already had after `prop_floor_button` — bones, sequences and
  the RLE animation blocks.

  Six findings.
  **The classname is behaviour, and a rename two statements later hides it.**
  `CDynamicProp::Spawn` promotes a `SOLID_NONE` prop to `SOLID_OBB` only
  `if ( FClassnameIs( this, "prop_dynamic" ) )` — and *then* renames
  `prop_dynamic_override` to `prop_dynamic`. So 2,622 props take the promotion
  and **211 `_override`s with the identical `solid 0` do not**.
  `CBaseProp::KeyValue` asks the same question about `health`, swallowing the
  key for everything but an `_override`; all 344 shipped keys write `0`.
  **`GotoSequence` deletes on a measurement.** The sequence *transition graph*
  — `$node`/`$transition`, which walks an NPC from "stand" to "crouch" through
  an intermediate — opens with "bail if we're going to or from a node 0", and
  across the **2,597 sequences of the 606 models the game's props name, not one
  has a non-zero entry or exit node and not one has `nodeflags`**. So no other
  branch is reachable, `m_iTransitionDirection` is `+1` everywhere, and every
  sequence starts playing *forwards* — which is why the 427 `SetPlaybackRate
  -1` connections in the game violate Valve's own `Assert` in `AnimThink`.
  **The server needed a fact from a `.mdl` and the answer is a table, not a
  call.** `AnimThink` fires `OnAnimationDone` (181 connections, **10 of them on
  `sp_a1_intro1`**) and reverts a finished animation to `DefaultAnim` (2,416
  props); both need the sequence's duration, and `server/` names no `studio`
  type. `src/server/sequences.rs` holds the *answers* — a duration and a loop
  flag per model and label — and `Engine::load_level` fills it in from the
  models `World::load_entity_models` has just read. Same shape as `world/`'s
  `Placement`, pointing the other way.
  **It forced a third answer onto that lookup.** A level loads `World::load` →
  `Server::level_init` → `World::load_entity_models`, and it cannot load in any
  other order, because the models an entity places are named by the entities.
  So **every `Spawn` in the game runs against an empty table**, and "nobody has
  loaded this model" has to be told apart from "it is loaded and has no such
  sequence": `Lookup::Unknown` succeeds where `LookupSequence` would have and
  yields no duration, so an animation with no model never finishes.
  **`ParsePropData` is a deletion, and it costs twelve entities.** A plain
  `prop_dynamic` whose model carries a `prop_data` block is removed at load by
  the shipped game with a `DevWarning`; an `_override` is not, which is what
  that classname is *for*. 15 of the 606 models have such a block and 106
  entities wear one — but **94 of the 106 are `prop_dynamic_override`**, so not
  porting the whole propdata system leaves **12 entities across three maps**
  drawn that the shipped game deletes.
  And **`$includemodel` was the measured gap, it belonged to `studio/`, and it
  is done** (above): nine of the 606 models keep their sequences in a companion
  `*_animation.mdl` and **926 entities wear one**. Of the game's 2,416
  `DefaultAnim` keys 2,233 name a sequence that exists somewhere — **849 only
  through an include**, which is what the merge bought — and **183 name one
  that is in no model at all**, Valve's own map errors, which the shipped game
  answers with a `Warning`. A second-order effect came with it: an animation
  that can now *end* fires `OnAnimationDone`, so the game's 5,311
  `SetAnimation` connections start more props animating than the maps'
  `DefaultAnim` keys alone do — 2,738 props are playing a sequence two seconds
  in, where 2,563 were.

  Six more rules produce a plausible wrong answer rather than an error
  (`rustdocs/SERVER.md` gotchas 60-65). **A prop's playback rate starts at
  zero, not one** — `CBaseProp::Spawn` sets it and only `ResetSequenceInfo`
  puts it back to 1, which is what makes the 6,046 props with no `DefaultAnim`
  stand perfectly still rather than looping their first sequence. And two
  divergences follow from the port *deriving* the cycle instead of accumulating
  it: **`AnimThink` cancels itself** once its sequence cannot end (looping,
  zero-length, or a model that never loaded) where Valve re-arms it at 10 Hz
  for the rest of the level, and **`SetPlaybackRate` re-bases `m_flCycle` and
  `m_flAnimTime`**, which Valve does not touch — without it each of those 427
  `-1`s would snap its prop to a different frame before running it backwards.
  And the one that was a real bug on the default map: **an empty sequence label
  is `m_nSequence`'s zero, not "no animation"** — the seam carries a *label*
  where Valve networks an `int`, and an `int` starts at 0 where a label starts
  empty. `Spawn` calls `PropSetAnim` only for a prop with a `DefaultAnim`, and
  `PropSetAnim` answers a name the model does not have with an explicit
  `SetSequence( 0 )`, so **every prop in the game is posed by some sequence**
  and the bind pose belongs to a model with none.

  **That matters because a bind pose is not a pose anybody ever looked at.**
  For most models it happens to equal sequence 0 frame 0; for **20 of
  `sp_a1_intro1`'s 91 entity models** it does not.
  `props_motel/hotel_container_furniture01`-`03` — the intro room's dresser,
  wardrobe and desk — bind at `rot_x(+90)` against a `poseToBone` of
  `rot_x(-90)`, whose product is the identity, while their `idle` holds
  `Quaternion64(0.5, 0.5, 0.5, 0.5)`, a 120° turn about `(1,1,1)`, which
  against the same `poseToBone` is `rot_z(+90)`. Drawn from the bind pose all
  three came out a quarter turn wrong and standing inside the bed — and the bed
  is `hotel_container_furniture04`, a **static prop** at the same anchor and
  the same yaw, on a path that never looks at a sequence, so it stayed put and
  the three around it did not. The `.mdl` says so without rendering anything:
  `hull_min`/`hull_max` and the `.vvd`'s own vertex bounds differ by exactly
  that quarter turn.

  It changed three things outside the class. `ModelEntityState`/`ModelEntity`
  are **keyed on an opaque id and carry `visible`**, where they were positional
  and `EF_NODRAW`-filtered — forced by the 556 `Kill` connections aimed at a
  prop and the 1,000 props that are `StartDisabled`. `ModelState::sequence` is
  a `&str` rather than a `&'static str`, because a prop's comes out of the map.
  And `CBaseEntity` gained the `solid` key — measured: **only the prop family
  writes it** — and the `DisableDraw`/`EnableDraw` inputs, whose 206 shipped
  connections are **all** aimed at a `prop_dynamic`.

  **`prop_testchamber_door` landed after it, and it is the door itself** —
  the big round one at both ends of every test chamber. `CPropTestChamberDoor`
  (`game/server/portal2/prop_testchamber_door.cpp`) is **138 entities across
  71 of the 106 maps, two of them on `sp_a1_intro1`**, carrying 247 output
  connections and 296 input ones. That took the port to **44 registered
  classnames and 34,644 of the game's 60,925 entity blocks** — 39 of them
  among the 200 the maps place.

  **Despite the classname it is not a prop**: it derives straight from
  `CBaseAnimating`, so it has no `DefaultAnim`, no `SetAnimation`, no
  propdata, no `StartDisabled` and none of `CDynamicProp`'s fifteen inputs. It
  is five inputs, four outputs and a playback rate, and it lives in
  `classes/prop.rs` only because what it needs — a model name, a sequence
  label and the five `ModelState` fields — is what that file already has.

  **The whole class is one sequence played in two directions.** `Spawn` resets
  it to cycle 0 of `open` at rate **zero**, which is the shut pose held still;
  `Open` sets the rate to `+1` and `Close` to `-1`. `close`,
  `idleopen` and `idleclose` are looked up into three fields that **nothing in
  the tree ever reads** — and the model says that is deliberate rather than an
  oversight, because `open` is 23 frames and `close` is **36**, so shutting a
  door with `close` would take 1.46 seconds where the shipped game takes 0.92.

  **It is the first thing in the port that needed `fadeouttime`.**
  `IsSequenceFinished()` is what `OnFullyOpen` waits on, and
  `GetLastVisibleCycle` calls a non-looping sequence finished
  `fadeouttime * cycleRate * playbackRate` **before** it ends — so the door's
  0.9167-second `open` is "finished" at cycle 0.782, 0.717 seconds in. Worth
  carrying rather than assuming, and measured: **10,664 of the shipped game's
  10,666 sequences write 0.2 and the other two write 0.5**, so the term never
  folds away. `studio::anim::Sequence`, `server::sequences::SequenceInfo` and
  the `world/` → `server/` seam each grew the field, and the seam's four-tuple
  became a `SequenceRow`.

  Five things about it read as bugs until you check the reference, and the
  first is the big one.

  **`m_bSequenceFinished` is sticky, so only a door's *first* opening reports
  its own end.** Nothing clears the flag but `ResetSequenceInfo`, and this
  class calls `ResetSequence` exactly once, in `Spawn`. So the first
  `OnFullyOpen` waits for the animation — 0.797 seconds on the tick grid — and
  **every later `OnFullyOpen` and every `OnFullyClosed` fires on the first
  think after the input**, 0.094 seconds in, while the door is still visibly
  moving. It is reproduced deliberately: 150 of the game's 247 door output
  connections are `OnFullyClosed` and were authored against it — 29 disable a
  `func_clip_vphysics` and 25 enable a fizzler — so a "correct" door would
  delay every one of them by three quarters of a second. Both numbers are
  identical for all 138 doors, because the whole schedule is quantised.
  **`IsOpen()` is where the door is *going*, not where it is** — it is set the
  instant `Open` is accepted — so a second `Open` during the travel is refused,
  and `LockOpen` opens *and then* locks, in that order, so the open itself gets
  through.
  **`AnimateThink` re-arms unconditionally and this class deliberately does
  *not* take `CDynamicProp::AnimThink`'s cancel-when-idle divergence.** That
  divergence is worth it for 8,462 props; there are 138 doors, two per map, and
  what re-arming buys is the exact 0.1-second grid that decides when those 181
  "fully" connections fire. The cost is one depot number: **`io.thinks` went
  from 4,420 to 7,318, and all 2,898 of those are doors.**
  **`Open`/`Close` must re-base the derived cycle**, the same way
  `SetPlaybackRate` does for a `prop_dynamic` — skip it and a door told to
  `Close` computes its position from the moment it spawned.
  And **the area portal window is parsed and does nothing**: 84 doors name a
  `func_areaportalwindow` and 94 write the fade triple, but all
  `AreaPortalOpen`/`Close` do is write two fade distances on a class that
  belongs to the engine's unported visibility system. The two call sites are
  marked so wiring them up later is one line each.

  Also absent: the bone followers, which *are* a chamber door's whole collision
  in the shipped game — so like every other model in this port it is drawn and
  walked through — and the two sounds.

  **What it looks like** is two rings and two leaves in two acts: the spinner
  rings turn about their own axes over the first 62% of `open`, *inside* the
  door's thickness, and only then do the two halves slide 53 units apart. So
  the first two thirds of the animation draw pixel-identically from outside and
  the doorway clears all at once. That is the model rather than the port, and
  it is what makes the door the test that says the per-bone draw split
  generalises: a floor button is two bone runs with one moving, a door is
  **five with three moving**.

  **To watch one, load `sp_a1_intro1` and walk into the first chamber.** Both
  of its doors are driven by `trigger_once` → `func_instance_io_proxy` →
  `logic_relay` → the door, which is ported end to end, and **130 of the
  game's 138 doors are opened by a chain in their own map** (five of the other
  eight carry no `targetname` at all and three are named and never fired at —
  Valve's dead map data, shut in the shipped game too). **And they shut again**,
  now that `logic_branch_listener` is ported — see below.

  **`logic_branch_listener` landed after it, and it is what shuts those doors.**
  `CLogicBranchList` (`logicentities.cpp:3026`) is **158 entities across 46 of the
  106 maps**, and it is an AND gate over a handful of `logic_branch`es: `Branch01`
  is "the map wants this door shut" and `Branch02` is "the player is not standing
  in the doorway", and when both go true `OnAllTrue` fires the relay that sends the
  door `Close`. That takes the port to **45 registered classnames and 34,802 of the
  game's 60,925 entity blocks** — 40 of them among the 200 the maps place.

  It is small — one `Activate`, one three-way test and three outputs — and what it
  cost was one framework addition and one field. `Context::find_all_by_name` is the
  `while ( pEntity = FindEntityGeneric( pEntity, … ) )` loop that
  `find_by_name`'s first-match form cannot express, and `logic_branch` grew a
  **listener list**: this is the first class in the port where one entity
  *registers* with another rather than sending it an input, which is
  `Context::behaviour_mut` doing exactly what it was written for. The branch then
  posts `_OnLogicBranchChanged` back at each listener when its value moves — an
  ordinary queued input, zero delay, so the whole chain lands inside one tick.
  `portdocs/SERVER.md` §10.3 named this class as the condition that would change
  the borrow shape and **it did not**: stage 4's answer was already enough.

  Three things about it read as bugs until you check the reference.
  **It reports nothing at level start**, because `Spawn` is empty, `Activate` only
  registers and `m_eLastState` begins `NOT_INIT` — so the first output comes from
  the first branch *change*, never from the first evaluation. Every door in the
  game depends on that: both branches of a door's listener read "shut me" at spawn,
  and a listener that tested itself on the way up would slam every door in the map
  closed on tick one. **`SetValue` fires no output of its own and still reaches a
  listener**, because the notification is guarded by the value having changed and
  the output by the input being a `*Test` form — two independent guards, and
  conflating them is silent either way: fold the notification under the output and
  no door in the game ever shuts (1,175 of the 1,601 connections into a branch are
  `SetValue`), or drop the change guard and `Test`'s 308 connections make every
  listener re-report. And **an empty branch list is `OnMixed`**, because neither
  `bOneTrue` nor `bOneFalse` gets set and `DoTest` falls through into the `else`.

  One Valve bug is **not** reproduced, because reproducing it would take more code
  than not: `CLogicBranch::UpdateOnRemove` walks its listener list and then posts
  `_OnLogicBranchRemoved` at *itself* rather than at the listener it just looked up,
  so no listener in the shipped game has ever received one and a dead branch is
  counted as false for the rest of the level. This port reaches the same state by
  having no removal hook at all. **No shipped map fires `Kill` at a `logic_branch`**,
  so the two are indistinguishable.

  **The class is invisible to the 106-map census**, and that is the finding worth
  carrying: in the first two seconds of a level **not one `logic_branch` in the game
  changes value** — a chamber door shuts after the player has walked through it,
  which is minutes in — so every event, input and think total in
  `every_shipped_map_spawns_its_entities` is *identical* with the class registered
  and with it disabled. Its own depot test drives the maps instead: per map it opens
  every chamber door, sets every `logic_branch` true and reads back what each
  listener reported. **350 `Branch*` keys written and 350 resolved; 157 of the 157
  listeners that survive their map's bootstrap report a verdict; and 79 of the 99
  chamber doors on those 46 maps are shut again by one going all-true.** Only 56 end
  up all-true, and that is the map logic rather than a fault — a door's
  `OnAllTrue → logic_relay` chain ends by setting the branch that asked for it back
  to `0`, inside the same tick.
- **Everything else is unported** and lives in `legacy/`.

**Frame cost is measurable and has been measured.** `engine::world::bench` (depot-gated,
`--ignored`) loads a real map, records real passes against a real device with no window in
the way, and times the CPU. **`engine::exposure` is its sibling** — same shape, same
gating — and answers the other headless question: where the exposure settles on a real map
and what the histogram looks like when it gets there. On `sp_a1_intro1` at 1280x720 the
two passes the tone mapper added cost **0.004 ms of CPU a frame** against 0.64 ms for the
scene draw they sit around — read as a ratio rather than as absolutes, because both move
together with thermal state (an earlier run read 0.008 against 1.21). Reach for it before and after any change to the draw path —
the running game cannot be profiled from outside, because macOS stops delivering redraws
to an occluded window and `sample` only ever shows a main thread parked in `mach_msg`.
`sp_a1_intro1` records a whole frame in **1.86 ms** (release, 2.14 with the refracting
pass) / 6.6 ms (debug); it was 12.7 ms when static props first drew, and
`portdocs/STUDIO.md` §11.8 has what the three causes were. Terrain did not move that
number: it added 2 batches and 1,408 triangles to a frame whose cost is 1,080 prop draws.
**`Refract` did move it, and it is the copy rather than the draw** — one full-screen
`copy_texture_to_texture` and one extra pass recording a single prop, which is why
`needs_frame_buffer_copy` gates both and 35 of the game's 106 maps pay neither.
**`prop_dynamic` moved it more than anything since static props**: the five
sub-benchmarks are now 0.25 ms of world brushes, 0.10 of brush models, 1.01 of static
props and **0.72 of entity models**, so the class costs about 60% of what all 1,080
static props do — for 91 instances, because they carry **355,469 triangles against the
props' 224,924** and each bone run is its own draw.
**`prop_testchamber_door` barely moved it**, and the exact numbers are the ones to
quote rather than the timings: 91 instances became 93, 52 models 54, and 355,469
triangles **361,072** — one more model and ten more draws, because a door is five bone
runs and there are two of them. The timed share of the entity-model pass against the
static-prop pass went from 0.71 to 0.76 in a back-to-back run where every figure was
about 3x high, which is the thermal inflation the note below is about.
**The benchmark itself had to be fixed to see that**, and the fix is worth knowing about:
`World::load` cannot read the models an entity places, because they are named by the
entity lump it has just parsed — so `bench` now spawns a `Server` and calls
`load_entity_models`, the same two calls `Level::load` makes. Before that it was silently
measuring a frame with the largest thing in it missing.
Run the five sub-benchmarks on their own — back to back they share thermal
state and read 2-3x high. The two rules that came out of it live in `rustdocs/MATERIALS.md`:
**uniform writes are staged and flushed once per pass, not queued per draw**, and
**redundant pipeline and bind-group state is elided** — the correctness hazard for the
second is A/B/A, not A/B.

Next: **the boot path is complete as far as one player can take it**, the level shell
is geometrically complete — world, brush entities, static props and terrain — **every
model in it is lit the way the shipped game lights it**, it is
**auto-exposed to the map's own limits**, the map's **entity logic runs**, its
**doors and panels move**, **it notices the player**, and **it can kill them**:
triggers fire, filters decide who counts, a shut door is a wall, a floor button
presses when you stand on it, **a chamber door opens when you walk up to it and
shuts behind you**, and a `trigger_hurt` takes your health and restarts the level
when it runs out.
**`portdocs/SERVER.md` is finished** — all five stages — so the game layer's
next steps are individual classes and subsystems rather than a staged plan.
`client/` stage 5 and everything below it needs `net/`, which is a long way from here.
The candidates, in the order they are worth doing:

- **`CPhysicsPushedEntities` — a door that shoves the player.** `trace/` stage 4
  is no longer in the way, so this is unblocked for the first time:
  `physics_main.cpp:130-1130`, ~1,000 lines of speculative push, blocker
  enumeration and rollback, and `EntityCore::local_time` is already the field
  its answer goes in. The condition is the first puzzle that cannot be solved
  without standing on something that moves.
- **The local/abs transform pair on `EntityCore`**, which is smaller than a stage and
  unblocks two things at once: `SetParent`/`ClearParent`/`SetParentAttachment*` —
  **1,103 of the 1,285 inputs the depot test reports as unhandled** — and parented
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
  before skinning buys nothing. `vvd::Vertex` grows a `bones` field, `vtx` stops
  discarding `StripHeader_t`'s bone plumbing, and the bone matrices move to the GPU.
- **`world/`'s 3D skybox** — now that terrain draws, the last structural reason
  `sp_a1_intro1` does not look like the shipped game. A second camera over a second set of
  geometry, plus `sky_camera`'s scale.
- **`world/`'s visibility** (§7.14's PVS, and the areas/areaportals that live in
  `cmodel.cpp` and belong to it). Every face is still drawn every frame.
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

**Resolved:** the **view angles and the free-fly camera** used to live in
`src/engine/input/view.rs`, to be moved "to `client/` when it exists". `client/` stage 1
is that, and the file is deleted rather than moved: `ViewAngles` is the client's,
`MoveButtons` became `Buttons` with the fractional `KeyState` the wart said not to build
against a camera, and `FlyCamera` became a `Player` in `MOVETYPE_NOCLIP` moved by
`FullNoClipMove`. **The one placeholder that outlived it is also gone**: `+jump` and
`+duck` used to drive the vertical axis, because `ComputeUpwardMove` reads
`+moveup`/`+movedown` and Portal 2 binds neither, so without the hack a noclip player
could not rise. Stage 4 made walking real, which makes jump and duck buttons; a noclip
player now flies up the way the shipped game does it, by looking up and holding forward.
`bind SPACE +moveup` brings the axis back.

**Resolved:** **`noclip` used to be registered by the game client and it is a *server*
command.** Move type is server state that gets networked down, so `ConCommand noclip`
lives in `game/server/` in the original; with one process and no server it had to live
somewhere, and `src/client/` was where the move type was. The condition this wart
recorded was exact — "`portdocs/SERVER.md` stage 5, where the move type becomes the
server's state rather than a field on `client::Player`" — and that is what happened.
`Server::toggle_noclip` is the command, `PlayerState::move_type` carries the answer back
*to* the client, and `Client::toggle_noclip` is deleted. `god`, `kill` and `hurtme` came
with it, because they are its neighbours in `game/server/client.cpp`. The other half of
the prediction — "where the movement itself moves" — deliberately did **not** happen; see
`portdocs/SERVER.md` stage 5 for why.

**Resolved:** `CommandLine` used to live in `src/launcher/` and be read from
`src/engine/window/`, to be moved "when a third subsystem needs it". `console/` was that
third subsystem — `stuffcmds` and the `+<cvar>` default seeding both read it — so it now
lives at `src/cmdline.rs`. The move also fixed a real divergence: `CCommandLine::ParmValue`
refuses a value beginning with `-` or `+` (`tier0/commandline.cpp:646`) and the port's
`value()` did not, which would have had `-window` swallow `+map`.

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
