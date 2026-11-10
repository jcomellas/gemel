#!/bin/sh
# Builds gemel's AppImage, for this machine's architecture (x86_64 or
# aarch64).
#
#   scripts/package-appimage.sh [OUTPUT-DIR]     # default target/package
#
# Runs on Linux. Needs curl, file (for appimagetool) and cargo-about.
# appimagetool and AppImage's runtime are downloaded at pinned versions into target/appimage, and
# checked against their published checksums. The AppImage holds gemel-gui,
# gemel and gemel-fsmonitor, the desktop entry, the metadata and the
# notices, and runs gemel-gui. It links the system's C library, so it runs
# where glibc is as new as the builder's or newer: CI builds it on Ubuntu
# 22.04. Writes OUTPUT-DIR/Gemel-<version>-<arch>.AppImage, and
# OUTPUT-DIR/THIRD-PARTY-LICENSES, which the Flatpak's build takes.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/target/package}
cd "$root"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
id=io.github.jcomellas.gemel

# appimagetool 1.9.1, and the runtime of type2-runtime's 20251108 release:
# their SHA-256s as GitHub publishes them.
arch=$(uname -m)
case $arch in
    x86_64)
        tool_sum=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
        runtime_sum=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d ;;
    aarch64)
        tool_sum=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
        runtime_sum=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444 ;;
    *) echo "no AppImage for $arch" >&2; exit 1 ;;
esac
work=$root/target/appimage
mkdir -p "$work" "$out"

# Downloads URL to FILE unless FILE already has SHA256, and checks it.
fetch() {
    if ! echo "$3  $2" | sha256sum -c --status 2>/dev/null; then
        curl -fsSL -o "$2.part" "$1"
        echo "$3  $2.part" | sha256sum -c --quiet
        mv "$2.part" "$2"
    fi
}
tool=$work/appimagetool-$arch.AppImage
runtime=$work/runtime-$arch
fetch "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-$arch.AppImage" \
    "$tool" "$tool_sum"
fetch "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-$arch" \
    "$runtime" "$runtime_sum"
chmod +x "$tool"

cargo build --locked --release -p gemel-gui -p gemel -p gemel-fsmonitor
bin=${CARGO_TARGET_DIR:-$root/target}/release
scripts/third-party-licenses.sh "$out/THIRD-PARTY-LICENSES"

# The AppDir: AppRun is gemel-gui, which finds gemel and gemel-fsmonitor
# beside itself; the desktop entry and icon at the top, where appimagetool
# looks for them; the rest where a system installs them.
app=$work/Gemel.AppDir
rm -rf "$app"
mkdir -p "$app/usr/bin"
install -m 755 "$bin/gemel-gui" "$bin/gemel" "$bin/gemel-fsmonitor" "$app/usr/bin/"
ln -s usr/bin/gemel-gui "$app/AppRun"
cp "packaging/linux/$id.desktop" "packaging/linux/$id.png" "$app/"
install -D -m 644 "packaging/linux/$id.desktop" -t "$app/usr/share/applications"
install -D -m 644 "packaging/linux/$id.metainfo.xml" -t "$app/usr/share/metainfo"
install -D -m 644 "packaging/linux/$id.png" -t "$app/usr/share/icons/hicolor/256x256/apps"
install -D -m 644 packaging/icon/gemel.svg "$app/usr/share/icons/hicolor/scalable/apps/$id.svg"
install -D -m 644 LICENSE NOTICE "$out/THIRD-PARTY-LICENSES" -t "$app/usr/share/doc/gemel"

# Extracted rather than mounted, since builders may have no FUSE.
image=$out/Gemel-$version-$arch.AppImage
ARCH=$arch "$tool" --appimage-extract-and-run --runtime-file "$runtime" "$app" "$image"
echo "wrote $image"
