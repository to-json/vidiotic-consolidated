# Harness Addendum: Scriptable IPC

## Tier: Heavy
Protocol definition + new workspace crate + render-loop surgery across three streams.

## Branch: plan/ipc (in-place on the real vidiotic repo — no worktree)
The worktree isolation model doesn't fit this layout: `/Users/j/code/loot/vidiotic`
is a cargo workspace but not a git repo; each member is its own repo; the parent
`Cargo.toml` (members list) is untracked. A vidiotic-only worktree can't build
(parent workspace rejects a non-member nested checkout). Decided with the user:
develop in-place on `plan/ipc`, `vidiotic-wire` as its own git repo.

## New crate: ../vidiotic-wire (own git repo, workspace member)

## Personas (no `become` tool available — lenses adopted directly at review)
- **Mara Bos** — Rust concurrency/atomics/lock design. Watches: the reader/writer
  threads, bounded-queue backpressure, no shared-state races with the render loop.
- **Rob Pike** — protocol design, simplicity, composition. Watches: envelope shape,
  is the vocabulary orthogonal, does JSON-lines stay dumb.
- **Joe Armstrong** — let-it-crash, process isolation. Watches: a bad/slow/dead
  client can never take down or stall the engine; connection failure is contained.

## Permissions in Effect
In-place execution; no settings.local.json scoping applied (that ceremony targets
worktree background-subagent auto-fail, which doesn't apply here). Standard tool
access. Never: `git push`, `git reset --hard`, edits to `.env`/`.claude/settings*`.

## Test Baseline
- vidiotic: 114 passed / 0 failed / 1 ignored (lib unittests)
- vidiotic-ctl: 54 passed / 0 failed
- Pre-existing clippy warning (NOT ours): `app.rs:2407` semicolon_if_nothing_returned — leave untouched.
- Test command: `cd /Users/j/code/loot/vidiotic && cargo test -p vidiotic` (cargo at
  /Users/j/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/cargo)

## Commit Sequence (batched at end; each green, each one intention)
Stack base: `b50f71b` (plan commit) on plan/ipc.
1. wire crate: protocol types + envelope + round-trip tests  *(vidiotic-wire repo)*
2. workspace wiring: parent members + vidiotic dep + Cargo.lock  *(vidiotic)*
3. mirror additions: UiMirror.project_path + ClipEntry duration/fps  *(vidiotic)*
4. origin-split apply_command (solicit prelude vs inner)  *(vidiotic)*
5. session epoch (bump on load_project / SetClipDir)  *(vidiotic)*
6. ipc.rs server + to_command + drift tripwire + tick integration  *(vidiotic)*
7. main.rs --ipc/--no-ipc flags + server spawn + socket lifecycle  *(vidiotic)*
8. docs/ipc.md + examples + wire `client` feature  *(both repos)*

The reviewable stack spans two repos plus one untracked parent-manifest edit;
that's inherent to the workspace layout, not a harness defect.

## Test Discipline
- MUST stay green: all 114 vidiotic lib tests + 54 ctl tests.
- New tests expected: wire round-trip catalog; ipc drift tripwire; to_command mapping.
- MAY break (acknowledged): none — this is purely additive.
- Run after each unit and before each commit.

## Escape Hatch
If the plan no longer matches reality, STOP, update docs/ipc-plan.md, tell the
user what diverged, ask before proceeding. (Already invoked once — for the
worktree/workspace mismatch.)
