mod app;
mod panels;
mod undo;

fn main() -> eframe::Result {
    phosphor::bundle::init_logging("Vidiotic", "vidiotic-ctl", "info");
    // Optional: `vidiotic-ctl <map.vmap>` opens that map instead of the
    // default one. `bundle::args` drops Launch Services' `-psn_…` serial.
    let initial = phosphor::bundle::args()
        .into_iter()
        .nth(1)
        .map(std::path::PathBuf::from);
    phosphor::shell::run("vidiotic-ctl", eframe::NativeOptions::default(), |_cc| {
        let mut app = app::CtlApp::new();
        if let Some(path) = initial {
            app.open(path);
        }
        Box::new(app)
    })
}
