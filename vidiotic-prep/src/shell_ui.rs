//! The panels that stayed native.
//!
//! [`vidiotic_chop::ui`] is everything a marking session can draw with an
//! [`Editor`](vidiotic_chop::editor::Editor) and a [`PrepMirror`](vidiotic_chop::mirror::PrepMirror);
//! this is the remainder, and the remainder is small: two binding tables and
//! the export dialog. They are here rather than behind a `cfg` in `ui.rs`
//! because the split is not conditional compilation, it is ownership — these
//! read [`Controls`](crate::control_input::Controls) and `PrepApp`'s export
//! fields, neither of which a browser build would have at all.
//!
//! Both are deferred rather than blocked. `vidiotic-ctl`'s binding tables want
//! WebMIDI and a gamepad shim; the export dialog's destination is a folder path
//! and its progress comes off a bake thread, and both are replaced wholesale by
//! §3's browser backend. Neither is worth a portable shape invented before the
//! thing it would abstract over exists.

use phosphor::theme::palette;
use phosphor::widgets;
use vidiotic_ctl::ui as ctl_ui;
use vidiotic_ctl::{Action, ControlSource};

use crate::app::PrepApp;
use crate::control_input::{Controls, LearnTarget};
use vidiotic_chop::commands::Command;

/// The inspector's two binding tables, drawn into the portable panel's scroll
/// area through the hook [`vidiotic_chop::ui::draw`] takes for exactly this.
pub fn control_sections(c: &mut Controls, ui: &mut egui::Ui) {
    project_map_section(c, ui);
    ui.add_space(8.0);
    prep_keys_section(c, ui);
}

/// The project's control-mapping layer, plus the user's global map beneath
/// it as a read-only reference (shadowed entries — ones a project binding
/// already covers, and so never fire — dim).
/// This table edits the map embedded in the `.viproj`, which `vidiotic`
/// resolves — so it offers [`Action::player_catalog`], not the full catalog:
/// a prep verb bound here would serialize into the project and then resolve
/// to nothing, since vidiotic's `to_command` rejects the other app's half.
/// Prep's own keys are edited by [`prep_keys_section`], against `prep.vmap`.
fn project_map_section(c: &mut Controls, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("controls (this project → vidiotic)")
        .default_open(false)
        .show(ui, |ui| {
            let mut changed = false;
            let event = ctl_ui::binding_table(
                ui,
                &mut c.project,
                c.learn.and_then(LearnTarget::player_row),
                Action::player_catalog(),
                &mut changed,
            );
            match event {
                Some(ctl_ui::TableEvent::Learn(i)) => c.start_learn(LearnTarget::PlayerMap(i)),
                Some(ctl_ui::TableEvent::Remove(i)) => c.remove_project_binding(i),
                Some(ctl_ui::TableEvent::Add) => c.add_project_binding(),
                None => {}
            }

            ui.add_space(8.0);
            widgets::section_label(ui, "global (read-only)");
            if c.global.bindings.is_empty() {
                ui.weak("no global bindings");
            }
            let project_sources: Vec<ControlSource> = c
                .project
                .bindings
                .iter()
                .map(|b| b.source.clone())
                .collect();
            if let Some(source) = ctl_ui::readonly_map(ui, &c.global, &project_sources) {
                c.mask_global_binding(source);
            }
        });
}

/// Prep's *own* keys, persisted to the global `prep.vmap` — a user preference,
/// not a project property, so it's deliberately separate from the table above
/// and never travels with a `.viproj`. The built-in defaults show read-only
/// beneath, so they're discoverable and one click from being masked.
fn prep_keys_section(c: &mut Controls, ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("editor keys (this app)")
        .default_open(false)
        .show(ui, |ui| {
            let mut changed = false;
            let event = ctl_ui::binding_table(
                ui,
                &mut c.mapper.over,
                c.learn.and_then(LearnTarget::prep_row),
                Action::prep_catalog(),
                &mut changed,
            );
            if changed {
                c.mark_dirty();
            }
            match event {
                Some(ctl_ui::TableEvent::Learn(i)) => c.start_learn(LearnTarget::PrepMap(i)),
                Some(ctl_ui::TableEvent::Remove(i)) => c.remove_prep_binding(i),
                Some(ctl_ui::TableEvent::Add) => c.add_prep_binding(),
                None => {}
            }

            if !c.mapper.over.bindings.is_empty()
                && widgets::bracket_button(ui, "reset to defaults", None, 0.0)
                    .on_hover_text("remove every override above")
                    .clicked()
            {
                c.reset_prep_map();
            }

            ui.add_space(8.0);
            widgets::section_label(ui, "built-in defaults (read-only)");
            let overridden: Vec<ControlSource> = c
                .mapper
                .over
                .bindings
                .iter()
                .map(|b| b.source.clone())
                .collect();
            let defaults = crate::control_input::default_map();
            if let Some(source) = ctl_ui::readonly_map(ui, &defaults, &overridden) {
                c.mask_prep_default(source);
            }
        });
}

/// Destination, name, bake options, and live progress. A floating window, so
/// unlike the binding tables it needs no hook — the shell draws it after
/// [`vidiotic_chop::ui::draw`] returns.
pub fn export_dialog(app: &mut PrepApp, ctx: &egui::Context) {
    let mut open = app.editor.show_export_dialog;
    let exporting = app.exporting();
    egui::Window::new("Export project")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("destination");
                let text = app
                    .export_dest
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(choose a folder)".to_string());
                ui.label(egui::RichText::new(text).color(palette().fg_secondary));
                if widgets::bracket_button(ui, "choose…", None, 0.0).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        app.export_dest = Some(dir);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("project name");
                ui.text_edit_singleline(&mut app.export_name);
            });
            widgets::glyph_checkbox(
                ui,
                &mut app.export_starter_cue_bank,
                "starter cue bank (\"A\", one full-length cue per clip)",
            );
            widgets::glyph_checkbox(
                ui,
                &mut app.export_high_quality,
                "high-quality BC1 (ClusterFit, ~6x slower bake)",
            )
            .on_hover_text(
                "off = RangeFit: slightly softer gradients, much faster; fine for iterating",
            );
            ui.label(format!("{} span(s) to bake", app.editor.spans.spans.len()));

            if exporting {
                if let Some(p) = &app.export_progress {
                    let span_frac = if p.cur_total > 0 {
                        p.cur_done as f32 / p.cur_total as f32
                    } else {
                        0.0
                    };
                    let frac = if p.total > 0 {
                        (p.done as f32 + span_frac) / p.total as f32
                    } else {
                        0.0
                    };
                    ui.horizontal(|ui| {
                        widgets::glyph_level(ui, frac, 24);
                        ui.label(format!(
                            "span {}/{} {}",
                            (p.done + 1).min(p.total),
                            p.total,
                            p.current
                        ));
                    });
                    ui.horizontal(|ui| {
                        widgets::glyph_level(ui, span_frac, 12);
                        let mut detail = format!(
                            "{}/{} frames · decode @ {:.2}s · {:.1} enc f/s",
                            p.cur_done, p.cur_total, p.src_sec, p.enc_fps
                        );
                        if p.enc_fps > 0.0 && p.cur_total > p.cur_done {
                            detail.push_str(&format!(
                                " · ~{:.0}s left in span",
                                (p.cur_total - p.cur_done) as f64 / p.enc_fps
                            ));
                        }
                        ui.label(egui::RichText::new(detail).color(palette().fg_muted));
                    });
                }
            } else {
                ui.horizontal(|ui| {
                    let ready = app.export_dest.is_some() && !app.export_name.trim().is_empty();
                    ui.add_enabled_ui(ready, |ui| {
                        if widgets::bracket_button(ui, "export", None, 0.0).clicked() {
                            app.editor.post(Command::StartExport);
                        }
                    });
                    if widgets::bracket_button(ui, "close", None, 0.0).clicked() {
                        app.editor.show_export_dialog = false;
                    }
                });
            }

            // Cloned out so the buttons below can take `app` mutably.
            let exported = app.export_result.clone();
            match &exported {
                Some(Ok(path)) => {
                    ui.horizontal(|ui| {
                        ui.colored_label(palette().phosphor, format!("wrote {}", path.display()));
                        if widgets::bracket_button(ui, "reveal", None, 0.0)
                            .on_hover_text("show in Finder")
                            .clicked()
                        {
                            let _ = std::process::Command::new("open")
                                .arg("-R")
                                .arg(path)
                                .spawn();
                        }
                        let engine = app
                            .engine
                            .as_ref()
                            .map(|e| e.socket().display().to_string());
                        if let Some(socket) = engine {
                            ui.add_enabled_ui(!app.sending_to_engine(), |ui| {
                                if widgets::bracket_button(ui, "send to vidiotic", None, 0.0)
                                    .on_hover_text(format!(
                                        "load this project in the running vidiotic ({socket}) — \
                                         replaces what it has live"
                                    ))
                                    .clicked()
                                {
                                    app.send_to_engine(path.clone());
                                }
                            });
                        }
                    });
                }
                Some(Err(e)) => {
                    ui.colored_label(palette().error, e);
                }
                None => {}
            }
        });
    // `open` is the window's own close button; the "close" button above writes
    // the flag directly, so only fold `open` back in when it actually went down.
    if !open {
        app.editor.show_export_dialog = false;
    }
}
