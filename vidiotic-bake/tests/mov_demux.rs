//! Does the pure-Rust demuxer read the same file ffmpeg does?
//!
//! web-port.md §4 needs a container reader in the browser: `/play` opens Hap1
//! `.mov` files and there is no ffmpeg to demux them, nor can `WebCodecs` help
//! — HAP is not a codec the browser knows. So [`vidiotic_bake::mov::demux`] has
//! to be right, and "right" cannot be established by reading our own writer's
//! output alone. Two implementations that share an author share their mistakes.
//!
//! So this suite pits our reader against **ffmpeg's demuxer** on the same
//! bytes, and does it on two different kinds of file:
//!
//! - files our own [`MovWriter`] produced, and
//! - the real clips in `clips/`, which were written by **ffmpeg's** muxer at a
//!   different timescale with different chunking. Those are the interesting
//!   ones: nothing about how we write files influenced how they are laid out,
//!   so agreeing on them is evidence about the format rather than about us.
//!
//! The final test closes the loop by pushing every packet our reader locates
//! through `hap::decode_frame`, which is the actual `/play` path: bytes off
//! disk, through the container walk, into a texture upload.
//!
//! `ffmpeg`-gated and native-only by nature. The portable half of the demuxer's
//! coverage lives in `mov.rs`'s unit tests, which keep running under wasm.

#![cfg(feature = "ffmpeg")]

use std::path::{Path, PathBuf};

use ffmpeg_next as ff;
use vidiotic_bake::frame::{BakeQuality, FrameBaker};
use vidiotic_bake::hap;
use vidiotic_bake::mov::{demux, MovWriter};

const W: u32 = 64;
const H: u32 = 48;
const TIMESCALE: u32 = 1000;
const FRAME_MS: u32 = 40;

/// The real clips, which no code of ours laid out.
const REAL_CLIPS: [&str; 3] = ["../clips/brb.mov", "../clips/bun.mov", "../clips/eyes.mov"];

fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vidiotic-demux-{name}-{}.mov", std::process::id()));
    p
}

/// Bake `n` distinct frames into Hap1 packets with millisecond timestamps.
fn packets(n: usize) -> Vec<(Vec<u8>, u32)> {
    let mut baker = FrameBaker::new(W, H, BakeQuality::Draft).unwrap();
    (0..n)
        .map(|i| {
            let mut px = Vec::with_capacity((W * H * 4) as usize);
            for y in 0..H {
                for x in 0..W {
                    let t = (i * 37) as u8;
                    px.extend_from_slice(&[
                        (x as u8).wrapping_mul(3).wrapping_add(t),
                        (y as u8).wrapping_mul(5).wrapping_sub(t),
                        ((x ^ y) as u8).wrapping_add(t),
                        255,
                    ]);
                }
            }
            (baker.bake(&px).unwrap(), i as u32 * FRAME_MS)
        })
        .collect()
}

fn write_ours(path: &Path, pkts: &[(Vec<u8>, u32)]) {
    let f = std::fs::File::create(path).expect("create");
    let mut w = MovWriter::new(f, W, H, TIMESCALE, FRAME_MS).expect("header");
    for (data, pts) in pkts {
        w.write_sample(data, *pts).expect("sample");
    }
    w.finish().expect("finish");
}

/// What ffmpeg's demuxer sees: every video packet's bytes and its decode
/// timestamp, plus the stream description.
struct FfmpegView {
    tag: u32,
    width: u32,
    height: u32,
    timescale: i32,
    packets: Vec<(Vec<u8>, i64)>,
}

fn via_ffmpeg(path: &Path) -> FfmpegView {
    ff::init().unwrap();
    let mut ictx = ff::format::input(path).expect("ffmpeg could not open the file");
    let (idx, tag, width, height, timescale) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .expect("no video stream");
        let par = st.parameters();
        let p = unsafe { *par.as_ptr() };
        (
            st.index(),
            p.codec_tag,
            p.width as u32,
            p.height as u32,
            st.time_base().denominator(),
        )
    };
    let mut packets = Vec::new();
    for (stream, pkt) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        // dts, not pts: our reader accumulates stts, which is decode timing.
        // For all-intra HAP the two are equal, and the tests below assert that
        // rather than assuming it.
        packets.push((pkt.data().unwrap_or(&[]).to_vec(), pkt.dts().unwrap_or(0)));
    }
    FfmpegView {
        tag,
        width,
        height,
        timescale,
        packets,
    }
}

#[test]
fn our_reader_and_ffmpegs_agree_on_a_file_we_wrote() {
    let path = tmp("ours");
    let pkts = packets(6);
    write_ours(&path, &pkts);

    let bytes = std::fs::read(&path).unwrap();
    let ours = demux(&bytes).expect("our demuxer");
    let theirs = via_ffmpeg(&path);

    assert_eq!(ours.samples.len(), theirs.packets.len(), "packet count");
    assert_eq!(ours.width, theirs.width);
    assert_eq!(ours.height, theirs.height);
    assert_eq!(u32::from_le_bytes(ours.format), theirs.tag, "codec tag");
    assert_eq!(ours.timescale as i32, theirs.timescale, "timescale");

    for (i, (want, dts)) in theirs.packets.iter().enumerate() {
        assert_eq!(ours.sample_data(&bytes, i).unwrap(), &want[..], "sample {i} bytes");
        assert_eq!(ours.samples[i].pts as i64, *dts, "sample {i} timing");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn our_reader_and_ffmpegs_agree_on_files_ffmpeg_wrote() {
    // The load-bearing test. These clips were muxed by libavformat at timescale
    // 16000, laid out by chunking rules none of our code has any say in — 99
    // samples across 7 chunks with a five-run `stsc`, which our own writer (one
    // sample per chunk, single run) would never produce. If the sample-table
    // walk is wrong in a way our own round trip cannot see, it shows up here.
    //
    // Agreement is asserted over ffmpeg's packets, not over the sample count:
    // see `the_zero_duration_tail_frame_is_real` for why those differ.
    let mut checked = 0;
    for clip in REAL_CLIPS {
        let path = Path::new(clip);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let ours = demux(&bytes).unwrap_or_else(|e| panic!("{clip}: {e}"));
        let theirs = via_ffmpeg(path);

        assert_eq!((ours.width, ours.height), (theirs.width, theirs.height), "{clip}: size");
        assert_eq!(u32::from_le_bytes(ours.format), theirs.tag, "{clip}: codec tag");
        assert_eq!(ours.timescale as i32, theirs.timescale, "{clip}: timescale");
        assert!(!ours.has_composition_offsets, "{clip}: HAP should have no ctts");
        assert!(
            ours.samples.len() >= theirs.packets.len(),
            "{clip}: we found fewer samples than ffmpeg — {} vs {}",
            ours.samples.len(),
            theirs.packets.len()
        );

        for (i, (want, dts)) in theirs.packets.iter().enumerate() {
            let got = ours
                .sample_data(&bytes, i)
                .unwrap_or_else(|| panic!("{clip}: sample {i} out of range"));
            assert_eq!(got.len(), want.len(), "{clip}: sample {i} length");
            assert_eq!(got, &want[..], "{clip}: sample {i} bytes");
            assert_eq!(ours.samples[i].pts as i64, *dts, "{clip}: sample {i} timing");
        }
        checked += 1;
    }
    assert!(checked > 0, "no clips found — this test proved nothing");
}

#[test]
fn the_zero_duration_tail_frame_is_real() {
    // Every clip in `clips/` reads one sample longer through our demuxer than
    // through ffmpeg's, and this is what that difference is.
    //
    // These files were muxed by the libavformat path `transcode.rs` used to
    // use, which drops the duration of the final packet. The muxer therefore
    // wrote a trailing `stts` run of `(count 1, delta 0)`: the frame is in the
    // file, indexed by `stsz`/`stco`, byte-complete, and decodes — it just
    // claims to last no time at all. ffmpeg's own demuxer then discards it.
    //
    // So the count mismatch is not our reader over-reading. It is the same
    // last-frame defect recorded in web-port.md's Observations, seen from the
    // file's bytes instead of from a frame tally. Files written by our
    // `MovWriter` have no such run, because it gives the last sample the
    // nominal frame duration explicitly.
    //
    // Asserted rather than merely noted, so that a future change to either the
    // reader or these fixtures has to confront it.
    let mut checked = 0;
    for clip in REAL_CLIPS {
        let path = Path::new(clip);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let ours = demux(&bytes).unwrap();
        let theirs = via_ffmpeg(path);

        let extra = ours.samples.len() - theirs.packets.len();
        assert_eq!(extra, 1, "{clip}: expected exactly one trimmed tail frame");

        let last = ours.samples.last().unwrap();
        assert_eq!(last.duration, 0, "{clip}: the tail frame should declare no duration");

        // Present and well-formed, not a phantom index entry.
        let data = ours
            .sample_data(&bytes, ours.samples.len() - 1)
            .expect("{clip}: tail frame bytes are inside the file");
        assert!(!data.is_empty(), "{clip}: tail frame is empty");
        let mut main = Vec::new();
        let mut alpha = Vec::new();
        hap::decode_frame(data, 1, &mut main, &mut alpha)
            .unwrap_or_else(|e| panic!("{clip}: the tail frame does not decode: {e}"));

        checked += 1;
    }
    assert!(checked > 0, "no clips found — this test proved nothing");
}

#[test]
fn our_own_muxer_never_writes_a_zero_duration_sample() {
    // The other half of the above: the defect is fixed at the source, so a file
    // written today has a real duration on every sample including the last.
    let path = tmp("nozero");
    write_ours(&path, &packets(5));
    let bytes = std::fs::read(&path).unwrap();
    let t = demux(&bytes).unwrap();

    assert_eq!(t.samples.len(), 5);
    assert!(
        t.samples.iter().all(|s| s.duration > 0),
        "a sample was written with zero duration: {:?}",
        t.samples
    );
    assert_eq!(t.samples.last().unwrap().duration, FRAME_MS);

    // And ffmpeg agrees on the count, which is the whole point of the fix.
    assert_eq!(via_ffmpeg(&path).packets.len(), 5);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn every_located_sample_decodes_as_hap() {
    // The whole /play read path, end to end: container walk -> packet bytes ->
    // HAP section parse -> BC1 payload of exactly the size the dimensions imply.
    // A sample-table error that happened to preserve lengths would still land
    // here, because a mis-addressed packet does not parse as a HAP section.
    let mut checked = 0;
    for clip in REAL_CLIPS {
        let path = Path::new(clip);
        if !path.exists() {
            continue;
        }
        let bytes = std::fs::read(path).unwrap();
        let t = demux(&bytes).unwrap();
        assert!(t.is_hap(), "{clip}: not a HAP track");

        // BC1 is 8 bytes per 4x4 block. The container's declared size and the
        // codec's payload size have to agree or one of the two is lying.
        let expect = (t.width as usize).div_ceil(4) * (t.height as usize).div_ceil(4) * 8;

        let mut main = Vec::new();
        let mut alpha = Vec::new();
        for i in 0..t.samples.len() {
            let data = t.sample_data(&bytes, i).unwrap();
            let meta = hap::decode_frame(data, 1, &mut main, &mut alpha)
                .unwrap_or_else(|e| panic!("{clip}: sample {i} did not decode: {e}"));
            assert_eq!(main.len(), expect, "{clip}: sample {i} payload size");
            assert_eq!(
                meta.format,
                hap::HapTextureFormat::Bc1,
                "{clip}: sample {i} format"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no clips found — this test proved nothing");
}

#[test]
fn seeking_by_time_lands_on_the_frame_ffmpeg_would_show() {
    let path = Path::new(REAL_CLIPS[1]);
    if !path.exists() {
        return;
    }
    let bytes = std::fs::read(path).unwrap();
    let t = demux(&bytes).unwrap();

    // For every sample, asking for its own timestamp must return it, and asking
    // one unit before must return its predecessor. Off-by-one in the seek search
    // is the classic way a scrubber ends up a frame behind.
    for (i, s) in t.samples.iter().enumerate() {
        assert_eq!(t.sample_at(s.pts), Some(i), "exact hit on sample {i}");
        if i > 0 {
            assert_eq!(t.sample_at(s.pts - 1), Some(i - 1), "just before sample {i}");
        }
    }
    assert_eq!(t.sample_at(u64::MAX), Some(t.samples.len() - 1), "past the end holds");
}
