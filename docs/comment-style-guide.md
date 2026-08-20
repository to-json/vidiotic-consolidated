# Comment style guide

What's actually in this tree's comments, where the written rule and the real
code disagree, and the specific spots worth fixing. Established by reading
the tree at 2026-08-20, not assumed. Every count below has its command in the
appendix, so it can be regenerated instead of taken on faith as this codebase
grows.

---

## 1. Current state (measured)

~44,400 non-blank lines of Rust across the nine workspace crates, plus a
`web/` JS shell and ~64 ISF shader files (`vidiotic-play/shaders/vidvox/*.fs`).

| Dialect | Count |
|---|---|
| Rust `///`/`//!` doc comments | 7,310 lines |
| Rust `//` line comments (non-doc) | 1,757 lines |
| Rust `/* */` block comments | 23 occurrences |
| Shell `#` comments | 687 lines |
| JS `//` comments (`web/*.js`) | 410 lines |
| Shader comments (`.fs`/`.frag`/`.vert`/`.wgsl`) | 707 lines |

Public API coverage: 590 bare `pub fn` items (not counting `pub(crate)` /
`pub(super)`, which aren't really public API), of which 68 (11.5%) have no
`///` immediately above them. That average hides a wide spread by crate:

| Crate | `pub fn` | missing doc | % missing |
|---|---|---|---|
| vidiotic-ctl | 51 | 19 | 37% |
| vidiotic | 106 | 18 | 17% |
| vidiotic-play | 162 | 23 | 14% |
| vidiotic-prep | 45 | 5 | 11% |
| vidiotic-chop | 71 | 2 | 3% |
| phosphor | 41 | 1 | 2% |
| vidiotic-bake | 38 | 0 | 0% |
| vidiotic-core | 65 | 0 | 0% |
| vidiotic-wire | 11 | 0 | 0% |

`vidiotic-core`, `vidiotic-bake`, and `vidiotic-wire` are fully covered.
`vidiotic-ctl` is the outlier: over a third of its public functions have no
doc comment, worse than its raw count of 19 missing suggests.

TODO/FIXME/HACK markers and commented-out dead code are both essentially
absent (see §5). This is not a codebase short on comments; it's one where the
comments that exist aren't consistently governed.

---

## 2. House style by dialect

**Rust `///`/`//!` doc comments** carry the architecture, not just the API
surface. Every crate root states its own boundary in `//!`: `vidiotic-core`'s
`lib.rs` opens with "nothing in it may touch wgpu, winit, egui, cpal, or a
window server"; `vidiotic-play`'s says the crate "names no filesystem, no
ffmpeg, no audio device, and no socket, so it crosses to
`wasm32-unknown-unknown` intact"; `vidiotic-wire`'s says it "must never depend
on `vidiotic`". These are invariants a compiler can't enforce, stated where a
future editor will actually read them. Model these when writing a new crate
root or a public item: state the constraint, not a restatement of the
signature.

**Rust `//` rationale comments** explain why the obvious alternative was
rejected, with numbers where there are numbers. `Cargo.toml:14-24` is the
clearest example even though it's TOML, not Rust: it justifies
`debug = "line-tables-only"` with a measured rlib size (68 MB vs 25 MB) rather
than asserting the setting is good. `vidiotic-core/src/bundle.rs:33-38`
explains why `zip` stores instead of deflating: the payload is already
snappy-compressed, so deflate would spend CPU for a smaller win than it looks
like on paper. A `//` comment that only restates the next line's syntax is
not this. A `//` comment that says why the next line isn't what a reader
would guess, is.

**`SAFETY:` comments** are mandatory on every `unsafe` block, no exceptions.
`vidiotic/src/video/capture.rs` and `decoder.rs` follow this without a single
gap: `// SAFETY: reading a framework string constant.`,
`// SAFETY: the handler block is retained by AVFoundation for the
duration...`, one line per `unsafe`, stating the precondition that makes it
sound. §4 lists where this convention isn't followed yet.

**ISF shader header comments** (`vidiotic-play/shaders/vidvox/*.fs`) are a
different grammar entirely: a `/* {...} */` block holding structured JSON
(`CATEGORIES`, `DESCRIPTION`, `INPUTS`). This is metadata the ISF runtime
parses, not prose for a human reader, so it doesn't answer to the "why, not
what" bar above. Its correctness bar is its own: valid JSON, an `INPUTS` list
that actually matches the shader's uniforms.

**Shell script `#` comments** hold to the same "why, not what" bar as Rust
`//` comments, just more sparingly, matching the shorter scripts they sit in.

**JS comments** in `web/boot.js` and `web/chop.js` read like the Rust `//`
style transplanted whole: `boot.js:1-20` explains why the boot code is a
separate file rather than an inline `<script>` (the deploy CSP has no
`unsafe-inline`) and what that forces about how `scripts/release-play.sh`
hashes and ships it. `chop.js:19-22` ties a JS constant to a Rust one by name
(`FPS` must match `web::ASSUMED_FPS`) so the coupling is visible instead of
implicit.

---

## 3. The provenance-comment rule, reconciled

`vidiotic/docs/egui-elegance-plan.md:42` states, as a ground rule for that
one refactor: "Comment standards: doc comments on all public items; no
point-in-time, milestone, or provenance comments." Read literally and
workspace-wide, that rule is wrong: it was written for a single phased plan
(don't leave "Phase 3 added this" markers scattered through UI code) and
doesn't hold up against comments elsewhere that use past tense to state a
real invariant.

Checked nine comments that use "used to" / "previously" phrasing outside that
one plan:

| Location | What it says | Verdict |
|---|---|---|
| `vidiotic-core/src/project.rs:490` | Dropping `canonicalize` was a fix, not a regression: only one side of a `strip_prefix` was ever canonicalized, so symlinked project dirs broke silently | Keep. The "not a concession" framing is the point |
| `vidiotic-bake/src/transcode.rs:84` | Prefer `r_frame_rate` over `avg_frame_rate`; the old path's unconditional `avg_frame_rate` inherited duration errors from a known zero-duration final sample | Keep. Names the exact bug it prevents recurring |
| `vidiotic-core/src/bank.rs:48` | The cap used to be defined twice, once per capture backend, which made a model constant something two platform files had to agree on by hand | Keep. States why it must stay single-sourced |
| `vidiotic-prep/src/app.rs:211` | `open_video_then` carries a guard that `finish_open_project` used to carry inline | Keep, but weak. Reword to state the guard's purpose rather than lean on the history |
| `vidiotic-prep/src/app.rs:757` | Same guard, referenced again from a test name | Same as above |
| `phosphor/src/theme.rs:321` | `row()` is a function now, not the constant it used to be, because it's a property of the face | Keep. Short, states the actual invariant (face-dependent) |
| `phosphor/src/theme.rs:617` | A regression test: the old icon codepoints must render as nothing now, or a font that once added 2.44 MB to the bundle is silently linked back in | Keep. Quantified regression guard, exactly the case this rule should protect |
| `vidiotic-play/src/commands.rs:162` | `BpmDigit` deliberately excluded from `repeats_on_hold`: a held `1` used to spam "11111" into the entry | Keep. The history is the whole reason for the exclusion |
| `vidiotic-chop/src/commands.rs:117` | `then` used to be two separate command variants (`OpenFollowup`/`SpanFollowup`), now unified | Keep, but weak. The mechanism description that follows carries the real content; the naming history could be cut |

Two other hits from the same keyword search turned out not to be provenance
comments at all: `vidiotic-prep/src/app.rs:278` ("every source previously
autosaved this session") describes runtime state, not code history, and
`phosphor/src/theme.rs:640` matched on "Legacy Computing block", a Unicode
block name, not the word "legacy". Worth noting because it means the
egui-elegance-plan rule, taken as workspace policy, would have flagged
comments that were never provenance comments in the first place.

**Amended rule**, to replace the line-42 wording for anything outside that
one plan doc:

- **Banned**: a comment that states only that behavior changed, with nothing
  a future editor needs to know to avoid breaking it again. That belongs in
  the commit message, not the source.
- **Keep**: past tense used to state a non-obvious invariant, a rejected
  alternative, or why something that looks wrong on first read, isn't.
- **Litmus test**: delete the historical framing and reread the comment. If
  it still conveys the same warning, reword it to drop the history. If it
  doesn't, the history *is* the invariant, so keep it.

By that test, seven of the nine above are clean keeps as written; two
(`app.rs:211`, `app.rs:757`) would read just as well with the "used to"
clause cut, since the guard's current purpose is what matters, not that it
moved.

---

## 4. Punch list

**Fix: stale cross-reference.** `vidiotic/src/app/keys.rs:16-19` cites
`vidiotic-prep/UNDO_TODO.md` as tracking an undo/redo rebinding decision.
That file doesn't exist anywhere in the tree. The real content now lives at
`vidiotic-prep/docs/undo.md`. Either retarget the comment to that file or, if
the decision it refers to has since been made, restate the current answer
inline instead of pointing at a decision doc at all.

**Fix: missing `SAFETY:` comments.** The `SAFETY:` convention followed
throughout `vidiotic/src/video/capture.rs` and `decoder.rs` (§2) isn't
followed in `vidiotic-bake/tests/`, where four `unsafe` blocks read raw
FFmpeg struct fields with no safety comment at all:

- `vidiotic-bake/tests/gen_fixtures.rs:73`
- `vidiotic-bake/tests/mov_roundtrip.rs:94`
- `vidiotic-bake/tests/mov_roundtrip.rs:142`
- `vidiotic-bake/tests/mov_demux.rs:98`

Each reads a codec-parameter pointer that `ffmpeg-next`'s safe API doesn't
expose. That's a real precondition (`params`/`par` must be a valid,
live-for-the-duration pointer) worth stating the same way `capture.rs` does,
not skipped because it's test code.

**Close the gap: missing `pub fn` doc comments.** 68 public functions have
no `///` (§1). Don't work this list top to bottom; start with
`vidiotic-ctl`, where the *rate* is worst (37%, not just the raw count).
Representative examples, one crate at a time:

- `vidiotic-ctl/src/app.rs:45,77,89,94,108`: `new`, `save`, `save_as`,
  `revert`, `open`
- `vidiotic-play/src/clip.rs:120,125,130`: `width`, `height`, `frame_count`
- `vidiotic-play/src/clock.rs:70,176,301`: three separate `new`s across
  different clock types, exactly the kind of function where the doc comment
  needs to say which clock this is and why it's a separate type
- `vidiotic/src/app/mod.rs:261`, `vidiotic/src/control_input.rs:143`:
  `App::new`, `project_map`
- `vidiotic-prep/src/control_input.rs:193,237,258,281`: `start_learn`,
  `remove_project_binding`, `remove_prep_binding`, `mark_dirty`
- `phosphor/src/theme.rs:228`: `set_state`, the one gap in an otherwise
  fully-documented crate

Rerun the script in the appendix for the full, current list; don't copy this
one forward as new functions get added.

---

## 5. What's already fine, leave alone

- **TODO/FIXME/HACK markers**: essentially none in the tree. The handful of
  grep hits for "hack" are the `Hack` font name in `phosphor/src/theme.rs`
  and `vidiotic/examples/isf_aesthetics/`, not markers. Whatever process
  keeps this codebase at zero open TODOs, don't disturb it by encouraging
  people to leave more.
- **Commented-out dead code**: none found. Don't add a lint for this; there's
  nothing to catch.
- **Per-file license headers**: none exist, and none are needed. Licensing is
  centralized in the top-level `LICENSE` and `vidiotic/licenses/*.md`; adding
  per-file SPDX headers would be new ceremony this repo has deliberately not
  carried.

---

## Appendix: regeneration commands

Run from the repo root. All exclude `target/` and `dist/`.

```sh
# Non-blank Rust LOC
find . -name '*.rs' -not -path './target/*' -not -path './dist/*' \
  | xargs grep -cve '^\s*$' | awk -F: '{sum+=$2} END{print sum}'

# Rust doc comment lines (///, //!)
find . -name '*.rs' -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 grep -E '^\s*(///|//!)' | wc -l

# Rust non-doc // comment lines
find . -name '*.rs' -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 grep -E '^\s*//[^/!]' | wc -l

# Rust /* */ block comment occurrences
find . -name '*.rs' -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 grep -c '/\*' | awk -F: '{sum+=$2} END{print sum+0}'

# Shell # comments
find . -name '*.sh' -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 grep -cE '^\s*#' | awk -F: '{sum+=$2} END{print sum+0}'

# JS // comments in web/
find web -name '*.js' | xargs grep -cE '^\s*//' | awk -F: '{sum+=$2} END{print sum+0}'

# Shader comment lines
find . \( -name '*.fs' -o -name '*.frag' -o -name '*.vert' -o -name '*.wgsl' \) \
  -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 grep -cE '//|/\*|\*/' | awk -F: '{sum+=$2} END{print sum+0}'
```

`pub fn` doc coverage needs more than grep can do alone: it has to find the
nearest non-blank, non-attribute line above each `pub fn` and check whether
it starts with `///` or `//!`, skipping over `#[...]` attribute lines in
between. A short Node script does it:

```js
const fs = require('fs'), path = require('path');
function walk(dir, out) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (['target', 'dist', '.git'].includes(e.name)) continue;
    const full = path.join(dir, e.name);
    if (e.isDirectory()) walk(full, out);
    else if (e.name.endsWith('.rs')) out.push(full);
  }
}
const files = []; walk('.', files);
let total = 0, missing = 0;
for (const file of files) {
  const lines = fs.readFileSync(file, 'utf8').split('\n');
  for (let i = 0; i < lines.length; i++) {
    if (!/^\s*pub\s+(async\s+)?fn\s+\w+/.test(lines[i])) continue;
    total++;
    let j = i - 1, hasDoc = false;
    while (j >= 0) {
      const s = lines[j].trim();
      if (s.startsWith('///') || s.startsWith('//!')) { hasDoc = true; break; }
      if (s.startsWith('#[') || s === '') { j--; continue; }
      break;
    }
    if (!hasDoc) { missing++; console.log(`${file}:${i + 1}`); }
  }
}
console.log(`${missing}/${total} missing`);
```
