//! `vidiotic-prep`: clip-authoring companion to the `vidiotic` VJ app. Loads a
//! source video, lets the user mark and trim spans, tags them with metadata,
//! and exports each span as a transcoded HAP clip plus a `.viproj` project.

mod app;
mod control_input;
mod engine;
mod export;
mod preview;
mod session;
mod shell_ui;

fn main() -> eframe::Result {
    phosphor::bundle::init_logging("Vidiotic", "vidiotic-prep", "info");
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    let _ = ffmpeg_next::init();
    // Optional: `vidiotic-prep <video|project.viproj>` opens it immediately.
    // Read through `bundle::args`, which drops the `-psn_…` serial Launch
    // Services prepends — otherwise a Finder launch tries to open it as a file.
    let initial = phosphor::bundle::args()
        .into_iter()
        .nth(1)
        .map(std::path::PathBuf::from);
    phosphor::shell::run("vidiotic-prep", eframe::NativeOptions::default(), |_cc| {
        let mut app = app::PrepApp::default();
        if let Some(path) = initial {
            app.note_launch_project(path.clone());
            app.request_open(path);
        }
        Box::new(app)
    })
}
