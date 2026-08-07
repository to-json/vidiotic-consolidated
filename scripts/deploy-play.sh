#!/usr/bin/env bash
# deploy-play — rsync dist/play to a server, in an order that cannot serve a
# broken page to somebody who reloads mid-deploy.
#
# The page names its bundle directory, and the bundle directory is named after a
# hash of its contents. That is what makes caching safe, and it is also what
# makes deploy order matter: push index.html first and, for the length of the
# transfer, the live page points at a 6 MB directory that is not there yet. So:
#
#   1. everything except index.html — the new bundle lands *alongside* the old
#      one, and nothing references it yet, so the site is untouched;
#   2. index.html — one small file, and the site flips to the new bundle at the
#      moment it lands;
#   3. --prune, separately and only when asked — remove the bundle directories
#      nothing points at any more.
#
# Step 3 is separate because a tab that loaded the old page thirty seconds ago
# may still be fetching the old bundle. Sweeping is a thing to do on the next
# deploy, not this one.
#
# Usage:  scripts/deploy-play.sh [--go] [--prune] [target]
#
#   target   an rsync destination, e.g. me@box:/srv/www/play
#            defaults to $VIDIOTIC_PLAY_TARGET
#            Avoid spaces in a remote path: macOS ships openrsync, which has no
#            --protect-args, so the path is handed to the remote shell and
#            re-splits. Nothing here can fix that from this side.
#
# Dry run unless --go. Uploading is the one step here that other people can see,
# and rsync's own -n output is a better description of what is about to happen
# than anything this script could print.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

GO=0
PRUNE=0
TARGET=${VIDIOTIC_PLAY_TARGET:-}
for arg in "$@"; do
  case "$arg" in
    --go) GO=1 ;;
    --prune) PRUNE=1 ;;
    -*) echo "unknown option: $arg" >&2; exit 2 ;;
    *) TARGET=$arg ;;
  esac
done

OUT=dist/play

if [ -z "$TARGET" ]; then
  cat >&2 <<'USAGE'
no target. Pass one, or set VIDIOTIC_PLAY_TARGET:

  scripts/deploy-play.sh me@box:/srv/www/play
  export VIDIOTIC_PLAY_TARGET=me@box:/srv/www/play

The target directory is what nginx serves as the site root; dist/play/nginx.conf
is generated to be included from that server block.
USAGE
  exit 2
fi

[ -f "$OUT/index.html" ] || { echo "no $OUT — run scripts/release-play.sh" >&2; exit 2; }

PKG_DIR=$(find "$OUT" -maxdepth 1 -type d -name 'pkg-*' | head -1)
[ -n "$PKG_DIR" ] || { echo "no bundle directory in $OUT — rebuild it" >&2; exit 2; }
PKG=$(basename "$PKG_DIR")

# A stamp still reading 'dev' means release-play.sh did not produce this tree,
# which means the caching and identity this script relies on are not there.
#
# The stamp lives in boot.js, inside the bundle — it moved there when the boot
# script stopped being inline so that a CSP without 'unsafe-inline' would run
# it. Checking index.html for it, as this did, would now always pass and catch
# nothing.
if [ ! -f "$PKG_DIR/boot.js" ] || grep -q "const BUILD = 'dev';" "$PKG_DIR/boot.js"; then
  echo "$PKG_DIR/boot.js is missing or unstamped — this looks like a copy of web/, not a release" >&2
  exit 2
fi
BUILD=$(sed -n 's/.*"build": "\(.*\)".*/\1/p' "$OUT/version.json")

# Ask the server what its nginx can do, before anything is uploaded.
#
# The generated site config turns on gzip_static and brotli_static, and an nginx
# built without either refuses to start on the unknown directive. A reload
# survives that — the old config keeps serving — but a restart does not, and
# finding out during a restart is finding out with the site down. This is the
# one question that cannot be answered from here, so it gets asked over ssh
# during the dry run, which is read-only and happens before any decision.
probe_nginx() {
  case "$TARGET" in *:*) ;; *) return ;; esac
  local host=${TARGET%%:*} path=${TARGET#*:}
  echo "--- probing $host (read-only)"
  # The directory is asked about first because its absence is what makes a dry
  # run look broken when the real deploy would work: rsync -n never creates it,
  # so phase 2 has nowhere to put index.html. --go creates it; this says so.
  ssh -o BatchMode=yes "$host" "test -d \"$path\"" 2>/dev/null \
    && echo "    target dir:    exists" \
    || echo "    target dir:    absent — --go will mkdir -p it"
  ssh -o BatchMode=yes "$host" 'sh -s' <<'PROBE' || echo "    (could not reach it — check by hand before the first restart)"
if ! command -v nginx >/dev/null 2>&1; then
  echo "    nginx: not on PATH — check by hand"
  exit 0
fi
nginx -v 2>&1 | sed 's/^/    /'
if nginx -V 2>&1 | grep -q -- --with-http_gzip_static_module; then
  echo "    gzip_static:   available"
else
  echo "    gzip_static:   MISSING — comment it out of nginx.conf or nginx will not start"
fi
if { nginx -T 2>/dev/null; cat /etc/nginx/nginx.conf /etc/nginx/modules/*.conf 2>/dev/null; } \
     | grep -q ngx_http_brotli_static_module; then
  echo "    brotli_static: loaded"
elif ls /usr/lib/nginx/modules/ngx_http_brotli_static_module.so \
        /etc/nginx/modules/ngx_http_brotli_static_module.so >/dev/null 2>&1; then
  echo "    brotli_static: module on disk but not loaded — add load_module, or comment the directive out"
else
  echo "    brotli_static: MISSING — comment it out of nginx.conf (costs 580 KiB per cold visit)"
fi
PROBE
  echo
}

# A plain string rather than an array: macOS still ships bash 3.2, where
# expanding an empty array under `set -u` is itself an error.
DRY=-n
if [ "$GO" = 1 ]; then DRY=; else echo "DRY RUN — nothing is transferred. Add --go to do it."; fi
echo "deploying build $BUILD ($PKG) to $TARGET"
echo
[ "$GO" = 1 ] || probe_nginx

# rsync creates at most one missing directory level and never creates any under
# -n, so a first deploy into a path that does not exist fails in phase 2 with
# "No such file or directory" — and fails in the *dry run* while --go would have
# worked, which is the most misleading order to discover it in.
TARGET_READY=1
if [ "$GO" = 1 ]; then
  case "$TARGET" in
    *:*) ssh -o BatchMode=yes "${TARGET%%:*}" "mkdir -p \"${TARGET#*:}\"" \
           || { echo "cannot create ${TARGET#*:} on ${TARGET%%:*}" >&2; exit 1; } ;;
    *) mkdir -p "$TARGET" || exit 1 ;;
  esac
else
  case "$TARGET" in
    *:*) ssh -o BatchMode=yes "${TARGET%%:*}" "test -d \"${TARGET#*:}\"" 2>/dev/null || TARGET_READY=0 ;;
    *) [ -d "$TARGET" ] || TARGET_READY=0 ;;
  esac
fi

# Trailing slash on the source: copy the *contents* of dist/play, not the
# directory itself. Without it everything lands one level too deep and the site
# is a 404 that looks like a DNS problem.
echo "--- 1/2  bundle and config (no index.html, so the live page does not move yet)"
# shellcheck disable=SC2086
rsync $DRY -rlptv --checksum --exclude=index.html "$OUT/" "$TARGET/" || exit 1

echo
echo "--- 2/2  index.html (the site flips to $PKG here)"
if [ "$TARGET_READY" = 0 ]; then
  echo "    skipped: $TARGET does not exist yet, and a dry run cannot create it."
  echo "    --go creates it first, and this phase will run."
else
  # shellcheck disable=SC2086
  rsync $DRY -rlptv --checksum "$OUT/index.html" "$TARGET/index.html" || exit 1
fi

if [ "$PRUNE" = 1 ]; then
  echo
  echo "--- prune: bundle directories other than $PKG"
  # Done over ssh rather than with rsync --delete, because --delete would also
  # remove anything else the server keeps in that directory, and because the
  # thing being deleted is 6 MB of somebody's possibly-still-loading page.
  case "$TARGET" in
    *:*)
      host=${TARGET%%:*}
      path=${TARGET#*:}
      cmd="find '$path' -maxdepth 1 -type d -name 'pkg-*' ! -name '$PKG' -print"
      [ "$GO" = 1 ] && cmd="$cmd -exec rm -rf {} +"
      ssh "$host" "$cmd" || exit 1
      ;;
    *)
      find "$TARGET" -maxdepth 1 -type d -name 'pkg-*' ! -name "$PKG" -print \
        $([ "$GO" = 1 ] && echo "-exec rm -rf {} +") || exit 1
      ;;
  esac
fi

echo
if [ "$GO" = 1 ]; then
  echo "deployed $BUILD."
  echo
  echo "on the server, once:  include this artifact's nginx.conf from the server"
  echo "block whose root is $TARGET, then \`nginx -t && nginx -s reload\`."
  echo "docker/play/nginx.conf is a working example of that block."
  echo
  echo "then check the browser console reads 'vidiotic /play — build $BUILD',"
  echo "and that the page is on https:// — WebGPU is not exposed otherwise."
else
  echo "dry run only. Re-run with --go to transfer."
fi
