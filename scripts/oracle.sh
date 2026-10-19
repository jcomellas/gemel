#!/bin/sh
# Runs the oracle harness (tools/oracle) on Linux in Docker, with OCaml
# Unison built from a Unison checkout as the reference.
#
#   scripts/oracle.sh /path/to/unison [gemel-oracle arguments...]
#   scripts/oracle.sh /path/to/unison --candidate /work/target/debug/gemel --count 100
#
# Builds the reference (scripts/build-ocaml-unison.sh) if it is missing.
# Paths given to the harness are inside the container, where the checkout
# is /work.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
unison=${1:?usage: $0 /path/to/unison [gemel-oracle arguments...]}
shift
# Docker runs the host's architecture; macOS calls aarch64 arm64.
arch=$(uname -m)
[ "$arch" = arm64 ] && arch=aarch64
ref="target/oracle/unison-linux-$arch"
[ -x "$root/$ref" ] || "$root/scripts/build-ocaml-unison.sh" "$unison"
exec "$root/scripts/linux-docker.sh" cargo run --locked -q -p gemel-oracle -- \
    --reference "/work/$ref" "$@"
