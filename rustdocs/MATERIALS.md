# `src/materials/` — API reference

The material system. Right now that means five things: the GPU device and the frame
boundary; the texture path from a `.vtf` on disk to a sampler on the GPU; the material
path from a `.vmt` to a compiled pipeline; the geometry that pipeline draws; and the
render context that opens a pass and puts a camera behind it.

Porting design doc: [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named
after the *original* module (`materialsystem/`), while this file is named after the Rust
one (`src/materials/`). Same subject, two names, on purpose.

| | |
|---|---|
| Module | `crate::materials` |
| Lines | ~10,400 Rust including tests, plus ~260 of WGSL |
| Tests | 124 (`cargo test materials`) — 19 of them run on a real GPU |
| Dependencies | `wgpu` 30, `glam`, `bytemuck`, `pollster`, `thiserror` |
| Status | **Stages 1-4 of 8.** GPU bring-up, `.vtf` -> `wgpu::Texture`, `.vmt` -> `Material`, meshes, the render context and a depth buffer. Stages 5+ not started |

```
src/materials/
  renderer.rs      Renderer, Frame, RendererOptions — the device, the frame boundary, the depth buffer
  vtf.rs           Vtf, TextureFlags — reading .vtf files
  image_format.rs  ImageFormat, ColorSpace — pixel formats, size maths, CPU conversions
  texture.rs       Texture, TextureCache, SamplerKey — textures on the GPU
  var.rs           MaterialVar, MaterialFlags — what a .vmt says, and the coercions
  vmt.rs           Vmt — reading a .vmt: patches, conditionals, flags, vars
  shader.rs        ShaderKind, ShaderParam, render_state, vertex_layout — the shader set and its shadow phase
  uniforms.rs      FrameUniforms, DrawUniforms, from_mat4, from_row_major — the constant ABI (§7.4)
  pipeline.rs      RenderState, PipelineKey, TargetFormat, PipelineCache, BindLayouts
  material.rs      Material, MaterialCache — a .vmt bound to a shader and its textures
  mesh.rs          SimpleVertex, VertexLayout, VertexBuffer, IndexBuffer, DynamicBuffers, slices
  target.rs        DepthBuffer, RenderTarget, DEPTH_FORMAT — what a pass draws into
  context.rs       RenderContext, Pass, Camera, Load, StateOverride — passes and the constants under them
  preview.rs       MaterialPreview — the stage-4 verification draw. Temporary
  shaders/prelude.wgsl        the shared prelude (§7.5)
  shaders/unlitgeneric.wgsl   the one shader
  error.rs         RendererError, VtfError, VmtError, TextureError
```

**What is deliberately absent:** there is no `IShaderDevice`, no `IShaderAPI`, no
`IShaderShadow`, no device-abstraction trait, and no second backend. `wgpu` is called
directly. If you find yourself adding a trait so that "another renderer could be plugged
in later", stop — `wgpu` is already that abstraction, and re-adding the tower is the
specific mistake `portdocs/MATERIALSYSTEM.md` §5.1 exists to prevent.

**Also deliberately absent, as of stage 4:** the matrix stack, the render-target stack
and the scissor stack. They are not "not yet" — they are replaced. See
[Passes replace three stacks](#passes-replace-three-stacks).

## Quick start

```rust
use glam::{Mat4, Vec3};
use crate::materials::context::{Camera, Load};
use crate::materials::mesh::{IndexBuffer, SimpleVertex, VertexBuffer};
use crate::materials::{MaterialCache, RenderContext, Renderer, RendererOptions, CLEAR_COLOR};

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
let mut context = RenderContext::new(renderer.device(), renderer.queue(), materials.pipelines());

// Cannot fail: a missing or broken material is the error material.
let wall = materials.load(&vfs, "tools/toolsblack");

// Geometry that outlives the frame.
let vertices = VertexBuffer::new(renderer.device(), "quad", &[
    SimpleVertex::new([0.0, 0.0, 0.0], [0.0, 0.0]),
    SimpleVertex::new([1.0, 0.0, 0.0], [1.0, 0.0]),
    SimpleVertex::new([1.0, 1.0, 0.0], [1.0, 1.0]),
    SimpleVertex::new([0.0, 1.0, 0.0], [0.0, 1.0]),
]);
let indices = IndexBuffer::new(renderer.device(), "quad", &[0, 2, 1, 0, 3, 2]);

// ... once per frame:
context.begin_frame();                        // reclaims last frame's arenas
if let Some(mut frame) = renderer.begin_frame() {
    {
        let mut pass = context.pass(
            &mut frame,
            materials.pipelines(),
            &Camera::screen(),
            Load::Clear(CLEAR_COLOR),
        );
        pass.draw(&wall, &vertices.slice(), &indices.slice(), Mat4::IDENTITY);
    }                                          // the pass ends here, on drop
    window.pre_present_notify();
    frame.present();
}

// ... on WindowEvent::Resized. The depth buffer follows the surface.
renderer.resize(size.width, size.height);
```

Two orderings in there are not stylistic:

- **`context.begin_frame()` comes before `renderer.begin_frame()`.** It resets the
  uniform and geometry arenas, and anything still holding a slice from last frame will
  read whatever overwrites it.
- **The pass must end before the frame is presented**, which the inner block does. A
  `wgpu` encoder allows one open pass at a time, and `Frame::present` consumes the frame.

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
pub fn target_format(&self) -> TargetFormat;   // colour + depth + samples
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
pub fn size(&self) -> (u32, u32);              // physical pixels
pub fn target_format(&self) -> TargetFormat;   // == Renderer::target_format
pub fn clear(&mut self, color: wgpu::Color);   // colour and depth; draws nothing
pub fn present(self);                          // #[must_use] on the struct
```

One acquired swap-chain image, the renderer's depth attachment, and the `CommandEncoder`
recording into both. `present` consumes it: submit, then present. Dropping a `Frame`
without presenting discards everything recorded into it — correct for an abandoned
frame, silent data loss if accidental, which is why the type is `#[must_use]`.

**A `Frame` borrows the renderer mutably for its lifetime**, so anything that needs the
renderer itself — `resize`, another `begin_frame` — has to happen outside that borrow.
`Surface::configure` **panics** if a frame is alive, so the borrow checker is enforcing a
real `wgpu` rule. Do not work around it with interior mutability.

What that borrow does *not* block is drawing, because
[`RenderContext`](#the-render-context) holds its own `Device`/`Queue` handles: a pass
borrows the frame and the context together, and both are satisfied.

`Frame::clear` clears colour *and* depth and opens no pass of its own beyond that — it is
for a frame with nothing to draw. A frame that is drawing should clear as part of its
first pass (`Load::Clear`) rather than pay for two passes over the target.

Passes are opened through `RenderContext`, not here: `Frame::parts` — the encoder and the
two attachment views, borrowed together — is `pub(super)`, so opening a pass and deciding
what constants it carries stay in one place.

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

Note what is *not* a field of `PipelineKey`: the vertex layout. It comes from
`ShaderKind::vertex_layout()`, because that is where `IShaderShadow::VertexShaderVertexFormat`
put it — the shader declares the layout it reads, in its shadow phase. The key grows a
field the day one shader has two layouts, which will be `LightmappedGeneric`'s bumped
variant.

---

## Meshes

`src/materials/mesh.rs`. Replaces `public/materialsystem/imesh.h` (4,402 lines, ~3,900 of
them the inlined `CMeshBuilder`). None of that is ported: a vertex is a `#[repr(C)]`
struct, its GPU layout is derived from the struct, and filling a buffer is
`bytemuck::cast_slice`.

### Vertices and layouts

```rust
pub enum VertexLayout { Simple }
impl VertexLayout {
    pub fn buffer_layout(self) -> wgpu::VertexBufferLayout<'static>;
    pub fn stride(self) -> u64;
}

pub trait Vertex: Pod { const LAYOUT: VertexLayout; }

#[repr(C)]
pub struct SimpleVertex { pub position: [f32; 3], pub texcoord: [f32; 2], pub color: [f32; 4] }
impl SimpleVertex { pub const fn new(position: [f32; 3], texcoord: [f32; 2]) -> SimpleVertex; }
```

`VertexLayout` is `VertexFormat_t` (a `uint64` of flags plus per-texcoord sizes) reduced
to what it was used for. It is an enum, not a bitfield, because the set is not open: a
layout exists only if some shader reads it.

The layouts the rest of the shader set will need are enumerated on `VertexLayout`'s own
docs, with the `VertexShaderVertexFormat` call in each helper that says so. The short
version, since it is the expensive half of stage 4's reading:

| Shader | Attributes |
|---|---|
| `UnlitGeneric` | position, one texcoord, colour — `Simple` |
| `LightmappedGeneric` | position, base uv, lightmap uv, lightmap-offset uv, normal, tangent s/t, colour; the last three only when bumped |
| `VertexLitGeneric` | bone weights, position, normal, uv, plus a tangent from the `.vvd` when bumped |

Those structs arrive **with the shaders that read them**, not before.

### Buffers, and why vertices and indices are separate

```rust
pub struct VertexBuffer;   // static, immutable
impl VertexBuffer {
    pub fn new<V: Vertex>(device: &wgpu::Device, label: &str, vertices: &[V]) -> VertexBuffer;
    pub fn layout(&self) -> VertexLayout;
    pub fn slice(&self) -> VertexSlice;
}

pub struct IndexBuffer;    // static, immutable, 16-bit
impl IndexBuffer {
    pub fn new(device: &wgpu::Device, label: &str, indices: &[u16]) -> IndexBuffer;
    pub fn slice(&self) -> IndexSlice;
    pub fn range(&self, first: u32, count: u32) -> IndexSlice;   // IMesh::Draw(first, count)
}
```

**There is no `Mesh` type, deliberately.** `IMesh` inherits from both `IVertexBuffer` and
`IIndexBuffer`, so Valve's unit of geometry holds both — and every real draw path in the
engine works around that, which is why `GetDynamicMesh` grew `vertexOverride` and
`indexOverride` parameters:

- World brushes: every surface sharing a material goes into one static vertex buffer at
  map load (`engine/matsys_interface.cpp:1864`); each frame the *visible* ones' indices
  are gathered into a dynamic buffer (`engine/gl_rsurf.cpp:1168`).
- Models: identical shape (`studiorender/r_studiodraw.cpp:2268`).

Static vertices with dynamic indices is *the* pattern, not a special case. So a draw
takes a `VertexSlice` and an `IndexSlice` and does not care where either came from.

A slice holds a cloned `wgpu::Buffer`, which is a refcounted handle rather than the
allocation — one atomic increment. That is what lets a slice outlive the borrow of the
arena it came from.

`new` **panics on an empty slice**, matching `Assert( g_Meshes[i].vertCount > 0 )`:
`wgpu` refuses a zero-sized buffer, and an empty static buffer is a caller bug rather
than a state worth representing.

### `DynamicBuffers` — geometry that lives one frame

```rust
pub fn begin_frame(&mut self, device: &wgpu::Device);
pub fn vertices<V: Vertex>(&mut self, device, queue, vertices: &[V]) -> VertexSlice;
pub fn indices(&mut self, device, queue, indices: &[u16]) -> IndexSlice;
pub fn vertices_remaining(&self, layout: VertexLayout) -> u32;   // GetMaxVerticesToRender
pub fn indices_remaining(&self) -> u32;                          // GetMaxIndicesToRender
```

`shaderapidx9/dynamicvb.h`'s ring allocation, with the reasoning kept and the code
discarded: `Queue::write_buffer` stages its copy and orders it ahead of the submission
that reads it, so a bump allocator reset once a frame is the whole of it. It **grows**
rather than failing, unlike Valve's fixed allocation, but the `*_remaining` queries are
still there because a batcher that splits before it overflows produces fewer
reallocations.

In practice you reach these through `Pass::vertices` / `Pass::indices`, which is where
allocation and drawing interleave.

---

## The render context

`src/materials/context.rs`. Replaces `cmatrendercontext.cpp` (3,455 lines);
`CMatQueuedRenderContext` and `cmaterial_queuefriendly` are deleted outright (§5.3).

### Passes replace three stacks

`CMatRenderContextBase` carries `m_MatrixStacks[NUM_MATRIX_MODES]`, `m_RenderTargetStack`
and `m_ScissorRectStack`, driven in the fixed-function idiom OpenGL 1.x taught it —
`MatrixMode`, `PushMatrix`, `LoadIdentity`, `Ortho`, draw, `PopMatrix`
(`engine/gl_rmain.cpp:920-985` does it three times over). Those stacks exist because D3D9
had one global device whose state every draw shared.

**A `wgpu` render pass already is that saved state.** So:

| `CMatRenderContext` | Here |
|---|---|
| `m_RenderTargetStack` entry: targets + depth + viewport | the arguments to `pass` / `target_pass` |
| `MATERIAL_VIEW` / `MATERIAL_PROJECTION` stacks | `Camera`, a pass argument |
| `MATERIAL_MODEL` stack | a parameter of `Pass::draw` |
| `m_ScissorRectStack` | `Pass::set_scissor`, which the pass ends |
| `PushRenderTargetAndViewport` / `Pop` | opening a pass and letting it drop |
| `OverrideDepthEnable`, `CullMode`, `FlipCullMode` | `StateOverride` |

**`wgpu` render passes do not nest.** One must end before the next begins on the same
encoder, so portal views, water reflections and post-processing run *innermost first* —
render into a `RenderTarget`, end that pass, then open the pass that samples it. That is
the resolution of what `portdocs/MATERIALSYSTEM.md` §10 called the highest-risk unknown
after the shaders: the RT stack does not need restructuring, it needs deleting, and the
dependency order it implied becomes explicit.

### `RenderContext`

```rust
pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, pipelines: &PipelineCache) -> RenderContext;
pub fn begin_frame(&mut self);

pub fn pass<'a>(&'a mut self, frame: &'a mut Frame<'_>, pipelines: &'a mut PipelineCache,
                camera: &Camera, load: Load) -> Pass<'a>;
pub fn target_pass<'a>(&'a mut self, frame: &'a mut Frame<'_>, pipelines: &'a mut PipelineCache,
                       target: &'a RenderTarget, camera: &Camera, load: Load) -> Pass<'a>;
pub fn offscreen_pass<'a>(&'a mut self, encoder: &'a mut wgpu::CommandEncoder,
                          pipelines: &'a mut PipelineCache, target: &RenderTarget,
                          camera: &Camera, load: Load) -> Pass<'a>;
```

`pass` draws to the swap-chain image and the renderer's depth buffer; `target_pass` to an
offscreen `RenderTarget` using the frame's encoder; `offscreen_pass` to one with an
encoder the caller supplies and submits, for rendering that is not part of a presented
frame.

`pipelines` is passed in rather than owned because `MaterialCache` owns the
`PipelineCache` and needs it to build materials. The two `&mut` borrows are of different
objects, so they coexist; a material comes out of the cache as an `Arc<Material>` and is
therefore independent of the cache's borrow.

### Uniforms are arenas, not single buffers

**The one hazard this module exists to prevent, and it is not obvious.** Every pass
recorded into a frame is submitted as *one command buffer*, and `Queue::write_buffer`
stages its copy to run before that whole command buffer — not at the point in the
recording where it was called. So writing one uniform buffer per draw and rewriting it
for the next does not give each draw its own constants: it gives **every draw in the
frame the last values written**.

Both blocks are therefore bump-allocated out of a large buffer and bound with a dynamic
offset — one slot per pass for `FrameUniforms`, one per draw for `DrawUniforms`. Slots
are padded to `min_uniform_buffer_offset_alignment` (256 on the portable floor), so a
96-byte block costs 256 bytes. That is the price of one bind group per draw instead of
one buffer per draw.

`each_draw_gets_its_own_model_matrix` in `preview.rs` is the regression test.

### `Camera`

```rust
pub struct Camera { pub view: Mat4, pub projection: Mat4, pub eye: Vec3 }

pub fn screen() -> Camera;
pub fn perspective(eye: Vec3, view: Mat4, fov_x_degrees: f32, aspect: f32,
                   z_near: f32, z_far: f32) -> Camera;
pub fn orthographic(eye: Vec3, view: Mat4, left: f32, right: f32, bottom: f32, top: f32,
                    z_near: f32, z_far: f32) -> Camera;
pub fn view_proj(&self) -> Mat4;   // projection * view
```

**Use these rather than reaching into `glam` directly.** They go through
`glam::camera::rh::proj::directx`, where `directx` names the *NDC convention* and not the
API: right-handed Y-up view space in, depth in `0..1` and Y-up out, which is exactly
WebGPU's. The `opengl` module produces `-1..1` and the `vulkan` one flips Y; either would
draw a picture, just the wrong one. (`Mat4::perspective_rh` is the deprecated spelling of
the same function.)

`fov_x_degrees` is the **horizontal** field of view, because every Valve entry point takes
it that way (`CViewSetup::fov`); `glam` wants the vertical one, and the conversion here is
`CalcFovY`'s.

`Camera::screen()` is the 2D setup, `y` down and **`z` away from the viewer**. It passes
`glam`'s `near`/`far` reversed, and that is deliberate: `glam`'s are distances along `-z`,
so the natural-looking `-1.0, 1.0` would make a *larger* `z` mean *nearer* — a
painter's-order trap for anything that thinks in layers.
`the_screen_camera_puts_z_into_the_screen` pins it.

### `Pass<'a>`

```rust
pub fn draw(&mut self, material: &Material, vertices: &VertexSlice, indices: &IndexSlice,
            model: Mat4);
pub fn draw_modulated(&mut self, material: &Material, vertices: &VertexSlice,
                      indices: &IndexSlice, model: Mat4, modulation: [f32; 4]);

pub fn vertices<V: Vertex>(&mut self, vertices: &[V]) -> VertexSlice;
pub fn indices(&mut self, indices: &[u16]) -> IndexSlice;
pub fn vertices_remaining(&self, layout: VertexLayout) -> u32;
pub fn indices_remaining(&self) -> u32;

pub fn set_viewport(&mut self, x: f32, y: f32, width: f32, height: f32);
pub fn set_depth_range(&mut self, x: f32, y: f32, width: f32, height: f32,
                       near: f32, far: f32);
pub fn set_scissor(&mut self, x: u32, y: u32, width: u32, height: u32);
pub fn set_state_override(&mut self, overrides: StateOverride);
pub fn target_format(&self) -> TargetFormat;
```

The pass ends when it drops. `model` is object space to world space — the
`MATERIAL_MODEL` matrix, as a parameter rather than a stack, because unlike view and
projection it genuinely changes between draws and a caller doing a hierarchical traversal
already has it in hand.

`draw_modulated` is `IMesh::DrawModulated`: the per-instance colour is multiplied by the
material's own, which is what `CBaseMeshDX8::DrawMesh` (`shaderapidx9/meshdx8.cpp:2378`)
did before every draw.

**`draw` panics if the vertex slice's layout is not what the material's shader declared.**
That is a programming error, not a data error — both halves are ours — and the
alternative is `wgpu` reading a model's bone weights as a lightmap coordinate and drawing
something that merely looks wrong. An empty slice is *not* an error; it draws nothing.

`vertices`/`indices` live on the pass, not on `RenderContext`, because that is how the
engine draws: `GetDynamicMesh` -> fill -> `Draw`, once per batch, over and over inside
what is one pass here. An API that made a caller allocate everything before opening a
pass would be unusable for exactly the two call sites stage 4 was designed against.

### `Load` and `StateOverride`

```rust
pub enum Load { Clear(wgpu::Color), Keep }

pub struct StateOverride {
    pub cull: Option<bool>,          // CullMode / FlipCullMode
    pub depth_test: Option<bool>,    // OverrideDepthEnable
    pub depth_write: Option<bool>,
}
```

`Load::Clear` clears colour *and* depth — `ClearBuffers( true, true )`. `None` fields of a
`StateOverride` leave the material's own choice alone; it applies from the call to the end
of the pass. `FlipCullMode` is not a debug feature: a mirror or a portal view flips the
view matrix horizontally, reversing every triangle's winding, and without the flip the
whole reflected world is back-face culled.

---

## Render targets and the depth buffer

`src/materials/target.rs`.

```rust
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;
pub const CLEAR_DEPTH: f32 = 1.0;

pub struct DepthBuffer;
impl DepthBuffer {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> DepthBuffer;   // panics on zero
    pub fn view(&self) -> &wgpu::TextureView;
    pub fn size(&self) -> (u32, u32);
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) -> bool;
}

pub struct RenderTarget;
impl RenderTarget {
    pub fn new(device: &wgpu::Device, name: &str, width: u32, height: u32,
               color_format: wgpu::TextureFormat, depth: bool) -> RenderTarget;
    pub fn texture(&self) -> &Arc<Texture>;    // bindable as a material's texture
    pub fn format(&self) -> TargetFormat;
    pub fn size(&self) -> (u32, u32);
}
```

**The back buffer's depth attachment belongs to `Renderer`**, not to the render context:
D3D9 created it as part of the swap chain, and resizing it anywhere else would mean two
places that have to agree about the window's size. `Renderer::resize` keeps them in step;
a mismatch is a `wgpu` validation error, not a stretched picture.

`DEPTH_FORMAT` is 24-bit depth plus 8 bits of stencil, and the choice is load-bearing for
three reasons documented at the constant: Portal 2's portal surfaces need the stencil,
`DepthBias::Decal`'s -64 was derived by multiplying Valve's `mat_depthbias_decal` by 2^24
(a float depth format scales the bias differently and would be quietly wrong), and it
costs nothing from the capability tier. Nothing writes the stencil yet — pipelines carry
`StencilState::default()` and passes leave `stencil_ops` at `None`, which `wgpu` reads as
a read-only stencil aspect.

A `RenderTarget` is **not** registered in `TextureCache`.
`CreateNamedRenderTargetTextureEx` put render targets in the same dictionary as `.vtf`
files, so a `.vtf` of the same name could silently shadow one. Here a render target is a
value the caller holds, and binding one to a material is an explicit act.

Multiple colour attachments (`MAX_RENDER_TARGETS` is 4) are not implemented: the only
things in the tree that bind more than one are a lighting-preview G-buffer path behind an
`#if 0` and CS:GO's `character_ssao`.


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
4. **Per-draw constants must go in distinct arena slots, and this is not obvious.**
   `Queue::write_buffer` stages its copy to run before the *whole* command buffer, not at
   the point in the recording where it was called — so one uniform buffer rewritten
   between draws gives every draw in the frame the last values written. `RenderContext`
   handles it; anything that adds a new per-draw block must too. See
   [Uniforms are arenas](#uniforms-are-arenas-not-single-buffers).
5. **`RenderContext::begin_frame` must run before anything allocates, once a frame.** It
   resets the uniform and geometry arenas. A slice held over from the previous frame
   reads whatever overwrites it — silently, and only under load, because the arena has to
   wrap round to the same offset first.
6. **A `Frame` borrows the renderer.** `resize` and a second `begin_frame` have to happen
   outside that borrow: `Surface::configure` panics if a frame is alive, so the borrow
   checker is enforcing a real `wgpu` rule. Drawing is unaffected — `RenderContext` holds
   its own device handles.
7. **`wgpu` render passes do not nest**, so a render target is filled by a pass that has
   *ended* before the pass that samples it begins. See
   [Passes replace three stacks](#passes-replace-three-stacks).
8. **`Pass::draw` panics on a vertex-layout mismatch.** The layout comes from the
   material's shader (`ShaderKind::vertex_layout`), not from the buffer, and drawing
   model data through a world shader would otherwise reinterpret bone weights as
   coordinates and draw something merely wrong.
9. **A zero-size window is legal and must not reach `Surface::configure`, which panics on
   it.** Minimizing a window reports width or height 0. `resize(w, 0)` marks the surface
   unconfigured and `begin_frame` then returns `None` until real dimensions arrive. If you
   add another path that configures the surface, replicate that guard.
10. **`pre_present_notify` is the caller's job.** The renderer does not own a `winit`
   window, so it cannot make the call itself. It must happen immediately before
   `Frame::present`; skipping it costs compositor scheduling accuracy, not correctness.
11. **Sizes are physical pixels.** See `Renderer::new` above.
12. **The surface format is sRGB when the platform offers one** (`Bgra8UnormSrgb` on
   macOS/Metal). That is the replacement for `IShaderDevice::SetHardwareGammaRamp`: the
   hardware encodes on write instead of the engine warping the display's gamma ramp
   process-wide — and leaving it warped if it crashed. **Consequence:** values written by
   a shader are treated as *linear* and encoded on the way out. Do not apply an sRGB curve
   in shader code as well.
13. **Copies into a compressed texture use the level's *physical* size, not its logical
    one.** The tail of a DXT mip chain is levels smaller than a 4x4 block (a 64x64 DXT1
    texture ends 2x2, 1x1), and WebGPU requires a copy to be a whole number of blocks —
    writing the logical 2x2 is a validation error, not a silent truncation.
    `ImageFormat::mem_required` rounds the same way, because `GetMemRequired` did, so the
    byte counts agree. This bit once; `Texture::from_vtf` handles it.
14. **`Features::TEXTURE_COMPRESSION_BC` is required, not requested.** Essentially every
    texture Valve ships is DXT, and there is no fallback tier — decompressing on the CPU
    would quadruple both load time and video memory for the whole game. An adapter without
    it fails at startup with `RendererError::NoBlockCompression` rather than half-working.
15. **`required_limits` is `wgpu::Limits::default()` — the portable floor, not the
    adapter's ceiling.** Deliberate: §4.6 replaces `IMaterialSystemHardwareConfig`'s ~50
    caps queries and the `dxlevel` ladder with one fixed capability tier, and asking every
    machine for the same limits is what makes that tier mean anything. Raise it
    deliberately when a shader needs more; never adapter-by-adapter.
16. **Colour space of the swap chain is `Auto`, i.e. SDR.** Portal 2 ships HDR-lit maps,
    and HDR is still an open question (`portdocs/MATERIALSYSTEM.md` §10). Switching it on
    means a float format and a tonemap pass, not just changing this field.
17. **Backends are `METAL | VULKAN | GL`.** DX12 and BrowserWebGPU are omitted rather than
    merely unreachable, per `PORTING.md`'s POSIX-only rule. `WGPU_BACKEND` still overrides
    at runtime (as do `WGPU_ADAPTER_NAME` and `WGPU_DEBUG`) — those are left enabled on
    purpose as the modern equivalent of the old `-gl`/`-dx9` switches.
18. **`Renderer::new` blocks** on `pollster::block_on` for the adapter and device requests.
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
| The matrix, render-target and scissor stacks do not exist | a `wgpu` render pass already *is* the saved state those stacks restored, and it is scoped by the borrow checker. A nested render target becomes a pass that ends before the one sampling it begins | — |
| Render targets are not in the texture dictionary | `CreateNamedRenderTargetTextureEx` shared a namespace with `.vtf` files, so a file could silently shadow a target. Binding one is now an explicit act | — |
| The depth buffer is `Depth24PlusStencil8` for everything | Portal's stencil, and `DepthBias::Decal`'s constant being in 24-bit depth units. Valve chose the format per render target | `target::DEPTH_FORMAT` |
| The dynamic geometry arena grows instead of overflowing | `CDynamicVB` was a fixed allocation and callers split batches to fit. The `*_remaining` queries are still there for callers that want to | `mesh::ARENA_BYTES` |

## Not implemented

Stages 5-8: lightmaps, the rest of the shader set, paint maps, GPU morph. The shader set
is one shader deep, so most `.vmt` files in shipped content resolve to the error material
because they name a shader that does not exist yet. Also deliberately absent, and listed
so nobody looks for them:

- **`WorldVertex` and `ModelVertex`.** The layouts are enumerated on `VertexLayout` — the
  reading is done — but the structs arrive with `LightmappedGeneric` and
  `VertexLitGeneric`, because a vertex struct with no shader to read it cannot be checked
  against anything.
- **Vertex compression** (`VERTEX_FORMAT_COMPRESSED`, packed normals and bone weights).
  Still open, and answered *with* skinning: the shaders unpack what the vertex format
  packs, so the two halves are one decision.
- **32-bit indices.** `MATERIAL_INDEX_FORMAT_32BIT` exists in the enum but nothing in the
  engine's draw paths asks for it — a batch is bounded by `GetMaxIndicesToRender` long
  before it reaches 65,536 vertices.
- **Multiple colour attachments.** `MAX_RENDER_TARGETS` is 4; nothing in scope binds more
  than one.
- **Stencil.** The depth format carries eight bits of it for Portal's sake, and no
  pipeline writes it. Portal surfaces are what will.
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
- **MSAA.** `-mat_antialias` is parsed nowhere yet. `TargetFormat::samples` is in the
  pipeline key and always 1; a multisampled back buffer needs a resolve target on the
  pass and the same `samples` on every pipeline — one field in each of two places, not a
  design question.
- **Exclusive fullscreen video modes.** `CVideoMode_Common`'s mode enumeration and
  `AdjustWindow`'s mode switching are not ported; fullscreen is borderless on the current
  monitor. On a modern compositor an exclusive mode change buys nothing and costs a
  display reconfiguration on every alt-tab.
- **Refresh rate, gamma, `mat_queue_mode`.** All config-file territory.
- **Parallel command encoding.** §5.3 deletes the queued render context but keeps its
  reasoning; `RenderContext` records into one encoder on one thread. Nothing in it reaches
  global mutable state, which is the property that keeps the door open.
- **Any headless/null path** (`mat_stub.cpp`, `cmatnullrendercontext.cpp`,
  `shaderapiempty/`). §5.4: if one is ever wanted it is a single enum branch here, not
  three parallel no-op implementations.
- **Non-block-aligned compressed textures.** D3D9 padded internally; WebGPU refuses. A
  `.vtf` whose base level is not a whole number of 4x4 blocks reports
  `TextureError::NotBlockAligned`. Nothing in shipped Portal 2 content is one.

## `MaterialPreview` — temporary, and meant to be deleted

`preview.rs` draws one material on two overlapping cubes and a ground quad, seen through
an orbiting perspective camera. It exists because `portdocs/MATERIALSYSTEM.md` §9 makes
stage 4's deliverable typed vertex buffers, static and dynamic geometry, and a depth
buffer that resolves occlusion — and none of those is a thing a unit test can see. A
full-screen quad could not tell a working depth buffer from an absent one.

What changed from stage 3's version is the measure of the stage: it used to own the
vertex and index buffers, both uniform blocks, the bind groups and the choice of where
the quad went. All of that is `RenderContext`'s now. What is left is a cube, a camera and
a clock — a *scene*, which is the engine's job.

`src/launcher/`'s `-vmt <name>` switch is the way to ask for one:

```
cargo run -- -basedir /path/to/game -game portal2 -window -vmt tools/toolsblack
```

A missing or broken name draws the error material — magenta checkerboard — which is
itself worth seeing. Most Portal 2 world materials name `LightmappedGeneric` and will do
exactly that until stage 6.

Two things it pins down that are worth reading before writing the real one:

- **The cube is built rather than written out**, from a per-face `(normal, u, v)` triple
  chosen so that `u × v == n`. That is what makes each face wind counter-clockwise as
  seen from outside without anyone having to check twenty-four literals. Getting one face
  backwards does not draw it mirrored — it draws a hole, silently, invisible from most
  angles.
- **The ground quad is dynamic on purpose**, not because it changes. The dynamic vertex
  path is what every immediate-mode draw in the engine uses, and a path only the tests
  exercise is a path that rots.

**Do not grow this.** When map loading lands, delete `preview.rs` and the `-vmt` switch
together, and move the GPU tests at the bottom of that file onto whatever draws the world
— they are the only place the whole path is checked against real pixels.

## Test coverage

124 tests, in two groups.

**Pure logic, no GPU** (105) — the parts where a mistake is invisible rather than loud:

| Tests | Guard |
|---|---|
| `vtf` (18) | every version 7.0-7.5, the seventh cubemap face, partial mip chains, the thumbnail, flag masking, and each way a file can be malformed |
| `vmt` (17) | the type sniffing, conditional keys, flags-are-not-vars, fallback blocks, and patch expansion against a real temp-directory `Vfs` |
| `image_format` (15) | the size arithmetic that decides where every mip level starts in a file, and every CPU format conversion, channel by channel |
| `shader` (13) | the shadow phase — every flag that maps onto pipeline state, the blend evaluation, the alpha-test reference, the texture transform, and the sRGB rule |
| `var` (12) | the value grammar and every coercion between the arms, plus the flag-name table against the bit constants |
| `texture` (7) | the `.vtf` flags -> sampler policy, and name normalization |
| `uniforms` (7) | the uniform block sizes WGSL expects, the no-fog packing, and the row-major/column-major conversion in both directions |
| `pipeline` (5) | `RenderState::default()` against `SetDefaultState` field by field, the blend factor pairs, and that the shader is what decides the vertex layout |
| `context` (5) | the projection conventions — depth in `0..1`, `z` into the screen, horizontal-to-vertical fov — and that a `StateOverride` touches only what it names |
| `mesh` (4) | the vertex layout against the struct it describes, and the copy-alignment padding at every remainder |
| `material` (2) | material name normalization, and that the error material is a valid `UnlitGeneric` — it is built with `expect` at startup, so a typo in it would be a panic on every run |

The `vtf` tests build files with an in-memory writer that can produce *archaic* and
*malformed* ones deliberately — a 7.1 cubemap with its spheremap face, a 7.4 cubemap
without one, a truncated image, a mip count longer than the chain. Those are the cases
real content actually contains and that no valid-file test would reach. The `vmt` patch
tests write a small game directory into the temp dir and mount it, because `include` names
a file and there is no honest way to test the chain without one.

The `context` projection tests deserve a word: `glam` offers three NDC conventions one
module path apart, and its `near`/`far` are distances along `-z` rather than `z` values.
`the_screen_camera_puts_z_into_the_screen` exists because that second point was got wrong
first time round — the GPU depth test is what caught it, and this is the cheap check that
keeps it caught.

**End to end, on a real GPU** (19, in `preview.rs`) — a `.vmt` and a `.vtf`, through the
material system, onto the GPU, through real WGSL, and back to the CPU by rendering to an
offscreen `RenderTarget` and reading the pixels back:

| Test | What it would catch |
|---|---|
| `a_material_draws_its_base_texture_the_right_way_up` | the three composed orientation conventions: the camera's `y` flip, the quad's texture coordinates, and WebGPU's framebuffer origin |
| `the_depth_buffer_decides_which_of_two_overlapping_quads_is_seen` | a depth buffer that is attached but not tested, or tested in the wrong direction |
| `without_a_depth_buffer_the_last_draw_wins` | the control for the one above — without it, that test proves nothing |
| `each_draw_gets_its_own_model_matrix` | the per-draw uniform arena: one buffer rewritten between draws would give both draws the second matrix |
| `a_render_target_can_be_drawn_into_and_then_sampled` | the render-to-texture path that replaces `PushRenderTargetAndViewport` |
| `a_state_override_turns_the_depth_test_off` | `OverrideDepthEnable`, the `$ignorez` path |
| `back_faces_are_culled_and_a_state_override_can_stop_it` | the winding convention, and `CullMode`/`FlipCullMode` |
| `a_static_vertex_buffer_can_be_drawn_with_dynamic_indices` | the world and model draw pattern — the reason vertex and index buffers are separate |
| `an_index_range_draws_only_its_batch` | `IMesh::Draw( first, count )` over a shared buffer |
| `an_odd_number_of_indices_draws` | the copy-alignment padding leaking into the draw as a second, garbage triangle |
| `the_cube_is_wound_so_that_every_face_survives_culling` | a hole in a cube, which is invisible from most angles and obvious from one |
| `the_preview_scene_draws_its_ground` | the other half of that: a quad wound the wrong way is not a wrong picture but an absent one |
| `a_dxt1_texture_is_decoded_by_the_hardware` | block layout and the BC feature |
| `the_error_material_draws_the_checkerboard` | the whole fallback path, through the real `MaterialCache` |
| `colour_modulation_multiplies_the_texture` | `$color * $color2` reaching `cModulationColor` |
| `modulation_multiplies_the_material_by_the_instance` | `IMesh::DrawModulated` — the per-instance colour on top of the material's |
| `an_alpha_tested_material_discards_below_its_reference` | the `discard` that replaces D3D9's fixed-function alpha test |
| `a_material_with_no_base_texture_draws_white_not_the_checkerboard` | the difference between an *undefined* texture parameter and a *failed* one |
| `identical_states_share_one_pipeline` | the dedup that replaces `TransitionTable` |

They earn the GPU: row pitch, block layout, channel order, winding, matrix convention,
depth direction and bind group layout are all invisible to a unit test, and each produces
a *plausible* wrong picture rather than a crash. `the_depth_buffer_decides_which_of_two_overlapping_quads_is_seen`
earned its place immediately: it caught `Camera::screen` passing `glam` its near and far
planes the wrong way round, which had inverted the depth comparison for every 2D draw.

**They skip, printing why, when no adapter with BC support is available**, so a machine
with no GPU still gets a green `cargo test`.
