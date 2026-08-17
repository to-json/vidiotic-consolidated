#!/usr/bin/env bash
# wasm-gate — which crates cross to wasm32-unknown-unknown, and why the rest don't.
#
# The web port (docs/web-port.md §8 step 3) is gated on cfg-ing the non-portable
# surfaces out of each crate until it builds for the browser target. This is the
# ratchet that tracks that work: every crate/feature combination is declared
# below with its expected state, and the gate fails if reality disagrees —
# in EITHER direction.
#
#   expected PASS, builds      -> ok
#   expected PASS, breaks      -> REGRESSION. Something portable stopped being portable.
#   expected FAIL, breaks      -> ok, and the recorded reason is the remaining work.
#   expected FAIL, builds      -> RATCHET. Move it to PASS in the table below.
#
# That last case is the point: a crate cannot quietly become portable without
# the table being updated to say so.
#
# Usage:  scripts/wasm-gate.sh [-v]
#   -v   show full cargo output for failures

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

# Without this, a script run from an unexpected path lands somewhere with no
# workspace, every `cargo check -p` fails for that reason alone, and the gate
# reports a tree-wide regression that is really a working-directory bug.
if ! grep -q '^\[workspace\]' Cargo.toml 2>/dev/null; then
  echo "not at the workspace root (cwd: $PWD) — refusing to report results" >&2
  exit 2
fi

VERBOSE=0
[ "${1:-}" = "-v" ] && VERBOSE=1

# crate | cargo feature args | PASS/FAIL | note
#
# Keep the note current: for FAIL rows it is the actual blocker, and it is the
# closest thing this repo has to a work list for the port.
#
# No apostrophes in the notes. Both tables are heredocs inside `$( )`, and
# bash 3.2 — which is what macOS ships as /bin/bash — mis-parses that
# combination: it applies quoting rules to the heredoc body while scanning for
# the closing paren, so one `'` swallows the rest of the file and the whole
# script dies with "unexpected EOF while looking for matching `''". Newer bash
# is fine, which is exactly why this is easy to reintroduce.
TABLE=$(cat <<'EOF'
vidiotic-wire|--no-default-features|PASS|protocol types are nanoserde over the vidiotic-core ISF model; core pulls ctl, so midir/gilrs build but do not block
phosphor|--no-default-features|PASS|egui theme + widgets, no shell feature
vidiotic-ctl|--no-default-features|PASS|binding tables; midir/gilrs/dirs do not block
vidiotic-core|--no-default-features|PASS|thumbnail decode is behind the ffmpeg feature
vidiotic-bake|--no-default-features|PASS|hap + frame are pure Rust; transcode is behind ffmpeg
vidiotic-play|--no-default-features|PASS|portable player: the render core, the engine — grammar, clock, sequencer, undo — and the control panels; no fs, no ffmpeg, no sockets
vidiotic-chop|--no-default-features|PASS|portable span editor: the marking session — spans, marks, playhead, jog window, undo — and every panel that draws it; no ffmpeg, no rfd, no fs
vidiotic-wire|--features client|FAIL|std::os::unix in client.rs:11 — web transport is BroadcastChannel (web-port.md §10)
vidiotic-core|--no-default-features --features ffmpeg|FAIL|ffmpeg-sys-next build script — this row is why the feature exists
vidiotic-bake|--no-default-features --features ffmpeg|FAIL|bindgen/ffmpeg via transcode.rs — this row is why the feature exists
EOF
)

# Test suites that must pass **under wasm32, in a real engine**.
#
# Building for wasm proves only that the portable half compiles. These prove it
# behaves: the same assertions, run in V8 via wasm-bindgen-test-runner. Each
# test module aliases `#[test]` to `#[wasm_bindgen_test]` under wasm32, so this
# runs the same test bodies as the native suite rather than a parallel copy.
#
# The fourth field is a MINIMUM test count, not an exact one. That is deliberate:
# if the alias is ever dropped from a module, its tests silently compile away and
# the runner cheerfully reports "no tests to run!" — a pass. A minimum catches
# that without going stale every time a test is added. (An exact count is the
# same trap as a hardcoded variant count: it fails for the wrong reason.)
TESTS=$(cat <<'EOF'
vidiotic-bake|--no-default-features|bc1_golden|5|BC1 bytes identical to native — the /chop byte-identity claim
vidiotic-bake|--no-default-features|hap_conformance|6|Hap1 decode of real packets, in wasm
vidiotic-bake|--no-default-features|--lib|39|frame + hap + mov (write AND read) unit tests, and the ingest tier
vidiotic-core|--no-default-features|--lib|39|project/isf/time model, including load/save round-trip, the version refusal and clip relinking — the filesystem is behind the `Fs` trait now, so nothing here is native-only — plus the zip both browser shells write a bundle with
vidiotic-play|--no-default-features|--lib|103|GLSL->naga compile, the clip timeline (demux->hap->frame), the software BC1/BC3/BC4 fallback, the audio analyser, the engine (grammar arpeggios, beat clock, sequencer, undo, cue rotation), AND the real control panels — 2,228 lines of egui that used to be native-only — plus the pool-as-filesystem a browser .viproj resolves against
vidiotic-chop|--no-default-features|--lib|48|undo coalescing, bank reindexing, wall-clock playback, span reconstruction from a reopened project, the key table both shells resolve against, the .viproj assembly both exporters share, the offsets render that turns spans into trimmed cues, the .vprep sidecar both shells store, and the shell boundary itself — that every file chooser leaves as a request and a mid-frame drain parks what it cannot run
EOF
)

if ! rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown; then
  echo "wasm32-unknown-unknown target is not installed."
  echo "  rustup target add wasm32-unknown-unknown"
  exit 2
fi

fail=0
pass_ok=0; fail_ok=0

printf '%-16s %-24s %-8s %s\n' CRATE FEATURES EXPECT RESULT
printf '%.0s-' {1..92}; printf '\n'

while IFS='|' read -r crate feats expect note; do
  [ -z "$crate" ] && continue

  # shellcheck disable=SC2086
  out=$(cargo check -p "$crate" --target wasm32-unknown-unknown $feats --quiet 2>&1)
  if [ $? -eq 0 ]; then actual=PASS; else actual=FAIL; fi

  if [ "$actual" = "$expect" ]; then
    if [ "$expect" = PASS ]; then
      printf '%-16s %-24s %-8s ok\n' "$crate" "$feats" "$expect"
      pass_ok=$((pass_ok+1))
    else
      printf '%-16s %-24s %-8s ok — blocked: %s\n' "$crate" "$feats" "$expect" "$note"
      fail_ok=$((fail_ok+1))
    fi
  elif [ "$expect" = PASS ]; then
    printf '%-16s %-24s %-8s REGRESSION — was portable, now does not build\n' "$crate" "$feats" "$expect"
    [ $VERBOSE -eq 1 ] && echo "$out" | sed 's/^/    /'
    fail=$((fail+1))
  else
    printf '%-16s %-24s %-8s RATCHET — now builds; move this row to PASS\n' "$crate" "$feats" "$expect"
    fail=$((fail+1))
  fi
done <<< "$TABLE"

printf '%.0s-' {1..92}; printf '\n'
echo "$pass_ok portable, $fail_ok still blocked, $fail need attention"

echo
# `cargo install` puts the runner in ~/.cargo/bin, which is not on PATH when
# rustup came from a package manager rather than rustup-init — so look there
# before believing it is missing. Exported, because it is `cargo test` that has
# to find it, via the runner key in .cargo/config.toml.
if ! command -v wasm-bindgen-test-runner >/dev/null 2>&1 \
  && [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/wasm-bindgen-test-runner" ]; then
  export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
fi

if ! command -v wasm-bindgen-test-runner >/dev/null 2>&1; then
  echo "SKIPPING the wasm test run: wasm-bindgen-test-runner is not installed,"
  echo "and is not in ${CARGO_HOME:-$HOME/.cargo}/bin either."
  echo "  cargo install wasm-bindgen-cli --version \$(grep -A1 '^name = \"wasm-bindgen\"\$' Cargo.lock | sed -n 's/version = \"\\(.*\\)\"/\\1/p' | head -1) --locked"
  echo "  (the version must match Cargo.lock, or the runner rejects the module)"
  echo
  echo "Build rows above still ran. Exiting non-zero: an unrun gate is not a green gate."
  exit 1
fi

printf '%-16s %-24s %-18s %-7s %s\n' CRATE FEATURES SUITE TESTS RESULT
printf '%.0s-' {1..100}; printf '\n'

tests_ok=0; total_tests=0
while IFS='|' read -r crate feats suite min note; do
  [ -z "$crate" ] && continue

  # `--lib` selects the in-crate #[cfg(test)] modules; anything else is an
  # integration suite in tests/ and needs --test.
  if [ "$suite" = "--lib" ]; then sel=(--lib); else sel=(--test "$suite"); fi

  # shellcheck disable=SC2086
  out=$(cargo test -p "$crate" $feats --target wasm32-unknown-unknown "${sel[@]}" 2>&1)
  rc=$?
  # "test result: ok. 24 passed; ..." — 0 if the line never appeared.
  ran=$(echo "$out" | sed -n 's/^test result: ok\. \([0-9]*\) passed.*/\1/p' | head -1)
  ran=${ran:-0}
  total_tests=$((total_tests + ran))

  if [ $rc -ne 0 ]; then
    printf '%-16s %-24s %-18s %-7s FAILED\n' "$crate" "$feats" "$suite" "$ran"
    [ $VERBOSE -eq 1 ] && echo "$out" | sed 's/^/    /'
    fail=$((fail+1))
  elif [ "$ran" -lt "$min" ]; then
    # The silent-vanish case: a module that lost its #[test] alias compiles to
    # nothing and the runner reports success over an empty suite.
    printf '%-16s %-24s %-18s %-7s VANISHED — ran %s, expected >= %s\n' \
      "$crate" "$feats" "$suite" "$ran" "$ran" "$min"
    fail=$((fail+1))
  else
    printf '%-16s %-24s %-18s %-7s ok — %s\n' "$crate" "$feats" "$suite" "$ran" "$note"
    tests_ok=$((tests_ok+1))
  fi
done <<< "$TESTS"

printf '%.0s-' {1..100}; printf '\n'
echo "$tests_ok suites / $total_tests tests pass under wasm32 in V8"

exit $(( fail > 0 ? 1 : 0 ))
