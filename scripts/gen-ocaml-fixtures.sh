#!/bin/sh
# Regenerates gemel-compat's golden test vectors from OCaml Unison's sources.
#
#   scripts/gen-ocaml-fixtures.sh /path/to/unison [output-dir]
#   OCAML_BASE=debian:bookworm-slim scripts/gen-ocaml-fixtures.sh ...  # OCaml 4.13
#
# Builds tools/ocaml-fixtures/gen_fixtures.ml against Unison's own
# umarshal.ml, bytearray_stubs.c and hash_compat.c inside Docker (so the host
# needs no OCaml), runs it, and writes crates/gemel-compat/tests/fixtures/.
# The Unison commit and OCaml version are recorded in each file's header.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
unison=$(cd "${1:?usage: $0 /path/to/unison}" && pwd)
commit=$(git -C "$unison" rev-parse --short HEAD)
out=${2:-"$root/crates/gemel-compat/tests/fixtures"}
base=${OCAML_BASE:-debian:trixie-slim}
image=gemel-ocaml-fixtures-$(echo "$base" | tr -c 'a-z0-9.\n' -)

docker build --build-arg OCAML_BASE="$base" -t "$image" "$root/tools/ocaml-fixtures"
mkdir -p "$out"
docker run --rm \
    -e UNISON_COMMIT="bcpierce00/unison@$commit" \
    -v "$unison/src":/unison:ro \
    -v "$root/tools/ocaml-fixtures":/gen:ro \
    -v "$out":/out \
    "$image" sh -c '
        cp /unison/ubase/umarshal.mli /unison/ubase/umarshal.ml \
           /unison/bytearray_stubs.c /unison/hash_compat.c \
           /gen/gen_fixtures.ml . &&
        ocamlopt -o gen_fixtures bytearray_stubs.c hash_compat.c \
            umarshal.mli umarshal.ml gen_fixtures.ml &&
        ./gen_fixtures /out'
echo "wrote $out"
