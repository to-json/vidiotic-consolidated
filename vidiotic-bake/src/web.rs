//! Browser bake: RGBA frames a page decoded, in; a HAP `.mov` out.
//!
//! **Almost none of the work is here.** The compressor [`FrameBaker`] and the
//! muxer [`MovWriter`] are the same portable code the native baker drives, so a
//! clip baked in a browser is byte-identical to one baked on the desktop — not
//! an aspiration, but what this crate's `bc1_golden` test asserts, under wasm,
//! in the gate. What this module adds is the two things a browser bake needs
//! and a native one does not: a `Cursor<Vec<u8>>` where the native path has a
//! file, and a presentation clock that comes from the *page* rather than from a
//! demuxer.
//!
//! # Why it lives here rather than in a front end
//!
//! It was `vidiotic-play::web::bake`, because `/play` was the only thing that
//! baked in a browser. `/chop` bakes too — every span of an export — and the
//! two front ends cannot depend on each other (one carries wgpu, the other
//! must not). So the glue moved to the crate whose job baking already is, and
//! both shells drive one implementation.
//!
//! # Why the caller supplies timestamps
//!
//! There is no demuxer on this side: the page drives a `<video>` element and
//! hands over whatever it drew, so only the page knows what moment each frame
//! belongs to. A microsecond timescale and an explicit per-frame `pts` keep
//! that honest — the muxer records exactly the timing it was given rather than
//! assuming a rate, so a page that samples at a constant 30 and a page that
//! captures a variable-rate source both produce a correct clip, and a frame
//! that fails to arrive leaves a longer gap rather than shortening the whole
//! thing.
//!
//! It also means the ingest driver can change — seek-stepping today, `WebCodecs`
//! once there is a demuxer to feed it — without this type learning about it.

use std::io::Cursor;

use wasm_bindgen::prelude::*;

use crate::frame::{BakeErr, BakeQuality, FrameBaker, Tier};
use crate::mov::MovWriter;

/// Timescale for every browser bake: microseconds.
///
/// `pts` is a `u32`, so this caps a single ingest at ~71 minutes. That is a
/// deliberate trade rather than an oversight — microseconds make every
/// `mediaTime` land exactly on a unit, and a VJ clip that runs past an hour is
/// not a thing this tool is for. [`Baker::push`] says so by name when it
/// happens rather than wrapping.
const TIMESCALE: u32 = 1_000_000;

/// Nominal frame rate assumed for the *last* sample's duration when the caller
/// does not know the source's rate. It affects nothing else: every other
/// sample's duration is the difference to the next timestamp.
const ASSUMED_FPS: f64 = 30.0;

/// Per-frame container bookkeeping to allow for when sizing the output buffer:
/// 8 bytes for the HAP section header and 12 for the sample-table entry
/// [`MovWriter::finish`] writes.
const PER_FRAME_OVERHEAD: usize = 20;

/// Ceiling on the up-front reservation, so a frame estimate that arrives wrong
/// degrades to ordinary growth rather than to an allocation the heap cannot
/// serve. A bake past this is beyond what a page can hold either way.
const MAX_RESERVE: usize = 1 << 30;

/// An in-progress bake: construct, [`push`](Self::push) each decoded frame,
/// then [`finish`](Self::finish).
///
/// Dropping without finishing throws the work away, which is the right answer
/// for a cancelled ingest — a partial `.mov` with no `moov` is a file no player
/// opens, and pretending otherwise would produce a clip that plays back short.
#[wasm_bindgen]
pub struct Baker {
    baker: FrameBaker,
    mov: MovWriter<Cursor<Vec<u8>>>,
    /// `mediaTime` of the first accepted frame, so the bake starts at zero
    /// however far into the source the caller began.
    base_sec: Option<f64>,
    /// Last accepted `pts`, to enforce the strictly-increasing order the sample
    /// table is built by differencing.
    last_pts: Option<u32>,
    /// Frames rejected for arriving at or before [`Self::last_pts`].
    repeats: usize,
}

#[wasm_bindgen]
impl Baker {
    /// Prepare a bake for a source of `src_w` x `src_h`.
    ///
    /// The output size is [`Tier::fit`] of that — aspect preserved, never
    /// upscaled, aligned to whole 4x4 blocks — and the caller must scale each
    /// frame to [`width`](Self::width) x [`height`](Self::height) before
    /// pushing it. Scaling belongs to the page: it already has a hardware
    /// scaler in `drawImage`, and doing it here would mean a full-resolution
    /// readback per frame to avoid using it.
    ///
    /// `expected_frames` sizes the output buffer up front. It is a hint, not a
    /// promise — a bake that runs long or short still produces the same clip —
    /// but getting it roughly right matters more than it looks: BC1 is half a
    /// byte per pixel with no rate control, so a clip of any length is the
    /// largest allocation the page makes, and growing it by doubling means
    /// holding the old buffer and the new one at once at exactly the moment
    /// there is least room to. Pass 0 to opt out and grow as it goes.
    ///
    /// # Errors
    /// If the source dimensions are degenerate, or if the fitted size is not a
    /// legal compressor size — both surface as the same message, because both
    /// mean the same thing to the visitor.
    #[wasm_bindgen(constructor)]
    pub fn new(
        src_w: u32,
        src_h: u32,
        narrow: bool,
        high_quality: bool,
        expected_frames: usize,
    ) -> Result<Self, JsValue> {
        let tier = if narrow { Tier::Narrow } else { Tier::Wide };
        let (w, h) = tier.fit(src_w, src_h);
        let quality = if high_quality { BakeQuality::High } else { BakeQuality::Draft };
        let baker = FrameBaker::new(w, h, quality).map_err(|e: BakeErr| {
            JsValue::from_str(&format!("{src_w}x{src_h} cannot be baked: {e}"))
        })?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let frame_duration = (f64::from(TIMESCALE) / ASSUMED_FPS).round() as u32;
        let reserve = expected_frames
            .saturating_mul(baker.bc1_bytes() + PER_FRAME_OVERHEAD)
            .saturating_add(1024)
            .min(MAX_RESERVE);
        let mov = MovWriter::new(
            Cursor::new(Vec::with_capacity(reserve)),
            w,
            h,
            TIMESCALE,
            frame_duration,
        )
        .map_err(|e| JsValue::from_str(&format!("could not start the container: {e}")))?;
        log::info!("ingest: {src_w}x{src_h} -> {w}x{h} ({quality:?}), {reserve} bytes reserved");
        Ok(Self { baker, mov, base_sec: None, last_pts: None, repeats: 0 })
    }

    /// Baked width, in pixels. Frames must be exactly this wide.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn width(&self) -> u32 {
        self.baker.dimensions().0
    }

    /// Baked height, in pixels. Frames must be exactly this tall.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn height(&self) -> u32 {
        self.baker.dimensions().1
    }

    /// Frames written so far.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn frames(&self) -> usize {
        self.mov.sample_count()
    }

    /// Frames the caller offered that did not advance the source's clock.
    ///
    /// Not an error and not silent either: a page sampling faster than the
    /// source's own frame rate will offer the same moment twice. Dropping those
    /// is correct — two samples at one timestamp would give the first a zero
    /// duration — but a bake that is mostly repeats means the driver is asking
    /// for more frames than the source has, which is worth saying rather than
    /// leaving to be inferred from a suspiciously large file.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn repeats(&self) -> usize {
        self.repeats
    }

    /// Compress and append one frame, presented at `t_sec` of the source.
    ///
    /// `rgba` must be exactly `width * height * 4` bytes, tightly packed — what
    /// `CanvasRenderingContext2D.getImageData` hands back at the baked size.
    /// Returns whether the frame was accepted; `false` means it repeated a
    /// timestamp already written, which is a normal thing for a page to do and
    /// not worth an exception.
    ///
    /// # Errors
    /// If the frame is the wrong size, if `t_sec` runs past what the timescale
    /// can express, or if the container write fails.
    pub fn push(&mut self, rgba: &[u8], t_sec: f64) -> Result<bool, JsValue> {
        if !t_sec.is_finite() || t_sec < 0.0 {
            return Err(JsValue::from_str(&format!("frame timestamp {t_sec} is not a time")));
        }
        let base = *self.base_sec.get_or_insert(t_sec);
        let micros = (t_sec - base) * f64::from(TIMESCALE);
        if micros > f64::from(u32::MAX) {
            return Err(JsValue::from_str(
                "clip is longer than an ingest can hold (~71 minutes); trim it first",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pts = micros.max(0.0).round() as u32;
        // A frame at or before the last one is a re-presentation, not new
        // material. Accepting it would hand the muxer two samples with one
        // timestamp, and `stts` would give the earlier of them zero duration.
        if self.last_pts.is_some_and(|last| pts <= last) {
            self.repeats += 1;
            return Ok(false);
        }

        let packet = self
            .baker
            .bake(rgba)
            .map_err(|e| JsValue::from_str(&format!("frame {}: {e}", self.frames())))?;
        self.mov
            .write_sample(&packet, pts)
            .map_err(|e| JsValue::from_str(&format!("frame {}: {e}", self.frames())))?;
        self.last_pts = Some(pts);
        Ok(true)
    }

    /// Close the container and return the finished `.mov` bytes.
    ///
    /// Consumes the baker: the JS handle is invalid afterwards, which is the
    /// truthful shape — a `MovWriter` that has written its `moov` cannot take
    /// another sample.
    ///
    /// # Errors
    /// If the container could not be completed, or if nothing was ever pushed —
    /// a zero-frame clip parses but shows nothing, and "the ingest produced no
    /// frames" is a far more useful thing to say than a black output head.
    pub fn finish(self) -> Result<Vec<u8>, JsValue> {
        if self.frames() == 0 {
            return Err(JsValue::from_str(
                "no frames were decoded — the browser may not be able to play this file",
            ));
        }
        let frames = self.frames();
        let cursor = self
            .mov
            .finish()
            .map_err(|e| JsValue::from_str(&format!("could not finish the container: {e}")))?;
        let bytes = cursor.into_inner();
        log::info!("ingest: {frames} frames, {} bytes", bytes.len());
        Ok(bytes)
    }
}

/// Whether these bytes are already a HAP clip, so a page can skip a bake it
/// does not need.
///
/// Deliberately a container probe rather than a filename check: a `.mov` may
/// hold `ProRes` just as easily as Hap1, and a HAP clip renamed to `.mp4` is
/// still a HAP clip. This reads the sample description and answers about the
/// actual codec.
#[wasm_bindgen]
#[must_use]
pub fn is_baked(bytes: &[u8]) -> bool {
    crate::mov::demux(bytes).is_ok_and(|t| t.is_hap())
}

/// The dimensions a source of `src_w` x `src_h` would bake to.
///
/// Exported so the page can show the visitor what it is about to spend a minute
/// producing, and so the ingest driver can size its scratch canvas without
/// having constructed a [`Baker`] yet.
#[wasm_bindgen]
#[must_use]
pub fn bake_size(src_w: u32, src_h: u32, narrow: bool) -> Vec<u32> {
    let (w, h) = if narrow { Tier::Narrow } else { Tier::Wide }.fit(src_w, src_h);
    vec![w, h]
}
