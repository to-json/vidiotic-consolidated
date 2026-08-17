//! The modal grammar: pane focus, verb application, modal view.
//!
//! Every verb resolves into a [`Command`] rather than mutating anything
//! directly, which is what puts verbs, clicks, MIDI and IPC on one apply path.
//! It is also what makes an unimplemented verb honest in a shell that lacks the
//! machinery for it: the command comes back out of
//! [`Engine::apply_command`](super::Engine::apply_command) unhandled instead of
//! being silently swallowed.

use super::{Command, Engine};
use crate::commands::GrammarModalView;
use crate::grammar::{self, GrammarState, Pane, Verb};

impl Engine {
    /// Focus a grammar pane, remembering the previous one for the bb bounce.
    pub fn focus_pane(&mut self, pane: Pane) {
        if pane != self.focused_pane {
            self.prev_pane = std::mem::replace(&mut self.focused_pane, pane);
        }
    }

    /// The pending grammar sequence as the which-key overlay renders it, or
    /// `None` when idle.
    #[must_use]
    pub fn grammar_modal_view(&self) -> Option<GrammarModalView> {
        let pane = self.focused_pane.label();
        match self.grammar.state {
            GrammarState::Idle => None,
            GrammarState::AwaitingConjugation { root } => {
                let entry = &grammar::pane_table(self.focused_pane).roots[root.index()];
                let options = entry
                    .conjugations
                    .iter()
                    .enumerate()
                    .filter_map(|(i, c)| c.as_ref().map(|c| (grammar::KEY_TOKENS[i], c.label)))
                    .collect();
                Some(GrammarModalView {
                    trail: format!("{pane}·{}", grammar::KEY_TOKENS[root.index()]),
                    title: entry.label,
                    options,
                })
            }
            GrammarState::Sticky {
                label,
                entries,
                trail_root,
            } => {
                let options = entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| {
                        v.as_ref().map(|v| {
                            (
                                grammar::KEY_TOKENS[i],
                                match v {
                                    Verb::NudgeParam(_, d) if *d > 0 => "step +",
                                    Verb::NudgeParam(_, d) if *d < 0 => "step -",
                                    Verb::BpmDelta(d) if *d > 0.0 => "step +",
                                    Verb::BpmDelta(_) => "step -",
                                    Verb::NudgeBpm(r) if *r > 0.0 => "step +",
                                    Verb::NudgeBpm(_) => "step -",
                                    Verb::SelectCueDelta(d) | Verb::SelectClipDelta(d)
                                        if *d < 0 =>
                                    {
                                        "up"
                                    }
                                    Verb::SelectCueDelta(_) | Verb::SelectClipDelta(_) => "down",
                                    Verb::TapTempo => "tap",
                                    _ => "again",
                                },
                            )
                        })
                    })
                    .collect();
                Some(GrammarModalView {
                    trail: format!("{pane}·{}·{label}", grammar::KEY_TOKENS[trail_root.index()]),
                    title: label,
                    options,
                })
            }
        }
    }

    /// Feed one grammar input; apply any completed verb. Returns whether the
    /// input was consumed — `false` only for cancel-while-idle, which falls
    /// through to the shell's own Escape handling.
    pub fn grammar_step(&mut self, input: grammar::Input) -> bool {
        match self
            .grammar
            .step(grammar::pane_table(self.focused_pane), input)
        {
            grammar::Step::Rejected => false,
            grammar::Step::Verb(v) => {
                self.apply_verb(v);
                true
            }
            grammar::Step::Pending | grammar::Step::Cancelled => true,
        }
    }

    /// Map a completed grammar verb onto engine commands, resolving the
    /// selection and bank context verbs deliberately leave open. Everything is
    /// raised as a command so verbs and direct commands share one apply path.
    pub fn apply_verb(&mut self, verb: Verb) {
        self.last_verb = Some(format!("{verb:?}"));
        match verb {
            Verb::FocusPane(p) => self.focus_pane(p),
            Verb::FocusPrevPane => self.focus_pane(self.prev_pane),
            Verb::SelectCueDelta(d) => self.raise(Command::SelectCueDelta(d)),
            Verb::SelectCueFirst => self.raise(Command::SelectCueFirst),
            Verb::SelectCueLast => self.raise(Command::SelectCueLast),
            Verb::SelectClipDelta(d) => self.raise(Command::SelectClipDelta(d)),
            Verb::SelectClipFirst => self.raise(Command::SelectClipFirst),
            Verb::SelectClipLast => self.raise(Command::SelectClipLast),
            Verb::EditBankDelta(d) => {
                let n = self.banks.len() as i32;
                let i = (self.edit_bank as i32 + d).rem_euclid(n.max(1)) as usize;
                self.raise(Command::SetEditBank(i));
            }
            Verb::ClipBankDelta(d) => {
                let n = self.clip_banks.len() as i32;
                let i = (self.active_clip_bank as i32 + d).rem_euclid(n.max(1)) as usize;
                self.raise(Command::SetActiveClipBank(i));
            }
            Verb::SendEditBankLive => self.raise(Command::SetLiveBank(self.edit_bank)),
            Verb::CycleLiveBank(d) => self.raise(Command::CycleLiveBank(d)),
            Verb::MarkInToPlayhead => {
                if let Some(id) = self.selected_cue {
                    self.raise(Command::SetCueInToPlayhead(id));
                }
            }
            Verb::MarkOutToPlayhead => {
                if let Some(id) = self.selected_cue {
                    self.raise(Command::SetCueOutToPlayhead(id));
                }
            }
            Verb::CyclePreserve => {
                if let Some(id) = self.selected_cue {
                    if let Some(cue) = self.banks[self.edit_bank].cue(id) {
                        let next = match cue.preserve {
                            None => Some(true),
                            Some(true) => Some(false),
                            Some(false) => None,
                        };
                        self.raise(Command::SetCuePreserve(id, next));
                    }
                }
            }
            Verb::AddBank => self.raise(Command::AddBank),
            Verb::CloneBank => self.raise(Command::CloneBank),
            Verb::AddCueAtClip => {
                // Guard against a cursor left on a since-removed clip.
                if let Some(clip) = self
                    .selected_clip
                    .filter(|id| self.clips.iter().any(|c| c.id == *id))
                {
                    self.raise(Command::AddCue(clip));
                }
            }
            Verb::RemoveSelectedCue => {
                if let Some(id) = self.selected_cue {
                    self.raise(Command::RemoveCue(id));
                }
            }
            Verb::NudgeParam(kind, dir) => self.raise(Command::NudgeCueParam(kind, dir)),
            Verb::TapTempo => self.raise(Command::TapTempo),
            Verb::TapDownbeat => self.raise(Command::TapDownbeat),
            Verb::BpmDelta(amount) => self.raise(Command::BpmDelta(amount)),
            Verb::NudgeBpm(ratio) => self.raise(Command::NudgeBpm(ratio)),
            Verb::SoftReset => self.raise(Command::SoftReset),
            Verb::HardReset => self.raise(Command::HardReset),
            Verb::SaveProject => self.raise(Command::SaveProject),
            Verb::OpenProject => self.raise(Command::OpenProject),
            Verb::OpenProjectEditor => self.raise(Command::OpenProjectEditor),
            Verb::ToggleFullscreen => self.raise(Command::ToggleFullscreen),
            Verb::ToggleAdvanced => self.raise(Command::SetAdvancedMode(!self.advanced)),
            Verb::ToggleCommandPalette => self.raise(Command::ToggleCommandPalette),
            Verb::GrammarOff => self.raise(Command::SetGrammarMode(false)),
        }
    }
}
