#!/bin/sh
# Regenerates the real-archive fixtures in crates/gemel-fs/tests/archives/:
# archives written by OCaml Unison on Linux (in Docker) and, on a macOS host
# with unison installed, on macOS.
#
#   scripts/gen-archive-fixtures.sh /path/to/unison-linux-binary
#
# The Linux binary is the one scripts/build-ocaml-unison.sh builds into
# target/oracle/. It must be inside this checkout.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$(cd "$(dirname "${1:?usage: $0 /path/to/unison-linux-binary}")" && pwd)/$(basename "$1")
rel=${bin#"$root"/}
[ "$rel" != "$bin" ] || { echo "$1 must be inside $root" >&2; exit 2; }
out=crates/gemel-fs/tests/archives
mkdir -p "$root/$out"
docker run --rm -v "$root":/work debian:trixie-slim \
    sh "/work/crates/gemel-fs/tests/make-archives.sh" "/work/$rel" /tmp/fx "/work/$out" linux
if [ "$(uname -s)" = Darwin ] && command -v unison >/dev/null; then
    "$root/crates/gemel-fs/tests/make-archives.sh" "$(command -v unison)" \
        "${TMPDIR:-/tmp}/gemel-fx" "$root/$out" macos
fi
{
    echo "Archives written by OCaml Unison; see scripts/gen-archive-fixtures.sh."
    [ -f "$bin.commit" ] && echo "linux: $(cat "$bin.commit")"
    command -v unison >/dev/null && echo "macos: $(unison -version)"
} > "$root/$out/README"
