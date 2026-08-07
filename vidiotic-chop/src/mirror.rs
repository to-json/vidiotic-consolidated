//! What the panels can see that isn't the marking session.
//!
//! # Why this is two fields
//!
//! `vidiotic`'s `UiMirror` is a large struct rebuilt every tick, because its
//! panels must not touch the engine and the engine is full of things a browser
//! has never heard of — capture services, audio devices, a wgpu renderer. The
//! mirror is the flattening that hides all that.
//!
//! Prep needs almost none of it, and the reason is the split that came first:
//! [`Editor`](crate::editor::Editor) *is already* the portable half. There is
//! nothing to hide from a panel that reads it — no decoder, no socket, no
//! dialog — so panels read it directly and post into its queue, and this type
//! carries only what genuinely lives on the far side of the line:
//!
//! - a decoded frame, which arrived through ffmpeg and is now an egui texture
//! - whether a bake thread is running
//!
//! Everything else a panel used to reach through `PrepApp` for is either in the
//! editor now (the open video's shape, the dialog flags) or is asked for by
//! posting a command (`Pick*`, `StartExport`). If this struct grows, it should
//! be because something new is genuinely native — not because a panel wanted a
//! shortcut.

/// The shell's per-frame overlay: what a *machine* knows, as opposed to what a
/// marking session knows.
#[derive(Clone, Default)]
pub struct PrepMirror {
    /// The current frame, decoded and uploaded. `None` before the first decode
    /// or with no video open.
    pub preview: Option<egui::TextureHandle>,
    /// Whether a bake is in flight. Panels use it for the mode word and to
    /// swap the export dialog's buttons for a progress readout.
    pub exporting: bool,
}
