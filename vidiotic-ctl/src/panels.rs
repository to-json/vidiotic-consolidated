//! Panel layout for [`crate::app::CtlApp`]: the chrome around the shared
//! binding table in [`vidiotic_ctl::ui`] — toolbar, device list, live monitor.
//!
//! The table itself lives in the lib because `vidiotic-prep` draws it too.
//! This bin edits *any* `.vmap` (it has open/save-as), so it offers the full
//! [`Action::catalog`]; prep's two editors narrow to one app's vocabulary.

use phosphor::icon;
use phosphor::theme::palette;
use phosphor::widgets;
use vidiotic_ctl::ui::TableEvent;
use vidiotic_ctl::{source_key, Action, EventValue};

use crate::app::CtlApp;

/// Lay out every panel for one frame: toolbar, status line, live monitor,
/// device list, and the shared binding table itself.
pub fn draw(app: &mut CtlApp, ui: &mut egui::Ui) {
    toolbar(app, ui);
    phosphor::shell::statusline_panel(ui, mode_word(app), &status_summary(app));
    egui::Panel::bottom("monitor")
        .resizable(true)
        .default_size(140.0)
        .size_range(80.0..=280.0)
        .show(ui, |ui| monitor_panel(app, ui));
    egui::Panel::right("devices")
        .resizable(true)
        .default_size(220.0)
        .size_range(160.0..=360.0)
        .show(ui, |ui| device_panel(app, ui));
    egui::CentralPanel::default_margins().show(ui, |ui| binding_panel(app, ui));
}

fn toolbar(app: &mut CtlApp, ui: &mut egui::Ui) {
    egui::Panel::top("toolbar").show(ui, |ui| {
        let p = palette();
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if widgets::bracket_button(ui, "save", None, 0.0).clicked() {
                app.save();
            }
            if widgets::bracket_button(ui, "revert", None, 0.0).clicked() {
                app.revert();
            }
            if widgets::bracket_button(ui, "open…", None, 0.0).clicked() {
                if let Some(path) =
                    rfd::FileDialog::new().add_filter("vidiotic control map", &["vmap"]).pick_file()
                {
                    app.open(path);
                }
            }
            if widgets::bracket_button(ui, "save as…", None, 0.0).clicked() {
                if let Some(path) =
                    rfd::FileDialog::new().add_filter("vidiotic control map", &["vmap"]).save_file()
                {
                    app.save_as(path);
                }
            }
            if widgets::bracket_button(ui, icon::REFRESH, None, 0.0)
                .on_hover_text("rescan devices")
                .clicked()
            {
                app.hub.rescan();
            }
            ui.add_space(8.0);
            if let Some(path) = &app.path {
                ui.label(egui::RichText::new(path.display().to_string()).color(p.fg_secondary));
            }
            if app.dirty {
                ui.colored_label(p.accent, "*");
            }
        });
        ui.add_space(4.0);
    });
}

fn device_panel(app: &CtlApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        widgets::section_label(ui, "midi");
        let midi_names = app.hub.port_names();
        if midi_names.is_empty() {
            ui.weak("no midi devices");
        }
        for name in midi_names {
            ui.label(name);
        }
        ui.add_space(8.0);
        widgets::section_label(ui, "gamepad");
        let pad_names = app.pads.device_names();
        if pad_names.is_empty() {
            ui.weak("no gamepads");
        }
        for name in pad_names {
            ui.label(name);
        }
    });
}

fn value_label(value: EventValue) -> String {
    match value {
        EventValue::Pressed => "pressed".to_string(),
        EventValue::Released => "released".to_string(),
        EventValue::Continuous(v) => format!("{v:.2}"),
    }
}

fn monitor_panel(app: &CtlApp, ui: &mut egui::Ui) {
    let p = palette();
    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
        for ev in app.monitor.iter().rev().take(12).rev() {
            ui.label(
                egui::RichText::new(format!(
                    "{}  {}",
                    source_key(&ev.source),
                    value_label(ev.value)
                ))
                .monospace()
                .color(p.fg_secondary),
            );
        }
    });
}

/// Vim-style mode word: ERROR on a failed op > LEARN while capturing >
/// NORMAL otherwise.
fn mode_word(app: &CtlApp) -> (&'static str, Option<egui::Color32>) {
    let p = palette();
    if app.status_is_error {
        ("ERROR", Some(p.error))
    } else if app.learn.is_some() {
        ("LEARN", Some(p.accent))
    } else {
        ("NORMAL", None)
    }
}

fn status_summary(app: &CtlApp) -> String {
    let path =
        app.path.as_ref().map_or_else(|| "(unsaved)".to_string(), |p| p.display().to_string());
    let dirty = if app.dirty { " · dirty" } else { "" };
    format!("{path} · {} binding(s){dirty}", app.map.bindings.len())
}

fn binding_panel(app: &mut CtlApp, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let mut dirty = false;
        let event = vidiotic_ctl::ui::binding_table(
            ui,
            &mut app.map,
            app.learn,
            Action::catalog(),
            &mut dirty,
        );
        if dirty {
            app.dirty = true;
        }
        match event {
            Some(TableEvent::Learn(i)) => app.start_learn(i),
            Some(TableEvent::Remove(i)) => app.remove_binding(i),
            Some(TableEvent::Add) => app.add_binding(),
            None => {}
        }
    });
}
