#!/bin/sh
# Starts gemel-gui's window as a Linux package runs it, under Xvfb, and
# checks that it shows: a window whose class is the application's id,
# which desktops match to the desktop entry, with the program still running
# a few seconds later.
#
#   scripts/smoke-linux-gui.sh COMMAND [ARG...]
#
# COMMAND [ARG...] starts gemel-gui; the script adds two empty roots, in a
# directory under $HOME, since a Flatpak's /tmp is its own. Needs xvfb-run,
# python3 and libX11, which Flathub's image for building Flatpaks has.
set -eu
if [ -z "${DISPLAY:-}" ]; then
    exec xvfb-run -a -s "-screen 0 1280x800x24" "$0" "$@"
fi
id=io.github.jcomellas.gemel
dir=$(mktemp -d "$HOME/gemel-smoke.XXXXXX")
mkdir "$dir/a" "$dir/b"

# The class of each window on the display, one a line, from WM_CLASS.
classes() {
    python3 -c '
import ctypes as c
x = c.CDLL("libX11.so.6")
x.XOpenDisplay.restype = c.c_void_p
x.XOpenDisplay.argtypes = [c.c_char_p]
x.XDefaultRootWindow.restype = c.c_ulong
x.XDefaultRootWindow.argtypes = [c.c_void_p]
Windows = c.POINTER(c.c_ulong)
x.XQueryTree.argtypes = [c.c_void_p, c.c_ulong, Windows, Windows,
                         c.POINTER(Windows), c.POINTER(c.c_uint)]

class ClassHint(c.Structure):
    _fields_ = [("res_name", c.c_char_p), ("res_class", c.c_char_p)]

x.XGetClassHint.argtypes = [c.c_void_p, c.c_ulong, c.POINTER(ClassHint)]
d = x.XOpenDisplay(None)
if not d:
    raise SystemExit("cannot open the display")

def walk(w):
    h = ClassHint()
    if x.XGetClassHint(d, w, c.byref(h)) and h.res_class:
        print(h.res_class.decode())
    root, parent, kids, n = c.c_ulong(), c.c_ulong(), Windows(), c.c_uint()
    if x.XQueryTree(d, w, c.byref(root), c.byref(parent), c.byref(kids), c.byref(n)):
        for i in range(n.value):
            walk(kids[i])

walk(x.XDefaultRootWindow(d))
'
}

"$@" "$dir/a" "$dir/b" >"$dir/output" 2>&1 &
pid=$!
fail() {
    echo "$1" >&2
    sed 's/^/  | /' "$dir/output" >&2
    kill "$pid" 2>/dev/null || true
    exit 1
}

# The window, within a minute: a package may be extracted first.
i=0
until classes | grep -x "$id"; do
    kill -0 "$pid" 2>/dev/null || fail "gemel-gui ended before showing its window"
    i=$((i + 1))
    [ "$i" -le 60 ] || fail "no window of class $id after a minute"
    sleep 1
done
sleep 5
kill -0 "$pid" 2>/dev/null || fail "gemel-gui ended after showing its window"
kill "$pid"
wait "$pid" 2>/dev/null || true
echo "gemel-gui showed its window, and was still running 5 seconds later; its output:"
sed 's/^/  | /' "$dir/output"
rm -rf "$dir"
