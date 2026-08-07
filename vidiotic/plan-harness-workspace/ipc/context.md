# Plan Harness Context

## Plan: docs/ipc-plan.md
## Tier: Heavy
## Execution: in-place on branch plan/ipc (NO worktree — see addendum for why)
## Wire crate: ../vidiotic-wire (own git repo)
## Branch: plan/ipc
## Start Ref: b50f71b (plan commit)

## Review Personas (no become tool; lenses adopted directly)
- Mara Bos — Rust concurrency — threads/bounded queues/no render-loop races
- Rob Pike — protocol design — envelope simplicity, orthogonal vocabulary
- Joe Armstrong — failure isolation — bad/slow/dead client never stalls engine

## Permissions
In-place, standard tool access. No settings.local.json scoping (worktree-specific ceremony, N/A).

## Test Baseline
- vidiotic lib: 114 passed / 0 failed / 1 ignored
- vidiotic-ctl: 54 passed / 0 failed
- pre-existing clippy warning app.rs:2407 (not ours, leave it)
- cargo: /Users/j/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/cargo, run from /Users/j/code/loot/vidiotic

## Unit Sequence
- [ ] impl [ ] commit — 1 wire crate types+envelope+tests (vidiotic-wire) — DELEGATED, agent a5e6ae001e094f2eb running
- [x] impl [ ] commit — 2 workspace wiring (parent members + vidiotic dep) — DONE (edited, not committed)
- [x] impl [ ] commit — 3 mirror additions project_path + clip duration/fps — DONE, green
- [x] impl [ ] commit — 4 origin-split apply_command (apply_command wrapper + apply_command_inner) — DONE, green
- [x] impl [ ] commit — 5 session epoch (epoch: Arc<AtomicU64>, bump_epoch() in load_project + set_clip_dir) — DONE, green
- [ ] impl [ ] commit — 6 ipc.rs server + to_command + drift tripwire + tick integration (blocked by 1,3,4,5)
- [ ] impl [ ] commit — 7 main.rs flags + spawn (blocked by 6)
- [ ] impl [ ] commit — 8 docs + examples + client feature (blocked by 6)

## Unit File Map
- unit 2: /Users/j/code/loot/vidiotic/Cargo.toml (untracked parent), vidiotic/Cargo.toml, vidiotic/Cargo.lock
- unit 3: vidiotic/src/commands.rs (UiMirror, ClipEntry), vidiotic/src/app.rs (build_mirror)
- unit 4: vidiotic/src/app.rs (apply_command split)
- unit 5: vidiotic/src/app.rs (epoch field, load_project, set_clip_dir)
- unit 6: vidiotic/src/ipc.rs (new), vidiotic/src/lib.rs, vidiotic/src/app.rs (tick), vidiotic/src/commands.rs? no
- unit 7: vidiotic/src/main.rs
- unit 8: vidiotic/docs/ipc.md (new), vidiotic/examples/*.sh (new), vidiotic-wire/src (client feature)

## Current State
Phase: implementing. Setup done (branch, wire skeleton, workspace wired, addendum).
Wire types delegated to background agent. Doing engine units 3/4/5 inline next (disjoint from wire crate).
Key facts from probe: ClipMeta (project.rs:654) has duration_sec+fps already; UiMirror/ClipEntry never destructured (new fields safe); App fields private so tick code lives in app.rs, ipc.rs holds server only; Boot.cmd_tx is pub; clap-derive CLI, channel at main.rs:288, server spawns in run_player (main.rs:260-319); tick step 1 drain at app.rs:1519-1523, build_mirror step 8 at 1659; ClipEntry literal at app.rs:1762-1776, CueView at 1892; load_project next_cue_id reset at 513, project_path set at 552.

## Decisions Made
- WireCommand excludes exactly OpenProject + SaveProjectAs; includes SaveProject/OpenProjectEditor (engine-side gated).
- Cursor-relative commands included+documented (user confirmed).
- ok ack = dispatched; validate ids/indices/pathless-save at drain, else err.
- Session epoch bumped on load_project AND SetClipDir (both invalidate ids).
