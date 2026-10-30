#!/bin/sh
# Builds the packages' icons from packaging/icon/gemel.svg:
# - packaging/macos/Gemel.icns, for the macOS application bundle;
# - packaging/windows/gemel.ico, for the Windows installer's shortcut.
#
#   scripts/make-icons.sh
#
# Needs rsvg-convert (librsvg) and, for the .icns, macOS's iconutil. The
# icons are committed, so packaging doesn't need either; run this after
# changing the SVG.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
svg=$root/packaging/icon/gemel.svg
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

png() { rsvg-convert -w "$1" -h "$1" "$svg" -o "$2"; }

# macOS: an iconset of each size at 1x and 2x.
set=$tmp/Gemel.iconset
mkdir -p "$set" "$root/packaging/macos"
for s in 16 32 128 256 512; do
    png $s "$set/icon_${s}x${s}.png"
    png $((s * 2)) "$set/icon_${s}x${s}@2x.png"
done
iconutil -c icns -o "$root/packaging/macos/Gemel.icns" "$set"

# Windows: an .ico of PNG images, which Windows Vista and later read.
mkdir -p "$root/packaging/windows"
sizes="16 24 32 48 64 128 256"
for s in $sizes; do png $s "$tmp/$s.png"; done
python3 - "$root/packaging/windows/gemel.ico" "$tmp" $sizes <<'PY'
import struct, sys
out, tmp, sizes = sys.argv[1], sys.argv[2], [int(s) for s in sys.argv[3:]]
images = [open(f"{tmp}/{s}.png", "rb").read() for s in sizes]
# ICONDIR, then an ICONDIRENTRY per image (a size of 256 is written as 0),
# then the images.
data = struct.pack("<HHH", 0, 1, len(images))
offset = 6 + 16 * len(images)
for s, img in zip(sizes, images):
    data += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(img), offset)
    offset += len(img)
open(out, "wb").write(data + b"".join(images))
PY
echo "wrote packaging/macos/Gemel.icns and packaging/windows/gemel.ico"
