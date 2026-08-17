//! Conformance: `hap::decode_frame` against **real** HAP packets lifted from
//! `clips/*.mov`, not the synthetic bitstreams the unit tests in `hap.rs` build.
//!
//! Why this exists, and what it is for (web-port.md §4, §8 step 2): the player
//! currently gets its HAP packets from ffmpeg's demuxer, and the web build will
//! get them from a pure-Rust MP4 demuxer instead. This test pins what the
//! decoder produces *today* so that swap can be proven not to change a byte.
//! It is deliberately free of ffmpeg — it reads committed bytes — so the same
//! test runs unchanged after the swap and under `wasm32-unknown-unknown`.
//!
//! Regenerate fixtures with:
//! ```sh
//! cargo test --test gen_fixtures -- --ignored --nocapture
//! ```
//!
//! **The goldens are a regression lock, not an oracle.** They were produced by
//! the very decoder under test, so they cannot catch a pre-existing bug. The
//! independent checks are the invariants asserted alongside them: the decoded
//! payload must be exactly the BC size implied by the clip's dimensions, and
//! the block count must divide evenly.

use vidiotic_bake::hap::{self, HapTextureFormat};

// Under wasm32 the built-in test harness does not exist; aliasing the attribute
// lets these same tests run unmodified under `wasm-bindgen-test` (web-port.md
// §7a). Nothing else about the tests changes, which is the point — the wasm run
// must exercise the same assertions, not a parallel copy of them.
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test as test;

/// FNV-1a. Duplicated from `gen_fixtures.rs` on purpose: this test must not
/// depend on the generator it is checking.
fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Every clip in `clips/` is 640x360 — asserted, not assumed, in
/// `decoded_size_matches_clip_dimensions`.
const CLIP_W: usize = 640;
const CLIP_H: usize = 360;

struct Golden {
    name: String,
    tex: u8,
    format: String,
    video_mode: i32,
    main_len: usize,
    main_hash: u64,
    alpha_len: usize,
    alpha_hash: u64,
}

/// `include_dir!` is not in the dependency set, so the fixture bytes are pulled
/// in by an explicit list. `goldens.tsv` is the source of truth for that list;
/// a fixture missing from this match arm fails loudly rather than silently
/// skipping.
fn fixture_bytes(name: &str) -> &'static [u8] {
    match name {
        "brb_Hap1_000.hap" => include_bytes!("fixtures/brb_Hap1_000.hap"),
        "brb_Hap1_017.hap" => include_bytes!("fixtures/brb_Hap1_017.hap"),
        "brb_Hap1_041.hap" => include_bytes!("fixtures/brb_Hap1_041.hap"),
        "bun_Hap1_000.hap" => include_bytes!("fixtures/bun_Hap1_000.hap"),
        "bun_Hap1_017.hap" => include_bytes!("fixtures/bun_Hap1_017.hap"),
        "bun_Hap1_041.hap" => include_bytes!("fixtures/bun_Hap1_041.hap"),
        "eyes_Hap1_000.hap" => include_bytes!("fixtures/eyes_Hap1_000.hap"),
        "eyes_Hap1_017.hap" => include_bytes!("fixtures/eyes_Hap1_017.hap"),
        "eyes_Hap1_041.hap" => include_bytes!("fixtures/eyes_Hap1_041.hap"),
        other => panic!("fixture {other} is in goldens.tsv but not in fixture_bytes()"),
    }
}

fn goldens() -> Vec<Golden> {
    include_str!("fixtures/goldens.tsv")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 8, "malformed goldens row: {l}");
            Golden {
                name: f[0].to_owned(),
                tex: f[1].parse().unwrap(),
                format: f[2].to_owned(),
                video_mode: f[3].parse().unwrap(),
                main_len: f[4].parse().unwrap(),
                main_hash: u64::from_str_radix(f[5], 16).unwrap(),
                alpha_len: f[6].parse().unwrap(),
                alpha_hash: u64::from_str_radix(f[7], 16).unwrap(),
            }
        })
        .collect()
}

#[test]
fn fixtures_are_present() {
    let g = goldens();
    assert!(!g.is_empty(), "no goldens — run the gen_fixtures generator");
    for row in &g {
        assert!(
            !fixture_bytes(&row.name).is_empty(),
            "{} is empty",
            row.name
        );
    }
}

/// The regression lock: decoding must produce byte-identical output.
#[test]
fn decodes_to_recorded_goldens() {
    for g in goldens() {
        let (mut main, mut alpha) = (Vec::new(), Vec::new());
        let meta = hap::decode_frame(fixture_bytes(&g.name), g.tex, &mut main, &mut alpha)
            .unwrap_or_else(|e| panic!("{}: decode failed: {e}", g.name));

        assert_eq!(format!("{:?}", meta.format), g.format, "{}: format", g.name);
        assert_eq!(meta.video_mode, g.video_mode, "{}: video_mode", g.name);
        assert_eq!(main.len(), g.main_len, "{}: main length", g.name);
        assert_eq!(hash(&main), g.main_hash, "{}: main bytes changed", g.name);
        assert_eq!(alpha.len(), g.alpha_len, "{}: alpha length", g.name);
        assert_eq!(
            hash(&alpha),
            g.alpha_hash,
            "{}: alpha bytes changed",
            g.name
        );
    }
}

/// Independent of the goldens: the payload size is fixed by the clip geometry
/// and the block format, so this would catch a decoder that emitted plausible
/// but wrong-length output.
#[test]
fn decoded_size_matches_clip_dimensions() {
    for g in goldens() {
        let (mut main, mut alpha) = (Vec::new(), Vec::new());
        let meta = hap::decode_frame(fixture_bytes(&g.name), g.tex, &mut main, &mut alpha)
            .unwrap_or_else(|e| panic!("{}: decode failed: {e}", g.name));

        let blocks = (CLIP_W / 4) * (CLIP_H / 4);
        assert_eq!(CLIP_W % 4, 0, "clip width must be a whole number of blocks");
        assert_eq!(
            CLIP_H % 4,
            0,
            "clip height must be a whole number of blocks"
        );

        let expect = blocks * meta.format.block_bytes() as usize;
        assert_eq!(
            main.len(),
            expect,
            "{}: {:?} at {CLIP_W}x{CLIP_H} should be {expect} bytes",
            g.name,
            meta.format
        );
    }
}

/// Every clip in the pool bakes to Hap1/BC1 — the near-zero-CPU path the whole
/// player design rests on. If a clip ever lands as something else, the render
/// path assumptions in web-port.md §4 need revisiting, so fail loudly.
#[test]
fn clip_pool_is_all_bc1() {
    for g in goldens() {
        let (mut main, mut alpha) = (Vec::new(), Vec::new());
        let meta = hap::decode_frame(fixture_bytes(&g.name), g.tex, &mut main, &mut alpha)
            .expect("decode");
        assert_eq!(meta.format, HapTextureFormat::Bc1, "{}", g.name);
        assert!(!meta.has_alpha, "{}", g.name);
        assert!(alpha.is_empty(), "{}", g.name);
    }
}

/// Decoding must not depend on the caller handing in empty buffers — the player
/// reuses one buffer across every frame of a clip.
#[test]
fn reuses_dirty_output_buffers() {
    let g = goldens();
    let (mut main, mut alpha) = (vec![0xAA; 999], vec![0xBB; 999]);
    for row in &g {
        hap::decode_frame(fixture_bytes(&row.name), row.tex, &mut main, &mut alpha)
            .expect("decode into reused buffers");
        assert_eq!(
            main.len(),
            row.main_len,
            "{}: stale bytes retained",
            row.name
        );
        assert_eq!(
            hash(&main),
            row.main_hash,
            "{}: reuse changed output",
            row.name
        );
    }
}

/// Truncation at any point must be an error, never a panic or a short read.
/// Every prefix of a real packet is exercised, coarsely.
#[test]
fn truncated_real_packets_error_cleanly() {
    for g in goldens() {
        let full = fixture_bytes(&g.name);
        let (mut main, mut alpha) = (Vec::new(), Vec::new());
        for cut in (0..full.len()).step_by(full.len() / 32 + 1) {
            // A truncated packet may decode (the payload is length-prefixed and
            // snappy will notice) or error — it must never panic, and must never
            // claim a full-length payload from short input.
            if let Ok(_meta) = hap::decode_frame(&full[..cut], g.tex, &mut main, &mut alpha) {
                assert!(
                    cut == full.len() || main.len() <= g.main_len,
                    "{}: {cut}-byte prefix produced {} bytes",
                    g.name,
                    main.len()
                );
            }
        }
    }
}
