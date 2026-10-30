#!/bin/sh
# Builds gemel's Flatpak, as a single-file bundle, for this machine's
# architecture.
#
#   scripts/package-flatpak.sh [OUTPUT-DIR]     # default target/package
#
# Runs on Linux, with flatpak and flatpak-builder. The runtime, the SDK and
# its Rust extension are installed from Flathub for the user if needed: not
# system-wide, which in a container (CI's) may be on overlayfs, where
# updating fails. The build is packaging/linux/io.github.jcomellas.gemel.yml.
# THIRD-PARTY-LICENSES is target/flatpak/THIRD-PARTY-LICENSES if it is
# there (CI copies it from the AppImage's job: its container has no cargo),
# and cargo-about writes it otherwise. Writes OUTPUT-DIR/Gemel-<version>-<arch>.flatpak, which
# `flatpak install --user <file>` installs, with the runtime from Flathub.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$root/target/package}
cd "$root"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
id=io.github.jcomellas.gemel
arch=$(flatpak --default-arch)
work=$root/target/flatpak
mkdir -p "$work" "$out"

[ -s "$work/THIRD-PARTY-LICENSES" ] ||
    scripts/third-party-licenses.sh "$work/THIRD-PARTY-LICENSES"

flathub=https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak remote-add --user --if-not-exists flathub "$flathub"
# Without rofiles-fuse, which containers may not have.
flatpak-builder --user --install-deps-from=flathub --force-clean \
    --disable-rofiles-fuse --state-dir="$work/state" --repo="$work/repo" \
    "$work/build" "packaging/linux/$id.yml"
bundle=$out/Gemel-$version-$arch.flatpak
flatpak build-bundle --runtime-repo="$flathub" "$work/repo" "$bundle" "$id"
echo "wrote $bundle"
