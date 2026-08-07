//! Manual timing harness for span bakes: `cargo test --test bake_timing --release -- --nocapture --ignored`.
//! Reads the source path from `BAKE_SRC` (defaults to the repo test clip).

// Drives the ffmpeg transcode/demux path; nothing here is portable.
#![cfg(feature = "ffmpeg")]

use std::time::Instant;

#[test]
#[ignore = "manual timing harness, needs a local video"]
fn bake_timing() {
    let _ = env_logger::try_init();
    let src = std::env::var("BAKE_SRC")
        .unwrap_or_else(|_| "/Users/j/code/loot/vidiotic/clips/bun.mov".to_string());
    let out = std::env::temp_dir().join("bake_timing_out.mov");
    for quality in [
        vidiotic_bake::transcode::BakeQuality::Draft,
        vidiotic_bake::transcode::BakeQuality::High,
    ] {
        let t = Instant::now();
        let report =
            vidiotic_bake::transcode::run_span_with(src.as_ref(), &out, 0.3, Some(2.3), quality, |_| {})
                .expect("bake failed");
        let dt = t.elapsed().as_secs_f64();
        println!(
            "{quality:?}: baked {} frames ({}x{}) in {dt:.2}s = {:.1} frames/s",
            report.frames,
            report.width,
            report.height,
            report.frames as f64 / dt
        );
    }
    let _ = std::fs::remove_file(&out);
}
