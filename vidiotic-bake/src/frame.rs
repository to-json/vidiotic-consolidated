//! One frame in, one Hap1 packet out — the part of the bake that has no
//! container and no operating system in it.
//!
//! This exists because the bake is really two separable jobs, and only one of
//! them is portable. Demuxing a source, scaling it, and muxing the result are
//! ffmpeg's (see [`crate::transcode`]); turning a tightly-packed RGBA frame into
//! a Hap1 packet is arithmetic, and the browser build (web-port.md §8 step 3)
//! needs exactly that half with none of the rest — its frames arrive from
//! `WebCodecs` and its output goes to a pure-Rust muxer.
//!
//! [`crate::transcode`] drives this type rather than duplicating it, so the
//! native and web bakers cannot drift: whatever bytes one produces for a frame,
//! the other produces too. `frame_bake.rs` is the test that holds that line.

use texpresso::{Format, Params};

use crate::hap;

/// BC1 encoder quality/speed trade-off. `texpresso` is rayon-parallel either
/// way; at 1080p `Draft` (`RangeFit`) block compression is ~6x faster than
/// `High` (`ClusterFit`) and the difference dominates bake time.
///
/// The web tier leans harder on this than the desktop one: web-port.md §3c
/// measured `Draft` at ~2.7x realtime single-threaded under wasm and `High` at
/// 6.3x *slower* than realtime, so `High` is opt-in there, not a default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BakeQuality {
    /// `RangeFit`: fast, slightly worse gradients. Right for iterating.
    #[default]
    Draft,
    /// `ClusterFit`: texpresso's default quality, several times slower.
    High,
}

impl BakeQuality {
    /// The texpresso parameters this quality maps to.
    #[must_use]
    pub fn params(self) -> Params {
        let algorithm = match self {
            Self::Draft => texpresso::Algorithm::RangeFit,
            Self::High => texpresso::Algorithm::ClusterFit,
        };
        Params { algorithm, ..Params::default() }
    }
}

/// A normalized crop rectangle [0.0..1.0] relative to original frame dimensions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl CropRect {
    /// Construct a normalized crop rectangle, clamping `x, y, w, h` to [0.0, 1.0].
    #[must_use]
    pub fn normalized(x: f64, y: f64, w: f64, h: f64) -> Self {
        let x = x.clamp(0.0, 0.999);
        let y = y.clamp(0.0, 0.999);
        let w = w.clamp(0.001, 1.0 - x);
        let h = h.clamp(0.001, 1.0 - y);
        Self { x, y, w, h }
    }

    /// Map normalized crop coordinates to pixel rectangle `(px_x, px_y, px_w, px_h)`.
    #[must_use]
    pub fn to_pixel_rect(&self, src_w: u32, src_h: u32) -> (u32, u32, u32, u32) {
        if src_w == 0 || src_h == 0 {
            return (0, 0, 0, 0);
        }
        let sw = src_w as f64;
        let sh = src_h as f64;
        let px = (self.x * sw).floor().clamp(0.0, sw - 1.0) as u32;
        let py = (self.y * sh).floor().clamp(0.0, sh - 1.0) as u32;
        let max_w = (src_w - px).max(1);
        let max_h = (src_h - py).max(1);
        let pw = (self.w * sw).round().clamp(1.0, max_w as f64) as u32;
        let ph = (self.h * sh).round().clamp(1.0, max_h as f64) as u32;
        (px, py, pw, ph)
    }
}

/// Round a source's dimensions down to whole 4x4 blocks (at most 3 px off each
/// axis). BC1 works on blocks and the render path copies block rows assuming
/// aligned dimensions, so every bake crops to this before it starts.
#[must_use]
pub fn align4(w: u32, h: u32) -> (u32, u32) {
    (w & !3, h & !3)
}

/// The fixed working resolution everything downstream of ingest runs at
/// (web-port.md §3a).
///
/// Both axes of both tiers are divisible by 4, which is the whole reason these
/// are named constants rather than whatever the source happened to be: BC1
/// operates on 4x4 blocks, and the conventional 854x480 is *not* aligned
/// (854/4 = 213.5) and would force padding on every frame.
///
/// This is a *box* to fit inside, not a size to stretch to — [`Tier::fit`]
/// preserves the source's aspect ratio and never upscales.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tier {
    /// 848x480. The default; §3c measured single-threaded wasm `Draft` at ~2.7x
    /// realtime here, which is what makes browser ingest viable at all.
    #[default]
    Wide,
    /// 568x320, for weaker devices and longer clips.
    Narrow,
}

impl Tier {
    /// The bounding box, in pixels.
    #[must_use]
    pub fn box_size(self) -> (u32, u32) {
        match self {
            Self::Wide => (848, 480),
            Self::Narrow => (568, 320),
        }
    }

    /// Fit `w` x `h` inside this tier: aspect preserved, never upscaled, and
    /// aligned down to whole 4x4 blocks so the result is a legal
    /// [`FrameBaker::new`] size.
    ///
    /// Returns `(0, 0)` for a degenerate source, which [`FrameBaker::new`] then
    /// rejects as [`BakeErr::Unaligned`] — the same answer it gives for any
    /// other unusable size, rather than a second error path to handle.
    #[must_use]
    pub fn fit(self, w: u32, h: u32) -> (u32, u32) {
        if w == 0 || h == 0 {
            return (0, 0);
        }
        let (bw, bh) = self.box_size();
        // `min(1.0)` is the no-upscale clause: a source already below the tier
        // is baked at its own size. Blowing a 320x240 clip up to 848 would cost
        // 7x the bake time and the BC1 payload to add no information.
        let scale = (f64::from(bw) / f64::from(w))
            .min(f64::from(bh) / f64::from(h))
            .min(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (dw, dh) = (
            (f64::from(w) * scale).round() as u32,
            (f64::from(h) * scale).round() as u32,
        );
        align4(dw, dh)
    }
}

/// Errors a frame bake can produce. Both are caller mistakes rather than data
/// problems, which is why they are distinct from [`hap::HapErr`].
#[derive(Debug, PartialEq, Eq)]
pub enum BakeErr {
    /// Dimensions were not whole 4x4 blocks, or were zero. See [`align4`].
    Unaligned { w: u32, h: u32 },
    /// The supplied frame was not exactly `w * h * 4` bytes of tight RGBA.
    /// A stride-padded buffer hits this — repack before calling.
    FrameSize { expected: usize, got: usize },
}

impl std::fmt::Display for BakeErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unaligned { w, h } => {
                write!(f, "{w}x{h} is not a whole number of 4x4 blocks")
            }
            Self::FrameSize { expected, got } => {
                write!(f, "frame is {got} bytes, expected {expected} of tight RGBA")
            }
        }
    }
}

impl std::error::Error for BakeErr {}

/// Compresses frames of one fixed size, reusing its scratch buffer across the
/// whole clip so a bake allocates once rather than once per frame.
pub struct FrameBaker {
    w: usize,
    h: usize,
    quality: BakeQuality,
    params: Params,
    bc1: Vec<u8>,
}

/// Hand-written because texpresso's `Params` is not `Debug`; [`BakeQuality`]
/// carries the same information in a printable form.
impl std::fmt::Debug for FrameBaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FrameBaker({}x{}, {:?})", self.w, self.h, self.quality)
    }
}

impl FrameBaker {
    /// Prepare a baker for `w` x `h` frames.
    ///
    /// # Errors
    /// [`BakeErr::Unaligned`] if the dimensions are not non-zero multiples of 4;
    /// run them through [`align4`] first.
    pub fn new(w: u32, h: u32, quality: BakeQuality) -> Result<Self, BakeErr> {
        if w == 0 || h == 0 || !w.is_multiple_of(4) || !h.is_multiple_of(4) {
            return Err(BakeErr::Unaligned { w, h });
        }
        let (w, h) = (w as usize, h as usize);
        Ok(Self {
            w,
            h,
            quality,
            params: quality.params(),
            bc1: vec![0u8; Format::Bc1.compressed_size(w, h)],
        })
    }

    /// Baked frame dimensions.
    #[must_use]
    pub fn dimensions(&self) -> (u32, u32) {
        (self.w as u32, self.h as u32)
    }

    /// The quality this baker was built with.
    #[must_use]
    pub fn quality(&self) -> BakeQuality {
        self.quality
    }

    /// Bytes of tight RGBA this baker expects per frame.
    #[must_use]
    pub fn frame_bytes(&self) -> usize {
        self.w * self.h * 4
    }

    /// Bytes of BC1 this baker produces per frame, before Snappy.
    ///
    /// Fixed by the dimensions rather than by the content, which is what lets a
    /// caller size a buffer for a whole clip up front instead of baking first
    /// and measuring.
    #[must_use]
    pub fn bc1_bytes(&self) -> usize {
        self.bc1.len()
    }

    /// Block-compress one tightly-packed RGBA frame, returning the BC1 payload.
    /// The slice is valid until the next call.
    ///
    /// Kept separate from [`Self::bake`] so a caller that wants the raw payload
    /// — or wants to time compression apart from framing — can have it.
    ///
    /// # Errors
    /// [`BakeErr::FrameSize`] if `rgba` is not exactly [`Self::frame_bytes`].
    pub fn compress(&mut self, rgba: &[u8]) -> Result<&[u8], BakeErr> {
        if rgba.len() != self.frame_bytes() {
            return Err(BakeErr::FrameSize {
                expected: self.frame_bytes(),
                got: rgba.len(),
            });
        }
        Format::Bc1.compress(rgba, self.w, self.h, self.params, &mut self.bc1);
        Ok(&self.bc1)
    }

    /// Block-compress and frame one RGBA frame into a complete Hap1 packet,
    /// ready to hand to a muxer.
    ///
    /// # Errors
    /// See [`Self::compress`].
    pub fn bake(&mut self, rgba: &[u8]) -> Result<Vec<u8>, BakeErr> {
        self.compress(rgba)?;
        Ok(hap::encode_hap1_frame(&self.bc1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    // Nothing else changes, which is the point — the wasm run must exercise the
    // same assertions, not a parallel copy of them.
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn solid(w: u32, h: u32) -> Vec<u8> {
        vec![0x40; (w * h * 4) as usize]
    }

    #[test]
    fn align4_crops_at_most_three_pixels() {
        assert_eq!(align4(1920, 1080), (1920, 1080));
        assert_eq!(align4(1919, 1079), (1916, 1076));
        assert_eq!(align4(3, 3), (0, 0));
    }

    #[test]
    fn tier_fits_without_stretching_or_upscaling() {
        // 16:9 HD lands on the tier's width and loses 4 rows to block alignment
        // rather than being stretched to the full 848x480 box.
        assert_eq!(Tier::Wide.fit(1920, 1080), (848, 476));
        // Portrait fits on *height*, which is the axis the box constrains here.
        assert_eq!(Tier::Wide.fit(1080, 1920), (268, 480));
        // Already below the tier: left alone, not blown up.
        assert_eq!(Tier::Wide.fit(640, 480), (640, 480));
        assert_eq!(Tier::Narrow.fit(1920, 1080), (568, 320));
        // Degenerate input becomes a size FrameBaker rejects, not a panic.
        assert_eq!(Tier::Wide.fit(0, 1080), (0, 0));
        assert_eq!(Tier::Wide.fit(3, 3), (0, 0));
    }

    #[test]
    fn every_tier_fit_is_a_legal_baker_size() {
        // The contract that matters: whatever a browser hands over, `fit` must
        // produce something `FrameBaker::new` accepts, or reject it outright.
        for tier in [Tier::Wide, Tier::Narrow] {
            for (w, h) in [(1920, 1080), (1280, 720), (640, 360), (720, 1280), (101, 97)] {
                let (fw, fh) = tier.fit(w, h);
                assert!(fw.is_multiple_of(4) && fh.is_multiple_of(4), "{tier:?} {w}x{h}");
                let (bw, bh) = tier.box_size();
                assert!(fw <= bw && fh <= bh, "{tier:?} {w}x{h} -> {fw}x{fh} escaped the box");
                assert!(fw <= w && fh <= h, "{tier:?} {w}x{h} -> {fw}x{fh} upscaled");
            }
        }
    }

    #[test]
    fn unaligned_dimensions_are_rejected_up_front() {
        // Better here than as a corrupt payload later: an odd width silently
        // produces a BC1 buffer that the player would read as a different shape.
        //
        // `unwrap_err` rather than comparing the whole Result: FrameBaker holds
        // texpresso `Params`, which is not Debug, so it cannot be asserted on.
        assert_eq!(
            FrameBaker::new(1919, 1080, BakeQuality::Draft).unwrap_err(),
            BakeErr::Unaligned { w: 1919, h: 1080 }
        );
        assert_eq!(
            FrameBaker::new(0, 0, BakeQuality::Draft).unwrap_err(),
            BakeErr::Unaligned { w: 0, h: 0 }
        );
        assert!(FrameBaker::new(848, 480, BakeQuality::Draft).is_ok());
    }

    #[test]
    fn wrong_frame_size_is_an_error_not_a_panic() {
        // This is the failure mode of handing over a stride-padded buffer, which
        // is what every decoder hands out by default.
        let mut b = FrameBaker::new(64, 64, BakeQuality::Draft).unwrap();
        assert_eq!(b.frame_bytes(), 64 * 64 * 4);
        assert_eq!(
            b.compress(&solid(64, 63)).unwrap_err(),
            BakeErr::FrameSize { expected: 16384, got: 16128 }
        );
        assert!(b.compress(&solid(64, 64)).is_ok());
    }

    #[test]
    fn compressed_payload_is_half_a_byte_per_pixel() {
        let mut b = FrameBaker::new(64, 32, BakeQuality::Draft).unwrap();
        assert_eq!(b.compress(&solid(64, 32)).unwrap().len(), 64 * 32 / 2);
    }

    /// The scratch buffer is reused across frames; a second frame must not
    /// inherit anything from the first.
    #[test]
    fn reuse_does_not_leak_between_frames() {
        let mut b = FrameBaker::new(16, 16, BakeQuality::Draft).unwrap();
        let dark = b.compress(&vec![0x10; 16 * 16 * 4]).unwrap().to_vec();
        let _bright = b.compress(&vec![0xf0; 16 * 16 * 4]).unwrap().to_vec();
        let dark_again = b.compress(&vec![0x10; 16 * 16 * 4]).unwrap();
        assert_eq!(dark, dark_again);
    }

    /// The whole point of the type: what it emits is a packet `hap::decode_frame`
    /// accepts, and the round trip preserves the payload exactly.
    #[test]
    fn baked_packet_round_trips_through_the_decoder() {
        let mut b = FrameBaker::new(32, 32, BakeQuality::Draft).unwrap();
        let rgba = solid(32, 32);
        let expect = b.compress(&rgba).unwrap().to_vec();
        let packet = b.bake(&rgba).unwrap();

        let (mut main, mut alpha) = (Vec::new(), Vec::new());
        let meta = hap::decode_frame(&packet, 1, &mut main, &mut alpha).unwrap();
        assert_eq!(meta.format, hap::HapTextureFormat::Bc1);
        assert_eq!(main, expect);
        assert!(alpha.is_empty());
    }
}
