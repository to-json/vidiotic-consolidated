//! The playing session: cues, banks, sources, and the sequencer feed.

use super::{
    bank_letter_name, clone_bank_with_ids, resolve_speed, step_index, Bank, ChainSlot, Clip,
    ClipBank, ClipId, Cue, CueId, CueStep, Engine, OpenRequest, Sequencer, SequencerEvent,
    LOOP_LADDER, LOOP_TICKS_PER_BEAT,
};
use crate::bank::Toggle;
use crate::commands::{CueParam, CueParamKind};

impl Engine {
    /// The cue with `id`, looked up in the live bank.
    #[must_use]
    pub fn live_cue(&self, id: CueId) -> Option<&Cue> {
        self.banks.get(self.live_bank).and_then(|b| b.cue(id))
    }

    /// Hand out the next unused [`CueId`].
    pub fn alloc_cue_id(&mut self) -> CueId {
        let id = self.next_cue_id;
        self.next_cue_id += 1;
        id
    }

    /// Open a source for `id` if it has none.
    ///
    /// A refusal is normal and is not recorded: natively a camera whose device
    /// is off-air gives no tap, and the shell's per-tick resolver retries, which
    /// is how toggling a device on-air picks up an already-armed cue without a
    /// re-trigger.
    pub fn ensure_decoder(&mut self, id: CueId) {
        if self.decoders.contains_key(&id) {
            return;
        }
        let Some(cue) = self.live_cue(id).cloned() else { return };
        let Some(clip) = self.clips.iter().find(|c| c.id == cue.clip) else { return };
        // Advanced mode: the sample-start nudge shifts the in-point, and playback
        // speed (BPM-sync × user multiplier) is baked in at open time.
        let nudge = if self.advanced && cue.start_nudge.on { cue.start_nudge.val } else { 0.0 };
        let in_sec = (cue.in_sec + nudge).max(0.0);
        let out_sec = cue.out_sec.filter(|&o| o > in_sec);
        let speed = resolve_speed(self.advanced, self.last_bpm, &cue, clip.bpm);
        let req = OpenRequest { cue: &cue, clip, in_sec, out_sec, speed, bpm: self.last_bpm };
        if let Some(src) = self.opener.open(&req) {
            self.decoders.insert(id, src);
        }
    }

    /// This clip's source-tempo metadata, if set.
    #[must_use]
    pub fn clip_bpm(&self, id: ClipId) -> Option<f64> {
        self.clips.iter().find(|c| c.id == id).and_then(|c| c.bpm)
    }

    /// Effective playback speed for a cue: `1.0` unless advanced mode is on.
    #[must_use]
    pub fn effective_speed(&self, cue: &Cue) -> f64 {
        resolve_speed(self.advanced, self.last_bpm, cue, self.clip_bpm(cue.clip))
    }

    /// The sequencer timing a cue contributes, resolving inherited dwell against
    /// the global phrase length. In simple mode every cue uses the global dwell
    /// with no trig delay, reproducing a fixed phrase grid.
    #[must_use]
    pub fn step_for(&self, cue: &Cue) -> CueStep {
        let tpb = f64::from(LOOP_TICKS_PER_BEAT);
        let default = self.sequencer.phrase_len();
        if self.advanced {
            CueStep {
                id: cue.id,
                dwell: cue.dwell.map_or(default, |t| f64::from(t) / tpb),
                trig_delay: if cue.trig_delay.on { f64::from(cue.trig_delay.val) / tpb } else { 0.0 },
            }
        } else {
            CueStep { id: cue.id, dwell: default, trig_delay: 0.0 }
        }
    }

    /// The [`CueStep`]s for a bank's cues, in play order.
    #[must_use]
    pub fn cue_steps(&self, bank: usize) -> Vec<CueStep> {
        self.banks
            .get(bank)
            .map(|b| b.cues.iter().map(|c| self.step_for(c)).collect())
            .unwrap_or_default()
    }

    /// The re-loop grid (ticks) and phase offset (beats) for the playing cue:
    /// per-cue in advanced mode, else the global loop setting.
    #[must_use]
    pub fn current_loop_params(&self) -> (Option<u32>, f64) {
        let global = self.loop_len;
        if !self.advanced {
            return (global, 0.0);
        }
        let Some(cue) = self.current.and_then(|c| self.live_cue(c)) else {
            return (global, 0.0);
        };
        let ticks = match cue.loop_len {
            Some(0) => None,    // per-cue: force no re-loop
            Some(t) => Some(t), // per-cue rate
            None => global,     // inherit the global loop setting
        };
        let phase = if cue.loop_phase.on {
            f64::from(cue.loop_phase.val) / f64::from(LOOP_TICKS_PER_BEAT)
        } else {
            0.0
        };
        (ticks, phase)
    }

    /// Add a full-length cue for `clip` to the live bank if none exists there,
    /// else remove it. Keeps the sequencer's active set in step. (The quick pool
    /// path; finer control comes from the bank editor.)
    pub fn toggle_clip_active(&mut self, clip: ClipId, beat: f64) {
        let existing = self.banks[self.live_bank].cues.iter().position(|c| c.clip == clip);
        if let Some(pos) = existing {
            let cue = self.banks[self.live_bank].cues[pos].clone();
            let step = self.step_for(&cue);
            let ev = self.sequencer.toggle_active(step, beat);
            self.banks[self.live_bank].cues.remove(pos);
            if self.selected_cue == Some(cue.id) {
                self.selected_cue = None;
            }
            self.apply_seq_events(ev);
        } else {
            let cue_id = self.alloc_cue_id();
            let name = self.clip_name(clip);
            let cue = Cue::new(cue_id, clip, name);
            let step = self.step_for(&cue);
            self.banks[self.live_bank].cues.push(cue);
            let ev = self.sequencer.toggle_active(step, beat);
            self.apply_seq_events(ev);
        }
    }

    /// Drop sources that are neither playing nor armed.
    pub fn retain_decoders(&mut self) {
        let keep: Vec<CueId> = [self.current, self.sequencer.armed()].into_iter().flatten().collect();
        self.decoders.retain(|k, _| keep.contains(k));
    }

    /// Apply the [`Sequencer`]'s output events: arm/swap decoders and retain
    /// only sources still playing or armed.
    pub fn apply_seq_events(&mut self, events: Vec<SequencerEvent>) {
        for e in events {
            match e {
                SequencerEvent::ArmDecoder(c) => self.ensure_decoder(c),
                SequencerEvent::SwapTo(c) => {
                    self.ensure_decoder(c);
                    self.current = Some(c);
                    self.retain_decoders();
                    // With preserve off, the incoming clip should cut in from its
                    // in-point rather than the position it drifted to since arming.
                    // A cue's own `preserve` overrides the global default.
                    let preserve = self
                        .live_cue(c)
                        .and_then(|cue| cue.preserve)
                        .unwrap_or(self.preserve_playhead);
                    if !preserve {
                        if let Some(h) = self.decoders.get_mut(&c) {
                            h.request_restart();
                        }
                    }
                    // A freshly swapped clip starts from the top; re-anchor the
                    // re-loop grid so it doesn't restart mid-clip immediately.
                    self.loop_tracker.reset();
                }
                SequencerEvent::DisarmDecoder => self.retain_decoders(),
            }
        }
    }

    /// If the edit bank is also live, rebuild the sequencer's active set from it
    /// (call after adding/removing a cue in the edit bank).
    pub fn resync_live_if_editing(&mut self) {
        if self.edit_bank == self.live_bank {
            let steps = self.cue_steps(self.live_bank);
            let ev = self.sequencer.set_active_set(steps);
            self.apply_seq_events(ev);
        }
    }

    /// Append a new cue for `clip` to the edit bank and select it.
    pub fn add_cue(&mut self, clip: ClipId) {
        let cue_id = self.alloc_cue_id();
        let name = self.clip_name(clip);
        self.banks[self.edit_bank].cues.push(Cue::new(cue_id, clip, name));
        self.selected_cue = Some(cue_id);
        self.resync_live_if_editing();
    }

    /// Remove `id` from the edit bank, clearing selection if it was selected.
    /// No-op if `id` isn't in the edit bank.
    pub fn remove_cue(&mut self, id: CueId) {
        let Some(pos) = self.banks[self.edit_bank].cues.iter().position(|c| c.id == id) else {
            return;
        };
        self.banks[self.edit_bank].cues.remove(pos);
        if self.selected_cue == Some(id) {
            self.selected_cue = None;
        }
        self.resync_live_if_editing();
    }

    /// Mutate a cue in the edit bank. Trim/preserve changes take effect on the
    /// cue's next source open (they are read at open / swap time).
    pub fn edit_cue(&mut self, id: CueId, f: impl FnOnce(&mut Cue)) {
        if let Some(cue) = self.banks[self.edit_bank].cue_mut(id) {
            f(cue);
        }
    }

    /// Apply one advanced per-cue knob to the edit bank. Dwell/trig-delay change
    /// the rotation's timing, so those refresh the sequencer's active set; the
    /// rest are read at the cue's next source open or loop tick.
    pub fn set_cue_param(&mut self, id: CueId, p: CueParam) {
        self.edit_cue(id, |c| match p {
            CueParam::Dwell(v) => c.dwell = v,
            CueParam::Loop(v) => c.loop_len = v,
            CueParam::LoopPhase(t) => c.loop_phase = t,
            CueParam::StartNudge(t) => c.start_nudge = t,
            CueParam::TrigDelay(t) => c.trig_delay = t,
            CueParam::Bpm(v) => c.bpm = v,
            CueParam::BpmSync(on) => c.bpm_sync_on = on,
            CueParam::SpeedMul(t) => c.speed_mul = t,
            CueParam::CamDelay(d) => c.delay = d,
        });
        if matches!(p, CueParam::Dwell(_) | CueParam::TrigDelay(_)) {
            self.resync_live_if_editing();
        }
    }

    /// Reorder a cue within the edit bank to `target`, then re-sync the live set.
    pub fn move_cue(&mut self, id: CueId, target: usize) {
        let cues = &mut self.banks[self.edit_bank].cues;
        let Some(from) = cues.iter().position(|c| c.id == id) else {
            return;
        };
        let cue = cues.remove(from);
        let to = target.min(cues.len());
        cues.insert(to, cue);
        self.resync_live_if_editing();
    }

    /// Set (or clear) a source clip's tempo metadata.
    pub fn set_clip_bpm(&mut self, id: ClipId, bpm: Option<f64>) {
        if let Some(c) = self.clips.iter_mut().find(|c| c.id == id) {
            c.bpm = bpm.filter(|b| b.is_finite() && *b > 0.0);
        }
    }

    /// Toggle advanced sequencer mode. Per-cue timing resolution changes for the
    /// whole rotation, so rebuild the active set and re-prime the loop grid.
    pub fn set_advanced(&mut self, on: bool) {
        if self.advanced == on {
            return;
        }
        self.advanced = on;
        self.loop_tracker.reset();
        self.resync_live_if_editing();
    }

    /// Switch which bank the sequencer plays from. No-op for an out-of-range
    /// or already-live index.
    pub fn set_live_bank(&mut self, i: usize) {
        if i >= self.banks.len() || i == self.live_bank {
            return;
        }
        self.live_bank = i;
        // The new bank takes over: keep playing the current cue if it happens to
        // still resolve, otherwise the sequencer advances into the new set at the
        // next arm window.
        let steps = self.cue_steps(self.live_bank);
        let ev = self.sequencer.set_active_set(steps);
        self.apply_seq_events(ev);
    }

    /// Step the live bank by `delta`, wrapping around. No-op with fewer than
    /// two banks. [`Self::set_live_bank`] ignores a same-index target, so wrap
    /// is safe.
    pub fn cycle_live_bank(&mut self, delta: i32) {
        let n = self.banks.len();
        if n < 2 {
            return;
        }
        let next = (self.live_bank as i32 + delta).rem_euclid(n as i32) as usize;
        self.set_live_bank(next);
    }

    /// Switch which bank the cue editor targets, clearing cue selection.
    /// No-op for an out-of-range index.
    pub fn set_edit_bank(&mut self, i: usize) {
        if i >= self.banks.len() {
            return;
        }
        self.edit_bank = i;
        self.selected_cue = None;
    }

    /// Append a fresh, empty bank with the next letter name.
    pub fn add_bank(&mut self) {
        let name = bank_letter_name(self.banks.len());
        self.banks.push(Bank::new(name));
    }

    /// Duplicate the edit bank (fresh cue ids, next letter name) and append it.
    pub fn clone_bank(&mut self) {
        let mut bank = clone_bank_with_ids(&self.banks[self.edit_bank], &mut self.next_cue_id);
        bank.name = bank_letter_name(self.banks.len()).into();
        self.banks.push(bank);
    }

    /// Move cue selection through the edit bank's order, clamping at the ends.
    pub fn select_cue_delta(&mut self, delta: i32) {
        let cues = &self.banks[self.edit_bank].cues;
        let pos = self.selected_cue.and_then(|id| cues.iter().position(|c| c.id == id));
        if let Some(target) = step_index(cues.len(), pos, delta) {
            self.selected_cue = Some(cues[target].id);
        }
    }

    /// The active clip bank's clip order — the list the pool grid shows and the
    /// clip cursor moves through.
    #[must_use]
    pub fn active_clip_ids(&self) -> &[ClipId] {
        self.clip_banks
            .get(self.active_clip_bank)
            .map_or(&[][..], |b| b.clip_ids.as_slice())
    }

    /// Move the pool's clip cursor through the active clip bank, clamping at the
    /// ends. A cursor left in another bank counts as no selection.
    pub fn select_clip_delta(&mut self, delta: i32) {
        let ids = self.active_clip_ids();
        let pos = self.selected_clip.and_then(|id| ids.iter().position(|&c| c == id));
        if let Some(target) = step_index(ids.len(), pos, delta) {
            self.selected_clip = Some(ids[target]);
        }
    }

    /// Switch which clip bank [`Self::active_clip_ids`] reads from. No-op
    /// for an out-of-range index.
    pub fn set_active_clip_bank(&mut self, i: usize) {
        if i < self.clip_banks.len() {
            self.active_clip_bank = i;
        }
    }

    /// Find-or-create the pool clip for a source, returning its id.
    ///
    /// The engine owns the id space, so a shell that discovers a new source —
    /// a camera coming on air, a file dropped on the page — adds it here rather
    /// than reaching into `clips` and `next_clip_id` itself.
    pub fn intern_clip(
        &mut self,
        source: crate::clippool::ClipSource,
        name: std::sync::Arc<str>,
    ) -> ClipId {
        let key = match &source {
            crate::clippool::ClipSource::Camera { uid, .. } => Some(uid.clone()),
            crate::clippool::ClipSource::File(_) => None,
        };
        if let Some(uid) = &key {
            if let Some(c) = self.clips.iter().find(|c| c.camera_uid() == Some(uid.as_ref())) {
                return c.id;
            }
        }
        let id = self.next_clip_id;
        self.next_clip_id += 1;
        self.clips.push(Clip { id, source, name, bpm: None });
        id
    }

    /// Append `ids` to a clip bank, creating it when `bank` is past the end.
    /// The new bank becomes the active one.
    pub fn push_clip_bank(&mut self, name: std::sync::Arc<str>, dir: Option<std::path::PathBuf>, ids: Vec<ClipId>) {
        self.clip_banks.push(crate::clippool::ClipBank { name, dir, clip_ids: ids });
        self.active_clip_bank = self.clip_banks.len() - 1;
    }

    /// Step one advanced knob of the selected cue by ± one detent, mirroring the
    /// cue editor's drag speeds and ranges. Toggle-backed knobs switch on when
    /// nudged; inherit-backed ones materialize their effective value first
    /// (dwell from the global phrase, bpm from the source clip).
    pub fn nudge_cue_param(&mut self, kind: CueParamKind, dir: i32) {
        let Some(id) = self.selected_cue else { return };
        let Some(cue) = self.banks[self.edit_bank].cue(id) else { return };
        let d = dir.signum();
        let p = match kind {
            CueParamKind::Dwell => {
                let phrase = (self.phrase_cadence.beats(self.time_sig).max(1.0)
                    * f64::from(LOOP_TICKS_PER_BEAT))
                .round() as i64;
                let base = cue.dwell.map_or(phrase, i64::from);
                CueParam::Dwell(Some(
                    (base + i64::from(d) * i64::from(LOOP_TICKS_PER_BEAT)).clamp(8, 8192) as u32,
                ))
            }
            CueParamKind::Loop => {
                let idx = LOOP_LADDER
                    .iter()
                    .position(|e| *e == cue.loop_len)
                    // A hand-set tick count off the ladder: enter at the nearest
                    // cadence at or above it.
                    .unwrap_or_else(|| match cue.loop_len {
                        Some(t) => LOOP_LADDER
                            .iter()
                            .position(|e| matches!(e, Some(x) if *x >= t))
                            .unwrap_or(LOOP_LADDER.len() - 1),
                        None => 0,
                    });
                let next = (idx as i32 + d).clamp(0, LOOP_LADDER.len() as i32 - 1) as usize;
                CueParam::Loop(LOOP_LADDER[next])
            }
            CueParamKind::LoopPhase => CueParam::LoopPhase(Toggle {
                on: true,
                val: (cue.loop_phase.val + d).clamp(-256, 256),
            }),
            CueParamKind::StartNudge => CueParam::StartNudge(Toggle {
                on: true,
                val: (cue.start_nudge.val + f64::from(d) * 0.01).clamp(-600.0, 600.0),
            }),
            CueParamKind::TrigDelay => CueParam::TrigDelay(Toggle {
                on: true,
                val: (cue.trig_delay.val as i32 + d * 8).clamp(0, 1024) as u32,
            }),
            CueParamKind::Bpm => {
                let base = cue
                    .bpm
                    .or_else(|| self.clips.iter().find(|c| c.id == cue.clip).and_then(|c| c.bpm))
                    .unwrap_or(120.0);
                CueParam::Bpm(Some((base + f64::from(d)).clamp(20.0, 400.0)))
            }
            CueParamKind::BpmSync => CueParam::BpmSync(!cue.bpm_sync_on),
            CueParamKind::SpeedMul => CueParam::SpeedMul(Toggle {
                on: true,
                val: (cue.speed_mul.val + f64::from(d) * 0.05).clamp(0.05, 20.0),
            }),
        };
        self.set_cue_param(id, p);
    }

    /// Replace the pool wholesale — a project load or a clip-directory swap.
    ///
    /// Cues referenced the old pool's ids, so they go too. This is the one
    /// operation that invalidates the whole id space, which is why the native
    /// shell bumps its IPC epoch right after calling it.
    pub fn replace_pool(&mut self, clips: Vec<Clip>, clip_banks: Vec<ClipBank>, cue_banks: Vec<Bank>) {
        self.next_clip_id = clips.iter().map(|c| c.id).max().map_or(0, |m| m + 1);
        self.clips = clips;
        self.clip_banks = clip_banks;
        self.active_clip_bank = 0;
        self.decoders.clear();
        self.current = None;
        self.blanked_for = None;
        self.banks = if cue_banks.is_empty() { vec![Bank::new("A")] } else { cue_banks };
        self.next_cue_id = self.banks.iter().flat_map(Bank::ids).max().map_or(1, |m| m + 1);
        self.live_bank = 0;
        self.edit_bank = 0;
        self.selected_cue = None;
        self.selected_clip = None;
        self.sequencer = Sequencer::new(self.sequencer.phrase_len());
    }

    /// Drop every chain slot matching `pred` from every cue in every bank.
    ///
    /// Used when a pooled shader is removed: the slots pointing at it would
    /// otherwise render as a silent no-op forever.
    pub fn retain_chain_slots(&mut self, mut pred: impl FnMut(&ChainSlot) -> bool) {
        for bank in &mut self.banks {
            for cue in &mut bank.cues {
                cue.chain.retain(&mut pred);
            }
        }
    }
}
