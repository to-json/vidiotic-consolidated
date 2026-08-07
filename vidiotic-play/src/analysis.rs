//! The audio analysis itself: samples in, bands and a Shadertoy audio texture
//! out.
//!
//! Split out of `vidiotic::analysis` for the same reason the engine was split
//! out of `app.rs`. Getting a signal is machine-specific — a cpal device and a
//! lock-free ring natively, a `MediaStream` and an `AudioContext` in a browser —
//! but *what to do with the samples* is arithmetic, and it is the half that
//! decides what an audio-reactive shader sees. Two implementations of it would
//! mean a clip that reacts one way on the desktop and another way on the web,
//! which is the kind of divergence nobody notices until they are standing in
//! front of a projector.
//!
//! So this is the whole of it: a Hann window, a 2048-point FFT, 21 log-spaced
//! perceptual bands with fast-attack/slow-decay smoothing, and the 512x2 R8
//! texture the shaders sample as `iChannel0`. What stays behind on each side is
//! only the part that gets the samples here.
//!
//! [`Analyzer`] holds all the scratch it needs across calls, so a running
//! analysis allocates nothing per frame.

use std::collections::VecDeque;
use std::sync::Arc;

use rustfft::{num_complex::Complex, Fft, FftPlanner};

pub use crate::render::{AUDIO_TEX_LEN, AUDIO_TEX_W};

/// FFT length in samples; also the length of the sliding analysis window.
pub const FFT_SIZE: usize = 2048;
/// Number of log-spaced perceptual bands exposed to shaders via `fftBand`.
pub const NUM_BANDS: usize = 21;

// Smoothing per ~60 Hz hop: high attack so bars jump on a transient, decay
// multiplier so they fall visibly slower. Tuned by eye against real music.
const ATTACK: f32 = 0.7;
const DECAY: f32 = 0.88;

/// Compression divisor for the normalized spectrum row (matches the demo
/// shaders' `log(1+mag)/8` convention so raw magnitudes land in 0..1).
const SPEC_LOG_SCALE: f32 = 8.0;

/// How much un-analysed input [`Analyzer::feed`] will hold before it starts
/// dropping the oldest.
///
/// A producer that outruns the consumer must lose the *old* samples, not the
/// new ones: audio reactivity is a statement about now, and a backlog played
/// out in order would put the visuals progressively further behind the music
/// with no way to recover. Four hops is enough to absorb a scheduling hiccup
/// and short enough that nothing perceptible accumulates.
const MAX_PENDING_HOPS: usize = 4;

/// One analysis frame: what the renderer uploads and what the shaders read.
#[derive(Clone, Copy)]
pub struct AudioFrame {
    pub bands: [f32; NUM_BANDS],
    pub level: f32,
    /// Packed 512x2 R8 audio texture, row-major: `[0..512]` = FFT spectrum
    /// (linear frequency DC..Nyquist, normalized 0..1), `[512..1024]` = waveform
    /// (raw PCM, 0..1 centered on 0.5). Uploaded verbatim as a Shadertoy iChannel.
    pub audio_tex: [u8; AUDIO_TEX_LEN],
}

impl Default for AudioFrame {
    fn default() -> Self {
        let mut audio_tex = [0u8; AUDIO_TEX_LEN];
        // Silence: flat spectrum (0), waveform centered at 0.5.
        for w in &mut audio_tex[AUDIO_TEX_W..] {
            *w = 128;
        }
        Self {
            bands: [0.0; NUM_BANDS],
            level: 0.0,
            audio_tex,
        }
    }
}

/// Log-spaced band boundaries (FFT bin ranges), 20 Hz..20 kHz, clamped to the
/// usable DC..Nyquist bin range for the given sample rate.
fn log_bands(sample_rate: f32) -> [(usize, usize); NUM_BANDS] {
    let mut bounds = [(0usize, 0usize); NUM_BANDS];
    let (log_min, log_max) = (20.0f32.ln(), 20000.0f32.ln());
    for (i, bound) in bounds.iter_mut().enumerate() {
        let f_lo = (log_min + (log_max - log_min) * i as f32 / NUM_BANDS as f32).exp();
        let f_hi = (log_min + (log_max - log_min) * (i + 1) as f32 / NUM_BANDS as f32).exp();
        let b_lo = (f_lo * FFT_SIZE as f32 / sample_rate).round() as usize;
        let b_hi = (f_hi * FFT_SIZE as f32 / sample_rate).round() as usize;
        let half = FFT_SIZE / 2; // usable bins DC..Nyquist
        let b_lo = b_lo.clamp(1, half - 1);
        let b_hi = b_hi.clamp(b_lo + 1, half);
        *bound = (b_lo, b_hi);
    }
    bounds
}

/// A running analysis over a sliding window of mono samples.
///
/// Feed it whatever the platform hands over — a ring drain, an
/// `AudioWorklet` quantum — and take frames out when a hop's worth has
/// accumulated. The hop is a sixtieth of a second of input, so this produces
/// roughly one frame per display refresh regardless of how the input is
/// chunked.
pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    buf: Vec<Complex<f32>>,
    window: [f32; FFT_SIZE],
    /// The sliding window of the most recent input.
    samples: [f32; FFT_SIZE],
    smoothed: [f32; NUM_BANDS],
    /// Per-bin smoothing state for the FFT texture row.
    spec_smoothed: [f32; AUDIO_TEX_W],
    bands: [(usize, usize); NUM_BANDS],
    hop: usize,
    sample_rate: f32,
    pending: VecDeque<f32>,
    out: AudioFrame,
}

impl Analyzer {
    /// Prepare an analysis for a source running at `sample_rate` Hz.
    #[must_use]
    pub fn new(sample_rate: f32) -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        let mut window = [0.0f32; FFT_SIZE];
        for (i, w) in window.iter_mut().enumerate() {
            *w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos());
        }
        let mut me = Self {
            fft,
            scratch,
            buf: vec![Complex::default(); FFT_SIZE],
            window,
            samples: [0.0; FFT_SIZE],
            smoothed: [0.0; NUM_BANDS],
            spec_smoothed: [0.0; AUDIO_TEX_W],
            bands: log_bands(48000.0),
            hop: 800,
            sample_rate: 48000.0,
            pending: VecDeque::new(),
            out: AudioFrame::default(),
        };
        me.set_sample_rate(sample_rate);
        me
    }

    /// Re-derive the band edges and hop for a new source, and reset the
    /// smoothing so the old signal does not decay into the new one.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.bands = log_bands(sample_rate);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let hop = (sample_rate as usize / 60).max(64);
        self.hop = hop;
        self.samples.fill(0.0);
        self.smoothed.fill(0.0);
        self.spec_smoothed.fill(0.0);
        self.pending.clear();
        self.out = AudioFrame::default();
    }

    /// Samples consumed per produced frame — a sixtieth of a second of input.
    #[must_use]
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// The rate the band edges were derived from.
    #[must_use]
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Whether a call to [`Self::poll`] would produce a frame.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.pending.len() >= self.hop
    }

    /// Queue mono samples for analysis, dropping the oldest if the caller is
    /// producing faster than it is polling. See [`MAX_PENDING_HOPS`].
    pub fn feed(&mut self, samples: &[f32]) {
        self.pending.extend(samples.iter().copied());
        let cap = self.hop * MAX_PENDING_HOPS;
        if self.pending.len() > cap {
            let excess = self.pending.len() - cap;
            self.pending.drain(..excess);
        }
    }

    /// Queue one hop of silence.
    ///
    /// For a source that has stopped delivering — a capture device that went
    /// away, a shared tab that was closed, a `MediaStream` track that ended.
    /// Without this the last computed frame stands forever and every reactive
    /// effect latches at whatever level it happened to see last, which on stage
    /// looks exactly like the renderer having hung.
    ///
    /// Silence rather than a decay curve applied to the output: a dead source
    /// *is* silence, and running it through the same window and the same
    /// smoothing is both simpler and the only way the fall looks identical to a
    /// track that merely went quiet.
    pub fn feed_silence(&mut self) {
        for _ in 0..self.hop {
            self.pending.push_back(0.0);
        }
        let cap = self.hop * MAX_PENDING_HOPS;
        if self.pending.len() > cap {
            let excess = self.pending.len() - cap;
            self.pending.drain(..excess);
        }
    }

    /// Consume one hop and produce a frame, or `None` if not enough has been
    /// fed yet.
    pub fn poll(&mut self) -> Option<&AudioFrame> {
        if !self.ready() {
            return None;
        }
        let hop = self.hop;
        // Slide the window left by `hop` and append the newest `hop` samples.
        self.samples.copy_within(hop.., 0);
        for slot in &mut self.samples[FFT_SIZE - hop..] {
            *slot = self.pending.pop_front().unwrap_or(0.0);
        }
        Some(self.analyze())
    }

    /// The most recent frame, whether or not anything new has been fed.
    #[must_use]
    pub fn frame(&self) -> &AudioFrame {
        &self.out
    }

    /// Window, transform, bin and smooth the current sliding window.
    fn analyze(&mut self) -> &AudioFrame {
        for (b, (&s, &w)) in self.buf.iter_mut().zip(self.samples.iter().zip(&self.window)) {
            *b = Complex { re: s * w, im: 0.0 };
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);

        for (i, &(lo, hi)) in self.bands.iter().enumerate() {
            let mut sum = 0.0f32;
            let count = hi.saturating_sub(lo).max(1) as f32;
            for c in &self.buf[lo..hi] {
                sum += (c.re * c.re + c.im * c.im).sqrt();
            }
            let mag = sum / count;
            let s = &mut self.smoothed[i];
            if mag > *s {
                *s = *s * (1.0 - ATTACK) + mag * ATTACK;
            } else {
                *s *= DECAY;
            }
        }

        // Shadertoy-style 512x2 audio texture.
        let mut audio_tex = [0u8; AUDIO_TEX_LEN];
        // Row 0: linear-frequency spectrum. The 2048-pt FFT gives 1024 usable
        // bins (DC..Nyquist); average adjacent pairs down to 512, log-compress
        // to 0..1, and apply the same attack/decay smoothing as the bands.
        let (spec_row, wave_row) = audio_tex.split_at_mut(AUDIO_TEX_W);
        for (i, (out, smoothed)) in spec_row.iter_mut().zip(&mut self.spec_smoothed).enumerate() {
            let a = self.buf[2 * i];
            let b = self.buf[2 * i + 1];
            let mag =
                0.5 * ((a.re * a.re + a.im * a.im).sqrt() + (b.re * b.re + b.im * b.im).sqrt());
            let v = ((1.0 + mag).ln() / SPEC_LOG_SCALE).clamp(0.0, 1.0);
            if v > *smoothed {
                *smoothed = *smoothed * (1.0 - ATTACK) + v * ATTACK;
            } else {
                *smoothed *= DECAY;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                *out = (*smoothed * 255.0).round() as u8;
            }
        }
        // Row 1: waveform = the most recent 512 raw (un-windowed) samples,
        // mapped to 0..1 centered on 0.5.
        for (out, &s) in wave_row.iter_mut().zip(&self.samples[FFT_SIZE - AUDIO_TEX_W..]) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                *out = ((s * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }

        self.out = AudioFrame {
            bands: self.smoothed,
            level: self.smoothed[0] + self.smoothed[1] + self.smoothed[2],
            audio_tex,
        };
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[test]
    fn log_bands_valid_across_sample_rates() {
        for sr in [8000.0, 16000.0, 22050.0, 24000.0, 32000.0, 44100.0, 48000.0, 96000.0] {
            for (lo, hi) in log_bands(sr) {
                assert!(hi > lo, "sr {sr}: band {lo}..{hi} inverted");
                assert!((1..=FFT_SIZE / 2).contains(&lo));
                assert!(hi <= FFT_SIZE / 2);
            }
        }
    }

    /// A sine at a known frequency must light the band that contains it and
    /// leave the far ends of the spectrum alone. This is the assertion that
    /// would catch a windowing or bin-mapping mistake, which nothing else here
    /// can see — a wrong mapping still produces plausible-looking bars.
    #[test]
    fn a_tone_lands_in_the_band_that_contains_it() {
        const SR: f32 = 48000.0;
        let mut a = Analyzer::new(SR);
        let tone: Vec<f32> = (0..FFT_SIZE * 2)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR).sin())
            .collect();
        a.feed(&tone);
        while a.poll().is_some() {}
        let bands = a.frame().bands;

        // 1 kHz: bin 1000 * 2048 / 48000 ~= 42.7.
        let want = log_bands(SR)
            .iter()
            .position(|&(lo, hi)| (lo..hi).contains(&43))
            .expect("1 kHz falls in some band");
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(loudest, want, "1 kHz lit band {loudest}, expected {want}: {bands:?}");
        assert!(bands[0] < bands[want] * 0.1, "the bottom band picked up a 1 kHz tone");
        assert!(bands[NUM_BANDS - 1] < bands[want] * 0.1, "the top band did too");
    }

    #[test]
    fn silence_stays_silent_and_the_waveform_row_stays_centred() {
        let mut a = Analyzer::new(48000.0);
        a.feed(&vec![0.0; 48000 / 60]);
        let f = a.poll().expect("one hop in, one frame out");
        assert_eq!(f.level, 0.0);
        assert!(f.audio_tex[..AUDIO_TEX_W].iter().all(|&v| v == 0), "spectrum row not silent");
        assert!(
            f.audio_tex[AUDIO_TEX_W..].iter().all(|&v| v == 128),
            "waveform row is not centred on 0.5"
        );
    }

    #[test]
    fn a_partial_hop_produces_nothing_and_is_not_lost() {
        let mut a = Analyzer::new(48000.0);
        let hop = a.hop();
        a.feed(&vec![0.5; hop - 1]);
        assert!(a.poll().is_none(), "produced a frame from less than a hop");
        a.feed(&[0.5]);
        assert!(a.poll().is_some(), "the buffered remainder was dropped");
    }

    /// A source that stops delivering must fall to silence rather than holding
    /// its last value — a latched `lvl` looks exactly like a hung renderer.
    #[test]
    fn a_dead_source_decays_to_silence() {
        let mut a = Analyzer::new(48000.0);
        let tone: Vec<f32> = (0..FFT_SIZE * 2)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin())
            .collect();
        a.feed(&tone);
        while a.poll().is_some() {}
        let peak = a.frame().level;
        assert!(peak > 0.0, "the tone did not register");

        for _ in 0..120 {
            a.feed_silence();
            a.poll();
        }
        let after = a.frame().level;
        assert!(after < peak * 0.01, "level held at {after} from a peak of {peak}");
    }

    /// A producer that outruns the consumer must lose old samples, not new
    /// ones — otherwise the visuals fall progressively further behind the music
    /// and never recover.
    #[test]
    fn a_backlog_is_bounded() {
        let mut a = Analyzer::new(48000.0);
        let hop = a.hop();
        a.feed(&vec![0.0; hop * 100]);
        let mut frames = 0;
        while a.poll().is_some() {
            frames += 1;
        }
        assert_eq!(frames, MAX_PENDING_HOPS, "the backlog was not capped at {MAX_PENDING_HOPS}");
    }
}
