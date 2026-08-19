//! The player's prefix keymap — the machine behind what the UI calls grammar
//! mode. Two levels: 6 pane-sensitive verb prefixes plus two global ones
//! (Pane, Meta), each opening a which-key overlay of up to 8 bindings; a
//! second token resolves to a [`Verb`]. Rising-edge only.
//!
//! This is Emacs' prefix-key/keymap model with `repeat-mode` on the end of it,
//! and the names here are that vocabulary: a [`Keymap`] holds eight
//! [`Submap`]s, a [`Binding`] is one resolvable slot in a submap, and a
//! binding may enter a [`RepeatMap`] where its own token keeps firing.
//!
//! The verbs keep fixed meanings — Go moves, Cut deletes, Tune enters knob
//! modes — and the focused [`Pane`] supplies the object: Fire in the clock
//! pane taps tempo, Cut in the bank pane removes the selected cue. Their names
//! are [`PREFIX_LABELS`], one list for every pane. Pure state machine — no App
//! or UI dependencies. Verbs are context-free ("remove the selected cue", not
//! "remove cue #7"); the app resolves selection and bank context when a verb
//! is emitted. All keymap *content* lives in the per-pane statics (see
//! [`pane_keymap`]) so the taxonomy can be reorganized without touching the
//! machinery; [`Machine::step`] takes the keymap as a parameter and the app
//! passes the focused pane's on each press.

use vidiotic_ctl::model::ControlSource;

use crate::commands::CueParamKind;

/// How many abstract inputs there are. Every table in this module is this
/// wide, and [`Token`] indexes them.
pub const TOKEN_COUNT: usize = 8;

/// One of the [`TOKEN_COUNT`] abstract inputs. Every edge source
/// (keyboard, pad, MIDI) reduces to one of these before the state machine sees
/// it.
///
/// A newtype rather than a bare `u8` because this type is public and the whole
/// module indexes fixed-width arrays with it — submaps, bindings, repeat
/// entries, [`KEY_TOKENS`]. As an alias, a caller outside the crate could hand
/// [`Machine::step`] a `9` and panic it on an out-of-bounds index; the range
/// invariant is now established once, in [`Token::new`], and [`Token::index`]
/// is in-bounds by construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Token(u8);

impl Token {
    /// `i` as a token, or `None` if it isn't one of the [`TOKEN_COUNT`].
    #[must_use]
    pub const fn new(i: u8) -> Option<Self> {
        if (i as usize) < TOKEN_COUNT {
            Some(Self(i))
        } else {
            None
        }
    }

    /// Index into a `TOKEN_COUNT`-wide table. In range by construction.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An input the keymap answers to: one of the 8 tokens, or cancel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Token(Token),
    Cancel,
}

/// A focusable region of the app the verb keys act on. Selecting one (via the
/// Pane prefix) swaps which [`Keymap`] the machine resolves against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pane {
    /// The clip pool grid (and its clip banks).
    Pool,
    /// The cue banks and the edit bank's cue list — the performance surface.
    #[default]
    Bank,
    /// The selected cue's editor: trim marks and the advanced knobs.
    Cue,
    /// The beat grid: tempo, downbeat, resets.
    Clock,
}

impl Pane {
    /// Lowercase name, as the input trail and pane-picker options spell it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::Bank => "bank",
            Self::Cue => "cue",
            Self::Clock => "clock",
        }
    }

    /// Uppercase name, as the statusline mode word spells it.
    #[must_use]
    pub const fn mode_word(self) -> &'static str {
        match self {
            Self::Pool => "POOL",
            Self::Bank => "BANK",
            Self::Cue => "CUE",
            Self::Clock => "CLOCK",
        }
    }
}

/// A completed command, still context-free. The app maps these onto
/// [`crate::commands::Command`]s, resolving the selected cue / clip / bank.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verb {
    /// Focus a pane, switching the verb keys' table.
    FocusPane(Pane),
    /// Bounce back to the previously focused pane.
    FocusPrevPane,
    SelectCueDelta(i32),
    SelectCueFirst,
    SelectCueLast,
    SelectClipDelta(i32),
    SelectClipFirst,
    SelectClipLast,
    EditBankDelta(i32),
    ClipBankDelta(i32),
    SendEditBankLive,
    CycleLiveBank(i32),
    MarkInToPlayhead,
    MarkOutToPlayhead,
    CyclePreserve,
    AddBank,
    CloneBank,
    /// Add a cue for the pool's selected clip to the edit bank.
    AddCueAtClip,
    RemoveSelectedCue,
    /// Step one advanced cue knob by ± one detent.
    NudgeParam(CueParamKind, i32),
    TapTempo,
    TapDownbeat,
    BpmDelta(f64),
    NudgeBpm(f64),
    SoftReset,
    HardReset,
    SaveProject,
    /// Pick a `.viproj` to load, replacing the running session.
    OpenProject,
    /// Save in place and launch the project editor (vidiotic-prep) on it.
    OpenProjectEditor,
    ToggleFullscreen,
    ToggleAdvanced,
    ToggleCommandPalette,
    GrammarOff,
}

/// One slot of a repeat map: what it fires, and what the overlay
/// calls it. Carries its own label for the same reason [`Binding`] does —
/// inferring one from the verb payload can only ever be right for the verbs
/// that were thought of at the time.
#[derive(Clone, Copy, Debug)]
pub struct RepeatEntry {
    pub label: &'static str,
    pub verb: Verb,
}

/// Per-token entries of a repeat mode (Emacs' `repeat-mode` repeat-map). A
/// populated slot fires its verb and stays in the mode; a token the mode does
/// not own is swallowed, exactly as under an open prefix — one stray-token
/// rule for both pending states. Leaving a mode is Escape.
pub type RepeatMap = [Option<RepeatEntry>; TOKEN_COUNT];

/// One resolvable slot under a prefix — magit would call it a suffix.
/// `verb` fires on selection (`None` for pure mode entry); `repeat` is a
/// `(mode label, map)` the machine enters afterwards, for repeat-friendly
/// terminals (tap tempo, knob ±).
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub label: &'static str,
    pub verb: Option<Verb>,
    pub repeat: Option<(&'static str, RepeatMap)>,
}

/// What one prefix opens: its bindings, indexed by [`Token`]. The prefix's
/// own token slot holds its "doubled" hot verb — doubling needs no special
/// machinery. A submap can be entirely empty in a pane its verb doesn't apply
/// to; pressing that prefix opens nothing at all ([`Step::Empty`]) rather than
/// an overlay with no way out.
///
/// No label: a prefix's name is [`PREFIX_LABELS`], the same in every pane.
pub type Submap = [Option<Binding>; TOKEN_COUNT];

/// A complete keymap for one pane: one submap per token.
#[derive(Clone, Copy, Debug)]
pub struct Keymap {
    pub submaps: [Submap; TOKEN_COUNT],
}

/// Where the machine is between presses.
///
/// 280 bytes, nearly all of it `Repeat`'s entries, and there is exactly one of
/// these per app — boxing the variant would trade a `Copy` state and a
/// borrow-free machine for a heap allocation on every mode entry.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::large_enum_variant)]
pub enum State {
    #[default]
    Idle,
    /// A prefix is held open; the which-key overlay shows its bindings.
    AwaitingBinding { prefix: Token },
    /// A repeat-friendly terminal mode; `trail_prefix` is the prefix that led
    /// here, for the input-trail display.
    ///
    /// The entries are held *by value*. A `RepeatMap` is `Copy` and eight
    /// entries wide, so entering a mode costs one memcpy of a couple of
    /// hundred bytes — and in exchange the machine borrows nothing from the
    /// table it stepped against, which is what a table that is not compiled in
    /// would need.
    Repeat {
        label: &'static str,
        entries: RepeatMap,
        trail_prefix: Token,
    },
}

/// What one input did to the machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// Consumed; a sequence is (still) pending.
    Pending,
    /// Consumed; a verb completed.
    Verb(Verb),
    /// Consumed; the pending sequence was abandoned.
    Cancelled,
    /// Not consumed (cancel while idle) — the caller's fallthrough applies.
    Rejected,
    /// Consumed, but nothing opened: the pressed prefix has no slots filled
    /// in this pane. Carries the prefix's label so the shell can say which.
    ///
    /// Distinct from [`Self::Rejected`], which means "not consumed, fall
    /// through": this press *was* the keymap's, it just had nowhere to go.
    /// The machine stays idle — an option-less prefix is a trap, since under
    /// the swallow rule nothing but Escape gets out of it.
    Empty(&'static str),
}

/// The keymap state machine. One per app; feed it [`Input`]s via
/// [`Self::step`].
#[derive(Default)]
pub struct Machine {
    pub state: State,
    /// How the surface driving the pending sequence spells its tokens. Set by
    /// every [`Self::step`], so the overlay names the control under the
    /// operator's hand rather than always naming a key.
    pub spelling: Spelling,
}

impl Machine {
    /// Advance on one input, resolving against the focused pane's table.
    ///
    /// The table is borrowed only for the call: nothing in the machine
    /// outlives it (see [`State::Repeat`]), so a table built at runtime
    /// works exactly as the compiled-in ones do.
    pub fn step(&mut self, table: &Keymap, input: Input, from: Spelling) -> Step {
        self.spelling = from;
        match (&self.state, input) {
            (State::Idle, Input::Cancel) => Step::Rejected,
            (_, Input::Cancel) => {
                self.state = State::Idle;
                Step::Cancelled
            }
            (State::Idle, Input::Token(t)) => {
                // Never open a prefix with nothing under it: the overlay would
                // show no options and then swallow every press until Escape.
                if table.submaps[t.index()].iter().all(Option::is_none) {
                    Step::Empty(PREFIX_LABELS[t.index()])
                } else {
                    self.state = State::AwaitingBinding { prefix: t };
                    Step::Pending
                }
            }
            (State::AwaitingBinding { prefix }, Input::Token(t)) => {
                match &table.submaps[prefix.index()][t.index()] {
                    // Swallowed: an empty slot keeps the submap open.
                    None => Step::Pending,
                    Some(bind) => {
                        let prefix = *prefix;
                        self.state = match bind.repeat {
                            Some((label, entries)) => State::Repeat {
                                label,
                                entries,
                                trail_prefix: prefix,
                            },
                            None => State::Idle,
                        };
                        match bind.verb {
                            Some(v) => Step::Verb(v),
                            None => Step::Pending,
                        }
                    }
                }
            }
            (State::Repeat { entries, .. }, Input::Token(t)) => {
                // Swallowed, as under an open prefix. Re-rooting here would
                // let a stray press silently change what the *next* press means,
                // live; on a second screen "did nothing" is the safer failure.
                match entries[t.index()] {
                    Some(e) => Step::Verb(e.verb),
                    None => Step::Pending,
                }
            }
        }
    }

    /// Abandon any pending sequence.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }
}

/// What each prefix is called, in token order — the verb keys first, then the
/// two global nouns. One list, not one per pane: a verb keeps a fixed meaning
/// wherever it applies and the focused pane supplies the object, so nothing in
/// a table gets to disagree about what T1 is called. A pane the verb doesn't
/// apply to leaves its slots empty; it does not rename it.
pub const PREFIX_LABELS: [&str; TOKEN_COUNT] =
    ["Go", "Fire", "Mark", "Make", "Cut", "Tune", "Pane", "Meta"];

/// The canonical keyboard spelling of each token, in token order. These are
/// the strings `control_input::canon_key` / `vidiotic_ctl::keys` produce.
pub const KEY_TOKENS: [&str; TOKEN_COUNT] = ["g", "f", "m", "a", "d", "t", "b", ";"];

/// The gamepad spelling: the d-pad, then the face diamond. Same names the
/// mapper's binding rows show, so the overlay and the map agree.
pub const PAD_TOKENS: [&str; TOKEN_COUNT] = [
    "Up", "Down", "Left", "Right", "North", "South", "East", "West",
];

/// The MIDI spelling: notes 36–43, on any device and any channel.
pub const MIDI_TOKENS: [&str; TOKEN_COUNT] = ["36", "37", "38", "39", "40", "41", "42", "43"];

/// Which surface a pending sequence is being driven from, and so how the
/// which-key overlay spells its options.
///
/// The token abstraction is what lets one table serve a keyboard, a d-pad and
/// eight drum pads — but the operator is holding exactly one of them, and an
/// overlay that reads `g / f / m` while they press a d-pad is naming keys they
/// are not touching. This is the one place the abstraction has to leak.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Spelling {
    #[default]
    Key,
    Pad,
    Midi,
}

impl Spelling {
    /// How this surface spells the eight tokens, in token order.
    #[must_use]
    pub const fn tokens(self) -> &'static [&'static str; TOKEN_COUNT] {
        match self {
            Self::Key => &KEY_TOKENS,
            Self::Pad => &PAD_TOKENS,
            Self::Midi => &MIDI_TOKENS,
        }
    }

    /// How this surface spells cancel — the one control every pending state
    /// answers to.
    #[must_use]
    pub const fn cancel(self) -> &'static str {
        match self {
            Self::Key => "esc",
            Self::Pad => "Select",
            Self::Midi => "35",
        }
    }

    /// The surface an event came from. Keyboard sources reach the keymap
    /// through `handle_key`, not the event pump, so they never arrive here.
    #[must_use]
    pub fn of_source(source: &ControlSource) -> Self {
        match source {
            ControlSource::PadButton { .. } | ControlSource::PadAxis { .. } => Self::Pad,
            ControlSource::MidiNote { .. } | ControlSource::MidiCc { .. } => Self::Midi,
            ControlSource::Key { .. } => Self::Key,
        }
    }
}

/// Map a canonical key name to a keymap input. `Escape` cancels.
#[must_use]
pub fn token_of_key(canon: &str) -> Option<Input> {
    if canon == "Escape" {
        return Some(Input::Cancel);
    }
    KEY_TOKENS
        .iter()
        .position(|k| *k == canon)
        .and_then(|i| Token::new(i as u8))
        .map(Input::Token)
}

/// Map a non-keyboard edge source to a keymap input: gamepad d-pad and face
/// diamond are the 8 tokens (`Select` cancels), and MIDI notes 36–43 are the
/// 8 tokens (35 cancels) on any device or channel. Keyboard sources return
/// `None` — keys reach the keymap through `handle_key`, not the event pump.
#[must_use]
pub fn token_of_source(source: &ControlSource) -> Option<Input> {
    match source {
        ControlSource::PadButton { button, .. } => match button.as_str() {
            "DPadUp" => Some(Input::Token(Token(0))),
            "DPadDown" => Some(Input::Token(Token(1))),
            "DPadLeft" => Some(Input::Token(Token(2))),
            "DPadRight" => Some(Input::Token(Token(3))),
            "North" => Some(Input::Token(Token(4))),
            "South" => Some(Input::Token(Token(5))),
            "East" => Some(Input::Token(Token(6))),
            "West" => Some(Input::Token(Token(7))),
            "Select" => Some(Input::Cancel),
            _ => None,
        },
        ControlSource::MidiNote { note: 35, .. } => Some(Input::Cancel),
        ControlSource::MidiNote {
            note: n @ 36..=43, ..
        } => Token::new(n - 36).map(Input::Token),
        _ => None,
    }
}

const NC: Option<Binding> = None;
const NE: Option<RepeatEntry> = None;

/// A plain binding: emit and return to idle.
const fn bind(label: &'static str, verb: Verb) -> Option<Binding> {
    Some(Binding {
        label,
        verb: Some(verb),
        repeat: None,
    })
}

/// A binding that (optionally) emits, then enters a repeat mode.
const fn bind_repeat(
    label: &'static str,
    verb: Option<Verb>,
    mode: &'static str,
    entries: RepeatMap,
) -> Option<Binding> {
    Some(Binding {
        label,
        verb,
        repeat: Some((mode, entries)),
    })
}

/// One repeat slot: the verb, and what the overlay calls it.
const fn repeat(label: &'static str, verb: Verb) -> Option<RepeatEntry> {
    Some(RepeatEntry { label, verb })
}

/// A ± repeat table: T1 fires `up`, T2 fires `down`.
const fn pm_repeat(
    up_label: &'static str,
    up: Verb,
    down_label: &'static str,
    down: Verb,
) -> RepeatMap {
    let mut t = [NE; TOKEN_COUNT];
    t[0] = repeat(up_label, up);
    t[1] = repeat(down_label, down);
    t
}

/// The ± repeat table for one advanced cue knob: T1 steps up, T2 down.
const fn knob_repeat(kind: CueParamKind) -> RepeatMap {
    pm_repeat(
        "step +",
        Verb::NudgeParam(kind, 1),
        "step -",
        Verb::NudgeParam(kind, -1),
    )
}

/// A Tune binding: select the knob, then ± in repeat mode.
const fn knob(kind: CueParamKind) -> Option<Binding> {
    bind_repeat(kind.label(), None, kind.label(), knob_repeat(kind))
}

/// A prefix the focused pane has no use for. Pressing it opens nothing — see
/// [`Step::Empty`].
const EMPTY_SUBMAP: Submap = [NC; TOKEN_COUNT];

/// Cue-selection movement mode: T1 up, T2 down, entered from Go.
const MOVE_REPEAT: RepeatMap = pm_repeat(
    "up",
    Verb::SelectCueDelta(-1),
    "down",
    Verb::SelectCueDelta(1),
);

/// Clip-cursor movement mode in the pool pane: T1 up, T2 down.
const CLIP_MOVE_REPEAT: RepeatMap = pm_repeat(
    "up",
    Verb::SelectClipDelta(-1),
    "down",
    Verb::SelectClipDelta(1),
);

/// Tap mode: every further Fire press is a tap.
const TAP_REPEAT: RepeatMap = {
    let mut t = [NE; TOKEN_COUNT];
    t[1] = repeat("tap", Verb::TapTempo);
    t
};

/// Session-tempo ± modes for the clock pane's Tune prefix.
const BPM_REPEAT: RepeatMap = pm_repeat(
    "step +",
    Verb::BpmDelta(1.0),
    "step -",
    Verb::BpmDelta(-1.0),
);
const NUDGE_REPEAT: RepeatMap = pm_repeat(
    "step +",
    Verb::NudgeBpm(0.001),
    "step -",
    Verb::NudgeBpm(-0.001),
);

/// The Pane prefix, identical in every keymap: T1–T4 focus a pane, doubled (bb)
/// bounces back to the previous one. A pane press while its own pane is
/// focused simply re-focuses it — harmless.
const PANE_SUBMAP: Submap = [
    bind("pool", Verb::FocusPane(Pane::Pool)),
    bind("bank", Verb::FocusPane(Pane::Bank)),
    bind("cue", Verb::FocusPane(Pane::Cue)),
    bind("clock", Verb::FocusPane(Pane::Clock)),
    NC,
    NC,
    bind("back", Verb::FocusPrevPane),
    NC,
];

/// The Meta prefix, identical in every keymap: app-level nouns the pane never
/// recolors.
const META_SUBMAP: Submap = [
    bind("save", Verb::SaveProject),
    bind("fullscreen", Verb::ToggleFullscreen),
    bind("advanced", Verb::ToggleAdvanced),
    bind("edit proj", Verb::OpenProjectEditor),
    bind("open proj", Verb::OpenProject),
    bind("palette", Verb::ToggleCommandPalette),
    NC,
    bind("grammar off", Verb::GrammarOff),
];

/// Go through the edit bank's cue order: shared by the bank and cue panes.
const GO_CUES: [Option<Binding>; 4] = [
    bind_repeat("up", Some(Verb::SelectCueDelta(-1)), "move", MOVE_REPEAT),
    bind_repeat("down", Some(Verb::SelectCueDelta(1)), "move", MOVE_REPEAT),
    bind("first", Verb::SelectCueFirst),
    bind("last", Verb::SelectCueLast),
];

/// Cut's shared "dd removes the selected cue" slot for the bank and cue panes.
const CUT_CUE: Submap = [
    NC,
    NC,
    NC,
    NC,
    bind("cue", Verb::RemoveSelectedCue),
    NC,
    NC,
    NC,
];

/// The pool pane: the clip grid. Go moves the clip cursor (and clip banks);
/// Make turns the cursored clip into a cue.
static POOL_TABLE: Keymap = Keymap {
    submaps: [
        // Go
        [
            bind_repeat(
                "up",
                Some(Verb::SelectClipDelta(-1)),
                "move",
                CLIP_MOVE_REPEAT,
            ),
            bind_repeat(
                "down",
                Some(Verb::SelectClipDelta(1)),
                "move",
                CLIP_MOVE_REPEAT,
            ),
            bind("first", Verb::SelectClipFirst),
            bind("last", Verb::SelectClipLast),
            bind("bank-", Verb::ClipBankDelta(-1)),
            bind("bank+", Verb::ClipBankDelta(1)),
            NC,
            NC,
        ],
        EMPTY_SUBMAP,
        EMPTY_SUBMAP,
        // Make
        [
            NC,
            NC,
            NC,
            bind("cue @ clip", Verb::AddCueAtClip),
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_SUBMAP,
        EMPTY_SUBMAP,
        PANE_SUBMAP,
        META_SUBMAP,
    ],
};

/// The bank pane — the performance surface and the default focus. Go moves
/// cue selection and the edit bank; Fire is live-bank routing; Make and Cut
/// create and remove.
static BANK_TABLE: Keymap = Keymap {
    submaps: [
        // Go
        [
            GO_CUES[0],
            GO_CUES[1],
            GO_CUES[2],
            GO_CUES[3],
            bind("bank-", Verb::EditBankDelta(-1)),
            bind("bank+", Verb::EditBankDelta(1)),
            NC,
            NC,
        ],
        // Fire
        [
            bind("prev", Verb::CycleLiveBank(-1)),
            bind("send live", Verb::SendEditBankLive),
            bind("next", Verb::CycleLiveBank(1)),
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_SUBMAP,
        // Make
        [
            NC,
            NC,
            NC,
            bind("bank", Verb::AddBank),
            bind("clone bank", Verb::CloneBank),
            NC,
            NC,
            NC,
        ],
        CUT_CUE,
        EMPTY_SUBMAP,
        PANE_SUBMAP,
        META_SUBMAP,
    ],
};

/// The cue pane: the selected cue's editor. Mark trims, Tune steps the
/// advanced knobs; Go still moves selection so the pane is self-sufficient.
static CUE_TABLE: Keymap = Keymap {
    submaps: [
        // Go
        [
            GO_CUES[0], GO_CUES[1], GO_CUES[2], GO_CUES[3], NC, NC, NC, NC,
        ],
        EMPTY_SUBMAP,
        // Mark
        [
            bind("in @ playhead", Verb::MarkInToPlayhead),
            bind("out @ playhead", Verb::MarkOutToPlayhead),
            bind("preserve", Verb::CyclePreserve),
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_SUBMAP,
        CUT_CUE,
        // Tune
        [
            knob(CueParamKind::Dwell),
            knob(CueParamKind::Loop),
            knob(CueParamKind::LoopPhase),
            knob(CueParamKind::StartNudge),
            knob(CueParamKind::TrigDelay),
            knob(CueParamKind::Bpm),
            knob(CueParamKind::BpmSync),
            knob(CueParamKind::SpeedMul),
        ],
        PANE_SUBMAP,
        META_SUBMAP,
    ],
};

/// The clock pane — the beat grid, absorbing the old Beat prefix. Fire taps
/// (ff enters tap mode), Mark sets the downbeat, Cut resets, Tune steps the
/// session tempo.
static CLOCK_TABLE: Keymap = Keymap {
    submaps: [
        EMPTY_SUBMAP,
        // Fire
        [
            NC,
            bind_repeat("tap", Some(Verb::TapTempo), "tap", TAP_REPEAT),
            NC,
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        // Mark
        [
            NC,
            NC,
            bind("downbeat", Verb::TapDownbeat),
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_SUBMAP,
        // Cut
        [
            NC,
            NC,
            NC,
            NC,
            bind("soft reset", Verb::SoftReset),
            NC,
            NC,
            bind("hard reset", Verb::HardReset),
        ],
        // Tune
        [
            bind_repeat("bpm +1", Some(Verb::BpmDelta(1.0)), "bpm", BPM_REPEAT),
            bind_repeat("bpm -1", Some(Verb::BpmDelta(-1.0)), "bpm", BPM_REPEAT),
            bind_repeat(
                "nudge +",
                Some(Verb::NudgeBpm(0.001)),
                "nudge",
                NUDGE_REPEAT,
            ),
            bind_repeat(
                "nudge -",
                Some(Verb::NudgeBpm(-0.001)),
                "nudge",
                NUDGE_REPEAT,
            ),
            NC,
            NC,
            NC,
            NC,
        ],
        PANE_SUBMAP,
        META_SUBMAP,
    ],
};

/// The focused pane's keymap. Content is provisional by design — the
/// taxonomy reorganizes by editing these tables only. Token order: T1=Go
/// T2=Fire T3=Mark T4=Make T5=Cut T6=Tune T7=Pane T8=Meta (keys
/// [`KEY_TOKENS`]).
#[must_use]
pub fn pane_keymap(pane: Pane) -> &'static Keymap {
    match pane {
        Pane::Pool => &POOL_TABLE,
        Pane::Bank => &BANK_TABLE,
        Pane::Cue => &CUE_TABLE,
        Pane::Clock => &CLOCK_TABLE,
    }
}

/// Every pane, for table-sanity sweeps and pane pickers.
pub const PANES: [Pane; 4] = [Pane::Pool, Pane::Bank, Pane::Cue, Pane::Clock];

#[cfg(test)]
mod tests {
    use super::*;

    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn seq(table: &Keymap, inputs: &[Input]) -> (Machine, Vec<Step>) {
        let mut g = Machine::default();
        let steps = inputs
            .iter()
            .map(|i| g.step(table, *i, Spelling::Key))
            .collect();
        (g, steps)
    }

    const G: Input = Input::Token(Token(0));
    const F: Input = Input::Token(Token(1));
    const M: Input = Input::Token(Token(2));
    const A: Input = Input::Token(Token(3));
    const D: Input = Input::Token(Token(4));
    const T: Input = Input::Token(Token(5));
    const B: Input = Input::Token(Token(6));

    #[test]
    fn token_new_rejects_out_of_range() {
        assert_eq!(Token::new(0).map(Token::index), Some(0));
        assert_eq!(Token::new(7).map(Token::index), Some(7));
        // The whole point of the newtype: nothing out of range can be handed to
        // `step` and indexed into an 8-wide table.
        assert_eq!(Token::new(TOKEN_COUNT as u8), None);
        assert_eq!(Token::new(u8::MAX), None);
    }

    #[test]
    fn prefix_then_binding_emits_verb() {
        let (g, steps) = seq(&BANK_TABLE, &[G, M]);
        assert_eq!(steps, [Step::Pending, Step::Verb(Verb::SelectCueFirst)]);
        assert!(
            matches!(g.state, State::Idle),
            "a plain binding returns to idle"
        );
    }

    #[test]
    fn doubled_prefix_emits_hot_verb() {
        let (_, steps) = seq(&BANK_TABLE, &[F, F]);
        assert_eq!(
            steps[1],
            Step::Verb(Verb::SendEditBankLive),
            "ff sends the edit bank live"
        );
        let (_, steps) = seq(&BANK_TABLE, &[D, D]);
        assert_eq!(
            steps[1],
            Step::Verb(Verb::RemoveSelectedCue),
            "dd removes the selected cue"
        );
    }

    #[test]
    fn cancel_returns_to_idle_without_verb() {
        let (g, steps) = seq(&BANK_TABLE, &[B, Input::Cancel]);
        assert_eq!(steps[1], Step::Cancelled);
        assert!(matches!(g.state, State::Idle));
    }

    #[test]
    fn cancel_while_idle_is_rejected() {
        let (_, steps) = seq(&BANK_TABLE, &[Input::Cancel]);
        assert_eq!(
            steps,
            [Step::Rejected],
            "idle Escape falls through to the app"
        );
    }

    #[test]
    fn empty_binding_slot_keeps_the_submap_open() {
        let (g, steps) = seq(&BANK_TABLE, &[F, T]);
        assert_eq!(steps[1], Step::Pending, "unassigned slot is swallowed");
        assert!(
            matches!(g.state, State::AwaitingBinding { prefix } if prefix == Token(1)),
            "the Fire modal stays open"
        );
    }

    #[test]
    fn option_less_prefix_opens_nothing() {
        // Fire has no slots in the pool pane. Opening it would strand the user
        // in a modal with no options and no exit but Escape.
        let (g, steps) = seq(&POOL_TABLE, &[F]);
        assert_eq!(
            steps,
            [Step::Empty("Fire")],
            "the shell can name the prefix"
        );
        assert!(matches!(g.state, State::Idle), "nothing opened");
        // And the next press still means what it always meant.
        let (_, steps) = seq(&POOL_TABLE, &[F, A, A]);
        assert_eq!(
            steps[2],
            Step::Verb(Verb::AddCueAtClip),
            "aa still cues the cursored clip after a dead press"
        );
    }

    #[test]
    fn pane_prefix_focuses_panes_and_doubles_to_back() {
        let (_, steps) = seq(&BANK_TABLE, &[B, G]);
        assert_eq!(steps[1], Step::Verb(Verb::FocusPane(Pane::Pool)));
        let (_, steps) = seq(&CUE_TABLE, &[B, A]);
        assert_eq!(steps[1], Step::Verb(Verb::FocusPane(Pane::Clock)));
        let (g, steps) = seq(&POOL_TABLE, &[B, B]);
        assert_eq!(
            steps[1],
            Step::Verb(Verb::FocusPrevPane),
            "bb bounces to the previous pane"
        );
        assert!(matches!(g.state, State::Idle));
    }

    #[test]
    fn clock_fire_fire_enters_tap_repeat_and_each_press_taps() {
        let (g, steps) = seq(&CLOCK_TABLE, &[F, F, F, F]);
        assert_eq!(
            &steps[1..],
            [Step::Verb(Verb::TapTempo); 3],
            "ff starts tapping; every further f is a tap"
        );
        assert!(matches!(g.state, State::Repeat { label: "tap", .. }));
    }

    #[test]
    fn unowned_token_is_swallowed_by_the_repeat_mode() {
        let (g, steps) = seq(&CLOCK_TABLE, &[F, F, G]);
        assert_eq!(steps[2], Step::Pending, "a token tap mode doesn't own");
        assert!(
            matches!(g.state, State::Repeat { label: "tap", .. }),
            "the mode survives it — one stray press cannot reroute the next"
        );
        // Escape is the way out, and it leaves the machine where a cancel does.
        let (mut g, _) = seq(&CLOCK_TABLE, &[F, F]);
        assert_eq!(
            g.step(&CLOCK_TABLE, Input::Cancel, Spelling::Key),
            Step::Cancelled
        );
        assert!(matches!(g.state, State::Idle));
    }

    #[test]
    fn tune_binding_enters_the_nudge_repeat_and_steps_signed() {
        let (g, steps) = seq(&CUE_TABLE, &[T, G, G, G, F]);
        assert_eq!(
            steps[1],
            Step::Pending,
            "knob selection alone emits nothing"
        );
        assert_eq!(
            &steps[2..],
            [
                Step::Verb(Verb::NudgeParam(CueParamKind::Dwell, 1)),
                Step::Verb(Verb::NudgeParam(CueParamKind::Dwell, 1)),
                Step::Verb(Verb::NudgeParam(CueParamKind::Dwell, -1)),
            ]
        );
        assert!(
            matches!(g.state, State::Repeat { .. },),
            "± stays in the knob mode"
        );
    }

    #[test]
    fn go_motion_repeats_for_repeated_movement() {
        let (_, steps) = seq(&BANK_TABLE, &[G, G, G, F]);
        assert_eq!(
            &steps[1..],
            [
                Step::Verb(Verb::SelectCueDelta(-1)),
                Step::Verb(Verb::SelectCueDelta(-1)),
                Step::Verb(Verb::SelectCueDelta(1)),
            ],
            "gg moves up and stays in move mode; f moves down"
        );
    }

    #[test]
    fn pool_go_moves_clip_cursor_and_make_adds_cue() {
        let (_, steps) = seq(&POOL_TABLE, &[G, G, F]);
        assert_eq!(
            &steps[1..],
            [
                Step::Verb(Verb::SelectClipDelta(-1)),
                Step::Verb(Verb::SelectClipDelta(1)),
            ],
            "gg moves the clip cursor up; f down"
        );
        let (_, steps) = seq(&POOL_TABLE, &[A, A]);
        assert_eq!(
            steps[1],
            Step::Verb(Verb::AddCueAtClip),
            "aa cues the cursored clip"
        );
    }

    #[test]
    fn clock_tune_steps_bpm_in_the_repeat_mode() {
        let (_, steps) = seq(&CLOCK_TABLE, &[T, G, G, F]);
        assert_eq!(
            &steps[1..],
            [
                Step::Verb(Verb::BpmDelta(1.0)),
                Step::Verb(Verb::BpmDelta(1.0)),
                Step::Verb(Verb::BpmDelta(-1.0)),
            ],
            "tg enters bpm mode at +1; further g/f repeat ±"
        );
        let (_, steps) = seq(&CLOCK_TABLE, &[M, M]);
        assert_eq!(
            steps[1],
            Step::Verb(Verb::TapDownbeat),
            "mm marks the downbeat"
        );
        let (_, steps) = seq(&CLOCK_TABLE, &[D, D]);
        assert_eq!(
            steps[1],
            Step::Verb(Verb::SoftReset),
            "dd soft-resets the grid"
        );
    }

    #[test]
    fn meta_reaches_the_project_verbs_from_any_pane() {
        for pane in PANES {
            let (_, steps) = seq(pane_keymap(pane), &[Input::Token(Token(7)), A]);
            assert_eq!(steps[1], Step::Verb(Verb::OpenProjectEditor), "{pane:?} ;a");
            let (_, steps) = seq(pane_keymap(pane), &[Input::Token(Token(7)), D]);
            assert_eq!(steps[1], Step::Verb(Verb::OpenProject), "{pane:?} ;d");
        }
    }

    #[test]
    fn every_surface_spells_all_eight_tokens_and_cancel() {
        // The overlay used to read `g / f / m` no matter what was in the
        // operator's hands. Each surface names its own controls, and its
        // spelling has to line up with what `token_of_*` actually accepts.
        for (spelling, cancel) in [
            (Spelling::Key, "esc"),
            (Spelling::Pad, "Select"),
            (Spelling::Midi, "35"),
        ] {
            let tokens = spelling.tokens();
            assert_eq!(spelling.cancel(), cancel);
            for (i, t) in tokens.iter().enumerate() {
                assert!(!t.is_empty(), "{spelling:?} token {i}");
            }
        }
        for (i, name) in PAD_TOKENS.iter().enumerate() {
            // The pad's d-pad names are abbreviated in the overlay; the face
            // diamond's are the gilrs names the mapper shows.
            let button = match *name {
                "Up" => "DPadUp",
                "Down" => "DPadDown",
                "Left" => "DPadLeft",
                "Right" => "DPadRight",
                other => other,
            };
            let src = ControlSource::PadButton {
                device: String::new(),
                button: button.into(),
            };
            assert_eq!(
                token_of_source(&src),
                Some(Input::Token(Token(i as u8))),
                "{name} is spelled but does not resolve"
            );
            assert_eq!(Spelling::of_source(&src), Spelling::Pad);
        }
        for (i, note) in MIDI_TOKENS.iter().enumerate() {
            let src = ControlSource::MidiNote {
                device: String::new(),
                channel: 1,
                note: note.parse().expect("a MIDI spelling is a note number"),
            };
            assert_eq!(token_of_source(&src), Some(Input::Token(Token(i as u8))));
            assert_eq!(Spelling::of_source(&src), Spelling::Midi);
        }
    }

    #[test]
    fn key_tokens_round_trip() {
        for (i, k) in KEY_TOKENS.iter().enumerate() {
            assert_eq!(
                token_of_key(k),
                Some(Input::Token(Token(i as u8))),
                "key {k:?}"
            );
        }
        assert_eq!(token_of_key("Escape"), Some(Input::Cancel));
        assert_eq!(token_of_key("z"), None);
        assert_eq!(token_of_key("Space"), None);
    }

    #[test]
    fn pad_buttons_map_dpad_then_diamond() {
        let names = [
            "DPadUp",
            "DPadDown",
            "DPadLeft",
            "DPadRight",
            "North",
            "South",
            "East",
            "West",
        ];
        for (i, name) in names.iter().enumerate() {
            let src = ControlSource::PadButton {
                device: String::new(),
                button: (*name).into(),
            };
            assert_eq!(
                token_of_source(&src),
                Some(Input::Token(Token(i as u8))),
                "{name}"
            );
        }
        let select = ControlSource::PadButton {
            device: String::new(),
            button: "Select".into(),
        };
        assert_eq!(token_of_source(&select), Some(Input::Cancel));
        let start = ControlSource::PadButton {
            device: String::new(),
            button: "Start".into(),
        };
        assert_eq!(token_of_source(&start), None);
    }

    #[test]
    fn midi_notes_map_tokens_and_cancel() {
        for (i, note) in (36..=43).enumerate() {
            let src = ControlSource::MidiNote {
                device: String::new(),
                channel: 1,
                note,
            };
            assert_eq!(
                token_of_source(&src),
                Some(Input::Token(Token(i as u8))),
                "note {note}"
            );
        }
        let cancel = ControlSource::MidiNote {
            device: String::new(),
            channel: 1,
            note: 35,
        };
        assert_eq!(token_of_source(&cancel), Some(Input::Cancel));
        let out = ControlSource::MidiNote {
            device: String::new(),
            channel: 1,
            note: 44,
        };
        assert_eq!(token_of_source(&out), None);
    }

    #[test]
    fn every_pane_keymap_is_labelled_and_shares_the_global_prefixes() {
        // The prefixes are named once, so "T1 is Go in every pane" is now a
        // property of the module rather than four hand-copied lists.
        for (i, label) in PREFIX_LABELS.iter().enumerate() {
            assert!(!label.is_empty(), "prefix {i} has a label");
        }
        assert_eq!(PREFIX_LABELS[0], "Go");
        assert_eq!(PREFIX_LABELS[6], "Pane", "the pane selector stays on T7");
        assert_eq!(PREFIX_LABELS[7], "Meta", "Meta stays on T8");
        for pane in PANES {
            let table = pane_keymap(pane);
            for (i, submap) in table.submaps.iter().enumerate() {
                for bind in submap.iter().flatten() {
                    assert!(
                        !bind.label.is_empty(),
                        "{pane:?} {} slot has a label",
                        PREFIX_LABELS[i]
                    );
                }
            }
            let focus_slots = table.submaps[6]
                .iter()
                .flatten()
                .filter(|c| matches!(c.verb, Some(Verb::FocusPane(_))))
                .count();
            assert_eq!(focus_slots, PANES.len(), "{pane:?} can focus every pane");
        }
    }

    #[test]
    fn a_table_built_at_runtime_steps_like_a_compiled_one() {
        // Nothing in the machine outlives the call, so the table need not be a
        // module static — which is the precondition for keymaps the user edits
        // (that is what `vidiotic-ctl` exists for), not the feature itself.
        let mut submaps = [EMPTY_SUBMAP; TOKEN_COUNT];
        submaps[0] = {
            let mut r = EMPTY_SUBMAP;
            r[0] = bind_repeat(
                "louder",
                Some(Verb::BpmDelta(2.0)),
                "gain",
                pm_repeat("up", Verb::BpmDelta(2.0), "down", Verb::BpmDelta(-2.0)),
            );
            r
        };
        let table = Keymap { submaps };

        let (g, steps) = seq(&table, &[G, G, F]);
        assert_eq!(
            &steps[1..],
            [
                Step::Verb(Verb::BpmDelta(2.0)),
                Step::Verb(Verb::BpmDelta(-2.0))
            ],
            "the runtime table's own repeat mode repeats"
        );
        assert!(matches!(g.state, State::Repeat { label: "gain", .. }));
        // And its empty submaps behave like the compiled ones.
        let (_, steps) = seq(&table, &[F]);
        assert_eq!(steps, [Step::Empty("Fire")]);
    }

    #[test]
    fn every_reachable_repeat_entry_is_labelled() {
        // The overlay used to reverse-engineer these from the verb payload,
        // ending in `_ => "again"` — so a new repeat verb got a *wrong* label
        // rather than a missing one, and nothing failed. This is the check
        // that inference is gone for good.
        let mut checked = 0;
        for pane in PANES {
            for submap in &pane_keymap(pane).submaps {
                for bind in submap.iter().flatten() {
                    let Some((mode, entries)) = &bind.repeat else {
                        continue;
                    };
                    assert!(!mode.is_empty(), "{pane:?} {} mode label", bind.label);
                    for (i, e) in entries.iter().enumerate() {
                        let Some(e) = e else { continue };
                        assert!(!e.label.is_empty(), "{pane:?} {mode} slot {i}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 0, "the sweep found no repeat entries at all");
    }

    #[test]
    fn every_cue_knob_is_reachable_from_some_binding() {
        // `CueParamKind` has exactly 8 variants and the cue pane's Tune prefix
        // has exactly 8 slots. A ninth knob has nowhere to go, and without
        // this nothing fails — it is simply unreachable from the keymap.
        for kind in CueParamKind::ALL {
            // Exhaustive on purpose: a new variant fails to compile here, and
            // then fails at runtime until `ALL` grows to match.
            let slot = match kind {
                CueParamKind::Dwell => 0,
                CueParamKind::Loop => 1,
                CueParamKind::LoopPhase => 2,
                CueParamKind::StartNudge => 3,
                CueParamKind::TrigDelay => 4,
                CueParamKind::Bpm => 5,
                CueParamKind::BpmSync => 6,
                CueParamKind::SpeedMul => 7,
            };
            assert_eq!(CueParamKind::ALL[slot], kind, "ALL is missing a knob");

            let nudges = |v: Option<Verb>| matches!(v, Some(Verb::NudgeParam(k, _)) if k == kind);
            let reachable = PANES.iter().any(|pane| {
                pane_keymap(*pane).submaps.iter().any(|submap| {
                    submap.iter().flatten().any(|c| {
                        nudges(c.verb)
                            || c.repeat.is_some_and(|(_, entries)| {
                                entries.iter().flatten().any(|e| nudges(Some(e.verb)))
                            })
                    })
                })
            });
            assert!(reachable, "{} is bound nowhere", kind.label());
        }
    }

    #[test]
    fn every_pane_is_reachable_from_every_other() {
        for from in PANES {
            let table = pane_keymap(from);
            for to in PANES {
                let reachable = table.submaps[6]
                    .iter()
                    .flatten()
                    .any(|c| c.verb == Some(Verb::FocusPane(to)));
                assert!(reachable, "{from:?} → {to:?}");
            }
        }
    }
}
