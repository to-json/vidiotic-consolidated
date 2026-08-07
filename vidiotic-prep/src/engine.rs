//! The link back to a running `vidiotic` engine.
//!
//! The engine launches prep on a project (`Command::OpenProjectEditor`) and
//! passes its socket down as `$VIDIOTIC_SOCK`. That environment variable is the
//! entire handshake: its presence means an engine is waiting on the project we
//! were opened with, and its value says where to reach that engine. Prep closes
//! the loop by sending `LoadProject` back once the project is re-exported.
//!
//! Standalone prep still discovers the newest engine via the
//! `vidiotic-latest.sock` symlink, but only ever talks to it on an explicit
//! user action — reloading a project is destructive to a live set, so nothing
//! automatic fires at an engine that didn't ask for us.

use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use vidiotic_wire::{WireClient, WireCommand};

/// The convenience symlink the engine points at its newest instance's socket.
const LATEST_SOCK: &str = "vidiotic-latest.sock";

/// A resolved engine socket, plus whether that engine is the one that launched
/// this prep session.
pub struct EngineLink {
    socket: PathBuf,
    launched_us: bool,
}

impl EngineLink {
    /// Resolve an engine to talk to: the launching engine's socket from
    /// `$VIDIOTIC_SOCK`, else the newest instance's `vidiotic-latest.sock` if
    /// one is listening. `None` when no engine is reachable.
    ///
    /// Only the path is checked here — an engine that dies later surfaces as a
    /// connect error on the next send, which is the same failure either way.
    pub fn discover() -> Option<Self> {
        if let Some(sock) = std::env::var_os(vidiotic_wire::SOCK_ENV) {
            let socket = PathBuf::from(sock);
            if socket.exists() {
                return Some(Self { socket, launched_us: true });
            }
            log::warn!("engine: ${} points at a missing socket", vidiotic_wire::SOCK_ENV);
        }
        let latest = std::env::temp_dir().join(LATEST_SOCK);
        if latest.exists() {
            Some(Self { socket: latest, launched_us: false })
        } else {
            None
        }
    }

    /// Whether this engine spawned us, and so is holding the project we were
    /// opened with — the condition for handing an export back unprompted.
    #[must_use]
    pub const fn launched_us(&self) -> bool {
        self.launched_us
    }

    /// The socket this link addresses.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Ask the engine to load `project`, off the UI thread. The returned
    /// receiver yields exactly one outcome; a stalled engine stalls the worker
    /// thread, never the editor.
    pub fn reload(&self, project: &Path) -> Receiver<Result<(), String>> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let socket = self.socket.clone();
        let project = project.to_path_buf();
        let spawned = std::thread::Builder::new().name("engine-reload".into()).spawn(move || {
            let outcome = WireClient::connect(&socket)
                .and_then(|mut c| c.command(WireCommand::LoadProject(project.display().to_string())))
                .map_err(|e| e.to_string());
            let _ = tx.send(outcome);
        });
        if let Err(e) = spawned {
            log::error!("engine: could not spawn reload thread: {e}");
        }
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three: `discover` reads process-wide environment, and
    /// separate cases would race under the default parallel test harness.
    #[test]
    fn discover_prefers_the_launching_engine() {
        let dir = std::env::temp_dir().join(format!("prep-engine-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join("vidiotic-99.sock");
        let latest = dir.join(LATEST_SOCK);
        std::fs::write(&launcher, b"").unwrap();
        std::env::set_var("TMPDIR", &dir);

        // No engine at all.
        std::env::remove_var(vidiotic_wire::SOCK_ENV);
        assert!(EngineLink::discover().is_none());

        // A listening instance, but it didn't launch us: reachable, not trusted
        // to be handed an export unprompted.
        std::fs::write(&latest, b"").unwrap();
        let link = EngineLink::discover().unwrap();
        assert_eq!(link.socket(), latest);
        assert!(!link.launched_us());

        // Launched by an engine: its socket wins over the symlink.
        std::env::set_var(vidiotic_wire::SOCK_ENV, &launcher);
        let link = EngineLink::discover().unwrap();
        assert_eq!(link.socket(), launcher);
        assert!(link.launched_us());

        // A stale variable falls back rather than failing outright.
        std::env::set_var(vidiotic_wire::SOCK_ENV, dir.join("gone.sock"));
        let link = EngineLink::discover().unwrap();
        assert_eq!(link.socket(), latest);
        assert!(!link.launched_us());

        std::env::remove_var(vidiotic_wire::SOCK_ENV);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
