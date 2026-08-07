#!/bin/bash
#
# Assemble Vidiotic.app — one double-clickable bundle containing all three
# front ends. Prep and Ctl are *nested helper apps*, so each gets its own name,
# icon, and menu bar when it runs (the Xcode/Chrome pattern):
#
#   Vidiotic.app/Contents/
#     MacOS/vidiotic
#     Info.plist                     .viproj doc type, camera/mic usage strings
#     Frameworks/lib*.dylib          ffmpeg & co, relocated off /opt/homebrew
#     Resources/shaders/             seeded into ~/Library/Application Support
#     Library/Vidiotic Prep.app/Contents/MacOS/vidiotic-prep
#     Library/Vidiotic Ctl.app/Contents/MacOS/vidiotic-ctl
#
# The helpers reach the *outer* Frameworks dir via an LC_RPATH four levels up,
# so the ~100 MB of ffmpeg is paid for once.
#
# Usage:
#   ./packaging/bundle.sh                      release build, ad-hoc signed
#   ./packaging/bundle.sh --debug              debug build (faster iteration)
#   ./packaging/bundle.sh --sign "Developer ID Application: Name (TEAMID)"
#   ./packaging/bundle.sh --sign "..." --notarize <keychain-profile>
#   ./packaging/bundle.sh --dmg                also roll a dist/Vidiotic.dmg
#   ./packaging/bundle.sh --install            also copy to /Applications
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="$ROOT/packaging"
DIST="$ROOT/dist"
APP="$DIST/Vidiotic.app"

PROFILE=release
IDENTITY="-"            # ad-hoc; override with --sign
NOTARY_PROFILE=""
MAKE_DMG=0
INSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --debug)     PROFILE=debug ;;
    --release)   PROFILE=release ;;
    --sign)      IDENTITY="$2"; shift ;;
    --notarize)  NOTARY_PROFILE="$2"; shift ;;
    --dmg)       MAKE_DMG=1 ;;
    --install)   INSTALL=1 ;;
    -h|--help)   sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

VERSION="$(awk -F'"' '/^version =/ {print $2; exit}' "$ROOT/vidiotic/Cargo.toml")"
say() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }

# ---------------------------------------------------------------- build

say "building ($PROFILE)"
# `cargo` is not necessarily on PATH (rustup's shims can be missing while the
# toolchains are fine). Word-splitting on $CARGO below is deliberate — the
# rustup fallback is three words.
if [ -z "${CARGO:-}" ]; then
  if command -v cargo >/dev/null 2>&1; then
    CARGO=cargo
  elif command -v rustup >/dev/null 2>&1; then
    CARGO="rustup run stable cargo"
  else
    echo "no cargo and no rustup on PATH; set \$CARGO to a cargo binary" >&2
    exit 1
  fi
fi
# Bare word, not an array: /bin/bash on macOS is 3.2, where expanding an empty
# array under `set -u` is an error.
RELEASE_FLAG=""
[ "$PROFILE" = release ] && RELEASE_FLAG="--release"
# One invocation per crate. `--bin` is a *global* target filter in cargo, not a
# per-`-p` one: `-p vidiotic --bin vidiotic -p vidiotic-prep` silently builds no
# prep binary at all, because prep has no target named `vidiotic`. The filter is
# needed on the player to skip its four `spike_*` bins.
(cd "$ROOT" && $CARGO build $RELEASE_FLAG -p vidiotic --bin vidiotic)
(cd "$ROOT" && $CARGO build $RELEASE_FLAG -p vidiotic-prep)
(cd "$ROOT" && $CARGO build $RELEASE_FLAG -p vidiotic-ctl --bin vidiotic-ctl)

BIN="$ROOT/target/$PROFILE"
for b in vidiotic vidiotic-prep vidiotic-ctl; do
  [ -x "$BIN/$b" ] || { echo "missing binary: $BIN/$b" >&2; exit 1; }
done

# ---------------------------------------------------------------- layout

say "laying out $APP"
rm -rf "$APP"
FW="$APP/Contents/Frameworks"
RES="$APP/Contents/Resources"
LIB="$APP/Contents/Library"
mkdir -p "$APP/Contents/MacOS" "$FW" "$RES" "$LIB"

cp "$BIN/vidiotic" "$APP/Contents/MacOS/vidiotic"

# A helper: nested .app so it carries its own identity in the Dock and menu bar.
# $1 = app name, $2 = binary, $3 = plist template
helper() {
  local dir="$LIB/$1.app"
  mkdir -p "$dir/Contents/MacOS" "$dir/Contents/Resources"
  cp "$BIN/$2" "$dir/Contents/MacOS/$2"
  sed -e "s/@VERSION@/$VERSION/g" "$PKG/$3" > "$dir/Contents/Info.plist"
  echo -n 'APPL????' > "$dir/Contents/PkgInfo"
}
helper "Vidiotic Prep" vidiotic-prep VidioticPrep.Info.plist
helper "Vidiotic Ctl"  vidiotic-ctl  VidioticCtl.Info.plist

sed -e "s/@VERSION@/$VERSION/g" "$PKG/Vidiotic.Info.plist" > "$APP/Contents/Info.plist"
echo -n 'APPL????' > "$APP/Contents/PkgInfo"

# Shipped shader library. The app copies this into
# ~/Library/Application Support/Vidiotic/shaders on first run and livecodes
# against *that* — writing inside a signed bundle would break its seal.
#
# Under vidiotic-play, not vidiotic: the directory moved with the render core
# that `include_str!`s the built-in effects out of it (web-port.md §8 step 4).
# A stale path here fails silently — the bundle just ships without shaders — so
# copy it explicitly rather than globbing, and fail the build if it is missing.
[ -d "$ROOT/vidiotic-play/shaders" ] || { echo "no shaders/ at vidiotic-play — did the crate layout change?" >&2; exit 1; }
cp -R "$ROOT/vidiotic-play/shaders" "$RES/shaders"
[ -d "$ROOT/vidiotic/licenses" ] && cp -R "$ROOT/vidiotic/licenses" "$RES/licenses"

for icns in Vidiotic VidioticPrep VidioticCtl; do
  [ -f "$PKG/$icns.icns" ] || continue
  case "$icns" in
    Vidiotic)     cp "$PKG/$icns.icns" "$RES/Vidiotic.icns" ;;
    VidioticPrep) cp "$PKG/$icns.icns" "$LIB/Vidiotic Prep.app/Contents/Resources/VidioticPrep.icns" ;;
    VidioticCtl)  cp "$PKG/$icns.icns" "$LIB/Vidiotic Ctl.app/Contents/Resources/VidioticCtl.icns" ;;
  esac
done

# ---------------------------------------------------------------- dylibs

# Walk the link closure and pull every non-system dylib into Frameworks. The
# filesystem is the memo table: a lib already copied is a lib already walked.
collect() {
  local macho="$1" dep base
  otool -L "$macho" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    case "$dep" in /usr/lib/*|/System/*|@*) continue ;; esac
    base="$(basename "$dep")"
    [ -f "$FW/$base" ] && continue
    [ -f "$dep" ] || { echo "  ! missing dependency $dep" >&2; continue; }
    cp -L "$dep" "$FW/$base"
    chmod u+w "$FW/$base"
    collect "$FW/$base"
  done
}

say "collecting dylibs"
collect "$APP/Contents/MacOS/vidiotic"
collect "$LIB/Vidiotic Prep.app/Contents/MacOS/vidiotic-prep"
collect "$LIB/Vidiotic Ctl.app/Contents/MacOS/vidiotic-ctl"
printf '    %s dylibs, %s\n' "$(ls "$FW" | wc -l | tr -d ' ')" "$(du -sh "$FW" | cut -f1)"

say "rewriting install names"
# Every collected lib refers to its siblings by @rpath/<basename>.
for f in "$FW"/*.dylib; do
  install_name_tool -id "@rpath/$(basename "$f")" "$f" 2>/dev/null
  otool -L "$f" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    case "$dep" in /usr/lib/*|/System/*|@*) continue ;; esac
    install_name_tool -change "$dep" "@rpath/$(basename "$dep")" "$f" 2>/dev/null
  done
done

# $1 = executable, $2 = relative hop from its dir to Contents/Frameworks
relink() {
  local exe="$1" hop="$2" dep
  otool -L "$exe" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    case "$dep" in /usr/lib/*|/System/*|@*) continue ;; esac
    install_name_tool -change "$dep" "@rpath/$(basename "$dep")" "$exe"
  done
  install_name_tool -add_rpath "$hop" "$exe" 2>/dev/null || true
}
relink "$APP/Contents/MacOS/vidiotic" "@executable_path/../Frameworks"
# Library/<Helper>.app/Contents/MacOS -> ../../../../Frameworks
relink "$LIB/Vidiotic Prep.app/Contents/MacOS/vidiotic-prep" "@executable_path/../../../../Frameworks"
relink "$LIB/Vidiotic Ctl.app/Contents/MacOS/vidiotic-ctl"   "@executable_path/../../../../Frameworks"

# ---------------------------------------------------------------- sign

# Inside-out, as codesign requires: dylibs, then the nested apps, then the
# outer one. `--deep` is deprecated and does the wrong thing with helpers.
#
# `vidiotic.entitlements` deliberately carries no XML comments: AMFI's plist
# parser rejects them ("AMFIUnserializeXML: syntax error"), unlike the Info
# plists next to it. Rationale for each key lives here, not in that file:
#   device.camera / device.audio-input  — TCC-gated capture, per Info.plist
#   cs.disable-library-validation       — the helper apps load the outer
#                                         bundle's ffmpeg dylibs across a
#                                         bundle boundary
say "signing (identity: $IDENTITY)"
SIGN=(codesign --force --timestamp --options runtime -s "$IDENTITY")
[ "$IDENTITY" = "-" ] && SIGN=(codesign --force -s -)   # ad-hoc: no timestamp/hardened runtime

for f in "$FW"/*.dylib; do "${SIGN[@]}" "$f" >/dev/null 2>&1; done
"${SIGN[@]}" --entitlements "$PKG/vidiotic.entitlements" \
  "$LIB/Vidiotic Prep.app" >/dev/null
"${SIGN[@]}" --entitlements "$PKG/vidiotic.entitlements" \
  "$LIB/Vidiotic Ctl.app" >/dev/null
"${SIGN[@]}" --entitlements "$PKG/vidiotic.entitlements" "$APP" >/dev/null

codesign --verify --verbose=2 "$APP" 2>&1 | sed 's/^/    /'

# ---------------------------------------------------------------- notarize

if [ -n "$NOTARY_PROFILE" ]; then
  if [ "$IDENTITY" = "-" ]; then
    echo "refusing to notarize an ad-hoc signed app: pass --sign 'Developer ID Application: ...'" >&2
    exit 1
  fi
  say "notarizing via keychain profile '$NOTARY_PROFILE'"
  ZIP="$DIST/Vidiotic-notarize.zip"
  ditto -c -k --keepParent "$APP" "$ZIP"
  xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  rm -f "$ZIP"
  spctl --assess --type execute --verbose=2 "$APP" 2>&1 | sed 's/^/    /'
fi

# ---------------------------------------------------------------- deliver

if [ "$MAKE_DMG" = 1 ]; then
  say "building dmg"
  STAGE="$DIST/dmg"
  rm -rf "$STAGE" "$DIST/Vidiotic.dmg"
  mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname Vidiotic -srcfolder "$STAGE" -ov -format UDZO "$DIST/Vidiotic.dmg" >/dev/null
  rm -rf "$STAGE"
  echo "    $DIST/Vidiotic.dmg"
fi

if [ "$INSTALL" = 1 ]; then
  say "installing to /Applications"
  rm -rf "/Applications/Vidiotic.app"
  cp -R "$APP" /Applications/
  # Teach Launch Services about the bundle and its nested helpers now, rather
  # than whenever it next rescans.
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f -R "/Applications/Vidiotic.app" || true
fi

say "done — $APP ($(du -sh "$APP" | cut -f1))"
