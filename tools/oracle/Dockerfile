# Builds OCaml Unison's text UI from a source checkout, for the oracle
# (scripts/build-ocaml-unison.sh). Same Debian release as the gemel-linux
# image, so the binary runs there too.
FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ocaml make gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
