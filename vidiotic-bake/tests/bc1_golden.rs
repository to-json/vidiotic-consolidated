//! BC1 output determinism.
//!
//! Purpose (web-port.md §3c): the browser build compresses with the same
//! texpresso call as the native baker, and a bake must produce the same bytes
//! wherever it runs — otherwise a clip baked in `/chop` and one baked natively
//! are silently different files. §3c measured wasm *throughput* but never
//! checked that wasm *output* matches native. These hashes are that check's
//! native half; the wasm half compares against the same constants once
//! `vidiotic-bake` crosses the wasm gate (`scripts/wasm-gate.sh`).
//!
//! Input is generated procedurally rather than loaded, so both targets can
//! build the identical bytes without shipping a fixture.

use texpresso::{Algorithm, Format, Params};

// Under wasm32 the built-in test harness does not exist; aliasing the attribute
// lets these same tests run unmodified under `wasm-bindgen-test` (web-port.md
// §7a). Nothing else about the tests changes, which is the point — the wasm run
// must exercise the same assertions, not a parallel copy of them.
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test as test;

/// FNV-1a, matching `hap_conformance.rs`.
fn hash(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325, |h, &x| {
        (h ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The web tier from web-port.md §3a. Both dimensions divisible by 4.
const W: usize = 848;
const H: usize = 480;

/// Deterministic pseudo-video: smooth gradients (which stress endpoint
/// selection), a hard diagonal edge (which stresses block partitioning), and a
/// reproducible noise term (which prevents the whole frame collapsing into the
/// degenerate low-colour case described in §3c's benchmark trap).
fn synthetic_frame() -> Vec<u8> {
    let mut px = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            // xorshift-ish, pure integer, identical on every target.
            let mut n = (y * W + x) as u32;
            n ^= n << 13;
            n ^= n >> 17;
            n ^= n << 5;
            let noise = (n & 0x1f) as u8;
            let edge = u8::from(x * H > y * W) * 40;
            px.extend_from_slice(&[
                ((x * 255 / W) as u8).saturating_add(noise),
                ((y * 255 / H) as u8).saturating_add(edge),
                (((x ^ y) & 0xff) as u8).wrapping_add(noise),
                255,
            ]);
        }
    }
    px
}

fn compress(algorithm: Algorithm) -> Vec<u8> {
    let rgba = synthetic_frame();
    let mut out = vec![0u8; Format::Bc1.compressed_size(W, H)];
    Format::Bc1.compress(&rgba, W, H, Params { algorithm, ..Params::default() }, &mut out);
    out
}

/// If this fails, the generator changed and every golden below is invalid.
/// Checked separately so a generator change is distinguishable from a
/// texpresso behaviour change.
#[test]
fn synthetic_input_is_stable() {
    let f = synthetic_frame();
    assert_eq!(f.len(), W * H * 4);
    assert_eq!(hash(&f), 0x87cc_e6aa_9473_9450, "synthetic frame generator changed");
}

#[test]
fn bc1_output_size_is_half_a_byte_per_pixel() {
    // BC1 is 8 bytes per 4x4 block. This is the invariant `/play` relies on to
    // size its GPU uploads, and it is independent of the hashes below.
    assert_eq!(Format::Bc1.compressed_size(W, H), (W / 4) * (H / 4) * 8);
    assert_eq!(Format::Bc1.compressed_size(W, H), W * H / 2);
}

#[test]
fn rangefit_output_is_deterministic() {
    // BakeQuality::Draft — the web default per §3c.
    assert_eq!(hash(&compress(Algorithm::RangeFit)), 0x9898_27ec_2950_51a0);
}

#[test]
fn clusterfit_output_is_deterministic() {
    // BakeQuality::High — opt-in only on the web; still must be reproducible.
    assert_eq!(hash(&compress(Algorithm::ClusterFit)), 0xaf41_fbe9_b595_b2c1);
}

/// Compressing twice must give the same answer — rules out uninitialised
/// scratch or iteration-order dependence leaking into output, which is what
/// would break reproducibility under a different thread count.
#[test]
fn compression_is_idempotent() {
    for (name, algorithm) in [("RangeFit", Algorithm::RangeFit), ("ClusterFit", Algorithm::ClusterFit)] {
        assert_eq!(compress(algorithm), compress(algorithm), "{name} not reproducible");
    }
}
