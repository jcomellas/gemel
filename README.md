# gemel

A Rust port of the [Unison](https://github.com/bcpierce00/unison) file
synchronizer, with a portable GUI for macOS, Linux and Windows.

**Status:** early port. The Umarshal codec and the legacy OCaml hash
(`gemel-compat`) are done; everything else is a skeleton.

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
They are committed, so tests don't need OCaml. To regenerate them (needs
Docker and a Unison checkout):

```sh
scripts/gen-ocaml-fixtures.sh /path/to/unison
```

The generator is `tools/ocaml-fixtures/gen_fixtures.ml`. Each fixture
file records the Unison commit and OCaml version it came from. The
committed vectors come from OCaml 5.3.0; OCaml 4.13.1
(`OCAML_BASE=debian:bookworm-slim`) produces identical ones.

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
