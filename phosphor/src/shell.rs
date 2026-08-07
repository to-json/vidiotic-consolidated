//! Standardized eframe scaffolding for phosphor-styled apps: window setup,
//! per-frame theme sync, and a shared statusline panel. Behind the `shell`
//! feature so plain egui integrations (like vidiotic's own control window,
//! which drives egui+wgpu directly and isn't an eframe app) can depend on
//! phosphor without pulling in eframe — those call [`crate::theme::apply`]
//! and [`crate::theme::sync`] directly and share only theme + widgets.

use egui::{Color32, Context, Ui};

/// Run an eframe app with the phosphor theme applied at construction.
/// `build` gets the [`eframe::CreationContext`] after the theme (fonts,
/// palette, transparent-window viewport command) is already set up, and
/// returns the boxed [`eframe::App`] — mirroring how `eframe::run_native`
/// callers already build their app inside the creation closure.
///
/// # Errors
/// Propagates [`eframe::run_native`]'s error if graphics setup fails.
pub fn run<'app>(
    app_name: &str,
    native_options: eframe::NativeOptions,
    build: impl 'app + FnOnce(&eframe::CreationContext<'_>) -> Box<dyn 'app + eframe::App>,
) -> eframe::Result {
    eframe::run_native(
        app_name,
        native_options,
        Box::new(move |cc| {
            crate::theme::apply(&cc.egui_ctx);
            Ok(build(cc))
        }),
    )
}

/// Re-derive the palette/style if the theme controls changed since last
/// frame. Call once at the top of [`eframe::App::ui`], before drawing.
pub fn begin_frame(ctx: &Context) {
    crate::theme::sync(ctx);
}

/// Show the shared statusline as a bottom panel: mode word, free-text
/// summary. For apps (like vidiotic-prep) that embed [`crate::widgets::statusline`]
/// inside an existing panel instead, call that directly rather than this.
pub fn statusline_panel(ui: &mut Ui, mode: (&str, Option<Color32>), summary: &str) {
    egui::Panel::bottom("statusline").show(ui, |ui| {
        crate::widgets::statusline(ui, mode, summary);
    });
}
