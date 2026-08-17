# Web port — plan

Two browser surfaces carved out of the existing workspace:

- **`/chop`** — ~half of `vidiotic-prep`: open a local video, mark spans, export
  MP4 and/or HAP clips plus a `.viproj`.
- **`/play`** — `vidiotic` a few features down: load an exported chop and run the
  clip/cue/shader engine.

Both are **fully client-side**. The host serves `.html` / `.js` / `.wasm` and
nothing else; no media ever crosses the network. Decisions locked at plan time:

| Decision | Chosen | Rejected |
|---|---|---|
| Source ingest | User supplies a local file; source layer left pluggable | Backend yt-dlp proxy (deferred, see §6) |
| Site topology | One origin, two routes, shared OPFS + zip for portability | Two independent deploys |
| Playback | HAP fast path **plus** RGBA/WebCodecs fallback | HAP-only (desktop-only) |
| Working resolution | Downscale on ingest to a 480p / 320p tier (§3a) | Preserve source resolution |

---

## 1. What actually blocks a naive port

Established by reading the tree, not assumed:

- **`ffmpeg-next`** is the only hard dependency blocker, and it is shallower than
  it looks. In `transcode.rs` ffmpeg is used *purely as a container writer*:
  `format::output`, `add_stream(Id::HAP)` deliberately creates a null-codec
  stream (no HAP encoder exists in ffmpeg), then `Packet::copy` of bytes this
  codebase already produced. The HAP bitstream — texpresso BC1 → snappy →
  `Hap1` framing — is entirely ours and already pure Rust.
- **`wgpu::Features::TEXTURE_COMPRESSION_BC` is `required` at `gfx.rs:113`.** In
  WebGPU that feature is optional. **Measured — see §1a; it is present, and the
  plan holds.** It must still become negotiated rather than required, because
  that is a statement about *other* machines, not this one.
- **`std::os::unix::net`** (`ipc.rs`, 992 lines) has no browser analogue. Cut.
- **`rusty_link`** (Ableton Link) is UDP multicast. Impossible in a browser, no
  workaround. Cut.
- **`std::fs` + `notify`** across `project.rs`, `clippool.rs`, `session.rs`,
  `store.rs`, `shaderwatch.rs`. Replaced by OPFS (async — this ripples).
- **`objc2-*` camera capture** is macOS-only by construction. Cut.
- **wasm32 is 4GB of address space, ~2–3GB in practice.** Nothing may hold a
  decoded source in memory. Export streams frame-at-a-time or it dies.

Ports untouched: `snap`, `texpresso`, `rustfft`, `bytemuck`, `nanoserde`,
`naga`, `egui`, `winit`, `wgpu`.

---

## 1a. Measured: browser capability (resolved)

`docs/spikes/webgpu-capability.html` — open it in any browser to be supported.
It answers most of the plan's outstanding "does the browser actually do this"
questions in one pass, and it does not take a feature flag's word for anything
that can be tested directly.

**The BC1 check is an end-to-end decode, not a capability query.** It uploads one
real BC1 block, samples it with `textureLoad` (no sampler, so no filtering to
confound the result), reads the pixels back, and compares them to what BC1
decoding must produce. It deliberately uses only palette indices 0 and 1 — the
two endpoint colours, which every implementation reproduces exactly. Indices 2
and 3 are interpolated and legitimately vary by ±1 between vendors, so asserting
on them would manufacture false failures.

Chrome 141 / macOS 15.6 / M3 Pro (`apple` / `metal-3`), 2026-07-30 — **zero
failures**:

| Question | Result |
|---|---|
| `texture-compression-bc` | exposed, **and a real BC1 block decodes to exactly the right pixels** |
| ASTC / ETC2 | both also exposed — fallback targets exist if a machine lacks BC |
| WebCodecs decode | H.264 baseline + high, VP9, AV1, HEVC all supported |
| WebCodecs encode | H.264 @848×480 supported — MP4 export (§3) is viable |
| `importExternalTexture` | present — zero-copy video → GPU |
| OPFS | write → read round-trips over an http origin |
| Window Management / Document PiP | both present — §10 dual-head has two viable mechanisms |
| `crossOriginIsolated` | false, `SharedArrayBuffer` absent — **as expected and as designed** (§3c) |
| limits | `maxTextureDimension2D` 16384, `maxBufferSize` 4 GiB |

Two traps this probe fell into, recorded so they are not re-encountered:

- **`file://` has an opaque origin, so OPFS is unavailable there.** The first run
  reported an OPFS failure that was purely an artefact of how the page was
  loaded. Serve the probe over `http://localhost` — which is also how it
  deploys.
- **`createSyncAccessHandle` is `[Exposed=DedicatedWorker]`.** Its absence on the
  main thread says nothing about availability in a worker, which is the only
  place it would ever be called. Probe from a worker before concluding anything
  about synchronous OPFS I/O.

What this does *not* establish: any browser other than the one it was run in.
Safari and Firefox need their own runs before "works on the web" is a claim
rather than a hope — and mobile is the case most likely to lack BC entirely,
which is what the ASTC/ETC2 rows are there to inform.

---

## 2. `/chop` — carving up prep

`vidiotic-prep` is 4,506 lines. Disposition:

| Module | Lines | Disposition |
|---|---|---|
| `spans.rs` | 83 | **As-is.** Pure data, except `source: PathBuf` → source id (§5). |
| `timeline.rs` | 306 | **As-is.** egui custom widget over `phosphor::theme`. |
| `undo.rs` | 149 | **As-is.** Pure. |
| `commands.rs` | 152 | **As-is.** |
| `ui.rs` | 867 | **Mostly as-is**, minus native file dialogs. |
| `app.rs` | 1,685 | **I/O surgery.** State machine survives; every path and blocking load becomes async/OPFS. |
| `session.rs` | 353 | **I/O surgery.** RON logic intact, `.vprep` sidecar moves to OPFS. |
| `export.rs` | 302 | **I/O surgery + new backend.** Orchestration and progress channel survive; the bake behind it is replaced (§3). |
| `preview.rs` | 206 | **Rewrite.** ffmpeg random-access seek → WebCodecs seek-and-decode. |
| `engine.rs` | 126 | **Delete.** `$VIDIOTIC_SOCK` handshake is meaningless here. |
| `control_input.rs` | 246 | **Defer.** MIDI/gamepad in the editor; WebMIDI shim later if wanted. |

Roughly 1,557 lines lift cleanly, 2,340 need I/O work, ~200 are a genuine
rewrite, ~370 are cut. Plus `vidiotic::project` (1,297) comes along, since
export writes `.viproj`.

**Measured, and the two "as-is" UI rows are wrong.** `timeline.rs` and `ui.rs`
do not lift, and native file dialogs are not what stops them: both take
`&mut PrepApp`, and `PrepApp` is the module with ffmpeg, `rfd`, `std::fs` and
the wire client in it. `timeline.rs` reads **11** members of it; `ui.rs` reads
**48**. `commands.rs` says so itself, in its own header — *"panels hold
`&mut PrepApp` and read it directly"*.

| module | lines | reaches into `PrepApp` |
|---|---|---|
| `spans.rs`, `undo.rs`, `commands.rs` | 384 | **no** — doc-comment mentions only |
| `timeline.rs` | 306 | 11 members |
| `ui.rs` | 867 | 48 members |

Both rows are resolved in §2b, and neither cost what this table implies: 32 of
those 48 were the editor by the time 4d landed, and the remainder mostly turned
into commands rather than mirror fields.

This is the difference between prep and the player, and it is worth being exact
about, because §8 step 4g moved 2,102 lines of the player's egui with *zero*
edits and that is easy to read as a promise about egui. It was not. Those
panels crossed for free because steps 4b and 4d had already done this work —
`UiMirror` existed, and every panel was already
`fn show(&mut Ui, &UiMirror, &Sender<Command>)`. Nothing in prep has had that
done, so **prep's UI cannot move until prep gets its own 4d**: a `PrepApp`
split into a portable engine plus a native shell, with a mirror to read and a
command sink to write.

The corrected order is therefore the player's order, not the table's: split the
state machine first, and the 1,173 lines of egui become a move afterwards. The
384 genuinely pure lines can go ahead of it. Estimating the panels as portable
because they contain no OS calls is the same mistake as estimating
`Path::is_absolute` as portable because it compiles (§8 step 1).

### 2a. Prep's own step 4d — done

`PrepApp` (1,685) is now `PrepApp` (1,050) plus **`editor.rs` (860)**, and the
seam is the player's: [`Editor::step`] runs every command that acts only on the
marking session and returns `Some(cmd)` for anything needing an OS, which the
shell answers in `apply_shell_command`. Seven commands come back — `Open`,
`OpenVideo`, `ConfirmPendingOpen`, `FinishOpenProject`, `ShowExportDialog`,
`StartExport`, `ConfirmQuit` — and that list is the whole of prep's boundary.

Two seams do the load-bearing work, and both are the same trick: stop *reaching*
for a thing and take it as data.

- **`MediaInfo` is two numbers.** Every frame calculation in a marking session —
  clamping a seek, looping between marks, fitting the jog window — needs the
  source's length and rate and nothing else. So the ffmpeg `SourceMedia` stays
  in the shell and the editor gets `{ frames, fps }`. This is what makes the
  playhead portable, which is not obvious until you notice that none of it was
  ever about decoding.
- **Time is a parameter.** `ctx.input(|i| i.time)` and `stable_dt` are how the
  old code asked what time it was, which put a live window inside undo
  coalescing and playback stepping. Both now take an `f64`. The editor cannot
  request a repaint either — it sets a flag the shell honours.

**`std::time::Instant` → `web-time`**, for the status-line fade: the same
already-paid lesson as `clock.rs`, and cheaper to pay now than to find later.

**Measured, in V8.** The point of this is portability, and compiling is not
running (§7a) — so the four modules were assembled into a throwaway lib and
`cargo test --target wasm32-unknown-unknown` run against them: **16 tests
pass**, including undo coalescing, bank reindexing, wall-clock playback and the
OS boundary itself. Twelve of those were native-only tests in `app.rs` that
needed a `PrepApp::default()` — which discovers a unix socket, opens a MIDI hub,
polls gamepads and reads the user's global keymap. They now need an `Editor`.

**And that build found what the grep had not.** `commands.rs` was counted as
zero-native-references, and it is — but `Command::FinishOpenProject` carries a
`Box<session::ReopenedProject>`, and `session` parses RON off a disk. One type
in one variant made the entire command vocabulary unbuildable for wasm32.
`ReopenedProject` is plain data and now lives beside the editor; it was in the
loader's module because that is where it is *read*. Nothing in `commands.rs`
names a filesystem — it names a type that does, and no amount of reading it was
going to show that. This is the third time on this port that the compiler for
the target has been the only honest reviewer.

### 2b. The mirror, and the panels — done

`timeline.rs` and `ui.rs` now take `&mut Editor` and a `&PrepMirror`. `PrepApp`
is 826 lines and no panel has heard of it.

**The mirror is two fields.** This is the part worth recording, because it is
the opposite of what §2 predicted. `vidiotic`'s `UiMirror` is a page-long struct
rebuilt every tick, and prep's twin should have been comparable — 48 members
were being read. It is `{ preview: Option<TextureHandle>, exporting: bool }`,
because **the mirror only has to hide what the panels cannot have**, and after
step 4d that is almost nothing: the editor *is* the portable half, so panels
read it directly and post into its own queue. The player needs a big mirror
because its engine is full of wgpu and capture services. Prep has no such thing
left. A mirror is a consequence of what the split left behind, not a fixed cost
of moving panels — which means the player's mirror is the anomaly here, not this.

Of the 48 members, then: 32 were already `editor` after 4d, and the rest
resolved four ways rather than by being mirrored.

| was | became |
|---|---|
| `rfd::FileDialog` in three panels | `Command::PickVideo` / `PickProject` / `PickShaderPath` |
| `show_export_dialog`, `show_quit_dialog` | editor fields — the prompts raise and dismiss each other |
| `media.width/height/duration_sec` | `MediaInfo` — always probe data, never the decoder |
| `controls`, `prep_mapper`, `learn`, + 8 methods | a `Controls` struct, drawn through a hook |

The `Pick*` commands are the shape `/play` already answers with `PickIsf`
(§8 step 4g): a panel names a *want*, not a dialog. Natively `rfd` answers;
in a browser nothing can answer synchronously at all, which is exactly why the
panel must not be the one asking. That trade is the boundary growing by three
commands and losing one — `ShowExportDialog` is now the editor's, since a
dialog flag never needed a machine.

**`Controls` is where the hook came from.** The inspector draws its two binding
tables *inside* the same scroll area as the span list, so the shell has to pass
a closure down into portable layout — and that closure cannot borrow `PrepApp`
while `&mut editor` is already out of it. Two disjoint fields destructure;
twelve scattered ones do not. So the CoreMIDI/gamepad/`prep.vmap` state became
one struct, which is also precisely the part §2 defers behind a WebMIDI shim.
`draw` takes `&mut dyn FnMut(&mut egui::Ui)`; a browser passes nothing.

**The mid-frame drain changed meaning, and is better for it.** `timeline`
drains inside its own widget so drags don't paint a frame behind. It used to
call `drain_commands`, which would run *anything* queued — including opening a
video, mid-layout, underneath the panel that asked. It now calls
`Editor::drain_ui`, which runs what the editor owns and parks the rest for the
end-of-frame drain. Every command a drag can post is editor-owned, so the
tactile path is unchanged; the tear that was latent is gone.

**Measured, in V8.** Seven modules — `commands`, `spans`, `undo`, `editor`,
`mirror`, `timeline`, `ui`, 2,391 lines — build for `wasm32-unknown-unknown`
with the panels linked in, and their 19 tests pass in Node. Three are new and
cover exactly the seams above: that every chooser leaves as a request, that one
command owns both dialog flags, and that a mid-frame drain parks an `Open`
instead of running it.

Prep's line count went up (4,725 → 5,097) and that is the honest number: the
seam costs a mirror module, a shell-UI module, and a `Controls` struct. What it
bought is that the browser half compiles for the browser.

### 2c. `vidiotic-chop`, the crate — done

The seven portable modules are now a crate, and `vidiotic-prep` is a native
shell that depends on it. Same shape as `vidiotic-play`, for the same reason:
**a boundary a convention holds is not held.** Before this, nothing stopped
`use std::fs` reappearing in `ui.rs`; the proof it hadn't was a throwaway lib
in a temp directory that evaporated with the session. Now it is a row:

```
vidiotic-chop  --no-default-features  PASS  portable span editor …
vidiotic-chop  --no-default-features  --lib  19  … and the shell boundary itself
```

The gate reads 7 portable crates, and **6 suites / 203 tests passing under
wasm32 in V8**. `vidiotic-chop`'s dependency list is now the boundary, and it is
five entries long: `vidiotic-core` and `vidiotic-ctl` with default features off,
`phosphor` without `shell` or `logging`, `egui`, `web-time`. No ffmpeg, no rfd,
no sockets, no `std::fs`.

It earned its keep on the first build. `PendingOpen::then` was `pub(crate)` —
correct when the shell was in the same crate, and instantly a compile error when
it wasn't. That field is a continuation only a shell can run, and the crate
split is what made the code say so.

**It is a ninth repo, and that was already decided.** Whether `/chop`'s portable
half deserved its own repository the way `/play` does looked like an open
question worth putting to a human. It was not: this repo's `.gitignore` ignores
`/*` and re-includes only the workspace glue, with the reason written out —
*"Every member crate … is its own git repository and is intentionally not
tracked here."* Adding a crate directory to the composition repo would have
silently produced an untracked crate. The convention answered it; `git init` was
the only move consistent with the layout that already existed.

### 2d. The web shell — done

`web/chop.html` + `web/chop.js` + `vidiotic-chop/src/web/`, built by
`scripts/build-chop.sh` and driven by `scripts/chop-smoke.mjs`. **SMOKE PASS,
18 checks**: it boots, paints, opens a real video, decodes a preview frame,
marks a span from real keypresses, undoes it, reopens a `.viproj`, and opens a
file chooser from inside a rAF callback.

The shell is 600 lines and most of it is comments, because the interesting part
is what each of the nine commands *means* here:

| command | native | browser |
|---|---|---|
| `Pick*` | `rfd` returns a path | an event the page turns into `<input type=file>` |
| `OpenVideo` | open a decoder | only the open video reopens — there is no filesystem to find another |
| `ConfirmPendingOpen` | run the parked open | never raised: the page has the file before the editor hears a name |
| `FinishOpenProject` | adopt, then fill the export folder | adopt, then *ask for* the source video |
| `StartExport`/`ConfirmQuit` | a bake thread, a viewport close | say so on the status line |

Four things worth recording.

**`eframe`, not `/play`'s hand-rolled loop.** `/play` owns two canvases in two
documents and a wgpu pass for the output head, so it needs its own frame loop.
`/chop` is one egui app on one canvas, and `WebRunner` is the tool for one.

**WebGL2, not WebGPU, and this is the one place the two ports should differ.**
`/play` is *about* WebGPU and a silent WebGL fallback would defeat the
measurement. `/chop` paints egui and one RGBA image — no compute, no compressed
textures, no shaders. Measured after `wasm-opt -Oz`:

| eframe backend | bundle |
|---|---|
| `wgpu` | 9,881 KiB → 8,078 KiB |
| `glow` | 6,683 KiB → **5,823 KiB** |

2.2 MB for nothing, and `glow` runs where WebGPU is still behind a flag.

**The keyboard had to be split out, and nearly wasn't.** `control_input.rs` was
counted as "defer — MIDI/gamepad", so it stayed native wholesale. But resolving
a key is arithmetic over a `ControlMap`; only *getting* one is a machine. The
table is now `vidiotic-chop/src/keymap.rs` and both shells resolve against it,
which is the difference between a browser editor and a browser editor you can
only use with a mouse. The smoke presses `i`/`o`/`a` through Chrome's own input
pipeline and gets a span, so the egui→`vidiotic-ctl` key spelling is exercised
for real rather than agreed with.

**And the smoke earned its keep on the first run.** A browser egui repaints when
something asks it to, and everything the page calls in with arrives from outside
egui's world — a `change` handler, a `seeked` callback. So opening a video left
the editor holding it and the screen unchanged until the visitor happened to move
the mouse. Nothing native has this shape: eframe's winit loop is already awake,
and prep's decoder answers inside the frame that asked. `post` now requests a
repaint. That is a *fourth* class of bug this port has hit that only the real
target could show — after `Path::is_absolute` compiling, `Instant` panicking, and
a command payload typed against a RON loader.

### 2e. Export, in a tab — done

`/chop` bakes a marking session to a `.viproj` and its clips, in the browser,
and hands back a zip. **SMOKE PASS, 27 checks**, the last four of which unpack
the archive a visitor would actually receive and read the project out of it.

Three things carried this, and none of them is new code doing the work.

**The bake is `vidiotic-bake`'s, and now literally so.** The browser bake driver
was `vidiotic-play::web::bake` — the only thing baking in a browser was `/play`'s
ingest. `/chop` bakes too, and the two front ends cannot depend on each other
(one carries wgpu, the other must not). So it moved to `vidiotic-bake::web`,
where the compressor and muxer already were, and both shells drive one
implementation. `/play`'s bundle and `boot.js` are unchanged — the `Baker`,
`is_baked` and `bake_size` exports are `#[wasm_bindgen]` items in a crate it
links, so they land in its glue exactly as before. Verified by rebuilding `/play`
and re-running its smoke, not by reasoning about it.

**The `.viproj` is assembled once, for both exporters.** `vidiotic-chop::export`
takes what each shell learned while baking — a path, a provenance, an fps, a
frame count — and returns the `Project`. Prep's exporter lost 60 lines to it.
This is the one that would have rotted quietly: `.viproj` is the contract
between prep and the player, and a browser export with a subtly wrong field
produces a project that *loads* and then behaves differently. The two exporters
now cannot disagree, because there is nothing to disagree about.

There is a test for the round trip that closes: `assemble` → `from_project`
gives back the spans it started from. The retrim feature is exactly that loop.

**A zip, because a project is not a file.** A `.viproj` references
`clips/xxx.mov` relative to itself, and a browser can hand back one thing. So
`export::zip` — stored, not deflated, since Hap1 is snappy-compressed already
and deflating it spends CPU to produce a slightly larger file. Around 90 lines
including a bitwise CRC-32, with a test that walks the central directory the way
a reader does, plus the standard `0xCBF43926` check value so an arithmetic slip
shows up here rather than as an archive every unzip refuses.

**What a browser export cannot do, and says so.** Prep reopens each span's own
source by path, so it can export a session marked across several videos. There
is one `<video>` element here, so an export refuses — by name — if any span came
from a video that is not open. The smoke checks the refusal as well as the happy
path, because a limit that fails silently is worse than the limit.

Two smaller notes:

- **The export snapshots the document when it starts.** A bake takes minutes and
  nothing stops the visitor renaming a span while it runs; assembling from live
  state at the end would describe spans that no longer match the clips already
  written.
- **§3's "never accumulate" still holds where it meant to.** Each frame is
  decoded, compressed, appended and dropped inside the `Baker`. What the shell
  holds is *finished clips*, which is unavoidable when the deliverable is one
  archive.

### 2f. OPFS: closing the tab is not losing the evening — done

The video goes into OPFS as `source.bin`, the marking session beside it as
`session.vprep`, and the video's name into `localStorage`. **SMOKE PASS,
33 checks**: the last five mark a span, reload the page, and get the video and
its spans back — then press "forget it", reload again, and get an empty editor.

**The sidecar is prep's sidecar.** `SessionFile` moved to
`vidiotic-chop::session`, so what the browser stores in OPFS is byte-for-byte
what the desktop writes next to a `.mov`. Nobody asked for that; it is what
falls out of not writing the format twice. The alternative — a browser session
format invented separately because the *storage* happened to differ — is how two
halves of one tool stop being able to hand work to each other. Prep's
`session.rs` is now the filesystem around it and nothing else.

**Two stores, for the reason §7 already gives.** Bytes with a write API built
for bytes; one string that should be readable by a human in devtools. The
sidecar goes to OPFS rather than `localStorage` because it grows with the
session, and `localStorage` is a few MB per origin shared with everything else.
All of it is best-effort: private browsing denies OPFS, and a session with no
storage is exactly the session this page had before storage existed.

**`persist()` is asked for once, after the first write** — same reasoning as
`/play`: Firefox raises a prompt, and prompting before there is anything to
protect asks a question the visitor cannot answer.

**Storing a video clears the old sidecar.** Its spans belong to the *previous*
video, and restoring them against a new one would put marks at frame numbers
that mean something else entirely. The smoke checks the other half of the same
rule too: spans marked on a video that is not open are never stored.

**And the autosave needed a heartbeat, which is the second time this has bitten.**
It runs per frame, throttled to ~1 Hz — exactly prep's shape. Natively that is
free, because eframe's winit loop is already running. In a browser egui stops
drawing when idle, so an edit made just before the visitor stopped touching
anything sat unsaved forever. `request_repaint_after` the remaining throttle
closes it. The smoke found it by marking a span, waiting, reloading, and getting
back an empty session — which is precisely the bug a person would have hit on
their first real evening with this, and precisely the kind no compiler or unit
test can see. Same family as `post` needing a repaint (§2d): **anything in this
shell that must happen without input has to say so.**

### 2g. A third render: offsets — done

The export dialog's render control is now three-way: **clips**, **clips (hq)**,
**offsets**. The third one renders nothing. It writes a single `.viproj` naming
the source video as one clip, with every span as a *trimmed cue* into it.

**Nothing in the runtime had to learn a new idea.** `CueSpec` has carried
`in_sec`/`out_sec` all along — a trimmed cue is what the player already is. So
"offsets" is not a new format, it is the same session expressed the way the
player thinks about it: N cues over one clip instead of N clips.

The trade is stated in the dialog rather than left to be discovered. Baking is
minutes and it is why iterating on a chop is slow; an offsets project is
**1,776 bytes, written instantly**. What it costs is self-containment — the file
is useless without the source at the other end. That is exactly the trade to
make when handing your own work back to yourself, and exactly the wrong one when
handing it to somebody else.

**What makes it land is a rename both ends already agreed on.** `/play`'s
browser ingest bakes a dropped file and interns it as `<stem>.mov`; an offsets
project names its clip `SourceRef::clip_name_for(source)`, which is the same
string. Drop `bun.webm` into `/play`, load the project, and the clip matches.
There is a test for that spelling on its own, because it is the one thing that
would fail silently — a project whose clip is simply never found.

**Clip banks become cue banks**, since in this mode a span *is* a cue. The
grouping the visitor made is the grouping they get.

**Only in the browser, deliberately.** Prep could call the same function, and
should not: natively the source is whatever video the visitor opened, and
`vidiotic` reads Hap1 — so a native offsets project would name a clip the player
cannot play. In the browser the source has been through `/play`'s ingest and is
Hap1 by the time it matters. A mode that produces a broken project on one of two
shells is worse than a mode that only exists on one.

### 2h. The import half — done

Browser `/play` loads a `.viproj`. Drop one on the page or pick it; the round
trip closes.

**The filesystem is the pool.** `project::resolve_with` takes an injected `Fs`
whose doc comment has said since it was written that *"in a browser it is an
index of OPFS"*. It is one step simpler than that: an index of what is **already
interned**, because bytes reach this player through a drop, a file input or OPFS
and get a display name long before a project mentions them. Only the file name
is compared — the directory part of a stored path describes a layout on somebody
else's disk, and a project written on a desktop carries `clips/00_cut_10-40.mov`.

Everything downstream was already portable and already gated: `assemble` rebuilt
the pool, clip banks and cue banks with fresh ids, and the shell re-keyed its
loaded bytes onto them. The import is ~90 lines of glue over machinery that
existed.

**Loading is a swap, not a merge** — pool, banks, cues, tempo. That is what it
does natively too, and it is the honest behaviour: a `.viproj` names clip ids and
cue banks that only mean anything relative to each other. The *bytes* survive,
which is the whole point: load a project against clips you already dropped and
nothing is fetched or re-baked.

**A missing clip is named.** `this project needs clip(s) the page does not have:
bun.mov — drop them in first`. A project that quietly loads with half its clips
absent is a set of cues that fire and show nothing, which reads as a broken
player rather than a missing file.

**And one thing had to be decided rather than inherited.** `Engine::new` seeds
cue *banks* but not the sequencer's rotation, so a freshly loaded project has
cues and an empty rotation — it comes up **silent**. That is defensible in front
of a desktop with a controller in your hands, and it is the wrong answer for a
page somebody was handed a link to, where a black output head is
indistinguishable from a broken build. The browser shell puts the live bank's
cues into the rotation on load. Deliberately a shell decision and not a change to
`Engine::new`: what should be playing the moment a project loads is a question
about the front end, and the native app answers it by having a human there.

The smoke proves the join rather than the parse: it bakes `probe.webm` in the
page (interned `probe.mov`), loads an offsets project naming `clips/probe.mov`,
and checks the pool swapped to one clip, both cue banks arrived, the tempo took,
**and a cue rendered through the composite pass** — which is the only assertion
that would catch a re-key that silently produced ids pointing at nothing.

### 2i. Measured: why a bake is slow, and the two ways out

Reported symptom: 3–4 fps. Profiled in Chrome (headless, SwiftShader — so the
scale costs are inflated and the *shape* is what matters), per frame:

| source | seek | drawImage | **BC1 push** | fps |
|---|---|---|---|---|
| 640x360 | 4.1ms | 8.0ms | 7.6ms | 50 |
| 1920x1080 | 9.2ms | 29.0ms | 5.6ms | 22 |
| 3840x2160 | 29.9ms | 79.5ms | 5.3ms | 8.7 |

**The compressor is not the bottleneck.** BC1 is flat at ~5–6ms because
`Tier::Wide` caps output at 848x480 whatever the source is. Everything that
scales is the cost of getting pixels *out of the `<video>` element*. Three canvas
strategies — `willReadFrequently`, a GPU canvas, `createImageBitmap` with resize
— came out within 12% of each other, which is the signature of the work being
the scale itself rather than the API.

**Way out 1: don't seek per frame.** Setting `currentTime` asks the demuxer for
a random access, and on a long-GOP camera file a browser may decode from the
previous keyframe to reach it — at a 250-frame GOP, up to 250 decodes per
delivered frame. Capturing during playback via `requestVideoFrameCallback` has
no per-frame seek at all: 7.5 → 14 fps at 4K.

**Way out 2: a proxy, and it is much bigger.** An all-intra file at the bake
tier makes every frame a keyframe *and* removes the downscale:

    ffmpeg -i IN.mp4 \
      -vf "scale=w=848:h=480:force_original_aspect_ratio=decrease:force_divisible_by=4" \
      -r 30 -g 1 -c:v libx264 -preset veryfast -crf 20 -pix_fmt yuv420p -an \
      OUT_proxy.mp4

**7.5 → 73.7 fps**, and it costs nothing in quality: 848x476 is exactly what
`bake_size` produces for that 4K source, so the bake was going to throw the rest
away regardless. One second of ffmpeg for a 12-second clip.

**So the path is chosen by measurement, not by flag.** Neither wins everywhere —
playback capture cannot beat realtime (~33ms/frame), and on a proxy seeking
costs ~13ms. `bakeSpan` seek-steps four frames, times them, and switches to
playback only if seeking is losing to realtime. A prepared source gets the fast
path without knowing the flag exists; an unprepared one gets the better of the
two. `?capture=seek|play` forces either, for comparing on a real machine rather
than arguing about it, and the export line now shows achieved fps live — a bake
that will take four minutes should say so while there is still time to stop it.

Still outstanding: one session, one **source**. To be exact about which limit
this is, because the words are easy to blur — a session slices *one video* into
*as many clips as you like*, which is the entire job and is not restricted at
all. The smoke marks three spans, exports three distinct clips, and gets all
three back after a reload. What there is no model for is a second *source*: the
store holds one video, and the browser shell's `OpenVideo` can only reopen the
one already open (§2d), so a session marked across several files is prep-only.
Prep does it by reopening each span's own source by path, which is exactly the
thing a browser has no equivalent for.

`preview.rs` is the one place where the browser is *worse*: `VideoDecoder` has
no seek. Frame-accurate scrubbing means seeking the demuxer to the prior
keyframe and decoding forward, with a decoded-frame cache around the playhead.
The current ffmpeg implementation gets this for free.

---

## 3. Export

**MP4** — WebCodecs `VideoEncoder` (H.264/VP9/AV1) into an MP4 muxer.
Hardware-accelerated, unremarkable.

**HAP** — the interesting path, and now finished in both directions: `/play`
ingests a dropped mp4 and `/chop` exports a whole project (§2e). The list below
is what that took.

1. Decode source → RGBA. ~~WebCodecs.~~ **Done, and not with WebCodecs — see
   §3d.** A `<video>` element, seek-stepped.
2. RGBA → BC1 blocks. `texpresso`, or a WebGPU compute shader.
3. Snappy-compress the frame. `snap`, unchanged.
4. `Hap1` section framing. `video/hap.rs` logic, unchanged.
5. Mux to `.mov`. ~~**New**~~ **Done — `vidiotic-bake/src/mov.rs`** (§8 step 2).
   A single video track with a `Hap1` sample description, as predicted; it also
   fixed a frame-dropping bug in the muxer it replaced.

Steps 2–5 of this list are portable Rust that builds for wasm32, and step 1 is
answered by the browser. **The whole list now runs in a tab** — `/play` bakes a
dropped mp4 to Hap1 without a server, a native tool, or a second
implementation of anything (§8 step 4e). Nothing in the HAP export path is
unresolved.

Step 2 is the hot path — the workspace `Cargo.toml` already special-cases
texpresso and rayon at `opt-level = 3` because it is slow unoptimized. Its cost
is linear in pixel count, which is what §3a is really about.

Whatever the backend, `export.rs` must never accumulate: decode → compress →
snappy → append → **drop**. Its existing worker-thread-plus-progress-channel
shape is already right. This holds at every resolution — see §3a.

### 3a. Working resolution

Everything downstream of ingest runs at a fixed low tier: **848×480** or
**568×320**. Both dimensions must be divisible by 4, since BC1 operates on 4×4
blocks — the conventional 854×480 is *not* (854/4 = 213.5) and would force
padding. This belongs in the existing `BakeQuality` enum (already a parameter on
`transcode::run_span_with`), not hardcoded.

**Do the downscale on the GPU, for free.** `VideoFrame` → `importExternalTexture`
→ blit to the tier-sized render target → compress from that. No CPU scaling, no
full-resolution readback, and it lands in the WebGPU stack that exists anyway.

What this buys:

- **Single-threaded wasm BC1 becomes viable — measured, see §3c.** This is the
  decisive one, and it is now settled rather than assumed. It deletes the GPU
  compute compressor, the SharedArrayBuffer requirement, and the entire COOP/COEP
  hosting and embedding constraint in §7.
- **The RGBA fallback becomes nearly free.** 1.64MB/frame at 480p uploads
  cheaply, so failing BC feature negotiation stops being a meaningful downgrade.
- **`/play` can hold many more clips resident**, which matters for a tool whose
  point is switching between them.

What it does *not* buy:

- **Streaming is still mandatory.** 480p RGBA at 30fps is ~8.8GB for three
  minutes. The frame-at-a-time discipline in §3 is unchanged at any resolution.
- **`High`/ClusterFit does not get cheap.** It is not the ~5x that pixel count
  alone predicts — see §3c.

**It also changes why HAP is worth keeping.** At this tier the bandwidth/VRAM
argument for BC1 largely dissolves. The argument that survives is *random
access*: `sequencer.rs` retriggers clips on a musical grid via `request_restart`,
and every HAP frame is independently decodable — parse, snappy, upload. WebCodecs
has no seek, so a loop restart means re-seeking to a keyframe and decoding
forward. For a tool built around retriggering on the beat, that is the durable
reason to keep HAP, and downscaling does not weaken it.

### 3b. Filmgrain as the default shader

Grain is the default look, and it is doing technical work as well as aesthetic:
BC1 blocks on gradients and 480p is soft, and grain is the standard dither/mask
answer to both. Cheap to write — `preamble.frag` already supplies `time`, `lvl`,
`resolution`, and `video(uv)`, so it is a hash-noise fragment shader of roughly
fifteen lines inside the existing uniform contract.

### 3c. Measured: BC1 throughput (step 0, resolved)

Benchmarked 2026-07-27 on an M3 Pro (11 cores) against texpresso 2.0.2 at
`opt-level = 3, lto`, using the same `Format::Bc1.compress` call and `Params` as
`transcode.rs`. Source frames are two real 1080p+ videos (1080p animation, and a
2048×1536 grain-heavy film scan), three frames each, lanczos-scaled to each tier
exactly as the pipeline would. wasm figures are a real `wasm32-unknown-unknown`
build executed under node/V8 — a direct measurement, not a penalty factor.

**848×480, per frame:**

| Build | Draft (RangeFit) | High (ClusterFit) |
|---|---|---|
| native, single-thread | 7.4–8.5 ms | ~170 ms |
| native, rayon (11 cores) | 1.4–1.5 ms | 29–31 ms |
| **wasm, single-thread** | **12.0–12.8 ms** | **~210 ms** |

**Verdict: `Draft` runs single-threaded in wasm with room to spare.** ~12.4 ms
against a 33.3 ms budget at 30fps — roughly 2.7x faster than realtime, so a
60-second clip costs ~22 s of BC1 work. No threads, no SharedArrayBuffer, no
COOP/COEP, no GPU compute compressor. §7 collapses to "any static host".

**`High` is not viable as a default anywhere.** 210 ms/frame in wasm is 6.3x
slower than realtime; a 60-second clip is ~6.3 minutes of BC1 alone. Keep it as
an explicit opt-in bake setting, not the path a casual export takes.

Three findings worth carrying:

- **The wasm penalty is small: 1.5x for RangeFit, 1.24x for ClusterFit.** Better
  than the 2–3x this plan originally assumed.
- **`-C target-feature=+simd128` does nothing** (within noise on both algorithms).
  texpresso does not auto-vectorize. Do not carry the flag expecting a win.
- **ClusterFit does not scale with pixel count.** 1080p→480p is 5.06x fewer
  pixels but only ~3.1x faster, because downscaling *concentrates* detail: mean
  distinct colours per 4×4 block rises from 7.25 at 1080p to 9.8–10.4 at 848×480,
  and ClusterFit's cost tracks that. RangeFit is near-linear and essentially
  content-independent (45–58 Mpx/s across every tier and both sources).

**Benchmark trap, recorded so nobody repeats it.** Do not measure BC1 on frames
decoded from the project's own HAP clips. HAP *is* BC1, so its decoded pixels are
already quantised to ≤4 colours per block — 2.27 distinct colours/block measured,
versus 7–11 for real video — and ClusterFit runs ~10x too fast. The first run of
this benchmark used `clips/bun.mov` and produced nonsense (a 640×360 frame
compressing faster than a 568×320 one).

Harness: `scratchpad/bcbench` (not committed).

---

### 3d. Measured: decoding a visitor's video (step 4e)

The plan said WebCodecs for step 1 of §3. What shipped is a `<video>` element
whose `currentTime` is stepped frame by frame, drawn to a 2D canvas. Both the
reason and the cost are worth recording, because the WebCodecs route is still
the better one and this is what it will have to beat.

**Why not WebCodecs.** `VideoDecoder` takes *encoded chunks*, and a browser
ships no demuxer. Feeding it means either a JS dependency (mp4box.js) on a page
that is deliberately dependency-free, or extending `vidiotic-bake::mov` to hand
out non-HAP samples plus the `avcC`/`av1C` description — and that still leaves
WebM/Matroska unread. A `<video>` element decodes everything the browser can
play, which is exactly the promise to make to somebody dropping a file.

**Why seek-stepping and not `requestVideoFrameCallback`.** rVFC is the obvious
capture hook and it was the first implementation. It is driven by the
document's *rendering steps*, and Chrome separately pauses muted video-only
media in a hidden document. So the moment the visitor switches tabs — which
they will, because a bake takes minutes — playback stops, the callback stops
firing, and the bake silently produces a short clip or none at all. Measured,
not predicted; that is what the first version did, and the smoke test caught
it. Setting `currentTime` and waiting for `seeked` needs no playback, no
autoplay policy, and no rendering step.

**The constraint that survived, and it is a real one.** Chrome will not load a
media element in a hidden document at all: `readyState` stays 0, `networkState`
sits at `NETWORK_LOADING`, no `error` fires, and nothing is ever buffered. There
is no event and no timeout — an unguarded bake simply never ends. The page now
waits for visibility explicitly and says so in the status line, so the bake
stalls and resumes rather than hanging. **This is the durable argument for
doing the demuxer work later**: WebCodecs has no tie to document visibility, so
it is what makes a bake survive a backgrounded tab.

Two smaller notes:

- **Ingest is constant-rate** (`?fps=`, default 30). A seek-stepped source has
  no frame timing to preserve, and normalising is the same trade §3a already
  makes for resolution. `Baker` itself takes an explicit per-frame `pts` in a
  microsecond timescale, so it does not know or care — a WebCodecs driver can
  hand it true variable-rate timing later without the type changing.
- **The downscale is `drawImage`, not `importExternalTexture`.** §3a's GPU
  route is still right for a WebCodecs pipeline; with a `<video>` element in
  hand, the 2D canvas path is already hardware-scaled and costs one
  `getImageData` that the readback needs anyway.

Measured on the smoke asset (640x360 VP9, 2 s, 60 frames): 0.9 s end to end,
i.e. faster than realtime, on the same machine where §3c measured `Draft` BC1 at
2.7x realtime. Seek overhead is not the bottleneck at this tier; the compressor
is.

## 4. `/play` — vidiotic, a few features down

The features to drop are exactly the non-portable ones. That is the whole trick.

**Cut (~2,400 lines):** `ipc.rs` (992), `video/capture.rs` (844),
`transcode.rs` (337), `shaderwatch.rs` (57), Ableton Link, the `objc2` camera
stack, `rfd`.

**Survives largely intact:** `render.rs` (1,567), `isf.rs` (1,380),
`project.rs` (1,297), `grammar.rs` (800), `commands.rs` (490), `sequencer.rs`
(442), `clock.rs` (373), `shader.rs` (541), `analysis.rs` (208), `bank.rs`
(161), `clippool.rs` (189), `ui/*` (~1,800). Runtime GLSL→naga compilation
works in wasm, so the shader editor survives — it just loses *disk* hot-reload.

**Measured (step 4b): "survives intact" was right, and cheaper than it reads.**
`commands`, `grammar`, `sequencer`, `clock` and `undo` — 2,187 lines — moved to
`vidiotic-play` and **compiled for wasm32 on the first attempt**. The entire
cost was two lines of `web-time` and a `cfg` on `LinkClock`. `grammar.rs` in
particular, 800 lines of modal state machine, needed *no* edit: it was already
defined over `vidiotic_ctl`'s toolkit-neutral key names rather than winit's.

The remaining `ui/*` is in better shape than the estimate too. Only `ui/mod.rs`
(299) names winit or `rfd`; `editor`, `library`, `transport`, `status` and
`whichkey` are 2,104 lines of pure egui with **zero** non-portable references.

**Changes:**

- ~~`gfx.rs` (139)~~ — **done (step 4).** `TEXTURE_COMPRESSION_BC` is negotiated
  into `Graphics.caps: GpuCaps` rather than required. A struct, not a bool, so
  §1a's ASTC/ETC2 tiers are additive later. The `ensure!` survives *natively*
  under a cfg, and deliberately outlived the fallback: `video/softdec.rs`
  (step 4c) is reached from `web::Engine::pull_frame`, not from the native
  decode thread, so a native device without BC would still build a
  `Bc1RgbaUnorm` texture and show black. Wiring the native path to it is a
  separate job with no known machine that needs it. On the web it is a warning,
  and the CPU path takes over.
- `video/decoder.rs` (454) — **rewrite, and it gets simpler.** Pure-Rust MP4
  demux feeding the existing `video/hap.rs`. No paced-decode thread needed if
  frames are pulled by the render loop. Estimate ~250 lines for the HAP path.

  **Measured: 200 lines, and the estimate's reasoning was right for the right
  reason.** `vidiotic-play/src/clip.rs` is the whole read path — `Clip::open`,
  `duration_sec`, `sample_index_at`, `frame` — and pulling from the render loop
  is exactly what removed the thread. It also removed more than the pacing: with
  the loop asking "which sample now?", a 30 fps clip on a 60 Hz display decodes
  on half the frames and uploads on none of the rest, because `sample_index_at`
  returns the index already on the GPU. `decoder.rs`'s bounded channel,
  newest-wins drain, restart channel and `pace()` all have no counterpart.

  The part the estimate did not anticipate is that **the timeline is where the
  bugs are, not the container.** `sample_index_at` has to wrap rather than
  clamp (a cue keeps playing past the end), handle negative time (a cue nudged
  behind the beat), and survive a zero-duration sample. All three are unit
  tests, and they run in V8 — `MovWriter` takes `Write + Seek`, so
  `Cursor<Vec<u8>>` builds a real HAP `.mov` in memory with no filesystem.

  **The real clips are legacy-baked, and a test assuming otherwise failed.**
  `clips/*.mov` predate the timeline fix: `timescale = 16000` (libavformat's
  override) with per-frame durations alternating 528/544 to average 30 fps, plus
  the zero-duration tail frame. A walk stepping by `duration / frame_count`
  drifts a whole frame by mid-clip. The fix was to the test — drive off each
  sample's own span — and the finding is one more argument for re-baking.

  **The demux half is done: `vidiotic-bake::mov::demux`.** Written into the
  existing `mov.rs` beside the writer, so the two halves of the format cannot
  drift apart. It returns every sample's offset, size, pts and duration, plus
  `sample_at(t)` for seeking; `stsc` run-expansion, uniform `stsz` and `co64`
  are all handled, because real clips use shapes our own writer never emits.
  Verified by `tests/mov_demux.rs` against ffmpeg's demuxer on both our files
  and ffmpeg's, and by pushing every located sample through `hap::decode_frame`.
  Portable — it runs under wasm in the gate. What remains of `decoder.rs` is the
  pacing and the texture upload, not the container.
- `audio.rs` (185) — cpal → WebAudio. Small.
- `app.rs` — **the big unknown got much smaller, and it was a slice, not a
  port.** Split into `app/` (14 modules), the non-portable surface collapsed
  into one file: nine modules — `clips`, `dispatch`, `ipc`, `keys`, `mirror`,
  `project`, `session`, `verbs`, `windows`, 1,296 lines — name nothing
  non-portable at all; three more (`cameras`, `history`, `transport`, 313 lines)
  name only `Instant`; `shaders` (131) only `fs::read_to_string`. Everything
  that made it "the big unknown" is in `app/mod.rs` (916) — winit ×11, cpal ×4,
  one thread — and that is the OS shell the browser replaces rather than ports.

  The estimate said "threading, fs, and IPC woven through". Threading and fs
  were not: **two** thread references and **three** `fs::` calls in the whole
  2,784 lines. IPC and capture genuinely are woven (29 and 36 references), but
  both are *cut* for `/play`, so that is deletion, not translation.

  **Resolved — step 4d.** `App` no longer holds the session at all; it holds an
  `Engine` and the OS around it. The `/play` shell holds the same `Engine`.

**RGBA fallback.** ~~When `texture-compression-bc` is absent: decode via
WebCodecs to RGBA and upload uncompressed.~~ **Done — step 4c, and not via
WebCodecs.** `video/softdec.rs` expands the BC1/BC3/BC4 blocks HAP already
carries, on the CPU, in ~200 lines. WebCodecs would have meant a second decode
path with a second container story for a case that is *already decoded* by the
time BC support matters — the blocks are in hand, they just cannot be sampled.
Bandwidth-expensive and it gives up the near-zero-CPU path, as predicted, but it
runs only when the timeline moves to a new sample. The same module serves
non-HAP clips in an exported chop once that exists.

---

## 5. The chop bundle

A chop is a `.viproj` plus its clip files. Handoff is **same-origin OPFS** —
`/chop` writes, `/play` reads, no download, no upload, no round trip. Zip
export/import exists for moving a chop between machines.

**Done — §5a below.** The destination is a control beside the render, the
directory is claimed on boot, and the archive writer moved to
`vidiotic_core::bundle` so both shells write one format.

**Format change required.** `.viproj` references clips by `PathBuf`. On the web
that must become a bundle-relative name resolved against an OPFS directory or
zip root. `project.rs` already carries `PROJECT_VERSION` and a `migrate` path,
so this is a versioned change, not a fork — and it should be made in the
*native* project format too, so one `.viproj` opens in both worlds. Doing this
first, in the existing native codebase, de-risks everything downstream.

### 5a. The handoff, the store, and saving — done

Four things that turned out to be one, because they all sat on the same
limitation: `/play`'s OPFS held a single `clip.mov`.

**The store is a directory, and the entry names are the pool names.** A project
resolves its clips by name and `PoolFs` matches by name, so the honest store is
one whose listing *is* the pool — restoring a session becomes listing a
directory. Percent-encoded rather than sanitised, and that distinction is the
whole of it: a lossy rename restores a pool that no `.viproj` can match, which
looks exactly like the clips having gone missing. The old single file is
migrated in on first sight. The project text is kept beside the clips, because
clips without their project is a rack of clips and no set.

**The handoff is a directory too, not an archive.** Zipping so the tab next
door can unzip is work done to move a file between two directories that are
already the same directory. `/chop` writes `handoff/`, `/play` claims it on
boot and clears it — claimed rather than read, or it would reload itself over
the top of the session on every visit.

The `.viproj` is written **last** and is the marker. `/play` claims only when
it is there, and it is only there when the clips beside it are, so a
half-written handoff is an empty one rather than a project missing clips.

**The two kinds of handoff want opposite things from the store**, which is why
one function decides rather than two calls in a row. A *clips* handoff is a
whole session and replaces it — its project names its own clips and nothing
else, so leaving the previous pool underneath would fill the library with clips
no cue can reach. An *offsets* handoff is a project **about** a video that has
to be there already, so the stored clips are exactly what it needs and clearing
them would delete the thing it references.

**Saving is the mirror of loading**, through the same `Project::from_runtime`
the desktop player saves with — which is the only reason to be able to save one
at all: a set built in a browser opens on the machine driving the projector.
All three save commands land in one place, because in a browser they are one
action. `SaveProject` writes back over what was opened and `SaveProjectAs` asks
where, but a tab has no "where": every save is a download under a name the
visitor can change in the prompt, and drawing a destination picker that decides
nothing would be worse than not having one.

Two fields are recorded as absent rather than written wrongly. `sync` is always
`Internal`, because Link has no browser transport and writing `Link` would
produce a project that waits for a clock that cannot arrive. `shader_path` is
`None`, because a shader arrived as source text through a file chooser and
recording its *name* would write a project that fails to find a file by it.

**`vidiotic_core::bundle`** is where the zip writer lives now. `/chop` had it
because it exported first; the two browser shells cannot depend on each other,
so it moved to the crate that already owns the `.viproj` format. One
implementation, so the two cannot drift into two archive formats a reader has
to tell apart. `to_ron_bytes` went with it — the half of `save` with no
filesystem in it, exactly as `from_ron_versioned` is the half of `load` with
none.

Verified end to end rather than by inspection. chop-smoke asserts `/chop`
writes the shape; play-smoke plants one and asserts `/play` claims it, drains
it and keeps it. `/chop`'s browser is launched without WebGPU or popups (it
paints egui through WebGL2), so it cannot drive `/play` itself — what the two
share is the shape, and both assert the same one.

### 5b. Cameras in a browser — done

The last genuinely missing feature rather than a deliberate omission. A camera
cue was always a pool clip whose source is a device: timeline knobs inert, a
delay instead. Everything about what one *is* was already here and shared. What
was missing was pixels.

**A third `Opener`, not a second engine.** `getUserMedia` hands back a
`MediaStream` that only a media element will play, so the page owns the stream
and the `<video>` and `web/cameras.rs` samples that element.  `AddCameraCue`
and `RelinkCamera` are pure engine work and read line for line like
`vidiotic::app::cameras`; `RefreshCameras` and `SetCameraOnAir` become requests
to the page, because enumeration is async and starting a camera can be refused.

**The canvas readback is deliberate.** `Source::poll_newest` returns a
`DecodedFrame` — CPU-side pixels — so the element is drawn into a 2-D canvas
and read back. `copyExternalImageToTexture` would be cheaper and would put a
GPU upload path behind a trait whose whole contract is "hand me a frame", then
need a second one for every clip that is not a camera. The frame arrives as
`PixelData::Rgba`, which is what the software HAP path already produces, so
nothing downstream is new.

**Two facts about enumeration that shape the UI.** `enumerateDevices` lists
cameras before permission is granted but with **empty labels** — a privacy
rule, not a bug — so a first listing is positional and the page re-enumerates
once a stream is granted. And `deviceId` is the uid a `.viproj` stores: stable
per origin, which is what lets a saved camera cue find its device again, and
why it does *not* survive being opened on someone else's machine, where the row
goes missing and offers a relink.

The element is parked off-screen rather than hidden, because a hidden element
is one a browser may stop driving and a video that stops presenting is a cue
that freezes. Switching a camera on drops the open sources for its cues so it
lights immediately rather than at the next turn of the rotation.

**Tested for real, not mocked.** Chrome's `--use-fake-device-for-media-stream`
is a genuine `MediaStream` through the genuine API, so the smoke drives the
whole chain — enumerate, `getUserMedia`, element, readback, composite pass —
and reads a lit pixel off the output head. The cue is added *before* the device
is switched on, which is the harder order: it must be a blank cue rather than a
broken rotation, and switching on has to reach it.

One harness lesson worth keeping: a headless Chrome holding a live capture
track does not reliably go down on SIGTERM, and one left behind owns the
debugging port — so the *next* run fails to attach for a reason that has
nothing to do with the code under test. Cleanup is SIGKILL.

---

## 6. Deferred: YouTube

Not in the first cut. Recorded here so the source layer is designed for it.

**Video is hard.** Browsers cannot obtain YouTube video bytes: no CORS on the
stream endpoints, and the IFrame Player API is sealed — play/seek, never pixels.
Any URL-paste flow requires a server-side yt-dlp ingest, which means a backend,
storage, bandwidth, and ToS §5(B) exposure. Hence "file now, proxy later": the
`/chop` source layer should be a trait with a local-file implementation, so a
remote implementation can land without touching the editor.

~~**Audio is easy, and worth doing early.**~~ **Done — §8 step 4f**, and it was.
Audio *reactivity* needs a signal, not an asset, so none of §6's video problems
apply: `getDisplayMedia({audio: true})` captures a tab's audio behind a consent
prompt, with the microphone as the fallback, and neither needs a download, a
proxy, or an answer about anyone's terms of service.

---

## 7. Hosting

**Any static host serving wasm + js + html.** §3c settled this: single-threaded
BC1 is fast enough at the §3a tier, so there are no threads, no SharedArrayBuffer,
and therefore no COOP/COEP requirement. GitHub Pages is back on the table, and
COEP no longer conflicts with embedding anything cross-origin.

The host it went to is fubarchitect.com, as a warez — see the end of this
section for what a strict site CSP costs a wasm page, and what had to change.

**Storage, as built (step 4c):** the clip goes into OPFS as a single
`clip.mov`, and the session — tempo and selected effect — into `localStorage`
beside it. Two stores because they are two shapes: megabytes of opaque bytes
with a write API built for exactly that, and a handful of numbers that should be
readable by a human in devtools. Both are best-effort; private-browsing modes
deny one or both, and a denied store is a session with no persistence, not a
failed boot.

`navigator.storage.persist()` is now asked for, once, immediately after the
first clip is actually written — Firefox raises a permission prompt for it, and
prompting before there is anything to protect asks a question the visitor has no
way to answer. Chrome decides silently from site engagement and may refuse; that
is not a failure, the clip is stored either way, just evictable. The page logs
which answer it got.

Still outstanding: the page stores exactly one clip, because there is no
cue/bank model yet to store more *into* — the moment `.viproj` lands (step 1)
this becomes a project directory rather than a file.

Ingest changed what is *in* that store: it is now a clip the page baked, not one
a native tool did (§8 step 4e), so the bytes in OPFS are Hap1 either way and
nothing about the storage path had to change.

**The page must be served over https:// or localhost.** WebGPU is not exposed in
an insecure context, so a plain-`http://` deploy is not a degraded /play, it is
no /play — the page now detects this before the first click and says so.

### The release artifact (`scripts/release-play.sh`)

`build-play.sh` produces something that runs over localhost. `release-play.sh`
produces something that survives being on the internet, which is a different set
of problems:

| problem | what it does |
|---|---|
| 6.3 MB re-downloaded every visit | the bundle goes in `pkg-<hash-of-its-own-contents>/`, declared `immutable`; `index.html` names that directory and is declared `no-cache`. A new build gets a new name, so there is no cache to bust and no way to serve mismatched glue and module. |
| you cannot tell what is deployed | a `<play-rev> <UTC date>` stamp is substituted into the page, logged on load, exposed as `__vidiotic.build`, and written to `version.json`. `-dirty` is recorded, because by the time the build is in front of a projector nobody remembers. |
| hosts disagree about everything | a generated `nginx.conf` (the chosen target), `_headers` (Netlify, Cloudflare Pages), `.nojekyll` (GitHub Pages). All read the same directory. `nginx.conf` is generated rather than written by hand so that a redeploy with a new bundle name does not need anyone to remember to edit the server config; it is meant to be `include`d. |

The *directory* is hashed rather than the files in it, because wasm-bindgen's JS
resolves the `.wasm` against its own `import.meta.url` — move them together and
the reference stays correct without editing generated code. The hash covers both
files, so a wasm-bindgen upgrade cannot ship new glue under an old immutable URL.

Both substitutions assert they matched exactly once; a silent no-match ships a
page that 404s its own module, and the only symptom is a blank screen.
`play-smoke.mjs --dist` then drives the assembled artifact rather than the dev
tree, so the two substitutions — the one part of the release no compiler checks
— are verified from the outside.

**Measured (2026-08-02, `pkg-4fd58971795f`):** 6473 KiB raw, **2721 KiB gzip,
2141 KiB brotli**. The download a visitor actually pays is 2.1 MB, not 6.3 —
quoting the uncompressed figure overstates it threefold. `.br` and `.gz` are
emitted beside the wasm for hosts that serve precompressed files off disk
(nginx `brotli_static`); Netlify, Cloudflare and GitHub Pages compress on the
fly and ignore them.

### The rehearsal (`scripts/serve-play.sh`)

`python3 -m http.server` answers "does the page work" and is blind to everything
the *deployment* can get wrong. So `docker/play/` is an nginx container serving
the real `dist/play` under the real generated config, and `serve-play.sh` asserts
what comes back out of it.

It earned its keep before it finished being written. The first generated
`nginx.conf` opened with `types { application/wasm wasm; }` at server level, on
the reasoning that older nginx does not know `.wasm`. **An nginx `types` block
does not extend the MIME map, it replaces it.** Measured, by putting the
original config back into the container:

```
with a server-level types{} block:
Content-Type: application/octet-stream      # index.html
Content-Type: application/octet-stream      # vidiotic_play.js
```

Chrome refuses a module script with a non-JavaScript MIME type outright, so the
page would have been dead on arrival, with a console error naming the module
rather than the server. It would have shipped: nothing local reproduces it, and
the directive looks obviously correct.

The premise was wrong too — nginx has carried `application/wasm wasm;` in stock
`mime.types` since 1.21.4 (confirmed in 1.26.3). What ships now is an
exact-match `location` for the one `.wasm` file, where replacing the map for a
single known file is harmless, and which older nginx still needs.

What the container asserts, all currently green:

- `index.html` is `text/html` and `no-cache`; the glue is JavaScript;
- the module is `application/wasm` — the wrong type silently costs the browser
  its streaming compile;
- the bundle is `max-age=31536000, immutable`;
- `Accept-Encoding: br` returns **2141 KiB precompressed**, `gzip` **2721 KiB**,
  and each is decompressed and compared against `version.json`'s `wasm_sha256` —
  a truncated precompressed file is otherwise served happily, with correct
  headers and wrong bytes, under a year of `immutable`;
- the server's own security headers survive every location that sets a
  `Cache-Control`;
- `/nginx.conf` and `/_headers` 404 — deploy metadata is not site content;
- the same config also serves under `listen 443 ssl`.

Then `play-smoke.mjs --url http://localhost:8080` drives that server rather than
a static one: **25 checks, all passing**, against the same headers and
brotli-compressed body a visitor gets. That is the strongest form this check has
taken.

Alpine + `apk` rather than the official nginx image, for one reason: the brotli
module has to be built against the same nginx, and Alpine packages a matched
pair. TLS is served on the second port with a self-signed cert — a browser
refuses it, but `nginx -t` and `curl -k` do not, and `listen 443 ssl` is a
different path through the config than `listen 80`.

The one thing localhost cannot rehearse is a real certificate. It does not
change what the page does: localhost is a secure context regardless of scheme,
so WebGPU behaves identically.

### What an adversarial audit of all this found

A subagent was given docker, curl and a brief to break the release path before
it was used. Eleven findings; the ones that mattered, and what they cost:

| | found | fixed by |
|---|---|---|
| **fatal** | A truncated `.br` — interrupted compress, full disk — is served by nginx with a perfectly correct `Content-Encoding` under a year of `immutable`, and *every gate passed it*. Reproduced by truncating the file to 800 KiB: `[ OK ] br: served precompressed, 800 KiB on the wire`. | `release-play.sh` now round-trips every compressed file through `cmp` before letting it into the artifact, and `serve-play.sh` decompresses what nginx returns and compares it against `version.json`'s `wasm_sha256`. Both directions verified: the release refuses to publish a bad `.br`, and the rehearsal now fails on one. |
| **fatal, delayed** | A **subdirectory deploy** (`https://box/play/`) silently no-ops the entire generated config. `location` patterns are absolute request paths, so none match; everything still returns 200 and **the page boots perfectly**. Measured: 3x the download, no `Cache-Control` anywhere, deploy metadata public, MIME override inert. The delayed kill is losing `no-cache` on `index.html` — the browser heuristically caches it, and the next `--prune` deletes the bundle that cached page names. The obvious operator workaround (wrapping the include in `location /play/`) makes nginx refuse to start: *"location is outside location"*. | `release-play.sh --base /play`, threaded through every `location` and every `_headers` path. `serve-play.sh --base` rehearses it for real — the artifact mounted at that position in the document root, not aliased — and refuses to serve an artifact whose `version.json` records a different base. Full browser smoke passes at `/play/` as well as `/`. |
| **degrades** | Every `add_header` here **discards the server's own header set** — HSTS, `nosniff`, `X-Frame-Options` all gone on exactly the paths that set a `Cache-Control`, `always` no help. Same class as the `types{}` bug, and the rehearsal was structurally blind to it because `docker/play/nginx.conf` had no server-level headers to lose. | Every generated location now `include vidiotic-headers/*.conf;` — resolved against nginx's own prefix, and a glob matching nothing is not an error, so an empty deployment is fine. The operator writes their headers once. The container ships a representative set so the rehearsal reproduces the real box, and `serve-play.sh` asserts they survive on all four paths. |
| **degrades** | The **dry run failed where the real deploy succeeds** — `rsync -n` never creates the target directory, so phase 2 had nowhere to land. That is the first deploy, along the exact path the docs tell you to walk. | The probe reports whether the directory exists, `--go` does `mkdir -p` first, and the dry run says which phase it is skipping and why. |
| **degrades** | A failed release **destroyed the previous good `dist/play`** — `rm -rf` ran before anything could fail. | Assembled into `dist/play.tmp` and moved into place last. Verified against both a broken substitution and a broken brotli: the previous artifact survives intact and no temp tree is left behind. |
| **degrades** | A **saved session from an older build permanently half-boots the page**. `set_effect` throws on an out-of-range index, and the exception landed in the boot `catch` *after* the engine was running: the page looks booted, but the stored clip is never restored, autosave never registers, and **the `ResizeObserver` never installs** — so dragging the output head to a projector leaves its swapchain at the old size. Permanent, since nothing cleared the key. Ships the moment a build has one fewer effect. | The restore is guarded and drops the session on failure. Verified in a real browser with a seeded `{"effect":99}`: status `running`, bpm still applied, key removed, and the autosave interval rewrites a valid session 2.5 s later — proof boot ran to completion. |
| **degrades** | `brotli_static` is baked into a file **regenerated every release and rsynced with `--checksum`**, so an operator's hand-commenting is overwritten on every deploy and the next restart fails. | `--no-brotli`, recorded in `version.json` so the rehearsal knows not to fail on its absence. |
| **cosmetic** | `index.html` and the glue were never compressed — nginx's default `gzip_types` is `text/html` only. ~88 KB per cold visit. | Both precompressed and verified like the wasm. |
| **cosmetic** | A **stale bundle** awaiting `--prune` fell outside the config: served uncached and uncompressed, i.e. the grace period cost 3x. | The bundle location is now a quoted regex on the hash shape rather than this build's exact name, so the previous bundle keeps its headers and its `.br` for as long as it is on disk. Measured identical to the live one. |
| **cosmetic** | "since 1.21.4" overstated — wasm is in `mime.types` in **1.21.0** (absent in 1.20). | Corrected to "at least 1.21.0", with the measurement. |
| **cosmetic** | macOS ships **openrsync**, which has no `--protect-args`, so a remote path with a space re-splits in the remote shell. | Documented; nothing on this side can fix it. |

Clean bills, each attacked and found sound: two-phase ordering (the mid-deploy
remote state genuinely has no `index.html`), `--checksum` honoured by openrsync
and by GNU rsync 3.4.3 on Linux, all four probe branches, `--prune` refusing to
touch the live bundle or a non-bundle directory, hash stability and sensitivity,
both substitutions failing loudly, brotli winning the negotiation for every real
browser `Accept-Encoding` string, and the page itself being path-clean at
`/play/`. One caveat recorded rather than fixed: OPFS and `localStorage` are
per-**origin**, so two copies at different paths on one host share `clip.mov`.

### Deploying (`scripts/deploy-play.sh`)

**Target: a server of one's own, over rsync.** The artifact is relative-path and
server-side-free, so the choice stays reversible; `_headers` and `.nojekyll` are
still emitted for that reason.

Content-hashed bundle names make caching safe and also make *deploy order*
matter, because the page names a directory that has to exist before the page
does. So the transfer is two passes and a sweep:

1. everything except `index.html` — the new bundle lands **alongside** the old
   one, and nothing references it yet, so the live site is untouched;
2. `index.html` alone — one small file, and the site flips at the moment it
   lands. Push it first instead and the live page points at a 6 MB directory
   that is not there yet for the length of the transfer;
3. `--prune`, separately and only when asked — remove the bundle directories
   nothing points at. Separate because a tab that loaded the old page thirty
   seconds ago may still be fetching the old bundle; sweeping is a thing to do
   on the *next* deploy.

Dry run unless `--go`, and it refuses a `dist/play` whose stamp still reads
`dev` — that would be a copy of `web/`, without the caching or identity the
deploy relies on. Target comes from an argument or `$VIDIOTIC_PLAY_TARGET`.

The dry run also **probes the server over ssh**, read-only, for the one question
that cannot be answered from here: the generated config turns on `gzip_static`
and `brotli_static`, and an nginx built without either refuses to start on the
unknown directive. A *reload* survives that — the old config keeps serving — but
a *restart* does not, and finding out during a restart is finding out with the
site down. The probe reports the nginx version and whether each module is
available, on disk but unloaded, or absent, and says which line to comment out.
Both branches verified against containers with and without the module.

Verified end to end against a local directory standing in for the server:
dry run transfers nothing, `--go` lands a tree byte-identical to `dist/play`,
`--prune` without `--go` lists the stale bundle without deleting it, and with
`--go` removes exactly that one while the live bundle and the page pointing at
it stay put.

### Where it actually went: a warez on fubarchitect.com

`deploy-play.sh` is not the route taken, and the reason is worth recording. The
box turned out to be one that already exists — a Debian 13 VPS running a
hand-rolled Ruby SSG whose `warez/` entries are **copied verbatim** into the
site. A self-contained relative-path artifact is exactly what that wants, so
`/play` ships as `warez/08-vidiotic/` in the `webb` repo and rides the site's own
`just push`. The rsync deploy stays for a server of one's own; nothing about the
artifact had to change to suit either.

What did have to change is the policy around it. The site's CSP is
`script-src 'self' 'wasm-unsafe-eval'` with no `media-src`, and under it `/play`
does not degrade — it does not run. Three stops, each observed in a container
before anything was written:

1. **the boot script was inline.** 657 lines in a `<script type="module">`, and
   there is no `'unsafe-inline'`. Nothing executes; the page is a heading and a
   paragraph. Fixed in the page rather than the policy — `web/boot.js` is now a
   file, it joins the wasm and the glue in the content hash (a stale copy against
   new glue is a page that 404s on a symbol), and `deploy-play.sh` looks for its
   `dev` stamp there instead of in `index.html`.
2. **`media-src` is unset**, so it inherits `default-src 'self'` and the `<video>`
   that bakes a dropped clip refuses its own `blob:` URL. Every drop — the
   headline feature — reports *"this browser cannot decode it"*, blaming the
   browser for a decision made in an nginx file.
3. **`microphone=()`** site-wide is a flat denial that outranks any prompt:
   `getUserMedia({audio})` rejects without ever asking, so the fallback when a
   visitor declines tab-audio sharing has no route at all.

Plus two things that were merely expensive: `application/wasm` is not in the
site's `gzip_types` and there is no `gzip_static`, so the module shipped **raw at
6,892,218 bytes**; and the site's aggressive-caching rule is nested under
`location /`, which a prefix match on `/warez/vidiotic/` never reaches, so the
bundle had no `Cache-Control` at all.

**Rehearsed before it was written.** `docker/fubar/` is fubarchitect's serving
stack reconstructed from its own Ansible templates — Debian 13, nginx 1.26.3,
the same header set, the same `limit_req` zones, and the gzip deficiency
deliberately preserved. `scripts/stage-fubar.sh` builds the real `_site`, drops
the release into it and runs both curl assertions and the full browser smoke.
`--strict` is the counterfactual: with the site policy and no warez block, 5
passed / 8 failed and the page never booted. With the block, **14 passed / 0
failed and a full browser smoke pass at `/warez/vidiotic/`**, in-browser baking
and the audio path included. `gzip_static` alone took the module to 2,888,696
bytes on the wire; the `.br` beside it (2,264,940) waits for an `ngx_brotli` the
box does not have.

The block is four `location`s — the path itself, a regex on the hashed bundle
(`gzip_static` and a year of `immutable`), an exact match on `index.html`
(`no-cache`, because it names the bundle and a stale copy boots into a 404), and
a 404 on `nginx.conf`/`_headers`. Each repeats the full header set for the same
reason `/warez/basalt/` does: one `add_header` in a location discards every
inherited one, `always` included. It is now in `webb`'s `vhost.conf.j2`, and the
rendered production config passes `nginx -t` on the real image — verified
directive-for-directive identical to the copy the smoke test passed against.

**One thing is reasoned rather than observed.** The `blob:` in `script-src` and
`microphone=(self)` are read off `boot.js`, not seen working: `play-smoke.mjs`
drives the analyser through `push_audio` with a synthesised tone, because
`getDisplayMedia` needs a consent prompt no headless run can answer, so the
AudioWorklet is never constructed under a policy at all. One human click on
"Listen" against the staging container settles it. Until then those are the least
certain lines in the config.

**~~Blocking a build from a clean clone:~~ fixed.**
`vidiotic-play/src/web/input.rs:188,190` calls
`vidiotic_ctl::keys::from_character` and `::from_named`, and at `vidiotic-ctl`
`b50d535` `src/keys.rs` exported only `canon` — both functions lived in a
191-line uncommitted diff, so a fresh checkout of all eight repos did not
compile `/play`: no CI, no second machine, no reproducing a release from its
stamp. `3df9e2d` ("Give keys.rs one name table and three ways in") commits both.

What is *not* fixed is the same hazard elsewhere: `phosphor`, `vidiotic-prep`
and `vidiotic-wire` all still carry uncommitted changes the workspace needs —
`phosphor`'s committed `Cargo.toml` declares its own `[workspace]`, so a clean
clone of it is not a member of this one and `cargo` refuses the tree outright.
This is a recurring shape, not an incident: eight repos, one workspace, and
nothing that checks the committed state assembles.

Should `High`/ClusterFit ever need to be the default bake — it is 6.3x slower
than realtime single-threaded (§3c) — threads or a GPU compute compressor come
back, and with them COOP/COEP, the loss of GitHub Pages, and a conflict with
cross-origin embeds. That is a strong reason to keep `Draft` the default.

---

## 7a. Tests that carry the port

The port is executed against tests written in the **native** tree first, so each
one passes today and keeps passing as code crosses to wasm. Two exist.

### `scripts/wasm-gate.sh` — the ratchet

Declares every crate/feature combination with its expected wasm32 state and
fails if reality disagrees **in either direction**: a PASS row that breaks is a
regression, and a FAIL row that starts building means the table is stale and
must be moved forward. A crate cannot quietly become portable without the gate
being updated to say so. For FAIL rows the recorded note is the actual blocker,
which makes the table the port's work list.

Measured 2026-08-01, after step 4c:

| Crate | Features | State | Blocker |
|---|---|---|---|
| `vidiotic-wire` | `--no-default-features` | **portable** | — |
| `phosphor` | `--no-default-features` | **portable** | — |
| `vidiotic-ctl` | `--no-default-features` | **portable** | — |
| `vidiotic-core` | `--no-default-features` | **portable** | — |
| `vidiotic-bake` | `--no-default-features` | **portable** | — |
| `vidiotic-play` | `--no-default-features` | **portable** | — (the render core, the clip read path, the engine, and the web shell) |
| `vidiotic-wire` | `--features client` | blocked | `std::os::unix` at `client.rs:11`; web transport is `BroadcastChannel` (§10) |
| `vidiotic-core` | `--features ffmpeg` | blocked | `ffmpeg-sys-next` build script — *this row is why the feature exists* |
| `vidiotic-bake` | `--features ffmpeg` | blocked | bindgen/ffmpeg via `transcode.rs` — likewise |

The two ffmpeg rows are deliberately kept as expected-FAIL. They are not a work
list any more; they are the assertion that the feature boundary is real. If
either ever builds, ffmpeg stopped being the thing that separates the halves and
the split needs re-justifying.

The gate also runs a second section: **the portable test suites under wasm32, in
V8.** Building for wasm proves the portable half *compiles*; this proves it
*behaves*. 176 tests, executed via `wasm-bindgen-test-runner` in Node:

| Suite | Tests | What it settles |
|---|---|---|
| `vidiotic-bake --lib` | 39 | `frame` + `hap` + `mov` in wasm, and the ingest tier |
| `bc1_golden` | 5 | **BC1 bytes identical to native** |
| `hap_conformance` | 6 | Hap1 decode of real packets, in wasm |
| `vidiotic-core --lib` | 29 | project / isf / time model |
| `vidiotic-play --lib` | 97 | **GLSL→naga compilation in a browser**, the clip timeline end to end, the software BC fallback, the audio analyser, and the engine: grammar, clock, sequencer, undo, cue rotation |

That last row is three claims. `builtin_effects_compile` pushes all ten shipped
effects through preprocess → naga parse → validate under wasm, which is what the
shader editor's survival rests on; it replaced a `read_dir` scan and is stronger
than what it replaced, because it covers exactly what `include_str!` bakes into
the binary rather than whatever is sitting in a directory. The `clip` tests build
a real HAP `.mov` in memory with `MovWriter` over a `Cursor` and walk its
timeline — no filesystem, so the same bodies run in V8 and natively. The
`softdec` tests (step 4c) are the third: BC1/BC3/BC4 blocks hand-built to hit
each encoding mode, plus the `video()` transforms the shader would otherwise
have done — because on the fallback path it never gets the chance.

Each test module aliases `#[test]` to `#[wasm_bindgen_test]` under `wasm32`, so
this runs the *same test bodies* as the native suite — not a parallel copy that
could drift. Requires `wasm-bindgen-cli` at the version in `Cargo.lock`; if it
is missing the gate says so and exits **non-zero**, because an unrun gate is not
a green gate.

Each row also declares a **minimum test count**, and this is the load-bearing
part. Drop the alias from a module and its tests compile away to nothing, and
the runner reports `no tests to run!` — which is an exit code of zero and looks
exactly like success. `vidiotic-core` did precisely this on the first run: 0
tests, reported green. A minimum catches that. It is deliberately a *minimum*
rather than an exact count, because an exact count is the same trap as a
hardcoded variant count — it fails for the wrong reason every time a test is
added. The detector is self-tested by temporarily raising a minimum and
confirming the gate goes red.

### `vidiotic-bake/tests/` — conformance

| File | Role |
|---|---|
| `gen_fixtures.rs` | `#[ignore]`d generator. Lifts 9 real HAP packets from `clips/` plus `goldens.tsv`. **The only fixture code that touches ffmpeg.** |
| `hap_conformance.rs` | 6 tests. Pure Rust over committed bytes, so it runs unchanged after the demuxer swap and under wasm. |
| `bc1_golden.rs` | 5 tests. Pins BC1 output bytes for both algorithms over a procedurally generated frame. |
| `frame.rs` unit tests | 6 tests, in-crate. Cover `FrameBaker` — alignment, frame-size rejection, scratch reuse, and the bake→decode round trip. |
| `mov.rs` unit tests | 22 tests, in-crate, no ffmpeg. 9 for the writer, 13 for the reader. `boxes_tile_the_file_exactly` is the strongest cheap check available for the writer: right lengths ⇒ the boxes cover the file with no gap or overlap. The reader's half builds synthetic files in shapes `MovWriter` cannot produce — many samples per chunk, uniform `stsz`, `co64` — plus the malformed cases (truncated download, box overrunning its parent, sample pointing outside the file, non-video track). |
| `mov_roundtrip.rs` | 7 tests, `ffmpeg`-gated. Writes with our muxer and demuxes with **ffmpeg's**, asserting byte-identical packets, codec tag, dimensions, timing, keyframe flags, and that in-memory output equals on-disk. |
| `mov_demux.rs` | 6 tests, `ffmpeg`-gated. The mirror image: reads with **our** demuxer and checks against ffmpeg's, on files from both muxers. |
| `bake_integrity.rs` | 5 tests, `ffmpeg`-gated. Runs a real bake of `clips/bun.mov` and asserts the file contains every frame the bake reported, on the timescale it asked for, with every frame the same exact duration — and that the source's damaged `avg_frame_rate` is not inherited. |
| `transcode.rs` unit tests | 6 tests, in-crate, `ffmpeg`-gated (the module is). Pure logic: the frame-rate picker against both real traps, and the timescale derivation including degenerate rates. |

### `scripts/play-smoke.mjs` — the browser check

The gate proves the portable half behaves in V8, but V8 has no GPU and no
second document. Everything that makes `/play` `/play` — a device spanning two
realms, a swapchain, a canvas being composited — is invisible to every test
above. This is the check for that layer, and it is `bake_integrity.rs`'s
argument applied to the browser: *nothing had ever read the output back.*

Step 4b added a third claim to it, for the same reason the first two exist. The
beat clock and the modal grammar have **no pixels of their own** on either head,
so every check above is blind to them — a screenshot cannot tell a running state
machine from a linked one. So the driver presses real keys through
`Input.dispatchKeyEvent` and reads the engine's tempo and grammar state back:
the beat must advance, a root key must open a modal, `b a` must resolve
`FocusPane(Clock)`, two taps must move the tempo off its 120 default, and
Escape must cancel a pending sequence.

Chrome over CDP, driven from Node with no npm dependencies (Node 22+ has a
built-in `WebSocket`; Chrome speaks CDP). It serves the repo, clicks Start with
`userGesture: true` so `window.open()` is not blocked, fetches `clips/bun.mov`,
and then asserts what cannot be inferred:

- both heads render **non-black** pixels, read back through
  `copyTextureToBuffer` — not "no exception was thrown";
- the two heads' pixels **differ**, which is what catches one surface being
  presented to both;
- the output *window* is composited, captured as a screenshot of **that CDP
  target** rather than inferred from the opener;
- the playhead advances across 700 ms;
- all ten built-in effects change the output, which is the only thing that
  exercises the seed + ping-pong path rather than `render`'s one-pass fast path.
  **Playback is paused first**, and the base pixel is re-read afterwards to prove
  the frame really was held. It was not, originally: each effect was compared
  against one base sampled at the start while the clip kept running, so the
  check measured the playhead as much as the chain and scored 7/10 about one run
  in four. The tolerance that hid it (`>= names.length - 2`) is gone — on a held
  frame the only thing that can move the centre pixel is the effect, so the bar
  is 10/10 and a failure names the effects that did nothing;
- **the software BC fallback paints the same picture as the GPU** (step 4c). The
  driver pauses, reads the centre pixel, flips to the CPU path with
  `set_soft_decode(true)`, and reads the same sample again. Not equality — BC1
  endpoint interpolation is implementation-defined — but within a rounding step;
  measured 2/255. This is the only check that runs the fallback in a browser at
  all, because every machine in this project has BC;
- **the clip survives a reload.** Navigate away, click Start again, and the clip
  must come back out of OPFS with the same frame count and the tempo it was left
  at — then `Forget stored clip` must actually empty it. A store nobody can
  clear is worse than no store;
- **the cue rotation turns** (step 4d). A second clip is ingested, the bank must
  go to two cues, and `current` must land on more than one of them over three
  seconds at 600 bpm. Like the beat clock, a cue swap has **no pixels of its
  own** — it looks exactly like a clip that happened to cut — so nothing above
  can see it.

  The first version of this check compared `current` at the start and end of the
  window and failed a working rotation, because two cues over that many phrase
  boundaries swap an even number of times and land back where they started. It
  samples across the window now. Worth recording because the failure mode is
  generic: **an endpoint comparison cannot observe a cycle.**
- **a video nobody baked plays** (step 4e). The one check that speaks to whether
  this is deployable at all: fetch a VP9 `.webm`, assert it does *not* probe as
  Hap1, bake it in the browser, assert the bytes now do, put them through the
  page's own ingest path, and read a lit pixel off the output head while that
  cue is on air. Baking bytes is not the claim — playing them is.

  VP9, not H.264: a Chromium build without proprietary codecs cannot decode
  H.264, and a check that passes for the author and fails for a reviewer is
  worse than no check. The asset picks the codec every build can decode, not
  the only one the feature supports.

- **the reactive effects react** (step 4f). A 1 kHz tone is pushed through
  `push_audio` and `lvl` must rise from zero — then, when the tone stops, fall
  back to it. The second half is the one that found a real defect: a source that
  stops delivering used to latch every reactive shader at its last value.

  Synthetic on purpose. The capture is the only part of the audio path that is
  *not* shared with the native player, and it is also the only part that needs a
  consent prompt a headless run cannot answer. What this exercises is everything
  after it, which is the code both platforms run.

  This is also the check that found the hidden-document media stall in §3d,
  twice — once as rVFC silently capturing nothing, and once as a media element
  that never loads. Both looked like working code. The driver now passes
  `--disable-backgrounding-occluded-windows` and its siblings, because the
  output head makes the control page permanently non-frontmost in headless,
  which a visitor looking at their own control window never is.

`--dist` repoints all of the above at `dist/play/`, the artifact
`release-play.sh` assembles, and adds two checks that only mean anything there:
the page carries a real build stamp rather than `dev`, and it loads its module
from a content-hashed directory. Those two substitutions are the only part of a
release that no compiler checks, and both fail as a blank screen.

It found the §10b realm bug on its first run, in a place inspection had already
signed off. That is the argument for it existing.

**The driver serves the repo root, and it used to do so silently.** The server
is spawned with `stdio: 'ignore'`, so a leaked one from an earlier run kept the
port, `http.server` exited with "Address already in use" into the void, and
Chrome loaded *the other checkout*. That is the worst failure a test harness
has available: it graded a stale tree and printed `SMOKE PASS`, and the tell was
a check reporting a field as `undefined` that was demonstrably present in the
built wasm — which reads as a wasm-bindgen mystery and is nothing of the sort.
It now refuses to start when the port is taken. A driver that tests the wrong
tree is worse than one that does not run, and in a worktree — where a sibling
checkout of the same repo is a directory away — this is not a remote hazard.

**Two of these checks are load-sensitive, and it reads as a code failure.** The
decay half of the audio check and the tap-tempo check both sample the engine
after a wall-clock wait, so a machine under load — several agents, a Docker VM
at half a core — starves the render loop and they sample stale frames. The
decay one is the confusing one, because it fails *upward*: the reported `lvl`
comes back **above** the tone's own peak (0.93 against 0.270), which is the
attack envelope still climbing, not a latched level. That is the opposite
signature from the defect the check exists to catch, and worth recognising
before spending an afternoon bisecting one's own work for it. The way to
attribute it is to re-run a **known-good artifact** rather than to reason: an
untouched pre-session build failing the identical check settles it in one run.

`hap_conformance` is the regression lock for §8 step 2 and §4's decoder rewrite:
`decodes_to_recorded_goldens` pins byte-exact output so replacing ffmpeg's
demuxer can be proven not to change a thing. The goldens are a lock, not an
oracle — they came from the decoder under test — so two independent invariants
sit alongside them: `decoded_size_matches_clip_dimensions` (115 200 bytes =
640×360×0.5, forced by BC1 geometry, not by `hap.rs`) and `clip_pool_is_all_bc1`.

`bc1_golden.rs` exists because §3c measured wasm *throughput* but never checked
wasm *output*. A clip baked in `/chop` and one baked natively must be the same
file. **Resolved: it does.** The same hash constants
(`0x9898_27ec_2950_51a0` RangeFit, `0xaf41_fbe9_b595_b2c1` ClusterFit) pass
under `wasm32-unknown-unknown` in V8 as natively — so `texpresso` output is
byte-identical across targets, and a browser-baked clip is the same file as a
natively-baked one. That was the last unverified assumption underneath §3.

It has already earned its keep once. The gate runs it **both with and without
rayon**, because the web build compresses single-threaded and the desktop build
does not — and until that ran, "same bytes either way" was an assumption. It
holds: identical hashes, 1.58 s parallel vs 7.46 s serial. BC1 output does not
depend on thread count, so byte-identity between a browser bake and a native
bake survives the thread-count difference between them.

`mov_roundtrip.rs` is built on a deliberate choice: **the component being
replaced is the one that certifies its replacement.** Structural self-checks can
only show a file is internally consistent, and a consistently wrong file is
still wrong. Handing the output to ffmpeg's demuxer is what makes the result
mean something. It found a bug immediately — see the Observations.

`mov_demux.rs` applies the same principle in reverse, and adds the ingredient
the write side could not have: **files nobody on this project laid out.** Our
writer and our reader share an author, so agreeing with each other proves less
than it appears to. The clips in `clips/` were muxed by libavformat — 99 samples
across 7 chunks with a five-run `stsc`, where `MovWriter` writes one sample per
chunk and a single run — so reading them correctly is evidence about the format
rather than about our own habits. `every_located_sample_decodes_as_hap` then
closes the loop by running every located sample through `hap::decode_frame`,
which is the entire `/play` read path: bytes → container walk → BC1 payload of
exactly the size the dimensions imply. A sample-table error that happened to
preserve lengths still fails there, because a mis-addressed packet is not a
valid HAP section.

It also found something on its first run — the zero-duration tail frame in the
Observations, which corrected the earlier account of the muxer bug.

---

## 8. Sequence

0. ~~Benchmark texpresso BC1 at the target tiers.~~ **Done — §3c.** Single-threaded
   wasm `Draft` runs ~2.7x faster than realtime at 848×480. No threads, no
   COOP/COEP, no GPU compute compressor. `High` stays opt-in.
1. ~~**`.viproj` bundle-relative paths.**~~ **Largely already done, and the
   remainder is now named exactly.** `vidiotic-core::project` stores clip
   references as relativized *strings*, and running its tests under wasm (§7a)
   pinned the gap precisely: 29 of 33 pass unmodified. `relativize` and
   `resolve_path` take an explicit `project_dir` and are already portable. The
   four failures are all one thing — **`absolutize` is the only function that
   reaches for process state** (`canonicalize` / `std::path::absolute`, both of
   which need a cwd), plus `save`/`load` doing file I/O. So step 1 is: give
   `absolutize` an explicit base instead of the process cwd, and put save/load
   behind OPFS. Those four tests are `#[cfg(not(target_arch = "wasm32"))]` with
   that reason recorded; they should lose the gate when this lands, not keep it.

   **Done, and the gate went 29 → 33.** The three functions that reached for an
   OS now ask for one: `absolutize` takes its base, and `resolve` /
   `relink_by_root` take an `Fs` — two methods, `exists` and `walk`, which is
   the whole of what loading a project wanted from a filesystem. `NativeFs`
   implements it with `std::fs` and is `cfg`'d *off* on wasm32, because
   `std::fs` compiles there and then fails at runtime: without that gate a
   browser build would have loaded a project reporting every clip missing
   rather than failing to compile. The parsing half of `load` came out as
   `from_ron_versioned`, which is what OPFS calls once it holds the bytes;
   `save`, `load` and `gather` stay native, since what remains of them *is* the
   file I/O.

   **Dropping `canonicalize` is a fix, not a concession.** Only one side of the
   `strip_prefix` in `relativize` was ever canonicalized, so wherever the
   project directory was reached through a symlink — `/var` → `/private/var` on
   macOS, which is where every temp directory lives — the prefix stopped
   matching and the clip path was silently stored absolute. Lexical on both
   sides is portable *and* agrees with itself.

   **The four ungated tests earned their place immediately.** One failed in V8
   while passing natively: **`Path::is_absolute` is `false` for every path on
   `wasm32-unknown-unknown`**, because it is `has_root() && (cfg!(unix) ||
   windows prefix)` and that target is neither. Three portable call sites
   branched on it — `absolutize`, `resolve_path`, and `render.rs`'s ISF texture
   lookup — and every one of them was correct only because `PathBuf::push`
   checks `has_root` separately and threw the wrongly-joined base away again.
   Two bugs cancelling is not a design. They say `has_root` now. Nothing short
   of running these tests in a browser would have found it; inspection had
   already signed all three off.
2. ~~**Standalone MOV muxer** replacing ffmpeg in `vidiotic-bake/transcode.rs`.~~
   **Done — `vidiotic-bake/src/mov.rs`.** Pure Rust, ~430 lines, portable, and
   `transcode.rs` now uses it: ffmpeg's role in the bake is **decoding only**.

   It turned out to be less code than the workaround it replaced. libavformat has
   no HAP encoder, so the old path had to `add_stream(Id::HAP)` to get a
   null-codec stream and then reach through `unsafe` to fill in the codec
   parameters by hand. Writing the boxes directly is both shorter and honest
   about what it is doing.

   **This step found a shipping bug — see the Observations.** ffmpeg's mov muxer
   was dropping the last frame of every bake.

   Not needed after all: a *demuxer*. `/chop` gets its frames from WebCodecs,
   which is the browser's job, not Rust's.
3. ~~**wasm feature-gating** of `vidiotic-core` and `vidiotic-bake`.~~ **Done.**
   Both now cross the gate under `--no-default-features`. ffmpeg turned out to be
   confined to exactly one file per crate — `clippool.rs`'s thumbnail decode and
   `transcode.rs` — so each got an `ffmpeg` feature, on by default, and no native
   caller changed.

   The one real piece of work was that `transcode.rs` also held `BakeQuality` and
   the BC1 loop, which is precisely what the *web* baker needs. Gating the file
   would have stranded it. So the per-frame bake was extracted to
   `vidiotic-bake/src/frame.rs`: tight RGBA in, Hap1 packet out, no container and
   no OS. `transcode.rs` now drives `FrameBaker` rather than duplicating it,
   which is what makes web/native byte-identity structural instead of aspirational
   — there is one implementation, and both callers go through it.

   `texpresso/rayon` became its own feature at the same time. It is a *runtime*
   hazard, not a build one: rayon compiles for wasm32 and then panics when it
   tries to build a thread pool in a browser with no workers. §3c already
   concluded the web baker wants single-threaded; the feature makes that a
   compile-time fact rather than a deployment surprise.

   Still to gate: `vidiotic`/`vidiotic-prep`'s fs/IPC/capture/Link surfaces, and
   `vidiotic-wire --features client`.
4. ~~**`/play` first, not `/chop`.**~~ **P0 done — §10b.** A native-baked HAP
   `.mov` plays on two heads in a browser, through the unmodified composite
   pass, with an egui panel on the control head and all ten built-in effects
   running. What was *not* done at P0: no `.viproj`, no OPFS, no
   cues/banks/sequencer, no audio, no `BroadcastChannel`, no RGBA fallback, and
   no `app.rs`. P0 was scoped to the riskiest assumption and nothing else.
   (OPFS and the fallback landed in step 4c; the rest still stand.)

   **The enabling move was a crate split, and it is the reusable part.**
   `render.rs` (1,568 lines) turned out to be portable already — one
   non-portable line, `decode_still` for ISF `IMPORTED` images — but it lived in
   a crate that links ffmpeg, cpal, `rusty_link` and unix sockets. So the
   portable core moved to a new member, **`vidiotic-play`**
   (`crate-type = ["cdylib", "rlib"]`), which native `vidiotic` now depends on
   and re-exports exactly as it already did for `vidiotic-core` and
   `vidiotic-bake` — `crate::render::…` and `crate::shader::…` mean what they
   always did, and `app.rs` was never opened. What moved: `render.rs`,
   `shader.rs`, `gfx.rs`, `video/frame.rs`, and `shaders/` (the `include_str!`s
   go with the sources that bake them in). What is new there: `clip.rs` (the
   read path) and `web/` (the shell, `cfg(target_arch = "wasm32")`).

   Three things worth keeping from how it went:

   - **`decode_still` became an injected `StillLoader` closure**, not a feature
     flag. A flag would have put ffmpeg back in the new crate's manifest, which
     is the one thing it exists to avoid — and the closure is the shape the
     browser needs anyway, since image decode there is async and must be
     pre-fetched.
   - **`pollster` is a `cfg(not(wasm32))` dependency.** Blocking on a browser
     future compiles perfectly and then hangs; making the crate not *have*
     pollster on wasm turns that into a build error. Same lesson as
     `vidiotic-bake`'s `rayon` feature: a runtime hazard converted into a
     compile-time fact.
   - **Moving `shaders/` was the only change `cargo check` could not catch.**
     Four consumers: 12 `include_str!`s and two `read_dir` tests fail loudly;
     `assets.rs`'s `repo_shaders()` and `packaging/bundle.sh` fail *silently* —
     a wrong path just makes `default_shader()` return `None` and the app boots
     to a black screen that reads like a renderer bug. Both are now guarded: a
     unit test asserts the directory and `demo.frag` exist, and `bundle.sh`
     exits rather than shipping a bundle with no shaders.

   Still true: `/chop` is pointless if `/play` cannot run its output — and it
   now demonstrably can. ~~**Settle §9a's 8×8 grid before porting `ui.rs`.**~~
   **Settled and built — §9a.** `/play`'s panel is now on the grid face, which
   makes it the reference the native panels get rebuilt against rather than a
   thing that would have to be laid out twice.
4b. **The engine crosses.** `commands`, `grammar`, `sequencer`, `clock` and
   `undo` moved to `vidiotic-play`, and `/play` now has a live beat grid and the
   modal grammar driven from the keyboard. The clip is no longer the only thing
   running in the browser.

   **It compiled for wasm32 first try.** 2,187 lines, and the only changes were
   `web-time` in place of `std::time::Instant` and a `cfg` hiding `LinkClock`.
   Two things earned that, and both are worth naming because neither was luck:

   - **The `pub use vidiotic_core::{bank, chain, clippool, isf}` trick worked a
     second time.** `crate::bank::…` and `crate::isf::…` resolve identically in
     both crates, so `commands.rs`, `grammar.rs` and `sequencer.rs` moved with
     *zero* edits — the same mechanism that moved `shader.rs` in step 4.
   - **`vidiotic-ctl::keys` had already paid for the keyboard bridge.** It is
     deliberately winit- *and* egui-free, canonicalizing both toolkits' spellings
     onto one name, so the browser became a third caller of an existing contract
     rather than a third spelling. The whole bridge is one function: a browser's
     `KeyboardEvent.key` reports either a literal character or a name, which is
     exactly the split `keys` has two entry points for. Length tells them apart.
     One special case — the browser says `" "` where both toolkits say `Space`.

   **`Instant` was the carried risk from step 4 and it is now discharged** for
   the code that crossed: `web-time` is a drop-in that reads `performance.now()`
   on the web and re-exports `std` verbatim elsewhere, so there is one
   implementation. Two `clock.rs` tests used `thread::sleep`, which would have
   cost them their place in the V8 run; spinning on the same `Instant` the code
   under test reads is portable and keeps the assertions byte-identical.

   **Tap tempo moved *down*, not across.** `App::tap_tempo` was pure timing
   sitting in the native shell; it is now `clock::TapTempo`, so the web taps
   tempo through the same estimator rather than a second average that could
   drift. Six new unit tests it never had, all running in V8 — including the one
   that pins *why* it averages the span over the interval count rather than
   meaning adjacent gaps.

   **What is wired, and what is honestly not.** The clock and pane verbs are
   live; the rest name banks, cues and clips the web shell does not have yet.
   Those are shown in the panel readout rather than silently swallowed — a verb
   that resolves and then does nothing is indistinguishable from a broken
   grammar unless it says so. The flat `t`/`b` tap keys are deliberately
   *absent* from the web shell: they are grammar tokens, so the grammar claims
   them first exactly as it does natively, and a duplicate flat binding would be
   unreachable while the panel hint claimed otherwise.

   `scripts/play-smoke.mjs` now drives real keys through `Input.dispatchKeyEvent`
   — Chrome's own delivery path, not a synthetic `new KeyboardEvent`, so the
   adapter cannot agree with a mistake the real pipeline never makes — and
   asserts the beat advances, a root opens, `b a` resolves `FocusPane(Clock)`,
   tapping moves the tempo off its default, and Escape cancels.
4c. **Publishable.** The four things standing between step 4b and a page a
   stranger could open: a fallback for GPUs without BC, a bundle that is not
   10 MB, a refusal that explains itself, and a clip that survives a reload.

   **The RGBA fallback exists — `vidiotic-play/src/video/softdec.rs`.** HAP is
   block-compressed by construction, so a device without `texture-compression-bc`
   could not show a clip at all; the browser's answer was a black canvas and a
   console line. Desktop GPUs all have BC. Apple silicon, Android, and most
   integrated mobile parts do not, and those are the machines a *link* gets
   opened on — which is the whole difference between a tool you run and a tool
   you publish.

   What it has to agree with is **not** the GPU's BC decoder. BC1/BC3 endpoint
   interpolation is explicitly implementation-defined, so bit-equality is neither
   achievable nor required. What it must match is `preamble.frag`'s `video()`:
   on this path the frame arrives as `PixelData::Rgba`, which reports
   `video_mode` 0, so the shader never runs the YCoCg unswizzle or the
   alpha-only expansion — the CPU has to have done both already. Every branch of
   `video()` is reproduced against the same constants, and the tests check the
   *transform*, not just the block layout, because a mismatch there is not a
   crash. It is a clip that looks right on every machine the author owns and
   wrong on one class of machine they will never see.

   BC7 is named rather than silently skipped: nothing in this repo emits it
   (`transcode.rs` bakes Hap1/BC1 only), so a Hap R clip from another tool
   reports why instead of showing black.

   **The fallback is exercised on hardware that does not need it.** `?soft=1`
   and `set_soft_decode(true)` force the CPU path on a BC-capable device, and
   the smoke test pauses playback and compares the same sample decoded both
   ways. Without that, the one path that only ever runs on other people's
   machines would only ever be tested on other people's machines — which is the
   same as not having one.

   **The bundle: 10.2 MB → 8.4 MB**, via a `release-wasm` profile (fat LTO, one
   codegen unit, `panic = "abort"`) and a `wasm-opt -Oz` pass in
   `build-play.sh`. Two things worth recording:

   - **The size-tier `opt-level`s make it bigger.** Measured, post-`wasm-opt`:
     `3` → 8.4 MB, `"s"` → 9.3 MB, `"z"` → 9.2 MB. This is the opposite of the
     folklore. naga and wgpu are full of small generic functions whose inlined
     forms then collapse against each other; deny the inlining and every copy
     survives into the binary.
   - **`wasm-opt` is optional and loud about it.** A contributor without
     binaryen gets a working page, just a fatter one, and a line saying so.

   **3.6 MB of the remaining 8.4 MB is two fonts** — `NotoSansSymbols2` and
   `SymbolsNerdFont`, `include_bytes!`'d into `phosphor::theme` because it
   extends `FontDefinitions::default()`. That is the single largest lever left,
   and it is a subsetting job in `phosphor`, not a wasm one. It belongs with
   §9a's font work rather than here.

   **And that is where it was pulled: 8.4 MB → 6.3 MB (§9a).** Not by
   subsetting — by moving `icon.rs` off the private-use area, which left
   `SymbolsNerdFont` with nothing pointing at it. The remaining `NotoSansSymbols2`
   is 1.23 MB and is only there for `Face::Classic`, which the browser does not
   use.

   **A refusal that explains itself.** `Cannot read properties of undefined
   (reading 'requestAdapter')` in a console nobody has open is indistinguishable
   from a broken link, and WebGPU is still absent or flagged off in enough
   places that this is the most likely thing to happen to a first-time visitor.
   The page now checks `isSecureContext` and `navigator.gpu` *before* the first
   click and names the actual cause — insecure origin, or a browser without
   WebGPU, with the versions that have it.

   Not a blocker after all: **`wgpu::Limits::default()`**, carried since step 4
   as "may exceed what the browser reports". It does not. Checked against
   `wgpu-types-29.0.4`: `Limits::defaults()` *is* the WebGPU spec's mandatory
   minimum set — 8192 2-D textures, 256 MiB buffers, 4 bind groups — which every
   conformant implementation must grant. `request_device` cannot fail on limits.
   The real failure surface was always "no adapter at all", which is the message
   above.

   **Persistence, and its shape.** The clip goes to OPFS and the session to
   `localStorage` — see §7. Doing it in JS rather than Rust is deliberate: this
   is the same job `index.html` already had (get bytes into the engine), the
   storage API is a browser API, and the Rust alternative is ~150 lines and six
   more `web-sys` features to do what 30 lines of JS does. The engine stays the
   thing that knows what a bpm *is*; the page just hands one back through
   `set_bpm`.

   Drag-and-drop landed with it, on the document rather than a rectangle: a clip
   is the one thing you bring to this tool.

   **What is still not done, and is not a publishing blocker:** cues, banks and
   the sequencer in the browser; `.viproj`; the `ui/*` port (still gated on
   §9a); audio; MIDI; `BroadcastChannel`; `app.rs`. A page that plays a clip you
   drop on it, with effects and a beat grid, that survives a reload, is a
   smaller claim than /play — and it is now true.
4d. **One engine, two shells.** Everything above this line was `/play` catching
   up to `vidiotic` by reimplementation: the browser had its own clock, its own
   grammar state, its own playhead. Cues, banks and the sequencer were the point
   where that stopped being viable — the next feature would have been written
   twice and then drifted. So the session moved out of the shell.

   **`vidiotic-play/src/engine/` is `vidiotic::app` with the OS taken out.** The
   clock, the sequencer, the clip pool, the cue banks, the modal grammar, the
   undo stack, and the whole `Command` vocabulary that is not a syscall. `App`
   keeps what a browser does not have — two winit windows, cpal, the capture
   registry, IPC, `rfd`, the filesystem — and holds an `Engine`. The web shell
   holds the same `Engine` and is now canvas plumbing rather than a second
   player.

   What crossed for free, in the sense that not a line of it was written for the
   browser: cue rotation on the phrase grid, cue banks, the clip pool, document
   undo/redo, and the ~30 grammar verbs that previously resolved and then hit a
   `_ => {}`.

   Four things are worth keeping from how it went.

   - **The refactor was picked because the compiler could check all of it.**
     Moving `shaders/` in step 4 had two consumers that failed *silently*. Moving
     fields between structs has none: a field on the wrong side is a build error,
     and the native binary is the regression lock the whole way. That is why this
     was worth doing as one sweep rather than incrementally behind a shim.
   - **The seam is one trait, because only one thing genuinely differs.**
     `engine::source::Opener` turns a cue into a `Box<dyn Source>`: natively a
     decode worker on a thread or a tap onto a camera service, in a browser a
     `.mov` in memory walked by the render loop. Everything else — *when* a cue
     arms, which cue is current, when the re-loop grid restarts it — is shared.
     The polymorphism already existed natively (`SourceHandle` was a two-variant
     enum with a match in every method), so the browser became a third
     implementation rather than a second compilation.
   - **Unhandled commands come back rather than being dropped.**
     `Engine::apply_command` returns `Some(cmd)` for anything it does not
     implement. The native shell's `apply_shell_command` ends in `unreachable!`,
     so adding a command without deciding who owns it fails loudly; the web shell
     puts the name in its status line, which is how "no, `/play` cannot save a
     project" became a sentence on screen instead of a dead key. The step-4b
     readout that named unwired verbs is now the whole vocabulary, for free.
   - **The engine became testable, and immediately paid.** Cue rotation had *no*
     unit test before, because exercising it meant constructing a struct that
     owned two winit windows and a cpal stream. `Engine` takes its clock as a
     trait object — the seam Ableton Link already needed — so a test drives the
     beat by hand and asserts the swap. Three of those run in V8 in the gate.

   **The `Engine`'s fields are `pub`, deliberately.** The native mirror builder
   reads about forty of them to publish the UI's read-only view; forty accessors
   would be forty places for the two to disagree. Anything with an invariant —
   bank indices, cue ids, the sequencer's active set — is behind a method, and
   those methods are the only way the invariant is kept.

   Verified end to end rather than by inspection: the native player decodes and
   plays a cue and rotates between two of them (driven over IPC, roles read off
   the live mirror), and the browser does the same through the same engine
   (`play-smoke.mjs`, 26 checks). The gate's `vidiotic-play --lib` row went
   79 → 91.

   **Still not done, and not blocked by this:** `.viproj` in the browser, the
   `ui/*` port, audio, MIDI, `BroadcastChannel`. Each is a smaller job than it
   was, because none of them has to be built twice.
4e. **A visitor's own video.** Everything before this took a `.mov` baked by
   the *native* `vidiotic transcode`, which is a file no visitor has. `/play`
   now bakes what it is given: drop an mp4 and the page turns it into Hap1 in
   the tab. That closes the last thing standing between the page and somebody
   who is not the author using it.

   **It is almost entirely a transport job, and that is the point.** The
   compressor is `vidiotic-bake::frame::FrameBaker` and the container is
   `vidiotic-bake::mov::MovWriter` — the same code the desktop baker drives,
   already in `vidiotic-play`'s dependency graph, already crossing the gate.
   What `web/bake.rs` adds is a `Cursor<Vec<u8>>` where the native path has a
   file, and an explicit per-frame `pts` because there is no demuxer to supply
   one. `bc1_golden` already asserted a browser bake is byte-identical to a
   native one; this is what made that claim reachable from a drag and drop.

   The tier policy moved into `vidiotic-bake::frame::Tier` next to `align4`,
   where the other dimension rule already lived — §3a said it belonged in the
   bake parameters rather than hardcoded, and `Tier::fit` is that: fit the box,
   preserve aspect, never upscale, land on whole 4x4 blocks.

   **Two things went differently from the plan, both recorded in §3d.** The
   decode is a `<video>` element rather than WebCodecs, because a browser ships
   no demuxer and this page has no dependencies. And it is seek-stepped rather
   than captured from `requestVideoFrameCallback`, because rVFC needs the
   document to be rendering — the first implementation looked correct and
   quietly produced truncated clips the moment the tab lost focus.

   **The constraint that remains is worth knowing before deploying:** Chrome
   will not load a media element in a hidden document, with no event and no
   timeout to detect it by. The page waits for visibility and says so rather
   than hanging, so a bake pauses and resumes; that is honest, but it is also
   the reason to do the demuxer work eventually, since WebCodecs has no such
   tie.

   Verified as everything else here is: `play-smoke.mjs` fetches a VP9 `.webm`
   nobody baked, asserts it does *not* probe as Hap1, bakes it in the browser,
   asserts the bytes now do, rounds them through the page's own ingest path,
   and reads a lit pixel back off the output head while that cue is on air.
   30 checks.
4f. **The reactive effects start reacting.** All ten bundled effects read `lvl`
   or `fftBand`, and every one of them had been reading a hardcoded zero —
   compiling, running, and sitting perfectly still. This is the smallest step
   here and probably the largest visible difference, because half of what the
   tool looks like is the picture moving with the music.

   **The same cut as the engine and the same cut as `Opener`.** `analysis.rs`
   was one file doing two jobs: getting samples (a cpal device, a lock-free
   ring, a thread, a triple buffer) and deciding what they mean (Hann window,
   2048-point FFT, 21 log-spaced bands, attack/decay smoothing, the 512x2 R8
   texture the shaders sample). The second half moved to
   `vidiotic-play::analysis` as an `Analyzer`; the first half stayed native and
   shrank to about sixty lines of plumbing. The browser's replacement for it is
   an `AudioWorklet` posting mono quanta into one exported `push_audio`.

   That split is the whole point: an audio-reactive shader *cannot* look
   different on the two platforms, because there is one implementation of what
   the bands mean. Two would have diverged, and the divergence is the sort
   nobody notices until they are standing in front of a projector.

   **Two things the extraction paid for immediately**, neither of which had a
   test before because testing them meant owning a cpal device:

   - **A tone lands in the band that contains it.** A wrong bin mapping or a
     dropped window still produces plausible-looking bars, so nothing short of
     this assertion could see it.
   - **A dead source decays instead of latching.** `Analyzer` only produces a
     frame when a hop has been fed, so a tap that stops — a closed tab, an
     ended track, a device that went away — leaves the last frame standing
     forever and every reactive effect frozen mid-flash. The shell now feeds
     silence after 250 ms of nothing, which is both the correct value and the
     one that makes the fall look identical to a track that merely went quiet.
     Found by the smoke test, and it would have looked exactly like a hung
     renderer on stage.

   The capture itself is the one part not shared, so it is the one part the
   smoke test drives synthetically: a 1 kHz tone through `push_audio`, `lvl`
   rising from 0 to 0.27, and falling back to 0.003 when the tone stops.
   `getDisplayMedia` needs a consent prompt no headless run can answer, and
   everything after it is shared code.
5. ~~**Dual-head spike** (§10): can wgpu build a surface from a canvas in a second
   same-origin document, sharing one `GPUDevice`?~~ **Done — §10a. Yes.** One
   device, two documents, one submit, pixels verified by readback, 120 sustained
   frames. `/play` gets the native-equivalent architecture and `gfx.rs`'s shape
   survives; the BroadcastChannel fallback is not needed for *rendering* (it is
   still the transport for `vidiotic-wire`).

   The one constraint it turned up: the output head must build its surface from
   `SurfaceTarget::Canvas` directly, because winit's `RawWindowHandle::Web` path
   looks its canvas up in the opener's document and panics. Costs nothing — the
   output window takes no input, so it wants no winit.

   **Step 4 then found a second, worse one that inspection could not have
   caught — §10b.** wgpu's `create_surface` casts the canvas context with a
   realm-local `instanceof`, which a cross-document context fails even though
   WebGPU itself accepts it. The spike could not have seen this: it never went
   through wgpu.
4g. **The real UI, moved rather than ported. Done.** Cheaper than §2's line
   counts implied — because those counted `app.rs`/`session.rs`/`export.rs`, and
   the *egui* was a different story. Measured across `vidiotic/src/ui/`:

   | module | lines | non-portable references |
   |---|---|---|
   | `library.rs`, `status.rs`, `transport.rs`, `whichkey.rs` | 1,338 | **none** |
   | `editor.rs` | 821 | 2 x `Path::new(…).file_stem()` on a display string |
   | `mod.rs` | 299 | `egui_winit::State`, the `rfd` block |

   Every panel was already `fn show(&mut Ui, &UiMirror, &Sender<Command>)` — no
   `App`, no OS — and `UiMirror` has lived in `vidiotic-play::commands` since
   step 4b, which already crosses the gate. So this was a move, and the same
   three seams as everything above it:

   - **`EguiCtl` stays native.** It is `egui_winit::State` plus a `WindowSurface`;
     `web/input.rs` is already the browser's equivalent. Panels portable, input
     adapter per shell.
   - **`pick_file` becomes four commands.** Six call sites, all of the form "ask
     the visitor for a path of kind X". `OpenProject` and `SaveProjectAs` are
     already commands the shell answers; `ClipDir`, `ClipBankDir`, `Shader` and
     `Isf` should be too. Native answers with `rfd`, the browser with an
     `<input type=file>` — and a kind the browser cannot serve lands in the
     status line by itself, which is what `apply_command` returning `Some` is
     for.
   - **`build_mirror` splits the way `app.rs` did.** `app/mirror.rs` is 269
     lines and about 90% of it reads `self.engine.*`. The native-only reads are
     exactly eight: cached thumbnails, project path, audio device list and
     current device, shader name and error, `clip_meta`'s duration/fps, and the
     camera rows. So `Engine::build_mirror(&self, snap, &mut UiMirror)` fills
     what it owns and the shell overlays those eight — compiler-checked
     throughout, with the native binary as the regression lock, exactly as in
     step 4d.

   One constant was in the wrong crate and blocked the move on its own:
   `DELAY_CAP` was defined in `vidiotic::video::capture` and read by
   `editor.rs`. It is a model fact about a cue's delay, so it moved beside
   `CamDelay` in `vidiotic-core::bank` — and once it was a `CamDelay` method
   rather than a bare number, the four open-coded `.clamp(0.0, DELAY_CAP)`
   call sites collapsed into `seconds_capped()`. The compiler found a fourth
   the grep had missed.

   This is what delivers "pick from a stack of effects" and "play with the
   system" in a browser, because `editor.rs` already contained the whole chain
   editor and the ISF parameter UI.

   **As built.** The three seams landed first and separately, each with the
   native binary as the lock; then the move itself was **2,102 lines relocated
   with zero edits**, exactly as `shader.rs` went in step 4 and for the same
   reason — nothing in them named an OS. `vidiotic::ui` kept `EguiCtl`,
   `pick_file` and the wgpu clear colour (299 → 251 lines) and re-exports
   `control_ui`, so **not one call site changed**. `app/mirror.rs` went 269 →
   140: the engine fills what it owns, the shell overlays the eight it doesn't.

   The browser shell then deleted its own `web/panel.rs` (324 lines) — the
   placeholder from step 4b — and draws `ui::control_ui` against the same
   mirror, draining the command channel on the same frame and putting anything
   the engine declines into the status line. What went with the placeholder is
   its play/pause button and seek scrubber: neither is an engine concept, no
   `Command` exists for either, and natively there is no button for one. Space
   already toggles pause and the JS API is unchanged, so nothing became
   unreachable. Its effect dropdown is not a loss either — the real chain
   editor supersedes it.

   **Cost: 325 KiB** (6,730 → 7,055 KiB), for the entire control surface. The
   wasm gate went 97 → 99 tests on `vidiotic-play` and the panels run in V8;
   the browser smoke passes with them drawing.

   **Then the buttons had to do something.** Moving the panels moved their
   `Pick*` commands too, and a browser that answers none of them draws a chain
   editor whose "+ ISF" writes a line in the status bar. Two of the four are
   answerable and are now answered:

   - **ISF crosses whole.** `isf::transpile` has always been in `vidiotic-core`
     and `Renderer::load_isf` in `vidiotic-play`, both taking source text — the
     only native thing in that path was the `std::fs::read_to_string` in front
     of it. So the browser compiles the identical shader, and a visitor can
     bring one of the thousands of ISF shaders that already exist. Two honest
     differences: `IMPORTED` images have no directory to resolve against and
     bind black (which `load_isf` already does for a missing image, so it
     loads rather than refuses), and with no cue selected it compiles into the
     pool instead of declining, because the pool is a list the chain editor can
     assign from later.
   - **WGSL is the one-shot half** of `SetShaderPath`. The `ShaderWatcher` is
     what does not cross — there is no file left to watch after the read — so
     picking again is the reload.

   `PickClipDir` and `PickClipBankDir` are still unanswered, and deliberately:
   a directory of clips means ingesting and baking each one, and OPFS holds a
   single clip until step 7.

   The bridge is a `vidiotic-pick` DOM event out and a `load_isf_source` /
   `load_shader_source` export back, which keeps every file read in `boot.js`
   where the page already reads clips — the boundary `load_clip` drew.

   **Cost: another 425 KiB** (7,055 → 7,480 KiB), which is not the twenty lines
   of bridge. It is naga's GLSL front-end: nothing in the browser called
   `load_isf` before, so the whole transpiler was being dead-stripped out of
   the wasm. ISF support was never free, it was merely unreachable.

   The load-bearing risk was **transient user activation**. A file chooser
   needs it, and by the time a `Pick*` is drained the visitor's click has been
   through egui inside a `requestAnimationFrame` callback rather than the
   pointer handler that started it. The activation window is five seconds wide
   and not scoped to the dispatching task, so this is allowed — but it is
   allowed by a rule rather than by construction, and it is precisely the shape
   of thing that works until a browser tightens it. So the smoke intercepts the
   chooser and asserts it opens, rather than trusting the rule.
6. **`/chop`**: preview (WebCodecs seek) → span editor (mostly a lift) → MP4
   export → HAP export. Cheaper than §2 says too, and for the same reason:
   `timeline.rs` and `undo.rs` have no native references at all, `ui.rs` has
   five call sites (four `rfd`, one `open -R`) in 867 lines, and `spans.rs` and
   `commands.rs` only use `PathBuf` as a data type.
7. ~~**OPFS handoff + zip**, tying the routes together.~~ **Done — §5a.**
   `/play`'s store became a directory, which is what everything else was
   waiting on: the handoff, saving a session, and a `.viproj` naming more than
   one clip all needed a pool that survives a reload.
8. Deferred, any time after 4: tab-audio capture; WebMIDI; the cabinet and CRT
   pass (§9b); URL ingest (§6).

Steps 0–3 are all in the existing native tree and independently shippable. No
browser code is written until the format and the muxer are proven natively.

---

## 9. Presentation: lo-res phosphor + the floor-model TV

The §3a resolution tier, §3b grain, a pixel-font UI, and a CRT cabinet are one
decision, not four. Treated together the resolution ceiling stops reading as a
compromise and becomes the design.

### 9a. Lo-res phosphor

**Built.** `phosphor::theme::Face::Grid`, opt-in at runtime, with `/play`
wearing it and `vidiotic`/`vidiotic-prep` still on `Face::Classic` until their
panels are rebuilt for the cell. What follows is the plan as decided; the
**"As built"** section at the end of §9a records what it cost and what changed.

A **theme variant of `phosphor`, not a fork.** The toolkit is already built for
it: `lib.rs` describes the "character-grid buffer idiom... monospace glyph
widgets... square corners everywhere", `theme.rs:127` is the single buffer font
every glyph widget lays out with, corner radius is one variable, and every
color goes through `palette()`. Same widget API, swapped font and metrics — so
`vidiotic-ctl` and both web routes keep rendering through one toolkit.

**The font: C64 / PETSCII, 8×8.** Chosen for the graphics charset, which maps
onto phosphor's existing idiom rather than fighting it. **Resolved: Unscii
2.0** — see "As built".

Target **real Unicode, not a private-use area** — strictly better than `icon.rs`'s
current dependence on Nerd Font PUA assignments:

| Block | Range | Supplies |
|---|---|---|
| Symbols for Legacy Computing | U+1FB00–1FBFF | Sextants, diagonals, quadrants, corner pieces |
| Block Elements | U+2580–259F | Eighth-blocks, checkerboards |
| Box Drawing | U+2500–257F | Frames, corners, junctions |
| Misc Symbols | U+2660–2667 | Card suits |

`widgets.rs` already lays out eighth-block meters from Block Elements, so this
continues the existing toolkit instead of replacing it.

Consequences:

- **It shrinks the `icon.rs` problem without erasing it.** Chrome — borders,
  fills, meters, corners, bracket buttons — becomes real characters. Transport
  iconography (play, pause, step, zoom) has no PETSCII equivalent and still needs
  authoring, but an 8×8 bitmap glyph is trivial next to sourcing a matched pixel
  icon set, and can live as data in `widgets.rs` beside the hand-drawn widgets.
- **8×8 settles the integer-scaling question.** Unambiguous design size: render at
  2x (16px) or 3x (24px), never fractional. `pixels_per_point` snaps to an
  integer and text rounds to pixel boundaries. At 2x a 1920-wide window is 120
  columns — a workable grid, and now a literal one, which suits a toolkit whose
  stated idiom is the character grid.
- **Decided: 2x. The cell is 16 points.** 3x is the stronger aesthetic
  commitment but forces every panel to be redesigned rather than transcribed;
  2x lands close to today's density, so existing panel structure mostly
  survives. On a 1512-point retina window that is 94 × 61 cells against roughly
  54 rows today.

  The constants this moves, all of them in `phosphor::theme` (measured, not
  estimated):

  | | now | 2x grid | in cells |
  |---|---|---|---|
  | `mono()` | `monospace(12.0)` | `16.0` | 1 |
  | `ROW` | 18.0 | 16.0 | 1 |
  | `interact_size.y` | 22.0 | 16.0 | 1 |
  | `item_spacing` | (8, 6) | (8, 0) | (½, 0) |
  | `button_padding` | (6, 3) | (4, 0) | (¼, 0) |
  | `SP_XS`/`SM`/`MD`/`LG` | 2/4/8/16 | unchanged | ⅛/¼/½/1 |

  Vertical spacing goes to zero because `ROW` *is* the cell — leading is what a
  character grid does not have.

  **Write them as cell multiples, not point literals.** `CELL: f32 = 16.0` with
  everything else expressed against it is what makes 3x a one-line change later
  instead of a second relayout. That is the whole reason this had to be decided
  before `ui.rs` is ported, and it is cheap to honour now and expensive to
  retrofit.
- **Layout must be designed to the coarse grid, not retrofitted.** Panels lose
  fine granularity. That is the intent, but `ui.rs` in both apps should be built
  for it. **Decide early even if built late** — retrofitting integer-snapped
  layout onto tuned layouts is the expensive order. The surface this governs,
  measured: `vidiotic/src/ui/*` 2,403 lines, `vidiotic-prep/src/ui.rs` 867 +
  `timeline.rs` 306.
- **Licensing needs care; these are public sites.** ~~Do not ship the Commodore
  character ROM — it is Cloanto/Commodore IP however freely it circulates. Use a
  reimplementation and verify its terms: *C64 Pro Mono* (Style) and *PetMe64*
  (Kreative Korp) are candidates.~~ **Resolved, and neither candidate was
  needed: Unscii 2.0, public domain.**
- ~~**Open call: VIC-II palette vs. Everforest at 8×8.**~~ **Done as specified:**
  Everforest is the default, VIC-II is a switch (`theme::Colors`). The muddiness
  is real and the switchboard is where it belongs.

#### As built

**Unscii 2.0 (Viznut), public domain.** It settles the licensing question
outright — every variant except `unscii-16-full` is PD, and that one is GPL only
because it incorporates GNU Unifont, which is not the one used here. It is also
a better fit than either candidate above for the reason §9a picked this
direction in the first place: **Unscii 2.0 exists specifically to carry
Unicode 13's Symbols for Legacy Computing block.** The table above was a wish
list; this font is somebody else's completed version of it. 3,159 codepoints,
245 KB.

`phosphor/scripts/vendor-unscii.sh` is the derivation, runnable — a binary asset
nobody can diff becomes folklore otherwise. It prints the upstream hash, makes
one edit, and fails if the result stops covering what `icon.rs` and `widgets.rs`
draw from.

**The one edit: `lineGap` 3 → 0, in both `hhea` and `OS/2`.** Unscii ships 3/32
em of leading, which is right for a terminal and wrong for a character grid.
epaint computes `row_height = ascent - descent + line_gap`
(`epaint-0.35.0/src/text/font.rs:588`) and `FontTweak` exposes no way to
override the gap — it has scale and y-offset and nothing for leading. Left
alone, an 8×8 face at 16pt lays out on **17.5-point rows**, and "the cell is 16
points" would be false in the one place it has to be true. Measured, then
asserted: `the_grid_is_literally_a_grid` lays out real text and requires one row
to be one cell and four characters to be four.

**`Metrics` is the type §9a asked for.** `Metrics::GRID` is written as `CELL`
times a fraction throughout, and `every_grid_metric_is_a_fraction_of_the_cell`
is that instruction as an assertion — every value has to be a whole number of
eighth-cells. Going to 3× is editing one `const`.

**`icon.rs` left the private-use area, and that turned out to be the largest
single win in the whole port so far.** The icons were Font Awesome codepoints
out of a bundled 2.44 MB Symbols Nerd Font: a mapping nothing outside Nerd Fonts
agrees with, that no other font can substitute for. Forcing them through an 8×8
face — which can carry Box Drawing and Legacy Computing and cannot carry
somebody's private icon set — is what made the cost visible. **The bundle went
8.4 MB → 6.3 MB**, and the native app dropped the same 2.44 MB. Two icons are
two cells (`│◄`, `►│`); on a character grid that is a normal amount of room for
a control, not a compromise to fix later.

Only one glyph the plan predicted would need authoring actually did, and it did
not: `↻` is absent, so REFRESH is `◴`, and every call site says the word
"refresh" beside it.

**epaint's `Fonts::has_glyph` is unusable in 0.35** — implemented as
`resolve_face(c) != replacement_face_key`, it returns `false` for every
character, including `'A'` in a stock context. The first version of the coverage
test reported that Hack could not draw a plus sign. `glyph_width(id, c) > 0.0`
is the property that works, and is what `can_draw` uses.

**`/play` wears the grid; the two native apps do not.** That is §9a's own
sequencing: `vidiotic/src/ui/*` (2,403 lines) and `vidiotic-prep`'s 1,173 are
laid out for 12pt on 18pt rows, and retrofitting integer-snapped layout onto
tuned layouts is the expensive order. `/play`'s panel is the P0 skeleton and was
never tuned, so it is *designed* to the cell rather than retrofitted onto one —
which makes it the working reference the other two get rebuilt against.
`play-smoke.mjs` reads the face and cell size back out of the running engine, so
"the grid is what is painting" is checked rather than assumed.

**Still open:** the native panels' rebuild, and the second 1.23 MB — Noto Sans
Symbols 2 is still linked for `Face::Classic`, which the browser build has no
use for. Gating it out is a cargo feature in `phosphor`, and worth doing when
the browser stops needing the classic face at all.

### 9b. The cabinet

Render everything — video **and** the egui UI — to an offscreen target, then run
a CRT pass (barrel distortion, scanlines, aperture grille, bloom, vignette) with
the console cabinet composited around it. Not a CSS bezel around the canvas: that
leaves a flat rectangle inside a picture frame and cannot touch the UI.

The machinery already exists. `preamble.frag` exposes `inputTex` at `set = 2`
and `prev(uv)`, so shaders already compose in a chain — the CRT pass is just the
last link in it.

**The cabinet is a mode, not a constant.** Per §10 the app is dual-head: the TV
console frames the *control* surface, and how the site presents itself to a
visitor. A projected output feed wants to be clean — or to carry its own CRT pass
as a deliberate creative effect, which is a different thing from UI chrome.

**Pointer mapping, control window only.** With the UI inside the barrel
distortion, click coordinates no longer match what is on screen. Inverse-map
pointer positions before handing them to egui — standard barrel formulas are
analytically invertible, roughly ten lines in the web shell. The output window
has no UI to click, so distortion there is free.

Free win: `theme.rs` already implements a **global hue rotation**. If the cabinet
gets functional knobs, the tint knob is already written.

---

## 10. Dual-head

`vidiotic` is natively dual-head: control UI on the operator's screen, clean feed
to the projector. The web build keeps that split — this is **not** "pop the app
out into a second window", it is two different renders of the same engine state.

| Surface | Contents | Input |
|---|---|---|
| Control window | egui UI, small preview, TV cabinet (§9b) | all of it |
| Output window | clean visual chain, no UI, no chrome | none |

Consequences, mostly simplifying:

- **Keyboard routing stops being a risk.** Every keystroke stays in the control
  window, so `grammar.rs`'s modal verb-object input never has to survive a
  document move. The output window needs no input at all.
- **`window.open()` is the right primitive**, not Document Picture-in-Picture. A
  real window drags to a projector and goes fullscreen; PiP is always-on-top,
  small, single-instance, and Chromium-only. Keep PiP as an optional *monitor*
  convenience, off the critical path.
- **`getScreenDetails()` + `requestFullscreen({screen})`** (Window Management API,
  Chromium, permission-gated) places the output on the projector programmatically
  — the closest browser equivalent to native display selection. Nice-to-have; the
  manual drag works everywhere.

**Dual-surface rendering — ~~the one real unknown~~. Resolved: option 1 works.**
In descending preference:

1. **One `GPUDevice`, two canvas contexts, one per document.** Same-origin
   `window.open()` shares an agent cluster, so window B's canvas can be handed to
   window A's render loop with no serialization. Structurally identical to native
   dual-head. **This is the architecture** — see §10a.
2. **Two render loops over BroadcastChannel-synced state.** Certain to work. Not
   2x cost since the control preview renders small; call it ~1.1x. ~~The fallback.~~
   No longer needed.
3. **Render once, transfer frames** via `transferToImageBitmap` + `postMessage`.
   Full readback and added latency every frame. Rejected.

### 10a. Measured: cross-document rendering (resolved)

`docs/spikes/dual-head.html`, Chrome headless, M3 Pro (`apple` / `metal-3`).
Every row passed:

| Check | Result |
|---|---|
| `getContext("webgpu")` on a canvas in the child document | context obtained |
| `configure()` that context with the **opener's** `GPUDevice` | accepted |
| One `CommandEncoder`, one `submit()`, spanning both canvases | accepted |
| Cross-document pixels, verified by `copyTextureToBuffer` readback | `rgb(224,64,128)` — exact |
| Sustained loop, one `rAF` driving both heads | 120 frames |

The readback is the point. "No exception was thrown" is not evidence that
anything rendered, so the spike clears each canvas to a *different* exact 8-bit
colour and maps the texture back — a crossed or blank surface would show up as
wrong numbers rather than have to be eyeballed.

**The wgpu half is settled by inspection, and it has one trap in it.**
`SurfaceTarget::Canvas` lowers to `RawWindowHandle::WebCanvas`, which
`wgpu-29.0.4/src/backend/webgpu.rs:1544` casts straight from the `JsValue`. It
never reaches for `web_sys::window()` or `document()`, so the canvas's owning
document is nothing to it. The *sibling* branch is the trap:
`RawWindowHandle::Web` (`:1529`) — which is what **winit** hands over — resolves
its canvas by `document.query_selector_all("[data-raw-handle=…]")` against
`web_sys::window()`. In the opener that finds nothing and `.expect()`s.

So: **the output head takes `SurfaceTarget::Canvas` directly and must not route
through winit.** That costs nothing — the output window needs no input (above),
so it needs no winit. ~~The control head keeps winit, as native.~~ **The control
head dropped winit too** — see 10b; on the web neither head uses it.

Consequence for the port: `gfx.rs`'s `Graphics { device, queue, output, control }`
survives as-is. Only surface *construction* differs per head, and the render loop
does not change shape at all.

~~Still to prove end-to-end: the same thing through real wgpu-on-wasm rather than
raw WebGPU. That arrives free with the first `/play` skeleton; the architecture
decision did not need to wait for it.~~ **Proved, and it was not free — see 10b.**

### 10b. Measured: the same thing through real wgpu (step 4)

The architecture holds. The *binding* did not, and inspection was the wrong
instrument for finding out.

`Graphics::new_web` panicked on the first run of `scripts/play-smoke.mjs`:

```
canvas context is not a GPUCanvasContext:
  Object { obj: JsValue(GPUCanvasContext), ... }
```

— from `wgpu-29.0.4/src/backend/webgpu.rs:1126`, a `dyn_into` that wgpu itself
comments as "a type error that shouldn't happen unless the browser, JS builtin
objects, or wasm bindings are misbehaving somehow". The object plainly *is* a
`GPUCanvasContext`. Probed directly, in plain JS, no Rust involved:

| Check | Result |
|---|---|
| same-document context `instanceof GPUCanvasContext` | `true` |
| popup's context `instanceof` **the opener's** `GPUCanvasContext` | **`false`** |
| popup's context `instanceof` the **popup's** `GPUCanvasContext` | `true` |
| `GPUCanvasContext === win.GPUCanvasContext` | `false` — two realms |
| `configure({device})` on that same cross-realm context | **accepted** |

**Every realm has its own `GPUCanvasContext` constructor, and `instanceof` is
realm-local.** wasm-bindgen implements `dyn_into` as exactly that `instanceof`,
against *this* module's realm. So `create_surface` rejects the output canvas
before making a single WebGPU call — while the WebGPU operation on the very same
object is accepted. That combination is why §10a's inspection missed it: the
trap it did find (`RawWindowHandle::Web` reaching for `web_sys::window()`) is
real, and avoiding it is necessary but not sufficient. `SurfaceTarget::Canvas`
is document-agnostic; the `dyn_into` two lines later is not.

The fix is four lines of JS in the host page, relaxing that one check to a
structural one:

```js
Object.defineProperty(GPUCanvasContext, Symbol.hasInstance, {
  configurable: true,
  value: (o) => native?.(o) || (o != null && typeof o === 'object'
    && typeof o.configure === 'function'
    && typeof o.getCurrentTexture === 'function'),
});
```

Distasteful, and the right size for the problem: it is scoped to our page, the
property it loosens is precisely the realm identity we do not want enforced, and
it keeps the one-device architecture that everything else in §10 rests on. The
alternatives are worse — a second `GPUDevice` in the popup is option 2, the
fallback §10a retired; patching wgpu is a fork.

**Carry this forward.** It is a wgpu-version-sensitive workaround around an
`.expect()`, so it should be re-checked on every wgpu bump, and it is worth an
upstream issue: `create_surface` could use a structural check, or return
`CreateSurfaceError` instead of panicking, and either would make cross-document
rendering work without a shim.

The rest, measured the same run (`node scripts/play-smoke.mjs`, Chrome headless,
`clips/bun.mov`):

| Check | Result |
|---|---|
| `Graphics::new_web` across two documents, one device | ok, after the shim |
| `texture-compression-bc` negotiated | present — HAP uploads as blocks |
| Output head, real HAP clip through the composite pass | `rgba(79,12,33,255)` |
| Control head, egui through `egui-wgpu` | `rgba(44,52,58,255)` — phosphor's `bg_base` |
| The two heads differ | yes — genuinely separate surfaces |
| Output window composited (screenshot of *that* CDP target) | 212 KB PNG |
| Playback advances | pixel moves across 700 ms |
| Effect chain (seed + ping-pong path) | 10/10 built-ins change the output |

The last row matters more than its size: an empty chain takes `render`'s
one-pass fast path, so a clip appearing on screen proves only that. Driving all
ten built-ins proves the ping-pong path, and proves runtime GLSL→naga
compilation works in a browser — which is what the shader editor survives on.

**Either way this restores the IPC surface §4 gave up.** Two windows need a
transport between them, and that transport is `vidiotic-wire` over
`BroadcastChannel` — same origin, structured clone, no server. `vidiotic-wire` is
pure nanoserde and ports untouched; only the transport changes. Dual-head and
scriptable control are the same build, not two.

---

## Observations

- **Inspection cleared the cross-document render path, and inspection was
  wrong.** §10a reasoned carefully from wgpu's source, found a real trap
  (`RawWindowHandle::Web` reaching for the opener's document), avoided it
  correctly, and concluded the wgpu half was "settled by inspection". It was
  not: two lines past the branch it examined, `create_surface` does a `dyn_into`
  that wasm-bindgen implements as a realm-local `instanceof`, and a
  cross-document canvas context fails it — while WebGPU accepts the identical
  object. Reading one branch of a function does not clear the function. The
  general form: **a spike that bypasses the binding cannot certify the
  binding**, and `docs/spikes/dual-head.html` is raw JS by design. What caught
  it was the first run of the first end-to-end test, which is where it should
  have been expected to surface — and the fact that §10a wrote down "still to
  prove end-to-end" is the reason nobody was relying on the inspection.

- **The estimates that held were the ones with a mechanism behind them.**
  `decoder.rs` was estimated at ~250 lines *because* pulling from the render
  loop removes the pacing thread; it came in at 200, and for that reason. The
  §9a constants table was measured, not guessed, and moved cleanly. What was not
  anticipated at all was where the difficulty actually sat in the rewrite —
  not the container, which was already proven, but the *timeline*: wrapping
  versus clamping, negative time, zero-duration samples. Estimating the parts
  you have already reasoned about works; the surprises come from the parts
  nobody has thought about hard enough to estimate.

- **Two moves converted runtime hazards into build errors, and both paid.**
  `pollster` as a `cfg(not(wasm32))` dependency means blocking on a browser
  future cannot compile, rather than compiling and hanging. `BUILTIN_EFFECTS`
  as the source for the shader test means a shipped effect cannot silently stop
  being covered. Neither is clever; both are the same instinct as
  `vidiotic-bake`'s `rayon` feature, and it keeps being the highest-yield habit
  in this port.

- **The one change `cargo check` could not see was the one that moved a
  directory.** Moving `shaders/` broke four consumers; two failed loudly at
  compile time and two — `assets.rs` and `bundle.sh` — would have failed
  silently at runtime, as a black screen and a shaderless bundle. Type systems
  do not check paths. Both now have a test or a hard failure in front of them,
  and the general rule is worth keeping: **when a refactor moves data rather
  than code, ask what reads it by name.**

- **ffmpeg's mov muxer was silently dropping the last frame of every bake.**
  Found by `mov_roundtrip.rs`, then confirmed on the real path against a real
  clip: `bun.mov` bakes to 30 frames and the output file contained 29. The cause
  is that the bake supplies packets with no explicit duration, so for the final
  sample — which has no successor to difference against — libavformat has no
  duration to write, and the sample does not survive. `vidiotic-prep`'s exported
  clips have therefore always been one frame short of the length recorded in
  their `.viproj` metadata.

  **Sharpened once we had our own demuxer to look with (step 4).** The frame is
  not absent from the file. Reading `clips/*.mov` — all three written by the old
  libavformat path — shows the sample tables declare 99 / 90 / 66 samples while
  ffmpeg's *demuxer* yields 98 / 89 / 65. The last sample is present, indexed by
  `stsz`/`stco`, byte-complete (it ends exactly at `mdat`'s end, with zero
  slack), and decodes cleanly as HAP. What the muxer actually wrote is a
  trailing `stts` run of `(count 1, delta 0)` — a frame declaring **zero
  duration** — and ffmpeg's demuxer then trims it away as having no extent.

  So the defect is one step earlier than "the frame is lost": a missing packet
  duration becomes a zero-duration sample, and a zero-duration sample is
  invisible to the reader. The correction matters practically, because it means
  **the pixels in every clip already exported are recoverable** — the file is
  not truncated, only mis-timed. `mov_demux.rs::the_zero_duration_tail_frame_is_real`
  asserts all of this against the real fixtures so the finding cannot rot, and
  `our_own_muxer_never_writes_a_zero_duration_sample` asserts the fix from the
  other side.

  **Re-baking is now safe, and it was not before.** The zero-duration sample
  also skews the source's `avg_frame_rate`, which the bake used to trust — so
  re-baking an affected clip traded the missing frame for a 1.1% speed error.
  See the frame-rate entry below; both are fixed, and a re-bake today recovers
  the frame *and* the correct rate, and writes the correct `fps` into the clip's
  `.viproj` metadata.

  Fixed by owning the muxer (step 2). Three things now hold the line: an
  `anyhow::ensure!` inside `run_span_with` comparing samples written against
  frames emitted; `bake_integrity.rs`, which bakes a real clip and counts what
  comes back; and a test that asserts libavformat *still* drops the frame, so
  that if a future ffmpeg fixes it the stale rationale in `transcode.rs` fails
  loudly rather than quietly misleading whoever reads it next.

  The general lesson, which is the reason the muxer work was worth doing before
  any browser code: **a frame count taken from the encode loop is not evidence
  about the file.** Nothing had ever read the output back and counted.

- ~~**The bake's millisecond timeline quantizes frame timing.**~~ **Done — and
  investigating it found a second, worse defect underneath.**

  The recorded symptom was that ffprobe reported a 30 fps bake as ≈30.33 fps,
  attributed to millisecond rounding. That attribution was wrong. Rounding is
  real but tiny: `pts = round(idx * 1000 / fps)` has an error ≤0.5 ms and does
  **not** accumulate, so against a 16.7 ms display frame it is invisible. It
  could not produce a 1% rate error.

  The 30.33 was **an inherited frame rate**. `transcode.rs` derived fps from
  `avg_frame_rate`, which is `frames / duration` — and therefore inherits any
  error in the declared duration, including the one the old libavformat muxer
  introduced. Every clip that muxer wrote has a zero-duration final sample, so
  its duration is one frame short and its `avg_frame_rate` correspondingly high:
  `clips/bun.mov` declares `r_frame_rate = 30/1` and `avg_frame_rate =
  30000/989 = 30.334`. Re-baking one produced a file that played **1.11% fast** —
  measured, 2.934 s for 89 frames where 30 fps is 2.967 s, short by exactly one
  frame. At 120 bpm a 4-bar loop drifts ~89 ms.

  **The trap was that re-baking is exactly what someone does to pick up the
  muxer fix.** The old defect cost a frame; the fix for it, applied naively,
  cost a tempo.

  Both are fixed together, since both change every baked file's timestamps and
  one re-bake beats two:

  - `pick_fps` prefers `r_frame_rate`, and warns when the two fields disagree.
    Not a straight swap — `r_frame_rate` is derived from timestamp *spacing*, so
    our own pre-fix millisecond output reports **1000 fps**, and "always prefer
    r" would have been worse than what it replaced. Hence a plausibility ceiling
    of 240 fps, with `avg_frame_rate` as the fallback above it.
  - `timeline(fps)` derives the timescale as `round(fps * 1000)` with every
    duration exactly 1000, so `timescale / duration` *is* the frame rate.

  Measured after, on the same clip: `r_frame_rate=30/1`, `avg_frame_rate=30/1`,
  `time_base=1/30000`, `duration=2.966667`. Both fields exact and agreeing.

  **The pixels did not move.** All 89 HAP payloads are byte-identical across the
  change — nothing here touches `frame.rs`. Only container timing differs, which
  also means the `/chop` byte-identity claim is untouched.

  Six unit tests pin the picker (including both real traps, with the real
  numbers) and `bake_integrity.rs` gained `every_frame_has_the_same_exact_duration`
  and `a_damaged_source_rate_is_not_inherited`, the latter asserted against the
  actual file that springs the trap — and it prints a notice if that fixture is
  ever replaced with a clean one, since it would then pass while proving nothing.

  **The general lesson, again:** the first account of a bug was assembled from a
  plausible mechanism rather than from measurement, and it was wrong. Milliseconds
  were a real imprecision sitting next to the actual fault, which made them a
  convincing culprit.

- **`app.rs` on both sides is the real risk.** 2,645 lines in `vidiotic` and
  1,685 in `vidiotic-prep`, both with fs/threading/IPC woven through. Every
  estimate above is confident about the leaf modules and speculative about these
  two. Expect them to dominate the schedule.
- **Async contagion is underestimated here.** OPFS is async; `std::fs` is not.
  Every load path that is currently a synchronous call inside a UI frame becomes
  a state machine. This is a bigger tax on `/chop` than the ffmpeg removal.
  P0 dodged this entirely — a file input hands over a `Vec<u8>` and `Clip::open`
  is synchronous — so it remains completely untested, not disproven.

- **Two hazards P0 did not hit, both waiting for the next step.**
  `std::time::Instant` panics on `wasm32-unknown-unknown`, and nothing in
  `vidiotic-play` uses it, so the gate is green while `clock.rs`,
  `sequencer.rs`, `app.rs` and `video/mod.rs:105` are all still holding it. The
  shell uses `performance.now()` directly; the moment the sequencer crosses,
  `web-time` (a drop-in `std::time` shim that `winit` and `egui-winit` already
  depend on) is the answer. It compiles and *then* panics on first call, which
  is the worst failure shape available.
  Separately, `wgpu::Limits::default()` is requested unconditionally. **Checked
  in step 4c: this is fine.** `wgpu-types-29.0.4`'s `Limits::defaults()` is the
  WebGPU spec's mandatory minimum set, which every conformant implementation
  must grant, so `request_device` cannot fail on limits. Chrome/Metal remains
  the only browser actually *run*, but the reason to doubt the others was this,
  and it does not hold.

- **P0 dropped winit from the browser build entirely, and it was the right
  trade at this size.** §10a concluded the output head must avoid winit; step 4
  found no reason for the *control* head to keep it either. That removed
  `spawn_app` (which never returns), `egui-winit`'s wasm feature pitfalls, and
  canvas attachment, in exchange for ~140 lines translating pointer events into
  `egui::RawInput` — and it left the native `gfx.rs` path completely unchanged,
  so `app.rs` and `ui/mod.rs` needed no edits at all. **The debt is keyboard
  input**, which P0 has none of and which `grammar.rs`'s modal verb-object
  vocabulary will need. That is the decision to revisit when `ui/*` is ported:
  extend the input bridge, or bring winit back for the control head only.
- **The scriptable-control story survives, via §10.** Cutting `ipc.rs` initially
  looked like giving up the scriptable-control surface — "anything the UI can do,
  a script can do", documented in `vidiotic-wire`'s module docs and
  `vidiotic/src/ipc.rs`. Dual-head brings it back for free: two windows need a transport,
  and `vidiotic-wire` over `BroadcastChannel` is that transport. Cheap to keep in
  scope; do not let it drift out.
- **HAP's justification changed under §3a, and that is load-bearing.** At the
  480p tier the bandwidth argument for BC1 mostly dissolves; what justifies the
  whole BC pipeline is *random access* — every HAP frame independently decodable,
  so `sequencer.rs` can retrigger on the beat without a keyframe seek. If that
  turns out not to matter in practice, or if WebCodecs seek proves fast enough at
  this resolution, a large fraction of §3 becomes unnecessary. Re-examine after
  step 4, not before.
- **Presentation (§9) is not on the critical path, but one part of it is.** The
  cabinet, grain, and CRT pass can land any time. The 8×8 grid cannot: §9a's
  layout consequences have to be decided before `ui.rs` is ported, or the port
  gets done twice.
