//! The plain-data frame types the render pass consumes, and the Hap1 bitstream
//! parser that produces them.
//!
//! Nothing here opens a container or a device. That is the shell's job, and it
//! is precisely what differs between the native player (ffmpeg demux, a decode
//! thread per clip, `AVFoundation` capture) and the browser one (a byte slice
//! from a file input, walked by `vidiotic_bake::mov::demux`, pulled by the
//! render loop). Both ends produce the same [`frame::DecodedFrame`].

pub mod frame;

/// CPU block decompression, for a GPU with no `texture-compression-bc`. Not a
/// codec — HAP's bitstream is [`hap`]'s job either way — but the last step of
/// it, done in software because the device cannot sample the result.
pub mod softdec;

/// Hap1 bitstream parsing. Shared with the baker that writes it, so the codec
/// itself lives in `vidiotic-bake`; this is the decode-side name for it.
pub use vidiotic_bake::hap;
