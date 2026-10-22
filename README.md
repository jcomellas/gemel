# gemel

A Rust port of the [Unison](https://github.com/bcpierce00/unison) file
synchronizer, with a portable GUI for macOS, Linux and Windows.

**Status:** early port. The foundation crates are done and tested against
OCaml Unison:
- `gemel-compat`: the Umarshal codec, the legacy OCaml hash, Unison's
  Unicode handling and the file-watcher protocol;
- `gemel-base`: case modes, names, paths, preferences and profiles, the
  command line, root specifications, the feature registry, property lists,
  tracing and logging, and the Rx pattern engine.

Milestone 2 is done ([plan](docs/milestone-2-plan.md)): the oracle
harness (`tools/oracle`), `gemel-fs` (file metadata, `stat`, directory
operations, fingerprints), the local half of `gemel-rpc` (roots, the
command registry, the engine thread), the engine's foundations (update
items, ignore and other path predicates, roots and feature negotiation,
locks, the fingerprint cache), archives (loading, storing, locking, crash
recovery and commits, byte-compatible with OCaml's), update detection,
reconciliation (with move detection), and propagation between local roots
(transport, copying, deleting, props, renames, moves, backups, diff and
merge) are done: a whole sync between local roots ends as OCaml Unison's
does. So is the frontends' shared startup (`gemel-ui-api`'s `uicommon`:
choosing and loading a profile, the command line, setting up the roots),
so a sync can run from a `.prf` profile as OCaml's does. The engine
reports what an interface shows: progress for each item, the path being
scanned, status messages and timers, and a merge's result; and an abort
reaches a copy as it runs. The session contract the frontends use
(`gemel-ui-api`'s `session`: commands in, events out, on the engine
thread) is done too, and so is the `gemel` program's text interface:
`gemel <profile>` (or `gemel root1 root2`) goes through the items as
`unison -ui text` does, with its keys, matching conditions and proceed
menu, in raw mode or in a dumb terminal, and syncs, prints and logs as it
does, in batch mode too; Ctrl-C and `SIGTERM` are handled as OCaml 5
handles them. `-repeat` with an interval and the profile manager (`-i`) work as
OCaml's do, and `-help` lists the options as OCaml's does, except
`-acl` and `-xattrs`. On generated scenarios gemel ends as OCaml Unison
does, and OCaml accepts the archives gemel writes. CI checks this on
Linux, and Windows replays outcomes recorded from OCaml. Watch mode
works too: `-repeat watch` syncs whenever `gemel-fsmonitor`, the file
watcher, reports changes, and scans only what changed. The watcher runs on
Linux, macOS (FSEvents, which OCaml has no watcher for) and Windows, and
OCaml Unison uses it as its `unison-fsmonitor` on Linux and macOS.
Synchronising xattrs and ACLs, the GUI and remote connections are still
to come.

Design and rationale: [docs/unison-port-analysis.md](docs/unison-port-analysis.md).

## Compatibility goal (Tier A)

gemel aims to be wire- and archive-compatible with Unison 2.52 and later
(RPC version 1, archive format 23). A gemel client should work with an
OCaml `unison -server` and the reverse, and both should share the archives
in `~/.unison` and the `.prf` profiles.

To mix the two:

- OCaml client → gemel server: set `servercmd = gemel` in the profile,
  or install gemel on the server under the name `unison`.
- OCaml Unison using the gemel file watcher: install `gemel-fsmonitor`
  as `unison-fsmonitor`.

## Layout

Crates are layered; each depends only on the ones above it:

| Crate | Role |
|---|---|
| `crates/gemel-compat` | Bit-exact formats shared with OCaml Unison (Umarshal, legacy hash, Unicode tables, fsmonitor protocol) |
| `crates/gemel-base` | Preferences/profiles, CLI flags, names, paths, case handling, feature registry |
| `crates/gemel-fs` | OS layer: filesystem, properties, xattrs, ACLs, macOS metadata, fingerprints |
| `crates/gemel-rpc` | RPC framing, version handshake, command registry, ssh/socket connections |
| `crates/gemel-engine` | Update detection, reconciliation, propagation, archives |
| `crates/gemel-ui-api` | The engine/frontend contract |
| `bins/gemel` | Text UI and `-server` mode (no GUI dependencies, enforced in CI) |
| `bins/gemel-gui` | Slint GUI frontend (no engine yet: shows fake data) |
| `bins/gemel-fsmonitor` | File-watcher helper |

Each module's doc comment names the Unison source files it ports.

## Golden test vectors

Code that must match OCaml Unison bit for bit is tested against vectors
produced by Unison's own code, in `crates/gemel-compat/tests/fixtures/`.
Unison's Unicode tables are extracted the same way, into
`crates/gemel-compat/src/unicode/tables.rs`. Both are committed, so builds
and tests don't need OCaml. To regenerate them (needs Docker and a Unison
checkout):

```sh
scripts/gen-ocaml-fixtures.sh /path/to/unison
```

The generators are in `tools/ocaml-fixtures/`. Each generated file records
the Unison commit and OCaml version it came from. The committed files come
from OCaml 5.3.0; OCaml 4.13.1 (`OCAML_BASE=debian:bookworm-slim`)
produces identical ones.

## Licence and provenance

gemel is derived from Unison and is distributed under the GNU General
Public License, version 3 or (at your option) any later version. See
[LICENSE](LICENSE).

The GUI binary (`gemel-gui`) links [Slint](https://slint.dev) under
its GPL-3.0-only licence, so that binary is distributed under GPLv3 only.
The other binaries keep the "or any later version" option.

Unison is Copyright Benjamin C. Pierce and other contributors; each ported
module keeps the copyright lines of its source. gemel is an independent
project and is not affiliated with or endorsed by the Unison project.

When porting a module, keep the copyright lines of the Unison source file
in the Rust module's doc comment (`//! Derived from Unison, Copyright ...`).
