//! Beat clock. Everything the app quantizes to comes from a `ClockSource`:
//! the internal host-time clock and Ableton Link behind the same small trait
//! (kept small so Pro DJ Link can slot in beside them). All timing is derived
//! from host time, never frame counts, so it survives frame-rate variation and
//! can be quantized.
//!
//! `web_time::Instant` rather than `std::time::Instant`: the std one *compiles*
//! for wasm32 and panics on the first `now()`, so the native build would stay
//! green while the browser died on the first tick. `web-time` re-exports std
//! verbatim off the web, so this is one implementation and not a fork.

use web_time::Instant;

/// One coherent reading of a clock, taken once per engine tick.
#[derive(Clone, Copy, Debug)]
pub struct ClockSnapshot {
    pub bpm: f64,
    /// Continuous beats since the anchor. Only jumps backwards on a tap/phase reset.
    pub beat: f64,
    /// Position within the quantum: `beat.rem_euclid(quantum)`.
    pub phase: f64,
    /// Beats per cycle the source aligns to (a bar = 4).
    pub quantum: f64,
    pub is_playing: bool,
}

/// What a clock source allows, so the UI can grey out unsupported controls
/// (a MIDI-clock follower, say, could not set tempo).
#[derive(Clone, Copy, Debug)]
pub struct ClockCaps {
    pub can_set_tempo: bool,
    pub can_set_phase: bool,
    /// Connected Link peers (0 for non-networked sources).
    pub peers: u64,
}

/// A tempo/phase source the engine can quantize to. All methods may mutate
/// because sources like Link capture session state on read.
pub trait ClockSource {
    /// Read the current tempo/beat/phase.
    fn snapshot(&mut self) -> ClockSnapshot;
    /// Set an absolute tempo, clamped to the source's supported range.
    fn set_bpm(&mut self, bpm: f64);
    /// Change the bar length in beats (a time-signature edit). Beat
    /// continuity is unaffected; only phase/downbeat derivation changes.
    fn set_quantum(&mut self, quantum: f64);
    /// Multiply tempo by `1 + ratio`; `ratio = ±0.001` for the ±0.1% controls.
    fn nudge_bpm(&mut self, ratio: f64);
    /// Make "now" an exact quantum (bar) boundary — sets the downbeat anchor.
    fn tap_downbeat(&mut self);
    /// Reset the grid to its origin: `beat = 0` — beat one of bar one, phrase one.
    fn reset(&mut self);
    /// What this source supports (see `ClockCaps`).
    fn caps(&self) -> ClockCaps;
}

const BPM_MIN: f64 = 20.0;
const BPM_MAX: f64 = 1000.0;

/// App-owned clock: beats accrue from host time at the current tempo, anchored
/// so tempo changes and taps never re-price already-elapsed time.
pub struct InternalClock {
    anchor: Instant,
    bpm: f64,
    beats_at_anchor: f64,
    quantum: f64,
}

impl InternalClock {
    /// Start anchored at `Instant::now()` with zero beats elapsed.
    pub fn new(bpm: f64, quantum: f64) -> Self {
        Self {
            anchor: Instant::now(),
            bpm,
            beats_at_anchor: 0.0,
            quantum,
        }
    }

    /// Seed from another clock's snapshot when switching sync source (continuity).
    pub fn from_snapshot(s: &ClockSnapshot) -> Self {
        Self {
            anchor: Instant::now(),
            bpm: s.bpm,
            beats_at_anchor: s.beat,
            quantum: s.quantum,
        }
    }

    fn beat_now(&self) -> f64 {
        self.beats_at_anchor + self.anchor.elapsed().as_secs_f64() * self.bpm / 60.0
    }

    /// Fold elapsed time into `beats_at_anchor` at the OLD tempo, then move the
    /// anchor to now. A subsequent bpm change then cannot re-price already-elapsed
    /// time, so `beat` stays continuous across tempo changes.
    fn reanchor(&mut self) {
        let now = Instant::now();
        self.beats_at_anchor += now.duration_since(self.anchor).as_secs_f64() * self.bpm / 60.0;
        self.anchor = now;
    }
}

impl ClockSource for InternalClock {
    fn snapshot(&mut self) -> ClockSnapshot {
        let beat = self.beat_now();
        ClockSnapshot {
            bpm: self.bpm,
            beat,
            phase: beat.rem_euclid(self.quantum),
            quantum: self.quantum,
            is_playing: true,
        }
    }

    fn set_bpm(&mut self, bpm: f64) {
        self.reanchor();
        self.bpm = bpm.clamp(BPM_MIN, BPM_MAX);
    }

    fn set_quantum(&mut self, quantum: f64) {
        self.quantum = quantum.max(0.25);
    }

    fn nudge_bpm(&mut self, ratio: f64) {
        self.reanchor();
        self.bpm = (self.bpm * (1.0 + ratio)).clamp(BPM_MIN, BPM_MAX);
    }

    /// Round the current beat to the NEAREST quantum multiple: a tap 0.3 beats
    /// after the true downbeat snaps back -0.3 rather than jumping +3.7 forward.
    /// Worst-case correction is quantum/2, and `beat` may step backwards by up to
    /// that much — `BoundaryTracker` absorbs it without firing a transition.
    fn tap_downbeat(&mut self) {
        self.reanchor();
        self.beats_at_anchor = (self.beats_at_anchor / self.quantum).round() * self.quantum;
    }

    fn reset(&mut self) {
        self.anchor = Instant::now();
        self.beats_at_anchor = 0.0;
    }

    fn caps(&self) -> ClockCaps {
        ClockCaps {
            can_set_tempo: true,
            can_set_phase: true,
            peers: 0,
        }
    }
}

/// Ableton Link clock: follows a shared session's tempo and phase. rekordbox 6+
/// in Performance mode speaks Link, as do Ableton Live and many apps. We always
/// report `is_playing = true` — VJ visuals should keep running regardless of the
/// session's transport (start/stop) state.
///
/// Listen-only: we capture the session state to read it but never commit, so a
/// VJ visualizer can never hijack the DAW/DJ's master tempo or phase. All the
/// mutating trait methods are deliberate no-ops and `caps()` reports the source
/// as read-only, so the UI greys out the tempo/phase controls.
///
/// Native only. Link is a LAN protocol over UDP multicast and a browser has no
/// way to speak it — not a missing binding, a missing capability. Cfg'd out
/// rather than stubbed so `/play` reaching for it is a compile error instead of
/// a clock that silently never ticks; the web's tempo source is
/// [`InternalClock`], with tap and nudge doing the work Link would.
#[cfg(not(target_arch = "wasm32"))]
pub struct LinkClock {
    link: rusty_link::AblLink,
    state: rusty_link::SessionState, // reusable scratch; capture fills it in place
    quantum: f64,
}

#[cfg(not(target_arch = "wasm32"))]
impl LinkClock {
    /// Start Link peer discovery at `initial_bpm`. Actual tempo becomes
    /// whatever the Link session settles on once peers negotiate.
    pub fn new(initial_bpm: f64, quantum: f64) -> Self {
        let link = rusty_link::AblLink::new(initial_bpm);
        link.enable_start_stop_sync(true);
        link.enable(true); // begins peer discovery
        Self {
            link,
            state: rusty_link::SessionState::new(),
            quantum,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ClockSource for LinkClock {
    fn snapshot(&mut self) -> ClockSnapshot {
        let t = self.link.clock_micros();
        self.link.capture_app_session_state(&mut self.state);
        ClockSnapshot {
            bpm: self.state.tempo(),
            beat: self.state.beat_at_time(t, self.quantum),
            phase: self.state.phase_at_time(t, self.quantum),
            quantum: self.quantum,
            is_playing: true,
        }
    }

    // Listen-only: tempo and phase come from the session, so none of the
    // mutators write to it. They stay as no-ops rather than panicking because
    // the engine calls them generically for any source; `caps()` tells the UI
    // to disable the controls that route here.
    fn set_bpm(&mut self, _bpm: f64) {}

    // Local only: the bar length is a display concern, not part of the
    // shared Link session state.
    fn set_quantum(&mut self, quantum: f64) {
        self.quantum = quantum.max(0.25);
    }

    fn nudge_bpm(&mut self, _ratio: f64) {}

    fn tap_downbeat(&mut self) {}

    fn reset(&mut self) {}

    fn caps(&self) -> ClockCaps {
        ClockCaps {
            can_set_tempo: false,
            can_set_phase: false,
            peers: self.link.num_peers(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for LinkClock {
    fn drop(&mut self) {
        self.link.enable(false);
    }
}

/// Tap-tempo: a gap longer than this starts a fresh measurement, and at most
/// this many recent taps are averaged.
const TAP_TIMEOUT: core::time::Duration = core::time::Duration::from_millis(2000);
const TAP_MAX: usize = 8;

/// Derives a tempo from the spacing of recent taps.
///
/// Averaging the *span* over the interval count rather than the mean of the
/// gaps is deliberate: it weights every tap equally and does not let one late
/// tap dominate, which is what a running mean of adjacent gaps does.
///
/// Separate from [`ClockSource`] because it is an estimator, not a clock — it
/// tells you a tempo and leaves applying it to the caller, which is what lets
/// the same code serve a `Box<dyn ClockSource>` natively and a bare
/// [`InternalClock`] in the browser.
#[derive(Default)]
pub struct TapTempo {
    times: Vec<Instant>,
}

impl TapTempo {
    /// Record a tap and return the tempo it implies, if there is one yet.
    ///
    /// `None` on the first tap of a measurement — one tap names no interval —
    /// and on any tap that opens a fresh one after a [`TAP_TIMEOUT`] gap. The
    /// result is clamped to the same range [`InternalClock`] accepts, so a
    /// double-tap cannot produce a nonsense tempo.
    ///
    /// `now` is a parameter rather than read here so a test can drive it.
    pub fn tap(&mut self, now: Instant) -> Option<f64> {
        if self
            .times
            .last()
            .is_some_and(|&last| now.duration_since(last) > TAP_TIMEOUT)
        {
            self.times.clear();
        }
        self.times.push(now);
        if self.times.len() > TAP_MAX {
            let excess = self.times.len() - TAP_MAX;
            self.times.drain(0..excess);
        }
        if self.times.len() < 2 {
            return None;
        }
        let intervals = (self.times.len() - 1) as f64;
        let avg = now.duration_since(self.times[0]).as_secs_f64() / intervals;
        (avg > 0.0).then(|| (60.0 / avg).clamp(BPM_MIN, BPM_MAX))
    }

    /// Abandon the current measurement, so the next tap starts fresh.
    pub fn clear(&mut self) {
        self.times.clear();
    }
}

/// Detects when the beat clock crosses a phrase boundary, tolerating the
/// backwards jumps a tap can cause.
pub struct BoundaryTracker {
    prev_beat: Option<f64>,
}

const BACKWARD_EPS: f64 = 1e-6;

impl BoundaryTracker {
    /// Start with no prior beat, so the first [`Self::crossed`] call only
    /// primes the tracker and never fires.
    pub fn new() -> Self {
        Self { prev_beat: None }
    }

    /// Returns `Some(phrase_index)` exactly once when a phrase boundary is crossed.
    pub fn crossed(&mut self, cur_beat: f64, phrase_len: f64) -> Option<u64> {
        // first frame: prime only, never fire
        let prev = self.prev_beat.replace(cur_beat)?;
        if cur_beat < prev - BACKWARD_EPS {
            // tap round-down / phase renegotiation / rewind: resync silently
            return None;
        }
        let prev_idx = (prev / phrase_len).floor() as i64;
        let cur_idx = (cur_beat / phrase_len).floor() as i64;
        (cur_idx > prev_idx).then_some(cur_idx as u64)
    }

    /// Call on pause, sync-source switch, or phrase-length change.
    pub fn reset(&mut self) {
        self.prev_beat = None;
    }
}

impl Default for BoundaryTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// Let real time pass, portably.
    ///
    /// `thread::sleep` is the obvious spelling and the wrong one: there is no
    /// thread to park in a browser, so it would have cost these two tests their
    /// place in the V8 run. Spinning on the same clock the code under test reads
    /// works everywhere — `performance.now()` advances during a busy loop just as
    /// a monotonic counter does — and it keeps the assertions byte-identical
    /// instead of forking them per target.
    fn spin(ms: u64) {
        let t = Instant::now();
        while t.elapsed() < core::time::Duration::from_millis(ms) {}
    }

    #[test]
    fn first_frame_never_fires() {
        let mut t = BoundaryTracker::new();
        assert_eq!(t.crossed(0.0, 16.0), None);
    }

    #[test]
    fn forward_cross_fires_once() {
        let mut t = BoundaryTracker::new();
        assert_eq!(t.crossed(15.0, 16.0), None); // prime
        assert_eq!(t.crossed(15.9, 16.0), None); // same phrase 0
        assert_eq!(t.crossed(16.1, 16.0), Some(1)); // into phrase 1
        assert_eq!(t.crossed(16.5, 16.0), None); // still phrase 1
    }

    #[test]
    fn backward_jump_does_not_fire() {
        let mut t = BoundaryTracker::new();
        assert_eq!(t.crossed(31.5, 16.0), None); // prime, phrase 1
        // tap snaps beat back to 30.0 (still phrase 1) — must NOT fire
        assert_eq!(t.crossed(30.0, 16.0), None);
        // and continuing forward from 30 into 32 fires once
        assert_eq!(t.crossed(32.2, 16.0), Some(2));
    }

    #[test]
    fn multi_phrase_skip_fires_once() {
        let mut t = BoundaryTracker::new();
        assert_eq!(t.crossed(1.0, 16.0), None); // prime
        // a frame hitch skips from phrase 0 to phrase 3 — fires once
        assert_eq!(t.crossed(50.0, 16.0), Some(3));
    }

    #[test]
    fn internal_clock_bpm_change_is_continuous() {
        let mut c = InternalClock::new(120.0, 4.0);
        let b0 = c.snapshot().beat;
        c.set_bpm(174.0);
        let b1 = c.snapshot().beat;
        // beat must not jump on a tempo change (allow tiny elapsed advance)
        assert!((b1 - b0).abs() < 0.05, "beat jumped by {}", b1 - b0);
        assert_eq!(c.snapshot().bpm, 174.0);
    }

    #[test]
    fn internal_clock_quantum_change_is_continuous() {
        let mut c = InternalClock::new(120.0, 4.0);
        let b0 = c.snapshot().beat;
        c.set_quantum(3.5);
        let s = c.snapshot();
        // beat must not jump on a signature change (allow tiny elapsed advance)
        assert!((s.beat - b0).abs() < 0.05, "beat jumped by {}", s.beat - b0);
        assert_eq!(s.quantum, 3.5);
        assert_eq!(s.phase, s.beat.rem_euclid(3.5));
    }

    #[test]
    fn bpm_clamps_to_max() {
        let mut c = InternalClock::new(120.0, 4.0);
        c.set_bpm(1200.0);
        assert_eq!(c.snapshot().bpm, BPM_MAX);
    }

    // Native only, and not for want of a shim: these exercise the `rusty_link`
    // FFI against a real UDP-multicast session, which a browser cannot join.
    // `LinkClock` itself is cfg'd out there (see its doc comment).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn link_clock_constructs_and_snapshots() {
        // Proves the Ableton Link FFI binding works: construct, follow tempo,
        // read peers. (No assertion on exact tempo — another Link app on the LAN
        // could negotiate it; peers may be >0 for the same reason.)
        let mut c = LinkClock::new(128.0, 4.0);
        let s = c.snapshot();
        assert!(s.bpm.is_finite() && s.bpm > 0.0);
        assert!(s.is_playing);
        assert_eq!(s.quantum, 4.0);
        let _ = c.caps().peers;
    }

    // Native only, and not for want of a shim: these exercise the `rusty_link`
    // FFI against a real UDP-multicast session, which a browser cannot join.
    // `LinkClock` itself is cfg'd out there (see its doc comment).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn link_clock_is_listen_only() {
        // Link must never write back to the session: it reports itself read-only
        // (so the UI greys the tempo/phase controls) and its mutators are no-ops.
        let mut c = LinkClock::new(128.0, 4.0);
        assert!(!c.caps().can_set_tempo);
        assert!(!c.caps().can_set_phase);
        // Calling the mutators must not panic or commit anything.
        c.set_bpm(200.0);
        c.nudge_bpm(0.5);
        c.tap_downbeat();
        c.reset();
    }

    #[test]
    fn tap_snaps_phase_to_boundary() {
        let mut c = InternalClock::new(120.0, 4.0);
        // advance a bit, then tap: phase should be ~0 right after.
        spin(5);
        c.tap_downbeat();
        let phase = c.snapshot().phase;
        assert!(!(0.05..=3.95).contains(&phase), "phase not near boundary: {phase}");
    }

    #[test]
    fn one_tap_names_no_tempo() {
        let mut t = TapTempo::default();
        assert_eq!(t.tap(Instant::now()), None, "a single tap has no interval");
    }

    #[test]
    fn even_taps_give_their_tempo() {
        let mut t = TapTempo::default();
        let t0 = Instant::now();
        // 500 ms apart is 120 bpm exactly.
        let half = core::time::Duration::from_millis(500);
        assert_eq!(t.tap(t0), None);
        let bpm = t.tap(t0 + half).expect("two taps name a tempo");
        assert!((bpm - 120.0).abs() < 1e-9, "got {bpm}");
        // A third on the same grid must not drag it off.
        let bpm = t.tap(t0 + half * 2).expect("three taps");
        assert!((bpm - 120.0).abs() < 1e-9, "got {bpm}");
    }

    #[test]
    fn a_late_tap_does_not_dominate() {
        // Averaging the span over the interval count weights every tap equally.
        // A running mean of adjacent gaps would swing much further on this.
        let mut t = TapTempo::default();
        let t0 = Instant::now();
        let ms = core::time::Duration::from_millis(1);
        t.tap(t0);
        t.tap(t0 + ms * 500);
        t.tap(t0 + ms * 1000);
        let bpm = t.tap(t0 + ms * 1600).expect("four taps"); // 100 ms late
        assert!((100.0..120.0).contains(&bpm), "one late tap swung it to {bpm}");
    }

    #[test]
    fn a_long_gap_starts_a_fresh_measurement() {
        let mut t = TapTempo::default();
        let t0 = Instant::now();
        t.tap(t0);
        t.tap(t0 + core::time::Duration::from_millis(500));
        // Past TAP_TIMEOUT: the run is abandoned, so this is tap one again.
        let late = t0 + TAP_TIMEOUT + core::time::Duration::from_millis(600);
        assert_eq!(t.tap(late), None, "a stale run must not seed the new one");
    }

    #[test]
    fn only_the_last_taps_count() {
        // Past TAP_MAX the window slides, so a tempo change is followed rather
        // than averaged against the whole history.
        let mut t = TapTempo::default();
        let t0 = Instant::now();
        let ms = core::time::Duration::from_millis(1);
        let mut at = t0;
        for _ in 0..12 {
            at += ms * 500; // 120 bpm
            t.tap(at);
        }
        for _ in 0..TAP_MAX {
            at += ms * 250; // 240 bpm
            t.tap(at);
        }
        let bpm = t.tap(at + ms * 250).expect("plenty of taps");
        assert!((bpm - 240.0).abs() < 1.0, "window did not slide: {bpm}");
    }

    #[test]
    fn a_frantic_double_tap_cannot_produce_a_nonsense_tempo() {
        let mut t = TapTempo::default();
        let t0 = Instant::now();
        t.tap(t0);
        let bpm = t.tap(t0 + core::time::Duration::from_micros(1)).expect("two taps");
        assert_eq!(bpm, BPM_MAX, "clamped to the range InternalClock accepts");
    }

    #[test]
    fn reset_returns_to_grid_origin() {
        // A high tempo so a few beats accrue quickly, then reset → beat ~0.
        let mut c = InternalClock::new(600.0, 4.0);
        spin(30);
        assert!(c.snapshot().beat > 0.1, "expected the beat to advance first");
        c.reset();
        let beat = c.snapshot().beat;
        assert!(beat < 0.05, "beat not at origin after reset: {beat}");
    }
}
