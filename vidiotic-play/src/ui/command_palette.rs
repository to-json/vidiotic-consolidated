//! The floating Command Palette search modal.
//!
//! Activated via default keybindings (`Cmd+P`, `Ctrl+P`, `Cmd+K`, `Ctrl+K`, `Cmd+Shift+P`, `Ctrl+Shift+P`)
//! or via the modal grammar (`m` -> `palette`). Shows a searchable list of commands,
//! filterable by typing, navigable with Arrow Up / Arrow Down and Enter, or mouse click.

use crossbeam_channel::Sender;
use egui::{Align2, Area, Frame, Id, Key, Order, RichText, ScrollArea, Stroke, TextEdit};
use phosphor::theme::{mono, palette, SP_MD, SP_SM};

use crate::commands::{Command, UiMirror};

struct PaletteItem {
    name: &'static str,
    category: &'static str,
    shortcut: &'static str,
    command: Command,
}

fn palette_items(m: &UiMirror) -> Vec<PaletteItem> {
    vec![
        // File & Session
        PaletteItem {
            name: "Save Project",
            category: "File",
            shortcut: "Cmd+S",
            command: Command::SaveProject,
        },
        PaletteItem {
            name: "Save Project As...",
            category: "File",
            shortcut: "",
            command: Command::SaveProjectAs,
        },
        PaletteItem {
            name: "Open Project...",
            category: "File",
            shortcut: "",
            command: Command::OpenProject,
        },
        PaletteItem {
            name: "Open Project Editor (vidiotic-prep)",
            category: "File",
            shortcut: "",
            command: Command::OpenProjectEditor,
        },
        PaletteItem {
            name: "Open Control Mapper (vidiotic-ctl)",
            category: "File",
            shortcut: "",
            command: Command::OpenControlMapper,
        },
        PaletteItem {
            name: "Quit",
            category: "File",
            shortcut: "Cmd+Q",
            command: Command::Quit,
        },
        // Transport & Beat Grid
        PaletteItem {
            name: "Tap Downbeat",
            category: "Transport",
            shortcut: "t",
            command: Command::TapDownbeat,
        },
        PaletteItem {
            name: "Tap Tempo",
            category: "Transport",
            shortcut: "b",
            command: Command::TapTempo,
        },
        PaletteItem {
            name: "Soft Reset Beat Grid",
            category: "Transport",
            shortcut: "r",
            command: Command::SoftReset,
        },
        PaletteItem {
            name: "Hard Reset (Grid & Playlist)",
            category: "Transport",
            shortcut: "Shift+R",
            command: Command::HardReset,
        },
        PaletteItem {
            name: "BPM +1",
            category: "Transport",
            shortcut: "+",
            command: Command::BpmDelta(1.0),
        },
        PaletteItem {
            name: "BPM -1",
            category: "Transport",
            shortcut: "-",
            command: Command::BpmDelta(-1.0),
        },
        PaletteItem {
            name: "Nudge Tempo -0.1%",
            category: "Transport",
            shortcut: "[",
            command: Command::NudgeBpm(-0.001),
        },
        PaletteItem {
            name: "Nudge Tempo +0.1%",
            category: "Transport",
            shortcut: "]",
            command: Command::NudgeBpm(0.001),
        },
        PaletteItem {
            name: "Toggle Preserve Playhead on Cut",
            category: "Transport",
            shortcut: "",
            command: Command::SetPreservePlayhead(!m.preserve_playhead),
        },
        // Banks & Cues
        PaletteItem {
            name: "Cycle Live Bank Next",
            category: "Banks",
            shortcut: ".",
            command: Command::CycleLiveBank(1),
        },
        PaletteItem {
            name: "Cycle Live Bank Prev",
            category: "Banks",
            shortcut: ",",
            command: Command::CycleLiveBank(-1),
        },
        PaletteItem {
            name: "Send Edit Bank to Live",
            category: "Banks",
            shortcut: "",
            command: Command::SetLiveBank(m.edit_bank),
        },
        PaletteItem {
            name: "Add New Cue Bank",
            category: "Banks",
            shortcut: "",
            command: Command::AddBank,
        },
        PaletteItem {
            name: "Clone Edit Cue Bank",
            category: "Banks",
            shortcut: "",
            command: Command::CloneBank,
        },
        PaletteItem {
            name: "Select Next Cue",
            category: "Cues",
            shortcut: "",
            command: Command::SelectCueDelta(1),
        },
        PaletteItem {
            name: "Select Prev Cue",
            category: "Cues",
            shortcut: "",
            command: Command::SelectCueDelta(-1),
        },
        PaletteItem {
            name: "Select First Cue",
            category: "Cues",
            shortcut: "",
            command: Command::SelectCueFirst,
        },
        PaletteItem {
            name: "Select Last Cue",
            category: "Cues",
            shortcut: "",
            command: Command::SelectCueLast,
        },
        PaletteItem {
            name: "Remove Selected Cue",
            category: "Cues",
            shortcut: "",
            command: match m.selected_cue {
                Some(id) => Command::RemoveCue(id),
                None => Command::SelectCueFirst,
            },
        },
        PaletteItem {
            name: "Mark Cue In to Playhead",
            category: "Cues",
            shortcut: "",
            command: match m.selected_cue {
                Some(id) => Command::SetCueInToPlayhead(id),
                None => Command::SelectCueFirst,
            },
        },
        PaletteItem {
            name: "Mark Cue Out to Playhead",
            category: "Cues",
            shortcut: "",
            command: match m.selected_cue {
                Some(id) => Command::SetCueOutToPlayhead(id),
                None => Command::SelectCueFirst,
            },
        },
        // Clips & Pool
        PaletteItem {
            name: "Add Cue for Selected Clip",
            category: "Clips",
            shortcut: "",
            command: match m.selected_clip {
                Some(id) => Command::AddCue(id),
                None => Command::SelectClipFirst,
            },
        },
        PaletteItem {
            name: "Pick / Change Clip Directory...",
            category: "Clips",
            shortcut: "",
            command: Command::PickClipDir,
        },
        PaletteItem {
            name: "Add Clip Directory as Bank...",
            category: "Clips",
            shortcut: "",
            command: Command::PickClipBankDir,
        },
        PaletteItem {
            name: "Select Next Clip",
            category: "Clips",
            shortcut: "",
            command: Command::SelectClipDelta(1),
        },
        PaletteItem {
            name: "Select Prev Clip",
            category: "Clips",
            shortcut: "",
            command: Command::SelectClipDelta(-1),
        },
        PaletteItem {
            name: "Select First Clip",
            category: "Clips",
            shortcut: "",
            command: Command::SelectClipFirst,
        },
        PaletteItem {
            name: "Select Last Clip",
            category: "Clips",
            shortcut: "",
            command: Command::SelectClipLast,
        },
        // Shader & FX
        PaletteItem {
            name: "Capture Live Shader to Pool",
            category: "Shader",
            shortcut: "c",
            command: Command::CaptureShader,
        },
        PaletteItem {
            name: "Pick & Compile ISF Effect...",
            category: "Shader",
            shortcut: "",
            command: Command::PickIsf,
        },
        PaletteItem {
            name: "Pick Main Shader File...",
            category: "Shader",
            shortcut: "",
            command: Command::PickShader,
        },
        // View & Modes
        PaletteItem {
            name: "Toggle Command Palette",
            category: "View",
            shortcut: "Cmd+P",
            command: Command::ToggleCommandPalette,
        },
        PaletteItem {
            name: "Toggle Fullscreen",
            category: "View",
            shortcut: "f",
            command: Command::ToggleFullscreen,
        },
        PaletteItem {
            name: "Toggle Advanced Mode",
            category: "View",
            shortcut: "",
            command: Command::SetAdvancedMode(!m.advanced),
        },
        PaletteItem {
            name: "Toggle Modal Grammar Mode",
            category: "View",
            shortcut: "",
            command: Command::SetGrammarMode(!m.grammar_on),
        },
        // History
        PaletteItem {
            name: "Undo",
            category: "History",
            shortcut: "Cmd+Z",
            command: Command::Undo,
        },
        PaletteItem {
            name: "Redo",
            category: "History",
            shortcut: "Cmd+Shift+Z",
            command: Command::Redo,
        },
    ]
}

pub(super) fn show(ctx: &egui::Context, m: &UiMirror, tx: &Sender<Command>) {
    let p = palette();
    let query_id = Id::new("palette_query");
    let selected_id = Id::new("palette_selected");
    let search_input_id = Id::new("palette_input_box");

    let mut query: String = ctx.data_mut(|d| d.get_temp(query_id).unwrap_or_default());
    let mut selected_index: usize = ctx.data_mut(|d| d.get_temp(selected_id).unwrap_or(0));

    let items = palette_items(m);
    let filtered: Vec<&PaletteItem> = items
        .iter()
        .filter(|item| {
            if query.trim().is_empty() {
                return true;
            }
            let q = query.to_lowercase();
            item.name.to_lowercase().contains(&q)
                || item.category.to_lowercase().contains(&q)
                || item.shortcut.to_lowercase().contains(&q)
        })
        .collect();

    if selected_index >= filtered.len() && !filtered.is_empty() {
        selected_index = filtered.len() - 1;
    }

    // Handle keys: Escape closes, Up/Down navigates, Enter selects
    let mut execute_cmd: Option<Command> = None;

    ctx.input(|i| {
        if i.key_pressed(Key::Escape) {
            let _ = tx.send(Command::ToggleCommandPalette);
        } else if i.key_pressed(Key::ArrowUp) {
            selected_index = selected_index.saturating_sub(1);
        } else if i.key_pressed(Key::ArrowDown) {
            if !filtered.is_empty() {
                selected_index = (selected_index + 1).min(filtered.len() - 1);
            }
        } else if i.key_pressed(Key::Enter) {
            if let Some(item) = filtered.get(selected_index) {
                execute_cmd = Some(item.command.clone());
            }
        }
    });

    if let Some(cmd) = execute_cmd {
        let _ = tx.send(cmd);
        let _ = tx.send(Command::ToggleCommandPalette);
        ctx.data_mut(|d| {
            d.insert_temp(query_id, String::new());
            d.insert_temp(selected_id, 0usize);
        });
        return;
    }

    Area::new(Id::new("command_palette"))
        .order(Order::Foreground)
        .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 80.0))
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.bg_elevated)
                .stroke(Stroke::new(1.0, p.accent))
                .inner_margin(SP_MD)
                .show(ui, |ui| {
                    ui.set_width(540.0);
                    ui.spacing_mut().item_spacing = egui::vec2(SP_MD, SP_SM);

                    // Header / Search Input
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(">").font(mono()).color(p.accent));
                        let response = ui.add(
                            TextEdit::singleline(&mut query)
                                .id(search_input_id)
                                .hint_text("Type a command or search...")
                                .font(mono())
                                .desired_width(f32::INFINITY)
                                .lock_focus(true),
                        );
                        if response.changed() {
                            selected_index = 0;
                        }
                        response.request_focus();
                    });

                    ui.separator();

                    // Items list
                    ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            if filtered.is_empty() {
                                ui.label(
                                    RichText::new("No matching commands")
                                        .font(mono())
                                        .color(p.fg_muted),
                                );
                            } else {
                                for (idx, item) in filtered.iter().enumerate() {
                                    let is_selected = idx == selected_index;
                                    let bg = if is_selected {
                                        p.bg_inset
                                    } else {
                                        p.bg_elevated
                                    };
                                    let fg = if is_selected {
                                        p.accent
                                    } else {
                                        p.fg_primary
                                    };

                                    let resp = ui.horizontal(|ui| {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                        // Category badge
                                        ui.label(
                                            RichText::new(format!("[{}]", item.category))
                                                .font(mono())
                                                .color(p.fg_muted),
                                        );
                                        // Command name
                                        ui.label(RichText::new(item.name).font(mono()).color(fg));
                                        // Shortcut right-aligned
                                        if !item.shortcut.is_empty() {
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        RichText::new(item.shortcut)
                                                            .font(mono())
                                                            .color(p.accent_dim),
                                                    );
                                                },
                                            );
                                        }
                                    });

                                    let rect = resp.response.rect;
                                    if ui
                                        .interact(
                                            rect,
                                            Id::new(("cmd_item", idx)),
                                            egui::Sense::click(),
                                        )
                                        .clicked()
                                    {
                                        let _ = tx.send(item.command.clone());
                                        let _ = tx.send(Command::ToggleCommandPalette);
                                        query.clear();
                                        selected_index = 0;
                                    }
                                }
                            }
                        });

                    ui.separator();
                    ui.label(
                        RichText::new("↑↓ navigate  ·  enter select  ·  esc close")
                            .font(mono())
                            .color(p.fg_muted),
                    );
                });
        });

    ctx.data_mut(|d| {
        d.insert_temp(query_id, query);
        d.insert_temp(selected_id, selected_index);
    });
}
