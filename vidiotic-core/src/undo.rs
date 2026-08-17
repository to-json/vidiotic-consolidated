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
        self.duration_since(*earlier).as_secs_f64()
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
        Self {
            undo: VecDeque::new(),
            redo: Vec::new(),
            last: None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// The shape both session editors instantiate: a `String` document, a `&str`
    /// edit tag, and the `f64` elapsed-seconds clock.
    type H = SnapshotHistory<String, &'static str, f64>;

    fn doc(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn undo_then_redo_walks_back_and_forward() {
        let mut h = H::default();
        h.push(doc("a"), None, 0.0);
        h.push(doc("b"), None, 1.0);
        // Document is now "c"; undo restores "b", banking "c" for redo.
        assert_eq!(h.undo(doc("c")), Some(doc("b")));
        assert_eq!(h.undo(doc("b")), Some(doc("a")));
        assert_eq!(h.undo(doc("a")), None, "empty stack yields nothing");
        assert_eq!(h.redo(doc("a")), Some(doc("b")));
        assert_eq!(h.redo(doc("b")), Some(doc("c")));
        assert_eq!(h.redo(doc("c")), None);
    }

    #[test]
    fn a_streaming_edit_coalesces_inside_the_window() {
        let mut h = H::default();
        // The first edit always pushes: there is nothing to fold into.
        assert!(h.should_push(Some("dwell"), 0.0));
        h.push(doc("a"), Some("dwell"), 0.0);
        // Same knob, still inside the gesture — fold.
        assert!(!h.should_push(Some("dwell"), 0.0 + COALESCE_SECS - 0.01));
        // Same knob, gesture expired — a new step.
        assert!(h.should_push(Some("dwell"), COALESCE_SECS + 0.01));
        // A different knob is a different gesture however fast it follows.
        assert!(h.should_push(Some("loop"), 0.01));
        // An untagged edit never coalesces.
        assert!(h.should_push(None, 0.01));
    }

    #[test]
    fn touch_extends_the_gesture_rather_than_restarting_it() {
        let mut h = H::default();
        h.push(doc("a"), Some("dwell"), 0.0);
        // Held knob: repeated touches keep the window open past its own length
        // from the original push, which is the whole point of a gesture.
        h.touch(0.5);
        assert!(!h.should_push(Some("dwell"), 1.0));
        h.touch(1.0);
        assert!(!h.should_push(Some("dwell"), 1.5));
        // Let go, and the next one is its own step.
        assert!(h.should_push(Some("dwell"), 1.0 + COALESCE_SECS + 0.01));
    }

    #[test]
    fn a_fresh_edit_invalidates_the_redo_branch() {
        let mut h = H::default();
        h.push(doc("a"), None, 0.0);
        assert_eq!(h.undo(doc("b")), Some(doc("a")));
        // "b" is sitting in redo. Editing from here abandons that future.
        h.push(doc("a"), None, 1.0);
        assert_eq!(h.redo(doc("z")), None, "redo does not survive a new edit");
    }

    /// The coalescing path has to invalidate redo too. It pushes no snapshot, so
    /// it would be easy to leave the redo branch intact — and then redo would
    /// reinstate a document that predates the coalesced edit.
    #[test]
    fn a_coalesced_edit_also_invalidates_redo() {
        let mut h = H::default();
        h.push(doc("a"), Some("dwell"), 0.0);
        assert_eq!(h.undo(doc("b")), Some(doc("a")));
        h.touch(0.1);
        assert_eq!(h.redo(doc("z")), None);
    }

    #[test]
    fn depth_is_capped_by_dropping_the_oldest() {
        let mut h = H::default();
        for i in 0..DEPTH_CAP + 10 {
            h.push(doc(&i.to_string()), None, i as f64);
        }
        // The cap holds, and it is the *oldest* steps that went: undoing all the
        // way back lands on step 10, not step 0.
        let mut last = None;
        while let Some(prev) = h.undo(doc("cur")) {
            last = Some(prev);
        }
        assert_eq!(last, Some(doc("10")));
    }

    /// Stepping through history ends the gesture. Otherwise the next turn of the
    /// same knob would fold into a step the user has just walked out of, and the
    /// edit would silently overwrite it.
    #[test]
    fn undo_and_redo_clear_the_gesture() {
        let mut h = H::default();
        h.push(doc("a"), Some("dwell"), 0.0);
        h.push(doc("b"), Some("dwell"), 5.0);
        // Same tag, same instant as the top-of-stack edit — it would coalesce if
        // the undo had not cleared the gesture. The stack is still non-empty, so
        // this is not the trivially-true first-edit case.
        h.undo(doc("c"));
        assert!(h.should_push(Some("dwell"), 5.0));
        h.redo(doc("b"));
        assert!(h.should_push(Some("dwell"), 5.0));
    }

    #[test]
    fn reset_drops_both_directions() {
        let mut h = H::default();
        h.push(doc("a"), Some("dwell"), 0.0);
        h.undo(doc("b"));
        h.reset();
        assert_eq!(h.undo(doc("x")), None);
        assert_eq!(h.redo(doc("x")), None);
        assert!(h.should_push(Some("dwell"), 0.0));
    }

    /// `web_time::Instant` is the other clock a session editor passes.
    #[test]
    fn the_instant_clock_measures_the_same_window() {
        let t0 = web_time::Instant::now();
        let mut h: SnapshotHistory<String, &str, web_time::Instant> = SnapshotHistory::default();
        h.push(doc("a"), Some("dwell"), t0);
        let inside = t0 + std::time::Duration::from_millis(100);
        let outside = t0 + std::time::Duration::from_millis(1_000);
        assert!(!h.should_push(Some("dwell"), inside));
        assert!(h.should_push(Some("dwell"), outside));
    }
}
