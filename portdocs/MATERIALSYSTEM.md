# MATERIALSYSTEM.md

Porting design doc for `materialsystem/` (plus `togl/`, `public/materialsystem/`,
`public/shaderapi/`) → `src/materials/`.

Read [`../PORTING.md`](../PORTING.md) first. Paths here are relative to the original
tree; prefix them with `legacy/` to open them.

**Status: stages 1-4 of §9 done** — `wgpu`/`winit` bring-up (a cleared window), the
texture path (`.vtf` → `wgpu::Texture`, with the error checkerboard), the material path
(`.vmt` → `Material` → `UnlitGeneric` in WGSL), and meshes plus the render context (typed
vertex buffers, static and dynamic geometry, render targets, and a depth buffer). Stages
5-8 not started. The implemented API is documented in
[`../rustdocs/MATERIALS.md`](../rustdocs/MATERIALS.md); read that before calling into
`src/materials/`, and this document before extending it.

**Headline decision (settled):** the `IShaderDeviceMgr` / `IShaderDevice` / `IShaderAPI`
/ `IShaderShadow` tower is **deleted, not ported**. `wgpu` is used *directly* from inside
the material system — there is no device-abstraction layer, no "shader API" interface,
and no second implementation to swap in. Everything `shaderapidx9/`, `glmgr/`, `togl/`
and `ps3gcm/` do is either provided by `wgpu` or is D3D9 bookkeeping that exists only
because D3D9 lacked something `wgpu` has.

---

## 1. Why this module is the pivot

PORTING.md's boot path is: bootstrap → filesystem → windowing (`winit`) → rendering
(`wgpu`, enough to clear a frame) → engine host loop → map loading → game layer.

**"Rendering" in that list *is* this module.** There is no separate renderer to port —
`engine/` draws the world by calling `IMatRenderContext`, and everything below that call
is `materialsystem` and its shader-API backend. So this doc covers the whole `wgpu`
groundwork step, and it lands *before* the engine frame loop, deliberately: the shape of
frame submission (command encoders, surface acquire/present, when a pipeline can be
created) constrains how the host loop can be written.

Prerequisite: **`filesystem` must be usable first.** Materials (`.vmt`), textures
(`.vtf`) and precompiled shaders (`.vcs`) are all loaded through `g_pFullFileSystem`
from search paths and VPKs. See [`FILESYSTEM.md`](FILESYSTEM.md).

## 2. Inventory

Line counts are whole-subtree, `.cpp`/`.h`/`.inl`/`.mm`, not all of it compiled today.

| Path | Lines | Disposition |
|---|---:|---|
| `materialsystem/` (root, 62 files) | 48,391 | **The actual port target.** ~30k after cuts below |
| `materialsystem/shaderapidx9/` (71 files) | 77,721 | **Delete.** D3D9 backend. Mine 4 files for knowledge (§6) |
| `materialsystem/stdshaders/` (230 files) | 54,303 | 166 `.cpp` shader classes + 256 `.fxc` HLSL. **Rewrite, heavily culled** (§7) |
| `materialsystem/ps3gcm/` (38 files) | 22,198 | **Delete.** PS3 out of scope |
| `materialsystem/glmgr/` (9 files) | 15,650 | **Delete.** D3D9→GL translation |
| `materialsystem/shaderapiempty/` (1 file) | 2,741 | **Delete.** Null backend |
| `materialsystem/shaderlib/` (5 files) | 1,348 | **Delete.** Shader-DLL registration boilerplate |
| `materialsystem/xbox/` | — | **Delete** |
| `togl/` (21 files) | 29,841 | **Delete.** The other half of the D3D9→GL layer |
| `public/materialsystem/*.h` (37 files) | 10,635 | Read as spec, then discard |
| `public/shaderapi/*.h` (10 files) | 2,348 | Read as spec, then discard |
| **Total legacy surface** | **~262,500** | |
| **Realistic port target** | **~30–35k** | plus a hand-written WGSL shader set |

### `materialsystem/` root, by size

| File | Lines | Disposition |
|---|---:|---|
| `ctexture.cpp` | 5,832 | **Port** — VTF load, mips, render targets, procedural, streaming. Sheds console/streaming bulk |
| `cmaterialsystem.cpp` | 5,699 | **Port** — the god object; much of it is lifecycle/`DELEGATE_TO_OBJECT` glue that evaporates |
| `cmaterial.cpp` | 3,729 | **Port** — VMT parse, shader resolution, fallbacks, precache/uncache, refcounting |
| `cmatrendercontext.cpp` | 3,455 | **Port, restructured** — render state stack, matrix stack, RT stack, frame begin/end |
| `mat_stub.cpp` | 2,606 | **Delete** — `mat_stub` cvar's no-op passthrough of every interface |
| `cmatqueuedrendercontext.cpp` + `.h` | 3,330 | **Delete** — see §5.3 |
| `cmatlightmaps.cpp` + `.h` | 2,664 | **Port** — lightmap page allocation/update. BSP-fixed data layout |
| `morph.cpp` | 2,283 | **Defer** — GPU morph (flex) path; CPU fallback lives in `studiorender` |
| `cmaterialvar.cpp` | 2,105 | **Port, collapses hard** — becomes a Rust enum (§4.2) |
| `shadersystem.cpp` + `.h` | 2,099 | **Port, restructured** — snapshot generation + `DrawElements` (§4.3) |
| `texturemanager.cpp` + `.h` | 1,822 | **Port** — texture dictionary, defaults, error checkerboard |
| `CMaterialSubRect.cpp` | 1,140 | **Probably delete** — atlas sub-rect materials; no callers found outside the module (verify) |
| `composite_texture.cpp` + `.h` | 1,260 | **Delete** — CS:GO weapon-finish compositing |
| `CColorCorrection.cpp` | 992 | **Port** — 3D LUT color correction. Portal 2 uses this heavily |
| `cmatnullrendercontext.cpp` | 987 | **Defer** — headless path; see §5.4 |
| `cmaterialsystem_ps3fonts.cpp` | 798 | **Delete** |
| `checkmaterials.cpp` | 668 | **Skip** — `mat_checkmaterials` debug command |
| `colorspace.cpp` + `.h` | 668 | **Port the math** — bumped-lightmap exponent encoding, fixed by BSP format |
| `custom_material.cpp` + `.h` | 554 | **Delete** — CS:GO custom materials |
| `cmaterial_queuefriendly.cpp` + `.h` | 553 | **Delete** — with the queued context |
| `cmatpaintmaps.cpp` + `.h` | ~520 | **Port** — **Portal 2 essential** (§8) |
| `occlusionquerymgr.cpp` + `.h` | 367 | **Port, small** — `wgpu` has native occlusion queries |
| `base_visuals_data_processor.cpp` | ~200 | **Delete** — CS:GO |
| `SubdMgr.cpp` | 170 | **Delete** — DX11 subdivision surfaces, unused |
| `imagepacker.cpp` + `.h` | ~290 | **Port faithfully** — lightmap atlas packing |
| `cmaterialdict.cpp` + `.h` | 340 | **Port** — material registry; becomes a `HashMap` + handle table |
| `shader_dll_verify.cpp` | 99 | **Delete** — anti-tamper for the shader `.so` that no longer exists |

### Adjacent modules this port drags in

These aren't `materialsystem/` but you cannot do textures or draw a model without them:

| Module | Lines | Note |
|---|---:|---|
| `vtf/` | 6,230 | **Done** — `src/materials/vtf.rs`, ~350 lines of it. The other 5,900 are `vtex`'s content pipeline compiled into the engine (mip generation, spheremap projection, cubemap border blending), which a reader does not need |
| `bitmap/` | 10,550 | **Mostly done** — the format table, size arithmetic and CPU conversions are `src/materials/image_format.rs`; hardware decodes DXT, so no decoder was needed. What is left is mip *generation* and resampling, which only a content tool wants |
| `studiorender/` | 18,725 | Consumes `IMatRenderContext` + `IMesh`. Separate port; constrains the mesh API |
| `vgui2/matsys_controls/` | 13,552 | **Delete** — → `egui` |

## 3. Dependency graph

**Downward (what it needs):** `filesystem` (VMT/VTF/VCS reads), `mathlib` (→ `glam`),
`bitmap`+`vtf` (image decode), `tier1` `KeyValues` (→ a real VMT parser), `ILauncherMgr`
for the window handle (`g_pLauncherMgr`, set in `CMaterialSystem::Connect`,
`cmaterialsystem.cpp:206`) → **`winit`**.

**Upward (fan-in).** `materialsystem/imaterialsystem.h` is included by ~50 files; the
concentrations that matter:

- **`engine/`** — the dominant consumer. World rendering, `matsys_interface.cpp`,
  `shadowmgr.cpp`, `staticpropmgr.cpp`, `r_studio*.cpp`, `paint.cpp`.
- **`game/client/`** — 14 includers. View rendering, particles, effects, screen-space
  post, material proxies.
- **`studiorender/`** — model drawing; the heaviest single user of `IMesh`/`CMeshBuilder`.
- **`vgui_surfacelib/` + `MatSystemSurface`** — UI drawing. **Goes away with `egui`.**
- **`scaleformuiimpl/`, `rocketuiimpl/`** — **go away with `egui`.** These are also the
  only reason `IShaderDeviceDependentObject` (device-lost callbacks) exists.
- Tools (`vbsp`, `vtfcombine`, `toolutils`, `perftest`) — out of scope.

`IShaderAPI` itself leaks upward in a handful of places (`client/`, `MatSystemSurface`,
`shadowmgr.cpp`, `staticpropmgr.cpp`). Those are the sites where the abstraction was
already failing; each needs a real, narrow Rust API rather than a raw device handle.

## 4. The architecture you need in your head

Five layers in the original, top to bottom:

```
  engine / game / studiorender
        │  IMaterial, IMatRenderContext, IMesh
  ┌─────▼──────────────────────────────────────┐
  │ CMaterialSystem  (cmaterialsystem.cpp)     │  materials, textures, lightmaps,
  │   ├── CMatRenderContext                    │  render targets, frame lifecycle
  │   └── CShaderSystem  (shadersystem.cpp)    │  shadow/dynamic dispatch, snapshots
  ├────────────────────────────────────────────┤
  │ IShader implementations (stdshaders/)      │  per-material-kind draw logic
  ├────────────────────────────────────────────┤
  │ IShaderAPI / IShaderShadow / IShaderDevice │  ← DELETED
  │   shaderapidx9/  (D3D9-shaped)             │
  ├────────────────────────────────────────────┤
  │ togl/ + glmgr/  D3D9 → OpenGL translation  │  ← DELETED
  └────────────────────────────────────────────┘
```

The port collapses the bottom three into `wgpu`.

### 4.1 Materials are VMT files bound to a shader

A `.vmt` is a `KeyValues` document naming a shader (`LightmappedGeneric`,
`VertexLitGeneric`, `UnlitGeneric`, …) plus parameters (`$basetexture`, `$bumpmap`,
`$translucent`, …). `CMaterial` parses it, resolves the shader by name, applies
*fallbacks* (a shader can redirect to a cheaper variant based on hardware caps), inits
parameters, loads referenced textures, and then asks `CShaderSystem` to compute its
render state. Materials are refcounted and can be uncached/recached on level transitions.

**Port faithfully:** the `.vmt` grammar and parameter semantics — content comes from
Valve's depot, so this is a fixed format. **Discard:** `KeyValues`, the fallback-shader
mechanism (hardware caps no longer vary the way they did in 2004), the four-phase
init/precache lifecycle.

### 4.2 `IMaterialVar` is a dynamic variant

`cmaterialvar.cpp` (2,105 lines) implements a hand-rolled tagged union over float / int /
vector / matrix / string / texture / material, with a fast-path accessor for each. In
Rust this is:

```rust
enum MaterialVar {
    Float(f32), Int(i32), Vec(Vec4), Matrix(Mat4),
    Str(String), Texture(TextureHandle), Material(MaterialHandle),
}
```

Most of those 2,105 lines are coercion rules and dirty-flag bookkeeping. Read them for
the *coercion semantics* (a `.vmt` may write `"1"` where a vector is expected), keep
those, discard everything else.

**Material proxies** (`IMaterialProxy`) let game code mutate material vars per-bind —
6 files in `game/` implement them. Keep the concept (a per-frame `fn(&mut MaterialVars)`
hook); discard the `CreateInterface`-registered factory.

### 4.3 The shadow/dynamic two-phase model — *the important one*

From `shadersystem.h`'s own header comment: anything affecting **vertex format or fixed
pipeline state** must be set during the **shadow/snapshot phase**; anything driven by a
material var at draw time happens in the **dynamic phase**.

Each `IShader` (in `stdshaders/`) is written once and runs twice:

- **Shadow pass** (`IShaderShadow`): declares blend/depth/cull/vertex-format/texture
  stages. The result is hashed into a `StateSnapshot_t` — an immutable, deduplicated
  state object. `TransitionTable.cpp` then computes the minimal D3D9 state-change
  sequence between any two snapshots.
- **Dynamic pass** (`IShaderDynamicAPI`): binds textures, uploads constants, picks the
  shader combo, issues the draw.

A material's `ShaderRenderState_t` holds up to `MAX_RENDER_PASSES = 3` snapshots per
modulation variant, up to `SNAPSHOT_COUNT_NORMAL = 8` variants.

**This is the port's biggest gift.** Valve invented immutable pipeline state objects
because D3D9 didn't have them. `wgpu` has them natively:

| Valve concept | `wgpu` equivalent |
|---|---|
| `StateSnapshot_t` | `wgpu::RenderPipeline` |
| `TransitionTable` (minimal state deltas) | **Nothing — delete it.** The driver does this |
| Shadow-phase `IShaderShadow` calls | `RenderPipelineDescriptor` fields |
| `VertexFormat_t` (uint64 bitfield) | `VertexBufferLayout` derived from a typed struct |
| Dynamic-phase constant sets | `BindGroup` + uniform buffer writes |
| `DrawElements` | `RenderPass::set_pipeline` + `set_bind_group` + `draw_indexed` |
| Static shader combos | Pipeline variants, keyed and cached |
| Dynamic shader combos | Uniform-driven branches, or a small number of extra variants |

So the Rust shape is: a `PipelineCache: HashMap<PipelineKey, RenderPipeline>` where
`PipelineKey` is the moral equivalent of a snapshot, and each material kind is a Rust
type with two methods — one producing a `PipelineKey`, one binding per-draw resources.
That's the same two-phase idea with the encoding thrown away, which is exactly what
PORTING.md asks for.

### 4.4 Meshes and `CMeshBuilder`

`public/materialsystem/imesh.h` is **4,402 lines**, almost all of it an inlined
`CMeshBuilder`/`CVertexBuilder` that writes interleaved vertex data one attribute at a
time through a `VertexFormat_t` bitfield, with compile-time-templated compressed-normal
and compressed-bone-weight paths.

**Do not port this.** The Rust replacement is typed vertex structs plus `bytemuck`:

```rust
#[repr(C)] #[derive(Clone, Copy, Pod, Zeroable)]
struct WorldVertex { pos: [f32;3], normal: [f32;3], uv: [f32;2], lightmap_uv: [f32;2] }
```

Build a `Vec<WorldVertex>`, upload it. Vertex layouts are derived from the struct, not
decoded from a bitfield at runtime. What *does* carry across:

- The **set** of vertex formats the engine actually uses, because BSP and MDL data
  arrive in fixed layouts. Enumerate them from `imesh.h`'s flags and from what
  `studiorender`/`engine` actually request.
- **Vertex compression** (`VERTEX_FORMAT_COMPRESSED`, 4-byte packed normals, packed bone
  weights) — this is a bandwidth optimization worth keeping, and the packing is defined
  by the shaders that unpack it. Decide once whether to keep it or unpack at load time.
- **Dynamic vertex/index buffer ring allocation** (`shaderapidx9/dynamicvb.h`,
  `dynamicib.h`) — the pattern of sub-allocating from a large buffer that's rotated per
  frame. `wgpu` equivalent is a staging belt / per-frame arena. The *reasoning* is
  identical; the code is not reusable.

### 4.5 Textures and lightmaps

*The lightmap half is ported; see `../rustdocs/MATERIALS.md`'s "Lightmaps" section. The
instruction below to port the packing faithfully was followed, and the reason turned out
to be sharper than stated: the page and the offset within it are not merely baked into
BSP-derived draw data, they are computed from the packer at load and* become *the texture
coordinates.*

- `ctexture.cpp` / `texturemanager.cpp` — a name→texture dictionary with refcounting,
  VTF loading, mipmap handling, render-target creation, procedural (CPU-written)
  textures, the error checkerboard, and texture exclusion/streaming lists.
- `cmatlightmaps.cpp` + `imagepacker.cpp` — the BSP's per-face lightmap samples are
  packed into a small number of atlas pages at map load; `imagepacker` does the
  rectangle packing. **Port the algorithm faithfully** — page indices and UVs are baked
  into the BSP-derived draw data.
- `colorspace.cpp` — lightmap samples are RGBE-style (RGB + shared exponent) and bumped
  lightmaps have 3 directional components. Fixed by the BSP format; port the math.

### 4.6 Hardware config

`IMaterialSystemHardwareConfig` (~50 caps queries: `SupportsCompressedVertices`,
`MaxTextureWidth`, `GetHDRType`, `GetShadowFilterMode`, `NumPixelShaderConstants`,
`MaxUserClipPlanes`, …) plus a "DX level" (`dxlevel` 70/80/90/95) that gates shader
fallbacks and config presets.

**Most of this dies.** `wgpu::Limits` and `wgpu::Features` answer the questions that
still have meaning; the rest describes 2004-era GPU variety that no longer exists. Pick
a single target capability tier (roughly `dxlevel 95` / SM3.0-and-better), drop the
fallback ladder, and keep only the handful of caps that genuinely vary today (texture
compression format availability, max texture size, anisotropy, MSAA sample counts).

Note this deletes a *lot* of transitive complexity: no `dxlevel`, no fallback shaders,
no `mat_dxlevel`, no per-vendor hacks (`NeedsATICentroidHack`, `NeedsAAClamp`).

## 5. What's deleted and why

### 5.1 The whole shader-API tower

`IShaderDeviceMgr` (adapter enumeration, `SetMode(hWnd, …)` returning a
`CreateInterfaceFn`), `IShaderDevice` (present, back buffer format, gamma ramp,
child-window `AddView`/`SetView`, shader compilation, VB/IB creation), `IShaderAPI` (228
virtuals), `IShaderShadow`, `IShaderDynamicAPI` (77 virtuals) — **all deleted.**

Mapped onto `wgpu`:

| `IShaderDevice*` responsibility | Replacement |
|---|---|
| Adapter enumeration, `SetAdapter` | `wgpu::Instance::enumerate_adapters` / `request_adapter` |
| `SetMode(hWnd, ShaderDeviceInfo_t)` | `Surface::configure(&device, &SurfaceConfiguration)` |
| Mode/display enumeration | `winit` monitor/video-mode APIs |
| `Present()` | `SurfaceTexture::present()` |
| `ReleaseResources`/`ReacquireResources`, device-lost | **Gone.** `wgpu` handles surface loss via `SurfaceError::Lost` → reconfigure |
| `CompileShader`, `CreateVertexShader`/`CreatePixelShader` | `Device::create_shader_module` (§7) |
| `CreateVertexBuffer`/`CreateIndexBuffer`/`CreateStaticMesh` | `Device::create_buffer` |
| `AddView`/`RemoveView`/`SetView` (child windows for Hammer) | **Gone.** One window |
| `SetHardwareGammaRamp` | **Gone.** sRGB targets + a tonemap/gamma pass |
| `EnableNonInteractiveMode` (360 loading-screen front-buffer tick) | **Gone** |
| `IShaderDeviceDependentObject` (Scaleform device-lost hooks) | **Gone** with Scaleform |

The `wgpu` `Instance`/`Adapter`/`Device`/`Queue`/`Surface` are **owned directly by the
material system** — a `Renderer` struct inside `src/materials/`, not a trait, not a
separate module pretending to be a device layer. If a second backend is ever wanted,
`wgpu` already *is* the backend abstraction; adding another on top is the mistake this
project exists to avoid.

Consequently `shaderapidx9/` (77.7k), `shaderapiempty/` (2.7k), `glmgr/` (15.7k),
`togl/` (29.8k) and `ps3gcm/` (22.2k) — **148k lines, 56% of the module's total** — are
deleted outright without a Rust counterpart.

### 5.2 Shader DLL loading

`CMaterialSystem::CreateShaderAPI` / `SetShaderAPI` (`cmaterialsystem.cpp:712,748`)
`dlopen`s a named shader `.so`; `shaderlib/ShaderDLL.cpp` registers `IShader`
implementations into a global list; `shader_dll_verify.cpp` checks the result wasn't
tampered with. All of it is the app-system mechanism PORTING.md discards. Shaders become
ordinary Rust types registered in a table at compile time.

### 5.3 The queued render context

`CMatQueuedRenderContext` + `CMatCallQueue` + `cmaterial_queuefriendly` (~3.9k lines of
which a large fraction is 12 arities × 3 variants of `DEFINE_QUEUED_CALL_*` macros)
exist because **D3D9 required all submission from one thread**, so Valve recorded the
client's render calls into a byte-packed command queue on a worker thread and replayed
them on the render thread. `MaterialThreadMode_t` (`SINGLE_THREADED`,
`QUEUED_SINGLE_THREADED`, `QUEUED_THREADED`) selects the mode, and
`CMaterialSystem::m_pRenderContexts[2]` + a thread-local pointer route each thread to the
right context.

`wgpu` records command buffers on any thread and submits them in order. The concept is
native; the machinery is deleted. **But keep the knowledge:** the *reason* for the split
(build draw lists off the main thread, submit once) is still the right architecture, and
the queue boundary tells you exactly which state is per-context and which is global.

### 5.4 Null / stub paths

`mat_stub.cpp` (a full no-op `IMaterialSystem` implementation behind the `mat_stub`
cvar), `cmatnullrendercontext.cpp` and `shaderapiempty/` all serve headless or
render-disabled configurations. Portal 2 single-player doesn't need them at first. If a
headless path is wanted later it should be one enum branch in the renderer, not three
parallel no-op implementations of large interfaces.

## 6. Files in `shaderapidx9/` worth reading before deleting

The directory is deleted, but four files encode knowledge that isn't written down
anywhere else:

- **`hardwareconfig.cpp`** (1,026) — how caps were probed and what each cap actually
  gated. Read it to decide the single capability tier (§4.6).
- **`TransitionTable.cpp`** (1,317) — how snapshots were deduplicated and compared. The
  dedup/hashing strategy informs the `PipelineKey` design even though the transition
  logic itself is dead.
- **`meshdx8.cpp`** (6,931) + **`dynamicvb.h`**/**`dynamicib.h`** — dynamic buffer ring
  allocation, buffer rotation, and the "how much do we allocate per frame" heuristics.
- **`colorformatdx8.cpp`** (797) — `ImageFormat` → API format mapping, including which
  formats were emulated. You need the equivalent table for `wgpu::TextureFormat`.

Ignore `dxabstract.cpp`, `dx9asmtogl*.cpp`, `d3d_async.*`, `shaderapidx10*`,
`textureheap.cpp`, `stubd3ddevice.h`, `dx9hook.h` entirely.

## 7. Shaders → WGSL

**Settled: every GPU program is rewritten in WGSL, translated from the `.fxc` HLSL
sources in `stdshaders/`.** No `.vcs` consumption, no runtime bytecode translation, no
port of the offline compile farm. This is the largest single workstream in the module and
the rest of this section is the plan for it.

### 7.1 Why the shipped shaders are unusable

Shipping game data contains **precompiled `.vcs` blobs** (D3D9 bytecode) under
`shaders/fxc/`, loaded by `shaderapidx9/vertexshaderdx8.cpp` via a `ShaderHeader_t` plus
a static/dynamic combo index (`public/materialsystem/shader_vcs_version.h`). `wgpu`
consumes WGSL or SPIR-V; it cannot consume D3D9 bytecode, and `naga` does not ingest
HLSL. Writing a `.vcs` → SPIR-V translator would recreate `dx9asmtogl2.cpp` — precisely
the tower §5.1 deletes.

The `.fxc` HLSL **is** in the tree (256 files), so this is a translation job against a
reference, not reverse engineering. That is the whole reason the decision is tractable.

### 7.2 What the C++ side actually is

`stdshaders/` has 166 `.cpp` files and 55 declare a shader via
`BEGIN_SHADER`/`BEGIN_VS_SHADER` — but those are mostly **thin wrappers**. There are only
**29 `*_helper.cpp` files**, and those hold the real logic. `unlitgeneric_dx9.cpp`, for
instance, declares ~60 `SHADER_PARAM`s, fills a `VertexLitGeneric_DX9_Vars_t`, and
delegates entirely to `vertexlitgeneric_dx9_helper.cpp`.

So the implementation count is ~29, not 55, and the Portal 2 subset is smaller still.

The `SHADER_PARAM( NAME, TYPE, default, help )` block is a **declarative parameter
table** — name, type, default value, documentation. That is exactly a Rust `const` table
or a derive macro over a params struct, and it is the one part of the C++ shader files
worth transliterating closely, because the parameter names and defaults are `.vmt`
surface area fixed by shipped content.

### 7.3 The combo system, and why it is deleted

Combos are declared in a comment DSL at the top of each `.fxc`:

```
// STATIC:  "BUMPMAP"               "0..2"
// STATIC:  "DETAIL_BLEND_MODE"     "0..12"
// STATIC:  "FANCY_BLENDING"        "0..3"  [ps20b] [ps30]
// STATIC:  "FANCY_BLENDING"        "0..0"  [ps20]
// STATIC:  "CASCADED_SHADOW_MAPPING" "0..1" [ = g_pHardwareConfig->SupportsCascadedShadowMapping() ]
// DYNAMIC: "FASTPATH"              "0..1"
//  SKIP:   ($DETAIL_BLEND_MODE == 9) && ( $BUMPMAP )
```

`STATIC` axes are baked at material-init time; `DYNAMIC` axes are chosen per draw. The
`[ps20] [ps20b] [ps30] [PC] [CONSOLE] [XBOX]` tags gate an axis by target profile, and
`[ = expr ]` pins an axis to a C++ expression. `SKIP` rules prune invalid combinations.
The C++ side selects them with `DECLARE_STATIC_PIXEL_SHADER` +
`SET_STATIC_PIXEL_SHADER_COMBO( NAME, value )` (see
`lightmappedgeneric_dx9_helper.cpp:715+`).

**The scale:** `lightmappedgeneric_ps2x.fxc` alone declares 18 static axes whose cartesian
product is **~15.3 million variants**, pruned by 32 `SKIP` rules. This is why
`utils/shadercompile` + `utils/shadercompile_launcher` exist — a *distributed compile
farm* whose only job was building these. All of it is deleted; nothing outside the shader
system depends on combo IDs.

**Replacement policy — sort every axis into one of three buckets:**

1. **Pinned at compile time — delete the axis.** Anything constant for our single
   capability tier (§4.6): `SHADER_SRGB_READ`, `LIGHTING_PREVIEW`, every `[CONSOLE]` /
   `[XBOX]` / `[SONYPS3]` gate, every `[ps20]`-only branch (keep the `[ps20b] [ps30]`
   side), `CSM_MODE`, tools-only axes. This alone removes most of the product.
2. **Uniform branch — one shader, an `if` on a flag word.** Per-draw `DYNAMIC` axes
   (`FASTPATH`, `FASTPATHENVMAPCONTRAST`, `WRITEWATERFOGTODESTALPHA`,
   `FLASHLIGHTSHADOWS`) and cheap material features that only gate a texture fetch or a
   blend (`SELFILLUM`, `ENVMAPMASK`, `BASEALPHAENVMAPMASK`, `DETAIL_BLEND_MODE`,
   `ENVMAPANISOTROPY`). Modern GPUs handle uniform-control-flow branches essentially free.
3. **A real pipeline variant — only when it changes the pipeline.** Vertex layout changes
   (`BUMPMAP` needs tangents; `BASETEXTURE2`/`SEAMLESS` need different vertex data) or
   fixed-function state (blend/depth/cull). Expect **single digits per shader**, and each
   one is a `PipelineKey` field (§4.3).

Do the bucketing explicitly per shader as the first step of porting it, and write the
result down — it is the actual design work, and it is where the 15.3M becomes ~5.

### 7.4 The constant-register ABI — port this deliberately

`common_hlsl_cpp_consts.h` is literally a header **shared between C++ and HLSL** to keep
register assignments in sync. The register map is a global ABI that every shader assumes,
and it is the most directly reusable thing in the whole shader tree — it tells you the
engine's real per-frame / per-draw / per-material data split:

| Register | Symbol | Frequency → Rust binding |
|---|---|---|
| VS `c1` | `cConstants1` | per-frame uniform |
| VS `c2` | `cEyePos_WaterHeightW` | per-frame |
| VS `c4..c7` | `cModelViewProj` (`float4x4`) | per-draw |
| VS `c8..c11` | `cViewProj` (`float4x4`) | per-frame |
| VS `c13` | `cFlexScale` | per-draw (morph) |
| VS `c16` | `cFogParams` | per-frame |
| VS `c17..c20` | `cViewModel` (`float4x4`) | per-frame |
| VS `c27+` | `cLightInfo[2]` or `[4]` | per-draw light block |
| VS `c37` / `c47` | `cModulationColor` | per-draw |
| VS `c48+` | `cModel[16]` (`float4x3`) | skinning — storage buffer |
| VS `c1024+` | `cFlexWeights[512]` | morph weights — storage buffer |
| PS `c28` | `cFlashlightColor` | flashlight block |
| PS `c29` | `g_LinearFogColor` | per-frame |
| PS `c30` | `cLightScale` (tone mapping) | per-frame |
| PS `c31` | `cFlashlightScreenScale` | flashlight block |
| PS `c32` | `cScreenSize` | per-frame |
| PS `c13`, `c15` | depth-feather proj/viewport | per-frame |

Note what this tells you: **`cModel[16]` and `cFlexWeights[512]` are why SM3.0 constant
registers ran out** — bone matrices and morph weights are bulk data, and in `wgpu` they
are storage buffers, not uniforms.

Recommended bind group layout, derived from the frequency column:

- `group(0)` — per-frame (view/proj, eye pos, fog, tonemap, screen size, time)
- `group(1)` — per-material (textures, samplers, material params, feature flag word)
- `group(2)` — per-draw (model matrix, modulation color, lights) via dynamic offsets
- `group(3)` — skinning + morph storage buffers, bound only for skinned draws

The Rust equivalent of `common_hlsl_cpp_consts.h` is **one module of `#[repr(C)]`
`bytemuck::Pod` uniform structs**, used by the binding code and mirrored in the WGSL
prelude. Valve kept two files hand-synced with `#define`s; we get one source of truth and
a compile error when it drifts.

### 7.5 The HLSL prelude → WGSL prelude, ported first

`common_*.h` in `stdshaders/` is 5,181 lines of shared HLSL that every shader includes.
Port this **before** any individual shader — everything depends on it:

| File | Lines | Contents |
|---|---:|---|
| `common_flashlight_fxc.h` | 1,391 | Flashlight + shadow filtering. Portal 2 uses projected textures |
| `common_vs_fxc.h` | 1,010 | **Skinning** (`SkinPosition`, `SkinPositionAndNormal`, `DecompressBoneWeights`), **morph** (`SampleMorphDelta`, `ApplyMorph` — vertex-texture fetch), matrix helpers |
| `common_ps_fxc.h` | 1,000 | Fog, tone mapping, sRGB, depth feathering |
| `common_fxc.h` | 559 | Math and utility |
| `common_vertexlitgeneric_dx9.h` | 384 | Vertex-lit lighting core |
| `common_lightmappedgeneric_fxc.h` | 286 | Lightmapped lighting core |
| `common_4wayblend_fxc.h` | 175 | 4-way blend |
| `common_fog_*_fxc.h` (7 files) | ~180 | Fog variants — collapse into one |
| `common_spritecard_fxc.h`, `common_decaltexture_fxc.h`, `common_splinerope_fxc.h`, `common_shinyblood_fxc.h` | ~150 | Narrow helpers; port on demand |
| `common_pragmas.h` | 42 | **Delete** — D3D compiler pragmas |

`DecompressBoneWeights` in `common_vs_fxc.h` is the far end of §4.4's vertex-compression
question: **the shaders unpack what the vertex format packs, so decide compression and
skinning together**, not separately.

### 7.6 Additional deletions

- **12 `.vsh`/`.psh` files** (`WorldVertexTransition.vsh`, `UnlitGeneric.psh`,
  `macros.vsh`, …) — hand-written DX8-era shader *assembly*. Dead on a `dxlevel 95`
  target. Delete.
- **`utils/shadercompile/`, `utils/shadercompile_launcher/`,
  `public/ishadercompiledll.h`** — the distributed compile farm.
- All `[CONSOLE]`/`[XBOX]`/`[SONYPS3]` shader content and `ps20`-only variants.
- The CS:GO shader set listed below.

### 7.7 Porting one shader — the recipe

1. Read the `*_helper.cpp` (not the wrapper `.cpp`) for the shadow/dynamic split.
2. Extract the `SHADER_PARAM` table → a Rust params struct with the same names and
   defaults (fixed by `.vmt` content).
3. Bucket every `STATIC`/`DYNAMIC` axis per §7.3. Write the bucketing down.
4. Translate the `.fxc` pair (`*_vs20.fxc`, `*_ps2x.fxc`), taking the `[ps20b] [ps30]`
   branch everywhere, against the already-ported prelude.
5. Map constants onto the bind groups in §7.4 — do not invent a new layout per shader.
6. Emit `PipelineKey` from the surviving variant axes; everything else becomes a uniform
   flag word.

### 7.8 Target shader set

Roughly 15–25 shaders, against 55 declared classes / 29 helpers. **Verify against a real
Portal 2 map's material list before committing** — this list is derived from the tree, not
from shipped content.

| Shader | Why |
|---|---|
| `LightmappedGeneric` (+ `_dx9_helper`) | World brushes. The single most important one |
| `VertexLitGeneric` (+ `_dx9_helper`) | Props and characters |
| `UnlitGeneric` | Sprites, UI-in-world, tool textures |
| `WorldVertexTransition` | Blended world materials |
| `Refract`, `Water` | Water, glass, refractive surfaces |
| `Portal`, `Portal_Refract` | **Portal-specific** — the portal surfaces themselves |
| `LightmappedPaint`, `PaintBlob` | **Portal 2 paint/gel** (§8) |
| `Blob` | Portal 2 gel blobs |
| `Engine_Post` | Post-processing / tonemap / color correction |
| `Sprite`, `SpriteCard` | Particles |
| `EyeRefract`, `Phong` | Characters (GLaDOS, Wheatley, turrets) |
| `Cloak`, `VortWarp` | Effects — evaluate whether Portal 2 uses them |

Explicitly out: `character.cpp`, `customcharacter.cpp`, `customweapon_dx9*`,
`customhero_dx9_helper.cpp`, `customclothing.cpp`, `weapondecal_dx9*`,
`character_ssao.cpp` (~5k lines) — all CS:GO/Dota weapon-finish and agent-customization
shaders, matching `composite_texture.cpp` / `custom_material.cpp` /
`base_visuals_data_processor.cpp` on the C++ side.

**Open question, flagged and unresolved:** whether WGSL is written by hand or generated
from a small DSL/preprocessor to handle the variants that survive. Start by hand — 15–25
shaders is manageable, and the shape of the variant problem only becomes clear once
several are written.

## 8. Portal 2 specifics

- **Paint maps are essential.** `cmatpaintmaps.cpp` + `public/materialsystem/ipaintmapdatamanager.h`
  implement the gel/paint system: a set of paint textures allocated in parallel with
  lightmap pages (`AllocatePaintmap(paintmap, width, height)` mirrors lightmap
  dimensions), updated incrementally by dirty rectangles
  (`UpdatePaintmap(id, data, numRects, rects)`). The split is deliberate — the *data* is
  owned by the client (`IPaintmapDataManager`, driven by `engine/paint.cpp`, 1,656
  lines) and the *texture* is owned by the material system (`IPaintmapTextureManager`).
  Port both halves together, and keep the dirty-rect update path: full re-uploads of
  lightmap-sized atlases every frame would be a real regression.
- **Portal surfaces**: `stdshaders/portal.cpp`, `portal_refract.cpp` +
  `portal_refract_helper.cpp`, `portal_ps2x.fxc`. These render the recursive portal view
  and are tied to render-target management in `cmatrendercontext.cpp` (portal views are
  rendered to RTs and sampled). Expect the render-target stack to matter more than it
  would for a non-Portal game.
- **Color correction** is used per-area in Portal 2; `CColorCorrection.cpp` stays.
- **Watch for CS:GO-shaped defaults** in `public/materialsystem/materialsystem_config.h`
  and `materialsystem_cvar.cpp` — per PORTING.md's warning about the `cstrike15` base.
  Audit the default config before assuming a value is game-neutral.

## 9. Staged plan

Each stage is meant to be independently reviewable. Nothing here produces a playable
game; stages 1–3 produce a window with pixels in it, which is the first visible
milestone the project has.

1. ~~**`src/materials/renderer` — `wgpu` + `winit` bring-up.**~~ **Done.** Instance →
   adapter → device/queue → surface configured against the `winit` window;
   acquire/clear/present. Landed as `src/materials/renderer.rs` (the `wgpu` half) plus
   `src/engine/window/` (the `winit` half — windowing is `ENGINE.md` §7.3's subsystem,
   and keeping it there is what lets `src/materials/` avoid naming `winit` at all).
   Verified on macOS/Metal: `Bgra8UnormSrgb` surface, first frame presented.

   Decisions made here that the later stages inherit, all recorded in
   `../rustdocs/MATERIALS.md`: an **sRGB surface format** (so shaders write linear and
   must not apply the curve themselves); **SDR / `SurfaceColorSpace::Auto`**, leaving §10's
   HDR question open; **`wgpu::Limits::default()`** as the single capability tier of §4.6;
   backends restricted to `METAL | VULKAN | GL`; and a frame boundary where a skipped
   frame is `begin_frame() -> None` rather than an error. **Not** done here and still
   owed: MSAA (`-mat_antialias`), exclusive fullscreen modes, refresh rate, gamma.
2. ~~**Texture path.**~~ **Done.** `.vtf` parse → `ImageFormat`→`wgpu::TextureFormat`
   mapping → upload, mips, sampler creation, and the error checkerboard. Landed as
   `src/materials/{vtf,image_format,texture}.rs`, with a `blit.rs` + `shaders/blit.wgsl`
   and a `-vtf <name>` switch as the deliverable's "on screen" half — both deleted by
   stage 3, which replaced them with `preview.rs` and `-vmt`. **API:
   `../rustdocs/MATERIALS.md`.**

   Decisions made here that the later stages inherit:

   - **`binrw` was not used**, contrary to the line this bullet used to carry. The VTF
     header is four inherited `pack(1)` structs whose real layout is stated only in a
     comment (`public/vtf/vtf.h:300`), with hand-written padding, a version-dependent
     tail, and fields at unaligned offsets — describing that declaratively is longer and
     less clear than reading fifteen fields at named offsets. Same conclusion the VPK
     directory reached, for the same reason; both are recorded in `Cargo.toml`.
   - **`Features::TEXTURE_COMPRESSION_BC` is now required of the adapter.** Essentially
     all Valve content is DXT and there is no fallback tier, so this is the first
     deliberate raise of §4.6's single capability tier.
   - **sRGB is a load-time parameter, not a property of the file.** `EnableSRGBRead` was
     per-sampler and per-shader; `wgpu` bakes it into the `TextureFormat`. Stage 3 moved
     the decision into the shader, where `EnableSRGBRead` made it — see
     `shader::texture_requests`.
   - **One GPU texture per animation frame**, as `CTexture::m_pTextureHandles[iFrame]` had
     it. Cube faces and volume slices are layers within one texture; frames are not.
   - Five `ImageFormat` values are knowingly not uploadable (`P8`, `NULL`,
     `RGBA16161616`, `BGRA1010102`, `UVLX8888`). `RGBA16161616` is the one that will
     matter: it is the HDR lightmap format, and stage 5 has to decide between the
     `TEXTURE_FORMAT_16BIT_NORM` feature and converting to `Rgba16Float` on load.

   Still owed from this stage: `mat_picmip`/mip-skipping on load, texture streaming and
   exclusion lists, and frames past 0 of an animated `.vtf`.
3. ~~**Material path + WGSL prelude.**~~ **Done.** `.vmt` parse (patches, conditional
   keys, flags, vars) → `MaterialVar` → a material registry, the bind-group layout of
   §7.4, the WGSL prelude of §7.5, and `UnlitGeneric` end to end through a pipeline
   cache. Landed as `src/materials/{var,vmt,shader,uniforms,pipeline,material}.rs` plus
   `shaders/{prelude,unlitgeneric}.wgsl`, with `preview.rs` and a `-vmt <name>` switch as
   the deliverable's "on screen" half. **API: `../rustdocs/MATERIALS.md`.**

   Decisions made here that the later stages inherit:

   - **Matrices are column-major and multiply on the left** (`m * v`), against Valve's
     row-major `mul( float4(pos,1), M )`. Translating every shader through a transpose
     forever would be a permanent tax; the transpose happens once, on the way into a
     uniform. This is the single most expensive convention in the module to get wrong,
     because it produces a plausible picture rather than an error, and a GPU test pins it
     against a view projection that is deliberately not the identity.
   - **The §7.4 bind groups are frequency groups, and groups 0 and 2 are shared by every
     shader.** Only group 1 — the material's block plus its textures — is the shader's
     own. Group 3 (skinning, morph) is not created: it is bulk data wanting storage
     buffers, and nothing is skinned yet.
   - **§10's "how are variants expressed" question stays open, and that is the answer for
     now.** `ShaderKind::wgsl` concatenates the prelude with the body, which is what a
     `#include` of `common_ps_fxc.h` was. `UnlitGeneric`'s bucketing produced *no*
     textual variants at all — bucket 3 is pipeline state and bucket 2 is a flag word in
     a uniform — so `naga_oil`, `override` constants and a build-time preprocessor all
     remain unchosen, which is what §10 asks for until three shaders exist.
   - **Alpha testing is a `discard`, not state.** D3D9 had `AlphaFunc`; WebGPU does not,
     so it becomes a branch on a flag bit against `$alphatestreference`. The same applies
     to anything else that was fixed-function and is not in `RenderState`.
   - **`bIsAlphaModulating` is read from `$alpha` alone**, because there is no render
     context to override it. That is the input that made Valve keep up to eight state
     snapshots per material; here it is one field of a cache key, and when the render
     context lands it becomes an argument to `render_state` with nothing else changing.
   - **The error material is an ordinary `UnlitGeneric`** whose `$basetexture` is the
     error checkerboard, built in memory exactly as `CreateDebugMaterials` builds
     `___error.vmt`. Material fallback and texture fallback are the same mechanism one
     layer apart.

   **Two findings that correct this document**, both recorded at their sites:

   - **§7.2 is wrong that `SHADER_PARAM`'s declared default is `.vmt` surface area.**
     `m_pDefaultValue` is read by exactly one file in the tree — `tools/vmt/vmtdoc.cpp`,
     the material editor. At runtime an undefined parameter takes a *type*-based default
     from `CShaderSystem::InitShaderParameters` (`shadersystem.cpp:865`) or an explicit
     one from the shader's own `SHADER_INIT_PARAMS` block. The parameter *names* are
     still fixed content surface area; the defaults in the table are documentation.
   - **The `.vmt` value grammar is two layers, not one**, and they have to be read
     together: `KeyValues`' text loader sniffs int-vs-float-vs-string with `strtol`/
     `strtod` end pointers (`tier1/KeyValues.cpp:2620`) *before*
     `CreateMaterialVarFromKeyValue` (`cmaterial.cpp:1085`) ever sees a string and looks
     for matrices and vectors. Reading only the second gets `" 1 "` and `0x10` wrong.

   Still owed from this stage: material proxies, `$fallbackmaterial`, `$frame` animation,
   and everything `UnlitGeneric` can do past a base texture (detail, envmap, distance
   alpha, decals, phong, flashlight) — each of which §7.8 puts with the shaders that
   share it.
4. ~~**Mesh + render context.**~~ **Done.** Typed vertex structs, static and dynamic
   buffers, render targets, the depth buffer, and the pass that replaces the render
   state / matrix / render-target stacks. Landed as
   `src/materials/{mesh,target,context}.rs`, with `preview.rs` rewritten on top of them
   and `-vmt` now drawing two cubes and a ground quad instead of a full-screen quad.
   **API: `../rustdocs/MATERIALS.md`.** `glam` was added for the matrix maths, which is
   this file's `mathlib` substitution arriving.

   Decisions made here that the later stages inherit:

   - **The three stacks are deleted, not ported, and that answers §10's highest-risk
     question.** `m_MatrixStacks`, `m_RenderTargetStack` and `m_ScissorRectStack` exist
     because D3D9 had one global device whose state every draw shared. A `wgpu` render
     pass already *is* that saved state, so a target, a viewport and a camera are the
     *arguments* to opening a pass, and the model matrix is an argument to a draw. The
     one thing that genuinely changes shape: **passes do not nest**, so portal views,
     water reflections and post-processing run innermost-first — fill a `RenderTarget`,
     end that pass, then open the pass that samples it. The RT stack needed deleting
     rather than restructuring, and the dependency order it implied becomes explicit.
   - **Vertex and index buffers are separate objects, and there is no `IMesh` equivalent.**
     This is the thing reading `studiorender/` and `engine/` actually changed. Both real
     draw paths build one static vertex buffer per material at load and gather the visible
     indices into a dynamic buffer per frame — `GetDynamicMesh( false, g_WorldStaticMeshes[sortID] )`
     (`engine/gl_rsurf.cpp:1168`) and `GetDynamicMeshEx( fmt, false, 0, pGroup->m_pMesh )`
     (`studiorender/r_studiodraw.cpp:2268`). `IMesh` inheriting from both buffer
     interfaces is what the `vertexOverride`/`indexOverride` parameters exist to work
     around; fusing them here would have reproduced the bug.
   - **The vertex layout belongs to the shader, not to the mesh and not to the pipeline
     key.** `IShaderShadow::VertexShaderVertexFormat` is called by each shader's shadow
     phase, so `ShaderKind::vertex_layout()` is where it lives. The key grows a layout
     field the day one shader has two — which is `LightmappedGeneric`'s bumped variant,
     exactly as §10 predicted.
   - **Per-frame and per-draw constants are bump-allocated with dynamic offsets.**
     `Queue::write_buffer` stages its copy ahead of the *whole* command buffer, so a
     single uniform buffer rewritten between draws gives every draw in the frame the last
     values written. This is the module's sharpest trap and it is silent; a GPU test pins
     it.
   - **The depth buffer belongs to the `Renderer`**, because D3D9 created it as part of
     the swap chain and resizing it anywhere else means two places that must agree about
     the window size. `Depth24PlusStencil8` for everything: Portal needs the stencil, and
     `DepthBias::Decal`'s -64 was derived in 24-bit depth units, so a float format would
     make the decal offset quietly wrong.
   - **`Camera::screen`'s near and far go in reversed.** `glam`'s are distances along
     `-z`; passing them the natural way round makes a larger `z` mean *nearer*. This was
     wrong first time round and the depth GPU test is what caught it.

   **A finding that corrects §10:** the **`IShaderAPI` leakage is not real in this tree.**
   §10 lists `shadowmgr.cpp`, `staticpropmgr.cpp`, `MatSystemSurface.cpp` and `client/` as
   sites needing purpose-built Rust APIs before stage 4. Grepping every spelling —
   `IShaderAPI`, `g_pShaderAPI`, `IShaderDynamicAPI`, `IShaderDevice`, `IShaderShadow` —
   outside `materialsystem/`, `public/shaderapi/` and `togl/` finds only `scaleformui` and
   `rocketui` (deleted with `egui`), the shader-authoring headers `public/materialsystem/IShader.h`
   and `public/shaderlib/cshader.h` (deleted with the tower), and one Perl build script.
   Nothing in `engine/` or `game/` touches it. That item is closed.

   Still owed from this stage: MSAA (`TargetFormat::samples` is in the key and always 1),
   multiple colour attachments, 32-bit indices, stencil operations, and the `WorldVertex`
   and `ModelVertex` structs — whose layouts are enumerated on `VertexLayout` but which
   arrive with the shaders that read them.
5. ~~**Lightmaps.**~~ **Done.** `imagepacker` port, atlas pages, the `colorspace` math,
   and `LightmappedGeneric` — flat *and* bumped. Landed as `src/materials/lightmap.rs`
   plus `shaders/lightmappedgeneric.wgsl`, `mesh::WorldVertex`, a fourth bind group, and
   the atlas driving in `src/engine/world/`. **Deliverable met:** `sp_a1_intro1` draws
   5,512 faces with real baked lighting — 4,828 lit surfaces over 12 atlas pages, 58 of
   66 materials resolving where 4 did before. **API: `../rustdocs/MATERIALS.md`.**

   Decisions made here that the later stages inherit:

   - **Portal 2 ships HDR-only maps, so the LDR lightmap path is not available.**
     `sp_a1_intro1.bsp` has an empty `LUMP_LIGHTING` and 5.4 MB of `LUMP_LIGHTING_HDR`,
     and its `LUMP_FACES` entries all carry `light_ofs` 0 — only `LUMP_FACES_HDR` is
     meaningful. This *narrows* §10's HDR question rather than answering it: the
     lightmaps are HDR, the render path still is not, so `cLightScale` is 1.0 and a map
     is as bright as `vrad` left it. The float-target-and-tonemap half is still open.
   - **A page is `Rgba16Float` holding linear radiance.** Of Valve's three page formats
     the HDR-float one is the only one whose contents are just the numbers
     (`GetLightMapScaleFactor` is 1.0 for it against 16.0 for HDR-integer), and
     `Rgba16Float` is the only float format filterable at §4.6's capability tier.
   - **`CImagePacker` is ported line for line, edge cases included** — the `>=` in
     `GetMaxYIndex` and the off-by-one in the height test. It is not an implementation
     detail: the page a surface lands on and its offset within that page *are* its
     texture coordinates, so a different packer is a different atlas, a different page
     count and a different number of draw batches.
   - **A batch is a (material, lightmap page) pair, which is what a sort ID was.**
     `AllocateLightmap`'s material-change rule — close every open page but the last —
     is kept, because it is what makes that pair coarse.
   - **The lightmap page is bind group 3, not part of the material.** One material spans
     as many pages as its surfaces needed, so the page is per batch;
     `Pass::bind_lightmap_page` is `IMatRenderContext::BindLightmapPage`. Group 3 was
     reserved for skinning in stage 4 and is declared only by shaders that read a
     lightmap, since a pipeline layout is per shader.
   - **Bumpedness is read from the `.bsp`'s `SURF_BUMPLIGHT`, not re-derived from the
     material.** Valve does the latter and therefore reads the lighting lump at the wrong
     stride if a `.vmt` changed after the map was compiled. The flag agrees with the data
     on all 4,982 of `sp_a1_intro1`'s lit faces. How wide a block to *reserve* still comes
     from the material, and the two are reconciled rather than assumed equal.
   - **Only lightstyle 0 is baked.** The other three are switchable and animated lights,
     which need `LightStyleValue` and a per-frame page rebuild — the whole dynamic
     lighting path. `WorldStats::faces_with_lightstyles` counts what this understates.

   **The finding that corrects §10, and the one that cost the most:** a lightmap sample
   decodes with `TexLightToLinear` (`c * 2^e / 255`), **not** `ColorRGBExp32ToVector`,
   which is the same thing times 255. The second is the obvious-looking "decode this
   colour", sits immediately below the table, and carries Valve's own *"FIXME: Why is
   there a factor of 255 built into this?"*. It is for world-light intensities and ambient
   cubes; the lightmap path calls the first directly (`gl_lightmap.cpp:572`). Getting it
   wrong is a uniformly white screen — no error, no warning, and plausible from either
   end. It was found by taking a screenshot.

   Still owed from this stage: dynamic lights and lightstyle animation, the last page's
   `GetMinimumDimensions` shrink, and everything `LightmappedGeneric` can do past a base
   texture, a bump map and a lightmap (`$basetexture2` blending, detail, envmap,
   selfillum, phong, seamless, flashlight, CSM, paint).
6. **`VertexLitGeneric` + the remaining core shader set** (§7). This is where the vertex
   layout question §10 expected `LightmappedGeneric` to force actually lands — see below.
7. **Paint maps** (§8), color correction, occlusion queries, post-processing.
8. **Deferred:** GPU morph (`morph.cpp`), headless/null path, anything left in §5.4.

Stages 1–5 are done. `PORTING.md`'s ordering rule — follow the boot path depth-first —
puts **input** next, then `console/`; stage 6 is a breadth move and is not gated on
either.

## 10. Open questions and risks

- **How the surviving WGSL variants are expressed.** WGSL has no preprocessor, so §7.3's
  bucket 3 needs a mechanism. Three candidates: `wgpu` **pipeline-overridable constants**
  (`override`, the natural fit — one module, constants specialized at pipeline creation),
  string preprocessing at build time, or `naga_oil` for `#include`/`#ifdef`-style
  composition (which would also serve the §7.5 prelude). **Decide after the prelude and
  ~3 shaders exist**, not before — the shape of the problem isn't visible yet.
  *Stage 3 update, one shader in:* still open, and `UnlitGeneric` did not need any of
  them. Its bucketing produced no textual variants at all, so the prelude is prepended by
  string concatenation (`ShaderKind::wgsl`) and nothing else is chosen. The shader most
  likely to force the question is `LightmappedGeneric`, whose bumped/unbumped split
  changes the vertex layout — a real bucket-3 axis, and therefore a pipeline variant
  before it is a source-text one.
- **How many pipeline variants actually survive?** §7.3 predicts single digits per
  shader. If a shader genuinely needs dozens, the pipeline cache needs an on-disk warm
  cache and the plan needs a stage for it. *Stage 3 update:* `UnlitGeneric`'s
  `RenderState` has 2 × 2 × 2 × 2 × 5 × 2 × 2 reachable combinations on paper, but the
  ones content actually asks for are far fewer — `PipelineCache::len()` is the
  measurement, and the honest answer needs a real map's material list. *Stage 4 update:*
  the target format is now part of the key and has one more field that varies (a depth
  attachment, present for the back buffer and optional for render targets), so the count
  is per-shader-per-target rather than per-shader. Still single digits.
- **Vertex-texture-fetch morph** (`ApplyMorph`/`SampleMorphDelta` in `common_vs_fxc.h`)
  is tied to `morph.cpp`, which §2 defers. Make sure deferring it doesn't silently break
  the shaders that call it — the prelude needs a no-op path.
- **Vertex compression**: keep the packed formats (saves bandwidth, matches the shipped
  MDL data) or unpack at load (simpler shaders, more memory)? *Stage 4 update: still
  open, and deliberately not forced.* Nothing in the current shader set declares
  `VERTEX_FORMAT_COMPRESSED` — `VertexLitGeneric` is the first that will
  (`vertexlitgeneric_dx9_helper.cpp:893`) — and `common_vs_fxc.h`'s
  `DecompressBoneWeights` is the other half of the same decision, so **answer it with
  skinning, not before it**. *Stage 5 update: still open.* `LightmappedGeneric`'s
  `WorldVertex` is uncompressed and so is Valve's — brush geometry never declared the flag.
- ~~**Render-target stack semantics.**~~ **Resolved in stage 4, and the answer was
  simpler than the question.** `wgpu` render passes are not nestable, so the stack was
  deleted rather than restructured: a target, a viewport and a camera are the arguments
  to opening a pass, the pass ends when it drops, and nested rendering becomes
  *sequential* rendering in dependency order — fill a `RenderTarget`, end that pass, then
  open the pass that samples it. Portal recursion is a loop over passes rather than a
  stack of pushes. See `rustdocs/MATERIALS.md`, "Passes replace three stacks".
- ~~**`IShaderAPI` leakage**~~ **— closed; the premise was wrong.** Enumerating every
  spelling of it outside `materialsystem/`, `public/shaderapi/` and `togl/` finds
  `scaleformui` and `rocketui` (both deleted with `egui`), the two shader-authoring
  headers that go with the tower, and one Perl build script. `engine/` and `game/` do not
  reference it at all, so there is nothing to design a replacement API for.
- **HDR.** `GetHDRType`/`SupportsHDRMode` gate a whole rendering mode (float render
  targets + tonemapping). Portal 2 ships HDR-lit maps. **Still open, and now deferred by
  default:** stage 1 configures the swap chain SDR (`SurfaceColorSpace::Auto` with an
  sRGB format). That is a one-field change to reverse, but the rest — a float format and
  a tonemap pass — is real work, so decide before the post-processing shaders, not after.
- **Threading.** The queued context is deleted (§5.3), but the eventual replacement —
  parallel command encoding — should be designed for, not retrofitted. *Stage 4 update:
  the property is held so far.* `RenderContext` owns its arenas and reaches no global
  mutable state; the thing that will need thought is that the per-draw uniform arena is a
  single bump allocator, so parallel encoding wants one arena per thread rather than a
  lock.
- **`CMaterialSubRect`** — confirm nothing outside the module creates sub-rect materials
  before deleting.
