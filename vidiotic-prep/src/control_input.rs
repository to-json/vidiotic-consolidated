//! The devices prep binds input through, and the tables that edit them.
//!
//! The *table* — which key does what, and how an `Action` becomes a
//! [`Command`] — is [`vidiotic_chop::keymap`], shared with the browser shell.
//! What is here is everything that feeds it and none of it exists in a browser:
//! CoreMIDI ports, gamepad polling, the user's `prep.vmap` on disk, and the
//! learn session that rebinds a row by pressing the thing.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use vidiotic_ctl::{
    Action, Binding, ControlEvent, ControlMap, ControlSource, EventValue, Learn, Mapper, MidiHub,
    PadPoller,
};

use vidiotic_chop::commands::Command;
pub use vidiotic_chop::keymap::default_map;
use vidiotic_chop::keymap::resolve;

/// How often the control-mapping `MidiHub` rescans for new/vanished ports —
/// same interval as the ctl bin; `midir` exposes no CoreMIDI hotplug.
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Live control-event monitor ring-buffer capacity (small: this is a corner
/// of the inspector, not a dedicated monitor window like the ctl bin's).
const MONITOR_CAP: usize = 40;

/// Which binding table's row is currently capturing a source. There is one
/// learn session across both tables — two at once would race for the next
/// actuation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LearnTarget {
    /// A row of the project's player-map layer (`controls`), bound for the
    /// `.viproj` and resolved by `vidiotic`.
    PlayerMap(usize),
    /// A row of prep's own map (`over`), persisted to `prep.vmap`.
    PrepMap(usize),
}

impl LearnTarget {
    /// The row being learned in the player table, if that's the active target.
    #[must_use]
    pub fn player_row(self) -> Option<usize> {
        match self {
            Self::PlayerMap(i) => Some(i),
            Self::PrepMap(_) => None,
        }
    }

    /// The row being learned in prep's own table, if that's the active target.
    #[must_use]
    pub fn prep_row(self) -> Option<usize> {
        match self {
            Self::PrepMap(i) => Some(i),
            Self::PlayerMap(_) => None,
        }
    }
}

/// Every device, map and learn session prep binds input through.
///
/// This is one struct rather than a dozen `PrepApp` fields for a reason that is
/// only half tidiness: the inspector draws its binding tables *inside* the same
/// scroll area as the span list, so the shell has to hand the panel layer a
/// closure that edits all of this while `&mut Editor` is already borrowed out
/// of the same `PrepApp`. Two disjoint fields destructure; twelve scattered
/// ones do not.
///
/// It is also, precisely, the part of prep with no browser answer yet —
/// CoreMIDI ports, gamepad polling, a `prep.vmap` on disk (web-port.md §2
/// defers all of it behind a WebMIDI shim).
pub struct Controls {
    /// This project's control-mapping layer (layered over the user's global
    /// map at resolve time in `vidiotic`; project wins). Prep only edits and
    /// persists it — this is the *player's* map, and prep never resolves it.
    /// Prep's own bindings are `mapper`.
    pub project: ControlMap,
    /// Prep's own key/MIDI/gamepad bindings: [`default_map`] as the base layer,
    /// the user's `prep.vmap` over it.
    pub mapper: Mapper,
    /// The user's global map, loaded once at startup for the read-only
    /// "global (read-only)" list under the project layer's binding table.
    pub global: ControlMap,
    pub hub: MidiHub,
    pub pads: PadPoller,
    tx: crossbeam_channel::Sender<ControlEvent>,
    rx: crossbeam_channel::Receiver<ControlEvent>,
    pub monitor: VecDeque<ControlEvent>,
    /// The one active learn session, across both binding tables.
    pub learn: Option<LearnTarget>,
    /// Set when prep's own map changes, so it's only rewritten on edit.
    dirty: bool,
    learner: Learn,
    last_rescan: Instant,
}

impl Default for Controls {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            project: ControlMap::default(),
            mapper: mapper(),
            global: vidiotic_ctl::store::load_global(),
            hub: MidiHub::new(tx.clone()),
            pads: PadPoller::new(),
            tx,
            rx,
            monitor: VecDeque::with_capacity(MONITOR_CAP),
            learn: None,
            dirty: false,
            learner: Learn::new(),
            // Elapsed already past the interval so the first frame rescans immediately.
            last_rescan: Instant::now() - RESCAN_INTERVAL,
        }
    }
}

impl Controls {
    /// Poll gamepads and rescan MIDI ports on a timer. Call once per frame.
    pub fn poll_devices(&mut self) {
        self.pads.poll(&self.tx);
        if self.last_rescan.elapsed() >= RESCAN_INTERVAL {
            self.hub.rescan();
            self.last_rescan = Instant::now();
        }
    }

    /// Device events queued since the last call.
    pub fn drain_device_events(&mut self) -> Vec<ControlEvent> {
        self.rx.try_iter().collect()
    }

    /// Learn, monitor, and resolve one control event into a command.
    ///
    /// `repeat` is the OS's key-repeat flag, which the device channel can't
    /// carry — so keys are handed here inline rather than round-tripping.
    pub fn observe(&mut self, ev: ControlEvent, repeat: bool) -> Option<Command> {
        // Undo/redo are reserved accelerator chords, resolved ahead of the
        // mapper (and of learn, so they can't be captured as a binding).
        // Cmd+Z on mac, Ctrl+Z elsewhere; Shift or `y` for redo. Only on the
        // press edge, and only when a text field isn't eating keys — the caller
        // already gates that, and egui's TextEdit keeps its own inline undo.
        if !repeat && matches!(ev.value, EventValue::Pressed) {
            if let ControlSource::Key {
                key,
                ctrl,
                alt,
                shift,
                cmd,
            } = &ev.source
            {
                let accel = (*ctrl || *cmd) && !*alt;
                if accel && key == "z" {
                    return Some(if *shift { Command::Redo } else { Command::Undo });
                }
                if accel && !*shift && key == "y" {
                    return Some(Command::Redo);
                }
            }
        }
        // Key-repeat is an artifact of holding a key down, not a new
        // actuation: it must not be captured as a binding or clutter the
        // monitor. `resolve` decides which commands still want it (only the
        // frame-steppers).
        if !repeat {
            // A learn session consumes the event outright — the actuation that
            // *names* a binding must not also fire it.
            if let Some(target) = self.learn {
                if let Some(source) = self.learner.observe(&ev) {
                    match target {
                        LearnTarget::PlayerMap(i) => {
                            if let Some(b) = self.project.bindings.get_mut(i) {
                                b.source = source;
                            }
                        }
                        LearnTarget::PrepMap(i) => {
                            if let Some(b) = self.mapper.over.bindings.get_mut(i) {
                                b.source = source;
                                self.dirty = true;
                            }
                        }
                    }
                    self.learn = None;
                }
                self.push_monitor(ev);
                return None;
            }
            self.push_monitor(ev.clone());
        }
        resolve(&mut self.mapper, ev.source, ev.value, repeat)
    }

    fn push_monitor(&mut self, ev: ControlEvent) {
        self.monitor.push_back(ev);
        if self.monitor.len() > MONITOR_CAP {
            self.monitor.pop_front();
        }
    }

    pub fn start_learn(&mut self, target: LearnTarget) {
        self.learn = Some(target);
        self.learner = Learn::new();
    }

    /// A binding with no source yet — the placeholder a new row learns into.
    fn unbound() -> Binding {
        Binding {
            source: ControlSource::Key {
                key: String::new(),
                ctrl: false,
                alt: false,
                shift: false,
                cmd: false,
            },
            action: Action::Nothing,
        }
    }

    /// Fix up the learn session after row `removed` was deleted from one
    /// table: drop it if that *was* its row, shift it down if it sat above.
    /// A session on the other table is unaffected — its indices didn't move.
    fn reindex_learn_after_remove(
        &mut self,
        row_of: fn(LearnTarget) -> Option<usize>,
        table: fn(usize) -> LearnTarget,
        removed: usize,
    ) {
        let Some(current) = self.learn else { return };
        let Some(row) = row_of(current) else { return };
        self.learn = match row {
            r if r == removed => None,
            r if r > removed => Some(table(r - 1)),
            _ => Some(current),
        };
    }

    /// Append a placeholder binding to the project layer and immediately
    /// start learning its source.
    pub fn add_project_binding(&mut self) {
        self.project.bindings.push(Self::unbound());
        self.start_learn(LearnTarget::PlayerMap(self.project.bindings.len() - 1));
    }

    pub fn remove_project_binding(&mut self, idx: usize) {
        if idx >= self.project.bindings.len() {
            return;
        }
        self.project.bindings.remove(idx);
        self.reindex_learn_after_remove(LearnTarget::player_row, LearnTarget::PlayerMap, idx);
    }

    /// Add a project-layer binding that masks a global-layer one: same
    /// source, `Action::Nothing`.
    pub fn mask_global_binding(&mut self, source: ControlSource) {
        self.project.bindings.push(Binding {
            source,
            action: Action::Nothing,
        });
    }

    /// Append a placeholder binding to prep's own map and start learning it.
    pub fn add_prep_binding(&mut self) {
        self.mapper.over.bindings.push(Self::unbound());
        self.dirty = true;
        self.start_learn(LearnTarget::PrepMap(self.mapper.over.bindings.len() - 1));
    }

    pub fn remove_prep_binding(&mut self, idx: usize) {
        if idx >= self.mapper.over.bindings.len() {
            return;
        }
        self.mapper.over.bindings.remove(idx);
        self.dirty = true;
        self.reindex_learn_after_remove(LearnTarget::prep_row, LearnTarget::PrepMap, idx);
    }

    /// Add an override that suppresses a built-in default outright.
    pub fn mask_prep_default(&mut self, source: ControlSource) {
        self.mapper.over.bindings.push(Binding {
            source,
            action: Action::Nothing,
        });
        self.dirty = true;
    }

    /// Clear every override, restoring the built-in keys. The undo for a
    /// binding table that autosaves.
    pub fn reset_prep_map(&mut self) {
        self.mapper.over = ControlMap::default();
        self.learn = None;
        self.dirty = true;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Persist prep's own bindings if they changed. Unlike the `.vprep`
    /// sidecar this is global user config, so it's written on edit rather than
    /// on a timer.
    ///
    /// # Errors
    /// Propagates the store's write failure so the caller can surface it.
    pub fn flush_prep_map(&mut self) -> anyhow::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        vidiotic_ctl::store::save_prep(&self.mapper.over)
    }
}

/// Build prep's mapper: the built-in defaults under the user's `prep.vmap`.
///
/// Deliberately *not* layered over `global.vmap` — that map speaks the
/// player's verbs, and one of its bindings landing on a prep default's source
/// would suppress the default (any match in the upper layer wins) and then
/// resolve to a verb [`to_command`] rejects, silently killing the key.
#[must_use]
pub fn mapper() -> Mapper {
    Mapper::new(default_map(), vidiotic_ctl::store::load_prep())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removing a row shifts the ones above it, so an in-flight learn session
    /// has to move with them or it captures into the wrong binding.
    #[test]
    fn removing_a_row_reindexes_an_active_learn_session() {
        let mut c = Controls::default();
        c.add_prep_binding();
        c.add_prep_binding();
        c.add_prep_binding();
        c.start_learn(LearnTarget::PrepMap(2));

        c.remove_prep_binding(0);
        assert_eq!(
            c.learn,
            Some(LearnTarget::PrepMap(1)),
            "the session must follow its row"
        );

        c.remove_prep_binding(1);
        assert_eq!(c.learn, None, "removing the learned row ends the session");
    }

    /// The two tables have independent indices — deleting from one must not
    /// disturb a session on the other.
    #[test]
    fn removing_a_row_leaves_the_other_tables_learn_session_alone() {
        let mut c = Controls::default();
        c.add_project_binding();
        c.add_prep_binding();
        c.add_prep_binding();
        c.start_learn(LearnTarget::PlayerMap(0));

        c.remove_prep_binding(0);
        assert_eq!(c.learn, Some(LearnTarget::PlayerMap(0)));
    }

    /// Prep's own keys are a user preference: they belong in prep.vmap, never
    /// in the project's map (which ships inside the .viproj to the player).
    #[test]
    fn prep_bindings_stay_out_of_the_project_map() {
        let mut c = Controls::default();
        c.add_prep_binding();
        assert!(
            c.project.bindings.is_empty(),
            "prep's keys must not enter the player's map"
        );
        assert_eq!(c.mapper.over.bindings.len(), 1);
    }

    #[test]
    fn resetting_prep_bindings_restores_the_built_in_defaults() {
        let mut c = Controls::default();
        c.add_prep_binding();
        c.reset_prep_map();
        assert!(c.mapper.over.bindings.is_empty());
        assert!(
            c.learn.is_none(),
            "reset must end any session pointing into the cleared map"
        );
        // The base layer is the shared table, and is untouched.
        assert!(!c.mapper.base.bindings.is_empty());
    }
}
