#!/bin/sh
# Builds OCaml Unison's text UI (`make tui`) for Linux in Docker, from the
# Unison checkout given, for the oracle harness (tools/oracle).
#
#   scripts/build-ocaml-unison.sh /path/to/unison
#
# Writes target/oracle/unison-linux-<arch> and records the commit next to it.
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
    make tui >/build/make.log 2>&1 || { tail -40 /build/make.log; exit 1; }
    cp src/unison /out/unison-linux-$arch &&
    echo 'bcpierce00/unison@$commit' > /out/unison-linux-$arch.commit"
echo "built $out/unison-linux-$arch from $commit"
