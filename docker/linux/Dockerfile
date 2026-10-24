# Linux build and test environment for gemel.
#
# Has what the workspace needs on Linux, including the Slint GUI frontend:
# the Rust toolchain with clippy and rustfmt, fontconfig headers (Slint's
# text stack links fontconfig at build time), and enough of X11, Mesa and
# Xvfb to start the GUI headlessly for smoke tests.
#
# The source tree is bind-mounted, not copied; see scripts/linux-docker.sh.
# Build from this directory so the build context stays small:
#
#   docker build -t gemel-linux -f docker/linux/Dockerfile docker/linux

ARG RUST_VERSION=1.99.1
FROM rust:${RUST_VERSION}-slim-trixie

RUN rustup component add clippy rustfmt

# Build: pkg-config and fontconfig for Slint (fontique -> yeslogic-fontconfig-sys).
# Runtime, loaded with dlopen by winit, glutin and wgpu: X11/xkbcommon/Wayland
# client libraries and Mesa's GL, EGL and Vulkan drivers.
# Smoke tests: Xvfb, plus a font so text has something to lay out.
# Tests: netbase's /etc/services, for ports by service name (gemel-fs's net).
# No Qt: Slint builds its Qt backend whenever it finds qmake.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libfontconfig-dev \
        fonts-dejavu-core \
        libx11-6 libx11-xcb1 libxcursor1 libxrandr2 libxi6 \
        libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 \
        libgl1 libegl1 libgl1-mesa-dri mesa-vulkan-drivers \
        xvfb xauth \
        netbase \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_TARGET_DIR=/target
WORKDIR /work
