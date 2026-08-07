# Undo — decisions and open questions

Session-scoped document undo shipped for `vidiotic-prep` (see `src/undo.rs`).
Whole-document snapshot stack at the command choke point (`drain_commands`),
depth-capped at 256, with gesture coalescing. What follows is the record of
what was decided and what is still open.

## 1. Undo/redo is an unbindable magic chord — **accepted, not a bug**

Cmd/Ctrl+Z (and Shift / `y` for redo) are hardcoded in
`PrepApp::handle_ctl_event`, intercepted **before** the `Mapper`. Every other
prep input routes through `Mapper` → `PrepVerb` and is rebindable; undo/redo
are the lone exception.

> `vidiotic-ctl` and `vidiotic` took the same hardcoded-chord approach
> (`CtlApp::handle_history_keys`; the player's `App::handle_key` intercept before
> the grammar/mapper). All three apps are consistent.

**Decision: leave it hardcoded.** This is a knowing exception, not an oversight.
The cost is real and stated here so nobody has to rediscover it:

- The user can't rebind the chord, and can't reach undo from MIDI or gamepad.
- It contradicts the "every key is a plain source→verb pair" design that
  `control_input.rs` otherwise holds to.

Accepted anyway, because every way of fixing it is worse than the wound:

- Routing through the mapper means `PrepVerb::Undo`/`Redo` in
  `vidiotic-ctl::model` and a `MAP_VERSION` bump (currently 2) — putting an
  app-meta operation into a vocabulary that is otherwise all *document* verbs,
  shared with the player. The concept doesn't fit the type.
- A reserved chord deliberately sidesteps the mapper's "any match in the upper
  layer wins" arbitration. Undo that can be shadowed by a layer is worse than
  undo that can't be rebound: the failure is silent and arrives exactly when
  the user is trying to recover from a mistake.
- The third option — a separate non-document "app command" binding layer — is
  a whole second mapper to make one chord configurable.

Cmd/Ctrl+Z is also the one binding on the machine that nobody wants moved. If
this is revisited, the trigger is a real user asking for undo on a foot pedal,
not the inconsistency on its own.

Related, and also accepted: while an egui `TextEdit` (span rename) has focus,
`pump_controls` gates keys off and egui's own inline text-undo eats Cmd+Z, so
document undo only fires with no field focused. That is the behaviour we want —
mid-rename, Cmd+Z should undo typing, not the document edit before it.

## 2. Snapshot → optic/patch migration path (if the document grows)

Current impl clones the whole `Doc` (spans + bank names + defaults) per edit.
Fine while the document is tens of spans (~10–20 KB/snapshot, capped stack =
single-digit MB). If the document format grows past that being feasible:

- The `PrepApp::snapshot`/`restore` pair is the seam — swap its internals for
  a focused/patch representation without touching callers.
- The design we discussed: a command acts through a lens; the undo entry is
  `(lens, f⁻¹)`. Defunctionalized for Rust, that's `apply_command` returning
  an **inverse command** (an "anti-command"): `MoveSpanUp(i)` →
  `MoveSpanDown(i-1)`, `SetSpanName(i,new)` → `SetSpanName(i,old_before_image)`.
  True `f⁻¹` for structural edits, before-image for `set`s. Costs a per-command
  inverse (must stay correct as commands evolve) but gives minimal memory + a
  semantic history log. Index-based lenses are safe under **linear** undo
  (LIFO replay hits the exact shape each anti-command was computed against);
  they'd need stable span IDs only for selective/non-linear undo.
- One cheap middle step without going full-optic: hold `Arc<Span>` (or
  `source: Arc<Path>`) in `SpanList` so snapshots become pointer-bump clones.

## 3. Replicate to the other apps

"All the apps need undo." Prep is the proof-of-pattern. Status:

- `vidiotic-ctl` — **done** (`vidiotic-ctl/src/undo.rs`). No command choke point,
  so it snapshots by diffing `map` against a `baseline` at each frame boundary
  (`CtlApp::commit_undo`) rather than wrapping a command; one frame = one step,
  committing deferred during learn. Generic `History<ControlMap>`, depth-capped.
- `vidiotic` — **done** (`vidiotic/src/undo.rs`). Turned out single-threaded
  after all: the winit loop drains `cmd_rx` and applies inline in `App::update`,
  which is the choke point (the "engine publishes UiMirror" is an internal render
  snapshot, not a thread). Key decisions specific to the player:
  - **Scope narrowed to cue/bank authoring.** Undo covers the cue/bank subset of
    `app::mutates_project`; live/transport/nav/device commands (tap tempo, resets,
    sync, live-bank switch, selection, mode toggles, cameras) are excluded — the
    "can't un-show a frame" problem made concrete. `undo::classify` is the split.
  - **Live vs edit bank.** Snapshotting `banks` is safe because undoable edits
    only ever touch the edit bank; `restore_doc` clamps bank/selection indices
    and calls the existing `resync_live_if_editing` + `retain_decoders` to
    reconcile the sequencer/decoders with restored content.
  - **Targeted clip-BPM snapshot.** `SetClipBpm` mutates `self.clips`, which also
    holds camera clips added outside the undo path; the snapshot stores an id→bpm
    map and writes it back field-wise rather than cloning the whole pool.
  - **Boundaries reset history.** `LoadProject` / `SetClipDir` re-id the pool
    (epoch bump), so they drop the stack instead of being undoable.
  - Only the `undo` module is unit-tested — `App` needs a GPU/audio boot, so the
    `restore_doc` reconciliation path is covered by reasoning, not a test.

**All three apps now have session undo.** With §1 settled, the only open item
left is §2 — and it is contingent, not scheduled: it opens if a document
outgrows per-edit cloning.

## 4. Persistence

Explicitly session-only for now (in-memory, cleared on quit). Persisted /
cross-session undo was considered and rejected — reintroduces the
non-linear-history and cross-document hazards. If revisited, that's a much
bigger design (serialized history, versioning, the "undo across a document
load" ambiguity).
