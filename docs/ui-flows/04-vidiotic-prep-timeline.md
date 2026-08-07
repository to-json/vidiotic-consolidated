# vidiotic-prep — Timeline / Prep Flow

## Surface

**vidiotic-prep** is a **video trimming and clip-marking tool** (Rust/egui) for preparing source footage into marked **spans** — segments of video trimmed to frame-exact boundaries and tagged for export. A user loads a video, marks in/out points with keyboard, scrubs and previews frame-by-frame on a zoomable timeline, builds a list of named clips, and exports them as HAP-compressed `.mov` files packaged into a `.viproj` project file that the main **vidiotic** VJ engine can load.

| File | Purpose |
|------|---------|
| **ui.rs** (36 KB) | Panel layout and widget drawing: top toolbar (open/export buttons, media info), central preview, bottom transport bar, right inspector (spans, banks, defaults, control bindings) |
| **timeline.rs** (11 KB) | Zoomable frame timeline with span bands, in/out marks, playhead, and minimap; handles scrubbing, panning, zooming, and mark dragging |
| **preview.rs** (7 KB) | Frame-accurate decoding on demand via ffmpeg; seeks and decodes the current frame for display |
| **control_input.rs** (10 KB) | Key/MIDI/gamepad mapping: 14 built-in key bindings (Space, I/O, J/K/L, arrows, Enter) + user overrides over a layered mapper |
| **export.rs** (10 KB) | Worker thread that transcode spans to HAP `.mov` clips, writes `.viproj` project file, streams progress back over channel |
| **spans.rs** (2.6 KB) | In-memory span list (ordered vector) + selection state; each span carries its own source path so spans persist when switching videos |
| **session.rs** | Autosave: per-source `.vprep` sidecar files (RON) storing spans and session defaults; allows crash recovery and multi-source sessions |
| **commands.rs** | Command enum: 50+ deferred mutations (transport, marks, spans, export, file ops) with no undo yet; single executor in app.rs |
| **engine.rs** | Optional IPC link back to a running **vidiotic** engine via Unix socket; used to hand off an exported project or reload live |
| **app.rs** (60 KB) | Main state machine: PrepApp struct, command executor, playback loop, control event pipeline (keyboard → mapper → command → apply) |

## States & Modes

**Vim-style mode word** (statusline priority: ERROR > EXPORT > PLAY > NORMAL)

| Mode | When | How to exit |
|------|------|-------------|
| **NORMAL** | App is idle, no video or spans loaded | Any action changes the mode |
| **PLAY** | Playback running (`play_speed ≠ 0`) | Press Space or reach in/out mark boundary |
| **EXPORT** | Baking spans to HAP and writing `.viproj` | Wait for worker thread to finish |
| **ERROR** | A command failed (file I/O, decode, export) | Error persists; plain status fades after 6 seconds |

**Playback speed** (`play_speed: f64`)
- `0.0` = paused
- `1.0` = forward 1×
- `−1.0` = reverse 1×
- J/L shuttle: ±1 frame/tick (rate depends on frame time accumulated in `play_accum`)
- Playback **loops** within the pending in/out marks (not the clip endpoints)

**View window** (timeline zoom state: `view_start` frame index, `view_len` frame count)
- Mouse wheel vertical = zoom ±2% per notch (anchored under cursor)
- Shift+drag on timeline = pan horizontally
- Middle-click drag = pan horizontally
- "fit" button = zoom to show whole clip
- "to marks" button = zoom to frame range [in..out)

**Selection** (`spans.selected: Option<usize>`)
- Affects which span is shown in the inspector
- Used by audition loop and span retrim buttons
- Cleared when the selected span is deleted

## Flow (step by step)

### Representative session: load → scrub → mark spans → preview → export

1. **App launches** → `PrepApp::default()` initializes empty state; discovers running **vidiotic** engine if present; loads user's global control map and prep.vmap overrides
   
2. **User: "open video…"** (top toolbar button) → Opens file picker (video/mov/mp4/mkv/webm) → Posts `Command::Open(path)`
   
3. **Command::Open applied** → Determines file type: if `.viproj`, calls `open_project()` (reconstructs spans from export metadata); if video, calls `request_open()` → Checks file size against `LARGE_FILE_BYTES` (`512 * 1024 * 1024` = 512 MB, a named const at app.rs:33): if larger, parks in `pending_open` and shows confirmation dialog; else directly calls `open_video()`
   
4. **User confirms large file (or file is small)** → `open_video_then()` opens source via `SourceMedia::open()` (ffmpeg probe: fps, frame count, dimensions) → Decodes first frame for preview → Sets `pending_in=0, pending_out=frame_count` → Posts `Command::ZoomFit` to frame the whole clip
   
5. **Session file restore** → On first video open, looks for `<video>.vprep` sidecar → If found, merges stored spans into `app.spans` (adopts banks/defaults/controls only on *first* open of the session, to preserve per-source settings when switching videos)
   
6. **User scrubs on timeline** → Mouse drag on main strip: if near in/out bracket, drags that mark; else seeks playhead → Vertical scroll = zoom ±2% anchored under cursor → Minimap click/drag = pan view window to cursor
   
7. **User sets marks with keys** (I/O) or buttons (Set In / Set Out) → `Command::SetIn` captures `cur_frame → pending_in`; `Command::SetOut` captures `cur_frame → pending_out` (exclusive, so next frame after last included frame)
   
8. **User plays back** → Space toggles play; J/L shuttle ±1 frame/tick; arrows step ±1 or ±10 frames (holding arrows repeats steps; holding Space does not) → Playback loops within [pending_in..pending_out) → Can play from in mark with Shift+Space → Preview frame updates every frame via `update_preview_texture()` (decode on demand via ffmpeg)
   
9. **User adjusts marks (optional)** → Drag in/out brackets on timeline OR edit frame numbers in inspector → Or use "snap out" feature: `pending_out = pending_in + (N beats × bpm ÷ 60 fps)` → Click retrim buttons to load marks from a selected span into the UI for adjustment
   
10. **User: "add span"** (Enter/A key or button) → Validates `pending_out > pending_in` → Posts `Command::AddSpan` → Creates span with auto-name ("span 1", "span 2", …), marks [pending_in..pending_out), stores source path, selects the new span, opens it in inspector
    
11. **User edits span** in inspector list:
    - Double-click span number = audition (load marks, set playback 1×, ensure source is open)
    - Click #N = select span (opens its inspector row; seeks to its in point if source is open)
    - Edit name, in/out frames directly
    - Check "bpm" + adjust = per-span override (session default used if unchecked)
    - Pick clip bank (which "folder" this clip goes into on export)
    - "retrim" button = load this span's marks; "update" button = write current marks back to this span (overwrite range)
    - Up/down buttons = reorder spans
    - Delete button = remove span
    
12. **Multi-source sessions** → Switch open video (File → Open Video) → Spans from previous video are retained in `app.spans` (each span carries its `source: PathBuf`) → Timeline only draws spans matching currently open source → When reopening a different source, its `.vprep` sidecar is loaded (banks/defaults preserved from *first* video, so switching doesn't stomp settings)
    
13. **User: "export…"** (button enabled when spans exist) → Shows export dialog:
    - Pick destination folder
    - Enter project name
    - Checkboxes: "starter cue bank" (one full-length cue per clip), "high-quality" (ClusterFit vs RangeFit BC1 bake)
    - Shows span count
    - Once destination + name are set, "export" button becomes active → Posts `Command::StartExport`
    
14. **Export worker spawns** → Runs on background thread (app doesn't block) → For each span:
    - Probes the span's source video (fps)
    - Transcodes [in_sec..out_sec) from source to `dest/clips/{idx}_{name}_{in}-{out}.mov` (HAP format)
    - Streams progress back: span #/total, frames done/total, encode fps, ETA
    - Collects `ClipSpec` with path, bpm, fps, frame count, provenance (original path + frame range)
    - Groups clips by clip_bank index
    - Creates starter cue bank if enabled (one full-length cue per clip)
    - Writes `dest/{project_name}.viproj` (serialized Project with clips, banks, cues, controls, defaults)
    - Sends `ExportMsg::Done(proj_path)` on success or `ExportMsg::Error(msg)` on failure
    
15. **Export completes** → Dialog shows "wrote <path>" + "reveal" button (Finder) + conditional "send to vidiotic" button (if engine is reachable AND was the one that launched prep)
    - "send to vidiotic" button = asks engine to load the exported project (destroys the live set; only auto-offered if engine launched prep)
    - "reveal" button = `open -R <path>` to show in Finder
    
16. **User quits** → If spans exist and have never been exported since last change, shows confirmation "N span(s) haven't been exported yet" → Options: export now, quit without exporting, cancel → On quit, autosaves final session state (sidecar `.vprep` files)
    
17. **Session persists** → Each source video has a `.vprep` sidecar that autosaves spans + banks + defaults + snap_beats + controls (for that video's spans only). Sidecar is updated every ~1 second (throttled via `last_autosave_check`) when any document state changes.

## Timeline & preview interactions

### Timeline widget (timeline.rs)
- **Main strip** (44 px tall)
  - Span bands: one bar per span, colored by clip bank (golden-angle hue walk), selected span has accent border
  - In/out marks: thin vertical lines with inward feet at top/bottom; semi-transparent accent fill between them
  - Playhead: vertical line + top triangle cap
  - Zoom window visible as semi-transparent rectangle in minimap below
  
- **Minimap** (14 px tall)
  - 1:1 pixel-to-frame scale showing whole clip
  - Colored span bands (same hues as main strip)
  - Bright outline = current view window
  - Click/drag = pan main view to cursor (center the view on clicked frame)
  
- **Interaction** 
  - Drag on main strip: if within 6 px of in/out bracket, drag that edge; else seek playhead (pauses during drag)
  - Drag with Shift or middle button = pan view horizontally
  - Hover over in/out bracket = cursor changes to ⟷ (resize)
  - Vertical scroll = zoom ±2% per notch (0.99^scroll_delta), anchor under cursor
  - Horizontal scroll = pan view by pixels/ppf

### Preview (central area, below timeline)
- Shows scaled frame (scaled to fit window, preserving aspect ratio, max 1× native)
- Updated every frame via `update_preview_texture()` → Requests frame at `cur_frame` from `SourceMedia::frame_at()` → Uploads RGBA pixels to egui texture
- No interactive painting; display only

### Frame-accurate decoding (preview.rs)
- `SourceMedia::open()` probes fps, frame count, duration; scales preview to fixed width (preserving AR)
- `frame_at(idx)` seeks and decodes on demand; caches `last_decoded_frame` to avoid re-seeking for sequential access
- Forward fast-path: if idx == last+1, decode sequentially without seeking (no seek overhead on scrubbing forward)
- Seek rebuilds ffmpeg decoder state, then decodes forward until a packet arrives whose timestamp rounds to `idx`

## Key bindings

### Built-in defaults (control_input.rs, hardcoded)

| Key | Command | Notes |
|-----|---------|-------|
| **Space** | Toggle play | Pause if playing, play if paused |
| **Shift+Space** | Play from in | Seek to in mark, play at 1× |
| **J** | Shuttle −1 | Decrement speed by 1 frame/tick (reverse speed) |
| **K** | Pause | Explicit pause |
| **L** | Shuttle +1 | Increment speed by 1 frame/tick (forward speed) |
| **I** | Set in | Capture `cur_frame → pending_in` |
| **O** | Set out | Capture `cur_frame → pending_out` (exclusive) |
| **Enter** / **A** | Add span | Create span from marks [pending_in..pending_out) |
| **→** | Step +1 | Pause, step forward 1 frame (repeats on hold) |
| **Shift+→** | Step +10 | Pause, step forward 10 frames (repeats on hold) |
| **←** | Step −1 | Pause, step back 1 frame (repeats on hold) |
| **Shift+←** | Step −10 | Pause, step back 10 frames (repeats on hold) |
| **Home** | Seek start | Pause, seek to frame 0 |
| **End** | Seek end | Pause, seek to last frame |

### Remapping

- **Project control map** (visible in inspector, "controls (this project → vidiotic)")
  - Editable table of bindings that serialize into `.viproj`
  - Resolved by vidiotic (the player), not prep
  - Prep only edits and stores; doesn't resolve it
  - Offer `Action::player_catalog()` (vidiotic's verbs only)
  
- **Prep's own keys** (visible in inspector, "editor keys (this app)")
  - Bindings stored in user's global `~/.config/prep.vmap` (NOT in the project)
  - Layer over the built-in defaults: rebind any key or add `Action::Nothing` to mask a default
  - Offer `Action::prep_catalog()` (prep's verbs only)
  - "Learn" mode: click binding row → press a key to capture it
  - "reset to defaults" button = clear all overrides, revert to hardcoded

### Key event pipeline
1. `egui::Event::Key` arrives in `eframe::App::ui()`
2. `pump_controls()` converts egui key to `ControlSource::Key` (via `vidiotic_ctl::keys::canon()`)
3. Only if UI is not capturing keyboard (no text field focused)
4. `Mapper::resolve()` checks prep.vmap overrides first, then falls through to built-in defaults
5. `control_input::resolve()` → `control_input::to_command()` maps `Action::Prep(verb)` → `Command`
6. Only `Command::Step(_)` fires on key-repeat; holding Space does NOT re-fire play/pause
7. `app.post(cmd)` enqueues command
8. `drain_commands()` runs command executor after panels finish drawing

### MIDI and gamepad
- `MidiHub` listens for MIDI CC and note events; `PadPoller` polls gamepad axes/buttons
- Events sent to `ctl_tx` channel, received in `pump_controls()`, go through same mapper/resolver
- Not gated by text field focus (e.g., a pad press fires while naming a span)

## Observations

### Confusions / Inconsistencies

1. **PendingOpen two-stage flow** (app.rs line 315–350)
   - Videos over `LARGE_FILE_BYTES` (512 MB, named const at app.rs:33) park in `pending_open` and need user confirmation before actually opening
   - Confusion: small videos open immediately with no dialog; behavior invisible to user unless they check file sizes (the 512 MB cutoff is arbitrary and undocumented in the UI)

2. **Span frame range validation**
   - Spans enforce `out_frame ≥ in_frame + 1` (minimum 1-frame clip)
   - Pending marks are set/dragged freely; validation only happens when creating or updating a span
   - Dragging an out mark below in+1 on the timeline is possible (UI allows it), but adding a span with inverted range is clamped in place

3. **Multi-source span ownership via PathBuf**
   - Each span carries its source as `PathBuf`; session file (`.vprep`) is stored next to that source
   - When you open a different video, its `.vprep` sidecar is loaded separately
   - If you edit a span from video A, then switch to video B, then back to A: are the edits persisted? (Likely yes via autosave, but unclear in the flow)
   - Spans from different videos coexist in `app.spans`, but timeline only draws spans matching currently open source — can be confusing if you have 10 spans but only 3 are visible

4. **Marks loop playback, not spans**
   - Playback loops within `[pending_in..pending_out)`, NOT within the selected span's range
   - User might expect audition (double-click span) to loop only that span, but the loop boundary is the UI's pending marks, not the span's range
   - "audition" does load the span's marks, so it *is* that span — but the mechanic is indirect

5. **Session defaults adoption**
   - First video opened in a session: adopt banks/defaults/controls/snap_beats from its `.vprep`
   - Subsequent videos opened: ignore their sidecars' session defaults, only merge spans
   - Rationale: prevent editing defaults on video 1, switching to video 2, and having video 2's old defaults silently overwrite your changes
   - But this means if you manually change defaults on video 2, then open video 1 again, video 1's old defaults (from first load) are *never* restored — you're stuck with the modified state
   - User likely expects each video to carry its own defaults; this layering is subtle

6. **Export fingerprinting for dirty state**
   - Quit dialog only fires if `last_export_fingerprint` (a string hash of span list) differs from current
   - If you add a span, export, remove the span, and quit: the fingerprints are identical, no warning
   - Edge case: "undo" doesn't exist yet, so this is less critical, but could trap a user who exports, makes changes, and doesn't export again

7. **Banks can't be empty**
   - UI hides delete button when only one bank remains, but code enforces it too
   - Spans always reference a bank by index; if a span refers to bank 2 and you delete bank 1, the span's index is decremented
   - No orphaning, but reordering on delete is invisible to the user

8. **Bake quality trade-off**
   - "high-quality" checkbox = BC1 ClusterFit (6× slower) vs RangeFit
   - No indication of what "slower" means in absolute time for a typical project
   - Checkbox hint says "fine for iterating" for fast mode, which implies slow mode is for "final" — but when to choose is left to user intuition

### Dead ends / unimplemented

1. **No undo/redo** — Document state changes are commands, but no executor logs or undo stack; all data loss is permanent
2. **No multi-select for spans** — Only one span selected at a time; batch operations (delete 5 spans, reorder 3) must be done one by one
3. **No search/filter for spans** — No way to find a span by name in a large list
4. **No keyboard shortcut discovery** — No menu or `?` key to show all bindings; hidden in collapsible inspector section
5. **Autosave only on change** — If you open video, tweak a span name, then crash without focus leaving the field, you lose that edit (autosave only fires on throttled checks + document state change events)
6. **No batch export / scene stacking** — One project per export; can't export the same span set with different settings (quality, codec, frame rate) in one pass

### Confusing UI elements

1. **"Retrim" button** → Loads span into marks for adjustment; name suggests "retrim" but it's actually "load for retrim"
2. **"Update" button** → Overwrites span from marks; could be labeled "save to span" or "commit marks"
3. **Span "source" badge in inspector** → Only shown when multiple sources are open; disappears if you close all but one video, making the UI jitter (resizing the row)
4. **"Reopen" button** → Shown when span's source is not currently open; opening it jumps to the span's in point and loads marks, but "reopen" doesn't clearly communicate this

### Strengths

- **Frame-accurate preview** with ffmpeg on-demand decode and seek caching
- **Timeline visual feedback**: spans, marks, playhead, zoom window all clearly visible
- **Session persistence** via sidecars: crash recovery and multi-source sessions work
- **Flexible control mapping** with prep own keys + project controls, learned via binding table
- **Export worker thread** doesn't block UI; progress stream back ~10 Hz
- **Layered mapper** allows project-specific bindings without shadowing user defaults
