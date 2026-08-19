//! vidiotic — VJ controller: audio-reactive live-reloaded shader over video clips,
//! driven by an app-owned beat clock.
//!
//! This crate is the native shell: the window, audio, IPC, capture, and clock
//! layers, plus the `Command` vocabulary its input surfaces speak. Three crates
//! sit under it and are all re-exported below, so `crate::project::…`,
//! `crate::render::…`, and `crate::transcode::…` mean here what they always
//! have: the session model in `vidiotic-core`, the Hap1 bake in `vidiotic-bake`,
//! and the portable render core in `vidiotic-play`.

pub mod analysis;
pub mod app;
pub mod assets;
pub mod audio;
pub mod control_input;
pub mod ipc;
pub mod shaderwatch;
pub mod ui;
pub mod video;

/// The session model: content banks, the clip pool, ISF, and the `.viproj`
/// format. Re-exported rather than declared so the split from `vidiotic-core`
/// stays invisible to this crate's internals — and so `vidiotic-prep` can take
/// the same modules without linking a renderer.
pub use vidiotic_core::{bank, clippool, isf, project};

/// The offline Hap1 bake. `crate::video::hap` re-exports its codec half.
pub use vidiotic_bake::transcode;

/// The portable player: the render core — wgpu setup ([`gfx`]), the composite
/// pass ([`render`]), the GLSL/WGSL→naga compiler ([`shader`]) — and the engine
/// that drives it: the [`commands`] vocabulary, the [`keymap`] that produces
/// them, the beat [`clock`], the [`sequencer`], and the [`undo`] history.
///
/// Split out for the browser build (web-port.md §8 step 4). The line is the OS,
/// not the feature set: none of this names a filesystem, a socket, an audio
/// device, or ffmpeg, which is exactly what lets it cross to wasm32. What stayed
/// behind is the shell those things live in — the window loop, audio capture,
/// IPC, camera capture, and disk hot-reload.
///
/// Re-exported so `crate::render::…`, `crate::keymap::…` and the rest are
/// unchanged here; `crate::video::frame` comes through `video/mod.rs` instead,
/// because `decoder` and `capture` are still declared locally there.
pub use vidiotic_play::{clock, commands, gfx, keymap, render, sequencer, shader, undo};
