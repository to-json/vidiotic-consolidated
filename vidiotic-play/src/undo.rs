//! Session-scoped document undo for the player.
//!
//! Same whole-document-snapshot model as `vidiotic-prep`'s `undo`, but the
//! player's document is entangled with live playback, so the scope is
//! deliberately narrow: undo covers **cue/bank authoring only** — the content
//! you build — and never live performance. Tempo taps, resets, sync changes,
//! live-bank switches, selection, mode toggles, and device/camera actions are
//! all excluded, because undoing them mid-set would be dangerous or meaningless
//! (you can't un-show a frame). The classifier below is the cue/bank subset of
//! `app::mutates_project`.
//!
//! The snapshot ([`Doc`]) is the cue banks plus the cue-id counter and the
//! selection, taken at the command choke point in `App::update`. Clip source
//! BPM (the one non-cue field an undoable command touches) is snapshotted as a
//! *targeted* id→bpm map rather than by cloning the whole clip pool: the pool
//! also holds camera clips added outside the undo path, and restoring a whole
//! clone would clobber them. Playback state — the clock, sequencer, decoders,
//! and which bank is live — is never snapshotted; `App::restore_doc` reconciles
//! it after a restore.

// See `clock.rs`: std's `Instant` compiles for wasm32 and panics on first use.
use web_time::Instant;

use vidiotic_core::bank::{Bank, CueId};
use vidiotic_core::chain::ClipId;

use crate::commands::{Command, CueParam, CueParamKind};

/// The undoable document: the cue banks, the id counter (so ids stay consistent
/// across undo/redo), the selection, and a targeted map of clip source BPMs.
#[derive(Clone)]
pub struct Doc {
    pub banks: Vec<Bank>,
    pub next_cue_id: CueId,
    pub selected_cue: Option<CueId>,
    /// `(clip id, bpm)` for every clip at snapshot time — restored field-wise,
    /// never used to add or drop clips. See the module note.
    pub clip_bpms: Vec<(ClipId, Option<f64>)>,
}

/// Coalescing identity of a streaming edit: same tag + close in time = same
/// gesture (a slider drag, a held nudge). Structural edits get none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditTag {
    CueRange(CueId),
    ChainParam(CueId, usize),
    CueParam(CueId, u8),
    NudgeParam(CueParamKind),
    ClipBpm(ClipId),
}

fn param_disc(p: &CueParam) -> u8 {
    match p {
        CueParam::Dwell(_) => 0,
        CueParam::Loop(_) => 1,
        CueParam::LoopPhase(_) => 2,
        CueParam::StartNudge(_) => 3,
        CueParam::TrigDelay(_) => 4,
        CueParam::Bpm(_) => 5,
        CueParam::BpmSync(_) => 6,
        CueParam::SpeedMul(_) => 7,
        CueParam::CamDelay(_) => 8,
    }
}

/// How a command relates to the undo stack:
/// - `None` — not an undoable document edit (live/transport/nav/device).
/// - `Some(None)` — a structural cue/bank edit; always its own step.
/// - `Some(Some(tag))` — a streaming setter; coalesces with an adjacent
///   same-tag edit.
#[must_use]
pub fn classify(cmd: &Command) -> Option<Option<EditTag>> {
    Some(match cmd {
        Command::AddCue(_)
        | Command::RemoveCue(_)
        | Command::MoveCue(..)
        | Command::AddBank
        | Command::CloneBank
        | Command::LoadIsf(_)
        | Command::SetCuePreserve(..)
        | Command::SetCueInToPlayhead(_)
        | Command::SetCueOutToPlayhead(_)
        | Command::SetCueChain(..) => None,
        Command::SetCueIn(c, _) | Command::SetCueOut(c, _) => Some(EditTag::CueRange(*c)),
        Command::SetChainParam { cue, slot, .. } => Some(EditTag::ChainParam(*cue, *slot)),
        Command::SetCueParam(c, p) => Some(EditTag::CueParam(*c, param_disc(p))),
        Command::NudgeCueParam(kind, _) => Some(EditTag::NudgeParam(*kind)),
        Command::SetClipBpm(c, _) => Some(EditTag::ClipBpm(*c)),
        _ => return None,
    })
}

/// Whether a command replaces or invalidates the document wholesale — a project
/// load or a clip-dir replace bumps the session epoch and re-ids everything, so
/// stale snapshots must be dropped rather than restored over the new pool.
#[must_use]
pub fn is_history_boundary(cmd: &Command) -> bool {
    matches!(cmd, Command::LoadProject(_) | Command::SetClipDir(_))
}

/// The undo/redo history for one session — the shared snapshot stack with
/// this crate's document, edit tag, and `web_time::Instant` clock. See
/// [`vidiotic_core::undo::SnapshotHistory`].
pub type UndoStack = vidiotic_core::undo::SnapshotHistory<Doc, EditTag, Instant>;

#[cfg(test)]
mod tests {
    use super::*;

    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    use std::sync::Arc;
    use std::time::Duration;
    use vidiotic_core::bank::{Bank, Cue};
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn doc(names: &[&str], next: CueId) -> Doc {
        let cues: Vec<Cue> = names
            .iter()
            .enumerate()
            .map(|(i, n)| Cue::new(i as CueId, 0, Arc::from(*n)))
            .collect();
        Doc {
            banks: vec![Bank {
                name: Arc::from("A"),
                cues,
            }],
            next_cue_id: next,
            selected_cue: None,
            clip_bpms: Vec::new(),
        }
    }

    fn cue_names(d: &Doc) -> Vec<String> {
        d.banks[0].cues.iter().map(|c| c.name.to_string()).collect()
    }

    #[test]
    fn undo_then_redo_round_trips() {
        let mut h = UndoStack::default();
        let t = Instant::now();
        // Edit 1: [] -> [a]
        h.push(doc(&[], 0), None, t);
        // Edit 2: [a] -> [a,b]
        h.push(doc(&["a"], 1), None, t);
        let current = doc(&["a", "b"], 2);

        let u1 = h.undo(current).unwrap();
        assert_eq!(cue_names(&u1), ["a"], "first undo drops b");
        let u2 = h.undo(u1).unwrap();
        assert_eq!(cue_names(&u2), [] as [String; 0], "second undo drops a");
        assert!(h.undo(u2.clone()).is_none(), "history exhausted");

        let r = h.redo(u2).unwrap();
        assert_eq!(cue_names(&r), ["a"], "redo reinstates a");
    }

    #[test]
    fn same_tag_within_window_coalesces() {
        let mut h = UndoStack::default();
        let t = Instant::now();
        h.push(doc(&["orig"], 1), Some(EditTag::CueRange(0)), t);
        // A second same-tag edit a moment later must fold in, not push.
        assert!(!h.should_push(Some(EditTag::CueRange(0)), t + Duration::from_millis(100)));
        // A different cue is a distinct gesture.
        assert!(h.should_push(Some(EditTag::CueRange(1)), t + Duration::from_millis(100)));
        // The same tag past the window is a new gesture.
        assert!(h.should_push(Some(EditTag::CueRange(0)), t + Duration::from_millis(700)));
    }

    #[test]
    fn a_new_edit_after_undo_clears_redo() {
        let mut h = UndoStack::default();
        let t = Instant::now();
        h.push(doc(&[], 0), None, t);
        let undone = h.undo(doc(&["a"], 1)).unwrap();
        // Redo is available...
        assert!(h.should_push(None, t));
        // ...until a fresh edit lands, which drops it.
        h.push(undone, None, t);
        assert!(
            h.redo(doc(&["x"], 1)).is_none(),
            "the redo branch was cleared"
        );
    }

    #[test]
    fn classify_splits_edits_from_live_actions() {
        assert!(classify(&Command::AddCue(0)) == Some(None), "structural");
        assert!(matches!(
            classify(&Command::SetCueIn(3, 1.0)),
            Some(Some(EditTag::CueRange(3)))
        ));
        assert!(classify(&Command::TapTempo).is_none(), "live: not undoable");
        assert!(
            classify(&Command::SetLiveBank(1)).is_none(),
            "performance: not undoable"
        );
        assert!(
            classify(&Command::SelectCue(None)).is_none(),
            "navigation: not undoable"
        );
        assert!(is_history_boundary(&Command::LoadProject("/x".into())));
        assert!(!is_history_boundary(&Command::AddCue(0)));
    }
}
