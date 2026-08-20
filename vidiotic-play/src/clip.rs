//! A baked HAP clip held in memory, addressed by time.
//!
//! This is the whole `/play` read path with the GPU and the browser taken out
//! of it: bytes in, a located sample out, a decoded BC frame out. The container
//! walk is `vidiotic_bake::mov::demux` and the bitstream parse is
//! `vidiotic_bake::hap::decode_frame` — both already proven against ffmpeg's
//! demuxer and against real packets — so what is left here is the part neither
//! of them covers: turning a wall-clock time into a sample index, and looping.
//!
//! Whole-file-in-memory is a deliberate choice rather than a shortcut. A chop
//! is a handful of seconds at the §3a tier, `demux` wants the whole slice
//! anyway, and it is what the browser hands over — a `File` read to an
//! `ArrayBuffer`. Nothing streams, so nothing needs a state machine.
//!
//! Every HAP frame is independently decodable, which is the property the whole
//! format choice rests on: a loop restart is a seek to any sample, not a seek
//! to a keyframe and a decode forward (web-port.md §3a).

use std::rc::Rc;

use vidiotic_bake::hap::{self, HapErr};
use vidiotic_bake::mov::{self, MovErr, MovTrack};

use crate::video::frame::{DecodedFrame, PixelData};

/// Why a clip could not be opened or a frame could not be produced.
#[derive(Debug)]
pub enum ClipErr {
    /// The container did not parse.
    Container(MovErr),
    /// It parsed, but the video track is not one this player can decode. Holds
    /// the `stsd` fourcc so the message can name what it actually got.
    NotHap([u8; 4]),
    /// The track has no samples at all — a valid file with nothing to show.
    Empty,
    /// A located sample did not decode as HAP.
    Hap(HapErr),
    /// No such sample index.
    NoSuchSample(usize),
}

impl std::fmt::Display for ClipErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(e) => write!(f, "not a readable QuickTime file: {e}"),
            Self::NotHap(cc) => {
                write!(f, "video track is '{}', not HAP", String::from_utf8_lossy(cc))
            }
            Self::Empty => write!(f, "video track contains no samples"),
            Self::Hap(e) => write!(f, "HAP decode failed: {e}"),
            Self::NoSuchSample(i) => write!(f, "no sample at index {i}"),
        }
    }
}

impl std::error::Error for ClipErr {}

impl From<MovErr> for ClipErr {
    fn from(e: MovErr) -> Self {
        Self::Container(e)
    }
}

impl From<HapErr> for ClipErr {
    fn from(e: HapErr) -> Self {
        Self::Hap(e)
    }
}

/// A baked HAP clip, demuxed and ready to be sampled by time.
pub struct Clip {
    bytes: Rc<Vec<u8>>,
    track: MovTrack,
    /// From the fourcc: `HapM` carries a second (BC4 alpha) texture section.
    texture_count: u8,
    /// Reused across decodes so a steady-state render loop is not resizing two
    /// vectors every frame.
    main: Vec<u8>,
    alpha: Vec<u8>,
}

impl Clip {
    /// Demux `bytes` and check it is something we can decode.
    ///
    /// The buffer is shared rather than owned because the whole baked file stays
    /// in memory (see the module note) and one clip is opened once per cue that
    /// plays it. Taking an owned `Vec` would mean a full copy of the clip per
    /// cue, on top of the pool's own — for a clip of any length that is the
    /// largest allocation the player makes, so it is worth not making it twice.
    ///
    /// # Errors
    /// [`ClipErr::Container`] if the file does not parse, [`ClipErr::NotHap`]
    /// if its video track is some other codec, [`ClipErr::Empty`] if it has no
    /// samples.
    pub fn open(bytes: Rc<Vec<u8>>) -> Result<Self, ClipErr> {
        let track = mov::demux(&bytes)?;
        if !track.is_hap() {
            return Err(ClipErr::NotHap(track.format));
        }
        if track.samples.is_empty() {
            return Err(ClipErr::Empty);
        }
        let texture_count = u8::from(&track.format == b"HapM") + 1;
        Ok(Self {
            bytes,
            track,
            texture_count,
            main: Vec::new(),
            alpha: Vec::new(),
        })
    }

    /// The demuxed track, for callers that want dimensions or the sample list.
    #[must_use]
    pub fn track(&self) -> &MovTrack {
        &self.track
    }

    /// Pixel width of the decoded frame.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.track.width
    }

    /// Pixel height of the decoded frame.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.track.height
    }

    /// Number of samples in the demuxed track.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.track.samples.len()
    }

    /// Total length in seconds. Zero for a track whose declared duration is
    /// zero, which a degenerate single-frame file can be.
    #[must_use]
    pub fn duration_sec(&self) -> f64 {
        if self.track.timescale == 0 {
            return 0.0;
        }
        self.track.duration as f64 / f64::from(self.track.timescale)
    }

    /// The sample to show at `t_sec`, looping.
    ///
    /// `t_sec` wraps into `[0, duration)` rather than clamping, because a
    /// player's clock runs past the end of a clip and the cue keeps playing —
    /// looping is the normal case, not the edge case. Negative times wrap the
    /// same way (`rem_euclid`), so a cue nudged behind the beat still resolves.
    ///
    /// A zero-length track cannot be wrapped into, so it holds sample 0.
    #[must_use]
    pub fn sample_index_at(&self, t_sec: f64) -> usize {
        let total = self.track.duration;
        if total == 0 || !t_sec.is_finite() {
            return 0;
        }
        let units = t_sec * f64::from(self.track.timescale);
        // Wrap in floating point *before* the integer cast: a clip left running
        // for an hour is still a small number of units, but the cast itself
        // saturates rather than wrapping, so ordering matters here.
        let wrapped = units.rem_euclid(total as f64);
        self.track
            .sample_at(wrapped as u64)
            .unwrap_or(0)
            .min(self.track.samples.len() - 1)
    }

    /// Decode sample `index` into a frame ready for `Renderer::upload_frame`.
    ///
    /// The returned frame owns its pixels, because that is
    /// [`DecodedFrame`]'s contract on both targets — the native decode thread
    /// hands ownership across a channel. The scratch buffers here still earn
    /// their place: `hap::decode_frame` fills them without reallocating, and
    /// only the final handoff copies.
    ///
    /// # Errors
    /// [`ClipErr::NoSuchSample`] for an out-of-range index, [`ClipErr::Hap`] if
    /// the packet does not decode.
    pub fn frame(&mut self, index: usize) -> Result<DecodedFrame, ClipErr> {
        let data = self
            .track
            .sample_data(&self.bytes, index)
            .ok_or(ClipErr::NoSuchSample(index))?;
        let meta = hap::decode_frame(data, self.texture_count, &mut self.main, &mut self.alpha)?;
        let pts_sec = if self.track.timescale == 0 {
            0.0
        } else {
            self.track.samples[index].pts as f64 / f64::from(self.track.timescale)
        };
        Ok(DecodedFrame {
            pixels: PixelData::Bc {
                format: meta.format,
                data: self.main.clone(),
                alpha: meta.has_alpha.then(|| self.alpha.clone()),
                video_mode: meta.video_mode,
            },
            w: self.track.width,
            h: self.track.height,
            pts_sec,
        })
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

    const W: u32 = 64;
    const H: u32 = 64;
    /// BC1 is 8 bytes per 4x4 block.
    const BC1_LEN: usize = (W as usize / 4) * (H as usize / 4) * 8;

    /// Build a real HAP `.mov` in memory. No filesystem, so this runs in V8 —
    /// the same trick `mov.rs`'s own reader tests use. Going through
    /// `MovWriter` rather than hand-rolling a header means these tests exercise
    /// the container the baker actually emits.
    fn synth(frames: usize, timescale: u32, frame_dur: u32) -> Vec<u8> {
        let mut w = mov::MovWriter::new(
            std::io::Cursor::new(Vec::new()),
            W,
            H,
            timescale,
            frame_dur,
        )
        .expect("writer");
        for i in 0..frames {
            // Distinct payload per frame so a mis-addressed sample is visible
            // as wrong bytes rather than passing by coincidence.
            let bc1 = vec![u8::try_from(i % 251).unwrap(); BC1_LEN];
            let packet = hap::encode_hap1_frame(&bc1);
            w.write_sample(&packet, u32::try_from(i).unwrap() * frame_dur)
                .expect("write");
        }
        w.finish().expect("finish").into_inner()
    }

    /// 10 frames at 30 fps on the timescale `transcode.rs` derives.
    fn clip10() -> Clip {
        open(synth(10, 30_000, 1_000)).expect("open")
    }

    /// [`Clip::open`] over an owned buffer, which is what every test has.
    fn open(bytes: Vec<u8>) -> Result<Clip, ClipErr> {
        Clip::open(Rc::new(bytes))
    }

    #[test]
    fn a_baked_clip_opens_and_reports_its_shape() {
        let c = clip10();
        assert_eq!((c.width(), c.height()), (W, H));
        assert_eq!(c.frame_count(), 10);
        assert!((c.duration_sec() - 10.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn time_zero_is_the_first_sample() {
        assert_eq!(clip10().sample_index_at(0.0), 0);
    }

    #[test]
    fn each_frames_own_span_maps_to_it() {
        let c = clip10();
        let frame = 1.0 / 30.0;
        for i in 0..10 {
            let start = i as f64 * frame;
            assert_eq!(c.sample_index_at(start), i, "at the start of frame {i}");
            // Just inside the same frame's span must not advance.
            assert_eq!(
                c.sample_index_at(start + frame * 0.99),
                i,
                "just before the end of frame {i}"
            );
        }
    }

    #[test]
    fn time_past_the_end_wraps_rather_than_clamping() {
        let c = clip10();
        let dur = c.duration_sec();
        // A player's clock keeps running; the cue loops.
        assert_eq!(c.sample_index_at(dur), 0, "exactly one loop on");
        assert_eq!(c.sample_index_at(dur + 1.0 / 30.0), 1);
        assert_eq!(c.sample_index_at(dur * 7.0 + 3.5 / 30.0), 3, "many loops on");
    }

    #[test]
    fn negative_time_wraps_from_the_end() {
        let c = clip10();
        // -1 frame is the last frame, not frame 0 and not a panic.
        assert_eq!(c.sample_index_at(-1.0 / 30.0), 9);
        assert_eq!(c.sample_index_at(-c.duration_sec()), 0);
    }

    #[test]
    fn a_non_thirty_fps_timescale_maps_correctly() {
        // 24 fps, and a timescale that is not a multiple of 1000.
        let c = open(synth(6, 24, 1)).expect("open");
        assert!((c.duration_sec() - 0.25).abs() < 1e-9);
        for i in 0..6 {
            assert_eq!(c.sample_index_at(i as f64 / 24.0), i);
        }
        assert_eq!(c.sample_index_at(0.25), 0, "wraps at exactly one loop");
    }

    #[test]
    fn a_single_frame_clip_holds_that_frame() {
        let c = open(synth(1, 30_000, 1_000)).expect("open");
        assert_eq!(c.frame_count(), 1);
        for t in [0.0, 0.5, 100.0, -3.0] {
            assert_eq!(c.sample_index_at(t), 0, "at t={t}");
        }
    }

    #[test]
    fn every_sample_decodes_to_the_bytes_that_were_baked() {
        // The end-to-end claim: the sample the timeline picks is the one whose
        // payload comes back. Distinct fill values per frame are what make a
        // mis-addressed sample fail here instead of passing silently.
        let mut c = clip10();
        for i in 0..c.frame_count() {
            let f = c.frame(i).expect("decode");
            assert_eq!((f.w, f.h), (W, H));
            let PixelData::Bc { data, alpha, .. } = &f.pixels else {
                panic!("frame {i} did not decode as block-compressed");
            };
            assert!(alpha.is_none(), "Hap1 has no alpha plane");
            assert_eq!(data.len(), BC1_LEN, "frame {i} wrong size");
            assert!(
                data.iter().all(|b| *b == u8::try_from(i % 251).unwrap()),
                "frame {i} decoded to another frame's bytes"
            );
        }
    }

    #[test]
    fn frame_pts_matches_the_timeline() {
        let mut c = clip10();
        for i in 0..c.frame_count() {
            let f = c.frame(i).expect("decode");
            assert!(
                (f.pts_sec - i as f64 / 30.0).abs() < 1e-9,
                "frame {i} pts {} is not {}",
                f.pts_sec,
                i as f64 / 30.0
            );
        }
    }

    #[test]
    fn an_out_of_range_sample_is_an_error_not_a_panic() {
        let mut c = clip10();
        assert!(matches!(c.frame(10), Err(ClipErr::NoSuchSample(10))));
    }

    /// The synthetic clips above are laid out by the same writer this crate's
    /// author controls, so agreeing with them proves less than it appears to.
    /// This runs the same path over a clip the *native tools actually baked* —
    /// which is precisely what web-port.md §8 step 4 says `/play` must play.
    ///
    /// Native-only: the pool is not part of the crate, and there is no
    /// filesystem in V8. Skips rather than fails if the checkout lacks it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_real_baked_clip_plays_end_to_end() {
        const POOL: [&str; 3] = ["../clips/brb.mov", "../clips/bun.mov", "../clips/eyes.mov"];
        let mut checked = 0;
        for path in POOL {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let mut c = open(bytes).unwrap_or_else(|e| panic!("{path}: {e}"));
            assert!(c.frame_count() > 1, "{path} has no timeline to speak of");
            assert!(c.duration_sec() > 0.0, "{path} has zero duration");
            assert_eq!(c.width() % 4, 0, "{path} width is not BC-block aligned");
            assert_eq!(c.height() % 4, 0, "{path} height is not BC-block aligned");

            // Every sample the timeline can select must decode, and to exactly
            // the size the dimensions imply — a mis-addressed packet is not a
            // valid HAP section, so this catches a table error that happened to
            // preserve lengths.
            let expect = (c.width() as usize / 4) * (c.height() as usize / 4) * 8;
            for i in 0..c.frame_count() {
                let f = c.frame(i).unwrap_or_else(|e| panic!("{path} frame {i}: {e}"));
                let PixelData::Bc { data, .. } = &f.pixels else {
                    panic!("{path} frame {i} is not block-compressed");
                };
                assert_eq!(data.len(), expect, "{path} frame {i} wrong payload size");
            }

            // Walking the clip in real time must visit every frame in order.
            //
            // Driven off each sample's own span rather than a uniform
            // `duration / frame_count` step, because these files do not have
            // one. The pool was baked before the timeline fix, so it carries
            // the old shape: timescale 16000 (libavformat's override) and
            // per-frame durations alternating 528/544 to average 30 fps. A
            // uniform-step walk drifts a whole frame by the middle of the clip
            // — which is a fact about the files, not about the timeline.
            let ts = f64::from(c.track().timescale);
            let mut zero_dur = Vec::new();
            for (i, s) in c.track().samples.iter().enumerate() {
                if s.duration == 0 {
                    // An empty span: no time can land inside it. That is the
                    // known zero-duration tail frame these bakes ended with.
                    zero_dur.push(i);
                    continue;
                }
                let t = (s.pts as f64 + f64::from(s.duration) * 0.5) / ts;
                assert_eq!(c.sample_index_at(t), i, "{path} at t={t}");
            }
            // The defect is a *tail* frame. One mid-clip would mean something
            // else is wrong, and would silently hide frames from the walk above.
            let last = c.frame_count() - 1;
            assert!(
                zero_dur.is_empty() || zero_dur == [last],
                "{path}: zero-duration samples at {zero_dur:?}, expected at most the last ({last})"
            );
            checked += 1;
        }
        if checked == 0 {
            eprintln!("skipping: none of {POOL:?} present");
        }
    }

    #[test]
    fn a_file_that_is_not_a_movie_is_rejected() {
        let Err(err) = open(b"this is not a QuickTime file at all".to_vec()) else {
            panic!("random bytes opened as a clip");
        };
        assert!(matches!(err, ClipErr::Container(_)), "got {err:?}");
    }
}
