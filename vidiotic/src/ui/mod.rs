//! Control window: egui integrated as a library (not eframe). Owns the egui
//! context, winit input translation, the wgpu paint renderer, and the cached
//! clip thumbnails. `control_ui` reads a `UiMirror` and emits `Command`s.
//!
//! Layout is split by panel: [`transport`] (top), [`status`] (bottom),
//! [`editor`] (right, the selected cue's fields), and [`library`] (center,
//! the clip pool and cue banks). The palette, spacing scale, and shared
//! custom-painted controls come from the `phosphor` crate
//! ([`phosphor::theme`] / [`phosphor::widgets`]).

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use phosphor::theme;
use winit::window::Window;

use crate::commands::{ClipId, Command, UiMirror};
// The panels themselves. They were declared here as private modules until they
// moved to `vidiotic-play` so a browser could draw them too; this crate reaches
// for them the same way it already reaches for `render`, `grammar` and the rest
// (web-port.md §8 step 4g).
use crate::gfx::WindowSurface;
use vidiotic_play::ui::control_ui;

/// The control window's egui stack: context, winit input translation, wgpu
/// paint renderer, and the cached clip thumbnails.
pub struct EguiCtl {
    ctx: egui::Context,
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    thumbs: HashMap<ClipId, egui::TextureHandle>,
}

impl EguiCtl {
    /// Set up egui for the control window, painting to surfaces of `format`.
    pub fn new(window: &Window, device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(
            device,
            format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        );
        Self {
            ctx,
            state,
            renderer,
            thumbs: HashMap::new(),
        }
    }

    /// Whether a thumbnail texture is cached for this clip.
    pub fn has_thumb(&self, id: ClipId) -> bool {
        self.thumbs.contains_key(&id)
    }

    /// Drop all cached thumbnails (the clip pool was replaced).
    pub fn clear_thumbnails(&mut self) {
        self.thumbs.clear();
    }

    /// Feed a window event to egui. Returns (consumed, repaint).
    pub fn on_window_event(
        &mut self,
        window: &Window,
        event: &winit::event::WindowEvent,
    ) -> (bool, bool) {
        let r = self.state.on_window_event(window, event);
        (r.consumed, r.repaint)
    }

    /// Cache a thumbnail as an egui texture (called once per clip when decoded).
    pub fn set_thumbnail(&mut self, id: ClipId, w: usize, h: usize, rgba: &[u8]) {
        let image = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba);
        let handle =
            self.ctx
                .load_texture(format!("thumb:{id}"), image, egui::TextureOptions::LINEAR);
        self.thumbs.insert(id, handle);
    }

    /// Run `control_ui` for one frame and paint it into the control surface.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ws: &WindowSurface,
        mirror: &UiMirror,
        cmd_tx: &Sender<Command>,
    ) {
        // Re-derive the palette/style if the statusline's theme controls
        // (dark/light, hue) changed last frame.
        theme::sync(&self.ctx);
        let raw_input = self.state.take_egui_input(&ws.window);
        let ctx = self.ctx.clone();
        let thumbs = &self.thumbs;
        let full = ctx.run_ui(raw_input, |ui| control_ui(ui, mirror, cmd_tx, thumbs));
        self.state
            .handle_platform_output(&ws.window, full.platform_output);

        for (id, delta) in &full.textures_delta.set {
            self.renderer.update_texture(device, queue, *id, delta);
        }
        let prims = self.ctx.tessellate(full.shapes, full.pixels_per_point);
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [ws.config.width, ws.config.height],
            pixels_per_point: full.pixels_per_point,
        };

        if let Some(frame) = ws.acquire(device) {
            let view = frame.texture.create_view(&Default::default());
            let mut encoder = device.create_command_encoder(&Default::default());
            let user_bufs = self
                .renderer
                .update_buffers(device, queue, &mut encoder, &prims, &sd);
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu_clear_color(
                                    theme::palette().bg_base,
                                )),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                self.renderer.render(&mut pass, &prims, &sd);
            }
            queue.submit(user_bufs.into_iter().chain([encoder.finish()]));
            frame.present();
        }
        for id in &full.textures_delta.free {
            self.renderer.free_texture(id);
        }
        // No repaint scheduling: the engine requests a control redraw every
        // tick anyway (see `App::update`), which outpaces egui's repaint_delay.
    }
}

/// Convert a palette color to a wgpu clear color. The render targets in this
/// app are non-sRGB (see `gfx::WindowSurface::configure`), so a `LoadOp::Clear`
/// writes each channel's `0..1` fraction straight into the framebuffer with no
/// gamma conversion — matching how egui-wgpu paints `Color32` onto the same
/// non-sRGB target.
fn wgpu_clear_color(c: egui::Color32) -> wgpu::Color {
    wgpu::Color {
        r: c.r() as f64 / 255.0,
        g: c.g() as f64 / 255.0,
        b: c.b() as f64 / 255.0,
        a: 1.0,
    }
}

pub(crate) enum PickKind {
    ClipDir,
    ClipBankDir,
    Shader,
    /// Pick an ISF `.fs` to add to the selected cue's effect chain.
    Isf,
    /// Save the project. The payload is the currently-loaded path, used to
    /// pre-fill the dialog's directory and file name (`None` = a fresh session).
    SaveProject(Option<PathBuf>),
    /// Pick a `.viproj` to load, replacing the running session.
    OpenProject,
}

/// Open a native picker on a worker thread and deliver the choice as a Command.
/// (`NSOpenPanel` is main-thread-only for blocking dialogs, so use the async API.)
///
/// Every kind is the same four steps — configure a dialog, await it off the main
/// thread, wrap the path in a command, send it — so only the two parts that
/// differ are written per kind: how the dialog is built, and which command the
/// path becomes. Six copies of the thread-and-`block_on` scaffolding was six
/// places to get the "user cancelled" case wrong.
pub(crate) fn pick_file(tx: Sender<Command>, kind: PickKind) {
    /// Await `fut` on a worker thread and send `to_command(path)` if the user
    /// picked something. A cancelled dialog yields `None` and sends nothing.
    fn deliver<F>(tx: Sender<Command>, fut: F, to_command: fn(PathBuf) -> Command)
    where
        F: std::future::Future<Output = Option<rfd::FileHandle>> + Send + 'static,
    {
        std::thread::spawn(move || {
            if let Some(h) = pollster::block_on(fut) {
                let _ = tx.send(to_command(h.path().to_path_buf()));
            }
        });
    }

    let dialog = rfd::AsyncFileDialog::new;
    match kind {
        PickKind::ClipDir => deliver(tx, dialog().pick_folder(), Command::SetClipDir),
        PickKind::ClipBankDir => deliver(tx, dialog().pick_folder(), Command::AddClipDirAsBank),
        PickKind::Shader => deliver(
            tx,
            dialog()
                .add_filter("shaders", &["frag", "fs", "glsl", "wgsl"])
                .pick_file(),
            Command::SetShaderPath,
        ),
        PickKind::Isf => deliver(
            tx,
            dialog()
                .add_filter("ISF shader", &["fs", "frag", "glsl"])
                .pick_file(),
            Command::LoadIsf,
        ),
        PickKind::OpenProject => deliver(
            tx,
            dialog().add_filter("project", &["viproj"]).pick_file(),
            Command::LoadProject,
        ),
        PickKind::SaveProject(current) => {
            // The only kind whose dialog depends on session state: it opens on
            // the loaded project's directory under its own name, so a plain
            // save-as is one keystroke rather than a navigation.
            let mut d = dialog().add_filter("project", &["viproj"]);
            let name = current
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("session.viproj");
            d = d.set_file_name(name);
            if let Some(dir) = current.as_ref().and_then(|p| p.parent()) {
                d = d.set_directory(dir);
            }
            deliver(tx, d.save_file(), Command::SaveProjectTo);
        }
    }
}
