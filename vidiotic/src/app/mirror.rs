//! Building the read-only `UiMirror` published to the UI each tick.

use super::cameras::camera_device_pairs;
use super::*;

impl App {
    /// Publish this tick's read-only view for the UI.
    ///
    /// The engine fills what a *session* knows; this fills what a *machine*
    /// does. Splitting it that way is what lets the panels reading this mirror
    /// compile for a browser, where none of the overlay below exists
    /// (web-port.md §8 step 4g) — and it means there is one builder for the
    /// shared 90%, not two to keep in step.
    pub(super) fn build_mirror(&mut self, snap: &crate::clock::ClockSnapshot, audio: &AudioFrame) {
        self.engine.build_mirror(snap, audio, &mut self.mirror);

        // --- the overlay: everything the engine has no way to answer ---

        self.mirror.project_path = self.project_path.as_ref().map(|p| p.display().to_string());
        self.mirror.bpm_entry = self.bpm_entry.clone();
        self.mirror.audio_devices = self.audio_devices.clone();
        self.mirror.current_device = Some(self.audio_capture.device_name.clone());
        self.mirror.shader_name = self
            .shader_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
        self.mirror.shader_error = self
            .renderer
            .as_ref()
            .and_then(|r| r.shader_error())
            .cloned();
        self.mirror.shader_pool = self
            .renderer
            .as_ref()
            .map(|r| r.pool_view())
            .unwrap_or_default();
        self.mirror.fullscreen = self
            .graphics
            .as_ref()
            .is_some_and(|g| g.output.window.fullscreen().is_some());

        // Cached thumbnails and probe metadata: a texture this shell uploaded,
        // and the fps/duration a runtime `Clip` does not retain. The engine
        // leaves both at their defaults rather than guessing, so this is a
        // patch over its output, not a second pass building the rows.
        let has_thumb = |id: ClipId| self.egui.as_ref().is_some_and(|e| e.has_thumb(id));
        for e in &mut self.mirror.clips {
            e.has_thumb = has_thumb(e.id);
            e.duration_sec = self.clip_meta.get(&e.id).and_then(|m| m.duration_sec);
            e.fps = self.clip_meta.get(&e.id).and_then(|m| m.fps);
        }
        for c in &mut self.mirror.cues {
            c.has_thumb = has_thumb(c.clip);
        }

        self.build_camera_rows();
    }

    /// The cameras section: last enumeration + live service status, with the
    /// same active/role marking as clip tiles (via the device's pool clip).
    ///
    /// The row builder is `Engine::camera_rows`, shared with the browser shell;
    /// what differs here is the status source — an `AVFoundation` capture service
    /// rather than a `MediaStream` tap.
    fn build_camera_rows(&mut self) {
        let devices = camera_device_pairs(&self.camera_devices);
        self.mirror.cameras = self.engine.camera_rows(&devices, |uid| {
            let caps = self.captures.borrow();
            let on_air = caps.is_on_air(uid);
            let status = if on_air {
                match caps.status(uid) {
                    Some(capture::ServiceStatus::Running { width, height, fps }) => {
                        format!("{width}x{height} @ {fps:.0}")
                    }
                    Some(capture::ServiceStatus::Failed(e)) => format!("error: {e}"),
                    Some(capture::ServiceStatus::Starting) | None => "starting…".into(),
                }
            } else {
                "off air".into()
            };
            (on_air, status)
        });
    }
}
