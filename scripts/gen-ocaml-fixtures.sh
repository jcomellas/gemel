#!/bin/sh
# Regenerates gemel-compat's golden test vectors from OCaml Unison's sources.
#
#   scripts/gen-ocaml-fixtures.sh /path/to/unison [output-dir]
#   OCAML_BASE=debian:bookworm-slim scripts/gen-ocaml-fixtures.sh ...  # OCaml 4.13
#
# Builds the generators in tools/ocaml-fixtures/ against Unison's own sources
# inside Docker (so the host needs no OCaml) and runs them:
# - gen_fixtures.ml (umarshal.ml, bytearray_stubs.c, hash_compat.c) and
#   gen_unicode.ml (unicode.ml, unicode_tables.ml) write the golden vectors
#   in crates/gemel-compat/tests/fixtures/;
# - gen_names.ml (rx.ml) writes the vectors for Name and Path's regular
#   expressions to the same directory;
# - gen_unicode.ml also writes Unison's Unicode tables as Rust source, to
#   crates/gemel-compat/src/unicode/tables.rs.
# With an output directory, everything goes there instead (for comparing
# OCaml versions). The Unison commit and OCaml version are recorded in each
# file's header.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
unison=$(cd "${1:?usage: $0 /path/to/unison}" && pwd)
commit=$(git -C "$unison" rev-parse --short HEAD)
out=${2:-"$root/crates/gemel-compat/tests/fixtures"}
tables=${2:-"$root/crates/gemel-compat/src/unicode"}
base=${OCAML_BASE:-debian:trixie-slim}
image=gemel-ocaml-fixtures-$(echo "$base" | tr -c 'a-z0-9.\n' -)

docker build --build-arg OCAML_BASE="$base" -t "$image" "$root/tools/ocaml-fixtures"
mkdir -p "$out" "$tables"
docker run --rm \
    -e UNISON_COMMIT="bcpierce00/unison@$commit" \
    -v "$unison/src":/unison:ro \
    -v "$root/tools/ocaml-fixtures":/gen:ro \
    -v "$out":/out \
    -v "$tables":/tables \
    "$image" sh -c '
        cp /unison/ubase/umarshal.mli /unison/ubase/umarshal.ml \
           /unison/bytearray_stubs.c /unison/hash_compat.c \
           /unison/unicode.ml /unison/unicode_tables.ml \
           /unison/ubase/rx.mli /unison/ubase/rx.ml \
           /gen/gen_fixtures.ml /gen/gen_unicode.ml /gen/gen_names.ml . &&
        ocamlopt -o gen_fixtures bytearray_stubs.c hash_compat.c \
            umarshal.mli umarshal.ml gen_fixtures.ml &&
        ocamlopt -o gen_unicode hash_compat.c \
            unicode_tables.ml unicode.ml gen_unicode.ml &&
        ocamlopt -o gen_names rx.mli rx.ml gen_names.ml &&
        ./gen_fixtures /out &&
        ./gen_names /out &&
        ./gen_unicode /out /tables'
echo "wrote $out and $tables"
