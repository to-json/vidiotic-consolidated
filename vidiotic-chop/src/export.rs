//! Turning a finished marking session into a `.viproj` and a bag of clips.
//!
//! # What is here and what is not
//!
//! Baking a span is a machine: natively ffmpeg seeks and decodes on a worker
//! thread, in a browser a `<video>` element is seek-stepped and the frames go
//! through [`vidiotic_bake::web::Baker`]. Neither is here.
//!
//! What *is* here is everything after that — assembling the clip specs, the
//! clip banks, the starter cue bank and the `Project` — and it turns out to be
//! all of it: [`assemble`] takes what each shell learned while baking and
//! returns the project. So the two exporters do not agree about the `.viproj`
//! format by being written carefully against the same spec. They agree because
//! there is one function.
//!
//! That mattered more than it looked like it would. `.viproj` is the contract
//! between prep and the player; a browser export that got a field subtly wrong
//! would produce a project that loads and then behaves differently, which is the
//! failure mode with no error message.
//!
//! [`zip`] is here for the same reason a zip is needed at all: a project is a
//! `.viproj` *plus a directory*, and a browser can only hand back one file.

use vidiotic_core::project::{
    self, ClipBankSpec, ClipSpec, CueBankSpec, CueSpec, Project, SessionDefaults, SpanProvenance,
};

use crate::spans::Span;

/// What a shell learned while baking one span.
///
/// The shape of a bake's *result*, not of the bake — deliberately, because the
/// two shells produce it in completely different ways and this is the only part
/// they need to agree on.
#[derive(Clone, Debug)]
pub struct BakedClip {
    /// Where the clip file sits, relative to the `.viproj`.
    pub path: String,
    /// What it was cut from, for [`SpanProvenance`] — natively a canonical
    /// absolute path, in a browser the name of the file the visitor opened.
    /// Either way it is what a reopen will try to match against.
    pub source_path: String,
    pub in_sec: f64,
    pub out_sec: f64,
    pub fps: f64,
    pub frames: u64,
    pub duration_sec: f64,
}

/// A filesystem-safe file name for span `i`'s baked clip.
///
/// The index prefix keeps identically-named spans from clobbering each other
/// and makes the clips directory sort in span order.
#[must_use]
pub fn clip_file_name(i: usize, span: &Span) -> String {
    format!(
        "{i:02}_{}_{}-{}.mov",
        sanitize(&span.name),
        span.in_frame,
        span.out_frame
    )
}

/// The source video, as an offsets export needs to describe it.
#[derive(Clone, Debug)]
pub struct SourceRef {
    /// The clip file name the project will reference — the source's stem with
    /// `.mov`, because that is what `/play`'s ingest renames a baked clip to.
    /// The two ends agreeing on this rename is the whole reason an offsets
    /// project finds its clip.
    pub clip_name: String,
    pub fps: f64,
    pub frames: u64,
    pub duration_sec: f64,
    /// What the visitor opened, for provenance.
    pub source_path: String,
}

impl SourceRef {
    /// The clip name `/play` will have interned for `source`.
    ///
    /// `/play`'s browser ingest bakes a dropped file and renames it
    /// `<stem>.mov`; a file already in HAP keeps its own name. Producing the
    /// same string here is what lets an offsets project name a clip the player
    /// will actually have.
    #[must_use]
    pub fn clip_name_for(source: &str) -> String {
        let stem = source.rsplit_once('.').map_or(source, |(stem, _)| stem);
        format!("{stem}.mov")
    }
}

/// Build a `.viproj` that renders nothing: one clip pointing at the whole
/// source, and every span as a *trimmed cue* into it.
///
/// # Why this is a third rendering mode and not a flag
///
/// [`assemble`] turns N spans into N baked clips. This turns them into N cues
/// over one clip, which is the same session expressed the way the player
/// already thinks: `CueSpec` has carried `in_sec`/`out_sec` all along, so
/// nothing in the runtime had to learn a new idea.
///
/// What it buys is the round trip. Baking is minutes and it is the reason
/// iterating on a chop is slow; an offsets project is a few KB, written
/// instantly, and lands on a source the player has already ingested once. What
/// it costs is self-containment: the file is useless without that source, which
/// is exactly the trade to make when you are handing your own work back to
/// yourself and exactly the wrong one when you are handing it to someone else.
///
/// Clip banks become **cue** banks here, because in this mode a span is a cue.
/// The grouping the visitor made is the grouping they get.
#[must_use]
pub fn assemble_offsets(
    spans: &[Span],
    source: &SourceRef,
    bank_names: &[String],
    defaults: SessionDefaults,
    controls: vidiotic_ctl::ControlMap,
) -> Project {
    let stem = source
        .clip_name
        .rsplit_once('.')
        .map_or(source.clip_name.as_str(), |(stem, _)| stem)
        .to_string();

    #[allow(clippy::needless_update)]
    let clip = ClipSpec {
        id: 0,
        path: source.clip_name.clone(),
        name: stem,
        bpm: None,
        fps: Some(source.fps),
        frames: Some(source.frames),
        duration_sec: Some(source.duration_sec),
        // The whole file, so a reopen of this project reconstructs one span
        // covering everything rather than reporting no provenance at all.
        source: Some(SpanProvenance {
            original_path: source.source_path.clone(),
            in_frame: 0,
            out_frame: source.frames,
            in_sec: 0.0,
            out_sec: source.duration_sec,
            ..Default::default()
        }),
        ..Default::default()
    };

    // Spans grouped by the bank they were put in, in bank order.
    let mut banks: std::collections::BTreeMap<usize, Vec<&Span>> =
        std::collections::BTreeMap::new();
    for span in spans {
        banks.entry(span.clip_bank).or_default().push(span);
    }

    #[allow(clippy::needless_update)]
    let cue_banks: Vec<CueBankSpec> = banks
        .into_iter()
        .map(|(idx, group)| CueBankSpec {
            name: bank_names
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("bank {idx}")),
            cues: group
                .into_iter()
                .map(|span| CueSpec {
                    name: span.name.clone(),
                    in_sec: span.in_frame as f64 / source.fps,
                    out_sec: Some(span.out_frame as f64 / source.fps),
                    bpm: span.bpm,
                    ..CueSpec::full_length(0, span.name.clone())
                })
                .collect(),
            ..Default::default()
        })
        .collect();

    #[allow(clippy::needless_update)]
    Project {
        version: project::FORMAT_VERSION,
        defaults,
        clips: vec![clip],
        clip_banks: vec![ClipBankSpec {
            name: bank_names
                .first()
                .cloned()
                .unwrap_or_else(|| "clips".to_string()),
            clip_ids: vec![0],
            ..Default::default()
        }],
        cue_banks,
        controls,
        ..Default::default()
    }
}

/// Build the `.viproj` for a completed bake.
///
/// `baked[i]` is the result of baking `spans[i]`; the two must be the same
/// length and in the same order, which is the one thing a caller can get wrong
/// and the reason this takes them as parallel slices rather than a map — a
/// missing key would be a silently short project.
///
/// # Panics
/// If `baked` is shorter than `spans`.
#[must_use]
pub fn assemble(
    spans: &[Span],
    baked: &[BakedClip],
    bank_names: &[String],
    defaults: SessionDefaults,
    controls: vidiotic_ctl::ControlMap,
    starter_cue_bank: bool,
) -> Project {
    assert!(
        baked.len() >= spans.len(),
        "every span must have been baked"
    );

    // `..Default::default()` on the spec literals even where every field is
    // set: additive `.viproj` fields must not break this build.
    #[allow(clippy::needless_update)]
    let clips: Vec<ClipSpec> = spans
        .iter()
        .zip(baked)
        .enumerate()
        .map(|(i, (span, b))| ClipSpec {
            id: u32::try_from(i).unwrap_or(u32::MAX),
            path: b.path.clone(),
            name: span.name.clone(),
            bpm: span.bpm,
            fps: Some(b.fps),
            frames: Some(b.frames),
            duration_sec: Some(b.duration_sec),
            source: Some(SpanProvenance {
                original_path: b.source_path.clone(),
                in_frame: span.in_frame,
                out_frame: span.out_frame,
                in_sec: b.in_sec,
                out_sec: b.out_sec,
                crop: span.crop,
                ..Default::default()
            }),
            crop: span.crop,
            ..Default::default()
        })
        .collect();

    // A `BTreeMap` rather than grouping in place: banks come out in index
    // order regardless of what order the spans were marked in.
    let mut banks: std::collections::BTreeMap<usize, Vec<u32>> = std::collections::BTreeMap::new();
    for (i, span) in spans.iter().enumerate() {
        banks
            .entry(span.clip_bank)
            .or_default()
            .push(u32::try_from(i).unwrap_or(u32::MAX));
    }

    #[allow(clippy::needless_update)]
    let clip_banks = banks
        .into_iter()
        .map(|(bank_idx, clip_ids)| ClipBankSpec {
            name: bank_names
                .get(bank_idx)
                .cloned()
                .unwrap_or_else(|| format!("bank {bank_idx}")),
            clip_ids,
            ..Default::default()
        })
        .collect();

    #[allow(clippy::needless_update)]
    let cue_banks = if starter_cue_bank {
        vec![CueBankSpec {
            name: "A".into(),
            cues: clips
                .iter()
                .map(|c| CueSpec::full_length(c.id, c.name.clone()))
                .collect(),
            ..Default::default()
        }]
    } else {
        Vec::new()
    };

    #[allow(clippy::needless_update)]
    Project {
        version: project::FORMAT_VERSION,
        defaults,
        clips,
        clip_banks,
        cue_banks,
        controls,
        ..Default::default()
    }
}

/// A project as `.viproj` bytes.
///
/// Wrapped so callers do not each reach for nanoserde, and so the browser and
/// desktop exporters cannot end up serializing through different paths — which
/// is the same reason [`assemble`] exists.
#[must_use]
pub fn viproj_bytes(p: &Project) -> Vec<u8> {
    vidiotic_core::project::to_ron_bytes(p)
}

// The zip writer moved to `vidiotic_core::bundle`: `/play` needs to write the
// same archive when it saves a session, and the two browser shells cannot
// depend on each other. Re-exported rather than renamed at the call sites,
// because a bundle is what `export` produces and this is where a reader looks
// for it.
pub use vidiotic_core::bundle::{sanitize, zip};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn span(name: &str, bank: usize) -> Span {
        Span {
            name: name.to_string(),
            in_frame: 10,
            out_frame: 40,
            bpm: Some(128.0),
            clip_bank: bank,
            source: PathBuf::from("/v.mov"),
            crop: None,
        }
    }

    fn baked(path: &str) -> BakedClip {
        BakedClip {
            path: path.to_string(),
            source_path: "/v.mov".to_string(),
            in_sec: 0.333,
            out_sec: 1.333,
            fps: 30.0,
            frames: 30,
            duration_sec: 1.0,
        }
    }

    #[test]
    fn a_clip_carries_the_provenance_a_reopen_needs() {
        let spans = [span("cut", 0)];
        let p = assemble(
            &spans,
            &[baked("clips/00_cut_10-40.mov")],
            &["clips".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
            false,
        );
        let prov = p.clips[0].source.as_ref().expect("provenance");
        assert_eq!(prov.in_frame, 10);
        assert_eq!(prov.out_frame, 40);
        assert_eq!(p.clips[0].path, "clips/00_cut_10-40.mov");
        assert_eq!(p.clips[0].bpm, Some(128.0));
    }

    /// The round trip the whole retrim feature rests on: what `assemble` writes
    /// is what `ReopenedProject::from_project` reads back.
    #[test]
    fn an_assembled_project_reopens_into_the_spans_it_came_from() {
        let spans = [span("one", 0), span("two", 1)];
        let p = assemble(
            &spans,
            &[baked("clips/a.mov"), baked("clips/b.mov")],
            &["first".to_string(), "second".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
            true,
        );
        let re = crate::editor::ReopenedProject::from_project(&p, "p").expect("reopen");
        assert_eq!(re.spans.len(), 2);
        assert_eq!(re.spans[0].in_frame, 10);
        assert_eq!(re.spans[1].name, "two");
        assert_eq!(
            re.bank_names,
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn spans_group_into_their_banks_in_index_order() {
        let spans = [span("a", 1), span("b", 0), span("c", 1)];
        let p = assemble(
            &spans,
            &[baked("a.mov"), baked("b.mov"), baked("c.mov")],
            &["zero".to_string(), "one".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
            false,
        );
        assert_eq!(p.clip_banks.len(), 2);
        assert_eq!(p.clip_banks[0].name, "zero");
        assert_eq!(p.clip_banks[0].clip_ids, vec![1]);
        assert_eq!(p.clip_banks[1].clip_ids, vec![0, 2]);
    }

    #[test]
    fn the_starter_cue_bank_is_one_full_length_cue_per_clip() {
        let spans = [span("a", 0), span("b", 0)];
        let p = assemble(
            &spans,
            &[baked("a.mov"), baked("b.mov")],
            &["clips".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
            true,
        );
        assert_eq!(p.cue_banks.len(), 1);
        assert_eq!(p.cue_banks[0].name, "A");
        assert_eq!(p.cue_banks[0].cues.len(), 2);
    }

    /// A span name is a caption, not a path, and people type `/` in captions.
    #[test]
    fn a_span_name_cannot_escape_the_clips_directory() {
        let s = span("../../etc/passwd", 0);
        let name = clip_file_name(0, &s);
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
        assert!(name.starts_with("00_"));
    }

    #[test]
    fn an_unnameable_span_still_gets_a_file_name() {
        assert_eq!(sanitize("///"), "___");
        assert_eq!(sanitize(""), "span");
    }

    /// The provenance an offsets project carries: which source a span came out
    /// of, and at what rate, so the player can seek it.
    fn source_ref() -> SourceRef {
        SourceRef {
            clip_name: "v.mov".to_string(),
            fps: 30.0,
            frames: 900,
            duration_sec: 30.0,
            source_path: "v.webm".to_string(),
        }
    }

    /// The rename both ends have to agree on: `/play`'s ingest bakes a dropped
    /// file to `<stem>.mov`, so an offsets project must name the clip that or
    /// the player will never match it to anything in its pool.
    #[test]
    fn a_clip_name_matches_what_play_will_have_interned() {
        assert_eq!(SourceRef::clip_name_for("bun.webm"), "bun.mov");
        assert_eq!(SourceRef::clip_name_for("bun.mp4"), "bun.mov");
        assert_eq!(SourceRef::clip_name_for("bun.mov"), "bun.mov");
        assert_eq!(SourceRef::clip_name_for("no-extension"), "no-extension.mov");
    }

    /// The whole mode in one assertion: one clip, and the spans became cues
    /// with trims rather than files.
    #[test]
    fn offsets_render_one_clip_and_a_cue_per_span() {
        let spans = [span("a", 0), span("b", 0)];
        let p = assemble_offsets(
            &spans,
            &source_ref(),
            &["cuts".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
        );
        assert_eq!(p.clips.len(), 1, "one clip, whatever the span count");
        assert_eq!(p.clips[0].path, "v.mov");
        assert_eq!(p.cue_banks.len(), 1);
        assert_eq!(p.cue_banks[0].cues.len(), 2);
        for cue in &p.cue_banks[0].cues {
            assert_eq!(cue.clip, 0, "every cue points at the one clip");
        }
    }

    /// A span's marks are frame indices; a cue's trim is seconds. Getting this
    /// conversion wrong is the failure that plays the wrong part of the video
    /// and looks like a bug in the player.
    #[test]
    fn a_spans_frames_become_a_cues_seconds() {
        let spans = [span("a", 0)]; // [10..40) at 30 fps
        let p = assemble_offsets(
            &spans,
            &source_ref(),
            &["cuts".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
        );
        let cue = &p.cue_banks[0].cues[0];
        assert!((cue.in_sec - 10.0 / 30.0).abs() < 1e-9);
        assert!((cue.out_sec.expect("trimmed") - 40.0 / 30.0).abs() < 1e-9);
    }

    /// In this mode a span is a cue, so the grouping the visitor made has to
    /// come out as *cue* banks — the clip banks have one entry and nothing to
    /// group.
    #[test]
    fn clip_banks_become_cue_banks() {
        let spans = [span("a", 1), span("b", 0), span("c", 1)];
        let p = assemble_offsets(
            &spans,
            &source_ref(),
            &["zero".to_string(), "one".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
        );
        assert_eq!(p.clip_banks.len(), 1);
        assert_eq!(p.cue_banks.len(), 2);
        assert_eq!(p.cue_banks[0].name, "zero");
        assert_eq!(p.cue_banks[0].cues.len(), 1);
        assert_eq!(p.cue_banks[1].name, "one");
        assert_eq!(p.cue_banks[1].cues.len(), 2);
    }

    /// An offsets project must still reopen in chop, or the round trip that
    /// makes it worth having only goes one way. It reconstructs one span for
    /// the whole source, not the cues — the cues are the player's now.
    #[test]
    fn an_offsets_project_still_reopens() {
        let spans = [span("a", 0)];
        let p = assemble_offsets(
            &spans,
            &source_ref(),
            &["cuts".to_string()],
            SessionDefaults::default(),
            vidiotic_ctl::ControlMap::default(),
        );
        let re = crate::editor::ReopenedProject::from_project(&p, "p").expect("reopen");
        assert_eq!(re.spans.len(), 1);
        assert_eq!(re.spans[0].out_frame, 900);
    }
}
