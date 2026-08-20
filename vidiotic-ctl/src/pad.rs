//! Game controller IO (`gilrs`, `IOKit` backend on macOS).
//!
//! `gilrs` is poll-driven and its handle isn't assumed `Send` on macOS, so
//! [`PadPoller::poll`] must run on the same thread that created it — the
//! UI/main thread (vidiotic: inside `App::update`, whose loop is
//! `ControlFlow::Poll`; eframe apps: top of `update()`, paired with
//! `request_repaint_after` so polling keeps happening even when idle).
//!
//! `gilrs`'s macOS backend is the least-maintained of the three device
//! backends here; [`PadPoller::new`] never panics on init failure — it logs
//! and disables itself — so a flaky gamepad backend can't take down MIDI or
//! keyboard input.

use crossbeam_channel::Sender;
use gilrs::{EventType, Gilrs};

use crate::event::{ControlEvent, EventValue};
use crate::model::ControlSource;

pub struct PadPoller {
    gilrs: Option<Gilrs>,
}

impl PadPoller {
    /// Open the `gilrs` backend. Never panics on failure, per the module
    /// docs: it logs and disables gamepad input instead.
    #[must_use]
    pub fn new() -> Self {
        match Gilrs::new() {
            Ok(g) => Self { gilrs: Some(g) },
            Err(err) => {
                log::warn!("gamepad: init failed, gamepad input disabled: {err}");
                Self { gilrs: None }
            }
        }
    }

    /// Drain pending events onto `tx`. No-op if init failed.
    pub fn poll(&mut self, tx: &Sender<ControlEvent>) {
        let Some(gilrs) = self.gilrs.as_mut() else { return };
        while let Some(gilrs::Event { id, event, .. }) = gilrs.next_event() {
            let device = gilrs.gamepad(id).name().to_string();
            let sent = match event {
                EventType::ButtonPressed(button, _) => Some(ControlEvent {
                    source: ControlSource::PadButton { device, button: format!("{button:?}") },
                    value: EventValue::Pressed,
                }),
                EventType::ButtonReleased(button, _) => Some(ControlEvent {
                    source: ControlSource::PadButton { device, button: format!("{button:?}") },
                    value: EventValue::Released,
                }),
                EventType::AxisChanged(axis, value, _) => Some(ControlEvent {
                    source: ControlSource::PadAxis { device, axis: format!("{axis:?}") },
                    value: EventValue::Continuous((value + 1.0) / 2.0),
                }),
                _ => None,
            };
            if let Some(ev) = sent {
                let _ = tx.send(ev);
            }
        }
    }

    /// Currently connected gamepad names.
    pub fn device_names(&self) -> Vec<String> {
        let Some(gilrs) = &self.gilrs else { return Vec::new() };
        gilrs.gamepads().map(|(_, gamepad)| gamepad.name().to_string()).collect()
    }
}

impl Default for PadPoller {
    fn default() -> Self {
        Self::new()
    }
}
