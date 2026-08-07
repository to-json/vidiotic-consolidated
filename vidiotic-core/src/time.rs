//! Musical time: the tick grid, time signatures, and the cadence lengths the
//! sequencer's auto-advance and re-loop grids are expressed in.
//!
//! Persisted in `.viproj` (as [`crate::project::CadenceSpec`] / `SyncSpec`) and
//! read by both the player's clock and the span editor's timeline, so it lives
//! in the model rather than in either front end.

/// Resolution of the musical re-loop grid: 32 ticks per beat (quarter note), so
/// an eighth note is 16 ticks and a 4/4 bar is 128. Lets `SetLoopLen` stay an
/// integer while still expressing sub-beat divisions.
pub const LOOP_TICKS_PER_BEAT: u32 = 32;

/// Musical time signature. Tempo (BPM) always counts quarter notes — Link's
/// convention — so the signature only changes the bar length and beat
/// subdivision: `num` notes of `1/den` each per bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSig {
    pub num: u8,
    pub den: u8,
}

/// Denominators the UI offers; a bar is always a whole number of
/// `LOOP_TICKS_PER_BEAT`-scaled ticks for each of these.
pub const TIME_SIG_DENS: [u8; 5] = [1, 2, 4, 8, 16];

impl TimeSig {
    /// Beats (quarter notes) per bar: `num * 4 / den`. Fractional for x/8, x/16
    /// signatures (e.g. 7/8 is 3.5 beats).
    pub fn quantum(self) -> f64 {
        self.num as f64 * 4.0 / self.den as f64
    }

    /// Ticks (`LOOP_TICKS_PER_BEAT`-scaled) per bar — always an integer since
    /// every allowed denominator divides `4 * LOOP_TICKS_PER_BEAT`.
    pub fn bar_ticks(self) -> u32 {
        self.num as u32 * (4 * LOOP_TICKS_PER_BEAT / self.den as u32)
    }

    /// Clamp to a valid signature: `num >= 1`, `den` snapped to the nearest
    /// allowed denominator.
    pub fn sanitized(self) -> Self {
        let num = self.num.max(1);
        let den = TIME_SIG_DENS
            .iter()
            .min_by_key(|&&d| (d as i32 - self.den as i32).abs())
            .copied()
            .unwrap_or(4);
        Self { num, den }
    }
}

impl Default for TimeSig {
    fn default() -> Self {
        Self { num: 4, den: 4 }
    }
}

/// A musical cadence length for the auto-advance / re-loop grids: either an
/// absolute note value, or a count of bars of the current [`TimeSig`] (so it
/// re-resolves automatically when the signature changes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cadence {
    /// Absolute length in `LOOP_TICKS_PER_BEAT`-scaled ticks.
    Note(u32),
    /// Whole bars of the current time signature.
    Bars(u32),
}

impl Cadence {
    /// Resolve to ticks against a time signature.
    pub fn ticks(self, ts: TimeSig) -> u32 {
        match self {
            Self::Note(t) => t,
            Self::Bars(n) => n * ts.bar_ticks(),
        }
    }

    /// Resolve to beats (quarter notes) against a time signature.
    pub fn beats(self, ts: TimeSig) -> f64 {
        self.ticks(ts) as f64 / LOOP_TICKS_PER_BEAT as f64
    }
}

impl Default for Cadence {
    /// Matches the pre-existing default phrase length of 16 beats (4 bars of 4/4).
    fn default() -> Self {
        Self::Bars(4)
    }
}

/// Which `ClockSource` drives the beat grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SyncKind {
    #[default]
    Internal,
    Link,
}

#[cfg(test)]
mod tests {
    use super::*;


    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    // Without it they compile away to nothing and the runner reports "no tests to
    // run!", which reads as success.
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;
    #[test]
    fn time_sig_quantum_and_bar_ticks() {
        let cases = [
            (TimeSig { num: 4, den: 4 }, 4.0, 128),
            (TimeSig { num: 7, den: 8 }, 3.5, 112),
            (TimeSig { num: 3, den: 4 }, 3.0, 96),
            (TimeSig { num: 5, den: 16 }, 1.25, 40),
            (TimeSig { num: 2, den: 1 }, 8.0, 256),
        ];
        for (ts, quantum, ticks) in cases {
            assert_eq!(ts.quantum(), quantum, "{ts:?} quantum");
            assert_eq!(ts.bar_ticks(), ticks, "{ts:?} bar_ticks");
        }
    }

    #[test]
    fn time_sig_sanitized_clamps() {
        assert_eq!(TimeSig { num: 0, den: 4 }.sanitized(), TimeSig { num: 1, den: 4 });
        assert_eq!(TimeSig { num: 4, den: 3 }.sanitized().den, 2);
        assert_eq!(TimeSig { num: 4, den: 32 }.sanitized().den, 16);
        assert_eq!(TimeSig { num: 4, den: 0 }.sanitized().den, 1);
    }

    #[test]
    fn cadence_note_is_signature_independent() {
        let sig_4_4 = TimeSig { num: 4, den: 4 };
        let sig_7_8 = TimeSig { num: 7, den: 8 };
        assert_eq!(Cadence::Note(32).ticks(sig_4_4), 32);
        assert_eq!(Cadence::Note(32).ticks(sig_7_8), 32);
    }

    #[test]
    fn cadence_bars_tracks_signature() {
        let sig_7_8 = TimeSig { num: 7, den: 8 };
        assert_eq!(Cadence::Bars(2).ticks(sig_7_8), 224);
        assert_eq!(Cadence::Bars(2).beats(sig_7_8), 7.0);
    }
}
