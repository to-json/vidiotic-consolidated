//! Building the read-only `UiMirror` published to the UI each tick.

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

        self.mirror.project_path =
            self.project_path.as_ref().map(|p| p.display().to_string());
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
    /// Entirely native. A camera here is an `AVFoundation` device behind a capture
    /// service; the browser's equivalent is a `MediaStream` and shares no part of
    /// this but the row shape.
    fn build_camera_rows(&mut self) {
        let armed = self.engine.sequencer.armed();
        let live = &self.engine.banks[self.engine.live_bank];
        let active_clips: std::collections::HashSet<ClipId> =
            live.cues.iter().map(|c| c.clip).collect();
        let playing_clip = self.engine.current.and_then(|cid| live.cue(cid)).map(|c| c.clip);
        let armed_clip = armed.and_then(|cid| live.cue(cid)).map(|c| c.clip);

        self.mirror.cameras = self
            .camera_devices
            .iter()
            .map(|d| {
                let clip_id = self
                    .engine
                    .clips
                    .iter()
                    .find(|c| c.camera_uid() == Some(d.uid.as_str()))
                    .map(|c| c.id);
                let on_air = self.captures.borrow().is_on_air(&d.uid);
                let status: Arc<str> = if on_air {
                    match self.captures.borrow().status(&d.uid) {
                        Some(capture::ServiceStatus::Running { width, height, fps }) => {
                            format!("{width}x{height} @ {fps:.0}").into()
                        }
                        Some(capture::ServiceStatus::Failed(e)) => format!("error: {e}").into(),
                        Some(capture::ServiceStatus::Starting) | None => "starting…".into(),
                    }
                } else {
                    "off air".into()
                };
                CameraEntry {
                    uid: d.uid.as_str().into(),
                    name: d.name.as_str().into(),
                    on_air,
                    status,
                    missing: false,
                    active: clip_id.is_some_and(|id| active_clips.contains(&id)),
                    role: if clip_id.is_some() && playing_clip == clip_id {
                        ClipRole::Playing
                    } else if clip_id.is_some() && armed_clip == clip_id {
                        ClipRole::Armed
                    } else {
                        ClipRole::None
                    },
                }
            })
            .collect();
        // Camera clips whose device isn't connected get a missing-device row:
        // the project loaded anyway (their cues render black); the row offers
        // relinking onto a connected device.
        for c in &self.engine.clips {
            let ClipSource::Camera { uid, name } = &c.source else { continue };
            let enumerated = self.camera_devices.iter().any(|d| d.uid == uid.as_ref());
            let already = self.mirror.cameras.iter().any(|e| e.uid == *uid);
            if enumerated || already {
                continue;
            }
            self.mirror.cameras.push(CameraEntry {
                uid: uid.clone(),
                name: name.clone(),
                on_air: false,
                status: "missing device".into(),
                missing: true,
                active: active_clips.contains(&c.id),
                role: if playing_clip == Some(c.id) {
                    ClipRole::Playing
                } else if armed_clip == Some(c.id) {
                    ClipRole::Armed
                } else {
                    ClipRole::None
                },
            });
        }
    }
}
