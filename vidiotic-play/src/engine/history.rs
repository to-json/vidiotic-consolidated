//! Resets plus the undo/redo document snapshots.

use web_time::Instant;

use super::{Command, Engine};

impl Engine {
    /// Reset the beat grid to its origin (bar 1, beat 1, phrase 1) and re-prime
    /// the phrase/loop boundary trackers so nothing misfires on the backward
    /// jump. Playlist position and playhead are untouched.
    pub fn soft_reset(&mut self) {
        self.clock.reset();
        self.loop_tracker.reset();
        self.sequencer.reset_boundary();
    }

    /// Soft reset, plus jump the playlist back to its first cue and restart that
    /// cue's playhead from its in-point — regardless of the preserve-playhead
    /// setting, since a hard reset means "start over".
    pub fn hard_reset(&mut self) {
        self.soft_reset();
        let ev = self.sequencer.reset_to_first();
        self.apply_seq_events(ev);
        if let Some(cur) = self.current {
            if let Some(h) = self.decoders.get_mut(&cur) {
                h.request_restart();
            }
        }
    }

    /// A clone of the undoable document — cue banks, the id counter, the
    /// selection, and clip source BPMs (targeted, id→bpm). See [`crate::undo`].
    #[must_use]
    pub fn doc_snapshot(&self) -> crate::undo::Doc {
        crate::undo::Doc {
            banks: self.banks.clone(),
            next_cue_id: self.next_cue_id,
            selected_cue: self.selected_cue,
            clip_bpms: self.clips.iter().map(|c| (c.id, c.bpm)).collect(),
        }
    }

    /// Overwrite the document with a snapshot (undo/redo), then reconcile the
    /// live side: clamp bank/selection indices to the restored content, rebuild
    /// the sequencer's active set if the live bank was edited, and drop sources
    /// for cues that no longer exist.
    pub fn restore_doc(&mut self, doc: crate::undo::Doc) {
        self.banks = doc.banks;
        self.next_cue_id = doc.next_cue_id;
        self.selected_cue = doc.selected_cue;
        for (id, bpm) in doc.clip_bpms {
            if let Some(c) = self.clips.iter_mut().find(|c| c.id == id) {
                c.bpm = bpm;
            }
        }
        // Banks always hold at least the default "A", so `len - 1` is valid.
        let last = self.banks.len() - 1;
        self.live_bank = self.live_bank.min(last);
        self.edit_bank = self.edit_bank.min(last);
        if let Some(id) = self.selected_cue {
            if self.banks[self.edit_bank].cue(id).is_none() {
                self.selected_cue = None;
            }
        }
        self.resync_live_if_editing();
        self.retain_decoders();
    }

    /// Record the pre-edit state for `cmd`, unless it isn't an undoable edit or
    /// it coalesces into the current gesture. Runs before the command applies.
    pub fn record_undo(&mut self, cmd: &Command) {
        let Some(tag) = crate::undo::classify(cmd) else {
            return;
        };
        let now = Instant::now();
        if self.undo.should_push(tag, now) {
            let snapshot = self.doc_snapshot();
            self.undo.push(snapshot, tag, now);
        } else {
            self.undo.touch(now);
        }
    }

    pub fn undo_document(&mut self) {
        let current = self.doc_snapshot();
        if let Some(prev) = self.undo.undo(current) {
            self.restore_doc(prev);
        }
    }

    pub fn redo_document(&mut self) {
        let current = self.doc_snapshot();
        if let Some(next) = self.undo.redo(current) {
            self.restore_doc(next);
        }
    }
}
