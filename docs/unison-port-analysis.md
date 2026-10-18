# Twinner: Porting Unison to Rust — Analysis and Recommendation

*Status: Tier A adopted (2026-10-18), GUI spike pending · Source analysed: `bcpierce00/unison` at `8772d26` (post-2.54.0) · Date: 2026-10-17*

## 1. Summary

- **Most of the work is the sync engine, not the GUI.** About 30k lines of OCaml and C are the engine. The GUIs are about 9k lines: GTK3 plus Cocoa.
- **The first decision is compatibility, before the GUI library.** Either twinner talks to and shares archives with existing Unison installations, or it is a clean-room tool with the same semantics. We recommend **wire- and archive-compatibility with Unison ≥ 2.52** as the v1 target. It is cheaper than it looks (§3), and it gives us the OCaml binary as a test oracle and a way for users to migrate gradually.
- **Build the engine as a GUI-free library**, with a text UI and headless `-server` mode first and the GUI as a separate frontend. Unison's own macOS bridge (`uimacbridge.ml`) already defines a clean engine↔GUI contract we can copy.
- **GUI recommendation: Slint**, with **egui** as the fallback. **Tauri 2** is the alternative if the team prefers a web frontend. Validate the choice with a 1–2 day spike of the main recon table (50k rows) in the top two candidates before committing (§6).
- **Licensing:** a translation is a derivative work, so **twinner must be GPLv3** and must keep Unison's copyright notices (§7).

## 2. What Unison is, structurally

| Layer | Modules (src/) | Approx. lines | Notes |
|---|---|---|---|
| Foundation ("ubase") | `prefs`, `proplist`, `umarshal`, `rx`, `trace`, `uarg`, `util`, `safelist`, `myMap` | 4.7k | Includes the custom binary serializer and its own glob/regex engine |
| Concurrency | `lwt/` (vendored, old, **forked** Lwt) | 2.1k | Cooperative threads. Replace, don't port |
| OS abstraction | `system/`, `os`, `fspath`, `fs`, `fileinfo`, `props`, `osx`, `case`, `unicode(+_tables)` | ~7k | Includes Windows wide-char APIs, macOS resource forks/Finder info, permissions, ownership, times |
| C stubs | `props_xattr.c`, `props_acl.c`, `copy_stubs.c` (reflink/clonefile), `osxsupport.c`, `pty.c`, `bytearray_stubs.c`, `hash_compat.c` | ~2.8k | All have Rust crate equivalents except `hash_compat.c` (see §3) |
| Engine | `update` (3.1k), `recon`, `transport`, `files`, `copy`, `transfer` (rsync algorithm), `fingerprint`/`fpcache` (MD5), `stasher` (backups), `moves`, `xferhint`, `lock`, `globals`, `pred` | ~13k | The core. `update.ml` is the biggest and hardest part |
| Remote/RPC | `remote` (2.3k), `clroot`, `negotiate`, `features`, `terminal` | ~3.5k | Runs over ssh stdio or a socket. RPC version handshake, ~67 registered commands, feature negotiation |
| File watching | `fswatch`, `fsmonitor/` (inotify, Windows; Solaris is deprecated) | ~2.9k | A separate `unison-fsmonitor` helper process that speaks a line protocol |
| UI-shared | `uicommon` (1.2k), `sortri`, `main` | ~1.7k | Profile handling, formatting, stats, the driver |
| Text UI | `uitext` | 1.8k | Doubles as the reference "driver" for the engine |
| GTK3 GUI | `uigtk3`, `pixmaps` | 5.7k | lablgtk3 |
| macOS GUI | `uimac/` (Obj-C) + `uimacbridge.ml` | 4.5k + 0.8k | Cocoa. Upstream says it is unmaintained |
| Docs/strings | `strings.ml` | 3.2k | Generated manual text |

Context from upstream (NEWS.md, 2.54.0):

- The pre-2.52 wire protocol and archive format were dropped.
- External rsync support is deprecated.
- The GTK GUI "may end suddenly" because lablgtk is hard to maintain.
- The Mac GUI has no maintainer.

So there is real demand for a maintained, portable GUI, and the compatibility surface we would have to match has recently *shrunk*.

### The GUI's functional requirements (from `uigtk3` / `uimac`)

1. **Recon table:** one row per changed path (often tens of thousands). Columns: path, left status, direction arrow, right status, size and progress. Needs virtualisation, **multi-select**, sorting, and per-row status icons.
2. **Keyboard-driven actions** on the selection: `>` `<` `/` (skip), merge, "prefer newer/older", revert, ignore path/name/extension. Plus menu equivalents.
3. **Details pane** and a **diff viewer** window.
4. **Progress:** per item and overall, with rate and ETA (`Uicommon.Stats`), and a client/server/total transfer readout.
5. **Profile chooser**, **profile editor** (a typed preference list) and a **new-profile wizard** (local/ssh/socket, directory pickers).
6. **SSH password/passphrase dialog**, driven by a PTY on the ssh process (`terminal.ml`, `pty.c`).
7. Error/warning dialogs, a "contacting server" state, a repeat/watch mode, and notifications.
8. Native file dialogs, native menu bar on macOS, and accessibility.

## 3. The pivotal decision: compatibility tier

| Tier | What it means | Benefit | Cost |
|---|---|---|---|
| **A. Wire-compatible** (includes B) | A Rust client works with an OCaml `unison -server`, and the reverse | Users can migrate one host at a time. Mixed-version testing gives a very strong oracle | Umarshal codec, RPC v1 framing, the ~67 commands, feature negotiation, rsync block protocol, legacy hash |
| **B. Archive-compatible** | Reads and writes `~/.unison/ar*` archives (format 23) and `fp*` caches | Switching costs no full rescan, and archive output can be compared with OCaml's | Umarshal codec, legacy hash, and exact Unicode/case semantics |
| **C. Clean-room** | Same sync semantics, own formats | Freedom to modernise (e.g. BLAKE3, serde-based formats) | Black-box comparison with OCaml (recon decisions, final trees) still works. Lost: archive- and wire-level comparison, mixed-fleet migration, and reuse of existing archives (every user does a full rescan) |

**`.prf` profile compatibility is cheap and worth keeping in every tier.**

Why A/B is more tractable than it first appears:

- **Umarshal** is a schema-less, combinator-based binary format: ints, strings, options, lists, sums and products. A small Rust codec, or a `serde` data format, can mirror it one-to-one.
- **The legacy hash.** `Uutil.hash` is OCaml's pre-4.00 polymorphic hash (`hash_compat.c`). It feeds `checkArchive`, props hashing and case-insensitive name hashing, and the archive checksums are **compared across replicas** in `loadArchives`/`archivesIdentical`. A mixed pair must therefore match it bit-for-bit. The good news: it is applied to a **small, fixed set of value shapes**:
  - strings and ints (names, digests, symlink text, permission bits, uid/gid);
  - `string option` (ACLs, type/creator);
  - xattrs: `(string * attrvalue) list * int64` inside an option.

  So we only need to emulate the old algorithm's traversal (block tags, sizes, the 10/100 count limits, int64 custom blocks) for those shapes. We do not need a general OCaml-heap emulation. Golden vectors from the OCaml binary make this a small, testable job.
- **Archive filenames** are `MD5("<thisRoot>;<roots>;<format>")` (`update.ml: archiveHash`), so finding existing archives is trivial.
- **Unicode and case folding** use Unison's own tables (`unicode_tables.ml`, `unicode.ml`). Port these tables as data instead of using the `unicode-normalization` crate. That crate may follow a different Unicode version, which would silently break case-insensitive mode and archive checksums.
- **Feature negotiation** (`features.ml`) lets twinner advertise only the features it implements.

**Recommendation:** target **Tier A for v1**. Keep the bit-exact compatibility code (the Umarshal codec, legacy hash, OCaml-compatible Unicode tables) in `twinner-compat`, and the RPC v1 mechanism in `twinner-rpc`. A native protocol v2 (e.g. stronger hashes, serde) can then be negotiated later without touching the engine.

> **Decision (2026-10-18): Tier A adopted.** twinner v1 is wire- and archive-compatible with Unison ≥ 2.52 (RPC version 1, archive format 23).

## 4. Architecture recommendation

This layout is implemented in the workspace scaffold. Each crate depends only on the ones listed above it:

```
twinner/
  crates/
    twinner-compat   # bit-exact formats: umarshal codec, legacy OCaml hash, Unicode tables, fsmonitor protocol
    twinner-base     # prefs/.prf profiles, uarg flags, glob (rx.ml), features registry, case, name, path, clroot
    twinner-fs       # OS layer: system, fspath, os, props (xattr/ACL), macOS metadata, reflink, fingerprints
    twinner-rpc      # RPC framing + version handshake, command registry/dispatch, ssh/socket connections, PTY
    twinner-engine   # update detection + archives, recon, transport, transfer (rsync), stasher, moves, negotiate
    twinner-ui-api   # the engine<->frontend contract (commands + event stream), uicommon, sortri, exit codes
  bins/
    twinner          # text UI + `-server` mode. MUST have zero GUI deps (runs on headless servers; checked in CI)
    twinner-gui      # GUI frontend (no toolkit dependency until the §6 spike decides)
    twinner-fsmonitor# watcher helper, same line protocol as unison-fsmonitor (uses the `notify` crate)
```

Three constraints from the Unison sources shaped this layering:

- **RPC sits below the engine.** Engine modules register their own remote commands (`Remote.registerRootCmd` in `update.ml`, `files.ml`, `copy.ml`, `stasher.ml`, ...), and the server loop dispatches whatever was registered. So `twinner-engine` depends on `twinner-rpc`, not the other way round.
- **Case and Unicode handling sit in the foundation.** `Name.compare`/`Name.hash` and `Path.compare` go through `Case`, which uses the Unicode tables and the legacy hash. These feed the sorted archive maps and `checkArchive`, so they live in `twinner-compat`/`twinner-base` rather than in `twinner-fs`.
- **The CLI uses Unison's single-dash flags** (`-server`, `-batch`), parsed by a port of `uarg.ml`, because OCaml clients start the server as `<servercmd> -server`.

- **Concurrency.** Replace the vendored Lwt with **tokio**: async process I/O for ssh, plus `spawn_blocking`/rayon for filesystem scanning and MD5. The GUI talks to the engine only through channels (commands in, events out). No shared mutable state across the UI thread.
- **Engine↔UI contract.** Model it on `uimacbridge.ml`. Its ~40 functions are a ready-made spec:
  - lifecycle: `init1(profile)`, `init2()` (connect plus update detection)
  - item access: per-item accessors (path, left/right status, direction, size, progress)
  - item actions: setters (left, right, skip/conflict, merge, newer, older, revert)
  - sync control: `synchronize()`, `abortAll()`
  - other: ignore path/name/extension, diff, and progress/status callbacks

  Defining this contract first is what makes the GUI library swappable and lowers the stakes of §5.
- **Port order:**
  1. compat and base
  2. local↔local engine plus the text UI (port of `uitext`)
  3. remote
  4. GUI
  5. platform polish

  The text UI is the first frontend *and* the test harness.
- **Verification:**
  - Differential testing against the OCaml binary on identical replica trees: same recon result, same final trees, byte-identical or semantically equal archives.
  - Mixed-version client/server matrices (Tier A).
  - Property tests on the codec and the hash.
  - Unison's own `test.ml` is small, so most coverage will be new.
- **Scope cuts:** Solaris fsmonitor, external rsync (deprecated upstream) and the pre-2.52 protocol (already removed upstream).

### Crate mapping for the C stubs and system dependencies

| Unison | Rust |
|---|---|
| Lwt / Lwt_unix | `tokio` |
| `Digest.MD5` | `md-5` |
| `props_xattr.c` | `xattr` |
| `props_acl.c` | `exacl` |
| `copy_stubs.c` (clonefile/FICLONE/reflink) | `reflink-copy` |
| `pty.c` | `portable-pty` (Unix PTYs plus Windows ConPTY) |
| `system_win*` (wide-char APIs) | Rust `std` (`OsString`/UTF-16 is native). Long-path handling still needs care |
| `osxsupport.c` (resource forks, Finder info) | `xattr` (`com.apple.ResourceFork`, `com.apple.FinderInfo`) plus a little `libc` |
| inotify / Windows watchers | `notify` (also covers FSEvents on macOS, which Unison lacked) |
| `rx.ml` glob/regex | **Port semantics.** The `regex` crate's syntax differs, so write a translator to `regex` and test it against OCaml |
| `hash_compat.c` | Hand port of the pre-4.00 hash for the specific value shapes Unison hashes (§3), with golden tests generated from the OCaml binary |

## 5. Rust GUI options, scored against Unison's needs

Versions were checked against upstream changelogs in Oct 2026.

| | **Slint** (1.18.1, Sep 2026) | **egui** (0.36.x, Sep 2026) | **Tauri 2** (WebView) | **gtk4-rs / Relm4** | **iced** (0.14.0, Dec 2025) | **Qt via cxx-qt** |
|---|---|---|---|---|---|---|
| macOS / Linux / Windows | ✅ all, winit backend by default since 1.16 | ✅ | ✅ (WebKit / WebKitGTK / WebView2) | Linux ✅. macOS/Windows workable but must bundle the GTK runtime | ✅ | ✅ |
| Look on each OS | Themed (Fluent default; Cupertino/Material; optional Qt-backed native style) | Custom, non-native | Whatever we build in HTML/CSS | Native on GNOME, foreign elsewhere | Custom | **Most native** |
| Virtualised 50k-row table | `ListView` virtualises. `StandardTableView` supports sorting (callbacks, with the app sorting the model) but only single selection (`current-row`), with text-only cells → likely **custom table on `ListView`** for multi-select and icons | ✅ `egui_extras::TableBuilder` handles very large row counts | Via JS virtual-table libraries. Needs batched IPC | ✅ `ColumnView` | `table` widget new in 0.14 | ✅ `QTableView` (best in class) |
| Native macOS menu bar | ✅ `MenuBar` (1.10), native on macOS via muda (1.16) | ❌ (add `muda` separately) | ✅ | Partial | ❌ | ✅ |
| Accessibility | ✅ AccessKit, actively improved (1.17–1.19) | ✅ AccessKit always on (since 0.34) | ✅ via the browser engine | ✅ (AT-SPI, weaker elsewhere) | ⚠ not listed in the 0.14 changelog | ✅ |
| API stability | **1.x, stable** | 0.x, frequent but manageable breaks | 2.x stable | Stable bindings | 0.x, large breaks between releases | Stable Qt, younger bindings |
| Licence | Tri-licence incl. **GPLv3** (fits twinner) or royalty-free | MIT/Apache | MIT/Apache | MIT (gtk-rs), LGPL GTK | MIT | LGPL Qt / MIT-Apache bindings |
| Build/ship burden | Low (pure Rust, no runtime) | Lowest | Medium (JS toolchain. WebKitGTK dependency on Linux) | High on macOS/Windows | Low | **High** (Qt SDK, C++ toolchain) |
| Team cost | One new DSL (`.slint`), live-preview tooling | Pure Rust. Immediate-mode mental model | Second language stack (TS/HTML) | GTK idioms. Closest to the existing `uigtk3` | Elm architecture | C++/QML knowledge |

### Assessment

- **Slint (recommended).**
  - It is the only pure-Rust option that combines a stable 1.x API, native macOS menus, real accessibility work and a low shipping burden on all three platforms. Its GPLv3 option matches the licence twinner must use anyway.
  - The declarative `.slint` files keep UI layout separate from engine glue, which suits a port where the dialogs (profile editor, wizard, SSH prompt) are mostly forms.
  - **Main risk:** `StandardTableView` sorts but is single-select with text-only cells, so the recon table (multi-select, status icons, per-row progress) probably has to be built ourselves on `ListView` with a Rust-side selection model. The spike must prove this.
- **egui (fallback).**
  - The quickest path to a working GUI. It already handles huge tables easily and has solid accessibility.
  - The trade-off is a non-native look and no native menu bar without extra work (`muda`).
  - It is a very good choice if function matters more than platform feel. It is also useful for an early internal debugging UI regardless.
- **Tauri 2 (alternative).**
  - The most mature packaging, menus, tray, dialogs and updater, and the most freedom in visual design.
  - It costs a second technology stack and a WebView/IPC boundary that the 50k-row table and progress events must be designed around. WebKitGTK on Linux is also its weakest platform.
  - Choose it if the team is stronger in web than in Rust UI.
- **gtk4-rs / Relm4.**
  - The most direct port of `uigtk3`, and excellent on Linux.
  - But it brings back the macOS/Windows packaging and "foreign look" problems Unison already has.
  - Not recommended as the cross-platform primary.
- **iced.**
  - Promising (COSMIC uses it), and `table` arrived in 0.14.
  - Still pre-1.0, with no native menus and no released accessibility. Revisit later.
- **Qt (cxx-qt).**
  - The best native widgets and table view.
  - Its toolchain and build complexity are disproportionate for a Rust-first project. Consider it only if native fidelity becomes a hard requirement.
- **Not considered viable for this app today:** Dioxus desktop, Freya, Xilem/Masonry, gpui. Each is either immature or not intended as a general-purpose toolkit.

## 6. Proposed spike (before committing to a GUI library)

Implement the same slice in **Slint** and **egui** (or **Tauri**, if the web path is attractive), fed by a fake engine stream:

1. A recon table with 50k rows: virtualised scroll, multi-select (shift/cmd), sort by path/status, and status icons. Keyboard `>` `<` `/` should apply to the whole selection.
2. Live progress updates on ~500 rows per second, without UI stalls.
3. The macOS menu bar with shortcuts, a native directory picker (`rfd`), and a modal SSH-password dialog.
4. A screen-reader smoke test (VoiceOver and Narrator) on the table.
5. Release builds and installers for macOS (.app, signed), Windows (.msi/.exe) and Linux (AppImage/Flatpak). Record binary size and startup time.

Decide on: effort for (1), smoothness of (2), and friction in (5).

## 7. Licensing and provenance

- Unison is **GPLv3**. A translation is a derivative work, so **twinner must be distributed under GPLv3**, keep the original copyright and attribution notices, and say it is derived from Unison. Every GUI option above is compatible with this; for Slint, use its GPLv3 licence option.
- Unison's `CONTRIBUTING.md` (since 2.54.0) rejects LLM-generated contributions, and the maintainer regards LLM output as a derived work of its training data. That policy covers contributions *to Unison*, not an independent fork. Still, if twinner is written with LLM assistance:
  - plan on **no upstreaming** of code;
  - keep a **distinct name and branding** (twinner already is), and don't present it as an official Unison project;
  - form your own view on the provenance question. This document is not legal advice.

## 8. Risks

| Risk | Mitigation |
|---|---|
| Subtle semantic drift in update detection or recon (data-loss class bugs) | Differential tests against OCaml on generated trees. Run in "dry-run and compare" mode before writing |
| Mismatch in Unicode/case or the legacy hash breaks mixed-version sync | Port the tables and hash exactly. Golden vectors produced by the OCaml binary |
| Platform metadata (ACLs, xattrs, resource forks, Windows attributes) | Test with real filesystems per platform in CI (APFS, ext4/btrfs, NTFS) |
| The GUI table cannot be built well in the chosen toolkit | The §6 spike, and an engine/UI contract that keeps the frontend replaceable |
| Scope creep (re-designing while porting) | Tier A compatibility locks behaviour. Improvements go behind negotiated features after v1 |
