//! The marking session itself: spans, marks, the jog window, the playhead —
//! and the half of `apply_command` that needs nothing but them.
//!
//! # Why this is a separate type
//!
//! `PrepApp` was the whole application: the document *and* an ffmpeg decoder, a
//! `.vprep` sidecar, `rfd` dialogs, a MIDI hub, a unix socket to a running
//! player. The panels took `&mut PrepApp` and read it directly, which meant the
//! editing model could not be compiled — or tested — without all of that
//! (web-port.md §2).
//!
//! So this is the same split `vidiotic` made in §8 step 4d, made for the same
//! reason and with the same seam: [`Editor`] owns what a marking session *is*,
//! [`Editor::step`] runs every command that acts only on it, and anything
//! needing an OS comes back out as a `Some(Command)` for the shell to answer.
//! The shell keeps the decoder, the dialogs, the sockets and the export thread.
//!
//! Two things make that boundary hold rather than merely describe it:
//!
//! - **The frame count and rate come in as data.** [`MediaInfo`] is what the
//!   editor knows about the open video, and it is a handful of numbers the
//!   shell probed once. The decoder that produced them stays there, which is
//!   why seeking, looping and zooming are all reachable here — they were never
//!   about decoding.
//! - **Time comes in as a parameter.** `ctx.input(|i| i.time)` and `stable_dt`
//!   are how the old code asked what time it was, which put an
//!   `egui::Context` — a live window — inside undo coalescing and playback.
//!   Both now take an `f64`.
//!
//! The panels read this type directly and post into its queue, which is why
//! prep's [`PrepMirror`](crate::mirror::PrepMirror) is two fields where the
//! player's is a page: once the session is portable there is nothing left for a
//! mirror to hide from a panel that reads it.

use std::collections::VecDeque;
use std::path::PathBuf;

use vidiotic_core::project::SessionDefaults;
use web_time::Instant;

use crate::commands::Command;
use crate::spans::SpanList;

/// Cap on commands run in one drain, so a command that re-posts itself can't
/// wedge the frame.
pub const DRAIN_BUDGET: usize = 256;

/// What the editor knows about the open video: its shape, length and rate.
///
/// Deliberately not the decoder. Every frame calculation in a marking session —
/// clamping a seek, looping between marks, fitting the jog window — needs
/// `frames` and `fps` and nothing else; the rest is what the toolbar prints.
/// All of it is numbers the shell probed once at open, so this is the whole of
/// what it has to tell the editor when a video lands.
#[derive(Clone, Copy, Debug)]
pub struct MediaInfo {
    pub frames: u64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub duration_sec: f64,
}

/// A `.viproj` reduced to what prep needs to resume editing it: the single
/// source video all clips were cut from, plus reconstructed spans.
///
/// Lives here rather than in `session`, which is where it is *read*. It is
/// plain data — paths, spans, names, a control map — and putting it beside the
/// loader tied `Command::FinishOpenProject` to a module that does RON parsing
/// off a disk, which is enough to make the whole command vocabulary
/// unbuildable for wasm32. That is not something inspection catches: nothing
/// in `commands.rs` names a filesystem, it just names a type that does.
#[derive(Debug)]
pub struct ReopenedProject {
    pub source: PathBuf,
    pub spans: Vec<crate::spans::Span>,
    pub bank_names: Vec<String>,
    pub defaults: SessionDefaults,
    pub project_name: String,
    pub project_dir: PathBuf,
    pub controls: vidiotic_ctl::ControlMap,
}

/// A video open waiting on the large-file confirmation dialog, holding the
/// commands to run once it lands.
pub struct PendingOpen {
    pub path: PathBuf,
    pub size_bytes: u64,
    /// The continuation, public because only a shell can run it: nothing here
    /// can open a video, so the shell takes this whole struct on
    /// `ConfirmPendingOpen` and hands `then` back through [`Editor::resume`]
    /// once the file is actually loaded.
    pub then: Vec<Command>,
}

/// A marking session: the document, the playhead, and the jog window.
pub struct Editor {
    /// Commands posted this frame, drained by the shell. Panels hold
    /// `&mut Editor`, so this is a deferred-mutation queue rather than a
    /// channel — see [`crate::commands`].
    queued: VecDeque<Command>,
    /// Shell commands met during a mid-frame [`Self::drain_ui`], parked for the
    /// end-of-frame drain. A panel is mid-layout when it calls that; opening a
    /// decoder or closing the window from inside one is exactly the tear the
    /// drain ordering exists to avoid.
    deferred: VecDeque<Command>,
    /// Length and rate of the open video, or `None` when nothing is open.
    pub media: Option<MediaInfo>,
    pub source_path: Option<PathBuf>,
    /// Directory the current project's clips were cut from — the parent of its
    /// source video. Seeds the "open video…" dialog so reopening a project
    /// points it straight at the footage folder. `None` until a project loads.
    pub clip_dir: Option<PathBuf>,
    pub spans: SpanList,
    /// Editable clip-bank names; spans reference these by index.
    pub bank_names: Vec<String>,
    pub cur_frame: u64,
    /// Pending in/out marks used to seed a new span (set via "Set In"/"Set Out").
    pub pending_in: u64,
    pub pending_out: u64,
    /// Playback speed multiplier: 0.0 = paused, negative = reverse. While
    /// nonzero, `cur_frame` advances at `fps * |speed|` and loops within the
    /// pending in/out marks.
    pub play_speed: f64,
    /// Fractional-frame carry so playback tracks wall-clock time, not frame rate.
    play_accum: f64,
    /// First frame of the visible jog window (the slider's low end).
    pub view_start: u64,
    /// Length of the visible jog window in frames (the slider's span).
    pub view_len: u64,
    /// Beat count for the "snap out to N beats" helper (sidecar-persisted).
    pub snap_beats: f64,
    /// Large-file open waiting on confirmation before it actually loads.
    pub pending_open: Option<PendingOpen>,
    /// Whether the export window is up. Which *backend* an export runs on is a
    /// shell question (§3); whether a dialog is open is not, and keeping both
    /// flags here is what lets the dialogs that raise and dismiss each other be
    /// panels rather than shell code.
    pub show_export_dialog: bool,
    /// Whether the unexported-spans prompt is up. Raised by the shell (only it
    /// sees an OS close request), dismissed by the panel.
    pub show_quit_dialog: bool,
    pub defaults: SessionDefaults,
    pub status: Option<String>,
    pub status_is_error: bool,
    /// When the status was set; non-error status fades after a few seconds.
    pub status_at: Option<Instant>,
    /// Set when something changed that the window must be redrawn to show.
    /// The shell clears it — an editor cannot request a repaint because it does
    /// not know it is in a window.
    pub repaint: bool,
    /// Session-scoped document undo/redo — snapshots taken at the command
    /// choke point. See [`crate::undo`].
    undo: crate::undo::UndoStack,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            queued: VecDeque::new(),
            deferred: VecDeque::new(),
            media: None,
            source_path: None,
            clip_dir: None,
            spans: SpanList::default(),
            bank_names: vec!["clips".to_string()],
            cur_frame: 0,
            pending_in: 0,
            pending_out: 0,
            play_speed: 0.0,
            play_accum: 0.0,
            view_start: 0,
            view_len: 1,
            snap_beats: 4.0,
            pending_open: None,
            show_export_dialog: false,
            show_quit_dialog: false,
            defaults: SessionDefaults {
                bpm: 120.0,
                quantum: 4.0,
                phrase_len: 16,
                ..Default::default()
            },
            status: None,
            status_is_error: false,
            status_at: None,
            repaint: false,
            undo: crate::undo::UndoStack::default(),
        }
    }
}

impl Editor {
    /// Queue `cmd` for this frame's drain.
    pub fn post(&mut self, cmd: Command) {
        self.queued.push_back(cmd);
    }

    fn post_all(&mut self, cmds: Vec<Command>) {
        for c in cmds {
            self.post(c);
        }
    }

    /// Take the next queued command, if any.
    pub fn pop(&mut self) -> Option<Command> {
        self.queued.pop_front()
    }

    /// How many commands are still queued (for the drain-budget warning).
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// Drop every queued command (the drain budget's escape hatch).
    pub fn clear_queue(&mut self) {
        self.queued.clear();
    }

    /// Run the queue mid-frame, from inside a panel, parking anything the shell
    /// owns for the end-of-frame drain.
    ///
    /// The timeline calls this: its drags post `Seek`/`SetPendingIn`/
    /// `SetViewStart`, and painting the strip before those run leaves it a frame
    /// behind the pointer on the app's most tactile gesture. Every command a
    /// drag can post is one the editor owns outright, so this resolves them all.
    ///
    /// Anything else — a video to open, a window to close — goes to
    /// [`Self::take_deferred`] rather than running here. A panel is mid-layout
    /// at this point, and swapping the open video underneath it would tear the
    /// frame that asked.
    pub fn drain_ui(&mut self, now: f64) {
        for _ in 0..DRAIN_BUDGET {
            let Some(cmd) = self.pop() else { return };
            if let Some(rest) = self.step(cmd, now) {
                self.deferred.push_back(rest);
            }
        }
        log::warn!("mid-frame drain hit its budget of {DRAIN_BUDGET}; dropping the rest");
        self.clear_queue();
    }

    /// Take the shell commands parked by [`Self::drain_ui`].
    pub fn take_deferred(&mut self) -> VecDeque<Command> {
        std::mem::take(&mut self.deferred)
    }

    /// Run one command, recording undo first.
    ///
    /// Returns `Some(cmd)` for anything that needs an OS — a file to open, an
    /// export to start, a window to close. That list is short and it is exactly
    /// this front end's boundary; the shell answers it in
    /// `PrepApp::apply_shell_command`.
    ///
    /// `now` is the frame's timestamp, used for undo coalescing. It is passed
    /// in rather than read, because reading it is what tied the document to a
    /// live window.
    pub fn step(&mut self, cmd: Command, now: f64) -> Option<Command> {
        match cmd {
            Command::Undo => {
                self.undo_edit();
                None
            }
            Command::Redo => {
                self.redo_edit();
                None
            }
            other => {
                self.record_undo(&other, now);
                self.apply_command(other)
            }
        }
    }

    /// A clone of the undoable document state — see [`crate::undo::Doc`].
    fn snapshot(&self) -> crate::undo::Doc {
        crate::undo::Doc {
            spans: self.spans.spans.clone(),
            selected: self.spans.selected,
            bank_names: self.bank_names.clone(),
            defaults: self.defaults.clone(),
        }
    }

    /// Overwrite the document with a snapshot (undo/redo). Transient UI —
    /// playhead, marks, view, textures — is deliberately left as-is.
    fn restore(&mut self, doc: crate::undo::Doc) {
        self.spans.spans = doc.spans;
        self.spans.selected = doc.selected;
        self.bank_names = doc.bank_names;
        self.defaults = doc.defaults;
    }

    /// Record the pre-edit state for `cmd`, unless it isn't a document edit or
    /// it coalesces into the current step. Runs before the command applies.
    fn record_undo(&mut self, cmd: &Command, now: f64) {
        let Some(tag) = crate::undo::classify(cmd) else { return };
        if self.undo.should_push(tag, now) {
            let snapshot = self.snapshot();
            self.undo.push(snapshot, tag, now);
        } else {
            self.undo.touch(now);
        }
    }

    fn undo_edit(&mut self) {
        let current = self.snapshot();
        if let Some(prev) = self.undo.undo(current) {
            self.restore(prev);
            self.repaint = true;
        } else {
            self.set_status("nothing to undo".to_string());
        }
    }

    fn redo_edit(&mut self) {
        let current = self.snapshot();
        if let Some(next) = self.undo.redo(current) {
            self.restore(next);
            self.repaint = true;
        } else {
            self.set_status("nothing to redo".to_string());
        }
    }

    /// Throw away undo history — a reopened project replaces the document
    /// wholesale, and a snapshot from before it would undo into an unrelated
    /// document.
    pub fn reset_undo(&mut self) {
        self.undo.reset();
    }

    /// Run one command that acts only on the session.
    fn apply_command(&mut self, cmd: Command) -> Option<Command> {
        match cmd {
            Command::TogglePlay => self.toggle_play(),
            Command::Pause => self.pause(),
            Command::PlayFromIn => {
                self.seek(self.pending_in.min(self.max_frame()));
                self.play_speed = 1.0;
            }
            Command::Shuttle(dir) => self.shuttle(dir),
            Command::SetSpeed(s) => self.play_speed = s,
            Command::Seek(f) => self.seek(f),
            Command::Step(n) => {
                self.pause();
                self.seek(self.cur_frame.saturating_add_signed(n));
            }
            Command::SeekStart => {
                self.pause();
                self.seek(0);
            }
            Command::SeekEnd => {
                self.pause();
                self.seek(self.max_frame());
            }
            Command::SeekFrac(t) => {
                self.pause();
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let f = (t.clamp(0.0, 1.0) * self.max_frame() as f64).round() as u64;
                self.seek(f);
            }
            Command::JumpToIn => self.seek(self.pending_in),
            Command::JumpToOut => self.seek(self.pending_out.saturating_sub(1)),

            Command::ZoomView(factor) => self.zoom_view(factor),
            Command::ZoomViewAt(factor, anchor) => self.zoom_view_at(factor, anchor),
            Command::ZoomFit => self.reset_view(),
            Command::ZoomToMarks => self.zoom_to_marks(),
            Command::SetViewStart(start) => self.set_view_start(start),

            Command::SetIn => self.set_in_here(),
            Command::SetOut => self.set_out_here(),
            Command::SetPendingIn(f) => {
                self.pending_in = f.min(self.pending_out.saturating_sub(1));
            }
            Command::SetPendingOut(f) => {
                // `lo` is floored at `hi`: the timeline's drag arm could assume
                // `pending_in < total_frames()` because it never ran without
                // media, but a command can (nothing is open, or a bound
                // controller fired), and `clamp` panics if min > max.
                let hi = self.total_frames();
                let lo = (self.pending_in + 1).min(hi);
                self.pending_out = f.clamp(lo, hi);
            }
            Command::SnapOut => self.snap_out_to_beats(),

            Command::AddSpan => self.add_span_from_marks(),
            Command::RemoveSpan(i) => self.spans.remove(i),
            Command::MoveSpanUp(i) => self.spans.move_up(i),
            Command::MoveSpanDown(i) => self.spans.move_down(i),
            Command::UpdateSpanFromMarks(i) => {
                let (pin, pout) = (self.pending_in, self.pending_out);
                if let Some(span) = self.spans.spans.get_mut(i) {
                    span.in_frame = pin;
                    span.out_frame = pout.max(pin + 1);
                }
            }
            Command::SetSpanName(i, name) => {
                if let Some(span) = self.spans.spans.get_mut(i) {
                    span.name = name;
                }
            }
            Command::SetSpanRange { idx, in_frame, out_frame } => {
                if let Some(span) = self.spans.spans.get_mut(idx) {
                    span.in_frame = in_frame;
                    span.out_frame = out_frame.max(in_frame + 1);
                }
            }
            Command::SetSpanBpm(i, bpm) => {
                if let Some(span) = self.spans.spans.get_mut(i) {
                    span.bpm = bpm;
                }
            }
            Command::SetSpanBank(i, bank) => {
                if let Some(span) = self.spans.spans.get_mut(i) {
                    span.clip_bank = bank;
                }
            }

            Command::AddBank => {
                let n = self.bank_names.len() + 1;
                self.bank_names.push(format!("bank {n}"));
            }
            Command::RemoveBank(i) => {
                // Never remove the last one: spans index into this list, so an
                // empty `bank_names` would leave every span dangling. The UI
                // also hides the button, but a command can arrive from anywhere.
                if self.bank_names.len() > 1 && i < self.bank_names.len() {
                    self.bank_names.remove(i);
                    for span in &mut self.spans.spans {
                        if span.clip_bank > i {
                            span.clip_bank -= 1;
                        } else if span.clip_bank >= self.bank_names.len() {
                            span.clip_bank = self.bank_names.len() - 1;
                        }
                    }
                }
            }
            Command::SetBankName(i, name) => {
                if let Some(slot) = self.bank_names.get_mut(i) {
                    *slot = name;
                }
            }
            Command::SetDefaults(d) => self.defaults = *d,

            Command::SelectSpan(i) => self.open_span_then(i, vec![Command::SelectLoadedSpan(i)]),
            Command::LoadMarksFromSpan(i) => {
                self.open_span_then(i, vec![Command::LoadMarksFromLoadedSpan(i)]);
            }
            Command::AuditionSpan(i) => self.open_span_then(
                i,
                vec![Command::LoadMarksFromLoadedSpan(i), Command::SetSpeed(1.0)],
            ),
            Command::SelectLoadedSpan(i) => {
                self.spans.select(i);
                if let Some(in_frame) = self.spans.spans.get(i).map(|s| s.in_frame) {
                    self.seek(in_frame);
                }
            }
            Command::LoadMarksFromLoadedSpan(i) => self.load_marks_from_span(i),

            // Dropping `PendingOpen` drops its continuation with it.
            Command::CancelPendingOpen => self.pending_open = None,

            Command::ShowExportDialog => {
                self.show_quit_dialog = false;
                self.show_export_dialog = true;
            }

            // Everything below needs an OS: a path to canonicalize or stat, a
            // decoder to open, a bake thread, a file chooser, a window to close.
            other @ (Command::Open(_)
            | Command::OpenVideo { .. }
            | Command::ConfirmPendingOpen
            | Command::FinishOpenProject(_)
            | Command::PickVideo
            | Command::PickProject
            | Command::PickShaderPath
            | Command::StartExport
            | Command::ConfirmQuit) => return Some(other),

            // Intercepted in `step` before reaching here — they act on the undo
            // stack, not the document.
            Command::Undo | Command::Redo => {}
        }
        None
    }

    /// Ensure span `i`'s source video is open, then run `then` once it is.
    ///
    /// Posts rather than opens: the size gate and the decoder are the shell's,
    /// so this asks for the open and the continuation rides along with it.
    fn open_span_then(&mut self, i: usize, then: Vec<Command>) {
        if let Some(source) = self.spans.spans.get(i).map(|s| s.source.clone()) {
            self.post(Command::OpenVideo { path: source, then });
        }
    }

    /// Select span `i`, load its range into the pending marks, seek to its in
    /// point, and frame it in the view. Assumes its source video is open.
    fn load_marks_from_span(&mut self, i: usize) {
        let Some((inf, outf)) = self.spans.spans.get(i).map(|s| (s.in_frame, s.out_frame)) else {
            return;
        };
        self.spans.select(i);
        self.pending_in = inf;
        self.pending_out = outf.max(inf + 1);
        self.seek(inf);
        self.zoom_to_marks();
    }

    /// Run the commands a gated open was holding, now that it has landed.
    pub fn resume(&mut self, then: Vec<Command>) {
        self.post_all(then);
    }

    /// Clamp and set the current preview frame.
    pub fn seek(&mut self, frame: u64) {
        self.cur_frame = frame.min(self.max_frame());
    }

    /// Highest frame index of the source (0 if nothing loaded).
    pub fn max_frame(&self) -> u64 {
        self.media.as_ref().map_or(0, |m| m.frames.saturating_sub(1))
    }

    /// Total frame count of the source (1 if nothing loaded).
    pub fn total_frames(&self) -> u64 {
        self.media.as_ref().map_or(1, |m| m.frames.max(1))
    }

    /// Mark the in point at the current frame, pushing the out point past it
    /// if needed. `pending_out` is exclusive, matching [`crate::spans::Span`].
    pub fn set_in_here(&mut self) {
        self.pending_in = self.cur_frame;
        if self.pending_out <= self.pending_in {
            self.pending_out = (self.pending_in + 1).min(self.total_frames());
        }
    }

    /// Mark the out point *after* the current frame (the current frame is the
    /// last one included), pulling the in point back if needed.
    pub fn set_out_here(&mut self) {
        self.pending_out = (self.cur_frame + 1).min(self.total_frames());
        if self.pending_in >= self.pending_out {
            self.pending_in = self.pending_out - 1;
        }
    }

    /// Append a span from the pending marks (on the currently open video)
    /// and select it. No-op if no video is open.
    pub fn add_span_from_marks(&mut self) {
        let Some(source) = self.source_path.clone() else { return };
        self.spans.add(source, self.pending_in, self.pending_out);
    }

    /// Last frame of the visible jog window (inclusive).
    pub fn view_end(&self) -> u64 {
        (self.view_start + self.view_len.saturating_sub(1)).min(self.max_frame())
    }

    /// Whether playback is running (in either direction).
    pub fn playing(&self) -> bool {
        self.play_speed != 0.0
    }

    /// Space-bar behavior: pause if playing, else play forward at 1×.
    pub fn toggle_play(&mut self) {
        self.play_speed = if self.playing() { 0.0 } else { 1.0 };
    }

    pub fn pause(&mut self) {
        self.play_speed = 0.0;
    }

    /// J/L shuttle: a press in the current direction doubles speed (max 4×);
    /// a press the other way (or from pause) starts 1× that way.
    pub fn shuttle(&mut self, dir: f64) {
        let same = self.playing() && self.play_speed.signum() == dir.signum();
        let mag = if same { (self.play_speed.abs() * 2.0).min(4.0) } else { 1.0 };
        self.play_speed = dir.signum() * mag;
    }

    /// Advance `cur_frame` by `dt` seconds of wall clock while playing, looping
    /// within the pending in/out marks. No-op when paused or nothing is open.
    ///
    /// `dt` is clamped by the caller's frame clock rather than read from one:
    /// this is the same arithmetic in a window and in a tab.
    pub fn advance_playback(&mut self, dt: f64) {
        if !self.playing() || self.media.is_none() {
            self.play_accum = 0.0;
            return;
        }
        let fps = self.fps();
        // Clamp dt so a stalled frame (window drag, disk hitch) doesn't fling
        // the playhead forward.
        let dt = dt.min(0.1);
        self.play_accum += dt * fps * self.play_speed.abs();
        let steps = self.play_accum.floor();
        if steps >= 1.0 {
            self.play_accum -= steps;
            // Loop region: the marks. `pending_out` is exclusive.
            #[allow(clippy::cast_possible_wrap)]
            let max = self.max_frame() as i64;
            #[allow(clippy::cast_possible_wrap)]
            let start = (self.pending_in as i64).min(max);
            #[allow(clippy::cast_possible_wrap)]
            let end = (self.pending_out as i64 - 1).clamp(start, max);
            let span = end - start + 1;
            let dir = if self.play_speed < 0.0 { -1 } else { 1 };
            #[allow(clippy::cast_possible_wrap)]
            let f = self.cur_frame as i64;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let f = if f < start || f > end {
                // Playhead outside the loop: enter it from the near side.
                if dir > 0 { start } else { end }
            } else {
                start + (f - start + dir * steps as i64).rem_euclid(span)
            };
            #[allow(clippy::cast_sign_loss)]
            {
                self.cur_frame = f as u64;
            }
            // Page the view so the playhead stays visible while zoomed.
            if self.cur_frame < self.view_start || self.cur_frame > self.view_end() {
                let half = self.view_len / 2;
                #[allow(clippy::cast_precision_loss)]
                self.set_view_start(self.cur_frame.saturating_sub(half) as f64);
            }
        }
        self.repaint = true;
    }

    /// Zoom the jog window by `factor` (<1 zooms in), keeping `anchor`'s
    /// on-screen position fixed.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn zoom_view_at(&mut self, factor: f64, anchor: u64) {
        let total = self.total_frames();
        let old_len = self.view_len.max(1);
        let new_len = ((old_len as f64 * factor).round() as u64).clamp(4.min(total), total);
        let frac = (anchor.saturating_sub(self.view_start) as f64 / old_len as f64).clamp(0.0, 1.0);
        let start = anchor as f64 - frac * new_len as f64;
        self.view_len = new_len;
        self.set_view_start(start);
    }

    /// Zoom the jog window by `factor` (<1 zooms in), anchored on `cur_frame`.
    pub fn zoom_view(&mut self, factor: f64) {
        self.zoom_view_at(factor, self.cur_frame);
    }

    /// Set the view window's first frame (fractional input from pixel math is
    /// fine), clamped so the window stays within the source.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn set_view_start(&mut self, start: f64) {
        let max_start = self.total_frames().saturating_sub(self.view_len);
        self.view_start = (start.round().max(0.0) as u64).min(max_start);
    }

    /// Fit the jog window to the whole source.
    pub fn reset_view(&mut self) {
        self.view_start = 0;
        self.view_len = self.total_frames();
    }

    /// Zoom the jog window to the current in/out marks, with a little padding.
    pub fn zoom_to_marks(&mut self) {
        let total = self.total_frames();
        // `pending_out` is exclusive; frame the last included frame.
        let lo = self.pending_in;
        let hi = self.pending_out.saturating_sub(1).max(lo);
        let pad = ((hi - lo) / 8).max(2);
        let start = lo.saturating_sub(pad);
        let end = (hi + pad).min(total.saturating_sub(1));
        self.view_start = start;
        self.view_len = (end - start + 1).max(4).min(total);
    }

    /// Current source frame rate, or a sane fallback if nothing is loaded.
    pub fn fps(&self) -> f64 {
        self.media.as_ref().map_or(30.0, |m| m.fps)
    }

    /// Duration of `n` frames in beats at `bpm`, falling back to the session
    /// default bpm.
    #[allow(clippy::cast_precision_loss)]
    pub fn beats(&self, n: u64, bpm: Option<f64>) -> f64 {
        let bpm = bpm.unwrap_or(self.defaults.bpm).max(1.0);
        n as f64 / self.fps() * bpm / 60.0
    }

    /// Move the out mark to `snap_beats` beats after the in mark, at the
    /// session bpm.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn snap_out_to_beats(&mut self) {
        let frames_len =
            (self.snap_beats * 60.0 / self.defaults.bpm.max(1.0) * self.fps()).round().max(1.0);
        self.pending_out = (self.pending_in + frames_len as u64).min(self.total_frames());
    }

    /// Reset the per-video transient state a newly opened source invalidates.
    /// Spans retained from other videos are deliberately untouched.
    pub fn open_media(&mut self, path: PathBuf, info: MediaInfo) {
        self.media = Some(info);
        self.source_path = Some(path);
        self.cur_frame = 0;
        self.play_speed = 0.0;
        self.play_accum = 0.0;
        self.pending_in = 0;
        self.pending_out = info.frames.max(1);
        self.reset_view();
    }

    /// Replace this source's spans, banks and defaults with a reopened
    /// project's, leaving spans from other retained videos alone.
    ///
    /// The editor half of answering `FinishOpenProject`. Both shells do exactly
    /// this and then add their own: prep also fills the export destination from
    /// the project's folder, which a browser has no equivalent for.
    pub fn adopt_project(&mut self, re: &ReopenedProject) {
        // Project state wins over anything retained or sidecar-restored for
        // this one source; spans from other retained videos are untouched.
        self.spans.spans.retain(|s| s.source != re.source);
        self.spans.spans.extend(re.spans.iter().cloned());
        self.spans.selected = None;
        // A reopened project replaces the document wholesale; a snapshot from
        // before it would undo into an unrelated document.
        self.reset_undo();
        self.bank_names.clone_from(&re.bank_names);
        self.defaults = re.defaults.clone();
        self.set_status(format!(
            "reopened {} ({} span(s))",
            re.source.display(),
            self.spans.spans.len()
        ));
    }

    pub fn set_error(&mut self, msg: String) {
        log::error!("{msg}");
        self.status = Some(msg);
        self.status_is_error = true;
        self.status_at = Some(Instant::now());
    }

    /// Set a non-error status line (fades out after a few seconds).
    pub fn set_status(&mut self, msg: String) {
        self.status = Some(msg);
        self.status_is_error = false;
        self.status_at = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// An editor with `frames` frames of 30fps media open.
    fn loaded(frames: u64) -> Editor {
        let mut ed = Editor::default();
        ed.open_media(
            PathBuf::from("/v.mov"),
            MediaInfo {
                frames,
                fps: 30.0,
                width: 1920,
                height: 1080,
                duration_sec: frames as f64 / 30.0,
            },
        );
        ed
    }

    fn span(name: &str) -> crate::spans::Span {
        crate::spans::Span {
            name: name.to_string(),
            in_frame: 0,
            out_frame: 10,
            bpm: None,
            clip_bank: 0,
            source: PathBuf::from("/v.mov"),
        }
    }

    #[test]
    fn seek_clamps_to_the_last_frame() {
        let mut ed = loaded(100);
        ed.seek(999);
        assert_eq!(ed.cur_frame, 99);
    }

    #[test]
    fn step_saturates_rather_than_wrapping_past_zero() {
        let mut ed = loaded(100);
        ed.step(Command::Step(-5), 0.0);
        assert_eq!(ed.cur_frame, 0);
    }

    // The whole point of the split: a command that needs a decoder, a dialog or
    // a socket comes back out rather than being silently dropped, and one that
    // does not never leaves.
    #[test]
    fn commands_needing_an_os_come_back_out() {
        let mut ed = loaded(100);
        assert!(ed.step(Command::StartExport, 0.0).is_some());
        assert!(ed.step(Command::Open(PathBuf::from("/x.mov")), 0.0).is_some());
        assert!(ed.step(Command::ConfirmQuit, 0.0).is_some());
        assert!(ed.step(Command::Seek(4), 0.0).is_none());
        assert!(ed.step(Command::AddBank, 0.0).is_none());
    }

    /// A panel asks for a file by name, not by dialog: there is no `rfd` in a
    /// browser and no synchronous answer either, so every chooser is a request
    /// the shell fulfils on its own terms.
    #[test]
    fn every_file_chooser_is_a_request_to_the_shell() {
        let mut ed = loaded(100);
        for cmd in [Command::PickVideo, Command::PickProject, Command::PickShaderPath] {
            assert!(ed.step(cmd, 0.0).is_some());
        }
    }

    /// Both dialog flags are the editor's, so the button that swaps one prompt
    /// for the other is a single command rather than a panel writing shell
    /// state on its way past.
    #[test]
    fn showing_the_export_dialog_lowers_the_quit_prompt() {
        let mut ed = loaded(100);
        ed.show_quit_dialog = true;
        assert!(ed.step(Command::ShowExportDialog, 0.0).is_none(), "no shell needed to open it");
        assert!(ed.show_export_dialog);
        assert!(!ed.show_quit_dialog, "the prompt it replaced must come down");
    }

    /// The timeline drains mid-layout so its drags don't paint a frame behind.
    /// Its own commands must run there — and anything else must not, or a drag
    /// could swap the open video out from under the panel drawing it.
    #[test]
    fn a_mid_frame_drain_runs_the_editors_own_and_parks_the_rest() {
        let mut ed = loaded(100);
        ed.post(Command::Seek(42));
        ed.post(Command::Open(PathBuf::from("/x.mov")));
        ed.post(Command::Seek(7));

        ed.drain_ui(0.0);

        assert_eq!(ed.cur_frame, 7, "both seeks ran, in order");
        assert_eq!(ed.queued_len(), 0, "the queue is empty");
        let parked = ed.take_deferred();
        assert!(
            matches!(parked.front(), Some(Command::Open(_))),
            "the open was parked for the shell, not run: {parked:?}"
        );
        assert_eq!(parked.len(), 1);
        assert!(ed.take_deferred().is_empty(), "taking it twice must not repeat it");
    }

    // Selecting a span whose video is not open cannot open it here, so it has
    // to ask — and carry the continuation with it, or selecting a span on
    // another video would seek the wrong one.
    #[test]
    fn selecting_a_span_asks_the_shell_to_open_its_source() {
        let mut ed = loaded(100);
        ed.spans.spans.push(span("a"));
        ed.step(Command::SelectSpan(0), 0.0);
        match ed.pop() {
            Some(Command::OpenVideo { path, then }) => {
                assert_eq!(path, PathBuf::from("/v.mov"));
                assert!(matches!(then.as_slice(), [Command::SelectLoadedSpan(0)]));
            }
            other => panic!("expected an OpenVideo request, got {other:?}"),
        }
    }

    #[test]
    fn setting_the_out_mark_with_no_media_clamps_instead_of_panicking() {
        let mut ed = Editor::default();
        ed.step(Command::SetPendingOut(50), 0.0);
        assert_eq!(ed.pending_out, 1);
    }

    #[test]
    fn setting_a_span_range_keeps_out_above_in() {
        let mut ed = loaded(100);
        ed.spans.spans.push(span("a"));
        ed.step(Command::SetSpanRange { idx: 0, in_frame: 20, out_frame: 5 }, 0.0);
        let s = &ed.spans.spans[0];
        assert_eq!((s.in_frame, s.out_frame), (20, 21));
    }

    #[test]
    fn removing_a_bank_reindexes_the_spans_pointing_at_it() {
        let mut ed = loaded(100);
        ed.bank_names = vec!["a".into(), "b".into(), "c".into()];
        for bank in 0..3 {
            let mut s = span("s");
            s.clip_bank = bank;
            ed.spans.spans.push(s);
        }
        ed.step(Command::RemoveBank(1), 0.0);
        let banks: Vec<usize> = ed.spans.spans.iter().map(|s| s.clip_bank).collect();
        assert_eq!(banks, vec![0, 1, 1]);
    }

    #[test]
    fn the_last_bank_cannot_be_removed() {
        let mut ed = loaded(100);
        ed.step(Command::RemoveBank(0), 0.0);
        assert_eq!(ed.bank_names.len(), 1);
    }

    #[test]
    fn toggle_play_round_trips() {
        let mut ed = loaded(100);
        assert!(!ed.playing());
        ed.step(Command::TogglePlay, 0.0);
        assert!(ed.playing());
        ed.step(Command::TogglePlay, 0.0);
        assert!(!ed.playing());
    }

    #[test]
    fn undo_and_redo_a_structural_edit() {
        let mut ed = loaded(100);
        ed.spans.spans.push(span("a"));
        ed.step(Command::AddBank, 1.0);
        assert_eq!(ed.bank_names.len(), 2);
        ed.step(Command::Undo, 2.0);
        assert_eq!(ed.bank_names.len(), 1);
        ed.step(Command::Redo, 3.0);
        assert_eq!(ed.bank_names.len(), 2);
    }

    #[test]
    fn undo_with_empty_history_is_a_noop() {
        let mut ed = loaded(100);
        ed.step(Command::Undo, 0.0);
        assert_eq!(ed.status.as_deref(), Some("nothing to undo"));
    }

    // Coalescing is driven by the timestamp, which is now a parameter — so the
    // behaviour is testable without a window to ask what time it is.
    #[test]
    fn streaming_edits_coalesce_into_one_undo() {
        let mut ed = loaded(100);
        ed.spans.spans.push(span("a"));
        for (i, t) in [(1usize, 1.0), (2, 1.05), (3, 1.1)] {
            ed.step(Command::SetSpanName(0, format!("n{i}")), t);
        }
        ed.step(Command::Undo, 5.0);
        assert_eq!(ed.spans.spans[0].name, "a");
    }

    #[test]
    fn distinct_targets_do_not_coalesce() {
        let mut ed = loaded(100);
        ed.spans.spans = vec![span("a0"), span("b0")];
        ed.step(Command::SetSpanName(0, "a1".to_string()), 1.0);
        ed.step(Command::SetSpanName(1, "b1".to_string()), 1.05);

        ed.step(Command::Undo, 2.0);
        assert_eq!(ed.spans.spans[1].name, "b0", "first undo reverts span 1 only");
        assert_eq!(ed.spans.spans[0].name, "a1");

        ed.step(Command::Undo, 3.0);
        assert_eq!(ed.spans.spans[0].name, "a0", "second undo reverts span 0");
    }

    #[test]
    fn dragging_the_in_bracket_stops_below_the_out_mark() {
        let mut ed = loaded(100);
        ed.pending_in = 0;
        ed.pending_out = 5;
        ed.step(Command::SetPendingIn(99), 0.0);
        assert_eq!(ed.pending_in, 4);
    }

    #[test]
    fn playback_advances_the_playhead_by_wall_clock() {
        let mut ed = loaded(100);
        ed.pending_out = 100;
        ed.step(Command::TogglePlay, 0.0);
        ed.advance_playback(0.1); // 3 frames at 30fps
        assert_eq!(ed.cur_frame, 3);
    }

    #[test]
    fn zoom_to_marks_frames_the_selection() {
        let mut ed = loaded(1000);
        ed.pending_in = 400;
        ed.pending_out = 500;
        ed.zoom_to_marks();
        assert!(ed.view_start < 400);
        assert!(ed.view_end() > 498);
        assert!(ed.view_len < 200);
    }
}
