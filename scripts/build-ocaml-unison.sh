#!/bin/sh
# Builds OCaml Unison for Linux in Docker, from the Unison checkout given,
# for the oracle harness (tools/oracle) and the tests that compare with
# OCaml.
#
#   scripts/build-ocaml-unison.sh /path/to/unison           # its checked-out tree
#   scripts/build-ocaml-unison.sh /path/to/unison v2.52.1   # a tag, for the remote matrix
#
# The checked-out tree builds the text UI and the file watcher (`make tui
# fsmonitor`) as target/oracle/unison-linux-<arch>, recording the commit next
# to it, and the watcher as target/oracle/fsmonitor-linux-<arch>: a name
# OCaml doesn't look for next to its binary, so that each test chooses the
# watcher OCaml uses.
#
# A tag builds the text UI alone, as target/oracle/unison-<version>-linux-<arch>.
# 2.52 links against bigarray.cmxa, which OCaml 5 removed, so it builds on
# Debian bookworm (OCaml 4.13); the rest build on trixie (OCaml 5.3), as the
# gemel-linux image is. Both binaries run there.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
unison=$(cd "${1:?usage: $0 /path/to/unison [tag]}" && pwd)
tag=${2:-}
out="$root/target/oracle"
mkdir -p "$out"

case $tag in
v2.52.* | 2.52.*) debian=bookworm ;;
*) debian=trixie ;;
esac
image=gemel-ocaml-unison${tag:+-$debian}
docker build -q --build-arg "DEBIAN=$debian-slim" -t "$image" "$root/tools/oracle" >/dev/null
arch=$(docker run --rm "$image" uname -m)

build() {
    docker run --rm -i -v "$out":/out "$image" sh -c "
        mkdir -p /build && cd /build && tar xf - &&
        { $1; } >/build/make.log 2>&1 || { tail -40 /build/make.log; exit 1; }
        $2"
}

if [ -z "$tag" ]; then
    commit=$(git -C "$unison" rev-parse --short HEAD)
    # The working tree as it is, uncommitted changes included.
    tar cf - --no-xattrs -C "$unison" --exclude .git . | build "make tui fsmonitor" "
        cp src/unison /out/unison-linux-$arch &&
        cp src/unison-fsmonitor /out/fsmonitor-linux-$arch &&
        echo 'bcpierce00/unison@$commit' > /out/unison-linux-$arch.commit"
    echo "built $out/unison-linux-$arch from $commit"
else
    version=${tag#v}
    # 2.52's Makefile has `text`; the build system 2.53 brought has `tui`.
    case $version in
    2.52.*) target=text ;;
    *) target=tui ;;
    esac
    git -C "$unison" archive "$tag" | build "make $target" "
        cp src/unison /out/unison-$version-linux-$arch"
    echo "built $out/unison-$version-linux-$arch from $tag ($debian)"
fi
