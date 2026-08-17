//! Custom timeline/jog widget: a zoomable strip showing the visible frame
//! window with span bands, in/out brackets, and the playhead, plus a
//! whole-clip minimap. Replaces a stock slider so marking decisions aren't
//! made blind.

use phosphor::theme;

use crate::commands::Command;
use crate::editor::Editor;

/// What a drag on the main strip grabbed, latched at drag start so the
/// gesture stays on its target even when the pointer strays.
#[derive(Clone, Copy)]
enum Drag {
    Seek,
    In,
    Out,
    Pan { grab_x: f32, grab_view: f64 },
}

/// Pixel slop for grabbing an in/out bracket instead of seeking.
const GRAB_PX: f32 = 6.0;
const STRIP_H: f32 = 44.0;
const MINIMAP_H: f32 = 14.0;

/// Draw the whole timeline (main strip + minimap). No-op without media.
pub fn timeline(ed: &mut Editor, ui: &mut egui::Ui) {
    if ed.media.is_none() {
        return;
    }
    main_strip(ed, ui);
    ui.add_space(2.0);
    minimap(ed, ui);
}

/// Frame-edge → x for a given window; `f` may lie outside the window.
fn edge_x(rect: &egui::Rect, ppf: f32, view_start: u64, f: u64) -> f32 {
    rect.left() + (f as f64 - view_start as f64) as f32 * ppf
}

fn main_strip(ed: &mut Editor, ui: &mut egui::Ui) {
    let total = ed.total_frames();
    let max = total - 1;
    let response = ui.allocate_response(
        egui::vec2(ui.available_width(), STRIP_H),
        egui::Sense::click_and_drag(),
    );
    let rect = response.rect;

    // Mapping for *interaction* uses the window as displayed last frame —
    // pointer coordinates refer to what the user saw.
    let view_start = ed.view_start;
    let view_len = ed.view_len.max(1);
    let ppf = rect.width() / view_len as f32;
    // Frame whose slice contains x (for seeking).
    let frame_under = |x: f32| -> u64 {
        let f = ((x - rect.left()) / ppf).floor() as i64 + view_start as i64;
        f.clamp(0, max as i64) as u64
    };
    // Nearest frame *edge* to x (for the exclusive-boundary marks).
    let edge_under = |x: f32| -> u64 {
        let e = ((x - rect.left()) / ppf).round() as i64 + view_start as i64;
        e.clamp(0, total as i64) as u64
    };

    let in_x = edge_x(&rect, ppf, view_start, ed.pending_in);
    let out_x = edge_x(&rect, ppf, view_start, ed.pending_out);

    // -- interaction --
    let drag_id = ui.id().with("tl_drag");
    if response.drag_started() {
        let drag = match response.interact_pointer_pos() {
            Some(pos) => {
                let pan = response.dragged_by(egui::PointerButton::Middle)
                    || ui.input(|i| i.modifiers.shift);
                let d_in = (pos.x - in_x).abs();
                let d_out = (pos.x - out_x).abs();
                if pan {
                    Drag::Pan {
                        grab_x: pos.x,
                        grab_view: view_start as f64,
                    }
                } else if d_in <= GRAB_PX && d_in <= d_out {
                    Drag::In
                } else if d_out <= GRAB_PX {
                    Drag::Out
                } else {
                    Drag::Seek
                }
            }
            None => Drag::Seek,
        };
        ui.memory_mut(|m| m.data.insert_temp(drag_id, drag));
    }
    if response.dragged() || response.clicked() {
        let drag = ui
            .memory(|m| m.data.get_temp::<Drag>(drag_id))
            .unwrap_or(Drag::Seek);
        if let Some(pos) = response.interact_pointer_pos() {
            match drag {
                Drag::Seek => {
                    ed.post(Command::Pause);
                    ed.post(Command::Seek(frame_under(pos.x)));
                }
                Drag::In => ed.post(Command::SetPendingIn(edge_under(pos.x))),
                Drag::Out => ed.post(Command::SetPendingOut(edge_under(pos.x))),
                Drag::Pan { grab_x, grab_view } => {
                    ed.post(Command::SetViewStart(
                        grab_view - f64::from((pos.x - grab_x) / ppf),
                    ));
                }
            }
        }
    }
    if response.drag_stopped() {
        ui.memory_mut(|m| m.data.remove::<Drag>(drag_id));
    }

    // Wheel: vertical zooms anchored under the cursor, horizontal pans.
    if response.hovered() {
        let (scroll, hover_x) =
            ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos().map(|p| p.x)));
        if scroll.y != 0.0 {
            let factor = 0.99_f64.powf(f64::from(scroll.y));
            let anchor = hover_x.map_or(ed.cur_frame, frame_under);
            ed.post(Command::ZoomViewAt(factor, anchor));
        }
        if scroll.x != 0.0 {
            ed.post(Command::SetViewStart(
                ed.view_start as f64 - f64::from(scroll.x / ppf),
            ));
        }
        // Resize cursor near a grabbable bracket.
        if let Some(x) = hover_x {
            if (x - in_x).abs() <= GRAB_PX || (x - out_x).abs() <= GRAB_PX {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeHorizontal);
            }
        }
    }

    // Run what the interaction above asked for *before* painting. The frame's
    // main drain happens after `ui::draw` returns, which would leave the strip
    // painting last frame's window and marks — a visible lag on the app's most
    // tactile gesture. Everything a drag can post is the editor's own, so
    // `drain_ui` resolves all of it; anything else parks for the shell.
    let now = ui.ctx().input(|i| i.time);
    ed.drain_ui(now);

    // -- painting (with the post-interaction window, so there's no lag frame) --
    if !ui.is_rect_visible(rect) {
        return;
    }
    let view_start = ed.view_start;
    let view_len = ed.view_len.max(1);
    let ppf = rect.width() / view_len as f32;

    let painter = ui.painter().with_clip_rect(rect);
    let p = theme::palette();
    let hue = theme::state(ui.ctx()).hue;
    painter.rect_filled(rect, 0.0, p.bg_inset);

    // Span bands, tinted by bank. Frame indices are only meaningful relative
    // to the video they were marked on, so only draw spans from the video
    // that's actually open — a foreign-source span's frame numbers would
    // land at the wrong place (or off-strip) on this timeline.
    let current = ed.source_path.clone();
    for (i, span) in ed.spans.spans.iter().enumerate() {
        if Some(&span.source) != current.as_ref() {
            continue;
        }
        let x0 = edge_x(&rect, ppf, view_start, span.in_frame);
        let x1 = edge_x(&rect, ppf, view_start, span.out_frame);
        if x1 <= rect.left() || x0 >= rect.right() {
            continue;
        }
        let is_sel = ed.spans.selected == Some(i);
        let band = egui::Rect::from_min_max(
            egui::pos2(x0.max(rect.left()), rect.top() + 3.0),
            egui::pos2(x1.min(rect.right()), rect.bottom() - 3.0),
        );
        painter.rect_filled(band, 0.0, bank_color(span.clip_bank, is_sel, hue));
        if is_sel {
            painter.rect_stroke(
                band,
                0.0,
                egui::Stroke::new(1.0, p.accent),
                egui::StrokeKind::Inside,
            );
        }
    }

    // Pending in/out marks: filled region + brackets with inward feet.
    let in_x = edge_x(&rect, ppf, view_start, ed.pending_in);
    let out_x = edge_x(&rect, ppf, view_start, ed.pending_out);
    if out_x > rect.left() && in_x < rect.right() {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(in_x.max(rect.left()), rect.top()),
                egui::pos2(out_x.min(rect.right()), rect.bottom()),
            ),
            0.0,
            theme::with_alpha(p.accent, 46),
        );
    }
    let mark_stroke = egui::Stroke::new(2.0, p.accent);
    for (x, dir) in [(in_x, 1.0_f32), (out_x, -1.0)] {
        if x < rect.left() - GRAB_PX || x > rect.right() + GRAB_PX {
            continue;
        }
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            mark_stroke,
        );
        for y in [rect.top() + 1.0, rect.bottom() - 1.0] {
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(x + dir * 5.0, y)],
                mark_stroke,
            );
        }
    }

    // Playhead: line through the current frame's center + a top cap triangle.
    let ph_x = edge_x(&rect, ppf, view_start, ed.cur_frame) + ppf * 0.5;
    let ph_color = p.phosphor;
    painter.line_segment(
        [
            egui::pos2(ph_x, rect.top()),
            egui::pos2(ph_x, rect.bottom()),
        ],
        egui::Stroke::new(2.0, ph_color),
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(ph_x - 5.0, rect.top()),
            egui::pos2(ph_x + 5.0, rect.top()),
            egui::pos2(ph_x, rect.top() + 6.0),
        ],
        ph_color,
        egui::Stroke::NONE,
    ));
}

fn minimap(ed: &mut Editor, ui: &mut egui::Ui) {
    let total = ed.total_frames();
    let response = ui.allocate_response(
        egui::vec2(ui.available_width(), MINIMAP_H),
        egui::Sense::click_and_drag(),
    );
    let rect = response.rect;
    let ppf = rect.width() / total as f32;

    // Click/drag centers the view window at the cursor: pan without zooming.
    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let f = f64::from((pos.x - rect.left()) / ppf);
            ed.post(Command::SetViewStart(f - ed.view_len as f64 / 2.0));
        }
    }

    // As in `main_strip`: apply the pan before drawing the window rect, or it
    // trails the pointer by a frame.
    let now = ui.ctx().input(|i| i.time);
    ed.drain_ui(now);

    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter().with_clip_rect(rect);
    let p = theme::palette();
    let hue = theme::state(ui.ctx()).hue;
    painter.rect_filled(rect, 0.0, p.bg_inset);

    let current = ed.source_path.clone();
    for (i, span) in ed.spans.spans.iter().enumerate() {
        if Some(&span.source) != current.as_ref() {
            continue;
        }
        let x0 = rect.left() + span.in_frame as f32 * ppf;
        let x1 = rect.left() + span.out_frame as f32 * ppf;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0, rect.top() + 2.0),
                egui::pos2(x1.max(x0 + 1.0), rect.bottom() - 2.0),
            ),
            0.0,
            bank_color(span.clip_bank, ed.spans.selected == Some(i), hue),
        );
    }

    // The view window.
    let wx0 = rect.left() + ed.view_start as f32 * ppf;
    let wx1 = rect.left() + (ed.view_start + ed.view_len) as f32 * ppf;
    let wrect = egui::Rect::from_min_max(
        egui::pos2(wx0, rect.top()),
        egui::pos2(wx1.max(wx0 + 2.0), rect.bottom()),
    );
    painter.rect_filled(wrect, 0.0, theme::with_alpha(p.fg_primary, 14));
    painter.rect_stroke(
        wrect,
        0.0,
        egui::Stroke::new(1.0, p.accent),
        egui::StrokeKind::Inside,
    );

    // Playhead tick.
    let px = rect.left() + (ed.cur_frame as f32 + 0.5) * ppf;
    painter.line_segment(
        [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
        egui::Stroke::new(1.0, p.phosphor),
    );
}

/// Deterministic per-bank tint: a golden-angle walk from the phosphor hue
/// anchor, following the theme's global rotation; selected spans pop.
fn bank_color(bank: usize, selected: bool, hue: f32) -> egui::Color32 {
    let h = 83.0 + hue + bank as f32 * 137.5;
    let (s, l, a) = if selected {
        (0.55, 0.62, 230)
    } else {
        (0.40, 0.45, 140)
    };
    theme::with_alpha(theme::hsl(h, s, l), a)
}
