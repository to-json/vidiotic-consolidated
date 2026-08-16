# Refactor remainder — notes

What the first pass of obvious simplifications deliberately left alone, and what
each item would cost to do. None of these were done because each is either a
mechanical consolidation with no behaviour change (safe to fold into other work)
or a change that touches behaviour and therefore wants its own session. They are
recorded here so the analysis is not re-done by whoever meets them next.

Already done, for context, in the same pass:

- **camera shell dedup** — `Engine::camera_rows` / `add_camera_cue` /
  `relink_camera` in `vidiotic-play/src/engine/cameras.rs`, now the single
  session half shared by `vidiotic::app::cameras` + `mirror.rs` (native) and
  `web::Shell` (browser). Each shell keeps only its machine half: the
  enumeration as `(uid, name)` pairs and a status closure. The two shells'
  `build_camera_rows` and `relink_camera` bodies are gone.
- **action catalogs** — one list in `vidiotic-ctl/src/model.rs` expands to
  `PLAYER_CATALOG` / `PREP_CATALOG` / `CATALOG`, removing the hand-synced
  subset bug (the full catalog silently omitted `ToggleCommandPalette`).
- **undo stacks** — `vidiotic-core::undo::SnapshotHistory<T, Tag, Clock>` is
  the shared stack behind `vidiotic-chop`'s and `vidiotic-play`'s `UndoStack`
  (with `f64` and `web_time::Instant` clocks). `vidiotic-ctl` keeps its local
  `History<T>` because core already depends on ctl for `ControlMap`, so ctl
  cannot import core without a cycle.
- **ISF value model** — `IsfValue` / `IsfInputKind` / `IsfInput` are defined
  once in `vidiotic-core::isf` and re-exported by the wire crate, deleting the
  to/from converters in `vidiotic/src/ipc.rs`. The wire JSON is unchanged.

---

## 1. The wgpu texture ceremony — `vidiotic-play/src/render.rs`

Three ceremonies, all mechanical, none behaviour-changing:

- **Sized-texture creation**: `TextureDescriptor { … }` written out at ten
  sites (394, 411, 429, 460, 943, 956, 1410, 1448, 1461, 1501). The
  descriptors differ only in label, size and format — `mip_level_count: 1`,
  `sample_count: 1`, `dimension: D2`, `view_formats: &[]` are identical.
- **View creation**: `create_view(&TextureViewDescriptor::default())` at
  eleven sites (408, 425, 443, 474, 953, 966, 1424, 1458, 1494, 1515), every
  one the default descriptor. This is a free collapse.
- **RGBA upload**: the full `write_texture` ceremony at five sites (900, 1020,
  1045, 1475, 1576), differing only in `bytes_per_row`/`rows_per_image` and the
  destination extent.

`upload_audio` (1019–1063) already contains the precedent: a local `write_row`
closure that does the whole upload for two textures instead of writing the
ceremony twice. The pattern to promote is a pair of helpers — `tex2d(device,
label, format, w, h)` and `upload_rgba(queue, tex, w, h, stride, data)` — which
accounts for most of the ~100 net lines.

What does **not** collapse cleanly, so a helper must not absorb it:

- the **BC block textures** in `create_video_texture` carry block-alignment
  padding (`(w + 3) & !3`) and a paired `Bc4RUnorm` alpha plane; `create_video_texture`
  is already the collapsed form of that,
- the **ping-pong buffers** (1410, 1448) have `RENDER_ATTACHMENT` in their
  usage, unlike the upload-only textures,
- **bind-group assembly** is per-texture and out of scope.

Low risk; it is pure deletion of repetition. Do it whenever render.rs is next
touched for something real, or not at all — there is no correctness pressure.

## 2. The ffmpeg decode → scale → repack-stride glue, ×3

Three sites run the identical tail — `scaler.run(&decoded, &mut rgba)` then a
row-wise copy collapsing the scaler's `stride` into tightly-packed RGBA:

- `vidiotic-core/src/clippool.rs:156–201` — `first_frame_rgba`, the thumbnail
  path, gated on the `ffmpeg` feature,
- `vidiotic/src/video/decoder.rs:336–388` — `run_software`'s `send_rgba`
  closure; also paces and wraps the result in a `DecodedFrame` (stride kept),
- `vidiotic-prep/src/preview.rs:129–160` — `frame_at`; scales to
  `preview_w × preview_h` rather than the source size.

The genuinely shared piece is only the ~8-line copy:
`packed[y*row .. (y+1)*row].copy_from_slice(&src[y*stride .. y*stride+row])`.
The three sites otherwise differ in where the stride goes, what size the target
is, and what error type they speak. The honest cost of sharing is a small
`#[cfg(feature = "ffmpeg")]` helper in `vidiotic-core` (e.g. `repack_rgba`) that
the two native crates and prep would import — worth doing only because this is
exactly the kind of copy that rots, where one site gets a padding/alignment fix
and the other two silently keep an old assumption. If it is done, the helper
should take `(src, stride, w, h) -> Vec<u8>` and stay free of scaling, so the
three call sites keep their different sizes.

## 3. The wasm thumbnail re-implements the BC decompress `softdec` owns

`web/mod.rs:218–226` matches `HapTextureFormat` and calls `texpresso` Bc1/Bc3
decompress directly to full-resolution RGBA, then hand-downscales to a 128×86
thumbnail by nearest-neighbour sampling (233–243). The CPU BC expansion the
soft-decode fallback already needs is `vidiotic-play/src/video/softdec.rs:86`
(`to_rgba`).

The duplication is not total — `to_rgba` returns the decoded frame, which the
thumbnail already needs at full resolution to sample — but the format match and
the texpresso calls are the same knowledge written twice, and softdec covers
more formats (Bc4 among them) than the thumbnail's two-arm match. A thumbnail
that decoded through `to_rgba` would inherit the full format list and could
never disagree with the soft-decode path about what a clip's blocks mean.

Lower priority than 1 and 2: the duplicated match is ~9 lines, the two paths
are unlikely to drift on format names, and the thumbnail's own downscale is a
deliberate size choice, not a bug source.

## 4. The four `CustomEvent` request helpers — wasm only, just do it

`web/mod.rs:1262–1325`: `request_file`, `request_cameras`, `request_camera`,
`deliver_file` each repeat the same ~18 lines — `CustomEventInit::new`, set a
detail, `CustomEvent::new_with_event_init_dict`, `dispatch_event`, log the
error — differing only in the event name and the detail value. Four × 18 →
roughly 20 once a `fn dispatch(name: &str, detail: &js_sys::Object)` exists.
Purely mechanical, wasm-only, no risk. This is the item on the list that should
simply be done, not scheduled.

## 5. The browser flat keys are a hand-written subset of `default_map`

`web/mod.rs:384–395` maps canonical key names to verbs in a hardcoded match
(`Space`, `=`, `+`, `-`, `[`, `]`, `r` with/without shift). The native source
of truth is `vidiotic-ctl::control_input::default_map`, resolved through
`Mapper`. The web table is a deliberately small subset of it — and nothing
connects the two.

This is the one item that changes behaviour rather than deleting ceremony, so
it is recorded rather than folded in. Driving the flat keys from `default_map`
through the same resolution native uses would mean a binding added natively
appears in the browser for free — but it would also import the native binding
set wholesale (modifiers, cadence keys, every mapped command) unless the web
shell filters, and the canonical-key spelling is winit's, which the browser
input layer already adapts. Deciding that the two tables should move together
is a product question (does the page want every native binding? which
subset?) before it is a code one. The risk of the status quo is drift — the
same failure mode the catalog-sync bug had — but at a smaller scale and with a
native tester present to catch it.

---

One finding that belongs with these notes because it is the same failure mode
as #5: during the catalog work it surfaced that `CATALOG` had omitted
`ToggleCommandPalette` while `PLAYER_CATALOG` included it — a hand-synced
subset that had silently diverged. The macro fixed the instance; the pattern
(one table being a hand-written subset of another) is what #5 and, more mildly,
#3 still are.
