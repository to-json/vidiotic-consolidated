//! Which key does what: prep's built-in bindings, and `Action -> Command`.
//!
//! `vidiotic-ctl` owns the serialized `Action` vocabulary; this module owns the
//! translation into [`Command`], because the ctl crate must not depend on
//! either app. It is the twin of `vidiotic::control_input`.
//!
//! # Why this half is here and the rest is not
//!
//! Resolving a key is arithmetic over a `ControlMap`. *Getting* a key is a
//! machine: `vidiotic-prep` reads egui events, polls gamepads and opens `CoreMIDI`
//! ports, and layers the user's `prep.vmap` off disk over these defaults —
//! none of which a browser has (web-port.md §2 defers all of it behind a `WebMIDI`
//! shim). So the shells share the table and differ in what feeds it.
//!
//! That split is what gives the browser editor a keyboard at all. Without it,
//! `i`/`o`/Enter/space are native-only and the panels are a video editor you
//! can only use with a mouse.
//!
//! Prep's hardcoded defaults are the mapper's *base layer* rather than a
//! fallback arbitrated by `Mapper::has_binding`: the player needs that dance
//! because its `handle_key` has non-bindable behaviour tangled in (the BPM digit
//! accumulator), whereas every one of prep's keys is a plain source→verb pair.
//! Expressed as a `ControlMap` under the user's `prep.vmap`, `Mapper::resolve`
//! gets the layering right on its own — a rebound key wins, an `Action::Nothing`
//! masks a default outright, and anything untouched falls through.

use vidiotic_ctl::{Action, Binding, ControlEvent, ControlMap, ControlSource, EventValue, Mapper, PrepVerb};

use crate::commands::Command;

fn key(k: &str) -> ControlSource {
    ControlSource::Key { key: k.into(), ctrl: false, alt: false, shift: false, cmd: false }
}

fn shift_key(k: &str) -> ControlSource {
    ControlSource::Key { key: k.into(), ctrl: false, alt: false, shift: true, cmd: false }
}

fn bind(source: ControlSource, verb: PrepVerb) -> Binding {
    Binding { source, action: Action::Prep(verb) }
}

/// Prep's built-in key bindings — the mapper's base layer, which `prep.vmap`
/// overrides per-source.
///
/// Key names are `vidiotic_ctl::keys::from_named` of `egui::Key`'s `Debug`
/// name, the same normalization `PrepApp::key_events` applies to live events:
/// letters lowercase, named keys keep their W3C-style name, and punctuation or
/// digits become the literal character (`"["`, not egui's `OpenBracket`) — a
/// default spelled egui's way would never match a live event.
#[must_use]
pub fn default_map() -> ControlMap {
    ControlMap {
        bindings: vec![
            bind(key("Space"), PrepVerb::TogglePlay),
            bind(shift_key("Space"), PrepVerb::PlayFromIn),
            bind(key("j"), PrepVerb::Shuttle { dir: -1 }),
            bind(key("k"), PrepVerb::Pause),
            bind(key("l"), PrepVerb::Shuttle { dir: 1 }),
            bind(key("i"), PrepVerb::SetIn),
            bind(key("o"), PrepVerb::SetOut),
            bind(key("Enter"), PrepVerb::AddSpan),
            bind(key("a"), PrepVerb::AddSpan),
            bind(key("ArrowRight"), PrepVerb::Step { frames: 1 }),
            bind(key("ArrowLeft"), PrepVerb::Step { frames: -1 }),
            bind(shift_key("ArrowRight"), PrepVerb::Step { frames: 10 }),
            bind(shift_key("ArrowLeft"), PrepVerb::Step { frames: -10 }),
            bind(key("Home"), PrepVerb::SeekStart),
            bind(key("End"), PrepVerb::SeekEnd),
        ],
    }
}

/// Resolve one control event into a command.
///
/// `repeat` gates the OS's key-repeat events: only commands that opt in
/// ([`Command::repeats_on_hold`]) re-fire while a key is held.
pub fn resolve(
    mapper: &mut Mapper,
    source: ControlSource,
    value: EventValue,
    repeat: bool,
) -> Option<Command> {
    let (action, v) = mapper.resolve(&ControlEvent { source, value })?;
    let cmd = to_command(&action, v)?;
    (!repeat || cmd.repeats_on_hold()).then_some(cmd)
}

/// `Action -> Command`. `value` (normalized `0..=1`) only matters for `Scrub`;
/// every other verb carries its own params.
///
/// Returns `None` for `Nothing` and for every player verb — prep and the
/// player share one `Action` enum (they share the `.vmap` format and its
/// editor) and each rejects the other's half.
fn to_command(action: &Action, value: f32) -> Option<Command> {
    let Action::Prep(verb) = action else { return None };
    Some(match verb {
        PrepVerb::TogglePlay => Command::TogglePlay,
        PrepVerb::Pause => Command::Pause,
        PrepVerb::PlayFromIn => Command::PlayFromIn,
        PrepVerb::Shuttle { dir } => Command::Shuttle(f64::from(*dir)),
        PrepVerb::Step { frames } => Command::Step(i64::from(*frames)),
        PrepVerb::SeekStart => Command::SeekStart,
        PrepVerb::SeekEnd => Command::SeekEnd,
        PrepVerb::JumpToIn => Command::JumpToIn,
        PrepVerb::JumpToOut => Command::JumpToOut,
        PrepVerb::SetIn => Command::SetIn,
        PrepVerb::SetOut => Command::SetOut,
        PrepVerb::SnapOut => Command::SnapOut,
        PrepVerb::AddSpan => Command::AddSpan,
        PrepVerb::ZoomView { factor } => Command::ZoomView(*factor),
        PrepVerb::ZoomFit => Command::ZoomFit,
        PrepVerb::ZoomToMarks => Command::ZoomToMarks,
        PrepVerb::Scrub => Command::SeekFrac(f64::from(value)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn press(mapper: &mut Mapper, source: ControlSource) -> Option<Command> {
        resolve(mapper, source, EventValue::Pressed, false)
    }

    fn defaults() -> Mapper {
        Mapper::new(default_map(), ControlMap::default())
    }

    /// A default spelled outside the canonical key-name space never matches a
    /// live event, and fails silently — the hardcoded behaviour just keeps
    /// working, so it reads as "the binding didn't take". Punctuation and
    /// digits are the trap: they must be spelled `"["`, not `"OpenBracket"`.
    #[test]
    fn every_default_binds_an_already_canonical_key_name() {
        for binding in default_map().bindings {
            let ControlSource::Key { key, .. } = &binding.source else {
                continue;
            };
            assert_eq!(
                &vidiotic_ctl::keys::canon(key),
                key,
                "default binding for {:?} is not a canonical key name",
                binding.action
            );
        }
    }

    /// Adding a verb without wiring it here would leave it selectable in the
    /// editor and silently dead. Every verb must translate.
    #[test]
    fn every_prep_verb_maps_to_a_command() {
        for action in Action::prep_catalog() {
            if matches!(action, Action::Nothing) {
                continue;
            }
            assert!(
                to_command(action, 1.0).is_some(),
                "{action:?} is offered by the editor but maps to no command"
            );
        }
    }

    #[test]
    fn player_actions_yield_no_prep_command() {
        for action in Action::player_catalog() {
            assert!(
                to_command(action, 1.0).is_none(),
                "{action:?} is the player's and must not resolve here"
            );
        }
    }

    /// The regression net for the port: every key that was hardcoded in
    /// `ui::transport_controls` must still resolve to the same command.
    /// Transcribed from that block before it was deleted.
    #[test]
    fn default_map_covers_every_previously_hardcoded_key() {
        let mut m = defaults();
        assert!(matches!(press(&mut m, key("Space")), Some(Command::TogglePlay)));
        assert!(matches!(press(&mut m, shift_key("Space")), Some(Command::PlayFromIn)));
        assert!(matches!(press(&mut m, key("j")), Some(Command::Shuttle(d)) if d < 0.0));
        assert!(matches!(press(&mut m, key("k")), Some(Command::Pause)));
        assert!(matches!(press(&mut m, key("l")), Some(Command::Shuttle(d)) if d > 0.0));
        assert!(matches!(press(&mut m, key("i")), Some(Command::SetIn)));
        assert!(matches!(press(&mut m, key("o")), Some(Command::SetOut)));
        assert!(matches!(press(&mut m, key("Enter")), Some(Command::AddSpan)));
        assert!(matches!(press(&mut m, key("a")), Some(Command::AddSpan)));
        assert!(matches!(press(&mut m, key("ArrowRight")), Some(Command::Step(1))));
        assert!(matches!(press(&mut m, key("ArrowLeft")), Some(Command::Step(-1))));
        assert!(matches!(press(&mut m, shift_key("ArrowRight")), Some(Command::Step(10))));
        assert!(matches!(press(&mut m, shift_key("ArrowLeft")), Some(Command::Step(-10))));
        assert!(matches!(press(&mut m, key("Home")), Some(Command::SeekStart)));
        assert!(matches!(press(&mut m, key("End")), Some(Command::SeekEnd)));
    }

    /// Holding an arrow scrubbed before the port and must still: egui's
    /// `key_pressed` counts key-repeat events. Holding Space must not flutter.
    #[test]
    fn only_steppers_refire_on_key_repeat() {
        let mut m = defaults();
        let repeat = |m: &mut Mapper, s: ControlSource| resolve(m, s, EventValue::Pressed, true);
        assert!(
            matches!(repeat(&mut m, key("ArrowRight")), Some(Command::Step(1))),
            "a held arrow must keep stepping"
        );
        assert!(
            repeat(&mut m, key("Space")).is_none(),
            "a held Space must not re-fire play/pause"
        );
        assert!(repeat(&mut m, key("j")).is_none(), "a held J must not keep doubling speed");
    }

    /// The layering prep relies on: `prep.vmap` overrides a default, and an
    /// `Action::Nothing` there masks one outright.
    #[test]
    fn prep_map_overrides_and_masks_defaults() {
        let over = ControlMap {
            bindings: vec![
                bind(key("Space"), PrepVerb::AddSpan),
                Binding { source: key("j"), action: Action::Nothing },
            ],
        };
        let mut m = Mapper::new(default_map(), over);
        assert!(matches!(press(&mut m, key("Space")), Some(Command::AddSpan)), "rebound");
        assert!(press(&mut m, key("j")).is_none(), "masked");
        assert!(matches!(press(&mut m, key("k")), Some(Command::Pause)), "untouched default");
    }

    #[test]
    fn scrub_lerps_across_the_source_and_clamps() {
        assert!(matches!(to_command(&Action::Prep(PrepVerb::Scrub), 0.0), Some(Command::SeekFrac(t)) if t == 0.0));
        assert!(matches!(to_command(&Action::Prep(PrepVerb::Scrub), 1.0), Some(Command::SeekFrac(t)) if t == 1.0));
    }

    #[test]
    fn step_carries_its_sign() {
        assert!(matches!(
            to_command(&Action::Prep(PrepVerb::Step { frames: -10 }), 1.0),
            Some(Command::Step(-10))
        ));
    }
}
