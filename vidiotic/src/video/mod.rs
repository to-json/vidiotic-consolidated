//! Video subsystem: HAP frame parsing (`hap`), per-clip decode workers
//! (`decoder`), camera capture (`capture`), and the plain-data frame types
//! they hand to the renderer (`frame`).

#[cfg(target_os = "macos")]
pub mod capture;
pub mod decoder;

/// Hap1 bitstream parsing and the decoded-frame types it produces. Both live in
/// `vidiotic-play` (which is where `hap` is now re-exported from
/// `vidiotic-bake`), so the browser build gets them without this module's
/// ffmpeg decoder or its `AVFoundation` capture stack. Re-exported here so
/// `crate::video::frame::…` and `crate::video::hap::…` are unchanged.
pub use vidiotic_play::video::{frame, hap};

/// Non-macOS stub with the same shape as `capture`, so the app compiles
/// without platform cfg noise: no devices enumerate, taps never yield, the
/// registry is inert.
#[cfg(not(target_os = "macos"))]
pub mod capture {
    use std::time::Instant;

    use crate::video::frame::DecodedFrame;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Authorization {
        NotDetermined,
        Restricted,
        Denied,
        Authorized,
    }

    #[derive(Debug, Clone)]
    pub struct DeviceFormat {
        pub width: u32,
        pub height: u32,
        pub fourcc: [u8; 4],
        pub min_fps: f64,
        pub max_fps: f64,
    }

    #[derive(Debug, Clone)]
    pub struct DeviceInfo {
        pub index: usize,
        pub uid: String,
        pub name: String,
        pub model_id: String,
        pub device_type: String,
        pub muxed: bool,
        pub formats: Vec<DeviceFormat>,
    }

    #[derive(Debug, Clone)]
    pub enum ServiceStatus {
        Starting,
        Running { width: u32, height: u32, fps: f64 },
        Failed(String),
    }

    pub struct CameraTap {
        pub delay_eff: f64,
    }

    impl CameraTap {
        /// Stub: never yields a frame, matching a tap on a device that never
        /// starts.
        pub fn poll(&mut self, _now: Instant) -> Option<DecodedFrame> {
            None
        }
    }

    #[derive(Default)]
    pub struct CaptureRegistry;

    impl CaptureRegistry {
        /// Stub: no-op, so callers built against the real registry compile
        /// unchanged; there is never anything to turn on.
        pub fn set_on_air(&mut self, _uid: &str, _on: bool) {}
        /// Stub: always `false`, since [`set_on_air`](Self::set_on_air) never
        /// starts anything.
        pub fn is_on_air(&self, _uid: &str) -> bool {
            false
        }
        /// Stub: always `None`, matching an empty registry.
        pub fn tap(&self, _uid: &str) -> Option<CameraTap> {
            None
        }
        /// Stub: always `None`, matching an empty registry.
        pub fn status(&self, _uid: &str) -> Option<ServiceStatus> {
            None
        }
    }

    /// Stub: always denied, since there is no capture backend to ask.
    pub fn authorization() -> Authorization {
        Authorization::Denied
    }

    /// Stub: no-op; there is no system prompt to trigger on this platform.
    pub fn request_access(_on_result: impl Fn(bool) + 'static) {}

    /// Stub: always empty, since no devices can ever be found.
    pub fn enumerate() -> Vec<DeviceInfo> {
        Vec::new()
    }

    /// Move `current` toward `target` by at most `rate * dt`, same contract as
    /// the real `capture::slew` this mirrors.
    pub fn slew(current: f64, target: f64, dt: f64, rate: f64) -> f64 {
        let step = (rate * dt).max(0.0);
        let diff = target - current;
        if diff.abs() <= step {
            target
        } else {
            current + step * diff.signum()
        }
    }
}

use std::time::Instant;

use frame::DecodedFrame;
use vidiotic_play::engine::Source;

/// A per-cue file decode worker, as the engine sees it.
///
/// The engine holds `Box<dyn Source>` and never learns which kind it got — the
/// whole reason the browser can hand it a `.mov` in memory instead. What used to
/// be a two-variant enum with a match in every method is now two implementations
/// of the same trait, which is also what makes the camera exemptions below read
/// as statements about cameras rather than as holes in a file decoder.
pub struct FileSource(pub decoder::DecodeHandle);

impl Source for FileSource {
    fn request_restart(&mut self) {
        self.0.request_restart();
    }

    /// Drain the decode channel newest-wins: the worker paces to the clip's
    /// timeline, so anything older than the last frame in the queue is already
    /// stale by the time this tick draws.
    fn poll_newest(&mut self, _now: Instant) -> Option<DecodedFrame> {
        let mut newest = None;
        while let Ok(f) = self.0.frames.try_recv() {
            newest = Some(f);
        }
        newest
    }
}

/// A per-cue tap onto a shared camera capture service.
///
/// Everything a live feed cannot do is a default method it declines to
/// override: there is no timeline, so there is nothing to restart, pause or
/// seek. What it does have and a file does not is a delay behind the live edge,
/// which is what `delay_eff` reports.
pub struct CameraSource(pub capture::CameraTap);

impl Source for CameraSource {
    /// No-op: a live feed has nothing to seek back to.
    fn request_restart(&mut self) {}

    fn poll_newest(&mut self, now: Instant) -> Option<DecodedFrame> {
        self.0.poll(now)
    }

    fn delay_eff(&self) -> Option<f64> {
        Some(self.0.delay_eff)
    }

    fn set_delay_eff(&mut self, sec: f64) {
        self.0.delay_eff = sec;
    }
}
