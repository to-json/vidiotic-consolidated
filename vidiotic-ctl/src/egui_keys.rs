//! The egui side of keyboard input: `egui::Event::Key` → [`ControlEvent`], and
//! the reserved undo/redo chord.
//!
//! Three shells were carrying near-identical copies of both — `vidiotic-chop`'s
//! browser shell, `vidiotic-prep`, and `vidiotic-ctl`'s own window — down to the
//! same explanatory comment about `egui::Key`'s Debug name. The key *name* table
//! ([`crate::keys`]) was already shared; the adapter around it was not, so a
//! change to how a modifier or a chord is read had to be made in three places
//! and stayed correct in however many of them somebody remembered.
//!
//! This lives in `vidiotic-ctl` because [`ControlEvent`] does, and behind
//! `egui-ui` so the lib stays usable headless.

use crate::event::{ControlEvent, EventValue};
use crate::model::ControlSource;

/// This frame's key events as `(event, repeat)` pairs, newest last.
///
/// `repeat` is the OS's key-repeat flag, carried alongside rather than inside
/// [`ControlEvent`]: the device channel cannot carry it, so keys are handled
/// inline while MIDI and gamepad input round-trips. Callers that have no use for
/// held keys filter on it; [`crate::mapper`] uses it to keep a repeat from being
/// captured as a binding.
///
/// This does **not** check [`egui::Context::egui_wants_keyboard_input`]. Whether
/// a focused text field should eat keys is the shell's call — it knows whether
/// the field in question is a span name or a rebinding capture — and every
/// current caller gates on it before calling.
#[must_use]
pub fn key_events(ctx: &egui::Context) -> Vec<(ControlEvent, bool)> {
    ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| {
                let egui::Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } = event
                else {
                    return None;
                };
                let source = ControlSource::Key {
                    // `egui::Key`'s Debug name, canonicalized into the
                    // toolkit-free space `vidiotic-ctl` binds against. egui
                    // names every key, punctuation and digits included
                    // (`OpenBracket`, `Num1`); `from_named` folds those onto
                    // winit's spelling of the same physical key.
                    key: crate::keys::from_named(&format!("{key:?}")),
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                    cmd: modifiers.mac_cmd,
                };
                let value = if *pressed {
                    EventValue::Pressed
                } else {
                    EventValue::Released
                };
                Some((ControlEvent { source, value }, *repeat))
            })
            .collect()
    })
}

/// Which half of the history the reserved chord asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum History {
    Undo,
    Redo,
}

/// The reserved undo/redo accelerator, if that is what `ev` is.
///
/// Cmd+Z on mac, Ctrl+Z elsewhere; Shift or `y` for redo. Resolved ahead of the
/// mapper — and ahead of learn, so the chord cannot be captured as a binding —
/// which is why it is a separate question from [`key_events`] rather than a
/// value the mapper could return.
///
/// Press edge only, and never on key-repeat: holding Cmd+Z should not walk the
/// whole history at the OS's repeat rate. Alt disqualifies the chord, so
/// Cmd+Alt+Z stays available as an ordinary binding.
#[must_use]
pub fn history_chord(ev: &ControlEvent, repeat: bool) -> Option<History> {
    if repeat || !matches!(ev.value, EventValue::Pressed) {
        return None;
    }
    let ControlSource::Key {
        key,
        ctrl,
        alt,
        shift,
        cmd,
    } = &ev.source
    else {
        return None;
    };
    if !((*ctrl || *cmd) && !*alt) {
        return None;
    }
    match key.as_str() {
        "z" if *shift => Some(History::Redo),
        "z" => Some(History::Undo),
        "y" => Some(History::Redo),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, ctrl: bool, shift: bool, alt: bool) -> ControlEvent {
        ControlEvent {
            source: ControlSource::Key {
                key: name.to_string(),
                ctrl,
                alt,
                shift,
                cmd: false,
            },
            value: EventValue::Pressed,
        }
    }

    #[test]
    fn the_chord_is_accel_z_and_accel_y() {
        assert_eq!(
            history_chord(&key("z", true, false, false), false),
            Some(History::Undo)
        );
        assert_eq!(
            history_chord(&key("z", true, true, false), false),
            Some(History::Redo)
        );
        assert_eq!(
            history_chord(&key("y", true, false, false), false),
            Some(History::Redo)
        );
        // Shift+Y is redo too: the shift is redundant, not disqualifying.
        assert_eq!(
            history_chord(&key("y", true, true, false), false),
            Some(History::Redo)
        );
    }

    #[test]
    fn a_bare_or_alt_z_is_not_the_chord() {
        assert_eq!(history_chord(&key("z", false, false, false), false), None);
        // Alt disqualifies it, leaving Cmd+Alt+Z bindable.
        assert_eq!(history_chord(&key("z", true, false, true), false), None);
        assert_eq!(history_chord(&key("a", true, false, false), false), None);
    }

    #[test]
    fn repeat_and_release_are_not_the_chord() {
        // Holding it must not walk the history at the OS repeat rate.
        assert_eq!(history_chord(&key("z", true, false, false), true), None);
        let mut released = key("z", true, false, false);
        released.value = EventValue::Released;
        assert_eq!(history_chord(&released, false), None);
    }

    #[test]
    fn a_non_key_source_is_never_the_chord() {
        let ev = ControlEvent {
            source: ControlSource::MidiNote {
                device: "pad".into(),
                channel: 1,
                note: 36,
            },
            value: EventValue::Pressed,
        };
        assert_eq!(history_chord(&ev, false), None);
    }
}
