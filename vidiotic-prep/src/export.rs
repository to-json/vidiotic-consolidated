//! Bake selected spans to HAP `.mov` clips and write the `.viproj` that
//! references them. Runs on a worker thread — transcoding is CPU-heavy — and
//! streams progress back over a channel so the UI thread never blocks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossbeam_channel::Sender;

use vidiotic_core::project::{self, SessionDefaults};
use vidiotic_bake::transcode::BakeQuality;

use vidiotic_chop::export::{assemble, clip_file_name, BakedClip};
use vidiotic_chop::spans::Span;

/// Live export position: which span is baking and how far through it is.
#[derive(Clone, Default)]
pub struct ExportProgress {
    /// Spans fully baked.
    pub done: usize,
    pub total: usize,
    /// Name of the span currently baking (empty once finished).
    pub current: String,
    /// Frames emitted so far for the current span.
    pub cur_done: u64,
    /// Frames the current span is expected to emit.
    pub cur_total: u64,
    /// Source seconds the decoder has reached (advances even while skipping
    /// pre-in frames — movement here with `cur_done` stuck at 0 means the
    /// decoder is still seeking toward the in-point, or pts don't line up).
    pub src_sec: f64,
    /// Encode throughput for the current span, frames/sec.
    pub enc_fps: f64,
}

/// Messages streamed from the export worker to the UI.
pub enum ExportMsg {
    Progress(ExportProgress),
    Done(PathBuf),
    Error(String),
}

/// Spawn the export worker: transcode every span to `dest_dir/clips/`
/// (reading each from its own `span.source`, reopened by path — the video
/// doesn't need to still be open in the app), then write
/// `dest_dir/<project_name>.viproj`. `bank_names[span.clip_bank]` names the
/// clip bank each span's baked clip is grouped into. If `starter_cue_bank`,
/// a cue bank named "A" is added with one full-length cue per clip.
#[allow(clippy::too_many_arguments)]
pub fn spawn_export(
    spans: Vec<Span>,
    bank_names: Vec<String>,
    defaults: SessionDefaults,
    controls: vidiotic_ctl::ControlMap,
    dest_dir: PathBuf,
    project_name: String,
    starter_cue_bank: bool,
    quality: BakeQuality,
    tx: Sender<ExportMsg>,
) {
    std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            if let Err(e) = run(
                &spans,
                &bank_names,
                defaults,
                controls,
                &dest_dir,
                &project_name,
                starter_cue_bank,
                quality,
                &tx,
            ) {
                let _ = tx.send(ExportMsg::Error(format!("{e:#}")));
            }
        })
        .ok();
}

/// Probe a source's own average frame rate, just enough to convert its
/// spans' frame-index marks into seconds before handing off to
/// `run_span_with` (which does its own equivalent probe internally, but only
/// after it's already been told what range to decode). Spans from different
/// source videos can have different frame rates, so this must be done
/// per-source rather than reusing one shared fps across the whole export.
fn probe_fps(path: &Path) -> anyhow::Result<f64> {
    let ictx = ffmpeg_next::format::input(path)?;
    let st = ictx
        .streams()
        .best(ffmpeg_next::media::Type::Video)
        .ok_or_else(|| anyhow::anyhow!("no video stream in {}", path.display()))?;
    let rate = st.avg_frame_rate();
    Ok(if rate.denominator() != 0 && rate.numerator() != 0 {
        rate.numerator() as f64 / rate.denominator() as f64
    } else {
        30.0
    })
}

// The spec literals below end in `..Default::default()` even when every field
// is set today, so additive `.viproj` fields don't break prep's build.
#[allow(clippy::too_many_arguments, clippy::needless_update)]
fn run(
    spans: &[Span],
    bank_names: &[String],
    defaults: SessionDefaults,
    controls: vidiotic_ctl::ControlMap,
    dest_dir: &Path,
    project_name: &str,
    starter_cue_bank: bool,
    quality: BakeQuality,
    tx: &Sender<ExportMsg>,
) -> anyhow::Result<()> {
    let _ = ffmpeg_next::init();
    let clips_dir = dest_dir.join("clips");
    std::fs::create_dir_all(&clips_dir)?;

    let mut fps_cache: HashMap<PathBuf, f64> = HashMap::new();
    for span in spans {
        if !fps_cache.contains_key(&span.source) {
            fps_cache.insert(span.source.clone(), probe_fps(&span.source)?);
        }
    }

    let total = spans.len();
    let total_frames: u64 = spans.iter().map(|s| s.out_frame - s.in_frame).sum();
    log::info!(
        "export: baking {total} span(s), {total_frames} frames total, {} source(s) -> {}",
        fps_cache.len(),
        dest_dir.display()
    );
    let export_started = std::time::Instant::now();
    let mut baked: Vec<BakedClip> = Vec::with_capacity(total);

    for (i, span) in spans.iter().enumerate() {
        let fps = fps_cache[&span.source];
        let source_abs =
            std::fs::canonicalize(&span.source).unwrap_or_else(|_| span.source.clone());
        let in_sec = span.in_frame as f64 / fps;
        let out_sec = span.out_frame as f64 / fps;
        let expected = span.out_frame - span.in_frame;
        // Index prefix keeps identically-named spans from clobbering each other
        // and makes clips/ sort in span order.
        let file_name = clip_file_name(i, span);
        let out_path = clips_dir.join(&file_name);

        log::info!(
            "export: span {}/{total} \"{}\" [{}..{}) = {expected} frames",
            i + 1,
            span.name,
            span.in_frame,
            span.out_frame
        );
        let _ = tx.send(ExportMsg::Progress(ExportProgress {
            done: i,
            total,
            current: span.name.clone(),
            cur_done: 0,
            cur_total: expected,
            src_sec: 0.0,
            enc_fps: 0.0,
        }));

        let span_started = std::time::Instant::now();
        let mut last_sent = std::time::Instant::now();
        let report = vidiotic_bake::transcode::run_span_with(
            &span.source,
            &out_path,
            in_sec,
            Some(out_sec),
            quality,
            |u| {
                // ~10 Hz is plenty for a progress bar; don't flood the channel.
                if last_sent.elapsed().as_secs_f64() >= 0.1 {
                    last_sent = std::time::Instant::now();
                    let _ = tx.send(ExportMsg::Progress(ExportProgress {
                        done: i,
                        total,
                        current: span.name.clone(),
                        cur_done: u.emitted,
                        cur_total: expected,
                        src_sec: u.src_sec,
                        enc_fps: u.emitted as f64
                            / span_started.elapsed().as_secs_f64().max(1e-9),
                    }));
                }
            },
        )?;
        log::info!(
            "export: span \"{}\" -> {} frames in {:.1}s",
            span.name,
            report.frames,
            span_started.elapsed().as_secs_f64()
        );
        anyhow::ensure!(
            report.frames > 0,
            "span \"{}\" produced 0 frames: the source's timestamps never reached \
             [{in_sec:.3}..{out_sec:.3})s — its pts may not match the frame index \
             (see the log for the decoded range)",
            span.name
        );

        baked.push(BakedClip {
            path: format!("clips/{file_name}"),
            source_path: source_abs.to_string_lossy().into_owned(),
            in_sec,
            out_sec,
            fps: report.fps,
            frames: report.frames,
            duration_sec: report.duration_sec,
        });
    }

    // The `.viproj` itself is assembled by `vidiotic-chop`, which is also what
    // the browser exporter calls. The two do not agree about the format by
    // being written carefully against one spec — they agree because this is one
    // function (web-port.md §3e).
    let proj = assemble(
        spans,
        &baked,
        bank_names,
        defaults,
        controls,
        starter_cue_bank,
    );

    let _ = tx.send(ExportMsg::Progress(ExportProgress {
        done: total,
        total,
        ..ExportProgress::default()
    }));

    let proj_path = dest_dir.join(format!("{project_name}.viproj"));
    project::save(&proj, &proj_path)?;
    log::info!(
        "export: wrote {} ({total} clips, {total_frames} frames) in {:.1}s",
        proj_path.display(),
        export_started.elapsed().as_secs_f64()
    );
    let _ = tx.send(ExportMsg::Done(proj_path));
    Ok(())
}
