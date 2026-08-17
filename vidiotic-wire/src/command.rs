//! The command half of the protocol: [`WireCommand`] mirrors vidiotic's
//! internal `Command` enum (minus the two picker-opening variants that make no
//! sense without a human at the UI), plus the aux value types its payloads
//! carry.
//!
//! Mapping conventions, applied uniformly: `Arc<str>` / `PathBuf` / `String`
//! source fields become `String`; `usize` becomes `u64`; ids stay `u32`;
//! tuples become named structs; the generic `Toggle<T>` is monomorphized as
//! [`WireToggleI32`] / [`WireToggleF64`] / [`WireToggleU32`].

use nanoserde::{DeJson, SerJson};

use crate::isf::{WireIsfValue, WireParam};

/// Which shader runs at one position in a cue's effect chain.
///
/// Mirrors `vidiotic::commands::SlotRef`. `Builtin` carries the effect's
/// stable name; `Pinned` is a runtime-only pool id; `Live` is the current
/// livecoded shader; `Isf` carries the ISF shader's file path.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub enum WireSlotRef {
    /// The current livecoded shader.
    Live,
    /// A bundled effect, addressed by stable name.
    Builtin(String),
    /// A pinned (captured) pool shader, addressed by runtime id.
    Pinned(u32),
    /// An ISF shader, addressed by file path (project-relative or absolute).
    Isf(String),
}

/// One entry in a cue's effect chain: the shader plus per-slot ISF input
/// overrides (empty for non-ISF slots or inputs left at their schema default).
///
/// Mirrors `vidiotic::commands::ChainSlot`.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub struct WireChainSlot {
    /// The shader occupying this slot.
    pub shader: WireSlotRef,
    /// Per-slot ISF input overrides.
    pub params: Vec<WireParam>,
}

/// Musical time signature: `num` notes of `1/den` each per bar. Tempo (BPM)
/// always counts quarter notes.
///
/// Mirrors `vidiotic::commands::TimeSig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub struct WireTimeSig {
    /// Beats per bar (numerator).
    pub num: u8,
    /// Note value of one beat (denominator; the engine snaps to 1/2/4/8/16).
    pub den: u8,
}

/// A musical cadence length: an absolute note value in 1/32-beat ticks, or a
/// count of bars of the current time signature.
///
/// Mirrors `vidiotic::commands::Cadence`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub enum WireCadence {
    /// Absolute length in 1/32-beat ticks.
    Note(u32),
    /// Whole bars of the current time signature.
    Bars(u32),
}

/// Which clock source drives the beat grid.
///
/// Mirrors `vidiotic::commands::SyncKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub enum WireSyncKind {
    /// The engine's internal clock.
    Internal,
    /// Ableton Link (listen-only: tempo/phase edits are rejected).
    Link,
}

/// An on/off knob retaining an `i32` value while off (monomorphized
/// `Toggle<i32>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub struct WireToggleI32 {
    /// Whether the knob is engaged.
    pub on: bool,
    /// The retained value (applies only while `on`).
    pub val: i32,
}

/// An on/off knob retaining an `f64` value while off (monomorphized
/// `Toggle<f64>`).
#[derive(Clone, Copy, Debug, PartialEq, SerJson, DeJson)]
pub struct WireToggleF64 {
    /// Whether the knob is engaged.
    pub on: bool,
    /// The retained value (applies only while `on`).
    pub val: f64,
}

/// An on/off knob retaining a `u32` value while off (monomorphized
/// `Toggle<u32>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub struct WireToggleU32 {
    /// Whether the knob is engaged.
    pub on: bool,
    /// The retained value (applies only while `on`).
    pub val: u32,
}

/// A camera cue's voluntary delay behind the live edge, dialed in seconds or
/// beats, slewed continuously or re-targeted at loop-grid boundaries.
///
/// Mirrors `vidiotic::bank::CamDelay`.
#[derive(Clone, Copy, Debug, PartialEq, SerJson, DeJson)]
pub struct WireCamDelay {
    /// The dialed delay amount.
    pub value: f64,
    /// `value` is in beats (re-resolved against the live tempo) rather than
    /// seconds.
    pub beats: bool,
    /// Re-target at loop-grid boundaries instead of slewing continuously.
    pub quantize: bool,
}

/// One advanced per-cue timing/speed knob, set via
/// [`WireCommand::SetCueParam`]. Ticks are 1/32-beat.
///
/// Mirrors `vidiotic::commands::CueParam`.
#[derive(Clone, Copy, Debug, PartialEq, SerJson, DeJson)]
pub enum WireCueParam {
    /// Beats-until-advance in ticks; `None` = inherit global.
    Dwell(Option<u32>),
    /// Re-loop grid in ticks; `None` = inherit global.
    Loop(Option<u32>),
    /// Loop-grid micro-timing, signed ticks.
    LoopPhase(WireToggleI32),
    /// In-point nudge, seconds.
    StartNudge(WireToggleF64),
    /// Swap-in lead-in, ticks.
    TrigDelay(WireToggleU32),
    /// Source tempo override; `None` = inherit clip.
    Bpm(Option<f64>),
    /// Retime to session tempo.
    BpmSync(bool),
    /// User speed multiplier.
    SpeedMul(WireToggleF64),
    /// Camera cues: voluntary delay behind the live edge.
    CamDelay(WireCamDelay),
}

/// The nudgeable [`WireCueParam`] knobs (all but the camera-only delay), for
/// [`WireCommand::NudgeCueParam`].
///
/// Mirrors `vidiotic::commands::CueParamKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerJson, DeJson)]
pub enum WireCueParamKind {
    /// Beats-until-advance.
    Dwell,
    /// Re-loop grid length.
    Loop,
    /// Loop-grid micro-timing.
    LoopPhase,
    /// In-point nudge.
    StartNudge,
    /// Swap-in lead-in.
    TrigDelay,
    /// Source tempo override.
    Bpm,
    /// Retime to session tempo.
    BpmSync,
    /// User speed multiplier.
    SpeedMul,
}

/// Everything a wire client can ask the engine to do.
///
/// Mirrors `vidiotic::commands::Command` with four exclusions.
///
/// `OpenProject` and `SaveProjectAs` open native file pickers and only make
/// sense from the UI — scripts use [`Self::LoadProject`] /
/// [`Self::SaveProjectTo`] with explicit paths. `SaveProject` and
/// `OpenProjectEditor` stay in the vocabulary; the engine gates them to
/// sessions with a known project path.
///
/// `Undo` and `Redo` are excluded because the edit history is per-UI state, not
/// session state: a wire client stepping someone else's undo stack would act on
/// a history it cannot see.
#[derive(Clone, Debug, PartialEq, SerJson, DeJson)]
pub enum WireCommand {
    /// Set the session tempo, quarter notes per minute.
    SetBpm(f64),
    /// Step the tempo by an absolute amount (the UI's +/- keys use ±1).
    BpmDelta(f64),
    /// Nudge the tempo by a ratio (±0.001 for the ±0.1% controls).
    NudgeBpm(f64),
    /// Snap the downbeat phase to now (does not change tempo).
    TapDownbeat,
    /// Derive BPM from the interval between successive taps.
    TapTempo,
    /// Reset the beat grid to bar 1 / beat 1 / phrase 1; playlist position and
    /// playhead untouched.
    SoftReset,
    /// Soft reset, plus jump the playlist back to its first cue and restart
    /// its playhead.
    HardReset,
    /// Select which clock source drives the beat grid.
    SetSyncSource(WireSyncKind),
    /// Set the musical time signature.
    SetTimeSig(WireTimeSig),
    /// Musical length between auto-transitions to the next clip.
    SetPhraseCadence(WireCadence),
    /// Forced video re-loop grid; `None` = loop on EOF only.
    SetLoopCadence(Option<WireCadence>),
    /// On cut, carry the playhead over (`true`) or restart the incoming clip
    /// from its start (`false`).
    SetPreservePlayhead(bool),
    /// Toggle a pool clip's active flag.
    ToggleClipActive(u32),
    /// Set the pool's clip cursor; `None` clears the selection.
    SelectClip(Option<u32>),
    /// Move the clip cursor through the active clip bank's order.
    SelectClipDelta(i32),
    /// Jump the clip cursor to the active clip bank's first clip.
    SelectClipFirst,
    /// Jump the clip cursor to the active clip bank's last clip.
    SelectClipLast,
    /// Add a full-length cue for this clip to the edit bank (also selects it).
    AddCue(u32),
    /// Remove a cue from the edit bank.
    RemoveCue(u32),
    /// Set the cue selection; `None` clears it.
    SelectCue(Option<u32>),
    /// Move the cue selection through the edit bank's cue order.
    SelectCueDelta(i32),
    /// Jump the cue selection to the edit bank's first cue.
    SelectCueFirst,
    /// Jump the cue selection to the edit bank's last cue.
    SelectCueLast,
    /// Set a cue's in-point, seconds.
    SetCueIn(u32, f64),
    /// Set a cue's out-point, seconds; `None` = clip end.
    SetCueOut(u32, Option<f64>),
    /// Snap a cue's in-point to the displayed playhead.
    SetCueInToPlayhead(u32),
    /// Snap a cue's out-point to the displayed playhead.
    SetCueOutToPlayhead(u32),
    /// Per-cue preserve-playhead override; `None` = inherit global.
    SetCuePreserve(u32, Option<bool>),
    /// Replace a cue's effect chain; empty = the live shader.
    SetCueChain(u32, Vec<WireChainSlot>),
    /// Set one ISF input on one chain slot of a cue, without replacing the
    /// whole chain.
    SetChainParam {
        /// The target cue.
        cue: u32,
        /// Index into the cue's chain.
        slot: u64,
        /// The ISF input's uniform name.
        name: String,
        /// The new value.
        value: WireIsfValue,
    },
    /// Compile an ISF `.fs` file into the pool and append it to the selected
    /// cue's chain.
    LoadIsf(String),
    /// Set one advanced per-cue timing/speed knob.
    SetCueParam(u32, WireCueParam),
    /// Step the selected cue's knob by ± one detent.
    NudgeCueParam(WireCueParamKind, i32),
    /// Reorder a cue within the edit bank to a target index.
    MoveCue(u32, u64),
    /// Source-clip tempo metadata; `None` clears it.
    SetClipBpm(u32, Option<f64>),
    /// Gate per-cue timing/speed resolution + the extended UI.
    SetAdvancedMode(bool),
    /// Enable/disable the modal command grammar.
    SetGrammarMode(bool),
    /// Append a new empty cue bank.
    AddBank,
    /// Duplicate the edit bank (cues get fresh ids) and append it.
    CloneBank,
    /// Select which bank the sequencer plays.
    SetLiveBank(u64),
    /// Step the live bank by ±1, wrapping.
    CycleLiveBank(i32),
    /// Select which bank the UI edits.
    SetEditBank(u64),
    /// Pin the current live shader's last-good compile into the pool.
    CaptureShader,
    /// Drop a pinned shader (cues referencing it fall back to the live
    /// shader).
    RemoveShader(u32),
    /// Replace the whole pool with one bank from this directory.
    SetClipDir(String),
    /// Append this directory as a new clip bank (keeps existing clips/cues).
    AddClipDirAsBank(String),
    /// Select which clip bank the pool grid shows.
    SetActiveClipBank(u64),
    /// Re-enumerate capture devices.
    RefreshCameras,
    /// Run/stop a capture device's service, by device uid.
    SetCameraOnAir(String, bool),
    /// Find-or-create the device's pool clip and add a cue for it to the edit
    /// bank, by device uid.
    AddCameraCue(String),
    /// Point every clip referencing the missing device `from` at the
    /// connected device `to`.
    RelinkCamera {
        /// The missing device's uid.
        from: String,
        /// The connected device's uid.
        to: String,
    },
    /// Load a livecode shader file.
    SetShaderPath(String),
    /// Select the audio input device by name; `None` = default.
    SetAudioDevice(Option<String>),
    /// Toggle output-window fullscreen.
    ToggleFullscreen,
    /// Save to the loaded project path. Engine-gated: errors when the session
    /// has no path (the UI would open a picker; the wire never does).
    SaveProject,
    /// Save the project to an explicit path.
    SaveProjectTo(String),
    /// Replace the running session with this `.viproj`.
    LoadProject(String),
    /// Save in place and launch the sibling prep GUI on the project file.
    /// Engine-gated like [`Self::SaveProject`].
    OpenProjectEditor,
    /// Launch the sibling `vidiotic-ctl` mapper. Not gated — it edits `.vmap`
    /// files, so it makes sense in a session with no project.
    OpenControlMapper,
    /// Quit the application.
    Quit,
}

#[cfg(test)]
mod tests {
    use nanoserde::{DeJson, SerJson};

    use super::*;
    use crate::isf::{WireIsfValue, WireParam};

    /// Compile-time exhaustiveness guard: adding a `WireCommand` variant fails
    /// this match until the variant is named here, and fails
    /// `catalog_covers_every_variant` until it is added to [`catalog`].
    fn variant_name(cmd: &WireCommand) -> &'static str {
        match cmd {
            WireCommand::SetBpm(..) => "SetBpm",
            WireCommand::BpmDelta(..) => "BpmDelta",
            WireCommand::NudgeBpm(..) => "NudgeBpm",
            WireCommand::TapDownbeat => "TapDownbeat",
            WireCommand::TapTempo => "TapTempo",
            WireCommand::SoftReset => "SoftReset",
            WireCommand::HardReset => "HardReset",
            WireCommand::SetSyncSource(..) => "SetSyncSource",
            WireCommand::SetTimeSig(..) => "SetTimeSig",
            WireCommand::SetPhraseCadence(..) => "SetPhraseCadence",
            WireCommand::SetLoopCadence(..) => "SetLoopCadence",
            WireCommand::SetPreservePlayhead(..) => "SetPreservePlayhead",
            WireCommand::ToggleClipActive(..) => "ToggleClipActive",
            WireCommand::SelectClip(..) => "SelectClip",
            WireCommand::SelectClipDelta(..) => "SelectClipDelta",
            WireCommand::SelectClipFirst => "SelectClipFirst",
            WireCommand::SelectClipLast => "SelectClipLast",
            WireCommand::AddCue(..) => "AddCue",
            WireCommand::RemoveCue(..) => "RemoveCue",
            WireCommand::SelectCue(..) => "SelectCue",
            WireCommand::SelectCueDelta(..) => "SelectCueDelta",
            WireCommand::SelectCueFirst => "SelectCueFirst",
            WireCommand::SelectCueLast => "SelectCueLast",
            WireCommand::SetCueIn(..) => "SetCueIn",
            WireCommand::SetCueOut(..) => "SetCueOut",
            WireCommand::SetCueInToPlayhead(..) => "SetCueInToPlayhead",
            WireCommand::SetCueOutToPlayhead(..) => "SetCueOutToPlayhead",
            WireCommand::SetCuePreserve(..) => "SetCuePreserve",
            WireCommand::SetCueChain(..) => "SetCueChain",
            WireCommand::SetChainParam { .. } => "SetChainParam",
            WireCommand::LoadIsf(..) => "LoadIsf",
            WireCommand::SetCueParam(..) => "SetCueParam",
            WireCommand::NudgeCueParam(..) => "NudgeCueParam",
            WireCommand::MoveCue(..) => "MoveCue",
            WireCommand::SetClipBpm(..) => "SetClipBpm",
            WireCommand::SetAdvancedMode(..) => "SetAdvancedMode",
            WireCommand::SetGrammarMode(..) => "SetGrammarMode",
            WireCommand::AddBank => "AddBank",
            WireCommand::CloneBank => "CloneBank",
            WireCommand::SetLiveBank(..) => "SetLiveBank",
            WireCommand::CycleLiveBank(..) => "CycleLiveBank",
            WireCommand::SetEditBank(..) => "SetEditBank",
            WireCommand::CaptureShader => "CaptureShader",
            WireCommand::RemoveShader(..) => "RemoveShader",
            WireCommand::SetClipDir(..) => "SetClipDir",
            WireCommand::AddClipDirAsBank(..) => "AddClipDirAsBank",
            WireCommand::SetActiveClipBank(..) => "SetActiveClipBank",
            WireCommand::RefreshCameras => "RefreshCameras",
            WireCommand::SetCameraOnAir(..) => "SetCameraOnAir",
            WireCommand::AddCameraCue(..) => "AddCameraCue",
            WireCommand::RelinkCamera { .. } => "RelinkCamera",
            WireCommand::SetShaderPath(..) => "SetShaderPath",
            WireCommand::SetAudioDevice(..) => "SetAudioDevice",
            WireCommand::ToggleFullscreen => "ToggleFullscreen",
            WireCommand::SaveProject => "SaveProject",
            WireCommand::SaveProjectTo(..) => "SaveProjectTo",
            WireCommand::LoadProject(..) => "LoadProject",
            WireCommand::OpenProjectEditor => "OpenProjectEditor",
            WireCommand::OpenControlMapper => "OpenControlMapper",
            WireCommand::Quit => "Quit",
        }
    }

    /// `Command` has 64 variants; the wire excludes four — `OpenProject` and
    /// `SaveProjectAs` (native file pickers), `Undo` and `Redo` (local edit
    /// history) — leaving 60.
    const EXPECTED_VARIANTS: usize = 60;

    /// One entry per `WireCommand` variant, with payloads exercising every
    /// aux type and `Option`/`Vec` shape (both `Some` and `None`, empty and
    /// populated).
    fn catalog() -> Vec<WireCommand> {
        vec![
            WireCommand::SetBpm(128.5),
            WireCommand::BpmDelta(-1.0),
            WireCommand::NudgeBpm(0.001),
            WireCommand::TapDownbeat,
            WireCommand::TapTempo,
            WireCommand::SoftReset,
            WireCommand::HardReset,
            WireCommand::SetSyncSource(WireSyncKind::Link),
            WireCommand::SetTimeSig(WireTimeSig { num: 7, den: 8 }),
            WireCommand::SetPhraseCadence(WireCadence::Bars(4)),
            WireCommand::SetLoopCadence(Some(WireCadence::Note(32))),
            WireCommand::SetPreservePlayhead(true),
            WireCommand::ToggleClipActive(3),
            WireCommand::SelectClip(None),
            WireCommand::SelectClipDelta(-2),
            WireCommand::SelectClipFirst,
            WireCommand::SelectClipLast,
            WireCommand::AddCue(7),
            WireCommand::RemoveCue(9),
            WireCommand::SelectCue(Some(4)),
            WireCommand::SelectCueDelta(1),
            WireCommand::SelectCueFirst,
            WireCommand::SelectCueLast,
            WireCommand::SetCueIn(2, 1.25),
            WireCommand::SetCueOut(2, None),
            WireCommand::SetCueInToPlayhead(5),
            WireCommand::SetCueOutToPlayhead(5),
            WireCommand::SetCuePreserve(6, Some(false)),
            WireCommand::SetCueChain(
                8,
                vec![
                    WireChainSlot {
                        shader: WireSlotRef::Live,
                        params: vec![],
                    },
                    WireChainSlot {
                        shader: WireSlotRef::Builtin("kaleido".into()),
                        params: vec![WireParam {
                            name: "sides".into(),
                            value: WireIsfValue::Long(6),
                        }],
                    },
                    WireChainSlot {
                        shader: WireSlotRef::Pinned(2),
                        params: vec![],
                    },
                    WireChainSlot {
                        shader: WireSlotRef::Isf("fx/glitch.fs".into()),
                        params: vec![WireParam {
                            name: "center".into(),
                            value: WireIsfValue::Point2D([0.5, 0.25]),
                        }],
                    },
                ],
            ),
            WireCommand::SetChainParam {
                cue: 8,
                slot: 1,
                name: "level".into(),
                value: WireIsfValue::Float(0.75),
            },
            WireCommand::LoadIsf("shaders/warp.fs".into()),
            WireCommand::SetCueParam(
                3,
                WireCueParam::StartNudge(WireToggleF64 {
                    on: true,
                    val: -0.05,
                }),
            ),
            WireCommand::NudgeCueParam(WireCueParamKind::SpeedMul, -1),
            WireCommand::MoveCue(3, 0),
            WireCommand::SetClipBpm(1, Some(174.0)),
            WireCommand::SetAdvancedMode(true),
            WireCommand::SetGrammarMode(false),
            WireCommand::AddBank,
            WireCommand::CloneBank,
            WireCommand::SetLiveBank(2),
            WireCommand::CycleLiveBank(-1),
            WireCommand::SetEditBank(0),
            WireCommand::CaptureShader,
            WireCommand::RemoveShader(4),
            WireCommand::SetClipDir("/clips/setA".into()),
            WireCommand::AddClipDirAsBank("/clips/setB".into()),
            WireCommand::SetActiveClipBank(1),
            WireCommand::RefreshCameras,
            WireCommand::SetCameraOnAir("uid:cam0".into(), true),
            WireCommand::AddCameraCue("uid:cam0".into()),
            WireCommand::RelinkCamera {
                from: "uid:old".into(),
                to: "uid:new".into(),
            },
            WireCommand::SetShaderPath("live.frag".into()),
            WireCommand::SetAudioDevice(None),
            WireCommand::ToggleFullscreen,
            WireCommand::SaveProject,
            WireCommand::SaveProjectTo("/proj/a.viproj".into()),
            WireCommand::LoadProject("/proj/b.viproj".into()),
            WireCommand::OpenProjectEditor,
            WireCommand::OpenControlMapper,
            WireCommand::Quit,
        ]
    }

    #[test]
    fn catalog_covers_every_variant() {
        let catalog = catalog();
        let names: std::collections::BTreeSet<_> = catalog.iter().map(variant_name).collect();
        assert_eq!(names.len(), catalog.len(), "catalog repeats a variant");
        assert_eq!(catalog.len(), EXPECTED_VARIANTS);
    }

    #[test]
    fn every_command_round_trips() {
        for cmd in catalog() {
            let json = cmd.serialize_json();
            let back = WireCommand::deserialize_json(&json)
                .unwrap_or_else(|e| panic!("{}: {e} in {json}", variant_name(&cmd)));
            assert_eq!(back, cmd, "round-trip mismatch for {json}");
        }
    }

    #[test]
    fn every_cue_param_round_trips() {
        let params = [
            WireCueParam::Dwell(Some(128)),
            WireCueParam::Dwell(None),
            WireCueParam::Loop(Some(64)),
            WireCueParam::LoopPhase(WireToggleI32 { on: true, val: -3 }),
            WireCueParam::StartNudge(WireToggleF64 {
                on: false,
                val: 0.125,
            }),
            WireCueParam::TrigDelay(WireToggleU32 { on: true, val: 16 }),
            WireCueParam::Bpm(Some(90.0)),
            WireCueParam::BpmSync(true),
            WireCueParam::SpeedMul(WireToggleF64 { on: true, val: 2.0 }),
            WireCueParam::CamDelay(WireCamDelay {
                value: 1.5,
                beats: true,
                quantize: true,
            }),
        ];
        for p in params {
            let json = p.serialize_json();
            assert_eq!(WireCueParam::deserialize_json(&json).unwrap(), p, "{json}");
        }
    }

    #[test]
    fn every_cue_param_kind_round_trips() {
        let kinds = [
            WireCueParamKind::Dwell,
            WireCueParamKind::Loop,
            WireCueParamKind::LoopPhase,
            WireCueParamKind::StartNudge,
            WireCueParamKind::TrigDelay,
            WireCueParamKind::Bpm,
            WireCueParamKind::BpmSync,
            WireCueParamKind::SpeedMul,
        ];
        for k in kinds {
            let json = k.serialize_json();
            assert_eq!(
                WireCueParamKind::deserialize_json(&json).unwrap(),
                k,
                "{json}"
            );
        }
    }
}
