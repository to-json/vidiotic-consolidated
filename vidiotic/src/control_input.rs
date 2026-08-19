//! Mapped MIDI/keyboard/gamepad input for the player, over `vidiotic-ctl`.
//!
//! `vidiotic-ctl` defines its own `Action` vocabulary rather than
//! [`Command`] (it must not depend on this crate — this crate depends on it
//! to embed a `ControlMap` in `.viproj`, so the reverse would cycle); this
//! module owns the `Action -> Command` translation instead.

use crossbeam_channel::Sender;
use vidiotic_ctl::model::Binding;
use vidiotic_ctl::{
    Action, ControlEvent, ControlMap, ControlSource, EventValue, Mapper, MidiHub, PadPoller,
};
use winit::keyboard::Key;

use crate::commands::Command;
use crate::keymap::Verb;

/// A key binding with no modifiers held.
fn key(k: &str, action: Action) -> Binding {
    let source = ControlSource::Key {
        key: vidiotic_ctl::keys::canon(k),
        ctrl: false,
        alt: false,
        shift: false,
        cmd: false,
    };
    Binding { source, action }
}

/// The same, with one modifier. `winit` reports a shifted character as the
/// character it produces (`R`, `+`), which `keys::from_character` lowercases —
/// so a shifted binding names the *lowercase* key plus `shift: true`.
fn chord(k: &str, ctrl: bool, shift: bool, cmd: bool, action: Action) -> Binding {
    let source = ControlSource::Key {
        key: vidiotic_ctl::keys::canon(k),
        ctrl,
        alt: false,
        shift,
        cmd,
    };
    Binding { source, action }
}

/// The player's built-in key bindings.
///
/// These were a hardcoded `match` on the winit key inside `App::handle_key`,
/// sitting *below* the mapper and invisible to it. That had three costs, all
/// of which this removes: `Mapper::has_binding` could not see them, so binding
/// a digit silently swallowed the first character of a typed tempo and the
/// built-in kept working in its place; none of them appeared in the mapping
/// editor; and none could be reached from MIDI or a gamepad. As ordinary
/// bindings in the fallback layer they mask, rebind, and list like everything
/// else. `vidiotic-prep` has always done it this way — this is the player
/// catching up.
///
/// Modifiers are exact here where the old match ignored them, so `Ctrl+T` no
/// longer taps the downbeat. `+` is bound both shifted and bare, since which
/// one the key produces is a layout question.
pub(crate) fn default_map() -> ControlMap {
    let mut bindings = vec![
        key("t", Action::TapDownbeat),
        key("b", Action::TapTempo),
        key("=", Action::BpmDelta { amount: 1.0 }),
        key("+", Action::BpmDelta { amount: 1.0 }),
        chord("+", false, true, false, Action::BpmDelta { amount: 1.0 }),
        key("-", Action::BpmDelta { amount: -1.0 }),
        key("[", Action::NudgeBpm { ratio: -0.001 }),
        key("]", Action::NudgeBpm { ratio: 0.001 }),
        key("r", Action::SoftReset),
        chord("r", false, true, false, Action::HardReset),
        key("c", Action::CaptureShader),
        key(",", Action::CycleLiveBank { delta: -1 }),
        key(".", Action::CycleLiveBank { delta: 1 }),
        key("f", Action::ToggleFullscreen),
        chord("q", false, false, true, Action::Quit),
        chord("s", false, false, true, Action::SaveProject),
        chord("s", true, false, false, Action::SaveProject),
        chord("p", false, false, true, Action::ToggleCommandPalette),
        chord("p", true, false, false, Action::ToggleCommandPalette),
        chord("k", false, false, true, Action::ToggleCommandPalette),
        chord("k", true, false, false, Action::ToggleCommandPalette),
        chord("p", false, true, true, Action::ToggleCommandPalette),
        chord("p", true, true, false, Action::ToggleCommandPalette),
        key("Enter", Action::BpmCommit),
        key("Escape", Action::BpmClear),
    ];
    for digit in 0..=9u8 {
        bindings.push(key(&digit.to_string(), Action::BpmDigit { digit }));
    }
    ControlMap { bindings }
}

/// Canonicalize a winit logical key into the toolkit-free string space
/// `vidiotic_ctl::keys` defines, which egui-based capturers (prep, the ctl
/// bin) reach through the same module. Each winit variant has its own entry
/// point there: a `Character` is already the canonical spelling of a
/// punctuation or digit key, whereas egui only ever reports a name. `None`
/// for dead/compose keys, which have no stable identity to bind.
#[must_use]
pub fn canon_key(key: &Key) -> Option<String> {
    match key {
        Key::Character(c) => Some(vidiotic_ctl::keys::from_character(c.as_str())),
        Key::Named(named) => Some(vidiotic_ctl::keys::from_named(&format!("{named:?}"))),
        _ => None,
    }
}

const RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct ControlInput {
    hub: MidiHub,
    pads: PadPoller,
    mapper: Mapper,
    tx: Sender<ControlEvent>,
    rx: crossbeam_channel::Receiver<ControlEvent>,
    last_rescan: std::time::Instant,
}

impl ControlInput {
    /// Three layers of precedence — project > global > built-in — in a mapper
    /// that has two.
    ///
    /// `project_map` is this session's `.viproj`-embedded layer and takes
    /// `Mapper`'s `over`, where any match at all (including a masking
    /// `Nothing`) stops the search. The other two share `base`, with the
    /// user's `global.vmap` stacked *ahead* of `default_map`: `find_in_layer`
    /// returns the first match at a device tier and every key binding shares
    /// one tier, so "ahead" is literally earlier in the vector.
    #[must_use]
    pub fn new(project_map: ControlMap) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut base = vidiotic_ctl::store::load_global();
        base.bindings.extend(default_map().bindings);
        Self {
            hub: MidiHub::new(tx.clone()),
            pads: PadPoller::new(),
            mapper: Mapper::new(base, project_map),
            tx,
            rx,
            // Elapsed already past the interval so the first pump rescans immediately.
            last_rescan: std::time::Instant::now() - RESCAN_INTERVAL,
        }
    }

    #[must_use]
    pub fn project_map(&self) -> &ControlMap {
        &self.mapper.over
    }

    /// Replace the `.viproj`-embedded mapping layer (a project was loaded
    /// mid-session). Device connections and the global base layer persist.
    pub fn set_project_map(&mut self, map: ControlMap) {
        self.mapper.over = map;
    }

    /// Poll gamepads, rescan MIDI on a timer, and drain the pending raw
    /// events. Call once per engine tick, before draining `cmd_rx`, and feed
    /// each event back through [`Self::resolve`] — the caller gets to
    /// intercept events in between (the grammar claims token presses).
    #[must_use]
    pub fn collect(&mut self) -> Vec<ControlEvent> {
        self.pads.poll(&self.tx);
        if self.last_rescan.elapsed() >= RESCAN_INTERVAL {
            self.hub.rescan();
            self.last_rescan = std::time::Instant::now();
        }
        self.rx.try_iter().collect()
    }

    /// Resolve one collected event through the mapping layer onto `cmd_tx`.
    pub fn resolve(&mut self, ev: &ControlEvent, cmd_tx: &Sender<Command>) {
        if let Some((action, value)) = self.mapper.resolve(ev) {
            if let Some(cmd) = to_command(&action, value) {
                let _ = cmd_tx.send(cmd);
            }
        }
    }

    /// Offer a key event to the mapping layer. Returns `true` if this exact
    /// key+modifiers combination has *any* binding — including a masking
    /// `Action::Nothing`. With the built-ins now in the fallback layer there is
    /// no longer a hardcoded default below this to suppress, so the return
    /// value only tells the caller the key was spoken for.
    ///
    /// `repeat` gates re-firing while a key is held: a trigger fires once, but
    /// the commands that opt in via [`Command::repeats_on_hold`] keep going, so
    /// holding `[` still drifts the tempo the way it did when these keys were
    /// a hardcoded match with no repeat guard.
    #[allow(clippy::too_many_arguments)]
    pub fn offer_key(
        &mut self,
        key: &str,
        ctrl: bool,
        alt: bool,
        shift: bool,
        cmd: bool,
        repeat: bool,
        cmd_tx: &Sender<Command>,
    ) -> bool {
        let source = ControlSource::Key {
            key: key.to_string(),
            ctrl,
            alt,
            shift,
            cmd,
        };
        if !self.mapper.has_binding(&source) {
            return false;
        }
        let ev = ControlEvent {
            source,
            value: EventValue::Pressed,
        };
        if let Some((action, value)) = self.mapper.resolve(&ev) {
            if let Some(c) = to_command(&action, value) {
                if !repeat || c.repeats_on_hold() {
                    let _ = cmd_tx.send(c);
                }
            }
        }
        true
    }
}

/// `Action -> Command`. `value` (normalized `0..=1`) only matters for
/// `SetBpm`; every other variant carries its own params.
///
/// Returns `None` for `Nothing` and for every `Prep` verb: the player and
/// `vidiotic-prep` share one `Action` enum (they share the `.vmap` format and
/// its editor) and each rejects the other's half. A prep verb reaching here
/// means a map was hand-edited or authored in the ctl bin against the wrong
/// app — the binding simply doesn't fire.
fn to_command(action: &Action, value: f32) -> Option<Command> {
    match action {
        Action::Nothing | Action::Prep(_) => None,
        Action::TapDownbeat => Some(Command::TapDownbeat),
        Action::TapTempo => Some(Command::TapTempo),
        Action::SoftReset => Some(Command::SoftReset),
        Action::HardReset => Some(Command::HardReset),
        Action::CaptureShader => Some(Command::CaptureShader),
        Action::ToggleFullscreen => Some(Command::ToggleFullscreen),
        Action::SaveProject => Some(Command::SaveProject),
        Action::ToggleCommandPalette => Some(Command::ToggleCommandPalette),
        // Selection-relative: no params to carry, and the target only exists
        // at press time. The player resolves them exactly as the grammar's
        // own `d d` / `m g` do — one resolution, two front doors.
        Action::RemoveSelectedCue => Some(Command::Verb(Verb::RemoveSelectedCue)),
        Action::AddCueAtSelectedClip => Some(Command::Verb(Verb::AddCueAtClip)),
        Action::MarkInToPlayhead => Some(Command::Verb(Verb::MarkInToPlayhead)),
        Action::MarkOutToPlayhead => Some(Command::Verb(Verb::MarkOutToPlayhead)),
        Action::CyclePreserve => Some(Command::Verb(Verb::CyclePreserve)),
        Action::BpmDelta { amount } => Some(Command::BpmDelta(*amount)),
        Action::NudgeBpm { ratio } => Some(Command::NudgeBpm(*ratio)),
        Action::CycleLiveBank { delta } => Some(Command::CycleLiveBank(*delta)),
        Action::SetLiveBank { index } => Some(Command::SetLiveBank(*index as usize)),
        Action::SetEditBank { index } => Some(Command::SetEditBank(*index as usize)),
        Action::SetBpm { min, max } => {
            let bpm = min + (max - min) * f64::from(value);
            Some(Command::SetBpm(bpm.clamp(20.0, 1000.0)))
        }
        Action::Quit => Some(Command::Quit),
        Action::BpmDigit { digit } => Some(Command::BpmDigit(*digit)),
        Action::BpmCommit => Some(Command::BpmCommit),
        Action::BpmClear => Some(Command::BpmClear),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action in the player's catalog must translate, or a binding the
    /// editor happily offers silently does nothing. `Nothing` is the mask and
    /// the prep half belongs to another app; everything else is this crate's
    /// job.
    #[test]
    fn every_player_action_translates() {
        for action in vidiotic_ctl::model::PLAYER_CATALOG {
            if matches!(action, Action::Nothing) {
                continue;
            }
            assert!(
                to_command(action, 0.0).is_some(),
                "{action:?} is offered by the editor but translates to nothing"
            );
        }
    }

    /// The selection-relative half of the catalog: no params to carry, and a
    /// target that only exists at press time. These reach the engine as verbs
    /// so the resolution lives in one place rather than two.
    #[test]
    fn selection_relative_actions_resolve_to_verbs() {
        let pairs = [
            (Action::RemoveSelectedCue, Verb::RemoveSelectedCue),
            (Action::AddCueAtSelectedClip, Verb::AddCueAtClip),
            (Action::MarkInToPlayhead, Verb::MarkInToPlayhead),
            (Action::MarkOutToPlayhead, Verb::MarkOutToPlayhead),
            (Action::CyclePreserve, Verb::CyclePreserve),
        ];
        for (action, verb) in pairs {
            match to_command(&action, 0.0) {
                Some(Command::Verb(v)) => assert_eq!(v, verb, "{action:?}"),
                other => panic!("{action:?} resolved to {other:?}"),
            }
        }
    }

    /// The key-name contract, tested against both real key enums — this crate
    /// is the only one that depends on winit *and* egui, so it is the only
    /// place the two spellings of a physical key can actually be compared.
    /// The ctl editor is egui and the player is winit: a key the editor binds
    /// must be the key the player looks up, or the binding silently never
    /// fires and the hardcoded default keeps working in its place.
    ///
    /// Pairs are physical keys: the `egui::Key` the editor reports, and the
    /// character winit's `Key::Character` carries for the same keypress.
    #[test]
    fn egui_and_winit_spellings_of_a_key_canonicalize_alike() {
        let pairs = [
            (egui::Key::OpenBracket, "["),
            (egui::Key::CloseBracket, "]"),
            (egui::Key::Comma, ","),
            (egui::Key::Period, "."),
            (egui::Key::Minus, "-"),
            (egui::Key::Plus, "+"),
            (egui::Key::Equals, "="),
            (egui::Key::Num0, "0"),
            (egui::Key::Num1, "1"),
            (egui::Key::Num9, "9"),
            (egui::Key::Backtick, "`"),
            (egui::Key::Semicolon, ";"),
            (egui::Key::Slash, "/"),
            (egui::Key::Backslash, "\\"),
            (egui::Key::Quote, "'"),
        ];
        for (egui_key, character) in pairs {
            // What the egui-side editor writes into the map.
            let bound = vidiotic_ctl::keys::from_named(&format!("{egui_key:?}"));
            // What the winit-side player looks up when that key is pressed.
            let pressed = canon_key(&Key::Character(character.into()))
                .expect("a character key must canonicalize");
            assert_eq!(
                bound, pressed,
                "{egui_key:?} bound in the editor must match {character:?} pressed in the player"
            );
            assert_eq!(
                bound, character,
                "the canonical form is the literal character"
            );
        }
    }

    /// Letters and named keys were never broken; the name table must not have
    /// regressed them.
    #[test]
    fn letters_and_named_keys_still_agree_across_toolkits() {
        assert_eq!(
            vidiotic_ctl::keys::from_named(&format!("{:?}", egui::Key::T)),
            canon_key(&Key::Character("t".into())).expect("letter"),
        );
        for (egui_key, named) in [
            (egui::Key::Space, winit::keyboard::NamedKey::Space),
            (egui::Key::ArrowLeft, winit::keyboard::NamedKey::ArrowLeft),
            (egui::Key::F1, winit::keyboard::NamedKey::F1),
            (egui::Key::Escape, winit::keyboard::NamedKey::Escape),
        ] {
            assert_eq!(
                vidiotic_ctl::keys::from_named(&format!("{egui_key:?}")),
                canon_key(&Key::Named(named)).expect("named key"),
            );
        }
    }

    /// The punctuation and digit keys the built-ins bind must survive the
    /// winit boundary unchanged: `canon_key` is what a live press goes through
    /// before lookup, and canonicalizing it twice must not move it. If it did,
    /// a stored binding and a pressed key would spell the same physical key
    /// two ways and never meet.
    #[test]
    fn character_keys_canonicalize_idempotently() {
        for character in ["+", "=", "-", "[", "]", ",", ".", "0", "1", "9"] {
            let pressed = canon_key(&Key::Character(character.into())).expect("character key");
            assert_eq!(
                vidiotic_ctl::keys::canon(&pressed),
                pressed,
                "{character:?} must be storable in a .vmap as-is"
            );
        }
    }

    #[test]
    fn nothing_yields_no_command() {
        assert!(to_command(&Action::Nothing, 0.0).is_none());
    }

    /// Resolve `source` against a mapper layered the way `ControlInput::new`
    /// layers one, with `over` standing in for a project/global map.
    fn press(over: ControlMap, source: ControlSource) -> Option<Command> {
        let mut mapper = Mapper::new(default_map(), over);
        let ev = ControlEvent {
            source,
            value: EventValue::Pressed,
        };
        mapper.resolve(&ev).and_then(|(a, v)| to_command(&a, v))
    }

    fn plain(k: &str) -> ControlSource {
        ControlSource::Key {
            key: k.into(),
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
        }
    }

    /// A default whose key name isn't canonical can never match a live event,
    /// because the player canonicalizes at the winit boundary before looking
    /// up. Silent when it regresses — hence the guard.
    #[test]
    fn every_default_binds_an_already_canonical_key_name() {
        for binding in &default_map().bindings {
            let ControlSource::Key { key, .. } = &binding.source else {
                panic!("the built-ins are keys only, got {:?}", binding.source);
            };
            assert_eq!(
                &vidiotic_ctl::keys::canon(key),
                key,
                "{key:?} must be storable in a .vmap as-is"
            );
        }
    }

    /// The built-ins reproduce what the hardcoded match in `handle_key` did.
    /// Shifted characters are the interesting ones: winit reports `R` and `+`,
    /// which canonicalize to lowercase plus `shift: true`.
    #[test]
    fn defaults_still_fire_what_the_hardcoded_match_fired() {
        let none = ControlMap::default;
        assert!(matches!(
            press(none(), plain("t")),
            Some(Command::TapDownbeat)
        ));
        assert!(matches!(press(none(), plain("b")), Some(Command::TapTempo)));
        assert!(matches!(
            press(none(), plain("f")),
            Some(Command::ToggleFullscreen)
        ));
        assert!(matches!(
            press(none(), plain("c")),
            Some(Command::CaptureShader)
        ));
        assert!(matches!(
            press(none(), plain("r")),
            Some(Command::SoftReset)
        ));
        assert!(matches!(press(none(), plain("=")), Some(Command::BpmDelta(a)) if a == 1.0));
        assert!(matches!(press(none(), plain("-")), Some(Command::BpmDelta(a)) if a == -1.0));
        assert!(matches!(press(none(), plain("[")), Some(Command::NudgeBpm(r)) if r < 0.0));
        assert!(matches!(
            press(none(), plain(",")),
            Some(Command::CycleLiveBank(-1))
        ));
        assert!(matches!(
            press(none(), plain(".")),
            Some(Command::CycleLiveBank(1))
        ));

        let shift_r = ControlSource::Key {
            key: "r".into(),
            ctrl: false,
            alt: false,
            shift: true,
            cmd: false,
        };
        assert!(
            matches!(press(none(), shift_r), Some(Command::HardReset)),
            "shift+R is the hard reset, and stays distinct from a bare r"
        );
        let cmd_q = ControlSource::Key {
            key: "q".into(),
            ctrl: false,
            alt: false,
            shift: false,
            cmd: true,
        };
        assert!(matches!(press(none(), cmd_q), Some(Command::Quit)));
        let ctrl_s = ControlSource::Key {
            key: "s".into(),
            ctrl: true,
            alt: false,
            shift: false,
            cmd: false,
        };
        assert!(matches!(press(none(), ctrl_s), Some(Command::SaveProject)));
    }

    /// Every digit types, Enter commits, Escape abandons — the accumulator's
    /// whole vocabulary, now reachable from anything bindable rather than only
    /// the number row.
    #[test]
    fn every_digit_and_both_terminators_are_bound() {
        for d in 0..=9u8 {
            let cmd = press(ControlMap::default(), plain(&d.to_string()));
            assert!(
                matches!(cmd, Some(Command::BpmDigit(got)) if got == d),
                "digit {d} must reach the entry, got {cmd:?}"
            );
        }
        assert!(matches!(
            press(ControlMap::default(), plain("Enter")),
            Some(Command::BpmCommit)
        ));
        assert!(matches!(
            press(ControlMap::default(), plain("Escape")),
            Some(Command::BpmClear)
        ));
    }

    /// The bug this whole arrangement exists to kill: binding a digit used to
    /// leave the accumulator's claim on it buried below the mapper, so the
    /// keystroke was eaten and the first character of a typed tempo vanished.
    /// Now it is an ordinary override, and a `Nothing` masks it outright.
    #[test]
    fn a_user_binding_on_a_digit_overrides_rather_than_swallows() {
        let over = ControlMap {
            bindings: vec![Binding {
                source: plain("1"),
                action: Action::TapTempo,
            }],
        };
        assert!(
            matches!(press(over, plain("1")), Some(Command::TapTempo)),
            "the user's binding wins outright"
        );
        let masked = ControlMap {
            bindings: vec![Binding {
                source: plain("1"),
                action: Action::Nothing,
            }],
        };
        assert!(
            press(masked, plain("1")).is_none(),
            "a Nothing binding masks the default"
        );
        assert!(
            matches!(
                press(ControlMap::default(), plain("1")),
                Some(Command::BpmDigit(1))
            ),
            "and with nothing bound the built-in still fires"
        );
    }

    /// Holding a key must keep drifting the tempo but must not re-trigger
    /// anything else — and must not push a digit ten times a second.
    #[test]
    fn only_the_tempo_nudges_refire_on_key_repeat() {
        assert!(Command::NudgeBpm(0.001).repeats_on_hold());
        assert!(Command::BpmDelta(1.0).repeats_on_hold());
        assert!(!Command::BpmDigit(1).repeats_on_hold());
        assert!(!Command::ToggleFullscreen.repeats_on_hold());
        assert!(!Command::HardReset.repeats_on_hold());
    }

    /// The other half of the shared `Action` enum is `vidiotic-prep`'s, and
    /// none of it means anything to the player.
    #[test]
    fn prep_verbs_yield_no_player_command() {
        for action in vidiotic_ctl::Action::prep_catalog() {
            assert!(
                to_command(action, 1.0).is_none(),
                "{action:?} must not resolve to a player command"
            );
        }
    }

    #[test]
    fn trigger_actions_map_to_same_name_commands() {
        assert!(matches!(
            to_command(&Action::TapDownbeat, 1.0),
            Some(Command::TapDownbeat)
        ));
        assert!(matches!(
            to_command(&Action::TapTempo, 1.0),
            Some(Command::TapTempo)
        ));
        assert!(matches!(
            to_command(&Action::SoftReset, 1.0),
            Some(Command::SoftReset)
        ));
        assert!(matches!(
            to_command(&Action::HardReset, 1.0),
            Some(Command::HardReset)
        ));
        assert!(matches!(
            to_command(&Action::CaptureShader, 1.0),
            Some(Command::CaptureShader)
        ));
        assert!(matches!(
            to_command(&Action::ToggleFullscreen, 1.0),
            Some(Command::ToggleFullscreen)
        ));
        assert!(matches!(
            to_command(&Action::SaveProject, 1.0),
            Some(Command::SaveProject)
        ));
    }

    #[test]
    fn parameterized_triggers_carry_their_params() {
        assert!(matches!(
            to_command(&Action::BpmDelta { amount: 2.0 }, 1.0),
            Some(Command::BpmDelta(a)) if a == 2.0
        ));
        assert!(matches!(
            to_command(&Action::NudgeBpm { ratio: 0.01 }, 1.0),
            Some(Command::NudgeBpm(r)) if r == 0.01
        ));
        assert!(matches!(
            to_command(&Action::CycleLiveBank { delta: -1 }, 1.0),
            Some(Command::CycleLiveBank(d)) if d == -1
        ));
        assert!(matches!(
            to_command(&Action::SetLiveBank { index: 3 }, 1.0),
            Some(Command::SetLiveBank(i)) if i == 3
        ));
        assert!(matches!(
            to_command(&Action::SetEditBank { index: 2 }, 1.0),
            Some(Command::SetEditBank(i)) if i == 2
        ));
    }

    #[test]
    fn set_bpm_lerps_value_between_min_and_max() {
        let cmd = to_command(
            &Action::SetBpm {
                min: 60.0,
                max: 180.0,
            },
            0.5,
        );
        assert!(matches!(cmd, Some(Command::SetBpm(b)) if (b - 120.0).abs() < 1e-9));
    }

    #[test]
    fn set_bpm_clamps_out_of_range_lerp() {
        let cmd = to_command(
            &Action::SetBpm {
                min: 60.0,
                max: 180.0,
            },
            -10.0,
        );
        assert!(matches!(cmd, Some(Command::SetBpm(b)) if (b - 20.0).abs() < 1e-9));
    }
}
