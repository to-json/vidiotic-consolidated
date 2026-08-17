//! Does a bake's output file actually contain the bake?
//!
//! `TranscodeReport::frames` is what `vidiotic-prep` records as a clip's length
//! and what the player's timeline is built from. Nothing checked that the
//! container ended up holding that many frames — the count came from the encode
//! loop, and the muxer was trusted to keep them all.
//!
//! It did not. `tests/mov_roundtrip.rs` caught libavformat's mov muxer silently
//! dropping the final packet of a stream whose packets carry no explicit
//! duration, which is exactly how the bake fed it. This suite is the check on
//! the real path, against a real clip, so the claim is about the shipping bake
//! rather than about a reconstruction of it.
//!
//! `ffmpeg`-gated because it drives the whole native transcode.

#![cfg(feature = "ffmpeg")]

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;
use vidiotic_bake::transcode::{self, BakeQuality};

/// A clip from the repo's own pool. Small, real, and already in the tree.
const SOURCE: &str = "../clips/bun.mov";

fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vidiotic-bake-{name}-{}.mov", std::process::id()));
    p
}

/// Count the video packets ffmpeg finds in a file, and note its timescale.
fn demuxed(path: &Path) -> (u64, i32) {
    ff::init().unwrap();
    let mut ictx = ff::format::input(path).expect("open baked file");
    let (idx, den) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .expect("no video stream");
        (st.index(), st.time_base().denominator())
    };
    let n = ictx
        .packets()
        .filter(|(stream, _)| stream.index() == idx)
        .count();
    (n as u64, den)
}

/// The core claim: every frame the bake says it emitted is in the file.
#[test]
fn every_emitted_frame_reaches_the_file() {
    if !Path::new(SOURCE).exists() {
        // The pool is not part of the crate; skip rather than fail if a
        // checkout does not carry it.
        eprintln!("skipping: {SOURCE} not present");
        return;
    }
    let out = tmp("integrity");
    let report = transcode::run_span_with(
        Path::new(SOURCE),
        &out,
        0.0,
        Some(1.0),
        BakeQuality::Draft,
        |_| {},
    )
    .expect("bake");

    let (found, _) = demuxed(&out);
    assert!(
        report.frames > 0,
        "bake emitted nothing — test proves nothing"
    );
    assert_eq!(
        found, report.frames,
        "bake reported {} frames but the file contains {found}",
        report.frames
    );
    let _ = std::fs::remove_file(&out);
}

/// The bake chooses its own timeline. Under libavformat the muxer overrode it
/// (16000 was observed) and `transcode.rs` carried a `rescale` helper to
/// compensate. With the muxer under our control the request is honoured, so the
/// timeline in the file is the timeline the bake chose — and that timeline is
/// now derived from the frame rate rather than fixed at milliseconds, so
/// `timescale / frame_duration` reproduces the rate exactly.
#[test]
fn the_declared_timescale_is_the_one_written() {
    if !Path::new(SOURCE).exists() {
        eprintln!("skipping: {SOURCE} not present");
        return;
    }
    let out = tmp("timescale");
    let report = transcode::run_span_with(
        Path::new(SOURCE),
        &out,
        0.0,
        Some(1.0),
        BakeQuality::Draft,
        |_| {},
    )
    .expect("bake");

    let (_, den) = demuxed(&out);
    let expected = (report.fps * 1000.0).round() as i32;
    assert_eq!(den, expected, "timescale should be round(fps * 1000)");
    let _ = std::fs::remove_file(&out);
}

/// Every frame lasts exactly as long as every other, and the rate the file
/// declares is the rate the bake reported.
///
/// This is what the derived timescale buys. On the old fixed millisecond
/// timeline a 30 fps bake produced durations alternating 33 and 34 — the
/// average was right, no individual frame was, and ffmpeg reported the rate as
/// 30.30. Asserted through our own demuxer because it exposes per-sample
/// durations, which the ffmpeg packet iterator does not.
#[test]
fn every_frame_has_the_same_exact_duration() {
    if !Path::new(SOURCE).exists() {
        eprintln!("skipping: {SOURCE} not present");
        return;
    }
    let out = tmp("uniform");
    let report = transcode::run_span_with(
        Path::new(SOURCE),
        &out,
        0.0,
        Some(1.0),
        BakeQuality::Draft,
        |_| {},
    )
    .expect("bake");

    let bytes = std::fs::read(&out).unwrap();
    let track = vidiotic_bake::mov::demux(&bytes).expect("demux");
    assert_eq!(track.samples.len() as u64, report.frames);

    let first = track.samples[0].duration;
    assert!(first > 0, "a zero duration is the defect this replaced");
    for (i, s) in track.samples.iter().enumerate() {
        assert_eq!(s.duration, first, "sample {i} has a different duration");
    }
    // timescale / duration is the frame rate, with no remainder.
    let declared = f64::from(track.timescale) / f64::from(first);
    assert!(
        (declared - report.fps).abs() < 1e-6,
        "file declares {declared} fps, bake reported {}",
        report.fps
    );
    let _ = std::fs::remove_file(&out);
}

/// The source clips carry the old muxer's zero-duration final frame, which
/// inflates their `avg_frame_rate`. The bake must not inherit that.
///
/// `clips/bun.mov` declares `r_frame_rate = 30/1` and `avg_frame_rate =
/// 30000/989 = 30.334`. Taking the latter — which the bake used to do — made
/// the output play 1.1% fast, and re-baking is precisely what someone does to
/// pick up the muxer fix. So this asserts the trap stays shut on the real file
/// that springs it.
#[test]
fn a_damaged_source_rate_is_not_inherited() {
    if !Path::new(SOURCE).exists() {
        eprintln!("skipping: {SOURCE} not present");
        return;
    }
    ff::init().unwrap();
    let ictx = ff::format::input(Path::new(SOURCE)).expect("open source");
    let (r_rate, avg_rate) = {
        let st = ictx.streams().best(ff::media::Type::Video).unwrap();
        let r = st.rate();
        let a = st.avg_frame_rate();
        (
            f64::from(r.numerator()) / f64::from(r.denominator()),
            f64::from(a.numerator()) / f64::from(a.denominator()),
        )
    };
    drop(ictx);

    let out = tmp("rate");
    let report = transcode::run_span_with(
        Path::new(SOURCE),
        &out,
        0.0,
        Some(1.0),
        BakeQuality::Draft,
        |_| {},
    )
    .expect("bake");

    assert!(
        (report.fps - r_rate).abs() < 1e-6,
        "baked at {} fps; the source's true rate is {r_rate} (avg_frame_rate says {avg_rate})",
        report.fps
    );
    // If the fixture is ever replaced with a clean file the two rates agree and
    // this test still passes, but it stops proving anything — say so.
    if (r_rate - avg_rate).abs() < 1e-6 {
        eprintln!("note: {SOURCE} no longer has a skewed avg_frame_rate; this test is now vacuous");
    }
    let _ = std::fs::remove_file(&out);
}

/// A span bake must land on the span. Guards against the pts re-baselining
/// regressing when the muxer changed underneath it.
#[test]
fn a_span_bake_starts_at_zero() {
    if !Path::new(SOURCE).exists() {
        eprintln!("skipping: {SOURCE} not present");
        return;
    }
    let out = tmp("span");
    let report = transcode::run_span_with(
        Path::new(SOURCE),
        &out,
        0.5,
        Some(1.2),
        BakeQuality::Draft,
        |_| {},
    )
    .expect("bake");
    assert!(report.frames > 0, "span selected no frames");

    ff::init().unwrap();
    let mut ictx = ff::format::input(&out).expect("open");
    let idx = ictx
        .streams()
        .best(ff::media::Type::Video)
        .expect("video")
        .index();
    let first = ictx
        .packets()
        .find(|(stream, _)| stream.index() == idx)
        .and_then(|(_, p)| p.pts())
        .expect("at least one packet");
    assert_eq!(first, 0, "output should be re-baselined to t=0");
    let _ = std::fs::remove_file(&out);
}
