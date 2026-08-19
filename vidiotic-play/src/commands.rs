//! The UI↔engine contract: `Command`s flow from the control UI (and async file
//! pickers) to the engine; the engine publishes a `UiMirror` snapshot the UI
//! reads. Keeping these in one place lets other input sources (keys today,
//! MIDI eventually) map onto the same commands.
//!
//! The nouns a command names — clip and shader ids, effect-chain slots, time
//! signatures, cadences — are model types from `vidiotic-core`, re-exported
//! here so callers keep spelling them `commands::TimeSig`. The dependency only
//! runs one way: the model never mentions a `Command`, which is what lets the
//! span editor share the model without inheriting the player's verbs.

use std::path::PathBuf;
use std::sync::Arc;

use crate::bank::{CamDelay, CueId, Toggle};
use crate::isf::IsfValue;

pub use vidiotic_core::chain::{ChainSlot, ClipId, ShaderId, ShaderPoolView, SlotRef};
pub use vidiotic_core::time::{Cadence, SyncKind, TimeSig, LOOP_TICKS_PER_BEAT, TIME_SIG_DENS};

/// Everything an input surface (UI, keys, pickers) can ask the engine to do.
#[derive(Clone, Debug)]
pub enum Command {
    SetBpm(f64),
    BpmDelta(f64), // ±1 from the +/- keys
    NudgeBpm(f64), // ratio ±0.001 for the ±0.1% controls
    TapDownbeat,   // snap the downbeat phase to now (does not change tempo)
    TapTempo,      // derive BPM from the interval between successive taps
    SoftReset, // reset the beat grid to bar 1 / beat 1 / phrase 1; playlist position and playhead untouched
    HardReset, // soft reset, plus jump the playlist back to its first cue and restart its playhead
    SetSyncSource(SyncKind),
    SetTimeSig(TimeSig),
    SetPhraseCadence(Cadence), // musical length between auto-transitions to the next clip
    SetLoopCadence(Option<Cadence>), // forced video re-loop grid; None = loop on EOF only
    SetPreservePlayhead(bool), // on cut, carry the playhead over (true) or restart the incoming clip from its start (false)
    ToggleClipActive(ClipId),
    // The pool's clip cursor: click-select in the grid, or grammar Go/Make in
    // the pool pane. Delta moves through the active clip bank's order.
    SelectClip(Option<ClipId>),
    SelectClipDelta(i32),
    SelectClipFirst,
    SelectClipLast,
    // Cue/bank editing. Cue mutations target the *edit* bank; `AddCue` also
    // selects the new cue. Trim/preserve edits take effect the next time the
    // cue's decoder spawns (re-trigger, or when its bank goes live).
    AddCue(ClipId), // add a full-length cue for this clip to the edit bank
    RemoveCue(CueId),
    SelectCue(Option<CueId>),
    SelectCueDelta(i32), // move selection through the edit bank's cue order
    SelectCueFirst,
    SelectCueLast,
    SetCueIn(CueId, f64),                // in-point, seconds
    SetCueOut(CueId, Option<f64>),       // out-point, seconds; None = clip end
    SetCueInToPlayhead(CueId),           // snap in-point to the displayed playhead
    SetCueOutToPlayhead(CueId),          // snap out-point to the displayed playhead
    SetCuePreserve(CueId, Option<bool>), // per-cue preserve override; None = inherit global
    SetCueChain(CueId, Vec<ChainSlot>),  // replace the cue's effect chain; empty = the live shader
    // Set one ISF input on one chain slot of a cue, without replacing the whole
    // chain (so a slider drag doesn't clobber the rest of the stack).
    SetChainParam {
        cue: CueId,
        slot: usize,
        name: Arc<str>,
        value: IsfValue,
    },
    LoadIsf(PathBuf), // compile an ISF `.fs` into the pool and append it to the selected cue's chain
    SetCueParam(CueId, CueParam), // one advanced per-cue timing/speed knob
    NudgeCueParam(CueParamKind, i32), // step the selected cue's knob by ± one detent
    MoveCue(CueId, usize), // reorder within the edit bank to a target index (drag / ◀▶)
    SetClipBpm(ClipId, Option<f64>), // source-clip tempo metadata; None clears it
    SetAdvancedMode(bool), // gate per-cue timing/speed resolution + the extended UI
    SetGrammarMode(bool), // modal command grammar: token keys/pads drive verb sequences
    AddBank,
    CloneBank,          // duplicate the edit bank (cues get fresh ids) and append it
    SetLiveBank(usize), // which bank the sequencer plays
    CycleLiveBank(i32), // step the live bank by ±1, wrapping (keys , / .)
    SetEditBank(usize), // which bank the UI edits
    // Shader pool: pin the current live shader's last-good compile so a cue can
    // use it while you keep livecoding the main shader.
    CaptureShader,             // pin the current live shader into the pool
    RemoveShader(ShaderId),    // drop a pinned shader (cues fall back to the live shader)
    SetClipDir(PathBuf),       // replace the whole pool with one bank from this dir
    AddClipDirAsBank(PathBuf), // append this dir as a new clip bank (keeps existing clips/cues)
    SetActiveClipBank(usize),  // which clip bank the pool grid shows
    // Cameras. Devices are enumerated on demand (`RefreshCameras`); the on-air
    // toggle runs/stops the device's capture service independent of cue
    // rotation; `AddCameraCue` find-or-creates the device's pool clip and adds
    // a cue for it to the edit bank.
    RefreshCameras,
    SetCameraOnAir(Arc<str>, bool), // device uid
    AddCameraCue(Arc<str>),         // device uid
    // Point every clip referencing the missing device `from` at the connected
    // device `to` (the camera analogue of relinking a moved file).
    RelinkCamera {
        from: Arc<str>,
        to: Arc<str>,
    },
    SetShaderPath(PathBuf),
    SetAudioDevice(Option<String>), // id key; None = default
    ToggleFullscreen,               // shell-intercepted
    // Project persistence. `SaveProject` writes back to the loaded path (or opens
    // the picker if none), `SaveProjectAs` always opens the picker; both resolve
    // to a `SaveProjectTo` once a destination is known. `OpenProject` opens a
    // picker that resolves to a `LoadProject`, which replaces the running
    // session with the picked `.viproj`. `OpenProjectEditor` saves in place
    // and launches the sibling vidiotic-prep GUI on the project file.
    SaveProject,
    SaveProjectAs,
    SaveProjectTo(PathBuf),
    OpenProject,
    LoadProject(PathBuf),
    // The other four "ask the visitor for a path of kind X" requests. They are
    // commands for the same reason `OpenProject` is: choosing a file is a thing
    // the *shell* can do, and the shells disagree about how. Natively these open
    // an `rfd` dialog on a worker thread; in a browser they are an
    // `<input type=file>`, and a kind the browser cannot serve lands in the
    // status line by itself, because `apply_command` hands back what it does not
    // implement. Panels that called a picker directly could not move to wasm;
    // panels that send one of these are OS-free (web-port.md §8 step 4g).
    //
    // Each resolves to the command named beside it once a path is known.
    PickClipDir,     // -> SetClipDir
    PickClipBankDir, // -> AddClipDirAsBank
    PickShader,      // -> SetShaderPath
    PickIsf,         // -> LoadIsf
    OpenProjectEditor,
    /// Launch the sibling `vidiotic-ctl` mapper. Unlike `OpenProjectEditor`
    /// this needs no project — the mapper edits `.vmap` files on disk — so it
    /// is available in every session.
    OpenControlMapper,
    ToggleCommandPalette,
    Quit,

    // --- keyboard tempo entry ---
    /// Append one digit to the pending BPM entry. [`Self::BpmCommit`] parses
    /// the accumulated digits into a tempo and [`Self::BpmClear`] abandons
    /// them; the pending string rides out on `UiMirror::bpm_entry` so the
    /// transport can show it mid-type.
    ///
    /// The accumulator is the shell's, not the engine's: it exists to give a
    /// keyboard the numeric field the control UI already has, and a browser
    /// front end has a real `<input>` instead. Three commands rather than one
    /// stateful key handler, so the digits arrive through the same mapper as
    /// everything else — which is also what lets a numeric pad drive them.
    BpmDigit(u8),
    BpmCommit,
    BpmClear,

    // --- the grammar's vocabulary, as a command ---
    /// Apply one grammar [`Verb`](crate::grammar::Verb) — the engine resolves
    /// the selection and bank context the verb deliberately leaves open, then
    /// raises whatever concrete commands it means.
    ///
    /// The bridge exists for the verbs that are *only* reachable this way:
    /// "remove the selected cue" or "mark in at the playhead" name a target
    /// the mapper cannot spell, because a binding is authored long before
    /// there is a selection to point at. Without this a MIDI pad could not be
    /// bound to them at all — they lived in the wrong enum.
    ///
    /// Undo bookkeeping happens on what the verb raises, not on this: the
    /// shell drains the engine's pending queue in the same tick and dispatches
    /// each concrete command normally.
    Verb(crate::grammar::Verb),

    // --- history ---
    /// Undo the last cue/bank authoring edit. Intercepted in `App::update`
    /// before dispatch — it acts on the undo stack, not the document. Reserved
    /// chord (Cmd/Ctrl+Z), not a bindable action. See `crate::undo`.
    Undo,
    /// Redo the edit the last [`Self::Undo`] reverted.
    Redo,
}

impl Command {
    /// Whether the OS's key-repeat events should re-fire this command while a
    /// key is held.
    ///
    /// Only the tempo nudges. Holding `[` to drift the beat into phase is how
    /// the control is used, and it worked when these keys were a hardcoded
    /// match with no repeat guard; resolving them through the mapper (which
    /// drops repeats, so a held `f` cannot toggle fullscreen sixty times a
    /// second) would otherwise have taken it away.
    ///
    /// Deliberately *not* [`Self::BpmDigit`]: a held `1` used to push "11111"
    /// into the entry, which nobody wanted.
    #[must_use]
    pub fn repeats_on_hold(&self) -> bool {
        matches!(self, Self::BpmDelta(_) | Self::NudgeBpm(_))
    }
}

/// One advanced per-cue knob, edited via [`Command::SetCueParam`]. Mirrors the
/// advanced fields on [`crate::bank::Cue`]; ticks are 1/32-beat
/// ([`LOOP_TICKS_PER_BEAT`]).
#[derive(Clone, Copy, Debug)]
pub enum CueParam {
    Dwell(Option<u32>),      // beats-until-advance in ticks; None = inherit global
    Loop(Option<u32>),       // re-loop grid in ticks; None = inherit global
    LoopPhase(Toggle<i32>),  // loop-grid micro-timing, signed ticks
    StartNudge(Toggle<f64>), // in-point nudge, seconds
    TrigDelay(Toggle<u32>),  // swap-in lead-in, ticks
    Bpm(Option<f64>),        // source tempo override; None = inherit clip
    BpmSync(bool),           // retime to session tempo
    SpeedMul(Toggle<f64>),   // user speed multiplier
    CamDelay(CamDelay),      // camera cues: voluntary delay behind the live edge
}

/// The nudgeable [`CueParam`] knobs (all but the camera-only delay), for
/// [`Command::NudgeCueParam`] and the grammar's Tune root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueParamKind {
    Dwell,
    Loop,
    LoopPhase,
    StartNudge,
    TrigDelay,
    Bpm,
    BpmSync,
    SpeedMul,
}

impl CueParamKind {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Dwell => "dwell",
            Self::Loop => "loop",
            Self::LoopPhase => "swing",
            Self::StartNudge => "nudge",
            Self::TrigDelay => "delay",
            Self::Bpm => "bpm",
            Self::BpmSync => "sync",
            Self::SpeedMul => "speed",
        }
    }
}

/// A clip/cue's live-playback role, for UI markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipRole {
    None,
    Playing,
    Armed,
}

/// One source clip as shown in the pool grid.
#[derive(Clone, Debug)]
pub struct ClipEntry {
    pub id: ClipId,
    pub name: Arc<str>,
    pub active: bool,
    pub role: ClipRole,
    pub has_thumb: bool,           // texture cached in the UI's thumbnail map
    pub bpm: Option<f64>,          // source tempo metadata, if set
    pub duration_sec: Option<f64>, // probed clip length, if known
    pub fps: Option<f64>,          // probed source frame rate, if known
    pub bank: usize,               // the clip bank this entry is shown under
}

/// A clip bank's identity for the clip-bank bar above the pool grid.
#[derive(Clone, Debug)]
pub struct ClipBankView {
    pub name: Arc<str>,
    pub clip_count: usize,
}

/// One capture device in the pool's cameras section.
#[derive(Clone, Debug)]
pub struct CameraEntry {
    pub uid: Arc<str>,
    pub name: Arc<str>,
    pub on_air: bool,
    /// Human status line: "off air", "1920x1080 @ 30", or an error.
    pub status: Arc<str>,
    /// A saved project references this uid but no connected device has it;
    /// the row offers relinking instead of the on-air toggle.
    pub missing: bool,
    pub active: bool,   // has a cue in the live bank
    pub role: ClipRole, // playing/armed if its clip's cue is
}

/// One cue of the edit bank, as shown in the sequencer section / editor.
#[derive(Clone, Debug)]
pub struct CueView {
    pub id: CueId,
    pub clip: ClipId,
    pub name: Arc<str>,
    pub in_sec: f64,
    pub out_sec: Option<f64>,
    pub preserve: Option<bool>,
    pub chain: Vec<ChainSlot>, // per-cue effect chain; empty = the live shader
    pub role: ClipRole,        // Playing/Armed if this cue is the live bank's current/next
    pub has_thumb: bool,
    // Advanced-mode timing/speed (see `crate::bank::Cue`). Ticks are 1/32-beat.
    pub dwell: Option<u32>,
    pub loop_len: Option<u32>,
    pub loop_phase: Toggle<i32>,
    pub start_nudge: Toggle<f64>,
    pub trig_delay: Toggle<u32>,
    pub bpm: Option<f64>,      // this cue's source-tempo override
    pub clip_bpm: Option<f64>, // the source clip's own BPM (the inherited value)
    pub bpm_sync_on: bool,
    pub speed_mul: Toggle<f64>,
    pub speed: f64,      // resolved effective playback speed (for the readout)
    pub camera: bool,    // camera-sourced: timeline knobs inert, delay applies
    pub delay: CamDelay, // camera cues: voluntary delay behind the live edge
    pub delay_eff: f64,  // current slewed/quantized delay, seconds (readout)
}

/// A bank's identity for the bank bar.
#[derive(Clone, Debug)]
pub struct BankView {
    pub name: Arc<str>,
    pub cue_count: usize,
}

/// The pending grammar sequence, as the which-key overlay and statusline
/// trail render it.
#[derive(Clone, Debug)]
pub struct GrammarModalView {
    /// Input trail so far, e.g. `"t"` or `"t·swing"`.
    pub trail: String,
    /// The open root's label, or the sticky mode's.
    pub title: &'static str,
    /// `(key label, conjugation label)` for each populated slot, in token order.
    pub options: Vec<(&'static str, &'static str)>,
}

/// Read-only display state the engine republishes each tick for the control UI.
#[derive(Clone, Debug, Default)]
pub struct UiMirror {
    /// The loaded project's path, if any (display string). `None` before the
    /// first save/load — scripts read this to reason about `SaveProject`.
    pub project_path: Option<String>,
    pub bpm: f64,
    pub bpm_entry: Option<String>, // pending keyboard BPM entry, digits typed so far
    pub beat: f64,
    pub phase: f64, // 0..quantum
    pub quantum: f64,
    pub time_sig: TimeSig,
    pub phrase_cadence: Cadence, // source-of-truth "next every" length
    pub loop_cadence: Option<Cadence>, // source-of-truth "loop every" length; None = EOF-only
    pub bar_in_phrase: u32,
    pub bars_per_phrase: u32,
    pub phrase_beats: f64, // phrase_cadence resolved against time_sig, in beats
    pub loop_len: Option<u32>, // forced re-loop grid in 1/32-beat ticks; None = EOF-only
    pub preserve_playhead: bool, // carry the playhead over on a cut vs. restart the incoming clip
    pub advanced: bool,    // advanced sequencer mode: per-cue timing/speed + extended UI
    pub grammar_on: bool,  // modal command grammar enabled
    pub command_palette_open: bool, // floating command palette open
    pub grammar_modal: Option<GrammarModalView>, // pending sequence, if any
    /// A root just pressed that has nothing bound under it in this pane, for a
    /// beat or so after the press. Nothing opened; this is what says so.
    pub grammar_note: Option<&'static str>,
    /// The focused pane's statusline mode word (e.g. "BANK") while the
    /// grammar is on; `None` when it's off.
    pub grammar_pane: Option<&'static str>,
    pub sync: Option<SyncKind>,
    pub peers: u64,
    /// Whether the active clock source accepts tempo/phase edits. Link is
    /// listen-only, so its controls (BPM, nudge, tap, downbeat) grey out.
    pub can_set_tempo: bool,
    pub can_set_phase: bool,
    pub audio_devices: Vec<Arc<str>>, // device names; the name doubles as the selection key
    pub current_device: Option<Arc<str>>,
    pub audio_error: Option<String>,
    pub shader_name: Option<String>,
    pub shader_error: Option<Arc<str>>,
    pub clip_dir: Option<String>, // the active clip bank's source dir, for the header
    pub clip_banks: Vec<ClipBankView>,
    pub active_clip_bank: usize,
    pub clips: Vec<ClipEntry>, // the active clip bank's clips, in id order
    pub selected_clip: Option<ClipId>, // the pool's clip cursor
    pub cameras: Vec<CameraEntry>, // enumerated capture devices (RefreshCameras)
    // Cue banks.
    pub banks: Vec<BankView>,
    pub live_bank: usize,
    pub edit_bank: usize,
    pub cues: Vec<CueView>, // the edit bank's cues, in order
    pub selected_cue: Option<CueId>,
    pub shader_pool: Vec<ShaderPoolView>, // pinned shaders a cue can override with
    pub playhead_sec: f64,                // position of the currently displayed clip
    pub levels: [f32; 21],                // 21 perceptual log bands (native fftBand view)
    pub spectrum_linear: Vec<f32>,        // 512 linear bins 0..1 — the iChannel0 FFT row
    pub level: f32,
    pub fullscreen: bool,
}
