//! The commands the engine hands back: everything that needs an OS.
//!
//! [`Engine::apply_command`](vidiotic_play::engine::Engine::apply_command)
//! returns `Some(cmd)` for anything it does not implement, and this is where
//! those land. The list is short and it is exactly the port's boundary — a file
//! dialog, an audio device, a window, a camera, a sibling process, a shader
//! compile that reads from disk. Nothing here has a browser counterpart, which
//! is why none of it is in the engine.
//!
//! The final arm is load-bearing: adding a command without deciding which side
//! owns it has to fail *loudly* rather than be dropped on the floor by a
//! `_ => {}`. It panics in a debug build, where the mistake belongs, and logs in
//! a release one — an orphaned command is a development error, and taking the
//! output down mid-set is a worse answer to it than one line in a log nobody
//! is reading either.

use super::*;

/// Digits a typed tempo may hold — enough for `1000.0`, and a hard stop on a
/// leant-on number key filling the entry with garbage.
const BPM_ENTRY_MAX: usize = 5;

/// What a typed tempo is allowed to be. Matches the clock's own clamp; outside
/// it the entry is discarded rather than pinned, since a fat-fingered `14`
/// meant `140` and silently starting the set at 20 bpm is worse than nothing.
const BPM_ENTRY_RANGE: std::ops::RangeInclusive<f64> = 20.0..=1000.0;

impl App {
    pub(super) fn apply_shell_command(&mut self, cmd: Command) {
        match cmd {
            Command::SetSyncSource(kind) => self.set_sync_source(kind),
            Command::LoadIsf(path) => self.load_isf(path),
            Command::CaptureShader => self.capture_shader(),
            Command::RemoveShader(id) => self.remove_shader(id),
            Command::SetClipDir(dir) => self.set_clip_dir(dir),
            Command::AddClipDirAsBank(dir) => self.add_clip_dir_as_bank(dir),
            Command::RefreshCameras => self.refresh_cameras(),
            Command::SetCameraOnAir(uid, on) => self.set_camera_on_air(&uid, on),
            Command::AddCameraCue(uid) => self.add_camera_cue(&uid),
            Command::RelinkCamera { from, to } => self.relink_camera(&from, &to),
            Command::SetShaderPath(p) => {
                self.shader_path = p;
                self.watcher = ShaderWatcher::new(&self.shader_path).ok();
                self.load_shader();
            }
            Command::SetAudioDevice(name) => self.switch_audio_device(name),
            Command::ToggleFullscreen => self.toggle_fullscreen(),
            Command::SaveProject => {
                if let Some(p) = self.project_path.clone() {
                    self.save_project_to(&p);
                } else {
                    crate::ui::pick_file(
                        self.cmd_tx.clone(),
                        crate::ui::PickKind::SaveProject(None),
                    );
                }
            }
            Command::SaveProjectAs => {
                crate::ui::pick_file(
                    self.cmd_tx.clone(),
                    crate::ui::PickKind::SaveProject(self.project_path.clone()),
                );
            }
            Command::SaveProjectTo(p) => {
                self.save_project_to(&p);
                self.project_path = Some(p);
            }
            Command::OpenProject => {
                crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::OpenProject);
            }
            // The four the panels used to open themselves. Same picker, asked
            // for by the panel rather than reached for — which is what lets
            // those panels compile for a browser (web-port.md §8 step 4g).
            Command::PickClipDir => {
                crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::ClipDir);
            }
            Command::PickClipBankDir => {
                crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::ClipBankDir);
            }
            Command::PickShader => {
                crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::Shader);
            }
            Command::PickIsf => {
                crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::Isf);
            }
            Command::LoadProject(p) => self.load_project(p),
            Command::OpenProjectEditor => self.open_project_editor(),
            Command::OpenControlMapper => {
                spawn_control_mapper(self.ipc.as_ref().map(crate::ipc::IpcEngine::socket_path));
            }
            Command::Quit => self.should_quit = true,

            // Keyboard tempo entry. Shell state because it is a keyboard
            // affordance for the numeric field the control UI already has —
            // the browser front end has a real input instead. The commit
            // re-enters through `cmd_tx` rather than setting the tempo here,
            // so it takes the same path as every other `SetBpm`.
            Command::BpmDigit(d) => {
                let entry = self.bpm_entry.get_or_insert_with(String::new);
                if entry.len() < BPM_ENTRY_MAX {
                    entry.push(char::from(b'0' + d.min(9)));
                }
            }
            Command::BpmCommit => {
                if let Some(s) = self.bpm_entry.take() {
                    if let Ok(b) = s.parse::<f64>() {
                        if BPM_ENTRY_RANGE.contains(&b) {
                            let _ = self.cmd_tx.send(Command::SetBpm(b));
                        }
                    }
                }
            }
            Command::BpmClear => self.bpm_entry = None,

            other => {
                debug_assert!(false, "no shell owner for {other:?}");
                log::error!("dropped {other:?}: no shell owner and the engine declined it");
            }
        }
    }
}
