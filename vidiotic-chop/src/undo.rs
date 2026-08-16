//! Session-scoped document undo for the span editor.
//!
//! A whole-document snapshot stack, taken at the single command choke point
//! (`Editor::step`). The document is small — the span list, the
//! bank names, and the session defaults — so cloning it per edit is cheap: a
//! heavy session is tens of spans, ~10–20 KB a snapshot, and the stack is
//! capped at `DEPTH_CAP`, so the ceiling is single-digit MB however long the
//! session runs. Streaming setters (a rename, a mark drag) coalesce into one
//! step via [`classify`], so a gesture is one undo, not forty.
//!
//! Snapshotting the whole document — rather than recording each command's
//! inverse — means a new mutating command is undoable for free: the stack
//! never has to know what a command did. If the document ever outgrows
//! per-edit cloning, the snapshot can become a focused/patch representation
//! behind the same `Editor::snapshot`/`restore` pair without touching the
//! call sites here.

use vidiotic_core::project::SessionDefaults;

use crate::commands::Command;
use crate::spans::Span;

/// The undoable document: everything the "document state; undoable" commands
/// touch, and nothing transient (playhead, marks, view, textures). `selected`
/// rides along so an undo can't leave the cursor pointing at a span the same
/// undo just removed.
#[derive(Clone)]
pub struct Doc {
    pub spans: Vec<Span>,
    pub selected: Option<usize>,
    pub bank_names: Vec<String>,
    pub defaults: SessionDefaults,
}

/// Coalescing identity of a streaming edit: same tag + close in time = same
/// gesture. Structural edits don't get one (they always push a fresh step).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditTag {
    SpanRange(usize),
    SpanName(usize),
    SpanBpm(usize),
    SpanBank(usize),
    SpanCrop(usize),
    BankName(usize),
    Defaults,
}

/// How a command relates to the undo stack:
/// - `None` — not a document edit; no snapshot.
/// - `Some(None)` — a structural edit; always its own undo step.
/// - `Some(Some(tag))` — a streaming setter; coalesces with an adjacent
///   same-tag edit.
#[must_use]
pub fn classify(cmd: &Command) -> Option<Option<EditTag>> {
    Some(match cmd {
        Command::AddSpan
        | Command::RemoveSpan(_)
        | Command::MoveSpanUp(_)
        | Command::MoveSpanDown(_)
        | Command::AddBank
        | Command::RemoveBank(_) => None,
        Command::UpdateSpanFromMarks(i) => Some(EditTag::SpanRange(*i)),
        Command::SetSpanRange { idx, .. } => Some(EditTag::SpanRange(*idx)),
        Command::SetSpanName(i, _) => Some(EditTag::SpanName(*i)),
        Command::SetSpanBpm(i, _) => Some(EditTag::SpanBpm(*i)),
        Command::SetSpanBank(i, _) => Some(EditTag::SpanBank(*i)),
        Command::SetSpanCrop { idx, .. } => Some(EditTag::SpanCrop(*idx)),
        Command::ClearSpanCrop(i) => Some(EditTag::SpanCrop(*i)),
        Command::SetBankName(i, _) => Some(EditTag::BankName(*i)),
        Command::SetDefaults(_) => Some(EditTag::Defaults),
        _ => return None,
    })
}

/// The undo/redo history for one session — the shared snapshot stack with
/// this crate's document, edit tag, and `f64` frame-clock. See
/// [`vidiotic_core::undo::SnapshotHistory`].
pub type UndoStack = vidiotic_core::undo::SnapshotHistory<Doc, EditTag, f64>;
