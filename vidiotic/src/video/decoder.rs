//! Clip decode worker. One thread per active clip: demux with ffmpeg-next, take
//! the HAP fast path (parse packet -> BC bytes, near-zero CPU) or the software
//! RGBA fallback for other codecs, loop at EOF, and hand frames to the render
//! thread over a small bounded channel, paced to the clip's own timeline.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use ffmpeg_next as ff;

use crate::video::frame::{DecodedFrame, PixelData};
use crate::video::hap;

/// Handle to a running decode worker. Dropping it stops and joins the thread.
pub struct DecodeHandle {
    /// Decoded frames, paced to the clip's timeline (bounded; drain for newest).
    pub frames: Receiver<DecodedFrame>,
    restart_tx: Sender<()>,
    close_tx: Option<Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl DecodeHandle {
    /// Ask the worker to seek back to the clip's start on its next packet — used
    /// by the musical re-loop grid. Non-blocking; a coalesced no-op if pending.
    pub fn request_restart(&self) {
        let _ = self.restart_tx.try_send(());
    }
}

impl Drop for DecodeHandle {
    fn drop(&mut self) {
        // Signal by dropping the sender so the worker's close check disconnects,
        // then join. (Struct fields drop *after* this runs, so drop explicitly.)
        self.close_tx.take();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Spawn a decode worker for one cue. `in_sec`/`out_sec` trim the loop: playback
/// (and every restart) begins at `in_sec` and loops back once it reaches
/// `out_sec` (or the clip's natural end when `out_sec` is `None`). `speed` scales
/// the pacing: `2.0` plays twice as fast, `0.5` half; `1.0` is native.
///
/// # Errors
/// If ffmpeg initialization fails. Per-clip decode failures are logged on the
/// worker thread, not returned here.
pub fn spawn(
    path: PathBuf,
    in_sec: f64,
    out_sec: Option<f64>,
    speed: f64,
) -> anyhow::Result<DecodeHandle> {
    ff::init()?;
    let speed = if speed.is_finite() && speed > 0.0 {
        speed
    } else {
        1.0
    };
    let (frame_tx, frames) = bounded::<DecodedFrame>(3);
    let (close_tx, close_rx) = bounded::<()>(1);
    let (restart_tx, restart_rx) = bounded::<()>(1);
    let trim = Trim {
        in_sec,
        out_sec,
        speed,
    };
    let join = std::thread::spawn(move || {
        if let Err(e) = run(&path, &frame_tx, &close_rx, &restart_rx, trim) {
            log::error!("decode worker for {}: {e:#}", path.display());
        }
    });
    Ok(DecodeHandle {
        frames,
        restart_tx,
        close_tx: Some(close_tx),
        join: Some(join),
    })
}

fn should_stop(close_rx: &Receiver<()>) -> bool {
    !matches!(
        close_rx.try_recv(),
        Err(crossbeam_channel::TryRecvError::Empty)
    )
}

/// Drain any pending restart requests; true if at least one was waiting.
fn take_restart(restart_rx: &Receiver<()>) -> bool {
    let mut hit = false;
    while restart_rx.try_recv().is_ok() {
        hit = true;
    }
    hit
}

/// Send a frame, blocking on a full channel but bailing if asked to stop.
/// Returns true if the worker should exit.
fn send_or_stop(tx: &Sender<DecodedFrame>, close_rx: &Receiver<()>, frame: DecodedFrame) -> bool {
    let mut f = frame;
    loop {
        match tx.try_send(f) {
            Ok(()) => return false,
            Err(TrySendError::Disconnected(_)) => return true,
            Err(TrySendError::Full(returned)) => {
                if should_stop(close_rx) {
                    return true;
                }
                f = returned;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// When the frame at `pts` seconds is due, relative to the first frame of this
/// playthrough. `speed` compresses (>1) or stretches (<1) that timeline: the
/// frame lands at `(pts - first)/speed`.
///
/// Separate from [`pace`] so the arithmetic is testable without sleeping — the
/// only part with anything to get wrong, and the part that decides whether a
/// clip plays at the right rate.
fn due_at(base: Instant, first_pts: &mut Option<f64>, pts: f64, speed: f64) -> Instant {
    let fp = *first_pts.get_or_insert(pts);
    base + Duration::from_secs_f64(((pts - fp).max(0.0)) / speed)
}

/// Sleep so the frame at `pts` seconds appears at the right wall-clock time.
fn pace(base: Instant, first_pts: &mut Option<f64>, pts: f64, speed: f64) {
    let target = due_at(base, first_pts, pts, speed);
    let now = Instant::now();
    if target > now {
        std::thread::sleep(target - now);
    }
}

/// Seek the demuxer to `secs` (clamped at 0), in the container's own timeline.
fn seek_secs(ictx: &mut ff::format::context::Input, secs: f64) -> anyhow::Result<()> {
    let ts = (secs.max(0.0) * 1_000_000.0) as i64; // AV_TIME_BASE microseconds
    ictx.seek(ts, ..)?;
    Ok(())
}

/// What to play: the cue's trim and its playback rate. Travels together because
/// every layer below takes all three or none of them.
#[derive(Clone, Copy)]
struct Trim {
    in_sec: f64,
    out_sec: Option<f64>,
    speed: f64,
}

/// Everything a playthrough needs that does not change between playthroughs.
///
/// The two decode paths — HAP passthrough and software-decode-to-RGBA — differ
/// only in what they do with a packet. Restart handling, the in/out trim
/// comparisons, the timebase conversion and the send-or-stop dance are the same
/// in both, and were written out twice (three times for the trim comparisons,
/// which `run_software` repeats again in its EOF drain). This is the shared half,
/// and it is also what took both functions past the argument count clippy
/// complains about — they carried eleven and ten parameters, nine of them
/// identical.
struct LoopCtx<'a> {
    tx: &'a Sender<DecodedFrame>,
    close_rx: &'a Receiver<()>,
    restart_rx: &'a Receiver<()>,
    /// Index of the video stream in the container; packets from any other
    /// stream are skipped.
    vid_idx: usize,
    /// Stream timebase, as seconds per tick.
    tb: f64,
    trim: Trim,
}

impl LoopCtx<'_> {
    /// A container timestamp in seconds.
    fn secs(&self, ts: Option<i64>) -> f64 {
        ts.unwrap_or(0) as f64 * self.tb
    }

    /// Past the cue's out-point, so this playthrough is over.
    fn past_out(&self, pts: f64) -> bool {
        self.trim.out_sec.is_some_and(|o| pts >= o)
    }

    /// Before the in-point — a keyframe the seek landed on, to be dropped. The
    /// epsilon absorbs the timebase rounding in `secs`.
    fn before_in(&self, pts: f64) -> bool {
        pts + 1e-6 < self.trim.in_sec
    }

    /// Position at the cue's in-point for a fresh playthrough (also the target
    /// of an EOF loop, an out-point loop, or a musical re-loop restart), and
    /// return the wall clock the pacing is relative to.
    ///
    /// Priming the restart signal is the subtle part: a request that arrived
    /// during the seek would otherwise re-fire immediately and never play a frame.
    fn begin(&self, ictx: &mut ff::format::context::Input) -> Instant {
        let _ = seek_secs(ictx, self.trim.in_sec);
        take_restart(self.restart_rx);
        Instant::now()
    }

    fn should_stop(&self) -> bool {
        should_stop(self.close_rx)
    }

    fn take_restart(&self) -> bool {
        take_restart(self.restart_rx)
    }

    /// Sleep until this frame is due, then send it. True if the worker should exit.
    fn pace_and_send(
        &self,
        base: Instant,
        first_pts: &mut Option<f64>,
        pts: f64,
        frame: DecodedFrame,
    ) -> bool {
        pace(base, first_pts, pts, self.trim.speed);
        send_or_stop(self.tx, self.close_rx, frame)
    }
}

fn run(
    path: &Path,
    tx: &Sender<DecodedFrame>,
    close_rx: &Receiver<()>,
    restart_rx: &Receiver<()>,
    trim: Trim,
) -> anyhow::Result<()> {
    let mut ictx = ff::format::input(path)?;

    let (vid_idx, params, time_base) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| anyhow::anyhow!("no video stream"))?;
        (st.index(), st.parameters(), st.time_base())
    };
    let is_hap = params.id() == ff::codec::Id::HAP;
    // SAFETY: `params` (a live `Parameters` from the stream's `best()` lookup)
    // owns the `AVCodecParameters` and outlives this read; its fields are
    // populated by ffmpeg during demuxer open.
    let (fourcc, width, height) = unsafe {
        let p = params.as_ptr();
        (
            (*p).codec_tag.to_le_bytes(),
            (*p).width as u32,
            (*p).height as u32,
        )
    };
    let ctx = LoopCtx {
        tx,
        close_rx,
        restart_rx,
        vid_idx,
        tb: time_base.numerator() as f64 / time_base.denominator() as f64,
        trim,
    };

    if is_hap {
        let texture_count = if &fourcc == b"HapM" { 2 } else { 1 };
        log::info!(
            "clip {}: HAP {:?} {width}x{height}, {texture_count} texture(s)",
            path.display(),
            std::str::from_utf8(&fourcc).unwrap_or("?")
        );
        run_hap(&mut ictx, &ctx, width, height, texture_count)
    } else {
        log::info!(
            "clip {}: software decode {width}x{height} ({:?})",
            path.display(),
            params.id()
        );
        run_software(&mut ictx, &ctx, params)
    }
}

/// Avoid a hot seek-loop when a trim yields no frames (e.g. an in-point past the
/// clip's end): pause briefly before retrying the empty playthrough.
fn guard_empty_playthrough(sent_any: bool) {
    if !sent_any {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Decode a still image file (PNG/JPG/etc.) to a single RGBA [`DecodedFrame`],
/// synchronously. Used for ISF `IMPORTED` textures — one decode at load time,
/// off the per-clip worker path. Reuses the same ffmpeg demux + swscale-to-RGBA
/// pipeline as the software video fallback.
///
/// # Errors
/// If ffmpeg init/open fails, if the file has no video (image) stream, or if no
/// frame decodes.
pub fn decode_still(path: &Path) -> anyhow::Result<DecodedFrame> {
    use ff::format::Pixel;
    use ff::software::scaling;

    ff::init()?;
    let mut ictx = ff::format::input(path)?;
    let stream = ictx
        .streams()
        .best(ff::media::Type::Video)
        .ok_or_else(|| anyhow::anyhow!("no image stream in {}", path.display()))?;
    let vid_idx = stream.index();
    let params = stream.parameters();

    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .video()?;
    let (w, h) = (decoder.width(), decoder.height());
    let mut scaler = scaling::Context::get(
        decoder.format(),
        w,
        h,
        Pixel::RGBA,
        w,
        h,
        scaling::Flags::BILINEAR,
    )?;

    let mut decoded = ff::frame::Video::empty();
    let mut to_rgba = |decoded: &ff::frame::Video| -> anyhow::Result<DecodedFrame> {
        let mut rgba = ff::frame::Video::empty();
        scaler.run(decoded, &mut rgba)?;
        Ok(DecodedFrame {
            pixels: PixelData::Rgba {
                data: rgba.data(0).to_vec(),
                stride: rgba.stride(0) as u32,
            },
            w,
            h,
            pts_sec: 0.0,
        })
    };

    for (stream, packet) in ictx.packets() {
        if stream.index() != vid_idx {
            continue;
        }
        decoder.send_packet(&packet)?;
        if decoder.receive_frame(&mut decoded).is_ok() {
            return to_rgba(&decoded);
        }
    }
    // Flush: single-frame images may only surface the frame at EOF.
    decoder.send_eof()?;
    if decoder.receive_frame(&mut decoded).is_ok() {
        return to_rgba(&decoded);
    }
    anyhow::bail!("no frame decoded from {}", path.display())
}

/// HAP: the packet *is* the texture, so this path never decodes — it parses the
/// container's chunk tables and hands the compressed blocks to the uploader.
fn run_hap(
    ictx: &mut ff::format::context::Input,
    ctx: &LoopCtx<'_>,
    width: u32,
    height: u32,
    texture_count: u8,
) -> anyhow::Result<()> {
    loop {
        let base = ctx.begin(ictx);
        let mut first_pts = None;
        let mut sent_any = false;
        for (stream, packet) in ictx.packets() {
            if ctx.should_stop() {
                return Ok(());
            }
            if ctx.take_restart() {
                break; // musical re-loop: reseek at the top of the loop
            }
            if stream.index() != ctx.vid_idx {
                continue;
            }
            let pts = ctx.secs(packet.pts());
            if ctx.past_out(pts) {
                break;
            }
            if ctx.before_in(pts) {
                continue;
            }
            let Some(bytes) = packet.data() else { continue };

            let mut main = Vec::new();
            let mut alpha = Vec::new();
            let meta = match hap::decode_frame(bytes, texture_count, &mut main, &mut alpha) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("HAP frame parse failed: {e}");
                    continue;
                }
            };

            let frame = DecodedFrame {
                pixels: PixelData::Bc {
                    format: meta.format,
                    data: main,
                    alpha: if meta.has_alpha { Some(alpha) } else { None },
                    video_mode: meta.video_mode,
                },
                w: width,
                h: height,
                pts_sec: pts,
            };
            sent_any = true;
            if ctx.pace_and_send(base, &mut first_pts, pts, frame) {
                return Ok(());
            }
        }
        guard_empty_playthrough(sent_any);
    }
}

/// Everything that is not HAP: ffmpeg decodes, swscale converts to RGBA.
fn run_software(
    ictx: &mut ff::format::context::Input,
    ctx: &LoopCtx<'_>,
    params: ff::codec::Parameters,
) -> anyhow::Result<()> {
    use ff::format::Pixel;
    use ff::software::scaling;

    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .video()?;
    let (w, h) = (decoder.width(), decoder.height());
    let mut scaler = scaling::Context::get(
        decoder.format(),
        w,
        h,
        Pixel::RGBA,
        w,
        h,
        scaling::Flags::BILINEAR,
    )?;

    let send_rgba = |decoded: &ff::frame::Video,
                     scaler: &mut scaling::Context,
                     base: Instant,
                     first_pts: &mut Option<f64>,
                     pts: f64|
     -> anyhow::Result<bool> {
        let mut rgba = ff::frame::Video::empty();
        scaler.run(decoded, &mut rgba)?;
        let stride = rgba.stride(0) as u32;
        let frame = DecodedFrame {
            pixels: PixelData::Rgba {
                data: rgba.data(0).to_vec(),
                stride,
            },
            w,
            h,
            pts_sec: pts,
        };
        Ok(ctx.pace_and_send(base, first_pts, pts, frame))
    };

    loop {
        let base = ctx.begin(ictx);
        // The one thing HAP's playthrough does not need: buffered frames from
        // the previous pass would otherwise arrive before the seek's.
        decoder.flush();
        let mut first_pts = None;
        let mut decoded = ff::frame::Video::empty();
        let mut restarted = false;
        let mut hit_out = false;
        let mut sent_any = false;

        for (stream, packet) in ictx.packets() {
            if ctx.should_stop() {
                return Ok(());
            }
            if ctx.take_restart() {
                restarted = true;
                break; // musical re-loop: reseek at the top of the loop
            }
            if stream.index() != ctx.vid_idx {
                continue;
            }
            if let Err(e) = decoder.send_packet(&packet) {
                log::warn!("decode send_packet failed, skipping packet: {e}");
                continue;
            }
            while decoder.receive_frame(&mut decoded).is_ok() {
                let pts = ctx.secs(decoded.pts());
                if ctx.past_out(pts) {
                    hit_out = true;
                    break;
                }
                if ctx.before_in(pts) {
                    continue; // seek landed before the in-point; drop
                }
                sent_any = true;
                if send_rgba(&decoded, &mut scaler, base, &mut first_pts, pts)? {
                    return Ok(());
                }
            }
            if hit_out {
                break;
            }
        }
        // Natural EOF (not a restart or out-point cut): drain buffered frames.
        if !restarted && !hit_out {
            decoder.send_eof()?;
            while decoder.receive_frame(&mut decoded).is_ok() {
                let pts = ctx.secs(decoded.pts());
                if ctx.past_out(pts) {
                    break;
                }
                if ctx.before_in(pts) {
                    continue;
                }
                sent_any = true;
                if send_rgba(&decoded, &mut scaler, base, &mut first_pts, pts)? {
                    return Ok(());
                }
            }
        }
        guard_empty_playthrough(sent_any);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pacing clock: a playthrough's first frame is due immediately, and
    /// every later frame at its offset from that one, divided by `speed`.
    #[test]
    fn due_at_paces_from_the_first_frame_of_the_playthrough() {
        let base = Instant::now();
        let mut first = None;

        // A clip trimmed to start at 10 s is still due *now* on its first frame:
        // the offset is from the first frame seen, not from zero.
        assert_eq!(due_at(base, &mut first, 10.0, 1.0), base);
        assert_eq!(first, Some(10.0));

        assert_eq!(
            due_at(base, &mut first, 10.5, 1.0),
            base + Duration::from_millis(500)
        );
        // Double speed halves the wait; half speed doubles it.
        assert_eq!(
            due_at(base, &mut first, 10.5, 2.0),
            base + Duration::from_millis(250)
        );
        assert_eq!(
            due_at(base, &mut first, 10.5, 0.5),
            base + Duration::from_millis(1000)
        );
    }

    /// Out-of-order timestamps are due immediately rather than in the past:
    /// `Duration` cannot be negative, and computing one would panic.
    #[test]
    fn due_at_clamps_a_timestamp_before_the_first() {
        let base = Instant::now();
        let mut first = Some(10.0);
        assert_eq!(due_at(base, &mut first, 9.0, 1.0), base);
    }

    #[test]
    fn take_restart_drains_and_reports() {
        let (tx, rx) = bounded::<()>(4);
        assert!(!take_restart(&rx), "nothing pending");
        tx.send(()).unwrap();
        tx.send(()).unwrap();
        // One restart, however many requests piled up — a re-loop is a re-loop.
        assert!(take_restart(&rx));
        assert!(!take_restart(&rx), "the queue is drained, not just peeked");
    }

    /// `should_stop` is true for a closed *or* dropped close channel: a dropped
    /// sender means the app is gone, which is at least as good a reason to stop.
    #[test]
    fn should_stop_on_a_signal_or_a_dropped_sender() {
        let (tx, rx) = bounded::<()>(1);
        assert!(!should_stop(&rx));
        tx.send(()).unwrap();
        assert!(should_stop(&rx));

        let (tx, rx) = bounded::<()>(1);
        drop(tx);
        assert!(should_stop(&rx));
    }
}
