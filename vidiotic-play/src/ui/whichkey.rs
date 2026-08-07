//! The grammar's which-key overlay: while a verb sequence is pending, a
//! floating panel above the statusline lists the open root's conjugations
//! (or a sticky mode's repeats), keyed by their token spellings.

use egui::{Align2, Area, Color32, Frame, Id, Order, RichText, Stroke};
use phosphor::theme::{mono, palette, row, SP_MD, SP_SM};

use crate::commands::GrammarModalView;

/// Options per row before wrapping.
const COLS: usize = 4;

/// Draw the overlay for a pending sequence. Foreground-ordered so it floats
/// over every panel; input still goes through the normal key path (the
/// overlay is display-only).
pub(super) fn show(ctx: &egui::Context, modal: &GrammarModalView) {
    let p = palette();
    Area::new(Id::new("whichkey"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -(row() + SP_MD)))
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.bg_elevated)
                .stroke(Stroke::new(1.0, p.accent_dim))
                .inner_margin(SP_MD)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(SP_MD, SP_SM);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(modal.title).font(mono()).color(p.accent));
                        ui.label(
                            RichText::new(format!("· {}", modal.trail))
                                .font(mono())
                                .color(p.fg_muted),
                        );
                    });
                    for row in modal.options.chunks(COLS) {
                        ui.horizontal(|ui| {
                            for (key, label) in row {
                                option(ui, key, label, p.accent, p.fg_primary);
                            }
                        });
                    }
                    ui.label(RichText::new("esc cancel").font(mono()).color(p.fg_muted));
                });
        });
}

/// One `key label` pair, padded to a fixed column so rows align.
fn option(ui: &mut egui::Ui, key: &str, label: &str, key_color: Color32, color: Color32) {
    ui.label(RichText::new(key).font(mono()).color(key_color));
    ui.label(RichText::new(format!("{label:<14}")).font(mono()).color(color));
}
