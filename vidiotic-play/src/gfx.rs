//! wgpu setup: one shared Device/Queue driving two surfaces — the fullscreen
//! output (video+shader) and the control surface (egui).
//!
//! The two heads are the same idea on both targets, and deliberately so: native
//! gives each head a winit window, and the browser gives the control head a
//! winit canvas in this document and the output head a bare canvas in a second,
//! popped-out one. One device spans both either way, so a frame is still one
//! `CommandEncoder` and one `submit` (measured in `docs/spikes/dual-head.html`,
//! web-port.md §10a). Only surface *construction* differs; nothing downstream
//! of [`Graphics`] knows which target it is drawing to.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use winit::window::Window;

/// Device features negotiated rather than required.
///
/// `TEXTURE_COMPRESSION_BC` is the HAP fast path — universal on desktop, and
/// present in WebGPU wherever the browser exposes `texture-compression-bc`
/// (measured, web-port.md §1a). Because it is a statement about *other*
/// people's machines it has to be asked rather than demanded, and the answer
/// has to reach the decode path: without BC, a HAP clip must be decoded to RGBA
/// before upload instead of going to the GPU as blocks.
///
/// A struct rather than a bare `bool` so the ASTC/ETC2 tiers §1a measured for
/// mobile can be added here without changing anyone's signature.
#[derive(Clone, Copy, Debug)]
pub struct GpuCaps {
    /// BC1/BC3/BC4/BC7 block textures — the HAP upload path.
    pub bc: bool,
}

/// What a head's surface is built from.
///
/// Split by target rather than unified because the two worlds genuinely have
/// nothing in common here: natively a head is a winit window, and on the web it
/// is a canvas element that winit never sees.
enum HeadTarget {
    #[cfg(not(target_arch = "wasm32"))]
    Window(Arc<Window>),
    /// A canvas element, taken directly.
    ///
    /// This is the whole reason winit is absent from the browser build. The
    /// output head lives in a *second document* (the popped-out window), and
    /// winit would hand wgpu a `RawWindowHandle::Web` — a branch that resolves
    /// its canvas by `document.query_selector_all` against `web_sys::window()`,
    /// which from the opener searches the wrong document, finds nothing, and
    /// panics. `SurfaceTarget::Canvas` takes the element and never consults a
    /// document at all (web-port.md §10a).
    ///
    /// The control head is a canvas too, which the spike did not require but
    /// which falls out well: with no winit on the web there is no `spawn_app`
    /// that never returns, no `egui-winit`, and the render loop is a plain
    /// `requestAnimationFrame` — which is what pull-based decode wants anyway.
    /// The cost is that egui's raw input is assembled by hand; see `web::input`.
    #[cfg(target_arch = "wasm32")]
    Canvas(web_sys::HtmlCanvasElement),
}

impl HeadTarget {
    fn size(&self) -> (u32, u32) {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Window(w) => {
                let s = w.inner_size();
                (s.width, s.height)
            }
            #[cfg(target_arch = "wasm32")]
            Self::Canvas(c) => (c.width(), c.height()),
        }
    }

    fn create(self, instance: &wgpu::Instance) -> anyhow::Result<Head> {
        Ok(match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::Window(w) => Head {
                surface: instance.create_surface(w.clone())?,
                window: w,
            },
            #[cfg(target_arch = "wasm32")]
            Self::Canvas(c) => Head {
                surface: instance.create_surface(wgpu::SurfaceTarget::Canvas(c))?,
            },
        })
    }
}

/// A created-but-not-yet-configured head.
struct Head {
    surface: wgpu::Surface<'static>,
    #[cfg(not(target_arch = "wasm32"))]
    window: Arc<Window>,
}

/// One head and its configured swapchain surface.
pub struct WindowSurface {
    /// The winit window backing this head. Native only — on the web a head is
    /// a canvas and nothing asks it for a window, so the field does not exist
    /// rather than being an `Option` every native caller would have to unwrap.
    #[cfg(not(target_arch = "wasm32"))]
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
}

impl WindowSurface {
    fn configure(
        device: &wgpu::Device,
        adapter: &wgpu::Adapter,
        head: Head,
        size: (u32, u32),
        present_mode: wgpu::PresentMode,
    ) -> Self {
        let surface = head.surface;
        // Gamma-space pipeline everywhere: prefer a non-sRGB surface format.
        let caps = surface.get_capabilities(adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.0.max(1),
            height: size.1.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(device, &config);
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            window: head.window,
            surface,
            config,
        }
    }

    /// Reconfigure the surface for a new size (zero sizes are ignored).
    pub fn resize(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(device, &self.config);
        }
    }

    /// Get the next drawable, reconfiguring on Outdated/Lost. `None` means
    /// skip this frame (no drawable, or the surface was just rebuilt).
    pub fn acquire(&self, device: &wgpu::Device) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(device, &self.config);
                None
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => None,
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("surface validation error");
                None
            }
        }
    }
}

/// The shared GPU context: one Device/Queue driving both head surfaces.
pub struct Graphics {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// What the adapter actually granted. See [`GpuCaps`].
    pub caps: GpuCaps,
    pub output: WindowSurface,
    pub control: WindowSurface,
}

impl Graphics {
    /// Native setup: a winit window per head.
    ///
    /// # Errors
    /// If surface creation fails, if no adapter is available, if the device
    /// request fails, or if the GPU lacks BC textures.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(output_win: Arc<Window>, control_win: Arc<Window>) -> anyhow::Result<Self> {
        pollster::block_on(Self::build(
            HeadTarget::Window(output_win),
            HeadTarget::Window(control_win),
        ))
    }

    /// Web setup: a canvas per head. The control canvas lives in this document
    /// and the output canvas in the window this one opened — a distinction this
    /// function does not need to know about, which is the point of §10a.
    ///
    /// # Errors
    /// As [`Self::new`], except that missing BC is a warning rather than an
    /// error — the browser is where the RGBA fallback applies.
    #[cfg(target_arch = "wasm32")]
    pub async fn new_web(
        output_canvas: web_sys::HtmlCanvasElement,
        control_canvas: web_sys::HtmlCanvasElement,
    ) -> anyhow::Result<Self> {
        Self::build(
            HeadTarget::Canvas(output_canvas),
            HeadTarget::Canvas(control_canvas),
        )
        .await
    }

    /// Pick an adapter, negotiate features, create the device, and configure
    /// both surfaces — output on Fifo (vsync paces the render loop), control on
    /// `AutoVsync`.
    ///
    /// One body for both targets on purpose: negotiation and configuration are
    /// where the behaviour lives, so they are not cfg-forked. Natively the
    /// adapter and device futures are already-ready, which is why
    /// [`Self::new`] can block on this for free.
    async fn build(output: HeadTarget, control: HeadTarget) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::default();
        let (out_size, ctl_size) = (output.size(), control.size());
        let out_head = output.create(&instance)?;
        let ctl_head = control.create(&instance)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&out_head.surface),
                force_fallback_adapter: false,
            })
            .await?;

        let bc = adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);

        // Negotiated, not required — but only where there is something to fall
        // back to. On the web an absent BC means the RGBA path (web-port.md §4).
        // Natively there is no such path yet, and a device without BC would
        // build a `Bc1RgbaUnorm` texture in `render::upload_frame` and show
        // black, so keep failing loudly until the fallback exists. That is what
        // makes this a behaviour-preserving change rather than a silent
        // regression shipped ahead of its safety net.
        #[cfg(not(target_arch = "wasm32"))]
        anyhow::ensure!(
            bc,
            "GPU lacks BC texture compression (required for HAP clips)"
        );
        #[cfg(target_arch = "wasm32")]
        if !bc {
            log::warn!("no texture-compression-bc: HAP clips need the RGBA fallback path");
        }

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("vidiotic-device"),
                required_features: if bc {
                    wgpu::Features::TEXTURE_COMPRESSION_BC
                } else {
                    wgpu::Features::empty()
                },
                required_limits: wgpu::Limits::default(),
                ..Default::default()
            })
            .await?;

        let output = WindowSurface::configure(
            &device,
            &adapter,
            out_head,
            out_size,
            wgpu::PresentMode::Fifo,
        );
        let control = WindowSurface::configure(
            &device,
            &adapter,
            ctl_head,
            ctl_size,
            wgpu::PresentMode::AutoVsync,
        );
        Ok(Self {
            device,
            queue,
            caps: GpuCaps { bc },
            output,
            control,
        })
    }
}
