#!/usr/bin/env bash
# build-play — compile /play to wasm and emit the loadable bundle into web/.
#
# The build itself is `build-wasm.sh`, shared with /chop; this name stays because
# the docs, the smoke scripts, the Justfile and several error messages point at
# it. See that script's header for what it does and why there is no bundler.
#
# Usage:  scripts/build-play.sh [--debug] [--serve]
exec bash "$(dirname "$0")/build-wasm.sh" play "$@"
