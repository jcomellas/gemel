#!/bin/sh
# Builds gemel's Windows package: an installer (.msi) for x64.
#
#   scripts/package-windows.sh [OUTPUT-DIR]     # default target/package
#
# Runs on Windows, in Git Bash. Needs WiX 5 (`dotnet tool install --global
# wix --version 5.0.2`) and cargo-about. The installer is built from
# packaging/windows/gemel.wxs and isn't signed, so SmartScreen warns before
# the first run (see the README). Writes OUTPUT-DIR/Gemel-<version>-x64.msi.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/target/package}
cd "$root"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

cargo build --locked --release -p gemel-gui -p gemel -p gemel-fsmonitor
mkdir -p "$out"
scripts/third-party-licenses.sh "$out/THIRD-PARTY-LICENSES"

win() { cygpath -w "$1"; }
msi=$out/Gemel-$version-x64.msi
wix build -arch x64 \
    -d Version="$version" \
    -d BinDir="$(win "$root/target/release")" \
    -d Root="$(win "$root")" \
    -d ThirdParty="$(win "$out/THIRD-PARTY-LICENSES")" \
    -o "$(win "$msi")" \
    "$(win "$root/packaging/windows/gemel.wxs")"
echo "wrote $msi"
