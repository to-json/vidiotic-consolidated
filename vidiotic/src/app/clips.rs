//! Clip-pool directories and bank selection.
//!
//! Scanning a directory is the shell's half; what happens to the pool
//! afterwards is the engine's, which is why both functions here end in a call
//! into it rather than reaching for `clips`/`banks` themselves.

use super::*;

impl App {
    /// Replace the entire pool with a single clip bank scanned from `dir`. Cues
    /// referenced the old pool, so they are cleared. (The `＋` in the clip-bank
    /// bar appends instead — see [`Self::add_clip_dir_as_bank`].)
    pub(super) fn set_clip_dir(&mut self, dir: PathBuf) {
        let clips = clippool::scan(&dir);
        log::info!("clip pool: {} clips in {}", clips.len(), dir.display());
        self.thumb_rx = Some(clippool::spawn_thumbnailer(clips.clone()));
        let clip_ids = clips.iter().map(|c| c.id).collect();
        let name = dir_bank_name(&dir);
        self.engine.replace_pool(
            clips,
            vec![ClipBank {
                name,
                dir: Some(dir),
                clip_ids,
            }],
            Vec::new(),
        );
        if let Some(egui) = self.egui.as_mut() {
            egui.clear_thumbnails();
        }
        self.bump_epoch();
    }

    /// Append `dir` as a new clip bank, extending the flat pool with fresh global
    /// ids and thumbnailing only the added clips. Existing clips, cues, and
    /// thumbnails are untouched; the new bank becomes active.
    pub(super) fn add_clip_dir_as_bank(&mut self, dir: PathBuf) {
        let new = clippool::scan_from(&dir, self.engine.next_clip_id);
        if new.is_empty() {
            log::warn!("no clips found in {}", dir.display());
            return;
        }
        log::info!("clip bank: +{} clips from {}", new.len(), dir.display());
        let clip_ids: Vec<ClipId> = new.iter().map(|c| c.id).collect();
        self.engine.next_clip_id = clip_ids
            .iter()
            .max()
            .map_or(self.engine.next_clip_id, |m| m + 1);
        // A single thumb_rx is polled each tick; the new receiver carries only the
        // added clips, and already-cached thumbnails are kept (not cleared).
        self.thumb_rx = Some(clippool::spawn_thumbnailer(new.clone()));
        self.engine.clips.extend(new);
        let name = dir_bank_name(&dir);
        self.engine.push_clip_bank(name, Some(dir), clip_ids);
    }
}
