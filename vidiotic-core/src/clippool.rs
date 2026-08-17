//! Clip pool: scan a directory for video clips and extract first-frame
//! thumbnails on a background thread. Thumbnails are delivered over a channel so
//! a large pool never blocks the UI.
//!
//! The pool model — [`Clip`], [`ClipSource`], [`ClipBank`], [`Thumbnail`] — is
//! portable and unconditional. Only the decoding of a thumbnail is behind the
//! `ffmpeg` feature: the browser build has no ffmpeg and no worker thread here,
//! and fills the same [`Thumbnail`] from a `WebCodecs` decode instead
//! (web-port.md §8 step 3).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::chain::ClipId;

const VIDEO_EXTS: &[&str] = &["mov", "mp4", "mkv", "m4v", "avi", "webm", "hap"];

/// Thumbnail size the pool grid draws at. Public because a non-ffmpeg producer
/// of [`Thumbnail`] (the browser's decoder) has to match it.
pub const THUMB_W: u32 = 192;
/// See [`THUMB_W`].
pub const THUMB_H: u32 = 108;

/// Where a pool clip's frames come from.
#[derive(Clone, Debug)]
pub enum ClipSource {
    /// A video file on disk.
    File(PathBuf),
    /// A live capture device, by its stable `AVFoundation` `uniqueID`. `name` is
    /// the device's human name at the time the clip was created (also the
    /// relink hint when the uid is absent).
    Camera { uid: Arc<str>, name: Arc<str> },
}

/// One source in the pool: a video file, or a camera device.
#[derive(Clone, Debug)]
pub struct Clip {
    pub id: ClipId,
    pub source: ClipSource,
    pub name: Arc<str>,
    /// User-entered source tempo, used for advanced-mode BPM-synced playback;
    /// `None` until set (not derived from the file — container FPS is unreliable).
    pub bpm: Option<f64>,
}

impl Clip {
    /// The backing file, for file-sourced clips.
    pub fn file_path(&self) -> Option<&Path> {
        match &self.source {
            ClipSource::File(p) => Some(p),
            ClipSource::Camera { .. } => None,
        }
    }

    /// The capture-device uid, for camera-sourced clips.
    pub fn camera_uid(&self) -> Option<&str> {
        match &self.source {
            ClipSource::File(_) => None,
            ClipSource::Camera { uid, .. } => Some(uid),
        }
    }
}

/// A named group of clips over the flat pool, referenced by id. Purely a
/// pool-grid filter — a clip may belong to several banks or none; `ClipId`s stay
/// globally unique so cues reference clips regardless of grouping. `dir` is the
/// source folder when the bank came from a scan (`None` for ad-hoc groupings).
#[derive(Clone, Debug)]
pub struct ClipBank {
    pub name: Arc<str>,
    pub dir: Option<PathBuf>,
    pub clip_ids: Vec<ClipId>,
}

/// A decoded first-frame preview, delivered from the thumbnailer thread.
pub struct Thumbnail {
    pub id: ClipId,
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// List video clips in `dir` (non-recursive), sorted by name, with stable ids
/// starting at 0.
pub fn scan(dir: &Path) -> Vec<Clip> {
    scan_from(dir, 0)
}

/// Like [`scan`], but assign clip ids from `start_id` upward so several scanned
/// directories can share one flat, globally-unique id space.
pub fn scan_from(dir: &Path, start_id: ClipId) -> Vec<Clip> {
    // A directory that cannot be read is not an empty directory, and the
    // difference is the whole diagnostic: a mistyped `--clip-dir` used to come
    // back as a pool with no clips in it and nothing said anywhere, which reads
    // as "there are no videos in there".
    //
    // Still not an error return: the caller scans several directories and one bad
    // path should not lose the others. Saying so is enough.
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("clip directory {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut paths: Vec<PathBuf> = entries
        // A single unreadable entry within a readable directory *is* skipped
        // quietly — one bad `stat` in a folder of clips is not worth a line.
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| VIDEO_EXTS.contains(&e.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let name: Arc<str> = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("clip")
                .into();
            Clip {
                id: start_id + i as ClipId,
                source: ClipSource::File(path),
                name,
                bpm: None,
            }
        })
        .collect()
}

/// Spawn a worker that extracts a thumbnail for each clip and streams results.
/// The returned receiver closes when all clips are processed.
#[cfg(feature = "ffmpeg")]
pub fn spawn_thumbnailer(clips: Vec<Clip>) -> crossbeam_channel::Receiver<Thumbnail> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("thumbnailer".into())
        .spawn(move || {
            let _ = ffmpeg_next::init();
            for clip in clips {
                // Camera clips have no file to decode; their pool row wears a
                // static glyph instead.
                let Some(path) = clip.file_path() else {
                    continue;
                };
                match first_frame_rgba(path, THUMB_W, THUMB_H) {
                    Ok((w, h, rgba)) => {
                        let _ = tx.send(Thumbnail {
                            id: clip.id,
                            w,
                            h,
                            rgba,
                        });
                    }
                    Err(e) => log::warn!("thumbnail failed for {}: {e}", clip.name),
                }
            }
        })
        .ok();
    rx
}

/// Decode the first frame of a clip and scale it to a tight RGBA thumbnail.
#[cfg(feature = "ffmpeg")]
fn first_frame_rgba(path: &Path, tw: u32, th: u32) -> anyhow::Result<(usize, usize, Vec<u8>)> {
    use ffmpeg_next as ff;

    let mut ictx = ff::format::input(path)?;
    let (vid_idx, params) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| anyhow::anyhow!("no video stream"))?;
        (st.index(), st.parameters())
    };
    let mut decoder = ff::codec::context::Context::from_parameters(params)?
        .decoder()
        .video()?;
    let mut scaler = ff::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ff::format::Pixel::RGBA,
        tw,
        th,
        ff::software::scaling::Flags::BILINEAR,
    )?;

    let mut frame = ff::frame::Video::empty();
    for (stream, packet) in ictx.packets() {
        if stream.index() != vid_idx {
            continue;
        }
        decoder.send_packet(&packet)?;
        if decoder.receive_frame(&mut frame).is_ok() {
            let mut rgba = ff::frame::Video::empty();
            scaler.run(&frame, &mut rgba)?;
            let stride = rgba.stride(0);
            let row = (tw * 4) as usize;
            let mut packed = vec![0u8; row * th as usize];
            let src = rgba.data(0);
            for y in 0..th as usize {
                packed[y * row..(y + 1) * row].copy_from_slice(&src[y * stride..y * stride + row]);
            }
            return Ok((tw as usize, th as usize, packed));
        }
    }
    anyhow::bail!("no decodable frame")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real directory, because `scan_from` reads one directly rather than
    /// through the `Fs` trait — a clip pool is a native-only concept; the browser
    /// shells build theirs from files the visitor handed over.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("vidiotic-clippool-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn touch(&self, name: &str) {
            std::fs::write(self.0.join(name), b"not really a video").expect("write");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_takes_videos_by_extension_in_name_order() {
        let d = TempDir::new("exts");
        // Deliberately not in sorted order on disk.
        for name in ["b.mov", "a.MP4", "c.webm", "notes.txt", "cover.png", "x"] {
            d.touch(name);
        }
        let clips = scan(&d.0);
        let names: Vec<&str> = clips.iter().map(|c| &*c.name).collect();
        // Case-insensitive on the extension; sorted by path; everything that is
        // not a video left out — including an extensionless file.
        assert_eq!(names, ["a.MP4", "b.mov", "c.webm"]);
        assert_eq!(clips.iter().map(|c| c.id).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(clips.iter().all(|c| c.bpm.is_none()));
    }

    /// Ids are handed out from `start_id` so several directories can share one
    /// flat id space — a clip's id is what a `.viproj` cue points at, so a
    /// collision between two banks would repoint cues at the wrong video.
    #[test]
    fn scan_from_continues_an_existing_id_space() {
        let d = TempDir::new("ids");
        d.touch("one.mov");
        d.touch("two.mov");
        let clips = scan_from(&d.0, 7);
        assert_eq!(clips.iter().map(|c| c.id).collect::<Vec<_>>(), [7, 8]);
    }

    #[test]
    fn an_empty_directory_scans_to_nothing() {
        let d = TempDir::new("empty");
        assert!(scan(&d.0).is_empty());
    }

    /// The case that used to be indistinguishable from the one above. The pool is
    /// still empty — one bad path must not lose the other directories — but the
    /// warning is what tells a mistyped `--clip-dir` apart from an empty folder.
    #[test]
    fn a_missing_directory_scans_to_nothing_rather_than_panicking() {
        let missing = std::env::temp_dir().join("vidiotic-clippool-does-not-exist");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(scan(&missing).is_empty());
    }
}
