//! The control-surface panels: one implementation, drawn by both shells.
//!
//! Layout is split by panel: [`transport`] (top), [`status`] (bottom),
//! [`editor`] (right, the selected cue's fields), and [`library`] (center, the
//! clip pool and cue banks). The palette, spacing scale, and shared
//! custom-painted controls come from the `phosphor` crate
//! ([`phosphor::theme`] / [`phosphor::widgets`]).
//!
//! # Why these live here
//!
//! They were `vidiotic::ui` and they moved, which is a smaller change than it
//! sounds: every panel was already `fn show(&mut Ui, &UiMirror, &Sender<Command>)`
//! — no `App`, no window, no filesystem — so the only thing keeping them native
//! was the crate they were sitting in, and that crate links ffmpeg, cpal and
//! unix sockets. `vidiotic` re-exports this module, so `crate::ui::…` still
//! resolves there and not one call site changed (web-port.md §8 step 4g).
//!
//! What did *not* come with them is the egui *stack* — `EguiCtl`, its
//! `egui_winit` input translation and its wgpu paint pass stay in `vidiotic`,
//! and the browser has `web::input` as its own equivalent. Panels are portable;
//! plumbing a toolkit to a window is per-shell. Same seam as
//! [`engine::source::Opener`](crate::engine::source::Opener): the polymorphism
//! sits where the platforms actually differ, and nowhere else.
//!
//! Two things a shell still owns, because the engine cannot know them: the
//! thumbnail cache passed to `library::show`, and answering the `Pick*`
//! commands the panels emit instead of opening a file dialog themselves.

pub mod command_palette;
pub mod editor;
pub mod library;
pub mod status;
pub mod transport;
pub mod whichkey;

use std::collections::HashMap;

use crossbeam_channel::Sender;
use phosphor::widgets;

use crate::commands::{ClipId, ClipRole, Command, UiMirror};

/// "Loop every" cadence choices shared by the transport (global) and the cue
/// editor (per-cue): (label, ticks) at 32 ticks/beat (`LOOP_TICKS_PER_BEAT`).
/// A beat is a quarter note (32), so an eighth note is 16 and a 4/4 bar is
/// 128. Whole numbers label bars; fractions label sub-bar note values.
pub(crate) const LOOP_CADENCE: [(&str, u32); 8] = [
    ("1/8", 16),
    ("1/4", 32),
    ("1/2", 64),
    ("1", 128),
    ("2", 256),
    ("4", 512),
    ("8", 1024),
    ("16", 2048),
];

/// Draw the whole control surface into `ui`.
///
/// The shell supplies the mirror, a command sink, and its thumbnail cache; what
/// comes back is entirely `Command`s, which is what makes this drawable from a
/// winit window or a canvas without either being named here.
pub fn control_ui(
    ui: &mut egui::Ui,
    m: &UiMirror,
    tx: &Sender<Command>,
    thumbs: &HashMap<ClipId, egui::TextureHandle>,
) {
    transport::show(ui, m, tx);
    status::show(ui, m, tx);
    editor::show(ui, m, tx);
    library::show(ui, m, tx, thumbs);
    if let Some(modal) = &m.grammar_modal {
        whichkey::show(ui.ctx(), modal);
    }
    if m.command_palette_open {
        command_palette::show(ui.ctx(), m, tx);
    }
}

/// A live-playback role as the tile marker vocabulary phosphor paints.
pub(crate) fn tile_role(role: ClipRole) -> widgets::TileRole {
    match role {
        ClipRole::Playing => widgets::TileRole::Playing,
        ClipRole::Armed => widgets::TileRole::Armed,
        ClipRole::None => widgets::TileRole::None,
    }
}

/// Format a seconds count as `m:ss.cc`.
pub(crate) fn fmt_time(secs: f64) -> String {
    let secs = secs.max(0.0);
    let mins = (secs / 60.0).floor() as u64;
    let rem = secs - mins as f64 * 60.0;
    format!("{mins}:{rem:05.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[test]
    fn fmt_time_pads_and_floors() {
        assert_eq!(fmt_time(0.0), "0:00.00");
        assert_eq!(fmt_time(9.5), "0:09.50");
        assert_eq!(fmt_time(61.25), "1:01.25");
        // Negative playheads are a real transient while seeking; they read as 0
        // rather than as "-1:59.00".
        assert_eq!(fmt_time(-3.0), "0:00.00");
    }

    // The panels emit into a crossbeam channel, and the browser drains it on the
    // one thread it has. crossbeam-utils is the part that would have wanted
    // threads, so this asserts the round trip actually runs in V8 rather than
    // just compiling for it.
    #[test]
    fn commands_round_trip_through_the_sink() {
        let (tx, rx) = crossbeam_channel::unbounded::<Command>();
        tile_role(ClipRole::Playing);
        let _ = tx.send(Command::PickClipDir);
        let _ = tx.send(Command::TapTempo);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got.len(), 2);
        assert!(matches!(got[0], Command::PickClipDir));
        assert!(matches!(got[1], Command::TapTempo));
    }
}
