# Justfile for Vidiotic workspace

warez_target := "../webb/warez/08-vidiotic"
port := "8080"

# Default action: list recipes
default:
    @just --list

# Build both /play and /chop WASM binaries (pass --debug for the fast, slow-running build)
build *args:
    bash scripts/build-play.sh {{args}}
    bash scripts/build-chop.sh {{args}}

# Rewrite every source file to rustfmt's defaults
fmt:
    cargo fmt --all

# Fail if anything is unformatted. What CI runs.
fmt-check:
    cargo fmt --all -- --check

# Lint at the workspace lint tier, tests and examples included
lint:
    cargo clippy --workspace --all-targets

# The native test suite
test:
    cargo test --workspace

# fmt, lint, test, and the wasm ratchet — the whole gate, locally
check: fmt-check lint test gate

# Drop build artifacts (cargo's, and the web/native release trees)
clean:
    cargo clean
    rm -rf dist web/pkg web/pkg-chop

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

# Assemble native macOS application bundle (dist/Vidiotic.app)
app:
    bash packaging/bundle.sh

# Assemble debug build of native macOS application bundle
app-debug:
    bash packaging/bundle.sh --debug

# Assemble native macOS application bundle and DMG disk image (dist/Vidiotic.dmg)
dmg:
    bash packaging/bundle.sh --dmg

# Build native macOS application and install to /Applications
install-app:
    bash packaging/bundle.sh --install
