//! The reply half of the protocol: per-query payload structs, the view
//! structs they embed (mirrors of vidiotic's `UiMirror` sub-views), and the
//! [`WireReply`] sum carried in a reply envelope's `ok` field.
//!
//! [`WireReply`] has hand-written `SerJson`/`DeJson` impls so its JSON is
//! `{"Status":{...}}` — the variant name keying the payload object directly —
//! rather than the derived tuple-variant form `{"Status":[{...}]}` with a
//! one-element array wrapper.

use nanoserde::{DeJson, DeJsonErr, DeJsonState, SerJson, SerJsonState};

use crate::command::{
    WireCadence, WireCamDelay, WireChainSlot, WireSyncKind, WireTimeSig, WireToggleF64,
    WireToggleI32, WireToggleU32,
};
use crate::isf::WireIsfInput;

/// A clip/cue's live-playback role, for markers.
///
/// Mirrors `vidiotic::commands::ClipRole`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub enum WireClipRole {
    /// Not playing and not armed.
    None,
    /// Currently on the output.
    Playing,
    /// Queued to play next.
    Armed,
}

/// One source clip of the active clip bank.
///
/// Mirrors `vidiotic::commands::ClipEntry`, plus `duration_sec`/`fps` from the
/// engine's clip metadata (without a duration a script cannot compute a sane
/// out-point).
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireClipEntry {
    /// The clip's pool id.
    pub id: u32,
    /// Display name (file stem or camera name).
    pub name: String,
    /// Included in the auto-advance rotation.
    pub active: bool,
    /// Live-playback role marker.
    pub role: WireClipRole,
    /// A thumbnail is cached UI-side.
    pub has_thumb: bool,
    /// Source tempo metadata, if set.
    pub bpm: Option<f64>,
    /// The clip bank this entry is shown under.
    pub bank: u64,
    /// Source duration in seconds, when probed (`None` for cameras / unknown).
    pub duration_sec: Option<f64>,
    /// Source frame rate, when probed.
    pub fps: Option<f64>,
}

/// A clip bank's identity.
///
/// Mirrors `vidiotic::commands::ClipBankView`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireClipBankView {
    /// Display name (usually the source directory's).
    pub name: String,
    /// Number of clips in the bank.
    pub clip_count: u64,
}

/// One capture device in the pool's cameras section. `uid` is what
/// `SetCameraOnAir` / `AddCameraCue` / `RelinkCamera` consume.
///
/// Mirrors `vidiotic::commands::CameraEntry`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireCameraEntry {
    /// The device's stable unique id.
    pub uid: String,
    /// Human-readable device name.
    pub name: String,
    /// The device's capture service is running.
    pub on_air: bool,
    /// Human status line: "off air", "1920x1080 @ 30", or an error.
    pub status: String,
    /// A saved project references this uid but no connected device has it.
    pub missing: bool,
    /// Has a cue in the live bank.
    pub active: bool,
    /// Playing/armed if its clip's cue is.
    pub role: WireClipRole,
}

/// One cue of the edit bank, with its full trim/chain/timing state.
///
/// Mirrors `vidiotic::commands::CueView`. Ticks are 1/32-beat.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireCueView {
    /// The cue's id.
    pub id: u32,
    /// The source clip's pool id.
    pub clip: u32,
    /// Display name.
    pub name: String,
    /// In-point, seconds.
    pub in_sec: f64,
    /// Out-point, seconds; `None` = clip end.
    pub out_sec: Option<f64>,
    /// Per-cue preserve-playhead override; `None` = inherit global.
    pub preserve: Option<bool>,
    /// Per-cue effect chain; empty = the live shader.
    pub chain: Vec<WireChainSlot>,
    /// Playing/armed if this cue is the live bank's current/next.
    pub role: WireClipRole,
    /// A thumbnail is cached UI-side.
    pub has_thumb: bool,
    /// Beats-until-advance in ticks; `None` = inherit global.
    pub dwell: Option<u32>,
    /// Re-loop grid in ticks; `None` = inherit global.
    pub loop_len: Option<u32>,
    /// Loop-grid micro-timing, signed ticks.
    pub loop_phase: WireToggleI32,
    /// In-point nudge, seconds.
    pub start_nudge: WireToggleF64,
    /// Swap-in lead-in, ticks.
    pub trig_delay: WireToggleU32,
    /// This cue's source-tempo override.
    pub bpm: Option<f64>,
    /// The source clip's own BPM (the inherited value).
    pub clip_bpm: Option<f64>,
    /// Retime to session tempo.
    pub bpm_sync_on: bool,
    /// User speed multiplier.
    pub speed_mul: WireToggleF64,
    /// Resolved effective playback speed.
    pub speed: f64,
    /// Camera-sourced: timeline knobs inert, delay applies.
    pub camera: bool,
    /// Camera cues: voluntary delay behind the live edge.
    pub delay: WireCamDelay,
    /// Current slewed/quantized delay, seconds.
    pub delay_eff: f64,
}

/// A cue bank's identity.
///
/// Mirrors `vidiotic::commands::BankView`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireBankView {
    /// Display name.
    pub name: String,
    /// Number of cues in the bank.
    pub cue_count: u64,
}

/// A pool shader. `builtin` entries are bundled effects addressable by stable
/// name; non-builtin entries are livecoded pins (runtime-only).
///
/// Mirrors `vidiotic::commands::ShaderPoolView`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireShaderPoolView {
    /// The shader's pool id.
    pub id: u32,
    /// Display / stable name.
    pub name: String,
    /// A bundled effect (persistable by name) rather than a livecoded pin.
    pub builtin: bool,
    /// ISF input schema for parameter editing; empty for non-ISF entries.
    pub inputs: Vec<WireIsfInput>,
}

/// Answer to [`crate::query::WireQuery::Status`]: session identity and mode
/// flags.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireStatus {
    /// The loaded `.viproj` path; `None` for an unsaved session.
    pub project_path: Option<String>,
    /// The session generation, bumped on project load.
    pub epoch: u64,
    /// The protocol version the server speaks ([`crate::WIRE_VERSION`]).
    pub wire_version: u32,
    /// Advanced sequencer mode is on.
    pub advanced: bool,
    /// The modal command grammar is enabled.
    pub grammar_on: bool,
}

/// Answer to [`crate::query::WireQuery::Transport`]: clock and sync state.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireTransport {
    /// Session tempo, quarter notes per minute.
    pub bpm: f64,
    /// Beats since the grid origin.
    pub beat: f64,
    /// Position within the bar, `0..quantum`.
    pub phase: f64,
    /// Beats per bar (quarter notes; fractional for x/8, x/16 signatures).
    pub quantum: f64,
    /// The musical time signature.
    pub time_sig: WireTimeSig,
    /// Length between auto-transitions to the next clip.
    pub phrase_cadence: WireCadence,
    /// Forced video re-loop grid; `None` = loop on EOF only.
    pub loop_cadence: Option<WireCadence>,
    /// The active clock source; `None` while none is attached.
    pub sync: Option<WireSyncKind>,
    /// Link peers currently connected.
    pub peers: u64,
    /// The active clock source accepts tempo edits (Link is listen-only).
    pub can_set_tempo: bool,
    /// The active clock source accepts phase edits.
    pub can_set_phase: bool,
}

/// Answer to [`crate::query::WireQuery::Pool`]: clip banks, clips, cameras.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WirePool {
    /// All clip banks.
    pub clip_banks: Vec<WireClipBankView>,
    /// Index of the bank the pool grid shows (and `clips` lists).
    pub active_clip_bank: u64,
    /// The active clip bank's clips, in id order.
    pub clips: Vec<WireClipEntry>,
    /// The pool's clip cursor.
    pub selected_clip: Option<u32>,
    /// Enumerated capture devices (refresh with `RefreshCameras`).
    pub cameras: Vec<WireCameraEntry>,
}

/// Answer to [`crate::query::WireQuery::Cues`]: cue banks and the edit bank's
/// cues.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireCues {
    /// All cue banks.
    pub banks: Vec<WireBankView>,
    /// Index of the bank the sequencer plays.
    pub live_bank: u64,
    /// Index of the bank being edited (and whose cues `cues` lists).
    pub edit_bank: u64,
    /// The edit bank's cues, in order.
    pub cues: Vec<WireCueView>,
    /// The cue selection.
    pub selected_cue: Option<u32>,
}

/// Answer to [`crate::query::WireQuery::Shaders`]: the shader pool with ISF
/// input schemas.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireShaders {
    /// Every pool shader a cue chain can reference.
    pub shaders: Vec<WireShaderPoolView>,
}

/// Answer to [`crate::query::WireQuery::Audio`]: input devices and selection.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireAudio {
    /// Device names; the name doubles as the selection key.
    pub devices: Vec<String>,
    /// The currently selected device, `None` = default.
    pub current: Option<String>,
    /// The last audio subsystem error, if any.
    pub error: Option<String>,
}

/// Answer to [`crate::query::WireQuery::Levels`]: the audio analysis frame.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireLevels {
    /// 21 perceptual log bands, 0..1.
    pub levels: Vec<f32>,
    /// 512 linear spectrum bins, 0..1.
    pub spectrum_linear: Vec<f32>,
    /// Overall level, 0..1.
    pub level: f32,
}

/// A query's answer: one variant per [`crate::query::WireQuery`] kind.
///
/// JSON shape (hand-written impls): an object with the variant name keying
/// the payload directly, e.g. `{"Status":{"epoch":1,...}}`.
#[derive(Clone, Debug, PartialEq)]
pub enum WireReply {
    /// Answer to `WireQuery::Status`.
    Status(WireStatus),
    /// Answer to `WireQuery::Transport`.
    Transport(WireTransport),
    /// Answer to `WireQuery::Pool`.
    Pool(WirePool),
    /// Answer to `WireQuery::Cues`.
    Cues(WireCues),
    /// Answer to `WireQuery::Shaders`.
    Shaders(WireShaders),
    /// Answer to `WireQuery::Audio`.
    Audio(WireAudio),
    /// Answer to `WireQuery::Levels`.
    Levels(WireLevels),
}

impl SerJson for WireReply {
    fn ser_json(&self, d: usize, s: &mut SerJsonState) {
        s.out.push('{');
        let (label, payload): (&str, &dyn SerJson) = match self {
            Self::Status(p) => ("Status", p),
            Self::Transport(p) => ("Transport", p),
            Self::Pool(p) => ("Pool", p),
            Self::Cues(p) => ("Cues", p),
            Self::Shaders(p) => ("Shaders", p),
            Self::Audio(p) => ("Audio", p),
            Self::Levels(p) => ("Levels", p),
        };
        s.label(label);
        s.out.push(':');
        payload.ser_json(d, s);
        s.out.push('}');
    }
}

impl DeJson for WireReply {
    fn de_json(s: &mut DeJsonState, i: &mut core::str::Chars) -> Result<Self, DeJsonErr> {
        s.curly_open(i)?;
        // Copy the key out before advancing; `string()` + `colon()` step the
        // tokenizer onto the value, which would clobber `strbuf` if the value
        // began with a string token.
        let key = s.strbuf.clone();
        s.string(i)?;
        s.colon(i)?;
        let r = match key.as_ref() {
            "Status" => Self::Status(DeJson::de_json(s, i)?),
            "Transport" => Self::Transport(DeJson::de_json(s, i)?),
            "Pool" => Self::Pool(DeJson::de_json(s, i)?),
            "Cues" => Self::Cues(DeJson::de_json(s, i)?),
            "Shaders" => Self::Shaders(DeJson::de_json(s, i)?),
            "Audio" => Self::Audio(DeJson::de_json(s, i)?),
            "Levels" => Self::Levels(DeJson::de_json(s, i)?),
            _ => return Err(s.err_enum(&key)),
        };
        s.curly_close(i)?;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use nanoserde::{DeJson, SerJson};

    use super::*;
    use crate::command::WireSlotRef;
    use crate::isf::{WireIsfInputKind, WireIsfValue, WireParam};

    /// A fully populated view of every payload struct, with asymmetric values
    /// so field mix-ups fail the round-trip.
    pub(crate) fn reply_catalog() -> Vec<WireReply> {
        vec![
            WireReply::Status(WireStatus {
                project_path: Some("/proj/a.viproj".into()),
                epoch: 3,
                wire_version: crate::WIRE_VERSION,
                advanced: true,
                grammar_on: false,
            }),
            WireReply::Transport(WireTransport {
                bpm: 133.5,
                beat: 512.25,
                phase: 1.75,
                quantum: 3.5,
                time_sig: WireTimeSig { num: 7, den: 8 },
                phrase_cadence: WireCadence::Bars(4),
                loop_cadence: Some(WireCadence::Note(64)),
                sync: Some(WireSyncKind::Link),
                peers: 2,
                can_set_tempo: false,
                can_set_phase: true,
            }),
            WireReply::Pool(WirePool {
                clip_banks: vec![
                    WireClipBankView { name: "setA".into(), clip_count: 2 },
                    WireClipBankView { name: "setB".into(), clip_count: 0 },
                ],
                active_clip_bank: 0,
                clips: vec![
                    WireClipEntry {
                        id: 0,
                        name: "intro".into(),
                        active: true,
                        role: WireClipRole::Playing,
                        has_thumb: true,
                        bpm: Some(120.0),
                        bank: 0,
                        duration_sec: Some(12.48),
                        fps: Some(29.97),
                    },
                    WireClipEntry {
                        id: 1,
                        name: "cam".into(),
                        active: false,
                        role: WireClipRole::None,
                        has_thumb: false,
                        bpm: None,
                        bank: 0,
                        duration_sec: None,
                        fps: None,
                    },
                ],
                selected_clip: Some(1),
                cameras: vec![WireCameraEntry {
                    uid: "uid:cam0".into(),
                    name: "FaceTime HD".into(),
                    on_air: true,
                    status: "1920x1080 @ 30".into(),
                    missing: false,
                    active: true,
                    role: WireClipRole::Armed,
                }],
            }),
            WireReply::Cues(WireCues {
                banks: vec![WireBankView { name: "bank 1".into(), cue_count: 1 }],
                live_bank: 0,
                edit_bank: 0,
                cues: vec![WireCueView {
                    id: 5,
                    clip: 0,
                    name: "intro".into(),
                    in_sec: 0.5,
                    out_sec: Some(8.25),
                    preserve: Some(true),
                    chain: vec![WireChainSlot {
                        shader: WireSlotRef::Isf("fx/glitch.fs".into()),
                        params: vec![WireParam {
                            name: "amount".into(),
                            value: WireIsfValue::Float(0.3),
                        }],
                    }],
                    role: WireClipRole::Playing,
                    has_thumb: true,
                    dwell: Some(128),
                    loop_len: None,
                    loop_phase: WireToggleI32 { on: true, val: -4 },
                    start_nudge: WireToggleF64 { on: false, val: 0.02 },
                    trig_delay: WireToggleU32 { on: true, val: 16 },
                    bpm: None,
                    clip_bpm: Some(120.0),
                    bpm_sync_on: true,
                    speed_mul: WireToggleF64 { on: true, val: 0.5 },
                    speed: 0.5625,
                    camera: false,
                    delay: WireCamDelay { value: 2.0, beats: true, quantize: false },
                    delay_eff: 0.933,
                }],
                selected_cue: Some(5),
            }),
            WireReply::Shaders(WireShaders {
                shaders: vec![WireShaderPoolView {
                    id: 2,
                    name: "glitch".into(),
                    builtin: false,
                    inputs: vec![
                        WireIsfInput {
                            name: "amount".into(),
                            label: Some("Amount".into()),
                            kind: WireIsfInputKind::Float { min: 0.0, max: 1.0, default: 0.25 },
                        },
                        WireIsfInput {
                            name: "mode".into(),
                            label: None,
                            kind: WireIsfInputKind::Long {
                                values: vec![0, 1, 2],
                                labels: vec!["a".into(), "b".into(), "c".into()],
                                default: 1,
                            },
                        },
                    ],
                }],
            }),
            WireReply::Audio(WireAudio {
                devices: vec!["Built-in".into(), "Loopback".into()],
                current: Some("Loopback".into()),
                error: None,
            }),
            WireReply::Levels(WireLevels {
                levels: vec![0.5; 21],
                spectrum_linear: vec![0.125; 512],
                level: 0.75,
            }),
        ]
    }

    #[test]
    fn every_reply_round_trips() {
        for reply in reply_catalog() {
            let json = reply.serialize_json();
            let back = WireReply::deserialize_json(&json)
                .unwrap_or_else(|e| panic!("{e} in {json}"));
            assert_eq!(back, reply, "round-trip mismatch");
        }
    }

    #[test]
    fn reply_catalog_covers_every_variant() {
        // Compile-forcing guard: extending WireReply fails this match until
        // the new variant is classified, and the assert until it's in the
        // catalog.
        fn name(r: &WireReply) -> &'static str {
            match r {
                WireReply::Status(_) => "Status",
                WireReply::Transport(_) => "Transport",
                WireReply::Pool(_) => "Pool",
                WireReply::Cues(_) => "Cues",
                WireReply::Shaders(_) => "Shaders",
                WireReply::Audio(_) => "Audio",
                WireReply::Levels(_) => "Levels",
            }
        }
        let catalog = reply_catalog();
        let names: std::collections::BTreeSet<_> = catalog.iter().map(name).collect();
        assert_eq!(names.len(), 7);
        assert_eq!(catalog.len(), 7);
    }

    #[test]
    fn every_isf_value_and_input_kind_round_trips() {
        let values = [
            WireIsfValue::Float(0.5),
            WireIsfValue::Bool(true),
            WireIsfValue::Long(-3),
            WireIsfValue::Color([0.1, 0.2, 0.3, 1.0]),
            WireIsfValue::Point2D([0.5, -0.5]),
        ];
        for v in values {
            let json = v.serialize_json();
            assert_eq!(WireIsfValue::deserialize_json(&json).unwrap(), v, "{json}");
        }
        let kinds = [
            WireIsfInputKind::Float { min: -1.0, max: 1.0, default: 0.0 },
            WireIsfInputKind::Bool { default: true },
            WireIsfInputKind::Long {
                values: vec![1, 2],
                labels: vec!["one".into(), "two".into()],
                default: 2,
            },
            WireIsfInputKind::Color { default: [0.0, 0.5, 1.0, 1.0] },
            WireIsfInputKind::Point2D {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
                default: [0.5, 0.5],
            },
            WireIsfInputKind::Event,
            WireIsfInputKind::Image,
            WireIsfInputKind::Audio,
            WireIsfInputKind::AudioFft,
        ];
        for k in kinds {
            let json = k.serialize_json();
            assert_eq!(WireIsfInputKind::deserialize_json(&json).unwrap(), k, "{json}");
        }
    }
}
