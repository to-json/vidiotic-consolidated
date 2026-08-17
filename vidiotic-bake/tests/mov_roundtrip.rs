//! Does the pure-Rust muxer produce a file the rest of the world can read?
//!
//! web-port.md §8 step 2 replaces ffmpeg's muxer with [`vidiotic_bake::mov`].
//! Structural self-checks live in `mov.rs`'s own unit tests and prove the boxes
//! are internally consistent, which is not the same as being *correct* — a
//! consistently wrong file is still wrong. So this suite hands the output to
//! **ffmpeg's demuxer**, the exact component being replaced, and asserts it
//! recovers every packet byte-for-byte along with the codec tag, dimensions and
//! timing. The tool being retired is the one that certifies its replacement.
//!
//! It also muxes identical packets through *both* muxers and requires ffmpeg to
//! read the same stream out of each, so "as good as what we had" is a checked
//! claim rather than a hope.
//!
//! This suite is `ffmpeg`-gated and native-only by nature — it needs the thing
//! the browser build does not have. The portable half of the coverage is in
//! `mov.rs`'s unit tests, which keep running under wasm.

#![cfg(feature = "ffmpeg")]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use ffmpeg_next as ff;
use vidiotic_bake::frame::{BakeQuality, FrameBaker};
use vidiotic_bake::hap;
use vidiotic_bake::mov::MovWriter;

const W: u32 = 64;
const H: u32 = 48;
const TIMESCALE: u32 = 1000;
const FRAME_MS: u32 = 40; // 25 fps, which divides 1000 exactly

/// Deterministic frames that differ from each other, so a muxer that mixed up
/// sample offsets would produce mismatches rather than accidental passes.
fn frames(n: usize) -> Vec<Vec<u8>> {
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
            px
        })
        .collect()
}

/// Bake `n` frames into Hap1 packets, paired with their millisecond timestamps.
fn packets(n: usize) -> Vec<(Vec<u8>, u32)> {
    let mut baker = FrameBaker::new(W, H, BakeQuality::Draft).unwrap();
    frames(n)
        .iter()
        .enumerate()
        .map(|(i, f)| (baker.bake(f).unwrap(), i as u32 * FRAME_MS))
        .collect()
}

/// Test files go somewhere writable and uniquely named. `Path::new(file!())`
/// keeps the name traceable back to this suite if one is ever left behind.
fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("vidiotic-mov-{name}-{}.mov", std::process::id()));
    p
}

/// Mux with our writer.
fn mux_ours(path: &Path, pkts: &[(Vec<u8>, u32)]) {
    let f = std::fs::File::create(path).expect("create");
    let mut w = MovWriter::new(f, W, H, TIMESCALE, FRAME_MS).expect("mov header");
    for (data, pts) in pkts {
        w.write_sample(data, *pts).expect("sample");
    }
    assert_eq!(w.sample_count(), pkts.len());
    w.finish().expect("finish");
}

/// Mux the same packets the way `transcode.rs` used to, through libavformat.
fn mux_ffmpeg(path: &Path, pkts: &[(Vec<u8>, u32)]) {
    ff::init().unwrap();
    let mut octx = ff::format::output(path).expect("output");
    {
        let mut stream = octx.add_stream(ff::codec::Id::HAP).expect("add_stream");
        stream.set_time_base((1, TIMESCALE as i32));
        // Same hand-filled codec parameters the old bake path needed: there is
        // no HAP encoder for libavformat to interrogate.
        unsafe {
            let par = (*stream.as_mut_ptr()).codecpar;
            (*par).codec_type = ff::sys::AVMediaType::AVMEDIA_TYPE_VIDEO;
            (*par).codec_id = ff::sys::AVCodecID::AV_CODEC_ID_HAP;
            (*par).codec_tag = u32::from_le_bytes(*b"Hap1");
            (*par).width = W as i32;
            (*par).height = H as i32;
            (*par).format = ff::sys::AVPixelFormat::AV_PIX_FMT_RGBA as i32;
        }
    }
    octx.write_header().expect("write_header");
    let out_tb = octx.stream(0).unwrap().time_base();
    for (data, pts) in pkts {
        let mut pkt = ff::codec::packet::Packet::copy(data);
        let scaled = i64::from(*pts) * i64::from(out_tb.denominator())
            / (i64::from(TIMESCALE) * i64::from(out_tb.numerator()));
        pkt.set_pts(Some(scaled));
        pkt.set_dts(Some(scaled));
        pkt.set_stream(0);
        pkt.set_flags(ff::codec::packet::Flags::KEY);
        pkt.write_interleaved(&mut octx).expect("write packet");
    }
    octx.write_trailer().expect("trailer");
}

struct Demuxed {
    codec_tag: u32,
    width: u32,
    height: u32,
    /// Stream time base as a rational, for checking the timeline.
    time_base: (i32, i32),
    packets: Vec<(Vec<u8>, i64)>,
    stream_count: usize,
    /// Whether the demuxer considers every packet a keyframe.
    all_key: bool,
}

fn demux(path: &Path) -> Demuxed {
    ff::init().unwrap();
    let mut ictx = ff::format::input(path).expect("ffmpeg could not open the file at all");
    let stream_count = ictx.streams().count();
    let (idx, codec_tag, width, height, time_base) = {
        let st = ictx
            .streams()
            .best(ff::media::Type::Video)
            .expect("no video stream");
        let par = st.parameters();
        // codec_tag is not surfaced by ffmpeg-next's safe API.
        let (tag, w, h) = unsafe {
            let p = par.as_ptr();
            ((*p).codec_tag, (*p).width as u32, (*p).height as u32)
        };
        let tb = st.time_base();
        (st.index(), tag, w, h, (tb.numerator(), tb.denominator()))
    };

    let mut packets = Vec::new();
    let mut all_key = true;
    for (stream, packet) in ictx.packets() {
        if stream.index() != idx {
            continue;
        }
        if !packet.is_key() {
            all_key = false;
        }
        packets.push((
            packet.data().expect("empty packet").to_vec(),
            packet.pts().unwrap_or(-1),
        ));
    }

    Demuxed {
        codec_tag,
        width,
        height,
        time_base,
        packets,
        stream_count,
        all_key,
    }
}

#[test]
fn ffmpeg_recovers_every_packet_byte_for_byte() {
    // The whole point. If this passes, the container is not lying about where
    // the samples are or how big they are.
    let pkts = packets(12);
    let path = tmp("roundtrip");
    mux_ours(&path, &pkts);
    let got = demux(&path);

    assert_eq!(got.stream_count, 1, "exactly one track");
    assert_eq!(got.packets.len(), pkts.len(), "packet count");
    for (i, ((want, _), (have, _))) in pkts.iter().zip(&got.packets).enumerate() {
        assert_eq!(want, have, "packet {i} came back different");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_stream_is_tagged_hap1_at_the_baked_size() {
    // A player picks the HAP decode path off this tag. Wrong tag means the file
    // opens and then plays garbage, which is the worst failure mode available.
    let path = tmp("tag");
    mux_ours(&path, &packets(3));
    let got = demux(&path);

    assert_eq!(
        got.codec_tag.to_le_bytes(),
        *b"Hap1",
        "codec tag should be Hap1, got {:?}",
        std::str::from_utf8(&got.codec_tag.to_le_bytes())
    );
    assert_eq!((got.width, got.height), (W, H));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn timestamps_land_on_the_declared_timeline() {
    let pkts = packets(8);
    let path = tmp("timing");
    mux_ours(&path, &pkts);
    let got = demux(&path);

    // The muxer was told timescale 1000, and unlike libavformat it does not get
    // to pick its own — so pts values should come back unrescaled.
    assert_eq!(got.time_base, (1, TIMESCALE as i32));
    for (i, ((_, want), (_, have))) in pkts.iter().zip(&got.packets).enumerate() {
        assert_eq!(i64::from(*want), *have, "packet {i} pts");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn every_frame_is_seekable() {
    // No `stss` is written, which is the container's way of saying all samples
    // are sync samples. Check the demuxer actually reads it that way — the
    // player's scrubbing depends on it.
    let path = tmp("keyframes");
    mux_ours(&path, &packets(6));
    let got = demux(&path);
    assert!(got.all_key, "every HAP sample must demux as a keyframe");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn what_comes_back_out_still_decodes_as_hap() {
    // End to end: bake -> mux -> ffmpeg demux -> our decoder -> BC1 payload of
    // exactly the right size. This is the full path the player walks.
    let path = tmp("decode");
    mux_ours(&path, &packets(4));
    let got = demux(&path);

    let (mut main, mut alpha) = (Vec::new(), Vec::new());
    for (i, (data, _)) in got.packets.iter().enumerate() {
        let meta = hap::decode_frame(data, 1, &mut main, &mut alpha)
            .unwrap_or_else(|e| panic!("demuxed packet {i} failed to decode: {e}"));
        assert_eq!(meta.format, hap::HapTextureFormat::Bc1);
        assert_eq!(main.len(), (W * H / 2) as usize, "packet {i} payload size");
        assert!(alpha.is_empty());
    }
    let _ = std::fs::remove_file(&path);
}

/// Why the muxer was replaced, stated as a test rather than as a claim.
///
/// The same packets go through both muxers. libavformat's **drops the last one**
/// — the bake supplies no per-packet duration, and with no following packet the
/// mov muxer has nothing to derive the final sample's duration from, so the
/// sample does not survive. It also substitutes its own timescale for the one it
/// was given.
///
/// This test asserts both behaviours deliberately. If a future ffmpeg fixes
/// them it will fail, and that is the correct outcome: the note in
/// `transcode.rs` explaining why the bake stopped using libavformat would then
/// be out of date.
///
/// Everything ffmpeg *did* keep is byte-identical to ours, which is the other
/// half of the point — the replacement lost nothing.
#[test]
fn ffmpegs_muxer_loses_the_last_frame_and_ours_does_not() {
    let pkts = packets(10);
    let (a, b) = (tmp("ours"), tmp("theirs"));
    mux_ours(&a, &pkts);
    mux_ffmpeg(&b, &pkts);

    let (ours, theirs) = (demux(&a), demux(&b));

    assert_eq!(
        ours.packets.len(),
        pkts.len(),
        "ours must keep every packet"
    );
    assert_eq!(
        theirs.packets.len(),
        pkts.len() - 1,
        "libavformat is expected to drop exactly the final packet"
    );

    // Every packet ffmpeg did write matches ours byte for byte, so the only
    // difference between the two files is the one it lost.
    for (i, ((od, _), (td, _))) in ours.packets.iter().zip(&theirs.packets).enumerate() {
        assert_eq!(od, td, "packet {i} differs between muxers");
    }
    assert_eq!(ours.codec_tag, theirs.codec_tag, "codec tag");
    assert_eq!((ours.width, ours.height), (theirs.width, theirs.height));
    assert_eq!(ours.all_key, theirs.all_key, "keyframe flags");

    // The timescale the caller asked for is honoured by ours and overridden by
    // theirs — the reason `transcode.rs` used to carry a rescale helper.
    assert_eq!(ours.time_base, (1, TIMESCALE as i32));
    assert_ne!(
        theirs.time_base,
        (1, TIMESCALE as i32),
        "libavformat is expected to pick its own timescale"
    );

    let _ = std::fs::remove_file(&a);
    let _ = std::fs::remove_file(&b);
}

#[test]
fn a_bake_written_to_memory_is_the_same_file_as_one_written_to_disk() {
    // The browser has no filesystem: it muxes into a `Cursor<Vec<u8>>` and hands
    // the bytes to OPFS or a download. That path must not be a different
    // codepath in disguise.
    let pkts = packets(5);
    let path = tmp("ondisk");
    mux_ours(&path, &pkts);

    let mut w = MovWriter::new(Cursor::new(Vec::new()), W, H, TIMESCALE, FRAME_MS).unwrap();
    for (data, pts) in &pkts {
        w.write_sample(data, *pts).unwrap();
    }
    let in_memory = w.finish().unwrap().into_inner();

    assert_eq!(
        std::fs::read(&path).unwrap(),
        in_memory,
        "in-memory and on-disk output must be byte-identical"
    );
    let _ = std::fs::remove_file(&path);
}
