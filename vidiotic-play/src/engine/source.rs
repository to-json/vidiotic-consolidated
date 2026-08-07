//! Where a cue's frames come from — the one thing the two shells genuinely
//! disagree about.
//!
//! Everything else in [`crate::engine`] is the same code on both sides. This is
//! not: natively a cue opens a decode worker on a background thread (or a tap
//! onto a shared camera capture service), and in a browser there are no threads
//! at all, so a cue is a `.mov` in memory walked by the render loop. The engine
//! never learns which it got.
//!
//! The split is a trait rather than a `cfg` because both implementations are
//! *live at once* natively — a file cue and a camera cue behave differently in
//! the same session — so the polymorphism has to exist regardless. The browser
//! is then a third implementation rather than a second compilation.
//!
//! `Box<dyn Source>` rather than a generic parameter on `Engine`: the dispatch
//! happens once per cue per frame, which is nothing, and a type parameter here
//! would infect every signature in the engine and both shells for no benefit.

use web_time::Instant;

use crate::bank::Cue;
use crate::clippool::Clip;
use crate::video::frame::DecodedFrame;

/// A cue's live frame source, once opened.
///
/// The default methods are the honest ones for a source with a timeline and no
/// delay ring — a plain file decoder — so a shell only implements what it
/// actually differs on.
pub trait Source {
    /// Restart the source at its in-point: the musical re-loop and hard reset.
    /// A live feed has no timeline, so this is legitimately a no-op there.
    fn request_restart(&mut self);

    /// The newest frame available right now, if any.
    ///
    /// `None` means "nothing new" — the common case, and the reason a 30 fps
    /// clip on a 60 Hz display costs nothing on half its frames. It must not be
    /// read as an error; a source that has failed says so by logging and then
    /// staying quiet, exactly as the decode thread does.
    fn poll_newest(&mut self, now: Instant) -> Option<DecodedFrame>;

    /// This source's effective delay behind the live edge, for sources that
    /// have one. `None` — the default — means "has a timeline instead", which
    /// is what the engine and the camera-delay resolver both key off.
    fn delay_eff(&self) -> Option<f64> {
        None
    }

    /// Move the effective delay. Ignored by anything that reports no delay.
    fn set_delay_eff(&mut self, _sec: f64) {}

    /// Hold or resume the source's own clock.
    ///
    /// Natively nothing calls this: the sequencer always runs, and a paused set
    /// is not a thing the engine has. The browser shell has a play/pause button,
    /// and without this a resume would jump the clip forward by the length of
    /// the pause.
    fn set_paused(&mut self, _paused: bool) {}

    /// Jump to `sec` from the clip's start, when the source can. Default no-op:
    /// the native decode worker has no seek short of a respawn, and a live feed
    /// has nowhere to seek to.
    fn seek(&mut self, _sec: f64) {}
}

/// Everything a shell needs to open a source, resolved by the engine.
///
/// Trim and speed are already resolved against advanced mode here, so an opener
/// never has to know what advanced mode is. `clip` carries the
/// [`crate::clippool::ClipSource`], which is what an opener actually branches
/// on — a path natively, an id the browser looks up its bytes by.
pub struct OpenRequest<'a> {
    pub cue: &'a Cue,
    pub clip: &'a Clip,
    /// In-point in seconds, with the advanced-mode start nudge already folded in
    /// and clamped at zero.
    pub in_sec: f64,
    /// Out-point in seconds, already filtered to "strictly after `in_sec`".
    pub out_sec: Option<f64>,
    /// Playback rate: `1.0` outside advanced mode.
    pub speed: f64,
    /// The session tempo at open time, for resolving beat-relative quantities
    /// (a camera cue's delay) that the engine deliberately leaves in musical
    /// units until the last moment.
    pub bpm: f64,
}

/// Opens sources for cues.
///
/// The engine owns one of these and asks it whenever a cue arms. Returning
/// `None` is a normal answer, not a failure: natively a camera whose device is
/// off-air has no tap to give, and the per-tick resolver retries — which is how
/// toggling a device on-air picks up a cue that is already armed. The engine
/// blanks the output for a cue with no source rather than leaving the previous
/// clip's frame up.
pub trait Opener {
    fn open(&mut self, req: &OpenRequest<'_>) -> Option<Box<dyn Source>>;
}

/// An opener that never opens anything.
///
/// The engine's default, so [`super::Engine::new`] does not require a shell to
/// exist yet — and a usable stand-in for a headless test that only exercises
/// cue bookkeeping.
pub struct NoSources;

impl Opener for NoSources {
    fn open(&mut self, _req: &OpenRequest<'_>) -> Option<Box<dyn Source>> {
        None
    }
}
