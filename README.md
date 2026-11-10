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
Milestone 3 is done ([plan](docs/milestone-3-plan.md)). Remote roots
work over ssh and on sockets: `gemel -server` serves OCaml
clients (2.52 on) as an OCaml server does, `gemel -socket <port|path>`
serves them on a TCP port or a Unix-domain socket (on Unix), and a gemel client
syncs with a root over ssh (`ssh://`) or on a socket (`socket://`),
served by OCaml Unison (2.52 on) or by gemel. A transfer that was
interrupted resumes from what it left, and diff and merge work with a
remote root. As with OCaml's text interface, ssh asks for passwords and
passphrases at the terminal.
Milestone 4 is done ([plan](docs/milestone-4-plan.md)): `gemel-gui`
does what OCaml's GTK interface does, on macOS, Linux and Windows, with
ssh's password prompts in the window and the profile manager, assistant
and editor, and with `-ui text`, `-server` and `-socket` it is the same
program as `gemel`. Synchronising xattrs and ACLs, and installers, are
still to come.

The socket method has no authentication or encryption, as in OCaml
Unison: anyone who can reach the port can read and write the server's
files as the user it runs as. Use it only on a network you trust, or
through an ssh tunnel.

Design and rationale: [docs/unison-port-analysis.md](docs/unison-port-analysis.md).

## Installing

Each push builds packages of the graphical program, `gemel-gui`
(`.github/workflows/packages.yml`), and a version tag attaches them to a
draft release:

- **macOS:** `Gemel-<version>-macos.dmg`, with `Gemel.app` for Apple
  silicon and Intel Macs. Drag it to Applications.
- **Windows:** `Gemel-<version>-x64.msi`, which installs `gemel-gui`,
  `gemel` and `gemel-fsmonitor` in `Program Files\Gemel`, with a Start
  menu entry. `gemel-gui` opens no console; for the command line and the
  text interface, run `gemel`.
- **Linux, as an AppImage:** `Gemel-<version>-x86_64.AppImage`, for
  systems with glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36
  and later). Make it executable (`chmod +x`) and run it. Without FUSE,
  run it with `--appimage-extract-and-run`.
- **Linux, as a Flatpak:** `Gemel-<version>-x86_64.flatpak`. Install it
  with `flatpak install --user Gemel-<version>-x86_64.flatpak`, which
  fetches the Freedesktop runtime from Flathub, and start Gemel from the
  desktop's menu, or with `flatpak run io.github.jcomellas.gemel`. The
  command line is `flatpak run --command=gemel io.github.jcomellas.gemel`.
  The Flatpak can read and write all your files, as a synchronizer must.
  It runs its runtime's ssh, with your `~/.ssh` and ssh agent, but not
  your system's `/etc/ssh` or the programs a `ProxyCommand` names.

The packages aren't signed with a Developer ID or an Authenticode
certificate, so each system asks before the first run:

- **macOS:** Gatekeeper says it can't check the app for malicious
  software. Control-click `Gemel.app` in Finder, choose Open, and confirm;
  or, after the warning, choose Open Anyway in System Settings › Privacy &
  Security. Later runs don't ask.
- **Windows:** SmartScreen says it protected your PC. Choose More info,
  then Run anyway.

To build them yourself: `scripts/package-macos.sh` on macOS,
`scripts/package-windows.sh` on Windows in Git Bash, with WiX 5, and
`scripts/package-appimage.sh` and `scripts/package-flatpak.sh` on Linux
(each script's header lists what it needs). Each package carries `LICENSE`,
`NOTICE` (gemel's and Unison's notices, and where the source is) and
`THIRD-PARTY-LICENSES` (the licences of the crates it is built with).

## Compatibility goal (Tier A)

gemel aims to be wire- and archive-compatible with Unison 2.52 and later
(RPC version 1, archive format 23). A gemel client should work with an
OCaml `unison -server` and the reverse, and both should share the archives
in `~/.unison` and the `.prf` profiles.

To mix the two:

- OCaml client → gemel server: set `servercmd = gemel` in the profile,
  or install gemel on the server under the name `unison`.
- gemel client → OCaml server: set `servercmd = unison` in the profile
  (gemel starts `gemel -server` by default).
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
| `bins/gemel` | The program: text UI and server modes, as a library and the `gemel` binary (no GUI dependencies, enforced in CI) |
| `bins/gemel-gui` | Slint GUI frontend after `uigtk3`, on the session (milestone 4), and with `-ui text` or `-server` the same program as `gemel` |
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
module keeps the copyright lines of its source, and [NOTICE](NOTICE)
collects them. gemel is an independent
project and is not affiliated with or endorsed by the Unison project.

When porting a module, keep the copyright lines of the Unison source file
in the Rust module's doc comment (`//! Derived from Unison, Copyright ...`).
