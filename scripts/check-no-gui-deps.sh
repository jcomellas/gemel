#!/bin/sh
# The `gemel` binary runs headless on servers: fail if any GUI toolkit
# ends up in its dependency tree.
set -eu
pattern='^(slint|i-slint-[a-z-]+|egui|eframe|tauri|gtk4?|iced|winit|wry) v'
# Capture first so that a failing `cargo tree` aborts the script (set -e).
tree=$(cargo tree -p gemel -e normal --prefix none)
if printf '%s\n' "$tree" | grep -Ei "$pattern"; then
    echo "error: gemel depends on a GUI toolkit (see above)" >&2
    exit 1
fi
