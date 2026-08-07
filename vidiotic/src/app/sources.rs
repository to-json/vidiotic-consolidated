//! The native side of [`vidiotic_play::engine::source`]: turning a cue into a
//! decode worker or a camera tap.
//!
//! This is the whole of what the native shell knows about playback that the
//! browser does not. Everything above it — when a cue arms, which cue is
//! current, when to restart on the beat grid — is the engine's, and identical
//! in both.

use std::cell::RefCell;
use std::rc::Rc;

use vidiotic_play::clippool::ClipSource;
use vidiotic_play::engine::{OpenRequest, Opener, Source};

use crate::video::{capture, decoder, CameraSource, FileSource};

/// Shared ownership of the capture registry.
///
/// `Rc<RefCell<_>>` rather than a field on either side, because both need it:
/// the opener taps it when a camera cue arms, and the app toggles devices on
/// air and reads their status for the mirror. `Rc` is honest here — the app and
/// its opener live and die on the event-loop thread together, and the capture
/// services that *are* threaded keep their own synchronisation behind this.
pub type Captures = Rc<RefCell<capture::CaptureRegistry>>;

/// Opens cue sources against the local machine.
pub struct NativeSources {
    pub captures: Captures,
}

impl Opener for NativeSources {
    fn open(&mut self, req: &OpenRequest<'_>) -> Option<Box<dyn Source>> {
        match &req.clip.source {
            // A fresh tap onto the device's ring, starting at the dialed delay
            // (slew only handles later changes). An off-air device gives no tap,
            // and returning `None` is how the engine is told to keep asking —
            // `App::resolve_camera_delays` retries every tick, so toggling a
            // device on picks up a cue that armed while it was off.
            ClipSource::Camera { uid, .. } => {
                let mut tap = self.captures.borrow().tap(uid)?;
                tap.delay_eff = req.cue.delay.seconds_capped(req.bpm);
                Some(Box::new(CameraSource(tap)))
            }
            ClipSource::File(path) => {
                match decoder::spawn(path.clone(), req.in_sec, req.out_sec, req.speed) {
                    Ok(h) => Some(Box::new(FileSource(h))),
                    Err(e) => {
                        log::error!(
                            "decode spawn for cue {} (clip {}): {e:#}",
                            req.cue.id,
                            req.cue.clip
                        );
                        None
                    }
                }
            }
        }
    }
}
