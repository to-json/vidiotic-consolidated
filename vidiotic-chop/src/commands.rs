//! The UI↔app contract: everything the span editor can be asked to do.
//!
//! Unlike `vidiotic::commands`, there is no `Sender`: prep's panels hold
//! `&mut Editor` and post into its own queue, so a command here is a *deferred
//! mutation*, not a cross-thread message. What the indirection buys is a single
//! executor ([`crate::editor::Editor::step`]), a named vocabulary the binding
//! layer can target, and one choke point undo can wrap.
//!
//! It also buys the boundary. A command the editor cannot run comes back out of
//! `step` for the shell to answer, so this enum is the complete list of what a
//! marking session can ask the machine underneath it for — and short enough to
//! read (see [`crate::mirror`]).
//!
//! This is the runtime vocabulary. The *bindable* subset is
//! `vidiotic_ctl::PrepVerb`, which is deliberately smaller and `Copy`: every
//! heap payload (a path to open, a span's new name) lives only here, because
//! none of it can be baked into a binding.

use std::path::PathBuf;

/// Everything an input surface (buttons, keys, the timeline, dialogs) can ask
/// the span editor to do.
#[derive(Debug)]
pub enum Command {
    // --- transport ---
    TogglePlay,
    Pause,
    /// Seek to the in mark and play forward at 1x (shift+space).
    PlayFromIn,
    /// J/L shuttle: ±1 picks the direction; the magnitude is app state.
    Shuttle(f64),
    /// Seek without pausing. Callers that want the NLE "scrubbing stops
    /// playback" behaviour post [`Self::Pause`] alongside, as the timeline does.
    Seek(u64),
    /// Pause, then step the playhead by a signed frame count.
    Step(i64),
    /// Pause and jump to the first frame (Home).
    SeekStart,
    /// Pause and jump to the last frame (End).
    SeekEnd,
    /// Pause and seek proportionally, `0..=1` across the whole source — a
    /// fader or CC as a jog wheel.
    SeekFrac(f64),
    JumpToIn,
    /// Jump to the last frame the marks include (`pending_out` is exclusive).
    JumpToOut,

    // --- view ---
    /// Zoom the jog window by a factor (<1 zooms in), anchored on the playhead.
    ZoomView(f64),
    /// Zoom by a factor, keeping `anchor`'s on-screen position fixed.
    ZoomViewAt(f64, u64),
    ZoomFit,
    ZoomToMarks,
    /// Set the view window's first frame. Fractional input from pixel math is
    /// fine — the executor rounds and clamps.
    SetViewStart(f64),

    // --- pending marks ---
    SetIn,
    SetOut,
    /// Set the in mark directly, clamped below the out mark.
    SetPendingIn(u64),
    /// Set the out mark directly (exclusive), clamped above the in mark.
    SetPendingOut(u64),
    SnapOut,

    /// Set playback speed directly (0 = paused, negative = reverse).
    SetSpeed(f64),

    // --- spans (document state; undoable) ---
    AddSpan,
    RemoveSpan(usize),
    MoveSpanUp(usize),
    MoveSpanDown(usize),
    /// Overwrite a span's range from the pending marks.
    UpdateSpanFromMarks(usize),
    SetSpanName(usize, String),
    /// Set a span's range directly; the executor keeps `out > in`.
    SetSpanRange {
        idx: usize,
        in_frame: u64,
        out_frame: u64,
    },
    /// `None` falls back to the session bpm.
    SetSpanBpm(usize, Option<f64>),
    SetSpanBank(usize, usize),
    /// Set a span's crop box rect (`None` removes crop).
    SetSpanCrop {
        idx: usize,
        crop: Option<vidiotic_core::project::CropRect>,
    },
    ClearSpanCrop(usize),

    // --- banks / defaults (document state; undoable) ---
    AddBank,
    RemoveBank(usize),
    SetBankName(usize, String),
    /// Replace the session defaults wholesale. Boxed: `SessionDefaults` is 13
    /// fields, and this variant would otherwise set `Command`'s size.
    SetDefaults(Box<vidiotic_core::project::SessionDefaults>),

    // Each of these three first ensures the span's source video is open —
    // which may be gated behind a confirmation — and then continues as the
    // matching `*Loaded` command.
    SelectSpan(usize),
    /// Load a span's range into the pending marks for retrimming.
    LoadMarksFromSpan(usize),
    /// Load marks from a span and loop-play it.
    AuditionSpan(usize),

    /// Select a span and seek to its in point. Assumes its source is open.
    SelectLoadedSpan(usize),
    /// Load a span's range into the marks, seek to its in point, and frame it.
    /// Assumes its source is open.
    LoadMarksFromLoadedSpan(usize),

    // --- files / lifecycle ---
    /// Open a path: `.viproj` resumes a project, anything else is a source
    /// video. Videos are size-gated.
    Open(PathBuf),
    /// Open a source video, then run `then` — but only if it actually loaded.
    ///
    /// This is the continuation mechanism: `then` is what used to be
    /// `OpenFollowup`/`SpanFollowup`. A large file parks in `pending_open`
    /// with its `then` intact until the user confirms, so a command can wait
    /// on a dialog without any parallel machinery.
    OpenVideo {
        path: PathBuf,
        then: Vec<Self>,
    },
    /// Accept the large-file confirmation and run the parked open.
    ConfirmPendingOpen,
    /// Dismiss it, dropping the parked continuation with it.
    CancelPendingOpen,
    /// Reconstruct spans/banks/defaults from a reopened project. Assumes its
    /// source video is open.
    FinishOpenProject(Box<crate::editor::ReopenedProject>),

    /// Ask the shell for a source video to open.
    ///
    /// The three `Pick*` commands are how a panel raises a file chooser without
    /// naming one. Natively the shell answers with `rfd`; in a browser it
    /// cannot answer synchronously at all — `/play` answers the same shape by
    /// dispatching an event the page turns into an `<input type="file">`
    /// (web-port.md §8 step 4g). Either way the panel posts and forgets.
    PickVideo,
    /// Ask the shell for a `.viproj` to reopen for retrimming.
    PickProject,
    /// Ask the shell for a shader to set as the session default.
    PickShaderPath,

    ShowExportDialog,
    StartExport,
    ConfirmQuit,

    // --- history ---
    /// Restore the document to before the last undoable edit. Intercepted in
    /// `Editor::step` before `apply_command` — it acts
    /// on the undo stack, not the document, so it is not itself recorded. Not a
    /// `PrepVerb`: undo/redo are reserved chords (Cmd/Ctrl+Z), not rebindable.
    Undo,
    /// Reinstate the edit the last [`Self::Undo`] reverted.
    Redo,
}

impl Command {
    /// Whether the OS's key-repeat events should re-fire this command while a
    /// key is held.
    ///
    /// Only the frame-steppers. `egui::InputState::key_pressed` is documented
    /// as *"Includes key-repeat events"*, so holding an arrow scrubbed back
    /// when the transport keys were hardcoded `key_pressed` checks — and must
    /// keep doing so now they resolve through the mapper, which drops repeats
    /// (a held key must not re-fire `TogglePlay` sixty times a second).
    #[must_use]
    pub fn repeats_on_hold(&self) -> bool {
        matches!(self, Self::Step(_))
    }
}
