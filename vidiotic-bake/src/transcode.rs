//! Self-contained HAP transcoder: decode any clip with ffmpeg-next, block-
//! compress each frame to DXT1 (texpresso), wrap as a Snappy HAP1 frame, and mux
//! into a `QuickTime` `.mov`. This exists because a stock Homebrew ffmpeg is built
//! without libsnappy and therefore has no `-c:v hap` encoder — so the app ships
//! its own, and the resulting clips play back on the near-zero-CPU HAP path.
//!
//! [`run_span`] additionally bakes a frame-accurate sub-range of the source,
//! which the `vidiotic-prep` authoring tool uses to export selected spans as
//! standalone clips.
//!
//! ffmpeg's role here is now **decoding only**. The container is written by
//! [`crate::mov`] (web-port.md §8 step 2), which removed two problems along with
//! the dependency:
//!
//! - libavformat's mov muxer **dropped the last packet** of the stream, because
//!   the bake hands it packets with no explicit duration and there is no
//!   following packet to derive one from. Every clip ever baked was one frame
//!   short of what the bake reported. `tests/bake_integrity.rs` is the check.
//! - it also overrode the requested timescale (16000 was what it picked), which
//!   is why this module used to carry a `rescale` helper to convert its own
//!   timestamps into whatever the muxer had decided on.
//!
//! # Timing
//!
//! Two defects in the output timeline were fixed together, because both change
//! the timestamps of every baked file and one re-bake is better than two.
//!
//! - **The frame rate was read from the wrong field.** `avg_frame_rate` is
//!   `frames / duration`, so it inherits any error in the declared duration —
//!   including the one the old muxer introduced. Re-baking a clip that muxer
//!   wrote produced a file playing 1.1% fast. [`pick_fps`] prefers
//!   `r_frame_rate`, with a guard for the case where *that* is the unreliable
//!   one, and warns when they disagree.
//! - **The timeline was milliseconds**, so a 30 fps bake got durations
//!   alternating 33/34 ms: the average was right but no individual frame was.
//!   The timescale is now derived from the rate ([`timeline`]), making every
//!   duration identical and the declared rate exact.
//!
//! Files baked before this differ in their timestamps. The pixels are
//! unaffected — the BC1 payloads are byte-identical, since nothing here touches
//! [`crate::frame`].

use std::path::Path;

use ffmpeg_next as ff;

use crate::frame::{align4, FrameBaker};
use crate::mov::MovWriter;

/// Re-exported so `transcode::BakeQuality` keeps meaning what it always has.
/// The type moved to [`crate::frame`] when the per-frame bake was split out of
/// this module for the browser build — nothing about it was ffmpeg's.
pub use crate::frame::BakeQuality;

/// Units per frame on the output timeline.
///
/// The timescale is derived per-bake as `round(fps * FRAME_UNITS)`, so every
/// sample's duration is exactly `FRAME_UNITS` and the declared frame rate is
/// exact rather than approximated — see [`timeline`]. 1000 is arbitrary beyond
/// giving the timescale three digits of headroom for rates like 29.97.
const FRAME_UNITS: u32 = 1000;

/// Frame rate used when the source declares nothing usable.
const DEFAULT_FPS: f64 = 30.0;

/// Above this, a declared frame rate is treated as a timestamp artifact rather
/// than a capture rate.
///
/// `r_frame_rate` is the lowest rate that can represent every timestamp in the
/// stream exactly, so any jitter in the source's durations inflates it — a file
/// on a millisecond timeline with alternating 33/34 ms frames reports 1000 fps.
/// Real footage does not exceed a few hundred.
const MAX_PLAUSIBLE_FPS: f64 = 240.0;

/// Convert a stream rational to a rate, rejecting the degenerate forms.
fn rate_of(r: ff::Rational) -> Option<f64> {
    (r.numerator() > 0 && r.denominator() > 0)
        .then(|| f64::from(r.numerator()) / f64::from(r.denominator()))
}

/// Choose the source frame rate, preferring `r_frame_rate` over
/// `avg_frame_rate`, and say so when the two disagree.
///
/// **This is a bug fix with a specific history.** The bake used to take
/// `avg_frame_rate` unconditionally, which is `frames / duration` — and so it
/// inherits any error in the declared duration. Every clip written by the old
/// libavformat path carries a zero-duration final sample (see the module docs
/// and web-port.md's Observations), which makes its duration one frame short
/// and its `avg_frame_rate` correspondingly high: `clips/bun.mov` declares
/// `r_frame_rate = 30/1` and `avg_frame_rate = 30000/989 = 30.334`. Re-baking
/// one of those clips therefore produced a file that played **1.1% fast**, and
/// re-baking is exactly what someone does to pick up the muxer fix.
///
/// `r_frame_rate` is not simply better, though, which is why this is a choice
/// and not a swap: it is derived from timestamp *spacing*, so a jittery
/// timeline inflates it — our own pre-fix output reports 1000 fps. Hence the
/// plausibility guard.
///
/// The two disagreeing is worth surfacing either way. In the known-bad case
/// `avg` comes out *higher* than `r`, because a short duration inflates it;
/// a genuinely variable-rate source tends the other way.
fn pick_fps(r: Option<f64>, avg: Option<f64>) -> (f64, Option<String>) {
    let plausible = |f: Option<f64>| f.filter(|v| *v > 0.0 && *v <= MAX_PLAUSIBLE_FPS);
    let (r_ok, avg_ok) = (plausible(r), plausible(avg));

    // Only worth mentioning when both are believable and they differ; if one is
    // implausible the guard below already explains the choice.
    let disagree = match (r_ok, avg_ok) {
        (Some(a), Some(b)) => (a - b).abs() / a > 0.005,
        _ => false,
    };

    match (r_ok, avg_ok) {
        (Some(f), Some(other)) if disagree => (
            f,
            Some(format!(
                "source declares r_frame_rate {f:.4} but avg_frame_rate {other:.4}; \
                 using {f:.4}. A higher avg usually means the source has a \
                 zero-duration final frame — re-bake it and the discrepancy goes away"
            )),
        ),
        (Some(f), _) => (f, None),
        (None, Some(f)) => (
            f,
            r.map(|bad| {
                format!(
                    "ignoring implausible r_frame_rate {bad:.1} (timestamp jitter, \
                     not a capture rate); using avg_frame_rate {f:.4}"
                )
            }),
        ),
        (None, None) => (
            DEFAULT_FPS,
            Some(format!(
                "source declares no usable frame rate (r={r:?}, avg={avg:?}); \
                 assuming {DEFAULT_FPS}"
            )),
        ),
    }
}

/// The output timeline for a given frame rate: `(timescale, frame_duration)`.
///
/// Picking the timescale from the rate instead of fixing it at 1000 is what
/// makes frame timing *exact*. On a millisecond timeline a 30 fps source gets
/// durations alternating 33/34 ms — the average is right, but no duration is,
/// and the declared rate is an approximation. At `timescale = 30000` with every
/// duration `1000`, 30 fps is 30 fps. 29.97 becomes `29970/1000`, also exact.
fn timeline(fps: f64) -> (u32, u32) {
    let ts = (fps * f64::from(FRAME_UNITS)).round();
    let ts = if ts.is_finite() && ts >= 1.0 {
        ts.min(f64::from(u32::MAX)) as u32
    } else {
        DEFAULT_FPS as u32 * FRAME_UNITS
    };
    (ts, FRAME_UNITS)
}

/// What a transcode produced: baked dimensions (after the 4-px alignment crop),
/// frame rate, emitted frame count, and duration. `vidiotic-prep` records these
/// as clip metadata.
#[derive(Clone, Copy, Debug)]
pub struct TranscodeReport {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: u64,
    pub duration_sec: f64,
}

/// Transcode `input` (any decodable video) to a HAP1 `.mov` at `output`.
///
/// # Errors
/// Propagates ffmpeg initialization, demux/decode, and mux/write failures.
pub fn run(input: &Path, output: &Path) -> anyhow::Result<()> {
    run_span(input, output, 0.0, None).map(|_| ())
}

/// [`run_span_with`] without progress reporting, at [`BakeQuality::High`]
/// (the pre-existing quality of whole-file transcodes).
///
/// # Errors
/// See [`run_span_with`].
pub fn run_span(
    input: &Path,
    output: &Path,
    in_sec: f64,
    out_sec: Option<f64>,
) -> anyhow::Result<TranscodeReport> {
    run_span_with(input, output, in_sec, out_sec, BakeQuality::High, |_| {})
}

/// Live position of an in-flight [`run_span_with`] bake, reported once per
/// decoded frame. `src_sec` advances even while pre-in frames are being
/// skipped, so a caller can distinguish "decoding toward the in-point" (or a
/// pts mismatch) from a stall.
#[derive(Clone, Copy, Debug)]
pub struct BakeUpdate {
    /// Frames emitted to the output so far.
    pub emitted: u64,
    /// Source timestamp of the frame just decoded.
    pub src_sec: f64,
}

/// Transcode the `[in_sec, out_sec)` span of `input` to a HAP1 `.mov`,
/// invoking `progress` once per decoded frame.
///
/// The demuxer seeks to the keyframe at or before `in_sec`; frames whose source
/// pts precede `in_sec` are decoded (for inter-frame correctness) but not
/// emitted, and decoding stops once a frame's pts reaches `out_sec`. Output pts
/// is re-baselined so the file always begins at t=0. Pass `in_sec = 0.0` and
/// `out_sec = None` for a whole-file transcode.
///
/// # Errors
/// Propagates ffmpeg initialization, seek, demux/decode, and mux/write failures.
pub fn run_span_with(
    input: &Path,
    output: &Path,
    in_sec: f64,
    out_sec: Option<f64>,
    quality: BakeQuality,
    progress: impl FnMut(BakeUpdate),
) -> anyhow::Result<TranscodeReport> {
    run_span_cropped_with(input, output, in_sec, out_sec, None, quality, progress)
}

/// Transcode with an optional normalized crop box rect.
///
/// # Errors
/// See [`run_span_with`].
pub fn run_span_cropped_with(
    input: &Path,
    output: &Path,
    in_sec: f64,
    out_sec: Option<f64>,
    crop: Option<crate::frame::CropRect>,
    quality: BakeQuality,
    mut progress: impl FnMut(BakeUpdate),
) -> anyhow::Result<TranscodeReport> {
    ff::init()?;
    let started = std::time::Instant::now();

    let mut ictx = ff::format::input(input)?;
    let (vid_idx, params, fps, in_tb) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| anyhow::anyhow!("no video stream in {}", input.display()))?;
        let (fps, note) = pick_fps(rate_of(st.rate()), rate_of(st.avg_frame_rate()));
        if let Some(note) = note {
            log::warn!("{}: {note}", input.display());
        }
        // Stream time base, for turning a decoded frame's pts into seconds.
        let tb = st.time_base();
        let in_tb = if tb.denominator() != 0 {
            tb.numerator() as f64 / tb.denominator() as f64
        } else {
            0.0
        };
        (st.index(), st.parameters(), fps, in_tb)
    };

    let mut dec_ctx = ff::codec::context::Context::from_parameters(params)?;
    // Frame-threaded decoding (count 0 = auto): the source codec (h264 etc.) is
    // often the bake bottleneck, not BC1.
    dec_ctx.set_threading(ff::codec::threading::Config::kind(
        ff::codec::threading::Type::Frame,
    ));
    let mut decoder = dec_ctx.decoder().video()?;
    let (sw, sh) = (decoder.width(), decoder.height());
    let (px_x, px_y, target_w, target_h) = match crop {
        Some(c) => c.to_pixel_rect(sw, sh),
        None => (0, 0, sw, sh),
    };
    let (w, h) = align4(target_w, target_h);
    anyhow::ensure!(
        w >= 4 && h >= 4,
        "video/crop too small to transcode: {target_w}x{target_h}"
    );
    let mut baker = FrameBaker::new(w, h, quality)?;
    let mut scaler = ff::software::scaling::Context::get(
        decoder.format(),
        sw,
        sh,
        ff::format::Pixel::RGBA,
        sw,
        sh,
        ff::software::scaling::Flags::BILINEAR,
    )?;

    // --- output ---
    // A timeline whose unit is a fixed fraction of a frame, so every duration
    // is the same integer and none of them is a rounding of one.
    let (timescale, frame_units) = timeline(fps);
    let mut octx = MovWriter::new(
        std::io::BufWriter::new(std::fs::File::create(output)?),
        w,
        h,
        timescale,
        frame_units,
    )?;

    // Seek to (or just before) the in-point; flush so no pre-seek frames leak.
    if in_sec > 0.0 {
        seek_secs(&mut ictx, in_sec)?;
        decoder.flush();
    }

    let mut packed = vec![0u8; baker.frame_bytes()];

    let mut decoded = ff::frame::Video::empty();
    let mut idx: i64 = 0; // count of *emitted* frames — the re-baselined pts index
    let mut skipped: u64 = 0; // decoded-but-dropped pre-in frames
    let mut stages = StageTimes::default();

    // Returns Ok(true) once a frame at/after out_sec is seen (stop the demux).
    let mut process = |decoder: &mut ff::decoder::Video,
                       scaler: &mut ff::software::scaling::Context,
                       octx: &mut MovWriter<std::io::BufWriter<std::fs::File>>,
                       stages: &mut StageTimes|
     -> anyhow::Result<bool> {
        loop {
            let t0 = std::time::Instant::now();
            let got = decoder.receive_frame(&mut decoded).is_ok();
            stages.decode += t0.elapsed();
            if !got {
                return Ok(false);
            }
            let src_sec = decoded.pts().unwrap_or(0) as f64 * in_tb;
            progress(BakeUpdate {
                emitted: idx as u64,
                src_sec,
            });
            // Seek lands on a keyframe ≤ in_sec; skip anything before the in-point.
            if src_sec + 1e-6 < in_sec {
                skipped += 1;
                continue;
            }
            // Reached the out-point: nothing more to emit.
            if out_sec.is_some_and(|o| src_sec >= o) {
                return Ok(true);
            }

            let t0 = std::time::Instant::now();
            let mut rgba = ff::frame::Video::empty();
            scaler.run(&decoded, &mut rgba)?;

            // Repack cropped rows to a tight width*4 stride for texpresso.
            let src = rgba.data(0);
            let stride = rgba.stride(0);
            let row = (w * 4) as usize;
            for y in 0..h as usize {
                let src_y = (px_y as usize) + y;
                let src_offset = src_y * stride + (px_x as usize * 4);
                packed[y * row..(y + 1) * row].copy_from_slice(&src[src_offset..src_offset + row]);
            }
            stages.scale += t0.elapsed();

            let t0 = std::time::Instant::now();
            let bc1 = baker.compress(&packed)?;
            stages.bc1 += t0.elapsed();

            let t0 = std::time::Instant::now();
            let hap_frame = crate::hap::encode_hap1_frame(bc1);
            // Re-baselined pts. Exact by construction — a whole number of frame
            // units, with no division and therefore nothing to round. No
            // rescale either: the timescale asked for is the timescale written.
            let pts = idx as u32 * frame_units;
            octx.write_sample(&hap_frame, pts)?;
            stages.mux += t0.elapsed();
            idx += 1;
        }
    };

    let mut reached_out = false;
    for (stream, packet) in ictx.packets() {
        if stream.index() != vid_idx {
            continue;
        }
        let t0 = std::time::Instant::now();
        decoder.send_packet(&packet)?;
        stages.decode += t0.elapsed();
        if process(&mut decoder, &mut scaler, &mut octx, &mut stages)? {
            reached_out = true;
            break;
        }
    }
    if !reached_out {
        decoder.send_eof()?;
        process(&mut decoder, &mut scaler, &mut octx, &mut stages)?;
    }
    stages.log(idx.max(1) as u32);

    let written = octx.sample_count() as u64;
    octx.finish()?;
    // The muxer this replaced silently dropped the final sample, so the count is
    // now asserted rather than assumed: `TranscodeReport::frames` is what
    // `vidiotic-prep` records as the clip's length, and a file holding fewer
    // frames than that makes every downstream timeline wrong.
    anyhow::ensure!(
        written == idx as u64,
        "muxer wrote {written} samples but {idx} frames were emitted"
    );
    let duration_sec = if fps > 0.0 { idx as f64 / fps } else { 0.0 };
    let elapsed = started.elapsed().as_secs_f64();
    log::info!(
        "transcoded {idx} frames (skipped {skipped} pre-in) -> {} (Hap1, {w}x{h}, {fps:.2} fps) in {elapsed:.1}s = {:.1} enc f/s",
        output.display(),
        idx as f64 / elapsed.max(1e-9),
    );
    if idx == 0 {
        log::warn!(
            "bake emitted 0 frames: source pts never reached [{in_sec:.3}..{:?})s — \
             check the source's timestamps against the requested span",
            out_sec
        );
    }
    Ok(TranscodeReport {
        width: w,
        height: h,
        fps,
        frames: idx as u64,
        duration_sec,
    })
}

/// Wall-clock spent per bake stage, for the debug-level breakdown log.
#[derive(Default)]
struct StageTimes {
    decode: std::time::Duration,
    scale: std::time::Duration,
    bc1: std::time::Duration,
    mux: std::time::Duration,
}

impl StageTimes {
    fn log(&self, frames: u32) {
        log::debug!(
            "bake stages (ms/frame over {frames}): decode {:.1}, scale+pack {:.1}, bc1 {:.1}, hap+mux {:.1}",
            self.decode.as_secs_f64() * 1000.0 / f64::from(frames),
            self.scale.as_secs_f64() * 1000.0 / f64::from(frames),
            self.bc1.as_secs_f64() * 1000.0 / f64::from(frames),
            self.mux.as_secs_f64() * 1000.0 / f64::from(frames),
        );
    }
}

/// Seek the demuxer to `secs` (clamped at 0), in the container's own timeline.
fn seek_secs(ictx: &mut ff::format::context::Input, secs: f64) -> anyhow::Result<()> {
    let ts = (secs.max(0.0) * 1_000_000.0) as i64; // AV_TIME_BASE microseconds
    ictx.seek(ts, ..)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression that motivated `pick_fps`, with the real numbers from
    /// `clips/bun.mov`: `r_frame_rate = 30/1`, `avg_frame_rate = 30000/989`.
    /// Taking `avg` here is what made a re-bake play 1.1% fast.
    #[test]
    fn a_clip_with_a_zero_duration_tail_is_baked_at_its_true_rate() {
        let (fps, note) = pick_fps(Some(30.0), Some(30000.0 / 989.0));
        assert!(
            (fps - 30.0).abs() < 1e-9,
            "took the inflated average: {fps}"
        );
        assert!(note.is_some(), "a 1.1% disagreement should be reported");
    }

    /// The other half of the trap: our own pre-fix output. Millisecond
    /// durations of 33/34 make ffmpeg report `r_frame_rate = 1000/1`, so a
    /// naive "always prefer r" would be worse than what it replaced.
    #[test]
    fn an_inflated_r_frame_rate_is_rejected_in_favour_of_the_average() {
        let (fps, note) = pick_fps(Some(1000.0), Some(30000.0 / 989.0));
        assert!((fps - 30000.0 / 989.0).abs() < 1e-9, "kept 1000 fps: {fps}");
        assert!(
            note.is_some(),
            "silently ignoring a declared rate is worth saying"
        );
    }

    #[test]
    fn a_well_formed_source_produces_no_warning() {
        // The common case: CFR, both fields agree. Also NTSC, where the rate is
        // not an integer and the two fields still match exactly.
        for rate in [30.0, 24.0, 60.0, 30000.0 / 1001.0] {
            let (fps, note) = pick_fps(Some(rate), Some(rate));
            assert!((fps - rate).abs() < 1e-9);
            assert!(note.is_none(), "spurious warning for {rate}");
        }
    }

    #[test]
    fn a_source_declaring_nothing_falls_back_rather_than_dividing_by_zero() {
        let (fps, note) = pick_fps(None, None);
        assert!((fps - DEFAULT_FPS).abs() < 1e-9);
        assert!(note.is_some());

        // One field absent is not an error, just less to go on.
        assert_eq!(pick_fps(Some(25.0), None).0, 25.0);
        assert_eq!(pick_fps(None, Some(25.0)).0, 25.0);
    }

    #[test]
    fn the_timeline_makes_every_frame_duration_exact() {
        // The point of deriving the timescale: timescale / frame_units is the
        // frame rate, with no remainder. On the old fixed 1000 timeline, 30 fps
        // meant durations of 33 and 34 and a rate of 30.30.
        for rate in [30.0, 24.0, 25.0, 60.0, 29.97] {
            let (ts, units) = timeline(rate);
            let declared = f64::from(ts) / f64::from(units);
            assert!(
                (declared - rate).abs() < 1e-9,
                "{rate} fps declared as {declared} (timescale {ts}, units {units})"
            );
        }
    }

    #[test]
    fn a_degenerate_rate_cannot_produce_an_unusable_timescale() {
        // A timescale of 0 would make the file undecodable, so the guard matters
        // more than the value it picks.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let (ts, units) = timeline(bad);
            assert!(ts >= 1, "timescale {ts} from fps {bad}");
            assert!(units >= 1);
        }
    }
}
