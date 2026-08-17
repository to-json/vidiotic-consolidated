//! Camera device tracking, delay resolution, and camera cues.

use super::*;

/// How fast a camera cue's effective delay glides toward its target, in seconds
/// of delay per second of wall clock. Tuned by feel.
const DELAY_SLEW_RATE: f64 = 1.0;

impl App {
    /// Per-tick camera-cue upkeep. Re-attaches taps for camera cues that had
    /// none (the device was off-air when they armed), then moves each tap's
    /// effective delay toward the cue's target: slewing by default, snapping at
    /// loop-grid boundary crossings with the cue's quantize toggle on. Targets
    /// re-resolve every tick, so beats-mode delays track BPM drift musically.
    ///
    /// A camera source is one that reports a [`Source::delay_eff`]; a file
    /// decoder does not, and that is the whole test. It stays in the shell
    /// rather than the engine because delay slewing is meaningless without a
    /// capture service to be behind.
    pub(super) fn resolve_camera_delays(&mut self, boundary_crossed: bool) {
        let now = Instant::now();
        // Clamp dt so a stall (window drag, sleep) doesn't teleport the delay.
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.25);
        self.last_tick = now;

        let want: Vec<CueId> = [self.engine.current, self.engine.sequencer.armed()]
            .into_iter()
            .flatten()
            .filter(|&id| {
                !self.engine.decoders.contains_key(&id)
                    && self
                        .engine
                        .live_cue(id)
                        .is_some_and(|c| self.engine.clip_camera_uid(c.clip).is_some())
            })
            .collect();
        for id in want {
            self.engine.ensure_decoder(id);
        }

        let bpm = self.engine.last_bpm;
        let targets: Vec<(CueId, f64, bool)> = self
            .engine
            .decoders
            .iter()
            .filter(|(_, h)| h.delay_eff().is_some())
            .filter_map(|(&id, _)| {
                let cue = self.engine.live_cue(id)?;
                let target = cue.delay.seconds_capped(bpm);
                Some((id, target, cue.delay.quantize))
            })
            .collect();
        for (id, target, quantize) in targets {
            let is_current = self.engine.current == Some(id);
            let Some(src) = self.engine.decoders.get_mut(&id) else {
                continue;
            };
            let Some(current) = src.delay_eff() else {
                continue;
            };
            if quantize && is_current {
                if boundary_crossed {
                    src.set_delay_eff(target);
                }
            } else if quantize {
                // Not on screen: re-target immediately, nothing to glide.
                src.set_delay_eff(target);
            } else {
                src.set_delay_eff(capture::slew(current, target, dt, DELAY_SLEW_RATE));
            }
        }
    }

    /// Re-enumerate capture devices (startup does one pass; this is the manual
    /// refresh for hotplug).
    pub(super) fn refresh_cameras(&mut self) {
        self.camera_devices = capture::enumerate();
        log::info!("cameras: {} device(s)", self.camera_devices.len());
    }

    /// Toggle a device's capture service. Turning on when permission was never
    /// asked fires the TCC prompt alongside the open attempt.
    pub(super) fn set_camera_on_air(&mut self, uid: &str, on: bool) {
        if on && capture::authorization() == capture::Authorization::NotDetermined {
            capture::request_access(|granted| {
                log::info!("camera access request: granted={granted}");
            });
        }
        self.captures.borrow_mut().set_on_air(uid, on);
    }

    /// Point every clip referencing the missing device `from` at the connected
    /// device `to`, and drop those cues' taps so they re-attach to the new
    /// device's service on the next tick. The engine owns the relink itself
    /// (shared with the browser shell); this adds only the enumeration lookup
    /// and the log for a target that isn't connected.
    pub(super) fn relink_camera(&mut self, from: &str, to: &str) {
        let devices = camera_device_pairs(&self.camera_devices);
        if let Err(msg) = self.engine.relink_camera(&devices, from, to) {
            log::warn!("{msg}");
        }
    }

    /// Add a cue for a capture device to the edit bank, creating the device's
    /// pool clip on first use. The device name comes from the last enumeration,
    /// defaulting to "camera" for a uid it lacks.
    pub(super) fn add_camera_cue(&mut self, uid: &str) {
        let devices = camera_device_pairs(&self.camera_devices);
        self.engine.add_camera_cue(&devices, uid);
    }
}

/// The device enumeration as the engine's camera helpers expect it:
/// borrowed `(uid, name)` pairs, so a per-tick mirror build clones nothing.
/// A free function (not an `App` method) so its borrow covers only the
/// enumeration, leaving `self.engine` free for the mutable call that follows.
pub(super) fn camera_device_pairs(devices: &[capture::DeviceInfo]) -> Vec<(&str, &str)> {
    devices
        .iter()
        .map(|d| (d.uid.as_str(), d.name.as_str()))
        .collect()
}
