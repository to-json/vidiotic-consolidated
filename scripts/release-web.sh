#!/usr/bin/env bash
# release-web — assemble deployable /play and /chop web artifacts.
#
# Builds and packages both vidiotic-play and vidiotic-chop into dist/web/.
#
# It also used to copy the whole tree to dist/play, for one reader:
# `serve-play.sh`, which only knew that path. The copy did not actually work —
# the rehearsal rig read a two-page artifact as if it were a one-page one and
# degraded silently — so `serve-play.sh` reads dist/web directly now and the copy
# is gone. `release-play.sh` still writes dist/play; nothing else does.
#
# Output: dist/web/ — self-contained, relative-path, no server-side anything.

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
[ "$want_base" = 0 ] || { echo "--base needs a path, e.g. --base /warez/vidiotic" >&2; exit 2; }

BASE=${BASE%/}
case "$BASE" in
  ""|"/") BASE="" ;;
  /*) ;;
  *) BASE="/$BASE" ;;
esac

SRC=web
OUT=dist/web
TMP=$OUT.tmp

sha256() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$@" | awk '{print $1}'
  else sha256sum "$@" | awk '{print $1}'; fi
}

stamp_of() {
  local dir=$1 rev
  rev=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null) || { echo "unknown"; return; }
  [ -n "$(git -C "$dir" status --porcelain 2>/dev/null)" ] && rev="$rev-dirty"
  echo "$rev"
}

die() { echo "release-web: $*" >&2; rm -rf "$TMP"; exit 1; }

if [ "$BUILD" = 1 ]; then
  echo "--- 1/2  building vidiotic-play..."
  bash scripts/build-play.sh || exit 1
  echo
  echo "--- 2/2  building vidiotic-chop..."
  bash scripts/build-chop.sh || exit 1
  echo
fi

# --- verify source files ---------------------------------------------------

WASM_PLAY=$SRC/pkg/vidiotic_play_bg.wasm
GLUE_PLAY=$SRC/pkg/vidiotic_play.js
BOOT_PLAY=$SRC/boot.js

WASM_CHOP=$SRC/pkg-chop/vidiotic_chop_bg.wasm
GLUE_CHOP=$SRC/pkg-chop/vidiotic_chop.js
BOOT_CHOP=$SRC/chop.js

for f in "$WASM_PLAY" "$GLUE_PLAY" "$BOOT_PLAY" "$SRC/index.html" \
         "$WASM_CHOP" "$GLUE_CHOP" "$BOOT_CHOP" "$SRC/chop.html"; do
  [ -f "$f" ] || { echo "missing $f — check web/ directory and build scripts" >&2; exit 1; }
done

# --- hash bundles -----------------------------------------------------------

HASH_PLAY=$( { sha256 "$WASM_PLAY"; sha256 "$GLUE_PLAY"; sha256 "$BOOT_PLAY"; } | sha256 | cut -c1-12 )
PKG_PLAY="pkg-play-$HASH_PLAY"

HASH_CHOP=$( { sha256 "$WASM_CHOP"; sha256 "$GLUE_CHOP"; sha256 "$BOOT_CHOP"; } | sha256 | cut -c1-12 )
PKG_CHOP="pkg-chop-$HASH_CHOP"

PLAY_REV=$(stamp_of vidiotic-play)
CHOP_REV=$(stamp_of vidiotic-chop)
ROOT_REV=$(stamp_of .)
DATE=$(date -u +%Y-%m-%dT%H:%M:%SZ)
STAMP="$PLAY_REV/$CHOP_REV $DATE"

rm -rf "$TMP" "$OUT.old"
mkdir -p "$TMP/$PKG_PLAY" "$TMP/$PKG_CHOP" || die "cannot create $TMP"

# --- assemble /play ---------------------------------------------------------
cp "$WASM_PLAY" "$GLUE_PLAY" "$TMP/$PKG_PLAY/" || die "cannot copy play bundle"

python3 - "$SRC/index.html" "$TMP/index.html" "$PKG_PLAY" <<'PY' || die "index.html could not be stamped"
import sys
src, dst, pkg = sys.argv[1:4]
html = open(src, encoding='utf-8').read()

old, new = '"./boot.js"', f'"./{pkg}/boot.js"'
if html.count(old) != 1:
    sys.exit(f"expected exactly one {old!r} in {src}, found {html.count(old)}")
open(dst, 'w', encoding='utf-8').write(html.replace(old, new))
PY

python3 - "$BOOT_PLAY" "$TMP/$PKG_PLAY/boot.js" "$STAMP" <<'PY' || die "boot.js could not be stamped"
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

# --- assemble /chop ---------------------------------------------------------
cp "$WASM_CHOP" "$GLUE_CHOP" "$TMP/$PKG_CHOP/" || die "cannot copy chop bundle"

python3 - "$SRC/chop.html" "$TMP/chop.html" "$PKG_CHOP" <<'PY' || die "chop.html could not be stamped"
import sys
src, dst, pkg = sys.argv[1:4]
html = open(src, encoding='utf-8').read()

old, new = "'./chop.js'", f"'./{pkg}/chop.js'"
if html.count(old) != 1:
    sys.exit(f"expected exactly one {old!r} in {src}, found {html.count(old)}")
open(dst, 'w', encoding='utf-8').write(html.replace(old, new))
PY

python3 - "$BOOT_CHOP" "$TMP/$PKG_CHOP/chop.js" "$STAMP" <<'PY' || die "chop.js could not be stamped"
import sys
src, dst, stamp = sys.argv[1:4]
js = open(src, encoding='utf-8').read()

subs = [
    ("'./pkg-chop/vidiotic_chop.js'", "'./vidiotic_chop.js'"),
]
for old, new in subs:
    if js.count(old) != 1:
        sys.exit(f"expected exactly one {old!r} in {src}, found {js.count(old)}")
    js = js.replace(old, new)

# prepend stamp header
js = f"// vidiotic /chop — build {stamp}\n" + js
open(dst, 'w', encoding='utf-8').write(js)
PY

# --- precompression ---------------------------------------------------------

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

squeeze "$WASM_PLAY" "$TMP/$PKG_PLAY/vidiotic_play_bg.wasm"
squeeze "$GLUE_PLAY" "$TMP/$PKG_PLAY/vidiotic_play.js"
squeeze "$TMP/$PKG_PLAY/boot.js" "$TMP/$PKG_PLAY/boot.js"
squeeze "$TMP/index.html" "$TMP/index.html"

squeeze "$WASM_CHOP" "$TMP/$PKG_CHOP/vidiotic_chop_bg.wasm"
squeeze "$GLUE_CHOP" "$TMP/$PKG_CHOP/vidiotic_chop.js"
squeeze "$TMP/$PKG_CHOP/chop.js" "$TMP/$PKG_CHOP/chop.js"
squeeze "$TMP/chop.html" "$TMP/chop.html"

HAVE_BR=0
[ -f "$TMP/$PKG_PLAY/vidiotic_play_bg.wasm.br" ] && HAVE_BR=1

# --- host configuration -----------------------------------------------------

cat >"$TMP/_headers" <<HEADERS || die "cannot write _headers"
$BASE/$PKG_PLAY/*
  Cache-Control: public, max-age=31536000, immutable

$BASE/$PKG_CHOP/*
  Cache-Control: public, max-age=31536000, immutable

$BASE/index.html
  Cache-Control: no-cache
$BASE/chop.html
  Cache-Control: no-cache
$BASE/
  Cache-Control: no-cache
HEADERS

BROTLI_LINE="    brotli_static on;"
[ "$BROTLI" = 1 ] || BROTLI_LINE="    # brotli_static on;   # omitted: released with --no-brotli"

cat >"$TMP/nginx.conf" <<NGINX || die "cannot write nginx.conf"
# vidiotic /play and /chop — include this from the server{} block serving this site.
# Generated by scripts/release-web.sh for bundles $PKG_PLAY and $PKG_CHOP${BASE:+, under $BASE/}.

location ~ "^$BASE/pkg-(play|chop)-[0-9a-f]{12}/" {
    add_header Cache-Control "public, max-age=31536000, immutable";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}

location = $BASE/$PKG_PLAY/vidiotic_play_bg.wasm {
    types { }
    default_type application/wasm;
    add_header Cache-Control "public, max-age=31536000, immutable";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}

location = $BASE/$PKG_CHOP/vidiotic_chop_bg.wasm {
    types { }
    default_type application/wasm;
    add_header Cache-Control "public, max-age=31536000, immutable";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}

location = $BASE/index.html {
    add_header Cache-Control "no-cache";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}

location = $BASE/chop.html {
    add_header Cache-Control "no-cache";
    include vidiotic-headers/*.conf;
    gzip_static on;
$BROTLI_LINE
}

location = $BASE/version.json {
    add_header Cache-Control "no-cache";
    include vidiotic-headers/*.conf;
}

location = $BASE/nginx.conf { return 404; }
location = $BASE/_headers   { return 404; }
NGINX

: >"$TMP/.nojekyll" || die "cannot write .nojekyll"

cat >"$TMP/version.json" <<JSON || die "cannot write version.json"
{
  "build": "$STAMP",
  "date": "$DATE",
  "vidiotic_play": "$PLAY_REV",
  "vidiotic_chop": "$CHOP_REV",
  "workspace": "$ROOT_REV",
  "bundle_play": "$PKG_PLAY",
  "bundle_chop": "$PKG_CHOP",
  "base": "${BASE:-/}",
  "brotli": $([ "$HAVE_BR" = 1 ] && echo true || echo false),
  "wasm_play_sha256": "$(sha256 "$WASM_PLAY")",
  "wasm_chop_sha256": "$(sha256 "$WASM_CHOP")"
}
JSON

# Publish to dist/web
if [ -d "$OUT" ]; then mv "$OUT" "$OUT.old" || die "cannot move previous $OUT aside"; fi
mkdir -p "$(dirname "$OUT")"
mv "$TMP" "$OUT" || { mv "$OUT.old" "$OUT" 2>/dev/null; die "cannot publish $OUT"; }
rm -rf "$OUT.old"

echo "assembled $OUT — build $STAMP"
echo "  play bundle:  $PKG_PLAY"
echo "  chop bundle:  $PKG_CHOP"
echo "  base path:    ${BASE:-/}"
echo

if [ "$SERVE" = 1 ]; then
  echo "serving http://localhost:8081 (ctrl-c to stop)"
  exec python3 -m http.server -d "$OUT" 8081
fi
