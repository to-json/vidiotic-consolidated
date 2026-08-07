//! `vidiotic-bake`: the Hap1 side of the pipeline, on both sides of the file.
//!
//! [`hap`] is the Hap1 bitstream itself — section headers, snappy chunking, BC1
//! payloads — read by the player's decoder and written by the baker. [`frame`]
//! is one frame's worth of bake: tight RGBA in, Hap1 packet out. `transcode`
//! is the offline bake around it: decode a source (or a span of one), run each
//! frame through [`frame::FrameBaker`], and mux the result into a Hap1 `.mov`
//! that the player can upload to the GPU without a CPU-side format conversion.
//!
//! The `ffmpeg` feature (on by default) is the line between the two halves.
//! `hap` and [`frame`] are pure Rust and build for `wasm32-unknown-unknown`;
//! `transcode` is ffmpeg's demuxer and muxer and does not. The browser baker
//! (web-port.md §8 step 3) takes the portable half and supplies its own
//! container handling — which is only safe because both halves go through the
//! same [`frame::FrameBaker`], so a clip baked in the browser is byte-identical
//! to one baked natively.
//!
//! Split out of `vidiotic` so `vidiotic-prep` can drive a bake without linking
//! the player's GPU, window, and audio stack. Nothing here touches wgpu or
//! opens a window; the one texture-format type the renderer needs
//! ([`hap::HapTextureFormat`]) is a plain enum it maps to a `wgpu` format on
//! its own side.
//!
//! Build note: the BC1 encoder and its rayon fan-out are unusably slow
//! unoptimized, so the workspace root pins `opt-level = 3` for `texpresso`,
//! `rayon`, and `snap` even in dev profiles.

pub mod frame;
pub mod hap;
pub mod mov;
#[cfg(feature = "ffmpeg")]
pub mod transcode;

// The browser-side driver for the same compressor and muxer above. Both web
// shells (`/play`'s ingest, `/chop`'s export) bake through it, which is why it
// is here and not in either of them.
#[cfg(target_arch = "wasm32")]
pub mod web;
