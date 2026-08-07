//! `/chop`: a marking session, and the panels that draw it.
//!
//! Everything here compiles for a browser. That is the entire point of the
//! crate boundary — `vidiotic-prep` used to hold all of this alongside an
//! ffmpeg decoder, `rfd`, a `.vprep` sidecar and a unix socket, and the panels
//! took `&mut PrepApp`, so 1,173 lines of egui containing no OS call of their
//! own could not be compiled for the target they were destined for
//! (web-port.md §2).
//!
//! Splitting the type was step one ([`editor`]); splitting the crate is what
//! makes it stay split. A `use std::fs` added to [`ui`] is now a build failure
//! on a row of `scripts/wasm-gate.sh` rather than a thing someone notices later.
//! This port's recurring lesson is that the compiler for the target is the only
//! honest reviewer, so it gets to be the reviewer.
//!
//! # The shape
//!
//! - [`Editor`](editor::Editor) is the marking session: spans, marks, the
//!   playhead, the jog window, undo. [`Editor::step`](editor::Editor::step)
//!   runs every command that acts only on those.
//! - Anything needing a machine comes back out of `step` as a `Some(Command)`
//!   for a shell to answer. [`commands::Command`] is therefore the complete
//!   list of what this crate can ask for, and it is short.
//! - [`PrepMirror`](mirror::PrepMirror) is what a shell volunteers in return,
//!   and it is two fields.
//! - [`ui::draw`] and [`timeline::timeline`] take those two and nothing else.
//!
//! There is no shell in here, not even a trait for one. A shell is whoever
//! calls `step` and answers what comes back — natively `vidiotic-prep`, in a
//! browser the one still to be written.

pub mod commands;
pub mod editor;
pub mod export;
pub mod keymap;
pub mod mirror;
pub mod project;
pub mod session;
pub mod spans;
pub mod timeline;
pub mod ui;
pub mod undo;

// The browser shell. None of it exists natively — `vidiotic-prep` links this
// crate and gets not one line of it, exactly as `vidiotic` does with
// `vidiotic-play::web`.
#[cfg(target_arch = "wasm32")]
pub mod web;
