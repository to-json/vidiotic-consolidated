//! The browser side of [`crate::engine::source`]: a `.mov` in memory, walked by
//! the render loop.
//!
//! There is no thread here and no channel, and that is the whole difference from
//! the native opener. A native cue spawns a worker that paces frames into a
//! bounded queue; here the engine asks for a frame once per rAF callback and the
//! answer is computed on the spot — which is *cheaper*, because a 30 fps clip on
//! a 60 Hz display returns `None` half the time and costs nothing.
//!
//! Clips are keyed by [`ClipId`] rather than by path. A browser has no paths:
//! bytes arrive from a drop, a file input, or OPFS, and the engine's pool entry
//! carries a synthetic [`ClipSource::File`] whose "path" is only ever a display
//! name. The map below is what the name actually resolves against.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use web_time::Instant;

use crate::chain::ClipId;
use crate::clip::Clip as Movie;
use crate::clippool::ClipSource;
use crate::engine::{OpenRequest, Opener, Source};
use crate::video::frame::DecodedFrame;
use crate::video::softdec;

/// The bytes and probe of one loaded clip.
///
/// The bytes are `Rc` because every cue on the same clip opens its own
/// [`Movie`] over them — two cues can trim the same file differently — and
/// copying 6 MB per cue to say so would be absurd.
#[derive(Clone)]
pub struct Loaded {
    pub bytes: Rc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub duration: f64,
}

/// Everything the page has handed the engine, by clip id.
pub type Library = Rc<RefCell<HashMap<ClipId, Loaded>>>;

/// A flag shared between the shell and every open source.
///
/// Both of the ones below have to reach clips that are *already playing*, and
/// one of them has to reach clips that are not open yet: the sequencer arms and
/// disarms sources as the rotation turns, so a player built two bars from now
/// must come up in the state the shell is already in. Handing the cell to the
/// opener is what makes that automatic instead of a thing to remember.
pub type Flag = Rc<Cell<bool>>;

/// Whether to expand HAP's blocks on the CPU — a device fact the panel can also
/// force (`?soft=1`). [`ClipPlayer::poll_newest`] re-decodes when it changes
/// rather than waiting for the timeline to move.
pub type SoftFlag = Flag;

pub struct WebSources {
    pub library: Library,
    pub soft: SoftFlag,
    /// Whether the clips are held. Global by construction: pause here is a
    /// transport state of the page, not of one cue.
    pub paused: Flag,
    /// Cameras the page has switched on, by device uid.
    pub taps: super::cameras::Taps,
}

impl Opener for WebSources {
    fn open(&mut self, req: &OpenRequest<'_>) -> Option<Box<dyn Source>> {
        // A camera cue is a live device, not a file — a different source with
        // no timeline, resolved through the taps the page has opened rather
        // than the library. A cue on a device that is off air (or on a project's
        // device this machine does not have) opens nothing and the engine
        // blanks for it, which is the same answer as before this existed.
        if let ClipSource::Camera { uid, .. } = &req.clip.source {
            if !self.taps.borrow().contains_key(&**uid) {
                return None;
            }
            return Some(Box::new(super::cameras::CameraSource::new(
                uid.to_string(),
                Rc::clone(&self.taps),
                self.paused.clone(),
            )));
        }
        let ClipSource::File(_) = &req.clip.source else { return None };
        let bytes = self.library.borrow().get(&req.clip.id).map(|l| Rc::clone(&l.bytes))?;
        let movie = match Movie::open(bytes) {
            Ok(m) => m,
            Err(e) => {
                log::error!("cue {} (clip {}): {e}", req.cue.id, req.cue.clip);
                return None;
            }
        };
        let duration = movie.duration_sec();
        // The playable window, honouring trim. A zero or negative span would
        // make `rem_euclid` below divide by nothing, so it collapses to "hold
        // the first sample" rather than to a panic.
        let end = req.out_sec.unwrap_or(duration).min(duration);
        Some(Box::new(ClipPlayer {
            movie,
            in_sec: req.in_sec.min(duration),
            span: (end - req.in_sec).max(0.0),
            speed: if req.speed.is_finite() && req.speed > 0.0 { req.speed } else { 1.0 },
            pos: 0.0,
            last_poll: Instant::now(),
            paused: self.paused.clone(),
            last: None,
            soft: self.soft.clone(),
            last_soft: self.soft.get(),
        }))
    }
}

/// One cue's playback of one clip.
struct ClipPlayer {
    movie: Movie,
    in_sec: f64,
    /// Length of the playable window in seconds; `0.0` means "hold one sample".
    span: f64,
    speed: f64,
    /// Seconds of clip time elapsed since the in-point, before wrapping.
    pos: f64,
    last_poll: Instant,
    paused: Flag,
    /// Which sample the caller last received, so an unchanged timeline costs
    /// one comparison instead of a decode.
    last: Option<usize>,
    soft: SoftFlag,
    last_soft: bool,
}

impl Source for ClipPlayer {
    fn request_restart(&mut self) {
        self.pos = 0.0;
        self.last = None;
    }

    fn poll_newest(&mut self, now: Instant) -> Option<DecodedFrame> {
        // Clamp the step so a backgrounded tab does not fast-forward the clip by
        // however long it was hidden — the same reason the native decode worker
        // paces rather than catching up.
        let dt = now.saturating_duration_since(self.last_poll).as_secs_f64().min(0.25);
        self.last_poll = now;
        if !self.paused.get() {
            self.pos += dt * self.speed;
        }

        let t = if self.span > 0.0 { self.pos.rem_euclid(self.span) } else { 0.0 };
        let idx = self.movie.sample_index_at(self.in_sec + t);
        let soft = self.soft.get();
        if self.last == Some(idx) && soft == self.last_soft {
            return None;
        }
        self.last = Some(idx);
        self.last_soft = soft;

        let frame = match self.movie.frame(idx) {
            Ok(f) => f,
            Err(e) => {
                // One bad sample must not take the session down, and must not
                // scroll past silently either. `last` is already set, so this
                // logs once per sample rather than once per frame.
                log::error!("frame {idx}: {e}");
                return None;
            }
        };
        if !soft {
            return Some(frame);
        }
        // On the fallback path the blocks are expanded here, once per *new*
        // sample rather than once per displayed frame. `to_rgba` is a pure
        // function of the frame, so a failure means the clip is undecodable on
        // this machine, not that this instant went wrong.
        match softdec::to_rgba(&frame) {
            Ok(rgba) => Some(rgba),
            Err(e) => {
                log::error!("software decode: {e}");
                None
            }
        }
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused.set(paused);
    }

    fn seek(&mut self, sec: f64) {
        self.pos = (sec - self.in_sec).max(0.0);
        self.last = None;
    }
}
