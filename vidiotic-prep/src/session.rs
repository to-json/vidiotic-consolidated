//! Where prep's `.vprep` sidecar lives, and the `.viproj` re-open path.
//!
//! The sidecar's *format* is [`vidiotic_chop::session`], shared with the
//! browser shell — a session is a session, and writing the shape twice is how
//! two halves of one tool stop being able to hand work to each other. What is
//! here is the filesystem around it: a path beside the video, and a read.

use std::path::{Path, PathBuf};

use vidiotic_core::project;

use vidiotic_chop::editor::ReopenedProject;
pub use vidiotic_chop::session::SessionFile;

use crate::app::PrepApp;

/// Sidecar lives next to its source with `.vprep` appended: `bun.mov.vprep`.
#[must_use]
pub fn sidecar_path(source: &Path) -> PathBuf {
    let mut os = source.as_os_str().to_owned();
    os.push(".vprep");
    PathBuf::from(os)
}

/// Capture the app's session for `source`, ready to write.
#[must_use]
pub fn capture(app: &PrepApp, source: &Path) -> SessionFile {
    SessionFile::capture(&app.editor, &app.ctl.project, source)
}

/// Merge a loaded sidecar into the app. See
/// [`SessionFile::merge_into`](vidiotic_chop::session::SessionFile::merge_into)
/// for what `adopt_globals` gates.
pub fn merge_into(file: SessionFile, app: &mut PrepApp, source: &Path, adopt_globals: bool) {
    file.merge_into(&mut app.editor, &mut app.ctl.project, source, adopt_globals);
}

/// Read and parse a `.vprep` sidecar.
///
/// # Errors
/// Propagates read failures and RON parse errors.
pub fn load_sidecar(path: &Path) -> anyhow::Result<SessionFile> {
    let text = std::fs::read_to_string(path)?;
    vidiotic_chop::session::parse(&text, &path.display().to_string())
}

/// Load a `.viproj` for retrimming, reconstructing spans from each clip's
/// [`vidiotic_core::project::SpanProvenance`].
///
/// The reconstruction itself is `ReopenedProject::from_project`, in
/// `vidiotic-chop` — it is arithmetic over parsed data and both shells want the
/// same answer. What is native here is the two lines around it: reading the
/// file, and noting the folder to re-export into.
///
/// # Errors
/// Fails if the file doesn't parse, any clip lacks provenance, or clips were
/// cut from more than one source video.
pub fn reopen_project(path: &Path) -> anyhow::Result<ReopenedProject> {
    let proj = project::load(path)?;
    let name = path.file_stem().map_or_else(
        || "project".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let mut re = ReopenedProject::from_project(&proj, &name)?;
    re.project_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok(re)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sidecar's `controls` is the *player's* map, bound for the .viproj.
    /// Prep's own editor keys live in the global prep.vmap and must never be
    /// captured here — they'd travel with the video to another machine.
    ///
    /// The only sidecar test left in this crate: everything else about the
    /// format is `vidiotic_chop::session`'s, and this is the one assertion that
    /// is about prep having *two* control maps, which the browser does not.
    #[test]
    fn prep_key_bindings_never_enter_the_sidecar() {
        let mut app = PrepApp::default();
        app.ctl.add_prep_binding();
        let captured = capture(&app, &PathBuf::from("/tmp/a.mov"));
        assert!(
            captured.controls.bindings.is_empty(),
            "prep.vmap state leaked into the .vprep sidecar"
        );
    }

    #[test]
    fn a_sidecar_path_sits_beside_its_video() {
        assert_eq!(
            sidecar_path(Path::new("/tmp/bun.mov")),
            PathBuf::from("/tmp/bun.mov.vprep")
        );
    }
}
