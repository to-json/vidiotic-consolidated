# Grammar — fixes, and the rename to conventional keymap vocabulary

Work plan for `vidiotic-play/src/grammar.rs` and its consumers, from the
review of 2026-08-18. Two halves: behavioural fixes (Set A/C) that stand on
their own, and a mechanical rename (Set B) that changes no behaviour and can
be dropped without stranding anything. Nothing here has been applied yet.

Ordering: fixes first, rename last. The fixes are the value; putting them
first keeps their diffs readable against the file as it stands today, and
means abandoning the rename costs nothing. Each numbered item wants its own
commit.

## Why

The module is named for a linguistic metaphor that the machine does not
implement. `Conjugation` (`grammar.rs:150`) does not inflect anything — `g`
then `m` is "go first", verb + object, which is what
`docs/ui-flows/01-vidiotic-grammar-input.md` calls it in its own first line.
`Pane` and `Meta` are documented as "global nouns" but resolve through the
identical code path as the verb roots. Underneath the vocabulary is a
two-level prefix keymap with a repeat submode — the Emacs prefix-key/keymap
model, magit's transient prefix/suffix, `repeat-mode`'s repeat-map. All three
already have names for every piece.

The metaphor costs three concrete things, and they are what Set A fixes:

1. It promised compositionality the code never delivers, so "verbs keep fixed
   meanings across panes" lives as prose plus four hand-copied label lists
   rather than as one structure (A3).
2. A grammar is a fixed property of a language, so the tables are `const` and
   `step` demands `'static` (A4) — in an app whose `vidiotic-ctl` crate exists
   to let the user edit input maps.
3. Where the metaphor ran out — repetition — the design went incoherent: the
   un-grammatical `Sticky` state has different stray-token semantics from
   `AwaitingConjugation` (A1) and no labels on its entries (A2).

## Decision required before A1

A1 unifies the stray-token rule across the two pending states. Both possible
unifications are coherent and they trade against each other; the two friction
items already on record pull in opposite directions:

- `docs/ui-flows/06-lived-use-friction.md:22` — an empty slot silently
  swallowing a press "reads as broken, not as try-another-key".
- `06-lived-use-friction.md:84-89` — a stray key in a sticky mode silently
  exits it and re-roots, changing what the *next* key means, live.

**World A (default, assumed by A1 below): swallow everywhere, and never open
an option-less prefix.** A stray token never changes what the next key means.
The dead end goes away not by making the trap escapable but by refusing to
enter it: a prefix with zero filled slots emits nothing and stays idle.
Exiting a repeat mode costs one Escape (`Select` on pad, note 35 on MIDI).

**World B: replay everywhere.** A stray token always exits and re-roots.
Leaving a repeat mode stays one keypress, and the dead end becomes escapable
rather than impossible.

World A is the recommendation because this is live performance gear on a
second screen: "did nothing" is a safer failure than "silently rerouted". If
you would rather keep one-press mode exit, say so and A1 inverts — the work is
the same size either way.

## Set A — the fixes

### A1. One stray-token rule, and no option-less prefixes — `grammar.rs:224, 243-252, 745`

Today `AwaitingConjugation` swallows a token with no binding (`:224`) while
`Sticky` exits and replays it as a fresh root (`:247`). Same user action,
opposite semantics. Combined with the ten `empty_root(...)` entries this
produces a trap: in the pool pane `f`/`m`/`d`/`t` open an option-less modal
that then swallows *every* subsequent token, including `b` (Pane) and `;`
(Meta), until Escape.

Fix, in World A:
- Pressing a prefix whose slots are all `None` leaves the machine `Idle` and
  returns a `Step` the shell can surface as a brief "nothing here" note rather
  than an empty overlay. (New `Step` variant, or reuse `Rejected` — prefer a
  new one; `Rejected` currently means "not consumed, fall through", which is
  not what this is.)
- `Sticky` swallows a token it does not own instead of re-rooting.
- Drop the "teaching the matrix by silence" rationale from the module header:
  under this rule nothing is taught by silence, which is the point.

`empty_conjugation_slot_keeps_modal_pending` (`:745`) is the drift-guard that
pins the current rule. Rewrite it to pin the new one — do not delete it. Add a
test that an option-less prefix does not enter a pending state, and one that a
repeat mode survives an unowned token.

Doc updates: `01-vidiotic-grammar-input.md` "Empty conjugation slots are
forgiving", "Non-entry token exiting sticky modes", and both entries in
"Dead ends / surprising behaviors"; the two friction items above.

`fix(play): one stray-token rule, and no option-less prefixes`

### A2. Repeat-mode entries carry their own labels — `grammar.rs:144`, `engine/verbs.rs:52-71`

`StickyTable = [Option<Verb>; TOKEN_COUNT]` has no label field, so the overlay
reverse-engineers option text by pattern-matching verb payloads, ending in
`_ => "again"` (`verbs.rs:69`). Add a sticky verb and the UI silently says
"again" — a wrong label, not a missing one. `Conjugation`, the half the
metaphor covered, got a label field; the half it did not, did not.

Fix: make the entry a struct with `label` and `verb` (mirroring
`Conjugation`), fill the labels at the three sticky-table constructors
(`pm_sticky`, `knob_sticky`, `TAP_STICKY`), and delete the inference block in
`verbs.rs`. Test: every populated entry in every reachable repeat map has a
non-empty label.

`fix(play): repeat-mode entries carry their own labels`

### A3. One shared prefix-label list — `grammar.rs:351, 437, 487, 538, 582`

The header claims verbs keep fixed meanings and the pane supplies the object.
In the data each of the four tables restates its own labels, and
`empty_root(label)` (`:351`) exists only to carry a label for a prefix with no
content — ten of them. Nothing enforces that token 0 is "Go" everywhere; the
sanity sweep (`:959`) pins only roots 6 and 7.

Fix: one `PREFIX_LABELS: [&str; TOKEN_COUNT]`, tables supply bindings only,
`empty_root` and `NC`-padded label repetition go. This is what actually cashes
the claim in the header, and it is the item that most argues the metaphor was
worth having — taken seriously, it produces a smaller file.

Expect roughly -40 lines. No behaviour change; the existing table tests should
pass untouched, which is the check that it is a pure restructure.

`refactor(play): one shared prefix-label list, not four copies`

### A4. Drop `'static` from the machine — `grammar.rs:183, 211`

`GrammarState::Sticky` borrows `entries: &'static StickyTable` (`:183`), which
forces `step(&mut self, table: &'static GrammarTable, …)` (`:211`). The table
is `Copy` and eight entries wide; storing it by value costs a memcpy of a
couple of hundred bytes on mode entry and drops the `'static` entirely.

Fix: store the repeat map by value in the state; `step` takes `&Keymap`.
Nothing else changes today — the value is that it is the precondition for
tables that are not compiled in, and it lets tests build a table locally
instead of borrowing a module static.

`refactor(play): the machine no longer requires 'static tables`

### A5. `Verb` vs `Action` — decide, then do or drop

`Verb` (33 variants, `grammar.rs:103`) and `vidiotic-ctl`'s `Action`
(`model.rs:134`) are two vocabularies over one `Command`, resolved by two
hand-written matches (`engine/verbs.rs:104-172`,
`vidiotic/src/control_input.rs:234-255`). Ten concepts appear in both
(TapTempo, TapDownbeat, BpmDelta, NudgeBpm, Soft/HardReset, ToggleFullscreen,
SaveProject, CycleLiveBank, ToggleCommandPalette). Roughly 21 of the 33 verbs
are a rename of a `Command` with an identical payload. Adding one command
reachable from both surfaces touches about seven sites.

The sharper cost is the other direction: the ~8 verbs that genuinely earn a
separate type — `RemoveSelectedCue`, `AddCueAtClip`, `MarkInToPlayhead`,
`MarkOutToPlayhead`, `CyclePreserve` — are context-free by
design and are exactly the ones the mapper cannot reach. You cannot bind a
MIDI pad to "remove the selected cue" because it lives in the wrong enum.

Constraint: `Command` holds `PathBuf`/`Vec`/`Arc`, so it is not `Copy` and
cannot go into the `const` tables as-is. Collapsing `Verb` wholesale is not
free, and the pure-state-machine isolation of the module is a real benefit of
the split.

Recommended minimum: leave `Verb` alone, and lift the context-free verbs into
`Action` + `PLAYER_CATALOG` so they become bindable. That is additive, breaks
nothing, and takes the capability back from the grammar without a refactor.
Anything larger is a separate decision — do not fold it into this plan
silently.

`feat(ctl): selection-relative actions become bindable`

## Set C — smaller items the review surfaced

Listed separately so they are easy to cut. Neither is in the original five.

### C1. The overlay always spells options as keyboard keys — `engine/verbs.rs:35, 54`

`grammar_modal_view` renders `KEY_TOKENS[i]` unconditionally, so the which-key
overlay reads `g / f / m` even when the sequence is being driven from a
gamepad d-pad or MIDI notes 36-43. Fix: track which source opened the pending
sequence and pick the spelling table from it. Small, and it is the one place
the hardware-agnostic token abstraction leaks.

### C2. Eight knobs exactly fill eight slots — `commands.rs:195-204`, `grammar.rs:560-571`

`CueParamKind` has exactly 8 variants and the cue pane's Tune prefix has
exactly 8 slots. A ninth knob has nowhere to go and no test fails; it just
becomes unreachable. Fix: a test asserting every `CueParamKind` is reachable
from some binding, which fails loudly on the ninth rather than silently
dropping it.

## Set B — the rename

Pure mechanical sweep, no behaviour change, after Set A has landed.

### Vocabulary

| today | conventional | source |
|---|---|---|
| `grammar.rs` | `keymap.rs` | Emacs |
| `GrammarTable` | `Keymap` | Emacs — the per-mode table |
| `RootEntry` / "root" | `Submap` / "prefix" | Emacs prefix key → keymap |
| `.conjugations` | `.bindings` | — |
| `Conjugation` | `Binding` | (magit transient: "suffix") |
| `StickyTable` | `RepeatMap` | Emacs `repeat-mode` |
| `Conjugation.sticky` | `.repeat` | — |
| `Grammar` (the machine) | `keymap::Machine` | — |
| `GrammarState` | `keymap::State` | — |
| `AwaitingConjugation` | `AwaitingBinding` | — |
| `Sticky` | `Repeat` | — |
| `Verb` | `Verb` (unchanged) | see A5 |
| `Token`, `Input`, `Pane`, `Step` | unchanged | already accurate |

The overlay keeps the name `whichkey` — that is the conventional name for the
overlay specifically, and it is already correct.

### Tier 1 — internal only (recommended)

Rename the module, the types, and the rustdoc in the modules that are touched.
Do **not** touch anything wire- or user-visible: `WireCommand::SetGrammarMode`
is serialised by variant name (`vidiotic-wire/src/command.rs:276, 384`) and
`WireStatus.grammar_on` by field name (`reply.rs:182`, example JSON in
`envelope.rs:25`), both under `WIRE_VERSION = 1` (`lib.rs:29`, "bump on any
wire-visible breaking change"). Renaming them is a protocol break that buys a
reader of `keymap.rs` nothing.

Files: `vidiotic-play/src/{grammar.rs → keymap.rs, lib.rs, engine/{mod,verbs}.rs,
commands.rs, ui/{whichkey,status,mod,transport,command_palette}.rs,
web/{mod,input}.rs}`, `vidiotic/src/app/{keys,mod}.rs`,
`vidiotic/src/control_input.rs`, plus the note text in `scripts/wasm-gate.sh:52,
77`.

Leave `Engine::grammar_on` and `Command::SetGrammarMode` spelled as they are —
they are the wire's names — with a one-line comment at each saying so.

`refactor(play): grammar → keymap, the conventional name for a prefix keymap`

### Tier 2 — user-facing too (optional, breaking)

Only if you want the mode itself renamed in the UI and the protocol. Costs:
`WIRE_VERSION` bump to 2 and whatever compatibility that implies for
`vidiotic-ctl`/IPC clients; the transport checkbox and tooltip
(`ui/transport.rs:458-468`); the command-palette entry
(`ui/command_palette.rs:325`); the statusline mode word (`ui/status.rs:285`);
and a prose sweep of `docs/ui-flows/{00,01,02,06}`, `docs/web-port.md`,
`vidiotic/docs/ipc{,-plan}.md`, including renaming
`01-vidiotic-grammar-input.md` and this file.

Not recommended. Keeping "grammar" as the product name for the mode while the
code says `keymap` is a normal split, and it is the half of the rename that
costs nothing.

## Verification

Per commit:

    cargo test -p vidiotic-play
    cargo clippy --workspace --all-targets

Before the Set A commits land, and again after Set B:

    cargo test --workspace
    ./scripts/wasm-gate.sh

The gate's fourth field is a *minimum* test count (`wasm-gate.sh:66-71`), so
adding tests is safe; A1 rewrites its guard test rather than removing it, so
`vidiotic-play` stays at or above 103.

## Non-goals

- Deepening the machine past two tokens. The depth-2 ceiling is a feature; it
  is what makes every sequence learnable and the overlay complete.
- Changing the 8-token width. It comes from the pad — d-pad plus face diamond
  — not from the metaphor, and it is the best idea in the module.
- Runtime-editable keymaps. A4 is the precondition, not the feature; building
  the feature is a separate plan.
