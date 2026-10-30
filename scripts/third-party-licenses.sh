#!/bin/sh
# Writes the licences of the crates gemel's programs are built with, for
# the packages' THIRD-PARTY-LICENSES (packaging/about.toml, about.hbs).
#
#   scripts/third-party-licenses.sh OUTPUT
#
# Needs cargo-about (`cargo install cargo-about --features cli`).
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:?usage: $0 OUTPUT}
cd "$root"
cargo about generate --locked --workspace \
    -c packaging/about.toml packaging/about.hbs -o "$out"
