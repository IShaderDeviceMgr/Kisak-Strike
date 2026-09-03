//! GPU bring-up: adapter and device selection, the presentation surface, and
//! the frame boundary every later stage draws inside.
//!
//! This replaces the entire `IShaderDeviceMgr` / `IShaderDevice` /
//! `IShaderAPI` / `IShaderShadow` tower, `materialsystem/shaderapidx9/`,
//! `materialsystem/glmgr/` and `togl/` — ~148k lines of D3D9 abstraction and
//! D3D9→OpenGL translation, none of which is ported. `wgpu` is used directly.
//! See `portdocs/MATERIALSYSTEM.md` §5.1 for the responsibility-by-
//! responsibility mapping.
//!
//! What that section promises, concretely:
//!
//! | `IShaderDevice*` responsibility | here |
//! |---|---|
//! | `SetAdapter`, adapter enumeration | [`RendererOptions::adapter_index`] |
//! | `SetMode(hWnd, ShaderDeviceInfo_t)` | [`Renderer::new`] + [`Renderer::resize`] |
//! | `Present()` | [`Frame::present`] |
//! | `ReleaseResources`/`ReacquireResources`, device-lost | [`Renderer::begin_frame`] |
//! | `SetHardwareGammaRamp` | an sRGB surface format, chosen in [`Renderer::new`] |
//! | `AddView`/`RemoveView`/`SetView` (Hammer child windows) | gone; one window |
//!
//! The public surface here is deliberately the minimum that acquires, records
//! and presents. It has not grown since stage 1 and should not: stage 3 draws
//! by adding a `Frame` method in its own file (`preview.rs`), and the
//! render-target stack of stage 4 is the next thing with a claim on it.

use std::sync::Arc;

use super::error::RendererError;
use super::pipeline::TargetFormat;
use super::target::{DepthBuffer, CLEAR_DEPTH, DEPTH_FORMAT};

/// Backends we ask `wgpu` for.
///
/// POSIX only, per `PORTING.md`'s "Supported platforms": Metal on macOS,
/// Vulkan on Linux, GL as the Linux fallback. DX12 and BrowserWebGPU are
/// omitted rather than merely unreachable, so that a mistaken build
/// configuration fails loudly instead of quietly selecting something we do not
/// test. `WGPU_BACKEND` still overrides this at runtime (see
/// [`Renderer::new`]), which is the modern equivalent of `-gl`/`-dx9`-style
/// backend switches.
const BACKENDS: wgpu::Backends = wgpu::Backends::METAL
    .union(wgpu::Backends::VULKAN)
    .union(wgpu::Backends::GL);

/// How many frames the presentation engine may queue ahead.
///
/// `wgpu`'s own default, and the closest equivalent of
/// `MaterialSystem_Config_t::m_bWantTripleBuffered` (default `false`).
const FRAME_LATENCY: u32 = 2;

/// The colour an otherwise-empty frame is cleared to.
///
/// Not black, deliberately: a black window is indistinguishable from a window
/// that never got a frame at all, and "did the clear actually happen" is the
/// entire deliverable of stage 1.
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.09,
    g: 0.11,
    b: 0.13,
    a: 1.0,
};

/// A window the renderer can present to.
///
/// Deliberately *not* `winit::window::Window`. The material system has no
/// business knowing which windowing library the engine uses — it needs a raw
/// handle and nothing else, which is also why `ILauncherMgr`
/// (`public/appframework/ilaunchermgr.h`), the interface `CMaterialSystem::Connect`
/// reached through for `g_pLauncherMgr` (`cmaterialsystem.cpp:206`), has no
/// counterpart here.
///
/// Blanket-implemented, so `Arc<winit::window::Window>` — or anything else
/// carrying both handles — satisfies it without an adapter type.
pub trait RenderWindow:
    wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + Send + Sync + 'static
{
}

impl<T> RenderWindow for T where
    T: wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + Send + Sync + 'static
{
}

/// Startup knobs for the renderer.
///
/// The survivors of `MaterialSystem_Config_t`'s video half that actually reach
/// the device. Everything else in that struct is either 2004-era hardware
/// variety (`dxSupportLevel`, `bCompressedTextures`, `m_fMonitorGamma`) or
/// belongs to a later stage.
#[derive(Debug, Clone)]
pub struct RendererOptions {
    /// `-adapter <n>`: pick the n-th enumerated adapter instead of letting
    /// `wgpu` choose. `engine/gl_shader.cpp:73` reads the same switch.
    pub adapter_index: Option<usize>,

    /// Whether presentation waits for vertical blank.
    ///
    /// Inverted `MATSYS_VIDCFG_FLAGS_NO_WAIT_FOR_VSYNC`; `-mat_vsync <0|1>`
    /// sets it. See [`crate::engine::window::VideoConfig`] for why the default
    /// is the opposite of Valve's.
    pub vsync: bool,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            adapter_index: None,
            vsync: true,
        }
    }
}

/// Owns the GPU device and the window's presentation surface.
///
/// One per process — `PORTING.md`'s single-window rule. Constructing one is
/// the whole of `CShaderDeviceMgr::SetMode`, and it either yields a usable
/// device or an error; there is no half-initialized state and no
/// `Connect`/`Init`/`Shutdown`/`Disconnect` lifecycle. Teardown is `Drop`.
pub struct Renderer {
    /// Kept so the surface can be rebuilt after `SurfaceStatus::Lost`, which
    /// is the one case `wgpu` cannot recover from by reconfiguring.
    instance: wgpu::Instance,
    window: Arc<dyn RenderWindow>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// False while the window has no area — `Surface::configure` panics on a
    /// zero width or height, and minimizing a window reports exactly that.
    configured: bool,
    /// Set when the surface reported itself suboptimal; acted on at the start
    /// of the next frame, because reconfiguring while a `SurfaceTexture` is
    /// alive panics.
    reconfigure_pending: bool,
    /// The back buffer's depth-stencil attachment, kept in step with the
    /// surface's size. `None` exactly when the surface is unconfigured, which
    /// is when there is no area to allocate it for.
    ///
    /// It lives here rather than in the render context because it belongs to
    /// the *back buffer*: D3D9 created the depth buffer as part of the
    /// swap chain (`D3DPRESENT_PARAMETERS::AutoDepthStencilFormat`), and
    /// resizing it anywhere else would mean two places that have to agree
    /// about the window's size.
    depth: Option<DepthBuffer>,
}

impl Renderer {
    /// Brings up the GPU and configures the window's surface.
    ///
    /// `display` is the windowing system's display connection, kept separate
    /// from `window` because `wgpu` wants it on the instance: on Wayland and
    /// GLES the connection must be shared with the surfaces created from it.
    /// With `winit` this is `EventLoop::owned_display_handle()`.
    ///
    /// `size` is the window's *physical* (not logical) client size. A zero
    /// dimension is legal and leaves the surface unconfigured until
    /// [`resize`](Self::resize) reports a real one.
    ///
    /// Environment overrides `wgpu` reads here — `WGPU_BACKEND`,
    /// `WGPU_ADAPTER_NAME`, `WGPU_DEBUG` — are deliberately left enabled;
    /// they are the debugging equivalents of the `-gl`/`-dx9`/`-adapter`
    /// switches the original tree had.
    pub fn new<D>(
        window: Arc<dyn RenderWindow>,
        display: D,
        size: (u32, u32),
        options: &RendererOptions,
    ) -> Result<Self, RendererError>
    where
        D: wgpu::rwh::HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static,
    {
        let mut descriptor = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display));
        descriptor.backends = BACKENDS;
        let instance = wgpu::Instance::new(descriptor.with_env());

        let surface = instance.create_surface(window.clone())?;
        let adapter = select_adapter(&instance, &surface, options.adapter_index)?;
        let info = adapter.get_info();

        // Every texture Valve ships is DXT-compressed, so this is not optional
        // and there is no fallback tier to drop to — decompressing on the CPU
        // would quadruple both load time and video memory for the whole game.
        // Checked here rather than left to `request_device` so the message says
        // what is actually missing.
        if !adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            return Err(RendererError::NoBlockCompression {
                adapter: info.name.clone(),
            });
        }

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("source-engine"),
                required_features: wgpu::Features::TEXTURE_COMPRESSION_BC,
                // The portable floor, not the adapter's ceiling. `portdocs/
                // MATERIALSYSTEM.md` §4.6 replaces `IMaterialSystemHardwareConfig`'s
                // ~50 caps queries and the `dxlevel` ladder with one fixed
                // capability tier; asking for the same limits on every machine is
                // what makes that tier mean something. Raise this deliberately when
                // a shader needs more, not adapter-by-adapter.
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            }))?;

        let caps = surface.get_capabilities(&adapter);
        // An sRGB surface format is the replacement for
        // `IShaderDevice::SetHardwareGammaRamp`: the hardware does the encode
        // on write instead of the engine warping the display's gamma ramp
        // process-wide (and leaving it warped if it crashed).
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| RendererError::SurfaceUnsupported {
                adapter: info.name.clone(),
            })?;

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            // SDR. HDR is an open question in `portdocs/MATERIALSYSTEM.md` §10
            // (Portal 2 ships HDR-lit maps), and switching it on means picking
            // a float format here and adding a tonemap pass, not just changing
            // this field.
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.0,
            height: size.1,
            present_mode: if options.vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            desired_maximum_frame_latency: FRAME_LATENCY,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };

        eprintln!(
            "source-engine: renderer: {} ({:?}, {:?}) via {:?}",
            info.name, info.device_type, info.driver, info.backend
        );
        eprintln!(
            "source-engine: renderer: {}x{} {:?}, vsync {}",
            config.width,
            config.height,
            config.format,
            if options.vsync { "on" } else { "off" }
        );

        let mut renderer = Renderer {
            instance,
            window,
            device,
            queue,
            surface,
            config,
            configured: false,
            reconfigure_pending: false,
            depth: None,
        };
        renderer.reconfigure();
        Ok(renderer)
    }

    /// The GPU device, for building textures, buffers and pipelines.
    ///
    /// `wgpu::Device` and `wgpu::Queue` are cheap handles to shared state and
    /// are `Clone`, so a subsystem that needs to create resources holds its own
    /// copy rather than borrowing the renderer for its lifetime — which is what
    /// lets [`crate::materials::texture::TextureCache`] exist outside this struct.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The submission queue. See [`Renderer::device`].
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Reports a new physical window size.
    ///
    /// Cheap and idempotent: a resize to the current size does nothing. A zero
    /// dimension (a minimized window) is not an error — it unconfigures the
    /// surface, and [`begin_frame`](Self::begin_frame) then yields `None`
    /// until there is area to draw into again.
    /// What a pipeline drawn into the back buffer must be built for.
    ///
    /// The colour format the surface was configured with, plus the depth
    /// format of `target::DEPTH_FORMAT`, plus one sample. Pass it to
    /// [`Material::pipeline_key`](super::material::Material::pipeline_key), or
    /// read it off a [`Pass`](super::context::Pass), which carries the one it
    /// was opened against.
    pub fn target_format(&self) -> TargetFormat {
        TargetFormat {
            color: self.config.format,
            depth: Some(DEPTH_FORMAT),
            // MSAA is still owed from stage 1 (`-mat_antialias`); a
            // multisampled back buffer needs a resolve target here and a
            // matching `samples` on every pipeline, which is one field in each
            // of two places rather than a design question.
            samples: 1,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if self.config.width == width && self.config.height == height && self.configured {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.reconfigure();
    }

    /// Acquires the next frame, or `None` if this frame should be skipped.
    ///
    /// `None` is **not** an error and must not be logged per-frame: it is what
    /// an occluded, minimized, timed-out or just-invalidated surface looks
    /// like. Skip the frame and come back next tick; the renderer has already
    /// done whatever recovery was needed. This is the whole of what
    /// `IShaderDevice::ReleaseResources`/`ReacquireResources` and the
    /// `IShaderDeviceDependentObject` device-lost callbacks used to do.
    ///
    /// The returned [`Frame`] borrows the renderer mutably for its lifetime,
    /// which is not incidental — `Surface::configure` panics if a frame is
    /// still alive, so the borrow checker enforces a real `wgpu` rule.
    pub fn begin_frame(&mut self) -> Option<Frame<'_>> {
        if self.reconfigure_pending {
            self.reconfigure();
            self.reconfigure_pending = false;
        }
        if !self.configured {
            return None;
        }

        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => texture,
            // Usable this frame, but the surface no longer matches its
            // configuration. Draw it, then reconfigure before the next one.
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                self.reconfigure_pending = true;
                texture
            }
            // Nothing to recover from; the window simply is not on screen or
            // the presentation engine did not hand a buffer back in time.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return None
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.reconfigure();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.recreate_surface();
                return None;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("source-engine: renderer: surface validation error, frame skipped");
                return None;
            }
        };

        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        Some(Frame {
            queue: &self.queue,
            depth: self.depth.as_ref().map(DepthBuffer::view),
            size: (self.config.width, self.config.height),
            format: self.target_format(),
            texture,
            view,
            encoder,
        })
    }

    /// Applies [`Self::config`] to the surface, tracking whether it took, and
    /// keeps the depth buffer the same size as the colour one — a mismatch is
    /// a `wgpu` validation error rather than a stretched picture.
    fn reconfigure(&mut self) {
        self.configured = self.config.width > 0 && self.config.height > 0;
        if !self.configured {
            // Nothing to attach it to, and `wgpu` rejects a zero-sized
            // texture. Reallocated on the way back from minimized.
            self.depth = None;
            return;
        }
        self.surface.configure(&self.device, &self.config);
        match &mut self.depth {
            Some(depth) => {
                depth.resize(&self.device, self.config.width, self.config.height);
            }
            None => {
                self.depth = Some(DepthBuffer::new(
                    &self.device,
                    self.config.width,
                    self.config.height,
                ));
            }
        }
    }

    /// Rebuilds the surface from the window after `SurfaceStatus::Lost`.
    ///
    /// The one surface condition reconfiguring cannot fix. If the *device* is
    /// gone too this will keep failing, and the frames keep being skipped
    /// rather than the process dying — the original's response was
    /// `Sys_Error()`, and a black window that recovers when the compositor
    /// comes back is strictly better.
    fn recreate_surface(&mut self) {
        match self.instance.create_surface(self.window.clone()) {
            Ok(surface) => {
                self.surface = surface;
                self.reconfigure();
            }
            Err(err) => {
                self.configured = false;
                eprintln!("source-engine: renderer: lost surface, and could not rebuild it: {err}");
            }
        }
    }
}

/// Selects the adapter, honouring `-adapter <n>`.
fn select_adapter(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'_>,
    index: Option<usize>,
) -> Result<wgpu::Adapter, RendererError> {
    let Some(index) = index else {
        // No explicit choice: let `wgpu` rank them. `HighPerformance` is the
        // discrete-GPU preference, which is what a game wants and what
        // `CShaderDeviceMgr`'s adapter scan (largest video memory wins) was
        // approximating by hand.
        return Ok(pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(surface),
                ..Default::default()
            },
        ))?);
    };

    let adapters = pollster::block_on(instance.enumerate_adapters(BACKENDS));
    let available = adapters.len();
    adapters
        .into_iter()
        .nth(index)
        .ok_or(RendererError::NoSuchAdapter {
            requested: index,
            available,
        })
}

/// One acquired frame: a swap-chain image and the encoder recording into it.
///
/// Dropping a `Frame` without calling [`present`](Self::present) discards
/// everything recorded into it and releases the image un-presented, which is
/// the correct behavior when a frame is abandoned mid-way.
#[must_use = "a Frame that is never presented is silently discarded"]
pub struct Frame<'a> {
    queue: &'a wgpu::Queue,
    /// The renderer's depth-stencil attachment. Always `Some` in practice —
    /// a frame is only acquired when the surface is configured, which is when
    /// the depth buffer exists — but carried as an `Option` so that the one
    /// place the two could disagree is a `None` attachment rather than a
    /// panic.
    depth: Option<&'a wgpu::TextureView>,
    size: (u32, u32),
    format: TargetFormat,
    texture: wgpu::SurfaceTexture,
    view: wgpu::TextureView,
    encoder: wgpu::CommandEncoder,
}

impl Frame<'_> {
    /// The back buffer's physical size in pixels, for `cScreenSize` and for a
    /// viewport that means "all of it".
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// What a pipeline drawn into this frame must be built for.
    /// [`Renderer::target_format`].
    pub fn target_format(&self) -> TargetFormat {
        self.format
    }

    /// Clears the frame and draws nothing.
    ///
    /// `ClearBuffers( true, true )` with no pass after it — what a frame with
    /// no scene to draw does. A frame that *is* drawing should open its pass
    /// with [`Load::Clear`](super::context::Load::Clear) instead and clear as
    /// part of it, rather than paying for two passes over the frame.
    pub fn clear(&mut self, color: wgpu::Color) {
        let (encoder, view, depth) = self.parts();
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: depth.map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR_DEPTH),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// The encoder and the two attachments, borrowed together.
    ///
    /// One method rather than three accessors because a caller needs the
    /// encoder mutably *and* the views immutably at the same time, which only
    /// works as a single split borrow of the struct's fields.
    ///
    /// `pub(super)` so that [`RenderContext`](super::context::RenderContext)
    /// can open passes and nothing outside `src/materials/` can — opening a
    /// pass means deciding what its constants are, and that decision belongs
    /// in one place.
    pub(super) fn parts(
        &mut self,
    ) -> (
        &mut wgpu::CommandEncoder,
        &wgpu::TextureView,
        Option<&wgpu::TextureView>,
    ) {
        (&mut self.encoder, &self.view, self.depth)
    }

    /// Submits everything recorded and presents the frame.
    ///
    /// `IShaderDevice::Present`. Consuming `self` is the point: the frame
    /// ends here, and the renderer becomes usable again.
    ///
    /// Callers driving a `winit` window should call `Window::pre_present_notify`
    /// immediately before this — see `crate::engine::window`.
    pub fn present(self) {
        let Frame {
            queue,
            texture,
            encoder,
            view: _,
            depth: _,
            size: _,
            format: _,
        } = self;
        queue.submit(std::iter::once(encoder.finish()));
        queue.present(texture);
    }
}
