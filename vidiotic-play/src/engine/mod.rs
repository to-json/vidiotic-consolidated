//! The engine: the clock, the sequencer, the clip pool, the cue banks, the
//! modal grammar and the undo stack — everything a vidiotic session *is*,
//! with none of the machine it runs on.
//!
//! This is `vidiotic::app` with the OS taken out. It was extracted rather than
//! reimplemented, which is the whole point: `/play` in a browser was previously
//! a parallel shell with its own state, and the moment cues and banks reached it
//! there would have been two implementations of cue rotation drifting apart. Now
//! there is one, and the shells are what differ — windows, audio devices, IPC,
//! the filesystem, and [`source::Opener`].
//!
//! # What a shell owes the engine
//!
//! Three things, once per tick:
//!
//! 1. feed it commands — [`Engine::dispatch`] is the choke point, and it hands
//!    back anything it does not implement so the shell can;
//! 2. call [`Engine::tick`], which advances the clock and the rotation and
//!    returns what to draw;
//! 3. do the GPU work the [`Tick`] describes.
//!
//! Everything else — where a `.viproj` lives, whether there is a window, what a
//! camera is — is the shell's business and the engine names none of it.
//!
//! # Public fields
//!
//! Most of the state is `pub`. That is deliberate and not laziness: the native
//! shell's mirror builder reads about forty of these to publish the UI's
//! read-only view, and forty accessors would be forty places for the two to
//! disagree. Anything with an invariant to keep — bank indices, cue ids, the
//! sequencer's active set — is behind a method, and those methods are the only
//! way the invariant is maintained.

mod cameras;
mod history;
mod session;
pub mod source;
mod verbs;

use std::collections::{HashMap, VecDeque};

use web_time::Instant;

use crate::analysis::AudioFrame;
use crate::bank::{Bank, Cue, CueId};
use crate::chain::{ChainSlot, ClipId};
use crate::clippool::{Clip, ClipBank, ClipSource};
use crate::clock::{BoundaryTracker, ClockSnapshot, ClockSource, InternalClock, TapTempo};
use crate::commands::{
    BankView, Cadence, ClipBankView, ClipEntry, ClipRole, Command, CueView, SyncKind, TimeSig,
    UiMirror, LOOP_TICKS_PER_BEAT,
};
use crate::grammar::{Grammar, Pane};
use crate::sequencer::{CueStep, Sequencer, SequencerEvent};
use crate::undo::UndoStack;
use crate::video::frame::DecodedFrame;

pub use source::{NoSources, OpenRequest, Opener, Source};

/// How long the statusline says "nothing here" after a root pressed with
/// nothing under it. Long enough to read mid-performance, short enough that it
/// is gone before the next sequence starts.
const EMPTY_PREFIX_LINGER: std::time::Duration = std::time::Duration::from_millis(1200);

/// The loop-rate detent ladder the grammar's Tune knob steps through: inherit,
/// off, then the shared cadences (same ticks as the editor's combo choices).
const LOOP_LADDER: [Option<u32>; 10] = [
    None,
    Some(0),
    Some(16),
    Some(32),
    Some(64),
    Some(128),
    Some(256),
    Some(512),
    Some(1024),
    Some(2048),
];

/// The portable half of a session's startup state.
///
/// The native shell fills this from CLI arguments and a `.viproj`; the browser
/// fills it with defaults and then loads clips into the running engine. Anything
/// a browser could not produce — an audio device, a socket, a shader path —
/// is deliberately absent, so this struct cannot grow a native-only field
/// without the wasm build noticing.
pub struct Boot {
    pub bpm: f64,
    pub time_sig: TimeSig,
    pub phrase_cadence: Cadence,
    pub loop_cadence: Option<Cadence>,
    pub clips: Vec<Clip>,
    pub clip_banks: Vec<ClipBank>,
    /// Cue banks to seed the sequencer with; empty ⇒ one default bank "A".
    pub cue_banks: Vec<Bank>,
    /// Clips to activate at startup (`--clip`), each becoming a full-length cue
    /// in the live bank. Ignored when `cue_banks` is non-empty — a loaded
    /// project already says what should be playing.
    pub auto_active: Vec<ClipId>,
    pub preserve_playhead: bool,
    pub advanced: bool,
    pub opener: Box<dyn Opener>,
}

impl Default for Boot {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            time_sig: TimeSig::default(),
            phrase_cadence: Cadence::default(),
            loop_cadence: None,
            clips: Vec::new(),
            clip_banks: Vec::new(),
            cue_banks: Vec::new(),
            auto_active: Vec::new(),
            preserve_playhead: true,
            advanced: false,
            opener: Box::new(NoSources),
        }
    }
}

/// What one [`Engine::tick`] produced, for the shell to put on a GPU.
///
/// The engine does no drawing and holds no device, so everything a tick decides
/// about pixels comes back here rather than being done in place. That is what
/// lets the same tick drive a winit window and a canvas in another document.
pub struct Tick {
    /// This tick's clock reading. The shell needs it for the beat uniforms, and
    /// re-reading the clock would give a different (later) answer.
    pub snap: ClockSnapshot,
    /// A re-loop grid boundary was crossed this tick. Only interesting to a
    /// shell that quantizes something else to the same grid — natively, camera
    /// delay re-targeting.
    pub boundary_crossed: bool,
    /// A newly decoded frame to upload, if the current source produced one.
    pub frame: Option<DecodedFrame>,
    /// The current cue has no source (a camera off-air, a failed spawn, a clip
    /// the browser has no bytes for), and the output should be blanked once
    /// rather than left showing the previous cue.
    pub blank: bool,
    /// The playing cue's effect chain, or empty for the live shader.
    pub chain: Vec<ChainSlot>,
}

/// The engine. See the module docs.
pub struct Engine {
    // --- tempo and rotation ---
    pub clock: Box<dyn ClockSource>,
    /// Which clock source is installed. The engine cannot *build* a `LinkClock`
    /// — Link is a LAN protocol — so switching is a shell command; this records
    /// the answer for the UI and for a project save.
    pub sync: SyncKind,
    pub time_sig: TimeSig,
    /// Source-of-truth cadences: re-resolved against `time_sig` into the
    /// sequencer's dwell / `loop_len` on every mutation (see [`Self::apply_cadences`]).
    pub phrase_cadence: Cadence,
    pub loop_cadence: Option<Cadence>,
    pub sequencer: Sequencer,

    // --- the pool ---
    pub clips: Vec<Clip>,
    /// Clip banks group the flat pool for the UI; `ClipId`s stay globally unique
    /// so cues are unaffected. `active_clip_bank` is the one the grid shows.
    pub clip_banks: Vec<ClipBank>,
    pub active_clip_bank: usize,
    pub next_clip_id: ClipId,
    /// The pool grid's clip cursor: target of the pool pane's Go/Make verbs
    /// (and click-select in the grid).
    pub selected_clip: Option<ClipId>,

    // --- cues ---
    /// Cue banks: the sequencer plays `live_bank`; the UI edits `edit_bank` (they
    /// can differ so you play one set while modifying another). Sources and
    /// `current` are keyed by cue, not clip.
    pub banks: Vec<Bank>,
    pub live_bank: usize,
    pub edit_bank: usize,
    pub selected_cue: Option<CueId>,
    pub next_cue_id: CueId,

    // --- playback ---
    /// Open sources, keyed by cue. Never more than the playing and armed cues
    /// (see [`Self::retain_decoders`]).
    pub decoders: HashMap<CueId, Box<dyn Source>>,
    opener: Box<dyn Opener>,
    pub current: Option<CueId>,
    /// Playhead of the displayed clip, for set-in/out-to-playhead.
    pub current_pts: f64,
    /// The pixel layout of the frame on the GPU, as `preamble.frag`'s `video()`
    /// reads it.
    pub video_mode: i32,
    pub last_beat: f64,
    /// Most recent snapshot tempo, for spawn-time BPM-synced speed.
    pub last_bpm: f64,
    /// The cue a placeholder black frame was last reported for, so a source-less
    /// cue blanks the output once instead of on every tick.
    blanked_for: Option<CueId>,

    // --- timing knobs ---
    /// Musical re-loop: force the current clip back to its start on a beat grid,
    /// measured in 1/32-beat ticks. `None` = let the clip loop on EOF only.
    pub loop_len: Option<u32>,
    pub loop_tracker: BoundaryTracker,
    /// On a cut, carry the outgoing playhead into the incoming clip (true, the
    /// default) or restart the incoming clip from its start.
    pub preserve_playhead: bool,
    /// Advanced sequencer mode: when on, per-cue dwell/loop/offset/speed take
    /// effect and the extended UI shows. Off (default) reproduces the simple
    /// global-phrase behavior; per-cue edits are still stored, just inert.
    pub advanced: bool,
    /// Traditional tap-tempo: recent tap instants, averaged into a BPM.
    pub tap: TapTempo,

    // --- input model ---
    /// Modal command grammar: when on, token keys/pads/notes drive verb
    /// sequences (and mask their direct bindings).
    pub grammar_on: bool,
    pub command_palette_open: bool,
    pub grammar: Grammar,
    pub focused_pane: Pane,
    pub prev_pane: Pane,

    /// The most recently completed grammar verb, as its `Debug` spelling.
    ///
    /// A readout, not state: it exists because a verb that resolves and then
    /// does nothing — because this shell has no cameras, or no filesystem — is
    /// indistinguishable from a broken grammar unless something says which one
    /// fired. `Debug` rather than a display impl for exactly that reason: the
    /// variant name is the useful part.
    pub last_verb: Option<String>,

    /// The label of a root pressed with nothing bound under it, and when.
    ///
    /// Also a readout. An option-less root opens nothing (see
    /// [`grammar::Step::Empty`]), and a press that silently does nothing reads
    /// as broken gear; this is what lets the statusline say "nothing here"
    /// instead. Cleared by time, in [`Self::build_mirror`].
    pub empty_prefix: Option<(&'static str, Instant)>,

    /// Session-scoped document undo for cue/bank authoring. See [`crate::undo`].
    pub undo: UndoStack,

    /// Commands raised by the engine itself — grammar verbs resolve into these.
    /// A shell drains them through [`Self::dispatch`] in the same tick, which is
    /// what keeps verbs and direct commands on one apply path.
    pending: VecDeque<Command>,
}

impl Engine {
    /// Build a session.
    #[must_use]
    pub fn new(boot: Boot) -> Self {
        // A loaded project seeds cue banks; otherwise start with one empty "A".
        let seeded = !boot.cue_banks.is_empty();
        let next_clip_id = boot.clips.iter().map(|c| c.id).max().map_or(0, |m| m + 1);
        let cue_banks = if seeded {
            boot.cue_banks
        } else {
            vec![Bank::new("A")]
        };
        let next_cue_id = cue_banks
            .iter()
            .flat_map(Bank::ids)
            .max()
            .map_or(1, |m| m + 1);
        let mut engine = Self {
            clock: Box::new(InternalClock::new(boot.bpm, boot.time_sig.quantum())),
            sync: SyncKind::Internal,
            time_sig: boot.time_sig,
            phrase_cadence: boot.phrase_cadence,
            loop_cadence: boot.loop_cadence,
            sequencer: Sequencer::new(boot.phrase_cadence.beats(boot.time_sig)),
            clips: boot.clips,
            clip_banks: boot.clip_banks,
            active_clip_bank: 0,
            next_clip_id,
            selected_clip: None,
            banks: cue_banks,
            live_bank: 0,
            edit_bank: 0,
            selected_cue: None,
            next_cue_id,
            decoders: HashMap::new(),
            opener: boot.opener,
            current: None,
            current_pts: 0.0,
            video_mode: 0,
            last_beat: 0.0,
            last_bpm: boot.bpm,
            blanked_for: None,
            loop_len: boot.loop_cadence.map(|c| c.ticks(boot.time_sig)),
            loop_tracker: BoundaryTracker::new(),
            preserve_playhead: boot.preserve_playhead,
            advanced: boot.advanced,
            tap: TapTempo::default(),
            grammar_on: false,
            command_palette_open: false,
            grammar: Grammar::default(),
            focused_pane: Pane::default(),
            prev_pane: Pane::default(),
            last_verb: None,
            empty_prefix: None,
            undo: UndoStack::default(),
            pending: VecDeque::new(),
        };
        if !seeded {
            for id in boot.auto_active {
                engine.toggle_clip_active(id, 0.0);
            }
        }
        engine
    }

    /// Replace the source opener.
    ///
    /// A shell that has to build its opener *after* the engine — the browser's
    /// needs a GPU device to exist first — sets it here rather than deferring
    /// the whole engine.
    pub fn set_opener(&mut self, opener: Box<dyn Opener>) {
        self.opener = opener;
    }

    /// Advance the clock, the rotation, and the current source by one frame.
    ///
    /// The shell does the GPU half from the returned [`Tick`]. Steps that need
    /// an OS — camera delay resolution, shader hot-reload — are the shell's, and
    /// are why `boundary_crossed` is reported rather than consumed here.
    pub fn tick(&mut self, now: Instant) -> Tick {
        // Clock and sequencer.
        let snap = self.clock.snapshot();
        self.last_beat = snap.beat;
        self.last_bpm = snap.bpm;
        let ev = self.sequencer.tick(&snap);
        self.apply_seq_events(ev);

        // Musical re-loop: on each grid boundary (in 1/32-beat ticks), seek the
        // current clip back to its start so it restarts on the beat. The
        // rate/phase come from the playing cue in advanced mode, else the global
        // loop setting; a loop phase shifts the grid for swing/micro-timing.
        let (loop_ticks, loop_phase) = self.current_loop_params();
        let mut boundary_crossed = false;
        if let (Some(ticks), Some(cur)) = (loop_ticks, self.current) {
            let grid = f64::from(ticks) / f64::from(LOOP_TICKS_PER_BEAT);
            if snap.is_playing
                && self
                    .loop_tracker
                    .crossed(snap.beat - loop_phase, grid)
                    .is_some()
            {
                boundary_crossed = true;
                if let Some(h) = self.decoders.get_mut(&cur) {
                    // No-op for cameras — a live feed has nothing to seek. The
                    // crossing still feeds quantized delay re-targeting.
                    h.request_restart();
                }
            }
        } else {
            self.loop_tracker.reset();
        }

        // Pull the newest frame from the current source.
        let mut blank = false;
        let mut frame = None;
        if let Some(cur) = self.current {
            if !self.decoders.contains_key(&cur) && self.blanked_for != Some(cur) {
                self.blanked_for = Some(cur);
                self.current_pts = 0.0;
                self.video_mode = 0;
                blank = true;
            }
            frame = self.decoders.get_mut(&cur).and_then(|h| h.poll_newest(now));
            if let Some(f) = &frame {
                self.blanked_for = None;
                self.current_pts = f.pts_sec;
                self.video_mode = f.pixels.video_mode();
            }
        }

        // Point the renderer at the playing cue's effect chain, or an empty
        // chain (the live shader) when the cue has none.
        let chain = self
            .current
            .and_then(|c| self.live_cue(c))
            .map(|cue| cue.chain.clone())
            .unwrap_or_default();

        Tick {
            snap,
            boundary_crossed,
            frame,
            blank,
            chain,
        }
    }

    /// Apply one command, or hand it back.
    ///
    /// `Some(cmd)` means "not mine" — a shell command (a file dialog, an audio
    /// device, a window) that this engine deliberately knows nothing about. It
    /// is returned rather than logged-and-dropped so that adding a command
    /// without teaching some shell about it is visible rather than silent.
    ///
    /// Undo bookkeeping is the caller's, because only the caller knows whether a
    /// command came from a person (undoable) or from IPC.
    #[must_use]
    pub fn apply_command(&mut self, cmd: Command) -> Option<Command> {
        match cmd {
            Command::SetBpm(b) => self.clock.set_bpm(b),
            Command::BpmDelta(d) => {
                let b = self.clock.snapshot().bpm + d;
                self.clock.set_bpm(b);
            }
            Command::NudgeBpm(r) => self.clock.nudge_bpm(r),
            Command::TapDownbeat => self.clock.tap_downbeat(),
            Command::TapTempo => self.tap_tempo(),
            Command::SoftReset => self.soft_reset(),
            Command::HardReset => self.hard_reset(),
            Command::SetTimeSig(ts) => {
                self.time_sig = ts.sanitized();
                self.clock.set_quantum(self.time_sig.quantum());
                self.apply_cadences();
                self.sequencer.reset_boundary();
            }
            Command::SetPhraseCadence(c) => {
                self.phrase_cadence = c;
                self.apply_cadences();
            }
            Command::SetLoopCadence(c) => {
                self.loop_cadence = c;
                self.apply_cadences();
            }
            Command::SetPreservePlayhead(on) => self.preserve_playhead = on,
            Command::ToggleClipActive(id) => self.toggle_clip_active(id, self.last_beat),
            Command::AddCue(clip) => self.add_cue(clip),
            Command::RemoveCue(id) => self.remove_cue(id),
            Command::SelectCue(id) => self.selected_cue = id,
            Command::SelectCueDelta(d) => self.select_cue_delta(d),
            Command::SelectCueFirst => {
                self.selected_cue = self.banks[self.edit_bank].cues.first().map(|c| c.id);
            }
            Command::SelectCueLast => {
                self.selected_cue = self.banks[self.edit_bank].cues.last().map(|c| c.id);
            }
            Command::SelectClip(id) => self.selected_clip = id,
            Command::SelectClipDelta(d) => self.select_clip_delta(d),
            Command::SelectClipFirst => {
                self.selected_clip = self.active_clip_ids().first().copied();
            }
            Command::SelectClipLast => {
                self.selected_clip = self.active_clip_ids().last().copied();
            }
            Command::SetCueIn(id, s) => {
                self.edit_cue(id, |c| {
                    c.in_sec = s.max(0.0);
                    normalize_cue_trim(c);
                });
            }
            Command::SetCueOut(id, s) => {
                self.edit_cue(id, |c| {
                    c.out_sec = s;
                    normalize_cue_trim(c);
                });
            }
            Command::SetCueInToPlayhead(id) => {
                if self.current == Some(id) {
                    let p = self.current_pts.max(0.0);
                    self.edit_cue(id, |c| {
                        c.in_sec = p;
                        normalize_cue_trim(c);
                    });
                }
            }
            Command::SetCueOutToPlayhead(id) => {
                if self.current == Some(id) {
                    let p = self.current_pts.max(0.0);
                    self.edit_cue(id, |c| {
                        c.out_sec = Some(p);
                        normalize_cue_trim(c);
                    });
                }
            }
            Command::SetCuePreserve(id, v) => self.edit_cue(id, |c| c.preserve = v),
            Command::SetCueChain(id, chain) => self.edit_cue(id, |c| c.chain = chain),
            Command::SetChainParam {
                cue,
                slot,
                name,
                value,
            } => {
                self.edit_cue(cue, |c| {
                    if let Some(s) = c.chain.get_mut(slot) {
                        s.set_param(name, value);
                    }
                });
            }
            Command::SetCueParam(id, p) => self.set_cue_param(id, p),
            Command::NudgeCueParam(kind, dir) => self.nudge_cue_param(kind, dir),
            Command::MoveCue(id, to) => self.move_cue(id, to),
            Command::SetClipBpm(id, bpm) => self.set_clip_bpm(id, bpm),
            Command::SetAdvancedMode(on) => self.set_advanced(on),
            Command::SetGrammarMode(on) => {
                self.grammar_on = on;
                self.grammar.reset();
            }
            Command::ToggleCommandPalette => {
                self.command_palette_open = !self.command_palette_open;
            }
            Command::AddBank => self.add_bank(),
            Command::CloneBank => self.clone_bank(),
            Command::SetLiveBank(i) => self.set_live_bank(i),
            Command::CycleLiveBank(d) => self.cycle_live_bank(d),
            Command::SetEditBank(i) => self.set_edit_bank(i),
            Command::SetActiveClipBank(i) => self.set_active_clip_bank(i),

            // Intercepted by the caller's dispatch loop before it gets here —
            // they act on the undo stack, not the document.
            Command::Undo | Command::Redo => {}

            // Not the engine's: a filesystem, an audio device, a window, a
            // camera, a sibling process, or a shader compile that needs a GPU.
            other => return Some(other),
        }
        None
    }

    /// Take the next command the engine raised for itself, if any.
    ///
    /// A shell drains this after its own command source, feeding each back
    /// through the same dispatch path, so a grammar verb and a UI click reach
    /// the document identically.
    pub fn next_pending(&mut self) -> Option<Command> {
        self.pending.pop_front()
    }

    /// Queue a command from the engine's own machinery (grammar verbs).
    pub(crate) fn raise(&mut self, cmd: Command) {
        self.pending.push_back(cmd);
    }

    /// Derive BPM from the spacing of recent taps.
    ///
    /// The estimator is [`crate::clock::TapTempo`] — pure timing with no OS in
    /// it, which is why both shells tap tempo through one average rather than
    /// two that could drift.
    pub fn tap_tempo(&mut self) {
        if let Some(bpm) = self.tap.tap(Instant::now()) {
            self.clock.set_bpm(bpm);
        }
    }

    pub fn set_loop_len(&mut self, ticks: Option<u32>) {
        self.loop_len = ticks;
        self.loop_tracker.reset();
    }

    /// Re-resolve `phrase_cadence`/`loop_cadence` against the current
    /// `time_sig` and push the concrete lengths into the sequencer and the loop
    /// grid. Called after any edit to the cadences or the signature.
    pub fn apply_cadences(&mut self) {
        let ev = self
            .sequencer
            .set_phrase_len(self.phrase_cadence.beats(self.time_sig));
        self.apply_seq_events(ev);
        self.set_loop_len(self.loop_cadence.map(|c| c.ticks(self.time_sig)));
    }

    /// The backing file of a pool clip, for file-sourced clips.
    #[must_use]
    pub fn clip_path(&self, id: ClipId) -> Option<std::path::PathBuf> {
        self.clips
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.file_path().map(std::path::Path::to_path_buf))
    }

    /// The capture-device uid behind a clip, if it's camera-sourced.
    #[must_use]
    pub fn clip_camera_uid(&self, id: ClipId) -> Option<std::sync::Arc<str>> {
        self.clips
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| match &c.source {
                ClipSource::Camera { uid, .. } => Some(uid.clone()),
                ClipSource::File(_) => None,
            })
    }

    #[must_use]
    pub fn clip_name(&self, id: ClipId) -> std::sync::Arc<str> {
        self.clips
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    /// Fill `out` with everything the *engine* knows: the clock readout, the
    /// clip pool and its banks, the cue banks and the edit bank's cues, the
    /// grammar state, and the audio levels.
    ///
    /// It does **not** clear `out`, and it deliberately leaves some fields
    /// alone — the ones only a shell can answer. A shell calls this and then
    /// overlays its own: the project path, the audio device list, the shader
    /// name and error, the shader pool, cached thumbnails, per-clip probe
    /// metadata, the camera rows, the bpm text field, and fullscreen. Those are
    /// the eight-ish facts that are about a *machine* rather than a session.
    ///
    /// This exists so the panels can move to a browser without the mirror they
    /// read being built twice (web-port.md §8 step 4g). Two builders would be
    /// two things to keep in step, and the native one is 269 lines — about 90%
    /// of which never mentioned the shell at all.
    pub fn build_mirror(&self, snap: &ClockSnapshot, audio: &AudioFrame, out: &mut UiMirror) {
        let phrase = self.sequencer.phrase_len();
        // Resolve the playing/armed cues to their source clips so the pool grid
        // can mark them. `active` = the clip has a cue in the live bank.
        let armed_cue = self.sequencer.armed();
        let playing_cue = self.current;
        let live = &self.banks[self.live_bank];
        let active_clips: std::collections::HashSet<ClipId> =
            live.cues.iter().map(|c| c.clip).collect();
        let playing_clip = playing_cue.and_then(|cid| live.cue(cid)).map(|c| c.clip);
        let armed_clip = armed_cue.and_then(|cid| live.cue(cid)).map(|c| c.clip);
        let role_of = |id: Option<ClipId>| {
            if id.is_some() && playing_clip == id {
                ClipRole::Playing
            } else if id.is_some() && armed_clip == id {
                ClipRole::Armed
            } else {
                ClipRole::None
            }
        };

        out.bpm = snap.bpm;
        out.beat = snap.beat;
        out.phase = snap.phase;
        out.quantum = snap.quantum;
        out.time_sig = self.time_sig;
        out.phrase_cadence = self.phrase_cadence;
        out.loop_cadence = self.loop_cadence;
        out.phrase_beats = phrase;
        out.loop_len = self.loop_len;
        out.preserve_playhead = self.preserve_playhead;
        out.advanced = self.advanced;
        out.grammar_on = self.grammar_on;
        out.command_palette_open = self.command_palette_open;
        out.grammar_modal = self.grammar_modal_view();
        out.grammar_note = self
            .empty_prefix
            .filter(|(_, at)| at.elapsed() < EMPTY_PREFIX_LINGER)
            .map(|(label, _)| label);
        out.grammar_pane = self.grammar_on.then(|| self.focused_pane.mode_word());
        let q = snap.quantum.max(0.25);
        out.bars_per_phrase = (phrase / q).round().max(1.0) as u32;
        out.bar_in_phrase = (snap.beat.rem_euclid(phrase) / q) as u32;
        out.sync = Some(self.sync);
        let caps = self.clock.caps();
        out.peers = caps.peers;
        out.can_set_tempo = caps.can_set_tempo;
        out.can_set_phase = caps.can_set_phase;

        // The pool grid shows one clip bank at a time; the clip-bank bar lists
        // them all. Cues still resolve against the full flat pool (via ClipId),
        // so playing/armed marking works across banks.
        let active = self.active_clip_bank;
        let clip_ids: Vec<ClipId> = self
            .clip_banks
            .get(active)
            .map(|b| b.clip_ids.clone())
            .unwrap_or_default();
        out.clip_dir = self
            .clip_banks
            .get(active)
            .and_then(|b| b.dir.as_ref())
            .map(|d| d.display().to_string());
        out.clip_banks = self
            .clip_banks
            .iter()
            .map(|b| ClipBankView {
                name: b.name.clone(),
                clip_count: b.clip_ids.len(),
            })
            .collect();
        out.active_clip_bank = active;
        // `has_thumb`, `duration_sec` and `fps` are the shell's to fill: a
        // thumbnail is a cached texture and the rest is probe data the runtime
        // `Clip` does not retain. Left at their defaults here rather than
        // guessed.
        out.clips = clip_ids
            .iter()
            .filter_map(|&id| self.clips.iter().find(|c| c.id == id))
            .map(|c| ClipEntry {
                id: c.id,
                name: c.name.clone(),
                active: active_clips.contains(&c.id),
                role: role_of(Some(c.id)),
                has_thumb: false,
                bpm: c.bpm,
                duration_sec: None,
                fps: None,
                bank: active,
            })
            .collect();
        out.selected_clip = self.selected_clip;

        // Cue banks: the bank bar, and the edit bank's cues (with live roles).
        out.banks = self
            .banks
            .iter()
            .map(|b| BankView {
                name: b.name.clone(),
                cue_count: b.cues.len(),
            })
            .collect();
        out.live_bank = self.live_bank;
        out.edit_bank = self.edit_bank;
        out.selected_cue = self.selected_cue;
        out.playhead_sec = self.current_pts;

        let advanced = self.advanced;
        let last_bpm = self.last_bpm;
        let clips = &self.clips;
        let clip_bpm = |id: ClipId| clips.iter().find(|c| c.id == id).and_then(|c| c.bpm);
        let clip_camera = |id: ClipId| {
            clips
                .iter()
                .find(|c| c.id == id)
                .is_some_and(|c| c.camera_uid().is_some())
        };
        // Live tap delays for the editor's "effective" readout; cues without a
        // tap fall back to their resolved target. A camera source reports a
        // delay; a file decoder reports `None`. That is the same test
        // `resolve_camera_delays` uses, and it is the only thing either side
        // needs to know about which kind it has.
        let delay_effs: HashMap<CueId, f64> = self
            .decoders
            .iter()
            .filter_map(|(&id, h)| h.delay_eff().map(|d| (id, d)))
            .collect();
        out.cues = self.banks[self.edit_bank]
            .cues
            .iter()
            .map(|c| {
                let clip_bpm = clip_bpm(c.clip);
                CueView {
                    id: c.id,
                    clip: c.clip,
                    name: c.name.clone(),
                    in_sec: c.in_sec,
                    out_sec: c.out_sec,
                    preserve: c.preserve,
                    chain: c.chain.clone(),
                    role: if playing_cue == Some(c.id) {
                        ClipRole::Playing
                    } else if armed_cue == Some(c.id) {
                        ClipRole::Armed
                    } else {
                        ClipRole::None
                    },
                    // Shell's, like ClipEntry::has_thumb above.
                    has_thumb: false,
                    dwell: c.dwell,
                    loop_len: c.loop_len,
                    loop_phase: c.loop_phase,
                    start_nudge: c.start_nudge,
                    trig_delay: c.trig_delay,
                    bpm: c.bpm,
                    clip_bpm,
                    bpm_sync_on: c.bpm_sync_on,
                    speed_mul: c.speed_mul,
                    speed: resolve_speed(advanced, last_bpm, c, clip_bpm),
                    camera: clip_camera(c.clip),
                    delay: c.delay,
                    delay_eff: delay_effs
                        .get(&c.id)
                        .copied()
                        .unwrap_or_else(|| c.delay.seconds_capped(last_bpm)),
                }
            })
            .collect();

        out.levels = audio.bands;
        // The 512-bin linear FFT row of the iChannel0 texture, already 0..1.
        out.spectrum_linear.clear();
        out.spectrum_linear.extend(
            audio.audio_tex[..crate::analysis::AUDIO_TEX_W]
                .iter()
                .map(|&b| b as f32 / 255.0),
        );
        out.level = audio.level;
    }
}

/// Resolve a cue's effective playback speed: `1.0` in simple mode; in advanced
/// mode a BPM-sync factor (`session_bpm / source_bpm`, when synced and a source
/// tempo is known) stacked with the user multiplier, clamped to a sane range.
#[must_use]
pub fn resolve_speed(advanced: bool, session_bpm: f64, cue: &Cue, clip_bpm: Option<f64>) -> f64 {
    if !advanced {
        return 1.0;
    }
    let sync = if cue.bpm_sync_on {
        match cue.bpm.or(clip_bpm) {
            Some(src) if src > 0.0 => session_bpm / src,
            _ => 1.0,
        }
    } else {
        1.0
    };
    let mul = if cue.speed_mul.on {
        cue.speed_mul.val
    } else {
        1.0
    };
    (sync * mul).clamp(0.05, 20.0)
}

/// Keep stored trim consistent with the opener's rule (a source only honors an
/// out-point strictly after the in-point): collapse an out ≤ in to "untrimmed"
/// so the editor never shows a trim that playback ignores.
fn normalize_cue_trim(cue: &mut Cue) {
    if cue.out_sec.is_some_and(|o| o <= cue.in_sec) {
        cue.out_sec = None;
    }
}

/// Name banks A, B, C, … by count; past Z, suffix a number (A1, B1, …).
fn bank_letter_name(n: usize) -> String {
    if n < 26 {
        ((b'A' + n as u8) as char).to_string()
    } else {
        format!("{}{}", (b'A' + (n % 26) as u8) as char, n / 26)
    }
}

/// Step a cursor through a list of `len` items, clamping at the ends. With no
/// current position, a positive delta starts at the first item and a negative
/// one at the last; an empty list yields `None`.
fn step_index(len: usize, pos: Option<usize>, delta: i32) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match pos {
        Some(p) => (p as i32 + delta).clamp(0, len as i32 - 1) as usize,
        None if delta >= 0 => 0,
        None => len - 1,
    })
}

/// Duplicate a bank, drawing a fresh id for every cue from `next_id` — cue ids
/// are globally unique across banks (sources and selection key on them).
fn clone_bank_with_ids(bank: &Bank, next_id: &mut CueId) -> Bank {
    let mut b = bank.clone();
    for cue in &mut b.cues {
        cue.id = *next_id;
        *next_id += 1;
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bank::Toggle;
    use crate::clock::ClockSnapshot;
    use std::cell::Cell;
    use std::rc::Rc;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn cue() -> Cue {
        Cue::new(1, 0, "c")
    }

    #[test]
    fn clone_bank_remaps_every_cue_id() {
        let mut bank = Bank::new("A");
        bank.cues.push(Cue::new(3, 0, "x"));
        bank.cues.push(Cue::new(7, 1, "y"));
        let mut next = 8;
        let clone = clone_bank_with_ids(&bank, &mut next);
        assert_eq!(
            clone.cues.iter().map(|c| c.id).collect::<Vec<_>>(),
            [8, 9],
            "clones draw fresh globally-unique ids"
        );
        assert_eq!(next, 10, "the id counter advances past the clones");
        assert_eq!(bank.cues[0].id, 3, "the source bank is untouched");
        assert_eq!(clone.cues[1].clip, 1, "everything but the id carries over");
    }

    #[test]
    fn bank_letter_names_wrap_past_z() {
        assert_eq!(bank_letter_name(0), "A");
        assert_eq!(bank_letter_name(25), "Z");
        assert_eq!(bank_letter_name(26), "A1");
    }

    #[test]
    fn step_index_clamps_and_enters_from_either_end() {
        assert_eq!(step_index(0, None, 1), None, "empty list has no cursor");
        assert_eq!(
            step_index(3, None, 1),
            Some(0),
            "positive entry starts at the first"
        );
        assert_eq!(
            step_index(3, None, -1),
            Some(2),
            "negative entry starts at the last"
        );
        assert_eq!(step_index(3, Some(1), 1), Some(2));
        assert_eq!(step_index(3, Some(2), 1), Some(2), "clamps at the end");
        assert_eq!(step_index(3, Some(0), -5), Some(0), "clamps at the start");
    }

    #[test]
    fn speed_is_unity_in_simple_mode() {
        let mut c = cue();
        c.bpm_sync_on = true;
        c.speed_mul = Toggle { on: true, val: 2.0 };
        // advanced = false: every knob is inert, playback is native speed.
        assert_eq!(resolve_speed(false, 140.0, &c, Some(70.0)), 1.0);
    }

    #[test]
    fn bpm_sync_uses_session_over_source() {
        let mut c = cue();
        c.bpm_sync_on = true;
        // clip authored at 70 bpm, session at 140 -> play twice as fast
        assert_eq!(resolve_speed(true, 140.0, &c, Some(70.0)), 2.0);
        // cue-level bpm overrides the clip's
        c.bpm = Some(140.0);
        assert_eq!(resolve_speed(true, 140.0, &c, Some(70.0)), 1.0);
    }

    #[test]
    fn bpm_sync_without_source_is_unity() {
        let mut c = cue();
        c.bpm_sync_on = true;
        assert_eq!(resolve_speed(true, 140.0, &c, None), 1.0);
    }

    #[test]
    fn sync_and_multiplier_stack() {
        let mut c = cue();
        c.bpm_sync_on = true;
        c.speed_mul = Toggle { on: true, val: 1.5 };
        // (140/70) * 1.5 = 3.0
        assert_eq!(resolve_speed(true, 140.0, &c, Some(70.0)), 3.0);
        // multiplier alone, no sync
        c.bpm_sync_on = false;
        assert_eq!(resolve_speed(true, 140.0, &c, Some(70.0)), 1.5);
    }

    /// A clock the test drives by hand.
    ///
    /// The real one reads wall time, which makes "what happens sixteen beats
    /// from now" either a `sleep` or unobservable. The engine takes its clock as
    /// a trait object precisely so a shell can supply Link instead of the
    /// internal one; a test is the third caller of that same seam.
    struct FakeClock {
        beat: Rc<Cell<f64>>,
    }

    impl ClockSource for FakeClock {
        fn snapshot(&mut self) -> ClockSnapshot {
            let beat = self.beat.get();
            ClockSnapshot {
                bpm: 120.0,
                beat,
                phase: beat.rem_euclid(4.0),
                quantum: 4.0,
                is_playing: true,
            }
        }
        fn set_bpm(&mut self, _: f64) {}
        fn set_quantum(&mut self, _: f64) {}
        fn nudge_bpm(&mut self, _: f64) {}
        fn tap_downbeat(&mut self) {}
        fn reset(&mut self) {
            self.beat.set(0.0);
        }
        fn caps(&self) -> crate::clock::ClockCaps {
            crate::clock::ClockCaps {
                can_set_tempo: true,
                can_set_phase: true,
                peers: 0,
            }
        }
    }

    fn two_clips() -> Vec<Clip> {
        vec![
            Clip {
                id: 0,
                source: ClipSource::File("a.mov".into()),
                name: "a".into(),
                bpm: None,
            },
            Clip {
                id: 1,
                source: ClipSource::File("b.mov".into()),
                name: "b".into(),
                bpm: None,
            },
        ]
    }

    /// The rotation, with no GPU and no shell: two cues, a phrase boundary, and
    /// the sequencer's arm/swap contract. This is the whole reason the engine
    /// was extracted — none of it was testable while it lived inside a struct
    /// that owned two winit windows, and it is exactly the behaviour `/play` in
    /// a browser now gets for free.
    #[test]
    fn cues_rotate_over_a_phrase_boundary() {
        let beat = Rc::new(Cell::new(0.0));
        let mut e = Engine::new(Boot {
            clips: two_clips(),
            ..Boot::default()
        });
        e.clock = Box::new(FakeClock { beat: beat.clone() });
        e.add_cue(0);
        e.add_cue(1);
        assert_eq!(e.cue_steps(0).len(), 2, "both cues contribute a step");

        // No opener, so nothing ever opens. The rotation is bookkeeping over cue
        // ids and must turn regardless of whether any of them has pixels.
        let now = Instant::now();
        e.tick(now);
        let first = e
            .current
            .expect("the sequencer starts playing as soon as the set is non-empty");

        // One phrase is 16 beats by default (4 bars of 4/4). Step past it in bar
        // increments rather than jumping, because arming happens a bar early and
        // a single leap would skip the armed state entirely.
        for bar in 1..=6 {
            beat.set(f64::from(bar) * 4.0);
            e.tick(now);
        }
        assert_ne!(
            e.current,
            Some(first),
            "the sequencer did not swap after a phrase — the rotation is stuck on cue {first}"
        );
    }

    /// The same rotation, reached the way a browser reaches it.
    ///
    /// `toggle_clip_active` rather than `add_cue`: the pool path adds a cue
    /// *and* pushes its step into the sequencer directly, where the editor path
    /// rebuilds the whole active set. Both have to end up rotating, and only one
    /// of them was covered.
    #[test]
    fn the_pool_path_also_rotates() {
        let beat = Rc::new(Cell::new(0.0));
        let mut e = Engine::new(Boot {
            clips: two_clips(),
            ..Boot::default()
        });
        e.clock = Box::new(FakeClock { beat: beat.clone() });
        e.toggle_clip_active(0, 0.0);
        let now = Instant::now();
        e.tick(now);
        let first = e.current.expect("the first clip starts playing");
        e.toggle_clip_active(1, beat.get());
        assert_eq!(
            e.sequencer.active_len(),
            2,
            "the second clip joined the rotation"
        );
        for bar in 1..=6 {
            beat.set(f64::from(bar) * 4.0);
            e.tick(now);
        }
        assert_ne!(e.current, Some(first), "the pool path never swapped");
    }

    /// A one-cue set has nowhere to go, and must not thrash trying.
    #[test]
    fn a_lone_cue_keeps_playing() {
        let beat = Rc::new(Cell::new(0.0));
        let mut e = Engine::new(Boot {
            clips: two_clips(),
            ..Boot::default()
        });
        e.clock = Box::new(FakeClock { beat: beat.clone() });
        e.add_cue(0);
        let now = Instant::now();
        e.tick(now);
        let only = e.current;
        for bar in 1..=8 {
            beat.set(f64::from(bar) * 4.0);
            e.tick(now);
        }
        assert_eq!(e.current, only, "with one cue there is nothing to swap to");
    }

    #[test]
    fn removing_the_selected_cue_clears_the_selection() {
        let mut e = Engine::new(Boot {
            clips: vec![Clip {
                id: 0,
                source: ClipSource::File("a.mov".into()),
                name: "a".into(),
                bpm: None,
            }],
            ..Boot::default()
        });
        e.add_cue(0);
        let id = e.selected_cue.expect("add_cue selects what it added");
        e.remove_cue(id);
        assert_eq!(e.selected_cue, None, "a cursor must not survive its cue");
        assert!(e.banks[e.edit_bank].cues.is_empty());
    }

    /// An unhandled command comes back rather than vanishing. This is what makes
    /// "the web shell does not do that yet" a visible fact instead of a dead key.
    #[test]
    fn shell_commands_are_handed_back() {
        let mut e = Engine::new(Boot::default());
        assert!(
            e.apply_command(Command::ToggleFullscreen).is_some(),
            "a window command is not the engine's"
        );
        assert!(
            e.apply_command(Command::SetBpm(128.0)).is_none(),
            "a tempo command is"
        );
        assert!((e.clock.snapshot().bpm - 128.0).abs() < 1e-9);
    }
}
