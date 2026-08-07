//! Cameras in a browser: a `MediaStream` behind a `<video>`, sampled per frame.
//!
//! # What is shared with the desktop, and what is not
//!
//! Everything about *what a camera cue is* — that it is a pool clip with a
//! [`ClipSource::Camera`](crate::clippool::ClipSource), that a cue on it joins
//! the rotation like any other, that its timeline knobs are inert and its delay
//! is not — lives in the engine and was already here. What this module adds is
//! the one thing that genuinely differs: where the pixels come from.
//!
//! Natively that is an AVFoundation device behind a capture service on its own
//! thread. Here it is `getUserMedia`, which hands back a `MediaStream` that
//! only a media element can play, so the page attaches it to a hidden `<video>`
//! and this samples that element. Same seam as
//! [`ClipPlayer`](super::sources::ClipPlayer): a third `Opener` implementation,
//! not a second engine.
//!
//! # Why a canvas readback
//!
//! [`Source::poll_newest`] returns a [`DecodedFrame`], which is CPU-side
//! pixels — so the video element is drawn into a 2-D canvas and read back with
//! `getImageData`. That is a real cost, and it is a deliberate one: the
//! alternative is `copyExternalImageToTexture`, which would put a GPU upload
//! path behind a trait whose whole contract is "hand me a frame", and would
//! then need a second one for every clip that is not a camera.
//!
//! The frame arrives as [`PixelData::Rgba`], which is exactly what the software
//! HAP path produces, so nothing downstream is new: `preamble.frag`'s `video()`
//! sees `video_mode` 0 and samples it straight.
//!
//! # Permission, and the two-step enumeration
//!
//! `enumerateDevices` reports devices before permission is granted, but with
//! **empty labels** — a privacy rule, not a bug. So a first listing shows
//! positional names, and the page re-enumerates once a stream is granted, when
//! the real labels appear. The uid is `deviceId`, which is stable per origin
//! and is what a `.viproj` stores.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_time::Instant;

use super::sources::Flag;
use crate::engine::Source;
use crate::video::frame::{DecodedFrame, PixelData};

/// One capture device the page has told us about.
#[derive(Clone, Debug)]
pub struct Device {
    pub uid: String,
    pub name: String,
}

/// A live camera: the element playing it, and the canvas it is sampled through.
pub struct Tap {
    video: web_sys::HtmlVideoElement,
    canvas: web_sys::HtmlCanvasElement,
    ctx: web_sys::CanvasRenderingContext2d,
    /// Last size the canvas was sized to, so a resolution change re-sizes it
    /// rather than silently cropping.
    size: (u32, u32),
    /// What the row says. Filled once the element reports a real size.
    pub status: String,
}

/// Live cameras by device uid, shared between the shell and the opener.
///
/// Shared for the same reason [`Library`](super::sources::Library) is: the
/// sequencer arms sources as the rotation turns, so a camera cue that opens two
/// bars from now must find the tap the visitor switched on just now.
pub type Taps = Rc<RefCell<HashMap<String, Tap>>>;

impl Tap {
    /// Wrap a `<video>` the page has already attached a stream to and started.
    ///
    /// # Errors
    /// If the document will not make a canvas, or has no 2-D context — both of
    /// which mean this browser cannot sample a camera at all.
    pub fn new(video: web_sys::HtmlVideoElement) -> Result<Self, String> {
        let doc = super::window().document().ok_or("no document")?;
        let canvas: web_sys::HtmlCanvasElement = doc
            .create_element("canvas")
            .map_err(|e| format!("{e:?}"))?
            .dyn_into()
            .map_err(|_| "not a canvas".to_string())?;
        let ctx: web_sys::CanvasRenderingContext2d = canvas
            .get_context("2d")
            .map_err(|e| format!("{e:?}"))?
            .ok_or("no 2d context")?
            .dyn_into()
            .map_err(|_| "not a 2d context".to_string())?;
        Ok(Self { video, canvas, ctx, size: (0, 0), status: "starting…".to_string() })
    }

    /// Draw the current video frame and read it back as RGBA.
    ///
    /// `None` while the element has no picture yet — `videoWidth` is 0 until
    /// metadata arrives, and drawing a zero-sized source throws.
    fn grab(&mut self) -> Option<DecodedFrame> {
        let (w, h) = (self.video.video_width(), self.video.video_height());
        if w == 0 || h == 0 {
            return None;
        }
        if self.size != (w, h) {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
            self.size = (w, h);
            self.status = format!("{w}x{h}");
        }
        if let Err(e) = self.ctx.draw_image_with_html_video_element(&self.video, 0.0, 0.0) {
            self.status = format!("error: {e:?}");
            return None;
        }
        let data = match self.ctx.get_image_data(0.0, 0.0, f64::from(w), f64::from(h)) {
            Ok(d) => d,
            Err(e) => {
                // The one plausible cause is a tainted canvas, which cannot
                // happen for a `getUserMedia` stream — so this is worth saying
                // out loud rather than showing as a black cue.
                self.status = format!("error: {e:?}");
                return None;
            }
        };
        Some(DecodedFrame {
            pixels: PixelData::Rgba { data: data.data().0, stride: w * 4 },
            w,
            h,
            pts_sec: 0.0,
        })
    }
}

/// One cue's view of a live camera.
///
/// No timeline: a camera has no in-point, no wrap and no duration, so
/// [`Source::seek`] and [`Source::request_restart`] have nothing to do. That is
/// true natively too — it is why `CueParam` marks a camera cue's timeline knobs
/// inert and gives it a delay instead.
pub struct CameraSource {
    uid: String,
    taps: Taps,
    paused: Flag,
    /// When the last frame was taken, so a held camera is not resampled and a
    /// 60 Hz rAF loop does not read back a 30 fps device twice per frame.
    last: Option<Instant>,
    /// Minimum spacing between readbacks.
    interval: f64,
}

impl CameraSource {
    pub fn new(uid: String, taps: Taps, paused: Flag) -> Self {
        // 60 Hz. Higher than any device this is likely to see, so the cap only
        // ever removes duplicate readbacks rather than dropping frames — the
        // element holds the newest frame either way, so sampling faster than it
        // produces costs a full `getImageData` to learn nothing.
        Self { uid, taps, paused, last: None, interval: 1.0 / 60.0 }
    }
}

impl Source for CameraSource {
    fn request_restart(&mut self) {}

    fn seek(&mut self, _sec: f64) {}

    fn set_paused(&mut self, paused: bool) {
        self.paused.set(paused);
    }

    fn poll_newest(&mut self, now: Instant) -> Option<DecodedFrame> {
        // Held means held: the last frame stays on screen, which is what pause
        // does for a clip. Nothing is buffered, so there is nothing to catch up
        // on when it resumes — a camera resumes *live*, not where it stopped.
        if self.paused.get() {
            return None;
        }
        if let Some(t) = self.last {
            if now.saturating_duration_since(t).as_secs_f64() < self.interval {
                return None;
            }
        }
        self.last = Some(now);
        self.taps.borrow_mut().get_mut(&self.uid)?.grab()
    }
}
