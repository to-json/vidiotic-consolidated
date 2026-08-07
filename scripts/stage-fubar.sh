#!/usr/bin/env bash
# stage-fubar — run /play as a fubarchitect warez, on fubarchitect's own config,
# before it is on fubarchitect.
#
# `serve-play.sh` already rehearses /play as its own site at a document root,
# served by the nginx.conf `release-play.sh` generates. That is the deployment
# /play was built for. It is not the deployment happening.
#
# On fubarchitect a warez is a self-contained directory that a Ruby SSG copies
# verbatim into `_site/`, served by a vhost written for a text-and-CSS personal
# site with a deliberately strict CSP. The generated nginx.conf is never read;
# the `.br` and `.gz` files beside the wasm are never served; and the policy the
# page meets is one written before anybody thought about WebGPU. This script
# stands that up locally so the collisions happen here.
#
# Usage:  scripts/stage-fubar.sh [--strict] [--no-build] [--smoke] [--stop]
#                                [--dist PATH] [--site PATH] [--port N]
#
#   --strict    serve /warez/vidiotic/ with NO block of its own, so it inherits
#               the site-wide policy exactly as a warez dropped in today would.
#               This is the "before" picture; without it you get the "after".
#   --no-build  use the dist/play that is already there.
#   --smoke     after the checks, drive the page in a real browser via
#               scripts/play-smoke.mjs pointed at this container.
#   --stop      tear the container down and exit.
#   --dist      the assembled /play artifact (default: ./dist/play, then
#               $VIDIOTIC_DIST). See "Where the build happens" below.
#   --site      a built fubarchitect _site to use as the surrounding site
#               (default: ../webb/_site, then a generated stub).
#   --clips     the smoke test's video fixtures (default: ./clips, then the
#               directory beside --dist). Only staging serves these.
#   --port      host port for http (default 8080). https is --port + 363,
#               mirroring the 8180/8543 offset the Vagrantfile uses.
#
# ── Where the build happens ───────────────────────────────────────────────────
#
# This repo tracks only the workspace glue; every member crate is its own git
# repository and is not checked out beside a `git worktree` of it. So in a
# worktree there is no vidiotic-play to compile, and this script will say so and
# ask for a --dist built from the main checkout rather than fail inside cargo
# with a missing-member error that explains nothing.
#
# ── Media ─────────────────────────────────────────────────────────────────────
#
# The staging root is assembled by copy rather than bind-mounted, because
# `_site` is 186 MB and 181 MB of that is two music mixes that have nothing to
# do with this. They are excluded; everything else is real.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

STRICT=0
BUILD=1
SMOKE=0
STOP=0
PORT=8080
DIST=${VIDIOTIC_DIST:-}
SITE=${VIDIOTIC_FUBAR_SITE:-}
CLIPS=${VIDIOTIC_CLIPS:-}
want=""
for arg in "$@"; do
  if [ -n "$want" ]; then
    case "$want" in
      dist) DIST=$arg ;; site) SITE=$arg ;; port) PORT=$arg ;; clips) CLIPS=$arg ;;
    esac
    want=""; continue
  fi
  case "$arg" in
    --strict) STRICT=1 ;;
    --no-build) BUILD=0 ;;
    --smoke) SMOKE=1 ;;
    --stop) STOP=1 ;;
    --dist) want=dist ;;
    --site) want=site ;;
    --port) want=port ;;
    --clips) want=clips ;;
    --dist=*) DIST=${arg#*=} ;;
    --site=*) SITE=${arg#*=} ;;
    --port=*) PORT=${arg#*=} ;;
    --clips=*) CLIPS=${arg#*=} ;;
    -*) echo "unknown option: $arg" >&2; exit 2 ;;
    *) echo "unexpected argument: $arg" >&2; exit 2 ;;
  esac
done
[ -z "$want" ] || { echo "--$want needs a value" >&2; exit 2; }

TLS_PORT=$(( PORT + 463 ))
NAME=vidiotic-fubar
IMAGE=vidiotic-fubar:latest
BASE="http://localhost:$PORT/warez/vidiotic"
STAGE=${TMPDIR:-/tmp}/vidiotic-fubar-root

say()  { printf '%s\n' "$*"; }
head2() { printf '\n\033[1m── %s\033[0m\n' "$*"; }

if [ "$STOP" = 1 ]; then
  docker rm -f "$NAME" >/dev/null 2>&1 && say "stopped $NAME" || say "$NAME was not running"
  exit 0
fi

# ── 1. the artifact ───────────────────────────────────────────────────────────
head2 "artifact"

HAVE_CRATES=0
[ -d vidiotic-play ] && HAVE_CRATES=1

if [ "$BUILD" = 1 ] && [ "$HAVE_CRATES" = 0 ]; then
  cat >&2 <<'NOCRATES'
no ./vidiotic-play here — this is the workspace-glue repo without the member
crates beside it, which is what a `git worktree` of it looks like.

Build the artifact in the main checkout, then point this at it:

    (cd /path/to/vidiotic && scripts/release-play.sh --base /warez/vidiotic)
    scripts/stage-fubar.sh --no-build --dist /path/to/vidiotic/dist/play
NOCRATES
  exit 2
fi

if [ "$BUILD" = 1 ]; then
  # --base is what threads the deploy path through the generated config and
  # _headers. Neither is served here — fubarchitect's vhost is — but it is also
  # recorded in version.json, and a stamp that disagrees with where the thing
  # actually sits is how a later debugging session gets lied to.
  bash scripts/release-play.sh --base /warez/vidiotic || exit 1
fi

[ -n "$DIST" ] || DIST=dist/play
if [ ! -f "$DIST/index.html" ]; then
  echo "no artifact at $DIST — run scripts/release-play.sh, or pass --dist" >&2
  exit 2
fi

PKG=$(find "$DIST" -maxdepth 1 -type d -name 'pkg-*' | head -1)
[ -n "$PKG" ] || { echo "no pkg-* bundle in $DIST" >&2; exit 2; }
PKG=$(basename "$PKG")
BUILD_STAMP=$(sed -n 's/.*"build": "\(.*\)".*/\1/p' "$DIST/version.json" 2>/dev/null)

# The smoke test's fixtures. Not necessarily beside this script: in a worktree
# of the glue repo there is no clips/, and mounting the missing directory gets
# you an empty one rather than an error — every clip then 404s, `is_baked` reads
# an HTML error page, decides the file is not baked, and hands a HAP .mov to the
# browser's decoder. The failure surfaces as "this browser cannot decode it",
# which blames the browser for a missing bind mount. So it is resolved, and its
# absence is fatal here rather than ten minutes later.
if [ -z "${CLIPS:-}" ]; then
  for c in clips "$(dirname "$DIST")/clips" "$(cd "$(dirname "$DIST")/.." 2>/dev/null && pwd)/clips"; do
    [ -d "$c" ] && { CLIPS=$c; break; }
  done
fi
if [ -z "${CLIPS:-}" ] || [ ! -d "$CLIPS" ]; then
  echo "no clips/ found (looked beside this script and beside $DIST) — pass --clips" >&2
  exit 2
fi
CLIPS=$(cd "$CLIPS" && pwd)

say "artifact:  $DIST"
say "clips:     $CLIPS"
say "bundle:    $PKG"
say "build:     ${BUILD_STAMP:-unknown}"

# ── 2. the surrounding site ───────────────────────────────────────────────────
head2 "staging root"

if [ -z "$SITE" ]; then
  for c in ../webb/_site ../../webb/_site "$HOME/code/loot/webb/_site"; do
    [ -d "$c" ] && { SITE=$c; break; }
  done
fi

rm -rf "$STAGE"
mkdir -p "$STAGE/warez"

if [ -n "$SITE" ] && [ -d "$SITE" ]; then
  # Everything but the mixes. rsync is not assumed — macOS has it, but openrsync
  # and GNU rsync disagree enough elsewhere in this repo that a plain copy plus
  # a delete is the more predictable of the two here.
  (cd "$SITE" && tar cf - .) | (cd "$STAGE" && tar xf -) || exit 1
  find "$STAGE" \( -name '*.mp3' -o -name '*.wav' -o -name '*.flac' -o -name '*.ogg' \) -delete
  say "site:      $SITE (real build, audio stripped)"
else
  # A stub, so the rig still works for somebody who has vidiotic checked out and
  # not the site. The vhost is the part under test; the neighbours are context.
  cat > "$STAGE/index.html" <<'STUB'
<!doctype html><meta charset=utf-8><title>fubarchitect (stub)</title>
<h1>warez</h1><ul><li><a href="/warez/vidiotic/">vidiotic</a></li></ul>
<p>Stub root — no built <code>_site</code> was found. Pass <code>--site</code>
to stage against the real one.</p>
STUB
  printf '<!doctype html><title>404</title>not found\n' > "$STAGE/404.html"
  printf '<!doctype html><title>500</title>server error\n' > "$STAGE/500.html"
  say "site:      (stub — no _site found; pass --site for the real one)"
fi

cp -R "$DIST" "$STAGE/warez/vidiotic" || exit 1
say "warez:     $STAGE/warez/vidiotic"
say "size:      $(du -sh "$STAGE" | awk '{print $1}')"

# ── 3. the server ─────────────────────────────────────────────────────────────
head2 "container"

docker build -q -t "$IMAGE" docker/fubar >/dev/null || exit 1
docker rm -f "$NAME" >/dev/null 2>&1

VIDIOTIC_BLOCK=warez-vidiotic.conf
[ "$STRICT" = 1 ] && VIDIOTIC_BLOCK=warez-vidiotic-strict.conf
CONF=$PWD/docker/fubar

docker run -d --name "$NAME" \
  -p "$PORT:80" -p "$TLS_PORT:443" \
  -v "$STAGE:/srv/root:ro" \
  -v "$CLIPS:/srv/clips:ro" \
  -v "$CONF/vhost.conf:/etc/nginx/sites-enabled/fubarchitect.conf:ro" \
  -v "$CONF/site.conf:/etc/nginx/fubar/site.conf:ro" \
  -v "$CONF/staging-clips.conf:/etc/nginx/fubar/staging-clips.conf:ro" \
  -v "$CONF/$VIDIOTIC_BLOCK:/etc/nginx/fubar/warez-vidiotic.conf:ro" \
  -v vidiotic-fubar-tls:/etc/nginx/tls \
  "$IMAGE" >/dev/null || exit 1

if [ "$STRICT" = 1 ]; then
  say "policy:    SITE-WIDE (--strict) — /warez/vidiotic/ has no block of its own"
else
  say "policy:    warez-vidiotic.conf"
fi

# The first start generates a 2048-bit dhparam, which is not fast. Wait for the
# port rather than sleeping a guessed number of seconds.
for _ in $(seq 1 120); do
  curl -fsS -o /dev/null "http://localhost:$PORT/" 2>/dev/null && break
  docker ps --format '{{.Names}}' | grep -qx "$NAME" || {
    echo; echo "container exited — nginx -t output:" >&2
    docker logs "$NAME" 2>&1 | tail -30 >&2
    exit 1
  }
  sleep 1
done

say "http:      http://localhost:$PORT/"
say "page:      $BASE/"

# ── 4. what the page actually gets ────────────────────────────────────────────
head2 "checks"

PASS=0; FAIL=0
hdr() { curl -sS -D- -o /dev/null "$1" 2>/dev/null; }
ck() { # ck <label> <condition-result> <detail>
  if [ "$2" = 0 ]; then PASS=$((PASS+1)); printf '  \033[32mok\033[0m   %s\n' "$1"
  else FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n       %s\n' "$1" "$3"; fi
}

IDX=$(hdr "$BASE/index.html")
WASM_URL="$BASE/$PKG/vidiotic_play_bg.wasm"
WASM=$(hdr "$WASM_URL")

# The module is refused outright if this is octet-stream. Debian's mime.types
# has mapped it since 1.21.4, but "has" is a claim about a file on a box.
ct=$(printf '%s' "$WASM" | tr -d '\r' | sed -n 's/^content-type: //Ip')
ck "wasm served as application/wasm" \
   "$([ "$ct" = "application/wasm" ] && echo 0 || echo 1)" \
   "got: ${ct:-<none>}"

# Deploy metadata is not site content.
for meta in nginx.conf _headers; do
  code=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/$meta")
  ck "$meta is not public" "$([ "$code" = 404 ] && echo 0 || echo 1)" "got HTTP $code"
done

# A location that sets any add_header drops every inherited one. This is the
# check that catches that, and it is the one the basalt block exists to satisfy.
for probe_name in "index.html:$IDX" "wasm:$WASM"; do
  label=${probe_name%%:*}
  body=${probe_name#*:}
  ck "$label keeps the site's HSTS header" \
     "$(printf '%s' "$body" | grep -qi '^strict-transport-security:' && echo 0 || echo 1)" \
     "the location set an add_header and discarded the server's set"
done

csp=$(printf '%s' "$IDX" | tr -d '\r' | sed -n 's/^content-security-policy: //Ip')
if [ "$STRICT" = 1 ]; then
  say "  --   site-wide policy in force; the three checks below are expected to FAIL"
fi
ck "CSP admits a blob: <video> (clip ingest)" \
   "$(printf '%s' "$csp" | grep -q "media-src[^;]*blob:" && echo 0 || echo 1)" \
   "media-src falls back to default-src 'self'; every dropped clip errors"
ck "CSP admits a blob: worklet (audio capture)" \
   "$(printf '%s' "$csp" | grep -q "script-src[^;]*blob:" && echo 0 || echo 1)" \
   "AudioWorklet.addModule(blob:) is refused; Listen throws"
pp=$(printf '%s' "$IDX" | tr -d '\r' | sed -n 's/^permissions-policy: //Ip')
ck "Permissions-Policy allows the microphone fallback" \
   "$(printf '%s' "$pp" | grep -q "microphone=(self)" && echo 0 || echo 1)" \
   "microphone=() is a flat denial: getUserMedia rejects without prompting"

# Caching. The bundle name is a content hash, so it can be immutable; the page
# names the bundle, so it must not be.
cc=$(printf '%s' "$WASM" | tr -d '\r' | sed -n 's/^cache-control: //Ip')
ck "bundle is immutable" \
   "$(printf '%s' "$cc" | grep -q "immutable" && echo 0 || echo 1)" \
   "got: ${cc:-<none>} — a 6.6 MB revalidation on every visit"
icc=$(printf '%s' "$IDX" | tr -d '\r' | sed -n 's/^cache-control: //Ip')
ck "index.html is no-cache" \
   "$(printf '%s' "$icc" | grep -qi "no-cache" && echo 0 || echo 1)" \
   "got: ${icc:-<none>} — a cached page can outlive the bundle it names"

# Fixtures. A 404 here does not look like a 404 downstream: the page fetches the
# error document, `is_baked` finds no HAP header in it, and the browser is asked
# to decode a file it was never meant to touch. Checked explicitly so the next
# person reads "the clip is not being served" instead of "this browser cannot
# decode it".
clip_code=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/clips/bun.mov")
ck "smoke fixtures are reachable" \
   "$([ "$clip_code" = 200 ] && echo 0 || echo 1)" \
   "GET $BASE/clips/bun.mov returned $clip_code — check the clips mount"

# The neighbours. A per-warez block that changed anything outside itself would
# be a worse bug than the one it fixes.
root_csp=$(hdr "http://localhost:$PORT/" | tr -d '\r' | sed -n 's/^content-security-policy: //Ip')
ck "site root policy is untouched" \
   "$(printf '%s' "$root_csp" | grep -q "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'" && echo 0 || echo 1)" \
   "got: ${root_csp:-<none>}"

# ── 5. the number that decides whether this is worth doing ────────────────────
head2 "transfer"

# Asked for the way a browser asks, because that is the only way gzip_static
# ever answers — a request without Accept-Encoding gets the raw file and the
# rig would report a regression that is not there.
WASMZ=$(curl -sS -D- -o /dev/null -H 'Accept-Encoding: gzip, br' "$WASM_URL" 2>/dev/null)
enc=$(printf '%s' "$WASMZ" | tr -d '\r' | sed -n 's/^content-encoding: //Ip')
len=$(printf '%s' "$WASMZ" | tr -d '\r' | sed -n 's/^content-length: //Ip')
raw=$(wc -c < "$STAGE/warez/vidiotic/$PKG/vidiotic_play_bg.wasm" | tr -d ' ')
br=$(wc -c < "$STAGE/warez/vidiotic/$PKG/vidiotic_play_bg.wasm.br" 2>/dev/null | tr -d ' ')

ck "the module is compressed on the wire" \
   "$([ -n "$enc" ] && echo 0 || echo 1)" \
   "served raw: gzip_types has no application/wasm and gzip_static is off"

say ""
say "  on disk:      $raw bytes"
say "  on the wire:  ${len:-$raw} bytes${enc:+ ($enc)}"
if [ -n "$enc" ] && [ -n "$len" ]; then
  say "  saved:        $(( (raw - len) / 1048576 )) MB per cold visit"
  say ""
  say "  The .br beside it is ${br:-?} bytes — another $(( (len - ${br:-len}) / 1024 )) KB, if"
  say "  ngx_brotli is ever installed on the box. Nothing else has to change."
else
  say ""
  say "  fubarchitect's gzip_types has no application/wasm entry, and without"
  say "  gzip_static the .gz and .br beside it are never served."
fi

# ── 6. optional: a real browser ───────────────────────────────────────────────
if [ "$SMOKE" = 1 ]; then
  head2 "smoke"
  if [ ! -f scripts/play-smoke.mjs ]; then
    say "  scripts/play-smoke.mjs is not here — skipping"
  else
    node scripts/play-smoke.mjs --url "$BASE"
    ck "browser smoke" "$?" "see above"
  fi
fi

head2 "result"
say "  $PASS passed, $FAIL failed"
say ""
say "  page:   $BASE/"
say "  logs:   docker logs -f $NAME"
say "  stop:   scripts/stage-fubar.sh --stop"
[ "$FAIL" = 0 ] || exit 1
