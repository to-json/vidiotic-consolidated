# Scriptable IPC — every input surface is just another producer on `cmd_tx`

Date: 2026-07-19
Depends on: None

---

## Context

Every input surface (control UI, keys, grammar, pickers) already funnels into one
place: `Command` (`commands.rs:157`) over a crossbeam channel, drained by
`apply_command` (`app.rs:1362`), with read-only state republished as `UiMirror`
each tick (`build_mirror`, `app.rs:1659`). An IPC endpoint is another producer on
`cmd_tx` plus a read path off the mirror. The command pattern did the hard work
already.

What the substrate does *not* give us:

1. **No serializable vocabulary at Command granularity.** `Command` carries
   `Arc<str>`, `PathBuf`, `IsfValue` — no serde. The existing serialized
   vocabulary (`vidiotic_ctl::Action`, nanoserde) covers ~14 of 61 variants and
   doubles as the binding-editor catalog with physical-control semantics; growing
   it would pollute that UI. The wire vocabulary is a new thing.
2. **No way for a script to learn ids.** `ClipId`/`CueId`/`ShaderId` are runtime
   ids; queries are required even for a fire-and-forget feel.
3. **A dependency cycle constraint.** `vidiotic` depends on `vidiotic-ctl`, so
   ctl can never import protocol types from `vidiotic`. ctl is a named future
   client (live MIDI/pad bridge), so the protocol lives in a new leaf crate.

Interview decisions: unix socket + JSON lines now, architecture open to OSC and
TCP later; commands + queries now, open to push subscriptions later; clients are
scripts, ctl, prep, music tools, agents — generally, the UI must not be required
to drive vidiotic.

Decisions locked with the user (post bimodal review):

- Cursor-relative commands (`LoadIsf`, `NudgeCueParam`, `SetCueIn/OutToPlayhead`)
  are **included and documented** — "anything the UI can do" is the bar; a
  script that needs them drives `SelectCue` first and accepts that it moves the
  operator's selection. Explicit-target `Command` variants are a future engine
  change that won't break the wire.
- `ok` acks mean **dispatched**, not succeeded (`apply_command` fails by
  logging). Statically checkable failures (unknown ids, out-of-range indices,
  pathless save) are validated at the drain point and return `err`; everything
  else a script verifies by querying.
- Heavy commands (`LoadProject`, `SetClipDir`, shader compiles) run
  synchronously on the render thread and will hitch the output window; v1
  documents this, off-thread staging is a future plan.

Review findings verified against the code and folded in below: the implicit
save-picker latch in `apply_command`, inbound flow control, id reuse across
project reload, missing query data (`project_path`, clip durations), and the
non-issue status of `ToggleFullscreen` (nothing intercepts it; IPC commands
apply on the main thread where its window calls are safe — it ships in v1).

---

## Stream 1: `vidiotic-wire` — the protocol crate

**Problem**: No serializable, dependency-free vocabulary for the full command
surface.

**File(s)**: new crate `../vidiotic-wire/` (workspace member), deps: `nanoserde`
only.

### 1.1 Command types

`WireCommand` mirroring `Command` (61 variants; ~56 wire-able) with
wire-friendly payloads: `String` everywhere `Command` has `Arc<str>`/`PathBuf`
(nanoserde 0.2 has no impls for either — forced, not stylistic), `u32` ids.
Aux mirror types, all owned and monomorphized: `WireSlotRef` (4 variants),
`WireChainSlot`, `WireIsfValue` (5), `WireCueParam` (9, with `Toggle<i32>`/
`Toggle<f64>`/`Toggle<u32>` monomorphized as three concrete wire structs),
`WireCueParamKind`, `WireTimeSig`, `WireCadence`, `WireSyncKind`, `WireCamDelay`.

**Excluded** (`ExcludedInteractive`): the picker-opening variants —
`OpenProject`, `SaveProject`*, `SaveProjectAs`, `OpenProjectEditor`* — the wire
always carries explicit paths (`LoadProject`, `SaveProjectTo`).
(*) `SaveProject`/`OpenProjectEditor` are wire-able **gated**: they map through
only when a project path is known; pathless they return `err` instead of
opening a picker (see 2.3).

### 1.2 Query types and reply views

`WireQuery`: `Status`, `Transport`, `Pool`, `Cues`, `Shaders`, `Audio`,
`Levels` — selective; the serialization cost of the 512-bin spectrum is paid
only by `Levels` callers.

- `Status`: `project_path: Option<String>` (not currently in `UiMirror` — added
  in 2.5), session `epoch`, wire version, advanced/grammar flags.
- `Transport`: bpm, beat/phase/quantum, time sig, cadences, sync source, peers,
  can_set_tempo/phase.
- `Pool`: clip banks + active index, clips **with duration/fps** (from
  `clip_meta`, `app.rs:168` — not currently mirrored; added in 2.5; without it
  a script can't compute a sane `SetCueOut`), selected clip, **cameras**
  (uid/name/on_air/status/missing — uids are what `SetCameraOnAir`/
  `RelinkCamera` consume).
- `Cues`: cue banks, live/edit bank, full cue views (all `CueView` fields incl.
  chains and resolved speed), selected cue.
- `Shaders`: shader pool **including ISF input schemas** (`WireIsfInput` /
  `WireIsfInputKind`, 8 variants — required for scripted `SetChainParam`).
- `Audio`: devices, current, error. `Levels`: 21 bands + 512-bin spectrum + level.

Reply views mirror `ClipEntry`/`CueView`/`BankView`/`ClipBankView`/
`CameraEntry`/`ShaderPoolView` with ids included, `Arc<str> → String`.

### 1.3 Envelope

Newline-delimited JSON, one object per line. Request:
`{"id": u64, "epoch"?: u64, "req": Cmd(..) | Get(..)}`. Reply:
`{"id", "epoch", "ok": ...}` or `{"id", "epoch", "err": "..."}`.
**Every request gets exactly one reply** — command acks are the script's
barrier/flush primitive. `epoch` is the session generation (bumped on project
load, see 2.4); replies always carry it, requests optionally assert it. Push
frames (future subscriptions) carry no `id`, so adding `Sub(..)` later breaks
nothing. Server greets on connect with `{"vidiotic": {"wire": 1, "epoch": N}}`.

### 1.4 Tests

Exhaustive serialize→deserialize round-trip over a full `WireCommand`/
`WireQuery`/reply-view catalog (pattern: ctl's catalog test,
`vidiotic-ctl/src/model.rs:418`). Goldens for envelope shapes. Note: strict
JSON cannot carry NaN/Inf — engine-side clamps make well-typed hostile input
panic-free; assert that as a tested invariant where cheap.

---

## Stream 2: engine integration

**Problem**: A socket server whose clients can never stall or starve the render
loop — in either direction — with sane ordering semantics and no UI side
effects.

**File(s)**: new `src/ipc.rs`, surgery in `src/app.rs`, flags in `src/main.rs`.

### 2.1 Origin-gate the save-picker latch (the blocker)

`apply_command` opens a native save dialog *implicitly* before matching:
`app.rs:1366-1369` solicits a save path via `crate::ui::pick_file` whenever the
session is unsaved and `mutates_project(&cmd)` (`app.rs:113-137` — `AddCue`,
`SetCueIn`, `SetClipDir`, … exactly what scripts send). A script on a fresh
session would pop a modal NSSavePanel with nobody driving.

Split it: `apply_command_from_ui` = solicit prelude + `apply_command_inner`;
the UI-origin drain (`cmd_rx`, step 1) keeps today's behavior, the IPC drain
calls `apply_command_inner` directly. No other behavioral change.

### 2.2 Server and flow control

`UnixListener` at `--ipc <path>` (default on; default path
`$TMPDIR/vidiotic-<pid>.sock` plus a `vidiotic-latest.sock` symlink created via
temp-name + atomic `rename`, removed on exit only if it still points at self;
stale dead-pid sockets unlinked on startup; `--no-ipc` to disable). Accept
thread; per-connection reader and writer threads. All blocking I/O lives on
those threads — the engine only ever drains channels.

Flow control on **both** sides:

- Outbound: per-connection **bounded** outbox; overflow drops the connection —
  a non-reading client can never block the engine.
- Inbound: per-connection **bounded** queue into one `(ConnId, Envelope)`
  channel; when a connection's queue is full the reader **stops reading the
  socket** (natural backpressure). Per-tick drain cap (~32 requests) so a burst
  can't execute 10k commands inside one render tick; max line length (~1 MB) so
  a malformed line can't OOM the reader.

### 2.3 Tick integration — read-your-writes

In `update()`: drain the IPC queue alongside `cmd_rx` (step 1,
`app.rs:1519-1523`) — `WireCommand`s validate + translate + apply immediately
via `apply_command_inner`; queries are *parked*; after `build_mirror` (step 8,
`app.rs:1659`) answer parked queries from the fresh mirror. Per-connection FIFO
⇒ a script that sends `Cmd` then `Get` reads post-apply state. (Verified: the
clock snapshot is taken after command apply, so a `Transport` query sees a
same-tick `SetBpm`.)

Validation at the drain point, against current engine state, before apply —
these return `err` instead of silently no-oping: unknown `CueId`/`ClipId`/
`ShaderId`, out-of-range bank/slot indices, pathless `SaveProject`/
`OpenProjectEditor`, epoch mismatch (2.4). Clamps for the engine's blind spots:
`Cadence::Bars` (u32 overflow, `commands.rs:130`), non-finite/non-positive
`CueParam::Bpm`. Everything else: `ok` = dispatched; scripts verify via query.

Caveats to document (verified, not fixed): read-your-writes covers synchronous
state only — thumbnails, camera service status, decoder spawns complete async;
queries before the first tick see a `Default` mirror; parked queries whose
connection died are dropped.

### 2.4 Session epoch

Cue ids are **reused** across `LoadProject` (`next_cue_id` resets to loaded
max+1, `app.rs:513`) — a stale id can silently edit the wrong cue. Keep a
`u64` epoch on the app, bumped by `load_project` (and `SetClipDir`, which also
invalidates clip ids); stamp every reply and the greeting with it; a request
carrying `epoch` that doesn't match current returns `err` without applying.

### 2.5 Mirror additions

`UiMirror` gains `project_path: Option<Arc<str>>` and per-clip
`duration_sec`/`fps` on `ClipEntry` (from `clip_meta`). The wire view builder
destructures `UiMirror` **without `..`** so any future mirror field fails
compilation there instead of silently never reaching the wire (the struct-side
counterpart of the enum tripwire).

### 2.6 Translation + drift tripwire

`fn to_command(WireCommand) -> Command` in `ipc.rs`, exhaustive match.
Companion test: an exhaustive `match` over `Command` classifying every variant
`Wired | ExcludedInteractive` — a new `Command` variant is a compile error
here, so the wire can't silently drift.

---

## Stream 3: client ergonomics + docs

**Problem**: "Scriptable" is a claim tested by actually scripting it.

**File(s)**: `docs/ipc.md`, `examples/`, optional `client` feature in
`vidiotic-wire`.

### 3.1 Protocol doc

`docs/ipc.md`: envelope, every request with a copy-pasteable `nc -U` example,
the id-discovery pattern (query → pick id → command), the epoch rule, ordering
guarantees, ack semantics (`ok` = dispatched; verify via query), and the
documented sharp edges: cursor-relative commands move the operator's selection
and the playhead-snap pair no-ops unless the cue is playing
(`app.rs:1422-1433`); heavy commands hitch the render thread; camera commands
can trigger a TCC prompt (tmux-launched sessions auto-deny).

### 3.2 Example scripts

`examples/ipc-tap-tempo.sh`, `examples/ipc-load-and-go.sh` — `nc`/`jq` only,
proving the no-special-client claim.

### 3.3 Rust client helper (small)

`vidiotic-wire` feature `client`: blocking connect + `request(&mut self, Req)
-> Result<Reply>`, epoch tracking. The hook ctl/prep grab later; ~50 lines now,
saves a protocol reimplementation per sibling.

---

## Sequence integration

Independent of active work. Downstream unlocks: prep pushing edits into a
running player, ctl as a live bridge, recanon relinking a live session,
headless operation, and an OSC adapter that translates addresses to
`WireCommand` without touching the engine — `to_command` lives in vidiotic (the
`Action→Command` precedent, `control_input.rs:135`), so a new transport reuses
it wholesale; only the JSON-lines framing (~100 lines) is transport-specific.
Subscriptions later = a `Sub` request + push frames emitted from the
post-`build_mirror` point; the envelope (id-less push frames) and drain point
already accommodate them.

## Risks

- **Wire/Command drift** — enum tripwire (2.6) + no-`..` mirror destructure
  (2.5). Residual: docs/ipc.md drifts with nothing watching it.
- **Slow/hostile client stalls or starves engine** — bounded outboxes and
  inboxes, stop-reading backpressure, per-tick cap, max line length (2.2).
- **UI side effects from wire commands** — origin-gated solicit (2.1), picker
  variants excluded/gated (1.1); the residual risk is a future
  `mutates_project`-style implicit UI hook added to `apply_command_inner`.
- **Stale ids across project reload** — epoch (2.4) + drain-time id validation
  (2.3).
- **Render hitches from heavy commands** — documented v1 limitation; off-thread
  project staging is its own future plan.

## Followups (out of scope)

- Explicit-target `Command` variants for the cursor-relative surface.
- OSC adapter; TCP transport; push subscriptions.
- Off-thread `LoadProject`/`SetClipDir` staging with atomic swap.
- Name-based (non-id) addressing sugar.
