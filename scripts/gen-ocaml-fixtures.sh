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
# - gen_base.ml (clroot.ml, with tools/ocaml-fixtures/stubs/ standing in for
#   Util and Prefs) writes the vectors for gemel-base's clroot and util;
# - gen_rx.ml (rx.ml) writes the vectors for gemel-base's rx, running each
#   anchored pattern in its own process (see the comment in gen_rx.ml);
# - gen_names.ml (rx.ml) writes the vectors for Name and Path's regular
#   expressions to the same directory;
# - gen_pred.ml (pred.ml, rx.ml and unicode.ml, with stand-ins for Util,
#   Prefs and Case) writes the vectors for gemel-engine's pred;
# - gen_uicommon.ml (truncateString from util.ml, and quote and the Stats
#   module from uicommon.ml, copied out of those files, with unicode.ml and
#   the stand-ins in uicommon_prelude.ml) writes the vectors for
#   gemel-ui-api's uicommon and gemel-base's truncate_string;
# - gen_transfer.ml (checksum.ml and transfer.ml, with Unison's Lwt and
#   stand-ins for Trace and Uutil) writes the vectors for gemel-engine's
#   transfer;
# - gen_terminal.ml (the patterns for OpenSSH's messages and the parser of
#   control sequences, copied out of terminal.ml, with rx.ml) writes the
#   vectors for gemel-rpc's terminal;
# - gen_unicode.ml also writes Unison's Unicode tables as Rust source, to
#   crates/gemel-compat/src/unicode/tables.rs;
# - gen_manual.ml (strings.ml) writes Unison's manual as Rust source, to
#   crates/gemel-ui-api/src/manual/docs.rs.
# With an output directory, everything goes there instead (for comparing
# OCaml versions). The Unison commit and OCaml version are recorded in each
# file's header.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
unison=$(cd "${1:?usage: $0 /path/to/unison}" && pwd)
commit=$(git -C "$unison" rev-parse --short HEAD)
out=${2:-"$root/crates/gemel-compat/tests/fixtures"}
tables=${2:-"$root/crates/gemel-compat/src/unicode"}
manual=${2:-"$root/crates/gemel-ui-api/src/manual"}
base=${OCAML_BASE:-debian:trixie-slim}
image=gemel-ocaml-fixtures-$(echo "$base" | tr -c 'a-z0-9.\n' -)

docker build --build-arg OCAML_BASE="$base" -t "$image" "$root/tools/ocaml-fixtures"
mkdir -p "$out" "$tables" "$manual"
docker run --rm \
    -e UNISON_COMMIT="bcpierce00/unison@$commit" \
    -v "$unison/src":/unison:ro \
    -v "$root/tools/ocaml-fixtures":/gen:ro \
    -v "$out":/out \
    -v "$tables":/tables \
    -v "$manual":/manual \
    "$image" sh -c '
        cp /unison/ubase/umarshal.mli /unison/ubase/umarshal.ml \
           /unison/bytearray_stubs.c /unison/hash_compat.c \
           /unison/unicode.ml /unison/unicode_tables.ml \
           /unison/ubase/rx.mli /unison/ubase/rx.ml \
           /unison/clroot.mli /unison/clroot.ml /unison/pred.ml \
           /unison/bytearray.mli /unison/bytearray.ml /unison/checksum.ml \
           /unison/transfer.ml /unison/lwt/lwt.mli /unison/lwt/lwt.ml \
           /gen/gen_transfer.ml /unison/terminal.ml /unison/strings.ml \
           /gen/gen_fixtures.ml /gen/gen_unicode.ml /gen/gen_names.ml \
           /gen/gen_base.ml /gen/gen_rx.ml /gen/gen_pred.ml /gen/gen_manual.ml \
           /gen/stubs/*.ml . &&
        { cat /gen/uicommon_prelude.ml &&
          grep "^let mapSeparator" /unison/pred.ml && echo end &&
          sed -n "/^let truncateString/,/^let findsubstring/p" /unison/ubase/util.ml |
              sed "\$d" &&
          sed -n "/^let globx_quote/,/^let ignorePath/p" /unison/uicommon.ml |
              sed "\$d" &&
          sed -n "/^module Stats = struct/,/^end/p" /unison/uicommon.ml &&
          cat /gen/gen_uicommon.ml; } > gen_uicommon.ml &&
        { sed -n "/^let passwordRx/,/^let authenticity s/p" terminal.ml &&
          sed -n "/^type controlSt/,/^  parseCtrlSeq s\$/p" terminal.ml &&
          cat /gen/gen_terminal.ml; } > gen_terminal.ml &&
        ocamlopt -o gen_fixtures bytearray_stubs.c hash_compat.c \
            umarshal.mli umarshal.ml gen_fixtures.ml &&
        ocamlopt -o gen_unicode hash_compat.c \
            unicode_tables.ml unicode.ml gen_unicode.ml &&
        ocamlopt -o gen_names rx.mli rx.ml gen_names.ml &&
        ocamlopt -I +str -o gen_base str.cmxa safelist.ml util.ml prefs.ml \
            rx.mli rx.ml clroot.mli clroot.ml gen_base.ml &&
        ocamlopt -o gen_rx rx.mli rx.ml gen_rx.ml &&
        ocamlopt -I +str -o gen_pred str.cmxa bytearray_stubs.c hash_compat.c \
            umarshal.mli umarshal.ml unicode_tables.ml unicode.ml \
            safelist.ml util.ml prefs.ml rx.mli rx.ml case.ml pred.ml gen_pred.ml &&
        ocamlopt -o gen_uicommon hash_compat.c unicode_tables.ml unicode.ml safelist.ml \
            util.ml gen_uicommon.ml &&
        ocamlopt -o gen_transfer bytearray_stubs.c umarshal.mli umarshal.ml \
            bytearray.mli bytearray.ml safelist.ml util.ml uutil.ml trace.ml \
            lwt.mli lwt.ml \
            checksum.ml transfer.ml gen_transfer.ml &&
        ./gen_rx /out &&
        for p in $(./gen_rx --anchors); do ./gen_rx --anchor "$p" >> /out/rx.txt; done &&
        ./gen_fixtures /out &&
        ./gen_base /out &&
        ./gen_names /out &&
        ./gen_pred /out &&
        ./gen_uicommon /out &&
        ocamlopt -o gen_terminal rx.mli rx.ml gen_terminal.ml &&
        ./gen_transfer /out &&
        ./gen_terminal /out &&
        ./gen_unicode /out /tables &&
        ocamlopt -o gen_manual strings.ml gen_manual.ml &&
        ./gen_manual /manual'
echo "wrote $out, $tables and $manual"
