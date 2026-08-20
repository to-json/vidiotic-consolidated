# Comment style guide

What's actually in this tree's comments, where the written rule and the real
code disagree, and the specific spots worth fixing. Established by reading
the tree, not assumed. Every count below has its command in the appendix, so
it can be regenerated instead of taken on faith as this codebase grows.

Last measured 2026-08-20, after the grammar/keymap branch and this guide's own
first pass were merged. The numbers in §1 moved a lot in that merge and §4's
punch list closed out entirely, which is the argument for regenerating rather
than reading: this document is only as true as its last run.

---

## 1. Current state (measured)

51,142 non-blank lines of Rust across the nine workspace crates, plus a
`web/` JS shell and 81 shader files, 57 of them vendored ISF filters
(`vidiotic-play/shaders/vidvox/*.fs`).

| Dialect | Count |
|---|---|
| Rust `///`/`//!` doc comments | 8,015 lines |
| Rust `//` line comments (non-doc) | 2,036 lines |
| Rust `/* */` block comments | 23 occurrences |
| Shell `#` comments | 733 lines |
| JS `//` comments (`web/*.js`) | 416 lines |
| Shader comments (`.fs`/`.frag`/`.vert`/`.wgsl`) | 707 lines |

Public API coverage: **605 bare `pub fn` items** (not counting `pub(crate)` /
`pub(super)`, which aren't really public API), **all of them documented**.

| Crate | `pub fn` | missing doc |
|---|---|---|
| vidiotic-play | 163 | 0 |
| vidiotic | 105 | 0 |
| vidiotic-core | 77 | 0 |
| vidiotic-chop | 71 | 0 |
| vidiotic-ctl | 54 | 0 |
| vidiotic-prep | 45 | 0 |
| phosphor | 41 | 0 |
| vidiotic-bake | 38 | 0 |
| vidiotic-wire | 11 | 0 |

The previous run of this document found 68 missing across 590 items, with
`vidiotic-ctl` the outlier at 37%. Those closed in the grammar/keymap branch
and the passes after it. Holding at zero is now the bar; the appendix script
is what checks it.

All 16 `unsafe` blocks carry a `SAFETY:` comment. TODO/FIXME/HACK markers and
commented-out dead code are both absent (see §5). This is not a codebase short
on comments; it is one whose comments are now governed, and the job is keeping
them that way.

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

**`# Errors` sections** — 82 of them — lead with the condition, never with a
restatement of the return type. `Returns a JS error if the shell has not
booted` spends its first four words re-reading `-> Result<(), JsValue>` off
the signature the reader is already looking at. Two forms, and which one to
use is decided by whether the error type carries information:

- **Bare conditional**, where it does not — `anyhow::Result`, `io::Result`,
  `JsValue`, `String`. `/// If the file cannot be read, if the RON does not
  parse, or if the file was written by a newer format version.`
- **Type-first**, where it does. `/// [`MovErr`] if the box tree is malformed,
  a required box is absent, the sample tables disagree, or a sample's byte
  range escapes the file.` The link is the first thing on the line because
  the type *is* the first thing worth knowing.

A section that only cross-references another item's — `/// See
[`run_span_with`].`, `/// As [`Self::new`], except that missing BC is a
warning` — is a third construction, not a third voice, and is fine as is.
Banned openers: `Returns`, `Propagates`, `Fails`. The appendix has the census
command; the tree currently holds 58 bare-conditional, 19 type-first, 5
cross-reference, and 0 in the banned forms.

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
one plan. Anchored to the item each sits above rather than to a line number,
because line numbers are exactly what rots — the first run of this table cited
nine, and after one refactor branch seven of them pointed at the wrong line:

| Location | What it says | Verdict |
|---|---|---|
| `vidiotic-core/src/project.rs` — `absolutize` | Dropping `canonicalize` was a fix, not a regression: only one side of a `strip_prefix` was ever canonicalized, so symlinked project dirs broke silently | Keep. The "not a concession" framing is the point |
| `vidiotic-bake/src/transcode.rs` — module header, and `pick_fps` | Prefer `r_frame_rate` over `avg_frame_rate`; the old path's unconditional `avg_frame_rate` inherited duration errors from a known zero-duration final sample | Keep. Names the exact bug it prevents recurring |
| `vidiotic-core/src/bank.rs` — `DELAY_CAP` | The cap used to be defined twice, once per capture backend, which made a model constant something two platform files had to agree on by hand | Keep. States why it must stay single-sourced |
| `vidiotic-prep/src/app.rs` — `open_video_gated` | Carried a guard that another function "used to carry inline" | **Closed.** The clause is gone and the doc states the guard's purpose directly, which is what this table asked for |
| `vidiotic-prep/src/app.rs` — the same guard's test | Same | **Closed** with it |
| `phosphor/src/theme.rs` — `row()` | A function now, not the constant it used to be, because it's a property of the face | Keep. Short, states the actual invariant (face-dependent) |
| `phosphor/src/theme.rs` — the nerd-font regression test | The old icon codepoints must render as nothing now, or a font that once added 2.44 MB to the bundle is silently linked back in | Keep. Quantified regression guard, exactly the case this rule should protect |
| `vidiotic-play/src/commands.rs` — `repeats_on_hold` | `BpmDigit` deliberately excluded: a held `1` used to spam "11111" into the entry | Keep. The history is the whole reason for the exclusion |
| `vidiotic-chop/src/commands.rs` — `then` | Used to be two separate command variants (`OpenFollowup`/`SpanFollowup`), now unified | Keep, but weak. The mechanism description that follows carries the real content; the naming history could be cut |

These nine were a sample, not the census. The tree now has past-tense phrasing
in 34 files (appendix), most of it added by the same branch that closed the two
above. Re-run the search before trusting this table as coverage.

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

By that test, seven of the nine above are clean keeps as written. The two that
were not — both in `prep/src/app.rs`, both leaning on the history of a guard
whose current purpose was the only thing that mattered — have since had the
clause cut, which is the test working as intended.

---

## 4. Punch list

Everything the first run of this list named is done, so what follows is the
record of what closed and the one thing that replaced it.

**Closed: stale cross-reference.** `vidiotic/src/app/keys.rs` cited a
`vidiotic-prep/UNDO_TODO.md` that did not exist anywhere in the tree. It now
points at `vidiotic-prep/docs/undo.md §1`, which does.

**Closed: missing `SAFETY:` comments.** The four `unsafe` blocks in
`vidiotic-bake/tests/` that read raw FFmpeg struct fields with no safety
comment now each state the precondition — the pointer came from
`stream.parameters()` and is live as long as the stream, and the read is a
read. All 16 `unsafe` blocks in the tree are covered; the appendix has the
check.

**Closed: missing `pub fn` doc comments.** 68 of 590, worst in `vidiotic-ctl`
at 37%. Now 0 of 605 (§1).

**Open: nothing enforces any of this.** CI runs fmt, clippy, and the suites.
It did not run rustdoc, and the cost of that showed up the first time anyone
looked: `cargo doc --workspace -D warnings` could not document five of the
nine crates, across 21 broken intra-doc links. Some were simply wrong
(`Engine::dispatch` had been renamed `apply_command`; `vidiotic/src/ui/mod.rs`
described its panels as local modules three lines above the comment explaining
they had moved to `vidiotic-play`), and nothing said so, because nothing
looked. A `Docs` step now runs it with `-D warnings`, which is what keeps §2's
intra-doc links from being decorative.

The two conventions this document adds that a tool *cannot* check — "why, not
what", and the §3 litmus test — stay a matter of review. That is the point of
writing them down.

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

`# Errors` lead-in census (§2). Prints the opening word of every errors
section; `Returns`, `Propagates`, and `Fails` are the banned three, and should
come back empty.

```sh
find . -name '*.rs' -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 awk '/^[[:space:]]*\/\/\/ # Errors$/ {want=1; next}
                  want && /^[[:space:]]*\/\/\/[[:space:]]*$/ {next}
                  want {sub(/^[[:space:]]*\/\/\/[[:space:]]*/,""); print $1; want=0}' \
  | sort | uniq -c | sort -rn
```

`unsafe` blocks without a `SAFETY:` comment above them (§2, §4). Should be
empty; `just doc` does not check this, review does.

```sh
find . -name '*.rs' -not -path './target/*' -not -path './dist/*' -print0 \
  | xargs -0 awk '/^[[:space:]]*\/\// { if ($0 ~ /SAFETY/) safe=1; next }
                  /unsafe[[:space:]]*\{/ { if (!safe) print FILENAME":"FNR }
                  { safe=0 }'
```

Past-tense phrasing, for the §3 audit. This finds candidates, not defects —
each hit still has to go through the litmus test by hand.

```sh
grep -rn 'used to\|previously\|Previously' --include='*.rs' . \
  | grep -v './target' | sed 's|:.*||' | sort | uniq -c | sort -rn
```

`pub fn` doc coverage needs more than grep can do alone: it has to find the
nearest non-blank, non-attribute line above each `pub fn` and check whether
it starts with `///` or `//!`, skipping over `#[...]` attribute lines in
between — including multi-line ones, which the first version of this script
got wrong and reported as false positives. A short Node script does it:

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
    let j = i - 1, hasDoc = false, depth = 0;
    while (j >= 0) {
      const s = lines[j].trim();
      // Inside a multi-line attribute — `#[allow(\n  lint,\n)]` — keep walking
      // until the brackets balance. Without this the closing `)]` reads as
      // ordinary code and the item is reported as undocumented when it isn't.
      const close = (s.match(/[)\]]/g) || []).length;
      const open = (s.match(/[([]/g) || []).length;
      if (depth > 0) { depth += close - open; j--; continue; }
      if (s.startsWith('///') || s.startsWith('//!')) { hasDoc = true; break; }
      if (s === '' || s.startsWith('#[')) { j--; continue; }
      if (s.endsWith(']')) { depth = close - open; if (depth > 0) { j--; continue; } }
      break;
    }
    if (!hasDoc) { missing++; console.log(`${file}:${i + 1}`); }
  }
}
console.log(`${missing}/${total} missing`);
```
