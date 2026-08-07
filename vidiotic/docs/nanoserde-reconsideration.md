# Do we actually want nanoserde?

**Status:** open question, no action taken. Raised 2026-07-29 during a dependency
audit. This note exists so the question is written down with evidence rather than
re-litigated from memory later.

**Short answer:** probably yes, keep it — but for narrower reasons than the ones
`project.rs` currently gives, and with two failure modes worth knowing about.
The trigger to revisit is specific and named in [When to revisit](#when-to-revisit).

## Where it's load-bearing

| Crate | Format | What it carries |
|---|---|---|
| `vidiotic-core` | RON (`SerRon`/`DeRon`) | `.viproj` — the project save format |
| `vidiotic-core` | — | the ISF header tokenizer |
| `vidiotic-ctl` | RON | the persisted control map |
| `vidiotic-wire` | JSON (`SerJson`/`DeJson`) | the whole IPC protocol vocabulary |
| `vidiotic-prep` | both | session sidecar + wire client |

11 distinct symbols, 11 files, ~221 references. It is a genuinely heavily-used
dependency, which is why it is worth being deliberate about.

## What the audit checked, and what it found

Three plausible-sounding objections were tested directly. **All three were
wrong**, and they are recorded here so nobody re-raises them:

- **"Unbounded recursion on hostile input."** No. The JSON and RON paths are
  schema-directed — nesting depth is bounded at compile time by the target type,
  not by the input. The unknown-value skip (`whole_field`, `serde_json.rs:283`)
  uses an iterative bracket counter, not recursion. There is no stack-overflow
  DoS here.
- **"RON comments don't really work."** They do. Both `//` line comments and
  `/* */` block comments parse. `project.rs:12`'s stated rationale holds.
- **"No unknown-key rejection."** The RON path *does* reject them:
  `Unexpected key typo_here, line:6`. A typo in a hand-edited `.viproj` is a
  hard error, not a silent drop.

So the format that matters most — `.viproj` — is in good shape.

## The two things that are actually true

### 1. In JSON, unknown keys are silently ignored

The RON and JSON paths differ, and only RON is strict:

```
JSON  {"bmp":120.0}                      -> ERR  Key not found bpm        (required field, good)
JSON  {"bpm":120.0,"bogus":{...}}        -> OK   silently ignores bogus
RON   (..., typo_here: 3)                -> ERR  Unexpected key typo_here
```

The derive has the strict branch written and **commented out** — see
`nanoserde-derive-0.2.1/src/serde_json.rs:239-244`, `// TODO: maybe introduce
"exhaustive" attribute?`.

Combined with how pervasively this codebase uses `#[nserde(default)]`, the
failure mode is: **a typo on an optional field in an IPC command is accepted
silently and the field takes its default.** A script sending `{"SetTempo":
{"bpm":120,"rmap":0.5}}` gets `ramp: 0.0` and no error. `docs/ipc.md` is a
published protocol reference for script authors, so this is a real
bad-failure-mode issue, not a theoretical one — it is the same class of problem
as `docs/key-name-contract-bug.md` (silently does nothing, reads as "didn't
take").

Worth noting this is *arguably correct* for a wire protocol — forward
compatibility means an older server should tolerate keys a newer client sends.
The problem is that it is indistinguishable from a typo.

### 2. The format-migration story is untested

`project.rs:371` has a `migrate()`, and every step through it — v0→v4 — is a
no-op version bump ("nothing to fix up"). The format has never actually needed a
field renamed or retyped.

nanoserde has no `rename`, no `alias`, no `flatten`. So the first real migration
cannot be done with an attribute; it needs an old struct kept around and a
two-pass parse. That's tractable, but it is work that serde would have made a
one-line attribute, and it is unbudgeted because it has never come up.

## The case for keeping it

Strong, and it survived the audit intact:

- **2 crates vs 8.** Replacing it with `serde` + `serde_json` + `ron` adds a net
  8 crates not already in the graph (`serde`, `serde_core`, `serde_derive`,
  `serde_json`, `ron`, `base64`, `itoa`, `zmij`).
- **No `syn`.** `nanoserde-derive` hand-rolls its token parsing. serde's derive
  would put `syn` on the critical path of every one of these crates.
- **wasm-clean.** `docs/web-port.md` §1 already lists nanoserde under "ports
  untouched". It crosses to wasm32 today; the wasm gate depends on that
  (`vidiotic-wire --no-default-features|PASS|protocol types are pure nanoserde`).
- **Speed is genuinely irrelevant here** — `project.rs:10-13` is right that a
  `.viproj` is read once per open.

## The case against

- Last release **0.2.1, 2025-03-22** — 16 months at time of writing. 32 open
  issues, effectively one maintainer.
- The hand-rolled derive is the flip side of "no `syn`": it parses Rust syntax
  itself, so new syntax is its problem to keep up with.
- Error columns are always reported as `col:1` (the line number is correct).
  Minor, but it is friction in exactly the hand-editing workflow the format was
  chosen for.

## When to revisit

Concrete triggers, not vibes:

1. **The first real `.viproj` field rename or retype.** That is the moment the
   missing `#[serde(alias)]` costs actual work — compare the two-pass cost
   against the migration cost at that point.
2. **If strict IPC validation is wanted.** Fixing the silent-unknown-key
   behaviour means patching `nanoserde-derive` (the branch is already written,
   just commented out) or switching. Patching is a small fork of a 2-crate dep;
   that is the cheaper path and should be tried first.
3. **If nanoserde goes 24 months without a release**, or the derive breaks on a
   Rust edition bump.

Until one of those fires, this is not worth doing. The dependency is small,
portable, and — verified above, not assumed — correct where it matters most.

## Unrelated, found in the same pass

`docs/ipc.md` argues the socket's trust boundary from `$TMPDIR` being a
per-user `0700` directory. **That reasoning is macOS-specific.** On Linux
`$TMPDIR` is typically unset and `std::env::temp_dir()` returns `/tmp`
(mode `1777`, world-writable). Connecting to a Unix socket requires write
permission on the socket inode, so a default umask still keeps other local users
out — but the *stated* argument doesn't carry over, and the containing directory
being world-writable invites a different class of problem (a hostile local user
pre-creating or replacing paths). Worth re-deriving that section before the
Linux build ships.
