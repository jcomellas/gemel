#!/bin/sh
# Runs a command in the Linux build image (docker/linux/Dockerfile) with the
# repository mounted at /work. With no arguments it runs the CI checks.
#
#   scripts/linux-docker.sh                      # fmt, clippy, tests, GUI-deps check
#   scripts/linux-docker.sh cargo build -p twinner
#   TWINNER_PLATFORM=linux/amd64 scripts/linux-docker.sh   # emulated x86_64
#
# Build output and the Cargo registry live in named Docker volumes, so they
# don't mix with the host's target/ and survive between runs. The container
# runs as root: on a Linux host, files it writes into the checkout (there
# should be none, given --locked) would be root-owned.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
platform=${TWINNER_PLATFORM:-}
arch=${platform#linux/}
image=twinner-linux${arch:+-$arch}

docker build ${platform:+--platform "$platform"} -t "$image" \
    -f "$root/docker/linux/Dockerfile" "$root/docker/linux"

if [ "$#" -eq 0 ]; then
    set -- sh -c '
        cargo fmt --all --check &&
        cargo clippy --locked --workspace --all-targets -- -D warnings &&
        cargo test --locked --workspace &&
        ./scripts/check-no-gui-deps.sh'
fi

tty=
[ -t 0 ] && [ -t 1 ] && tty=-it
exec docker run --rm $tty ${platform:+--platform "$platform"} \
    -v "$root":/work \
    -v "$image-target":/target \
    -v "$image-registry":/usr/local/cargo/registry \
    "$image" "$@"
