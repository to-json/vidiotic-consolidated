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

OUT=dist/web
[ -d "$OUT" ] || OUT=dist/play

if [ -z "$TARGET" ]; then
  cat >&2 <<'USAGE'
no target. Pass one, or set VIDIOTIC_PLAY_TARGET:

  scripts/deploy-play.sh me@box:/srv/www/play
  export VIDIOTIC_PLAY_TARGET=me@box:/srv/www/play

The target directory is what nginx serves as the site root; dist/web/nginx.conf
is generated to be included from that server block.
USAGE
  exit 2
fi

[ -f "$OUT/index.html" ] || { echo "no $OUT — run scripts/release-web.sh" >&2; exit 2; }

# Every bundle directory in this artifact. The dist/web layout ships two — one
# per page (pkg-play-<hash>, pkg-chop-<hash>) — and the legacy dist/play layout
# ships one (pkg-<hash>). All of them are live.
#
# This used to be a single `find … | head -1`, which predates the two-bundle
# release: --prune then kept whichever bundle the filesystem happened to list
# first and deleted the *other* live page's bundle off the server.
PKGS=()
while IFS= read -r dir; do
  PKGS+=("$(basename "$dir")")
done < <(find "$OUT" -maxdepth 1 -type d -name 'pkg-*' | sort)
[ "${#PKGS[@]}" -gt 0 ] || { echo "no bundle directory in $OUT — rebuild it" >&2; exit 2; }

# Every boot file must carry a real build stamp, not just the first one found.
#
# `find … | head -1` checked one of them and passed the other through unlooked
# at, which is the wrong half of the two-bundle release exactly as often as not:
# an unstamped /chop next to a stamped /play deployed clean, and the page that
# went out said `build dev` in its console with no version to correlate a bug
# report against.
BOOT_FILES=$(find "$OUT" \( -name 'boot.js' -o -name 'chop.js' \))
[ -n "$BOOT_FILES" ] || { echo "$OUT has no boot files — run scripts/release-web.sh" >&2; exit 2; }
while IFS= read -r boot; do
  if grep -q "const BUILD = 'dev';" "$boot"; then
    echo "$boot is unstamped — run scripts/release-web.sh" >&2
    exit 2
  fi
done <<< "$BOOT_FILES"
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
  ssh -o BatchMode=yes "$host" "test -d \"$path\"" 2>/dev/null \
    && echo "    target dir:    exists" \
    || echo "    target dir:    absent — --go will mkdir -p it"
  ssh -o BatchMode=yes "$host" 'sh -s' <<'PROBE' || echo "    (could not reach it — check by hand before the first restart)"
  if command -v nginx >/dev/null 2>&1; then
    echo "    nginx:         $(nginx -v 2>&1 | sed 's/.*nginx\///')"
    mod() {
      local name=$1 mod=$2
      if nginx -V 2>&1 | grep -q -- "$mod"; then echo "    $name:        built-in"
      elif nginx -V 2>&1 | grep -q -- --with-compat && find /etc/nginx /usr/lib/nginx -name "*.so" 2>/dev/null | grep -q "$name"; then
        echo "    $name:        dynamic module found on disk (check nginx.conf includes it)"
      else echo "    $name:        ABSENT — comment out $name_static in dist/play/nginx.conf before restart"; fi
    }
    mod gzip gzip_static_module
    mod brotli ngx_brotli
  else
    echo "    nginx:         not in PATH"
  fi
PROBE
}

probe_nginx
echo

# Upload in two phases:
#   1. everything except the HTML pages (so new bundles land safely alongside existing ones)
#   2. index.html & chop.html (site flips to the new release)

if [ "$GO" = 1 ]; then
  case "$TARGET" in
    *:*) ssh -o BatchMode=yes "${TARGET%%:*}" "mkdir -p \"${TARGET#*:}\"" || exit 1 ;;
    *) mkdir -p "$TARGET" || exit 1 ;;
  esac
fi

TARGET_READY=1
if [ "$GO" = 0 ]; then
  case "$TARGET" in
    *:*) ssh -o BatchMode=yes "${TARGET%%:*}" "test -d \"${TARGET#*:}\"" 2>/dev/null || TARGET_READY=0 ;;
    *) [ -d "$TARGET" ] || TARGET_READY=0 ;;
  esac
fi

DRY=""
[ "$GO" = 0 ] && DRY="-n"

echo "--- 1/2  bundles and config (no HTML pages, so the live site does not move yet)"
# shellcheck disable=SC2086
rsync $DRY -rlptv --checksum --exclude='index.html*' --exclude='chop.html*' "$OUT/" "$TARGET/" || exit 1

echo
echo "--- 2/2  HTML entrypoints (the site flips to $BUILD here)"
if [ "$TARGET_READY" = 0 ]; then
  echo "    skipped: $TARGET does not exist yet, and a dry run cannot create it."
  echo "    --go creates it first, and this phase will run."
else
  # shellcheck disable=SC2086
  rsync $DRY -rlptv --checksum "$OUT/"index*.html* "$OUT/"chop*.html* "$TARGET/" || exit 1
fi

if [ "$PRUNE" = 1 ]; then
  echo
  echo "--- prune: bundle directories other than ${PKGS[*]}"
  # Done over ssh rather than with rsync --delete, because --delete would also
  # remove anything else the server keeps in that directory, and because the
  # thing being deleted is 6 MB of somebody's possibly-still-loading page.
  #
  # Exclude every bundle this artifact ships, not just one of them.
  KEEP_ARGS=()
  KEEP_EXPR=""
  for pkg in "${PKGS[@]}"; do
    KEEP_ARGS+=(! -name "$pkg")
    KEEP_EXPR="$KEEP_EXPR ! -name '$pkg'"
  done
  case "$TARGET" in
    *:*)
      host=${TARGET%%:*}
      path=${TARGET#*:}
      cmd="find '$path' -maxdepth 1 -type d -name 'pkg-*'$KEEP_EXPR -print"
      [ "$GO" = 1 ] && cmd="$cmd -exec rm -rf {} +"
      ssh "$host" "$cmd" || exit 1
      ;;
    *)
      find "$TARGET" -maxdepth 1 -type d -name 'pkg-*' "${KEEP_ARGS[@]}" -print \
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
