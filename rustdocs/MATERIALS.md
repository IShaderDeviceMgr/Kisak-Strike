# `src/materials/` — API reference

The material system. Right now that means three things: the GPU device and the frame
boundary; the texture path from a `.vtf` on disk to a sampler on the GPU; and the
material path from a `.vmt` to a compiled pipeline that can draw with it. Meshes and the
render context are not here yet, so the only geometry is one quad.

Porting design doc: [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named
after the *original* module (`materialsystem/`), while this file is named after the Rust
one (`src/materials/`). Same subject, two names, on purpose.

| | |
|---|---|
| Module | `crate::materials` |
| Lines | ~7,900 Rust including tests, plus ~260 of WGSL |
| Tests | 100 (`cargo test materials`) — 8 of them run on a real GPU |
| Dependencies | `wgpu` 30, `bytemuck`, `pollster`, `thiserror` |
| Status | **Stages 1-3 of 8.** GPU bring-up, `.vtf` -> `wgpu::Texture`, `.vmt` -> `Material` -> a drawn quad. Stages 4+ not started |

```
src/materials/
  renderer.rs      Renderer, Frame, RendererOptions — the device and the frame boundary
  vtf.rs           Vtf, TextureFlags — reading .vtf files
  image_format.rs  ImageFormat, ColorSpace — pixel formats, size maths, CPU conversions
  texture.rs       Texture, TextureCache, SamplerKey — textures on the GPU
  var.rs           MaterialVar, MaterialFlags — what a .vmt says, and the coercions
  vmt.rs           Vmt — reading a .vmt: patches, conditionals, flags, vars
  shader.rs        ShaderKind, ShaderParam, render_state — the shader set and its shadow phase
  uniforms.rs      FrameUniforms, DrawUniforms — the constant ABI (§7.4)
  pipeline.rs      RenderState, PipelineKey, PipelineCache, BindLayouts, Vertex
  material.rs      Material, MaterialCache — a .vmt bound to a shader and its textures
  preview.rs       MaterialPreview — the stage-3 verification draw. Temporary
  shaders/prelude.wgsl        the shared prelude (§7.5)
  shaders/unlitgeneric.wgsl   the one shader
  error.rs         RendererError, VtfError, VmtError, TextureError
```

**What is deliberately absent:** there is no `IShaderDevice`, no `IShaderAPI`, no
`IShaderShadow`, no device-abstraction trait, and no second backend. `wgpu` is called
directly. If you find yourself adding a trait so that "another renderer could be plugged
in later", stop — `wgpu` is already that abstraction, and re-adding the tower is the
specific mistake `portdocs/MATERIALSYSTEM.md` §5.1 exists to prevent.

## Quick start

```rust
use std::sync::Arc;
use crate::materials::pipeline::TargetFormat;
use crate::materials::uniforms::DrawUniforms;
use crate::materials::{MaterialCache, MaterialPreview, Renderer, RendererOptions, CLEAR_COLOR};

// `window` is an `Arc<winit::window::Window>`; `display` is the event loop's
// `OwnedDisplayHandle`.
let size = window.inner_size();
let mut renderer = Renderer::new(
    window.clone(),                 // coerces to Arc<dyn RenderWindow>
    display,
    (size.width, size.height),
    &RendererOptions::default(),
)?;

// The material dictionary, and the texture and pipeline caches under it.
// Independent of the renderer — it holds its own `Device`/`Queue` handles.
let mut materials = MaterialCache::new(renderer.device(), renderer.queue());
let preview = MaterialPreview::new(renderer.device(), materials.pipelines().layouts());

// Cannot fail: a missing or broken material is the error material.
let wall = materials.load(&vfs, "metal/metalwall048a");

// ... once per frame. Anything that needs the renderer happens *before*
// `begin_frame`, because the `Frame` borrows it for its whole lifetime:
let target = TargetFormat::color_only(renderer.surface_format());
let pipeline = materials.pipelines().get(&wall.pipeline_key(target));
preview.update(
    renderer.queue(),
    (size.width, size.height),
    &DrawUniforms { modulation: wall.modulation, ..DrawUniforms::identity() },
);

if let Some(mut frame) = renderer.begin_frame() {
    frame.clear(CLEAR_COLOR);
    frame.draw_material(&preview, &wall, &pipeline);
    window.pre_present_notify();
    frame.present();
}

// ... on WindowEvent::Resized:
renderer.resize(size.width, size.height);
```

`src/engine/window/` is the only caller; see [`rustdocs/ENGINE.md`](ENGINE.md).

---

## The renderer

### `Renderer`

Owns the `wgpu` `Instance`, `Device`, `Queue` and `Surface`, and the window handle they
were built from. One per process.

```rust
pub fn new<D>(
    window: Arc<dyn RenderWindow>,
    display: D,
    size: (u32, u32),
    options: &RendererOptions,
) -> Result<Self, RendererError>
where
    D: wgpu::rwh::HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static;

pub fn device(&self) -> &wgpu::Device;
pub fn queue(&self) -> &wgpu::Queue;
pub fn surface_format(&self) -> wgpu::TextureFormat;
pub fn resize(&mut self, width: u32, height: u32);
pub fn begin_frame(&mut self) -> Option<Frame<'_>>;
```

Construction either yields a working device or an error — there is no
`Connect`/`Init`/`Shutdown`/`Disconnect` lifecycle and no half-initialized state.
Teardown is `Drop`.

`size` is **physical** pixels, not logical: on a HiDPI display these differ by the scale
factor, and configuring a surface with logical pixels produces a quarter-resolution image
stretched to fit. `winit`'s `Window::inner_size()` is already physical.

`device()` and `queue()` are how a subsystem builds its own resources. Both types are
cheap `Clone` handles to shared state, so [`TextureCache`](#texturecache) keeps copies
rather than borrowing the renderer for its lifetime — see
`portdocs/MATERIALSYSTEM.md` §5.3 on leaving room for off-thread recording.

### `RenderWindow`

```rust
pub trait RenderWindow:
    wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + Send + Sync + 'static {}
```

Blanket-implemented, so `Arc<winit::window::Window>` satisfies it with no adapter type.
It exists so this module never names `winit` — the replacement for `ILauncherMgr`
(`public/appframework/ilaunchermgr.h`), which `CMaterialSystem::Connect` reached through
for `g_pLauncherMgr` (`materialsystem/cmaterialsystem.cpp:206`), is *no interface at
all*.

The `Arc` is not decoration: the renderer keeps the window alive so it can rebuild the
surface after `SurfaceStatus::Lost`.

### `RendererOptions`

```rust
pub struct RendererOptions {
    pub adapter_index: Option<usize>,  // -adapter <n>
    pub vsync: bool,                   // !MATSYS_VIDCFG_FLAGS_NO_WAIT_FOR_VSYNC
}
```

`Default` is `{ adapter_index: None, vsync: true }`. `None` means "let `wgpu` rank the
adapters with `PowerPreference::HighPerformance`", which is **not** the same as `Some(0)`
— the original's `ParmValue("-adapter", 0)` defaulted to literal adapter zero.

`src/engine/window/`'s `VideoConfig` is what actually builds one of these from the
command line.

### `Frame<'a>`

```rust
pub fn clear(&mut self, color: wgpu::Color);
pub fn draw_material(                         // stage 3 only; see below
    &mut self,
    preview: &MaterialPreview,
    material: &Material,
    pipeline: &wgpu::RenderPipeline,
);
pub fn present(self);   // #[must_use] on the struct
```

One acquired swap-chain image plus the `CommandEncoder` recording into it. `present`
consumes it: submit, then present. Dropping a `Frame` without presenting discards
everything recorded into it — correct for an abandoned frame, silent data loss if
accidental, which is why the type is `#[must_use]`.

**A `Frame` borrows the renderer mutably for its lifetime**, so everything that needs the
renderer — asking the pipeline cache for a pipeline (it needs the surface format), writing
a uniform buffer (it needs the queue) — has to happen *before* `begin_frame`. That is not
a wart to work around: `Surface::configure` panics if a frame is alive, so the borrow
checker is enforcing a real `wgpu` rule, and the ordering it forces is the one a render
context wants anyway.

Render passes against the back buffer are opened by `Frame::begin_color_pass`, which is
`pub(super)` on purpose: `portdocs/MATERIALSYSTEM.md` §10 calls the render-target stack
the highest-risk unknown after the shaders, and letting arbitrary callers open passes
against the swap-chain image before that design exists is how it gets decided by
accident.

### The frame boundary

This is the part that constrains the engine host loop (`portdocs/ENGINE.md` §6), so it is
worth stating precisely:

```
begin_frame()  ->  Some(Frame)  ->  [record]  ->  present()
               \-> None            (skip this frame entirely)
```

**`None` is normal and must not be logged per frame.** It is what an occluded, minimized,
timed-out or just-invalidated surface looks like, and the renderer has already done
whatever recovery was needed by the time it returns. Concretely:

| `wgpu` reports | `begin_frame` does | returns |
|---|---|---|
| `Success` | — | `Some` |
| `Suboptimal` | draws it, reconfigures before the *next* frame | `Some` |
| `Timeout`, `Occluded` | nothing | `None` |
| `Outdated` | reconfigures the surface | `None` |
| `Lost` | rebuilds the surface from the window | `None` |
| `Validation` | logs once, for that frame | `None` |

**The caller must back off when it gets `None`.** The renderer does not own the frame
clock, so it cannot pace the retry itself, and "ask again immediately" is a spin loop at
100% of a core whenever the window is off screen — on macOS/Metal that measured ~75,000
failed acquisitions per second. `src/engine/window/`'s `SKIP_RETRY` is the working
implementation; do not add a second caller without one.

Together those replace `IShaderDevice::ReleaseResources`/`ReacquireResources` and the
`IShaderDeviceDependentObject` device-lost callback interface, whose only real users were
Scaleform and RocketUI — both deleted with `egui`.

`Frame` borrows the renderer mutably for its lifetime. That is not incidental:
`Surface::configure` **panics** if a `SurfaceTexture` is still alive, so the borrow
checker is enforcing a real `wgpu` rule. Do not try to work around it with interior
mutability.

---

## The texture path

Three layers, each usable on its own: [`Vtf`](#vtf) reads the file,
[`ImageFormat`](#imageformat) says what its pixels become, [`Texture`](#texture) puts them
on the GPU, and [`TextureCache`](#texturecache) is the dictionary in front of all of it.

### `TextureCache`

`materialsystem/texturemanager.cpp`'s `CTextureManager`, minus refcounting, eviction,
exclusion lists and streaming.

```rust
pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> TextureCache;
pub fn load(&mut self, vfs: &Vfs, name: &str, color_space: ColorSpace) -> Arc<Texture>;
pub fn error_texture(&self) -> Arc<Texture>;
pub fn white_texture(&self) -> Arc<Texture>;
```

**The two standard textures are not interchangeable.** `error_texture` is the magenta
checkerboard a *failed* load resolves to; `white_texture` is the 1x1 white
`TEXTURE_WHITE` that an *undefined* texture parameter binds
(`vertexlitgeneric_dx9_helper.cpp:1255`). A `.vmt` with no `$basetexture` is not broken —
Valve's own `___flat.vmt` is one — and drawing it as a checkerboard would report a failure
that did not happen. `Material::new` picks between them; see `TextureFallbacks`.

The rest of `CTextureManager`'s standard family (`black`, `grey`, `greyalphazero`, the
normalization cubemap) is not here: each is reached only from a shader feature that is not
ported, and each is four lines when the shader that wants one lands.

**`load` cannot fail.** A missing or broken texture logs once and resolves to the error
checkerboard, which is what `CTextureManager::FindOrLoadTexture` did and what every later
stage depends on: a material referencing one bad texture must still draw. If you want the
reason a load failed, it is on stderr; there is deliberately no `try_load` until something
needs one.

`name` is the texture name *without* the `materials/` prefix or the `.vtf` extension —
`"metal/metalwall048a"`, exactly as a `.vmt` writes it. It is lowercased and
backslash-normalized for the cache key, so `Metal\Wall01` and `metal/wall01` are one
entry. The path actually read is `materials/<name>.vtf`
(`ctexture.cpp:3235`); the `Vfs` handles case-insensitivity of the lookup itself.

The cache key includes `color_space`, because the same `.vtf` sampled as colour and as
data are two different GPU textures.

### `Texture`

```rust
pub struct Texture {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub depth: u32,                                   // volume slices, else 1
    pub mip_count: u32,
    pub format: wgpu::TextureFormat,
    pub view_dimension: wgpu::TextureViewDimension,   // D2 | Cube | D3
    pub frame: u32,
}

pub fn view(&self) -> &wgpu::TextureView;
pub fn sampler(&self) -> &wgpu::Sampler;
pub fn is_translucent(&self) -> bool;

pub fn from_vtf(
    device: &wgpu::Device, queue: &wgpu::Queue, name: &str,
    vtf: &Vtf, frame: u32, color_space: ColorSpace, sampler: wgpu::Sampler,
) -> Result<Texture, TextureError>;

pub fn error(device: &wgpu::Device, queue: &wgpu::Queue, sampler: wgpu::Sampler) -> Texture;
pub fn white(device: &wgpu::Device, queue: &wgpu::Queue, sampler: wgpu::Sampler) -> Texture;
```

**One `Texture` is one animation frame of one `.vtf`.** Cube faces (6 array layers) and
volume slices (a `D3` texture) live *inside* one; animation frames do not. That is Valve's
own model — `CTexture::m_pTextureHandles[iFrame]` — and it is kept because `$frame`
animation swaps whole textures rather than indexing a layer.

`Texture::error` is the magenta-and-black checkerboard, reproduced exactly from
`CTextureManager::Init` (`texturemanager.cpp:632`): 32x32, `(255,0,255,255)` and
`(0,0,0,128)` alternating on `(x & 4) ^ (y & 4)`. Built in code rather than loaded, so it
exists before the filesystem does and cannot itself fail.

### `ColorSpace` — the one decision a caller must make

```rust
pub enum ColorSpace { Linear, Srgb }
```

In the original, sRGB is **not** a property of a texture. The *shader* decided, per
sampler, by calling `IShaderShadow::EnableSRGBRead(SHADER_SAMPLER0, true)`
(`materialsystem/stdshaders/BaseVSShader.cpp:815`, and about a hundred other sites).
`wgpu` has no such switch — sRGB is baked into the `TextureFormat` — so **the decision
moves to load time and the caller has to make it.**

**Stage 3 encodes the rule where it belongs: in the shader.**
[`shader::texture_requests`](#texture_requests) answers it per parameter, which is what
`EnableSRGBRead` did — the shader is the only thing that knows what it is going to do with
the pixels. Call that rather than deciding at a call site.

The rule it applies, and the one to extend when a shader gains a texture parameter:

| Kind of texture | `ColorSpace` |
|---|---|
| `$basetexture`, `$detail`, `$envmap`, anything a human picked as a colour | `Srgb` |
| `$bumpmap`, `$ssbump`, masks, DUDV maps, lightwarps, HDR data | `Linear` |
| any of the above on a material with `$gammacolorread 1` | `Linear` — content asking for the raw bytes |

`Vtf::is_normal_map()` (`TEXTUREFLAGS_NORMAL` or `TEXTUREFLAGS_SSBUMP`) is the one part a
`.vtf` can answer on its own — consult it when you have nothing better. Nothing is
inferred inside `from_vtf`: silently overriding a caller is worse than obeying a wrong
one, and the caller is the one that knows.

`TEXTUREFLAGS_SRGB` in the file is **not** this flag. Its comment says "SRGB correction
has already been applied to this texture"; it is essentially never set in shipped content
and does not mean "sample me as sRGB".

### `SamplerKey`

```rust
pub fn sampler_key(flags: TextureFlags, mip_count: u32) -> SamplerKey;
```

Reads sampler state out of a `.vtf`'s flags — `CTexture::SetWrapState`
(`ctexture.cpp:2580`) and `CTexture::SetFilterState` (`ctexture.cpp:2626`). The keys are
hashable and interned by `TextureCache`, because the whole game shares a handful of
distinct sampler states across thousands of textures.

The structural change worth knowing: Valve's sampler state was per-texture but *applied by
mutating global device state at draw time*. Here it is an immutable `wgpu::Sampler`
decided at load and never touched again.

### `Vtf`

```rust
pub fn parse(data: Vec<u8>) -> Result<Vtf, VtfError>;

pub fn is_cubemap(&self) -> bool;
pub fn is_volume(&self) -> bool;
pub fn is_normal_map(&self) -> bool;
pub fn mip_dimensions(&self, level: u32) -> (u32, u32, u32);
pub fn mip_data(&self, frame: u32, face: u32, level: u32) -> Option<&[u8]>;
pub fn low_res_data(&self) -> Option<&[u8]>;
```

Public fields: `version: (u32, u32)`, `width`, `height`, `depth`, `format: ImageFormat`,
`flags: TextureFlags`, `frame_count`, `start_frame`, `face_count`, `mip_count`,
`reflectivity: [f32; 3]`, `bump_scale`, `low_res: Option<LowResImage>`.

Takes ownership of the file bytes and indexes into them. `mip_data` returns exactly
`ImageFormat::mem_required` of the level's dimensions, or `None` for an out-of-range
index; every in-range slice is guaranteed present because `parse` validated the file's
extent up front.

**Mip levels are indexed largest-first** (0 is the full-size image), matching `wgpu` and
every modern API. The file stores them the other way round.

Versions 7.0 through 7.5 are read. X360 (`0x0360`) and PS3 (`0x0333`) files are rejected
by name rather than as corruption.

### `ImageFormat`

```rust
pub fn from_raw(raw: i32) -> Option<Self>;
pub fn name(self) -> &'static str;
pub fn is_compressed(self) -> bool;
pub fn bytes_per_block(self) -> usize;
pub fn mem_required(self, width: u32, height: u32, depth: u32) -> usize;
pub fn gpu_format(self, color_space: ColorSpace) -> Option<wgpu::TextureFormat>;
pub fn to_gpu_bytes(self, src: &[u8], texels: usize) -> Cow<'_, [u8]>;

pub fn full_mip_count(width: u32, height: u32, depth: u32) -> u32;
pub fn mip_dimensions(width: u32, height: u32, depth: u32, level: u32) -> (u32, u32, u32);
pub fn unsupported_name(raw: i32) -> Option<&'static str>;
```

39 variants, discriminants fixed by the file format. `to_gpu_bytes` borrows when the
bytes already are what the GPU wants — every block-compressed format and `BGRA8888`,
between them essentially all shipped content — and converts otherwise.

What each becomes:

| `.vtf` format | `wgpu` | Converted on the CPU? |
|---|---|---|
| `DXT1`, `DXT1_ONEBITALPHA` | `Bc1RgbaUnorm[Srgb]` | no |
| `DXT3` / `DXT5` | `Bc2` / `Bc3RgbaUnorm[Srgb]` | no |
| `ATI1N` / `ATI2N` | `Bc4RUnorm` / `Bc5RgUnorm` | no |
| `BGRA8888`, `BGRX8888` | `Bgra8Unorm[Srgb]` | `BGRX` only, to force alpha opaque |
| `RGBA8888` | `Rgba8Unorm[Srgb]` | no |
| `ABGR8888`, `ARGB8888`, `RGBX8888` | `Rgba8Unorm[Srgb]` | reordered |
| `RGB888`, `BGR888`, `*_BLUESCREEN` | `Rgba8Unorm[Srgb]` | widened to 32-bit |
| `RGB565`, `BGR565`, `BGRA4444`, `BGRA5551`, `BGRX5551` | `Rgba8Unorm[Srgb]` | unpacked |
| `I8`, `IA88`, `A8` | `Rgba8Unorm[Srgb]` | expanded — see below |
| `UV88` / `UVWQ8888` | `Rg8Snorm` / `Rgba8Snorm` | no |
| `R16F`, `R32F`, `RG1616F`, `RG3232F`, `RGBA16161616F`, `RGBA32323232F` | the matching float format | no |
| `RGB323232F` | `Rgba32Float` | padded with alpha 1.0 |
| `RGBA1010102` | `Rgb10a2Unorm` | no |
| `P8`, `NULL`, `RGBA16161616`, `BGRA1010102`, `UVLX8888` | — | **not uploadable** |

`I8`/`IA88`/`A8` are expanded to full RGBA rather than mapped to `R8Unorm`/`Rg8Unorm`.
`D3DFMT_L8` broadcast luminance to RGB and `D3DFMT_A8` read as `(0,0,0,A)`; `wgpu` has
neither format nor a component swizzle on a texture view, so the alternative is making
every shader that samples one remember to swizzle — where getting it wrong is silent.
Four times the memory for a handful of small textures is the cheaper mistake.

---

---

## The material path

Four layers, and the same shape as the texture path one level up: [`Vmt`](#vmt) reads the
file, [`ShaderKind`](#shaderkind) says what the file *means*, [`Material`](#material)
binds it to the GPU, and [`MaterialCache`](#materialcache) is the dictionary in front of
all of it.

### `MaterialCache`

`CMaterialDict` plus the parts of `CMaterialSystem` that own the texture manager and the
shader system.

```rust
pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> MaterialCache;
pub fn load(&mut self, vfs: &Vfs, name: &str) -> Arc<Material>;
pub fn error_material(&self) -> Arc<Material>;
pub fn pipelines(&mut self) -> &mut PipelineCache;
```

**`load` cannot fail**, for the same reason `TextureCache::load` cannot: a missing file, a
malformed one, an unresolvable patch chain and an unknown shader all resolve to the error
material and a line on stderr, because a map with one bad material still has to load.
`CMaterialSystem::FindMaterial` (`cmaterialsystem.cpp:3032`) is the same function.

`name` is normalized as `FindMaterial` normalizes it — lowercased, forward slashes,
extension stripped — so `Metal\Wall01.vmt` and `metal/wall01` are one entry. The path
actually read is `materials/<name>.vmt`.

The error material is **an ordinary `UnlitGeneric` whose `$basetexture` is the error
checkerboard**, built in memory at construction exactly as
`CMaterialSystem::CreateDebugMaterials` (`cmaterialsystem.cpp:462`) builds `___error.vmt`.
The material fallback and the texture fallback are the same mechanism one layer apart, and
that is worth knowing when something draws magenta: it means *either* the material or its
texture failed, and only stderr says which.

It owns the `TextureCache` and the `PipelineCache` rather than borrowing them: building a
material needs both, and neither is meant to be swappable.

### `Material`

```rust
pub struct Material {
    pub name: String,
    pub shader: ShaderKind,
    pub flags: MaterialFlags,
    pub state: RenderState,       // what the shadow phase decided
    pub modulation: [f32; 4],     // $color * $color2, with $alpha in w
}

pub fn bind_group(&self) -> &wgpu::BindGroup;               // group 1
pub fn pipeline_key(&self, target: TargetFormat) -> PipelineKey;

pub fn new(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layouts: &BindLayouts,
    name: &str,
    vmt: &Vmt,
    fallback: &TextureFallbacks,   // { white, error }
    resolve: impl FnMut(&str, ColorSpace) -> Arc<Texture>,
) -> Option<Material>;   // None if the .vmt names a shader we do not have
```

`resolve` is a callback rather than a `&mut TextureCache` because the two callers differ
in what they can do: an ordinary material reads through the `Vfs`, and the error material
— which has to exist before any content is mounted — hands back the checkerboard without
looking anything up.

Immutable once built. Everything a `.vmt` decides is resolved in `Material::new` — the
shader, the textures and their colour spaces, the pipeline state, the uniform block and
the bind group — and nothing recomputes any of it. The original re-ran
`RecomputeStateSnapshots` whenever a var changed, because proxies could change one at any
time; when proxies land here, what they mutate is the *draw* uniforms, not this.

`modulation` lives on the material but belongs to the draw: `CBaseMeshDX8::DrawMesh`
multiplies it by a per-instance modulation before every draw
(`shaderapidx9/meshdx8.cpp:2378`), so it is handed to
[`DrawUniforms`](#the-constant-abi) rather than baked into group 1.

### `Vmt`

```rust
pub struct Vmt {
    pub shader: String,                       // the outermost key, verbatim
    pub vars: Vec<(String, MaterialVar)>,     // lowercased names, `$` kept
    pub flags: MaterialFlags,                 // $flags
    pub flags_defined: MaterialFlags,         // $flags_defined
}

pub fn load(vfs: &Vfs, name: &str) -> Result<Vmt, VmtError>;
pub fn from_keyvalues(name: &str, document: &Block) -> Result<Vmt, VmtError>;
pub fn var(&self, name: &str) -> Option<&MaterialVar>;
```

`from_keyvalues` is the entry point for materials that were never files — the error
material is one — and matches `CMaterialSystem::CreateMaterial( name, pVMTKeyValues )`.

Reading a `.vmt` is mostly **rejection**, and every rule below silently changes what a
material means if it is missed:

| Key | What happens |
|---|---|
| `$translucent`, `$nocull`, and 30 others | a *flag*, not a var. Never appears in `vars` |
| `hdr?$basetexture` | a conditional key; kept or dropped whole, see the table below |
| `%tooltexture` | an editor-only key, always dropped |
| `$foo ""` or `$color "[]"` | no var at all, rather than an empty one |
| a second `$basetexture` | ignored, with a warning — the first wins |
| a second `$basetexture` written as `ldr?$basetexture` | **replaces** the first, silently. That is how content says "and this instead" |

**Conditional keys** are evaluated against this port's fixed capability tier, and each
answer is a decision somebody can reverse:

| Condition | Kept? | Because |
|---|---|---|
| `ldr`, `srgb`, `srgb_pc`, `GPU>=1`, `GPU>=2`, `GPU>=3` | kept | SDR; `wgpu` blends in linear space on an sRGB target; `gpu_level` defaults to 3 |
| `hdr`, `srgb_gameconsole`, `GPU<1`, `GPU<2`, `GPU<3`, `360`, `SonyPS3`, `gameconsole`, `lowfill` | dropped | the other half of the same answers, plus no consoles |
| `LowQualityCSM`, `HighQualityCSM` | **both dropped** | cascaded shadow maps are not ported, so neither quality applies |
| anything unrecognized | dropped, with a warning | the original's fall-through |

A leading `!` inverts the answer.

**Patch materials.** A `.vmt` whose outermost key is `patch` names another with `include`
and edits it with `insert` (set unconditionally) and `replace` (set only keys that already
exist). Portal 2 content leans on this heavily. Two orderings are easy to get backwards
and are Valve's:

- **The innermost patch wins.** Accumulation overwrites as the chain is walked *down*, so
  when a patch includes a patch, the one closer to the base survives a conflict.
- **`insert` runs before `replace`**, so a key `insert` adds is then visible to `replace`.

A chain deeper than ten levels stops and keeps `patch` as its shader name, which resolves
to the error material.

**Fallback blocks.** A `.vmt` may carry a `">=DX90" { ... }` or
`"UnlitGeneric_dx9" { ... }` block whose keys override the material's own.
`FindBuiltinFallbackBlock` tries thirteen suffixes against `gpu_level` and the DX level;
at our tier eight of them are reachable, tried in this order, **first match wins and the
others are not read**:

```
GPU>=1  GPU>=2  >=DX90_20b  >=DX90  >DX90  ldr  srgb  dx9
```

### `MaterialVar` and `MaterialFlags`

```rust
pub enum MaterialVar {
    Float(f32),
    Int(i32),
    Vec(Vec4, u8),      // and how many components the file wrote
    Matrix(Matrix),     // row-major, VMatrix layout
    Str(String),
}

pub fn parse(text: &str) -> Option<MaterialVar>;
pub fn as_f32(&self) -> f32;
pub fn as_i32(&self) -> i32;
pub fn as_bool(&self) -> bool;
pub fn as_vec4(&self) -> Vec4;
pub fn as_matrix(&self) -> Matrix;
pub fn as_str(&self) -> Option<&str>;
```

**The type of a value is guessed, not declared**, and `parse` is the rule whole — Valve's
is split between `KeyValues`' text loader and `CreateMaterialVarFromKeyValue`. In order:
empty (no var), float, int, matrix, vector, string. Two consequences worth knowing:

- **Whitespace is not trimmed first**, so `" 1 "` is a *string*, not an int. It still
  reads as `1.0` from `as_f32`, which is why the coercions exist.
- **`0x10` is a string.** `strtod` accepts hex under POSIX and `KeyValues` undoes that by
  hand so content behaves the same everywhere.

**Every accessor coerces and none of them fails.** `CMaterialVar` stored all
representations at once — `SetIntValue` also wrote the vector, `SetStringValue` also ran
`atoi`/`atof` — so `$color "1"` means white and `$alpha "half"` means 0. The arms are
computed on demand here, with the same answers.

`MaterialFlags` is a plain newtype over the 32 `$flags` bits, paired for the first time
with the *names* content writes: the bit values are in
`public/materialsystem/imaterial_declarations.h` and the names in
`materialsystem/shadersystem.cpp:544`, two files kept in sync by a comment in each asking
the reader to remember the other. One table here, and the bit is its index — which also
preserves the two rows that make no sense on their own (`$no_fullbright` is bit 1,
`MATERIAL_VAR_NO_DEBUG_OVERRIDE`; bit 9 is `$pseudotranslucent` and has no enumerator at
all).

### `ShaderKind`

```rust
pub enum ShaderKind { UnlitGeneric }

pub fn from_name(name: &str) -> Option<ShaderKind>;
pub fn name(self) -> &'static str;
pub fn params(self) -> impl Iterator<Item = &'static ShaderParam>;
pub fn param(self, name: &str) -> Option<&'static ShaderParam>;
pub fn wgsl(self) -> String;                  // prelude + body
```

Replaces `CShaderSystem::FindShader`'s `CUtlDict`, filled by whichever `shaderapi.so` had
been `dlopen`ed. There is no registration step and no way to fail to be registered.

**`_dx9`/`_dx8` suffixes are not handled and should not be.** Those were fallback shaders
picked by `GetFallbackShader` against a `dxlevel`; §4.1 deletes the mechanism with the
hardware variety that motivated it.

Free functions alongside it, all of them the shadow or dynamic phase for one material:

<a id="texture_requests"></a>

```rust
pub fn texture_requests(kind: ShaderKind, vmt: &Vmt) -> Vec<TextureRequest>;
pub fn param_value(kind: ShaderKind, vmt: &Vmt, name: &str) -> Option<MaterialVar>;
pub fn render_state(kind: ShaderKind, vmt: &Vmt, base: Option<&Texture>) -> RenderState;
pub fn modulation_color(kind: ShaderKind, vmt: &Vmt) -> [f32; 4];
pub fn unlit_uniforms(vmt: &Vmt) -> UnlitUniforms;
```

`param_value` is `InitShaderParameters` (`shadersystem.cpp:838`): the var if the file set
one, else the type's default, with `$color` and `$alpha` special-cased to white and 1 as
they are there. **Read parameters through it**, not through `Vmt::var`, or a material that
does not mention `$alpha` will read as fully transparent.

`render_state` needs the resolved base texture because it cannot decide blending without
it — `TextureIsTranslucent` asks whether the `.vtf` has an alpha channel *and* whether
some other flag has already claimed it (`$selfillum`, `$basealphaenvmapmask`,
`$opaquetexture`).

**`ShaderParam::declared_default` is documentation, not behaviour** — worth knowing
because `portdocs/MATERIALSYSTEM.md` §7.2 reads as though it were live. In the whole
original tree `m_pDefaultValue` is read by one file, `tools/vmt/vmtdoc.cpp`, the material
editor. At runtime the default comes from the type or from the shader's own
`SHADER_INIT_PARAMS` block.

### The constant ABI

`uniforms.rs` replaces `common_hlsl_cpp_consts.h` and the register maps at the top of
`common_vs_fxc.h` and `common_ps_fxc.h` — three files kept in sync by hand. Here there is
one set of `#[repr(C)] bytemuck::Pod` structs, mirrored once in `shaders/prelude.wgsl`.

Valve's register map is really a *frequency* map, and that frequency is the bind group:

| Group | Contents | Rate | Valve's registers |
|---|---|---|---|
| 0 | `FrameUniforms` | once a frame | VS `c2`, `c8..c11`, `c16`; PS `c29`, `c30`, `c32` |
| 1 | the shader's own block, plus its textures and samplers | once a material | the shader-specific block |
| 2 | `DrawUniforms` | once a draw | VS `c4..c7`, `c47` |
| 3 | skinning + morph storage buffers | once a skinned draw | VS `c58+`, `c1024+` |

Group 3 does not exist yet — nothing is skinned until `studiorender` is ported. Group 1's
*layout* is the shader's, which is the one thing that genuinely differs between shaders;
groups 0 and 2 are shared, which is what makes them worth being groups.

```rust
pub struct FrameUniforms {
    pub view_proj: ColumnMajor,             // cViewProj
    pub eye_pos_water_height: [f32; 4],     // cEyePos_WaterHeightW
    pub fog_params: [f32; 4],               // cFogParams
    pub fog_color: [f32; 4],                // g_LinearFogColor
    pub light_scale: [f32; 4],              // cLightScale
    pub screen_size: [f32; 4],              // cScreenSize
}
pub struct DrawUniforms {
    pub model: ColumnMajor,
    pub modulation: [f32; 4],               // cModulationColor
}
```

**Matrices are column-major and multiply on the left** — `m * vec4(pos, 1.0)`, with `m[3]`
the translation. Valve's is the opposite on both counts (row-major `VMatrix`, applied as
`mul( float4(pos,1), cViewProj )`), so a `VMatrix`-shaped matrix is transposed exactly
once on its way into a uniform. See the gotchas below; this is the single most expensive
thing in this module to get wrong, because it produces a plausible picture rather than an
error.

The 2x4 texture-coordinate transforms are the exception: they are passed as two explicit
*rows* and applied with `dot` against `(u, v, 0, 1)`, exactly as
`vertexlit_and_unlit_generic_vs20.fxc:498` does, so no matrix type is involved.

### `RenderState`, `PipelineKey` and `PipelineCache`

```rust
pub struct RenderState {
    pub blend: BlendMode,       // None | Blend | Add | BlendAdd | Multiply
    pub cull: bool,
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: DepthFunc,  // Nearer | NearerOrEqual
    pub depth_bias: DepthBias,  // None | Decal
    pub write_alpha: bool,
    pub alpha_to_coverage: bool,
}
pub struct PipelineKey { pub shader: ShaderKind, pub state: RenderState, pub target: TargetFormat }
pub struct TargetFormat { pub color: TextureFormat, pub depth: Option<TextureFormat>, pub samples: u32 }

pub fn get(&mut self, key: &PipelineKey) -> Arc<wgpu::RenderPipeline>;
pub fn layouts(&self) -> &BindLayouts;
```

`RenderState` is `StateSnapshot_t`, and `PipelineCache` is the deduplication half of
`TransitionTable.cpp`. The *other* half — computing the minimal state-change sequence
between two snapshots — is deleted with nothing in its place, because `set_pipeline` is
that, done by the driver against the hardware's real cost model.

`RenderState::default()` is `CShaderShadowDX8::SetDefaultState` field for field, and every
shadow phase starts there. The surprising default is `write_alpha: false`: Valve disables
alpha writes by default and shaders turn them back on only for fully opaque materials, so
that the frame's alpha channel is free to hold depth for the underwater pass. Two
orderings inside `render_state` are the original's and look wrong out of context —
`write_alpha` is decided *before* `$multiply` replaces the blend mode, and
`EnableAlphaBlending` turns depth writes off as a side effect of turning blending on.

`get` returns an `Arc` on purpose — asking the cache borrows it mutably and recording a
pass borrows the frame, so the pipeline has to outlive the lookup.

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **Matrices are column-major and multiply on the left.** `m[3]` is the translation, and
   WGSL applies `m * vec4(pos, 1.0)`. Valve is the opposite on both counts, so anything
   arriving from a `VMatrix` is transposed once on its way into a uniform and never again.
   Getting it backwards draws a plausible picture with everything in the wrong place — or,
   with an identity view, no visible difference at all, which is why
   `preview.rs`'s `the_model_matrix_places_the_quad` uses a view projection that is *not*
   the identity.
2. **`ColorSpace` is decided by the shader, and nothing checks it.** Getting it wrong
   produces a picture that looks *plausible* — a washed-out albedo, or a normal map that
   lights slightly wrong — rather than anything that errors.
   [`shader::texture_requests`](#texture_requests) is where the answer lives; a call site
   that decides for itself is a bug waiting to diverge from the shader that samples it.
3. **Read parameters through `shader::param_value`, not `Vmt::var`.** A `.vmt` that does
   not mention `$alpha` has no `$alpha` var, and treating that as zero makes every such
   material invisible. `param_value` is where `InitShaderParameters`' defaults live.
4. **A `Frame` borrows the renderer.** Compile the pipeline and write the uniform buffers
   *before* `begin_frame`. This is a real `wgpu` rule, not a Rust inconvenience — see
   [`Frame`](#framea).
5. **There is no depth buffer yet.** `TargetFormat::depth` is `None`, so `RenderState`'s
   four depth fields are carried in the pipeline key and not applied. They become live the
   day a depth attachment exists, with no other change — but until then, `$ignorez` and
   `$decal` look like they work and do nothing.
6. **A zero-size window is legal and must not reach `Surface::configure`, which panics on
   it.** Minimizing a window reports width or height 0. `resize(w, 0)` marks the surface
   unconfigured and `begin_frame` then returns `None` until real dimensions arrive. If you
   add another path that configures the surface, replicate that guard.
7. **`pre_present_notify` is the caller's job.** The renderer does not own a `winit`
   window, so it cannot make the call itself. It must happen immediately before
   `Frame::present`; skipping it costs compositor scheduling accuracy, not correctness.
8. **Sizes are physical pixels.** See `Renderer::new` above.
9. **The surface format is sRGB when the platform offers one** (`Bgra8UnormSrgb` on
   macOS/Metal). That is the replacement for `IShaderDevice::SetHardwareGammaRamp`: the
   hardware encodes on write instead of the engine warping the display's gamma ramp
   process-wide — and leaving it warped if it crashed. **Consequence:** values written by
   a shader are treated as *linear* and encoded on the way out. Do not apply an sRGB curve
   in shader code as well.
10. **Copies into a compressed texture use the level's *physical* size, not its logical
    one.** The tail of a DXT mip chain is levels smaller than a 4x4 block (a 64x64 DXT1
    texture ends 2x2, 1x1), and WebGPU requires a copy to be a whole number of blocks —
    writing the logical 2x2 is a validation error, not a silent truncation.
    `ImageFormat::mem_required` rounds the same way, because `GetMemRequired` did, so the
    byte counts agree. This bit once; `Texture::from_vtf` handles it.
11. **`Features::TEXTURE_COMPRESSION_BC` is required, not requested.** Essentially every
    texture Valve ships is DXT, and there is no fallback tier — decompressing on the CPU
    would quadruple both load time and video memory for the whole game. An adapter without
    it fails at startup with `RendererError::NoBlockCompression` rather than half-working.
12. **`required_limits` is `wgpu::Limits::default()` — the portable floor, not the
    adapter's ceiling.** Deliberate: §4.6 replaces `IMaterialSystemHardwareConfig`'s ~50
    caps queries and the `dxlevel` ladder with one fixed capability tier, and asking every
    machine for the same limits is what makes that tier mean anything. Raise it
    deliberately when a shader needs more; never adapter-by-adapter.
13. **Colour space of the swap chain is `Auto`, i.e. SDR.** Portal 2 ships HDR-lit maps,
    and HDR is still an open question (`portdocs/MATERIALSYSTEM.md` §10). Switching it on
    means a float format and a tonemap pass, not just changing this field.
14. **Backends are `METAL | VULKAN | GL`.** DX12 and BrowserWebGPU are omitted rather than
    merely unreachable, per `PORTING.md`'s POSIX-only rule. `WGPU_BACKEND` still overrides
    at runtime (as do `WGPU_ADAPTER_NAME` and `WGPU_DEBUG`) — those are left enabled on
    purpose as the modern equivalent of the old `-gl`/`-dx9` switches.
15. **`Renderer::new` blocks** on `pollster::block_on` for the adapter and device requests.
    Fine at startup, on the main thread, once. Do not call it from a frame.

## Deliberate divergences from Valve's behavior

Each of these changes what the engine does, and each names the thing that reverses it.

| Divergence | Why | Reversed by |
|---|---|---|
| Windowed 1280x720, vsync on | `videoconfig.cfg` is not ported, and the constructor defaults it would override are placeholders | `-fullscreen`, `-width`/`-height`, `-mat_vsync 0` |
| `DXT1_ONEBITALPHA` costs 8 bytes a block | `GetMemRequired` has no case for it and returns **0**, making any such `.vtf` unreadable. That is a defect, not a behavior | `ImageFormat::bytes_per_block` |
| `TEXTUREFLAGS_BORDER` clamps to edge | `AddressMode::ClampToBorder` needs a feature outside the capability tier. Visible only in the outermost texel of a texture sampled outside `[0,1]` | `SamplerKey::descriptor` |
| Missing mip levels are not allocated | `Unserialize` allocates the full chain to 1x1 and leaves absent levels uninitialized "for backward compatibility". `Vtf::mip_count` is what the file really has | — |
| `I8`/`IA88`/`A8` expand to RGBA | no `wgpu` equivalent of `D3DFMT_L8`/`A8` read semantics, and no view swizzle | `ImageFormat::to_gpu_bytes` |
| Anisotropy is on/off, never forced | `g_config.m_nForceAnisotropicLevel` and `MaximumAnisotropicLevel()` are video-options and caps state that do not exist yet | `sampler_key` |
| Frame `> 0` of an animated `.vtf` is never loaded | `TextureCache::load` asks for frame 0. `Texture::from_vtf` already takes the index | pass a `frame` to `from_vtf` |
| `$wireframe` draws solid | `PolygonMode::Line` needs `Features::POLYGON_MODE_LINE`, outside the capability tier, and Metal has no line fill mode at all | — |
| `$srgbtint` is not applied to the modulation colour | `ApplyColor2Factor` folds it in only under sRGB-correct blending, and linearizes it for the vertex path while leaving it gamma for the pixel path. It defaults to white and no Portal 2 content found so far sets it | `shader::modulation_color` |
| A blended material does not force alpha-testing at `1/255` | `CShaderShadowDX8` turns alpha test on at a reference of one 255th whenever standard alpha blending is on (`shadershadowdx8.cpp:793`), to skip fully transparent pixels. In WebGPU that is a `discard`, which costs early-Z, and the pixel contributes nothing either way | — |
| `$fallbackmaterial` is not followed | it is reachable only from inside the fallback-shader loop, which §4.1 deletes. A material that relies on it draws with its own shader instead | — |
| A cubemap or volume texture bound to a 2D sampler becomes the checkerboard | binding one is a `wgpu` validation error rather than a wrong picture, so it is treated as a broken texture and logged | — |

## Not implemented

Stages 4-8. There is no mesh API, no render context, no depth buffer and no render-target
stack, so the only thing that can be drawn is one full-screen quad. The shader set is one
shader deep. Also deliberately absent, and listed so nobody looks for them:

- **Everything `UnlitGeneric` can do beyond a base texture.** `$detail`, `$envmap` and
  `$envmapmask`, the distance-alpha family (`$distancealpha`, `$outline`, `$glow`, soft
  edges), `$decaltexture`, phong, and the flashlight. Each is declared in
  `unlitgeneric_dx9.cpp` and each is a texture and a branch away; `portdocs/MATERIALSYSTEM.md`
  §7.8 puts them with the shaders that share them, and each needs content to verify
  against. They are left out of the parameter table rather than declared-and-ignored,
  because a table that lists a parameter is a promise that setting it does something.
- **Material proxies.** `IMaterialProxy` and the `CreateInterface`-registered factory. The
  concept survives as a per-frame hook over the vars; the factory does not.
- **`$frame` animation.** Read but not acted on — `TextureCache` loads frame 0 (see the
  divergence table).
- **`CMaterialSubRect`** (`subrect` materials) and `mat_stub`.

- **`sv_pure`, `mat_picmip`, texture exclusion and streaming.** `CTextureManager` had all
  of it. None is worth rebuilding before there is a map to measure against.
- **MSAA.** `-mat_antialias` is parsed nowhere yet. A multisampled swap chain needs a
  separate render target plus a resolve, which belongs with the render-target stack in
  stage 4.
- **Exclusive fullscreen video modes.** `CVideoMode_Common`'s mode enumeration and
  `AdjustWindow`'s mode switching are not ported; fullscreen is borderless on the current
  monitor. On a modern compositor an exclusive mode change buys nothing and costs a
  display reconfiguration on every alt-tab.
- **Refresh rate, gamma, `mat_queue_mode`.** All config-file territory.
- **Any headless/null path** (`mat_stub.cpp`, `cmatnullrendercontext.cpp`,
  `shaderapiempty/`). §5.4: if one is ever wanted it is a single enum branch here, not
  three parallel no-op implementations.
- **Non-block-aligned compressed textures.** D3D9 padded internally; WebGPU refuses. A
  `.vtf` whose base level is not a whole number of 4x4 blocks reports
  `TextureError::NotBlockAligned`. Nothing in shipped Portal 2 content is one.

## `MaterialPreview` — temporary, and meant to be deleted

`preview.rs` draws one material over the whole frame. It exists because
`portdocs/MATERIALSYSTEM.md` §9 makes stage 3's deliverable "a quad drawn through a real
`.vmt` and a real WGSL shader", and because a `.vmt` that *parses* is no evidence that its
shader compiled, that its bind groups match their layouts, or that the matrix convention
this port just committed to is the one the shader reads.

What it owns is the render context's job, not a material's: the vertex and index buffers,
the per-frame and per-draw uniform blocks, and the decision of where the quad goes.

`src/launcher/`'s `-vmt <name>` switch is the way to ask for one:

```
cargo run -- -basedir /path/to/game -game portal2 -window -vmt metal/metalwall048a
```

A missing or broken name draws the error material — magenta checkerboard — which is itself
worth seeing.

Two things it pins down that are worth reading before writing the real one:

- **The quad is the unit square with `y` down**, and `VIEW_PROJ` maps it to the whole
  viewport with the flip. That flip reverses the winding, so `QUAD_INDICES` runs
  `0, 2, 1` rather than `0, 1, 2` — `FrontFace::Ccw` means counter-clockwise in *clip*
  space. Getting it backwards draws nothing at all, silently.
- **`VIEW_PROJ` is deliberately not the identity.** An identity view-projection would
  still draw a centred quad with a transposed matrix, and the convention would go
  unchecked until something with a real camera depended on it.

**Do not grow this.** Stage 4 brings typed vertex structs, real buffers and the render
state stack. When it lands, delete `preview.rs`, `Frame::draw_material` and the `-vmt`
switch together — and move the GPU tests at the bottom of that file onto whatever draws a
quad then, because they are the only place the whole path is checked against real pixels.

## Test coverage

100 tests, in two groups.

**Pure logic, no GPU** (92) — the parts where a mistake is invisible rather than loud:

| Tests | Guard |
|---|---|
| `image_format` (15) | the size arithmetic that decides where every mip level starts in a file, and every CPU format conversion, channel by channel |
| `vtf` (18) | every version 7.0-7.5, the seventh cubemap face, partial mip chains, the thumbnail, flag masking, and each way a file can be malformed |
| `vmt` (17) | the type sniffing, conditional keys, flags-are-not-vars, fallback blocks, and patch expansion against a real temp-directory `Vfs` |
| `shader` (13) | the shadow phase — every flag that maps onto pipeline state, the blend evaluation, the alpha-test reference, the texture transform, and the sRGB rule |
| `var` (12) | the value grammar and every coercion between the arms, plus the flag-name table against the bit constants |
| `texture` (7) | the `.vtf` flags -> sampler policy, and name normalization |
| `pipeline` (5) | `RenderState::default()` against `SetDefaultState` field by field, the blend factor pairs, and the vertex layout offsets |
| `uniforms` (3) | the uniform block sizes WGSL expects, and the no-fog packing |
| `material` (2) | material name normalization, and that the error material is a valid `UnlitGeneric` — it is built with `expect` at startup, so a typo in it would be a panic on every run |

The `vtf` tests build files with an in-memory writer that can produce *archaic* and
*malformed* ones deliberately — a 7.1 cubemap with its spheremap face, a 7.4 cubemap
without one, a truncated image, a mip count longer than the chain. Those are the cases
real content actually contains and that no valid-file test would reach. The `vmt` patch
tests write a small game directory into the temp dir and mount it, because `include` names
a file and there is no honest way to test the chain without one.

**End to end, on a real GPU** (8, in `preview.rs`) — a `.vmt` and a `.vtf`, through the
material system, onto the GPU, through real WGSL, and back to the CPU by rendering to an
offscreen target and reading the pixels back:

| Test | What it would catch |
|---|---|
| `a_material_draws_its_base_texture_the_right_way_up` | the three composed orientation conventions: the view projection's `y` flip, the quad's texture coordinates, and WebGPU's framebuffer origin |
| `the_model_matrix_places_the_quad` | a transposed matrix, against a view projection that is not the identity |
| `a_dxt1_texture_is_decoded_by_the_hardware` | block layout and the BC feature |
| `the_error_material_draws_the_checkerboard` | the whole fallback path, through the real `MaterialCache` |
| `colour_modulation_multiplies_the_texture` | `$color * $color2` reaching `cModulationColor` |
| `an_alpha_tested_material_discards_below_its_reference` | the `discard` that replaces D3D9's fixed-function alpha test |
| `a_material_with_no_base_texture_draws_white_not_the_checkerboard` | the difference between an *undefined* texture parameter and a *failed* one |
| `materials_with_the_same_state_share_one_pipeline` | the dedup that replaces `TransitionTable` |

They earn the GPU: row pitch, block layout, channel order, winding, matrix convention and
bind group layout are all invisible to a unit test, and each produces a *plausible* wrong
picture rather than a crash. The winding was in fact wrong first time round, and drew
nothing at all — silently — which is exactly what these are for.

**They skip, printing why, when no adapter with BC support is available**, so a machine
with no GPU still gets a green `cargo test`.

Nothing tests `Renderer` itself, and that stays deliberate: every function there either
calls `wgpu` or hands a value straight to it, so a unit test would assert that arguments
were passed along. What verifies it is running it. On macOS/Metal that produces:

```
source-engine: renderer: Apple M1 Pro (IntegratedGpu, "") via Metal
source-engine: renderer: 640x480 Bgra8UnormSrgb, vsync on
source-engine: materials: -vmt metal/wall -> UnlitGeneric (metal/wall), flags none
source-engine: renderer: first frame presented
```

The last line is the one that matters and is printed once, from `src/engine/window/`:
creating a device and creating a window both succeed on machines where nothing is ever
presented, so "a window opened" is not evidence that the GPU path works. That line is.
