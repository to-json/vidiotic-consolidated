//! The phosphor visual theme: the Everforest palette in HSL, generated from
//! a dark/light switch and a global hue rotation. Every `Color32` an app
//! paints with should come from [`palette`] rather than being constructed ad
//! hoc, so the look stays coherent as panels are added and survives hue
//! rotation.

use std::sync::Mutex;

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

/// Semantic color roles for the control window.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Window/panel fill.
    pub bg_base: Color32,
    /// Side/top panel fill.
    pub bg_panel: Color32,
    /// Hover and popup fill.
    pub bg_elevated: Color32,
    /// Meter wells, text-edit fills, and thumbnail placeholders.
    pub bg_inset: Color32,
    pub fg_primary: Color32,
    pub fg_secondary: Color32,
    pub fg_muted: Color32,
    /// Selection and interactive accent (Everforest yellow).
    pub accent: Color32,
    /// Selection fill — the statusline background.
    pub accent_dim: Color32,
    pub playing: Color32,
    pub armed: Color32,
    pub error: Color32,
    pub border: Color32,
    pub blue: Color32,
    pub magenta: Color32,
    /// Meter/beam green — always the dark-mode anchor, regardless of mode.
    pub phosphor: Color32,
}

/// Which typeface and grid the theme is wearing.
///
/// A *variant*, not a fork: same widget API, same palette roles, same call
/// sites. Only the font and the metrics change, which is the whole reason the
/// toolkit's idiom was a character grid to begin with.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Face {
    /// 12pt Hack on 18pt rows — what every panel in the tree is laid out for.
    #[default]
    Classic,
    /// Unscii 8x8 at 2x, on a literal 16-point character cell.
    ///
    /// Panels have to be *designed* to this rather than retrofitted onto it,
    /// so it is opt-in until each app's layout has been rebuilt for it.
    Grid,
}

/// Which set of colours the palette roles resolve to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Colors {
    /// Everforest, generated from HSL and rotatable.
    #[default]
    Everforest,
    /// The C64's 16 fixed hardware colours.
    VicII,
}

/// The theme switchboard: dark/light, a global hue rotation in degrees, the
/// typeface, and the colour set.
#[derive(Clone, Copy, PartialEq)]
pub struct ThemeState {
    pub dark: bool,
    pub hue: f32,
    pub face: Face,
    pub colors: Colors,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self {
            dark: true,
            hue: 0.0,
            face: Face::default(),
            colors: Colors::default(),
        }
    }
}

/// The palette roles resolved against `state`'s colour set.
pub fn palette_for(state: ThemeState) -> Palette {
    match state.colors {
        Colors::Everforest => everforest(state),
        Colors::VicII => vic_ii(state.dark),
    }
}

/// Everforest (medium) as HSL anchors, rotated by `state.hue` degrees.
fn everforest(state: ThemeState) -> Palette {
    let h = |base: f32| base + state.hue;
    let phosphor = hsl(h(83.0), 0.34, 0.63);
    if state.dark {
        Palette {
            bg_base: hsl(h(206.0), 0.13, 0.20),
            bg_panel: hsl(h(205.0), 0.13, 0.18),
            bg_elevated: hsl(h(199.0), 0.13, 0.24),
            bg_inset: hsl(h(202.0), 0.14, 0.14),
            fg_primary: hsl(h(41.0), 0.32, 0.75),
            fg_secondary: hsl(h(139.0), 0.06, 0.55),
            fg_muted: hsl(h(150.0), 0.06, 0.42),
            accent: hsl(h(40.0), 0.56, 0.68),
            accent_dim: hsl(h(199.0), 0.12, 0.27),
            playing: phosphor,
            armed: hsl(h(24.0), 0.60, 0.67),
            error: hsl(h(359.0), 0.68, 0.70),
            border: hsl(h(201.0), 0.11, 0.31),
            blue: hsl(h(172.0), 0.31, 0.62),
            magenta: hsl(h(332.0), 0.43, 0.72),
            phosphor,
        }
    } else {
        Palette {
            bg_base: hsl(h(44.0), 0.87, 0.94),
            bg_panel: hsl(h(44.0), 0.60, 0.91),
            bg_elevated: hsl(h(43.0), 0.67, 0.92),
            bg_inset: hsl(h(45.0), 0.45, 0.86),
            fg_primary: hsl(h(202.0), 0.11, 0.40),
            fg_secondary: hsl(h(111.0), 0.07, 0.55),
            fg_muted: hsl(h(111.0), 0.06, 0.66),
            accent: hsl(h(43.0), 1.0, 0.44),
            accent_dim: hsl(h(43.0), 0.57, 0.89),
            playing: hsl(h(68.0), 0.99, 0.32),
            armed: hsl(h(24.0), 0.75, 0.45),
            error: hsl(h(1.0), 0.92, 0.60),
            border: hsl(h(55.0), 0.26, 0.78),
            blue: hsl(h(201.0), 0.55, 0.50),
            magenta: hsl(h(319.0), 0.65, 0.64),
            phosphor,
        }
    }
}

/// The VIC-II's 16 fixed colours (Pepto's measured values), mapped onto the
/// palette roles.
///
/// A costume rather than a second design system, and honest about it: this is
/// sixteen muddy colours chosen in 1982 for a composite encoder, not a UI
/// palette, and several roles have no good candidate. Hue rotation does not
/// apply — there is nothing to rotate, the set is the hardware's.
///
/// The one deliberate infidelity is `fg_primary`. The authentic console is
/// light blue on blue, which is 1.6:1 against its own background and unusable
/// for a panel with real content in it; white and the two greys carry the text
/// instead, and light blue stays as the border and accent it reads best as.
fn vic_ii(dark: bool) -> Palette {
    const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
    const WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
    const RED: Color32 = Color32::from_rgb(0x68, 0x37, 0x2B);
    const CYAN: Color32 = Color32::from_rgb(0x70, 0xA4, 0xB2);
    const PURPLE: Color32 = Color32::from_rgb(0x6F, 0x3D, 0x86);
    const GREEN: Color32 = Color32::from_rgb(0x58, 0x8D, 0x43);
    const BLUE: Color32 = Color32::from_rgb(0x35, 0x28, 0x79);
    const YELLOW: Color32 = Color32::from_rgb(0xB8, 0xC7, 0x6F);
    const ORANGE: Color32 = Color32::from_rgb(0x6F, 0x4F, 0x25);
    const LT_RED: Color32 = Color32::from_rgb(0x9A, 0x67, 0x59);
    const DK_GREY: Color32 = Color32::from_rgb(0x44, 0x44, 0x44);
    const GREY: Color32 = Color32::from_rgb(0x6C, 0x6C, 0x6C);
    const LT_GREEN: Color32 = Color32::from_rgb(0x9A, 0xD2, 0x84);
    const LT_BLUE: Color32 = Color32::from_rgb(0x6C, 0x5E, 0xB5);
    const LT_GREY: Color32 = Color32::from_rgb(0x95, 0x95, 0x95);

    if dark {
        Palette {
            bg_base: BLUE,
            bg_panel: BLUE,
            bg_elevated: PURPLE,
            bg_inset: BLACK,
            fg_primary: WHITE,
            fg_secondary: LT_GREY,
            fg_muted: GREY,
            accent: YELLOW,
            accent_dim: PURPLE,
            playing: LT_GREEN,
            armed: ORANGE,
            error: LT_RED,
            border: LT_BLUE,
            blue: CYAN,
            magenta: PURPLE,
            phosphor: LT_GREEN,
        }
    } else {
        Palette {
            bg_base: LT_GREY,
            bg_panel: LT_GREY,
            bg_elevated: WHITE,
            bg_inset: WHITE,
            fg_primary: BLACK,
            fg_secondary: DK_GREY,
            fg_muted: GREY,
            accent: BLUE,
            accent_dim: CYAN,
            playing: GREEN,
            armed: ORANGE,
            error: RED,
            border: DK_GREY,
            blue: BLUE,
            magenta: PURPLE,
            phosphor: GREEN,
        }
    }
}

/// The palette last applied by [`sync`], readable from anywhere in the UI.
static CURRENT: Mutex<Option<Palette>> = Mutex::new(None);

/// The current frame's palette.
pub fn palette() -> Palette {
    CURRENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .unwrap_or_else(|| palette_for(ThemeState::default()))
}

/// The theme switchboard state, kept in egui memory (UI-local, not project
/// data).
pub fn state(ctx: &Context) -> ThemeState {
    ctx.data_mut(|d| d.get_temp(egui::Id::new("theme_state")))
        .unwrap_or_default()
}

pub fn set_state(ctx: &Context, st: ThemeState) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("theme_state"), st));
}

/// Grid metrics shared by the glyph widgets.
///
/// **Written as multiples of [`Metrics::cell`], never as point literals.** That
/// is the whole point of the type: the 8x8 face is decided at 2x (a 16-point
/// cell) and 3x is a plausible later change, and a tree full of `16.0` would
/// make that a second relayout instead of a one-line edit. Cheap to honour now
/// and expensive to retrofit, which is why §9a insisted the constants be
/// settled before any panel is ported to them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metrics {
    /// The character cell, in points. One glyph is `cell` wide and `cell` tall
    /// on [`Face::Grid`]; on [`Face::Classic`] it is the row height, and the
    /// glyph is narrower than it is tall like any text face.
    pub cell: f32,
    /// Body/monospace font size.
    pub font: f32,
    /// Row height. Equal to `cell` on the grid — leading is what a character
    /// grid does not have.
    pub row: f32,
    pub heading: f32,
    pub small: f32,
    pub item_spacing: egui::Vec2,
    pub button_padding: egui::Vec2,
}

impl Metrics {
    /// What the tree is laid out for today: 12pt on 18pt rows.
    const CLASSIC: Self = Self {
        cell: 18.0,
        font: 12.0,
        row: 18.0,
        heading: 14.0,
        small: 10.0,
        item_spacing: egui::vec2(SP_MD, SP_SM + SP_XS),
        button_padding: egui::vec2(6.0, 3.0),
    };

    /// Unscii 8x8 at 2x. Everything below is `CELL` times a fraction.
    ///
    /// Vertical spacing is zero in both spacing pairs because the row *is* the
    /// cell: a widget that adds 3 points of padding to a 16-point row is a
    /// widget that has left the grid. Horizontal spacing survives at ½ and ¼ of
    /// a cell, which are whole numbers of pixels at 2x and still line up.
    ///
    /// One size, not five. A character grid has no 14pt heading; a heading is
    /// distinguished by colour and by the row it occupies. (A 2-cell
    /// double-height title is the authentic move and stays available — it is
    /// `heading: CELL * 2.0` — but nothing is laid out for it yet.)
    const GRID: Self = {
        const CELL: f32 = 16.0;
        Self {
            cell: CELL,
            font: CELL,
            row: CELL,
            heading: CELL,
            small: CELL,
            item_spacing: egui::vec2(CELL * 0.5, 0.0),
            button_padding: egui::vec2(CELL * 0.25, 0.0),
        }
    };

    const fn for_face(face: Face) -> Self {
        match face {
            Face::Classic => Self::CLASSIC,
            Face::Grid => Self::GRID,
        }
    }
}

/// Spacing steps, and on the grid they are cell fractions: ⅛, ¼, ½, 1.
///
/// Unchanged between faces on purpose — they already landed on the 8×8 ladder,
/// which is the sort of coincidence that means the toolkit was built for this.
pub const SP_XS: f32 = 2.0;
pub const SP_SM: f32 = 4.0;
pub const SP_MD: f32 = 8.0;
pub const SP_LG: f32 = 16.0;

/// The metrics last applied by [`sync`], readable from anywhere in the UI —
/// the same arrangement as [`palette`], for the same reason.
static METRICS: Mutex<Metrics> = Mutex::new(Metrics::CLASSIC);

/// The current frame's grid metrics.
pub fn metrics() -> Metrics {
    *METRICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The buffer row height. A function rather than the constant it used to be,
/// because it is now a property of the face.
pub fn row() -> f32 {
    metrics().row
}

/// The buffer font every glyph widget lays out with.
pub fn mono() -> FontId {
    FontId::monospace(metrics().font)
}

/// A palette (or `Color32::BLACK`/`WHITE`) color at a different alpha, for
/// derived translucent overlays — hover brighten, tile scrims, beat-pulse
/// fades — that aren't a new color, just an existing one made partly
/// transparent.
pub fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// HSL → sRGB. `h` in degrees (wraps), `s`/`l` in `0..=1`. Palettes defined
/// through this stay coherent under global hue rotation.
pub fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color32::from_rgb(
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

/// Apply the theme to `ctx` at construction. Per-frame theme edits (the
/// statusline's dark/light and hue controls) land through [`sync`].
///
/// Also requests a transparent window backing store via
/// [`egui::ViewportCommand::Transparent`]. Without this, macOS's
/// rounded-corner window mask clips an opaque backing layer with hard,
/// unantialiased corners — visibly square nubs poking past the rounded
/// frame. This reaches the window on any runtime that processes root
/// viewport commands (eframe does this automatically); a custom egui+winit
/// integration that doesn't run [`egui_winit::process_viewport_commands`]
/// needs the equivalent `.with_transparent(true)` set directly on its
/// `winit::window::WindowAttributes` at creation instead.
pub fn apply(ctx: &Context) {
    let st = ThemeState::default();
    install_fonts(ctx, st.face);
    set_state(ctx, st);
    apply_style(ctx, st);
    ctx.send_viewport_cmd(egui::ViewportCommand::Transparent(true));
}

/// Install the face's fonts.
///
/// [`Face::Classic`] keeps egui's defaults and appends **Noto Sans Symbols 2**
/// as a fallback for both families, because Hack and Ubuntu do not carry the
/// Geometric Shapes block that [`crate::icon`] draws from. egui resolves
/// glyphs per-character down the chain, so plain text still uses the primary
/// font.
///
/// [`Face::Grid`] replaces the primary rather than appending to it — the whole
/// claim is that one 8x8 face draws everything, and a fallback would silently
/// paper over a hole in it with a 12pt outline glyph at 16 points, which is
/// exactly the drift the grid exists to prevent. It keeps the defaults *after*
/// unscii in the chain anyway, so an unforeseen codepoint degrades to visible
/// text rather than a missing-glyph box; `scripts/vendor-unscii.sh` is what
/// asserts the set we actually depend on is covered.
///
/// Symbols Nerd Font is gone. It was 2.44 MB carrying private-use codepoints
/// for [`crate::icon`], which now draws from real Unicode — §9a's point, and
/// worth 2.44 MB of a page load on its own.
fn install_fonts(ctx: &Context, face: Face) {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |key: &str, bytes: &'static [u8], first: bool| {
        fonts
            .font_data
            .insert(key.to_owned(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let chain = fonts.families.entry(family).or_default();
            if first {
                chain.insert(0, key.to_owned());
            } else {
                chain.push(key.to_owned());
            }
        }
    };
    match face {
        Face::Classic => add(
            "symbols2",
            &include_bytes!("../assets/NotoSansSymbols2-Regular.ttf")[..],
            false,
        ),
        Face::Grid => add(
            "unscii",
            &include_bytes!("../assets/unscii-8-grid.ttf")[..],
            true,
        ),
    }
    ctx.set_fonts(fonts);
}

/// Re-derive the palette and egui style when the theme state changed since
/// the last frame. Call once per frame, before building the UI.
///
/// A face change also reinstalls the fonts, which throws away egui's glyph
/// atlas — expensive, and gated on the face actually having changed rather
/// than on the state having changed at all, so dragging the hue slider does
/// not rebuild the atlas sixty times a second.
pub fn sync(ctx: &Context) {
    let st = state(ctx);
    let applied_id = egui::Id::new("theme_applied");
    let applied: Option<ThemeState> = ctx.data_mut(|d| d.get_temp(applied_id));
    if applied != Some(st) {
        if applied.map(|a| a.face) != Some(st.face) {
            install_fonts(ctx, st.face);
        }
        apply_style(ctx, st);
        ctx.data_mut(|d| d.insert_temp(applied_id, st));
    }
}

fn apply_style(ctx: &Context, st: ThemeState) {
    let p = palette_for(st);
    *CURRENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(p);
    let m = Metrics::for_face(st.face);
    *METRICS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = m;

    // The control window follows its own switch, never the OS preference.
    ctx.set_theme(if st.dark { egui::Theme::Dark } else { egui::Theme::Light });

    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = st.dark;
        v.panel_fill = p.bg_panel;
        v.window_fill = p.bg_elevated;
        v.extreme_bg_color = p.bg_inset;
        v.selection.bg_fill = p.accent_dim;
        v.selection.stroke = Stroke::new(1.0, p.accent);
        v.hyperlink_color = p.blue;
        v.error_fg_color = p.error;

        // The buffer aesthetic is square: no rounded corners anywhere,
        // including egui's own window/menu chrome (defaults to 6px).
        let radius = CornerRadius::ZERO;
        v.window_corner_radius = radius;
        v.menu_corner_radius = radius;
        v.widgets.noninteractive.corner_radius = radius;
        v.widgets.noninteractive.bg_fill = p.bg_panel;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.fg_primary);

        v.widgets.inactive.corner_radius = radius;
        v.widgets.inactive.weak_bg_fill = p.bg_elevated;
        v.widgets.inactive.bg_fill = p.bg_elevated;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.fg_primary);

        v.widgets.hovered.corner_radius = radius;
        v.widgets.hovered.weak_bg_fill = p.bg_elevated;
        v.widgets.hovered.bg_fill = p.bg_elevated;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.fg_secondary);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.accent);

        v.widgets.active.corner_radius = radius;
        v.widgets.active.weak_bg_fill = p.accent_dim;
        v.widgets.active.bg_fill = p.accent_dim;
        v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0, p.fg_primary);

        v.widgets.open.corner_radius = radius;
        v.widgets.open.weak_bg_fill = p.bg_elevated;
        v.widgets.open.bg_fill = p.bg_elevated;
        v.widgets.open.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.open.fg_stroke = Stroke::new(1.0, p.fg_primary);

        style.spacing.item_spacing = m.item_spacing;
        style.spacing.button_padding = m.button_padding;
        // On the grid the interactive box *is* one cell: a 22-point control on
        // a 16-point row is the retrofit §9a says to avoid.
        style.spacing.interact_size.y = if st.face == Face::Grid { m.cell } else { 22.0 };

        // One face: everything is buffer text.
        style.text_styles = [
            (TextStyle::Heading, FontId::new(m.heading, FontFamily::Monospace)),
            (TextStyle::Body, FontId::new(m.font, FontFamily::Monospace)),
            (TextStyle::Monospace, FontId::new(m.font, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(m.font, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(m.small, FontFamily::Monospace)),
        ]
        .into();

        style.animation_time = 0.12;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The globals `apply_style` writes are process-wide, and so is egui's
    /// font atlas cost. Tests that install a face take this so they cannot
    /// interleave and read each other's metrics.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Whether the installed face can actually draw `s`.
    ///
    /// **Not** `Fonts::has_glyph`, which is unusable in egui 0.35: it is
    /// implemented as `resolve_face(c) != replacement_face_key` and returns
    /// `false` for every character, including `'A'` in a stock context.
    /// Measured, not assumed — the first version of these tests reported that
    /// Hack could not draw a plus sign.
    ///
    /// A missing glyph has zero advance, and a present one never does. That is
    /// the property epaint exposes correctly, so it is the one used here.
    fn can_draw(f: &mut egui::epaint::text::FontsView<'_>, s: &str) -> bool {
        let id = mono();
        s.chars().all(|c| f.glyph_width(&id, c) > 0.0)
    }

    /// A context with `face` installed and one frame run, which is what makes
    /// `ctx.fonts` available — before a pass there is no font set to ask.
    fn ctx_with(face: Face) -> Context {
        let ctx = Context::default();
        let st = ThemeState { face, ..ThemeState::default() };
        install_fonts(&ctx, face);
        set_state(&ctx, st);
        apply_style(&ctx, st);
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        ctx
    }

    /// The claim the whole face rests on, and the reason
    /// `scripts/vendor-unscii.sh` edits the font at all: on the grid, a row of
    /// text is exactly one cell tall and one character is exactly one cell
    /// wide. Unscii ships 3/32 em of leading, epaint adds it to every row
    /// (`row_height = ascent - descent + line_gap`), and `FontTweak` has no
    /// way to take it back off — so an unpatched font lays 8x8 glyphs out on
    /// 17.5-point rows and "the cell is 16 points" is quietly false.
    #[test]
    fn the_grid_is_literally_a_grid() {
        let _guard = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ctx = ctx_with(Face::Grid);
        let m = Metrics::GRID;

        let galley = ctx.fonts_mut(|f| f.layout_no_wrap("MMMM".to_owned(), mono(), Color32::WHITE));
        assert_eq!(
            galley.rect.height(),
            m.row,
            "a row of grid text is {} tall, not the {} cell",
            galley.rect.height(),
            m.row
        );
        assert_eq!(
            galley.rect.width(),
            m.cell * 4.0,
            "four cells should be {}, got {}",
            m.cell * 4.0,
            galley.rect.width()
        );

        // Two rows must be exactly two cells — the failure mode is a leading
        // that only shows up once there is more than one line.
        let two = ctx.fonts_mut(|f| f.layout_no_wrap("M\nM".to_owned(), mono(), Color32::WHITE));
        assert_eq!(two.rect.height(), m.row * 2.0, "two rows are not two cells");
    }

    /// Every glyph the interface draws must exist in the face that is
    /// installed. A missing one is a hollow box on screen and completely
    /// invisible in review — which is exactly how a private-use-area icon set
    /// survives as long as it did.
    #[test]
    fn both_faces_can_draw_every_icon() {
        let _guard = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for face in [Face::Classic, Face::Grid] {
            let ctx = ctx_with(face);
            let missing: Vec<&str> = ctx.fonts_mut(|f| {
                crate::icon::ALL
                    .iter()
                    .filter(|(_, glyph)| !can_draw(f, glyph))
                    .map(|(name, _)| *name)
                    .collect()
            });
            assert!(missing.is_empty(), "{face:?} cannot draw {missing:?}");
        }
    }

    /// The other half of the same claim: the private-use codepoints these
    /// icons used to be must now render as *nothing*, in both faces. If Symbols
    /// Nerd Font were still linked this would pass silently and the 2.44 MB
    /// would still be in the bundle with nothing pointing at it.
    #[test]
    fn the_private_use_icon_set_is_really_gone() {
        let _guard = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Font Awesome 4's play/pause/floppy, as Nerd Fonts assigned them.
        for face in [Face::Classic, Face::Grid] {
            let ctx = ctx_with(face);
            ctx.fonts_mut(|f| {
                for c in ['\u{f04b}', '\u{f04c}', '\u{f0c7}'] {
                    assert!(
                        !can_draw(f, &c.to_string()),
                        "{face:?} still resolves U+{:04X} — a Nerd Font is still linked",
                        c as u32
                    );
                }
            });
        }
    }

    /// The glyph vocabulary the *widgets* draw with, as opposed to the icons:
    /// eighth-blocks for meters and faders, box drawing for frames, and the
    /// Legacy Computing block §9a picked the face for in the first place.
    #[test]
    fn the_grid_face_carries_the_blocks_the_widgets_are_built_from() {
        let _guard = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let ctx = ctx_with(Face::Grid);
        let required = [
            ("eighth blocks", "▁▂▃▄▅▆▇█"),
            ("left blocks", "▏▎▍▌▋▊▉█"),
            ("box drawing", "─│┌┐└┘├┤┬┴┼"),
            ("shades", "░▒▓"),
            ("card suits", "♠♡♢♣♥♦"),
            ("legacy computing", "🬀🬂🭰🭽🭾"),
        ];
        ctx.fonts_mut(|f| {
            for (what, glyphs) in required {
                for c in glyphs.chars() {
                    assert!(
                        can_draw(f, &c.to_string()),
                        "the grid face lacks {c:?} (U+{:04X}) from {what}",
                        c as u32
                    );
                }
            }
        });
    }

    /// §9a's actual instruction: *write them as cell multiples, not point
    /// literals*, so that going to 3x is a one-line change rather than a
    /// second relayout. This is that instruction as an assertion — every grid
    /// metric has to be a whole number of eighths of a cell.
    #[test]
    fn every_grid_metric_is_a_fraction_of_the_cell() {
        let m = Metrics::GRID;
        let eighth = m.cell / 8.0;
        for (name, v) in [
            ("font", m.font),
            ("row", m.row),
            ("heading", m.heading),
            ("small", m.small),
            ("item_spacing.x", m.item_spacing.x),
            ("item_spacing.y", m.item_spacing.y),
            ("button_padding.x", m.button_padding.x),
            ("button_padding.y", m.button_padding.y),
            ("SP_XS", SP_XS),
            ("SP_SM", SP_SM),
            ("SP_MD", SP_MD),
            ("SP_LG", SP_LG),
        ] {
            assert_eq!(
                v % eighth,
                0.0,
                "{name} = {v} is not a whole number of {eighth}-point eighth-cells"
            );
        }
        assert_eq!(m.row, m.cell, "the row is the cell; leading is not a thing here");
        assert_eq!(
            m.item_spacing.y, 0.0,
            "vertical item spacing must be zero — the row already is the cell"
        );
    }

    /// The classic face is what every panel in the tree is currently laid out
    /// for, and §9a is explicitly a variant rather than a replacement. Pinning
    /// it means the grid work cannot move the existing apps by accident.
    #[test]
    fn the_classic_face_is_unchanged() {
        let m = Metrics::CLASSIC;
        assert_eq!((m.font, m.row, m.heading, m.small), (12.0, 18.0, 14.0, 10.0));
        assert_eq!(m.item_spacing, egui::vec2(8.0, 6.0));
        assert_eq!(m.button_padding, egui::vec2(6.0, 3.0));
        assert_eq!(ThemeState::default().face, Face::Classic, "the default face must not move");
    }

    /// The VIC-II set is a costume over the same roles, not a second design
    /// system — so every role has to be filled, and the text has to be
    /// readable against the surface it lands on. Light blue on blue is the
    /// authentic console and is 1.6:1; this is the check that keeps the
    /// deliberate infidelity in place.
    #[test]
    fn the_vic_ii_costume_is_still_legible() {
        for dark in [true, false] {
            let p = vic_ii(dark);
            let lum = |c: Color32| {
                let f = |v: u8| {
                    let s = f32::from(v) / 255.0;
                    if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
                };
                0.0722f32.mul_add(f(c.b()), 0.2126f32.mul_add(f(c.r()), 0.7152 * f(c.g())))
            };
            let contrast = |a: Color32, b: Color32| {
                let (x, y) = (lum(a), lum(b));
                (x.max(y) + 0.05) / (x.min(y) + 0.05)
            };
            let ratio = contrast(p.fg_primary, p.bg_base);
            assert!(
                ratio >= 4.5,
                "dark={dark}: body text is {ratio:.1}:1 against the panel, needs 4.5:1"
            );
            assert!(
                contrast(p.fg_secondary, p.bg_base) >= 3.0,
                "dark={dark}: secondary text is below 3:1"
            );
        }
    }

    /// Hue rotation is an Everforest affordance — it generates from HSL. The
    /// VIC-II palette is sixteen fixed hardware colours with nothing to
    /// rotate, and silently rotating it would invent colours the chip cannot
    /// produce.
    #[test]
    fn hue_rotation_does_not_touch_the_hardware_palette() {
        let at = |hue: f32, colors: Colors| {
            palette_for(ThemeState { hue, colors, ..ThemeState::default() }).accent
        };
        assert_ne!(
            at(0.0, Colors::Everforest),
            at(120.0, Colors::Everforest),
            "everforest should rotate"
        );
        assert_eq!(
            at(0.0, Colors::VicII),
            at(120.0, Colors::VicII),
            "the VIC-II palette has no hue to rotate"
        );
    }
}
