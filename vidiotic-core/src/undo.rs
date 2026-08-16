//! The shared snapshot undo/redo stack.
//!
//! Whole-document-snapshot undo: each edit clones the document's undoable state
//! and pushes it; undo/redo swap stack states back and forth. The stack is
//! depth-capped so a long session stays bounded, and a streaming setter can
//! coalesce into the current gesture (same edit tag, close in time) rather
//! than pushing a fresh step.
//!
//! The session editors wrap this with their own document, edit-tag, and clock:
//! `vidiotic-chop` snapshots its span document with an `f64` frame timestamp,
//! and `vidiotic-play` snapshots its cue/bank document with a
//! [`web_time::Instant`]. `vidiotic-ctl`'s per-frame diff undo is the same
//! shape but lives locally there — core depends on ctl for `ControlMap`, so a
//! shared stack in core is unreachable from ctl without a cycle.

use std::collections::VecDeque;

/// Hard ceiling on undo depth; the oldest step is dropped past it. Far beyond
/// any human undo run — it exists only to bound memory over a long session.
const DEPTH_CAP: usize = 256;

/// A streaming setter re-firing on the same target within this window folds
/// into the existing step instead of pushing a new one.
const COALESCE_SECS: f64 = 0.6;

/// A monotonic clock the history can measure gesture time with. Implemented
/// for `f64` — the elapsed-seconds clock `vidiotic-chop`'s frame timestamps
/// are — and for `web_time::Instant`, which `vidiotic-play` passes. `web-time`
/// is `std::time` verbatim on native and the wasm-safe shim in the browser.
pub trait CoalesceClock: Clone {
    /// Seconds elapsed between `self` and `earlier`.
    fn elapsed_secs(&self, earlier: &Self) -> f64;
}

impl CoalesceClock for f64 {
    fn elapsed_secs(&self, earlier: &Self) -> f64 {
        self - earlier
    }
}

impl CoalesceClock for web_time::Instant {
    fn elapsed_secs(&self, earlier: &Self) -> f64 {
        self.duration_since(earlier.clone()).as_secs_f64()
    }
}

/// A snapshot undo/redo stack. `undo` holds prior document states (newest
/// last); `redo` holds states undone away (newest last).
///
/// `Tag` is the coalescing identity of a streaming edit; `Clock` is the
/// monotonic clock [`CoalesceClock`] measures the gesture window with.
pub struct SnapshotHistory<T, Tag, Clock = f64> {
    undo: VecDeque<T>,
    redo: Vec<T>,
    /// Tag + time of the edit that produced the current top-of-undo.
    last: Option<(Tag, Clock)>,
}

// Manual `Default` — `#[derive(Default)]` would demand `T: Default`, which the
// empty deques don't actually need.
impl<T, Tag, Clock> Default for SnapshotHistory<T, Tag, Clock> {
    fn default() -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new(), last: None }
    }
}

impl<T, Tag, Clock> SnapshotHistory<T, Tag, Clock>
where
    Clock: CoalesceClock,
{
    /// Whether `tag` at `now` should push a new step rather than fold into the
    /// current one.
    #[must_use]
    pub fn should_push(&self, tag: Option<Tag>, now: Clock) -> bool
    where
        Tag: PartialEq,
    {
        if self.undo.is_empty() {
            return true;
        }
        match (tag, &self.last) {
            (Some(t), Some((last, at))) => !(t == *last && now.elapsed_secs(at) < COALESCE_SECS),
            _ => true,
        }
    }

    /// Push a pre-edit snapshot, capping depth and invalidating redo.
    pub fn push(&mut self, snapshot: T, tag: Option<Tag>, now: Clock) {
        self.undo.push_back(snapshot);
        while self.undo.len() > DEPTH_CAP {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.last = tag.map(|t| (t, now));
    }

    /// A coalesced edit: no new snapshot, but it extends the gesture window and
    /// invalidates any redo branch.
    pub fn touch(&mut self, now: Clock) {
        if let Some((_, at)) = self.last.as_mut() {
            *at = now;
        }
        self.redo.clear();
    }

    /// Take the last pre-edit snapshot to restore, banking `current` for redo.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let prev = self.undo.pop_back()?;
        self.redo.push(current);
        self.last = None;
        Some(prev)
    }

    /// Take the last undone state to reinstate, banking `current` for undo.
    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.redo.pop()?;
        self.undo.push_back(current);
        self.last = None;
        Some(next)
    }

    /// Drop all history — used at a document boundary (project load, reopen)
    /// so a stale snapshot can't restore over a freshly loaded document.
    pub fn reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.last = None;
    }
}
