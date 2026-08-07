//! Output/control rendering, fullscreen, and monitor placement.

use super::*;

impl App {
    pub(super) fn render_output(&mut self) {
        let (Some(g), Some(r)) = (self.graphics.as_ref(), self.renderer.as_mut()) else {
            return;
        };
        if self.output_occluded {
            return;
        }
        let (w, h) = (g.output.config.width, g.output.config.height);
        if let Some(frame) = g.output.acquire(&g.device) {
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut encoder = g
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            r.render(&g.device, &g.queue, &mut encoder, &view, w, h);
            g.queue.submit([encoder.finish()]);
            frame.present();
        }
        if !self.fullscreen_applied {
            self.fullscreen_applied = true;
            self.apply_fullscreen_initial();
        }
    }

    pub(super) fn render_control(&mut self) {
        let (Some(g), Some(egui)) = (self.graphics.as_ref(), self.egui.as_mut()) else {
            return;
        };
        if self.control_occluded {
            return;
        }
        egui.render(&g.device, &g.queue, &g.control, &self.mirror, &self.cmd_tx);
    }

    pub(super) fn apply_fullscreen_initial(&mut self) {
        if self.windowed {
            return;
        }
        if let Some(g) = self.graphics.as_ref() {
            let monitor = pick_monitor_from_window(&g.output.window, self.monitor);
            g.output
                .window
                .set_fullscreen(Some(Fullscreen::Borderless(monitor)));
        }
    }

    pub(super) fn toggle_fullscreen(&mut self) {
        if let Some(g) = self.graphics.as_ref() {
            if g.output.window.fullscreen().is_some() {
                g.output.window.set_fullscreen(None);
            } else {
                let monitor = pick_monitor_from_window(&g.output.window, self.monitor);
                g.output
                    .window
                    .set_fullscreen(Some(Fullscreen::Borderless(monitor)));
            }
        }
    }
}
