//! Panel layout and widgets for the span editor, in the phosphor buffer idiom:
//! bracket buttons, glyph checkboxes, and segmented selectors over the
//! hue-rotatable Everforest palette. Numeric entry stays on stock
//! `DragValue`/`TextEdit` (the theme squares and recolors them), matching how
//! vidiotic's own control window uses the toolkit.
//!
//! # What these panels can reach
//!
//! An [`Editor`] and a [`PrepMirror`]. Not `PrepApp` — that was the whole
//! problem (web-port.md §2): every panel here took `&mut PrepApp`, and
//! `PrepApp` is the module with ffmpeg, `rfd`, `std::fs` and a unix socket in
//! it, so 1,173 lines of egui that contain no OS call of their own could not be
//! compiled for a browser.
//!
//! Reads go straight to the editor, which is why prep's mirror is two fields
//! and the player's is a page: after the split, the editor *is* the portable
//! half, so there is nothing to hide from a panel that reads it. Writes go
//! through [`Editor::post`] — including the ones that need a machine, which
//! come back out of the drain for the shell to answer. A file chooser is
//! `Command::PickVideo`, not an `rfd` call.
//!
//! Two panels stayed behind in `vidiotic-prep`'s `shell_ui`: the binding tables and the
//! export dialog. The inspector draws the binding tables inside its own scroll
//! area, so [`draw`] takes a hook for them rather than knowing what they are.

use phosphor::icon;
use phosphor::theme::palette;
use phosphor::widgets;
use vidiotic_core::project::SyncSpec;

use crate::commands::Command;
use crate::editor::Editor;
use crate::mirror::PrepMirror;

/// Draw the whole UI for one frame. `ui` is the root [`egui::Ui`] eframe hands
/// the app; panels are shown on it in order (top bar, then side, then central).
///
/// `inspector_extras` is drawn at the bottom of the inspector's scroll area —
/// natively the two binding tables, and nothing at all in a browser.
pub fn draw(
    ed: &mut Editor,
    m: &PrepMirror,
    ui: &mut egui::Ui,
    inspector_extras: &mut dyn FnMut(&mut egui::Ui),
) {
    let ctx = ui.ctx().clone();
    top_bar(ed, ui);
    if ed.pending_open.is_some() {
        confirm_open_dialog(ed, &ctx);
    }
    if ed.show_quit_dialog {
        quit_dialog(ed, &ctx);
    }
    egui::Panel::bottom("transport").show(ui, |ui| transport(ed, m, ui));
    egui::Panel::right("inspector")
        .resizable(true)
        .default_size(340.0)
        .size_range(280.0..=520.0)
        .show(ui, |ui| inspector(ed, ui, inspector_extras));
    egui::CentralPanel::default_margins().show(ui, |ui| central(ed, m, ui));
}

fn top_bar(ed: &mut Editor, ui: &mut egui::Ui) {
    egui::Panel::top("toolbar").show(ui, |ui| {
        let p = palette();
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if widgets::bracket_button(ui, "open video…", None, 0.0).clicked() {
                ed.post(Command::PickVideo);
            }
            if widgets::bracket_button(ui, "open project…", None, 0.0)
                .on_hover_text("reopen an exported .viproj to retrim its spans")
                .clicked()
            {
                ed.post(Command::PickProject);
            }
            ui.add_space(8.0);
            if let Some(media) = &ed.media {
                ui.label(
                    egui::RichText::new(format!(
                        "{}x{}  {:.2} fps  {} frames  {:.2}s",
                        media.width, media.height, media.fps, media.frames, media.duration_sec
                    ))
                    .color(p.fg_secondary),
                );
            } else {
                ui.label(egui::RichText::new("no source loaded").color(p.fg_muted));
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(!ed.spans.spans.is_empty(), |ui| {
                if widgets::bracket_button(ui, "export…", None, 0.0).clicked() {
                    ed.post(Command::ShowExportDialog);
                }
            });
        });
        if let Some(status) = &ed.status {
            // Errors persist; plain status fades out after a few seconds.
            let visible = ed.status_is_error
                || ed.status_at.is_none_or(|t| t.elapsed().as_secs_f32() < 6.0);
            if visible {
                let color = if ed.status_is_error {
                    p.error
                } else {
                    // Keep repainting so the fade actually happens without input.
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                    p.fg_muted
                };
                ui.colored_label(color, status);
            }
        }
        ui.add_space(4.0);
    });
}

fn central(ed: &Editor, m: &PrepMirror, ui: &mut egui::Ui) {
    if ed.media.is_none() {
        ui.centered_and_justified(|ui| ui.label("Open a video to begin"));
        return;
    }

    ui.centered_and_justified(|ui| {
        if let Some(tex) = &m.preview {
            let avail = ui.available_size();
            let tex_size = tex.size_vec2();
            let scale = (avail.x / tex_size.x).min(avail.y / tex_size.y).clamp(0.05, 1.0);
            ui.image((tex.id(), tex_size * scale));
        }
    });
}

/// Playback/trim controls, plus the statusline that always shows regardless
/// of whether a source is loaded (so the mode bar and theme toggle aren't
/// only reachable once a file's open).
fn transport(ed: &mut Editor, m: &PrepMirror, ui: &mut egui::Ui) {
    if ed.media.is_some() {
        transport_controls(ed, ui);
    } else {
        ui.weak("no source loaded");
    }
    ui.add_space(4.0);
    statusline_bar(ed, m, ui);
}

fn transport_controls(ed: &mut Editor, ui: &mut egui::Ui) {
    let frames = ed.total_frames();
    let fps = ed.fps();
    let max = frames.saturating_sub(1);

    // Transport keys are resolved in `Controls::observe`, through the same
    // `vidiotic-ctl` mapper as MIDI and gamepads — see
    // `crate::control_input::default_map` for the built-in bindings.

    ui.horizontal(|ui| {
        ui.label(format!("t = {:.3}s", ed.cur_frame as f64 / fps));
        ui.label("frame");
        let mut frame_val = ed.cur_frame;
        if ui.add(egui::DragValue::new(&mut frame_val).range(0..=max)).changed() {
            ed.post(Command::Pause);
            ed.post(Command::Seek(frame_val));
        }
        ui.label(format!("/ {max}"));
    });

    crate::timeline::timeline(ed, ui);

    ui.horizontal_wrapped(|ui| {
        let p = palette();
        let play_label = if ed.playing() { icon::PAUSE } else { icon::PLAY };
        if widgets::bracket_button(ui, play_label, Some(p.playing), 0.0)
            .on_hover_text("play/pause (space) · shift+space plays from in · J/K/L shuttle")
            .clicked()
        {
            ed.post(Command::TogglePlay);
        }
        if ed.playing() && ed.play_speed.abs() != 1.0 {
            ui.label(egui::RichText::new(format!("{:+}×", ed.play_speed)).color(p.fg_muted));
        }
        if widgets::bracket_button(ui, icon::STEP_BACK, None, 0.0)
            .on_hover_text("step back (←, shift = 10)")
            .clicked()
        {
            ed.post(Command::Step(-1));
        }
        if widgets::bracket_button(ui, icon::STEP_FWD, None, 0.0)
            .on_hover_text("step forward (→, shift = 10)")
            .clicked()
        {
            ed.post(Command::Step(1));
        }
        ui.add_space(8.0);
        widgets::wrap_unit(ui, "zoom_unit", |ui| {
            widgets::section_label(ui, "zoom");
            if widgets::bracket_button(ui, icon::ZOOM_OUT, None, 0.0).on_hover_text("zoom out").clicked() {
                ed.post(Command::ZoomView(2.0));
            }
            if widgets::bracket_button(ui, icon::ZOOM_IN, None, 0.0).on_hover_text("zoom in").clicked() {
                ed.post(Command::ZoomView(0.5));
            }
            if widgets::bracket_button(ui, icon::FIT, None, 0.0)
                .on_hover_text("show whole clip")
                .clicked()
            {
                ed.post(Command::ZoomFit);
            }
            if widgets::bracket_button(ui, icon::TO_MARKS, None, 0.0)
                .on_hover_text("zoom to in/out marks")
                .clicked()
            {
                ed.post(Command::ZoomToMarks);
            }
            ui.label(
                egui::RichText::new(format!("[{}..{}]", ed.view_start, ed.view_end()))
                    .color(p.fg_muted),
            );
        });
    });

    ui.horizontal_wrapped(|ui| {
        if widgets::bracket_button(ui, "set in", None, 0.0)
            .on_hover_text("set in point (I)")
            .clicked()
        {
            ed.post(Command::SetIn);
        }
        if widgets::bracket_button(ui, "set out", None, 0.0)
            .on_hover_text("set out point (O)")
            .clicked()
        {
            ed.post(Command::SetOut);
        }
        ui.add_space(8.0);
        if widgets::bracket_button(ui, icon::JUMP_IN, None, 0.0)
            .on_hover_text("jump to in")
            .clicked()
        {
            ed.post(Command::JumpToIn);
        }
        if widgets::bracket_button(ui, icon::JUMP_OUT, None, 0.0)
            .on_hover_text("jump to out (last included frame)")
            .clicked()
        {
            ed.post(Command::JumpToOut);
        }
    });

    ui.horizontal_wrapped(|ui| {
        let len = ed.pending_out.saturating_sub(ed.pending_in);
        ui.label(
            egui::RichText::new(format!(
                "marks [{}..{})  {:.2}s · {:.2} beats",
                ed.pending_in,
                ed.pending_out,
                len as f64 / fps,
                ed.beats(len, None)
            ))
            .color(palette().accent),
        );
        if widgets::bracket_button(ui, "add span", None, 0.0)
            .on_hover_text("add span from marks (Enter)")
            .clicked()
        {
            ed.post(Command::AddSpan);
        }
        ui.add_space(8.0);
        // Tier 2: ephemeral UI state, mutated directly. Undo must not see it,
        // so routing it through a command would be ceremony for nothing.
        ui.add(egui::DragValue::new(&mut ed.snap_beats).range(0.25..=256.0).speed(0.25));
        let hover = format!(
            "set out = in + {} beats @ {:.1} bpm (session bpm)",
            ed.snap_beats, ed.defaults.bpm
        );
        if widgets::bracket_button(ui, "snap out", None, 0.0).on_hover_text(hover).clicked() {
            ed.post(Command::SnapOut);
        }
    });
}

/// Vim-style mode word: ERROR on a failed op > EXPORT while baking > PLAY
/// while scrubbing/looping > NORMAL otherwise — mirrors vidiotic's own
/// statusline priority (ERROR > ENTRY > NORMAL).
fn mode_word(ed: &Editor, m: &PrepMirror) -> (&'static str, Option<egui::Color32>) {
    let p = palette();
    if ed.status_is_error {
        ("ERROR", Some(p.error))
    } else if m.exporting {
        ("EXPORT", Some(p.accent))
    } else if ed.playing() {
        ("PLAY", Some(p.playing))
    } else {
        ("NORMAL", None)
    }
}

/// The bottom statusline: mode word, loaded source + span/bpm summary, and
/// the collapsed theme toggle — the prep-side twin of vidiotic's statusline.
fn statusline_bar(ed: &Editor, m: &PrepMirror, ui: &mut egui::Ui) {
    let name = ed
        .source_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no source loaded".to_string());
    let sources: std::collections::HashSet<_> = ed.spans.spans.iter().map(|s| &s.source).collect();
    let summary = if sources.len() > 1 {
        format!(
            "{name}   {} span(s) across {} source(s) · {:.1} bpm",
            ed.spans.spans.len(),
            sources.len(),
            ed.defaults.bpm,
        )
    } else {
        format!("{name}   {} span(s) · {:.1} bpm", ed.spans.spans.len(), ed.defaults.bpm)
    };
    widgets::statusline(ui, mode_word(ed, m), &summary);
}

fn inspector(ed: &mut Editor, ui: &mut egui::Ui, extras: &mut dyn FnMut(&mut egui::Ui)) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        widgets::section_label(ui, "spans");
        span_list(ed, ui);

        ui.add_space(16.0);
        widgets::section_label(ui, "clip banks");
        bank_editor(ed, ui);

        ui.add_space(16.0);
        widgets::section_label(ui, "session defaults");
        defaults_editor(ed, ui);

        ui.add_space(16.0);
        extras(ui);
    });
}

fn span_list(ed: &mut Editor, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    let selected = ed.spans.selected;
    let bank_names = ed.bank_names.clone();
    let fps = ed.fps();
    let frames = ed.total_frames();
    let session_bpm = ed.defaults.bpm.max(1.0);
    let n = ed.spans.spans.len();
    let current_source = ed.source_path.clone();
    // Only show a per-row filename badge once more than one source is
    // actually present — keeps the common single-video case unchanged.
    let multi_source = ed
        .spans
        .spans
        .iter()
        .map(|s| &s.source)
        .collect::<std::collections::HashSet<_>>()
        .len()
        > 1;

    for i in 0..n {
        ui.push_id(i, |ui| {
            let p = palette();
            let span = &ed.spans.spans[i];
            let is_sel = selected == Some(i);
            let is_current = current_source.as_deref() == Some(span.source.as_path());
            // Border priority mirrors `media_tile`: selected > hovered > default.
            // Hover state for this row isn't known until after the frame is
            // painted, so read back last pass's response (egui replays widget
            // rects from the previous pass) via `ctx.read_response`.
            let hover_id = ui.make_persistent_id("row_hover");
            let hovered = ui.ctx().read_response(hover_id).is_some_and(|r| r.hovered());
            let border = if is_sel {
                p.accent
            } else if hovered {
                p.fg_secondary
            } else {
                p.border
            };
            let resp = egui::Frame::group(ui.style())
                .fill(if is_sel { p.accent_dim } else { p.bg_inset })
                .stroke(egui::Stroke::new(1.0, border))
                .corner_radius(egui::CornerRadius::ZERO)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let resp = ui
                            .selectable_label(is_sel, format!("#{i}"))
                            .on_hover_text("click: select · double-click: audition loop");
                        if resp.double_clicked() {
                            actions.push(Command::AuditionSpan(i));
                        } else if resp.clicked() {
                            actions.push(Command::SelectSpan(i));
                        }
                        // Copy-then-diff: document state routes through a
                        // command so one executor sees every edit. `resp` is
                        // kept in hand — undo will want lost_focus() as its
                        // coalescing barrier.
                        let mut name = span.name.clone();
                        let resp =
                            ui.add(egui::TextEdit::singleline(&mut name).desired_width(110.0));
                        if resp.changed() {
                            actions.push(Command::SetSpanName(i, name));
                        }
                        if multi_source {
                            let badge = span
                                .source
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.label(egui::RichText::new(badge).small().color(p.fg_muted));
                        }
                        if widgets::bracket_button(ui, icon::MOVE_UP, None, 0.0)
                            .on_hover_text("move up")
                            .clicked()
                        {
                            actions.push(Command::MoveSpanUp(i));
                        }
                        if widgets::bracket_button(ui, icon::MOVE_DOWN, None, 0.0)
                            .on_hover_text("move down")
                            .clicked()
                        {
                            actions.push(Command::MoveSpanDown(i));
                        }
                        if widgets::bracket_button(ui, icon::DELETE, Some(p.error), 0.0)
                            .on_hover_text("delete")
                            .clicked()
                        {
                            actions.push(Command::RemoveSpan(i));
                        }
                    });
                    ui.horizontal(|ui| {
                        if is_current {
                            // Bounds (`frames`/`fps`) belong to the currently
                            // open video, which this span is from — safe to
                            // edit in place.
                            let mut inf = span.in_frame;
                            let mut outf = span.out_frame;
                            let r_in = ui.add(
                                egui::DragValue::new(&mut inf).range(0..=outf.saturating_sub(1)),
                            );
                            ui.label("..");
                            let r_out =
                                ui.add(egui::DragValue::new(&mut outf).range(inf + 1..=frames));
                            if r_in.changed() || r_out.changed() {
                                actions.push(Command::SetSpanRange {
                                    idx: i,
                                    in_frame: inf,
                                    out_frame: outf,
                                });
                            }
                            let secs = (span.out_frame - span.in_frame) as f64 / fps;
                            ui.label(
                                egui::RichText::new(format!(
                                    "({:.2}s · {:.2}b)",
                                    secs,
                                    secs * span.bpm.unwrap_or(session_bpm).max(1.0) / 60.0
                                ))
                                .color(p.fg_secondary),
                            );
                            if widgets::bracket_button(ui, icon::EDIT, None, 0.0)
                                .on_hover_text("retrim: load span into marks")
                                .clicked()
                            {
                                actions.push(Command::LoadMarksFromSpan(i));
                            }
                            if widgets::bracket_button(ui, icon::SAVE, None, 0.0)
                                .on_hover_text("update span from marks")
                                .clicked()
                            {
                                actions.push(Command::UpdateSpanFromMarks(i));
                            }
                        } else {
                            // This span's own video isn't open, so its frame
                            // bounds/fps aren't known here — show it read-only
                            // rather than risk editing against the wrong
                            // video's length.
                            ui.label(
                                egui::RichText::new(format!(
                                    "[{}..{})",
                                    span.in_frame, span.out_frame
                                ))
                                .color(p.fg_secondary),
                            );
                            if widgets::bracket_button(ui, "reopen", None, 0.0)
                                .on_hover_text("reopen this span's source video to edit it")
                                .clicked()
                            {
                                actions.push(Command::LoadMarksFromSpan(i));
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut has_bpm = span.bpm.is_some();
                        if widgets::glyph_checkbox(ui, &mut has_bpm, "bpm").changed() {
                            actions.push(Command::SetSpanBpm(
                                i,
                                has_bpm.then_some(span.bpm.unwrap_or(120.0)),
                            ));
                        }
                        if let Some(current) = span.bpm {
                            let mut bpm = current;
                            if ui
                                .add(egui::DragValue::new(&mut bpm).range(20.0..=300.0).speed(0.5))
                                .changed()
                            {
                                actions.push(Command::SetSpanBpm(i, Some(bpm)));
                            }
                        }
                        widgets::section_label(ui, "bank");
                        let labels: Vec<&str> =
                            bank_names.iter().map(String::as_str).collect();
                        if let Some(bi) =
                            widgets::segmented(ui, ("bank", i), &labels, Some(span.clip_bank))
                        {
                            actions.push(Command::SetSpanBank(i, bi));
                        }
                    });
                });
            ui.interact(resp.response.rect, hover_id, egui::Sense::hover());
        });
    }

    for cmd in actions {
        ed.post(cmd);
    }
}

fn bank_editor(ed: &mut Editor, ui: &mut egui::Ui) {
    let can_remove = ed.bank_names.len() > 1;
    let mut actions = Vec::new();
    for (i, name) in ed.bank_names.iter().enumerate() {
        ui.horizontal(|ui| {
            let mut edited = name.clone();
            if ui.add(egui::TextEdit::singleline(&mut edited).desired_width(160.0)).changed() {
                actions.push(Command::SetBankName(i, edited));
            }
            if can_remove
                && widgets::bracket_button(ui, icon::DELETE, Some(palette().error), 0.0)
                    .on_hover_text("remove bank")
                    .clicked()
            {
                actions.push(Command::RemoveBank(i));
            }
        });
    }
    if widgets::bracket_button(ui, &format!("{} add bank", icon::ADD), None, 0.0).clicked() {
        actions.push(Command::AddBank);
    }
    for cmd in actions {
        ed.post(cmd);
    }
}

/// Copy-then-diff over the whole struct: `SessionDefaults` has no `PartialEq`,
/// so "did this change?" comes from the widgets' own `.changed()` responses
/// rather than a struct compare. Every `changed` write below must come from a
/// response or a click — set it unconditionally anywhere and this posts a
/// command every frame, which undo would later record as an edit per frame.
fn defaults_editor(ed: &mut Editor, ui: &mut egui::Ui) {
    let mut d = ed.defaults.clone();
    let mut changed = false;
    let mut pick_shader = false;

    egui::Grid::new("defaults_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("bpm");
        changed |= ui.add(egui::DragValue::new(&mut d.bpm).range(1.0..=999.0).speed(0.5)).changed();
        ui.end_row();

        ui.label("quantum");
        changed |=
            ui.add(egui::DragValue::new(&mut d.quantum).range(1.0..=32.0).speed(0.25)).changed();
        ui.end_row();

        ui.label("phrase len");
        changed |= ui.add(egui::DragValue::new(&mut d.phrase_len).range(1..=256)).changed();
        ui.end_row();

        ui.label("sync");
        ui.horizontal(|ui| {
            let selected = match d.sync {
                SyncSpec::Internal => 0,
                SyncSpec::Link => 1,
            };
            if let Some(i) = widgets::segmented(ui, "sync", &["internal", "link"], Some(selected)) {
                d.sync = if i == 0 { SyncSpec::Internal } else { SyncSpec::Link };
                changed = true;
            }
        });
        ui.end_row();

        ui.label("preserve playhead");
        changed |= widgets::glyph_checkbox(ui, &mut d.preserve_playhead, "").changed();
        ui.end_row();

        ui.label("shader path");
        ui.horizontal(|ui| {
            let mut text = d.shader_path.clone().unwrap_or_default();
            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(140.0)).changed() {
                d.shader_path = (!text.is_empty()).then_some(text);
                changed = true;
            }
            // The chooser is the shell's to raise; it applies the pick to the
            // defaults itself, so this posts nothing but the request.
            pick_shader = widgets::bracket_button(ui, "…", None, 0.0).clicked();
        });
        ui.end_row();
    });

    if changed {
        ed.post(Command::SetDefaults(Box::new(d)));
    }
    if pick_shader {
        ed.post(Command::PickShaderPath);
    }
}

/// Confirmation before opening a large video (decoding a multi-GB file can
/// take a moment); small videos open immediately without this.
fn confirm_open_dialog(ed: &mut Editor, ctx: &egui::Context) {
    let Some(pending) = &ed.pending_open else { return };
    let gb = pending.size_bytes as f64 / 1e9;
    let name = pending
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut action = None;
    egui::Window::new("Open a large file?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("{name} is {gb:.1} GB — opening it may take a moment."));
            ui.horizontal(|ui| {
                if widgets::bracket_button(ui, "open", None, 0.0).clicked() {
                    action = Some(Command::ConfirmPendingOpen);
                }
                if widgets::bracket_button(ui, "cancel", None, 0.0).clicked() {
                    action = Some(Command::CancelPendingOpen);
                }
            });
        });
    if let Some(cmd) = action {
        ed.post(cmd);
    }
}

/// Shown on quit when spans are marked but haven't been exported since they
/// last changed.
fn quit_dialog(ed: &mut Editor, ctx: &egui::Context) {
    let spans = ed.spans.spans.len();
    let mut action = None;
    let mut dismiss = false;
    egui::Window::new("Unexported spans")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("{spans} span(s) haven't been exported yet."));
            ui.horizontal(|ui| {
                // `ShowExportDialog` lowers this one itself, so it isn't
                // dismissed here — the two flags are set by one command.
                if widgets::bracket_button(ui, "export…", None, 0.0).clicked() {
                    action = Some(Command::ShowExportDialog);
                }
                if widgets::bracket_button(ui, "quit without exporting", None, 0.0).clicked() {
                    action = Some(Command::ConfirmQuit);
                    dismiss = true;
                }
                if widgets::bracket_button(ui, "cancel", None, 0.0).clicked() {
                    dismiss = true;
                }
            });
        });
    if dismiss {
        ed.show_quit_dialog = false;
    }
    if let Some(cmd) = action {
        ed.post(cmd);
    }
}
