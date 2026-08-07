//! Audio analysis thread: pull mono samples from the capture ring and hand them
//! to the portable analyser, publishing frames to the render thread wait-free.
//!
//! **The analysis itself is not here** — it moved to
//! [`vidiotic_play::analysis`], because it is arithmetic and both shells need
//! exactly the same arithmetic. What stays is the half that is a fact about this
//! machine: a cpal device, a lock-free ring, a thread, and a triple buffer. The
//! browser replaces all four and shares none of them, and shares every line of
//! the FFT.
//!
//! Band edges are recomputed from the capture device's actual sample rate on
//! every source swap, which is [`Analyzer::set_sample_rate`]'s job.

use std::time::Duration;

/// The analysis model, re-exported so `crate::analysis::…` keeps resolving for
/// `app.rs` and `spike_render.rs` and the two halves cannot disagree about the
/// texture geometry or the band count.
pub use vidiotic_play::analysis::{
    AudioFrame, Analyzer, AUDIO_TEX_LEN, AUDIO_TEX_W, FFT_SIZE, NUM_BANDS,
};

/// Control messages from the main thread to the analysis thread.
pub enum AudioCtl {
    /// A new capture source: its ring consumer and sample rate. The old consumer
    /// is dropped, band edges recomputed, and smoothing state reset.
    SwapSource {
        consumer: rtrb::Consumer<f32>,
        sample_rate: u32,
    },
    Shutdown,
}

/// Analysis thread body: FFT the ring's newest samples at ~60 Hz and publish
/// `AudioFrame`s until shutdown (or every control sender is dropped).
pub fn run(
    ctl_rx: crossbeam_channel::Receiver<AudioCtl>,
    mut tri_in: triple_buffer::Input<AudioFrame>,
) {
    let mut analyzer = Analyzer::new(48000.0);
    let mut cons: Option<rtrb::Consumer<f32>> = None;

    loop {
        match ctl_rx.try_recv() {
            Ok(AudioCtl::SwapSource {
                consumer,
                sample_rate,
            }) => {
                cons = Some(consumer);
                analyzer.set_sample_rate(sample_rate as f32);
            }
            Ok(AudioCtl::Shutdown) => return,
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => return,
        }

        let Some(c) = cons.as_mut() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let hop = analyzer.hop();
        if c.slots() < hop {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        // One hop per iteration rather than draining the ring: the sleep above
        // is what paces this loop to the capture rate, and swallowing a backlog
        // in one pass would just spin.
        if let Ok(chunk) = c.read_chunk(hop) {
            let (a, b) = chunk.as_slices();
            analyzer.feed(a);
            analyzer.feed(b);
            chunk.commit_all();
        }
        if let Some(frame) = analyzer.poll() {
            tri_in.write(*frame);
        }
    }
}
