#!/usr/bin/env bash
# release-play — assemble a deployable /play from the built bundle.
#
# `scripts/build-play.sh` produces something that *runs* over localhost. This
# produces something that survives being on the internet, which is a different
# set of problems:
#
#   1. The wasm is ~6.3 MB. Served without cache headers, every visit pays that
#      again, and a VJ tool that takes ten seconds to open in a venue with bad
#      wifi is not a tool. So the bundle goes in a directory named after a hash
#      of its own contents and is declared immutable; index.html points at that
#      directory and is declared no-cache. A new build gets a new directory
#      name, so there is no cache to bust and no way to serve a stale pairing
#      of glue and module.
#
#      The whole *directory* is hashed rather than the files inside it, because
#      wasm-bindgen's JS resolves the .wasm against its own import.meta.url:
#      move them together and the reference stays correct without editing
#      generated code.
#
#   2. You cannot tell what is deployed by looking at it. So the build stamp is
#      substituted into the page, logged to the console on load, and written to
#      version.json next to it.
#
#   3. Static hosts disagree about everything. `_headers` covers Netlify and
#      Cloudflare Pages; `.nojekyll` covers GitHub Pages; the generated
#      nginx.conf covers a VPS. All read the same directory.
#
# Usage:  scripts/release-play.sh [--no-build] [--serve] [--base PATH] [--no-brotli]
#   --no-build    use whatever is already in web/pkg/ (for iterating on this script)
#   --serve       serve dist/play on http://localhost:8081 when done
#   --base PATH   URL prefix the site is served under (default /). See below.
#   --no-brotli   omit brotli_static from the generated nginx.conf, for a server
#                 whose nginx has no ngx_brotli — the directive is not optional
#                 to nginx, it refuses to start on one it does not know.
#
# **--base matters and is easy to get wrong.** nginx `location` patterns are
# absolute request paths, not relative to anything. A site deployed at
# https://box/play/ with a config full of `location /pkg-abc/` matches *nothing*:
# every request falls through to the catch-all, the page still boots (its own
# imports are relative), and you lose caching, precompression, the MIME override
# and the 404s on deploy metadata — silently, with everything returning 200.
# Measured: 3x the download and no Cache-Control anywhere. So pass the prefix:
#
#   scripts/release-play.sh --base /play
#
# and put the site at <document root>/play/ so the paths line up. Do not wrap
# the include in a `location /play/` block — nginx rejects a nested location
# whose prefix does not match its parent ("location ... is outside location").
#
# Output: dist/play/ — self-contained, relative-path, no server-side anything.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

if ! grep -q '^\[workspace\]' Cargo.toml 2>/dev/null; then
  echo "not at the workspace root (cwd: $PWD)" >&2
  exit 2
fi

BUILD=1
SERVE=0
BROTLI=1
BASE=""
want_base=0
for arg in "$@"; do
  if [ "$want_base" = 1 ]; then BASE=$arg; want_base=0; continue; fi
  case "$arg" in
    --no-build) BUILD=0 ;;
    --serve) SERVE=1 ;;
    --no-brotli) BROTLI=0 ;;
    --base) want_base=1 ;;
    --base=*) BASE=${arg#*=} ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done
[ "$want_base" = 0 ] || { echo "--base needs a path, e.g. --base /play" >&2; exit 2; }

# Normalise to either "" (site is the document root) or "/play" — no trailing
# slash, one leading slash — so every interpolation below can just append.
BASE=${BASE%/}
case "$BASE" in
  ""|"/") BASE="" ;;
  /*) ;;
  *) BASE="/$BASE" ;;
esac

SRC=web
OUT=dist/play
TMP=$OUT.tmp

# With an argument, hashes that file; with none, hashes stdin.
sha256() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$@" | awk '{print $1}'
  else sha256sum "$@" | awk '{print $1}'; fi
}

# The commit of the crate that produced the wasm, not of this repo — the
# workspace root tracks only manifests and scripts, so its hash says nothing
# about what is in the module. `-dirty` is not a warning, it is a fact that
# will otherwise be forgotten by the time the build is on a projector.
stamp_of() {
  local dir=$1 rev
  rev=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null) || { echo "unknown"; return; }
  [ -n "$(git -C "$dir" status --porcelain 2>/dev/null)" ] && rev="$rev-dirty"
  echo "$rev"
}

die() { echo "release-play: $*" >&2; rm -rf "$TMP"; exit 1; }

if [ "$BUILD" = 1 ]; then
  bash scripts/build-play.sh || exit 1
  echo
fi

WASM=$SRC/pkg/vidiotic_play_bg.wasm
GLUE=$SRC/pkg/vidiotic_play.js
BOOT=$SRC/boot.js
for f in "$WASM" "$GLUE" "$BOOT" "$SRC/index.html"; do
  [ -f "$f" ] || { echo "missing $f — run scripts/build-play.sh" >&2; exit 1; }
done

# All three, in a fixed order, so the name changes if any of them does. Hashing
# only the wasm would let a wasm-bindgen upgrade ship new glue under an old
# immutable URL, which is exactly the failure the hash exists to prevent; boot.js
# joined the set when it stopped being inline, and for the same reason — it holds
# the import list, so a stale copy against new glue is a page that 404s on a
# symbol.
HASH=$( { sha256 "$WASM"; sha256 "$GLUE"; sha256 "$BOOT"; } | sha256 | cut -c1-12 )
PKG="pkg-$HASH"

PLAY_REV=$(stamp_of vidiotic-play)
ROOT_REV=$(stamp_of .)
DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
STAMP="$PLAY_REV $DATE"

# Assembled under a temporary name and moved into place at the very end. The
# previous dist/play is a known-good artifact somebody may be about to deploy;
# a release that fails halfway should cost a rebuild, not that.
rm -rf "$TMP" "$OUT.old"
mkdir -p "$TMP/$PKG" || die "cannot create $TMP"
cp "$WASM" "$GLUE" "$TMP/$PKG/" || die "cannot copy the bundle"

# --- the page and its boot script -------------------------------------------
#
# Three exact-string substitutions, each asserted. A silent no-match here ships a
# page that 404s on its own module, and the only symptom is a blank screen.
#
# The two files move in opposite directions, which is the whole point of putting
# boot.js inside the bundle:
#
#   index.html  ./boot.js                -> ./pkg-<hash>/boot.js
#   boot.js     ./pkg/vidiotic_play.js   -> ./vidiotic_play.js
#
# In web/ the page and the boot script are siblings and the bundle is a
# subdirectory. In the artifact the boot script has moved *into* the bundle, so
# it reaches the glue as a sibling and the page reaches it through the hash.
# Served either way, no file refers to anything that is not where it says.
python3 - "$SRC/index.html" "$TMP/index.html" "$PKG" <<'PY' || die "the page could not be stamped"
import sys
src, dst, pkg = sys.argv[1:4]
html = open(src, encoding='utf-8').read()

old, new = '"./boot.js"', f'"./{pkg}/boot.js"'
if html.count(old) != 1:
    sys.exit(f"expected exactly one {old!r} in {src}, found {html.count(old)}")
open(dst, 'w', encoding='utf-8').write(html.replace(old, new))
PY

python3 - "$BOOT" "$TMP/$PKG/boot.js" "$STAMP" <<'PY' || die "boot.js could not be stamped"
import sys
src, dst, stamp = sys.argv[1:4]
js = open(src, encoding='utf-8').read()

subs = [
    ("'./pkg/vidiotic_play.js'", "'./vidiotic_play.js'"),
    ("const BUILD = 'dev';",     f"const BUILD = {stamp!r};"),
]
for old, new in subs:
    if js.count(old) != 1:
        sys.exit(f"expected exactly one {old!r} in {src}, found {js.count(old)}")
    js = js.replace(old, new)

open(dst, 'w', encoding='utf-8').write(js)
PY

# --- precompression ---------------------------------------------------------
#
# For a host that serves precompressed files off disk (nginx gzip_static /
# brotli_static). Netlify, Cloudflare and GitHub Pages compress on the fly and
# ignore these. The numbers printed at the end are the download the visitor
# actually pays; quoting the uncompressed size overstates it roughly 3x.
#
# Every one of these is round-tripped and compared against the source before it
# is allowed into the artifact. A truncated .br — an interrupted compress, a
# full disk — is served happily by nginx with the right Content-Encoding and the
# wrong bytes, under a year-long `immutable`, and nothing downstream can tell.
# `cmp` is the only thing here that actually knows.
squeeze() {
  local src=$1 dst=$2 name
  name=$(basename "$src")
  if [ "$BROTLI" = 1 ] && command -v brotli >/dev/null 2>&1; then
    brotli -q 11 -f -o "$dst.br.part" "$src" 2>/dev/null \
      && brotli -dc "$dst.br.part" | cmp -s - "$src" \
      && mv "$dst.br.part" "$dst.br" \
      || die "brotli produced something that does not decompress back to $name"
  fi
  rm -f "$dst.br.part"
  gzip -9 -c "$src" >"$dst.gz.part" \
    && gzip -dc "$dst.gz.part" | cmp -s - "$src" \
    && mv "$dst.gz.part" "$dst.gz" \
    || die "gzip produced something that does not decompress back to $name"
  rm -f "$dst.gz.part"
}

squeeze "$WASM" "$TMP/$PKG/vidiotic_play_bg.wasm"
# The glue and the page are ~88 KB together and uncompressed on the wire
# otherwise: nginx's default gzip_types is text/html only, and gzip is off
# entirely in several distribution configs.
squeeze "$GLUE" "$TMP/$PKG/vidiotic_play.js"
# The substituted copy, not $BOOT — the source still says 'dev' and imports
# ./pkg/, and shipping a precompressed body that disagrees with the plain one is
# the exact failure `squeeze` round-trips against everywhere else.
squeeze "$TMP/$PKG/boot.js" "$TMP/$PKG/boot.js"
squeeze "$TMP/index.html" "$TMP/index.html"

HAVE_BR=0
[ -f "$TMP/$PKG/vidiotic_play_bg.wasm.br" ] && HAVE_BR=1

# --- host configuration -----------------------------------------------------

cat >"$TMP/_headers" <<HEADERS || die "cannot write _headers"
# Netlify and Cloudflare Pages read this file; other hosts ignore it. nginx uses
# the generated nginx.conf beside it. Paths here are absolute request paths, so
# they carry the same --base prefix.

# The bundle directory is named after a hash of its contents, so this URL can
# never serve different bytes than it did last time.
$BASE/$PKG/*
  Cache-Control: public, max-age=31536000, immutable

# The page names the bundle, so it is the only thing that has to be fresh.
$BASE/index.html
  Cache-Control: no-cache
$BASE/
  Cache-Control: no-cache
HEADERS

# The same rules for nginx, as a file rather than a paragraph — it is meant to
# be `include`d, so that a redeploy with a new bundle name does not need anyone
# to remember to edit the server config. It is written into the site directory
# and served like everything else, which is harmless (it contains no secrets and
# names only paths that are already public) and means the config that matches
# the deployed bundle is always sitting next to it.
BROTLI_LINE="    brotli_static on;"
[ "$BROTLI" = 1 ] || BROTLI_LINE="    # brotli_static on;   # omitted: released with --no-brotli"

cat >"$TMP/nginx.conf" <<NGINX || die "cannot write nginx.conf"
# vidiotic /play — include this from the server{} block that serves this site.
# Generated by scripts/release-play.sh for bundle $PKG${BASE:+, under $BASE/}.
#
# Every location below is an **absolute request path**, because that is the only
# kind nginx has. This file was generated for a site served at "${BASE:-/}"; if
# that is wrong, nothing here matches, everything still returns 200, and you
# quietly lose caching, precompression, the MIME override and the 404s. Rebuild
# with \`scripts/release-play.sh --base /whatever\`.
#
# Include it at server level, not inside a location — nginx rejects a nested
# location whose prefix does not match its parent.
#
# **Server-level add_header directives do not reach these locations.** nginx's
# add_header replaces the inherited set rather than merging with it, so a
# location that adds Cache-Control silently drops every HSTS / X-Frame-Options /
# nosniff / CSP header for that path, and \`always\` does not change it. Rather
# than make you remember to repeat them, every location below includes
#
#     vidiotic-headers/*.conf
#
# which nginx resolves against its own prefix (usually /etc/nginx). Put your
# add_header lines in one file there and they apply to every path this config
# claims. A glob matching nothing is not an error, so leaving it empty is fine.
#
# There is deliberately no server-level \`types { application/wasm wasm; }\` here.
# A types block does not *add* to nginx's MIME map, it **replaces** it, so one at
# this level would leave index.html and the JS glue as octet-stream and the page
# would not run at all. nginx has carried the wasm mapping in stock mime.types
# since at least 1.21.0 (measured: absent in 1.20, present in 1.21.0); the
# exact-match location below is the scoped override for anything older, where
# replacing the map for one known file is harmless.

# Named after a hash of its own contents, so this URL can never serve different
# bytes than it did last time. A new build is a new directory, not a new version
# of this one.
#
# Matched by pattern rather than by this build's exact name, so the *previous*
# bundle keeps its cache headers and its precompressed copies for as long as it
# is still on disk. deploy-play.sh deliberately leaves it there until the next
# --prune, as a grace period for a tab that is mid-fetch; naming only the
# current bundle would make that grace period serve 3x the bytes uncached.
# Quoted because nginx would otherwise read the regex's braces as config blocks.
location ~ "^$BASE/pkg-[0-9a-f]{12}/" {
    add_header Cache-Control "public, max-age=31536000, immutable";
    include vidiotic-headers/*.conf;
    # release-play.sh emits verified .gz and .br beside each file. Without these
    # nginx compresses a 6 MB module on every request instead of serving the
    # copy already on disk. gzip_static is in most distribution builds; brotli
    # needs ngx_brotli, which usually is not. An nginx missing either refuses to
    # start on the unknown directive — check \`nginx -V\` and the loaded modules,
    # or rebuild the release with --no-brotli.
    gzip_static on;
$BROTLI_LINE
}

# One file, one type: safe to replace the map inside a location this narrow, and
# it is what makes the wasm arrive correctly on nginx older than 1.21. The wrong
# type costs the browser its streaming compile — slower, and silent.
location = $BASE/$PKG/vidiotic_play_bg.wasm {
    types { }
    default_type application/wasm;
    add_header Cache-Control "public, max-age=31536000, immutable";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}

# The page names the bundle, so it is the only thing that has to be fresh.
location = $BASE/index.html {
    add_header Cache-Control "no-cache";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}
location = $BASE/version.json {
    add_header Cache-Control "no-cache";
    include vidiotic-headers/*.conf;
}

# Deploy metadata, not site content. Harmless to leak — they name only paths
# that are already public — but there is no reason to serve them.
location = $BASE/nginx.conf { return 404; }
location = $BASE/_headers   { return 404; }
NGINX

# GitHub Pages runs Jekyll by default, which drops files and directories it
# considers special. Nothing here starts with an underscore except _headers,
# but this costs one empty file and removes a whole class of "works locally".
: >"$TMP/.nojekyll" || die "cannot write .nojekyll"

cat >"$TMP/version.json" <<JSON || die "cannot write version.json"
{
  "build": "$STAMP",
  "date": "$DATE",
  "vidiotic_play": "$PLAY_REV",
  "workspace": "$ROOT_REV",
  "bundle": "$PKG",
  "base": "${BASE:-/}",
  "brotli": $([ "$HAVE_BR" = 1 ] && echo true || echo false),
  "wasm_sha256": "$(sha256 "$WASM")",
  "wasm_bytes": $(wc -c <"$WASM" | tr -d ' ')
}
JSON

# Everything is written and verified — publish atomically enough that a reader
# never sees a half-assembled tree.
if [ -d "$OUT" ]; then mv "$OUT" "$OUT.old" || die "cannot move the previous $OUT aside"; fi
mkdir -p "$(dirname "$OUT")"
mv "$TMP" "$OUT" || { mv "$OUT.old" "$OUT" 2>/dev/null; die "cannot publish $OUT"; }
rm -rf "$OUT.old"

BR=""
[ "$HAVE_BR" = 1 ] && BR=$(wc -c <"$OUT/$PKG/vidiotic_play_bg.wasm.br" | tr -d ' ')
GZ=$(wc -c <"$OUT/$PKG/vidiotic_play_bg.wasm.gz" | tr -d ' ')
RAW=$(wc -c <"$WASM" | tr -d ' ')

kib() { echo "$(( $1 / 1024 )) KiB"; }

echo "assembled $OUT — build $STAMP"
echo "  bundle    $PKG"
echo "  served at ${BASE:-/}"
echo "  wasm      $(kib "$RAW")  ->  gzip $(kib "$GZ")${BR:+  ->  brotli $(kib "$BR")}"
if [ "$HAVE_BR" = 0 ] && [ "$BROTLI" = 1 ]; then
  echo "            (no brotli — 'brew install brotli' for the smaller precompressed copy;"
  echo "             nginx.conf still declares brotli_static, so pass --no-brotli if the"
  echo "             server has no ngx_brotli either)"
fi
echo
echo "deploy: any static host over HTTPS. WebGPU needs a secure context, and"
echo "        there is nothing server-side to run."
echo
echo "  nginx                        include the generated $OUT/nginx.conf"
echo "  netlify / cloudflare pages   _headers is read as-is"
echo "  github pages                 .nojekyll is present; caching falls back to the host default"
echo
echo "rehearse:   scripts/serve-play.sh${BASE:+ --base $BASE}       (this artifact, behind real nginx)"
echo "            node scripts/play-smoke.mjs --url http://localhost:8080$BASE"
echo "then:       scripts/deploy-play.sh             (dry run, and probes the server)"
echo "            scripts/deploy-play.sh --go"

if [ "$SERVE" = 1 ]; then
  echo
  echo "serving http://localhost:8081 (ctrl-c to stop)"
  exec python3 -m http.server -d "$OUT" 8081
fi
