#!/bin/sh
# Builds OCaml Unison's text UI and file watcher (`make tui fsmonitor`) for
# Linux in Docker, from the Unison checkout given, for the oracle harness
# (tools/oracle) and the tests that compare with OCaml.
#
#   scripts/build-ocaml-unison.sh /path/to/unison
#
# Writes target/oracle/unison-linux-<arch> and records the commit next to
# it, and the watcher as target/oracle/fsmonitor-linux-<arch>: a name OCaml
# doesn't look for next to its binary, so that each test chooses the watcher
# OCaml uses.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
unison=$(cd "${1:?usage: $0 /path/to/unison}" && pwd)
commit=$(git -C "$unison" rev-parse --short HEAD)
image=gemel-ocaml-unison
docker build -t "$image" "$root/tools/oracle"
arch=$(docker run --rm "$image" uname -m)
out="$root/target/oracle"
mkdir -p "$out"
docker run --rm -v "$unison":/src:ro -v "$out":/out "$image" sh -c "
    cp -r /src/. /build/ && cd /build &&
    make tui fsmonitor >/build/make.log 2>&1 || { tail -40 /build/make.log; exit 1; }
    cp src/unison /out/unison-linux-$arch &&
    cp src/unison-fsmonitor /out/fsmonitor-linux-$arch &&
    echo 'bcpierce00/unison@$commit' > /out/unison-linux-$arch.commit"
echo "built $out/unison-linux-$arch from $commit"
