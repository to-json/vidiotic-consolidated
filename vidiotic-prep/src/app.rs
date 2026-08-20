//! Application state for the `vidiotic-prep` span editor.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use nanoserde::SerRon;

use crate::control_input::Controls;
use crate::engine::EngineLink;
use crate::export::{ExportMsg, ExportProgress};
use crate::preview::SourceMedia;
use crate::session;
use vidiotic_chop::commands::Command;
use vidiotic_chop::editor::{Editor, MediaInfo, PendingOpen, DRAIN_BUDGET};
use vidiotic_chop::mirror::PrepMirror;
use vidiotic_chop::spans::Span;

const PREVIEW_WIDTH: u32 = 960;
/// Above this size, opening a video asks for confirmation first instead of
/// loading immediately (decoding a multi-GB file can take a moment).
const LARGE_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// `path` resolved through symlinks and `..`, or unchanged when it can't be
/// (it doesn't exist yet). Only used to compare two paths for identity.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A path's final component, for status lines that would otherwise be mostly
/// directory.
fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The native shell: a marking session, plus every machine it sits on.
///
/// The panels do not see this type. They see [`Editor`] and
/// [`PrepMirror`] — everything below is what those
/// two exist to keep out of them (web-port.md §2).
pub struct PrepApp {
    /// The marking session: spans, marks, playhead, jog window, undo. Every
    /// command that acts only on those runs in here — see [`vidiotic_chop::editor`].
    pub editor: Editor,
    /// The open video's decoder. The editor holds its length and rate as
    /// [`MediaInfo`]; this is the ffmpeg that
    /// produced them, and the reason it is on this side of the line.
    pub media: Option<SourceMedia>,
    pub preview_tex: Option<egui::TextureHandle>,
    last_tex_frame: Option<u64>,
    /// RON of the last sidecar written or restored per source, for autosave
    /// dedup (each source video gets its own `.vprep`).
    last_saved_sessions: HashMap<PathBuf, String>,
    /// `Context::input.time` of the last autosave check (throttles to ~1 Hz).
    last_autosave_check: f64,

    pub export_dest: Option<PathBuf>,
    pub export_name: String,
    pub export_starter_cue_bank: bool,
    /// Bake BC1 with ClusterFit (several times slower) instead of RangeFit.
    pub export_high_quality: bool,
    export_rx: Option<Receiver<ExportMsg>>,
    pub export_progress: Option<ExportProgress>,
    pub export_result: Option<Result<PathBuf, String>>,
    /// The span list as of the last successful export, so the exit prompt only
    /// fires when something changed since then. `None` means nothing has ever
    /// been exported.
    ///
    /// The spans themselves, not a `format!("{:?}", …)` of them: a Debug string
    /// as a comparison key makes `Span`'s Debug output a serialization format
    /// that nobody knows they must not change, and a field added to `Span`
    /// without a thought about this is exactly how the prompt stops firing.
    last_export_spans: Option<Vec<Span>>,

    /// A reachable `vidiotic` engine, if one is listening.
    pub engine: Option<EngineLink>,
    /// The project an engine launched us to edit. Set only when that engine
    /// spawned us, and the trigger for handing an export straight back.
    launch_project: Option<PathBuf>,
    /// In-flight reload result from [`Self::send_to_engine`].
    engine_rx: Option<Receiver<Result<(), String>>>,

    /// Set once the user picks "quit without exporting", so the re-issued
    /// close request passes `handle_close_request`'s veto check.
    confirmed_quit: bool,

    /// Devices, binding tables and the learn session — see [`Controls`].
    pub ctl: Controls,
}

impl Default for PrepApp {
    fn default() -> Self {
        Self {
            editor: Editor::default(),
            media: None,
            preview_tex: None,
            last_tex_frame: None,
            last_saved_sessions: HashMap::new(),
            last_autosave_check: 0.0,
            export_dest: None,
            export_name: "project".to_string(),
            export_starter_cue_bank: true,
            export_high_quality: false,
            export_rx: None,
            export_progress: None,
            export_result: None,
            last_export_spans: None,
            engine: EngineLink::discover(),
            launch_project: None,
            engine_rx: None,
            confirmed_quit: false,
            ctl: Controls::default(),
        }
    }
}

impl PrepApp {
    /// Open `path` as the source video, resetting per-video transient UI
    /// state (playhead, marks, view window, preview). Spans retained from
    /// other videos this session are left untouched. If a `.vprep` sidecar
    /// exists next to the video and none of its spans are already retained
    /// in memory, the marking session it holds is merged in.
    pub fn open_video(&mut self, path: PathBuf) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        match SourceMedia::open(&path, PREVIEW_WIDTH) {
            Ok(media) => {
                let opened = format!(
                    "opened {} ({}x{}, {:.2} fps, {} frames)",
                    path.display(),
                    media.width,
                    media.height,
                    media.fps,
                    media.frames
                );
                let info = MediaInfo {
                    frames: media.frames,
                    fps: media.fps,
                    width: media.width,
                    height: media.height,
                    duration_sec: media.duration_sec,
                };
                self.preview_tex = None;
                self.last_tex_frame = None;
                self.media = Some(media);
                // The decoder stays here; the editor gets its length and rate,
                // and resets the per-video transient state around them.
                self.editor.open_media(path.clone(), info);

                let already_retained = self.editor.spans.spans.iter().any(|s| s.source == path);
                let sidecar = session::sidecar_path(&path);
                let mut restored = 0;
                if !already_retained && sidecar.exists() {
                    match session::load_sidecar(&sidecar) {
                        Ok(file) => {
                            restored = file.spans.len();
                            // Only the first video retained this session may
                            // adopt session-wide settings from its sidecar —
                            // a later one's stale copy shouldn't stomp them.
                            let adopt_globals = self.editor.spans.spans.is_empty();
                            session::merge_into(file, self, &path, adopt_globals);
                            // Spans just arrived from disk outside the command
                            // path; any prior snapshot predates them, so drop
                            // the history rather than let undo restore over them.
                            self.editor.reset_undo();
                            // Remember the normalized form so autosave doesn't
                            // immediately rewrite an identical file.
                            self.last_saved_sessions.insert(
                                path.clone(),
                                session::capture(self, &path).serialize_ron(),
                            );
                        }
                        Err(e) => log::warn!("ignoring sidecar {}: {e:#}", sidecar.display()),
                    }
                }
                self.editor.set_status(if restored > 0 {
                    format!("{opened} — restored {restored} span(s)")
                } else {
                    opened
                });
            }
            Err(e) => {
                self.editor
                    .set_error(format!("failed to open {}: {e:#}", path.display()));
            }
        }
    }

    /// Open `path`: `.viproj` resumes a project for retrimming, anything
    /// else opens as a source video (size-gated).
    pub fn request_open(&mut self, path: PathBuf) {
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("viproj"))
        {
            self.open_project(path);
        } else {
            self.editor.post(Command::OpenVideo {
                path,
                then: Vec::new(),
            });
        }
    }

    /// Open `path` as a video unless it's large enough to warrant confirming
    /// first (or it's already the open video, in which case this just runs
    /// `then` — re-selecting a span on the current video shouldn't reset
    /// playback state).
    fn open_video_gated(&mut self, path: PathBuf, then: Vec<Command>) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        if self.editor.source_path.as_deref() == Some(path.as_path()) {
            self.editor.resume(then);
            return;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > LARGE_FILE_BYTES {
            self.editor.pending_open = Some(PendingOpen {
                path,
                size_bytes: size,
                then,
            });
        } else {
            self.open_video_then(path, then);
        }
    }

    /// Open `path`, and queue `then` only if it actually loaded: the
    /// `source_path` check after `open_video` is the entire guard against
    /// running `then` on a failed open.
    fn open_video_then(&mut self, path: PathBuf, then: Vec<Command>) {
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.open_video(path.clone());
        if self.editor.source_path.as_deref() == Some(path.as_path()) {
            self.editor.resume(then);
        }
    }

    /// Reopen an exported `.viproj`: open its source video (size-gated) and
    /// reconstruct spans/banks/defaults from the project's span provenance.
    pub fn open_project(&mut self, path: PathBuf) {
        let mut re = match session::reopen_project(&path) {
            Ok(re) => re,
            Err(e) => {
                self.editor
                    .set_error(format!("reopen {}: {e:#}", path.display()));
                return;
            }
        };
        if !re.source.exists() {
            let picked = rfd::FileDialog::new()
                .set_title(format!("locate moved source ({})", re.source.display()))
                .pick_file();
            match picked {
                Some(p) => re.source = p,
                None => {
                    self.editor
                        .set_error(format!("source video not found: {}", re.source.display()));
                    return;
                }
            }
        }
        let source = re.source.clone();
        self.editor.post(Command::OpenVideo {
            path: source,
            then: vec![Command::FinishOpenProject(Box::new(re))],
        });
    }

    /// Runs only once its source video is open — `OpenVideo` drops the
    /// continuation if the open failed or was cancelled, so this no longer
    /// needs to re-check. The document half is
    /// [`Editor::adopt_project`](vidiotic_chop::editor::Editor::adopt_project),
    /// shared with the browser shell; what is added here is native.
    fn finish_open_project(&mut self, re: vidiotic_chop::editor::ReopenedProject) {
        self.editor.adopt_project(&re);
        self.editor.clip_dir = re.source.parent().map(Path::to_path_buf);
        self.ctl.project = re.controls;
        self.export_name = re.project_name;
        self.export_dest = Some(re.project_dir);
        self.last_saved_sessions.remove(&re.source); // let autosave persist this state fresh
    }

    /// Write the `.vprep` sidecar if the session changed. Throttled to ~1 Hz,
    /// so calling every frame is fine.
    pub fn autosave_session(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.last_autosave_check < 1.0 {
            return;
        }
        self.last_autosave_check = now;
        self.flush_session();
        self.flush_prep_map();
    }

    /// Write each source's `.vprep` sidecar immediately if it differs from
    /// the last write. Also the quit path, so nothing marked in the final
    /// second is lost. Covers every source with spans *and* every source
    /// previously autosaved this session — a video whose last span was just
    /// deleted still needs its sidecar rewritten to empty, or the stale file
    /// would resurrect those spans on a later reopen.
    pub fn flush_session(&mut self) {
        let mut sources: std::collections::BTreeSet<PathBuf> = self
            .editor
            .spans
            .spans
            .iter()
            .map(|s| s.source.clone())
            .collect();
        sources.extend(self.last_saved_sessions.keys().cloned());
        for src in sources {
            let ron = session::capture(self, &src).serialize_ron();
            if self.last_saved_sessions.get(&src) != Some(&ron) {
                let path = session::sidecar_path(&src);
                match std::fs::write(&path, &ron) {
                    Ok(()) => {
                        self.last_saved_sessions.insert(src.clone(), ron);
                    }
                    Err(e) => log::warn!("autosave {}: {e}", path.display()),
                }
            }
        }
    }

    /// Run every queued command, including any posted while draining.
    ///
    /// Safe to call more than once per frame. Bounded by [`DRAIN_BUDGET`] so a
    /// command that re-posts itself degrades to a dropped queue and a warning
    /// rather than a hung frame.
    pub fn drain_commands(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        // Anything the timeline's mid-frame drain met and could not run itself,
        // in the order it was met — before this frame's own queue.
        for cmd in self.editor.take_deferred() {
            self.apply_shell_command(cmd, ctx);
        }
        for _ in 0..DRAIN_BUDGET {
            let Some(cmd) = self.editor.pop() else {
                // The editor asks for a repaint by setting a flag, because it
                // has no window to ask. This is where that gets honoured.
                if std::mem::take(&mut self.editor.repaint) {
                    ctx.request_repaint();
                }
                return;
            };
            if let Some(rest) = self.editor.step(cmd, now) {
                self.apply_shell_command(rest, ctx);
            }
        }
        log::warn!(
            "command drain hit its budget of {DRAIN_BUDGET}; dropping {} queued",
            self.editor.queued_len()
        );
        self.editor.clear_queue();
    }

    /// The commands the editor hands back: everything that needs an OS.
    ///
    /// [`Editor::step`] returns `Some(cmd)` for anything it does not implement,
    /// and this is where those land. The list is short and it is exactly this
    /// front end's boundary — a file to stat, a decoder to open, a bake thread,
    /// a window to close. Nothing here has a browser counterpart yet, which is
    /// why none of it is in the editor (web-port.md §2).
    fn apply_shell_command(&mut self, cmd: Command, ctx: &egui::Context) {
        match cmd {
            Command::Open(path) => self.request_open(path),
            Command::OpenVideo { path, then } => self.open_video_gated(path, then),
            Command::ConfirmPendingOpen => {
                if let Some(pending) = self.editor.pending_open.take() {
                    self.open_video_then(pending.path, pending.then);
                }
            }
            Command::FinishOpenProject(re) => self.finish_open_project(*re),
            Command::PickVideo => self.pick_video(),
            Command::PickProject => self.pick_project(),
            Command::PickShaderPath => self.pick_shader_path(),
            Command::StartExport => self.start_export(),
            Command::ConfirmQuit => self.confirm_quit(ctx),

            // The editor only returns the arms above. Anything else arriving
            // here is a command that was added without deciding which side owns
            // it, and dropping it silently is how a verb comes to resolve and
            // then do nothing.
            other => {
                debug_assert!(false, "no shell owner for {other:?}");
                log::error!("dropped {other:?}: no shell owner and the editor declined it");
            }
        }
    }

    /// Raise the "open video…" chooser, seeded with the current project's
    /// footage folder when there is one.
    fn pick_video(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("video", &["mov", "mp4", "mkv", "m4v", "avi", "webm"]);
        // When a project is loaded, start in the folder its clips were cut
        // from rather than wherever the picker last landed.
        if let Some(dir) = &self.editor.clip_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.editor.post(Command::Open(path));
        }
    }

    fn pick_project(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("vidiotic project", &["viproj"])
            .pick_file()
        {
            self.editor.post(Command::Open(path));
        }
    }

    /// Raise the session-default shader chooser and apply the pick. The panel
    /// asked for a file, not for a `SetDefaults` it would have to assemble
    /// around a path it never sees.
    fn pick_shader_path(&mut self) {
        let Some(path) = rfd::FileDialog::new().pick_file() else {
            return;
        };
        let mut d = self.editor.defaults.clone();
        d.shader_path = Some(path.to_string_lossy().into_owned());
        self.editor.post(Command::SetDefaults(Box::new(d)));
    }

    /// This frame's read-only overlay for the panels: what a machine knows.
    fn build_mirror(&self) -> PrepMirror {
        PrepMirror {
            preview: self.preview_tex.clone(),
            exporting: self.exporting(),
        }
    }

    /// Decode the current frame (if it changed) and refresh the preview texture.
    pub fn update_preview_texture(&mut self, ctx: &egui::Context) {
        let Some(media) = self.media.as_mut() else {
            return;
        };
        if self.last_tex_frame == Some(self.editor.cur_frame) {
            return;
        }
        match media.frame_at(self.editor.cur_frame) {
            Ok(frame) => {
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [frame.w as usize, frame.h as usize],
                    &frame.rgba,
                );
                match &mut self.preview_tex {
                    Some(handle) => handle.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        self.preview_tex =
                            Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR));
                    }
                }
                self.last_tex_frame = Some(self.editor.cur_frame);
            }
            Err(e) => {
                self.editor
                    .set_error(format!("decode frame {}: {e:#}", self.editor.cur_frame));
            }
        }
    }

    /// Start a background export and begin polling for progress messages.
    /// Doesn't require a video to currently be open — each span is re-read
    /// from its own `source` path by the export worker.
    pub fn start_export(&mut self) {
        let Some(dest) = self.export_dest.clone() else {
            self.editor
                .set_error("pick a destination folder first".to_string());
            return;
        };
        if self.editor.spans.spans.is_empty() {
            self.editor.set_error("no spans to export".to_string());
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        self.export_rx = Some(rx);
        self.export_progress = Some(ExportProgress {
            total: self.editor.spans.spans.len(),
            ..ExportProgress::default()
        });
        self.export_result = None;
        crate::export::spawn_export(
            self.editor.spans.spans.clone(),
            self.editor.bank_names.clone(),
            self.editor.defaults.clone(),
            self.ctl.project.clone(),
            dest,
            self.export_name.clone(),
            self.export_starter_cue_bank,
            if self.export_high_quality {
                vidiotic_bake::transcode::BakeQuality::High
            } else {
                vidiotic_bake::transcode::BakeQuality::Draft
            },
            tx,
        );
    }

    /// Drain any pending export messages. Call every frame while an export is
    /// in flight; requests a repaint so progress updates even without input.
    pub fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.export_rx else { return };
        let mut finished = false;
        // Deferred past the loop: `rx` borrows self for the drain.
        let mut hand_back = None;
        loop {
            match rx.try_recv() {
                Ok(ExportMsg::Progress(p)) => self.export_progress = Some(p),
                Ok(ExportMsg::Done(path)) => {
                    self.export_result = Some(Ok(path.clone()));
                    self.last_export_spans = Some(self.editor.spans.spans.clone());
                    finished = true;
                    if self.is_launch_project(&path) {
                        hand_back = Some(path);
                    }
                }
                Ok(ExportMsg::Error(e)) => {
                    self.export_result = Some(Err(e));
                    finished = true;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                // Sender gone. Either the worker already said its piece (in
                // which case `export_result` is set and this is just the drain
                // finding the tail) or it died without one — a panic, or a
                // thread that never started. Treating a disconnect as "keep
                // waiting" is what wedged the app: `export_rx` never cleared,
                // `exporting()` stayed true, and quit was vetoed forever.
                // `poll_engine` below already handles it this way.
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    if self.export_result.is_none() {
                        self.export_result = Some(Err("export worker died".to_string()));
                    }
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.export_rx = None;
        } else {
            ctx.request_repaint();
        }
        if let Some(path) = hand_back {
            self.send_to_engine(path);
        }
    }

    /// Whether an export is currently in flight (a background thread is
    /// writing on the other end of `export_rx`).
    pub fn exporting(&self) -> bool {
        self.export_rx.is_some()
    }

    /// Record `path` as the project we were launched on, closing the loop the
    /// engine opened when it spawned us. Ignored unless an engine really did
    /// spawn us with a project — a video argument, or a prep the user started
    /// themselves, leaves nothing to hand back.
    pub fn note_launch_project(&mut self, path: PathBuf) {
        let launched = self.engine.as_ref().is_some_and(EngineLink::launched_us);
        if launched
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("viproj"))
        {
            self.launch_project = Some(canonical(&path));
        }
    }

    /// Whether `path` is the very project an engine launched us to edit.
    fn is_launch_project(&self, path: &Path) -> bool {
        self.launch_project.as_deref() == Some(canonical(path).as_path())
    }

    /// Hand `project` to the engine, replacing whatever it currently has
    /// loaded. Fires unprompted only for the launch project (an engine asking
    /// for its own project back); every other path is a deliberate user action,
    /// since a reload turns over the engine's whole clip/cue id space mid-set.
    pub fn send_to_engine(&mut self, project: PathBuf) {
        let Some(engine) = &self.engine else {
            self.editor
                .set_error("no running vidiotic to send to".to_string());
            return;
        };
        self.engine_rx = Some(engine.reload(&project));
        self.editor
            .set_status(format!("sending {} to vidiotic…", file_label(&project)));
    }

    /// Whether a send is still waiting on the engine's ack.
    pub fn sending_to_engine(&self) -> bool {
        self.engine_rx.is_some()
    }

    /// Drain the outcome of an in-flight [`Self::send_to_engine`]. Call every
    /// frame; a dropped sender (the worker thread never started) reads as a
    /// failure rather than hanging the indicator forever.
    pub fn poll_engine(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.engine_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(crossbeam_channel::TryRecvError::Empty) => {
                ctx.request_repaint();
                return;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Err("reload worker died".to_string())
            }
        };
        self.engine_rx = None;
        match outcome {
            Ok(()) => self
                .editor
                .set_status("vidiotic reloaded the project".to_string()),
            Err(e) => self.editor.set_error(format!("send to vidiotic: {e}")),
        }
    }

    /// Whether the span list has changed since the last successful export
    /// (or nothing has ever been exported).
    fn spans_dirty_since_export(&self) -> bool {
        self.last_export_spans.as_ref() != Some(&self.editor.spans.spans)
    }

    /// Veto the OS close request and show the quit-confirmation dialog if
    /// there are unexported spans, or veto (with a status message, no
    /// dialog) if an export is currently running. Call at the top of `ui`
    /// every frame; a no-op once the user has confirmed quitting.
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if self.confirmed_quit || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        if self.exporting() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.editor
                .set_error("export in progress — please wait".to_string());
        } else if !self.editor.spans.spans.is_empty() && self.spans_dirty_since_export() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.editor.show_quit_dialog = true;
        }
    }

    /// Confirm the "quit without exporting" choice and re-issue the close so
    /// it goes through this time.
    pub(crate) fn confirm_quit(&mut self, ctx: &egui::Context) {
        self.confirmed_quit = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Poll MIDI/gamepad devices, rescan on a timer, offer this window's own
    /// key events into the same pipeline, and resolve everything into
    /// commands. Call once per frame, next to `shell::begin_frame`.
    pub fn pump_controls(&mut self, ctx: &egui::Context) {
        self.ctl.poll_devices();
        // Keys only when no text field is capturing them, so typing a span
        // name doesn't scrub. MIDI and gamepads aren't gated — a pad press
        // still fires while a name is being typed.
        if !ctx.egui_wants_keyboard_input() {
            for (ev, repeat) in vidiotic_ctl::egui_keys::key_events(ctx) {
                if let Some(cmd) = self.ctl.observe(ev, repeat) {
                    self.editor.post(cmd);
                }
            }
        }
        for ev in self.ctl.drain_device_events() {
            if let Some(cmd) = self.ctl.observe(ev, false) {
                self.editor.post(cmd);
            }
        }
    }

    /// Persist prep's own bindings if they changed, surfacing a write failure
    /// on the status line — the editor owns that, so it can't live in
    /// [`Controls`] with the rest.
    pub fn flush_prep_map(&mut self) {
        if let Err(e) = self.ctl.flush_prep_map() {
            self.editor
                .set_error(format!("saving prep key bindings: {e:#}"));
        }
    }
}

impl eframe::App for PrepApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_close_request(&ctx);
        // Re-derive the palette/style if the toolbar's theme controls changed.
        phosphor::shell::begin_frame(&ctx);
        self.pump_controls(&ctx);
        if self.exporting() {
            self.poll_export(&ctx);
        }
        if self.sending_to_engine() {
            self.poll_engine(&ctx);
        }
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()))
        {
            self.editor.post(Command::Open(path));
        }
        self.editor
            .advance_playback(ctx.input(|i| i.stable_dt) as f64);
        self.update_preview_texture(&ctx);
        self.autosave_session(&ctx);

        // `editor` and `ctl` are borrowed disjointly: the panels take one, and
        // the hook they call for the binding tables takes the other. That is
        // the whole reason the control-mapping state is a struct rather than a
        // dozen fields here — see [`Controls`].
        let mirror = self.build_mirror();
        let Self { editor, ctl, .. } = self;
        vidiotic_chop::ui::draw(editor, &mirror, ui, &mut |ui| {
            crate::shell_ui::control_sections(ctl, ui);
        });
        if self.editor.show_export_dialog {
            crate::shell_ui::export_dialog(self, &ctx);
        }

        // After draw: panels post commands as they're interacted with, and a
        // command that mutates state the same panel already read would tear
        // this frame's layout. `timeline` is the exception — it drains
        // mid-widget so its drags don't paint a frame behind.
        self.drain_commands(&ctx);
    }

    fn on_exit(&mut self) {
        self.flush_session();
        self.flush_prep_map();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidiotic_core::project::SessionDefaults;

    /// `egui::Context::default()` needs no window, so the executor is testable
    /// headless — same trick `session`'s tests use on `PrepApp::default()`.
    fn ctx() -> egui::Context {
        egui::Context::default()
    }

    #[test]
    fn drain_empties_the_queue() {
        let mut app = PrepApp::default();
        app.editor.post(Command::SetIn);
        app.editor.post(Command::SetOut);
        app.drain_commands(&ctx());
        assert!(app.editor.queued_len() == 0, "drain must empty the queue");
    }

    fn span(name: &str) -> vidiotic_chop::spans::Span {
        vidiotic_chop::spans::Span {
            name: name.to_string(),
            in_frame: 0,
            out_frame: 1,
            bpm: None,
            clip_bank: 0,
            source: PathBuf::from("/tmp/x.mov"),
            crop: None,
        }
    }

    /// Loading spans from disk (a reopened project) drops stale history, so an
    /// undo can't restore over a freshly loaded document.
    #[test]
    fn reopening_a_project_clears_undo_history() {
        let mut app = PrepApp::default();
        app.editor.spans.spans = vec![span("old")];
        app.editor.post(Command::RemoveSpan(0));
        app.drain_commands(&ctx());
        assert!(app.editor.spans.spans.is_empty());

        app.finish_open_project(vidiotic_chop::editor::ReopenedProject {
            source: PathBuf::from("/tmp/reopened.mov"),
            spans: vec![span("loaded")],
            bank_names: vec!["clips".to_string()],
            defaults: SessionDefaults::default(),
            controls: Default::default(),
            project_name: "p".to_string(),
            project_dir: PathBuf::from("/tmp"),
        });

        app.editor.post(Command::Undo);
        app.drain_commands(&ctx());
        assert_eq!(
            app.editor.spans.spans.len(),
            1,
            "undo must not resurrect pre-load state"
        );
        assert_eq!(app.editor.spans.spans[0].name, "loaded");
    }

    /// A failed open must not run its continuation: `OpenVideo` is where that
    /// guard lives now (see `open_video_then`), and every continuation
    /// depends on it holding.
    #[test]
    fn a_failed_open_does_not_run_its_continuation() {
        let mut app = PrepApp::default();
        app.editor.post(Command::OpenVideo {
            path: PathBuf::from("/nonexistent/nope.mov"),
            then: vec![Command::TogglePlay],
        });
        app.drain_commands(&ctx());
        assert!(
            app.editor.source_path.is_none(),
            "the open must have failed"
        );
        assert!(!app.editor.playing(), "the continuation must not have run");
        assert!(app.editor.status_is_error);
    }

    /// Cancelling the large-file dialog drops the parked continuation with it.
    #[test]
    fn cancelling_a_pending_open_drops_its_continuation() {
        let mut app = PrepApp::default();
        app.editor.pending_open = Some(PendingOpen {
            path: PathBuf::from("/nonexistent/big.mov"),
            size_bytes: LARGE_FILE_BYTES + 1,
            then: vec![Command::TogglePlay],
        });
        app.editor.post(Command::CancelPendingOpen);
        app.drain_commands(&ctx());
        assert!(app.editor.pending_open.is_none());
        assert!(
            !app.editor.playing(),
            "the dropped continuation must not have run"
        );
    }

    /// Re-selecting a span on the already-open video runs its continuation
    /// straight away rather than reopening and resetting playback state.
    #[test]
    fn opening_the_already_open_video_runs_the_continuation_without_reopening() {
        let mut app = PrepApp::default();
        // Pretend this video is open; `open_video` is never reached because
        // the path matches, so no real file is needed.
        let path = PathBuf::from("/tmp/already-open.mov");
        app.editor.source_path = Some(path.clone());
        app.editor.cur_frame = 42;
        app.editor.post(Command::OpenVideo {
            path,
            then: vec![Command::TogglePlay],
        });
        app.drain_commands(&ctx());
        assert!(app.editor.playing(), "the continuation must have run");
        assert_eq!(app.editor.cur_frame, 42, "playback state must be untouched");
    }

    #[test]
    fn drain_caps_a_runaway_repost() {
        let mut app = PrepApp::default();
        for _ in 0..(DRAIN_BUDGET * 2) {
            app.editor.post(Command::Pause);
        }
        app.drain_commands(&ctx());
        // Over budget: the rest is dropped rather than wedging the frame.
        assert!(app.editor.queued_len() == 0);
    }
}
