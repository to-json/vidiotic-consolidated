# Quality review — findings and work plan

A full code/style review of the workspace (~48k LOC, 9 crates) plus build,
packaging, and docs. Findings are grouped into work sets ordered by leverage:
correctness bugs first, then structural debt, then doc rot. Each item carries
its file:line evidence and a one-line notion of the fix. Nothing here has been
applied yet; this file is the plan.

Overall verdict first: the codebase is top-decile for a private project.
Comments explain *why* with measurements, tests are behavioral and
drift-guarded, there is no `unsafe` outside well-justified FFI blocks, and
`cargo clippy --workspace --all-targets` is nearly clean (~5 warnings:
`clone_on_copy` at `vidiotic/src/ipc.rs:582`, doc-markdown and
missing-errors-doc nits elsewhere). The items below are the exceptions, not
the rule.

Per-crate scores from the review: play 9, wire 9, core 8.5, bake 7.5 (one
high-severity parser bug), ctl 8, prep 8, vidiotic 8, phosphor 8,
build/docs 8.

---

## Set A — correctness bugs (fix first, each wants its own commit)

### A1. HAP offset table never length-validated — `vidiotic-bake/src/hap.rs:203-214`

`decode_chunked` derives `chunk_count` from `sizes.len()/4` and checks
`compressors.len()` against it, but never checks `offsets.len()`. A crafted
packet with N chunks and a shorter offset table panics with
slice-index-out-of-bounds at `off[i*4]` — in release too. This is a parser of
untrusted container data; every other table in the module is validated, this
one was missed. Fix: return `HapErr::BadChunkTables`.

Related, same file: `hdr + len` / `start + end` arithmetic on `usize` from
`u32` payload lengths (`hap.rs:130, 161-162, 174, 181, 255`) overflows on
wasm32 for adversarial length words near `u32::MAX` — debug panics, release
wraps before failing via `get()`. wasm32 is a first-class target for this
crate per its own Cargo.toml docs. Fix: `checked_add`, as `mov.rs:710`
already does.

### A2. Deploy prune deletes the sibling live bundle — `scripts/deploy-play.sh:66, 150-168`

`--prune` finds bundles with `find … -name 'pkg-*' | head -1`, which
ambiguously matches both `pkg-play-*` and `pkg-chop-*` under the now-primary
`dist/web` layout, then deletes every other `pkg-*` on the server — i.e. the
live sibling page's bundle. The prune logic predates the two-bundle release.
Fix: match per-app prefixes (`pkg-play-*` / `pkg-chop-*`) explicitly.

### A3. Export-spawn failure permanently wedges prep — `vidiotic-prep/src/export.rs:61-79` + `src/app.rs:468-498, 574-576`

`spawn_export` swallows thread-spawn failure with `.ok()`, and `poll_export`'s
`while let Ok(msg) = rx.try_recv()` treats `Disconnected` as "keep waiting".
If the worker never starts, `export_rx` is never cleared, `exporting()` stays
`true` forever, and quit is vetoed with "export in progress — please wait".
Permanent app wedge. The correct pattern already exists in the same file:
`poll_engine` (`app.rs:541-552`) maps `Disconnected` to "reload worker died".
Mirror it.

### A4. Ring-buffer mutex held across YUV→RGBA conversion — `vidiotic/src/video/capture.rs:460-487`

`CameraTap::poll` holds the ring `state` mutex for the full `scaler.run`
conversion (the guard lives as long as `picked`, which borrows from it). The
capture worker's `push` (`capture.rs:631`) and every other tap on the same
device stall for the duration of each conversion. Fix: copy the frame (or its
planes) out of the lock, then convert. Only finding in the review with
runtime cost on a live path.

### A5. `grammar::Token` public alias can panic — `vidiotic-play/src/grammar.rs:22`

`pub type Token = u8` permits out-of-range tokens; `roots[*root as usize]`
(`grammar.rs:193`), `entries[t as usize]` (`:212`), and `verbs.rs:30/38/68`
index 8-element arrays unchecked. An external `Token(9)` panics. Fix: newtype
or bounds check at the API edge.

### A6. Small correctness stragglers (batch as one commit)

- `vidiotic-chop/src/web/mod.rs:347-372` — `accept_frame` displays a stale
  out-of-order frame (sets `shown` even when `awaiting` doesn't match); if the
  page drops the awaited frame, `request_preview` stays gated forever.
  One-line guard.
- `phosphor/src/widgets.rs:458` — `fader` divides by `max - min` unguarded;
  `min == max` yields NaN. Clamp or reject.
- `vidiotic-play/src/web/mod.rs:1239` — `expect("requestAnimationFrame")`
  inside the per-frame loop; one rejected rAF (transient) kills the page via
  the panic hook. Degrade instead.
- `vidiotic-bake/src/bundle.rs:45-49, 88-90` (in core, see C5 below) — zip
  sizes downcast with `.unwrap_or(u32::MAX)` on overflow: a >4 GiB bundle
  silently writes a corrupt archive instead of erroring.

---

## Set B — structural debt (schedule; each is a session-sized chunk)

### B1. No CI at all — highest-leverage item in the repo

There is no `.github/` (or any other CI config). The repo's best tooling —
`scripts/wasm-gate.sh` (the ratchet), the smoke suites, `serve-play.sh
--check` (all explicitly designed as "the CI-shaped form") — never runs
automatically. Minimum viable CI: `cargo fmt --check`, clippy with the
workspace lint tier, `cargo test --workspace`, `scripts/wasm-gate.sh`, and
the native smoke suites where runners allow. Note the smoke scripts hardcode
a macOS Chrome path (`play-smoke.mjs:82`, `chop-smoke.mjs:62`) — a
`CHROME`-env/Linux fallback is a prerequisite for CI-hosted runs.

### B2. Not rustfmt-clean — 414 hunks, no fmt enforcement

`cargo fmt --check` reports 414 hunks; there is no `rustfmt.toml`, and the
Justfile has no `fmt`/`clippy`/`test`/`clean` recipes (`just build` also
hardwires release-wasm with no `--debug` passthrough). One mechanical
`cargo fmt` commit plus `just fmt-check` closes it. Decide first whether to
adopt `rustfmt.toml` matching the house ~110-col style or take rustfmt's
defaults wholesale — the latter is the lower-maintenance answer.

### B3. Triplicated egui key adapter + undo chord

The `egui::Event::Key → ControlSource` adapter and the Cmd/Ctrl+Z/Y undo
chord exist nearly verbatim three times, same explanatory comment included:
`vidiotic-chop/src/web/mod.rs:388-433`,
`vidiotic-prep/src/app.rs:615-639` + `control_input.rs:143-153`,
`vidiotic-ctl/src/app.rs:208-259`. The key-name *table* is correctly shared
in `vidiotic-ctl::keys`; the adapters around it are not. An egui-gated helper
(shared module or phosphor) collapses all three.

### B4. Script duplication and release-path drift

- `scripts/build-play.sh` / `build-chop.sh`: ~110 of 119 lines byte-identical.
  One parameterized `build-wasm.sh <crate>`.
- `release-web.sh` is a near-superset of `release-play.sh` with thinner
  comments; two live release paths for the same artifact. The compat sync at
  `release-web.sh:292-294` writes a `dist/play` layout that
  `serve-play.sh:80-81, 143, 205` then mis-reads (finds an arbitrary `pkg-*`,
  greps a `wasm_sha256` key that the new `version.json` doesn't have) — the
  rehearsal rig silently degrades. Either teach `serve-play.sh` the
  `dist/web` layout or drop the compat sync.
- `deploy-play.sh:71-74` — stamp check inspects only the first of
  `boot.js`/`chop.js` found; an unstamped second bundle passes the gate.

### B5. Duplicated `CropRect` — `vidiotic-core/src/project.rs` vs `vidiotic-bake/src/frame.rs:47-83`

~37 lines of geometry-clamping math duplicated verbatim; the crates can't
depend on each other by design, so at minimum cross-reference both doc
comments so a change to one flags the other. (Same pattern as the
`hsl()` duplication between `phosphor/src/theme.rs:342` and
`vidiotic/examples/isf_aesthetics/util.rs:128` — lower stakes, examples.)

### B6. Duplicated shell-command dispatch in play — `vidiotic-play/src/web/mod.rs:326-348` vs `:660-679`

The same `RefreshCameras / SetCameraOnAir / AddCameraCue / RelinkCamera /
SaveProject*` match is written twice in one file (`dispatch` and `build_ui`'s
drain loop). Extract a shared `shell_command` so a future command can't be
added to only one arm.

### B7. Hand-rolled `format!` JSON

Two sites build JSON by string interpolation with local `json_escape`
helpers: `vidiotic-play/src/web/mod.rs:1678-1788` (`engine_state`, a single
25-placeholder `format!` at `:1770`) and `vidiotic-chop/src/web/mod.rs:617-651,
808-844, 953-967`. Both are smoke-test surfaces, but a missed escape on one
interpolated field yields invalid JSON sent to the page — the most likely
silent-corruption sites in either crate. A tiny builder or struct-based emit
removes the escape/quote minefield.

### B8. Consolidation inside `vidiotic`

- `ui/mod.rs:184-251` — `pick_file`: six structurally identical
  dialog→thread→`block_on`→Command arms; one helper taking
  `impl FnOnce(FileHandle) -> Command` collapses ~70 lines.
- `video/decoder.rs:127, 258, 335` — three `#[allow(clippy::too_many_arguments)]`
  and duplicated seek/restart/in-out loop scaffolding between `run_hap` and
  `run_software` (`:273-301` vs `:390-452`); a `LoopCtx` struct removes both.
- `app/mod.rs:365-522` — `App::update` is ~157 lines; steps 6/6b/7
  (upload + chain + uniforms) are a natural extractable unit.

### B9. Test-coverage gaps (each small, batchable)

- `vidiotic-core/src/undo.rs` — `SnapshotHistory` (coalescing window, depth
  cap, redo invalidation) has zero tests; every other core module has them.
- `vidiotic-wire` `client` feature (`src/client.rs`) — zero tests; the
  framing/skip-unmatched-id logic is testable against an in-process
  `UnixListener` + fake greeting.
- `phosphor/src/widgets.rs` — the crate's public widgets (detent, fader,
  chip, segmented, statusline) have zero behavioral tests; only `wrap_unit`
  is tested. Contrast the exemplary theme tests. Also delete or graduate
  `repro_cadence_row` (`widgets.rs:819-869`) — scratch code that asserts
  nothing and runs 18 egui frames per `cargo test`.
- `vidiotic` pure helpers — `mutates_project`, `pick_monitor_from_window`,
  `dir_bank_name`, `wgpu_clear_color`, decoder `pace`/`take_restart`
  logic: all trivially testable, all untested.
- `vidiotic-ctl/src/mapper.rs` — nothing tests the `Pressed` +
  continuous-action combination (see D3, it's an active doc/behavior bug).
- `vidiotic-bake/tests/bake_timing.rs:14` — default `BAKE_SRC` is a
  hardcoded personal absolute path (`/Users/j/...`); make it relative like
  every other test.

---

## Set C — lower-priority structural notes (fold into other work)

- `vidiotic-core/src/clippool.rs:91-122` — `scan_from` swallows `read_dir`
  errors via `.into_iter().flatten().flatten()`; a typo'd `--clip-dir`
  produces an empty pool with no diagnostic. Also no tests for `scan`.
- `vidiotic-core/src/bundle.rs:45-49` — `#[must_use] Vec<u8>` zip API leaves
  no room to fail; see A6 for the overflow consequence.
- `vidiotic-core/src/isf.rs:622-637` — `detect()` collapses "malformed JSON
  header" into "not ISF" (`.ok()?`); authors get a far-away naga error
  instead of the real cause.
- `vidiotic-core/Cargo.toml:31` — `crossbeam-channel` unconditional but only
  used behind `#[cfg(feature = "ffmpeg")]`; make it optional.
- `vidiotic-core` id aliases (`chain.rs:14-17`, `bank.rs:12`) — `ClipId` /
  `CueId` / `ShaderId` are bare `u32` aliases; newtypes are free and catch a
  real mixup class.
- `phosphor/src/theme.rs:211, 312` — process-wide `CURRENT`/`METRICS`
  globals: two egui contexts in one process get last-sync-wins silently;
  document the constraint on `sync()` at minimum.
- `vidiotic-prep/src/shell_ui.rs:221` — "reveal" runs `open -R` (Finder)
  while the crate builds for Linux/Windows; those get a silent no-op button.
  cfg-gate or hide.
- `vidiotic-prep/src/app.rs:563` — export-dirty fingerprint via
  `format!("{:?}", spans)`; Debug output is an implicit serialization
  contract. `Span: Eq` or an explicit hash.
- `vidiotic-chop/src/session.rs:27, 33` — `SESSION_VERSION` written but never
  validated on parse (contrast `vidiotic-ctl::store`, which refuses newer
  map versions).
- `vidiotic-play/src/render.rs:726, 1434` — `ISF_UBO_ALIGN = 256` hardcodes
  wgpu's default limit instead of reading `device.limits()`; documented, and
  universally 256, but a device with larger alignment would silently
  misbind. One-line `max()` fix.
- `vidiotic-play/src/web/mod.rs:252-272` — `deliver_thumbnail` matches clips
  by display *name*; two same-named clips misroute thumbnails where ids
  exist for the purpose.
- Cross-crate keymap/adapter tests exist and are good; the `to_command`
  tables in `vidiotic/src/ipc.rs:395-471` are large but exhaustiveness-
  guarded by tests — acceptable as-is.

---

## Set D — doc rot and style (cheap; one or two commits)

### D1. Spliced/misattributed doc comments (refactor damage)

- `vidiotic-core/src/project.rs:132-176` — `CropRect` carries `ClipSpec`'s
  first three doc lines (ends mid-sentence); `ClipSpec`'s doc begins with the
  orphaned continuation. Bad merge; rustdoc garbled for both.
- `vidiotic-play/src/web/mod.rs:1356-1371` — `load_project` export wears
  `load_isf_source`'s doc; the actual `load_isf_source` export (`:1524-1525`)
  has none.
- `vidiotic-play/src/web/mod.rs:741-769` — `Shell::load_isf_source`'s doc
  swallows `project_snapshot`'s heading.
- `vidiotic-play/src/web/mod.rs:1129-1132` — garbled merged comment in `boot`
  ("…`vidiotic` and / Set the theme face…").

### D2. Stale docs and links

- `vidiotic-bake/src/transcode.rs:177-178, 221-222` — `# Panics` documents a
  muxer-stream read-back that `MovWriter` never does; doc rot on a public
  contract.
- `docs/ui-flows/00-README.md:36` and `docs/web-port.md:2610` both reference
  `docs/ipc.md`, which does not exist.
- `README` — two-line placeholder, no `.md` extension, no dev entry point
  (`just --list`, `wasm-gate.sh`, clips/Chrome prerequisites, worktree
  layout). The onboarding path exists; it's undocumented at the front door.
- `scripts/stage-fubar.sh:32` vs `:87` — header says "https is --port + 363",
  code computes `+ 463`.
- `vidiotic-chop/src/ui.rs:257-259` — comment points at
  `crate::control_input::default_map` (a prep module); chop's is
  `keymap::default_map`.
- `web/boot.js:9` — header credits `release-play.sh`; `release-web.sh` is
  now the canonical copier.

### D3. Doc/behavior contradictions (fix behavior or doc, add the missing test)

- `vidiotic-ctl/src/mapper.rs:20-21` vs `:73-75` — module doc says continuous
  actions never fire on `Pressed`/`Released`, but `resolve` returns
  `Some((action, 1.0))` on `Pressed`: a key bound to `SetBpm` snaps to max
  on press.
- `vidiotic-ctl/src/ui.rs:219` — `readonly_map` marks shadowing by exact
  `PartialEq` on the whole `ControlSource`, but resolve-time shadowing is
  `shape_eq` + fuzzy `device_tier` (`mapper.rs:103-172`); the "(shadowed)"
  indicator and mask suggestion can both be wrong for near-miss device
  names.

### D4. Style polish

- `phosphor/src/widgets.rs:309-340` — `media_tile` hardcodes point literals
  (`14.0`, `FontId::monospace(10.0)`, `Vec2::splat(3.0)`, `6.0`) in
  violation of the crate's own stated invariant ("written as multiples of
  `Metrics::cell`, never point literals", `theme.rs:233-239`). Also
  `section_label` at `widgets.rs:197` (`.size(10.0)` instead of
  `metrics().small`).
- `vidiotic-play/src/ui/editor.rs:99-168` — 70-line block inside
  `if !cue.camera { … }` not re-indented; redundant double guards at
  `:189` and `:598` inside already-`add_enabled_ui(!cue.camera)` scopes.
- `vidiotic-play/src/ui/command_palette.rs` — style outlier in `ui/` (block
  comments instead of `///`), ~40-item `Vec` rebuilt every frame while open,
  Mac-only shortcut labels on all platforms, and a "Remove Selected Cue"
  entry whose fallback sends `Command::SelectCueFirst` (`:173-177`).
- `vidiotic/src/app/mod.rs:568` — one 200-char line cramming window creation;
  split it. Same file `:592` — `ShaderWatcher::new(...).ok()` discards the
  error silently; one `log::warn!` fixes a confusing dead hot-reload.
- `vidiotic-chop/src/export.rs:416-417` — misplaced doc comment (describes a
  zip CRC test that lives in core); `:516-518` trailing double blank lines.
- `.gitignore` — missing `clips/` (untracked smoke fixtures make
  `git status --porcelain` dirty, which `stamp_of` records as `-dirty`
  builds) and `.DS_Store`.
- Leftover `Cargo.lock` + `.gitignore` inside `vidiotic-ctl/` and
  `vidiotic-prep/` crate dirs (standalone-build leftovers; chop/wire don't
  have them).

---

## Notably good (recorded so it isn't "fixed" away)

- Play's wasm/native split is enforced by the dependency graph — native-only
  deps mean misuse is a compile error, not a runtime hang — and the
  `Engine::apply_command → Option<Command>` protocol makes unimplemented
  shell features visible instead of swallowed. Both are tested.
- Wire's golden-shape and exhaustiveness-guard tests (`envelope.rs`,
  `command.rs`, `reply.rs`) make protocol drift a compile/test failure by
  construction. Ctl's `keys` module is the model for fixing a stringly-typed
  contract bug.
- Core's `Fs` trait + `MemFs`, RON migration ladder with per-version
  fixtures, and the ISF transpiler's swizzle-collision adversarial test.
- The deploy tooling: round-trip-verified precompression, atomic publish
  with rollback, nginx module probing before upload. Above production grade.
- The comment culture: profile settings with measured size numbers,
  panic-vs-log tradeoffs argued in place. The one thing every reviewer
  flagged independently.

## Suggested order

1. Set A (five commits, the last one a batch) — bugs with user-visible or
   security-adjacent consequences.
2. B1 + B2 — CI + fmt: makes every later change cheaper to trust. Needs a
   `rustfmt` decision (recommend: take defaults, one mechanical commit).
3. B3–B8 in descending convenience — each is session-sized and independent.
4. Set D as filler commits whenever touching the relevant files.
5. Set C opportunistically; none is urgent.
