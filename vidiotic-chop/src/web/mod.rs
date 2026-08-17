//! The browser shell: the same marking session, on a canvas.
//!
//! `vidiotic-prep` is the native shell around this crate; this is the other
//! one. Neither is the app — [`Editor`] is, and both of these do the same job:
//! call [`Editor::step`], and answer the handful of commands that come back
//! because they need a machine.
//!
//! # What a shell actually is
//!
//! Nine commands. That is the whole interface, and it is worth listing what
//! each one means here rather than natively, because the differences are the
//! port:
//!
//! | command | native | here |
//! |---|---|---|
//! | `PickVideo`/`PickProject`/`PickShaderPath` | `rfd` returns a path | an event the page turns into `<input type=file>` |
//! | `Open` | stat it, route by extension | route by extension; the page already holds the bytes |
//! | `OpenVideo` | open a decoder | only the open video can be reopened — there is no filesystem to find another |
//! | `ConfirmPendingOpen` | run the parked open | never raised: the page has the file before the editor hears about it |
//! | `FinishOpenProject` | adopt, then fill the export folder | adopt, then ask for the source video |
//! | `StartExport` | spawn a bake thread | drive the page's baker span by span, then hand back a zip |
//! | `ConfirmQuit` | close the viewport | a tab is closed by closing it |
//!
//! # Two things the browser genuinely cannot do the same way
//!
//! **There are no paths.** A `Span::source` is a `PathBuf` and stays one, but
//! here it holds a *file name* — what the visitor dropped. Spans marked on a
//! video the page does not currently hold are shown read-only, which is the
//! behaviour prep already had for a span whose source is closed. Nothing needed
//! adding for that; it fell out of a rule written for a different reason.
//!
//! **Frames arrive late.** Natively `update_preview_texture` decodes the
//! current frame synchronously with ffmpeg. Here the page seeks a `<video>`
//! element and answers on some later turn of the event loop, so the shell asks
//! for a frame and keeps drawing the last one it has. The editor never waits —
//! it does not know a decoder exists.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use crate::commands::Command;
use crate::editor::{Editor, MediaInfo, ReopenedProject};
use crate::export::{self, BakedClip};
use crate::mirror::PrepMirror;

/// Frames sampled per second of source.
///
/// A `<video>` element reports a duration and no frame count, and seeking is by
/// time — so a frame index here is a *choice*, not a property of the file. 30 is
/// the same constant-rate assumption `/play`'s ingest makes (web-port.md §3d),
/// and it has to be the same number, because a span marked at frame 900 has to
/// mean the same instant when the exporter seeks to it.
pub const ASSUMED_FPS: f64 = 30.0;

// The bake driver, re-exported so it is linked into this bundle: `chop.js`
// constructs a `Baker` per span and pushes the frames it seek-steps out of the
// `<video>` element. It lives in `vidiotic-bake` because that is the crate whose
// job baking is, and because `/play`'s ingest drives the same type.
pub use vidiotic_bake::web::{bake_size, Baker};

fn window() -> web_sys::Window {
    web_sys::window().expect("a browser shell without a window")
}

/// Something the page has done, waiting for the next frame to be applied.
///
/// A queue rather than direct mutation because the page calls in from event
/// handlers — a `seeked` callback, a file-input `change` — which can land at any
/// point, including while egui is mid-layout inside `update`. Draining at a
/// known point is the same discipline the command queue itself exists for.
enum FromPage {
    VideoOpened {
        name: String,
        info: MediaInfo,
    },
    Failed(String),
    Session(String),
    Project {
        name: String,
        text: String,
    },
    Shader(String),
    Frame {
        index: u64,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    ExportFailed(String),
    Exported(String),
    StartExport,
}

#[derive(Default)]
struct Inbox {
    queue: Vec<FromPage>,
}

/// Everything an export needs, shared between the app and the page.
///
/// Shared rather than owned by [`ChopApp`] because `export_finish` has to hand
/// the archive straight back to the caller, and the app lives inside eframe's
/// runner where an exported function cannot reach it. The inbox answers the
/// other direction; this is the one that needs a return value.
#[derive(Default)]
struct ExportState {
    name: String,
    starter_cue_bank: bool,
    /// 0 = clips (draft), 1 = clips (high quality), 2 = offsets.
    ///
    /// One control rather than a quality flag plus an offsets flag, because
    /// "high quality offsets" is not a thing — offsets render nothing, so a
    /// quality knob beside them would be a setting with no effect, which is
    /// worse than no setting.
    render: usize,
    /// 0 = download, 1 = hand off to `/play` through OPFS.
    ///
    /// Separate from `render` because it is genuinely orthogonal: every render
    /// can go either way. A download leaves the origin and comes back through a
    /// file chooser; a handoff never leaves — both routes are the same origin,
    /// so `/play` can read what this tab wrote, and a chop stops being
    /// something you export and re-import to play.
    dest: usize,
    /// Clips finished so far, with their bytes.
    ///
    /// This does accumulate, which §3 says the *frame* pipeline must never do —
    /// and it does not: each frame is decoded, compressed, appended and dropped
    /// inside the `Baker`. What is held here is finished clips, which is
    /// unavoidable when the deliverable is a single archive.
    baked: Vec<(BakedClip, Vec<u8>)>,
    /// Progress line while a bake runs; `None` when nothing is exporting.
    note: Option<String>,
    /// The document as it was when the export started.
    ///
    /// Taken as a snapshot rather than read back off the editor at the end,
    /// because a bake takes minutes and nothing stops the visitor renaming a
    /// span or deleting one while it runs. Assembling from live state would
    /// produce a `.viproj` describing spans that no longer match the clips
    /// already written — the kind of project that loads and is simply wrong.
    plan: Option<ExportPlan>,
}

/// The document, frozen at the moment an export began.
struct ExportPlan {
    spans: Vec<crate::spans::Span>,
    bank_names: Vec<String>,
    defaults: vidiotic_core::project::SessionDefaults,
}

thread_local! {
    static INBOX: Rc<RefCell<Inbox>> = Rc::new(RefCell::new(Inbox::default()));
    static EXPORT: Rc<RefCell<ExportState>> = Rc::new(RefCell::new(ExportState {
        name: "project".to_string(),
        starter_cue_bank: true,
        ..ExportState::default()
    }));
    /// A flattened read of the editor, for the smoke test. See [`editor_state`].
    static STATE: RefCell<String> = const { RefCell::new(String::new()) };
    /// The live context, so [`post`] can wake the UI. Set once at boot.
    static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    /// The page's last word on storage, for the inspector line.
    static STORAGE: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
}

/// Hand the page's news to the next frame, and make sure there *is* one.
///
/// The repaint is the whole of why this is not a plain push. A browser egui
/// draws when something asks it to, and everything the page calls in with
/// arrives from outside egui's world — a file chooser's `change`, a `seeked`
/// callback. Without this, opening a video leaves the editor holding it and the
/// screen unchanged until the visitor happens to move the mouse.
///
/// Nothing native has this shape: eframe's winit loop is already awake for its
/// own reasons, and prep's decoder answers inside the frame that asked. Found by
/// `scripts/chop-smoke.mjs`, which opened a video and then watched an empty
/// session for twenty seconds.
fn post(ev: FromPage) {
    INBOX.with(|i| i.borrow_mut().queue.push(ev));
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.request_repaint();
        }
    });
}

/// The browser shell.
pub struct ChopApp {
    editor: Editor,
    mirror: PrepMirror,
    inbox: Rc<RefCell<Inbox>>,
    /// The frame the page has been asked for and has not yet delivered. One at
    /// a time: a scrub posts a seek per pointer-move, and firing a request for
    /// each would queue seconds of seeks behind a drag that ended long ago.
    awaiting: Option<u64>,
    /// The frame currently in `mirror.preview`, so a redraw that changed
    /// nothing does not re-ask for it.
    shown: Option<u64>,
    /// The key table, with no user layer over it — a browser has no
    /// `prep.vmap` to read and no rebinding UI to write one.
    keys: vidiotic_ctl::Mapper,
    /// A reopened project whose source video the visitor has not supplied yet.
    /// Held so that opening the right file finishes the reopen — natively the
    /// order is reversed, because there the file is already on disk.
    awaiting_source: Option<PathBuf>,
    /// The RON of the last session handed to the page, so an unchanged session
    /// is not rewritten. Prep dedups its `.vprep` writes the same way.
    last_saved: String,
    /// `Context::input.time` of the last autosave check (throttles to ~1 Hz).
    last_save_check: f64,
    /// What the page says about storage, shown in the inspector.
    storage: Rc<RefCell<Option<String>>>,
    /// The export dialog's fields and the bake in flight. No destination: a
    /// browser export is a download, so there is nowhere to choose.
    export: Rc<RefCell<ExportState>>,
}

impl Default for ChopApp {
    fn default() -> Self {
        Self {
            editor: Editor::default(),
            mirror: PrepMirror::default(),
            inbox: INBOX.with(Rc::clone),
            keys: vidiotic_ctl::Mapper::new(
                crate::keymap::default_map(),
                vidiotic_ctl::ControlMap::default(),
            ),
            awaiting: None,
            shown: None,
            awaiting_source: None,
            last_saved: String::new(),
            last_save_check: 0.0,
            storage: STORAGE.with(Rc::clone),
            export: EXPORT.with(Rc::clone),
        }
    }
}

impl ChopApp {
    fn drain_page(&mut self, ctx: &egui::Context) {
        let events: Vec<FromPage> = std::mem::take(&mut self.inbox.borrow_mut().queue);
        for ev in events {
            match ev {
                FromPage::VideoOpened { name, info } => self.open_media(&name, info),
                FromPage::Failed(msg) => {
                    self.awaiting = None;
                    self.editor.set_error(msg);
                }
                FromPage::Session(ron) => self.restore_session(&ron),
                FromPage::Project { name, text } => self.open_project(&name, &text),
                FromPage::Shader(name) => {
                    let mut d = self.editor.defaults.clone();
                    d.shader_path = Some(name);
                    self.editor.post(Command::SetDefaults(Box::new(d)));
                }
                FromPage::Frame {
                    index,
                    width,
                    height,
                    rgba,
                } => {
                    self.accept_frame(ctx, index, width, height, &rgba);
                }
                FromPage::ExportFailed(msg) => {
                    let mut st = self.export.borrow_mut();
                    st.note = None;
                    st.baked.clear();
                    st.plan = None;
                    drop(st);
                    self.editor.set_error(format!("export: {msg}"));
                }
                FromPage::StartExport => self.editor.post(Command::StartExport),
                FromPage::Exported(msg) => {
                    self.editor.show_export_dialog = false;
                    self.editor.set_status(msg);
                }
            }
        }
    }

    fn open_media(&mut self, name: &str, info: MediaInfo) {
        let path = PathBuf::from(name);
        self.editor.open_media(path.clone(), info);
        self.awaiting = None;
        self.shown = None;
        self.mirror.preview = None;
        self.editor.set_status(format!(
            "opened {name} ({}x{}, {:.2} fps, {} frames)",
            info.width, info.height, info.fps, info.frames
        ));
        // A project was reopened before its video arrived. If this is that
        // video, the reopen is now complete; if it is a different one, the
        // project's spans stay in the list read-only and the note stands.
        if self.awaiting_source.as_deref() == Some(path.as_path()) {
            self.awaiting_source = None;
        }
    }

    /// Merge a stored sidecar back into the session.
    ///
    /// Called after the source video is open, so the spans land against a real
    /// frame count. `adopt_globals` is true because a restore is by definition
    /// the first video of the session — there is nothing yet for it to stomp.
    fn restore_session(&mut self, ron: &str) {
        let Some(source) = self.editor.source_path.clone() else {
            log::warn!("a stored session arrived with no video open");
            return;
        };
        match crate::session::parse(ron, "stored session") {
            Ok(file) => {
                let spans = file.spans.len();
                let mut controls = vidiotic_ctl::ControlMap::default();
                file.merge_into(&mut self.editor, &mut controls, &source, true);
                // Spans arrived from storage outside the command path; any
                // prior snapshot predates them, so drop the history rather
                // than let undo restore over them. Same reasoning as prep's
                // sidecar restore.
                self.editor.reset_undo();
                self.last_saved.clear();
                self.editor
                    .set_status(format!("restored {spans} span(s) from your last session"));
            }
            Err(e) => log::warn!("ignoring the stored session: {e:#}"),
        }
    }

    /// Adopt a `.viproj`, then ask for its source video.
    ///
    /// The reverse of prep's order, and it has to be: natively the source is a
    /// path on disk that can be opened as part of answering `Open`, so the
    /// project is adopted only once its video is loaded. Here nothing can open
    /// a file the visitor has not handed over, so the spans land first and the
    /// video is a request. The editor tolerates this — every frame calculation
    /// clamps against `total_frames()`, which is 1 with no media.
    fn open_project(&mut self, name: &str, text: &str) {
        let label = name.to_string();
        let parsed = vidiotic_core::project::from_ron_versioned(text, &label)
            .and_then(|p| ReopenedProject::from_project(&p, name.trim_end_matches(".viproj")));
        let re = match parsed {
            Ok(re) => re,
            Err(e) => {
                self.editor.set_error(format!("reopen {name}: {e:#}"));
                return;
            }
        };
        self.editor.adopt_project(&re);
        let source = re.source.clone();
        let wanted = source.file_name().map_or_else(
            || source.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        if self.editor.source_path.as_deref() == Some(source.as_path()) {
            return;
        }
        self.awaiting_source = Some(source);
        self.editor.set_status(format!(
            "reopened {name} ({} span(s)) — open {wanted} to retrim them",
            self.editor.spans.spans.len()
        ));
    }

    fn accept_frame(
        &mut self,
        ctx: &egui::Context,
        index: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        // Only the frame that was asked for. A delivery for any other index is
        // stale — a request outstanding when the media changed, say — and
        // painting it would record a frame of the wrong video in `shown` while
        // leaving `awaiting` set, so `request_preview` (which gates on
        // `awaiting`) would never ask for anything again.
        if self.awaiting != Some(index) {
            return;
        }
        self.awaiting = None;
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            log::error!("frame {index}: {} bytes for {width}x{height}", rgba.len());
            return;
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], rgba);
        match &mut self.mirror.preview {
            Some(handle) => handle.set(img, egui::TextureOptions::LINEAR),
            None => {
                self.mirror.preview =
                    Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR));
            }
        }
        self.shown = Some(index);
    }

    /// Resolve this frame's key events into commands.
    ///
    /// The table is [`crate::keymap`] — the same one prep resolves against, so
    /// `i`/`o`/Enter/space/J-K-L mean here exactly what they mean on a desktop.
    /// What is missing compared to prep is only the layering: there is no
    /// `prep.vmap` to read, so the base table stands alone and the browser has
    /// no rebinding UI to change it with.
    ///
    /// Keys only when no text field is capturing them, so typing a span name
    /// does not scrub — the same gate `PrepApp::pump_controls` applies.
    fn pump_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        for (ev, repeat) in vidiotic_ctl::egui_keys::key_events(ctx) {
            // Undo/redo are reserved chords resolved ahead of the table, as in
            // `Controls::observe`.
            if let Some(history) = vidiotic_ctl::egui_keys::history_chord(&ev, repeat) {
                self.editor.post(match history {
                    vidiotic_ctl::egui_keys::History::Undo => Command::Undo,
                    vidiotic_ctl::egui_keys::History::Redo => Command::Redo,
                });
                continue;
            }
            if let Some(cmd) = crate::keymap::resolve(&mut self.keys, ev.source, ev.value, repeat) {
                self.editor.post(cmd);
            }
        }
    }

    /// Ask the page for the playhead's frame, unless it is already shown or
    /// already asked for.
    fn request_preview(&mut self) {
        if self.editor.media.is_none() || self.awaiting.is_some() {
            return;
        }
        let want = self.editor.cur_frame;
        if self.shown == Some(want) {
            return;
        }
        self.awaiting = Some(want);
        dispatch("vidiotic-chop-frame", &JsValue::from_f64(want as f64));
    }

    /// Run the queue, answering what the editor hands back.
    fn drain_commands(&mut self, ctx: &egui::Context) {
        for cmd in self.editor.take_deferred() {
            self.apply_shell_command(cmd);
        }
        for _ in 0..crate::editor::DRAIN_BUDGET {
            let Some(cmd) = self.editor.pop() else {
                if std::mem::take(&mut self.editor.repaint) {
                    ctx.request_repaint();
                }
                return;
            };
            let now = ctx.input(|i| i.time);
            if let Some(rest) = self.editor.step(cmd, now) {
                self.apply_shell_command(rest);
            }
        }
        log::warn!(
            "command drain hit its budget; dropping {} queued",
            self.editor.queued_len()
        );
        self.editor.clear_queue();
    }

    /// The nine commands, answered for a browser.
    ///
    /// Every arm that says "not in this build" says so *on the status line*,
    /// not only in the console. A verb that resolves and then does nothing is
    /// indistinguishable from a broken build unless something says which.
    fn apply_shell_command(&mut self, cmd: Command) {
        match cmd {
            Command::PickVideo => request_file("video"),
            Command::PickProject => request_file("project"),
            Command::PickShaderPath => request_file("shader"),

            // The page holds the bytes before the editor hears a name, so an
            // `Open` here is only ever a routing decision.
            Command::Open(path) => {
                let viproj = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("viproj"));
                if viproj {
                    request_file("project");
                } else {
                    request_file("video");
                }
            }

            // There is no filesystem to find another video in. If the span
            // being selected is on the open one, run its continuation; if not,
            // say which file would be needed. Prep would just open it.
            Command::OpenVideo { path, then } => {
                if self.editor.source_path.as_deref() == Some(path.as_path()) {
                    self.editor.resume(then);
                } else {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    self.editor
                        .set_error(format!("that span was marked on {name} — open it to edit"));
                }
            }

            // Raised only by the large-file dialog, which this shell never puts
            // up: by the time a name reaches the editor the page already has
            // the file, so there is nothing left to confirm.
            Command::ConfirmPendingOpen => {
                log::warn!("ConfirmPendingOpen in the browser shell, which never parks an open");
            }

            Command::FinishOpenProject(re) => {
                self.editor.adopt_project(&re);
            }

            Command::StartExport => self.start_export(),

            Command::ConfirmQuit => {
                self.editor.show_quit_dialog = false;
                self.editor
                    .set_status("close the tab when you're done".to_string());
            }

            other => {
                debug_assert!(false, "no shell owner for {other:?}");
                log::error!("dropped {other:?}: no shell owner and the editor declined it");
            }
        }
    }

    /// Hand the page a fresh sidecar if the session changed.
    ///
    /// Throttled to ~1 Hz and deduped against the last one written, which is
    /// exactly what prep's `autosave_session` does — and for the same reason,
    /// since the shape being written is the same `SessionFile`. Only the store
    /// differs: a file beside the video there, a record in OPFS here.
    ///
    /// The editor is never asked whether it is "dirty". Serializing and
    /// comparing is cheap next to a bake, and a dirty flag is a second source
    /// of truth that can be wrong in the direction that loses work.
    fn autosave(&mut self, ctx: &egui::Context) {
        let Some(source) = self.editor.source_path.clone() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let waited = now - self.last_save_check;
        if waited < 1.0 {
            // Ask for the frame that will do it. Natively the throttle is free
            // because eframe's winit loop is already running; here nothing else
            // would wake the app, so an edit made just before the visitor
            // stopped touching anything would sit unsaved forever. Found by the
            // smoke, which marked a span, waited, reloaded, and got it back
            // empty — the same class of bug as `post` needing a repaint.
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 - waited));
            return;
        }
        self.last_save_check = now;
        let file = crate::session::SessionFile::capture(
            &self.editor,
            &vidiotic_ctl::ControlMap::default(),
            &source,
        );
        let ron = crate::session::to_ron(&file);
        if ron == self.last_saved {
            return;
        }
        self.last_saved.clone_from(&ron);
        dispatch("vidiotic-chop-save", &JsValue::from_str(&ron));
    }

    /// Ask the page to bake every span.
    ///
    /// Natively this spawns a worker thread and streams progress back over a
    /// channel. Here the worker is the page: only it can seek the `<video>`
    /// element, so the shell hands over a plan and the frames come back through
    /// [`vidiotic_bake::web::Baker`] — the same compressor and muxer the desktop
    /// exporter drives, which is what makes a browser-baked clip byte-identical
    /// to a desktop one rather than merely similar.
    fn start_export(&mut self) {
        if self.editor.spans.spans.is_empty() {
            self.editor.set_error("no spans to export".to_string());
            return;
        }
        let Some(source) = self.editor.source_path.clone() else {
            self.editor
                .set_error("open the source video before exporting".to_string());
            return;
        };
        // Every span must be from the open video: there is one `<video>`
        // element and no way to fetch another file. Natively the exporter
        // reopens each span's own source by path, which is the whole reason
        // prep can export a session marked across several videos and this
        // cannot.
        let foreign: Vec<&str> = self
            .editor
            .spans
            .spans
            .iter()
            .filter(|s| s.source != source)
            .map(|s| s.name.as_str())
            .collect();
        if !foreign.is_empty() {
            self.editor.set_error(format!(
                "{} span(s) are from another video ({}) — a browser export can only bake the open one",
                foreign.len(),
                foreign.join(", ")
            ));
            return;
        }

        let fps = self.editor.fps();
        let source_label = source.file_name().map_or_else(
            || source.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let spans: Vec<String> = self
            .editor
            .spans
            .spans
            .iter()
            .enumerate()
            .map(|(i, sp)| {
                let crop_json = sp.crop.map_or_else(
                    || "null".to_string(),
                    |c| format!(r#"{{"x":{},"y":{},"w":{},"h":{}}}"#, c.x, c.y, c.w, c.h),
                );
                format!(
                    r#"{{"index":{i},"name":"{}","file":"clips/{}","source":"{}","in_sec":{},"out_sec":{},"crop":{crop_json}}}"#,
                    json_escape(&sp.name),
                    json_escape(&export::clip_file_name(i, sp)),
                    json_escape(&source_label),
                    sp.in_frame as f64 / fps,
                    sp.out_frame as f64 / fps,
                )
            })
            .collect();

        let render = self.export.borrow().render;
        if render == 2 {
            self.export_offsets(&source_label);
            return;
        }

        let (quality, dest) = {
            let mut st = self.export.borrow_mut();
            st.baked.clear();
            st.note = Some("baking…".to_string());
            st.plan = Some(ExportPlan {
                spans: self.editor.spans.spans.clone(),
                bank_names: self.editor.bank_names.clone(),
                defaults: self.editor.defaults.clone(),
            });
            (if render == 1 { "high" } else { "draft" }, st.dest)
        };
        let plan = format!(
            r#"{{"quality":"{quality}","dest":{dest},"spans":[{}]}}"#,
            spans.join(",")
        );
        dispatch("vidiotic-chop-export", &JsValue::from_str(&plan));
    }

    /// Write an offsets project: no bake, no archive, one `.viproj`.
    ///
    /// Nothing is dispatched to the page to do — there is no work — so this
    /// runs to completion here and hands the page the bytes to download.
    fn export_offsets(&mut self, source_label: &str) {
        let Some(media) = self.editor.media else {
            self.editor.set_error("no video is open".to_string());
            return;
        };
        let source = export::SourceRef {
            clip_name: export::SourceRef::clip_name_for(source_label),
            fps: media.fps,
            frames: media.frames,
            duration_sec: media.duration_sec,
            source_path: source_label.to_string(),
        };
        let project = export::assemble_offsets(
            &self.editor.spans.spans,
            &source,
            &self.editor.bank_names,
            self.editor.defaults.clone(),
            vidiotic_ctl::ControlMap::default(),
        );
        let bytes = export::viproj_bytes(&project);
        let cues: usize = project.cue_banks.iter().map(|b| b.cues.len()).sum();
        let name = {
            let st = self.export.borrow();
            if st.name.trim().is_empty() {
                "project".to_string()
            } else {
                export::sanitize(st.name.trim())
            }
        };
        let size = bytes.len();
        let handoff = self.export.borrow().dest == 1;
        deliver_file(
            if handoff {
                "vidiotic-chop-handoff"
            } else {
                "vidiotic-chop-download"
            },
            &format!("{name}.viproj"),
            &bytes,
        );
        self.editor.show_export_dialog = false;
        self.editor.set_status(if handoff {
            format!(
                "sent {name}.viproj to /play — {cues} cue(s) over {}, {size} bytes",
                source.clip_name
            )
        } else {
            format!(
                "exported {name}.viproj — {cues} cue(s) over {}, {size} bytes",
                source.clip_name
            )
        });
    }

    /// The browser's export window: the native dialog minus a destination,
    /// because a browser export is a download and there is nowhere to choose.
    ///
    /// Native prep draws its own in `shell_ui`, for the same reason this is
    /// here and not in `crate::ui`: what an export *is* differs between the two
    /// shells, so the panel belongs to whichever one is doing it.
    fn export_window(&mut self, ctx: &egui::Context) {
        let mut open = self.editor.show_export_dialog;
        let mut start = false;
        let spans = self.editor.spans.spans.len();
        let st = Rc::clone(&self.export);
        egui::Window::new("Export project")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let mut st = st.borrow_mut();
                ui.horizontal(|ui| {
                    ui.label("project name");
                    ui.text_edit_singleline(&mut st.name);
                });
                phosphor::widgets::section_label(ui, "render");
                let labels = ["clips", "clips (hq)", "offsets"];
                if let Some(i) =
                    phosphor::widgets::segmented(ui, "render", &labels, Some(st.render))
                {
                    st.render = i;
                }
                match st.render {
                    2 => {
                        ui.weak("no rendering: one .viproj naming this video, each span a");
                        ui.weak("trimmed cue. Instant, tiny, and needs the video at the");
                        ui.weak("other end — drop the same file into /play first.");
                    }
                    1 => {
                        ui.weak("one clip per span, ClusterFit BC1 (~6x slower bake)");
                    }
                    _ => {
                        ui.weak("one clip per span, RangeFit BC1 — softer gradients, much faster");
                    }
                }
                if st.render != 2 {
                    phosphor::widgets::glyph_checkbox(
                        ui,
                        &mut st.starter_cue_bank,
                        "starter cue bank (\"A\", one full-length cue per clip)",
                    );
                }

                phosphor::widgets::section_label(ui, "destination");
                let dests = ["download", "send to /play"];
                if let Some(i) =
                    phosphor::widgets::segmented(ui, "destination", &dests, Some(st.dest))
                {
                    st.dest = i;
                }

                ui.label(if st.render == 2 {
                    format!("{spans} span(s) as cues")
                } else {
                    format!("{spans} span(s) to bake")
                });
                if st.dest == 1 {
                    ui.weak("waits in this browser for /play — open it and the");
                    ui.weak(if st.render == 2 {
                        "project loads itself. Drop this video there too."
                    } else {
                        "project and its clips load themselves."
                    });
                } else {
                    ui.weak(if st.render == 2 {
                        "downloads a single .viproj"
                    } else {
                        "downloads as a .zip: the .viproj plus its clips/ folder"
                    });
                }

                if let Some(note) = st.note.clone() {
                    ui.label(egui::RichText::new(note).color(phosphor::theme::palette().accent));
                } else {
                    let ready = !st.name.trim().is_empty();
                    ui.add_enabled_ui(ready, |ui| {
                        start =
                            phosphor::widgets::bracket_button(ui, "export", None, 0.0).clicked();
                    });
                }
            });
        self.editor.show_export_dialog = open;
        if start {
            self.editor.post(Command::StartExport);
        }
    }

    /// A flattened read of the session, for `scripts/chop-smoke.mjs`.
    ///
    /// The panels are pixels on a canvas, so a browser test cannot see them.
    /// This is how it sees anything at all — the same reason `/play` has
    /// `engine_state`.
    fn state_json(&self) -> String {
        let spans: Vec<String> = self
            .editor
            .spans
            .spans
            .iter()
            .map(|s| {
                format!(
                    r#"{{"name":"{}","in":{},"out":{},"bank":{},"source":"{}"}}"#,
                    json_escape(&s.name),
                    s.in_frame,
                    s.out_frame,
                    s.clip_bank,
                    json_escape(&s.source.display().to_string())
                )
            })
            .collect();
        let source = self
            .editor
            .source_path
            .as_ref()
            .map_or_else(String::new, |p| json_escape(&p.display().to_string()));
        let (frames, fps) = self.editor.media.map_or((0, 0.0), |m| (m.frames, m.fps));
        format!(
            r#"{{"source":"{source}","frames":{frames},"fps":{fps},"cur":{},"in":{},"out":{},"playing":{},"spans":[{}],"banks":{},"status":"{}","error":{},"preview":{},"awaiting":{}}}"#,
            self.editor.cur_frame,
            self.editor.pending_in,
            self.editor.pending_out,
            self.editor.playing(),
            spans.join(","),
            self.editor.bank_names.len(),
            self.editor
                .status
                .as_deref()
                .map_or_else(String::new, json_escape),
            self.editor.status_is_error,
            self.mirror.preview.is_some(),
            self.awaiting.is_some(),
        )
    }
}

impl eframe::App for ChopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // `phosphor::shell::begin_frame` is this call and nothing else, and
        // `shell` is the feature that pulls eframe into phosphor. The theme
        // module is not gated, so the shell stays out of the dependency graph.
        phosphor::theme::sync(&ctx);
        self.drain_page(&ctx);

        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()))
        {
            self.editor.post(Command::Open(path));
        }
        self.pump_keys(&ctx);
        self.autosave(&ctx);
        self.editor
            .advance_playback(ctx.input(|i| i.stable_dt) as f64);
        self.request_preview();

        // Cloned before the editor is borrowed: the hook draws browser-only
        // rows inside the portable inspector's scroll area.
        let storage = Rc::clone(&self.storage);
        crate::ui::draw(&mut self.editor, &self.mirror, ui, &mut |ui| {
            inspector_note(ui, &storage);
        });
        if self.editor.show_export_dialog || self.export.borrow().note.is_some() {
            self.export_window(&ctx);
        }

        self.drain_commands(&ctx);
        // Playback and a pending seek both change what should be on screen
        // without any input arriving, and a browser egui only redraws when
        // asked.
        if self.editor.playing() || self.awaiting.is_some() {
            ctx.request_repaint();
        }
        let json = self.state_json();
        STATE.with(|s| *s.borrow_mut() = json);
    }
}

/// What sits where prep draws its two binding tables.
///
/// Not empty, deliberately: the inspector has a visible gap there in the native
/// app, and a browser visitor who has used both would reasonably conclude the
/// build is broken rather than that the feature is absent.
fn inspector_note(ui: &mut egui::Ui, storage: &Rc<RefCell<Option<String>>>) {
    ui.add_space(4.0);
    phosphor::widgets::section_label(ui, "this session");
    match storage.borrow().as_deref() {
        Some(note) => {
            ui.weak(note);
            // Not hidden behind a confirmation: what it deletes is one origin's
            // stored video and span list, and the alternative — a visitor who
            // cannot clear a session they no longer want — is worse than an
            // accidental clear they can redo by opening the file again.
            if phosphor::widgets::bracket_button(ui, "forget it", None, 0.0)
                .on_hover_text("delete the stored video and spans from this browser")
                .clicked()
            {
                dispatch("vidiotic-chop-forget", &JsValue::NULL);
            }
        }
        None => {
            ui.weak("nothing stored in this browser yet.");
        }
    }
    ui.add_space(4.0);
    ui.weak("MIDI, gamepads and editor key rebinding are not in this build.");
}

/// Fire a `CustomEvent` at the window for the page to answer.
///
/// The same bridge `/play` uses for `PickIsf` (web-port.md §8 step 4g), and for
/// the same reason: a file chooser has to be opened from inside the user
/// gesture that asked for it, and the gesture belongs to the page.
fn dispatch(name: &str, detail: &JsValue) {
    let init = web_sys::CustomEventInit::new();
    init.set_detail(detail);
    match web_sys::CustomEvent::new_with_event_init_dict(name, &init) {
        Ok(ev) => {
            let _ = window().dispatch_event(&ev);
        }
        Err(e) => log::error!("could not raise {name}: {e:?}"),
    }
}

/// Hand the page bytes to put somewhere, with the name to put them under.
///
/// `event` chooses which somewhere: a download, which is the only way *out* of
/// a tab and needs the anchor the page owns, or the OPFS handoff `/play` reads,
/// which needs the directory the page owns. Either way the bytes are built
/// here and written there, because storage and downloads are both browser APIs
/// and this crate deliberately holds neither.
///
/// The zip path goes through `export_finish`'s return value instead, because
/// that one is called *by* the page and can simply hand back.
fn deliver_file(event: &str, name: &str, bytes: &[u8]) {
    let detail = js_sys::Array::new();
    detail.push(&JsValue::from_str(name));
    detail.push(&js_sys::Uint8Array::from(bytes));
    dispatch(event, &detail);
}

fn request_file(kind: &str) {
    dispatch("vidiotic-chop-pick", &JsValue::from_str(kind));
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

struct ConsoleLog;

impl log::Log for ConsoleLog {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        let msg = JsValue::from_str(&format!("{}", record.args()));
        match record.level() {
            log::Level::Error => web_sys::console::error_1(&msg),
            log::Level::Warn => web_sys::console::warn_1(&msg),
            _ => web_sys::console::log_1(&msg),
        }
    }
    fn flush(&self) {}
}

static LOGGER: ConsoleLog = ConsoleLog;

/// Mount the editor on `canvas_id`.
///
/// # Errors
/// Propagates eframe's start failure — a missing canvas, or no WebGPU/WebGL
/// context to be had.
#[wasm_bindgen]
pub async fn boot(canvas_id: String) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));

    let canvas = window()
        .document()
        .and_then(|d| d.get_element_by_id(&canvas_id))
        .ok_or_else(|| JsValue::from_str(&format!("no element #{canvas_id}")))?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;

    eframe::WebRunner::new()
        .start(
            canvas,
            eframe::WebOptions::default(),
            Box::new(|cc| {
                phosphor::theme::apply(&cc.egui_ctx);
                CTX.with(|c| *c.borrow_mut() = Some(cc.egui_ctx.clone()));
                Ok(Box::<ChopApp>::default())
            }),
        )
        .await
}

/// The page has a video open and probed. `frames` is the page's choice, not the
/// file's — see [`ASSUMED_FPS`].
#[wasm_bindgen]
pub fn video_opened(name: &str, frames: f64, fps: f64, width: u32, height: u32, duration_sec: f64) {
    post(FromPage::VideoOpened {
        name: name.to_string(),
        info: MediaInfo {
            frames: frames.max(1.0) as u64,
            fps: if fps > 0.0 { fps } else { ASSUMED_FPS },
            width,
            height,
            duration_sec,
        },
    });
}

/// The page could not open something. Reaches the status line like any other
/// error, so a file the browser cannot decode reads the same as a bad seek.
#[wasm_bindgen]
pub fn open_failed(msg: &str) {
    post(FromPage::Failed(msg.to_string()));
}

/// A `.viproj`, as text.
#[wasm_bindgen]
pub fn load_project(name: &str, text: &str) {
    post(FromPage::Project {
        name: name.to_string(),
        text: text.to_string(),
    });
}

/// A shader chosen for the session defaults. Only the name travels: prep stores
/// a path string in the project and nothing here reads the file.
#[wasm_bindgen]
pub fn load_shader(name: &str) {
    post(FromPage::Shader(name.to_string()));
}

/// RGBA for a frame the shell asked for.
#[wasm_bindgen]
pub fn deliver_frame(index: f64, width: u32, height: u32, rgba: &[u8]) {
    post(FromPage::Frame {
        index: index.max(0.0) as u64,
        width,
        height,
        rgba: rgba.to_vec(),
    });
}

/// The session as JSON, for the smoke test. Empty until the first frame.
#[wasm_bindgen]
#[must_use]
pub fn editor_state() -> String {
    STATE.with(|s| s.borrow().clone())
}

/// One span's clip, baked by the page.
///
/// `path` is the plan's own `file` string handed straight back, so the name the
/// project references and the name in the archive cannot drift apart.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn export_baked(
    path: &str,
    source_path: &str,
    in_sec: f64,
    out_sec: f64,
    fps: f64,
    frames: f64,
    duration_sec: f64,
    bytes: Vec<u8>,
) {
    // Straight into the shared state rather than through the inbox: the page
    // hands over every clip and then calls `export_finish` in the same task, so
    // anything waiting for a frame to be drained would not have arrived yet.
    // The inbox is for what the *UI* must notice; this is what the export must
    // have.
    EXPORT.with(|st| {
        st.borrow_mut().baked.push((
            BakedClip {
                path: path.to_string(),
                source_path: source_path.to_string(),
                in_sec,
                out_sec,
                fps,
                frames: frames.max(0.0) as u64,
                duration_sec,
            },
            bytes,
        ));
    });
}

/// Start an export without going through the window.
///
/// A page-facing control like `/play`'s `set_bpm`: a host page may want its own
/// export button, and the smoke test needs a door into a flow whose only other
/// entrance is a click inside a canvas. It posts the same command the window's
/// button posts, so nothing here is a second path — the refusals for an empty
/// span list or a foreign source apply exactly as they would.
#[wasm_bindgen]
pub fn start_export() {
    post(FromPage::StartExport);
}

/// Choose how an export renders: 0 clips, 1 clips at high quality, 2 offsets.
///
/// Page-facing like [`start_export`]: a host page may want to offer the choice
/// outside the canvas, and the smoke needs to reach a control whose only other
/// door is a click inside one. Out-of-range values are ignored rather than
/// clamped — a caller that passes 7 has a bug, and silently picking a mode for
/// them hides it.
#[wasm_bindgen]
pub fn set_render(mode: usize) {
    if mode <= 2 {
        EXPORT.with(|st| st.borrow_mut().render = mode);
    }
}

/// Choose where an export goes: 0 download, 1 hand off to `/play` through OPFS.
///
/// Page-facing for the same reasons as [`set_render`], and ignoring
/// out-of-range values for the same reason.
#[wasm_bindgen]
pub fn set_destination(dest: usize) {
    if dest <= 1 {
        EXPORT.with(|st| st.borrow_mut().dest = dest);
    }
}

/// Progress, for the export window's line.
#[wasm_bindgen]
pub fn export_note(note: &str) {
    EXPORT.with(|st| st.borrow_mut().note = Some(note.to_string()));
    // The bake runs in the page's tasks, not egui's, so nothing else would
    // redraw the window this line is in — the same reason `post` repaints.
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.request_repaint();
        }
    });
}

/// The bake could not finish. Clears the export and says why.
#[wasm_bindgen]
pub fn export_failed(msg: &str) {
    post(FromPage::ExportFailed(msg.to_string()));
}

/// Assemble the `.viproj` and pack it with the clips.
///
/// Called once the page has handed over every clip. Returns the archive bytes
/// for the page to download — the one thing a browser can give back, and the
/// reason a zip is needed rather than a convenience: a project is a `.viproj`
/// *plus* a `clips/` directory whose relative paths it references.
///
/// # Errors
/// If nothing was baked, if the clips do not line up with the spans, or if the
/// result does not fit the zip format's 32-bit fields.
#[wasm_bindgen]
pub fn export_finish() -> Result<Vec<u8>, JsValue> {
    let (name, entries) = finish_entries()?;
    let archive = export::zip(&entries).map_err(|e| JsValue::from_str(&e.to_string()))?;
    post(FromPage::Exported(format!(
        "exported {name}.zip — {} clip(s), {} KiB",
        entries.len() - 1,
        archive.len() / 1024
    )));
    Ok(archive)
}

/// The same export, as loose files rather than an archive.
///
/// Returns `[[name, bytes], …]` — the `.viproj` first, then one entry per
/// clip, under exactly the paths the archive would have used, because the
/// project references them by those paths either way. The handoff writes these
/// straight into OPFS: zipping them so the other tab could unzip them would be
/// a compression pass and a decompression pass to move bytes between two
/// directories in the same origin.
///
/// # Errors
/// As [`export_finish`] — an export must be running and its clips must line up.
#[wasm_bindgen]
pub fn export_finish_files() -> Result<js_sys::Array, JsValue> {
    let (name, entries) = finish_entries()?;
    let out = js_sys::Array::new();
    for (path, bytes) in &entries {
        let pair = js_sys::Array::new();
        pair.push(&JsValue::from_str(path));
        pair.push(&js_sys::Uint8Array::from(&bytes[..]));
        out.push(&pair);
    }
    post(FromPage::Exported(format!(
        "sent {name} to /play — {} clip(s), {} KiB",
        entries.len() - 1,
        entries.iter().map(|(_, b)| b.len()).sum::<usize>() / 1024
    )));
    Ok(out)
}

/// Assemble the project and collect every file the export consists of.
///
/// Shared by both finishers, and it *consumes* the export: the plan is taken
/// and the baked clips cleared, so a second call reports "no export is running"
/// rather than silently delivering the same bake twice.
fn finish_entries() -> Result<(String, Vec<(String, Vec<u8>)>), JsValue> {
    EXPORT.with(|st| {
        let mut st = st.borrow_mut();
        let Some(plan) = st.plan.take() else {
            return Err(JsValue::from_str("no export is running"));
        };
        if st.baked.len() != plan.spans.len() {
            let (got, want) = (st.baked.len(), plan.spans.len());
            st.baked.clear();
            st.note = None;
            return Err(JsValue::from_str(&format!(
                "{got} clip(s) came back for {want} span(s)"
            )));
        }
        let clips: Vec<BakedClip> = st.baked.iter().map(|(c, _)| c.clone()).collect();
        let project = export::assemble(
            &plan.spans,
            &clips,
            &plan.bank_names,
            plan.defaults.clone(),
            vidiotic_ctl::ControlMap::default(),
            st.starter_cue_bank,
        );
        let name = if st.name.trim().is_empty() {
            "project".to_string()
        } else {
            export::sanitize(st.name.trim())
        };
        let mut entries: Vec<(String, Vec<u8>)> = vec![(
            format!("{name}/{name}.viproj"),
            export::viproj_bytes(&project),
        )];
        entries.extend(
            st.baked
                .iter()
                .map(|(c, bytes)| (format!("{name}/{}", c.path), bytes.clone())),
        );
        st.baked.clear();
        st.note = None;
        Ok((name, entries))
    })
}

/// A stored session, handed back after its video is open.
#[wasm_bindgen]
pub fn load_session(ron: &str) {
    post(FromPage::Session(ron.to_string()));
}

/// What the page wants to say about storage — how much is held, or why nothing
/// is. Shown in the inspector rather than logged, because a visitor who does
/// not know whether their work is being kept will not trust it either way.
#[wasm_bindgen]
pub fn storage_note(note: Option<String>) {
    STORAGE.with(|s| *s.borrow_mut() = note);
    CTX.with(|c| {
        if let Some(ctx) = c.borrow().as_ref() {
            ctx.request_repaint();
        }
    });
}
