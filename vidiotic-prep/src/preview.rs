//! Frame-accurate source decoding for the span editor. Unlike vidiotic's
//! realtime clip decoder (which paces output to a playback clock), this seeks
//! and decodes on demand for whichever frame the UI is scrubbed to.

use std::path::Path;

use ffmpeg_next as ff;

/// A decoded, RGBA-packed frame at the preview's scaled dimensions.
pub struct FramePixels {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// An opened source video, ready for random-access frame decode via [`Self::frame_at`].
pub struct SourceMedia {
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Video,
    scaler: ff::software::scaling::Context,
    vid_idx: usize,
    in_tb: f64,
    pub fps: f64,
    pub frames: u64,
    pub duration_sec: f64,
    pub width: u32,
    pub height: u32,
    preview_w: u32,
    preview_h: u32,
    last_decoded_frame: Option<u64>,
    cur: FramePixels,
}

impl SourceMedia {
    /// Open `path` and probe its stream metadata. Decoded frames are scaled to
    /// `preview_w` wide, preserving aspect ratio (dimensions rounded even, as
    /// required by the RGBA scaler).
    ///
    /// # Errors
    /// Propagates ffmpeg open/probe/decoder-construction failures, and fails if
    /// the file has no video stream.
    pub fn open(path: &Path, preview_w: u32) -> anyhow::Result<Self> {
        let ictx = ff::format::input(path)?;
        let (vid_idx, params, fps, in_tb, frames_hint, width, height) = {
            let st = ictx
                .streams()
                .best(ff::media::Type::Video)
                .ok_or_else(|| anyhow::anyhow!("no video stream in {}", path.display()))?;
            let rate = st.avg_frame_rate();
            let fps = if rate.denominator() != 0 && rate.numerator() != 0 {
                rate.numerator() as f64 / rate.denominator() as f64
            } else {
                30.0
            };
            let tb = st.time_base();
            let in_tb = if tb.denominator() != 0 {
                tb.numerator() as f64 / tb.denominator() as f64
            } else {
                0.0
            };
            let decoder = ff::codec::context::Context::from_parameters(st.parameters())?
                .decoder()
                .video()?;
            (
                st.index(),
                st.parameters(),
                fps,
                in_tb,
                st.frames(),
                decoder.width(),
                decoder.height(),
            )
        };
        let duration_sec = ictx.duration() as f64 / 1_000_000.0;
        let frames = if frames_hint > 0 {
            frames_hint as u64
        } else {
            (duration_sec.max(0.0) * fps).round() as u64
        };

        let decoder = ff::codec::context::Context::from_parameters(params)?
            .decoder()
            .video()?;

        let preview_w = preview_w.max(2) & !1;
        let preview_h = if width > 0 {
            (((preview_w as f64 * height as f64 / width as f64).round() as u32).max(2)) & !1
        } else {
            preview_w
        };

        let scaler = ff::software::scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ff::format::Pixel::RGBA,
            preview_w,
            preview_h,
            ff::software::scaling::Flags::BILINEAR,
        )?;

        Ok(Self {
            ictx,
            decoder,
            scaler,
            vid_idx,
            in_tb,
            fps,
            frames: frames.max(1),
            duration_sec,
            width,
            height,
            preview_w,
            preview_h,
            last_decoded_frame: None,
            cur: FramePixels {
                w: preview_w,
                h: preview_h,
                rgba: vec![0u8; (preview_w * preview_h * 4) as usize],
            },
        })
    }

    /// Decode (seeking if needed) the frame at `idx` and return a reference to it.
    /// Sequential access (idx == last + 1) fast-forwards without reseeking.
    ///
    /// # Errors
    /// Propagates ffmpeg seek/decode failures. Never panics.
    pub fn frame_at(&mut self, idx: u64) -> anyhow::Result<&FramePixels> {
        let idx = idx.min(self.frames.saturating_sub(1));
        if self.last_decoded_frame == Some(idx) {
            return Ok(&self.cur);
        }

        let mut decoded = ff::frame::Video::empty();
        let found = if self.last_decoded_frame == Some(idx.wrapping_sub(1)) && idx > 0 {
            self.decode_forward_to(idx, &mut decoded)?
        } else {
            seek_secs(&mut self.ictx, idx as f64 / self.fps)?;
            self.decoder.flush();
            self.decode_forward_to(idx, &mut decoded)?
        };

        if !found {
            anyhow::bail!("no decodable frame at index {idx}");
        }

        let mut rgba = ff::frame::Video::empty();
        self.scaler.run(&decoded, &mut rgba)?;
        let stride = rgba.stride(0);
        let row = (self.preview_w * 4) as usize;
        let src = rgba.data(0);
        for y in 0..self.preview_h as usize {
            self.cur.rgba[y * row..(y + 1) * row]
                .copy_from_slice(&src[y * stride..y * stride + row]);
        }
        self.cur.w = self.preview_w;
        self.cur.h = self.preview_h;
        self.last_decoded_frame = Some(idx);
        Ok(&self.cur)
    }

    /// Decode packets forward until a frame whose timestamp rounds to `idx` (or
    /// past it) arrives, leaving `decoded` holding it. If EOF arrives first,
    /// `decoded` is left on the final decodable frame (so a slightly-optimistic
    /// `frames` count — common with fractional fps — clamps to the last frame
    /// instead of failing). Returns `false` only if nothing decoded at all.
    fn decode_forward_to(
        &mut self,
        idx: u64,
        decoded: &mut ff::frame::Video,
    ) -> anyhow::Result<bool> {
        let fps = self.fps;
        let in_tb = self.in_tb;
        let vid_idx = self.vid_idx;
        let mut got_any = false;

        let packets = self.ictx.packets();
        for (stream, packet) in packets {
            if stream.index() != vid_idx {
                continue;
            }
            self.decoder.send_packet(&packet)?;
            while self.decoder.receive_frame(decoded).is_ok() {
                got_any = true;
                let sec = decoded.pts().unwrap_or(0) as f64 * in_tb;
                if (sec * fps).round() as u64 >= idx {
                    return Ok(true);
                }
            }
        }
        // EOF: flush any frames buffered inside the decoder.
        self.decoder.send_eof().ok();
        while self.decoder.receive_frame(decoded).is_ok() {
            got_any = true;
            let sec = decoded.pts().unwrap_or(0) as f64 * in_tb;
            if (sec * fps).round() as u64 >= idx {
                return Ok(true);
            }
        }
        // Ran past EOF without reaching idx: `decoded` holds the last frame.
        Ok(got_any)
    }
}

/// Seek the demuxer to `secs` (clamped at 0), in the container's own timeline.
fn seek_secs(ictx: &mut ff::format::context::Input, secs: f64) -> anyhow::Result<()> {
    let ts = (secs.max(0.0) * 1_000_000.0) as i64; // AV_TIME_BASE microseconds
    ictx.seek(ts, ..)?;
    Ok(())
}
