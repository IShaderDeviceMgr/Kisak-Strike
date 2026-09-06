# `src/materials/` — API reference

The material system. Right now that means six things: the GPU device and the frame
boundary; the texture path from a `.vtf` on disk to a sampler on the GPU; the material
path from a `.vmt` to a compiled pipeline; the geometry that pipeline draws; the render
context that opens a pass and puts a camera behind it; and the lightmap atlas a map's
baked lighting is packed into.

Porting design doc: [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named
after the *original* module (`materialsystem/`), while this file is named after the Rust
one (`src/materials/`). Same subject, two names, on purpose.

| | |
|---|---|
| Module | `crate::materials` |
| Lines | ~14,900 Rust including tests, plus ~1,200 of WGSL |
| Tests | 168 (`cargo test materials`) — 28 of them run on a real GPU |
| Dependencies | `wgpu` 30, `glam`, `bytemuck`, `pollster`, `thiserror`, and `egui`/`egui-wgpu` in [`ui`](#uirenderer) alone |
| Status | **Stages 1-6 of 8.** GPU bring-up, `.vtf` -> `wgpu::Texture`, `.vmt` -> `Material`, meshes, the render context, a depth buffer, lightmaps, and `VertexLitGeneric`. Stage 6's remaining shaders and stages 7-8 not started |

```
src/materials/
  renderer.rs      Renderer, Frame, RendererOptions — the device, the frame boundary, the depth buffer
  vtf.rs           Vtf, TextureFlags — reading .vtf files
  image_format.rs  ImageFormat, ColorSpace — pixel formats, size maths, CPU conversions
  texture.rs       Texture, TextureCache, SamplerKey — textures on the GPU
  var.rs           MaterialVar, MaterialFlags — what a .vmt says, and the coercions
  vmt.rs           Vmt — reading a .vmt: patches, conditionals, flags, vars
  shader.rs        ShaderKind, ShaderParam, render_state, vertex_layout — the shader set and its shadow phase
  uniforms.rs      FrameUniforms, DrawUniforms, ModelLighting, Light, from_mat4 — the constant ABI (§7.4)
  pipeline.rs      RenderState, PipelineKey, TargetFormat, PipelineCache, BindLayouts
  material.rs      Material, MaterialCache — a .vmt bound to a shader and its textures
  lightmap.rs      ImagePacker, LightmapAtlas, LightmapPages, ColorRgbExp32 — the lightmap atlas
  mesh.rs          SimpleVertex, WorldVertex, ModelVertex, VertexLayout, VertexBuffer, IndexBuffer, DynamicBuffers
  target.rs        DepthBuffer, RenderTarget, DEPTH_FORMAT — what a pass draws into
  context.rs       RenderContext, Pass, Camera, Load, StateOverride — passes and the constants under them
  preview.rs       MaterialPreview — the stage-4 verification draw. Temporary
  ui.rs            UiRenderer — the egui pass over the frame. Not part of the material system
  shaders/prelude.wgsl             the shared prelude (§7.5)
  shaders/unlitgeneric.wgsl        base texture, modulation, alpha test
  shaders/lightmappedgeneric.wgsl  base texture x baked lightmap, flat and bumped
  shaders/vertexlitgeneric.wgsl    models: ambient cube, local lights, baked vertex light
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
pub fn queue(&self) -> &wgpu::Queue;
pub fn layouts(&self) -> &BindLayouts;
```

`queue` and `layouts` exist for the lightmap atlas, which is built by
`src/engine/world/` and has to upload through the same device and bind against the same
layouts. They are immutable, unlike `pipelines`, so a caller can hold both at once —
`LightmapAtlas::upload` needs exactly that.

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
    pub lighting: Lighting,       // how wide a lightmap block its surfaces reserve
}

pub fn bind_group(&self) -> &wgpu::BindGroup;               // group 1

pub fn new(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layouts: &BindLayouts,
    name: &str,
    vmt: &Vmt,
    fallback: &TextureFallbacks,   // { white, error, black_cube }
    resolve: impl FnMut(&str, ColorSpace, TextureDimension) -> Arc<Texture>,
) -> Option<Material>;   // None if the .vmt names a shader we do not have
```

`resolve` is told the **view dimension** the shader declared, because a bind group layout
names one and binding the wrong shape is a `wgpu` validation error rather than a wrong
picture. `MaterialCache` uses it to send cubemaps through
[`load_cubemap`](#envmap_name)'s `.hdr` rule and everything else through `load`; a texture
that comes back the wrong shape is replaced with the fallback of the right one and logged.

`TextureFallbacks` has three entries and the split is by *situation*, not by failure — the
same distinction `CTextureManager`'s family of standard textures draws. A parameter nobody
set gets `white` (or `black_cube`); one that was set and could not be honoured gets
`error` (or, again, `black_cube` — there is no error cubemap to invent, and a checkerboard
reflection reads as a broken world rather than a broken material).

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
pub enum ShaderKind { UnlitGeneric, LightmappedGeneric, VertexLitGeneric }

pub fn from_name(name: &str) -> Option<ShaderKind>;
pub fn name(self) -> &'static str;
pub fn vertex_layout(self) -> VertexLayout;
pub fn lighting_binding(self) -> Option<LightingBinding>;   // what group 3 holds
pub fn params(self) -> impl Iterator<Item = &'static ShaderParam>;
pub fn param(self, name: &str) -> Option<&'static ShaderParam>;
pub fn wgsl(self) -> String;                  // prelude + body
```

Three deep. `UnlitGeneric` is sprites, tool textures and anything whose colour is entirely
in its texture; `LightmappedGeneric` is world brush surfaces — 62 of `sp_a1_intro1`'s 66
world materials — and multiplies a base texture by a baked lightmap, flat or
radiosity-normal-mapped; `VertexLitGeneric` is models, and is the largest shader in the
shipped game — 1,108 of Portal 2's 3,431 materials name it, including 1,012 of the 1,096
under `materials/models/`.

**A `.vmt` naming `VertexLitGeneric` does not always reach `VertexLitGeneric`.**
`DrawVertexLitGeneric_DX9` (`vertexlitgeneric_dx9_helper.cpp:2346`) opens by handing the
material to `DrawPhong_DX9` when `WantsPhongShader` says so — `$phong 1` plus any of a
`$bumpmap`, a `$lightwarptexture` or `$basemapalphaphongmask 1`. Measured against the real
game that is **317 of the 1,108**, and `Phong` is a separate §7.8 entry that is not
ported: they draw here, without their specular, and say so once on stderr at load. See
[`wants_phong`](#wants_phong).

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
pub fn lighting(kind: ShaderKind, vmt: &Vmt) -> Lighting;
pub fn unlit_uniforms(vmt: &Vmt) -> UnlitUniforms;
pub fn lightmapped_uniforms(vmt: &Vmt) -> LightmappedUniforms;
pub fn vertex_lit_uniforms(vmt: &Vmt) -> VertexLitUniforms;
```

<a id="wants_phong"></a>

```rust
pub fn wants_phong(vmt: &Vmt) -> bool;        // is this really a Phong material?
pub fn envmap_name(vmt: &Vmt) -> Option<&str>;
```

`param_value` is `InitShaderParameters` (`shadersystem.cpp:838`): the var if the file set
one, else the type's default, with `$color` and `$alpha` special-cased to white and 1 as
they are there. **Read parameters through it**, not through `Vmt::var`, or a material that
does not mention `$alpha` will read as fully transparent.

`render_state` needs the resolved base texture because it cannot decide blending without
it — `TextureIsTranslucent` asks whether the `.vtf` has an alpha channel *and* whether
some other flag has already claimed it (`$selfillum`, `$basealphaenvmapmask`,
`$opaquetexture`).

<a id="lighting"></a>

```rust
pub enum Lighting { None, Lightmap, BumpedLightmap }
impl Lighting {
    pub fn blocks(self) -> u32;          // 1, or BUMP_BLOCKS for the bumped case
    pub fn needs_lightmap(self) -> bool;
}
```

`lighting` is `MATERIAL_VAR2_LIGHTING_LIGHTMAP` and `..._BUMPED_LIGHTMAP` as one answer,
because nothing asks them separately. It reaches the engine as
[`Material::lighting`](#material), and **it is a material-system property with an
engine-side consequence**: `BumpedLightmap` means every surface wearing this material
reserves *four* lightmap blocks instead of one, so a `$bumpmap` in a `.vmt` changes how
the `.bsp`'s lighting lump is read. The rule is
`InitParamsLightmappedGeneric_DX9`'s: bumped iff `$bumpmap` is defined and
`$nodiffusebumplighting` is 0.

**`ShaderParam::declared_default` is documentation, not behaviour** — worth knowing
because `portdocs/MATERIALSYSTEM.md` §7.2 reads as though it were live. In the whole
original tree `m_pDefaultValue` is read by one file, `tools/vmt/vmtdoc.cpp`, the material
editor. At runtime the default comes from the type or from the shader's own
`SHADER_INIT_PARAMS` block.

<a id="two-defaults"></a>

**There are two default mechanisms and `param_value` is only the second one.** They run
in order, and the difference is a silent zero:

1. `SHADER_INIT_PARAMS` — the shader's own `InitParams*` function, which *writes* a real
   value into the var array for a parameter the `.vmt` left out. `$detailscale` becomes 4,
   `$envmapsaturation` becomes 1, `$selfillummaskscale` becomes 1.
2. `CShaderSystem::InitShaderParameters` (`shadersystem.cpp:865`), which fills anything
   still undefined from its declared *type* — 0 for a float, black for a colour.

`param_value` is (2). Calling it and appending `.unwrap_or( 4.0 )` looks right and is dead
code: the type default arrives first, so the fallback never fires and `$detailscale`
silently becomes 0 — which collapses every detail texture to a single texel. Anything with
a non-type default goes through `init_float`/`init_vec` instead, which read `Vmt::var`
directly. This cost a debugging session and is pinned by
`shader_supplied_defaults_beat_type_defaults`.

<a id="envmap_name"></a>

**`$envmap "env_cubemap"` is not a texture name.** `CShaderSystem::LoadCubeMap`
(`shadersystem.cpp:1840`) special-cases the literal string: it sets the var to
`(ITexture *)-1`, sets `MATERIAL_VAR2_USES_ENV_CUBEMAP`, and loads nothing. The cubemap
then arrives *per draw* from the render instance — `instance.m_pEnvCubemap`, falling back
to `m_StdTextureHandles[TEXTURE_LOCAL_ENV_CUBEMAP]` (`shaderapidx8.cpp:8370`) — because
which cubemap a model reflects depends on where the model is standing, not on its
material. 78 of Portal 2's non-phong `VertexLitGeneric` materials say it. `envmap_name`
answers `None` for it, which binds the black cube and turns the envmap branch off; when
the `.bsp`'s embedded cubemaps become readable this becomes render-context state alongside
the lightmap page, and *that* is the trigger to revisit it.

The other half of the same rule: **a cubemap name gains `.hdr` before it gains `.vtf`.**
`LoadCubeMap` appends the suffix whenever HDR is on and `CTexture` falls back to the
unsuffixed name when the suffixed file is missing (`ctexture.cpp:3882`), so
`$envmap "metal/foo"` means `materials/metal/foo.hdr.vtf` **or** `materials/metal/foo.vtf`,
in that order. `TextureCache::load_cubemap` is that, and it is the reason
`texture::normalize_name` strips every extension *except* `.hdr`.

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
| 3 | *where this shader's lighting comes from* — see below | once a batch, or once a model | PS `s1` (`TEXTURE_LIGHTMAP`); VS `c21..c26` + `c27..c46` |

**Group 3 is the shader's lighting**, not skinning as stage 4 reserved it for, and it has
two shapes:

```rust
pub enum LightingBinding { LightmapPage, ModelLighting }
```

| Shader | Group 3 | Set by | Rate |
|---|---|---|---|
| `UnlitGeneric` | nothing — no group 3 is declared | — | — |
| `LightmappedGeneric` | a lightmap atlas page: texture + sampler | `Pass::bind_lightmap_page` | per batch |
| `VertexLitGeneric` | `ModelLighting`: ambient cube + 4 lights, dynamic offset | `Pass::set_model_lighting` | per model instance |

Both are things Valve also kept out of the material: `BindLightmapPage` and
`PI_SetVertexShaderAmbientLightCube` are render-context state that neither the material nor
the draw call owns. A pipeline layout is per shader, so declaring group 3 everywhere would
oblige every draw of every shader to bind something there; a shader that reads neither
declares no group 3 at all. Skinning takes the next free group when `studiorender` lands.

Group 1's *layout* is the shader's, which is the one thing that genuinely differs between
shaders; groups 0 and 2 are shared, which is what makes them worth being groups.

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

pub const MAX_LIGHTS: usize = 4;            // MATERIAL_MAX_LIGHT_COUNT
pub const AMBIENT_CUBE_FACES: usize = 6;

pub struct Light {                          // LightInfo, VS c27..c46
    pub color: [f32; 4],                    // rgb; w = 1 for a directional light
    pub direction: [f32; 4],                // xyz; w = 1 for a spot light
    pub position: [f32; 4],
    pub spot: [f32; 4],                     // falloff, thetaDot, phiDot, 1/(theta-phi)
    pub attenuation: [f32; 4],              // constant, linear, quadratic
}
impl Light {
    pub const NONE: Light;                  // s_pTwoEmptyLights: dark, but finite
    pub fn point(color: [f32; 3], position: [f32; 3], attenuation: [f32; 3]) -> Light;
    pub fn spot(color: [f32; 3], position: [f32; 3], direction: [f32; 3],
                attenuation: [f32; 3], falloff: f32, theta_dot: f32, phi_dot: f32) -> Light;
    pub fn directional(color: [f32; 3], direction: [f32; 3]) -> Light;
}

pub struct ModelLighting {                  // group 3, for VertexLitGeneric
    pub ambient_cube: [[f32; 4]; AMBIENT_CUBE_FACES],  // +x -x +y -y +z -z, linear
    pub lights: [Light; MAX_LIGHTS],
    pub count: u32,                         // how many of `lights` are real
    pub static_light: u32,                  // is the vertex stream's baked light meaningful
    pub ambient_light: u32,                 // is the ambient cube meaningful
    pub _padding: u32,
}
impl ModelLighting { pub fn fullbright() -> ModelLighting; }
```

`ModelLighting` is `MaterialLightingState_t` as it reaches the GPU — what
`R_StudioSetupLighting` computes once per model instance. Three switches rather than two,
because Valve has three: `m_bStaticLight`, `m_bAmbientLight` and `m_nLocalLightCount`, and
`HasDynamicLight()` is the second or the third (`ishaderdynamic.h:43`). Slots past `count`
must hold `Light::NONE` rather than zeroes — the shader evaluates every slot, and a zeroed
attenuation is a division by zero rather than a dark light.

**Its tail padding is written out by hand**, and that is not tidiness: Rust's alignment for
a `[[f32; 4]; 6]` is 4 while WGSL rounds the struct up to 16, so the two languages disagree
about the size unless it is spelled. `uniform_blocks_are_the_size_wgsl_expects` is the
guard.

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

// BindLayouts
pub fn frame(&self) -> &wgpu::BindGroupLayout;                    // group 0
pub fn material(&self, shader: ShaderKind) -> &wgpu::BindGroupLayout;  // group 1
pub fn draw(&self) -> &wgpu::BindGroupLayout;                     // group 2
pub fn lightmap(&self) -> &wgpu::BindGroupLayout;                 // group 3, brushes
pub fn model_lighting(&self) -> &wgpu::BindGroupLayout;           // group 3, models
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
put it — the shader declares the layout it reads, in its shadow phase.

The key grows a layout field the day one shader genuinely has two layouts, and **that day
has now come close twice and been declined twice**, for different reasons each time.
`LightmappedGeneric`'s bumped variant does not need one: the bumped diffuse path dots a
tangent-space normal against a constant basis and never leaves tangent space, so bumped
and unbumped read the same vertices. `VertexLitGeneric`'s bumped variant *is* a second
layout in Valve's engine — the tangent is `userDataSize = 4` only when the material is
bumped — and this port declines it because the tangent is in the `.vvd` either way; the
reasoning and the trigger to revisit are on `ModelVertex`. The envmap variant of
`LightmappedGeneric` is still the one most likely to force it.

**How many pipelines actually survive the combo cull**, which `portdocs/MATERIALSYSTEM.md`
§10 asks: loading all 1,108 of Portal 2's `VertexLitGeneric` materials and asking the cache
for one pipeline each produces **15**. Single digits per shader was the prediction; a
low double digit against the whole game's material set is the measurement.

---

## Meshes

`src/materials/mesh.rs`. Replaces `public/materialsystem/imesh.h` (4,402 lines, ~3,900 of
them the inlined `CMeshBuilder`). None of that is ported: a vertex is a `#[repr(C)]`
struct, its GPU layout is derived from the struct, and filling a buffer is
`bytemuck::cast_slice`.

### Vertices and layouts

```rust
pub enum VertexLayout { Simple, World, Model }
impl VertexLayout {
    pub fn buffer_layout(self) -> wgpu::VertexBufferLayout<'static>;
    pub fn stride(self) -> u64;
}

pub trait Vertex: Pod { const LAYOUT: VertexLayout; }

#[repr(C)]
pub struct SimpleVertex { pub position: [f32; 3], pub texcoord: [f32; 2], pub color: [f32; 4] }
impl SimpleVertex { pub const fn new(position: [f32; 3], texcoord: [f32; 2]) -> SimpleVertex; }

#[repr(C)]
pub struct WorldVertex {
    pub position: [f32; 3],
    pub texcoord: [f32; 2],           // TEXCOORD0, base texture
    pub lightmap_texcoord: [f32; 2],  // TEXCOORD1, into the atlas page
    pub lightmap_offset: f32,         // TEXCOORD2.x, one block as a fraction of the page
    pub color: [f32; 4],
}
impl WorldVertex { pub const fn new(position: [f32; 3], texcoord: [f32; 2]) -> WorldVertex; }

#[repr(C)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub texcoord: [f32; 2],
    pub tangent: [f32; 4],            // xyz tangent S, w the binormal's sign
    pub color: [f32; 4],              // COLOR1, vrad's baked per-vertex light
}
impl ModelVertex {
    pub const fn new(position: [f32; 3], normal: [f32; 3], texcoord: [f32; 2]) -> ModelVertex;
}
```

`WorldVertex` is what `BuildMSurfaceVertexArrays` writes for a brush surface, minus the
attributes nothing in scope reads. `new` gives white, unlit, at the origin — a base to
override fields of, so adding an attribute does not have to be echoed at every literal.

**`lightmap_offset` is one float where Valve's is a `float2`**, because the second
component is unconditionally zero in both branches that write it
(`matsys_interface.cpp:1498`). We own both ends of that one, which is `PORTING.md`'s test
for a format that is ours to change.

**Bumped and unbumped `LightmappedGeneric` read the same layout**, which corrects what
`portdocs/MATERIALSYSTEM.md` §10 predicted. The bumped diffuse path dots a *tangent-space*
normal straight out of `$bumpmap` against a constant basis
(`lightmappedgeneric_ps2_3_x.h:665`) and never needs a world-space frame, so the shadow
phase adds `VERTEX_TANGENT_S | VERTEX_TANGENT_T | VERTEX_NORMAL` only for an `$envmap`
(`lightmappedgeneric_dx9_helper.cpp:670`). Bumped lighting is therefore a flag in a
uniform — §7.3's bucket 2 — and the envmap variant is what will force the second layout.

`ModelVertex` is `mstudiovertex_t` (`public/studio.h:1447`) plus the `.vvd`'s parallel
tangent array — the two arrays Valve stores a model vertex in. Three things about it are
worth knowing before writing one:

- **The tangent is always present, and Valve's was not.** `VertexLitGeneric`'s shadow
  phase asks for `userDataSize = 4` only when the material has a `$bumpmap`
  (`vertexlitgeneric_dx9_helper.cpp:824`), so bumped and unbumped are two vertex formats
  there. This port keeps one, because the data is in the `.vvd` either way, because only
  71 of the game's 801 non-phong `VertexLitGeneric` materials are bumped, and because a
  layout that depends on a `.vmt` rather than on a `ShaderKind` costs `PipelineKey` a
  field. The condition to revisit is on the type.
- **`color` is baked static light, never `$vertexcolor`.** The shadow phase picks
  `VERTEX_COLOR` *or* `VERTEX_COLOR_STREAM_1` and never both, and for this shader the
  choice is already made: `bHasVertexColor` is unconditionally false when
  `bVertexLitGeneric` (`:594`). `$vertexcolor` belongs to `UnlitGeneric`, which reads
  `SimpleVertex`.
- **It is gamma space and pre-multiplied by a half.** The shader's first act is
  `GammaToLinear( color * 2 )`, so a baked 0.5 is a linear 1.0 and *white is not the
  neutral value* — an unlit-by-`vrad` vertex is black. `ModelVertex::new` gives black for
  exactly that reason.

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
| `VertexLitGeneric` | position, normal, uv, tangent, baked static light — `Model` |

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
pub fn bind_lightmap_page(&mut self, page: &LightmapPage);
pub fn set_model_lighting(&mut self, lighting: &ModelLighting);
pub fn target_format(&self) -> TargetFormat;
```

`bind_lightmap_page` is `IMatRenderContext::BindLightmapPage( lightmapPageID )` and
`set_model_lighting` is `R_StudioSetupLighting` plus the two per-instance commands it
feeds. Both are the two halves of group 3, both are pass state like `set_state_override`,
both apply from the call to the end of the pass, and a draw of a shader that does not read
that half ignores it.

Neither has to be called. A lit brush draw with no page bound gets the 1x1 white page,
which is what `AllocateWhiteLightmap` hands unlit surfaces anyway; a model draw with no
lighting set gets `ModelLighting::fullbright` — a white ambient cube and no lights — which
every pass allocates for itself when it opens. Both are visible-and-wrong rather than a
validation error or a read of whatever the previous instance left behind.

**`set_model_lighting` takes an arena slot per call**, so setting it, drawing, setting it
again and drawing again within one pass is the intended shape — which is what a scene of
props is. It is per *instance* rather than per draw because `R_StudioSetupLighting` runs
once for a model and every mesh of that model is then drawn under it; binding it per draw
would re-upload the same 432 bytes once per material the model wears.

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

## Lightmaps

`src/materials/lightmap.rs`. Replaces `materialsystem/cmatlightmaps.cpp` (2,465 lines),
`materialsystem/imagepacker.cpp` (169) and the lightmap half of
`materialsystem/colorspace.h`.

A `.bsp` stores each surface's baked light as a small rectangle of samples. The atlas
packs those rectangles into a handful of GPU textures, and a surface's *page* plus its
*offset within that page* become its lightmap texture coordinates. That is why the packer
is ported faithfully rather than replaced: a different packer is not wrong, but it is a
different atlas, a different page count and a different set of draw batches.

`src/engine/world/` is the only caller; see [`rustdocs/ENGINE.md`](ENGINE.md).

### The page format, and the one number that matters

A page texel is **linear radiance in `Rgba16Float`, sampled with no sRGB decode**.

Valve picked the format from `GetHDRType()` (`cmatlightmaps.cpp:481`): `RGBA8888` plus an
sRGB read for LDR, `RGBA16161616` for HDR-integer, `RGBA16161616F` for HDR-float.
**Portal 2 ships HDR-only maps** — `sp_a1_intro1.bsp` has an empty `LUMP_LIGHTING` and
5.4 MB of `LUMP_LIGHTING_HDR` — so the LDR path is not available, and of the two HDR ones
the float path is the one whose page holds just the numbers: `GetLightMapScaleFactor` is
1.0 for it (`hardwareconfig.cpp:832`) against 16.0 for the integer path, so nothing is
pre-divided and nothing has to be multiplied back.

`Rgba16Float` is filterable at the portable capability floor, which `Rgba16Unorm`
(`Features::TEXTURE_FORMAT_16BIT_NORM`) and `Rgba32Float` (`Features::FLOAT32_FILTERABLE`)
are not, so this costs nothing from the single capability tier. `f32` is converted to
binary16 in `lightmap.rs` rather than by a crate; it is twenty lines and `half` would be a
seventh dependency.

### `ColorRgbExp32` — and the decode that is 255x wrong

```rust
#[repr(C)]
pub struct ColorRgbExp32 { pub r: u8, pub g: u8, pub b: u8, pub exponent: i8 }
impl ColorRgbExp32 { pub fn to_linear(self) -> [f32; 3]; }
```

`to_linear` is `c * 2^e / 255` — `TexLightToLinear` (`public/mathlib/mathlib.h:1453`)
against a table documented as `2^(index - 128) / 255`.

**It is not `ColorRGBExp32ToVector`**, and this is the trap the whole section exists for.
That function is the obvious-looking "decode this colour", it sits immediately below the
table, and it is the same thing *times 255* — with Valve's own comment on the extra factor
reading *"FIXME: Why is there a factor of 255 built into this?"*. It is for
`dworldlight_t` intensities, ambient cubes and particle colours; the lightmap path calls
`TexLightToLinear` directly (`gl_lightmap.cpp:572`). Using the wrong one makes every lit
surface 255 times too bright, which is a **uniformly white screen** — not something that
reads as a decoding error from either end, since the samples are plausible and the shader
is correct. It cost a screenshot to find. `a_full_mantissa_at_exponent_zero_is_one` pins
it.

The decoded range is the `[0..16]` that `LightmapBitsToPixelWriter_HDRI`'s comment names.
Measured over `sp_a1_intro1`: 96.7% of luxels are below 0.25 and the maximum is 1.32.

### `ImagePacker`

```rust
pub fn new(width: u32, height: u32) -> ImagePacker;
pub fn add_block(&mut self, width: u32, height: u32) -> Option<(u32, u32)>;
pub fn efficiency(&self) -> f32;
```

`CImagePacker`, a skyline packer: `wavefront[x]` is the highest occupied row in column
`x`, and a block goes at the leftmost position whose maximum wavefront over its width is
lowest. Two details are Valve's and are reproduced rather than tidied, because both move
blocks between pages and therefore move texture coordinates:

- **`GetMaxYIndex` prefers the *last* column of a tied run** (`>=`, not `>`). It is what
  lets the search jump past a whole run of equal columns instead of retrying each.
- **The height test is `y + height >= page_height - 1`**, so a page is full one row
  early. A port using `>` would place blocks the original spilled onto the next page.

### `LightmapAtlas` and `LightmapPages`

```rust
pub const PAGE_WIDTH: u32 = 512;
pub const PAGE_HEIGHT: u32 = 256;
pub const BUMP_BLOCKS: u32 = 4;
pub const WHITE_PAGE: u32 = 0;

pub struct Allocation { pub page: u32, pub x: u32, pub y: u32 }

impl LightmapAtlas {
    pub fn new() -> LightmapAtlas;
    pub fn begin_material(&mut self);
    pub fn allocate(&mut self, width: u32, height: u32) -> Option<Allocation>;
    pub fn write(&mut self, allocation: Allocation, width: u32, height: u32,
                 blocks: u32, samples: &[ColorRgbExp32]);
    pub fn page_size(&self, page: u32) -> (u32, u32);
    pub fn page_count(&self) -> u32;
    pub fn upload(self, device: &wgpu::Device, queue: &wgpu::Queue,
                  layouts: &BindLayouts) -> LightmapPages;
}

impl LightmapPages {
    pub fn page(&self, page: u32) -> &LightmapPage;   // out of range -> the white page
    pub fn len(&self) -> usize;
    pub fn bytes(&self) -> usize;
}
```

The CPU half and the GPU half are separate on purpose: allocating and writing are pure and
testable without a device, and `upload` is the single point that touches `wgpu`. A page is
512x256 because `GetMaxLightmapPageWidth`'s comment says so — "512x256 textures because
that's the only way bumped lighting on displacements can work given the 128x128
allowance".

**`begin_material` is not optional and is not a hint.** `AllocateLightmap`
(`cmatlightmaps.cpp:306`) closes every open page but the most recent whenever the material
changes, so that one material's surfaces cluster onto as few pages as possible. Skipping
it produces a correct atlas with many more batches; calling it mid-material produces the
opposite. Call it once per material run, before that material's first `allocate`.

**Page 0 is always the 1x1 white page.** `MATERIAL_SYSTEM_LIGHTMAP_PAGE_WHITE`, which
`AllocateWhiteLightmap` hands to unlit surfaces. Valve made it a negative page ID;
here it is a real page so that nothing downstream has to special-case it.

### A material and a face can disagree about bumpedness

Two independent things decide how many blocks are involved, and they can differ:

| Question | Answered by | Why that one |
|---|---|---|
| How wide a block to **reserve** | the material's [`Lighting`](#lighting) | Valve's rule (`RegisterLightmappedSurface`). Keeping it is what keeps every surface of one material sampling the same way |
| How many blocks the file **holds** | `SURF_BUMPLIGHT` in the face's `texinfo` | it is the file describing its own layout |

Valve derives the second from the first as well, so a `.vmt` edited after the map was
compiled makes its engine read the lighting lump at the wrong stride. Reading the flag is
both simpler and right by construction — checked against `sp_a1_intro1`, where the flag
agrees with the byte spacing between consecutive light offsets on all 4,982 lit faces,
with zero disagreements. `write` reconciles the two cases: material wants four and the
file has one, the flat map is copied into all four blocks; material wants one and the file
has four, only the flat map is written.

### The bumped correction

`write` applies `ColorSpace::LinearToBumpedLightmap`'s float overload: the three
directional maps are scaled so their average matches the flat one, so that a surface with
a normal map is as bright as the same surface without one. Two comments in the original
are worth carrying and are reproduced at the site — one saying the maths is *"completely
wrong"* according to Alex (it is reproduced anyway, because the shipped lightmaps were
baked against it), and one noting the correction was *"entirely missing in the float path
as of September '11"*.

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
2. **A lightmap sample decodes with `TexLightToLinear`, not `ColorRGBExp32ToVector`.**
   The two differ by a factor of 255 and the wrong one is the one that looks right in the
   source. See [`ColorRgbExp32`](#colorrgbexp32--and-the-decode-that-is-255x-wrong); the
   symptom is a uniformly white screen rather than anything that reads as a bad decode.
3. **`ColorSpace` is decided by the shader, and nothing checks it.** Getting it wrong
   produces a picture that looks *plausible* — a washed-out albedo, or a normal map that
   lights slightly wrong — rather than anything that errors.
   [`shader::texture_requests`](#texture_requests) is where the answer lives; a call site
   that decides for itself is a bug waiting to diverge from the shader that samples it.
4. **Read parameters through `shader::param_value`, not `Vmt::var`.** A `.vmt` that does
   not mention `$alpha` has no `$alpha` var, and treating that as zero makes every such
   material invisible. `param_value` is where `InitShaderParameters`' defaults live.
5. **Per-draw constants must go in distinct arena slots, and this is not obvious.**
   `Queue::write_buffer` stages its copy to run before the *whole* command buffer, not at
   the point in the recording where it was called — so one uniform buffer rewritten
   between draws gives every draw in the frame the last values written. `RenderContext`
   handles it; anything that adds a new per-draw block must too. See
   [Uniforms are arenas](#uniforms-are-arenas-not-single-buffers).
6. **`RenderContext::begin_frame` must run before anything allocates, once a frame.** It
   resets the uniform and geometry arenas. A slice held over from the previous frame
   reads whatever overwrites it — silently, and only under load, because the arena has to
   wrap round to the same offset first.
7. **A `Frame` borrows the renderer.** `resize` and a second `begin_frame` have to happen
   outside that borrow: `Surface::configure` panics if a frame is alive, so the borrow
   checker is enforcing a real `wgpu` rule. Drawing is unaffected — `RenderContext` holds
   its own device handles.
8. **`wgpu` render passes do not nest**, so a render target is filled by a pass that has
   *ended* before the pass that samples it begins. See
   [Passes replace three stacks](#passes-replace-three-stacks).
9. **`Pass::draw` panics on a vertex-layout mismatch.** The layout comes from the
   material's shader (`ShaderKind::vertex_layout`), not from the buffer, and drawing
   model data through a world shader would otherwise reinterpret bone weights as
   coordinates and draw something merely wrong.
10. **A zero-size window is legal and must not reach `Surface::configure`, which panics on
   it.** Minimizing a window reports width or height 0. `resize(w, 0)` marks the surface
   unconfigured and `begin_frame` then returns `None` until real dimensions arrive. If you
   add another path that configures the surface, replicate that guard.
11. **`pre_present_notify` is the caller's job.** The renderer does not own a `winit`
   window, so it cannot make the call itself. It must happen immediately before
   `Frame::present`; skipping it costs compositor scheduling accuracy, not correctness.
12. **Sizes are physical pixels.** See `Renderer::new` above.
13. **The surface format is sRGB when the platform offers one** (`Bgra8UnormSrgb` on
   macOS/Metal). That is the replacement for `IShaderDevice::SetHardwareGammaRamp`: the
   hardware encodes on write instead of the engine warping the display's gamma ramp
   process-wide — and leaving it warped if it crashed. **Consequence:** values written by
   a shader are treated as *linear* and encoded on the way out. Do not apply an sRGB curve
   in shader code as well.
14. **Copies into a compressed texture use the level's *physical* size, not its logical
    one.** The tail of a DXT mip chain is levels smaller than a 4x4 block (a 64x64 DXT1
    texture ends 2x2, 1x1), and WebGPU requires a copy to be a whole number of blocks —
    writing the logical 2x2 is a validation error, not a silent truncation.
    `ImageFormat::mem_required` rounds the same way, because `GetMemRequired` did, so the
    byte counts agree. This bit once; `Texture::from_vtf` handles it.
15. **`Features::TEXTURE_COMPRESSION_BC` is required, not requested.** Essentially every
    texture Valve ships is DXT, and there is no fallback tier — decompressing on the CPU
    would quadruple both load time and video memory for the whole game. An adapter without
    it fails at startup with `RendererError::NoBlockCompression` rather than half-working.
16. **`required_limits` is `wgpu::Limits::default()` — the portable floor, not the
    adapter's ceiling.** Deliberate: §4.6 replaces `IMaterialSystemHardwareConfig`'s ~50
    caps queries and the `dxlevel` ladder with one fixed capability tier, and asking every
    machine for the same limits is what makes that tier mean anything. Raise it
    deliberately when a shader needs more; never adapter-by-adapter.
17. **Colour space of the swap chain is `Auto`, i.e. SDR.** Portal 2 ships HDR-lit maps,
    and HDR is still an open question (`portdocs/MATERIALSYSTEM.md` §10). Switching it on
    means a float format and a tonemap pass, not just changing this field.
18. **Backends are `METAL | VULKAN | GL`.** DX12 and BrowserWebGPU are omitted rather than
    merely unreachable, per `PORTING.md`'s POSIX-only rule. `WGPU_BACKEND` still overrides
    at runtime (as do `WGPU_ADAPTER_NAME` and `WGPU_DEBUG`) — those are left enabled on
    purpose as the modern equivalent of the old `-gl`/`-dx9` switches.
19. **`Renderer::new` blocks** on `pollster::block_on` for the adapter and device requests.
    Fine at startup, on the main thread, once. Do not call it from a frame.
20. **`LightmapAtlas::begin_material` must bracket each material's run of allocations.**
    Forgetting it costs draw batches, not correctness; calling it inside a run costs more
    of them. See [`LightmapAtlas`](#lightmapatlas-and-lightmappages).
21. **The lightmap page is not a material property.** One material's surfaces are spread
    over as many atlas pages as the packer needed, so the page is bound per *batch* with
    `Pass::bind_lightmap_page` — that is what Valve's sort ID encodes. Putting it in the
    material's bind group would mean one `Material` per page.
22. **HDR lightmaps reach the shader unexposed.** `cLightScale.x` is 1.0 because there is
    no tone mapper, so a map is as bright as `vrad` left it — dimmer than the shipped
    game, which auto-exposes. It is one uniform field, not a redesign; see the divergence
    table.
23. **A parameter with a non-type default must be read with `init_float`/`init_vec`, not
    `param_value`.** There are two default mechanisms and `param_value` is the second one,
    so `param_value(..., "$detailscale").unwrap_or( 4.0 )` compiles, reads correctly and
    silently yields 0. See [the two defaults](#two-defaults) — this cost a debugging
    session on the shader it was introduced with.
24. **A model's baked vertex light is gamma space times a half, and white is not
    neutral.** `GammaToLinear( color * cOverbright )` with `cOverbright` 2
    (`common_vs_fxc.h:852`): a baked 0.5 is a linear 1.0, and an unlit vertex is *black*.
    Filling `ModelVertex::color` with white is twice the brightest value `vrad` can bake.
    `ModelLighting::static_light` is the switch that says whether the stream means
    anything at all.
25. **The ambient cube is ordered `+x, -x, +y, -y, +z, -z`.** It comes from three
    `float3[2]` register pairs (`cAmbientCubeX` at VS `c21`, `Y` at `c23`, `Z` at `c25`),
    so positive is always the even slot. A swapped pair lights every model in the level
    from the wrong side and looks entirely plausible;
    `the_ambient_cube_lights_each_axis_from_its_own_entry` pins all six.
26. **A local light's *type* lives in the `w` of two of its vectors**, not in an enum:
    `color.w` is 1 for a directional light and `direction.w` is 1 for a spot
    (`common_vs_fxc.h:119`). The shader selects with two `lerp`s, which is what a shader
    model with no branches had instead of an `if`. Build lights with `Light::point`,
    `Light::spot` and `Light::directional` rather than filling the fields, and fill unused
    slots with `Light::NONE` — a *zeroed* slot divides by zero in the attenuation
    denominator, which is why `s_pTwoEmptyLights` has a constant attenuation of 1.
27. **A bumped model gets no baked vertex light at all, and an unbumped one does.** That
    asymmetry is Valve's: `vertexlit_and_unlit_generic_bump_ps2x.fxc:452` calls
    `PixelShaderDoLighting` with `bStaticLight = false`, because the per-vertex stream
    cannot be re-evaluated against a per-pixel normal. The same model with and without a
    `$bumpmap` is therefore lit by different things, not by the same thing more precisely.
28. **Unbumped `VertexLitGeneric` is Gouraud-shaded** — `DoLighting` runs in the *vertex*
    shader (`vertexlit_and_unlit_generic_vs20.fxc:437`) and the fragment shader reads the
    interpolated result. Lighting it per pixel instead is prettier and wrong: content was
    authored against the flatter shading, and a lighting number measured at the middle of
    a surface will not match what the middle pixel shows.

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
| Lightmap pages are `Rgba16Float` and never LDR | Portal 2's maps have no `LUMP_LIGHTING` at all, so the LDR encoding (`LinearToLightmap`'s gamma-and-overbright table plus an sRGB page) has nothing to encode. A map with only LDR lighting loads and draws dimmer by the overbright factor that encoding divides out | `lightmap.rs`'s page format, plus an `sRGB` page and `GetLightMapScaleFactor`'s `GammaToLinearFullRange(2.0)` |
| The last atlas page is not shrunk | `GetMinimumDimensions` (`imagepacker.cpp:156`) trims the final page to a power-of-two height, which saves at most one page and makes every surface's coordinates depend on how full its page ended up. The aspect clamp it applies also reads `MaxTextureAspectRatio()`, a caps query that no longer exists | `LightmapAtlas::page_size`, which is already per page |
| Bumpedness is read from `SURF_BUMPLIGHT`, not re-derived from the material | the flag is the file describing its own layout; Valve's engine re-derives it and reads the lump at the wrong stride if a `.vmt` changed after compilation. The two are reconciled rather than assumed equal | `LightmapAtlas::write` |
| Only lightstyle 0 is baked into the atlas | the other three are switchable and animated lights, and summing them needs `LightStyleValue( style )` and a per-frame page rebuild (`R_BuildLightMap`) — the whole dynamic lighting path. `WorldStats::faces_with_lightstyles` counts the surfaces this understates | — |
| A map with interleaved lightmap alpha is refused | `LVLFLAGS_LIGHTMAP_ALPHA` puts a CS:GO-era cascaded-shadow term between every face's samples, so the stride changes for the whole lump. Portal 2 does not set it; misreading it would draw noise | `BspError::UnsupportedLightmapAlpha` |
| No tone mapping: `cLightScale` is all ones | HDR lightmaps arrive in `[0..16]` and `SetToneMappingScaleLinear` normally carries the exposure the tone-map controller chose. There is no controller, so a map is as bright as `vrad` left it | `FrameUniforms::light_scale` |
| The dynamic geometry arena grows instead of overflowing | `CDynamicVB` was a fixed allocation and callers split batches to fit. The `*_remaining` queries are still there for callers that want to | `mesh::ARENA_BYTES` |
| **Half-lambert is read from `$halflambert` again** | the tree this port is derived from hard-codes `bHalfLambert = false` over a commented-out read of the flag, with the comment *"Disabling half-lambert for CSGO (not compatible with CSM's, causes bad shadow aliasing)"* (`vertexlitgeneric_dx9_helper.cpp:679`). Portal 2 has no cascaded shadow maps and neither does this port, so the commented-out line is the behaviour and the constant is the divergence | `shader::vertex_lit_uniforms` |
| **`SoftenCosineTerm` is not applied to the diffuse term** | `(d + d²)/2` (`common_fxc.h:112`), tagged `// For CS:GO` at both of its call sites (`common_vs_fxc.h:796`, `common_vertexlitgeneric_dx9.h:99`). It changes the falloff of every lit surface in the game and postdates Portal 2 | `cosine_term` in `vertexlitgeneric.wgsl` |
| `VertexLitGeneric` carries a tangent even when unbumped | Valve's `userDataSize` is 4 only for a bumped material, so bumped and unbumped are two vertex formats there. The `.vvd` stores the tangent either way, 71 of 801 materials are bumped, and one layout per shader is what keeps it out of `PipelineKey` | `mesh::ModelVertex`, and a layout field on the key |
| `$phong` materials draw without specular | `WantsPhongShader` sends them to `DrawPhong_DX9`, a separate §7.8 shader that is not ported. 317 of Portal 2's 1,108 `VertexLitGeneric` materials; each says so once on stderr at load | port `Phong` |
| `$envmap "env_cubemap"` reflects nothing | it names no file — it is a request for the render instance's local cubemap, which needs the `.bsp`'s pak lump mounted. 78 materials; they bind the 1x1 black cube, so the reflection term contributes zero rather than a checkerboard | `shader::envmap_name` |
| An **opaque** material writes its base texture's alpha to the frame, where Valve writes 1 | `g_EyePos_BaseTextureTranslucency.w` is `TextureIsTranslucent( BASETEXTURE, true )` — 1 for a `$translucent` or `$alphatest` material, 0 for an opaque one — and the shader lerps the base alpha in by it. Blending and the alpha-test `discard` are unaffected, because both cases have `w` of 1; only the frame's alpha channel differs, and the underwater fog pass that reads it is not ported. Shared with `UnlitGeneric`, which reaches the same Valve source file | thread the resolved base texture into `*_uniforms` and add a flag |
| `$lightwarptexture`, `$rimlight` and self-illum fresnel are ignored | each belongs to the `Phong` path or needs a texture kind not yet loaded; all three are declared-and-unread rather than silently accepted, since the params table omits what it does not honour | — |

## Not implemented

Stage 6 is `VertexLitGeneric` and is done; the rest of §7.8's shader set, paint maps and
GPU morph (stages 7-8) are not. The shader set is three shaders deep, so a `.vmt` naming
any of the other 160-odd still resolves to the error material — measured against Portal 2,
those three cover 2,836 of its 3,431 materials. Also deliberately absent, and listed so
nobody looks for them:

- **Everything `LightmappedGeneric` can do past a base texture, a bump map and a
  lightmap.** `$basetexture2`/`$bumpmap2` two-layer blending and `$blendmodulatetexture`,
  `$detail`, `$envmap`/`$envmapmask` (the one that needs tangent space, and therefore a
  second vertex layout), `$selfillum`, phong, seamless mapping, the flashlight, cascaded
  shadow maps, and Portal 2's paint layer. Each is declared in
  `lightmappedgeneric_dx9_helper.cpp` and each is left out of the parameter table rather
  than declared-and-ignored.
- **Dynamic lights and lightstyle animation.** `R_BuildLightMap` rebuilt a page every
  frame from `LightStyleValue( style )` and the visible `dlight_t`s; a page here is
  written once at load. `LockLightmap`/`UpdateLightmap` and the ring of dynamic pages go
  with it.
- **Everything `VertexLitGeneric` can do past the features listed on `ShaderKind`.**
  `$phong` and its whole family (a separate shader — see [`wants_phong`](#wants_phong)),
  `$lightwarptexture`, `$rimlight`, self-illum fresnel, wrinkle maps, tree sway,
  `$decaltexture`, `$tintmask`, `$displacementmap`, seamless mapping, distance alpha, and
  the emissive-scroll, cloak and flesh-interior blended passes — which are three whole
  extra shaders drawn over the top of this one, and are HL2/Alien Swarm content rather
  than Portal 2's.
- **Skinning and morphing.** `ModelVertex` has no bone weights or indices, and
  `SkinPositionAndNormal` is replaced by the per-draw model matrix — which is what an
  unskinned draw did anyway, through `cModel[0]`. This is what makes group 3's "skinning"
  reservation from stage 4 land somewhere else.
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
itself worth seeing.

**Every shader can be previewed, and each is lit by whatever `-vmt` can honestly supply.**
The cube is built in both `SimpleVertex` and `ModelVertex`, and the draw picks whichever
layout the `.vmt`'s shader declared. A `LightmappedGeneric` material has no lightmap page
here — there is no world geometry — so it binds the white page and comes out fullbright. A
`VertexLitGeneric` one is drawn under `preview_lighting()`: an ambient cube deliberately
*unequal per axis*, so that a swapped cube entry shows as a face with the wrong tint
rather than as nothing, plus one point light offset from the centre so the falloff is
visible across the ground quad. Neither is a real lighting environment, and neither
pretends to be.

Three things it pins down that are worth reading before writing the real one:

- **The cube is built rather than written out**, from a per-face `(normal, u, v)` triple
  chosen so that `u × v == n`. That is what makes each face wind counter-clockwise as
  seen from outside without anyone having to check twenty-four literals. Getting one face
  backwards does not draw it mirrored — it draws a hole, silently, invisible from most
  angles.
- **The model cube's binormal sign is -1**, and it is not arbitrary: the cube's texture
  `v` runs opposite its in-plane `v` axis, and Valve's shader builds the binormal as
  `cross( normal, tangent ) * tangent.w`. With `+1` a normal map previews lit from the
  wrong side along one axis only — the kind of half-wrong that survives a glance.
- **The ground quad is dynamic on purpose**, not because it changes. The dynamic vertex
  path is what every immediate-mode draw in the engine uses, and a path only the tests
  exercise is a path that rots.

**Do not grow this.** Map loading has landed, so the deletion this asked for is now
possible and is deliberately *not* part of stage 5: the twenty-eight GPU tests at the bottom
of `preview.rs` are the only place the whole path is checked against real pixels, and
moving them onto the world draw means giving them a `.bsp` — which means shipping content
into the test suite or making them skip without it. Delete `preview.rs` and `-vmt`
together with whatever answers that, not before.

<a id="uirenderer"></a>

## `UiRenderer` — the `egui` pass

```rust
pub struct UiRenderer;
impl UiRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, target: TargetFormat) -> UiRenderer;
    pub fn draw(&mut self, frame: &mut Frame<'_>, primitives: &[egui::ClippedPrimitive],
                textures: egui::TexturesDelta, pixels_per_point: f32);
}
```

**This is not part of the material system.** Nothing here goes through `PipelineCache`,
`Material` or the constant ABI: `egui_wgpu::Renderer` owns its own pipeline, its own font
atlas and its own vertex format, and wrapping any of that in Valve's shapes would buy
nothing. It is a separate renderer that happens to draw into the same frame — which is
exactly what `vgui2` was to `materialsystem`.

It lives here for one concrete reason: [`Frame::parts`](#framea) is `pub(super)`, because
opening a pass means deciding what its constants are and that decision belongs in one
place. The UI is the one caller that legitimately opens a pass this module did not build
the pipeline for, and it belongs on this side of that boundary rather than widening it.
`window/` owns the instance and calls it; see [`ENGINE.md`](ENGINE.md#the-egui-boundary).

Four things about it are worth knowing, in the order they will bite:

1. **It loads, it does not clear.** The UI is an overlay over a world that has already
   been drawn into the frame.
2. **No depth attachment.** `egui`'s pipeline is built without depth-stencil state, so a
   pass carrying a depth attachment fails validation against it. `UiRenderer` opens its
   own pass with `depth_stencil_attachment: None` rather than reusing the frame's.
3. **`TexturesDelta` is taken by value and emptied.** `epaint` asserts on drop that every
   delta in it was applied (`epaint/src/textures.rs:335`); borrowing it would leave the
   caller holding something that panics in a debug build the moment it goes out of scope.
   The same rule is why the UI is built *inside* the acquired frame — see
   [`ENGINE.md`](ENGINE.md#the-frame) ordering #7.
4. **The target format decides the fragment shader.** `egui` works in gamma space, and
   `egui_wgpu` picks between two entry points on `output_color_format.is_srgb()`. This
   port's surface is sRGB (`Renderer::new` prefers one — that is what replaced
   `SetHardwareGammaRamp`), so passing the surface's real format is both necessary and
   sufficient. Getting it wrong is not an error; it is a visibly washed-out UI.

Its two tests (`cargo test materials::ui`) draw a real `egui` pass into an offscreen
target on a real device and read the pixels back, at both an `Rgba8Unorm` and an
`Rgba8UnormSrgb` target. They are the **only** tests that can see a `wgpu` validation
error in the UI path — the dialog's own tests in `engine::console::ui` run against a
headless `egui::Context` and never touch a GPU. The split that makes both possible is
`UiRenderer::record`, which takes an encoder and a view instead of a `Frame`, because a
`Frame` needs a swap chain and a swap chain needs a window.

## Test coverage

143 tests, in two groups.

**Pure logic, no GPU** (124) — the parts where a mistake is invisible rather than loud:

| Tests | Guard |
|---|---|
| `lightmap` (17) | the `ColorRGBExp32` decode including the 255 factor, the packer's two Valve-specific edge rules, page allocation and the material-change rule, the block layout a bumped surface writes, the bump correction, and the `f32` -> binary16 encoding at the magnitudes lightmaps actually use |
| `vtf` (18) | every version 7.0-7.5, the seventh cubemap face, partial mip chains, the thumbnail, flag masking, and each way a file can be malformed |
| `vmt` (17) | the type sniffing, conditional keys, flags-are-not-vars, fallback blocks, and patch expansion against a real temp-directory `Vfs` |
| `image_format` (15) | the size arithmetic that decides where every mip level starts in a file, and every CPU format conversion, channel by channel |
| `shader` (24) | the shadow phase — every flag that maps onto pipeline state, the blend evaluation, the alpha-test reference, the texture transform, and the sRGB rule — plus `VertexLitGeneric`'s parameter resolution: `WantsPhongShader`'s truth table, `env_cubemap`, the shader-supplied defaults that `param_value` does not give, the three envmap masks resolving against each other, and the alpha-test suppression when base alpha is spoken for |
| `var` (12) | the value grammar and every coercion between the arms, plus the flag-name table against the bit constants |
| `texture` (7) | the `.vtf` flags -> sampler policy, and `NormalizeTextureName` — extensions stripped except `.hdr`, and only in the last path component |
| `uniforms` (10) | the uniform block sizes WGSL expects — including `ModelLighting`'s hand-written tail padding, where Rust's alignment for an array of `[f32; 4]` is 4 and WGSL's is 16 — the no-fog packing, the row-major/column-major conversion in both directions, and the light type's two-`w`-component encoding |
| `pipeline` (5) | `RenderState::default()` against `SetDefaultState` field by field, the blend factor pairs, and that the shader is what decides the vertex layout |
| `context` (5) | the projection conventions — depth in `0..1`, `z` into the screen, horizontal-to-vertical fov — and that a `StateOverride` touches only what it names |
| `mesh` (6) | the vertex layouts against the structs they describe — including every attribute offset, since `wgpu` derives those by accumulating format sizes and a reordered field shifts everything after it — and the copy-alignment padding at every remainder |
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

**End to end, on a real GPU** (28, in `preview.rs`) — a `.vmt` and a `.vtf`, through the
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
| `the_ambient_cube_lights_each_axis_from_its_own_entry` | all six ambient-cube entries, in order. A swapped pair lights every model in a level from the wrong side and looks plausible |
| `the_ambient_cube_is_ignored_when_it_is_disabled` | `m_bAmbientLight` — a model with no lighting environment lit by whatever the previous instance left behind |
| `baked_vertex_light_is_gamma_decoded_and_doubled` | `GammaToLinear( c * cOverbright )`: dropping the doubling halves every prop in the game, dropping the decode brightens the darks |
| `baked_vertex_light_is_ignored_when_it_is_disabled` | `g_flStaticLightEnabled`, the only thing that says whether the colour stream means anything |
| `a_directional_light_ignores_distance_and_a_point_light_does_not` | the two `lerp`s that encode the light type in `color.w` and `direction.w`, and the `1/(a0 + a1·d + a2·d²)` denominator |
| `half_lambert_lights_a_surface_that_lambert_leaves_black` | `$halflambert` restored from the flag against the CS:GO tree's hard-coded `false` |
| `self_illumination_emits_where_the_lighting_is_black` | `$selfillum`, and with it the whole shader-supplied-defaults path — `$selfillummaskscale` reading 0 makes this test's quad black |
| `a_bumped_model_takes_no_baked_vertex_light` | Valve's asymmetry between its two files: `bStaticLight = false` on the bumped path only |
| `model_lighting_is_per_instance_and_two_draws_can_differ` | the group-3 arena — one lighting buffer rewritten between draws would give every draw in the frame the last values written |

They earn the GPU: row pitch, block layout, channel order, winding, matrix convention,
depth direction and bind group layout are all invisible to a unit test, and each produces
a *plausible* wrong picture rather than a crash. `the_depth_buffer_decides_which_of_two_overlapping_quads_is_seen`
earned its place immediately: it caught `Camera::screen` passing `glam` its near and far
planes the wrong way round, which had inverted the depth comparison for every 2D draw.

**They skip, printing why, when no adapter with BC support is available**, so a machine
with no GPU still gets a green `cargo test`.
