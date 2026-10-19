#!/bin/sh
# Has OCaml Unison write archives for a few small replicas, as fixtures for
# decoding real archives (archives.rs). Run by scripts/gen-archive-fixtures.sh,
# in Docker for Linux or on a macOS host.
#
#   make-archives.sh UNISON-BINARY WORK-DIR OUT-DIR PREFIX
#
# Each case syncs a tree, changes it, and syncs again; both roots' archives
# are copied to OUT-DIR as PREFIX-CASE-a and PREFIX-CASE-b.
set -eu
unison=$1 work=$2 out=$3 prefix=$4
mkdir -p "$out"

file() { # path content mtime
    mkdir -p "$(dirname "$1")"
    printf '%s' "$2" > "$1"
    touch -t "$3" "$1"
}

tree() { # root
    r=$1
    mkdir -p "$r/src/deep/er" "$r/empty"
    file "$r/README" "read me" 202001010000.00
    file "$r/alpha" "" 202001010001.00
    file "$r/Zeta" "zeta zeta" 202001010002.00
    file "$r/src/main.c" "int main(void) { return 0; }" 202001010003.00
    file "$r/src/deep/er/x" "x" 202001010004.00
    file "$r/café" "unicode" 202001010005.00
    file "$r/日本/語" "japanese" 202001010006.00
    file "$r/with space" "space" 202001010007.00
    chmod 600 "$r/alpha"
    chmod 755 "$r/src/main.c"
    ln -s README "$r/link"
}

run() { # case flags...
    c=$1; shift
    d="$work/$prefix-$c"
    rm -rf "$d"
    mkdir -p "$d/u" "$d/b"
    tree "$d/a"
    UNISON="$d/u" "$unison" "$d/a" "$d/b" -batch -ui text -log=false -contactquietly "$@" >/dev/null 2>&1 || true
    file "$d/a/README" "read me again" 202001020000.00
    file "$d/b/new" "new on b" 202001020001.00
    rm -f "$d/a/Zeta"
    UNISON="$d/u" "$unison" "$d/a" "$d/b" -batch -ui text -log=false -contactquietly "$@" >/dev/null 2>&1 || true
    n=0
    for f in "$d"/u/ar*; do
        # Name them by the root they belong to, from the archive header.
        if sed -n 2p "$f" | grep -q "for root [^ ]*$prefix-$c/a "; then side=a; else side=b; fi
        cp "$f" "$out/$prefix-$c-$side"
        n=$((n + 1))
    done
    [ "$n" = 2 ] || { echo "case $c: expected 2 archives, got $n" >&2; exit 1; }
}

run times -times
run notimes
run insensitive -times -ignorecase true
run owner -times -owner -group
run numericids -times -owner -group -numericids
echo "wrote $(ls "$out" | grep -c "^$prefix-") $prefix archives to $out"
