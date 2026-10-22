# Builds OCaml Unison from a source tree, for the oracle
# (scripts/build-ocaml-unison.sh). Trixie by default, the gemel-linux image's
# release, so the binary runs there too; 2.52 needs bookworm's OCaml 4.
ARG DEBIAN=trixie-slim
FROM debian:${DEBIAN}

RUN apt-get update \
    && apt-get install -y --no-install-recommends ocaml make gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
