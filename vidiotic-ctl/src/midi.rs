//! USB MIDI device IO (`CoreMIDI` via `midir`).
//!
//! Threading: `midir` callbacks run on a `CoreMIDI` OS thread. [`parse`] runs
//! there too — it's pure and allocation-light — and only the resulting
//! [`ControlEvent`] crosses the boundary, over a crossbeam
//! [`crossbeam_channel::Sender`]. Nothing else (no `Gilrs`, no UI state)
//! should ever touch that thread.
//!
//! Hotplug: `CoreMIDI` exposes no hotplug callback through `midir`, so
//! [`MidiHub::rescan`] is a polled enumerate-and-diff; callers run it on a
//! timer (the ctl bin and vidiotic's runtime both use ~2s).

use std::collections::{HashMap, HashSet};

use crossbeam_channel::Sender;
use midir::{Ignore, MidiInput, MidiInputConnection};

use crate::event::{ControlEvent, EventValue};
use crate::model::ControlSource;

const CLIENT_NAME: &str = "vidiotic-ctl";

pub struct MidiHub {
    conns: HashMap<String, MidiInputConnection<()>>,
    tx: Sender<ControlEvent>,
}

impl MidiHub {
    #[must_use]
    pub fn new(tx: Sender<ControlEvent>) -> Self {
        Self { conns: HashMap::new(), tx }
    }

    /// Currently connected device names.
    pub fn port_names(&self) -> Vec<String> {
        self.conns.keys().cloned().collect()
    }

    /// Enumerate ports, connect any new one, and drop connections whose
    /// port vanished (device unplugged).
    pub fn rescan(&mut self) {
        let probe = match MidiInput::new(CLIENT_NAME) {
            Ok(p) => p,
            Err(err) => {
                log::warn!("midi: failed to open input for enumeration: {err}");
                return;
            }
        };

        let mut seen = HashSet::new();
        for port in probe.ports() {
            let Ok(name) = probe.port_name(&port) else { continue };
            seen.insert(name.clone());
            if self.conns.contains_key(&name) {
                continue;
            }
            self.connect(&port, &name);
        }
        self.conns.retain(|name, _| seen.contains(name));
    }

    fn connect(&mut self, port: &midir::MidiInputPort, name: &str) {
        let Ok(mut input) = MidiInput::new(CLIENT_NAME) else { return };
        input.ignore(Ignore::None);
        let tx = self.tx.clone();
        let device = name.to_string();
        match input.connect(
            port,
            name,
            move |_stamp, bytes, ()| {
                if let Some(ev) = parse(bytes, &device) {
                    let _ = tx.send(ev);
                }
            },
            (),
        ) {
            Ok(conn) => {
                self.conns.insert(name.to_string(), conn);
            }
            Err(err) => log::warn!("midi: failed to connect {name}: {err}"),
        }
    }
}

/// Parse one MIDI message. `0x9n` with velocity > 0 is a note-on
/// (`Pressed`); `0x8n`, or `0x9n` with velocity 0 (running-status
/// note-off), is `Released`; `0xBn` is a control change (`Continuous`,
/// normalized `0..=1`). Channel is `(status & 0x0F) + 1` (1-16). Anything
/// else, or a too-short message, is `None`.
#[must_use]
pub fn parse(bytes: &[u8], device: &str) -> Option<ControlEvent> {
    if bytes.len() < 3 {
        return None;
    }
    let status = bytes[0];
    let d1 = bytes[1];
    let d2 = bytes[2];
    let channel = (status & 0x0F) + 1;

    match status & 0xF0 {
        0x90 => {
            let source = ControlSource::MidiNote { device: device.to_string(), channel, note: d1 };
            let value = if d2 > 0 { EventValue::Pressed } else { EventValue::Released };
            Some(ControlEvent { source, value })
        }
        0x80 => {
            let source = ControlSource::MidiNote { device: device.to_string(), channel, note: d1 };
            Some(ControlEvent { source, value: EventValue::Released })
        }
        0xB0 => {
            let source = ControlSource::MidiCc { device: device.to_string(), channel, cc: d1 };
            Some(ControlEvent { source, value: EventValue::Continuous(f32::from(d2) / 127.0) })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_on_with_velocity_is_pressed() {
        let ev = parse(&[0x90, 60, 100], "Foo").unwrap();
        assert_eq!(ev.value, EventValue::Pressed);
        assert_eq!(ev.source, ControlSource::MidiNote { device: "Foo".into(), channel: 1, note: 60 });
    }

    #[test]
    fn note_on_with_zero_velocity_is_released() {
        let ev = parse(&[0x90, 60, 0], "Foo").unwrap();
        assert_eq!(ev.value, EventValue::Released);
    }

    #[test]
    fn note_off_is_released() {
        let ev = parse(&[0x80, 60, 0], "Foo").unwrap();
        assert_eq!(ev.value, EventValue::Released);
    }

    #[test]
    fn control_change_is_continuous_normalized() {
        let ev = parse(&[0xB0, 21, 127], "Foo").unwrap();
        assert_eq!(ev.value, EventValue::Continuous(1.0));
        assert_eq!(ev.source, ControlSource::MidiCc { device: "Foo".into(), channel: 1, cc: 21 });
    }

    #[test]
    fn channel_is_one_indexed() {
        let ev = parse(&[0xB5, 21, 0], "Foo").unwrap();
        assert_eq!(ev.source, ControlSource::MidiCc { device: "Foo".into(), channel: 6, cc: 21 });
    }

    #[test]
    fn unhandled_status_is_none() {
        assert!(parse(&[0xF0, 1, 2], "Foo").is_none());
    }

    #[test]
    fn short_message_is_none() {
        assert!(parse(&[0x90, 60], "Foo").is_none());
    }
}
