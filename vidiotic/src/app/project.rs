//! Clip lookups, project save/load, and the editor hand-off.

use super::*;

impl App {
    /// Assemble the current session into a `Project` and write it to `path`. A
    /// failed write is logged, never fatal — losing a save must not kill the set.
    /// `SessionDefaults` are gathered from the live clock/sequencer here; the
    /// spec assembly itself lives in the shared [`crate::project`] module.
    pub(super) fn save_project_to(&mut self, path: &Path) {
        use crate::project::{self, Project, SessionDefaults, SyncSpec};
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        // The root any *relative* runtime path is relative to. `absolutize` used
        // to reach for this itself; it takes it as an argument now so the same
        // code runs in a browser, where the answer is the OPFS root instead.
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let defaults = SessionDefaults {
            bpm: self.engine.clock.snapshot().bpm,
            quantum: self.engine.time_sig.quantum(),
            phrase_len: self.engine.sequencer.phrase_len().round() as u32,
            sync: match self.engine.sync {
                SyncKind::Internal => SyncSpec::Internal,
                SyncKind::Link => SyncSpec::Link,
            },
            preserve_playhead: self.engine.preserve_playhead,
            loop_len: self.engine.loop_len,
            advanced: self.engine.advanced,
            ts_num: self.engine.time_sig.num,
            ts_den: self.engine.time_sig.den,
            phrase_cadence: Some(self.engine.phrase_cadence.into()),
            loop_cadence_set: true,
            loop_cadence: self.engine.loop_cadence.map(Into::into),
            // Absolutize like clip paths: a CWD-relative `--shader` would resolve
            // against the save dir on load and be lost.
            shader_path: Some(project::relativize(
                dir,
                &project::absolutize(&base, &self.shader_path),
            )),
        };
        let mut proj = Project::from_runtime(
            &base,
            dir,
            &self.engine.clips,
            &self.engine.clip_banks,
            &self.engine.banks,
            &self.clip_meta,
            defaults,
        );
        proj.controls = self.control_input.project_map().clone();
        match project::save(&proj, path) {
            Ok(()) => log::info!("saved project to {}", path.display()),
            Err(e) => log::error!("failed to save project to {}: {e:#}", path.display()),
        }
    }

    /// Replace the running session with a `.viproj` loaded from disk. A failed
    /// parse or missing clip files abort the load and keep the current session
    /// — mid-set, a bad load must be a no-op, not a teardown. (Relinking moved
    /// files is prep's job; the player refuses rather than guessing.)
    pub(super) fn load_project(&mut self, path: PathBuf) {
        use crate::project::{self, SyncSpec};
        let project = match project::load(&path) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to load {}: {e:#}", path.display());
                return;
            }
        };
        let dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let resolved = project::resolve(project, &dir);
        if !resolved.missing.is_empty() {
            log::error!(
                "{}: {} clip file(s) missing — relink in the project editor first",
                path.display(),
                resolved.missing.len()
            );
            return;
        }
        let assembled = project::assemble(&resolved);
        let d = &resolved.project.defaults;

        // Swap the pool and banks (the `set_clip_dir` pattern: decoders,
        // playback, selection, and thumbnails all restart).
        self.thumb_rx = Some(clippool::spawn_thumbnailer(assembled.clips.clone()));
        self.engine.replace_pool(assembled.clips, assembled.clip_banks, assembled.cue_banks);
        self.clip_meta = assembled.clip_meta;

        // Session defaults, then rebuild the sequencer over the new live bank.
        if d.bpm > 0.0 {
            self.engine.clock.set_bpm(d.bpm);
        }
        self.engine.time_sig = d.time_sig();
        self.engine.clock.set_quantum(self.engine.time_sig.quantum());
        self.engine.phrase_cadence = d.phrase_cadence();
        self.engine.loop_cadence = d.loop_cadence();
        self.engine.preserve_playhead = d.preserve_playhead;
        self.engine.advanced = d.advanced;
        self.set_sync_source(match d.sync {
            SyncSpec::Internal => SyncKind::Internal,
            SyncSpec::Link => SyncKind::Link,
        });
        self.engine.sequencer = Sequencer::new(self.engine.phrase_cadence.beats(self.engine.time_sig));
        self.engine.apply_cadences();
        let steps = self.engine.cue_steps(self.engine.live_bank);
        let ev = self.engine.sequencer.set_active_set(steps);
        self.engine.apply_seq_events(ev);
        self.engine.loop_tracker.reset();

        if let Some(shader) = assembled.shader {
            self.shader_path = shader;
            self.watcher = ShaderWatcher::new(&self.shader_path).ok();
            self.load_shader();
        }
        self.load_referenced_isf();
        self.control_input.set_project_map(assembled.controls);
        if let Some(egui) = self.egui.as_mut() {
            egui.clear_thumbnails();
        }
        log::info!("loaded project {}", path.display());
        self.project_path = Some(path);
        self.bump_epoch();
    }

    /// Advance the session generation — the whole clip/cue id space just turned
    /// over, so IPC clients holding older ids must re-query. See [`Self::epoch`].
    pub(super) fn bump_epoch(&self) {
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Save in place and launch the project editor (vidiotic-prep) on the
    /// project file. Without a path yet there is nothing to hand the editor:
    /// solicit one via the save picker instead, and re-invoke once saved.
    pub(super) fn open_project_editor(&mut self) {
        let Some(path) = self.project_path.clone() else {
            crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::SaveProject(None));
            return;
        };
        self.save_project_to(&path);
        spawn_project_editor(&path, self.ipc.as_ref().map(crate::ipc::IpcEngine::socket_path));
    }
}
