//! A `QuickTime` `.mov` writer for Hap1 video, in pure Rust.
//!
//! This is the muxer half of web-port.md §8 step 2. ffmpeg was never encoding
//! anything here — [`crate::transcode`] built HAP packets itself and handed them
//! to `libavformat` purely to have a container written around them (it even had
//! to reach through `unsafe` and fill the codec parameters by hand, because no
//! HAP encoder exists to describe the stream). Writing the boxes directly is
//! less code than that workaround, and it is code that crosses to wasm.
//!
//! The contract is deliberately narrow, because the bake only ever needs one
//! shape of file:
//!
//! - one video track, sample description `Hap1`
//! - every sample a sync sample (HAP is all-intra, so there is no `stss` —
//!   its absence *is* the statement that all samples are sync samples)
//! - one sample per chunk, which makes `stsc` a single entry
//! - a caller-chosen timescale, with presentation times supplied per sample
//!
//! # What proves this is right
//!
//! Not inspection. `tests/mov_roundtrip.rs` writes a file with this muxer,
//! demuxes it *with ffmpeg*, and asserts the packets come back byte-identical
//! with the right codec tag, dimensions and timing — so the tool being replaced
//! is the one that judges the replacement. `tests/bake_integrity.rs` does the
//! same for a whole real bake.
//!
//! The unit tests below are the portable half: they walk the box tree with no
//! ffmpeg present, so they keep running under `wasm32-unknown-unknown` where the
//! roundtrip suite cannot. The strongest of them is
//! `boxes_tile_the_file_exactly` — if every length word is right, the boxes cover
//! the file with no gap and no overlap, and one wrong length fails it.
//!
//! # Layout
//!
//! `ftyp`, then a streamed `mdat`, then `moov`. Samples are written as they
//! arrive and the sample tables are accumulated in memory (a few bytes per
//! frame), so the writer never holds a whole clip. It needs [`Seek`] only to go
//! back and patch the `mdat` length once at the end — satisfied by a `File`
//! natively and by a `Cursor<Vec<u8>>` in the browser.

use std::io::{self, Seek, SeekFrom, Write};

/// The `QuickTime` sample-description fourcc for HAP's BC1 variant.
const HAP1: [u8; 4] = *b"Hap1";

/// 16.16 fixed-point 1.0 — the identity entry in a transformation matrix.
const FIXED_ONE: u32 = 0x0001_0000;
/// 2.30 fixed-point 1.0, which is what the matrix's bottom-right corner uses.
const FIXED_ONE_2_30: u32 = 0x4000_0000;

/// Identity transformation matrix, as `tkhd` and `mvhd` both want it.
const IDENTITY_MATRIX: [u32; 9] = [
    FIXED_ONE,
    0,
    0, //
    0,
    FIXED_ONE,
    0, //
    0,
    0,
    FIXED_ONE_2_30,
];

/// One written sample: where it landed, how big it was, and when it shows.
struct Sample {
    offset: u64,
    size: u32,
    pts: u32,
}

/// Writes a single-track Hap1 `QuickTime` file.
///
/// Construct, [`write_sample`](Self::write_sample) each frame in presentation
/// order, then [`finish`](Self::finish). Dropping without finishing leaves a
/// file with no `moov`, which no player will open — the type does not attempt
/// to rescue that in `Drop`, because a truncated bake should look broken rather
/// than silently produce a file that plays back short.
pub struct MovWriter<W: Write + Seek> {
    w: W,
    width: u32,
    height: u32,
    timescale: u32,
    frame_duration: u32,
    /// Absolute offset of the `mdat` box header, so its length can be patched.
    mdat_start: u64,
    /// Running write position. Tracked rather than queried so the happy path
    /// never seeks.
    pos: u64,
    samples: Vec<Sample>,
}

impl<W: Write + Seek> MovWriter<W> {
    /// Begin a file.
    ///
    /// `timescale` is the unit `pts` is expressed in. The writer imposes no
    /// policy on it; [`crate::transcode`] derives one from the frame rate so
    /// that a frame is a whole number of units, but any unit works — the tests
    /// below use milliseconds. `frame_duration` is the nominal length of one frame in
    /// that unit; it is used only for the final sample, whose duration cannot be
    /// derived from the next sample's timestamp because there isn't one.
    ///
    /// # Errors
    /// Propagates the underlying writer's errors.
    pub fn new(
        mut w: W,
        width: u32,
        height: u32,
        timescale: u32,
        frame_duration: u32,
    ) -> io::Result<Self> {
        let mut head = Vec::with_capacity(32);

        // ftyp. `qt  ` rather than an ISO brand: this is a QuickTime file, and
        // HAP is defined in QuickTime terms.
        let b = open(&mut head, b"ftyp");
        head.extend_from_slice(b"qt  ");
        head.extend_from_slice(&0x0000_0200u32.to_be_bytes()); // minor version
        head.extend_from_slice(b"qt  "); // compatible brands
        close(&mut head, b);

        let mdat_start = head.len() as u64;

        // mdat, opened with a 64-bit extended size. Writing `size = 1` and an
        // 8-byte largesize unconditionally costs 8 bytes and removes the
        // question of what happens to a bake that crosses 4 GB — at 848x480
        // BC1 that is only about 20 000 frames.
        head.extend_from_slice(&1u32.to_be_bytes());
        head.extend_from_slice(b"mdat");
        head.extend_from_slice(&0u64.to_be_bytes()); // largesize, patched later

        w.write_all(&head)?;
        let pos = head.len() as u64;

        Ok(Self {
            w,
            width,
            height,
            timescale,
            frame_duration,
            mdat_start,
            pos,
            samples: Vec::new(),
        })
    }

    /// Append one Hap1 packet at presentation time `pts` (in the timescale given
    /// to [`new`](Self::new)).
    ///
    /// Samples must be supplied in increasing `pts` order; the sample table is
    /// built by differencing consecutive timestamps.
    ///
    /// # Errors
    /// Propagates the underlying writer's errors.
    pub fn write_sample(&mut self, data: &[u8], pts: u32) -> io::Result<()> {
        self.w.write_all(data)?;
        self.samples.push(Sample {
            offset: self.pos,
            size: data.len() as u32,
            pts,
        });
        self.pos += data.len() as u64;
        Ok(())
    }

    /// Number of samples written so far.
    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// Close the `mdat`, write the `moov`, and return the underlying writer.
    ///
    /// # Errors
    /// Propagates the underlying writer's errors, including the seek back to
    /// patch the `mdat` length.
    pub fn finish(mut self) -> io::Result<W> {
        // Patch the mdat largesize now that the payload length is known. The
        // +8 covers the `size`/`type` words that precede it.
        let mdat_len = self.pos - self.mdat_start;
        self.w.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.w.write_all(&mdat_len.to_be_bytes())?;
        self.w.seek(SeekFrom::Start(self.pos))?;

        let moov = self.build_moov();
        self.w.write_all(&moov)?;
        self.w.flush()?;
        Ok(self.w)
    }

    /// Total track duration in timescale units: the last sample's timestamp plus
    /// its nominal duration.
    fn duration(&self) -> u64 {
        self.samples
            .last()
            .map_or(0, |s| u64::from(s.pts) + u64::from(self.frame_duration))
    }

    fn build_moov(&self) -> Vec<u8> {
        let mut m = Vec::with_capacity(512 + self.samples.len() * 12);
        let moov = open(&mut m, b"moov");
        self.mvhd(&mut m);
        self.trak(&mut m);
        close(&mut m, moov);
        m
    }

    fn mvhd(&self, m: &mut Vec<u8>) {
        let b = open(m, b"mvhd");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&0u32.to_be_bytes()); // creation time
        m.extend_from_slice(&0u32.to_be_bytes()); // modification time
        m.extend_from_slice(&self.timescale.to_be_bytes());
        m.extend_from_slice(&(self.duration() as u32).to_be_bytes());
        m.extend_from_slice(&FIXED_ONE.to_be_bytes()); // preferred rate 1.0
        m.extend_from_slice(&0x0100u16.to_be_bytes()); // preferred volume 1.0
        m.extend_from_slice(&[0u8; 10]); // reserved
        for v in IDENTITY_MATRIX {
            m.extend_from_slice(&v.to_be_bytes());
        }
        m.extend_from_slice(&[0u8; 24]); // pre-defined (poster, selection, ...)
        m.extend_from_slice(&2u32.to_be_bytes()); // next track id
        close(m, b);
    }

    fn trak(&self, m: &mut Vec<u8>) {
        let trak = open(m, b"trak");

        let b = open(m, b"tkhd");
        // flags 0x000003 = track enabled | in movie.
        m.extend_from_slice(&0x0000_0003u32.to_be_bytes());
        m.extend_from_slice(&0u32.to_be_bytes()); // creation time
        m.extend_from_slice(&0u32.to_be_bytes()); // modification time
        m.extend_from_slice(&1u32.to_be_bytes()); // track id
        m.extend_from_slice(&0u32.to_be_bytes()); // reserved
        m.extend_from_slice(&(self.duration() as u32).to_be_bytes());
        m.extend_from_slice(&[0u8; 8]); // reserved
        m.extend_from_slice(&0u16.to_be_bytes()); // layer
        m.extend_from_slice(&0u16.to_be_bytes()); // alternate group
        m.extend_from_slice(&0u16.to_be_bytes()); // volume (0 for video)
        m.extend_from_slice(&0u16.to_be_bytes()); // reserved
        for v in IDENTITY_MATRIX {
            m.extend_from_slice(&v.to_be_bytes());
        }
        // Display size, 16.16 fixed. Equal to the coded size — the bake never
        // produces non-square pixels.
        m.extend_from_slice(&(self.width << 16).to_be_bytes());
        m.extend_from_slice(&(self.height << 16).to_be_bytes());
        close(m, b);

        self.mdia(m);
        close(m, trak);
    }

    fn mdia(&self, m: &mut Vec<u8>) {
        let mdia = open(m, b"mdia");

        let b = open(m, b"mdhd");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&0u32.to_be_bytes()); // creation time
        m.extend_from_slice(&0u32.to_be_bytes()); // modification time
        m.extend_from_slice(&self.timescale.to_be_bytes());
        m.extend_from_slice(&(self.duration() as u32).to_be_bytes());
        m.extend_from_slice(&0x55c4u16.to_be_bytes()); // language: undetermined
        m.extend_from_slice(&0u16.to_be_bytes()); // quality
        close(m, b);

        // hdlr. The ISO layout is used rather than QuickTime's component form;
        // both place the four bytes that matter ('vide') at the same offset,
        // which is why QuickTime accepts either.
        let b = open(m, b"hdlr");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&0u32.to_be_bytes()); // pre-defined
        m.extend_from_slice(b"vide");
        m.extend_from_slice(&[0u8; 12]); // reserved
        m.extend_from_slice(b"VideoHandler\0");
        close(m, b);

        self.minf(m);
        close(m, mdia);
    }

    fn minf(&self, m: &mut Vec<u8>) {
        let minf = open(m, b"minf");

        let b = open(m, b"vmhd");
        m.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // version 0, flags 1
        m.extend_from_slice(&0u16.to_be_bytes()); // graphics mode: copy
        m.extend_from_slice(&[0u8; 6]); // opcolor
        close(m, b);

        // dinf/dref declaring the media is in this same file.
        let dinf = open(m, b"dinf");
        let dref = open(m, b"dref");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&1u32.to_be_bytes()); // entry count
        let url = open(m, b"url ");
        m.extend_from_slice(&0x0000_0001u32.to_be_bytes()); // flags 1: self-contained
        close(m, url);
        close(m, dref);
        close(m, dinf);

        self.stbl(m);
        close(m, minf);
    }

    fn stbl(&self, m: &mut Vec<u8>) {
        let stbl = open(m, b"stbl");
        self.stsd(m);
        self.stts(m);
        self.stsc(m);
        self.stsz(m);
        self.stco(m);
        // No `stss`: in QuickTime, omitting it declares every sample a sync
        // sample, which is exactly true of HAP and is what lets a player seek
        // anywhere without an index of keyframes.
        close(m, stbl);
    }

    fn stsd(&self, m: &mut Vec<u8>) {
        let b = open(m, b"stsd");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&1u32.to_be_bytes()); // entry count

        let entry = open(m, &HAP1);
        m.extend_from_slice(&[0u8; 6]); // reserved
        m.extend_from_slice(&1u16.to_be_bytes()); // data reference index
        m.extend_from_slice(&0u16.to_be_bytes()); // version
        m.extend_from_slice(&0u16.to_be_bytes()); // revision level
        m.extend_from_slice(&[0u8; 4]); // vendor
        m.extend_from_slice(&0u32.to_be_bytes()); // temporal quality
        m.extend_from_slice(&0u32.to_be_bytes()); // spatial quality
        m.extend_from_slice(&(self.width as u16).to_be_bytes());
        m.extend_from_slice(&(self.height as u16).to_be_bytes());
        m.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horiz resolution 72 dpi
        m.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vert resolution 72 dpi
        m.extend_from_slice(&0u32.to_be_bytes()); // data size
        m.extend_from_slice(&1u16.to_be_bytes()); // frame count per sample
                                                  // Compressor name: a 32-byte Pascal string (length byte + padding).
        let name = b"HAP";
        m.push(name.len() as u8);
        m.extend_from_slice(name);
        m.extend_from_slice(&vec![0u8; 31 - name.len()]);
        m.extend_from_slice(&0x0018u16.to_be_bytes()); // depth: 24-bit colour
        m.extend_from_slice(&0xffffu16.to_be_bytes()); // colour table id: none
        close(m, entry);

        close(m, b);
    }

    /// Time-to-sample, run-length encoded. Constant frame rate collapses to a
    /// single entry; a variable-rate source produces one entry per rate change.
    fn stts(&self, m: &mut Vec<u8>) {
        let mut runs: Vec<(u32, u32)> = Vec::new(); // (count, duration)
        for i in 0..self.samples.len() {
            let dur = if i + 1 < self.samples.len() {
                self.samples[i + 1].pts - self.samples[i].pts
            } else {
                // The last sample has no successor to difference against.
                self.frame_duration
            };
            match runs.last_mut() {
                Some((count, d)) if *d == dur => *count += 1,
                _ => runs.push((1, dur)),
            }
        }

        let b = open(m, b"stts");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&(runs.len() as u32).to_be_bytes());
        for (count, dur) in runs {
            m.extend_from_slice(&count.to_be_bytes());
            m.extend_from_slice(&dur.to_be_bytes());
        }
        close(m, b);
    }

    /// Sample-to-chunk. One sample per chunk, so this is always one entry.
    fn stsc(&self, m: &mut Vec<u8>) {
        let b = open(m, b"stsc");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&1u32.to_be_bytes()); // entry count
        m.extend_from_slice(&1u32.to_be_bytes()); // first chunk
        m.extend_from_slice(&1u32.to_be_bytes()); // samples per chunk
        m.extend_from_slice(&1u32.to_be_bytes()); // sample description index
        close(m, b);
    }

    /// Sample sizes. HAP packets vary in size frame to frame (snappy), so the
    /// constant-size shortcut never applies.
    fn stsz(&self, m: &mut Vec<u8>) {
        let b = open(m, b"stsz");
        m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
        m.extend_from_slice(&0u32.to_be_bytes()); // sample size: 0 = per-sample
        m.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
        for s in &self.samples {
            m.extend_from_slice(&s.size.to_be_bytes());
        }
        close(m, b);
    }

    /// Chunk offsets — `stco` while they fit in 32 bits, `co64` beyond that.
    /// Both are universally supported; picking the narrow one keeps the table
    /// half the size for the files this actually produces.
    fn stco(&self, m: &mut Vec<u8>) {
        let needs_64 = self
            .samples
            .last()
            .is_some_and(|s| s.offset > u64::from(u32::MAX));

        if needs_64 {
            let b = open(m, b"co64");
            m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
            m.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
            for s in &self.samples {
                m.extend_from_slice(&s.offset.to_be_bytes());
            }
            close(m, b);
        } else {
            let b = open(m, b"stco");
            m.extend_from_slice(&0u32.to_be_bytes()); // version 0, flags 0
            m.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
            for s in &self.samples {
                m.extend_from_slice(&(s.offset as u32).to_be_bytes());
            }
            close(m, b);
        }
    }
}

/// Start a box: reserve its length word, write its type, return where to patch.
fn open(buf: &mut Vec<u8>, kind: &[u8; 4]) -> usize {
    let start = buf.len();
    buf.extend_from_slice(&[0u8; 4]);
    buf.extend_from_slice(kind);
    start
}

/// Finish the box opened at `start`, filling in its length.
fn close(buf: &mut [u8], start: usize) {
    let len = (buf.len() - start) as u32;
    buf[start..start + 4].copy_from_slice(&len.to_be_bytes());
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// What went wrong reading a `.mov`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovErr {
    /// A box claimed a length that runs past the end of its parent.
    Truncated {
        /// The box being read when the overrun was detected.
        kind: [u8; 4],
    },
    /// A box the sample tables cannot be built without was absent.
    Missing(&'static str),
    /// A box was present but too short to hold the fields it must have.
    Short(&'static str),
    /// The file has no track whose handler is `vide`.
    NoVideoTrack,
    /// The sample tables disagree with each other — `stsc` naming a chunk that
    /// `stco` does not have, or `stsz`/`stts` covering different sample counts.
    BadTables(&'static str),
    /// A sample's byte range is not inside the file.
    SampleOutOfBounds {
        /// Index of the offending sample.
        index: usize,
    },
}

impl std::fmt::Display for MovErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { kind } => {
                write!(
                    f,
                    "box '{}' runs past its parent",
                    String::from_utf8_lossy(kind)
                )
            }
            Self::Missing(b) => write!(f, "required box '{b}' is missing"),
            Self::Short(b) => write!(f, "box '{b}' is too short"),
            Self::NoVideoTrack => write!(f, "no track with a 'vide' handler"),
            Self::BadTables(why) => write!(f, "inconsistent sample tables: {why}"),
            Self::SampleOutOfBounds { index } => {
                write!(f, "sample {index} lies outside the file")
            }
        }
    }
}
impl std::error::Error for MovErr {}

/// Where one sample lives in the file, and when it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRef {
    /// Absolute byte offset in the file.
    pub offset: u64,
    /// Length in bytes.
    pub size: u32,
    /// Presentation time in [`MovTrack::timescale`] units.
    pub pts: u64,
    /// Duration in [`MovTrack::timescale`] units, from `stts`.
    pub duration: u32,
}

/// A demuxed video track: what it is, how big, and every sample's location.
///
/// The sample list is fully materialised — a few bytes per frame, and the
/// player wants random access anyway, so there is nothing to gain from lazy
/// table walking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovTrack {
    /// The `stsd` sample-description fourcc: `Hap1`, `Hap5`, `HapY`, `HapM`…
    pub format: [u8; 4],
    /// Coded width, from the sample description.
    pub width: u32,
    /// Coded height, from the sample description.
    pub height: u32,
    /// Units per second that `pts` and `duration` are expressed in.
    pub timescale: u32,
    /// Total track duration in `timescale` units.
    pub duration: u64,
    /// Every sample, in decode order.
    pub samples: Vec<SampleRef>,
    /// Whether the track carries a `ctts` composition-offset table — i.e.
    /// whether presentation order differs from decode order.
    ///
    /// When this is `false`, which it always is for HAP, `SampleRef::pts` is a
    /// presentation time. When `true` it is a *decode* time and this reader has
    /// not applied the offsets. Exposed rather than merely documented so that a
    /// caller handed a B-frame codec can notice instead of quietly showing
    /// frames in the wrong order.
    pub has_composition_offsets: bool,
}

impl MovTrack {
    /// Whether this track is one of the HAP variants, i.e. whether
    /// [`crate::hap::decode_frame`] can read its samples.
    #[must_use]
    pub fn is_hap(&self) -> bool {
        matches!(&self.format, b"Hap1" | b"Hap5" | b"HapY" | b"HapM")
    }

    /// The bytes of sample `index`, borrowed from the file it was demuxed from.
    ///
    /// Returns `None` for an out-of-range index. Ranges were bounds-checked at
    /// demux time, so passing the same slice back gives `Some`.
    #[must_use]
    pub fn sample_data<'a>(&self, file: &'a [u8], index: usize) -> Option<&'a [u8]> {
        let s = self.samples.get(index)?;
        let start = usize::try_from(s.offset).ok()?;
        file.get(start..start + s.size as usize)
    }

    /// The sample presented at time `t` (in `timescale` units), i.e. the last
    /// one whose `pts` is `<= t`.
    ///
    /// Returns `None` only for an empty track. Times past the end clamp to the
    /// final sample, which is what a player holding the last frame wants.
    #[must_use]
    pub fn sample_at(&self, t: u64) -> Option<usize> {
        if self.samples.is_empty() {
            return None;
        }
        // partition_point over a sorted pts column: the count of samples that
        // start at or before `t`. Saturating to 0 handles t < the first pts,
        // which only arises for a track that does not start at zero.
        Some(
            self.samples
                .partition_point(|s| s.pts <= t)
                .saturating_sub(1),
        )
    }
}

/// One box: its type and its payload (the bytes after the header).
type Child<'a> = ([u8; 4], &'a [u8]);

/// Split a container's payload into its immediate children.
///
/// Handles both the 64-bit extended size (`size == 1`) that [`MovWriter`] uses
/// for `mdat` and the "to end of parent" form (`size == 0`).
fn children(data: &[u8]) -> Result<Vec<Child<'_>>, MovErr> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    // A trailing run shorter than a header is padding, not a box; stopping at
    // 8 rather than erroring keeps files with a few slack bytes readable.
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();

        let (header, len) = match size {
            1 => {
                if pos + 16 > data.len() {
                    return Err(MovErr::Truncated { kind });
                }
                let big = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap());
                (16usize, big)
            }
            0 => (8usize, (data.len() - pos) as u64),
            _ => (8usize, size),
        };

        let len = usize::try_from(len).map_err(|_| MovErr::Truncated { kind })?;
        if len < header || pos + len > data.len() {
            return Err(MovErr::Truncated { kind });
        }
        out.push((kind, &data[pos + header..pos + len]));
        pos += len;
    }
    Ok(out)
}

/// Find the first child of a given type.
fn pick<'a>(kids: &[Child<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    kids.iter().find(|(k, _)| k == kind).map(|(_, v)| *v)
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Demux the video track of a `QuickTime`/MP4 file held in memory.
///
/// This is the read side of web-port.md §4: `/play` in the browser has no
/// ffmpeg to demux with, and `WebCodecs` cannot help because HAP is not a codec
/// the browser knows — so the container walk has to be ours. It is written
/// against whole files in memory, which suits both callers: a chop is a short
/// span, and the browser has already fetched it.
///
/// Multi-track files are handled by picking the first `vide` track, so a clip
/// that carries audio demuxes fine (the audio is simply not returned).
///
/// # Known limitations, none of which arise for baked HAP
///
/// - **`ctts` is not applied.** Composition offsets only exist where decode
///   order differs from presentation order, which requires B-frames; HAP is
///   all-intra. Its presence is still reported via
///   [`MovTrack::has_composition_offsets`], so this is a limitation a caller
///   can detect rather than one it has to know about.
/// - **`edts`/`elst` is ignored.** An edit list can shift or trim the timeline;
///   [`MovWriter`] writes none, and a shifted start would show up as a non-zero
///   first `pts`, which `tests/` asserts against.
/// - Only the first sample description is consulted, so a track that changes
///   codec mid-stream would be read as if it had not.
///
/// # Errors
///
/// Returns [`MovErr`] if the box tree is malformed, a required box is absent,
/// the sample tables disagree, or a sample's byte range escapes the file.
pub fn demux(data: &[u8]) -> Result<MovTrack, MovErr> {
    let top = children(data)?;
    let moov = pick(&top, b"moov").ok_or(MovErr::Missing("moov"))?;
    let moov_kids = children(moov)?;

    // Pick the video track. `trak` repeats, so this cannot be a `pick`.
    let mut chosen: Option<(Vec<Child<'_>>, Vec<Child<'_>>)> = None;
    for (kind, trak) in &moov_kids {
        if kind != b"trak" {
            continue;
        }
        let mdia = match pick(&children(trak)?, b"mdia") {
            Some(m) => children(m)?,
            None => continue,
        };
        // hdlr: version/flags(4), pre_defined(4), then the handler fourcc.
        let is_video = pick(&mdia, b"hdlr").and_then(|h| h.get(8..12)) == Some(b"vide");
        if !is_video {
            continue;
        }
        let minf = children(pick(&mdia, b"minf").ok_or(MovErr::Missing("minf"))?)?;
        let stbl = children(pick(&minf, b"stbl").ok_or(MovErr::Missing("stbl"))?)?;
        chosen = Some((mdia, stbl));
        break;
    }
    let (mdia, stbl) = chosen.ok_or(MovErr::NoVideoTrack)?;

    // mdhd. Version 0 packs the times as u32, version 1 as u64, which moves
    // timescale from offset 12 to offset 20.
    let mdhd = pick(&mdia, b"mdhd").ok_or(MovErr::Missing("mdhd"))?;
    let (timescale, duration) = match mdhd.first() {
        Some(0) => (
            be32(mdhd, 12).ok_or(MovErr::Short("mdhd"))?,
            u64::from(be32(mdhd, 16).ok_or(MovErr::Short("mdhd"))?),
        ),
        Some(1) => (
            be32(mdhd, 20).ok_or(MovErr::Short("mdhd"))?,
            be64(mdhd, 24).ok_or(MovErr::Short("mdhd"))?,
        ),
        _ => return Err(MovErr::Short("mdhd")),
    };

    // stsd: version/flags(4), entry_count(4), then sized entries. Within an
    // entry the coded dimensions sit 24 bytes past the fourcc header.
    let stsd = pick(&stbl, b"stsd").ok_or(MovErr::Missing("stsd"))?;
    let entries = children(stsd.get(8..).ok_or(MovErr::Short("stsd"))?)?;
    let (format, entry) = *entries.first().ok_or(MovErr::Short("stsd"))?;
    let width = u32::from(be16(entry, 24).ok_or(MovErr::Short("stsd"))?);
    let height = u32::from(be16(entry, 26).ok_or(MovErr::Short("stsd"))?);

    let sizes = read_stsz(pick(&stbl, b"stsz").ok_or(MovErr::Missing("stsz"))?)?;
    let times = read_stts(
        pick(&stbl, b"stts").ok_or(MovErr::Missing("stts"))?,
        sizes.len(),
    )?;
    let offsets = read_offsets(&stbl, &sizes)?;

    if times.len() != sizes.len() || offsets.len() != sizes.len() {
        return Err(MovErr::BadTables(
            "stts/stsz/chunk maps cover different counts",
        ));
    }

    let mut samples = Vec::with_capacity(sizes.len());
    let mut pts = 0u64;
    for (i, ((size, dur), offset)) in sizes.iter().zip(&times).zip(&offsets).enumerate() {
        let end = offset
            .checked_add(u64::from(*size))
            .ok_or(MovErr::SampleOutOfBounds { index: i })?;
        if end > data.len() as u64 {
            return Err(MovErr::SampleOutOfBounds { index: i });
        }
        samples.push(SampleRef {
            offset: *offset,
            size: *size,
            pts,
            duration: *dur,
        });
        pts += u64::from(*dur);
    }

    Ok(MovTrack {
        format,
        width,
        height,
        timescale,
        // Prefer the accumulated stts total when mdhd says nothing useful; a
        // zero mdhd duration is common in files written by streaming muxers.
        duration: if duration == 0 { pts } else { duration },
        samples,
        has_composition_offsets: pick(&stbl, b"ctts").is_some(),
    })
}

/// `stsz`: either one uniform size for every sample, or an explicit table.
fn read_stsz(b: &[u8]) -> Result<Vec<u32>, MovErr> {
    let uniform = be32(b, 4).ok_or(MovErr::Short("stsz"))?;
    let count = be32(b, 8).ok_or(MovErr::Short("stsz"))? as usize;
    if uniform != 0 {
        return Ok(vec![uniform; count]);
    }
    let table = b.get(12..).ok_or(MovErr::Short("stsz"))?;
    if table.len() < count * 4 {
        return Err(MovErr::Short("stsz"));
    }
    Ok((0..count)
        .map(|i| u32::from_be_bytes(table[i * 4..i * 4 + 4].try_into().unwrap()))
        .collect())
}

/// `stts`: run-length encoded durations, expanded to one per sample.
///
/// `expect` is the sample count the other tables agree on. A file whose `stts`
/// runs cover more samples than that is truncated to it rather than rejected —
/// but covering *fewer* is an error, because the missing durations cannot be
/// invented.
fn read_stts(b: &[u8], expect: usize) -> Result<Vec<u32>, MovErr> {
    let runs = be32(b, 4).ok_or(MovErr::Short("stts"))? as usize;
    let table = b.get(8..).ok_or(MovErr::Short("stts"))?;
    if table.len() < runs * 8 {
        return Err(MovErr::Short("stts"));
    }
    let mut out = Vec::with_capacity(expect);
    for i in 0..runs {
        let count = u32::from_be_bytes(table[i * 8..i * 8 + 4].try_into().unwrap()) as usize;
        let delta = u32::from_be_bytes(table[i * 8 + 4..i * 8 + 8].try_into().unwrap());
        // A corrupt count could otherwise ask for an enormous allocation.
        let count = count.min(expect.saturating_sub(out.len()));
        out.extend(std::iter::repeat_n(delta, count));
    }
    if out.len() < expect {
        return Err(MovErr::BadTables("stts covers fewer samples than stsz"));
    }
    Ok(out)
}

/// Resolve every sample's absolute file offset from `stsc` + `stco`/`co64`.
///
/// This is the one genuinely fiddly part of the format: `stsc` is run-length
/// encoded over *chunks*, so a sample's offset is its chunk's offset plus the
/// sizes of the samples ahead of it within that chunk. [`MovWriter`] writes one
/// sample per chunk, which makes this trivial for our own files — but real
/// clips from other tools pack many samples per chunk, so the general walk is
/// what gets implemented and tested.
fn read_offsets(stbl: &[Child<'_>], sizes: &[u32]) -> Result<Vec<u64>, MovErr> {
    let chunks: Vec<u64> = if let Some(co64) = pick(stbl, b"co64") {
        let n = be32(co64, 4).ok_or(MovErr::Short("co64"))? as usize;
        let t = co64.get(8..).ok_or(MovErr::Short("co64"))?;
        if t.len() < n * 8 {
            return Err(MovErr::Short("co64"));
        }
        (0..n)
            .map(|i| u64::from_be_bytes(t[i * 8..i * 8 + 8].try_into().unwrap()))
            .collect()
    } else {
        let stco = pick(stbl, b"stco").ok_or(MovErr::Missing("stco"))?;
        let n = be32(stco, 4).ok_or(MovErr::Short("stco"))? as usize;
        let t = stco.get(8..).ok_or(MovErr::Short("stco"))?;
        if t.len() < n * 4 {
            return Err(MovErr::Short("stco"));
        }
        (0..n)
            .map(|i| u64::from(u32::from_be_bytes(t[i * 4..i * 4 + 4].try_into().unwrap())))
            .collect()
    };

    let stsc = pick(stbl, b"stsc").ok_or(MovErr::Missing("stsc"))?;
    let runs = be32(stsc, 4).ok_or(MovErr::Short("stsc"))? as usize;
    let t = stsc.get(8..).ok_or(MovErr::Short("stsc"))?;
    if t.len() < runs * 12 {
        return Err(MovErr::Short("stsc"));
    }
    // (first_chunk, samples_per_chunk); the sample-description index is dropped
    // because only the first description is consulted.
    let map: Vec<(usize, usize)> = (0..runs)
        .map(|i| {
            let fc = u32::from_be_bytes(t[i * 12..i * 12 + 4].try_into().unwrap()) as usize;
            let spc = u32::from_be_bytes(t[i * 12 + 4..i * 12 + 8].try_into().unwrap()) as usize;
            (fc, spc)
        })
        .collect();
    if map.first().is_some_and(|(fc, _)| *fc != 1) {
        return Err(MovErr::BadTables("stsc does not start at chunk 1"));
    }

    let mut out = Vec::with_capacity(sizes.len());
    let mut run = 0usize;
    for (ci, chunk_off) in chunks.iter().enumerate() {
        if out.len() >= sizes.len() {
            break;
        }
        // Chunk indices in stsc are 1-based.
        let chunk_no = ci + 1;
        while run + 1 < map.len() && map[run + 1].0 <= chunk_no {
            run += 1;
        }
        let per = map.get(run).map_or(0, |(_, spc)| *spc);
        if per == 0 {
            return Err(MovErr::BadTables("stsc declares a chunk with no samples"));
        }
        let mut at = *chunk_off;
        for _ in 0..per {
            if out.len() >= sizes.len() {
                break;
            }
            out.push(at);
            at = at
                .checked_add(u64::from(sizes[out.len() - 1]))
                .ok_or(MovErr::BadTables("chunk offsets overflow"))?;
        }
    }
    if out.len() < sizes.len() {
        return Err(MovErr::BadTables("chunk table does not cover every sample"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Under wasm32 there is no built-in test harness; aliasing the attribute lets
    // these same tests run unmodified under `wasm-bindgen-test` (web-port.md §7a).
    // Nothing else changes, which is the point — the wasm run must exercise the
    // same assertions, not a parallel copy of them.
    use std::io::Cursor;
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// Walk the top-level boxes, yielding (type, payload range).
    fn top_level(buf: &[u8]) -> Vec<([u8; 4], usize, u64)> {
        let mut out = Vec::new();
        let mut p = 0usize;
        while p + 8 <= buf.len() {
            let size = u32::from_be_bytes(buf[p..p + 4].try_into().unwrap());
            let kind: [u8; 4] = buf[p + 4..p + 8].try_into().unwrap();
            let (len, hdr) = if size == 1 {
                let large = u64::from_be_bytes(buf[p + 8..p + 16].try_into().unwrap());
                (large, 16)
            } else {
                (u64::from(size), 8)
            };
            assert!(len >= hdr, "box {kind:?} claims {len} bytes");
            out.push((kind, p, len));
            p += len as usize;
        }
        assert_eq!(p, buf.len(), "boxes do not tile the file exactly");
        out
    }

    /// Find a nested box by path, returning its payload.
    ///
    /// This has to understand the 64-bit extended size, because the very first
    /// sibling it must skip past is `mdat`, which always uses one. Getting that
    /// wrong reads `size = 1`, walks into the payload, and spins on the first
    /// zero length word it finds — hence the explicit guard below.
    fn find<'a>(buf: &'a [u8], path: &[&[u8; 4]]) -> &'a [u8] {
        let mut region = buf;
        for want in path {
            let mut p = 0usize;
            loop {
                assert!(p + 8 <= region.len(), "ran out looking for {want:?}");
                let size = u32::from_be_bytes(region[p..p + 4].try_into().unwrap());
                let kind: [u8; 4] = region[p + 4..p + 8].try_into().unwrap();
                let (len, hdr) = if size == 1 {
                    assert!(p + 16 <= region.len(), "truncated extended size");
                    let large = u64::from_be_bytes(region[p + 8..p + 16].try_into().unwrap());
                    (large as usize, 16)
                } else {
                    (size as usize, 8)
                };
                assert!(
                    len >= hdr,
                    "box {kind:?} claims {len} bytes — not a box tree"
                );
                if &kind == *want {
                    region = &region[p + hdr..p + len];
                    break;
                }
                p += len;
            }
        }
        region
    }

    /// Path to the sample table, which most of these tests want.
    const STBL: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

    /// `STBL` extended by one more box.
    fn stbl_path(leaf: &'static [u8; 4]) -> Vec<&'static [u8; 4]> {
        let mut p = STBL.to_vec();
        p.push(leaf);
        p
    }

    fn write_clip(frames: &[(&[u8], u32)]) -> Vec<u8> {
        let mut w = MovWriter::new(Cursor::new(Vec::new()), 64, 32, 1000, 40).unwrap();
        for (data, pts) in frames {
            w.write_sample(data, *pts).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn boxes_tile_the_file_exactly() {
        // The strongest cheap structural check there is: if every box length is
        // right, they cover the file with no gap and no overlap. A single wrong
        // length word fails this.
        let buf = write_clip(&[(&[1u8; 100], 0), (&[2u8; 120], 40)]);
        let top: Vec<[u8; 4]> = top_level(&buf).iter().map(|(k, _, _)| *k).collect();
        assert_eq!(top, vec![*b"ftyp", *b"mdat", *b"moov"]);
    }

    #[test]
    fn mdat_length_is_patched_to_cover_the_payload() {
        let buf = write_clip(&[(&[1u8; 100], 0), (&[2u8; 120], 40)]);
        let (_, start, len) = top_level(&buf)[1];
        // 16-byte extended header plus both payloads.
        assert_eq!(len, 16 + 100 + 120);
        assert_eq!(&buf[start + 4..start + 8], b"mdat");
    }

    #[test]
    fn sample_table_locates_every_frame() {
        // The tables are the file: if stsz/stco disagree with what was written,
        // a player reads the wrong bytes and the failure looks like a corrupt
        // codec rather than a corrupt index.
        let a: Vec<u8> = (0..100).map(|i| i as u8).collect();
        let b: Vec<u8> = (0..120).map(|i| (255 - i) as u8).collect();
        let buf = write_clip(&[(&a, 0), (&b, 40)]);

        let stsz = find(&buf, &stbl_path(b"stsz"));
        let stco = find(&buf, &stbl_path(b"stco"));

        assert_eq!(u32::from_be_bytes(stsz[8..12].try_into().unwrap()), 2);
        let sizes = [
            u32::from_be_bytes(stsz[12..16].try_into().unwrap()) as usize,
            u32::from_be_bytes(stsz[16..20].try_into().unwrap()) as usize,
        ];
        assert_eq!(sizes, [100, 120]);

        assert_eq!(u32::from_be_bytes(stco[4..8].try_into().unwrap()), 2);
        let offs = [
            u32::from_be_bytes(stco[8..12].try_into().unwrap()) as usize,
            u32::from_be_bytes(stco[12..16].try_into().unwrap()) as usize,
        ];
        assert_eq!(&buf[offs[0]..offs[0] + sizes[0]], &a[..]);
        assert_eq!(&buf[offs[1]..offs[1] + sizes[1]], &b[..]);
    }

    #[test]
    fn constant_frame_rate_collapses_to_one_stts_run() {
        let f = [0u8; 8];
        let buf = write_clip(&[(&f, 0), (&f, 40), (&f, 80), (&f, 120)]);
        let stts = find(&buf, &stbl_path(b"stts"));
        assert_eq!(
            u32::from_be_bytes(stts[4..8].try_into().unwrap()),
            1,
            "entry count"
        );
        assert_eq!(
            u32::from_be_bytes(stts[8..12].try_into().unwrap()),
            4,
            "sample count"
        );
        assert_eq!(
            u32::from_be_bytes(stts[12..16].try_into().unwrap()),
            40,
            "duration"
        );
    }

    #[test]
    fn variable_frame_timing_produces_separate_runs() {
        // 29.97-style timing: millisecond timestamps do not divide evenly, so
        // consecutive durations differ and the table must say so rather than
        // averaging the drift away.
        let f = [0u8; 8];
        let buf = write_clip(&[(&f, 0), (&f, 33), (&f, 67), (&f, 100)]);
        let stts = find(&buf, &stbl_path(b"stts"));
        let entries = u32::from_be_bytes(stts[4..8].try_into().unwrap());
        assert!(entries > 1, "expected multiple runs, got {entries}");
    }

    #[test]
    fn sample_description_is_hap1_at_the_coded_size() {
        let buf = write_clip(&[(&[0u8; 8], 0)]);
        let stsd = find(&buf, &stbl_path(b"stsd"));
        // version/flags(4) + entry count(4), then the sample entry's own header.
        assert_eq!(&stsd[12..16], b"Hap1");
        // Within the entry payload: 6 reserved + 2 dri + 2 ver + 2 rev
        // + 4 vendor + 4 + 4 = 24 bytes before width/height.
        let entry = &stsd[16..];
        assert_eq!(u16::from_be_bytes(entry[24..26].try_into().unwrap()), 64);
        assert_eq!(u16::from_be_bytes(entry[26..28].try_into().unwrap()), 32);
    }

    #[test]
    fn no_stss_is_written() {
        // Deliberate, not an omission: absent stss means every sample is a sync
        // sample, which is the property that makes HAP seekable anywhere.
        let buf = write_clip(&[(&[0u8; 8], 0), (&[0u8; 8], 40)]);
        let stbl = find(&buf, &STBL);
        assert!(
            !stbl.windows(4).any(|w| w == b"stss"),
            "stss must not be present"
        );
    }

    #[test]
    fn a_zero_sample_bake_still_writes_a_parseable_file() {
        // transcode warns rather than fails when a span selects no frames; the
        // container must not become garbage in that case.
        let buf = write_clip(&[]);
        let top: Vec<[u8; 4]> = top_level(&buf).iter().map(|(k, _, _)| *k).collect();
        assert_eq!(top, vec![*b"ftyp", *b"mdat", *b"moov"]);
        let stsz = find(&buf, &stbl_path(b"stsz"));
        assert_eq!(u32::from_be_bytes(stsz[8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn durations_come_from_the_timestamps_not_the_nominal_rate() {
        let f = [0u8; 8];
        let buf = write_clip(&[(&f, 0), (&f, 100)]);
        let mvhd = find(&buf, &[b"moov", b"mvhd"]);
        // Last pts 100 + nominal frame duration 40.
        assert_eq!(u32::from_be_bytes(mvhd[16..20].try_into().unwrap()), 140);
    }

    // -----------------------------------------------------------------------
    // Reading
    //
    // The writer and reader are each other's strongest available check here:
    // `tests/mov_roundtrip.rs` already pits the writer against *ffmpeg's*
    // demuxer, so a file that survives both has been read by an implementation
    // that shares none of its assumptions. What these add is that the reader is
    // exercised on shapes the writer cannot produce — many samples per chunk,
    // uniform `stsz`, `co64` — because real clips have them and our own files
    // never will.
    // -----------------------------------------------------------------------

    #[test]
    fn demux_recovers_exactly_what_was_written() {
        let a: Vec<u8> = (0..100).map(|i| i as u8).collect();
        let b: Vec<u8> = (0..120).map(|i| (255 - i) as u8).collect();
        let c: Vec<u8> = (0..64).map(|i| (i * 3) as u8).collect();
        let buf = write_clip(&[(&a, 0), (&b, 40), (&c, 80)]);

        let t = demux(&buf).unwrap();
        assert_eq!(&t.format, b"Hap1");
        assert!(t.is_hap());
        assert_eq!((t.width, t.height), (64, 32));
        assert_eq!(t.timescale, 1000);
        assert_eq!(t.samples.len(), 3);
        assert_eq!(t.duration, 120); // last pts 80 + nominal 40

        // Byte-for-byte, not just the right lengths.
        assert_eq!(t.sample_data(&buf, 0).unwrap(), &a[..]);
        assert_eq!(t.sample_data(&buf, 1).unwrap(), &b[..]);
        assert_eq!(t.sample_data(&buf, 2).unwrap(), &c[..]);
        assert!(t.sample_data(&buf, 3).is_none());

        let pts: Vec<u64> = t.samples.iter().map(|s| s.pts).collect();
        assert_eq!(pts, vec![0, 40, 80]);
    }

    #[test]
    fn the_last_frame_survives_the_round_trip() {
        // The bug this whole file exists to have fixed: ffmpeg's muxer dropped
        // the final sample. Reading our own output back is the cheapest place
        // to keep asserting it, and it runs under wasm where ffmpeg cannot.
        let f = [7u8; 16];
        let buf = write_clip(&[(&f, 0), (&f, 40), (&f, 80), (&f, 120)]);
        assert_eq!(demux(&buf).unwrap().samples.len(), 4);
    }

    #[test]
    fn sample_at_maps_a_time_onto_the_frame_showing_then() {
        let f = [0u8; 8];
        let buf = write_clip(&[(&f, 0), (&f, 40), (&f, 80)]);
        let t = demux(&buf).unwrap();

        assert_eq!(t.sample_at(0), Some(0));
        assert_eq!(t.sample_at(39), Some(0)); // still on frame 0
        assert_eq!(t.sample_at(40), Some(1)); // exactly on the boundary
        assert_eq!(t.sample_at(79), Some(1));
        assert_eq!(t.sample_at(80), Some(2));
        // Past the end holds the last frame rather than going blank.
        assert_eq!(t.sample_at(10_000), Some(2));
    }

    #[test]
    fn an_empty_track_has_no_frame_to_show() {
        let t = demux(&write_clip(&[])).unwrap();
        assert!(t.samples.is_empty());
        assert_eq!(t.sample_at(0), None);
        assert_eq!(t.duration, 0);
    }

    /// Build a minimal file whose sample table uses shapes `MovWriter` never
    /// emits, so the general table walk is actually exercised.
    ///
    /// `chunks` is (chunk payload sizes) — each chunk holds `per_chunk` samples
    /// of `size` bytes, packed contiguously.
    fn synth(
        per_chunk: usize,
        chunks: usize,
        size: u32,
        uniform_stsz: bool,
        use_co64: bool,
    ) -> Vec<u8> {
        let n = per_chunk * chunks;
        let mut f = Vec::new();

        let b = open(&mut f, b"ftyp");
        f.extend_from_slice(b"qt  ");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(b"qt  ");
        close(&mut f, b);

        // mdat with a plain 32-bit size, which is itself a case MovWriter never
        // writes — it always uses the extended form.
        let mdat = open(&mut f, b"mdat");
        let mut chunk_offsets = Vec::new();
        for c in 0..chunks {
            chunk_offsets.push(f.len() as u64);
            for s in 0..per_chunk {
                f.extend(std::iter::repeat_n(
                    (c * per_chunk + s) as u8,
                    size as usize,
                ));
            }
        }
        close(&mut f, mdat);

        let moov = open(&mut f, b"moov");
        let trak = open(&mut f, b"trak");
        let mdia = open(&mut f, b"mdia");

        let b = open(&mut f, b"mdhd");
        f.extend_from_slice(&0u32.to_be_bytes()); // version 0
        f.extend_from_slice(&[0u8; 8]);
        f.extend_from_slice(&600u32.to_be_bytes()); // timescale
        f.extend_from_slice(&(n as u32 * 10).to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        close(&mut f, b);

        let b = open(&mut f, b"hdlr");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(b"vide");
        f.extend_from_slice(&[0u8; 13]);
        close(&mut f, b);

        let minf = open(&mut f, b"minf");
        let stbl = open(&mut f, b"stbl");

        let b = open(&mut f, b"stsd");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes());
        let e = open(&mut f, b"Hap1");
        f.extend_from_slice(&[0u8; 24]); // through spatial quality
        f.extend_from_slice(&128u16.to_be_bytes()); // width
        f.extend_from_slice(&96u16.to_be_bytes()); // height
        f.extend_from_slice(&[0u8; 50]);
        close(&mut f, e);
        close(&mut f, b);

        // One stts run covering everything.
        let b = open(&mut f, b"stts");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(&(n as u32).to_be_bytes());
        f.extend_from_slice(&10u32.to_be_bytes());
        close(&mut f, b);

        let b = open(&mut f, b"stsc");
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes()); // first_chunk
        f.extend_from_slice(&(per_chunk as u32).to_be_bytes());
        f.extend_from_slice(&1u32.to_be_bytes()); // sample description index
        close(&mut f, b);

        let b = open(&mut f, b"stsz");
        f.extend_from_slice(&0u32.to_be_bytes());
        if uniform_stsz {
            f.extend_from_slice(&size.to_be_bytes());
            f.extend_from_slice(&(n as u32).to_be_bytes());
        } else {
            f.extend_from_slice(&0u32.to_be_bytes());
            f.extend_from_slice(&(n as u32).to_be_bytes());
            for _ in 0..n {
                f.extend_from_slice(&size.to_be_bytes());
            }
        }
        close(&mut f, b);

        let b = open(&mut f, if use_co64 { b"co64" } else { b"stco" });
        f.extend_from_slice(&0u32.to_be_bytes());
        f.extend_from_slice(&(chunks as u32).to_be_bytes());
        for o in &chunk_offsets {
            if use_co64 {
                f.extend_from_slice(&o.to_be_bytes());
            } else {
                f.extend_from_slice(&(*o as u32).to_be_bytes());
            }
        }
        close(&mut f, b);

        close(&mut f, stbl);
        close(&mut f, minf);
        close(&mut f, mdia);
        close(&mut f, trak);
        close(&mut f, moov);
        f
    }

    #[test]
    fn many_samples_per_chunk_resolve_to_the_right_offsets() {
        // MovWriter writes one sample per chunk, so this shape only ever comes
        // from other tools — which is exactly why it needs its own test. Each
        // sample is filled with its own index, so a mis-walked stsc reads a
        // neighbour's bytes and the assert names which one.
        let buf = synth(4, 3, 32, false, false);
        let t = demux(&buf).unwrap();
        assert_eq!(t.samples.len(), 12);
        assert_eq!((t.width, t.height), (128, 96));
        assert_eq!(t.timescale, 600);
        for i in 0..12 {
            let d = t.sample_data(&buf, i).unwrap();
            assert_eq!(d.len(), 32);
            assert!(
                d.iter().all(|&b| b == i as u8),
                "sample {i} read wrong bytes"
            );
        }
    }

    #[test]
    fn a_uniform_stsz_expands_to_one_size_per_sample() {
        let buf = synth(2, 3, 48, true, false);
        let t = demux(&buf).unwrap();
        assert_eq!(t.samples.len(), 6);
        assert!(t.samples.iter().all(|s| s.size == 48));
        for i in 0..6 {
            assert!(t
                .sample_data(&buf, i)
                .unwrap()
                .iter()
                .all(|&b| b == i as u8));
        }
    }

    #[test]
    fn co64_offsets_are_read_as_64_bit() {
        let buf = synth(3, 2, 16, false, true);
        let t = demux(&buf).unwrap();
        assert_eq!(t.samples.len(), 6);
        for i in 0..6 {
            assert!(t
                .sample_data(&buf, i)
                .unwrap()
                .iter()
                .all(|&b| b == i as u8));
        }
    }

    #[test]
    fn pts_accumulates_from_stts_deltas() {
        let t = demux(&synth(2, 2, 8, true, false)).unwrap();
        let pts: Vec<u64> = t.samples.iter().map(|s| s.pts).collect();
        assert_eq!(pts, vec![0, 10, 20, 30]);
        assert!(t.samples.iter().all(|s| s.duration == 10));
    }

    #[test]
    fn a_file_with_no_moov_is_an_error_not_a_panic() {
        // A download cut off before the index. `moov` is written last, so this
        // is the shape a real interrupted fetch takes.
        let buf = write_clip(&[(&[1u8; 32], 0)]);
        let moov_at = top_level(&buf)[2].1;
        assert_eq!(demux(&buf[..moov_at]), Err(MovErr::Missing("moov")));
    }

    #[test]
    fn a_file_cut_off_inside_moov_names_the_box_that_did_not_fit() {
        // One byte further in and the failure is different in kind: the box
        // header arrived and promised more than the file holds. Worth keeping
        // distinct — "the index is missing" and "the index is damaged" are
        // different problems for whoever reads the error.
        let buf = write_clip(&[(&[1u8; 32], 0)]);
        let moov_at = top_level(&buf)[2].1;
        assert_eq!(
            demux(&buf[..moov_at + 16]),
            Err(MovErr::Truncated { kind: *b"moov" })
        );
    }

    #[test]
    fn a_box_claiming_more_than_its_parent_is_rejected() {
        let mut buf = write_clip(&[(&[1u8; 32], 0)]);
        // Inflate ftyp's length word past the end of the file.
        buf[0..4].copy_from_slice(&0xffff_0000u32.to_be_bytes());
        assert_eq!(demux(&buf), Err(MovErr::Truncated { kind: *b"ftyp" }));
    }

    #[test]
    fn a_sample_pointing_outside_the_file_is_rejected() {
        // A plausible corruption: the index survives but points nowhere. Better
        // to fail here than to hand `hap::decode_frame` someone else's bytes.
        let mut buf = write_clip(&[(&[1u8; 32], 0)]);
        let stco_at = buf
            .windows(4)
            .rposition(|w| w == b"stco")
            .expect("stco present");
        let off = stco_at + 4 + 8; // past version/flags and entry count
        buf[off..off + 4].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
        assert_eq!(demux(&buf), Err(MovErr::SampleOutOfBounds { index: 0 }));
    }

    #[test]
    fn a_track_that_is_not_video_is_skipped_rather_than_returned() {
        let mut buf = synth(1, 2, 16, true, false);
        // Turn the handler from 'vide' into 'soun'. The file is then a valid
        // audio-only movie, and a demuxer that ignores hdlr would happily hand
        // back its samples as if they were frames.
        let at = buf.windows(4).position(|w| w == b"vide").expect("hdlr");
        buf[at..at + 4].copy_from_slice(b"soun");
        assert_eq!(demux(&buf), Err(MovErr::NoVideoTrack));
    }
}
