//! `vidiotic-ctl`: control mapping (USB MIDI, keyboard, game controllers)
//! shared by `vidiotic` and `vidiotic-prep`.
//!
//! This lib must never depend on `vidiotic` — `vidiotic` depends on *it* to
//! embed [`model::ControlMap`] in `.viproj` — so it defines its own
//! [`model::Action`] vocabulary rather than `vidiotic::commands::Command`;
//! each app owns its own `Action -> Command` translation and rejects the
//! other's half of the vocabulary.

pub mod event;
pub mod keys;
pub mod learn;
pub mod mapper;
pub mod midi;
pub mod model;
pub mod pad;
pub mod store;
/// Shared binding-table widgets. Behind `egui-ui` so the lib stays usable
/// headless (and so `vidiotic` links it without any UI toolkit).
#[cfg(feature = "egui-ui")]
pub mod ui;

pub use event::{source_key, ControlEvent, EventValue};
pub use learn::Learn;
pub use mapper::Mapper;
pub use midi::MidiHub;
pub use model::{Action, Binding, ControlMap, ControlSource, PrepVerb};
pub use pad::PadPoller;
