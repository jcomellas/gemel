# twinner

A Rust port of the [Unison](https://github.com/bcpierce00/unison) file
synchronizer, with a portable GUI for macOS, Linux and Windows.

**Status:** workspace skeleton only. Nothing is implemented yet.

Design and rationale: [docs/unison-port-analysis.md](docs/unison-port-analysis.md).

## Compatibility goal (Tier A)

twinner aims to be wire- and archive-compatible with Unison 2.52 and later
(RPC version 1, archive format 23). A twinner client should work with an
OCaml `unison -server` and the reverse, and both should share the archives
in `~/.unison` and the `.prf` profiles.

To mix the two:

- OCaml client → twinner server: set `servercmd = twinner` in the profile,
  or install twinner on the server under the name `unison`.
- OCaml Unison using the twinner file watcher: install `twinner-fsmonitor`
  as `unison-fsmonitor`.

## Layout

Crates are layered; each depends only on the ones above it:

| Crate | Role |
|---|---|
| `crates/twinner-compat` | Bit-exact formats shared with OCaml Unison (Umarshal, legacy hash, Unicode tables, fsmonitor protocol) |
| `crates/twinner-base` | Preferences/profiles, CLI flags, names, paths, case handling, feature registry |
| `crates/twinner-fs` | OS layer: filesystem, properties, xattrs, ACLs, macOS metadata, fingerprints |
| `crates/twinner-rpc` | RPC framing, version handshake, command registry, ssh/socket connections |
| `crates/twinner-engine` | Update detection, reconciliation, propagation, archives |
| `crates/twinner-ui-api` | The engine/frontend contract |
| `bins/twinner` | Text UI and `-server` mode (no GUI dependencies, enforced in CI) |
| `bins/twinner-gui` | GUI frontend (toolkit decided by the spike) |
| `bins/twinner-fsmonitor` | File-watcher helper |

Each module's doc comment names the Unison source files it ports.

## Licence and provenance

twinner is derived from Unison and is distributed under the GNU General
Public License, version 3 or (at your option) any later version. See
[LICENSE](LICENSE).

Unison is Copyright Benjamin C. Pierce and other contributors; each ported
module keeps the copyright lines of its source. twinner is an independent
project and is not affiliated with or endorsed by the Unison project.

When porting a module, keep the copyright lines of the Unison source file
in the Rust module's doc comment (`//! Derived from Unison, Copyright ...`).
