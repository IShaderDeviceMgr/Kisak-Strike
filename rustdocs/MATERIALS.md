# `src/materials/` — API reference

The material system. Right now that means two things: the GPU device and the frame
boundary, and the texture path from a `.vtf` on disk to a sampler on the GPU. Materials,
shaders and meshes are not here yet.

Porting design doc: [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named
after the *original* module (`materialsystem/`), while this file is named after the Rust
one (`src/materials/`). Same subject, two names, on purpose.

| | |
|---|---|
| Module | `crate::materials` |
| Lines | ~3,700 including tests |
| Tests | 43 (`cargo test materials`) — 3 of them run on a real GPU |
| Dependencies | `wgpu` 30, `pollster`, `thiserror` |
| Status | **Stages 1-2 of 8.** GPU bring-up, and `.vtf` -> `wgpu::Texture`. Stages 3+ not started |

```
src/materials/
  renderer.rs      Renderer, Frame, RendererOptions — the device and the frame boundary
  vtf.rs           Vtf, TextureFlags — reading .vtf files
  image_format.rs  ImageFormat, ColorSpace — pixel formats, size maths, CPU conversions
  texture.rs       Texture, TextureCache, SamplerKey — textures on the GPU
  blit.rs          TextureBlit — the stage-2 verification draw. Temporary
  shaders/blit.wgsl
  error.rs         RendererError, VtfError, TextureError
```

**What is deliberately absent:** there is no `IShaderDevice`, no `IShaderAPI`, no
`IShaderShadow`, no device-abstraction trait, and no second backend. `wgpu` is called
directly. If you find yourself adding a trait so that "another renderer could be plugged
in later", stop — `wgpu` is already that abstraction, and re-adding the tower is the
specific mistake `portdocs/MATERIALSYSTEM.md` §5.1 exists to prevent.

## Quick start

```rust
use std::sync::Arc;
use crate::materials::{ColorSpace, Renderer, RendererOptions, TextureCache, CLEAR_COLOR};

// `window` is an `Arc<winit::window::Window>`; `display` is the event loop's
// `OwnedDisplayHandle`.
let size = window.inner_size();
let mut renderer = Renderer::new(
    window.clone(),                 // coerces to Arc<dyn RenderWindow>
    display,
    (size.width, size.height),
    &RendererOptions::default(),
)?;

// The texture dictionary. Independent of the renderer — it holds its own
// `Device`/`Queue` handles.
let mut textures = TextureCache::new(renderer.device(), renderer.queue());
let wall = textures.load(&vfs, "metal/metalwall048a", ColorSpace::Srgb);

// ... once per frame:
if let Some(mut frame) = renderer.begin_frame() {
    frame.clear(CLEAR_COLOR);
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
pub fn blit(&mut self, blit: &TextureBlit);   // stage 2 only; see below
pub fn present(self);   // #[must_use] on the struct
```

One acquired swap-chain image plus the `CommandEncoder` recording into it. `present`
consumes it: submit, then present. Dropping a `Frame` without presenting discards
everything recorded into it — correct for an abandoned frame, silent data loss if
accidental, which is why the type is `#[must_use]`.

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
```

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

pub fn from_vtf(
    device: &wgpu::Device, queue: &wgpu::Queue, name: &str,
    vtf: &Vtf, frame: u32, color_space: ColorSpace, sampler: wgpu::Sampler,
) -> Result<Texture, TextureError>;

pub fn error(device: &wgpu::Device, queue: &wgpu::Queue, sampler: wgpu::Sampler) -> Texture;
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

The rule to apply, until stage 3's materials encode it properly:

| Kind of texture | `ColorSpace` |
|---|---|
| `$basetexture`, `$detail`, `$envmap`, anything a human picked as a colour | `Srgb` |
| `$bumpmap`, `$ssbump`, masks, DUDV maps, lightwarps, HDR data | `Linear` |

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

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **`ColorSpace` is the caller's decision and nothing checks it.** Getting it wrong
   produces a picture that looks *plausible* — a washed-out albedo, or a normal map that
   lights slightly wrong — rather than anything that errors. See
   [above](#colorspace--the-one-decision-a-caller-must-make).
2. **A zero-size window is legal and must not reach `Surface::configure`, which panics on
   it.** Minimizing a window reports width or height 0. `resize(w, 0)` marks the surface
   unconfigured and `begin_frame` then returns `None` until real dimensions arrive. If you
   add another path that configures the surface, replicate that guard.
3. **`pre_present_notify` is the caller's job.** The renderer does not own a `winit`
   window, so it cannot make the call itself. It must happen immediately before
   `Frame::present`; skipping it costs compositor scheduling accuracy, not correctness.
4. **Sizes are physical pixels.** See `Renderer::new` above.
5. **The surface format is sRGB when the platform offers one** (`Bgra8UnormSrgb` on
   macOS/Metal). That is the replacement for `IShaderDevice::SetHardwareGammaRamp`: the
   hardware encodes on write instead of the engine warping the display's gamma ramp
   process-wide — and leaving it warped if it crashed. **Consequence:** values written by
   a shader are treated as *linear* and encoded on the way out. Do not apply an sRGB curve
   in shader code as well.
6. **Copies into a compressed texture use the level's *physical* size, not its logical
   one.** The tail of a DXT mip chain is levels smaller than a 4x4 block (a 64x64 DXT1
   texture ends 2x2, 1x1), and WebGPU requires a copy to be a whole number of blocks —
   writing the logical 2x2 is a validation error, not a silent truncation.
   `ImageFormat::mem_required` rounds the same way, because `GetMemRequired` did, so the
   byte counts agree. This bit once; `Texture::from_vtf` handles it.
7. **`Features::TEXTURE_COMPRESSION_BC` is required, not requested.** Essentially every
   texture Valve ships is DXT, and there is no fallback tier — decompressing on the CPU
   would quadruple both load time and video memory for the whole game. An adapter without
   it fails at startup with `RendererError::NoBlockCompression` rather than half-working.
8. **`required_limits` is `wgpu::Limits::default()` — the portable floor, not the
   adapter's ceiling.** Deliberate: §4.6 replaces `IMaterialSystemHardwareConfig`'s ~50
   caps queries and the `dxlevel` ladder with one fixed capability tier, and asking every
   machine for the same limits is what makes that tier mean anything. Raise it
   deliberately when a shader needs more; never adapter-by-adapter.
9. **Colour space of the swap chain is `Auto`, i.e. SDR.** Portal 2 ships HDR-lit maps,
   and HDR is still an open question (`portdocs/MATERIALSYSTEM.md` §10). Switching it on
   means a float format and a tonemap pass, not just changing this field.
10. **Backends are `METAL | VULKAN | GL`.** DX12 and BrowserWebGPU are omitted rather than
    merely unreachable, per `PORTING.md`'s POSIX-only rule. `WGPU_BACKEND` still overrides
    at runtime (as do `WGPU_ADAPTER_NAME` and `WGPU_DEBUG`) — those are left enabled on
    purpose as the modern equivalent of the old `-gl`/`-dx9` switches.
11. **`Renderer::new` blocks** on `pollster::block_on` for the adapter and device requests.
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

## Not implemented

Stages 3-8. Nothing here reads a `.vmt` or a real shader; there is no material, no
pipeline cache, no vertex buffer, and no render-target stack. Also deliberately absent,
and listed so nobody looks for them:

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

## `TextureBlit` — temporary, and meant to be deleted

`blit.rs` and `shaders/blit.wgsl` draw one texture over the whole frame. They exist
because `portdocs/MATERIALSYSTEM.md` §9 makes stage 2's deliverable a real `.vtf` on
screen, and because a `.vtf` that *parses* is no evidence that the bytes reached the GPU
in the right order, in the right format, with the right rows.

`src/launcher/`'s `-vtf <name>` switch is the way to ask for one:

```
cargo run -- -basedir /path/to/game -game portal2 -window -vtf metal/metalwall048a
```

A missing or broken name draws the error checkerboard, which is itself worth seeing.

**Do not grow this.** Stage 3 brings the real pipeline — a `.vmt`-driven bind-group layout
(§7.4), the WGSL prelude (§7.5), and a pipeline cache keyed on real state. When
`UnlitGeneric` can draw a quad, delete `blit.rs`, `shaders/blit.wgsl`, `Frame::blit` and
the `-vtf` switch together.

## Test coverage

43 tests, in three groups.

**Pure logic, no GPU** (40) — the parts where a mistake is invisible rather than loud:

| Tests | Guard |
|---|---|
| `image_format` (15) | the size arithmetic that decides where every mip level starts in a file, and every CPU format conversion, channel by channel |
| `vtf` (18) | every version 7.0-7.5, the seventh cubemap face, partial mip chains, the thumbnail, flag masking, and each way a file can be malformed |
| `texture` (7) | the `.vtf` flags -> sampler policy, and name normalization |

The `vtf` tests build files with an in-memory writer that can produce *archaic* and
*malformed* ones deliberately — a 7.1 cubemap with its spheremap face, a 7.4 cubemap
without one, a truncated image, a mip count longer than the chain. Those are the cases
real content actually contains and that no valid-file test would reach.

**End to end, on a real GPU** (3, in `blit.rs`) — `.vtf` bytes, through the loader, onto
the GPU, through the shader, and back to the CPU by rendering to an offscreen target and
reading the pixels back. They check that an uncompressed texture arrives the right way up
with its channels in order (clip space puts -1 at the bottom, texture space puts `v = 0`
at the top — getting it wrong renders a perfectly plausible upside-down picture), that a
DXT1 block is decoded by the hardware, and that the error checkerboard has the right
colours on the right 4-texel period.

These are the only tests in `src/materials/` that touch a GPU, and they earn it: row
pitch, block layout, channel order and orientation are all invisible to a unit test.
**They skip, printing why, when no adapter with BC support is available**, so a machine
with no GPU still gets a green `cargo test`.

Nothing tests `Renderer` itself, and that stays deliberate: every function there either
calls `wgpu` or hands a value straight to it, so a unit test would assert that arguments
were passed along. What verifies it is running it. On macOS/Metal that produces:

```
source-engine: renderer: Apple M1 Pro (IntegratedGpu, "") via Metal
source-engine: renderer: 800x600 Bgra8UnormSrgb, vsync on
source-engine: renderer: first frame presented
```

The third line is the one that matters and is printed once, from `src/engine/window/`:
creating a device and creating a window both succeed on machines where nothing is ever
presented, so "a window opened" is not evidence that the GPU path works. That line is.
