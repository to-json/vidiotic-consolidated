# vidiotic — Grammar / Modal Input Flow

## Surface

The grammar system is a **verb-object modal command interface** driven by 8 keyboard/pad/MIDI tokens. It lives across:

- **keymap.rs** — the prefix-keymap state machine, verb enums, per-pane keymaps (POOL_TABLE, BANK_TABLE, CUE_TABLE, CLOCK_TABLE). "Grammar" is the mode's name in the UI and on the wire; the code says keymap, submap, binding, repeat map — the Emacs vocabulary for what it actually is.
- **commands.rs** — `GrammarModalView` struct for the which-key overlay display
- **control_input.rs** — key canonicalization and event routing
- **ui/whichkey.rs** — which-key overlay renderer (floating panel above statusline)
- **app.rs** (lines ~2140–2292) — `grammar_modal_view()`, `grammar_step()`, `apply_verb()`, `handle_key()` integration

## States & Modes

The machine has three states (`keymap::State`):

### Idle
- **Entry:** App start, verb completion, cancel-while-pending
- **Exit:** User presses any token key
- **Display:** No modal overlay; statusline shows grammar pane name (e.g., "BANK") if grammar_on=true
- **Input handling:** A token with a non-empty submap enters `AwaitingBinding`; one with an empty submap returns `Step::Empty` and stays Idle; Cancel falls through to app's Escape handler

### AwaitingBinding { prefix: Token }
- **Entry:** User presses a token key (0–7) with at least one binding in this pane while Idle
- **Exit:** User completes a binding (second token) → fires verb & transitions to Idle or Repeat; user presses an unbound token → swallowed (submap stays open); user presses Escape → cancel
- **Display:** Which-key overlay shows the prefix label and up to 8 binding slots for tokens 0–7; trail shows "pane·prefix_key" (e.g., "bank·g" for Go in bank pane)
- **Input handling:** Any token (0–7) → check the submap for that prefix; Escape → cancel to Idle

### Repeat { label, entries, trail_prefix }
- **Entry:** A binding whose verb optionally emits then declares a `repeat` mode (repeat map)
- **Exit:** Escape only — every token the mode does not own is swallowed
- **Display:** Which-key overlay shows the repeat mode's label and entries (only populated slots); trail shows "pane·prefix_key·mode_label" (e.g., "bank·g·move"); unpopulated slots are hidden
- **Input handling:** Any token in the repeat map fires its verb and stays in Repeat; any other token is swallowed; Escape → cancel to Idle

---

## Flow (step by step)

### Initiation

1. User toggles grammar mode on via Command::SetGrammarMode(true) (default off at startup; typically via keybind or UI toggle).
2. App sets `self.grammar_on = true` and displays pane mode word in statusline (e.g., "BANK").
3. Grammar machine is in Idle; no modal visible.

### Entering a sequence

4. User presses one of the 8 token keys (g, f, m, a, d, t, b, ;) **without modifiers** (no Ctrl/Alt/Super).
5. app.rs::handle_key() canonicalizes the key, calls keymap::token_of_key() → Some(Input::Token(n)).
6. grammar_step() calls keymap.step(pane_keymap(focused_pane), Input::Token(n), Spelling::Key).
7. Machine transitions to AwaitingBinding { prefix: n } — unless that submap is empty, in which case it stays Idle and returns Step::Empty.
8. grammar_modal_view() generates which-key display:
   - Looks up submap = pane_keymap(pane).submaps[n]
   - Title = PREFIX_LABELS[n] (e.g., "Go")
   - Trail = "pane·key" (e.g., "bank·g")
   - Options = filled binding slots, in token order (token_label, binding_label)
9. ui/whichkey.rs renders a floating panel: title, trail, options (up to 4 per row), and a cancel footer. Options and footer are spelled for the surface driving the sequence (`keymap::Spelling`): `g` / `esc cancel` on a keyboard, `North` / `Select cancel` on a pad, `38` / `35 cancel` over MIDI.

### Binding (completing a simple sequence)

10. Overlay is open, user presses a second token (0–7).
11. grammar_step() → keymap.step(keymap, Input::Token(t), …) on AwaitingBinding.
12. Machine checks keymap.submaps[prefix][t]:
    - **Empty slot (None):** Step returns Pending; the submap stays open (the press is swallowed).
    - **Filled slot with verb, no repeat:** the binding emits its verb, machine returns to Idle.
    - **Filled slot with or without verb + repeat mode:** the binding optionally emits its verb, machine enters Repeat with the mode's label and repeat map.
13. If verb emitted → app.rs::grammar_step() calls apply_verb() to resolve context and send Command(s).
14. If returned to Idle → modal vanishes.

### Repeat mode (repeat-friendly terminal)

15. After a binding that declared a repeat mode, machine is in Repeat { label, entries, trail_prefix }.
16. grammar_modal_view() renders:
    - Title = mode label (e.g., "move")
    - Trail = "pane·prefix_key·mode_label" (e.g., "bank·g·move")
    - Options = only populated entries in the repeat map, with the labels the entries carry ("up", "down", "step +", "step -", "tap")
17. Each further press of a token in the repeat map fires that verb and **stays in Repeat** — no overlay flicker.
18. Press a token **not** in the repeat map → swallowed; the mode survives. Escape is the way out.
19. Repeat modes enable fluent multi-step actions: e.g., gg (enter move mode) then ggg (up, up, up) without re-entering.

### Cancel / Escape

20. **Cancel while AwaitingBinding or Repeat:** Escape key → token_of_key("Escape") → Some(Input::Cancel).
    - keymap.step() returns Step::Cancelled; grammar_step() returns true (consumed).
    - Machine resets to Idle; verb not emitted; modal vanishes.
21. **Cancel while Idle:** Escape → keymap.step() returns Step::Rejected; grammar_step() returns false (not consumed).
    - Falls through to app's built-in Escape handling (e.g., clear BPM entry, toggle fullscreen, etc.).

### Context & pane switching

22. Pane token (T7=b) opens the Pane submap, identical in every keymap. Token T1–T4 focus Pool/Bank/Cue/Clock; T6 (double-b) bounces to the previously focused pane.
23. FocusPane verbs call app.rs::focus_pane() → update focused_pane, pane_keymap() returns a different keymap for future sequences.
24. Meta token (T8=;) opens the Meta submap, also identical in every keymap; reaches project-level verbs (save, open, fullscreen, etc.) from any pane.

---

## Example key sequences

All examples assume grammar_on=true, BANK pane focused, no modifiers.

### Basic bindings (no repeat mode)

| Sequence | Verbs emitted | Action | Trail shown |
|----------|---|---|---|
| **g** then **g** (Go→up) | SelectCueDelta(-1) | **Enters the move repeat mode** (repeatable) | bank·g then bank·g·move |
| **g** then **m** (Go→first) | SelectCueFirst | Move to first cue, return to Idle | bank·g |
| **f** then **f** (Fire→send) | SendEditBankLive | Send edit bank live (double-Fire hot verb) | bank·f |
| **a** then **a** (Make→bank) | AddBank | Create new bank, return to Idle | bank·a |
| **d** then **d** (Cut→cue) | RemoveSelectedCue | Remove selected cue, return to Idle | bank·d |

### Repeat modes (repeat-friendly)

| Sequence | Repeat mode | Verbs emitted | Action |
|----------|---|---|---|
| **g** then **g** then **g** then **f** | move | SelectCueDelta(-1), SelectCueDelta(-1), SelectCueDelta(1) | Move up twice, then down once; stay in move mode after each press |
| **t** then **g** (in CUE pane) | dwell | NudgeParam(Dwell, 1) | Enter knob-tuning mode on Dwell; can now press g (up), f (down) repeatedly to step |
| **f** then **f** then **f** then **f** (in CLOCK pane) | tap | TapTempo, TapTempo, TapTempo | Enter tap-tempo mode; every further f is a tap |
| **t** then **g** then **f** then **g** (in CLOCK pane) | bpm | BpmDelta(1.0), BpmDelta(-1.0), BpmDelta(1.0) | Enter BPM mode at +1; then down, then up again |

### Exiting repeat modes

| Sequence | Action |
|----------|---|
| In move mode, press **Escape** | Leave move mode, back to Idle |
| In move mode, press **b** (Pane) | Swallowed — still in move mode |
| In tap mode (CLOCK), press **m** (Mark) | Swallowed — still in tap mode |

### Pane and Meta navigation

| Sequence | Action | Trail |
|----------|---|---|
| **b** then **g** | Focus Pool pane | bank·b then pool·g (next sequence uses pool table) |
| **b** then **f** | Focus Bank pane | bank·b |
| **b** then **m** | Focus Cue pane | bank·b |
| **b** then **a** | Focus Clock pane | bank·b |
| **b** then **b** | Bounce to previous pane | bank·b (verb FocusPrevPane) |
| **;** then **g** | Save project | bank·; |
| **;** then **f** | Toggle fullscreen | bank·; |
| **;** then **a** | Open project editor | bank·; |
| **;** then **d** | Open project | bank·; |
| **;** then **;** | Grammar off | bank·; |

### Context-dependent verbs

All verbs are context-free (just "remove selected cue"); app resolves which cue, bank, etc.

| Pane | Sequence | Resolved action |
|------|----------|---|
| **Bank** | d then d | Remove selected cue from edit bank |
| **Cue** | d then d | Remove selected cue from edit bank |
| **Pool** | a then a | Add cue for selected clip to edit bank |
| **Clock** | t then g | BpmDelta(+1.0) |
| **Clock** | d then d | SoftReset (reset beat grid to bar 1, beat 1) |
| **Clock** | d then ; | HardReset (reset grid + jump playlist to first cue) |

---

## Edge cases & escapes

### Empty binding slots are swallowed
- Pressing a token for an empty slot keeps the submap open and the state as-is (Step::Pending).
- The same rule holds inside a repeat mode (see "Non-entry token in repeat modes"): one stray-token rule for both pending states.
- Design rationale: a stray press never changes what the *next* press means. On a second screen, "did nothing" is a safer failure than "silently rerouted".

### Option-less prefixes open nothing
- Pressing a prefix with no filled slots in this pane returns Step::Empty(label) and leaves the machine Idle — no overlay opens.
- Example: in POOL, Fire/Mark/Cut/Tune have no bindings; press **f** → statusline says `Fire: nothing here` for about a second, and the next press still means what it always meant.
- Design rationale: under the swallow rule an option-less submap would be a trap — it would eat every press, including **b** (Pane) and **;** (Meta), until Escape. The fix is refusing to enter it, not making it escapable.

### Shift modifier breaks grammar entry
- **b** with Shift held → not consumed by grammar, falls through to mapped bindings or hardcoded default.
- Chords (Ctrl+key, Alt+key, Cmd+key) also bypass grammar.
- Rationale: grammar is chord-free to keep sequences fluent; chords are reserved for other bindings.

### Escape while idle falls through
- Idle Escape → Step::Rejected → grammar_step returns false.
- Falls through to app's hardcoded Escape handler (clear BPM entry, etc.).
- Escape only "owns" the grammar when a sequence is pending.

### Non-entry token in repeat modes
- In move mode, press **a** (Make) → swallowed (Step::Pending). The mode survives; the trail stays "bank·g·move".
- Escape is the way out of a repeat mode, and it costs one press (Select on a pad, note 35 on MIDI).
- Rationale: the alternative — exiting and replaying the token as a fresh prefix — let one stray key silently change what the next key means, live.

### Repeat mode has no grammar-off binding
- Once in move/tap/knob mode, only tokens in that repeat map have verbs; everything else is swallowed.
- To turn the grammar off from inside one: Escape back to Idle, then **;** **;** (Meta→grammar off), or use the UI / a mapped binding.

### Double presses (hot verbs)
- **ff** (Fire twice in BANK) → first press opens the Fire submap, second press fires SendEditBankLive (binding slot 1, i.e., Fire's "self").
- **dd** (Cut twice in BANK or CUE) → RemoveSelectedCue (slot 4).
- **bb** (Pane twice) → FocusPrevPane (slot 6).
- **gg** (Go twice) → SelectCueDelta(-1) **and enters the move repeat mode** (so it fires the verb and enters the mode both).
- Design: doubled tokens fire the slot at their own index, doubling as a quick-access hot key.

### Knob tuning (Tune prefix in CUE)
- **t** then **g** (select Dwell knob) → enters the dwell repeat mode with up/down mapping.
- Trail shows "cue·t·dwell".
- Further g = step up, f = step down. Options shown as "step +" and "step -".
- The repeat mode holds open; each press fires NudgeParam with ±1 direction.
- Exit by pressing unrelated token (e.g., **m** for Mark) or Escape.

### Preserve cycle in CUE
- Verb CyclePreserve toggles the selected cue's preserve override: None → Some(true) → Some(false) → None.
- **m** then **m** (Mark→preserve) in CUE pane.
- Not a repeat mode; single press cycles and returns to Idle.

---

## Observations

### Design strengths
1. **Pure state machine:** the keymap has no app dependencies; it's testable in isolation (keymap.rs carries its own test module). Verbs are context-free.
2. **Pane-sensitive tables:** Each pane has a different verb table; tokens mean different things in different contexts (e.g., Go moves clips in Pool but cues in Bank).
3. **Forgiving modals:** Empty slots keep the modal open. A typo doesn't crash, reset, or reroute the next press; you just see fewer options.
4. **Repeat modes for fluency:** repeat-friendly actions (move, tap, knob tune) don't require re-entering the sequence each time.
5. **Transparent which-key:** Modal shows exactly what's available; no hidden verbs.

### Potential confusion or rough edges
1. **"Doubled prefix" hot verbs are buried in the submap:** F-T1 (Fire's own slot) is SendEditBankLive, not explained as "ff hot verb" upfront. Trail shows "bank·f·send" on second press, which might confuse whether you're in a repeat mode (you're not).
2. **Repeat modes have one exit signal, and it is Escape:** there is no dedicated "exit repeat" verb, and every unowned token is swallowed. Uniform, but it means the way out is never one of the eight.
3. **Trail format changes between states:** "bank·g" vs "bank·g·move" doesn't immediately signal state transition; user must read the modal title.
4. **Empty submaps open nothing:** e.g. Fire in the POOL pane has no bindings; pressing **f** leaves the machine idle and puts `Fire: nothing here` on the statusline for about a second. Nothing to get stuck in, but nothing on the overlay either — the note is the only feedback.
5. **Repeat-mode verb display is human-readable but lossy:** "step +" for both NudgeParam and BpmDelta hides the actual direction and magnitude; user must remember which knob/parameter they entered.
6. **Focus-pane verbs bypass confirm/undo:** FocusPane switches the grammar table immediately; no visual feedback until the next pane-specific modal opens. If you mispress pane tokens, the entire verb context has shifted.
7. **context resolution happens at apply_verb, not at sequence completion:** Verb is context-free ("remove selected cue"); the actual cue ID is resolved late in apply_verb. If selection changes during a long repeat mode, the final verb still fires on the *current* selection, not the one when the verb was emitted. (Probably intentional for liveness.)

### Dead ends / surprising behaviors
1. **Escape is the only way out of a repeat mode:** every token the mode doesn't own is swallowed, so leaving move/tap/knob mode costs one Escape (Select on a pad, note 35 on MIDI) rather than any stray key. The trade is deliberate — see "Non-entry token in repeat modes".
2. **A repeat map is binary—either a verb exists or it doesn't:** No "skip" or "noop" option; every entry is all-or-nothing. Can't make a mode that partially blocks tokens; you must either define verbs for all of them or none.
3. **The keymap can't express sequences longer than 2 tokens:** the machine is depth-limited: prefix, then binding (optionally entering a repeat map). No "prefix → binding → sub-binding" chains. The overlay always shows 8 slots max.
4. **Hardcoded token spelling is immutable in the grammar:** KEY_TOKENS = ["g", "f", "m", "a", "d", "t", "b", ";"] is const, so you can't rebind tokens without recompiling. (Mapped bindings can point multiple keys to the same token via vidiotic-ctl, but the grammar itself doesn't know about it—only the UI does.)
5. **Cancel-while-idle is a fallthrough, not a hard Escape handler:** If a keybind shadows Escape (e.g., Escape → Action::Nothing), the grammar never sees Cancel; the app's built-in Escape behavior is suppressed, and the user has no way to toggle grammar off via keyboard alone.

### Missing or implicit
- **No visual confirmation of verb emission:** Once a verb fires, there's no feedback on the statusline or modal—it just vanishes. User must watch the cue list / bank / tempo change to know it worked.
- **No undo for grammar verbs:** They're regular Commands; undo is engine-level if implemented, not grammar-specific.
- **No trace/log of the input trail:** The trail is ephemeral in GrammarModalView; once the modal closes, the sequence is gone. No command palette or history.
- **No per-pane keybindings for grammar:** The 8 tokens are global; you can't rebind g to mean something different in CUE vs Bank (you must edit the pane table).

