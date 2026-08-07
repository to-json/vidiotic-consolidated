//! Reading a `.viproj` back into a marking session.
//!
//! The *file* is a shell's problem — natively a path to read, in a browser a
//! `File` the page hands over as text. What happens to the parsed
//! [`Project`] afterwards is not: reconstructing spans from each clip's
//! [`SpanProvenance`] is arithmetic over data already in hand, and both shells
//! want the same answer. So the transformation lives here and the two shells
//! differ only in how the string arrives.
//!
//! This is the same lesson `ReopenedProject` itself taught (see
//! [`crate::editor`]): the type was in the loader's module because that is
//! where it was *read*, which is not the same as where it belongs.

use std::path::PathBuf;

use vidiotic_core::project::{Project, SpanProvenance};

use crate::editor::ReopenedProject;
use crate::spans::Span;

impl ReopenedProject {
    /// Reconstruct a marking session from an exported project.
    ///
    /// **Lossy by design**: cue banks are discarded, so anything authored on
    /// cues downstream in vidiotic (trims, knob overrides, effect chains) does
    /// not survive a reopen→re-export round trip. Prep's job is source-trimming
    /// and first export, not cue preservation.
    ///
    /// `name` is what the project should be called on re-export — natively the
    /// file stem, in a browser the dropped file's name. `project_dir` is left
    /// empty; only a shell with a filesystem has anything to put there.
    ///
    /// # Errors
    /// Fails if any clip lacks provenance, or if clips were cut from more than
    /// one source video.
    pub fn from_project(proj: &Project, name: &str) -> anyhow::Result<Self> {
        let mut source: Option<&str> = None;
        let mut spans = Vec::with_capacity(proj.clips.len());
        let bank_names: Vec<String> = proj.clip_banks.iter().map(|b| b.name.clone()).collect();
        let bank_of = |id: u32| {
            proj.clip_banks
                .iter()
                .position(|b| b.clip_ids.contains(&id))
                .unwrap_or(0)
        };

        for clip in &proj.clips {
            let prov: &SpanProvenance = clip.source.as_ref().ok_or_else(|| {
                anyhow::anyhow!("clip \"{}\" has no span provenance; can't retrim", clip.name)
            })?;
            match source {
                None => source = Some(&prov.original_path),
                Some(s) if s != prov.original_path => anyhow::bail!(
                    "clips come from multiple sources ({s} and {}); prep edits one source at a time",
                    prov.original_path
                ),
                Some(_) => {}
            }
            spans.push(Span {
                name: clip.name.clone(),
                in_frame: prov.in_frame,
                out_frame: prov.out_frame.max(prov.in_frame + 1),
                bpm: clip.bpm,
                clip_bank: bank_of(clip.id),
                source: PathBuf::from(&prov.original_path),
            });
        }

        let source = source.ok_or_else(|| anyhow::anyhow!("project has no clips to retrim"))?;
        Ok(Self {
            source: PathBuf::from(source),
            spans,
            bank_names: if bank_names.is_empty() {
                vec!["clips".to_string()]
            } else {
                bank_names
            },
            defaults: proj.defaults.clone(),
            project_name: name.to_string(),
            project_dir: PathBuf::new(),
            controls: proj.controls.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn clip(id: u32, name: &str, src: &str, in_f: u64, out_f: u64) -> vidiotic_core::project::ClipSpec {
        vidiotic_core::project::ClipSpec {
            id,
            name: name.to_string(),
            source: Some(SpanProvenance {
                original_path: src.to_string(),
                in_frame: in_f,
                out_frame: out_f,
                in_sec: 0.0,
                out_sec: 0.0,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn spans_come_back_off_provenance() {
        let proj = Project {
            clips: vec![clip(1, "a", "/v.mov", 10, 20), clip(2, "b", "/v.mov", 30, 40)],
            ..Default::default()
        };
        let re = ReopenedProject::from_project(&proj, "p").expect("reopen");
        assert_eq!(re.source, PathBuf::from("/v.mov"));
        assert_eq!(re.spans.len(), 2);
        assert_eq!(re.spans[1].in_frame, 30);
    }

    /// Frame numbers only mean anything against one video, so a project whose
    /// clips came from several cannot be retrimmed as one session.
    #[test]
    fn clips_from_two_sources_are_refused() {
        let proj = Project {
            clips: vec![clip(1, "a", "/v.mov", 0, 10), clip(2, "b", "/other.mov", 0, 10)],
            ..Default::default()
        };
        assert!(ReopenedProject::from_project(&proj, "p").is_err());
    }

    /// An empty out point would make a zero-length span the editor then has to
    /// defend against everywhere downstream.
    #[test]
    fn an_inverted_span_is_widened_rather_than_kept() {
        let proj = Project { clips: vec![clip(1, "a", "/v.mov", 5, 5)], ..Default::default() };
        let re = ReopenedProject::from_project(&proj, "p").expect("reopen");
        assert_eq!(re.spans[0].out_frame, 6);
    }

    #[test]
    fn a_project_with_no_clips_has_nothing_to_retrim() {
        assert!(ReopenedProject::from_project(&Project::default(), "p").is_err());
    }
}
