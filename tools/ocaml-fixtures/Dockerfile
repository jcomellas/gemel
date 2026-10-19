# OCaml toolchain for regenerating gemel-compat's golden test vectors.
#
# Only scripts/gen-ocaml-fixtures.sh needs this; CI and normal builds use the
# committed fixtures and never need OCaml. Unison's sources and the output
# directory are bind-mounted at run time.

# trixie ships OCaml 5.3, bookworm 4.13; OCAML_BASE switches between them.
ARG OCAML_BASE=debian:trixie-slim
FROM ${OCAML_BASE}

RUN apt-get update \
    && apt-get install -y --no-install-recommends ocaml gcc libc6-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
