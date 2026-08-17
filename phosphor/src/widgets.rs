//! Shared custom-painted widgets in the phosphor idiom: every control is
//! buffer text on the character grid — bracket buttons and selectors, paren
//! tags, glyph checkboxes and faders, eighth-block meters — with square
//! bordered media tiles as the one bitmap concession. All colors come from
//! [`crate::theme::palette`], so the look stays coherent as panels adopt
//! these and survives hue rotation.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Popup, Rect, RectAlign, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::theme::{self, mono, palette, row};

/// How a [`media_tile`] participates in playback, carried on [`TileSpec`]:
/// picks the name-row glyph and the playing pulse border.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TileRole {
    Playing,
    Armed,
    None,
}

/// Eighth-block ramp for glyph meters.
pub const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Advance width of one buffer cell in the current mono font.
pub fn cell_width(ui: &Ui) -> f32 {
    ui.painter()
        .layout_no_wrap("─".into(), mono(), Color32::WHITE)
        .size()
        .x
}

/// Lay out `text` in the buffer font and paint it centered on a fresh
/// one-row allocation. Returns the rect for interaction.
fn alloc_text(ui: &mut Ui, text: &str, color: Color32) -> (Rect, egui::Response) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), mono(), color);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x, row()), Sense::click());
    ui.painter().galley(
        egui::pos2(rect.min.x, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    (rect, resp)
}

/// One-of-N selector as a bracket list: `[a] b c`. The selected label wears
/// the brackets and the accent; the rest sit dim until hovered. Items flow
/// into the parent layout one by one, so inside a wrapped row a long list
/// breaks across lines instead of running off the edge. Returns the clicked
/// index, if any.
pub fn segmented(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    labels: &[&str],
    selected: Option<usize>,
) -> Option<usize> {
    let p = palette();
    let base_id = ui.make_persistent_id(id_salt);
    let mut clicked = None;

    for (i, label) in labels.iter().enumerate() {
        let is_selected = selected == Some(i);
        let text = if is_selected {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        let galley = ui
            .painter()
            .layout_no_wrap(text.clone(), mono(), p.fg_muted);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(galley.size().x, row()), Sense::hover());
        let resp = ui.interact(rect, base_id.with(i), Sense::click());
        let color = if is_selected {
            p.accent
        } else if resp.hovered() {
            p.fg_primary
        } else {
            p.fg_secondary
        };
        ui.painter().text(
            egui::pos2(rect.min.x, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            mono(),
            color,
        );
        if resp.clicked() {
            clicked = Some(i);
        }
    }

    clicked
}

/// How far a drag must travel (in points) to step one detent.
const DETENT_DRAG_STEP: f32 = 24.0;
/// How far the wheel-scroll accumulator must travel (in points) to step one detent.
const DETENT_SCROLL_STEP: f32 = 40.0;

/// Scroll-with-detents selector: shows the current choice as `[ label ]`.
/// Left-click steps forward one detent, right-click steps back one; a
/// horizontal click-and-drag scrubs continuously (same "grab it" feel as
/// [`fader`]); hovering and scrolling the wheel also steps through the
/// detents. Values wrap around at either end. Renders `--` when `selected`
/// is `None`. Returns the newly selected index when the user steps to a
/// different one.
pub fn detent_scroll(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    labels: &[&str],
    selected: Option<usize>,
    width_chars: usize,
) -> Option<usize> {
    let text = match selected {
        Some(i) => format!("[{:^width$}]", labels[i], width = width_chars),
        None => format!("[{:^width$}]", "--", width = width_chars),
    };
    let id = ui.make_persistent_id(id_salt);
    let (rect, resp) = detent_frame(ui, &text);
    let stepped = detent_step(ui, id, &resp, labels.len());
    let new_selected = stepped.map(|delta| {
        let cur = selected.unwrap_or(0) as i32;
        (cur + delta).rem_euclid(labels.len() as i32) as usize
    });
    detent_paint(ui, rect, &text, resp.hovered());
    new_selected.filter(|&i| Some(i) != selected)
}

/// `detent_scroll` for a bare integer within `range` (wraps at either end).
/// Returns the new value when it changes.
pub fn detent_scroll_uint(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    v: u32,
    range: std::ops::RangeInclusive<u32>,
    width_chars: usize,
) -> Option<u32> {
    let text = format!("[{:^width$}]", v, width = width_chars);
    let id = ui.make_persistent_id(id_salt);
    let (rect, resp) = detent_frame(ui, &text);
    let span = *range.end() as i64 - *range.start() as i64 + 1;
    let stepped = detent_step(ui, id, &resp, span as usize);
    let new_v = stepped.map(|delta| {
        let cur = v as i64 - *range.start() as i64;
        (*range.start() as i64 + (cur + delta as i64).rem_euclid(span)) as u32
    });
    detent_paint(ui, rect, &text, resp.hovered());
    new_v.filter(|&x| x != v)
}

/// Allocate the fixed-width cell for a detent scroller, sensing click and
/// drag (painting happens after step handling so the updated value shows
/// immediately).
fn detent_frame(ui: &mut Ui, text: &str) -> (Rect, Response) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), mono(), Color32::WHITE);
    ui.allocate_exact_size(egui::vec2(galley.size().x, row()), Sense::click_and_drag())
}

/// Resolve this frame's interaction into a whole-detent step: a left-click
/// is `+1` and a right-click is `-1`, both immediate; otherwise a horizontal
/// drag or (while hovered) the wheel accumulates into `id`'s stored offset
/// and steps whenever it crosses a threshold. `count` gates how many detents
/// exist so a lone detent never "steps".
fn detent_step(ui: &mut Ui, id: egui::Id, resp: &Response, count: usize) -> Option<i32> {
    if count <= 1 {
        return None;
    }
    if resp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(id, 0.0_f32));
        return Some(1);
    }
    if resp.secondary_clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(id, 0.0_f32));
        return Some(-1);
    }
    let mut accum: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    let step_size = if resp.dragged() {
        accum += resp.drag_delta().x;
        DETENT_DRAG_STEP
    } else if resp.hovered() {
        accum += ui.input(|i| i.smooth_scroll_delta.y);
        DETENT_SCROLL_STEP
    } else {
        accum = 0.0;
        DETENT_SCROLL_STEP
    };
    let steps = (accum / step_size).trunc() as i32;
    accum -= steps as f32 * step_size;
    ui.ctx().data_mut(|d| d.insert_temp(id, accum));
    (steps != 0).then_some(steps)
}

/// Paint a detent scroller's bracketed text: accent on hover, primary
/// otherwise (mirrors [`segmented`]'s selected-item coloring).
fn detent_paint(ui: &mut Ui, rect: Rect, text: &str, hovered: bool) {
    let p = palette();
    let color = if hovered { p.accent } else { p.fg_primary };
    ui.painter().text(
        egui::pos2(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        mono(),
        color,
    );
}

/// Small lowercase muted label for grouping controls, e.g. "next every".
/// Never splits: in a wrapped row it moves to the next line as a unit.
pub fn section_label(ui: &mut Ui, text: &str) -> Response {
    unit_label(
        ui,
        egui::RichText::new(text.to_lowercase())
            .monospace()
            .size(theme::metrics().small)
            .color(palette().fg_muted),
    )
}

/// A label that moves to the next wrapped line as a unit instead of
/// splitting its text at the row edge.
pub fn unit_label(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend))
}

/// Lay out a cluster of row widgets as one wrapping unit inside a
/// `horizontal_wrapped` row: when the whole cluster no longer fits the
/// remaining row it breaks to the next line intact, rather than splitting
/// between its children. Only when the intact cluster is wider than a full
/// row — i.e. it would overflow even from the leftmost position — do the
/// children flow into the wrapped row individually so they can break.
///
/// The intact width comes from the previous frame's measurement (nested
/// groups report their size only after layout), so a resize corrects on the
/// next frame.
pub fn wrap_unit(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui),
) {
    let id = ui.make_persistent_id(id_salt);
    let known_width: Option<f32> = ui.data(|d| d.get_temp(id));
    let row_width = ui.max_rect().width();
    if known_width.is_some_and(|w| w > row_width) {
        let at_row_start = ui.available_size_before_wrap().x >= row_width - 0.5;
        if !at_row_start {
            ui.end_row();
        }
        add(ui);
    } else {
        if known_width.is_some_and(|w| w > ui.available_size_before_wrap().x) {
            ui.end_row();
        }
        let measured = ui.horizontal(|ui| add(ui)).response.rect.width();
        ui.data_mut(|d| d.insert_temp(id, measured));
    }
}

/// What happened to a [`chip`] this frame.
pub struct ChipResponse {
    pub clicked: bool,
    pub removed: bool,
    pub rect: Rect,
}

/// Parenthesized tag — cue metadata, the peers marker, error tags, pinned
/// shaders: `(2 peers)`. When `removable`, a trailing `✕` sits inside the
/// parens; its click reports as `removed`, separate from the tag's own
/// `clicked`.
pub fn chip(ui: &mut Ui, text: &str, tint: Option<Color32>, removable: bool) -> ChipResponse {
    let p = palette();
    let color = tint.unwrap_or(p.fg_secondary);
    let display = if removable {
        format!("({text} ×)")
    } else {
        format!("({text})")
    };
    let (rect, resp) = alloc_text(ui, &display, color);

    let mut removed = false;
    if removable {
        let cw = cell_width(ui);
        let close_rect =
            Rect::from_min_max(egui::pos2(rect.max.x - cw * 2.5, rect.min.y), rect.max);
        let close_resp = ui.interact(close_rect, resp.id.with("close"), Sense::click());
        if close_resp.hovered() {
            ui.painter().text(
                egui::pos2(close_rect.max.x - cw, close_rect.center().y),
                Align2::RIGHT_CENTER,
                "×)",
                mono(),
                p.error,
            );
        }
        removed = close_resp.clicked();
    }

    ChipResponse {
        clicked: resp.clicked() && !removed,
        removed,
        rect,
    }
}

/// What a [`media_tile`] needs painted: a clip pool tile or a cue chip.
pub struct TileSpec<'a> {
    pub name: &'a str,
    pub tex: Option<&'a egui::TextureHandle>,
    pub role: TileRole,
    /// Accent selection border (cue list: this cue is selected for editing).
    pub selected: bool,
    /// In-pool "referenced by a cue" marker (clip pool only).
    pub active: bool,
    /// 0..1, decays from 1.0 on the beat; drives the playing-tile border pulse.
    pub beat_pulse: f32,
    pub size: Vec2,
}

/// What happened to a [`media_tile`] this frame.
pub struct TileResponse {
    pub clicked: bool,
    pub double_clicked: bool,
    pub hovered: bool,
    pub rect: Rect,
}

/// Paint a clip/cue tile as a bordered buffer cell: thumbnail inset above a
/// one-row glyph name (`▶name`), border color carrying selection/role, and a
/// phosphor pulse border while playing.
pub fn media_tile(ui: &mut Ui, spec: &TileSpec) -> TileResponse {
    let p = palette();
    let m = theme::metrics();
    // Fractions of the cell, not point literals — `Metrics`' own invariant, and
    // this widget was the tree's only violation of it. At the Classic 18-point
    // cell these are the numbers that were hardcoded (14, 10, 3); on the grid's
    // 16 they scale with it instead of leaving a 16-point row with 3 points of
    // padding on it, which is a widget that has left the grid.
    let name_h = m.cell * (7.0 / 9.0);
    let inset = m.cell / 6.0;
    let label = FontId::monospace(m.small);
    let (rect, resp) = ui.allocate_exact_size(spec.size, Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, p.bg_inset);

    // Art sits inset with a reserved name row along the bottom.
    let art = Rect::from_min_max(
        rect.min + Vec2::splat(inset),
        egui::pos2(rect.max.x - inset, rect.max.y - name_h),
    );
    if let Some(tex) = spec.tex {
        egui::Image::new((tex.id(), art.size())).paint_at(ui, art);
        if resp.hovered() {
            painter.rect_filled(
                art,
                CornerRadius::ZERO,
                theme::with_alpha(Color32::WHITE, 20),
            );
        }
    } else {
        painter.text(
            art.center(),
            Align2::CENTER_CENTER,
            "decoding…",
            label.clone(),
            p.fg_muted,
        );
    }

    // Glyph-prefixed name, truncated by width.
    let glyph = match spec.role {
        TileRole::Playing => "▶",
        TileRole::Armed => "○",
        TileRole::None => " ",
    };
    let name_color = if spec.selected {
        p.accent
    } else {
        p.fg_primary
    };
    let mut job = LayoutJob::single_section(
        format!("{glyph}{}", spec.name),
        TextFormat::simple(label, name_color),
    );
    job.wrap = TextWrapping::truncate_at_width((spec.size.x - inset * 2.0).max(0.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(
            rect.min.x + inset,
            rect.max.y - name_h * 0.5 - galley.size().y * 0.5,
        ),
        galley,
        name_color,
    );

    // Border: selection beats the in-pool "active" marker, which beats hover;
    // a playing tile also gets a beat-synced phosphor pulse on top.
    let border = if spec.selected {
        p.accent
    } else if spec.active {
        p.armed
    } else if resp.hovered() {
        p.fg_secondary
    } else {
        p.border
    };
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    if spec.role == TileRole::Playing {
        let alpha = 120 + (spec.beat_pulse.powi(2) * 135.0) as u8;
        painter.rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme::with_alpha(p.phosphor, alpha)),
            StrokeKind::Inside,
        );
    }

    TileResponse {
        clicked: resp.clicked(),
        double_clicked: resp.double_clicked(),
        hovered: resp.hovered(),
        rect,
    }
}

/// Bracket button: `[ label ]` in buffer text. `color` tints the label (e.g.
/// error red for hard reset); hover swaps it to the accent. `flash` (0..1,
/// decaying) inverts the button onto an accent fill so taps read as hits.
pub fn bracket_button(ui: &mut Ui, label: &str, color: Option<Color32>, flash: f32) -> Response {
    let p = palette();
    let text = format!("[ {label} ]");
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), mono(), p.fg_primary);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x, row()), Sense::click());
    let mut fg = color.unwrap_or(p.fg_primary);
    if resp.hovered() {
        fg = p.accent;
    }
    let painter = ui.painter();
    if flash > 0.0 {
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            theme::with_alpha(p.accent, 60 + (flash * 195.0) as u8),
        );
        fg = p.bg_inset;
    } else if resp.is_pointer_button_down_on() {
        painter.rect_filled(rect, CornerRadius::ZERO, p.accent_dim);
    }
    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        mono(),
        fg,
    );
    resp
}

/// Glyph checkbox: `[x] label` / `[ ] label`. Click anywhere toggles; the
/// returned response reports `changed`.
pub fn glyph_checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> Response {
    let p = palette();
    let box_text = if *checked { "[x]" } else { "[ ]" };
    let text = if label.is_empty() {
        box_text.to_string()
    } else {
        format!("{box_text} {label}")
    };
    let galley = ui.painter().layout_no_wrap(text, mono(), p.fg_primary);
    let (rect, mut resp) =
        ui.allocate_exact_size(egui::vec2(galley.size().x, row()), Sense::click());
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    let box_color = if *checked { p.playing } else { p.fg_muted };
    let label_color = if resp.hovered() {
        p.accent
    } else {
        p.fg_primary
    };
    let painter = ui.painter();
    let box_text = if *checked { "[x]" } else { "[ ]" };
    painter.text(
        egui::pos2(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        box_text,
        mono(),
        box_color,
    );
    if !label.is_empty() {
        let cw = cell_width(ui);
        painter.text(
            egui::pos2(rect.min.x + cw * 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            mono(),
            label_color,
        );
    }
    resp
}

/// Fader: a solid cap sliding a tick-marked glyph track, one row tall:
/// `├────┼────┤` with a `█` cap. Click or drag along the track. A bipolar
/// range gets a bright center detent. The returned response reports `changed`.
pub fn fader(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    min: f32,
    max: f32,
    v: &mut f32,
    cells: usize,
) -> Response {
    let p = palette();
    let cw = cell_width(ui);
    let n = cells.max(4);
    // A degenerate range has no position to map a value onto: `(v - min) /
    // (max - min)` is NaN, `clamp` passes NaN through, and the cap lands
    // nowhere. Widen it by an epsilon so the fader draws pinned at its floor
    // and drags are inert, rather than painting garbage.
    let max = if max > min { max } else { min + f32::EPSILON };
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(cw * (n as f32 + 2.0), row()), Sense::hover());
    let mut resp = ui.interact(
        rect,
        ui.make_persistent_id(id_salt),
        Sense::click_and_drag(),
    );
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let t = ((pos.x - rect.min.x - cw) / (cw * n as f32)).clamp(0.0, 1.0);
            let next = min + t * (max - min);
            if next != *v {
                *v = next;
                resp.mark_changed();
            }
        }
    }
    let bipolar = min < 0.0 && max > 0.0;
    let t = ((*v - min) / (max - min)).clamp(0.0, 1.0);
    let mut track = String::with_capacity(n + 2);
    track.push('├');
    for k in 0..n {
        track.push(if k > 0 && k % (n / 4).max(1) == 0 {
            '┼'
        } else {
            '─'
        });
    }
    track.push('┤');
    let painter = ui.painter();
    let put = |col: f32, text: &str, color: Color32| {
        painter.text(
            egui::pos2(rect.min.x + col * cw, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            mono(),
            color,
        );
    };
    put(0.0, &track, theme::with_alpha(p.fg_muted, 200));
    if bipolar {
        put(1.0 + (n - 1) as f32 * 0.5, "┼", p.fg_primary);
    }
    let k = (t * (n - 1) as f32).round();
    put(1.0 + k, "█", if bipolar { p.magenta } else { p.playing });
    resp
}

/// Mono level as a short eighth-block bar: filled cells up to the magnitude
/// (already 0..1), phosphor under half scale, armed past it, error near
/// clipping.
pub fn glyph_level(ui: &mut Ui, mag: f32, cells: usize) {
    let p = palette();
    let mag = mag.clamp(0.0, 1.0);
    let color = if mag > 0.85 {
        p.error
    } else if mag > 0.5 {
        p.armed
    } else {
        p.phosphor
    };
    let filled = mag * cells as f32;
    let mut s = String::with_capacity(cells);
    for k in 0..cells {
        let f = (filled - k as f32).clamp(0.0, 1.0);
        s.push(if f <= 0.0 {
            '▁'
        } else {
            BLOCKS[((f * 7.99) as usize).min(7)]
        });
    }
    let galley = ui.painter().layout_no_wrap(s, mono(), color);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(galley.size().x, row()), Sense::hover());
    ui.painter().galley(
        egui::pos2(rect.min.x, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
}

/// Spectrum as per-column eighth blocks (magnitudes already 0..1): green with
/// brightness following the bin, red on clipping bins.
pub fn glyph_fft(ui: &mut Ui, mags: &[f32]) {
    let p = palette();
    let cw = cell_width(ui);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(cw * mags.len() as f32, row()), Sense::hover());
    let painter = ui.painter();
    for (k, &mag) in mags.iter().enumerate() {
        let mag = mag.clamp(0.0, 1.0);
        let ch = BLOCKS[((mag * 7.99) as usize).min(7)];
        let color = if mag > 0.85 { p.error } else { p.phosphor };
        painter.text(
            egui::pos2(rect.min.x + k as f32 * cw, rect.center().y),
            Align2::LEFT_CENTER,
            ch.to_string(),
            mono(),
            theme::with_alpha(color, 120 + (mag * 135.0) as u8),
        );
    }
}

/// Buffer cells [`theme_controls`] occupies: `[dark] light` (13) + the hue
/// strip (14) + margins. [`theme_toggle`]'s popup reserves this much width
/// (× [`cell_width`]) for the expanded picker.
pub const THEME_CELLS: f32 = 31.0;

/// Buffer cells [`theme_toggle`]'s collapsed button occupies at the edge of
/// its rect — callers reserve this much width (× [`cell_width`]) instead of
/// the full [`THEME_CELLS`], since the picker itself now lives in a popup.
pub const THEME_TOGGLE_CELLS: f32 = 5.0;

/// Rows [`theme_controls`] occupies. The second one carries the face and
/// colour-set selectors §9a added; the first is the original dark/light and
/// hue strip.
pub const THEME_ROWS: f32 = 2.0;

/// One `[selected] other` segmented picker, painted left to right from `col`
/// (in cells) and returning the cell it ended at.
///
/// Factored out when the switchboard grew from one selector to three: the
/// brackets-mark-the-selection idiom is the toolkit's, and three hand-rolled
/// copies of it would drift apart on the first change to any of them.
fn segments<T: PartialEq + Copy>(
    ui: &mut Ui,
    id: &str,
    row_rect: Rect,
    col: f32,
    current: &mut T,
    options: &[(&str, T)],
) -> f32 {
    let p = palette();
    let cw = cell_width(ui);
    let mut col = col;
    for (lab, value) in options {
        let selected = *current == *value;
        let text = if selected {
            format!("[{lab}]")
        } else {
            format!(" {lab} ")
        };
        let w = text.chars().count() as f32;
        let r = Rect::from_min_size(
            egui::pos2(row_rect.min.x + col * cw, row_rect.min.y),
            egui::vec2(w * cw, row_rect.height()),
        );
        let resp = ui.interact(r, ui.id().with((id, *lab)), Sense::click());
        let color = if selected {
            p.accent
        } else if resp.hovered() {
            p.fg_primary
        } else {
            p.fg_secondary
        };
        ui.painter().text(
            egui::pos2(r.min.x, r.center().y),
            Align2::LEFT_CENTER,
            text,
            mono(),
            color,
        );
        if resp.clicked() {
            *current = *value;
        }
        col += w;
    }
    col
}

/// Right-aligned theme switchboard painted inside `rect`: `[dark] light` and
/// the hue-rotation strip on the first row, the face and colour set on the
/// second. Mutations land through [`theme::set_state`], so the restyle shows
/// up when the app next calls [`theme::sync`] — typically the following frame.
///
/// `rect` is expected to be [`THEME_ROWS`] rows tall.
pub fn theme_controls(ui: &mut Ui, rect: Rect) {
    let p = palette();
    let cw = cell_width(ui);
    let mut st = theme::state(ui.ctx());

    // Split off the second row first, so the original row-0 arithmetic below
    // keeps working against a one-row rect exactly as it did.
    let rows = Rect::from_min_max(
        egui::pos2(rect.min.x, rect.min.y + rect.height() / THEME_ROWS),
        rect.max,
    );
    let rect = Rect::from_min_max(
        rect.min,
        egui::pos2(rect.max.x, rect.min.y + rect.height() / THEME_ROWS),
    );

    // Face and colour set. `8x8` rather than `grid` because the number is the
    // decision; `forest` rather than `everforest` because the row is 31 cells.
    let col = segments(
        ui,
        "theme_face",
        rows,
        1.0,
        &mut st.face,
        &[("12pt", theme::Face::Classic), ("8x8", theme::Face::Grid)],
    );
    segments(
        ui,
        "theme_colors",
        rows,
        col + 2.0,
        &mut st.colors,
        &[
            ("forest", theme::Colors::Everforest),
            ("vic-ii", theme::Colors::VicII),
        ],
    );

    let painter = ui.painter();

    // Hue strip at the right edge.
    const STRIP_CELLS: f32 = 14.0;
    let strip = Rect::from_min_size(
        egui::pos2(rect.max.x - cw * (STRIP_CELLS + 1.0), rect.min.y + 4.0),
        egui::vec2(cw * STRIP_CELLS, rect.height() - 8.0),
    );
    let resp = ui.interact(strip, ui.id().with("theme_hue"), Sense::click_and_drag());
    if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            // Hue is circular: the strip's right edge wraps back to 0.
            st.hue =
                (((pos.x - strip.min.x) / strip.width()).clamp(0.0, 1.0) * 360.0).rem_euclid(360.0);
        }
    }
    const N: usize = 28;
    for k in 0..N {
        let cell = Rect::from_min_size(
            egui::pos2(
                strip.min.x + strip.width() * k as f32 / N as f32,
                strip.min.y,
            ),
            egui::vec2(strip.width() / N as f32 + 0.5, strip.height()),
        );
        painter.rect_filled(
            cell,
            CornerRadius::ZERO,
            theme::hsl(k as f32 / N as f32 * 360.0, 0.5, 0.5),
        );
    }
    let x = strip.min.x + strip.width() * st.hue / 360.0;
    painter.line_segment(
        [
            egui::pos2(x, strip.min.y - 2.0),
            egui::pos2(x, strip.max.y + 2.0),
        ],
        Stroke::new(2.0, p.fg_primary),
    );

    // `[dark] light` selector to the strip's left. Its column origin is
    // absolute rather than rect-relative, which is why `segments` takes the
    // offset it does here.
    let origin = Rect::from_min_size(
        egui::pos2(0.0, rect.min.y),
        egui::vec2(rect.max.x, rect.height()),
    );
    segments(
        ui,
        "theme_mode",
        origin,
        strip.min.x / cw - 15.0,
        &mut st.dark,
        &[("dark", true), ("light", false)],
    );

    theme::set_state(ui.ctx(), st);
}

/// Collapsed stand-in for [`theme_controls`]: a small `[◐]`/`[◑]` toggle
/// painted at `rect` (dark/light glyph tracks the current mode) that opens
/// the full switchboard in a floating popup on click, instead of the picker
/// always eating a full row's width.
pub fn theme_toggle(ui: &mut Ui, rect: Rect) {
    let p = palette();
    let resp = ui
        .interact(rect, ui.id().with("theme_toggle"), Sense::click())
        .on_hover_text("theme: dark/light + hue");
    let glyph = if theme::state(ui.ctx()).dark {
        "[◐]"
    } else {
        "[◑]"
    };
    let color = if resp.hovered() {
        p.accent
    } else {
        p.fg_secondary
    };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, glyph, mono(), color);

    Popup::from_toggle_button_response(&resp)
        .align(RectAlign::TOP)
        .show(|ui| {
            let cw = cell_width(ui);
            let (prect, _) = ui.allocate_exact_size(
                egui::vec2(cw * (THEME_CELLS + 2.0), row() * THEME_ROWS + theme::SP_MD),
                Sense::hover(),
            );
            theme_controls(ui, prect);
        });
}

/// The shared statusline strip: a full-width `select`-filled bar with a mode
/// segment (`mode.0`, tinted with `mode.1` when something is happening, else
/// neutral), a `summary` readout, and the collapsed [`theme_toggle`] at the
/// right edge. Used as the last row of each app's bottom panel. Returns
/// whether the mode segment was clicked, so a tinted mode (e.g. "ERROR") can
/// double as a click target without the widget knowing what that means.
pub fn statusline(ui: &mut Ui, mode: (&str, Option<Color32>), summary: &str) -> bool {
    let p = palette();
    let cw = cell_width(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row()), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, p.accent_dim);

    // Mode segment: its own fill when something is happening.
    let (mode_label, mode_bg) = mode;
    let mode_cells = mode_label.chars().count() as f32 + 2.0;
    let mode_rect = Rect::from_min_size(rect.min, egui::vec2(cw * mode_cells, rect.height()));
    if let Some(bg) = mode_bg {
        painter.rect_filled(mode_rect, CornerRadius::ZERO, bg);
    }
    let mode_resp = ui.interact(mode_rect, ui.id().with("statusline_mode"), Sense::click());
    let mode_fg = if mode_bg.is_some() {
        p.bg_inset
    } else {
        p.fg_primary
    };
    ui.painter().text(
        egui::pos2(rect.min.x + cw, rect.center().y),
        Align2::LEFT_CENTER,
        mode_label,
        mono(),
        mode_fg,
    );

    // Clip the summary short of the theme toggle so a narrow window
    // truncates it instead of running the two together.
    let summary_clip = Rect::from_min_max(
        rect.min,
        egui::pos2(rect.max.x - cw * THEME_TOGGLE_CELLS, rect.max.y),
    );
    painter.with_clip_rect(summary_clip).text(
        egui::pos2(rect.min.x + cw * (mode_cells + 2.0), rect.center().y),
        Align2::LEFT_CENTER,
        summary,
        mono(),
        p.fg_secondary,
    );

    let toggle_rect = Rect::from_min_size(
        egui::pos2(rect.max.x - cw * THEME_TOGGLE_CELLS, rect.min.y),
        egui::vec2(cw * THEME_TOGGLE_CELLS, rect.height()),
    );
    theme_toggle(ui, toggle_rect);
    mode_resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive `wrap_unit` for a few frames at the given panel width and report
    /// the rects of the unit's two fixed-size children (30×10 and 90×10),
    /// preceded by a 100×10 filler in the same wrapped row.
    fn run_wrap_unit(panel_width: f32) -> (Rect, Rect, Rect) {
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(panel_width, 400.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
                ui.horizontal_wrapped(|ui| {
                    let (filler, _) =
                        ui.allocate_exact_size(egui::vec2(100.0, 10.0), Sense::hover());
                    let mut a = Rect::NOTHING;
                    let mut b = Rect::NOTHING;
                    wrap_unit(ui, "unit", |ui| {
                        a = ui
                            .allocate_exact_size(egui::vec2(30.0, 10.0), Sense::hover())
                            .0;
                        b = ui
                            .allocate_exact_size(egui::vec2(90.0, 10.0), Sense::hover())
                            .0;
                    });
                    out = Some((filler, a, b));
                });
            });
        }
        out.unwrap()
    }

    #[test]
    fn wrap_unit_stays_inline_when_it_fits() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let (filler, a, b) = run_wrap_unit(300.0);
        assert_eq!(a.min.y, filler.min.y, "unit should share the filler's row");
        assert_eq!(a.min.x, filler.max.x);
        assert_eq!(b.min.x, a.max.x, "children stay adjacent");
    }

    #[test]
    fn wrap_unit_breaks_to_next_row_intact() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        // 100 filler + 120 unit > 180 row: the unit moves down whole.
        let (filler, a, b) = run_wrap_unit(180.0);
        assert!(
            a.min.y > filler.max.y,
            "unit should start a new row, got {a:?}"
        );
        assert_eq!(a.min.x, 0.0, "unit should start at the row edge");
        assert_eq!(b.min.y, a.min.y, "children stay on one row");
        assert_eq!(b.min.x, a.max.x, "children stay adjacent");
    }

    #[test]
    fn wrap_unit_wider_than_row_lets_children_wrap() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        // Unit alone is 120 wide > 100 row: children wrap individually.
        let (filler, a, b) = run_wrap_unit(100.0);
        assert!(a.min.y > filler.max.y, "unit should leave the filler's row");
        assert_eq!(a.min.x, 0.0, "first child starts at the row edge");
        assert!(
            b.min.y >= a.max.y,
            "second child wraps below the first, got a={a:?} b={b:?}"
        );
    }

    /// One pass of the transport's cadence row at `width`, returning where each
    /// cluster landed.
    fn cadence_row(ctx: &egui::Context, width: f32) -> Vec<(&'static str, Rect)> {
        let mut rows = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 400.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 7.0);
            ui.horizontal_wrapped(|ui| {
                let nl = section_label(ui, "next every").rect;
                segmented(ui, "next_cadence", &["1", "2", "4", "8", "16"], Some(2));
                ui.add_space(8.0);
                let mut ll = Rect::NOTHING;
                let mut unit = Rect::NOTHING;
                wrap_unit(ui, "loop_every_unit", |ui| {
                    ll = section_label(ui, "loop every").rect;
                    segmented(
                        ui,
                        "loop_cadence",
                        &["off", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16"],
                        Some(0),
                    );
                    unit = ui.min_rect();
                });
                ui.add_space(8.0);
                let mut pp = true;
                let ppr = glyph_checkbox(ui, &mut pp, "preserve playhead").rect;
                rows.push(("next_label", nl));
                rows.push(("loop_label", ll));
                rows.push(("loop_unit", unit));
                rows.push(("preserve", ppr));
            });
        });
        rows
    }

    /// The transport's cadence row, at the widths a window actually gets dragged
    /// through. Two invariants, and this used to be a scratch reproduction that
    /// printed the numbers and asserted neither.
    ///
    /// **Stability.** A galley's width is not known on the frame that requests it,
    /// so a wrapped row can settle differently on frame 1 than on frame 2 — and if
    /// it never settles it oscillates, which reads as a row of controls flickering
    /// between two layouts forever. Frames 2 and 3 must be identical.
    ///
    /// **Integrity.** The "loop every" label and its segmented control are one
    /// [`wrap_unit`], so they belong to the same row at every width; a label
    /// stranded above its own control is the bug the unit exists to prevent.
    #[test]
    fn the_cadence_row_settles_and_keeps_its_label_with_its_control() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        for width in [900.0_f32, 700.0, 560.0, 420.0, 320.0, 240.0] {
            let ctx = egui::Context::default();
            let _first = cadence_row(&ctx, width);
            let second = cadence_row(&ctx, width);
            let third = cadence_row(&ctx, width);
            assert_eq!(
                second, third,
                "layout at width {width} did not settle: it differs between frames"
            );

            let unit = second
                .iter()
                .find(|(n, _)| *n == "loop_unit")
                .expect("loop_unit")
                .1;
            let label = second
                .iter()
                .find(|(n, _)| *n == "loop_label")
                .expect("loop_label")
                .1;
            assert!(
                unit.contains_rect(label),
                "at width {width} the loop label {label:?} escaped its unit {unit:?}"
            );
        }
    }

    /// Drive one widget for a few frames with the pointer held down at `at`,
    /// returning whatever the closure produced on the last frame.
    ///
    /// Three frames because a click needs a press and a release to have happened,
    /// and because egui only knows a widget's rect once it has been laid out once.
    fn with_pointer<T>(at: egui::Pos2, mut body: impl FnMut(&mut Ui) -> T) -> T {
        let ctx = egui::Context::default();
        let mut out = None;
        for frame in 0..3 {
            let mut events = vec![egui::Event::PointerMoved(at)];
            if frame == 1 {
                events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                });
            }
            if frame == 2 {
                events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                });
            }
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| out = Some(body(ui)));
        }
        out.expect("the ui closure never ran")
    }

    #[test]
    fn segmented_reports_the_option_that_was_clicked() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        // Far left of the row: the first label. Nothing else is drawn there.
        let clicked = with_pointer(egui::pos2(4.0, 8.0), |ui| {
            segmented(ui, "seg", &["a", "b", "c"], Some(2))
        });
        assert_eq!(clicked, Some(0));
    }

    #[test]
    fn segmented_reports_nothing_when_the_pointer_is_elsewhere() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let clicked = with_pointer(egui::pos2(500.0, 150.0), |ui| {
            segmented(ui, "seg", &["a", "b", "c"], Some(0))
        });
        assert_eq!(clicked, None);
    }

    /// Where a widget lands, laid out with the pointer far away so nothing
    /// interacts. The row is measured in theme cells, not points, so a test
    /// cannot hardcode a coordinate inside it.
    fn widget_rect(mut body: impl FnMut(&mut Ui) -> Rect) -> Rect {
        with_pointer(egui::pos2(-1000.0, -1000.0), |ui| body(ui))
    }

    #[test]
    fn fader_maps_a_click_along_the_track_to_a_value() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let track = widget_rect(|ui| {
            let mut v = 0.0;
            fader(ui, "f", 0.0, 10.0, &mut v, 16).rect
        });

        // The track spans one cell in from each end, so the far right is the top
        // of the range and the far left the bottom.
        let mut v = 0.0_f32;
        let changed = with_pointer(egui::pos2(track.max.x, track.center().y), |ui| {
            fader(ui, "f", 0.0, 10.0, &mut v, 16).changed()
        });
        assert!(changed, "a click on the track is a change");
        assert!(v > 9.0, "clicking the right end should reach the top: {v}");

        let mut v = 10.0_f32;
        with_pointer(egui::pos2(track.min.x, track.center().y), |ui| {
            fader(ui, "f", 0.0, 10.0, &mut v, 16);
        });
        assert_eq!(v, 0.0, "clicking the left end should reach the bottom");

        // And a click outside it changes nothing.
        let mut v = 5.0_f32;
        let changed = with_pointer(egui::pos2(track.max.x + 40.0, track.max.y + 40.0), |ui| {
            fader(ui, "f", 0.0, 10.0, &mut v, 16).changed()
        });
        assert!(!changed);
        assert_eq!(v, 5.0);
    }

    /// A degenerate range has no position to map onto, and `(v - min) / (max - min)`
    /// is NaN — which `clamp` passes straight through, so the cap was painted at
    /// a NaN offset.
    #[test]
    fn fader_survives_a_degenerate_range() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let mut v = 5.0_f32;
        with_pointer(egui::pos2(300.0, 8.0), |ui| {
            fader(ui, "f", 5.0, 5.0, &mut v, 16);
        });
        assert!(v.is_finite(), "value went non-finite: {v}");
    }

    #[test]
    fn glyph_checkbox_toggles_on_a_click_and_only_then() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let mut checked = false;
        with_pointer(egui::pos2(4.0, 8.0), |ui| {
            glyph_checkbox(ui, &mut checked, "on air");
        });
        assert!(checked, "a click on the box toggles it");

        let mut checked = false;
        with_pointer(egui::pos2(500.0, 150.0), |ui| {
            glyph_checkbox(ui, &mut checked, "on air");
        });
        assert!(!checked, "a click elsewhere does not");
    }

    #[test]
    fn chip_reports_a_remove_only_when_it_is_removable() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        // The chip's own [x] sits at its right end; a plain chip has none, so the
        // same click has to come back as a plain click instead.
        let r = with_pointer(egui::pos2(4.0, 8.0), |ui| {
            let r = chip(ui, "clip", None, false);
            (r.clicked, r.removed)
        });
        assert_eq!(r, (true, false), "a non-removable chip is never removed");
    }

    #[test]
    fn statusline_reports_a_click_on_the_mode_word() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let clicked = with_pointer(egui::pos2(4.0, 8.0), |ui| {
            statusline(ui, ("BANK", None), "4 cues")
        });
        assert!(clicked, "the mode word is the clickable part");
    }

    /// The level and spectrum readouts take arbitrary numbers from an analyser,
    /// including the ones a silent or broken input produces. They paint rather
    /// than return, so the assertion is that they lay out at all.
    #[test]
    fn the_meters_take_whatever_the_analyser_gives_them() {
        // Widget geometry is measured in theme cells, which live in a
        // process-wide global that another test can change mid-test. Held for the
        // whole test, not per helper call: a measure-then-click test needs the
        // same cell size for both halves.
        let _guard = crate::theme::test_lock();
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 200.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            for mag in [0.0, 0.5, 1.0, -1.0, 2.0, f32::NAN] {
                glyph_level(ui, mag, 8);
            }
            glyph_fft(ui, &[]);
            glyph_fft(ui, &[0.0, 0.5, 1.0, f32::NAN, -3.0]);
            // A zero-cell request still has to allocate something.
            glyph_level(ui, 0.5, 0);
        });
    }
}
