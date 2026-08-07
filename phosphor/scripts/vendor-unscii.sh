#!/usr/bin/env bash
# vendor-unscii — fetch unscii-8, zero its leading, and check its coverage.
#
# `assets/unscii-8-grid.ttf` is a *modified* upstream font, and a binary asset
# nobody can diff is exactly the kind of thing that becomes folklore. This is
# the derivation, runnable: it prints the upstream hash it started from, states
# the one edit, and fails if the result no longer covers what `phosphor::icon`
# and `phosphor::widgets` depend on.
#
# Licence: Unscii is by Viznut and is **public domain** — every variant except
# `unscii-16-full`, which is GPL because it incorporates GNU Unifont. This uses
# `unscii-8`, which is not that one. Public domain is what a font on a public
# site needs, and it is why this is here instead of a C64 ROM reimplementation
# whose terms would have to be argued about (web-port.md §9a).
#
# The edit: hhea.lineGap and OS/2.sTypoLineGap, 3 -> 0.
#
# Unscii ships 3/32 em of leading, which is right for a terminal and wrong for
# a character grid. epaint computes `row_height = ascent - descent + line_gap`
# (epaint-0.35.0/src/text/font.rs:588) and exposes no way to override the gap —
# `FontTweak` has scale and y-offset and nothing for leading. Left alone, an
# 8x8 font at 16pt lays out on 17.5pt rows, and "the cell is 16 points" would be
# false in the one place it has to be true. Leading is what a character grid
# does not have.
#
# Usage:  phosphor/scripts/vendor-unscii.sh [path-to-unscii-8.ttf]
#         (downloads from viznut.fi if no path is given)

set -euo pipefail
cd "$(dirname "$0")/.."

URL=http://viznut.fi/unscii/unscii-8.ttf
OUT=assets/unscii-8-grid.ttf
SRC=${1:-}

if [ -z "$SRC" ]; then
  SRC=$(mktemp -t unscii-8.XXXXXX).ttf
  echo "fetching $URL"
  curl -fsSL --max-time 120 -o "$SRC" "$URL"
fi

echo "upstream: $SRC"
shasum -a 256 "$SRC" | sed 's/^/  sha256 /'

python3 - "$SRC" "$OUT" <<'PY'
import struct, sys

src, out = sys.argv[1], sys.argv[2]
d = bytearray(open(src, 'rb').read())

num = struct.unpack('>H', d[4:6])[0]
tabs = {}
for i in range(num):
    o = 12 + 16 * i
    tag = d[o:o+4].decode('latin1')
    off, ln = struct.unpack('>II', d[o+8:o+16])
    tabs[tag] = (o, off, ln)

# --- the edit ------------------------------------------------------------
_, hhea, _ = tabs['hhea']
asc, desc, gap = struct.unpack('>hhh', d[hhea+4:hhea+10])
struct.pack_into('>h', d, hhea + 8, 0)

_, os2, _ = tabs['OS/2']
tgap = struct.unpack('>h', d[os2+72:os2+74])[0]
struct.pack_into('>h', d, os2 + 72, 0)
print(f"  hhea: ascent {asc}, descent {desc}, lineGap {gap} -> 0")
print(f"  OS/2: sTypoLineGap {tgap} -> 0")

upem = struct.unpack('>H', d[tabs['head'][1]+18:tabs['head'][1]+20])[0]
assert asc == upem and desc == 0, (
    f"expected the em box to be the cell (ascent {asc}, descent {desc}, upem {upem}); "
    "the grid metrics in theme.rs assume row == font size")

# --- checksums -----------------------------------------------------------
# Nothing in the skrifa/epaint path verifies these, but a font that says one
# thing and is another is a trap for the next tool that opens it.
def checksum(buf, off, ln):
    total = 0
    for i in range(0, (ln + 3) & ~3, 4):
        word = buf[off+i:off+i+4].ljust(4, b'\0')
        total = (total + struct.unpack('>I', word)[0]) & 0xFFFFFFFF
    return total

head_dir, head_off, _ = tabs['head']
struct.pack_into('>I', d, head_off + 8, 0)          # zero checkSumAdjustment first
for tag, (dir_off, off, ln) in tabs.items():
    struct.pack_into('>I', d, dir_off + 4, checksum(d, off, ln))
whole = checksum(d, 0, len(d))
struct.pack_into('>I', d, head_off + 8, (0xB1B0AFBA - whole) & 0xFFFFFFFF)

# --- coverage ------------------------------------------------------------
# Which characters this font is *required* to have. `icon.rs` picks from the
# real-Unicode blocks §9a targets rather than a private-use area, and this is
# what keeps that honest: pick a glyph the font lacks and the vendoring fails
# rather than the UI quietly showing a missing-glyph box.
cmap_off = tabs['cmap'][1]
n_sub = struct.unpack('>H', d[cmap_off+2:cmap_off+4])[0]
sub = None
for i in range(n_sub):
    _, _, off = struct.unpack('>HHI', d[cmap_off+4+8*i:cmap_off+12+8*i])
    if struct.unpack('>H', d[cmap_off+off:cmap_off+off+2])[0] == 12:
        sub = cmap_off + off                        # format 12 — reaches past the BMP
assert sub, "no format-12 cmap; the Legacy Computing block is above the BMP"
ngroups = struct.unpack('>I', d[sub+12:sub+16])[0]
covered = set()
for i in range(ngroups):
    s, e, _ = struct.unpack('>III', d[sub+16+12*i:sub+28+12*i])
    covered.update(range(s, e + 1))

REQUIRED = {
    'icon.rs': [0x25B6, 0x2016, 0x25C4, 0x25BA, 0x2502, 0x2212, 0x002B, 0x2194,
                0x253C, 0x25B2, 0x25BC, 0x00D7, 0x2261, 0x2193, 0x25A4, 0x25F4,
                0x25C8],
    'widgets.rs eighth-blocks': list(range(0x2581, 0x2589)) + list(range(0x2589, 0x2590)),
    'box drawing':             [0x2500, 0x2502, 0x250C, 0x2510, 0x2514, 0x2518, 0x253C],
    'block elements':          [0x2588, 0x2591, 0x2592, 0x2593, 0x25AE],
    'legacy computing':        [0x1FB00, 0x1FB3B, 0x1FB70, 0x1FB7D, 0x1FB8B],
    'card suits':              [0x2660, 0x2661, 0x2663, 0x2665, 0x2666],
}
missing = {k: [f"U+{c:04X}" for c in v if c not in covered] for k, v in REQUIRED.items()}
missing = {k: v for k, v in missing.items() if v}
if missing:
    for k, v in missing.items():
        print(f"  MISSING {k}: {' '.join(v)}")
    sys.exit(1)
print(f"  coverage: {len(covered)} codepoints, all {sum(len(v) for v in REQUIRED.values())} required present")

open(out, 'wb').write(bytes(d))
PY

echo "wrote $OUT"
shasum -a 256 "$OUT" | sed 's/^/  sha256 /'
ls -l "$OUT" | awk '{print "  " $5 " bytes"}'
