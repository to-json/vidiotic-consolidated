//! Sync source, audio device, tap tempo, and loop cadence.

use super::*;

impl App {
    pub(super) fn set_sync_source(&mut self, kind: SyncKind) {
        if kind == self.engine.sync {
            return;
        }
        let snap = self.engine.clock.snapshot();
        self.engine.clock = match kind {
            SyncKind::Internal => Box::new(InternalClock::from_snapshot(&snap)),
            SyncKind::Link => Box::new(LinkClock::new(snap.bpm, self.engine.time_sig.quantum())),
        };
        self.engine.sync = kind;
        self.engine.sequencer.reset_boundary(); // beat numbering may jump on switch
        self.engine.loop_tracker.reset();
        log::info!("sync source: {kind:?}");
    }

    pub(super) fn switch_audio_device(&mut self, name: Option<String>) {
        let (err_tx, err_rx) = crossbeam_channel::bounded::<cpal::Error>(8);
        match audio::build_capture(
            &self.host,
            None,
            name.as_deref(),
            &self.audio_ctl_tx,
            err_tx,
        ) {
            Ok(cap) => {
                log::info!("audio switched to '{}'", cap.device_name);
                self.audio_capture = cap;
                self.audio_err_rx = err_rx;
            }
            Err(e) => log::warn!("audio device switch failed: {e:#}"),
        }
    }
}
