//! The `.vprep` sidecar: the autosaved span list and settings, so a marking
//! session survives quits, crashes and reloads. RON via nanoserde, the same as
//! the `.viproj` format.
//!
//! # One format, two stores
//!
//! Natively this is a file beside the video (`bun.mov.vprep`). In a browser it
//! is a record in OPFS. Those are entirely different storage APIs and neither is
//! here — what is here is the *shape*, and it is deliberately the same shape,
//! because a session is a session. A `.vprep` written by the desktop app parses
//! in the browser and the other way round.
//!
//! That is not a feature anybody asked for; it is what falls out of not writing
//! the format twice. The alternative — a browser session format invented
//! separately because the storage happened to differ — is how two halves of one
//! tool stop being able to hand work to each other.

use std::path::Path;

use nanoserde::{DeRon, SerRon};

use vidiotic_core::project::SessionDefaults;

use crate::editor::Editor;
use crate::spans::Span;

pub const SESSION_VERSION: u32 = 1;

/// Everything worth restoring about a marking session, minus the media itself.
#[derive(SerRon, DeRon)]
pub struct SessionFile {
    #[nserde(default)]
    pub version: u32,
    pub spans: Vec<SpanRec>,
    pub bank_names: Vec<String>,
    pub defaults: SessionDefaults,
    #[nserde(default)]
    pub snap_beats: f64,
    #[nserde(default)]
    pub controls: vidiotic_ctl::ControlMap,
}

/// On-disk mirror of [`Span`].
#[derive(SerRon, DeRon)]
pub struct SpanRec {
    pub name: String,
    pub in_frame: u64,
    pub out_frame: u64,
    pub bpm: Option<f64>,
    pub clip_bank: usize,
    #[nserde(default)]
    pub crop: Option<vidiotic_core::project::CropRect>,
}

impl SessionFile {
    /// Snapshot the restorable parts of `ed` scoped to `source`: only spans
    /// marked on that video are written, since the sidecar lives beside it.
    /// Session-wide settings (banks/defaults/controls) are captured in full
    /// regardless, so the sidecar stays independently useful.
    ///
    /// `controls` is the *player's* map, bound for the `.viproj`. It is passed
    /// in rather than read off the editor because the editor does not have one:
    /// prep keeps it in `Controls`, and the browser has none at all.
    #[must_use]
    pub fn capture(ed: &Editor, controls: &vidiotic_ctl::ControlMap, source: &Path) -> Self {
        Self {
            version: SESSION_VERSION,
            spans: ed
                .spans
                .spans
                .iter()
                .filter(|s| s.source == source)
                .map(|s| SpanRec {
                    name: s.name.clone(),
                    in_frame: s.in_frame,
                    out_frame: s.out_frame,
                    bpm: s.bpm,
                    clip_bank: s.clip_bank,
                    crop: s.crop,
                })
                .collect(),
            bank_names: ed.bank_names.clone(),
            defaults: ed.defaults.clone(),
            snap_beats: ed.snap_beats,
            controls: controls.clone(),
        }
    }

    /// Merge this snapshot's spans (tagged with `source`) into `ed`, appending
    /// rather than replacing so spans retained from other videos this session
    /// aren't disturbed.
    ///
    /// `adopt_globals` controls whether the session-wide settings — banks,
    /// defaults, snap beats, controls — overwrite the live session state. Only
    /// the first video opened in a session should adopt them, or switching
    /// videos would silently stomp settings made while marking an earlier one.
    pub fn merge_into(
        self,
        ed: &mut Editor,
        controls: &mut vidiotic_ctl::ControlMap,
        source: &Path,
        adopt_globals: bool,
    ) {
        let mut spans: Vec<Span> = self
            .spans
            .into_iter()
            .map(|r| Span {
                name: r.name,
                in_frame: r.in_frame,
                out_frame: r.out_frame.max(r.in_frame + 1),
                bpm: r.bpm,
                clip_bank: r.clip_bank,
                source: source.to_path_buf(),
                crop: r.crop,
            })
            .collect();
        ed.spans.spans.append(&mut spans);
        if adopt_globals {
            if !self.bank_names.is_empty() {
                ed.bank_names = self.bank_names;
            }
            ed.defaults = self.defaults;
            if self.snap_beats > 0.0 {
                ed.snap_beats = self.snap_beats;
            }
            *controls = self.controls;
        }
    }
}

/// Parse a sidecar's RON.
///
/// # Errors
/// Propagates the RON parse error, labelled with `label` — a path natively, a
/// storage key in a browser.
pub fn parse(text: &str, label: &str) -> anyhow::Result<SessionFile> {
    SessionFile::deserialize_ron(text).map_err(|e| anyhow::anyhow!("parse {label}: {e}"))
}

/// Serialize a sidecar.
#[must_use]
pub fn to_ron(file: &SessionFile) -> String {
    file.serialize_ron()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn span(name: &str, source: &str) -> Span {
        Span {
            name: name.to_string(),
            in_frame: 0,
            out_frame: 10,
            bpm: None,
            clip_bank: 0,
            source: PathBuf::from(source),
            crop: None,
        }
    }

    #[test]
    fn capture_scopes_spans_to_source() {
        let mut ed = Editor::default();
        ed.spans.spans.push(span("on a", "/a.mov"));
        ed.spans.spans.push(span("on b", "/b.mov"));
        let controls = vidiotic_ctl::ControlMap::default();

        let a = SessionFile::capture(&ed, &controls, Path::new("/a.mov"));
        assert_eq!(a.spans.len(), 1);
        assert_eq!(a.spans[0].name, "on a");
        let b = SessionFile::capture(&ed, &controls, Path::new("/b.mov"));
        assert_eq!(b.spans[0].name, "on b");
    }

    #[test]
    fn merge_into_appends_and_gates_globals() {
        let mut ed = Editor::default();
        let mut controls = vidiotic_ctl::ControlMap::default();

        let first = SessionFile {
            version: SESSION_VERSION,
            spans: vec![SpanRec {
                name: "on a".into(),
                in_frame: 0,
                out_frame: 10,
                bpm: None,
                clip_bank: 0,
                crop: None,
            }],
            bank_names: vec!["from-a".to_string()],
            defaults: SessionDefaults {
                bpm: 100.0,
                ..Default::default()
            },
            snap_beats: 8.0,
            controls: vidiotic_ctl::ControlMap::default(),
        };
        first.merge_into(&mut ed, &mut controls, Path::new("/a.mov"), true);
        assert_eq!(ed.spans.spans.len(), 1);
        assert_eq!(ed.bank_names, vec!["from-a".to_string()]);
        assert!((ed.defaults.bpm - 100.0).abs() < f64::EPSILON);

        let second = SessionFile {
            version: SESSION_VERSION,
            spans: vec![SpanRec {
                name: "on b".into(),
                in_frame: 0,
                out_frame: 5,
                bpm: None,
                clip_bank: 0,
                crop: None,
            }],
            bank_names: vec!["from-b".to_string()],
            defaults: SessionDefaults {
                bpm: 200.0,
                ..Default::default()
            },
            snap_beats: 2.0,
            controls: vidiotic_ctl::ControlMap::default(),
        };
        second.merge_into(&mut ed, &mut controls, Path::new("/b.mov"), false);
        assert_eq!(ed.spans.spans.len(), 2, "spans append rather than replace");
        assert_eq!(
            ed.bank_names,
            vec!["from-a".to_string()],
            "globals not stomped"
        );
        assert!(
            (ed.defaults.bpm - 100.0).abs() < f64::EPSILON,
            "globals not stomped"
        );
    }

    /// The whole point of the sidecar: what a session writes is what it reads.
    #[test]
    fn a_session_round_trips_through_ron() {
        let mut ed = Editor::default();
        ed.spans.spans.push(span("cut", "/v.mov"));
        ed.bank_names = vec!["cuts".to_string()];
        ed.snap_beats = 8.0;
        let controls = vidiotic_ctl::ControlMap::default();

        let text = to_ron(&SessionFile::capture(&ed, &controls, Path::new("/v.mov")));
        let back = parse(&text, "test").expect("parse");

        let mut restored = Editor::default();
        let mut ctl = vidiotic_ctl::ControlMap::default();
        back.merge_into(&mut restored, &mut ctl, Path::new("/v.mov"), true);
        assert_eq!(restored.spans.spans.len(), 1);
        assert_eq!(restored.spans.spans[0].name, "cut");
        assert_eq!(restored.bank_names, vec!["cuts".to_string()]);
        assert!((restored.snap_beats - 8.0).abs() < f64::EPSILON);
    }

    /// The control map travels with the session, and must survive RON.
    #[test]
    fn the_control_map_round_trips() {
        use vidiotic_ctl::{Action, Binding, ControlSource};
        let controls = vidiotic_ctl::ControlMap {
            bindings: vec![Binding {
                source: ControlSource::MidiCc {
                    device: "Launchkey Mini MK3".into(),
                    channel: 1,
                    cc: 21,
                },
                action: Action::SetBpm {
                    min: 60.0,
                    max: 180.0,
                },
            }],
        };
        let file = SessionFile::capture(&Editor::default(), &controls, Path::new("/v.mov"));
        let back = parse(&to_ron(&file), "test").expect("parse");
        assert_eq!(back.controls.bindings, controls.bindings);
    }

    /// A hand-written sidecar predating the controls field still parses.
    #[test]
    fn missing_controls_field_defaults_empty() {
        let text = r"(
            version: 1,
            spans: [],
            bank_names: [],
            defaults: (bpm: 120.0, quantum: 4.0, phrase_len: 16),
        )";
        let file = parse(text, "hand-written").expect("parse");
        assert!(file.controls.bindings.is_empty());
    }

    /// An inverted range would make a zero-length span the editor then has to
    /// defend against everywhere downstream.
    #[test]
    fn a_degenerate_span_is_widened_on_the_way_back_in() {
        let file = SessionFile {
            version: SESSION_VERSION,
            spans: vec![SpanRec {
                name: "x".into(),
                in_frame: 5,
                out_frame: 5,
                bpm: None,
                clip_bank: 0,
                crop: None,
            }],
            bank_names: vec![],
            defaults: SessionDefaults::default(),
            snap_beats: 0.0,
            controls: vidiotic_ctl::ControlMap::default(),
        };
        let mut ed = Editor::default();
        let mut ctl = vidiotic_ctl::ControlMap::default();
        file.merge_into(&mut ed, &mut ctl, Path::new("/v.mov"), true);
        assert_eq!(ed.spans.spans[0].out_frame, 6);
    }
}
