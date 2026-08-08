# Justfile for Vidiotic workspace

warez_target := "../webb/warez/08-vidiotic"
port := "8080"

# Default action: list recipes
default:
    @just --list

# Build both /play and /chop WASM binaries
build:
    bash scripts/build-play.sh
    bash scripts/build-chop.sh

# Assemble production web release in dist/web/
release base="/":
    bash scripts/release-web.sh --base {{base}}

# Run a local web server with the current web build
demo port=port:
    @if [ ! -d dist/web ]; then bash scripts/release-web.sh; fi
    @echo "Serving http://localhost:{{port}} (ctrl-c to stop)"
    python3 -m http.server -d dist/web {{port}}

# Build web release for /warez/vidiotic/ and copy into webb's warez directory
ship warez=warez_target:
    bash scripts/release-web.sh --base /warez/vidiotic
    mkdir -p {{warez}}
    cp -r dist/web/* {{warez}}/
    @echo "Published release to {{warez}}"

# Check WASM compilation gate across all crates
gate:
    bash scripts/wasm-gate.sh

# Run browser smoke tests for both /play and /chop
smoke:
    node scripts/play-smoke.mjs
    node scripts/chop-smoke.mjs
