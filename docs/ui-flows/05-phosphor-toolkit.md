# phosphor — Shared UI Toolkit

## Purpose

Phosphor is a character-grid "buffer" UI idiom for egui that provides a cohesive, retro-inspired control interface. It wraps the Everforest palette in HSL with global hue rotation, delivering monospace glyph widgets (bracket buttons, glyph checkboxes, faders, eighth-block meters), square corners everywhere, and a unified theme system. Any app painting through the `phosphor::theme::palette()` function automatically inherits dark/light mode and hue-rotation edits.

## Widgets

**segmented** — One-of-N selector as bracket list `[a] b c`. Selected label wears brackets and accent; unselected items sit dim until hovered. Items flow into parent layout one by one; flows naturally into wrapped rows. (widgets.rs:48-84)

**detent_scroll** — Scroll-with-detents selector showing `[ label ]`. Left-click steps +1, right-click steps −1, both immediate. Horizontal drag scrubs continuously (same "grab" feel as fader). Hovering + mouse wheel also steps through detents. Values wrap at either end. Shows `--` when unselected. (widgets.rs:98-118)

**detent_scroll_uint** — Detent scroll for bare integers within a range; wraps at either end. Returns new value when it changes. (widgets.rs:122-140)

**section_label** — Small lowercase muted label for grouping controls (e.g., "next every"). Never splits; in wrapped rows it moves to the next line as a unit. (widgets.rs:194-199)

**unit_label** — A label that moves to the next wrapped line as a unit instead of splitting text at the row edge. (widgets.rs:203-205)

**wrap_unit** — Layout a cluster of row widgets as one wrapping unit inside `horizontal_wrapped`. When the cluster no longer fits the remaining row, it breaks to the next line intact rather than splitting between children. Responds to frame-to-frame width measurements. (widgets.rs:217-238)

**chip** — Parenthesized tag like `(2 peers)` or `(metadata ×)`. When removable, a trailing `✕` sits inside the parens; its click reports as `removed`, separate from the tag's own `clicked`. (widgets.rs:251-275)

**media_tile** — Paint a clip/cue tile as a bordered buffer cell: thumbnail inset above a one-row glyph name (`▶name`). Border color carries selection/role; a phosphor pulse border animates while playing. Reports single/double click and hover. TileRole enum (Playing, Armed, None) drives the glyph prefix. (widgets.rs:302-376)

**bracket_button** — Bracket button `[ label ]` in buffer text. Color tints the label (e.g., error red); hover swaps to accent. Flash parameter (0..1, decaying) inverts the button onto an accent fill so taps read as hits. (widgets.rs:381-399)

**glyph_checkbox** — Glyph checkbox `[x] label` / `[ ] label`. Click anywhere toggles; returned response reports `changed`. Checkbox color changes on check; label color swaps to accent on hover. (widgets.rs:403-429)

**fader** — Solid cap (█) sliding a tick-marked glyph track, one row tall: `├────┼────┤` with cap. Click or drag along the track to set value. Bipolar range (min < 0 < max) gets a bright center detent. Reports `changed`. (widgets.rs:434-482)

**glyph_level** — Mono level as a short eighth-block bar: filled cells up to magnitude (0..1). Phosphor under 50%, armed 50–85%, error near clipping (>85%). Uses character blocks ▁▂▃▄▅▆▇█. (widgets.rs:487-507)

**glyph_fft** — Spectrum as per-column eighth blocks (magnitudes already 0..1): green (phosphor) with alpha gradient per bin, red on clipping (mag > 0.85). (widgets.rs:511-528)

**theme_controls** — Right-aligned theme switchboard painted inside a rect: `[dark] light` selector and hue-rotation strip (14 cells), in buffer idiom. Mutations land through `theme::set_state()`, restyle shows on next `theme::sync()` call. (widgets.rs:544-603)

**theme_toggle** — Collapsed stand-in for `theme_controls`: a small `[◐]`/`[◑]` toggle (dark/light glyph tracks current mode) that opens the full switchboard in a floating popup on click. (widgets.rs:609-626)

**statusline** — Shared statusline strip: full-width `accent_dim`-filled bar with a mode segment (tinted with optional color when something is happening), a summary readout, and collapsed `theme_toggle` at the right edge. Returns whether the mode segment was clicked (tinted mode can double as a click target). (widgets.rs:634-679)

## Theme system

**Palette struct** — 18 semantic color roles (theme.rs:12-37):
  - `bg_base`, `bg_panel`, `bg_elevated`, `bg_inset` — background layers
  - `fg_primary`, `fg_secondary`, `fg_muted` — foreground layers  
  - `accent`, `accent_dim` — selection and interactive highlight (Everforest yellow)
  - `playing`, `armed`, `error`, `border` — semantic state colors
  - `blue`, `magenta` — derived hues
  - `phosphor` — meter/beam green (always the dark-mode anchor, independent of light/dark switch)

**ThemeState** — dark: bool + hue: f32 (degrees). Hue wraps circularly; defaults to hue 0.0 and dark=true. (theme.rs:40-50)

**Everforest HSL anchors** — Palette derived from medium Everforest in HSL, rotated by state.hue. Dark mode and light mode each have distinct saturation/lightness curves for all roles. Light mode keeps phosphor anchored independently. (theme.rs:53-95)

**palette() function** — Read the current frame's palette, thread-safe. Cached in static CURRENT. Returns default if not yet set. (theme.rs:101-106)

**state() / set_state()** — Manage theme state in egui memory (UI-local, not persisted). (theme.rs:110-117)

**Spacing constants** — All defined in theme.rs:
  - `ROW = 18.0` — buffer row height (the height all glyph widgets allocate)
  - `SP_XS = 2.0`, `SP_SM = 4.0`, `SP_MD = 8.0`, `SP_LG = 16.0` — spacing increments

**mono() function** — Returns `FontId::monospace(12.0)` — the buffer font every glyph widget uses. (theme.rs:128-130)

**hsl() function** — HSL → sRGB conversion (h in degrees, s/l in 0..1). Palettes stay coherent under global hue rotation. (theme.rs:142-160)

**with_alpha() function** — Return a palette color at a different alpha, for translucent overlays (hover brighten, tile scrims, beat-pulse fades). (theme.rs:136-138)

**apply() function** — Install icon fonts, set default theme state, apply style to egui context, and request transparent window backing store (macOS-specific fix for rounded corners). Call once at context setup. (theme.rs:174-179)

**sync() function** — Re-derive palette and egui style if theme state changed since last frame. Call once per frame, before building UI. (theme.rs:209-217)

## Shell scaffolding

**run() function** — Wrapper around `eframe::run_native` that applies phosphor theme to `CreationContext` before the app's `build` closure runs. Mirrors standard eframe patterns. (shell.rs:18-31)

**begin_frame() function** — Equivalent to `theme::sync()`. Call once at the top of `eframe::App::ui`, before drawing. (shell.rs:35-37)

**statusline_panel() function** — Show the shared statusline as a bottom panel, wrapping `phosphor::widgets::statusline()`. For apps that embed statusline inside an existing panel instead, call `widgets::statusline()` directly. (shell.rs:42-46)

## How other apps consume it

1. **Theme setup**: Call `phosphor::theme::apply(ctx)` once during context creation (or via `phosphor::shell::run()` for eframe apps).

2. **Per-frame sync**: Call `phosphor::theme::sync(ctx)` or `phosphor::shell::begin_frame(ctx)` at the top of each frame, before any UI layout.

3. **Paint with palette**: All color choices come from `phosphor::theme::palette()` to ensure dark/light and hue edits propagate. No ad-hoc color construction.

4. **Widget layout**: Use `ROW`, `SP_*` constants for spacing; all glyph widgets allocate exactly `ROW` height. They honor `cell_width(ui)` for horizontal positioning.

5. **Statusline**: Call `phosphor::widgets::statusline()` with mode string and optional tint color, and free-text summary. Automatically includes the theme toggle. Or use `phosphor::shell::statusline_panel()` to pin it to the bottom of the window.

6. **Icons**: Use constants from `phosphor::icon` (PLAY, PAUSE, STEP_BACK, etc.) rendered through `phosphor::theme::mono()`. Requires icon fonts installed via `theme::apply()`.

## Observations

**Strengths:**
- Character-grid paradigm is highly compact and scannable; wrapping behavior is predictable and testable (wrap_unit has unit tests).
- Palette system keeps the look coherent under dark/light and hue-rotation edits; no app-specific color workarounds needed.
- Monospace glyph vocabulary is deliberately narrow (brackets, eighth-blocks, box-drawing, FA glyphs) so every control reads instantly.

**Gaps and surprises:**
- **Inconsistent size:** `glyph_level` and `glyph_fft` don't allocate a full `ROW` height like other widgets; they allocate only their galley size and center-align it. This can cause vertical misalignment in wrapping layouts (widgets.rs:504, 514). Consider normalizing to `ROW` everywhere.
- **wrap_unit frame lag:** Intact width measurement comes from the previous frame (widgets.rs:214–215), so a resize corrects on the *next* frame. This is documented but can surprise newcomers if window geometry changes rapidly.
- **Hardcoded detent thresholds:** `DETENT_DRAG_STEP = 24.0` and `DETENT_SCROLL_STEP = 40.0` (widgets.rs:87–89) are baked constants; no API to tune feel for different interaction contexts.
- **media_tile exclusivity:** `media_tile` is the only widget that paints raster art (a texture thumbnail). All others are glyph-only; if an app needs other bitmap tiles, it must duplicate media_tile logic or paint outside the widget system.
- **Theme toggle in every statusline:** `statusline()` always includes `theme_toggle` at the right edge (widgets.rs:677). No option to hide it if the app has a separate theme UI or wants a cleaner statusline.
- **Beat pulse is hardcoded to phosphor:** Playing tiles always pulse with `p.phosphor` color (widgets.rs:365). If an app wants a different pulse color (e.g., per-role), it must fork `media_tile`.
- **No tooltip/help text system:** Widgets like `theme_toggle` have one-line `.on_hover_text()` (widgets.rs:613), but there's no shared "help mode" where all controls show context. Custom per-app.
- **Section labels are always lowercase:** `section_label()` forces `.to_lowercase()` (widgets.rs:197); no way to preserve case if desired.

**Architecture notes:**
- Phosphor is feature-gated (`#[cfg(feature = "shell")]` for shell.rs), allowing plain egui integrations to use theme + widgets without eframe dependency.
- Font installation happens in `theme::apply()` and happens once per context; if an app swaps fonts at runtime, it needs custom handling.
- Theme state lives in egui's temporary data store (not persisted); apps must wire their own serialization if dark/light/hue choices should survive restarts.
