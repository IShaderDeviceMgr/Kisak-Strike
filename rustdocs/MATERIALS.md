# `src/materials/` — API reference

The material system. Right now that means one thing: the GPU device and the frame
boundary. Textures, materials, shaders and meshes are not here yet.

Porting design doc: [`portdocs/MATERIALSYSTEM.md`](../portdocs/MATERIALSYSTEM.md) — named
after the *original* module (`materialsystem/`), while this file is named after the Rust
one (`src/materials/`). Same subject, two names, on purpose.

| | |
|---|---|
| Module | `crate::materials` |
| Lines | ~500 |
| Tests | none here — see [Test coverage](#test-coverage) |
| Dependencies | `wgpu` 30, `pollster`, `thiserror` |
| Status | **Stage 1 of 8.** Brings up the GPU and clears a window. Stages 2+ not started |

**What is deliberately absent:** there is no `IShaderDevice`, no `IShaderAPI`, no
`IShaderShadow`, no device-abstraction trait, and no second backend. `wgpu` is called
directly. If you find yourself adding a trait so that "another renderer could be plugged
in later", stop — `wgpu` is already that abstraction, and re-adding the tower is the
specific mistake `portdocs/MATERIALSYSTEM.md` §5.1 exists to prevent.

## Quick start

```rust
use std::sync::Arc;
use crate::materials::{Renderer, RendererOptions, CLEAR_COLOR};

// `window` is an `Arc<winit::window::Window>`; `display` is the event loop's
// `OwnedDisplayHandle`.
let size = window.inner_size();
let mut renderer = Renderer::new(
    window.clone(),                 // coerces to Arc<dyn RenderWindow>
    display,
    (size.width, size.height),
    &RendererOptions::default(),
)?;

// ... once per frame:
if let Some(mut frame) = renderer.begin_frame() {
    frame.clear(CLEAR_COLOR);
    window.pre_present_notify();
    frame.present();
}

// ... on WindowEvent::Resized:
renderer.resize(size.width, size.height);
```

That is the whole API today. `src/engine/window/` is the only caller; see
[`rustdocs/ENGINE.md`](ENGINE.md).

## Core types

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

pub fn resize(&mut self, width: u32, height: u32);
pub fn begin_frame(&mut self) -> Option<Frame<'_>>;
```

Construction either yields a working device or an error — there is no
`Connect`/`Init`/`Shutdown`/`Disconnect` lifecycle and no half-initialized state.
Teardown is `Drop`.

`size` is **physical** pixels, not logical: on a HiDPI display these differ by the scale
factor, and configuring a surface with logical pixels produces a quarter-resolution image
stretched to fit. `winit`'s `Window::inner_size()` is already physical.

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
pub fn present(self);   // #[must_use] on the struct
```

One acquired swap-chain image plus the `CommandEncoder` recording into it. `present`
consumes it: submit, then present. Dropping a `Frame` without presenting discards
everything recorded into it — correct for an abandoned frame, silent data loss if
accidental, which is why the type is `#[must_use]`.

### `RendererError`

Every variant is a **startup** failure: `NoAdapter`, `NoSuchAdapter { requested,
available }`, `SurfaceCreation`, `SurfaceUnsupported { adapter }`, `DeviceCreation`.
Per-frame surface conditions are not errors and never surface as one — see below.

## The frame boundary

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

## Invariants and gotchas

Ordered by how likely each is to bite.

1. **A zero-size window is legal and must not reach `Surface::configure`, which panics on
   it.** Minimizing a window reports width or height 0. `resize(w, 0)` marks the surface
   unconfigured and `begin_frame` then returns `None` until real dimensions arrive. If you
   add another path that configures the surface, replicate that guard.
2. **`pre_present_notify` is the caller's job.** The renderer does not own a `winit`
   window, so it cannot make the call itself. It must happen immediately before
   `Frame::present`; skipping it costs compositor scheduling accuracy, not correctness.
3. **Sizes are physical pixels.** See `Renderer::new` above.
4. **The surface format is sRGB when the platform offers one** (`Bgra8UnormSrgb` on
   macOS/Metal). That is the replacement for `IShaderDevice::SetHardwareGammaRamp`: the
   hardware encodes on write instead of the engine warping the display's gamma ramp
   process-wide — and leaving it warped if it crashed. **Consequence for later stages:**
   values written by a shader are treated as *linear* and encoded on the way out. Do not
   apply an sRGB curve in shader code as well.
5. **Colour space is `Auto`, i.e. SDR.** Portal 2 ships HDR-lit maps, and HDR is still an
   open question (`portdocs/MATERIALSYSTEM.md` §10). Switching it on means a float format
   and a tonemap pass, not just changing this field.
6. **`required_limits` is `wgpu::Limits::default()` — the portable floor, not the
   adapter's ceiling.** Deliberate: §4.6 replaces `IMaterialSystemHardwareConfig`'s ~50
   caps queries and the `dxlevel` ladder with one fixed capability tier, and asking every
   machine for the same limits is what makes that tier mean anything. Raise it
   deliberately when a shader needs more; never adapter-by-adapter.
7. **Backends are `METAL | VULKAN | GL`.** DX12 and BrowserWebGPU are omitted rather than
   merely unreachable, per `PORTING.md`'s POSIX-only rule. `WGPU_BACKEND` still overrides
   at runtime (as do `WGPU_ADAPTER_NAME` and `WGPU_DEBUG`) — those are left enabled on
   purpose as the modern equivalent of the old `-gl`/`-dx9` switches.
8. **`Renderer::new` blocks** on `pollster::block_on` for the adapter and device requests.
   Fine at startup, on the main thread, once. Do not call it from a frame.

## Not implemented

Everything except stage 1. Nothing here reads a `.vtf`, a `.vmt`, or any shader; there is
no pipeline, no bind group, no vertex buffer, and no `Vfs` connection at all. Also
deliberately absent, and listed so nobody looks for them:

- **MSAA.** `-mat_antialias` is parsed nowhere yet. A multisampled swap chain needs a
  separate render target plus a resolve, which belongs with the render-target stack in
  stage 4.
- **Exclusive fullscreen video modes.** `CVideoMode_Common`'s mode enumeration and
  `AdjustWindow`'s mode switching are not ported; fullscreen is borderless on the current
  monitor. On a modern compositor an exclusive mode change buys nothing and costs a
  display reconfiguration on every alt-tab.
- **Refresh rate, gamma, `-mat_antialias`, `mat_queue_mode`.** All config-file territory.
- **Any headless/null path** (`mat_stub.cpp`, `cmatnullrendercontext.cpp`,
  `shaderapiempty/`). §5.4: if one is ever wanted it is a single enum branch here, not
  three parallel no-op implementations.

## Extending it

Stage 2 is textures (`.vtf` → `wgpu::Texture`) and stage 3 is materials plus the WGSL
prelude; `portdocs/MATERIALSYSTEM.md` §9 has the staging and §7 has the shader plan. Two
things to hold on to while doing them:

- **Keep render-pass recording free of global mutable state.** The queued render context
  (§5.3) is deleted because `wgpu` records command buffers on any thread, but the
  *reason* Valve built it — build draw lists off the main thread, submit once — is still
  the right architecture. Retrofitting that later is much harder than not breaking it now.
- **Do not fix the mesh/render-context API before reading `studiorender/` and `engine/`'s
  draw paths.** §9 stage 4 says this explicitly and it is the most expensive mistake
  available in this module.

## Test coverage

There are no tests in `src/materials/`, and that is a deliberate call rather than an
oversight: every function here either calls `wgpu` or hands a value straight to it, so a
unit test would assert that the arguments were passed along, on a machine that may have
no GPU at all. The parts that *are* testable pure logic — the whole command-line video
policy — live in `src/engine/window/` and are tested there (12 tests).

What actually verifies this module today is running it. On macOS/Metal that produces:

```
source-engine: renderer: Apple M1 Pro (IntegratedGpu, "") via Metal
source-engine: renderer: 800x600 Bgra8UnormSrgb, vsync on
source-engine: renderer: first frame presented
```

The third line is the one that matters and is printed once, from
`src/engine/window/`: creating a device and creating a window both succeed on machines
where nothing is ever presented, so "a window opened" is not evidence that the GPU path
works. That line is.
