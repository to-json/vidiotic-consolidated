//! Shader and ISF pool loading, capture, and removal.

use super::*;

impl App {
    pub(super) fn load_shader(&mut self) {
        let (Some(g), Some(r)) = (self.graphics.as_ref(), self.renderer.as_mut()) else {
            return;
        };
        match std::fs::read_to_string(&self.shader_path) {
            Ok(src) => {
                r.set_shader(&g.device, &src, lang_of(&self.shader_path));
                match r.shader_error() {
                    Some(e) => log::warn!("shader error:\n{e}"),
                    None => log::info!("shader loaded: {}", self.shader_path.display()),
                }
            }
            Err(e) => log::warn!("cannot read shader {}: {e}", self.shader_path.display()),
        }
    }

    /// Pin the current live shader's last-good compile into the renderer's pool,
    /// named after the shader file plus a running count.
    pub(super) fn capture_shader(&mut self) {
        let Some(g) = self.graphics.as_ref() else {
            return;
        };
        let stem = self
            .shader_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("shader")
            .to_string();
        self.shader_pin_count += 1;
        let name = format!("{stem} #{}", self.shader_pin_count);
        if let Some(r) = self.renderer.as_mut() {
            if let Some(id) = r.capture_current(&g.device, name) {
                log::info!("pinned shader {id}");
            } else {
                self.shader_pin_count -= 1;
                log::warn!("no compiled shader to pin");
            }
        }
    }

    /// Drop a pinned or ISF pool shader and clear any cue references to it
    /// (they fall back to the live shader). No-op for builtins.
    pub(super) fn remove_shader(&mut self, id: crate::commands::ShaderId) {
        let Some(r) = self.renderer.as_mut() else {
            return;
        };
        let Some(name) = r.remove_pool_shader(id) else {
            return;
        };
        for bank in &mut self.engine.banks {
            for cue in &mut bank.cues {
                cue.chain.retain(|slot| {
                    slot.shader != crate::commands::SlotRef::Pinned(id)
                        && !matches!(&slot.shader, crate::commands::SlotRef::Isf(path) if path.as_ref() == name.as_ref())
                });
            }
        }
    }

    /// Compile an ISF `.fs` into the shader pool and append it to the selected
    /// cue's effect chain. No-op (logged) if no cue is selected or the file can't
    /// be read/compiled.
    pub(super) fn load_isf(&mut self, path: PathBuf) {
        let Some(cue) = self.engine.selected_cue else {
            log::warn!("Load ISF: no cue selected");
            return;
        };
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("Load ISF: read {}: {e}", path.display());
                return;
            }
        };
        let name: Arc<str> = path.to_string_lossy().into();
        let result = if let (Some(g), Some(r)) = (self.graphics.as_ref(), self.renderer.as_mut()) {
            Some(r.load_isf(
                &g.device,
                &g.queue,
                name.clone(),
                &src,
                &crate::video::decoder::decode_still,
            ))
        } else {
            None
        };
        match result {
            Some(Ok(_)) => {
                log::info!("loaded ISF {}", path.display());
                self.engine
                    .edit_cue(cue, |c| c.chain.push(ChainSlot::new(SlotRef::Isf(name))));
            }
            Some(Err(e)) => log::error!("Load ISF {}: {e}", path.display()),
            None => {}
        }
    }

    /// Compile every ISF shader referenced by a cue chain into the pool, so
    /// project-loaded `SlotRef::Isf` slots resolve. Called once the renderer
    /// exists; missing/broken files are logged and the slot renders as a no-op.
    pub(super) fn load_referenced_isf(&mut self) {
        let mut paths: Vec<Arc<str>> = Vec::new();
        for bank in &self.engine.banks {
            for cue in &bank.cues {
                for slot in &cue.chain {
                    if let SlotRef::Isf(p) = &slot.shader {
                        if !paths.iter().any(|q| q == p) {
                            paths.push(p.clone());
                        }
                    }
                }
            }
        }
        for p in paths {
            let src = match std::fs::read_to_string(p.as_ref()) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("ISF {p}: {e}");
                    continue;
                }
            };
            if let (Some(g), Some(r)) = (self.graphics.as_ref(), self.renderer.as_mut()) {
                if let Err(e) = r.load_isf(
                    &g.device,
                    &g.queue,
                    p.clone(),
                    &src,
                    &crate::video::decoder::decode_still,
                ) {
                    log::error!("ISF {p}: {e}");
                }
            }
        }
    }
}
