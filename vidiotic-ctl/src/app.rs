//! `CtlApp`: the mapping editor + live monitor over the global control map.
//! The binding table itself lives in [`vidiotic_ctl::ui`], shared with
//! `vidiotic-prep`'s two editors; [`crate::panels`] is the chrome around it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use vidiotic_ctl::{
    egui_keys, Action, Binding, ControlEvent, ControlMap, ControlSource, Learn, MidiHub, PadPoller,
};

/// How often [`MidiHub::rescan`] runs — `midir` exposes no `CoreMIDI` hotplug
/// callback, so this is the whole hotplug story.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Live-monitor ring-buffer capacity.
const MONITOR_CAP: usize = 200;

pub struct CtlApp {
    pub map: ControlMap,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub hub: MidiHub,
    pub pads: PadPoller,
    pub tx: Sender<ControlEvent>,
    pub rx: Receiver<ControlEvent>,
    pub monitor: VecDeque<ControlEvent>,
    /// Index into `map.bindings` currently being (re)learned, if any.
    pub learn: Option<usize>,
    pub learner: Learn,
    pub last_rescan: Instant,
    pub status: Option<String>,
    pub status_is_error: bool,
    /// Session-scoped document undo; snapshots taken by diffing against
    /// `baseline` at each frame boundary. See [`crate::undo`].
    pub history: crate::undo::History<ControlMap>,
    /// The map as of the last committed undo step — the frame-boundary diff
    /// compares against this to detect an edit.
    baseline: ControlMap,
}

impl CtlApp {
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let path = vidiotic_ctl::store::global_map_path();
        let map = vidiotic_ctl::store::load_global();
        Self {
            baseline: map.clone(),
            history: crate::undo::History::default(),
            map,
            path: Some(path),
            dirty: false,
            hub: MidiHub::new(tx.clone()),
            pads: PadPoller::new(),
            tx,
            rx,
            monitor: VecDeque::with_capacity(MONITOR_CAP),
            learn: None,
            learner: Learn::new(),
            // Elapsed already past the interval so the first frame rescans immediately.
            last_rescan: Instant::now() - RESCAN_INTERVAL,
            status: None,
            status_is_error: false,
        }
    }

    fn set_status(&mut self, msg: String, is_error: bool) {
        if is_error {
            log::error!("{msg}");
        }
        self.status = Some(msg);
        self.status_is_error = is_error;
    }

    pub fn save(&mut self) {
        let path = self
            .path
            .clone()
            .unwrap_or_else(vidiotic_ctl::store::global_map_path);
        match vidiotic_ctl::store::save_map(&path, &self.map) {
            Ok(()) => {
                self.dirty = false;
                self.path = Some(path.clone());
                self.set_status(format!("saved {}", path.display()), false);
            }
            Err(err) => self.set_status(format!("save failed: {err}"), true),
        }
    }

    pub fn save_as(&mut self, path: PathBuf) {
        self.path = Some(path);
        self.save();
    }

    pub fn revert(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match vidiotic_ctl::store::load_map(&path) {
            Ok(map) => {
                self.map = map;
                self.dirty = false;
                self.learn = None;
                self.reset_history();
                self.set_status(format!("reverted to {}", path.display()), false);
            }
            Err(err) => self.set_status(format!("revert failed: {err}"), true),
        }
    }

    pub fn open(&mut self, path: PathBuf) {
        match vidiotic_ctl::store::load_map(&path) {
            Ok(map) => {
                self.map = map;
                self.path = Some(path);
                self.dirty = false;
                self.learn = None;
                self.reset_history();
            }
            Err(err) => self.set_status(format!("open failed: {err}"), true),
        }
    }

    pub fn start_learn(&mut self, idx: usize) {
        self.learn = Some(idx);
        self.learner = Learn::new();
    }

    /// Append a placeholder binding and immediately start learning its
    /// source (the placeholder key is never a real binding target — it's
    /// overwritten the moment `learn` captures something).
    pub fn add_binding(&mut self) {
        self.map.bindings.push(Binding {
            source: ControlSource::Key {
                key: String::new(),
                ctrl: false,
                alt: false,
                shift: false,
                cmd: false,
            },
            action: Action::Nothing,
        });
        self.dirty = true;
        self.start_learn(self.map.bindings.len() - 1);
    }

    pub fn remove_binding(&mut self, idx: usize) {
        if idx >= self.map.bindings.len() {
            return;
        }
        self.map.bindings.remove(idx);
        self.dirty = true;
        self.learn = match self.learn {
            Some(j) if j == idx => None,
            Some(j) if j > idx => Some(j - 1),
            other => other,
        };
    }

    /// Fold this frame's edits into a single undo step. Run at the end of the
    /// frame, after the panels have mutated `map`.
    ///
    /// Skipped mid-learn so that adding a binding (which pushes a placeholder
    /// and immediately starts learning) and capturing its source land as one
    /// step, not two — the diff only commits once learn resolves.
    fn commit_undo(&mut self) {
        if self.learn.is_none() && self.map != self.baseline {
            self.history.record(self.baseline.clone());
            self.baseline = self.map.clone();
        }
    }

    /// Clear undo/redo and re-baseline to the current map — for when the
    /// document is replaced wholesale (open, revert).
    fn reset_history(&mut self) {
        self.history.reset();
        self.baseline = self.map.clone();
    }

    pub fn undo(&mut self) {
        match self.history.undo(self.map.clone()) {
            Some(prev) => {
                self.map = prev;
                // Re-baseline so `commit_undo` doesn't re-detect this restore
                // as a fresh edit next frame.
                self.baseline = self.map.clone();
                self.dirty = true;
                self.learn = None;
                self.set_status("undo".to_string(), false);
            }
            None => self.set_status("nothing to undo".to_string(), false),
        }
    }

    pub fn redo(&mut self) {
        match self.history.redo(self.map.clone()) {
            Some(next) => {
                self.map = next;
                self.baseline = self.map.clone();
                self.dirty = true;
                self.learn = None;
                self.set_status("redo".to_string(), false);
            }
            None => self.set_status("nothing to redo".to_string(), false),
        }
    }

    /// This frame's keys: the undo/redo chord acted on directly, everything
    /// else offered to the monitor and the learn session through the same
    /// channel device input arrives on.
    ///
    /// `history` is false during a learn session — there a keypress is the
    /// binding being captured, so Cmd+Z should be learnable rather than undo.
    /// Key-repeats are dropped: this window has no frame-steppers, and a held
    /// key is not a new actuation to learn or to log.
    fn pump_keys(&mut self, ctx: &egui::Context, history: bool) {
        let (mut undo, mut redo) = (false, false);
        for (ev, repeat) in egui_keys::key_events(ctx) {
            if history {
                match egui_keys::history_chord(&ev, repeat) {
                    Some(egui_keys::History::Undo) => undo = true,
                    Some(egui_keys::History::Redo) => redo = true,
                    None => {}
                }
            }
            // Offered to the monitor either way, chord included: this window's
            // live monitor is a debugging surface, and a chord that fired is
            // exactly the sort of thing somebody is looking for in it.
            if !repeat {
                let _ = self.tx.send(ev);
            }
        }
        // Undo wins a frame that somehow contained both.
        if undo {
            self.undo();
        } else if redo {
            self.redo();
        }
    }

    fn ingest(&mut self, ev: ControlEvent) {
        if let Some(idx) = self.learn {
            if let Some(source) = self.learner.observe(&ev) {
                if let Some(binding) = self.map.bindings.get_mut(idx) {
                    binding.source = source;
                    self.dirty = true;
                }
                self.learn = None;
            }
        }
        self.monitor.push_back(ev);
        if self.monitor.len() > MONITOR_CAP {
            self.monitor.pop_front();
        }
    }
}

impl Default for CtlApp {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for CtlApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        phosphor::shell::begin_frame(&ctx);

        self.pads.poll(&self.tx);

        if self.last_rescan.elapsed() >= RESCAN_INTERVAL {
            self.hub.rescan();
            self.last_rescan = Instant::now();
        }

        if !ctx.egui_wants_keyboard_input() {
            // Not while learning: there, a keypress is the binding being
            // captured, so Cmd+Z should be learnable rather than undo.
            self.pump_keys(&ctx, self.learn.is_none());
        }

        while let Ok(ev) = self.rx.try_recv() {
            self.ingest(ev);
        }

        crate::panels::draw(self, ui);

        // After the panels have mutated `map`, fold the frame's edits into one
        // undo step.
        self.commit_undo();

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(key: &str) -> Binding {
        Binding {
            source: ControlSource::Key {
                key: key.to_string(),
                ctrl: false,
                alt: false,
                shift: false,
                cmd: false,
            },
            action: Action::Nothing,
        }
    }

    /// A headless editor with a known, empty document — `new()` loads the real
    /// global map from disk, so tests re-baseline to a clean slate.
    fn app() -> CtlApp {
        let mut app = CtlApp::new();
        app.map = ControlMap::default();
        app.reset_history();
        app
    }

    #[test]
    fn undo_and_redo_an_edit() {
        let mut app = app();
        app.map.bindings.push(binding("a"));
        app.commit_undo();
        assert_eq!(app.map.bindings.len(), 1);

        app.undo();
        assert!(
            app.map.bindings.is_empty(),
            "undo removes the added binding"
        );

        app.redo();
        assert_eq!(app.map.bindings.len(), 1, "redo restores it");
    }

    /// The frame is the coalescing unit: several edits before one `commit_undo`
    /// are a single step.
    #[test]
    fn one_frame_of_edits_is_one_undo_step() {
        let mut app = app();
        app.map.bindings.push(binding("a"));
        app.map.bindings.push(binding("b"));
        app.commit_undo();

        app.undo();
        assert!(
            app.map.bindings.is_empty(),
            "one undo reverts the whole frame"
        );
    }

    /// Adding a binding starts a learn session; the edit isn't committed until
    /// learn ends, so add + capture collapse into one undoable step.
    #[test]
    fn edits_during_learn_defer_until_it_ends() {
        let mut app = app();

        app.learn = Some(0);
        app.map.bindings.push(binding("placeholder"));
        app.commit_undo(); // learning → nothing recorded yet

        app.learn = None;
        app.map.bindings[0] = binding("learned");
        app.commit_undo(); // now one step: empty → learned binding

        app.undo();
        assert!(
            app.map.bindings.is_empty(),
            "add + learn are one undoable step"
        );
    }

    #[test]
    fn undo_with_empty_history_is_a_noop() {
        let mut app = app();
        app.undo();
        assert_eq!(app.status.as_deref(), Some("nothing to undo"));
    }

    /// Replacing the document from disk (open/revert calls `reset_history`)
    /// drops the stack, so undo can't restore over a freshly loaded map.
    #[test]
    fn resetting_history_drops_undo() {
        let mut app = app();
        app.map.bindings.push(binding("a"));
        app.commit_undo();

        app.reset_history(); // stands in for open/revert
        app.undo();
        assert_eq!(app.map.bindings.len(), 1, "reset cleared the history");
        assert_eq!(app.status.as_deref(), Some("nothing to undo"));
    }
}
