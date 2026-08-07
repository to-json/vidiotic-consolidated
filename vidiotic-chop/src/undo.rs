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

use std::collections::VecDeque;

use vidiotic_core::project::SessionDefaults;

use crate::commands::Command;
use crate::spans::Span;

/// Hard ceiling on undo depth; the oldest step is dropped past it. Far beyond
/// any human undo run — it exists only to bound memory over a long session.
const DEPTH_CAP: usize = 256;

/// A streaming setter re-firing on the same target within this window folds
/// into the existing step instead of pushing a new one.
const COALESCE_SECS: f64 = 0.6;

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
        Command::SetBankName(i, _) => Some(EditTag::BankName(*i)),
        Command::SetDefaults(_) => Some(EditTag::Defaults),
        _ => return None,
    })
}

/// The undo/redo history for one session. `undo` holds pre-edit snapshots
/// (newest last); `redo` holds states undone away (newest last).
#[derive(Default)]
pub struct UndoStack {
    undo: VecDeque<Doc>,
    redo: Vec<Doc>,
    /// Tag + time of the edit that produced the current top-of-undo, so a
    /// same-tag edit inside [`COALESCE_SECS`] can fold into it.
    last: Option<(EditTag, f64)>,
}

impl UndoStack {
    /// Whether `tag` (from [`classify`]) at time `now` should push a new step
    /// rather than fold into the current one.
    #[must_use]
    pub fn should_push(&self, tag: Option<EditTag>, now: f64) -> bool {
        if self.undo.is_empty() {
            return true;
        }
        match (tag, self.last) {
            (Some(t), Some((last, at))) => !(t == last && now - at < COALESCE_SECS),
            _ => true,
        }
    }

    /// Push a pre-edit snapshot, capping depth and invalidating redo.
    pub fn push(&mut self, snapshot: Doc, tag: Option<EditTag>, now: f64) {
        self.undo.push_back(snapshot);
        while self.undo.len() > DEPTH_CAP {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.last = tag.map(|t| (t, now));
    }

    /// A coalesced edit: no new snapshot, but it still extends the gesture
    /// window and invalidates any redo branch.
    pub fn touch(&mut self, now: f64) {
        if let Some((_, at)) = self.last.as_mut() {
            *at = now;
        }
        self.redo.clear();
    }

    /// Take the last pre-edit snapshot to restore, banking `current` for redo.
    pub fn undo(&mut self, current: Doc) -> Option<Doc> {
        let prev = self.undo.pop_back()?;
        self.redo.push(current);
        self.last = None;
        Some(prev)
    }

    /// Take the last undone state to reinstate, banking `current` for undo.
    pub fn redo(&mut self, current: Doc) -> Option<Doc> {
        let next = self.redo.pop()?;
        self.undo.push_back(current);
        self.last = None;
        Some(next)
    }

    /// Drop all history — used when the document is repopulated from disk, so
    /// a stale snapshot can't restore over a freshly loaded document.
    pub fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last = None;
    }
}
