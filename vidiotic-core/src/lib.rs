//! `vidiotic-core`: the session model shared by the player and the span editor.
//!
//! Everything here is below the presentation layer. The rule that keeps this
//! crate honest: **nothing in it may touch wgpu, winit, egui, cpal, or a window
//! server.** What it does own is the content model ([`bank`], [`clippool`]),
//! the identity and musical-time vocabulary those are expressed in ([`chain`],
//! [`time`]), the ISF parser/transpiler ([`isf`]), the `.viproj` on-disk format
//! ([`project`]), and the GLSL uniform contract every shader path compiles
//! against ([`PREAMBLE`]).
//!
//! It exists because `vidiotic-prep` needs the project schema and nothing else:
//! before the split it depended on the `vidiotic` binary crate and therefore
//! compiled the entire GPU, window, and audio stack to read a `.viproj`.
//! `vidiotic` re-exports these modules at its own root, so its internal
//! `crate::project::…` paths are unchanged.
//!
//! Shader *compilation* is not here — [`isf::transpile`] emits GLSL text, and
//! `vidiotic::shader` is what hands that text to naga.

pub mod bank;
pub mod bundle;
pub mod chain;
pub mod clippool;
pub mod isf;
pub mod project;
pub mod time;

/// The GLSL prelude every user shader is compiled on top of: the uniform
/// blocks, sampler bindings, and Shadertoy-compatible aliases that define
/// vidiotic's shader contract (sets 0/1/2).
///
/// It lives with the model rather than with the renderer because [`isf`] builds
/// its transpiled output on it — the ISF path appends a `set = 3` parameter UBO
/// and reuses everything below it. The text is inert here; `vidiotic::shader`
/// owns the naga parse/validate that gives it meaning.
pub const PREAMBLE: &str = include_str!("../shaders/preamble.frag");
