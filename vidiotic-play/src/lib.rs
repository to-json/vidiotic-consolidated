//! The portable render core: wgpu setup, the composite pass, and the shader
//! compiler — everything the player draws with and nothing it draws *from*.
//!
//! Split out of `vidiotic` for the browser build (web-port.md §8 step 4). The
//! line is drawn at the OS: this crate names no filesystem, no ffmpeg, no audio
//! device, and no socket, so it crosses to `wasm32-unknown-unknown` intact. What
//! stayed behind is the native shell — the window/audio/IPC/capture layers and
//! the `Command` vocabulary they speak. `vidiotic` re-exports these modules at
//! its own root, so the split is invisible to its internals.
//!
//! Two things follow from that line and are worth stating, because both are
//! easy to erase by accident:
//!
//! - **Decoding is the caller's job.** A container walk and an image decode are
//!   OS-shaped on native and async in a browser, so neither happens here.
//!   [`render::Renderer::upload_frame`] takes an already-decoded
//!   [`video::frame::DecodedFrame`], and ISF `IMPORTED` images arrive through an
//!   injected [`render::StillLoader`].
//! - **Nothing here blocks.** `pollster` is a native-only dependency precisely
//!   so that blocking on a browser future fails to build rather than hanging.

pub mod analysis;
pub mod clip;
pub mod clock;
pub mod commands;
pub mod engine;
pub mod gfx;
pub mod keymap;
pub mod render;
pub mod sequencer;
pub mod shader;
pub mod ui;
pub mod undo;
pub mod video;

/// The browser shell: canvases, the `requestAnimationFrame` loop, clip ingest,
/// and the control panel. Everything JS can call lives here.
///
/// Absent natively, where `vidiotic` is the shell instead — which is why this
/// crate can be both the render core the native player links and the whole of
/// the web player without either half carrying the other's baggage.
#[cfg(target_arch = "wasm32")]
pub mod web;

/// The model types the renderer reads: chain slots, shader identity, and the
/// ISF schema it compiles. Re-exported rather than declared — as `vidiotic`
/// does at its own root — so `crate::isf::…` and `crate::chain::…` mean the same
/// thing on both sides of the split, and the moved sources needed no edit.
pub use vidiotic_core::{bank, chain, clippool, isf};

/// This checkout's shader directory, baked in at compile time.
///
/// It exists only so `vidiotic::assets` can still name the directory after it
/// moved here with the `include_str!`s that consume it. Meaningful for a cargo
/// run from the repo and nothing else: a bundled `.app` reads
/// `Contents/Resources/shaders`, and a browser build has no filesystem at all —
/// hence the cfg, which makes reaching for it on the web a compile error.
#[cfg(not(target_arch = "wasm32"))]
pub const REPO_SHADERS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/shaders");
