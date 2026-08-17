//! The camera section's session half, shared by the native and browser shells.
//!
//! A camera row is one part session and one part machine. The session part —
//! which pool clip a device maps to, which clips are active/armed/playing, and
//! which missing device a camera clip names — the engine owns and both shells
//! used to duplicate (`vidiotic::app::mirror::build_camera_rows` and
//! `vidiotic-play::web::build_camera_rows`). The machine part — whether a
//! device is actually on air and what its status text is — differs per shell
//! (an AVFoundation capture service vs. a `MediaStream` tap), so it arrives as
//! a closure rather than living here.
//!
//! [`Engine::add_camera_cue`] and [`Engine::relink_camera`] are the same split
//! in miniature: the engine knows how to intern a clip and repoint a source,
//! the shell knows the enumeration the device name comes from.

use crate::chain::ClipId;
use crate::clippool::ClipSource;
use crate::commands::{CameraEntry, ClipRole};
use crate::engine::Engine;

impl Engine {
    /// Build the camera section of the mirror: one row per enumerated device,
    /// plus a missing-device row per camera clip whose device is not in the
    /// enumeration (the project loaded anyway; the row is what offers the
    /// relink). `devices` is the shell's last enumeration as `(uid, name)`
    /// pairs; `status` answers `(on_air, status text)` for a device uid, which
    /// only the shell's capture service can say.
    pub fn camera_rows(
        &self,
        devices: &[(&str, &str)],
        status: impl Fn(&str) -> (bool, String),
    ) -> Vec<CameraEntry> {
        let armed = self.sequencer.armed();
        let live = &self.banks[self.live_bank];
        let active: std::collections::HashSet<ClipId> =
            live.cues.iter().map(|c| c.clip).collect();
        let playing_clip = self.current.and_then(|c| live.cue(c)).map(|c| c.clip);
        let armed_clip = armed.and_then(|c| live.cue(c)).map(|c| c.clip);
        let role_of = |clip: Option<ClipId>| {
            if clip.is_some() && playing_clip == clip {
                ClipRole::Playing
            } else if clip.is_some() && armed_clip == clip {
                ClipRole::Armed
            } else {
                ClipRole::None
            }
        };

        let mut rows: Vec<CameraEntry> = devices
            .iter()
            .map(|&(uid, name)| {
                let clip_id =
                    self.clips.iter().find(|c| c.camera_uid() == Some(uid)).map(|c| c.id);
                let (on_air, status) = status(uid);
                CameraEntry {
                    uid: uid.into(),
                    name: name.into(),
                    on_air,
                    status: status.into(),
                    missing: false,
                    active: clip_id.is_some_and(|id| active.contains(&id)),
                    role: role_of(clip_id),
                }
            })
            .collect();

        // A camera clip whose device isn't connected still gets a row: its cues
        // render black, and without the row there is nothing to relink it onto
        // a device that is.
        // One row per *device*, not per clip: `intern_clip` dedupes camera clips
        // by uid, but `project::assemble` maps saved specs 1:1, so a project file
        // carrying two camera clips with the same uid would otherwise show the
        // same absent device twice.
        for c in &self.clips {
            let Some(uid) = c.camera_uid() else { continue };
            if devices.iter().any(|&(d_uid, _)| d_uid == uid) {
                continue;
            }
            if rows.iter().any(|e| &*e.uid == uid) {
                continue;
            }
            rows.push(CameraEntry {
                uid: uid.into(),
                name: c.name.clone(),
                on_air: false,
                status: "not connected".into(),
                missing: true,
                active: active.contains(&c.id),
                role: role_of(Some(c.id)),
            });
        }
        rows
    }

    /// Add a cue for a capture device to the edit bank, creating the device's
    /// pool clip on first use. The device name comes from the shell's last
    /// enumeration, defaulting to "camera" for a uid the enumeration lacks.
    pub fn add_camera_cue(&mut self, devices: &[(&str, &str)], uid: &str) {
        let name: std::sync::Arc<str> = devices
            .iter()
            .find(|&&(d_uid, _)| d_uid == uid)
            .map_or("camera", |&(_, name)| name)
            .into();
        let clip =
            self.intern_clip(ClipSource::Camera { uid: uid.into(), name: name.clone() }, name);
        self.add_cue(clip);
    }

    /// Point every clip referencing the missing device `from` at the connected
    /// device `to`, and drop those cues' taps so they re-open against the new
    /// device's service on the next tick rather than holding one for a uid
    /// nothing points at any more. `Err` carries a message for the shell to
    /// surface when `to` is not in its last enumeration.
    pub fn relink_camera(
        &mut self,
        devices: &[(&str, &str)],
        from: &str,
        to: &str,
    ) -> Result<(), String> {
        let Some(&(_, name)) = devices.iter().find(|&&(d_uid, _)| d_uid == to) else {
            return Err(format!("no connected camera with id {to}"));
        };
        let name: std::sync::Arc<str> = name.into();
        for c in &mut self.clips {
            if c.camera_uid() == Some(from) {
                c.source = ClipSource::Camera { uid: to.into(), name: name.clone() };
                c.name = name.clone();
            }
        }
        let stale: Vec<crate::bank::CueId> = self
            .decoders
            .keys()
            .copied()
            .filter(|&id| {
                self.live_cue(id)
                    .is_some_and(|c| self.clip_camera_uid(c.clip).as_deref() == Some(to))
            })
            .collect();
        for id in stale {
            self.decoders.remove(&id);
        }
        Ok(())
    }
}
