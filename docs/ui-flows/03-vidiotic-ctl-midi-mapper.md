# vidiotic-ctl — MIDI Mapper Flow

## Surface

**vidiotic-ctl** is a graphical MIDI/keyboard/gamepad control mapper that lets users bind physical controls to actions. It edits `.vmap` files (RON-serialized `ControlMap`) which are consumed by both `vidiotic` (VJ player) and `vidiotic-prep` (video editor). The tool runs as a standalone desktop TUI/GUI in egui, with persistent storage in `~/.config/vidiotic/global.vmap` and `~/.config/vidiotic/prep.vmap`.

**Key files:**
- `app.rs` — `CtlApp` struct, state machine, learn orchestration
- `panels.rs` — layout and chrome (toolbar, device list, event monitor)
- `ui.rs` — shared binding table widget (reused by `vidiotic-prep`)
- `learn.rs` — MIDI-learn logic (first deliberate actuation capture)
- `pad.rs` — gamepad polling via `gilrs`
- `midi.rs` — MIDI I/O via `midir` / `CoreMIDI`
- `keys.rs` — keyboard name normalization (egui ↔ winit bridge)
- `model.rs` — data model: `ControlSource`, `Action`, `Binding`, `ControlMap`
- `store.rs` — RON persistence and versioning
- `event.rs` — `ControlEvent` normalization

## States & Modes

The app is driven by three persistent states in `CtlApp`:
- **`map: ControlMap`** — the binding table (vec of `Binding` structs)
- **`learn: Option<usize>`** — if `Some(idx)`, binding at `idx` is capturing next actuation (MIDI learn mode)
- **`dirty: bool`** — tracks unsaved changes
- **`monitor: VecDeque<ControlEvent>`** — rolling history (12 events) of live control inputs

**Modes** (reflected in statusline):
- **NORMAL** — no active learn session, no errors
- **LEARN** — capturing next actuation (statusline shows "LEARN", row highlighted in accent color, label shows "(learning…)")
- **ERROR** — a file op failed (statusline shows "ERROR" in error color)

**Learn Mode Details** (`learn.rs`):
- When user clicks "learn" button on a binding, `CtlApp::start_learn(idx)` creates a fresh `Learn` session
- The `Learn` struct watches `ControlEvent`s and captures the first one that looks deliberate:
  - **Pressed** events (keys, MIDI note-on, gamepad buttons) → immediate capture
  - **Released** events → never capture
  - **Continuous** events (CC faders, gamepad axes) → capture only if value moves ≥8% (`CAPTURE_THRESHOLD`) from first baseline; filters jitter and stick centering noise
- Once captured, the `ControlSource` is written to `map.bindings[idx].source` and learn exits

## Flow (step by step)

Representative session: **open → add binding → bind MIDI CC → set action → save**

1. **Start** → App launches, loads `global.vmap`, shows 4 panels (toolbar / central binding table / right device panel / bottom event monitor)

2. **Inspect current bindings** → Central panel shows each binding as a row:
   ```
   [source key]  [learn button]  [delete button]
   [action picker row]
   ```
   Each source is displayed as a canonical key string (e.g. `"midi:ch1:cc:21@Launchkey Mini MK3"`). Existing bindings are editable.

3. **Add new binding** → Click "add binding" button at bottom of table
   - `CtlApp::add_binding()` pushes placeholder binding with empty source + `Action::Nothing`
   - Immediately calls `start_learn(idx)` on new binding
   - **Mode → LEARN**, new row highlighted, label shows "(learning…)"

4. **Physically actuate control** → Turn a knob, press a key, press a gamepad button
   - Event flows through one of three channels:
     - **MIDI**: USB device → `midir` callback → `parse()` → `ControlEvent` on thread-safe channel
     - **Keyboard**: egui event → `app.offer_keys()` → `ControlEvent` on channel
     - **Gamepad**: main-thread poll → `gilrs` → `ControlEvent` on channel
   - All channels feed `app.rx` (crossbeam unbounded channel)

5. **App ingests event** → `app.ingest(ev)`:
   - If `learn.is_some()`, feed to `learner.observe(&ev)`
   - `Learn` logic decides if it's deliberate (see Learn Mode Details)
   - If yes, return `Some(source)` → write to `map.bindings[idx].source`, set `learn = None`, set `dirty = true`
   - **Mode → NORMAL**, row label now shows the captured source key
   - Event always appended to `monitor` ring buffer (rolling 12 events, shown in bottom panel)

6. **Pick action** → Row now shows action picker below the source:
   - Segmented buttons for namespace (if multiple apps in catalog): "player" / "prep" / none
   - Segmented buttons for verb within namespace
   - Drag values for any params (e.g. `BpmDelta { amount }`, `SetBpm { min, max }`)
   - User selects action kind and tweaks params
   - Picker calls `action_picker()` which tracks `changed` and updates `map.bindings[idx].action`
   - Set `dirty = true`

7. **Repeat** for more bindings (steps 3–6)

8. **Save** → Click toolbar "save" button or press Ctrl+S (if bound):
   - `CtlApp::save()` serializes map to `~/.config/vidiotic/global.vmap` via `store::save_map()`
   - Sets `dirty = false`, shows status "saved …"
   - If error, sets `status_is_error = true`, statusline shows "ERROR"

9. **Revert** → Toolbar "revert" button → reload from disk, clear `dirty`, clear `learn`

10. **Open/Save-As** → File dialogs to switch maps, live-edit separate `.vmap` files

## Panels / layout

**Top: Toolbar**
- Buttons: `save`, `revert`, `open…`, `save as…`, rescan device list
- Path label: current `.vmap` file
- Dirty indicator: `*` appears if unsaved

**Central: Binding Table** (scrollable)
- Each binding is one row:
  - Source label (monospace, accent color if learning)
  - "learn" button (click to enter learn for this binding)
  - "delete" button (remove binding entirely)
  - Action picker (2–3 segmented button rows + optional drag values)
- "add binding" button at bottom

**Right: Device Panel** (resizable, ~220px default)
- "midi" section: lists all connected MIDI device names (e.g. "Launchkey Mini MK3")
- "gamepad" section: lists connected gamepad names
- Rescanned every ~2 seconds (polled, not hotplug-callback driven by `midir`/`gilrs`)

**Bottom: Monitor Panel** (resizable, ~140px default)
- Real-time log of last 12 control events
- Format: `source_key` + value (e.g. `"midi:ch1:cc:21@Launchkey  0.84"` or `"key:shift+t  pressed"`)
- Scrolls with new events at bottom, useful for debugging what's firing

**Statusline**
- Left: mode word (NORMAL / LEARN / ERROR) with color coding
- Middle: file path, binding count, dirty state (e.g. `"~/.config/vidiotic/global.vmap · 12 binding(s) · dirty"`)

## Key bindings

The tool itself defines no explicit keyboard shortcuts in code. Navigation is mouse-driven via egui buttons. However:
- Bindings *to* keyboard keys are first-class in the model: users can bind `Shift+T`, `Ctrl+Space`, etc. as sources
- When in learn mode with a keyboard binding, pressing a key immediately captures it
- Modifier keys (Ctrl, Alt, Shift, Cmd/Cmd) are tracked in the source; key names are canonicalized (letters lowercase, punctuation as literal chars: `"["` not `"OpenBracket"`)

No documented hotkeys for the editor UI itself (save, undo, etc. are toolbar buttons).

## Observations

**Design strengths:**
- Clean separation: model (nanoserde) is toolkit-agnostic, UI (egui) is separate, MIDI/gamepad backends don't touch app state (thread-safe channels)
- Learn mode filtering (8% threshold) is smart: genuinely filters jitter while remaining fast to capture
- Normalized `EventValue` enum (Pressed / Released / Continuous 0–1) unifies all input types downstream
- Two-level action picker (namespace + verb) scales to multiple apps sharing one file format

**Confusing or surprising:**
- **Device names in source**: `ControlSource::MidiNote { device: "Launchkey Mini", … }` — the `device` field is the *concrete device name* in a live event, but in a binding's `device` field can be `""` (empty string) to mean "any device". This duality is never explicitly documented in the model, only in a comment (`"device" is "" in a binding to mean "any device"` in model.rs:14–15). The UI doesn't expose this mapping — learn mode captures concrete names, but `readonly_map()` exists as a layer feature to show shadowing by parent maps. This asymmetry could trip users.
- **No "save on exit" warning**: if user edits bindings, clicks close without saving, there's no "unsaved changes?" dialog. The `dirty` flag exists but isn't used as a guard.
- **Learn timeout**: there's no timeout on learn mode. If user clicks "learn" and then walks away, the session stays open indefinitely, capturing jitter into the log. The UI doesn't show a "cancel" button or escape key binding for this.
- **Prep verbs vs. player actions**: the shared catalog is confusing at first — `vidiotic-prep`'s actions are nested as `Action::Prep(PrepVerb::…)` to avoid a breaking change when prep's bindings were added. Works, but the namespace picker in the UI only shows if >1 namespace exists; a user editing a file with both player and prep bindings might not realize they're picking across two vocabularies.
- **No visual indication of "any device"**: rows binding `device: ""` are not visually distinguished from concrete devices in the table. A user might bind `"" → SetBpm` intending "any MIDI device" but if they reload or compare with another map, the empty device isn't obviously special.

**Dead ends / unclear UX:**
- **readonly_map() unused in ctl bin**: the `readonly_map()` widget in ui.rs (a dimmed list of bindings with a "mask" button) is referenced in comments but only used in `vidiotic-prep`'s inspector, not the main ctl editor. It's a layer-based override system for shadowing that doesn't appear in the ctl UI.
- **Action params out of sync**: if a user switches action kinds (e.g. `SetBpm { min: 60, max: 180 }` → `BpmDelta { amount: 1 }`), params reset to catalog defaults. This is by design (`action_params()` only displays params, doesn't drive a picker), but users might not expect it.
- **No action search/filter**: action picker is hard-coded segmented buttons; for 30+ actions split across namespaces, there's no search or quick-jump.

**Code quality observations:**
- Clean: minimal dependencies (nanoserde, crossbeam, midir, gilrs, egui, phosphor)
- Well-tested: model.rs, learn.rs, keys.rs all have comprehensive unit tests
- Thread-safe by design: MIDI and gamepad events cross thread boundary only as `ControlEvent` on a channel; no lock-based sharing
- Idempotent migrations: key canonicalization happens at load time; old spellings in hand-edited files are auto-converted
