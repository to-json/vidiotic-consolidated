//! The floating Command Palette search modal.
//!
//! Opened by [`Command::ToggleCommandPalette`] — from the default keybindings
//! (`Cmd`/`Ctrl` + `P`/`K`, with or without `Shift`) or from the modal grammar
//! (`;` then `palette`). Type to filter, Arrow Up / Arrow Down and Enter to
//! choose, or click.
//!
//! Every row is a [`Command`] and nothing else: the palette is a second way to
//! reach the same verbs the keys and the panels send, so it cannot do anything
//! they cannot and never needs a code path of its own.

use crossbeam_channel::Sender;
use egui::{Align2, Area, Frame, Id, Key, Order, RichText, ScrollArea, Stroke, TextEdit};
use phosphor::theme::{mono, palette, SP_MD, SP_SM};

use crate::commands::{Command, UiMirror};

/// The name of the accelerator modifier, for the shortcut column.
///
/// The chords themselves take either modifier (`(ctrl || cmd) && !alt`), so this
/// is only about which one to *print* — and printing "Cmd" to somebody on Linux
/// is a shortcut that appears not to work.
#[cfg(not(target_arch = "wasm32"))]
fn accel() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// As above. A tab has no `target_os` — wasm32's is `unknown` — so the browser
/// is the only thing that knows, and `navigator.platform` is the one answer
/// every engine still gives. Deprecated but universal; the alternative
/// (`userAgentData`) is Chromium-only and async.
#[cfg(target_arch = "wasm32")]
fn accel() -> &'static str {
    let apple = web_sys::window()
        .and_then(|w| w.navigator().platform().ok())
        .is_some_and(|p| p.starts_with("Mac") || p.starts_with("iP"));
    if apple {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// One row: what it is called, how it is grouped, the keys that also do it, and
/// the command it sends.
struct PaletteItem {
    name: &'static str,
    category: &'static str,
    /// The keys that reach this without the palette, for the right-hand column.
    /// Empty when there are none.
    shortcut: String,
    /// `None` when the verb needs a selection there isn't one of — the row is
    /// listed, greyed, and inert.
    ///
    /// It used to fall back to `SelectCueFirst`/`SelectClipFirst`, so choosing
    /// "Remove Selected Cue" with nothing selected quietly *selected* the first
    /// cue instead. A palette entry that does something other than what it says
    /// is worse than one that declines.
    command: Option<Command>,
}

/// Every row the palette can show, in display order.
///
/// Rebuilt each frame the palette is open, and that is fine: it is forty small
/// structs behind a modal, and several of the commands close over live mirror
/// state (`!m.advanced`, `m.edit_bank`, `m.selected_cue`) so a `static` table
/// would need a resolver pass to say the same thing.
fn palette_items(m: &UiMirror) -> Vec<PaletteItem> {
    let a = accel();
    vec![
        // File & Session
        PaletteItem {
            name: "Save Project",
            category: "File",
            shortcut: format!("{a}+S"),
            command: Some(Command::SaveProject),
        },
        PaletteItem {
            name: "Save Project As...",
            category: "File",
            shortcut: String::new(),
            command: Some(Command::SaveProjectAs),
        },
        PaletteItem {
            name: "Open Project...",
            category: "File",
            shortcut: String::new(),
            command: Some(Command::OpenProject),
        },
        PaletteItem {
            name: "Open Project Editor (vidiotic-prep)",
            category: "File",
            shortcut: String::new(),
            command: Some(Command::OpenProjectEditor),
        },
        PaletteItem {
            name: "Open Control Mapper (vidiotic-ctl)",
            category: "File",
            shortcut: String::new(),
            command: Some(Command::OpenControlMapper),
        },
        PaletteItem {
            name: "Quit",
            category: "File",
            shortcut: format!("{a}+Q"),
            command: Some(Command::Quit),
        },
        // Transport & Beat Grid
        PaletteItem {
            name: "Tap Downbeat",
            category: "Transport",
            shortcut: "t".to_string(),
            command: Some(Command::TapDownbeat),
        },
        PaletteItem {
            name: "Tap Tempo",
            category: "Transport",
            shortcut: "b".to_string(),
            command: Some(Command::TapTempo),
        },
        PaletteItem {
            name: "Soft Reset Beat Grid",
            category: "Transport",
            shortcut: "r".to_string(),
            command: Some(Command::SoftReset),
        },
        PaletteItem {
            name: "Hard Reset (Grid & Playlist)",
            category: "Transport",
            shortcut: "Shift+R".to_string(),
            command: Some(Command::HardReset),
        },
        PaletteItem {
            name: "BPM +1",
            category: "Transport",
            shortcut: "+".to_string(),
            command: Some(Command::BpmDelta(1.0)),
        },
        PaletteItem {
            name: "BPM -1",
            category: "Transport",
            shortcut: "-".to_string(),
            command: Some(Command::BpmDelta(-1.0)),
        },
        PaletteItem {
            name: "Nudge Tempo -0.1%",
            category: "Transport",
            shortcut: "[".to_string(),
            command: Some(Command::NudgeBpm(-0.001)),
        },
        PaletteItem {
            name: "Nudge Tempo +0.1%",
            category: "Transport",
            shortcut: "]".to_string(),
            command: Some(Command::NudgeBpm(0.001)),
        },
        PaletteItem {
            name: "Toggle Preserve Playhead on Cut",
            category: "Transport",
            shortcut: String::new(),
            command: Some(Command::SetPreservePlayhead(!m.preserve_playhead)),
        },
        // Banks & Cues
        PaletteItem {
            name: "Cycle Live Bank Next",
            category: "Banks",
            shortcut: ".".to_string(),
            command: Some(Command::CycleLiveBank(1)),
        },
        PaletteItem {
            name: "Cycle Live Bank Prev",
            category: "Banks",
            shortcut: ",".to_string(),
            command: Some(Command::CycleLiveBank(-1)),
        },
        PaletteItem {
            name: "Send Edit Bank to Live",
            category: "Banks",
            shortcut: String::new(),
            command: Some(Command::SetLiveBank(m.edit_bank)),
        },
        PaletteItem {
            name: "Add New Cue Bank",
            category: "Banks",
            shortcut: String::new(),
            command: Some(Command::AddBank),
        },
        PaletteItem {
            name: "Clone Edit Cue Bank",
            category: "Banks",
            shortcut: String::new(),
            command: Some(Command::CloneBank),
        },
        PaletteItem {
            name: "Select Next Cue",
            category: "Cues",
            shortcut: String::new(),
            command: Some(Command::SelectCueDelta(1)),
        },
        PaletteItem {
            name: "Select Prev Cue",
            category: "Cues",
            shortcut: String::new(),
            command: Some(Command::SelectCueDelta(-1)),
        },
        PaletteItem {
            name: "Select First Cue",
            category: "Cues",
            shortcut: String::new(),
            command: Some(Command::SelectCueFirst),
        },
        PaletteItem {
            name: "Select Last Cue",
            category: "Cues",
            shortcut: String::new(),
            command: Some(Command::SelectCueLast),
        },
        PaletteItem {
            name: "Remove Selected Cue",
            category: "Cues",
            shortcut: String::new(),
            command: m.selected_cue.map(Command::RemoveCue),
        },
        PaletteItem {
            name: "Mark Cue In to Playhead",
            category: "Cues",
            shortcut: String::new(),
            command: m.selected_cue.map(Command::SetCueInToPlayhead),
        },
        PaletteItem {
            name: "Mark Cue Out to Playhead",
            category: "Cues",
            shortcut: String::new(),
            command: m.selected_cue.map(Command::SetCueOutToPlayhead),
        },
        // Clips & Pool
        PaletteItem {
            name: "Add Cue for Selected Clip",
            category: "Clips",
            shortcut: String::new(),
            command: m.selected_clip.map(Command::AddCue),
        },
        PaletteItem {
            name: "Pick / Change Clip Directory...",
            category: "Clips",
            shortcut: String::new(),
            command: Some(Command::PickClipDir),
        },
        PaletteItem {
            name: "Add Clip Directory as Bank...",
            category: "Clips",
            shortcut: String::new(),
            command: Some(Command::PickClipBankDir),
        },
        PaletteItem {
            name: "Select Next Clip",
            category: "Clips",
            shortcut: String::new(),
            command: Some(Command::SelectClipDelta(1)),
        },
        PaletteItem {
            name: "Select Prev Clip",
            category: "Clips",
            shortcut: String::new(),
            command: Some(Command::SelectClipDelta(-1)),
        },
        PaletteItem {
            name: "Select First Clip",
            category: "Clips",
            shortcut: String::new(),
            command: Some(Command::SelectClipFirst),
        },
        PaletteItem {
            name: "Select Last Clip",
            category: "Clips",
            shortcut: String::new(),
            command: Some(Command::SelectClipLast),
        },
        // Shader & FX
        PaletteItem {
            name: "Capture Live Shader to Pool",
            category: "Shader",
            shortcut: "c".to_string(),
            command: Some(Command::CaptureShader),
        },
        PaletteItem {
            name: "Pick & Compile ISF Effect...",
            category: "Shader",
            shortcut: String::new(),
            command: Some(Command::PickIsf),
        },
        PaletteItem {
            name: "Pick Main Shader File...",
            category: "Shader",
            shortcut: String::new(),
            command: Some(Command::PickShader),
        },
        // View & Modes
        PaletteItem {
            name: "Toggle Command Palette",
            category: "View",
            shortcut: format!("{a}+P"),
            command: Some(Command::ToggleCommandPalette),
        },
        PaletteItem {
            name: "Toggle Fullscreen",
            category: "View",
            shortcut: "f".to_string(),
            command: Some(Command::ToggleFullscreen),
        },
        PaletteItem {
            name: "Toggle Advanced Mode",
            category: "View",
            shortcut: String::new(),
            command: Some(Command::SetAdvancedMode(!m.advanced)),
        },
        PaletteItem {
            name: "Toggle Modal Grammar Mode",
            category: "View",
            shortcut: String::new(),
            command: Some(Command::SetGrammarMode(!m.grammar_on)),
        },
        // History
        PaletteItem {
            name: "Undo",
            category: "History",
            shortcut: format!("{a}+Z"),
            command: Some(Command::Undo),
        },
        PaletteItem {
            name: "Redo",
            category: "History",
            shortcut: format!("{a}+Shift+Z"),
            command: Some(Command::Redo),
        },
    ]
}

/// Draw the palette and act on this frame's input.
///
/// Called only while it is open; the caller owns that flag. Query text and the
/// highlighted row live in egui's temp data rather than in the mirror, because
/// they are this widget's own state and nothing outside it has any use for them.
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
            // A row with no command is inert here as well as under the mouse.
            if let Some(item) = filtered.get(selected_index) {
                execute_cmd = item.command.clone();
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
                    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
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
                                let available = item.command.is_some();
                                let fg = if !available {
                                    p.fg_muted
                                } else if is_selected {
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
                                                    RichText::new(item.shortcut.as_str())
                                                        .font(mono())
                                                        .color(p.accent_dim),
                                                );
                                            },
                                        );
                                    }
                                });

                                let rect = resp.response.rect;
                                let clicked = ui
                                    .interact(
                                        rect,
                                        Id::new(("cmd_item", idx)),
                                        egui::Sense::click(),
                                    )
                                    .clicked();
                                if let (true, Some(cmd)) = (clicked, item.command.clone()) {
                                    let _ = tx.send(cmd);
                                    let _ = tx.send(Command::ToggleCommandPalette);
                                    query.clear();
                                    selected_index = 0;
                                }
                            }
                        }
                    });

                    ui.separator();
                    ui.label(
                        RichText::new(
                            "↑↓ navigate  ·  enter select  ·  esc close  ·  grey needs a selection",
                        )
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
