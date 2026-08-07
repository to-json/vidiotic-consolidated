//! MIDI-learn-style capture: watch a stream of events and report the first
//! source that looks like a deliberate actuation, so a UI can bind it.

use std::collections::HashMap;

use crate::event::{source_key, ControlEvent, EventValue};
use crate::model::ControlSource;

/// One capture session. Create fresh per "learn" click; drop it once it
/// returns `Some`.
#[derive(Default)]
pub struct Learn {
    /// First-seen value per live source key, so idle jitter or a resting
    /// stick's initial (nonzero) position doesn't itself count as movement.
    baseline: HashMap<String, f32>,
}

/// Continuous movement past this fraction of the first-seen baseline counts
/// as a deliberate actuation (vs. idle jitter or stick centering noise).
const CAPTURE_THRESHOLD: f32 = 0.08;

impl Learn {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event; returns the source to bind as soon as it looks
    /// deliberate. The returned source keeps its concrete device name — UIs
    /// that want "any device" blank that field themselves.
    pub fn observe(&mut self, ev: &ControlEvent) -> Option<ControlSource> {
        match ev.value {
            EventValue::Pressed => Some(ev.source.clone()),
            EventValue::Released => None,
            EventValue::Continuous(v) => {
                let key = source_key(&ev.source);
                let baseline = *self.baseline.entry(key).or_insert(v);
                if (v - baseline).abs() >= CAPTURE_THRESHOLD {
                    Some(ev.source.clone())
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc() -> ControlSource {
        ControlSource::MidiCc { device: "Foo".into(), channel: 1, cc: 21 }
    }

    #[test]
    fn pressed_captures_immediately() {
        let mut learn = Learn::new();
        let ev = ControlEvent { source: cc(), value: EventValue::Pressed };
        assert_eq!(learn.observe(&ev), Some(cc()));
    }

    #[test]
    fn released_never_captures() {
        let mut learn = Learn::new();
        let ev = ControlEvent { source: cc(), value: EventValue::Released };
        assert_eq!(learn.observe(&ev), None);
    }

    #[test]
    fn idle_jitter_around_resting_position_does_not_capture() {
        let mut learn = Learn::new();
        let source = cc();
        // Resting stick sits at 0.5, not 0 — first sample sets the baseline.
        for v in [0.5, 0.51, 0.49, 0.5, 0.52] {
            let ev = ControlEvent { source: source.clone(), value: EventValue::Continuous(v) };
            assert_eq!(learn.observe(&ev), None, "jitter at {v} must not capture");
        }
    }

    #[test]
    fn deliberate_movement_captures() {
        let mut learn = Learn::new();
        let source = cc();
        let baseline_ev =
            ControlEvent { source: source.clone(), value: EventValue::Continuous(0.5) };
        assert_eq!(learn.observe(&baseline_ev), None);
        let moved_ev = ControlEvent { source: source.clone(), value: EventValue::Continuous(0.7) };
        assert_eq!(learn.observe(&moved_ev), Some(source));
    }
}
