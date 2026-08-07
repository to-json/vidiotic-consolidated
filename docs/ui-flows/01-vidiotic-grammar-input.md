# vidiotic — Grammar / Modal Input Flow

## Surface

The grammar system is a **verb-object modal command interface** driven by 8 keyboard/pad/MIDI tokens. It lives across:

- **grammar.rs** — state machine, verb enums, per-pane verb tables (POOL_TABLE, BANK_TABLE, CUE_TABLE, CLOCK_TABLE)
- **commands.rs** — `GrammarModalView` struct for the which-key overlay display
- **control_input.rs** — key canonicalization and event routing
- **ui/whichkey.rs** — which-key overlay renderer (floating panel above statusline)
- **app.rs** (lines ~2140–2292) — `grammar_modal_view()`, `grammar_step()`, `apply_verb()`, `handle_key()` integration

## States & Modes

The grammar machine has three states (grammar::GrammarState):

### Idle
- **Entry:** App start, verb completion, cancel-while-pending
- **Exit:** User presses any token key
- **Display:** No modal overlay; statusline shows grammar pane name (e.g., "BANK") if grammar_on=true
- **Input handling:** Token keys enter `AwaitingConjugation`; Cancel falls through to app's Escape handler

### AwaitingConjugation { root: Token }
- **Entry:** User presses a token key (0–7) while Idle
- **Exit:** User completes a conjugation (second token) → fires verb & transitions to Idle or Sticky; user presses an unmapped token → no-op (keeps modal open); user presses Escape → cancel
- **Display:** Which-key overlay shows root label and up to 8 conjugation slots for tokens 0–7; trail shows "pane·root_key" (e.g., "bank·g" for Go in bank pane)
- **Input handling:** Any token (0–7) → check conjugation table for that root; Escape → cancel to Idle

### Sticky { label, entries, trail_root }
- **Entry:** A conjugation whose verb optionally emits then declares a `sticky` mode (repeat table)
- **Exit:** User presses a token not in the sticky table → replays that token as fresh root (opens AwaitingConjugation); user presses Escape → cancel to Idle
- **Display:** Which-key overlay shows sticky mode label and entries (only populated slots); trail shows "pane·root_key·mode_label" (e.g., "bank·g·move"); unpopulated slots are hidden
- **Input handling:** Any token in the sticky table fires its verb, stays in Sticky; any token not in table exits Sticky and opens that root; Escape → cancel to Idle

---

## Flow (step by step)

### Initiation

1. User toggles grammar mode on via Command::SetGrammarMode(true) (default off at startup; typically via keybind or UI toggle).
2. App sets `self.grammar_on = true` and displays pane mode word in statusline (e.g., "BANK").
3. Grammar machine is in Idle; no modal visible.

### Entering a sequence

4. User presses one of the 8 token keys (g, f, m, a, d, t, b, ;) **without modifiers** (no Ctrl/Alt/Super).
5. app.rs::handle_key() canonicalizes the key, calls grammar::token_of_key() → Some(Input::Token(n)).
6. grammar_step() calls grammar.step(pane_table(focused_pane), Input::Token(n)).
7. Machine transitions to AwaitingConjugation { root: n }.
8. grammar_modal_view() generates which-key display:
   - Looks up entry = pane_table(pane).roots[n]
   - Title = entry.label (e.g., "Go")
   - Trail = "pane·key" (e.g., "bank·g")
   - Options = filled conjugation slots, in token order (key_label, conjugation_label)
9. ui/whichkey.rs renders a floating panel: title, trail, options (up to 4 per row), "esc cancel" footer.

### Conjugation (completing a simple sequence)

10. Modal is open, user presses a second token (0–7).
11. grammar_step() → grammar.step(table, Input::Token(t)) on AwaitingConjugation.
12. Machine checks table.roots[root].conjugations[t]:
    - **Empty slot (None):** Step returns Pending; modal stays open (forgiving design).
    - **Filled slot with verb, no sticky:** Conjugation emits verb, machine returns to Idle.
    - **Filled slot with or without verb + sticky mode:** Conjugation optionally emits verb, machine enters Sticky with the mode's label and entry table.
13. If verb emitted → app.rs::grammar_step() calls apply_verb() to resolve context and send Command(s).
14. If returned to Idle → modal vanishes.

### Sticky mode (repeat-friendly terminal)

15. After a conjugation that declared a sticky mode, machine is in Sticky { label, entries, trail_root }.
16. grammar_modal_view() renders:
    - Title = mode label (e.g., "move")
    - Trail = "pane·root_key·mode_label" (e.g., "bank·g·move")
    - Options = only populated entries in the sticky table (shown with "up", "down", "step +", "step -", "tap", etc.)
17. Each further press of a token in the sticky table fires that verb and **stays in Sticky** — no modal flicker.
18. Press a token **not** in the sticky table → machine exits Sticky, replays that token as a fresh root press (opens AwaitingConjugation for the new root).
19. Sticky modes enable fluent multi-step actions: e.g., gg (enter move mode) then ggg (up, up, up) without re-entering.

### Cancel / Escape

20. **Cancel while AwaitingConjugation or Sticky:** Escape key → token_of_key("Escape") → Some(Input::Cancel).
    - grammar.step() returns Step::Cancelled; grammar_step() returns true (consumed).
    - Machine resets to Idle; verb not emitted; modal vanishes.
21. **Cancel while Idle:** Escape → grammar.step() returns Step::Rejected; grammar_step() returns false (not consumed).
    - Falls through to app's built-in Escape handling (e.g., clear BPM entry, toggle fullscreen, etc.).

### Context & pane switching

22. Pane token (T7=b) opens the Pane root, identical in all tables. Token T1–T4 focus Pool/Bank/Cue/Clock; T6 (double-b) bounces to the previously focused pane.
23. FocusPane verbs call app.rs::focus_pane() → update focused_pane, pane_table() returns a different grammar table for future sequences.
24. Meta token (T8=;) opens the Meta root, also identical in all tables; reaches project-level verbs (save, open, fullscreen, etc.) from any pane.

---

## Example key sequences

All examples assume grammar_on=true, BANK pane focused, no modifiers.

### Basic conjugations (not sticky)

| Sequence | Verbs emitted | Action | Trail shown |
|----------|---|---|---|
| **g** then **g** (Go→up) | SelectCueDelta(-1) | **Enters move sticky mode** (repeatable) | bank·g then bank·g·move |
| **g** then **m** (Go→first) | SelectCueFirst | Move to first cue, return to Idle | bank·g |
| **f** then **f** (Fire→send) | SendEditBankLive | Send edit bank live (double-Fire hot verb) | bank·f |
| **a** then **a** (Make→bank) | AddBank | Create new bank, return to Idle | bank·a |
| **d** then **d** (Cut→cue) | RemoveSelectedCue | Remove selected cue, return to Idle | bank·d |

### Sticky modes (repeat-friendly)

| Sequence | Sticky mode | Verbs emitted | Action |
|----------|---|---|---|
| **g** then **g** then **g** then **f** | move | SelectCueDelta(-1), SelectCueDelta(-1), SelectCueDelta(1) | Move up twice, then down once; stay in move mode after each press |
| **t** then **g** (in CUE pane) | dwell | NudgeParam(Dwell, 1) | Enter knob-tuning mode on Dwell; can now press g (up), f (down) repeatedly to step |
| **f** then **f** then **f** then **f** (in CLOCK pane) | tap | TapTempo, TapTempo, TapTempo | Enter tap-tempo mode; every further f is a tap |
| **t** then **g** then **f** then **g** (in CLOCK pane) | bpm | BpmDelta(1.0), BpmDelta(-1.0), BpmDelta(1.0) | Enter BPM mode at +1; then down, then up again |

### Exiting sticky modes

| Sequence | Action |
|----------|---|
| In move mode (sticky), press **b** (Pane) | Exit move mode; open Pane root for pane selection |
| In move mode, press **;** (Meta) | Exit move mode; open Meta root for project verbs |
| In tap mode (CLOCK), press **m** (Mark) | Exit tap mode; open Mark root (only downbeat in CLOCK) |

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

### Empty conjugation slots are forgiving
- Pressing a token for an empty slot keeps the modal open and the state as-is (Step::Pending).
- Example: In POOL pane, Fire root has no conjugations; press **f** then any token → modal stays open.
- Design rationale: teaches the matrix by silence (empty panes guide the user).

### Shift modifier breaks grammar entry
- **b** with Shift held → not consumed by grammar, falls through to mapped bindings or hardcoded default.
- Chords (Ctrl+key, Alt+key, Cmd+key) also bypass grammar.
- Rationale: grammar is chord-free to keep sequences fluent; chords are reserved for other bindings.

### Escape while idle falls through
- Idle Escape → Step::Rejected → grammar_step returns false.
- Falls through to app's hardcoded Escape handler (clear BPM entry, etc.).
- Escape only "owns" the grammar when a sequence is pending.

### Non-entry token exiting sticky modes
- In move mode, press **a** (Make) → machine exits move, opens Make root (AwaitingConjugation).
- No verb fired; trail updates from "bank·g·move" to "bank·a".
- Rationale: sticky modes are transient; the exiting token is "replayed" as a fresh root.

### Sticky mode has no grammar-off conjugation
- Once in move/tap/knob mode, only tokens in that sticky table have verbs.
- To escape and turn grammar off, exit the sticky (press unrelated token like ;) then press **;** then **;** (Meta→grammar off).
- Or press Escape to cancel back to Idle, then toggle grammar off via UI or a mapped binding.

### Double presses (hot verbs)
- **ff** (Fire twice in BANK) → first press opens Fire root, second press fires SendEditBankLive (conjugation slot 1, i.e., Fire's "self").
- **dd** (Cut twice in BANK or CUE) → RemoveSelectedCue (slot 4).
- **bb** (Pane twice) → FocusPrevPane (slot 6).
- **gg** (Go twice) → SelectCueDelta(-1) **and enters move sticky mode** (so actually fires the verb and sticky both).
- Design: doubled tokens fire the slot at their own index, doubling as a quick-access hot key.

### Knob tuning (Tune root in CUE)
- **t** then **g** (select Dwell knob) → enters dwell sticky with up/down mapping.
- Trail shows "cue·t·dwell".
- Further g = step up, f = step down. Options shown as "step +" and "step -".
- Sticky mode holds open; each press fires NudgeParam with ±1 direction.
- Exit by pressing unrelated token (e.g., **m** for Mark) or Escape.

### Preserve cycle in CUE
- Verb CyclePreserve toggles the selected cue's preserve override: None → Some(true) → Some(false) → None.
- **m** then **m** (Mark→preserve) in CUE pane.
- Not a sticky mode; single press cycles and returns to Idle.

---

## Observations

### Design strengths
1. **Pure state machine:** Grammar has no app dependencies; it's testable in isolation (140+ test cases in grammar.rs). Verbs are context-free.
2. **Pane-sensitive tables:** Each pane has a different verb table; tokens mean different things in different contexts (e.g., Go moves clips in Pool but cues in Bank).
3. **Forgiving modals:** Empty slots keep the modal open and teach by silence. A typo doesn't crash or reset; you just see fewer options.
4. **Sticky modes for fluency:** Repeat-friendly actions (move, tap, knob tune) don't require re-entering the sequence each time.
5. **Transparent which-key:** Modal shows exactly what's available; no hidden verbs.

### Potential confusion or rough edges
1. **"Doubled root" hot verbs are buried in the conjugation table:** F-T1 (Fire's own slot) is SendEditBankLive, not explained as "ff hot verb" upfront. Trail shows "bank·f·send" on second press, which might confuse whether you're in a sticky mode (you're not).
2. **Sticky modes have no uniform exit signal:** Must press an unmapped token or Escape; no dedicated "exit sticky" command. If all 8 tokens have entries in a sticky table, you can't escape except via Escape.
3. **Trail format changes between states:** "bank·g" vs "bank·g·move" doesn't immediately signal state transition; user must read the modal title.
4. **Empty root entries show a modal with no options:** E.g., Fire in POOL pane has no conjugations; pressing **f** opens Fire root with no options listed (only "esc cancel"), which teaches by silence but might feel stuck.
5. **Sticky mode verb display is human-readable but lossy:** "step +" for both NudgeParam and BpmDelta hides the actual direction and magnitude; user must remember which knob/parameter they entered.
6. **Focus-pane verbs bypass confirm/undo:** FocusPane switches the grammar table immediately; no visual feedback until the next pane-specific modal opens. If you mispress pane tokens, the entire verb context has shifted.
7. **context resolution happens at apply_verb, not at sequence completion:** Verb is context-free ("remove selected cue"); the actual cue ID is resolved late in apply_verb. If selection changes during a long sticky mode, the final verb still fires on the *current* selection, not the one when the verb was emitted. (Probably intentional for liveness.)

### Dead ends / surprising behaviors
1. **Replay-on-exit replays unbound tokens as roots, not as conjugations:** If in move mode and you press an unmapped token, it opens that root. But if you press a token with no conjugations in that pane (e.g., Fire in Pool), the modal opens showing no options—you're "stuck" until you press Escape or another token.
2. **Sticky table is binary—either a verb exists or it doesn't:** No "skip" or "noop" option; every entry is all-or-nothing. Can't make a mode that partially blocks tokens; you must either define verbs for all of them or none.
3. **Grammar can't express sequences longer than 2 tokens:** The machine is depth-limited: root, then conjugation (optionally entering a sticky for repeats). No "root → conj → sub-conj" chains. Modal always shows 8 slots max.
4. **Hardcoded token spelling is immutable in the grammar:** KEY_TOKENS = ["g", "f", "m", "a", "d", "t", "b", ";"] is const, so you can't rebind tokens without recompiling. (Mapped bindings can point multiple keys to the same token via vidiotic-ctl, but the grammar itself doesn't know about it—only the UI does.)
5. **Cancel-while-idle is a fallthrough, not a hard Escape handler:** If a keybind shadows Escape (e.g., Escape → Action::Nothing), the grammar never sees Cancel; the app's built-in Escape behavior is suppressed, and the user has no way to toggle grammar off via keyboard alone.

### Missing or implicit
- **No visual confirmation of verb emission:** Once a verb fires, there's no feedback on the statusline or modal—it just vanishes. User must watch the cue list / bank / tempo change to know it worked.
- **No undo for grammar verbs:** They're regular Commands; undo is engine-level if implemented, not grammar-specific.
- **No trace/log of the input trail:** The trail is ephemeral in GrammarModalView; once the modal closes, the sequence is gone. No command palette or history.
- **No per-pane keybindings for grammar:** The 8 tokens are global; you can't rebind g to mean something different in CUE vs Bank (you must edit the pane table).

