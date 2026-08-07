# Punctuation and digit keys can't be bound from the editor

**Status:** fixed 2026-07-16. Found 2026-07-15 while porting the Command pattern
into `vidiotic-prep`; deliberately left out of that port's scope. See
[The fix, as shipped](#the-fix-as-shipped) below for what actually landed.

**Severity:** low blast radius, bad failure mode. Nothing breaks — the binding
just never fires, and the hardcoded default keeps working, so it reads as
"rebinding didn't take" rather than as an error. It has presumably been broken
since bindings shipped and nobody has noticed.

## Symptom

Bind `[` (or `,`, `.`, `-`, `+`, `=`, or any digit) to an action in the
`vidiotic-ctl` editor, save, and run `vidiotic`. The binding never fires. The
built-in behaviour for that key still happens.

Affects exactly the keys `vidiotic` hardcodes as `Key::Character` matches in
`app.rs:1681-1719` — `+ = - [ ] , .` and the BPM digit entry. Letters and named
keys (`t`, `b`, `Space`, `F1`, `ArrowLeft`, …) are fine.

## Cause

*(This section describes the pre-fix state. See [The fix, as
shipped](#the-fix-as-shipped) for what replaced it.)*

The key-name contract was stringly-typed and only accidentally held. Two
adapters produced the raw name that `keys::canon` normalized:

- `vidiotic::control_input::canon_key` (winit): `Key::Character(c)` → `canon(c)`,
  `Key::Named(n)` → `canon(format!("{n:?}"))`
- `vidiotic_prep::app::PrepApp::key_events` (egui): `canon(format!("{key:?}"))`

That works for named keys and letters because `winit::keyboard::NamedKey` and
`egui::Key` both `Debug`-format to W3C-ish names, and `canon` lowercases single
characters — so egui's `Key::A` → `"A"` → `"a"` meets winit's `Character("a")` →
`"a"`.

It fails for everything else, because egui has no `Character` variant: it folds
punctuation and digits into named ones (`egui-0.35.0/src/data/key.rs:35-118`):

| physical key | egui `Debug` | winit | after `canon` |
|---|---|---|---|
| `[` | `OpenBracket` | `Character("[")` | `"OpenBracket"` vs `"["` |
| `,` | `Comma` | `Character(",")` | `"Comma"` vs `","` |
| `-` | `Minus` | `Character("-")` | `"Minus"` vs `"-"` |
| `1` | `Num1` | `Character("1")` | `"Num1"` vs `"1"` |

`canon` only lowercases single characters, so it never bridges the two. The ctl
editor (egui) writes `OpenBracket`; `vidiotic` (winit) looks up `[`; no binding
matches.

It then fails *silently* rather than loudly: no match means
`Mapper::has_binding` is false, so `vidiotic::app::handle_key` falls through to
its hardcoded default and the key still does its built-in thing.

## Why `vidiotic-prep` is unaffected

egui is on both sides of prep's pipeline — its editor and its resolver agree on
`OpenBracket`, whatever winit thinks — and `PREP_CATALOG`'s defaults use no
punctuation. That's why the Command port left this alone rather than growing in
scope. It is not a reason to think the contract holds.

## Fix (as originally proposed)

A shared name table in `keys.rs` that **both** adapters go through, mapping each
physical key to one canonical name in both directions. Not more special-casing
inside `canon`: `canon` receives an already-lossy string and can't know whether
`"Minus"` meant the key or a literal word.

Sketch: `keys::from_char(&str) -> String` and `keys::from_named(&str) -> String`
(or one `keys::canonical(raw) -> String` with an explicit alias table
`"OpenBracket" | "[" => "["`, `"Num1" | "1" => "1"`, …), with a round-trip test
asserting the winit and egui spellings of every affected key land on the same
canonical form. Pick a canonical spelling per key and write it down — the
literal character (`"["`) is the better choice, since it's what a hand-edited
`.vmap` reader would expect and the format is meant to be hand-editable.

Watch for: changing the canonical form is a `.vmap`/`.viproj` data migration if
any binding already stores the egui spelling — a prep-authored map could contain
`OpenBracket` today. `MAP_VERSION` exists for this.

## The fix, as shipped

A shared name table in `keys.rs` (`NAMED_TO_CHARACTER`) that both adapters go
through, mapping each egui-named punctuation/digit key to the canonical **literal
character** (`OpenBracket` → `[`, `Num1` → `1`). The module now has three entry
points instead of one bare `canon`:

- `keys::from_character(&str)` — for a key reported as the literal it types
  (winit's `Key::Character`). Lowercases only; deliberately does *not* consult
  the table, so a key that types the word `"Minus"` can never be read as `-`.
- `keys::from_named(&str)` — for a key reported as an enum `Debug` name (every
  `egui::Key`, winit's `NamedKey`). Resolves through the table.
- `keys::canon(&str)` — for a name already in this space: read off disk or
  hand-typed. Also resolves the table, so a hand-edited file may use either
  spelling.

One table serves both toolkits because winit's `NamedKey` has no variant that
collides with a table name, and every table name is multi-character while every
canonical form is a single character — asserted by
`keys::tests::table_names_and_characters_never_overlap`.

Adapters updated: `vidiotic::control_input::canon_key` (winit) now splits
`Character`/`Named` across `from_character`/`from_named`;
`vidiotic-ctl`'s and `vidiotic-prep`'s egui capturers call `from_named`.

**Hand-edited files.** Live events are canonicalized at the toolkit boundary, so
maps written by either editor are already correct. A *hand-typed* `.vmap` or
`.viproj` bypasses that, so `ControlMap::canonicalize_keys` normalizes its keys
on load (called by every loader), letting a hand editor write `[` or
`OpenBracket` interchangeably. No format-version bump — there were no extant maps
on disk to migrate, so `MAP_VERSION` and `.viproj` `FORMAT_VERSION` are
unchanged. If persisted maps ever predate this fix, revisit that.

**Tests.** The cross-toolkit contract is verified in
`vidiotic::control_input::tests` — the one crate that depends on both winit and
egui — by canonicalizing real `egui::Key` and `winit::Key` values for the same
physical key and asserting they land together. Confirmed to fail on the
pre-fix code (neutered `canon`) before shipping.

## Don't regress

Adding a punctuation or digit default to either app's built-in map is now safe —
spell it as the literal character (`"["`, `"1"`), which is the canonical form.
Spelling a default the egui way (`"OpenBracket"`) reintroduces a silent dead
binding; `vidiotic-prep`'s
`control_input::tests::every_default_binds_an_already_canonical_key_name` guards
prep's built-ins against exactly that. The contract is that `keys::canon`'s
output is toolkit-free, and it now holds for punctuation and digits too — but
only for names that pass through `from_character`/`from_named`/`canon`. A new
adapter that formats a key some other way is back on its own.
