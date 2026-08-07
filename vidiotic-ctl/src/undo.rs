//! Session-scoped undo for the control-map editor.
//!
//! Unlike `vidiotic-prep`, `CtlApp` has no command choke point — bindings are
//! mutated inline by the shared table widgets, by `add`/`remove_binding`, and
//! by learn. So undo works by diffing the document against a `baseline` at each
//! frame boundary (`CtlApp::commit_undo`) rather than wrapping a command: the
//! `ControlMap` is a `Vec` of bindings, cheap to clone, and the frame is the
//! natural coalescing unit (all edits made in one frame land as one step). Same
//! session-only, whole-document-snapshot model as prep's `undo`; the stack is
//! depth-capped so a long session stays bounded.

use std::collections::VecDeque;

/// Hard ceiling on undo depth; the oldest step is dropped past it. Far beyond
/// any human undo run — it exists only to bound memory over a long session.
const DEPTH_CAP: usize = 256;

/// A snapshot undo/redo stack. `undo` holds prior document states (newest
/// last); `redo` holds states undone away (newest last).
pub struct History<T> {
    undo: VecDeque<T>,
    redo: Vec<T>,
}

// Manual `Default` — `#[derive(Default)]` would demand `T: Default`, which the
// empty deques don't actually need.
impl<T> Default for History<T> {
    fn default() -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new() }
    }
}

impl<T: Clone> History<T> {
    /// Record `prev` (the state before an edit), capping depth and dropping any
    /// redo branch.
    pub fn record(&mut self, prev: T) {
        self.undo.push_back(prev);
        while self.undo.len() > DEPTH_CAP {
            self.undo.pop_front();
        }
        self.redo.clear();
    }

    /// Take the last recorded state to restore, banking `current` for redo.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let prev = self.undo.pop_back()?;
        self.redo.push(current);
        Some(prev)
    }

    /// Take the last undone state to reinstate, banking `current` for undo.
    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push_back(current);
        Some(next)
    }

    /// Drop all history — used when the document is replaced from disk (open,
    /// revert), so a stale snapshot can't restore over a freshly loaded map.
    pub fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}
