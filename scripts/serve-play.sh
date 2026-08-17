#!/usr/bin/env bash
# serve-play — run the web release behind the same nginx the server will run.
#
# Reads dist/web (what `release-web.sh` builds, two pages and two bundles) and
# falls back to dist/play (what `release-play.sh` builds, one of each). It used
# to read dist/play only, which `release-web.sh` kept filled with a copy of
# dist/web purely to keep this script working — and the copy did not keep it
# working: the bundle scan grabbed whichever of `pkg-play-*`/`pkg-chop-*` came
# first, and the hash check looked for a `wasm_sha256` key that the two-page
# version.json does not have. Both degraded quietly, which is the exact failure
# class this script exists to catch.
#
# `build-play.sh --serve` and `python3 -m http.server` both answer "does the
# page work", and both are blind to everything the *deployment* can get wrong:
# whether the generated nginx.conf is a config nginx will load at all, whether
# the .wasm arrives as application/wasm, whether the cache headers that make a
# second visit cheap are the ones that come back, whether the precompressed
# copies are the ones on the wire and whether they decompress to the module that
# was built, whether the server's own security headers survive a location that
# sets a Cache-Control. Those are discovered on the real box otherwise, which is
# the wrong place.
#
# So: a container running nginx over the real artifact, and a set of assertions
# about what comes out of it.
#
# Usage:  scripts/serve-play.sh [--check] [--stop] [--port N] [--base PATH] [--rebuild]
#
#   --check      start, assert, stop again — the CI-shaped form
#   --stop       stop and remove a running container
#   --port N     http port on the host (default 8080; TLS is N+363, i.e. 8443)
#   --base PATH  rehearse a subdirectory deploy: the artifact is served at
#                http://localhost:N/PATH/. It must match the --base the artifact
#                was released with, and the mismatch is the point — a config
#                built for / and served at /play matches nothing, returns 200 for
#                everything, and loses caching and precompression silently.
#   --rebuild    force a docker build even if the image exists
#
# Left running without --check, so the page can be opened by hand and driven
# with `node scripts/play-smoke.mjs --url http://localhost:8080`.
#
# http rather than https by default: localhost is a secure context either way,
# so WebGPU works and nothing has to be told to trust a self-signed cert. The
# container serves TLS as well, on the second port, because `listen 443 ssl` is
# a different path through the config than `listen 80` — the browser will refuse
# the certificate, but `nginx -t` and `curl -k` will not.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

IMAGE=vidiotic-play-nginx
NAME=vidiotic-play
PORT=8080
CHECK=0
STOP=0
REBUILD=0
BASE=""
want=
for arg in "$@"; do
  if [ -n "$want" ]; then
    case "$want" in port) PORT=$arg ;; base) BASE=$arg ;; esac
    want=; continue
  fi
  case "$arg" in
    --check) CHECK=1 ;;
    --stop) STOP=1 ;;
    --rebuild) REBUILD=1 ;;
    --port) want=port ;;
    --port=*) PORT=${arg#*=} ;;
    --base) want=base ;;
    --base=*) BASE=${arg#*=} ;;
    [0-9]*) PORT=$arg ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
[ -z "$want" ] || { echo "--$want needs a value" >&2; exit 2; }

BASE=${BASE%/}
case "$BASE" in ""|"/") BASE="" ;; /*) ;; *) BASE="/$BASE" ;; esac
TLS_PORT=$(( PORT + 363 ))

stop() { docker rm -f "$NAME" >/dev/null 2>&1; }

if [ "$STOP" = 1 ]; then
  stop && echo "stopped $NAME"
  exit 0
fi

command -v docker >/dev/null 2>&1 || { echo "docker is not installed" >&2; exit 2; }
# Which artifact to rehearse: the two-page release if it is there, else the
# single-page one. Named once, because everything below reads it.
ART=dist/web
[ -f "$ART/index.html" ] || ART=dist/play
if [ ! -f "$ART/index.html" ]; then
  echo "no dist/web or dist/play — run scripts/release-web.sh" >&2
  exit 2
fi
[ -f "$ART/nginx.conf" ] || { echo "$ART/nginx.conf is missing — rebuild it" >&2; exit 2; }
echo "rehearsing $ART"



# The artifact records the prefix it was generated for. Serving it under a
# different one is the silent-failure shape this script exists to expose, so say
# so rather than quietly producing a confusing run.
ART_BASE=$(sed -n 's/.*"base": "\(.*\)".*/\1/p' "$ART/version.json")
ART_BASE=${ART_BASE%/}
[ -n "$ART_BASE" ] || ART_BASE="/"
WANT_BASE=${BASE:-/}
if [ "$ART_BASE" != "$WANT_BASE" ]; then
  echo "$ART was released for base '$ART_BASE' but you asked to serve it at '$WANT_BASE'."
  echo "That mismatch makes every location in its nginx.conf match nothing —"
  echo "which is a real failure mode, but it is not a test of the artifact."
  echo "  scripts/release-web.sh --no-build --base $WANT_BASE"
  exit 2
fi

if [ "$REBUILD" = 1 ] || ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
  echo "building $IMAGE..."
  docker build -q -t "$IMAGE" docker/play || exit 1
fi

# The server block, with the site's location inside the document root filled in.
# Kept in a scratch file and bind-mounted so docker/play/nginx.conf stays the
# thing an operator reads and copies.
CONF=$(mktemp -t vidiotic-play-nginx)
SITE_DIR=${BASE:-.}
SITE_DIR=${SITE_DIR#/}
sed "s|SITE_DIR|$SITE_DIR|" docker/play/nginx.conf >"$CONF" || exit 1

stop
echo "starting nginx on http://localhost:$PORT$BASE/ (tls https://localhost:$TLS_PORT$BASE/)"
# The artifact is read-only: the container must not be able to change the file it
# is supposed to be serving faithfully. It is mounted at the position in the
# document root that its own --base claims, which is what makes a subdirectory
# rehearsal a real one rather than an alias trick.
#
# clips/ is mounted purely so the smoke test can fetch a real clip from the same
# origin, the way the file picker would hand one over. It is test scaffolding
# and is not part of what gets deployed.
docker run -d --name "$NAME" \
  -p "$PORT:80" -p "$TLS_PORT:443" \
  -v "$PWD/$ART:/srv/root${BASE}:ro" \
  -v "$PWD/clips:/srv/clips:ro" \
  -v "$CONF:/etc/nginx/http.d/play.conf:ro" \
  "$IMAGE" >/dev/null || exit 1

# nginx -t runs in the entrypoint before the port is bound, so a config error
# shows up as a container that exited rather than one that never answers.
for _ in $(seq 40); do
  curl -fsS -o /dev/null "http://localhost:$PORT$BASE/index.html" 2>/dev/null && break
  if [ -z "$(docker ps -q -f name="^$NAME$")" ]; then
    echo
    echo "nginx did not start. Its own account of why:"
    docker logs "$NAME" 2>&1 | tail -20
    stop; rm -f "$CONF"
    exit 1
  fi
  sleep 0.25
done

SITE="http://localhost:$PORT$BASE"
# The play bundle: `pkg-play-<hash>` under dist/web, plain `pkg-<hash>` under
# dist/play. Named by prefix rather than "the first pkg-* found", which under
# dist/web was as likely to be the chop bundle as the play one.
PKG=$(find "$ART" -maxdepth 1 -type d -name 'pkg-play-*' | sort | head -1)
[ -n "$PKG" ] || PKG=$(find "$ART" -maxdepth 1 -type d -name 'pkg-*' ! -name 'pkg-*-*' | sort | head -1)
[ -n "$PKG" ] || { echo "no play bundle in $ART" >&2; stop; rm -f "$CONF"; exit 2; }
PKG=$(basename "$PKG")
WASM="/$PKG/vidiotic_play_bg.wasm"
GLUE="/$PKG/vidiotic_play.js"
# Present only in a dist/web release. Empty means there is no /chop to check.
PKG_CHOP=$(find "$ART" -maxdepth 1 -type d -name 'pkg-chop-*' | sort | head -1)
[ -z "$PKG_CHOP" ] || PKG_CHOP=$(basename "$PKG_CHOP")

fails=0
ok()  { echo "[ OK ] $1"; }
bad() { echo "[FAIL] $1"; fails=$((fails + 1)); }

hdr() { curl -fsSI ${2:+-H "$2"} "$SITE$1" 2>/dev/null; }
field() { printf '%s' "$1" | tr -d '\r' | awk -F': ' -v k="$2" 'tolower($1)==k {print $2; exit}'; }
sha256() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 | awk '{print $1}'
  else sha256sum | awk '{print $1}'; fi
}

echo
echo "--- what nginx actually returns"

H=$(hdr /index.html)
CT=$(field "$H" content-type)
CC=$(field "$H" cache-control)
case "$CT" in
  text/html*) ok "index.html is $CT" ;;
  *) bad "index.html is $CT — a server-level types{} block replaces nginx's MIME map instead of extending it" ;;
esac
case "$CC" in
  *no-cache*) ok "index.html is $CC — a new build is picked up on reload" ;;
  *) bad "index.html Cache-Control is '${CC:-absent}', so a stale page can pin an old bundle forever" ;;
esac

# The bare directory, which is what anybody actually types. nginx's `index`
# does an internal redirect, and whether that re-enters `location = /index.html`
# is not obvious from reading the config.
CC=$(field "$(hdr /)" cache-control)
case "$CC" in
  *no-cache*) ok "the bare $BASE/ is $CC too" ;;
  *) bad "the bare $BASE/ has Cache-Control '${CC:-absent}' — the URL people type is the uncached one" ;;
esac

CT=$(field "$(hdr "$GLUE")" content-type)
case "$CT" in
  *javascript*) ok "the glue is $CT" ;;
  *) bad "the glue is '${CT:-absent}' — a module script with the wrong type is refused outright" ;;
esac

# `wasm_play_sha256` in a dist/web manifest, `wasm_sha256` in a dist/play one.
WANT=$(sed -n 's/.*"wasm_play_sha256": "\(.*\)".*/\1/p' "$ART/version.json")
[ -n "$WANT" ] || WANT=$(sed -n 's/.*"wasm_sha256": "\(.*\)".*/\1/p' "$ART/version.json")
[ -n "$WANT" ] || bad "version.json records no wasm hash — nothing to compare the wire against"
# A release made with --no-brotli ships no .br and comments the directive out.
# Failing it for that would be this script disagreeing with a deliberate choice.
HAS_BR=$(sed -n 's/.*"brotli": \(true\|false\).*/\1/p' "$ART/version.json")

# One bundle's module: its type, its caching, and — the part nothing else can
# check — that what comes off the wire precompressed actually decompresses to the
# bytes the release recorded. A truncated .br is served with a perfectly correct
# Content-Encoding under a year of `immutable`.
#
# A function because a dist/web release ships two of these and nginx treats them
# identically, so asserting one and assuming the other is how the play bundle
# came to stand in for both.
check_module() {
  local path=$1 want=$2 label=$3 H CT CC CE LEN dec GOT
  H=$(hdr "$path")
  CT=$(field "$H" content-type)
  CC=$(field "$H" cache-control)
  if [ "$CT" = "application/wasm" ]; then
    ok "$label: the module is application/wasm — instantiateStreaming can compile as it downloads"
  else
    bad "$label: the module is '${CT:-absent}', not application/wasm"
  fi
  case "$CC" in
    *immutable*) ok "$label: the bundle is $CC" ;;
    *) bad "$label: the bundle Cache-Control is '${CC:-absent}' — every visit re-downloads 6 MB" ;;
  esac
  for enc in br gzip; do
    if [ "$enc" = br ] && [ "$HAS_BR" = false ]; then
      echo "[NOTE] $label br: released with --no-brotli — gzip only, 580 KiB more per cold visit"
      continue
    fi
    H=$(hdr "$path" "Accept-Encoding: $enc")
    CE=$(field "$H" content-encoding)
    LEN=$(field "$H" content-length)
    if [ "$CE" != "$enc" ]; then
      bad "$label $enc: got '${CE:-none}' — nginx is compressing on every request, or ${enc}_static is off"
      continue
    fi
    case "$enc" in
      br)   dec=$(command -v brotli >/dev/null 2>&1 && echo "brotli -dc") ;;
      gzip) dec="gzip -dc" ;;
    esac
    if [ -z "${dec:-}" ]; then
      ok "$label $enc: served precompressed, $(( ${LEN:-0} / 1024 )) KiB on the wire (no local $enc to verify the body)"
      continue
    fi
    GOT=$(curl -fsS -H "Accept-Encoding: $enc" "$SITE$path" 2>/dev/null | $dec 2>/dev/null | sha256)
    if [ "$GOT" = "$want" ]; then
      ok "$label $enc: $(( ${LEN:-0} / 1024 )) KiB on the wire, and it decompresses to the module that was built"
    else
      bad "$label $enc: the body does not decompress to the released wasm — truncated or corrupt precompressed file"
    fi
  done
}

check_module "$WASM" "$WANT" /play

# /chop, when the artifact has one. Same nginx, same locations, same claims.
if [ -n "$PKG_CHOP" ]; then
  WANT_CHOP=$(sed -n 's/.*"wasm_chop_sha256": "\(.*\)".*/\1/p' "$ART/version.json")
  check_module "/$PKG_CHOP/vidiotic_chop_bg.wasm" "$WANT_CHOP" /chop
  CC=$(field "$(hdr /chop.html)" cache-control)
  case "$CC" in
    *no-cache*) ok "chop.html is $CC — a new build is picked up on reload" ;;
    *) bad "chop.html Cache-Control is '${CC:-absent}', so a stale page can pin an old bundle forever" ;;
  esac
fi

# Compression for the two small files as well: nginx's default gzip_types is
# text/html only and gzip is off entirely in several stock configs, so these are
# uncompressed unless the release precompressed them.
for path in "$GLUE" /index.html; do
  CE=$(field "$(hdr "$path" 'Accept-Encoding: gzip, br')" content-encoding)
  [ -n "$CE" ] && ok "$path is served $CE" \
               || bad "$path is uncompressed — ~88 KiB of avoidable transfer per cold visit"
done

# Also assert what the release *claimed*: a version.json saying brotli:true with
# no .br on the wire is the artifact and its manifest disagreeing.
if [ "$HAS_BR" = true ] && [ -z "$(field "$(hdr "$WASM" 'Accept-Encoding: br')" content-encoding)" ]; then
  bad "version.json claims brotli but nothing serves a .br"
fi

# The server's own headers, on the paths whose locations set a Cache-Control.
# nginx's add_header replaces the inherited set rather than merging with it, so
# this is the check that the shared vidiotic-headers snippet is doing its job.
# Without it these are silently absent on exactly the paths that matter.
for path in /index.html "$WASM" "$GLUE" /version.json; do
  H=$(hdr "$path")
  miss=
  for h in x-content-type-options x-frame-options strict-transport-security; do
    [ -n "$(field "$H" "$h")" ] || miss="$miss $h"
  done
  [ -z "$miss" ] && ok "$path keeps the server's own headers" \
                 || bad "$path lost:$miss — a location's add_header discards the inherited set"
done

# No -f here: it makes curl exit nonzero on a 4xx, which is the thing being
# asked about rather than a failure to ask.
for path in /nginx.conf /_headers; do
  code=$(curl -sS -o /dev/null -w '%{http_code}' "$SITE$path" 2>/dev/null)
  if [ "$code" = "404" ]; then ok "$path is not served"
  else bad "$path returned $code — deploy metadata is public"; fi
done

code=$(curl -ksS -o /dev/null -w '%{http_code}' "https://localhost:$TLS_PORT$BASE/index.html" 2>/dev/null)
[ "$code" = "200" ] && ok "the same config serves over TLS (self-signed, so a browser will still refuse it)" \
                    || bad "TLS returned $code"

echo
if [ "$fails" = 0 ]; then
  echo "nginx serves the artifact correctly${BASE:+ at $BASE/}."
else
  echo "$fails problem(s) — fix them here rather than on the server."
fi

if [ "$CHECK" = 1 ]; then
  stop; rm -f "$CONF"
  exit "$fails"
fi

echo
echo "running. open $SITE/  —  or drive it:"
echo "  node scripts/play-smoke.mjs --url $SITE"
echo "stop with: scripts/serve-play.sh --stop"
exit "$fails"
