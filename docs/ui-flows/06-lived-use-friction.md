# Lived-Use Friction

Where these flows *suck to use* — imagined from actually driving them: a live VJ
set in `vidiotic`, trimming a long video in `vidiotic-prep`, mapping a controller
in `vidiotic-ctl`. Not a feature audit (see each flow doc's "Observations" for
that). This is the embodied read: repeated effort, and negative-valence surprises.

The friction clusters into three systemic patterns, and each shows up in all
three apps. Undo (its absence) is the multiplier that turns every surprise into
manual rework.

## 1. Silence is the feedback channel — and it's hostile under pressure

The design repeatedly uses "nothing visibly happens" as a signal. Fine when
you're the author holding the model in your head; bad when you're live, or
returning after a break.

- **Grammar: fire a verb, the modal vanishes, the control window says nothing.**
  Confirmation lives in the *separate graphics output window*. You act into a
  void and must watch the other screen to know it landed. (doc 01, "No visual
  confirmation of verb emission")
- ~~**Empty conjugation slot = modal just stays open.**~~ *Fixed.* A slot with
  nothing in it still swallows the press, but a *root* with nothing under it no
  longer opens at all: it leaves the machine idle and the statusline says
  `Fire: nothing here` for about a second. The silence that read as "broken" was
  the option-less modal, and it is gone. (doc 01, "Option-less roots open
  nothing")
- **ctl learn captures the *concrete* device name, silently.** "Any device" needs
  `device: ""`, which the UI never exposes and never visually distinguishes. Map
  on a Launchkey, plug in a different controller next week, and nothing works —
  with a binding table that looks completely correct. Invisible cause, silent
  failure. (doc 03, "No visual indication of 'any device'")
- **prep: open video B and video A's spans vanish from the timeline.** They're
  still in memory (off-source spans aren't drawn), but the timeline lies. First
  reaction is "I lost my work." (doc 04, "Multi-source span ownership")

## 2. Every app has a "which of these two near-identical states is active?" trap

Guessing wrong is always silent and always costs work.

- **selected span vs. pending marks (prep).** Playback loops the *pending marks*,
  not the selected span. Click span 5, hit Space, hear span 20's leftover marks.
  (doc 04, "Marks loop playback, not spans")
- **focused pane vs. verb table (grammar).** `g` moves clips in Pool, cues in
  Bank. Pane focus only shows as the mode word, and one stray `b`+token silently
  swaps the entire command vocabulary with no confirm. (doc 01/02, "Grammar and
  Pane Focus")
- **concrete device vs. any-device (ctl)** and **global.vmap vs prep.vmap vs
  .viproj-embed (cross-app).** Rebind a key in prep expecting it everywhere; it
  only lands in `prep.vmap`. Three binding surfaces, subtly different scopes, no
  surface says which one you're writing to. (doc 03/04, control-map layering)

## 3. No undo, anywhere — so every surprise above becomes repeated effort

The multiplier. With undo, all the traps above are cheap Ctrl-Z moments. Here
each is a manual reversal:

- **prep retrim/update thrash:** "retrim" loads a span into marks, "update" writes
  it back. Click retrim on span 6 before hitting update on span 5 → span 5's
  adjustment is gone, silently, permanently. (doc 04, "Retrim / Update" labels)
- **live cut to the wrong bank:** the only fix is to cut again, on-screen, in
  front of the audience.
- **ctl action-kind switch resets params:** set min/max, switch action to
  compare, switch back — values gone. (doc 03, "Action params out of sync")

## Where effort repeats specifically

- **512 MB confirm dialog fires on *every* open** of a big file, including
  crash-recovery reopens. The threshold is invisible so you never learn why.
  (doc 04, `LARGE_FILE_BYTES`)
- **Backward scrub is laggy, forward is snappy** (only forward has a seek-free
  fast-path). Finding an exact in-point means oscillating across the slow
  direction, with no explanation for the asymmetry. (doc 04, preview seek cache)
- **20 spans auto-named "span 1…20", renamed one at a time.** No batch, no
  multi-select, no search to find one later. (doc 04, "No multi-select")
- **No help/discovery anywhere except the grammar which-key overlay.** prep's 14
  keys and vidiotic's transport keys are hover-to-learn, one button at a time.
  Every session-after-a-gap starts with re-discovering your own bindings.
- **ctl→vidiotic has no live round-trip.** Map → save `global.vmap` → switch to
  the running engine → test → switch back → adjust. The mapper is disconnected
  from the thing it maps.
- **ctl learn has no cancel/escape.** Click learn on the wrong row and you must
  actuate *something* to exit — which binds it — then delete it. (doc 03, "Learn
  timeout")

## The sharpest single negative surprise

~~**Grammar sticky-mode leakage.**~~ *Fixed.* `gg` still both fires a move and
drops you into move-sticky, but the next unrelated keystroke — say `a` — is now
swallowed instead of exiting the mode and opening a new root. A stray key can no
longer change what the next key means. The mode you did not realize you were in
is still a real cost, and it is still paid at the display: Escape leaves it.
(doc 01, "Non-entry token in sticky modes")

## Through-line

This is a toolchain that trusts the operator to *be its author* — to hold the
full model, track invisible state, and never need to walk back. Coherent and even
elegant for solo expert use. The friction is entirely in the gap between "I wrote
this and know it cold" and "I'm using it tired, live, or after two weeks off."

The three cheapest levers against all of the above: (1) make verb/action
emission *visibly* acknowledge (a statusline flash is enough), (2) give the two
near-identical states in each app distinct, always-on visual identity, and
(3) an undo stack — even a shallow one — to make every remaining surprise
survivable instead of permanent.
