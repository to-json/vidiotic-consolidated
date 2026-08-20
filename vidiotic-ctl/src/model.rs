//! The serialized control-mapping vocabulary (nanoserde RON), embedded in
//! `.viproj` and the global map file.
//!
//! Rules mirrored from `vidiotic::project`: every field added to an already-
//! shipped struct gets `#[nserde(default)]`, and there is deliberately no
//! `Option<T>` anywhere in this model — nanoserde's RON `Option` syntax
//! (`Some` serializes as the bare value, `None` omits the field) makes
//! hand-edited fixtures and some nested shapes fragile, and this format is
//! meant to be hand-editable. Struct-variant enums are fine (precedent:
//! `vidiotic::project::CueEffectSpec::Isf`).

use nanoserde::{DeRon, SerRon};

/// Where a control event comes from. `device` is `""` in a binding to mean
/// "any device"; a live [`crate::event::ControlEvent`] always carries a
/// concrete device name.
#[derive(SerRon, DeRon, Clone, Debug, PartialEq)]
pub enum ControlSource {
    /// `channel` is 1-16 (already offset from the MIDI wire's 0-15).
    MidiNote { device: String, channel: u8, note: u8 },
    MidiCc { device: String, channel: u8, cc: u8 },
    Key { key: String, ctrl: bool, alt: bool, shift: bool, cmd: bool },
    /// `button` is a gilrs `Debug`-formatted button name, e.g. `"South"`.
    PadButton { device: String, button: String },
    PadAxis { device: String, axis: String },
}

/// What a binding does in `vidiotic-prep`'s span editor. Namespaced under
/// [`Action::Prep`] because both apps' bindings share one file format and one
/// editor; each app's `to_command` rejects the other's verbs.
///
/// `Copy` with scalar-only params, like the rest of [`Action`]: only the
/// *bindable* subset of prep's commands lives here (the heap-carrying ones —
/// open this path, name this span — have no verb), so [`CATALOG`] stays a
/// `const`.
#[derive(SerRon, DeRon, Clone, Copy, Debug, PartialEq)]
pub enum PrepVerb {
    TogglePlay,
    Pause,
    /// Seek to the in mark and play forward at 1x (prep's shift+space).
    PlayFromIn,
    /// J/L shuttle: a press in the current direction doubles speed, a press
    /// the other way (or from pause) starts 1x that way. The speed state
    /// lives in the app, so this stays edge-triggered.
    Shuttle { dir: i32 },
    /// Pause and step the playhead by `frames` (signed). The only verb that
    /// re-fires on key-repeat — see
    /// `vidiotic_prep::commands::Command::repeats_on_hold`.
    Step { frames: i32 },
    SeekStart,
    SeekEnd,
    JumpToIn,
    JumpToOut,
    SetIn,
    SetOut,
    SnapOut,
    AddSpan,
    /// Zoom the jog window by `factor` (<1 zooms in), anchored on the playhead.
    ZoomView { factor: f64 },
    ZoomFit,
    ZoomToMarks,
    /// Continuous: value in `0..=1` seeks across the whole source — a fader
    /// or CC as a jog wheel.
    Scrub,
}

impl PrepVerb {
    /// Human-readable name for the action picker.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::TogglePlay => "Toggle Play",
            Self::Pause => "Pause",
            Self::PlayFromIn => "Play From In",
            Self::Shuttle { .. } => "Shuttle",
            Self::Step { .. } => "Step",
            Self::SeekStart => "Seek Start",
            Self::SeekEnd => "Seek End",
            Self::JumpToIn => "Jump To In",
            Self::JumpToOut => "Jump To Out",
            Self::SetIn => "Set In",
            Self::SetOut => "Set Out",
            Self::SnapOut => "Snap Out",
            Self::AddSpan => "Add Span",
            Self::ZoomView { .. } => "Zoom View",
            Self::ZoomFit => "Zoom Fit",
            Self::ZoomToMarks => "Zoom To Marks",
            Self::Scrub => "Scrub",
        }
    }
}

/// What a binding does. Deliberately not `vidiotic::commands::Command`:
/// this crate must not depend on `vidiotic` (vidiotic depends on it for the
/// `.viproj` embed — a dependency the other way would cycle), so params are
/// baked directly into variants and `vidiotic` owns the `Action -> Command`
/// translation.
///
/// Two vocabularies share this enum: the unprefixed variants are the player's
/// (`vidiotic`), and [`Self::Prep`] namespaces `vidiotic-prep`'s. The player's
/// stayed unprefixed rather than moving under a symmetric `Player(..)` so that
/// every `.vmap`/`.viproj` written before prep gained bindings still parses
/// byte-for-byte — nanoserde errors on an unknown variant, and
/// `vidiotic::project::migrate` runs *after* deserialization, so a rename here
/// would strand every existing file with no migration path. A new app gets its
/// own namespace variant. [`Self::Nothing`] belongs to neither — it is the
/// universal mask.
#[derive(SerRon, DeRon, Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Matches-and-masks: a binding with this action always resolves to
    /// nothing, even if a lower layer has a binding for the same source.
    Nothing,
    TapDownbeat,
    TapTempo,
    SoftReset,
    HardReset,
    CaptureShader,
    ToggleFullscreen,
    SaveProject,
    BpmDelta { amount: f64 },
    NudgeBpm { ratio: f64 },
    CycleLiveBank { delta: i32 },
    SetLiveBank { index: u32 },
    SetEditBank { index: u32 },
    /// Continuous: value in `0..=1` lerps between `min` and `max`.
    SetBpm { min: f64, max: f64 },
    ToggleCommandPalette,
    Quit,
    /// Append one digit to the player's pending BPM entry;
    /// [`Self::BpmCommit`] parses it into a tempo and [`Self::BpmClear`]
    /// abandons it.
    ///
    /// Ten bindings rather than one, because a binding names one physical
    /// control — which is what makes the entry reachable from a numeric pad
    /// and not just the number row.
    BpmDigit { digit: u8 },
    BpmCommit,
    BpmClear,
    Prep(PrepVerb),
}

/// The catalogs, expanded from one list of actions so the union can never
/// drift from the per-app halves. `Nothing` is the universal mask and belongs
/// to every app, so the macro prepends it to each.
macro_rules! action_catalogs {
    ([$($player:expr,)*] [$($prep:expr,)*]) => {
        /// The player's vocabulary — the catalog `vidiotic`'s map editors
        /// offer. Each entry carries placeholder params a `DragValue` then
        /// edits in place.
        pub const PLAYER_CATALOG: &[Action] = &[Action::Nothing, $($player,)*];

        /// `vidiotic-prep`'s vocabulary.
        pub const PREP_CATALOG: &[Action] = &[Action::Nothing, $($prep,)*];

        /// Every action kind, for the ctl bin's editor (which edits any
        /// `.vmap`). The union of the two app catalogs, expanded from the same
        /// lists so nothing has to be kept in sync by hand.
        pub const CATALOG: &[Action] = &[Action::Nothing, $($player,)* $($prep,)*];
    };
}

action_catalogs! {
    [
    Action::TapDownbeat,
    Action::TapTempo,
    Action::SoftReset,
    Action::HardReset,
    Action::CaptureShader,
    Action::ToggleFullscreen,
    Action::SaveProject,
    Action::ToggleCommandPalette,
    Action::BpmDelta { amount: 1.0 },
    Action::NudgeBpm { ratio: 0.01 },
    Action::CycleLiveBank { delta: 1 },
    Action::SetLiveBank { index: 0 },
    Action::SetEditBank { index: 0 },
    Action::SetBpm { min: 60.0, max: 180.0 },
    Action::Quit,
    Action::BpmDigit { digit: 0 },
    Action::BpmCommit,
    Action::BpmClear,
    ]
    [
    Action::Prep(PrepVerb::TogglePlay),
    Action::Prep(PrepVerb::Pause),
    Action::Prep(PrepVerb::PlayFromIn),
    Action::Prep(PrepVerb::Shuttle { dir: 1 }),
    Action::Prep(PrepVerb::Step { frames: 1 }),
    Action::Prep(PrepVerb::SeekStart),
    Action::Prep(PrepVerb::SeekEnd),
    Action::Prep(PrepVerb::JumpToIn),
    Action::Prep(PrepVerb::JumpToOut),
    Action::Prep(PrepVerb::SetIn),
    Action::Prep(PrepVerb::SetOut),
    Action::Prep(PrepVerb::SnapOut),
    Action::Prep(PrepVerb::AddSpan),
    Action::Prep(PrepVerb::ZoomView { factor: 0.5 }),
    Action::Prep(PrepVerb::ZoomFit),
    Action::Prep(PrepVerb::ZoomToMarks),
    Action::Prep(PrepVerb::Scrub),
    ]
}

impl Action {
    /// Every bindable action across both apps: what the ctl bin's editor
    /// offers, since it edits any `.vmap`. See [`CATALOG`].
    #[must_use]
    pub fn catalog() -> &'static [Self] {
        CATALOG
    }

    /// The subset of [`Self::catalog`] that `vidiotic` understands. See
    /// [`PLAYER_CATALOG`].
    #[must_use]
    pub fn player_catalog() -> &'static [Self] {
        PLAYER_CATALOG
    }

    /// The subset of [`Self::catalog`] that `vidiotic-prep` understands. See
    /// [`PREP_CATALOG`].
    #[must_use]
    pub fn prep_catalog() -> &'static [Self] {
        PREP_CATALOG
    }

    /// Which app's vocabulary this belongs to — the action picker's first
    /// level. `None` for [`Self::Nothing`], which every app understands.
    #[must_use]
    pub fn namespace(&self) -> Option<&'static str> {
        match self {
            Self::Nothing => None,
            Self::Prep(_) => Some("prep"),
            _ => Some("player"),
        }
    }

    /// Whether two actions are the same *kind*, ignoring their params — the
    /// question a picker asks to highlight the catalog entry matching the
    /// current binding.
    ///
    /// Not `mem::discriminant`: every `Prep` verb shares `Action::Prep`'s one
    /// discriminant, so a naive comparison collapses the whole prep catalog
    /// into a single entry. Reach inside the namespace.
    #[must_use]
    pub fn same_kind(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Prep(a), Self::Prep(b)) => {
                std::mem::discriminant(a) == std::mem::discriminant(b)
            }
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }

    /// Whether this action wants the raw `0..=1` value from a continuous
    /// controller (a CC or axis) rather than being edge-triggered.
    #[must_use]
    pub fn is_continuous(&self) -> bool {
        matches!(self, Self::SetBpm { .. } | Self::Prep(PrepVerb::Scrub))
    }

    /// Human-readable name for the action picker.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Nothing => "Nothing",
            Self::TapDownbeat => "Tap Downbeat",
            Self::TapTempo => "Tap Tempo",
            Self::SoftReset => "Soft Reset",
            Self::HardReset => "Hard Reset",
            Self::CaptureShader => "Capture Shader",
            Self::ToggleFullscreen => "Toggle Fullscreen",
            Self::SaveProject => "Save Project",
            Self::ToggleCommandPalette => "Command Palette",
            Self::BpmDelta { .. } => "Bpm Delta",
            Self::NudgeBpm { .. } => "Nudge Bpm",
            Self::CycleLiveBank { .. } => "Cycle Live Bank",
            Self::SetLiveBank { .. } => "Set Live Bank",
            Self::SetEditBank { .. } => "Set Edit Bank",
            Self::SetBpm { .. } => "Set Bpm",
            Self::Quit => "Quit",
            Self::BpmDigit { .. } => "Bpm Digit",
            Self::BpmCommit => "Bpm Commit",
            Self::BpmClear => "Bpm Clear",
            Self::Prep(v) => v.label(),
        }
    }
}

#[derive(SerRon, DeRon, Clone, Debug, PartialEq)]
pub struct Binding {
    pub source: ControlSource,
    pub action: Action,
}

#[derive(SerRon, DeRon, Clone, Debug, Default, PartialEq)]
pub struct ControlMap {
    #[nserde(default)]
    pub bindings: Vec<Binding>,
}

impl ControlMap {
    /// Normalize every key binding's name through [`crate::keys::canon`], so a
    /// hand-edited `.vmap`/`.viproj` matches at runtime whichever spelling it
    /// used. Live events are canonicalized at the toolkit boundary (the editor
    /// writes `[`, not egui's `OpenBracket`), but a hand-typed file bypasses
    /// that, so loaders run its keys through the same normalization here.
    /// Idempotent, and a no-op on a map that only binds letters or named keys.
    pub fn canonicalize_keys(&mut self) {
        for binding in &mut self.bindings {
            if let ControlSource::Key { key, .. } = &mut binding.source {
                let canonical = crate::keys::canon(key);
                if canonical != *key {
                    log::debug!("canonicalized key binding {key:?} -> {canonical:?}");
                    *key = canonical;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_sources() -> Vec<ControlSource> {
        vec![
            ControlSource::MidiNote { device: "Launchkey Mini MK3".into(), channel: 1, note: 60 },
            ControlSource::MidiCc { device: "Launchkey Mini MK3".into(), channel: 1, cc: 21 },
            ControlSource::Key { key: "t".into(), ctrl: false, alt: false, shift: true, cmd: false },
            ControlSource::PadButton { device: "Xbox Controller".into(), button: "South".into() },
            ControlSource::PadAxis { device: "Xbox Controller".into(), axis: "LeftStickX".into() },
        ]
    }

    fn all_actions() -> Vec<Action> {
        Action::catalog().to_vec()
    }

    #[test]
    fn every_source_variant_round_trips() {
        for source in all_sources() {
            let binding = Binding { source: source.clone(), action: Action::TapDownbeat };
            let ron = binding.serialize_ron();
            let back = Binding::deserialize_ron(&ron).unwrap();
            assert_eq!(binding, back, "round-trip failed for {source:?}");
        }
    }

    #[test]
    fn every_action_variant_round_trips() {
        let source = ControlSource::Key {
            key: "space".into(),
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
        };
        for action in all_actions() {
            let binding = Binding { source: source.clone(), action };
            let ron = binding.serialize_ron();
            let back = Binding::deserialize_ron(&ron).unwrap();
            assert_eq!(binding, back, "round-trip failed for {action:?}");
        }
    }

    /// The shape the whole namespacing design rests on: a struct variant
    /// nested inside a tuple variant, round-tripping through nanoserde's RON.
    /// If this ever fails, the fallback is flat variants (`Action::PrepStep {
    /// frames: i32 }`) and a prefix-matching `namespace()`.
    #[test]
    fn nested_prep_verb_round_trips() {
        for action in [
            Action::Prep(PrepVerb::Step { frames: 10 }),
            Action::Prep(PrepVerb::Step { frames: -1 }),
            Action::Prep(PrepVerb::Shuttle { dir: -1 }),
            Action::Prep(PrepVerb::ZoomView { factor: 0.5 }),
            Action::Prep(PrepVerb::TogglePlay),
            Action::Prep(PrepVerb::Scrub),
        ] {
            let ron = action.serialize_ron();
            let back = Action::deserialize_ron(&ron).unwrap_or_else(|err| {
                panic!("round-trip failed for {action:?}: {err}\nserialized as: {ron}")
            });
            assert_eq!(action, back, "round-trip changed {action:?} (ron: {ron})");
        }
    }

    /// A v1 map — written before prep had any bindings — must still parse
    /// byte-for-byte. This is the whole reason the player's variants stayed
    /// unprefixed instead of moving under a symmetric `Action::Player(..)`.
    #[test]
    fn pre_namespacing_action_names_still_parse() {
        let ron = r#"(source: Key(key:"t", ctrl:false, alt:false, shift:false, cmd:false), action: TapDownbeat)"#;
        let binding = Binding::deserialize_ron(ron).expect("legacy binding must parse");
        assert_eq!(binding.action, Action::TapDownbeat);
    }

    /// `label()`'s exhaustive match doesn't force catalog membership, so a new
    /// variant can be added and silently never offered by any editor. This
    /// guard fails to compile until the new variant is classified here, and
    /// fails at runtime until it's in a catalog.
    #[test]
    fn catalog_covers_every_variant() {
        fn representative(action: &Action) -> Action {
            match action {
                // Params differ from the catalog's placeholders; compare kinds.
                Action::BpmDelta { .. } => Action::BpmDelta { amount: 1.0 },
                Action::NudgeBpm { .. } => Action::NudgeBpm { ratio: 0.01 },
                Action::CycleLiveBank { .. } => Action::CycleLiveBank { delta: 1 },
                Action::SetLiveBank { .. } => Action::SetLiveBank { index: 0 },
                Action::SetEditBank { .. } => Action::SetEditBank { index: 0 },
                Action::SetBpm { .. } => Action::SetBpm { min: 60.0, max: 180.0 },
                Action::BpmDigit { .. } => Action::BpmDigit { digit: 0 },
                Action::Prep(PrepVerb::Shuttle { .. }) => Action::Prep(PrepVerb::Shuttle { dir: 1 }),
                Action::Prep(PrepVerb::Step { .. }) => Action::Prep(PrepVerb::Step { frames: 1 }),
                Action::Prep(PrepVerb::ZoomView { .. }) => {
                    Action::Prep(PrepVerb::ZoomView { factor: 0.5 })
                }
                // Parameterless variants are their own representative. Listed
                // rather than caught by `_` so that a new variant genuinely
                // fails to compile here, as this test's doc comment promises —
                // a catch-all let `ToggleCommandPalette` slip past in d4d08f9.
                a @ (Action::Nothing
                | Action::TapDownbeat
                | Action::TapTempo
                | Action::SoftReset
                | Action::HardReset
                | Action::CaptureShader
                | Action::ToggleFullscreen
                | Action::SaveProject
                | Action::ToggleCommandPalette
                | Action::Quit
                | Action::BpmCommit
                | Action::BpmClear) => *a,
                Action::Prep(
                    v @ (PrepVerb::TogglePlay
                    | PrepVerb::Pause
                    | PrepVerb::PlayFromIn
                    | PrepVerb::SeekStart
                    | PrepVerb::SeekEnd
                    | PrepVerb::JumpToIn
                    | PrepVerb::JumpToOut
                    | PrepVerb::SetIn
                    | PrepVerb::SetOut
                    | PrepVerb::SnapOut
                    | PrepVerb::AddSpan
                    | PrepVerb::ZoomFit
                    | PrepVerb::ZoomToMarks
                    | PrepVerb::Scrub),
                ) => Action::Prep(*v),
            }
        }
        // Every variant a caller could construct, listed exhaustively.
        let every = [
            Action::Nothing,
            Action::TapDownbeat,
            Action::TapTempo,
            Action::SoftReset,
            Action::HardReset,
            Action::CaptureShader,
            Action::ToggleFullscreen,
            Action::SaveProject,
            Action::ToggleCommandPalette,
            Action::BpmDelta { amount: 99.0 },
            Action::NudgeBpm { ratio: 99.0 },
            Action::CycleLiveBank { delta: 99 },
            Action::SetLiveBank { index: 99 },
            Action::SetEditBank { index: 99 },
            Action::SetBpm { min: 1.0, max: 2.0 },
            Action::Quit,
            Action::BpmDigit { digit: 99 },
            Action::BpmCommit,
            Action::BpmClear,
            Action::Prep(PrepVerb::TogglePlay),
            Action::Prep(PrepVerb::Pause),
            Action::Prep(PrepVerb::PlayFromIn),
            Action::Prep(PrepVerb::Shuttle { dir: 99 }),
            Action::Prep(PrepVerb::Step { frames: 99 }),
            Action::Prep(PrepVerb::SeekStart),
            Action::Prep(PrepVerb::SeekEnd),
            Action::Prep(PrepVerb::JumpToIn),
            Action::Prep(PrepVerb::JumpToOut),
            Action::Prep(PrepVerb::SetIn),
            Action::Prep(PrepVerb::SetOut),
            Action::Prep(PrepVerb::SnapOut),
            Action::Prep(PrepVerb::AddSpan),
            Action::Prep(PrepVerb::ZoomView { factor: 99.0 }),
            Action::Prep(PrepVerb::ZoomFit),
            Action::Prep(PrepVerb::ZoomToMarks),
            Action::Prep(PrepVerb::Scrub),
        ];
        for action in every {
            assert!(
                CATALOG.contains(&representative(&action)),
                "{action:?} is not offered by any catalog"
            );
        }
    }

    /// The trap `same_kind` exists for: `Action::Prep` is one variant, so
    /// `mem::discriminant` cannot tell two prep verbs apart. A picker built on
    /// it would show all 17 as a single entry.
    #[test]
    fn same_kind_distinguishes_prep_verbs() {
        let play = Action::Prep(PrepVerb::TogglePlay);
        let pause = Action::Prep(PrepVerb::Pause);
        assert_eq!(
            std::mem::discriminant(&play),
            std::mem::discriminant(&pause),
            "precondition: prep verbs share one Action discriminant"
        );
        assert!(!play.same_kind(&pause));
        assert!(play.same_kind(&Action::Prep(PrepVerb::TogglePlay)));
    }

    #[test]
    fn same_kind_ignores_params_but_not_variant() {
        assert!(Action::BpmDelta { amount: 1.0 }.same_kind(&Action::BpmDelta { amount: 9.0 }));
        assert!(Action::Prep(PrepVerb::Step { frames: 1 })
            .same_kind(&Action::Prep(PrepVerb::Step { frames: -10 })));
        assert!(!Action::Prep(PrepVerb::Step { frames: 1 })
            .same_kind(&Action::Prep(PrepVerb::Shuttle { dir: 1 })));
        assert!(!Action::BpmDelta { amount: 1.0 }.same_kind(&Action::NudgeBpm { ratio: 1.0 }));
    }

    /// Each catalog entry must be uniquely identifiable, or a picker cannot
    /// map a selection back to exactly one entry.
    #[test]
    fn catalog_entries_are_distinct_kinds() {
        for (i, a) in CATALOG.iter().enumerate() {
            for (j, b) in CATALOG.iter().enumerate() {
                if i != j {
                    assert!(!a.same_kind(b), "{a:?} and {b:?} are the same kind");
                }
            }
        }
    }

    #[test]
    fn namespaces_partition_the_catalog() {
        assert_eq!(Action::Nothing.namespace(), None);
        for action in PLAYER_CATALOG.iter().filter(|a| !matches!(a, Action::Nothing)) {
            assert_eq!(action.namespace(), Some("player"), "{action:?}");
        }
        for action in PREP_CATALOG.iter().filter(|a| !matches!(a, Action::Nothing)) {
            assert_eq!(action.namespace(), Some("prep"), "{action:?}");
        }
    }

    #[test]
    fn is_continuous_only_for_set_bpm_and_scrub() {
        let continuous: Vec<_> = CATALOG.iter().filter(|a| a.is_continuous()).collect();
        assert_eq!(
            continuous,
            vec![&Action::SetBpm { min: 60.0, max: 180.0 }, &Action::Prep(PrepVerb::Scrub)]
        );
    }

    #[test]
    fn missing_bindings_field_defaults_empty() {
        let map = ControlMap::deserialize_ron("()").unwrap();
        assert!(map.bindings.is_empty());
    }

    #[test]
    fn control_map_round_trips_with_bindings() {
        let map = ControlMap {
            bindings: vec![
                Binding {
                    source: ControlSource::MidiCc {
                        device: "Launchkey".into(),
                        channel: 1,
                        cc: 21,
                    },
                    action: Action::SetBpm { min: 60.0, max: 180.0 },
                },
                Binding {
                    source: ControlSource::Key {
                        key: "t".into(),
                        ctrl: false,
                        alt: false,
                        shift: false,
                        cmd: false,
                    },
                    action: Action::TapDownbeat,
                },
            ],
        };
        let ron = map.serialize_ron();
        let back = ControlMap::deserialize_ron(&ron).unwrap();
        assert_eq!(map.bindings, back.bindings);
    }
}
