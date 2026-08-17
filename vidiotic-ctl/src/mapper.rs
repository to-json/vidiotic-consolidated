//! Resolves live [`ControlEvent`]s against layered [`ControlMap`]s into
//! fired [`Action`]s.
//!
//! Layering: the `over` map wins outright if *any* of its bindings match the
//! event's source — including a `Nothing` binding, which is exactly how a
//! user masks a binding below. Only when `over` has no matching binding at
//! all does `base` get a look. The layers are deliberately named for their
//! precedence rather than their provenance: `vidiotic` layers a project's
//! embedded map over the user's `global.vmap`, while `vidiotic-prep` layers
//! the user's `prep.vmap` over its own hardcoded key defaults.
//!
//! Within a layer, several bindings can match the same non-device shape
//! (same MIDI channel+CC, same key+modifiers, …) with different `device`
//! strings; the closest device wins: exact string match, then a fuzzy match
//! (lowercased, whitespace-collapsed, trailing " <digits>" stripped,
//! equal-or-contains), then `""` (any device).
//!
//! Edge behavior: a trigger action (anything but a continuous one) fires
//! once on `Pressed` or on a continuous value's rising edge through 0.5;
//! `Released` never fires. A continuous action passes `Continuous(v)`
//! through as-is and never fires on `Pressed`/`Released`.

use std::collections::HashMap;

use crate::event::{source_key, ControlEvent, EventValue};
use crate::model::{Action, ControlMap, ControlSource};

#[derive(Default)]
pub struct Mapper {
    /// The fallback layer: consulted only when `over` has no binding at all
    /// for a source. `vidiotic` loads the user's `global.vmap` here;
    /// `vidiotic-prep` puts its hardcoded key defaults here.
    pub base: ControlMap,
    /// The winning layer: if *any* of its bindings match a source —
    /// including a `Nothing` one — `base` is never consulted for that
    /// source. That is how a user masks a lower binding.
    pub over: ControlMap,
    /// Last continuous value seen per live source key, for edge detection.
    last: HashMap<String, f32>,
}

impl Mapper {
    #[must_use]
    pub fn new(base: ControlMap, over: ControlMap) -> Self {
        Self {
            base,
            over,
            last: HashMap::new(),
        }
    }

    /// Whether any binding (in either layer) matches `source` — including a
    /// masking `Nothing` binding. `resolve` can't answer this on its own:
    /// it returns `None` for both "no binding" and "masked", but a caller
    /// arbitrating against a competing built-in (e.g. `vidiotic`'s hardcoded
    /// key defaults) needs to tell those apart to know whether to suppress
    /// the built-in.
    #[must_use]
    pub fn has_binding(&self, source: &ControlSource) -> bool {
        find_in_layer(&self.over, source).is_some() || find_in_layer(&self.base, source).is_some()
    }

    /// Resolve one event into a fired `(Action, value)`, or `None` if
    /// nothing fires this frame (no binding, masked, released, or a
    /// sub-threshold edge).
    pub fn resolve(&mut self, ev: &ControlEvent) -> Option<(Action, f32)> {
        let binding = find_in_layer(&self.over, &ev.source)
            .or_else(|| find_in_layer(&self.base, &ev.source))?;
        let action = binding.action;
        let key = source_key(&ev.source);

        if matches!(action, Action::Nothing) {
            return None;
        }

        if action.is_continuous() {
            return match ev.value {
                EventValue::Pressed => Some((action, 1.0)),
                EventValue::Released => None,
                EventValue::Continuous(v) => {
                    self.last.insert(key, v);
                    Some((action, v))
                }
            };
        }

        match ev.value {
            EventValue::Pressed => Some((action, 1.0)),
            EventValue::Released => {
                self.last.remove(&key);
                None
            }
            EventValue::Continuous(v) => {
                let prev = self.last.insert(key, v).unwrap_or(0.0);
                if prev < 0.5 && v >= 0.5 {
                    Some((action, 1.0))
                } else {
                    None
                }
            }
        }
    }
}

/// The non-device "shape" of a source: two bindings with the same shape but
/// different `device` compete only on device tier, never both match.
fn shape_eq(a: &ControlSource, b: &ControlSource) -> bool {
    use ControlSource::{Key, MidiCc, MidiNote, PadAxis, PadButton};
    match (a, b) {
        (
            MidiNote {
                channel: c1,
                note: n1,
                ..
            },
            MidiNote {
                channel: c2,
                note: n2,
                ..
            },
        ) => c1 == c2 && n1 == n2,
        (
            MidiCc {
                channel: c1,
                cc: cc1,
                ..
            },
            MidiCc {
                channel: c2,
                cc: cc2,
                ..
            },
        ) => c1 == c2 && cc1 == cc2,
        (
            Key {
                key: k1,
                ctrl: c1,
                alt: a1,
                shift: s1,
                cmd: m1,
            },
            Key {
                key: k2,
                ctrl: c2,
                alt: a2,
                shift: s2,
                cmd: m2,
            },
        ) => k1 == k2 && c1 == c2 && a1 == a2 && s1 == s2 && m1 == m2,
        (PadButton { button: b1, .. }, PadButton { button: b2, .. }) => b1 == b2,
        (PadAxis { axis: x1, .. }, PadAxis { axis: x2, .. }) => x1 == x2,
        _ => false,
    }
}

fn device_of(s: &ControlSource) -> &str {
    use ControlSource::{Key, MidiCc, MidiNote, PadAxis, PadButton};
    match s {
        MidiNote { device, .. }
        | MidiCc { device, .. }
        | PadButton { device, .. }
        | PadAxis { device, .. } => device,
        Key { .. } => "",
    }
}

fn device_norm(d: &str) -> String {
    let collapsed = d
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(pos) = collapsed.rfind(' ') {
        let tail = &collapsed[pos + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            return collapsed[..pos].to_string();
        }
    }
    collapsed
}

/// Lower is a closer match: `0` exact, `1` fuzzy, `2` any-device (`""`).
/// `None` = doesn't match at all.
fn device_tier(binding_device: &str, event_device: &str) -> Option<u8> {
    if binding_device == event_device {
        return Some(0);
    }
    if binding_device.is_empty() {
        return Some(2);
    }
    let b = device_norm(binding_device);
    let e = device_norm(event_device);
    if b == e || e.contains(&b) || b.contains(&e) {
        Some(1)
    } else {
        None
    }
}

fn find_in_layer<'a>(
    map: &'a ControlMap,
    ev_source: &ControlSource,
) -> Option<&'a crate::model::Binding> {
    let ev_device = device_of(ev_source);
    map.bindings
        .iter()
        .filter(|b| shape_eq(&b.source, ev_source))
        .filter_map(|b| device_tier(device_of(&b.source), ev_device).map(|tier| (tier, b)))
        .min_by_key(|(tier, _)| *tier)
        .map(|(_, b)| b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Binding;

    fn cc(device: &str, channel: u8, cc: u8) -> ControlSource {
        ControlSource::MidiCc {
            device: device.into(),
            channel,
            cc,
        }
    }

    fn key(k: &str) -> ControlSource {
        ControlSource::Key {
            key: k.into(),
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
        }
    }

    fn binding(source: ControlSource, action: Action) -> Binding {
        Binding { source, action }
    }

    #[test]
    fn trigger_fires_once_on_pressed() {
        let mut mapper = Mapper::new(
            ControlMap::default(),
            ControlMap {
                bindings: vec![binding(key("t"), Action::TapDownbeat)],
            },
        );
        let ev = ControlEvent {
            source: key("t"),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&ev), Some((Action::TapDownbeat, 1.0)));
    }

    #[test]
    fn released_never_fires() {
        let mut mapper = Mapper::new(
            ControlMap::default(),
            ControlMap {
                bindings: vec![binding(key("t"), Action::TapDownbeat)],
            },
        );
        let ev = ControlEvent {
            source: key("t"),
            value: EventValue::Released,
        };
        assert_eq!(mapper.resolve(&ev), None);
    }

    #[test]
    fn trigger_fires_once_per_rising_edge() {
        let mut mapper = Mapper::new(
            ControlMap::default(),
            ControlMap {
                bindings: vec![binding(cc("Foo", 1, 21), Action::TapDownbeat)],
            },
        );
        let src = cc("Foo", 1, 21);
        let low = ControlEvent {
            source: src.clone(),
            value: EventValue::Continuous(0.2),
        };
        let high = ControlEvent {
            source: src,
            value: EventValue::Continuous(0.9),
        };
        assert_eq!(mapper.resolve(&low), None);
        assert_eq!(mapper.resolve(&high), Some((Action::TapDownbeat, 1.0)));
        // Still high: no repeat fire until it drops back below 0.5.
        assert_eq!(mapper.resolve(&high), None);
        assert_eq!(mapper.resolve(&low), None);
        assert_eq!(mapper.resolve(&high), Some((Action::TapDownbeat, 1.0)));
    }

    #[test]
    fn continuous_action_passes_value_through() {
        let mut mapper = Mapper::new(
            ControlMap::default(),
            ControlMap {
                bindings: vec![binding(
                    cc("Foo", 1, 21),
                    Action::SetBpm {
                        min: 60.0,
                        max: 180.0,
                    },
                )],
            },
        );
        let ev = ControlEvent {
            source: cc("Foo", 1, 21),
            value: EventValue::Continuous(0.25),
        };
        assert_eq!(
            mapper.resolve(&ev),
            Some((
                Action::SetBpm {
                    min: 60.0,
                    max: 180.0
                },
                0.25
            ))
        );
    }

    #[test]
    fn over_layer_wins_outright() {
        let mut mapper = Mapper::new(
            ControlMap {
                bindings: vec![binding(key("t"), Action::TapDownbeat)],
            },
            ControlMap {
                bindings: vec![binding(key("t"), Action::TapTempo)],
            },
        );
        let ev = ControlEvent {
            source: key("t"),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&ev), Some((Action::TapTempo, 1.0)));
    }

    #[test]
    fn over_nothing_masks_base_binding() {
        let mut mapper = Mapper::new(
            ControlMap {
                bindings: vec![binding(key("t"), Action::TapDownbeat)],
            },
            ControlMap {
                bindings: vec![binding(key("t"), Action::Nothing)],
            },
        );
        let ev = ControlEvent {
            source: key("t"),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&ev), None);
    }

    /// The layering `vidiotic-prep` relies on: its hardcoded key defaults sit
    /// in `base`, so a source the user has not rebound still fires.
    #[test]
    fn base_is_the_fallback_for_sources_over_does_not_bind() {
        let mut mapper = Mapper::new(
            ControlMap {
                bindings: vec![
                    binding(key("t"), Action::TapDownbeat),
                    binding(key("b"), Action::TapTempo),
                ],
            },
            ControlMap {
                bindings: vec![binding(key("t"), Action::SoftReset)],
            },
        );
        // Rebound in `over`.
        let rebound = ControlEvent {
            source: key("t"),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&rebound), Some((Action::SoftReset, 1.0)));
        // Untouched by `over`: the base default still fires.
        let default = ControlEvent {
            source: key("b"),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&default), Some((Action::TapTempo, 1.0)));
    }

    #[test]
    fn exact_device_beats_fuzzy_beats_any() {
        let map = ControlMap {
            bindings: vec![
                binding(cc("", 1, 21), Action::SoftReset),
                binding(cc("launchkey", 1, 21), Action::TapTempo),
                binding(cc("Launchkey Mini MK3", 1, 21), Action::TapDownbeat),
            ],
        };
        let mut mapper = Mapper::new(ControlMap::default(), map);
        let ev = ControlEvent {
            source: cc("Launchkey Mini MK3", 1, 21),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&ev), Some((Action::TapDownbeat, 1.0)));
    }

    #[test]
    fn fuzzy_match_ignores_case_whitespace_and_trailing_number() {
        let map = ControlMap {
            bindings: vec![binding(
                cc("launchkey mini mk3 1", 1, 21),
                Action::TapDownbeat,
            )],
        };
        let mut mapper = Mapper::new(ControlMap::default(), map);
        let ev = ControlEvent {
            source: cc("Launchkey Mini MK3", 1, 21),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&ev), Some((Action::TapDownbeat, 1.0)));
    }

    #[test]
    fn no_matching_binding_resolves_to_none() {
        let mut mapper = Mapper::new(ControlMap::default(), ControlMap::default());
        let ev = ControlEvent {
            source: key("z"),
            value: EventValue::Pressed,
        };
        assert_eq!(mapper.resolve(&ev), None);
    }

    #[test]
    fn has_binding_distinguishes_masked_from_unmapped() {
        let mapper = Mapper::new(
            ControlMap::default(),
            ControlMap {
                bindings: vec![binding(key("t"), Action::Nothing)],
            },
        );
        // Masked: resolve() is None, but a binding does exist.
        assert!(mapper.has_binding(&key("t")));
        // Genuinely unmapped.
        assert!(!mapper.has_binding(&key("z")));
    }
}
