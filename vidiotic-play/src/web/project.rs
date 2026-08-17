//! Loading a `.viproj` in a browser.
//!
//! # The filesystem that isn't one
//!
//! `project::resolve_with` asks two questions — does this path exist, and what
//! is under this directory — through an injected [`Fs`]. Its doc comment has
//! said since the trait was written that *"in a browser it is an index of
//! OPFS"*. This is that, one step simpler: an index of what is **already in the
//! pool**, because bytes reach this player through a drop, a file input or OPFS
//! and are interned under a display name long before a project mentions them.
//!
//! So a project's `clips/bun.mov` resolves against a pool entry called
//! `bun.mov`. Only the file name is compared: the directory part of a stored
//! path describes a layout on somebody's disk, and there is no disk here.
//!
//! # Why this is a swap and not a merge
//!
//! Loading a project replaces the session — pool, clip banks, cue banks, tempo.
//! That is what it does natively too, and it is the honest behaviour: a
//! `.viproj` names clip ids and cue banks that only mean anything relative to
//! each other, so merging one into a live session would either renumber
//! everything or collide. The clips the page has *loaded* survive, because they
//! are bytes and the new pool is re-keyed onto them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vidiotic_core::project::{self, Fs};

use crate::clippool::ClipSource;

/// The pool's clip names, standing in for a filesystem.
pub struct PoolFs {
    names: Vec<String>,
}

impl PoolFs {
    #[must_use]
    pub fn new(names: Vec<String>) -> Self {
        Self { names }
    }
}

impl Fs for PoolFs {
    fn exists(&self, p: &Path) -> bool {
        base_name(p).is_some_and(|n| self.names.iter().any(|held| held == n))
    }

    /// Every clip the pool holds, whatever `root` says.
    ///
    /// `walk` exists for relinking, which searches a directory tree for a
    /// missing clip. There is one flat namespace here, so the honest answer to
    /// "what is under this directory" is "everything" — a relink then matches
    /// by name, which is the only thing that could have matched anyway.
    fn walk(&self, _root: &Path) -> Vec<PathBuf> {
        self.names.iter().map(PathBuf::from).collect()
    }
}

/// The file-name part of a stored clip path.
///
/// A `.viproj` written on a desktop carries `clips/00_cut_10-40.mov`, or an
/// absolute path from somebody else's machine. Neither directory means anything
/// here, and comparing them would make every project miss.
#[must_use]
pub fn base_name(p: &Path) -> Option<&str> {
    p.file_name().and_then(|n| n.to_str())
}

/// A clip's display name as the pool holds it, or `None` for a camera clip.
#[must_use]
pub fn pool_name(source: &ClipSource) -> Option<&str> {
    match source {
        ClipSource::File(name) => base_name(Path::new(&**name)),
        ClipSource::Camera { .. } => None,
    }
}

/// Names a project needs that the pool does not hold, in project order.
///
/// Reported rather than skipped: a project that quietly loads with half its
/// clips missing is a set of cues that fire and show nothing, which reads as a
/// broken player rather than a missing file.
#[must_use]
pub fn missing_names(resolved: &project::ResolvedProject) -> Vec<String> {
    resolved
        .missing
        .iter()
        .filter_map(|id| resolved.project.clips.iter().find(|c| &c.id == id))
        .map(|c| {
            base_name(Path::new(&c.path))
                .unwrap_or(c.name.as_str())
                .to_string()
        })
        .collect()
}

/// Re-key `held` (by clip name) onto the ids a freshly assembled pool uses.
///
/// The ids in a `.viproj` are the project's, and `assemble` rebuilds the pool
/// with its own; the bytes are keyed by neither. This is the join.
#[must_use]
pub fn rekey<T: Clone>(
    clips: &[crate::clippool::Clip],
    held: &HashMap<String, T>,
) -> HashMap<crate::chain::ClipId, T> {
    let mut out = HashMap::new();
    for clip in clips {
        if let Some(name) = pool_name(&clip.source) {
            if let Some(loaded) = held.get(name) {
                out.insert(clip.id, loaded.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn fs() -> PoolFs {
        PoolFs::new(vec!["bun.mov".to_string(), "probe.mov".to_string()])
    }

    /// The whole point: a desktop-written path finds a browser pool entry.
    #[test]
    fn a_stored_path_resolves_by_file_name() {
        assert!(fs().exists(Path::new("clips/bun.mov")));
        assert!(fs().exists(Path::new("/Users/someone/gig/clips/bun.mov")));
        assert!(fs().exists(Path::new("bun.mov")));
    }

    #[test]
    fn a_clip_the_pool_does_not_hold_is_missing() {
        assert!(!fs().exists(Path::new("clips/eyes.mov")));
        // Same stem, different container: the pool holds what it holds.
        assert!(!fs().exists(Path::new("bun.mp4")));
    }

    /// `walk` answers with the flat namespace, so a relink search has candidates
    /// rather than an empty directory.
    #[test]
    fn walk_offers_every_loaded_clip() {
        let found = fs().walk(Path::new("anywhere"));
        assert_eq!(found.len(), 2);
        assert!(found.contains(&PathBuf::from("bun.mov")));
    }

    #[test]
    fn a_camera_clip_has_no_pool_name() {
        let cam = ClipSource::Camera {
            uid: "uid".into(),
            name: "FaceTime".into(),
        };
        assert!(pool_name(&cam).is_none());
    }
}
