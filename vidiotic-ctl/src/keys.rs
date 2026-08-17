//! Canonical key-name strings shared by every consumer of a `ControlSource::Key`
//! binding. Deliberately winit- and egui-free — but *not* toolkit-agnostic by
//! accident. The two toolkits spell the same physical key differently, so an
//! adapter must hand its key to the entry point matching how its toolkit named
//! it: [`from_character`] for a literal character (winit's `Key::Character`),
//! [`from_named`] for a key enum's `Debug` name (winit's `NamedKey`, and every
//! `egui::Key` — egui has no character variant). [`canon`] normalizes a name
//! that is already in this space: one read back off disk, or hand-typed.
//!
//! The canonical spelling of a punctuation or digit key is the **literal
//! character** (`"["`, `"1"`), not either toolkit's word for it — `.vmap` is
//! meant to be hand-editable, and `[` is what a hand-editor writes. Letters are
//! lowercase (`"t"`, so `"T"` and `"t"` bind the same). Every other named key
//! keeps its W3C-ish `Debug` name (`"Space"`, `"ArrowLeft"`, `"F1"`), which
//! winit and egui already agree on without either side depending on the other.

/// egui folds punctuation and digits into named variants (`OpenBracket`,
/// `Num1`); winit reports the same keys as `Key::Character("[")`. This table is
/// the bridge — every `egui::Key` whose `Debug` name isn't already canonical,
/// paired with the canonical form.
///
/// One table serves both toolkits because nothing in winit's `NamedKey` shares
/// a name with the left column, and every left-column name is multi-character
/// while every canonical form here is a single one — so a name can never be
/// mistaken for the other kind. Adding a row that breaks either property breaks
/// [`from_character`]'s guarantee that a literal character is passed through
/// untouched.
const NAMED_TO_CHARACTER: &[(&str, &str)] = &[
    ("Backslash", "\\"),
    ("Backtick", "`"),
    ("CloseBracket", "]"),
    ("CloseCurlyBracket", "}"),
    ("Colon", ":"),
    ("Comma", ","),
    ("Equals", "="),
    ("Exclamationmark", "!"),
    ("Minus", "-"),
    ("Num0", "0"),
    ("Num1", "1"),
    ("Num2", "2"),
    ("Num3", "3"),
    ("Num4", "4"),
    ("Num5", "5"),
    ("Num6", "6"),
    ("Num7", "7"),
    ("Num8", "8"),
    ("Num9", "9"),
    ("OpenBracket", "["),
    ("OpenCurlyBracket", "{"),
    ("Period", "."),
    ("Pipe", "|"),
    ("Plus", "+"),
    ("Questionmark", "?"),
    ("Quote", "'"),
    ("Semicolon", ";"),
    ("Slash", "/"),
];

/// Canonicalize a key reported as the literal character it produces — winit's
/// `Key::Character`. The character *is* the canonical name, so this only
/// lowercases it; the [`NAMED_TO_CHARACTER`] table is deliberately not
/// consulted, so a key that types the word `"Minus"` could never be read as the
/// `-` key.
#[must_use]
pub fn from_character(c: &str) -> String {
    lower_single(c)
}

/// Canonicalize a key reported as a name — the `Debug` form of `egui::Key` or
/// winit's `NamedKey`. Punctuation and digits resolve through
/// [`NAMED_TO_CHARACTER`] to their literal character, letters lowercase, and
/// anything else passes through.
#[must_use]
pub fn from_named(name: &str) -> String {
    canon(name)
}

/// Normalize a key name that is already in this module's space: one read back
/// off disk or hand-typed into a `.vmap`. Tolerates surrounding whitespace and
/// the egui spelling of a punctuation/digit key, which maps written before the
/// name table existed can contain. Idempotent: `canon(canon(k)) == canon(k)`.
#[must_use]
pub fn canon(raw: &str) -> String {
    let raw = raw.trim();
    NAMED_TO_CHARACTER
        .iter()
        .find(|(named, _)| *named == raw)
        .map_or_else(
            || lower_single(raw),
            |(_, character)| (*character).to_string(),
        )
}

/// Lowercase a single character; leave any longer name alone.
fn lower_single(raw: &str) -> String {
    if raw.chars().count() == 1 {
        raw.to_lowercase()
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chars_lowercase() {
        assert_eq!(from_character("T"), "t");
        assert_eq!(from_character("t"), "t");
        assert_eq!(from_named("T"), "t");
    }

    #[test]
    fn named_keys_pass_through() {
        for name in ["Space", "ArrowLeft", "F1", "Escape", "Enter"] {
            assert_eq!(from_named(name), name);
            assert_eq!(canon(name), name);
        }
    }

    /// The bug this table exists for: the winit and egui spellings of every
    /// affected physical key must land on one canonical name. Left column is
    /// what `egui::Key` `Debug`s to, right is what winit's
    /// `Key::Character` carries.
    #[test]
    fn winit_and_egui_spellings_agree() {
        let pairs = [
            ("OpenBracket", "["),
            ("CloseBracket", "]"),
            ("OpenCurlyBracket", "{"),
            ("CloseCurlyBracket", "}"),
            ("Comma", ","),
            ("Period", "."),
            ("Minus", "-"),
            ("Plus", "+"),
            ("Equals", "="),
            ("Colon", ":"),
            ("Semicolon", ";"),
            ("Quote", "'"),
            ("Backtick", "`"),
            ("Backslash", "\\"),
            ("Slash", "/"),
            ("Pipe", "|"),
            ("Questionmark", "?"),
            ("Exclamationmark", "!"),
            ("Num0", "0"),
            ("Num1", "1"),
            ("Num2", "2"),
            ("Num3", "3"),
            ("Num4", "4"),
            ("Num5", "5"),
            ("Num6", "6"),
            ("Num7", "7"),
            ("Num8", "8"),
            ("Num9", "9"),
        ];
        for (egui_name, winit_character) in pairs {
            assert_eq!(
                from_named(egui_name),
                from_character(winit_character),
                "{egui_name} (egui) and {winit_character:?} (winit) must canonicalize alike"
            );
            assert_eq!(
                from_named(egui_name),
                winit_character,
                "the canonical form of {egui_name} is the literal character"
            );
        }
    }

    /// Every row must be reachable from `from_named` and stable under `canon`,
    /// or a spelling silently keeps missing its binding.
    #[test]
    fn every_table_row_canonicalizes_to_its_character() {
        for (named, character) in NAMED_TO_CHARACTER {
            assert_eq!(from_named(named), *character);
            assert_eq!(canon(character), *character);
        }
    }

    /// `from_character` must never consult the table: a key that types the word
    /// `"Minus"` is not the `-` key. Guaranteed by every table name being
    /// multi-character and every canonical form single — assert it directly so
    /// a bad row fails here rather than in the field.
    #[test]
    fn table_names_and_characters_never_overlap() {
        for (named, character) in NAMED_TO_CHARACTER {
            assert!(named.chars().count() > 1, "{named} must be multi-character");
            assert_eq!(
                character.chars().count(),
                1,
                "{character:?} must be one character"
            );
            assert_eq!(
                from_character(named),
                *named,
                "from_character must not map {named}"
            );
        }
    }

    #[test]
    fn canon_is_idempotent() {
        for raw in [
            "T",
            "t",
            "Space",
            "ArrowLeft",
            "F1",
            " q ",
            "OpenBracket",
            "[",
            "Num1",
            "1",
        ] {
            let once = canon(raw);
            let twice = canon(&once);
            assert_eq!(once, twice, "canon not idempotent for {raw:?}");
        }
    }

    /// Maps written before the name table existed can hold the egui spelling.
    #[test]
    fn canon_migrates_the_egui_spelling() {
        assert_eq!(canon("OpenBracket"), "[");
        assert_eq!(canon("Num1"), "1");
        assert_eq!(canon(" Comma "), ",");
    }
}
