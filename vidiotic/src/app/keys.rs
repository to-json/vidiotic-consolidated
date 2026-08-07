//! Winit-level key handling: three layers, none of them a special case.
//!
//! Undo/redo chords claim a press first, then the grammar's token keys, then
//! the mapper. There is deliberately no fourth — the player's built-in
//! bindings live in the mapper's fallback layer (see
//! [`crate::control_input::default_map`]), not in a hardcoded match down here
//! where nothing could see or override them.

use super::*;

impl App {
    pub(super) fn handle_key(&mut self, ev: &KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        // Undo/redo: reserved accelerator chords, resolved before the grammar,
        // mapper, and hardcoded defaults. Cmd+Z on mac, Ctrl+Z elsewhere; Shift
        // or `y` for redo. Hardcoded and unrebindable, matching prep/ctl —
        // pending the decision tracked in vidiotic-prep/UNDO_TODO.md.
        if !ev.repeat {
            let m = self.modifiers.state();
            if (m.control_key() || m.super_key()) && !m.alt_key() {
                if let Some(k) = crate::control_input::canon_key(&ev.logical_key) {
                    match k.to_ascii_lowercase().as_str() {
                        "z" if m.shift_key() => {
                            let _ = self.cmd_tx.send(Command::Redo);
                            return;
                        }
                        "z" => {
                            let _ = self.cmd_tx.send(Command::Undo);
                            return;
                        }
                        "y" => {
                            let _ = self.cmd_tx.send(Command::Redo);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
        // With the grammar on, chord-free presses of its token keys belong to
        // it outright — before the mapper, so sequences can't be torn apart by
        // a colliding single-key binding. (Beat/Meta roots cover what the
        // masked defaults did.) Escape only counts while a sequence is
        // pending; idle it falls through to the mapper, where the built-in
        // binding clears a pending BPM entry.
        if self.engine.grammar_on && !ev.repeat {
            let m = self.modifiers.state();
            if !m.control_key() && !m.alt_key() && !m.super_key() {
                if let Some(input) = crate::control_input::canon_key(&ev.logical_key)
                    .as_deref()
                    .and_then(grammar::token_of_key)
                {
                    if self.engine.grammar_step(input) {
                        return;
                    }
                }
            }
        }
        // Everything else is the mapper's: the user's bindings first, then the
        // built-ins underneath them.
        if let Some(key) = crate::control_input::canon_key(&ev.logical_key) {
            let m = self.modifiers.state();
            self.control_input.offer_key(
                &key,
                m.control_key(),
                m.alt_key(),
                m.shift_key(),
                m.super_key(),
                ev.repeat,
                &self.cmd_tx,
            );
        }
    }
}
