//! Modal command grammar: 6 pane-sensitive verb-roots plus two global nouns
//! (Pane, Meta), each opening a which-key modal of up to 8 conjugations; a
//! second token resolves to a [`Verb`]. Rising-edge only.
//!
//! The verbs keep fixed meanings — Go moves, Cut deletes, Tune enters knob
//! modes — and the focused [`Pane`] supplies the object: Fire in the clock
//! pane taps tempo, Cut in the bank pane removes the selected cue. Pure state
//! machine — no App or UI dependencies. Verbs are context-free ("remove the
//! selected cue", not "remove cue #7"); the app resolves selection and bank
//! context when a verb is emitted. All grammar *content* lives in the
//! per-pane table statics (see [`pane_table`]) so the taxonomy can be
//! reorganized without touching the machinery; [`Grammar::step`] takes the
//! table as a parameter and the app passes the focused pane's each press.

use vidiotic_ctl::model::ControlSource;

use crate::commands::CueParamKind;

/// How many abstract grammar inputs there are. Every table in this module is
/// this wide, and [`Token`] indexes them.
pub const TOKEN_COUNT: usize = 8;

/// One of the [`TOKEN_COUNT`] abstract grammar inputs. Every edge source
/// (keyboard, pad, MIDI) reduces to one of these before the state machine sees
/// it.
///
/// A newtype rather than a bare `u8` because this type is public and the whole
/// module indexes fixed-width arrays with it — roots, conjugations, sticky
/// entries, [`KEY_TOKENS`]. As an alias, a caller outside the crate could hand
/// `Grammar::step` a `9` and panic it on an out-of-bounds index; the range
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

    /// Index into a `TOKEN_COUNT`-wide grammar table. In range by construction.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A grammar-relevant input event: one of the 8 tokens, or the cancel key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Token(Token),
    Cancel,
}

/// A focusable region of the app the verb keys act on. Selecting one (via the
/// Pane root) swaps which [`GrammarTable`] the machine resolves against.
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

/// One repeat slot of a sticky mode: what it fires, and what the overlay
/// calls it. Carries its own label for the same reason [`Conjugation`] does —
/// inferring one from the verb payload can only ever be right for the verbs
/// that were thought of at the time.
#[derive(Clone, Copy, Debug)]
pub struct StickyEntry {
    pub label: &'static str,
    pub verb: Verb,
}

/// Per-token entries of a sticky mode. A populated slot fires its verb and
/// stays in the mode; a token the mode does not own is swallowed, exactly as
/// under an open root — one stray-token rule for both pending states. Leaving
/// a mode is Escape.
pub type StickyTable = [Option<StickyEntry>; TOKEN_COUNT];

/// One resolvable slot under a root. `verb` fires on selection (`None` for
/// pure mode entry); `sticky` is a `(mode label, table)` the machine enters
/// afterwards, for repeat-friendly terminals (tap tempo, knob ±).
#[derive(Clone, Copy, Debug)]
pub struct Conjugation {
    pub label: &'static str,
    pub verb: Option<Verb>,
    pub sticky: Option<(&'static str, StickyTable)>,
}

/// One verb-root's conjugation slots, indexed by [`Token`]. The root's own
/// token slot holds its "doubled" hot verb — doubling needs no special
/// machinery. A root can be entirely empty in a pane its verb doesn't apply
/// to; pressing it opens nothing at all ([`Step::Empty`]) rather than a modal
/// with no way out.
///
/// No label: a root's name is [`PREFIX_LABELS`], the same in every pane.
pub type RootEntry = [Option<Conjugation>; TOKEN_COUNT];

/// A complete grammar: one root per token.
#[derive(Clone, Copy, Debug)]
pub struct GrammarTable {
    pub roots: [RootEntry; TOKEN_COUNT],
}

/// Where the machine is between presses.
///
/// 280 bytes, nearly all of it `Sticky`'s entries, and there is exactly one of
/// these per app — boxing the variant would trade a `Copy` state and a
/// borrow-free machine for a heap allocation on every mode entry.
#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::large_enum_variant)]
pub enum GrammarState {
    #[default]
    Idle,
    /// A root is held open; the which-key modal shows its conjugations.
    AwaitingConjugation { root: Token },
    /// A repeat-friendly terminal mode; `trail_root` is the root that led
    /// here, for the input-trail display.
    ///
    /// The entries are held *by value*. A `StickyTable` is `Copy` and eight
    /// entries wide, so entering a mode costs one memcpy of a couple of
    /// hundred bytes — and in exchange the machine borrows nothing from the
    /// table it stepped against, which is what a table that is not compiled in
    /// would need.
    Sticky {
        label: &'static str,
        entries: StickyTable,
        trail_root: Token,
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
    /// Consumed, but nothing opened: the pressed root has no slots filled in
    /// this pane. Carries the root's label so the shell can say which one.
    ///
    /// Distinct from [`Self::Rejected`], which means "not consumed, fall
    /// through": this press *was* the grammar's, it just had nowhere to go.
    /// The machine stays idle — an option-less modal is a trap, since under
    /// the swallow rule nothing but Escape gets out of it.
    Empty(&'static str),
}

/// The grammar state machine. One per app; feed it [`Input`]s via
/// [`Self::step`].
#[derive(Default)]
pub struct Grammar {
    pub state: GrammarState,
}

impl Grammar {
    /// Advance on one input, resolving against the focused pane's table.
    ///
    /// The table is borrowed only for the call: nothing in the machine
    /// outlives it (see [`GrammarState::Sticky`]), so a table built at runtime
    /// works exactly as the compiled-in ones do.
    pub fn step(&mut self, table: &GrammarTable, input: Input) -> Step {
        match (&self.state, input) {
            (GrammarState::Idle, Input::Cancel) => Step::Rejected,
            (_, Input::Cancel) => {
                self.state = GrammarState::Idle;
                Step::Cancelled
            }
            (GrammarState::Idle, Input::Token(t)) => {
                // Never open a root with nothing under it: the modal would show
                // no options and then swallow every press until Escape.
                if table.roots[t.index()].iter().all(Option::is_none) {
                    Step::Empty(PREFIX_LABELS[t.index()])
                } else {
                    self.state = GrammarState::AwaitingConjugation { root: t };
                    Step::Pending
                }
            }
            (GrammarState::AwaitingConjugation { root }, Input::Token(t)) => {
                match &table.roots[root.index()][t.index()] {
                    // Forgiving: an empty slot keeps the modal open.
                    None => Step::Pending,
                    Some(conj) => {
                        let root = *root;
                        self.state = match conj.sticky {
                            Some((label, entries)) => GrammarState::Sticky {
                                label,
                                entries,
                                trail_root: root,
                            },
                            None => GrammarState::Idle,
                        };
                        match conj.verb {
                            Some(v) => Step::Verb(v),
                            None => Step::Pending,
                        }
                    }
                }
            }
            (GrammarState::Sticky { entries, .. }, Input::Token(t)) => {
                // Swallowed, as under an open root. Re-rooting here would let a
                // stray press silently change what the *next* press means,
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
        self.state = GrammarState::Idle;
    }
}

/// What each root is called, in token order — the verb keys first, then the
/// two global nouns. One list, not one per pane: a verb keeps a fixed meaning
/// wherever it applies and the focused pane supplies the object, so nothing in
/// a table gets to disagree about what T1 is called. A pane the verb doesn't
/// apply to leaves its slots empty; it does not rename it.
pub const PREFIX_LABELS: [&str; TOKEN_COUNT] =
    ["Go", "Fire", "Mark", "Make", "Cut", "Tune", "Pane", "Meta"];

/// The canonical keyboard spelling of each token, in token order. These are
/// the strings `control_input::canon_key` / `vidiotic_ctl::keys` produce.
pub const KEY_TOKENS: [&str; TOKEN_COUNT] = ["g", "f", "m", "a", "d", "t", "b", ";"];

/// Map a canonical key name to a grammar input. `Escape` cancels.
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

/// Map a non-keyboard edge source to a grammar input: gamepad d-pad and face
/// diamond are the 8 tokens (`Select` cancels), and MIDI notes 36–43 are the
/// 8 tokens (35 cancels) on any device or channel. Keyboard sources return
/// `None` — keys reach the grammar through `handle_key`, not the event pump.
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

const NC: Option<Conjugation> = None;
const NE: Option<StickyEntry> = None;

/// A plain conjugation: emit and return to idle.
const fn conj(label: &'static str, verb: Verb) -> Option<Conjugation> {
    Some(Conjugation {
        label,
        verb: Some(verb),
        sticky: None,
    })
}

/// A conjugation that (optionally) emits, then enters a sticky mode.
const fn conj_mode(
    label: &'static str,
    verb: Option<Verb>,
    mode: &'static str,
    entries: StickyTable,
) -> Option<Conjugation> {
    Some(Conjugation {
        label,
        verb,
        sticky: Some((mode, entries)),
    })
}

/// One repeat slot: the verb, and what the overlay calls it.
const fn repeat(label: &'static str, verb: Verb) -> Option<StickyEntry> {
    Some(StickyEntry { label, verb })
}

/// A ± sticky table: T1 fires `up`, T2 fires `down`.
const fn pm_sticky(
    up_label: &'static str,
    up: Verb,
    down_label: &'static str,
    down: Verb,
) -> StickyTable {
    let mut t = [NE; TOKEN_COUNT];
    t[0] = repeat(up_label, up);
    t[1] = repeat(down_label, down);
    t
}

/// The ± sticky table for one advanced cue knob: T1 steps up, T2 down.
const fn knob_sticky(kind: CueParamKind) -> StickyTable {
    pm_sticky(
        "step +",
        Verb::NudgeParam(kind, 1),
        "step -",
        Verb::NudgeParam(kind, -1),
    )
}

/// A Tune conjugation: select the knob, then ± in sticky mode.
const fn knob(kind: CueParamKind) -> Option<Conjugation> {
    conj_mode(kind.label(), None, kind.label(), knob_sticky(kind))
}

/// A root the focused pane has no use for. Pressing it opens nothing — see
/// [`Step::Empty`].
const EMPTY_ROOT: RootEntry = [NC; TOKEN_COUNT];

/// Cue-selection movement mode: T1 up, T2 down, entered from Go.
const MOVE_STICKY: StickyTable = pm_sticky(
    "up",
    Verb::SelectCueDelta(-1),
    "down",
    Verb::SelectCueDelta(1),
);

/// Clip-cursor movement mode in the pool pane: T1 up, T2 down.
const CLIP_MOVE_STICKY: StickyTable = pm_sticky(
    "up",
    Verb::SelectClipDelta(-1),
    "down",
    Verb::SelectClipDelta(1),
);

/// Tap mode: every further Fire press is a tap.
const TAP_STICKY: StickyTable = {
    let mut t = [NE; TOKEN_COUNT];
    t[1] = repeat("tap", Verb::TapTempo);
    t
};

/// Session-tempo ± modes for the clock pane's Tune root.
const BPM_STICKY: StickyTable = pm_sticky(
    "step +",
    Verb::BpmDelta(1.0),
    "step -",
    Verb::BpmDelta(-1.0),
);
const NUDGE_STICKY: StickyTable = pm_sticky(
    "step +",
    Verb::NudgeBpm(0.001),
    "step -",
    Verb::NudgeBpm(-0.001),
);

/// The Pane root, identical in every table: T1–T4 focus a pane, doubled (bb)
/// bounces back to the previous one. A pane press while its own pane is
/// focused simply re-focuses it — harmless.
const PANE_ROOT: RootEntry = [
    conj("pool", Verb::FocusPane(Pane::Pool)),
    conj("bank", Verb::FocusPane(Pane::Bank)),
    conj("cue", Verb::FocusPane(Pane::Cue)),
    conj("clock", Verb::FocusPane(Pane::Clock)),
    NC,
    NC,
    conj("back", Verb::FocusPrevPane),
    NC,
];

/// The Meta root, identical in every table: app-level nouns the pane never
/// recolors.
const META_ROOT: RootEntry = [
    conj("save", Verb::SaveProject),
    conj("fullscreen", Verb::ToggleFullscreen),
    conj("advanced", Verb::ToggleAdvanced),
    conj("edit proj", Verb::OpenProjectEditor),
    conj("open proj", Verb::OpenProject),
    conj("palette", Verb::ToggleCommandPalette),
    NC,
    conj("grammar off", Verb::GrammarOff),
];

/// Go through the edit bank's cue order: shared by the bank and cue panes.
const GO_CUES: [Option<Conjugation>; 4] = [
    conj_mode("up", Some(Verb::SelectCueDelta(-1)), "move", MOVE_STICKY),
    conj_mode("down", Some(Verb::SelectCueDelta(1)), "move", MOVE_STICKY),
    conj("first", Verb::SelectCueFirst),
    conj("last", Verb::SelectCueLast),
];

/// Cut's shared "dd removes the selected cue" slot for the bank and cue panes.
const CUT_CUE: RootEntry = [
    NC,
    NC,
    NC,
    NC,
    conj("cue", Verb::RemoveSelectedCue),
    NC,
    NC,
    NC,
];

/// The pool pane: the clip grid. Go moves the clip cursor (and clip banks);
/// Make turns the cursored clip into a cue.
static POOL_TABLE: GrammarTable = GrammarTable {
    roots: [
        // Go
        [
            conj_mode(
                "up",
                Some(Verb::SelectClipDelta(-1)),
                "move",
                CLIP_MOVE_STICKY,
            ),
            conj_mode(
                "down",
                Some(Verb::SelectClipDelta(1)),
                "move",
                CLIP_MOVE_STICKY,
            ),
            conj("first", Verb::SelectClipFirst),
            conj("last", Verb::SelectClipLast),
            conj("bank-", Verb::ClipBankDelta(-1)),
            conj("bank+", Verb::ClipBankDelta(1)),
            NC,
            NC,
        ],
        EMPTY_ROOT,
        EMPTY_ROOT,
        // Make
        [
            NC,
            NC,
            NC,
            conj("cue @ clip", Verb::AddCueAtClip),
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_ROOT,
        EMPTY_ROOT,
        PANE_ROOT,
        META_ROOT,
    ],
};

/// The bank pane — the performance surface and the default focus. Go moves
/// cue selection and the edit bank; Fire is live-bank routing; Make and Cut
/// create and remove.
static BANK_TABLE: GrammarTable = GrammarTable {
    roots: [
        // Go
        [
            GO_CUES[0],
            GO_CUES[1],
            GO_CUES[2],
            GO_CUES[3],
            conj("bank-", Verb::EditBankDelta(-1)),
            conj("bank+", Verb::EditBankDelta(1)),
            NC,
            NC,
        ],
        // Fire
        [
            conj("prev", Verb::CycleLiveBank(-1)),
            conj("send live", Verb::SendEditBankLive),
            conj("next", Verb::CycleLiveBank(1)),
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_ROOT,
        // Make
        [
            NC,
            NC,
            NC,
            conj("bank", Verb::AddBank),
            conj("clone bank", Verb::CloneBank),
            NC,
            NC,
            NC,
        ],
        CUT_CUE,
        EMPTY_ROOT,
        PANE_ROOT,
        META_ROOT,
    ],
};

/// The cue pane: the selected cue's editor. Mark trims, Tune steps the
/// advanced knobs; Go still moves selection so the pane is self-sufficient.
static CUE_TABLE: GrammarTable = GrammarTable {
    roots: [
        // Go
        [
            GO_CUES[0], GO_CUES[1], GO_CUES[2], GO_CUES[3], NC, NC, NC, NC,
        ],
        EMPTY_ROOT,
        // Mark
        [
            conj("in @ playhead", Verb::MarkInToPlayhead),
            conj("out @ playhead", Verb::MarkOutToPlayhead),
            conj("preserve", Verb::CyclePreserve),
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_ROOT,
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
        PANE_ROOT,
        META_ROOT,
    ],
};

/// The clock pane — the beat grid, absorbing the old Beat root. Fire taps
/// (ff enters tap mode), Mark sets the downbeat, Cut resets, Tune steps the
/// session tempo.
static CLOCK_TABLE: GrammarTable = GrammarTable {
    roots: [
        EMPTY_ROOT,
        // Fire
        [
            NC,
            conj_mode("tap", Some(Verb::TapTempo), "tap", TAP_STICKY),
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
            conj("downbeat", Verb::TapDownbeat),
            NC,
            NC,
            NC,
            NC,
            NC,
        ],
        EMPTY_ROOT,
        // Cut
        [
            NC,
            NC,
            NC,
            NC,
            conj("soft reset", Verb::SoftReset),
            NC,
            NC,
            conj("hard reset", Verb::HardReset),
        ],
        // Tune
        [
            conj_mode("bpm +1", Some(Verb::BpmDelta(1.0)), "bpm", BPM_STICKY),
            conj_mode("bpm -1", Some(Verb::BpmDelta(-1.0)), "bpm", BPM_STICKY),
            conj_mode(
                "nudge +",
                Some(Verb::NudgeBpm(0.001)),
                "nudge",
                NUDGE_STICKY,
            ),
            conj_mode(
                "nudge -",
                Some(Verb::NudgeBpm(-0.001)),
                "nudge",
                NUDGE_STICKY,
            ),
            NC,
            NC,
            NC,
            NC,
        ],
        PANE_ROOT,
        META_ROOT,
    ],
};

/// The focused pane's grammar. Content is provisional by design — the
/// taxonomy reorganizes by editing these tables only. Token order: T1=Go
/// T2=Fire T3=Mark T4=Make T5=Cut T6=Tune T7=Pane T8=Meta (keys
/// [`KEY_TOKENS`]).
#[must_use]
pub fn pane_table(pane: Pane) -> &'static GrammarTable {
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

    fn seq(table: &GrammarTable, inputs: &[Input]) -> (Grammar, Vec<Step>) {
        let mut g = Grammar::default();
        let steps = inputs.iter().map(|i| g.step(table, *i)).collect();
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
    fn root_then_conjugation_emits_verb() {
        let (g, steps) = seq(&BANK_TABLE, &[G, M]);
        assert_eq!(steps, [Step::Pending, Step::Verb(Verb::SelectCueFirst)]);
        assert!(
            matches!(g.state, GrammarState::Idle),
            "plain conjugation returns to idle"
        );
    }

    #[test]
    fn doubled_root_emits_hot_verb() {
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
        assert!(matches!(g.state, GrammarState::Idle));
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
    fn empty_conjugation_slot_keeps_modal_pending() {
        let (g, steps) = seq(&BANK_TABLE, &[F, T]);
        assert_eq!(steps[1], Step::Pending, "unassigned slot is swallowed");
        assert!(
            matches!(g.state, GrammarState::AwaitingConjugation { root } if root == Token(1)),
            "the Fire modal stays open"
        );
    }

    #[test]
    fn option_less_root_opens_nothing() {
        // Fire has no slots in the pool pane. Opening it would strand the user
        // in a modal with no options and no exit but Escape.
        let (g, steps) = seq(&POOL_TABLE, &[F]);
        assert_eq!(steps, [Step::Empty("Fire")], "the shell can name the root");
        assert!(matches!(g.state, GrammarState::Idle), "nothing opened");
        // And the next press still means what it always meant.
        let (_, steps) = seq(&POOL_TABLE, &[F, A, A]);
        assert_eq!(
            steps[2],
            Step::Verb(Verb::AddCueAtClip),
            "aa still cues the cursored clip after a dead press"
        );
    }

    #[test]
    fn pane_root_focuses_panes_and_doubles_to_back() {
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
        assert!(matches!(g.state, GrammarState::Idle));
    }

    #[test]
    fn clock_fire_fire_enters_tap_sticky_and_each_repeat_taps() {
        let (g, steps) = seq(&CLOCK_TABLE, &[F, F, F, F]);
        assert_eq!(
            &steps[1..],
            [Step::Verb(Verb::TapTempo); 3],
            "ff starts tapping; every further f is a tap"
        );
        assert!(matches!(g.state, GrammarState::Sticky { label: "tap", .. }));
    }

    #[test]
    fn unowned_token_is_swallowed_by_sticky() {
        let (g, steps) = seq(&CLOCK_TABLE, &[F, F, G]);
        assert_eq!(steps[2], Step::Pending, "a token tap mode doesn't own");
        assert!(
            matches!(g.state, GrammarState::Sticky { label: "tap", .. }),
            "the mode survives it — one stray press cannot reroute the next"
        );
        // Escape is the way out, and it leaves the machine where a cancel does.
        let (mut g, _) = seq(&CLOCK_TABLE, &[F, F]);
        assert_eq!(g.step(&CLOCK_TABLE, Input::Cancel), Step::Cancelled);
        assert!(matches!(g.state, GrammarState::Idle));
    }

    #[test]
    fn tune_conjugation_enters_nudge_sticky_and_steps_signed() {
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
            matches!(g.state, GrammarState::Sticky { .. },),
            "± stays in the knob mode"
        );
    }

    #[test]
    fn go_motion_is_sticky_for_repeated_movement() {
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
    fn clock_tune_steps_bpm_in_sticky() {
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
            let (_, steps) = seq(pane_table(pane), &[Input::Token(Token(7)), A]);
            assert_eq!(steps[1], Step::Verb(Verb::OpenProjectEditor), "{pane:?} ;a");
            let (_, steps) = seq(pane_table(pane), &[Input::Token(Token(7)), D]);
            assert_eq!(steps[1], Step::Verb(Verb::OpenProject), "{pane:?} ;d");
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
    fn every_pane_table_is_labelled_and_shares_the_global_roots() {
        // The roots are named once, so "T1 is Go in every pane" is now a
        // property of the module rather than four hand-copied lists.
        for (i, label) in PREFIX_LABELS.iter().enumerate() {
            assert!(!label.is_empty(), "root {i} has a label");
        }
        assert_eq!(PREFIX_LABELS[0], "Go");
        assert_eq!(PREFIX_LABELS[6], "Pane", "the pane selector stays on T7");
        assert_eq!(PREFIX_LABELS[7], "Meta", "Meta stays on T8");
        for pane in PANES {
            let table = pane_table(pane);
            for (i, root) in table.roots.iter().enumerate() {
                for conj in root.iter().flatten() {
                    assert!(
                        !conj.label.is_empty(),
                        "{pane:?} {} slot has a label",
                        PREFIX_LABELS[i]
                    );
                }
            }
            let focus_slots = table.roots[6]
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
        let mut roots = [EMPTY_ROOT; TOKEN_COUNT];
        roots[0] = {
            let mut r = EMPTY_ROOT;
            r[0] = conj_mode(
                "louder",
                Some(Verb::BpmDelta(2.0)),
                "gain",
                pm_sticky("up", Verb::BpmDelta(2.0), "down", Verb::BpmDelta(-2.0)),
            );
            r
        };
        let table = GrammarTable { roots };

        let (g, steps) = seq(&table, &[G, G, F]);
        assert_eq!(
            &steps[1..],
            [
                Step::Verb(Verb::BpmDelta(2.0)),
                Step::Verb(Verb::BpmDelta(-2.0))
            ],
            "the runtime table's own sticky mode repeats"
        );
        assert!(matches!(
            g.state,
            GrammarState::Sticky { label: "gain", .. }
        ));
        // And its empty roots behave like the compiled ones.
        let (_, steps) = seq(&table, &[F]);
        assert_eq!(steps, [Step::Empty("Fire")]);
    }

    #[test]
    fn every_reachable_repeat_entry_is_labelled() {
        // The overlay used to reverse-engineer these from the verb payload,
        // ending in `_ => "again"` — so a new sticky verb got a *wrong* label
        // rather than a missing one, and nothing failed. This is the check
        // that inference is gone for good.
        let mut checked = 0;
        for pane in PANES {
            for root in &pane_table(pane).roots {
                for conj in root.iter().flatten() {
                    let Some((mode, entries)) = &conj.sticky else {
                        continue;
                    };
                    assert!(!mode.is_empty(), "{pane:?} {} mode label", conj.label);
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
    fn every_pane_is_reachable_from_every_other() {
        for from in PANES {
            let table = pane_table(from);
            for to in PANES {
                let reachable = table.roots[6]
                    .iter()
                    .flatten()
                    .any(|c| c.verb == Some(Verb::FocusPane(to)));
                assert!(reachable, "{from:?} → {to:?}");
            }
        }
    }
}
