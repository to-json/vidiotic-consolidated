//! Interface glyphs, in **real Unicode** — no private-use area.
//!
//! These were Font Awesome codepoints out of a bundled Symbols Nerd Font, and
//! that was a 2.44 MB dependency on one vendor's PUA assignments: a mapping
//! nothing outside Nerd Fonts agrees with, that no other font can substitute
//! for, and that renders as a missing-glyph box anywhere the bundled font is
//! not. §9a's lo-res face made the cost visible — an 8×8 font can carry Box
//! Drawing, Block Elements and Legacy Computing, and cannot carry somebody's
//! private icon set — so the set moved to characters that mean the same thing
//! everywhere.
//!
//! Chosen against `unscii-8`'s coverage, which is the narrower of the two
//! faces; `scripts/vendor-unscii.sh` fails if any of these is absent, so a
//! future edit here cannot silently reintroduce a hole.
//!
//! Some are two cells. That is not a compromise to fix later — `▶│` says
//! "play to the boundary" in a way no single character in Unicode does, and on
//! a character grid two cells is a normal amount of room for a control.
//!
//! Render through [`crate::theme::mono`] or any monospace `FontId`;
//! [`crate::theme::apply`] installs whichever face backs them.

/// `▶` — play.
pub const PLAY: &str = "\u{25B6}";
/// `‖` — pause. Not U+23F8 ⏸, which almost nothing outside emoji fonts has.
pub const PAUSE: &str = "\u{2016}";
/// `◄` — one frame back.
pub const STEP_BACK: &str = "\u{25C4}";
/// `►` — one frame forward.
pub const STEP_FWD: &str = "\u{25BA}";
/// `│◄` — to the previous boundary. The bar is the boundary.
pub const JUMP_IN: &str = "\u{2502}\u{25C4}";
/// `►│` — to the next boundary.
pub const JUMP_OUT: &str = "\u{25BA}\u{2502}";
/// `−` — zoom out. U+2212 rather than a hyphen so it sits on the maths axis
/// next to `+` instead of hanging low.
pub const ZOOM_OUT: &str = "\u{2212}";
/// `+` — zoom in.
pub const ZOOM_IN: &str = "+";
/// `↔` — fit to width.
pub const FIT: &str = "\u{2194}";
/// `┼` — frame the marks. The crosshair it replaces, drawn from Box Drawing.
pub const TO_MARKS: &str = "\u{253C}";
/// `▲` — move up in a list.
pub const MOVE_UP: &str = "\u{25B2}";
/// `▼` — move down in a list.
pub const MOVE_DOWN: &str = "\u{25BC}";
/// `×` — remove.
pub const DELETE: &str = "\u{00D7}";
/// `+` — add.
pub const ADD: &str = "+";
/// `≡` — edit. Lines of text, which is what every one of these opens.
pub const EDIT: &str = "\u{2261}";
/// `↓` — save, i.e. write it out.
pub const SAVE: &str = "\u{2193}";
/// `▤` — a folder, as a filled index card.
pub const FOLDER: &str = "\u{25A4}";
/// `◴` — rescan. The nearest thing to a cycle arrow that an 8×8 face has, and
/// every call site says the word "refresh" beside it.
pub const REFRESH: &str = "\u{25F4}";
/// `◈` — pin.
pub const PIN: &str = "\u{25C8}";

/// Every glyph above, for the tests that assert the installed faces can draw
/// them. A missing icon is invisible in review and obvious to a user.
pub const ALL: &[(&str, &str)] = &[
    ("PLAY", PLAY),
    ("PAUSE", PAUSE),
    ("STEP_BACK", STEP_BACK),
    ("STEP_FWD", STEP_FWD),
    ("JUMP_IN", JUMP_IN),
    ("JUMP_OUT", JUMP_OUT),
    ("ZOOM_OUT", ZOOM_OUT),
    ("ZOOM_IN", ZOOM_IN),
    ("FIT", FIT),
    ("TO_MARKS", TO_MARKS),
    ("MOVE_UP", MOVE_UP),
    ("MOVE_DOWN", MOVE_DOWN),
    ("DELETE", DELETE),
    ("ADD", ADD),
    ("EDIT", EDIT),
    ("SAVE", SAVE),
    ("FOLDER", FOLDER),
    ("REFRESH", REFRESH),
    ("PIN", PIN),
];
