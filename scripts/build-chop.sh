#!/usr/bin/env bash
# build-chop — compile /chop to wasm and emit the loadable bundle into web/.
#
# No bundler and no npm, deliberately. `wasm-bindgen-cli` is already a
# requirement of scripts/wasm-gate.sh and is pinned to Cargo.lock, so this
# reuses a toolchain the repo already declares rather than adding a second one
# with its own config format. The output is a plain ES module and a static
# index.html; the whole site is five files that any static host will serve —
# the wasm, wasm-bindgen's glue, the page, boot.js, and the .wasm's sibling .js.
# boot.js is a separate file rather than a script block in the page because the
# deploy target's CSP has no 'unsafe-inline'; see its header.
#
# Usage:  scripts/build-chop.sh [--debug] [--serve]
#   --debug   unoptimized build (much faster to compile, much slower to run)
#   --serve   after building, serve web/ on http://localhost:8080
#
# Serve it over http://, never file:// — a file:// page has an opaque origin,
# which costs OPFS entirely and changes how popups and permissions behave. The
# localhost server is also how this deploys, so it is the honest environment.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

if ! grep -q '^\[workspace\]' Cargo.toml 2>/dev/null; then
  echo "not at the workspace root (cwd: $PWD)" >&2
  exit 2
fi

PROFILE=release-wasm
CARGO_PROFILE_FLAG="--profile release-wasm"
SERVE=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug; CARGO_PROFILE_FLAG= ;;
    --serve) SERVE=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

if ! rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown; then
  echo "wasm32-unknown-unknown target is not installed."
  echo "  rustup target add wasm32-unknown-unknown"
  exit 2
fi

# The generated JS shim and the .wasm agree on a schema version, so a mismatched
# CLI produces a bundle that fails at load with an opaque error. Pin to the lock
# file, exactly as the gate does.
WBG_VERSION=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/version = "\(.*\)"/\1/p' | head -1)
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "wasm-bindgen is not installed."
  echo "  cargo install wasm-bindgen-cli --version $WBG_VERSION --locked"
  exit 2
fi
HAVE=$(wasm-bindgen --version | awk '{print $2}')
if [ "$HAVE" != "$WBG_VERSION" ]; then
  echo "wasm-bindgen $HAVE does not match Cargo.lock's wasm-bindgen $WBG_VERSION."
  echo "  cargo install wasm-bindgen-cli --version $WBG_VERSION --locked"
  echo "(a mismatch fails at page load, not here — refusing to build a broken bundle)"
  exit 2
fi

echo "building vidiotic-chop ($PROFILE) for wasm32..."
# --no-default-features matches the wasm-gate row: it is what keeps ffmpeg out
# of the graph, and the browser build must be the same one the gate checks.
# shellcheck disable=SC2086
cargo build -p vidiotic-chop --target wasm32-unknown-unknown --no-default-features $CARGO_PROFILE_FLAG || exit 1

WASM="target/wasm32-unknown-unknown/$PROFILE/vidiotic_chop.wasm"
[ -f "$WASM" ] || { echo "no wasm at $WASM" >&2; exit 1; }

echo "running wasm-bindgen..."
wasm-bindgen --target web --out-dir web/pkg-chop --no-typescript "$WASM" || exit 1

OUT=web/pkg-chop/vidiotic_chop_bg.wasm
BEFORE=$(wc -c <"$OUT" | tr -d ' ')

# wasm-opt is a whole-module optimizer and sees things LLVM could not: it runs
# after wasm-bindgen has spliced in the JS glue and deleted the unused exports,
# so a large amount of code is only provably dead at this point. The bundle is a
# download — this is user-facing latency, not tidiness.
#
# Optional rather than required, and loud about it: a contributor without
# binaryen should still get a working page, just a fatter one. The gate is not
# where this belongs (it does not build a bundle), so the warning is here.
if command -v wasm-opt >/dev/null 2>&1; then
  echo "running wasm-opt -Oz..."
  # --enable-* mirrors what rustc emits for this target; binaryen refuses a
  # module using a feature it was not told to allow, and the error names the
  # feature rather than the fix.
  wasm-opt -Oz \
    --enable-bulk-memory \
    --enable-mutable-globals \
    --enable-nontrapping-float-to-int \
    --enable-sign-ext \
    --enable-reference-types \
    --enable-simd \
    -o "$OUT.opt" "$OUT" && mv "$OUT.opt" "$OUT"
else
  echo
  echo "NOTE: wasm-opt is not installed, so the bundle ships unoptimized."
  echo "  brew install binaryen        (or your platform's binaryen package)"
  echo "  measured: it removes roughly a third of the module."
fi

AFTER=$(wc -c <"$OUT" | tr -d ' ')
echo
if [ "$AFTER" -lt "$BEFORE" ]; then
  echo "built $OUT ($(( BEFORE / 1024 )) KiB -> $(( AFTER / 1024 )) KiB)"
else
  echo "built $OUT ($(( AFTER / 1024 )) KiB)"
fi
echo "serve:  python3 -m http.server -d web 8080   then open http://localhost:8080/chop.html"

if [ "$SERVE" = 1 ]; then
  echo
  echo "serving http://localhost:8080/chop.html (ctrl-c to stop)"
  exec python3 -m http.server -d web 8080
fi
