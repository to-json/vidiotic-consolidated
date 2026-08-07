# vidiotic-prep — model conformity plan

## Context

`vidiotic-prep` depends on the `vidiotic` crate by path and consumes **only**
`vidiotic::project` (the `.viproj` spec types + load/save) and
`vidiotic::transcode` (the bake path). It does **not** touch `render`/`shader`,
so render-side model changes — the effect-chain's `prev()` helper, the pass
list, the built-in registry, `ChainSlot`/`SlotRef` — are invisible to prep.

What *does* reach prep is any change to the serialized `project` spec structs.
The effect-chain plan (`../vidiotic/docs/effect-chain-plan.md`, §4 / FR2 / FR2a)
adds a **`chain` field to `CueSpec`**. This document keeps prep conformant with
that change and establishes a policy so future spec changes don't silently break
prep's build or quietly corrupt round-tripped projects.

## Where prep is coupled to the model

Two coupling surfaces:

1. **Struct-literal construction** (breaks the build on any field add). prep
   builds these spec types as full literals with no `..Default::default()`:
   - `CueSpec` — `src/export.rs:20` (`full_length_cue`)
   - `ClipSpec` — `src/export.rs:202`
   - `SpanProvenance` — `src/export.rs:210`
   - `ClipBankSpec` — `src/export.rs:223`
   - `CueBankSpec` — `src/export.rs:233`
   - `Project` — `src/export.rs:244`
   - `SessionDefaults` — `src/app.rs:84` (already uses `..Default::default()`)

2. **Round-trip / reopen** (`src/session.rs:reopen_project`). Loads a `.viproj`
   for retrimming, reconstructs **only** spans from each clip's
   `SpanProvenance`, and **discards cue banks entirely**. Anything authored on a
   cue downstream in `vidiotic` (including an effect chain) is lost when a project
   is reopened in prep and re-exported.

## Required changes for the `CueSpec.chain` addition

The `chain` field is net-new (`CueSpec` has no shader field today; the v1-absent
comment is at `../vidiotic/src/project.rs:112`). Two coordinated changes:

- **In `vidiotic` (dependency, tracked by the effect-chain plan's FR2a):** give
  `CueSpec` a `Default` impl *or* a `CueSpec::full_length(clip, name)`
  constructor, and mark the new field `#[nserde(default)]` so older `.viproj`
  parse. prep cannot land until this exists.
- **In prep:** switch `full_length_cue` (`src/export.rs:19`) to the new
  constructor / `..Default::default()` instead of the bare literal, so adding
  `chain` (and future fields) doesn't break the build. `full_length_cue`'s intent
  — a whole-clip cue with every knob at default — maps exactly onto
  `CueSpec::full_length` / `Default`, so this is a simplification, not a
  workaround.

## Cue round-trip policy (decision needed)

Reopen→re-export currently drops all cue banks (`reopen_project` reconstructs
spans only). With chains now living on cues, a retrim silently discards any
downstream chain authoring. Two options:

- **A — Accept lossy (recommended for v1, matches effect-chain FR2a).** Keep
  reopen span-only; document in `reopen_project` that cue banks (and therefore
  chains) do not survive a retrim round-trip. Prep's job is source-trimming and
  first export, not cue/chain preservation. Zero new work.
- **B — Preserve cue banks (pass-through).** On reopen, retain the original
  `Project.cue_banks` and re-emit them on export, so downstream chains survive a
  retrim. More faithful, but prep would carry spec data it doesn't understand or
  edit, and must keep cue↔clip id references valid across a retrim (clip ids are
  re-assigned by span index in `build_project`, so a naive carry-through would
  dangle). Non-trivial; out of scope unless retrim-preserves-chains becomes a
  real workflow need.

Recommendation: **A**, with a one-line doc-comment on `reopen_project` stating
the loss explicitly. Revisit only if users start retrimming already-authored
projects.

## Hardening: reduce future breakage

The effect-chain change won't be the last spec addition. To stop every field add
from breaking prep's build:

- Ask `vidiotic` to `#[derive(Default)]` (or provide constructors) for the spec
  types prep literal-builds where a sensible default exists — `CueSpec`
  (required now), and opportunistically `ClipSpec`/`ClipBankSpec`/`CueBankSpec`/
  `Project`. All their fields already carry `#[nserde(default)]`, so `Default` is
  cheap and consistent with the format contract.
- Where a literal must set most fields anyway (`ClipSpec`, `SpanProvenance` in
  the export loop carry real per-clip data), switching to
  `..Default::default()` for the tail still prevents build breaks on additive
  fields. Adopt it in `build_project`.
- Keep prep's dependency surface minimal: continue to import only `project` and
  `transcode`. Do not reach into render/shader — chain *semantics* are the
  runtime's concern; prep only needs the serialized *shape* to be valid.

## Format version

Prep writes `project::FORMAT_VERSION` verbatim (`src/export.rs:245`) and relies
on `project::load` to migrate older files (`../vidiotic/src/project.rs:167-181`).
If the chain change bumps `FORMAT_VERSION` and adds a migration, prep needs **no
version logic** — but it must be rebuilt against the updated `vidiotic` so the
constant and struct layout match. This is automatic given the path dependency;
just don't ship a prep binary built against a stale `vidiotic`.

## Verification

- `cargo build` in `vidiotic-prep` against the updated `vidiotic` (the `CueSpec`
  `Default`/constructor must be in place first) — proves the literal-construction
  break is resolved.
- `cargo test` in prep — export/reopen tests still pass.
- End-to-end: export a `.viproj` from prep, open it in `vidiotic`, confirm it
  loads and cues play (chains default-empty on freshly exported projects). Then
  reopen the same `.viproj` in prep and re-export; confirm it still loads in
  `vidiotic` and that the documented cue-bank loss (policy A) is the only
  difference.
- Confirm an *older* `.viproj` (pre-`chain`) still loads in both, exercising the
  `#[nserde(default)]` path.
