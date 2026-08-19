# vidiotic — On-Screen Panels Flow

## Surface

**Files**: `/vidiotic/src/ui/mod.rs`, `editor.rs`, `library.rs`, `transport.rs`, `status.rs`, `whichkey.rs`

**Overall screen layout** (egui panels, drawn in order: transport → status → editor → library → modal):
- **Top**: Transport panel (fixed height, spans full width)
- **Bottom**: Status panel (fixed height, spans full width)
- **Right**: Editor panel (resizable, defaults 272px wide, min 210px)
- **Center**: Library panel (fills remaining space)
- **Overlay**: Grammar modal (floats above statusline when a verb sequence is pending)

The library panel is a `CentralPanel`, so it expands to fill the gap left by the four fixed/side panels. All panels refresh each frame via `App::render_control()` which calls `egui.render()`.

---

## Panels

### Transport (Top)

**Purpose**: Global playback timing, tempo control, and sequencer settings.

**Layout** (responsive to window width; stacks at <800px):
- Tempo cluster: BPM hero (32pt monospace, drag to scrub or click to type), ± 0.1% nudge buttons
- Tap cluster: Downbeat (▼1), Soft reset (⟲), Hard reset (⏮), Tap tempo
- Bar/beat cluster: Time signature (numerator / denominator scrollers), bar count, beat glyphs (● current, ○ rest)
- Sync cluster: Internal / Link toggle, peers count (when on Link)
- Cadence rows (wrapped, two settings):
  - "Next every": how often to auto-advance to the next active clip (1/4 note to 16 bars)
  - "Loop every": how often to retrigger the current clip on a beat grid (off / 1/8 note to 16 bars)
- Toggles: "preserve playhead" (carry playhead on cut), "advanced" (per-cue timing), "grammar" (modal verb input)
- Phrase strip: ▰▱▰▱ progress bar to next cut (filled cells = elapsed beats)

**User actions**:
- Drag BPM hero to scrub tempo; click to type; enter to commit (shows "ENTRY" in statusline while typing)
- Tap tempo button: tap 2+ times to derive BPM from intervals
- Downbeat (Space key or button): snap phase to nearest bar (only when `can_set_phase`, e.g., not on Link listen-only)
- Soft reset (r key): clock back to bar 1, beat 1 (preserves playlist position and playhead)
- Hard reset (Shift+r): soft reset + jump playlist to first cue
- Nudge tempo ([ and ] keys): ±0.1% for beat-matching drift
- Cadence selector: click to open dropdown and pick from detent list
- Toggle buttons: click to enable/disable preserve, advanced, grammar modes

**Visible affordances**:
- Bracket buttons: `[−.1%]`, `[+.1%]`, `[▼1]`, etc. (phosphor widget)
- Beat glyphs pulse; downbeat tinted phosphor (green-ish), others accent (blue)
- Phrase strip is monospace, blue
- Hover text on all controls

### Library (Center)

**Purpose**: Source clip pool, camera feed list, and cue bank playlist editing.

**Layout**:
1. **Clip pool header**: "clips" label, "📁 folder…" button, current folder path (truncated, muted)
2. **Clip bank tabs** (if any banks exist): bracket-text tabs `[bank (N)]` with count; `[+]` to add another bank
3. **Instruction text**: "double-click a clip to add it as a cue to the edit bank"
4. **Scroll area (max 190px height)**: clip tiles in horizontal wrap
   - Each tile: 128×86px thumbnail, name label, `●●◯` role marker (playing/armed/none), beat-synced pulse border (white decay on beat)
   - Single-click: select clip (highlights, shows `selected_clip`); double-click: add cue to edit bank
5. **Cameras section**: "cameras" label, "🔄 refresh" button
   - One row per device: on-air toggle `☑`, camera glyph `◉ name`, status tag (online/offline/missing), role tag (playing/armed)
   - Missing devices: `◉ name`, "missing device" tag (error red), relink combo
   - Double-click camera name: add a camera cue to edit bank
6. **Cue bank header**: "banks" label, bracket-text bank tabs `[name (M)]` with count, `[+]` to add bank
   - Live bank shows `●` dot before name; edit bank wears brackets
   - Hover over non-live tab: reveal `▶` play button (hover color accent) to send it live
7. **Scroll area (fills rest)**: cue chips in horizontal wrap, one cue per chip
   - Each chip: 146×122px, thumbnail, name, role tag, metadata row (trim, keep/cut, fx badges, advanced: dwell/loop/offset/speed)
   - Single-click: select cue (highlights, populates right editor); hover: shows remove button (× in top-right corner); click ×: remove cue
   - Advanced mode: chips are drag-to-reorder handles; `◀ ▶` move buttons on hover

**User actions**:
- Click "folder…": open native file picker for clip directory (async, spawns thread)
- Click clip bank tab: switch active clip bank (pool shows that folder's clips)
- Click "+" on clip bank tab: pick a folder to add as another bank
- Click clip tile: select it (single-click) or add cue (double-click)
- Click camera refresh: re-enumerate capture devices
- Toggle camera on-air: keep camera capturing regardless of cue rotation (privacy light on)
- Double-click camera: add a camera cue to edit bank
- Click camera "relink…": reassign a saved-but-missing device to a connected one
- Click bank tab: switch edit bank (cues below change to show that bank)
- Click ▶ on bank tab (hover-only): set that bank live (plays at next phrase boundary)
- Click cue chip: select it (populates right editor)
- Hover cue chip: reveal × and (in advanced mode) ◀ ▶ buttons
- Click ×: remove cue from edit bank
- Click ◀ ▶ (advanced): move cue earlier/later in the bank
- Drag cue chip (advanced): drag-and-drop to reorder; drag overlay darkens the source

**Visible affordances**:
- Square bordered tiles: media tiles with thumbnails, numbered role glyphs, beat pulse (white outline)
- Bracket tabs: `[name]` or ` name ` (selected vs not), live dot `●` before name
- Tags: "online", "missing", "playing" (green), "armed" (yellow), "keep" (green), "cut" (muted)
- Chips: compact parameter badges (⌛ dwell, ↻ loop, offset, speed multiplier) only in advanced
- Scroll area wheel input works on narrow windows (no `push_id` on drag overlay; see comment in code)

### Editor (Right)

**Purpose**: Edit the selected cue's parameters: timeline trim, preservation, effect chain, and per-cue advanced knobs.

**Default state**: Empty-state prompt "No cue selected" + instruction "Double-click a clip to add a cue, then click it here to edit"

**When a cue is selected**, layout:
1. **Header**: cue name (primary color), role chip (playing/armed/idle), cue ID chip
2. **Playhead readout**: ⏱ time in m:ss.cc format (secondary color)
3. **Trim section** (only for non-camera cues):
   - Grid: "in" / value + ⏺ button (set to playhead), "out" toggle + value / "clip end"
   - DragValue for seconds (fixed 2 decimals); suffix " s"
   - Buttons always appear; greyed when out is off
4. **Preserve playhead section** (greyed for camera cues):
   - Segmented buttons: "inherit" / "on" / "off" (one selected)
   - Hover text explains inherit follows global toggle
5. **Effect chain section**:
   - Header with hover text (explains prev() and live shader placement)
   - Empty state: "No effects — runs the live shader."
   - One row per slot:
     - Slot index (1., 2., etc.)
     - Shader name (truncated left if panel narrow)
     - Reorder buttons: ▲ ▼ (up/down), only enabled if not at ends
     - Delete button (error red ×)
   - For ISF slots: inline parameters below (floats = faders, bools = checkboxes, longs = bracket lists, colors = picker, points = drag values, events = buttons)
   - Append combo: "Add effect" opens dropdown with Live, pool shaders (builtin/pinned shown directly, ISF shown with "ISF:" prefix), and "Load ISF file…" picker
6. **Advanced sections** (only when `m.advanced` is true):
   - Dwell: "inherit" toggle + beats field (overrides global phrase length)
   - Loop rate: combo for inherit / off / grid cadence (1/8 note to 16 bars)
   - Offsets section:
     - Swing: toggle + tick offset (±256, granularity 1 tick = 1/32 beat)
     - Nudge: toggle + seconds offset (for camera-less cues only); greyed for cameras
     - Delay: toggle + beats offset (how long to hold previous cue)
   - Speed section (greyed for camera cues):
     - Clip BPM: toggle + BPM field (shared by all cues on the clip)
     - Cue BPM: toggle + BPM field (per-cue override)
     - Sync to tempo: toggle (only enabled if source BPM is known)
     - Multiplier: toggle + field (stacked on sync factor)
     - Effective speed readout: "→ 1.50× effective"
7. **Remove button** (error red bracket button): removes cue from bank
8. **Footer note**: "Trim, timing & speed apply the next time this cue is triggered."

**User actions**:
- Drag DragValue in/out: enter trim point; speed 0.05 s per pixel
- Click ⏺ button (in/out): snap in/out point to current playhead
- Toggle "out": enable/disable trim (off = play to clip end); enabling presets out to in+1s
- Click "preserve playhead" segmented buttons: choose inherit/on/off
- Reorder effect chain: click ▲ ▼ (adjacent only)
- Delete effect: click ×
- Adjust ISF parameters: faders drag/click, checkboxes toggle, bracket lists click to open, color picker click swatch
- Add effect: click combo, pick shader or ISF file
- Toggle/adjust advanced knobs: enable with checkbox, drag value (except segmented cadences)
- Remove cue: click bracket button at bottom

**Visible affordances**:
- Grid layout for trim (right-aligned labels, dual-field rows)
- DragValue with monospace, fixed decimals
- Glyph checkboxes (phosphor): `☑` on, `☐` off
- Bracket buttons for single actions
- Segmented buttons (3 options) for enum choices
- Combos with detent scroll for cadences
- Color picker (integrated egui)
- Hover text (on_hover_text) on every control
- Chip tags (role, cue ID) above the scroll area

### Status (Bottom)

**Purpose**: Live shader control, pinned shader pool, audio device selection, spectrum/level meters, compile errors, and global theme/mode readout.

**Layout** (two lines when wide, wrapped when narrow):
1. **Shader control row**:
   - "📁 shader…" button: pick a GLSL/WGSL file to livecode
   - Shader name (monospace, error red if compile failed, primary otherwise)
   - "📌 pin" button: capture last-good compile into pool
   - (if pool not empty) Pinned shaders collapsed button: `[N pinned]` opens popup listing each pinned shader with delete option (builtin ones can't be deleted)
   - Spacer
   - "💾 save" button: save project (asks where if fresh)
   - "💾 save as…" button: save to new .viproj file
   - "✎ edit…" button: save and open in vidiotic-prep
2. **Meters row** (right-justified if >480px wide, else wrapped):
   - Audio device combo: dropdown showing "default" + enumerated devices
   - Spectrum toggle: "21·log" / "512·lin" chip (blue); click to toggle between perceptual bands (fftBand) and linear bins (iChannel0)
   - Spectrum bar: glyph columns (▅▅▃▁ etc.) showing live frequency content, height = magnitude
   - Level meter: glyph glyphs (`▁▂▃▄▅▆▇█`) showing overall level (log-compressed)
   - Audio error tag (if capturing failed): "audio!" chip (error red), hover for message
3. **Statusline** (full width, select-filled strip):
   - Left: mode indicator (NORMAL | CMD | ENTRY | ERROR | focused pane name)
   - Center: session summary (shader name · clip count · cue count · bpm)
   - Right: theme toggle area (not detailed in code, but controlled here)
   - When in grammar: trail appended to summary (e.g., "g · f · a" showing pending sequence)
   - If ERROR mode: clicking the mode indicator opens shader error window
4. **Error window** (floating, resizable):
   - Title: "Shader error"
   - Full compile error text (monospace, scroll if tall)
   - Persists even after error clears; user can close or leave open
   - Keyed by temp ID so it survives error clearing

**User actions**:
- Click "shader…": open file picker for shader file (async)
- Click "pin": capture current shader to pool
- Click "N pinned": open popup, click × on a shader to unpin (builtin shaders can't be unpinned)
- Click device combo: select audio input device (or "default")
- Click spectrum toggle chip: switch between 21 perceptual bands and 512 linear bins
- Click mode indicator (when ERROR): open shader error window
- In error window: scroll to read full text, close button to hide

**Visible affordances**:
- Bracket buttons: shader picker, pin, save, edit
- Monospace shader name (color-coded)
- Chips: spectrum mode (blue), audio error (red)
- Statusline: left/center/right justified, mode colored (accent for CMD/ENTRY, error red for ERROR, dim accent for grammar pane name)
- Spectrum bars: glyph glyphs showing columns
- Level meter: glyph glyphs showing single bar
- Hover text on spectrum toggle and device combo

### Whichkey Modal (Overlay)

**Purpose**: Display pending grammar verb sequence and available next options.

**Layout**: Floating panel above statusline (centered, translucent bg, 1px border)
- Title: pane name (e.g., "LIBRARY", "EDITOR")
- Trail: pending sequence (e.g., "g · f" showing root key and first choice)
- Options grid: 4 columns of `key label` pairs (e.g., "a add", "d delete", "m move", "e edit")
- Footer: "esc cancel" (muted)

**Appears when**:
- `grammar_modal` is Some (set when a verb sequence is pending but not completed)
- User has pressed a grammar root key (g/f/m/a/d/t/b/;) and hasn't yet picked a conjugation

**Disappears when**:
- Sequence is completed (verb applied, grammar.reset())
- User presses Esc (grammar.reset())
- Grammar mode is turned off

**User actions**:
- Press a token key (shown in options) to pick a conjugation
- Press Esc to cancel the sequence
- (No mouse interaction; display-only overlay)

---

## Flow (Step by Step)

### Representative Session: Browse → Select → Edit → Play

1. **Start**: App launches
   - Transport shows BPM (default 120), beat glyphs (●○○○), cadence dropdowns (inherit)
   - Library center shows "No clips loaded", with "Pick a folder to fill the pool" prompt and "📁 folder…" button
   - Editor right shows "No cue selected" empty state
   - Status bottom shows shader name "<none>", clip count 0, cue count 0

2. **Browse clips**: User clicks "📁 folder…" button in library header
   - Native file picker opens (async, threaded)
   - User selects a directory of video clips
   - Clips are decoded asynchronously; thumbnails are cached
   - `Command::SetClipDir` sent to engine

3. **Pool fills**: Engine loads clips, sends thumbnails
   - Library pool scroll area now shows clip tiles (128×86 thumbnails with names)
   - Each tile single-clickable (no role marker yet, gray)
   - Instruction text remains: "double-click a clip to add it as a cue to the edit bank"

4. **Select a clip** (optional): User single-clicks a clip tile
   - Tile highlights (selected = true, renders bright border)
   - In main output window, that clip's thumbnail shows (pool pane's Make target)
   - `Command::SelectClip(Some(id))` sent

5. **Add a cue**: User double-clicks the same clip tile
   - Cue is added to the edit bank
   - Library cue bank scroll area now shows one cue chip (thumbnail + name)
   - Right editor panel now shows the cue's fields (in/out trim, preserve, chain, etc.)
   - `Command::AddCue(clip_id)` sent
   - **Cue is automatically selected** (shows in editor immediately)

6. **Edit trim** (in editor panel):
   - User drags in-point DragValue to 2.5s
   - User drags out-point DragValue to 5.0s
   - Or: user presses Space to set downbeat, scrubs main playhead in output window, clicks ⏺ button next to in/out to snap points
   - Footer notes: "Trim, timing & speed apply the next time this cue is triggered."
   - `Command::SetCueIn(cue_id, 2.5)` and `Command::SetCueOut(cue_id, Some(5.0))` sent

7. **Add effect** (in editor):
   - User clicks "Add effect" combo at bottom of chain section
   - Dropdown shows "Live shader", pool entries, "Load ISF file…"
   - User picks a pool shader
   - New chain row appears above the combo, showing slot 1, shader name, up/down/delete buttons
   - `Command::SetCueChain(cue_id, [slot])` sent

8. **Adjust ISF parameters** (if ISF shader has inputs):
   - Editor shows inline faders/checkboxes/lists below the shader name row
   - User drags a fader: `Command::SetChainParam { cue, slot, name, value }` sent

9. **Enable advanced mode** (in transport):
   - User clicks "advanced" checkbox in cadence row
   - Transport shows "advanced" is now checked
   - Editor panel expands: new sections appear (dwell, loop rate, offsets, speed)
   - Cue chips in library now show compact badges (⌛1b for dwell, ↻4b for loop, etc.)
   - `Command::SetAdvancedMode(true)` sent

10. **Set per-cue timing** (in advanced editor):
    - User sets dwell to 2 beats (cue plays for 2 beats before next clip cuts in)
    - User sets loop rate to 1/4 note (cue restarts on quarter-note grid while playing)
    - Editor sends `Command::SetCueParam(cue_id, CueParam::Dwell(Some(64)))` (64 ticks = 2 beats at 32 ticks/beat)
    - Cue chip updates: now shows `⌛2b` and `↻1/4` badges

11. **Play back**: Transport ready
    - User clicks "▶" on a bank tab, or the bank is already live (showing `●` dot)
    - User adjusts BPM via hero drag or nudge buttons
    - Playhead advances in main output (not shown in control window, happens in graphics window)
    - When the cue's time comes, it plays (output window shows thumbnail, role chip switches to "playing")
    - If loop rate is set, cue restarts on grid boundaries
    - After dwell beats, sequencer advances to next cue

12. **Tempo sync**: User taps tap-tempo button 3+ times
    - Transport shows flash on button (opacity decay)
    - BPM updates based on inter-tap intervals
    - Statusline shows current BPM

13. **Reset**: User clicks hard reset (⏮)
    - Playhead jumps back to start of first cue in live bank
    - Clock resets to bar 1, beat 1 (phrase restarts)
    - Live bank advances to first cue (if not already there)

14. **Compile error**: User opens shader file with syntax error
    - Status shader name turns red (error color)
    - Status mode turns "ERROR" (error red)
    - Error window is not automatically open; user must click the mode indicator to see full text
    - Once user fixes shader and it recompiles successfully, mode returns to "NORMAL"

15. **Save project**: User clicks "💾 save" button in status panel
    - If project already has a path, writes back to that file
    - If fresh session (no path), native save picker opens
    - `Command::SaveProject` or `Command::SaveProjectTo(path)` sent
    - Statusline updates with new project name if visible

---

## Cross-Panel Interactions

### Selection and Role Marking

- **Clip selection** (single-click in pool): Sets `selected_clip`, highlights tile (bright border), shows clip in main output window (Make target)
  - File: `library.rs:138` (resp.clicked)
  - Command: `Command::SelectClip(Some(clip.id))`
- **Cue selection** (single-click in cue bank): Sets `selected_cue`, populates right editor panel, highlights cue chip
  - File: `library.rs:392` (cue_chip click)
  - Command: `Command::SelectCue(Some(cue.id))`
- **Role marking**: Clip and cue tiles show role chip (playing/armed/idle) and beat-synced pulse border when playing
  - File: `library.rs:28` (beat_pulse derived from `m.phase`)
  - Color: playing = green, armed = yellow

### Bank and Playlist Switching

- **Edit bank tab**: Click to change which bank's cues are shown in the pool below
  - File: `library.rs:333` (bank_tab click)
  - Command: `Command::SetEditBank(i)`
- **Live bank tab**: Click ▶ (hover-only) to set bank live (plays at next phrase boundary)
  - File: `library.rs:328` (play_resp click)
  - Command: `Command::SetLiveBank(i)`
- **Clip bank tab**: Click to switch which source folder (clip bank) the pool shows
  - File: `library.rs:286` (clip_bank_tab click)
  - Command: `Command::SetActiveClipBank(i)`

### Advanced Mode Toggle and UI Expansion

- **Transport checkbox** "advanced": Toggled in transport, affects all panels
  - File: `transport.rs:415` (checkbox change)
  - Command: `Command::SetAdvancedMode(advanced)`
  - Effects:
    - Editor right panel: expands with dwell, loop rate, offsets, speed sections (editor.rs:199-200)
    - Library cue chips: show advanced badges (library.rs:445)
    - Library cue chips: become drag-to-reorder handles (library.rs:370-389)

### Grammar Mode and Command Menus

- **Transport checkbox** "grammar": Toggled, enables modal verb sequence input
  - File: `transport.rs:428` (checkbox change)
  - Command: `Command::SetGrammarMode(grammar)`
  - Effects:
    - Key presses (g/f/m/a/d/t/b/;) now start verb sequences instead of direct actions
    - Whichkey overlay appears showing pending sequence and options (whichkey.rs:16)
    - Statusline mode shows "CMD" (accent color) when sequence pending, or focused pane name when idle
    - Verb application sets `focused_pane` (app.rs:2219) and routes through `grammar_step()` (app.rs:2199)
    - Esc key cancels pending sequence (app.rs grammar.reset())

### Preserve Playhead and Cut Behavior

- **Transport toggle** "preserve playhead": Global setting affecting all cues
  - File: `transport.rs:403` (checkbox change)
  - Command: `Command::SetPreservePlayhead(preserve)`
- **Editor segmented** "preserve playhead" (per-cue override): Per-cue setting (inherit/on/off)
  - File: `editor.rs:183` (segmented click)
  - Command: `Command::SetCuePreserve(cue.id, val)`
- **Semantics**: On a cue cut, if preserve is true, playhead carries over to the new cue already running; if false, new cue starts from in-point

### Spectrum Display and Audio Device

- **Status combo** "audio device": Selects input device
  - File: `status.rs:150` (combo change)
  - Command: `Command::SetAudioDevice(device_name)`
- **Status spectrum toggle**: "21·log" ↔ "512·lin" (state stored in egui temp data)
  - File: `status.rs:190-191` (toggle state)
  - Display: 21 perceptual bands (fftBand) vs 512 linear FFT bins (iChannel0)
  - No command sent; display-only toggle

### Shader Error Indicator

- **Status mode** "ERROR": Shown when shader compile fails
  - File: `status.rs:257-258` (has_error check)
  - Color: error red
- **Status mode clickable**: Click ERROR to open error window
  - File: `status.rs:281-284` (mode_clicked && has_error)
- **Error window**: Floating, resizable, shows full compile error text
  - File: `status.rs:220-247` (error_window function)
  - Persists after error clears (temp storage of last error text)

### Cadence and Timing Propagation

- **Transport "next every"**: Global cadence for cue advancement
  - File: `transport.rs:376-378` (detent_scroll and SetPhraseCadence)
  - Applied to all cues in simple mode
- **Transport "loop every"**: Global cadence for cue retriggering (or None for off)
  - File: `transport.rs:393-398` (detent_scroll and SetLoopCadence)
  - Applied to all cues unless overridden in advanced mode
- **Editor "dwell"** (advanced mode only): Per-cue override for phrase length
  - File: `editor.rs:636` (SetCueParam CueParam::Dwell)
  - Overrides global cadence for this cue only
- **Editor "loop rate"** (advanced mode only): Per-cue override for loop grid
  - File: `editor.rs:680` (SetCueParam CueParam::Loop)
  - Inherit (None) = use global; off (Some(0)) = no loop; Some(ticks) = loop on this grid

### BPM Sync and Tempo

- **Transport hero drag/click**: Set global BPM (only when `can_set_tempo`, greyed if listening to Link)
  - File: `transport.rs:137-144` (bpm_cluster)
  - Command: `Command::SetBpm(bpm)`
- **Transport tap tempo**: Derive BPM from inter-tap intervals
  - File: `transport.rs:255-267` (tap_button "tempo")
  - Command: `Command::TapTempo`
- **Transport nudge buttons**: ±0.1% drift adjustment (for beat-matching)
  - File: `transport.rs:147-159` (nudge buttons)
  - Command: `Command::NudgeBpm(factor)`
- **Editor clip BPM**: Set or toggle source clip tempo (shared by all cues on that clip)
  - File: `editor.rs:744-756` (clip_bpm row)
  - Command: `Command::SetClipBpm(clip_id, bpm)`
- **Editor cue BPM**: Per-cue tempo override (for tempo-synced playback)
  - File: `editor.rs:759-774` (cue_bpm row)
  - Command: `Command::SetCueParam(cue_id, CueParam::Bpm(bpm))`
- **Editor sync to tempo**: Enable/disable BPM-sync retiming (only when source BPM known)
  - File: `editor.rs:777-785` (sync checkbox)
  - Command: `Command::SetCueParam(cue_id, CueParam::BpmSync(sync))`
- **Editor speed multiplier**: User multiplier stacked on sync factor
  - File: `editor.rs:788-809` (speed_mul row)
  - Command: `Command::SetCueParam(cue_id, CueParam::SpeedMul(Toggle { on, val }))`
- **Effective speed readout**: "→ 1.50× effective" (display-only, calculated from BPM-sync × multiplier)
  - File: `editor.rs:811-815`

---

## Observations

### Confusing or Inconsistent Patterns

1. **Camera vs. Clip Cue Semantics**
   - Camera cues have no timeline (no trim grid, in/out disabled), no speed controls
   - Preserve playhead is greyed for camera cues (meaningless without restart)
   - Dwell and loop-rate controls greyed for camera cues
   - The UI doesn't clearly label why; hover text says "live camera — no timeline to trim" (editor.rs:93-97)
   - User might not immediately understand why some controls vanish when switching cue types

2. **Preserve Playhead Complexity**
   - Global toggle in transport + per-cue override (inherit/on/off) in editor
   - Inherit semantics are indirect: "inherit follows the global toggle"
   - Three-state field (None=inherit, Some(true)=on, Some(false)=off) is not immediately obvious from UI
   - Unclear when a user should use global vs. per-cue override

3. **Bank Selection Orthogonality**
   - Clip banks (source folders) vs. cue banks (playlists) have separate tab bars
   - Live bank indicator (●) only shows on cue banks, not clip banks
   - No visual link between "show this clip bank in the pool" and "edit this cue bank"
   - New users might not realize pools and playlists are independent

4. **Advanced Mode Scope**
   - Advanced mode appears as a checkbox in transport (global setting)
   - But it only affects per-cue parameters (dwell, loop, offsets, speed)
   - The name "advanced" doesn't clearly indicate "per-cue timing" or "sequencer resolution"
   - Hover text helps, but it's a paragraph (editor.rs:417-420)

5. **Grammar and Pane Focus**
   - Grammar mode enables modal verb input (press root key, pick conjugation)
   - Pane focus (LIBRARY / EDITOR / TRANSPORT / etc.) affects which verb table is active
   - Mode word in statusline shows focused pane only when grammar is on
   - User must infer that pane focus affects available commands; no other visual cue
   - Verb tables are defined in `keymap.rs` (not shown in UI modules), so the mapping is invisible from the UI code

6. **Shader vs. ISF Distinction**
   - Effect chain shows "Live shader", builtin shaders, pinned shaders, and ISF slots
   - ISF entries are marked with "ISF: filename.fs" in the chain display
   - In the append combo, ISF shaders are labeled "ISF: stem" but builtin/pinned aren't
   - User must learn that ISF = Interactive Shader Format = has parameters
   - ISF parameters render inline below the shader name row, only if that slot is an ISF entry
   - No legend or help text explaining ISF vs. builtin

7. **Chip Wrapping and Responsive Layout**
   - Library clips and cues render in horizontal wraps (wrapped flows)
   - Transport clusters stack at <800px width, but no clear visual breakpoint indicator
   - Status meter row switches layout at 480px width (right-justified vs. wrapped)
   - On very narrow windows, controls can wrap awkwardly, e.g., "Add effect" combo dropping to next line
   - No explicit mobile/responsive design; appears ad-hoc per panel

8. **Live Playback Visibility**
   - Main playback happens in a separate graphics window (not shown in this panel analysis)
   - Control window shows clip/cue role markers (playing/armed) but no playhead position
   - User must look at the graphics output to see actual playback progress
   - Transport beat glyphs and phrase strip give rhythm context, but not real-time position

9. **Async File Pickers and Threading**
   - File pickers are spawned on background threads (mod.rs:236-298)
   - User gets no loading indicator while clips are being decoded
   - Clips appear in pool once thumbnails are ready; no progressive load feedback
   - If picker is slow (large folder), UI remains responsive but user doesn't know why nothing changed

10. **Effect Chain Reordering Complexity** (Advanced Mode)
    - Tiles are draggable in advanced mode, but require exact drop target (index-based MoveCue)
    - No visual drag preview beyond a darkened overlay
    - Drag-and-drop conflicts with ScrollArea wheel input (code has a note: "push_id breaks scroll"; see library.rs:440)
    - Up/down buttons are simpler and always available, but combo boxes and wraps can hide them

### Dead Ends or Missing Features

1. **No keyboard shortcut display** (except scattered on_hover_text hints)
   - Transport has several keys (Space, r, Shift+r, [, ], b, c)
   - Grammar has root keys (g/f/m/a/d/t/b/;) but they're only shown in the which-key overlay when active
   - New users won't know shortcuts exist without hovering every button

2. **No undo/redo**
   - All edits are immediate and sent as commands
   - No obvious undo button or redo history
   - User must manually reverse a change (e.g., re-edit a trim point)

3. **Pinned shader pool has no export/save mechanism**
   - Shaders are pinned at runtime but only saved if user explicitly saves the project
   - No "export pinned shader" or "snapshot pool" feature
   - Shaders captured in one session may be lost if project not saved

4. **Library has no search or filter**
   - Clip pool and cue list are flat, wrapped grids
   - No way to search for a clip by name or filter by role/bank
   - Large sessions could have dozens of clips requiring scroll hunting

5. **No cue renaming**
   - Cues inherit the clip name; no field to customize
   - Multiple cues on the same clip can't be easily distinguished by name alone
   - User must use ID chips and metadata badges for identification

6. **Transport cadence controls are dropdown selects, not knobs**
   - No visual representation of "next" vs. "loop" (other than labels)
   - No musical duration display (just label like "1/4" or "4")
   - Musically, the difference between 1/4 note cadence and 1 bar is significant, but visually identical

7. **No live shader parameter editing**
   - Live shader runs in the output window and accepts GLSL uniforms
   - No UI to tweak live shader parameters in real time
   - Only ISF slots (in the effect chain) expose parameters
   - User must pin a shader to an ISF slot if they want to adjust parameters, or use external GLSL parameter binding (not shown)

### Surprises and Elegances

1. **Beat-synced pulse on playing clip/cue**
   - Tiles show white outline pulse that brightens on the beat, decays toward next (library.rs:28)
   - Clear, immediate visual feedback of rhythm
   - Calculated from `m.phase.fract()` (fractional phase within beat), no extra state

2. **Phrase strip progress bar**
   - Transport shows `▰▱▰▱` strip (filled/empty per beat) for time-to-next-cut
   - Intuitive at a glance; no BPM conversion needed
   - Updates every frame

3. **Spectrum display toggle** (perceptual vs. linear)
   - Status panel lets user switch 21 log bands vs. 512 linear FFT bins
   - Useful for different shader development workflows (audio-reactive shaders often use fftBand)
   - Display-only toggle stored in egui temp data

4. **Inline ISF parameter editing**
   - ISF parameters render directly in the effect chain, no separate dialog
   - Faders, checkboxes, lists, colors, points all available
   - Feels native and immediate
   - Only shown for ISF slots, not builtin/pinned shaders (sensible filtering)

5. **Drag-and-drop reordering in advanced mode**
   - Cue tiles become draggable; overlay darkens on drag
   - Reordering also available via ◀ ▶ buttons (non-advanced-only friendliness)
   - Integrates cleanly without breaking ScrollArea wheel input (explicit interact + no push_id)

6. **Tap tempo**
   - Tap 2+ times to derive BPM from intervals
   - Flash decay on button shows responsiveness
   - No state indicator (e.g., "waiting for second tap"); user learns by doing

7. **On-air camera toggle**
   - Cameras can stay on regardless of cue rotation (privacy light on)
   - Separate from "playing" role (a camera can be on-air but not the selected cue)
   - Useful for multi-camera setups where one camera should always be recording

8. **Shader pinning**
   - "Pin" button captures last-good compile into pool
   - Allows livecoding a shader while cues reference the pinned version
   - Popup menu to manage pinned shaders; builtin ones can't be unpinned (read-only protection)
   - Clever workflow: code → pin → reference in cue → continue coding

---

## Summary

The vidiotic control UI is a **four-panel layout** (transport, library, editor, status) surrounding a modal grammar overlay. **Selection flows** through clip tile → cue chip → editor parameters, with **role marking** (playing/armed) propagating across panels via the `UiMirror`. **Advanced mode** expands editor knobs and enables drag-to-reorder; **grammar mode** routes key input through verb menus shown by the which-key overlay. **Tight integration** between tempo, cadence, and per-cue timing allows both simple playback (global settings) and complex sequencing (per-cue overrides). The UI is **responsive** (stacking on narrow windows) and **real-time** (no save/undo, all edits immediate), with **visual feedback** via beat-synced pulse, color-coded role chips, and live meters.
