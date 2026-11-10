#!/bin/sh
# Builds gemel's macOS package: Gemel.app, in a disk image.
#
#   scripts/package-macos.sh [OUTPUT-DIR]       # default target/package
#   GEMEL_ARCHS=aarch64 scripts/package-macos.sh # this Mac's only, faster
#
# The bundle holds gemel-gui (its executable), gemel and gemel-fsmonitor,
# which gemel-gui finds beside itself, as universal binaries (arm64 and
# x86_64) unless GEMEL_ARCHS says otherwise; the icon; and LICENSE, NOTICE
# and THIRD-PARTY-LICENSES. It is signed ad hoc, not with a Developer ID,
# so Gatekeeper asks before the first run (see the README).
#
# Needs Xcode's command-line tools, the Rust targets for each
# architecture, and cargo-about. Writes OUTPUT-DIR/Gemel.app and
# OUTPUT-DIR/Gemel-<version>-macos.dmg.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/target/package}
archs=${GEMEL_ARCHS:-aarch64 x86_64}
bins="gemel-gui gemel gemel-fsmonitor"
cd "$root"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

for a in $archs; do
    cargo build --locked --release --target "$a-apple-darwin" \
        $(for b in $bins; do printf -- '-p %s ' "$b"; done)
done

app=$out/Gemel.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
for b in $bins; do
    lipo -create -output "$app/Contents/MacOS/$b" \
        $(for a in $archs; do printf '%s ' "target/$a-apple-darwin/release/$b"; done)
done
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
cp packaging/macos/Gemel.icns LICENSE NOTICE "$app/Contents/Resources/"
scripts/third-party-licenses.sh "$app/Contents/Resources/THIRD-PARTY-LICENSES"
plutil -lint "$app/Contents/Info.plist"

# Ad hoc: the helpers, then the bundle, whose signature covers Info.plist
# and Resources (the linker's covers only each binary).
for b in gemel gemel-fsmonitor; do
    codesign --force --sign - "$app/Contents/MacOS/$b"
done
codesign --force --sign - "$app"
codesign --verify --strict --verbose=2 "$app"

# The disk image: the bundle and a link to /Applications to drag it to.
# hdiutil now and then fails with "Resource busy" on CI's runners, so it
# has a few tries.
stage=$out/dmg
rm -rf "$stage"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
dmg=$out/Gemel-$version-macos.dmg
rm -f "$dmg"
for try in 1 2 3; do
    if hdiutil create -volname Gemel -srcfolder "$stage" -ov -format UDZO "$dmg"; then
        break
    fi
    [ "$try" -lt 3 ] || exit 1
    sleep 5
done
rm -rf "$stage"
echo "wrote $app and $dmg"
