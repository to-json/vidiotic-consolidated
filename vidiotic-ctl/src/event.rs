//! Live control events — not serialized, so no nanoserde derives here (the
//! serialized vocabulary lives in [`crate::model`]).

use crate::model::ControlSource;

/// A control's value at the moment it fired, normalized so every source
/// type (MIDI note velocity, CC, gamepad button/axis, key) speaks the same
/// language downstream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventValue {
    Pressed,
    Released,
    /// Normalized `0..=1`.
    Continuous(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlEvent {
    pub source: ControlSource,
    pub value: EventValue,
}

/// Canonical string identity for a *live* source (always a concrete
/// device), e.g. `"midi:ch1:cc:21@Launchkey Mini MK3"`. Used as the
/// [`crate::mapper::Mapper`] edge-detection key and the live-monitor label.
#[must_use]
pub fn source_key(source: &ControlSource) -> String {
    match source {
        ControlSource::MidiNote { device, channel, note } => {
            format!("midi:ch{channel}:note:{note}@{device}")
        }
        ControlSource::MidiCc { device, channel, cc } => {
            format!("midi:ch{channel}:cc:{cc}@{device}")
        }
        ControlSource::Key { key, ctrl, alt, shift, cmd } => {
            let mut mods = String::new();
            if *ctrl {
                mods.push_str("ctrl+");
            }
            if *alt {
                mods.push_str("alt+");
            }
            if *shift {
                mods.push_str("shift+");
            }
            if *cmd {
                mods.push_str("cmd+");
            }
            format!("key:{mods}{key}")
        }
        ControlSource::PadButton { device, button } => format!("pad:button:{button}@{device}"),
        ControlSource::PadAxis { device, axis } => format!("pad:axis:{axis}@{device}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_key_matches_documented_format() {
        let source = ControlSource::MidiCc {
            device: "Launchkey Mini MK3".into(),
            channel: 1,
            cc: 21,
        };
        assert_eq!(source_key(&source), "midi:ch1:cc:21@Launchkey Mini MK3");
    }

    #[test]
    fn source_key_includes_modifiers_in_order() {
        let source = ControlSource::Key {
            key: "t".into(),
            ctrl: true,
            alt: false,
            shift: true,
            cmd: false,
        };
        assert_eq!(source_key(&source), "key:ctrl+shift+t");
    }
}
