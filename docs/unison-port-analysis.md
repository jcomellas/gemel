# Gemel: Porting Unison to Rust — Analysis and Recommendation

*Status: Tier A adopted (2026-10-18) · Slint adopted for the GUI under its GPL-3.0-only licence (2026-10-19), VoiceOver check pending (§6) · Source analysed: `bcpierce00/unison` at `8772d26` (post-2.54.0) · Date: 2026-10-17*

## 1. Summary

- **Most of the work is the sync engine, not the GUI.** About 30k lines of OCaml and C are the engine. The GUIs are about 9k lines: GTK3 plus Cocoa.
- **The first decision is compatibility, before the GUI library.** Either gemel talks to and shares archives with existing Unison installations, or it is a clean-room tool with the same semantics. We recommend **wire- and archive-compatibility with Unison ≥ 2.52** as the v1 target. It is cheaper than it looks (§3), and it gives us the OCaml binary as a test oracle and a way for users to migrate gradually.
- **Build the engine as a GUI-free library**, with a text UI and headless `-server` mode first and the GUI as a separate frontend. Unison's own macOS bridge (`uimacbridge.ml`) already defines a clean engine↔GUI contract we can copy.
- **GUI recommendation: Slint**, with **egui** as the fallback. **Tauri 2** is the alternative if the team prefers a web frontend. The §6 spike of the main recon table (50k rows) in both Slint and egui confirmed Slint; a manual VoiceOver check is still pending (§6, "Spike results").
- **Licensing:** a translation is a derivative work, so **gemel must be GPLv3** and must keep Unison's copyright notices (§7).

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
- **Feature negotiation** (`features.ml`) lets gemel advertise only the features it implements.

**Recommendation:** target **Tier A for v1**. Keep the bit-exact compatibility code (the Umarshal codec, legacy hash, OCaml-compatible Unicode tables) in `gemel-compat`, and the RPC v1 mechanism in `gemel-rpc`. A native protocol v2 (e.g. stronger hashes, serde) can then be negotiated later without touching the engine.

> **Decision (2026-10-18): Tier A adopted.** gemel v1 is wire- and archive-compatible with Unison ≥ 2.52 (RPC version 1, archive format 23).

## 4. Architecture recommendation

This layout is implemented in the workspace scaffold. Each crate depends only on the ones listed above it:

```
gemel/
  crates/
    gemel-compat   # bit-exact formats: umarshal codec, legacy OCaml hash, Unicode tables, fsmonitor protocol
    gemel-base     # prefs/.prf profiles, uarg flags, glob (rx.ml), features registry, case, name, path, clroot
    gemel-fs       # OS layer: system, fspath, os, props (xattr/ACL), macOS metadata, reflink, fingerprints
    gemel-rpc      # RPC framing + version handshake, command registry/dispatch, ssh/socket connections, PTY
    gemel-engine   # update detection + archives, recon, sortri, transport, transfer (rsync), stasher, moves, negotiate
    gemel-ui-api   # the engine<->frontend contract (commands + event stream), uicommon, exit codes
  bins/
    gemel          # text UI + `-server` mode. MUST have zero GUI deps (runs on headless servers; checked in CI)
    gemel-gui      # GUI frontend, built with Slint (chosen by the §6 spike)
    gemel-fsmonitor# watcher helper, same line protocol as unison-fsmonitor (uses the `notify` crate)
```

Three constraints from the Unison sources shaped this layering:

- **RPC sits below the engine.** Engine modules register their own remote commands (`Remote.registerRootCmd` in `update.ml`, `files.ml`, `copy.ml`, `stasher.ml`, ...), and the server loop dispatches whatever was registered. So `gemel-engine` depends on `gemel-rpc`, not the other way round.
- **Case and Unicode handling sit in the foundation.** `Name.compare`/`Name.hash` and `Path.compare` go through `Case`, which uses the Unicode tables and the legacy hash. These feed the sorted archive maps and `checkArchive`, so they live in `gemel-compat`/`gemel-base` rather than in `gemel-fs`.
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

  The text UI is the first frontend *and* the test harness. Milestones 1 and 2 are done ([milestone-2-plan.md](milestone-2-plan.md)); milestone 3 is planned in [milestone-3-plan.md](milestone-3-plan.md).
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
| `rx.ml` glob/regex | **Ported as is** (milestone 1): the derivative-based DFA, not a translation to the `regex` crate, tested against OCaml |
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
| Licence | Tri-licence incl. **GPLv3** (fits gemel) or royalty-free | MIT/Apache | MIT/Apache | MIT (gtk-rs), LGPL GTK | MIT | LGPL Qt / MIT-Apache bindings |
| Build/ship burden | Low (pure Rust, no runtime) | Lowest | Medium (JS toolchain. WebKitGTK dependency on Linux) | High on macOS/Windows | Low | **High** (Qt SDK, C++ toolchain) |
| Team cost | One new DSL (`.slint`), live-preview tooling | Pure Rust. Immediate-mode mental model | Second language stack (TS/HTML) | GTK idioms. Closest to the existing `uigtk3` | Elm architecture | C++/QML knowledge |

### Assessment

- **Slint (recommended).**
  - It is the only pure-Rust option that combines a stable 1.x API, native macOS menus, real accessibility work and a low shipping burden on all three platforms. Its GPLv3 option matches the licence gemel must use anyway.
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

### Spike results (2026-10-18)

**Recommendation: Slint, as §5 proposed.** The main risk is retired. A custom table on `ListView` does everything the recon table needs: multi-select, sorting, status icons, live progress and screen-reader semantics. egui starts faster, uses less CPU per frame and makes a smaller binary. But its gaps are structural: it has no menu bar of its own, row semantics for screen readers need patching through egui internals, and it is non-native everywhere. Slint's gaps are small and local: there is no in-window modal and no built-in multi-select table. Before committing, do a real VoiceOver pass (steps below), since that could not be run here.

**What was built.** The slice was built twice, as throwaway crates. They were committed in `d33970e` and removed in the next commit, which started the real `bins/gemel-gui` from the Slint spike (see "Afterwards" below). To reproduce the numbers in this section, `git checkout d33970e` first. That commit predates the rename to gemel, so its crates still carry the old project name, twinner:

- `bins/twinner-gui-spike-slint`, on Slint 1.18.1;
- `bins/twinner-gui-spike-egui`, on egui/eframe 0.36.2 (0.37.0 shipped on 2026-10-15, too late for the spike);
- `crates/twinner-gui-spike-model`, shared by both. It has no toolkit dependencies and holds the fake engine feed, the row model, sorting, and a selection keyed by stable row id (so it survives sorting). Each UI only renders this state and forwards input, so the comparison measures the toolkits rather than two different models.

Each spike has a test that drives the real UI with synthetic input and checks the screen-reader tree. The egui test runs a bare `egui::Context` and reads AccessKit output. The Slint test uses Slint's headless testing backend. Both tests do the same steps: click, shift-click, cmd-click, then `>`. They check:

- the selection;
- the directions;
- the per-row label, selected state and position;
- that `>` `<` `/` are ignored while the SSH modal is open.

At `d33970e`, run the spikes with:

```bash
cargo run --release -p twinner-gui-spike-slint
```

or `-p twinner-gui-spike-egui`. Options: `--rows N`, `--rate N`, `--bench SECS [--headless]`, `--exit-after-first-frame`.

**Test conditions.** The machine was an Apple-silicon Mac on macOS 27, with Rust 1.99. **The screen was locked and the display asleep for the whole run.** This has three consequences:

- Nothing was checked by eye.
- Windowed frame rates are not meaningful: Slint stops rendering an occluded window, while egui keeps calling its UI code.
- Smoothness (criterion 2) was therefore measured headless: the main-thread work per frame, at 60 Hz, while auto-scrolling 7 rows a frame through 50k rows, with the feed running.

| Criterion | Slint 1.18.1 | egui 0.36.2 |
|---|---|---|
| **1. Table (50k rows)** | Custom table on `ListView` (virtualised: about 30 rows exist at a time). A Rust `Model` builds rows on demand and calls `row_changed` per changed row. Modifier clicks via `TouchArea.pointer-event` (⌘ arrives as `control`). Keys go through a `FocusScope`. Icons and progress bars are plain elements. Sort is done in Rust, then `reset()`. | `egui_extras::TableBuilder` virtualises out of the box. Selection, icons (painted) and progress (`ProgressBar`) are a few lines each. Keys come from `Event::Text`. |
| Code size | 455 lines of Rust plus 458 of `.slint` | 663 lines of Rust (both counts include the headless bench and exclude tests and the ~840-line shared model) |
| **2. Main-thread work per frame** (headless, 500 updates/s) | p50 3.9–4.0 ms, p99 6.8–6.9 ms, max 7–11 ms. Includes **software** rendering of the whole 1000×700 frame, so it is an upper bound: the real app renders on the GPU with femtovg. | p50 2.2 ms, p99 2.6–3.0 ms, max 3.5–4.4 ms. Layout and tessellation only, no rasterisation. |
| At 5,000 updates/s (10×) | p50 3.9 ms, p99 6.9 ms (unchanged) | p50 2.3 ms, p99 3.0 ms (unchanged) |
| One-off frames: select all 50k / sort / `>` on 50k | 4–8 ms / 19–20 ms / 5–6 ms | 2.5–2.7 ms / 16–19 ms / 2.5–3 ms |
| Windowed, visible (`--bench 10`, 3 runs, 2026-10-19) | 59.4–59.5 fps; frame interval p50 16.7 ms, p99 18–23 ms. One ≈56 ms frame per run (cause not yet found) | 52–60 fps; p50 16.7 ms, p99 19–21 ms, max 47–48 ms. One run had a single 1.35 s stall that didn't recur |
| While occluded | Stops rendering (the winit backend skips drawing an occluded window) | Keeps repainting on every feed wake-up (≈100/s). Long syncs would need repaint throttling |
| **3. Menu bar** | Built-in `MenuBar` with `@keys(...)` shortcuts, native on macOS. On Windows and Linux Slint draws it in the window | None in eframe. Added `muda`: create it after NSApp exists, then forward events. macOS only: Windows needs accelerator translation in winit's message loop, and on Linux `muda` pulls in GTK3 |
| Directory picker | `rfd::AsyncFileDialog` with the window as parent, so it opens as a sheet and the feed keeps running. Same code in both | Same |
| SSH-password modal | No in-window modal (`Dialog` is a separate window). Built a scrim with a card instead (~50 lines). The password field takes focus, so the table never sees keys | `egui::Modal` is built in. Table keys had to be suppressed by hand while it is open |
| **4. Screen reader** (AccessKit tree, checked by the tests) | Correct by default: `ListView` is a `ListBox` with an item count of 50k, and each row is a `ListBoxOption` with label, selected state and index. Set declaratively on the row | Wrong by default: selection becomes "toggled" (a button state) and rows aren't items of a list. Fixed by patching AccessKit nodes and tagging the rows' parent `Ui`, found through the cell's `ui.stack()`. That depends on `TableBuilder` internals. The other five cells per row also had to be hidden, or a screen reader would step through ~140 cell containers between the 28 visible rows |
| VoiceOver / Narrator | **Not run.** Needs an unlocked session and a person (manual steps below). Narrator needs Windows | **Not run** (same) |
| **5. Release binary** (arm64, default features) | 17.5 MB; 10.5 MB stripped with fat LTO, 1 codegen unit and `panic=abort` | 14.4 MB; 8.3 MB with the same settings (for scale: headless `gemel` is 0.4 MB) |
| Startup, median of 5 warm runs | 195 ms to first rendered frame; 251 ms process wall time to exit | 109 ms to first `ui()` call (before painting); 166 ms process wall time |
| Build friction | One build script (`slint-build`). Generated code needs `#![allow(missing_docs, unsafe_code, clippy::all, clippy::todo)]`. Element IDs must not shadow enum names (`table`, `password`). Tests need `with_debug_info(true)` | None beyond Cargo |

The sort frame is mostly the shared Rust sort of 50k rows: the same code in both toolkits, costing one dropped frame in either. The fix belongs in the model: precomputed sort keys, or sorting in the background. The same sort takes 4–5 ms when timed at startup. The in-loop figure is higher, probably because the bench thread sleeps between frames and runs at lower clocks.

**Effort notes.**

- **egui** was quicker to get on screen: the whole UI compiled on the first attempt, and the table itself was the easy part. The time went on what egui doesn't have:
  - a native menu (`muda`, macOS-only glue);
  - list-box semantics for screen readers. That needed reading egui's AccessKit mapping and AccessKit's macOS adapter. The fix relies on how `TableBuilder` nests `Ui`s.
- **Slint** took longer up front: a new DSL, the custom table, a `Model` adapter and a modal. But each step was declarative and local, and the menu bar and screen-reader semantics were correct as soon as they were written.
- Neither toolkit was a dead end. The single-select, text-only `StandardTableView` was not needed.

**Packaging notes** (not built here; the brief narrowed §6 item 5 to notes):

- **Both:** a single self-contained binary, with no runtime to ship.
- **macOS:**
  - A `.app` via `cargo-packager` or `cargo-bundle`. The bundle's `Info.plist` sets the menu-bar app name, which is currently the binary name.
  - Signing and notarisation work the same for both toolkits, and need a Developer ID.
- **Windows:**
  - An `.msi` via `cargo-wix`, or NSIS via `cargo-packager`. Both toolkits use the windows-sys crates; there are no C build dependencies.
  - Slint's default renderer is OpenGL (femtovg), with the software renderer compiled in as a fallback for VMs and RDP. eframe defaults to wgpu (DX12/Vulkan).
  - The egui menu bar would need extra work.
- **Linux:**
  - AppImage or Flatpak. The default `rfd` backend is the XDG portal (`zbus`, no GTK), which suits Flatpak.
  - **Slint needs `pkg-config` and `libfontconfig-dev` at build time**, via `fontique` → `yeslogic-fontconfig-sys`. Debian trixie only has `libfontconfig-dev`; some older releases call it `libfontconfig1-dev`. `RUST_FONTCONFIG_DLOPEN` doesn't avoid it: `fontique` 0.11 fails to compile with it. CI installs both on the Ubuntu runner (decided 2026-10-19). Ubuntu 24.04 also has `libfontconfig-dev`; there, `libfontconfig1-dev` is a dummy package.
  - **Linux build image:** `docker/linux/Dockerfile` (Rust 1.99.1 on Debian trixie) has those build dependencies plus X11, Mesa and Xvfb for headless smoke runs. `scripts/linux-docker.sh` builds it and runs the CI checks with the checkout mounted; extra arguments run any other command instead. Build output and the Cargo registry live in named Docker volumes. The container runs as root.
  - egui loads X11, Wayland and xkbcommon at runtime and needs no build-time system packages.
  - Verified on 2026-10-19 in that image on **arm64** Linux (Docker on Apple silicon): `cargo fmt --check`, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (including both spikes' input and accessibility tests) and `check-no-gui-deps.sh` pass. Under `xvfb-run`, both spikes start on X11 with Mesa and render a first frame: Slint after 453 ms, egui after 79 ms.
  - The image is for reproducing Linux builds locally. CI keeps the GitHub Ubuntu runner, adding one `apt-get` step, and is the first x86_64 Linux run. `GEMEL_PLATFORM=linux/amd64 scripts/linux-docker.sh` emulates x86_64 if one is ever needed locally (slow).
  - Both spikes pass `clippy -D warnings` for `x86_64-pc-windows-msvc`, cross-checked from macOS. Neither was linked or run on Windows.
- **Licence:**
  - **Decided 2026-10-19: use Slint under its GPL-3.0-only option.** gemel's sources stay GPL-3.0-or-later. The GUI binary, which links Slint, is distributed under GPLv3 only. The headless `gemel` binary doesn't link Slint and is unaffected.
  - Slint's royalty-free desktop licence was considered and not needed.
  - egui is MIT/Apache.

**Not verified.** These need an unlocked session. The screen was unlocked on 2026-10-19 for the windowed bench in the table above; the items below are still open:

- Screenshots of both windows (2026-10-19) show the tables render as intended. Getting there took two egui fixes:
  - Status icons wrapped above their labels until the cell got an explicit horizontal layout.
  - egui's bundled fonts have no `▲`/`▼` or `↑`/`↓`, so the sort indicator rendered as a missing-glyph box. It now uses `⬆`/`⬇` from egui's emoji font. A real egui app would need to bundle a fuller font.
  - Slint rendered correctly first time, using the system font.
- The native menus and the `rfd` sheet were built but never opened. The SSH modal was only driven in tests.
- The **VoiceOver smoke test**. For each spike:
  1. Run the spike.
  2. Turn on VoiceOver with ⌘F5.
  3. Move into the table with VO-arrows. The list should be announced as having 50,000 items, and a row as "*path*, left …, *direction*, right …, *size*, N% done".
  4. Shift-click and ⌘-click a few rows. VoiceOver should report them as selected.
  5. Press `>`. The focused row's label should change to "left to right".
  6. Open the SSH dialog (⌘K). The password field should be focused and announced as a secure text field.
- Without VoiceOver: `swift scripts/macos-ax-dump.swift PID` prints a running app's accessibility tree, the same API VoiceOver reads. It shows list rows and selection counts, and each menu item's key equivalent. It needs the Accessibility permission for the app that runs it.

**Afterwards (2026-10-19).** The Slint spike became `bins/gemel-gui`:

- `ui/app.slint` is the spike's window, titled "gemel".
- `src/recon_view.rs` holds the recon view model: rows, sorting and the multi-selection. Selection stays in the GUI. The item types are placeholders until `gemel-ui-api` defines the reconciliation items, and sorting is due to move to `gemel_engine::sortri`.
- `src/fake_engine.rs` holds the spike's fake feed. It is the GUI's only data source until the engine exists, and must not depend on `gemel-engine` directly.
- The bench and startup-timing flags were dropped. `--rows` and `--rate` remain as development flags.
- The input and accessibility test came across unchanged.
- The egui spike and the shared spike crate were deleted; they remain in `d33970e`.
- Verified on macOS and in the Linux image (arm64): fmt, workspace clippy `-D warnings`, tests and `check-no-gui-deps.sh` pass. Clippy also passes for `x86_64-pc-windows-msvc`. The window renders as in the spike, both on macOS (checked from a screenshot) and under Xvfb.

## 7. Licensing and provenance

- Unison is **GPLv3**. A translation is a derivative work, so **gemel must be distributed under GPLv3**, keep the original copyright and attribution notices, and say it is derived from Unison. Every GUI option above is compatible with this; for Slint, use its GPLv3 licence option. That option is GPL-3.0-only, so the GUI binary is GPLv3-only (§6, decided 2026-10-19).
- Unison's `CONTRIBUTING.md` (since 2.54.0) rejects LLM-generated contributions, and the maintainer regards LLM output as a derived work of its training data. That policy covers contributions *to Unison*, not an independent fork. Still, if gemel is written with LLM assistance:
  - plan on **no upstreaming** of code;
  - keep a **distinct name and branding** (gemel already is), and don't present it as an official Unison project;
  - form your own view on the provenance question. This document is not legal advice.

## 8. Risks

| Risk | Mitigation |
|---|---|
| Subtle semantic drift in update detection or recon (data-loss class bugs) | Differential tests against OCaml on generated trees. Run in "dry-run and compare" mode before writing |
| Mismatch in Unicode/case or the legacy hash breaks mixed-version sync | Port the tables and hash exactly. Golden vectors produced by the OCaml binary |
| Platform metadata (ACLs, xattrs, resource forks, Windows attributes) | Test with real filesystems per platform in CI (APFS, ext4/btrfs, NTFS) |
| The GUI table cannot be built well in the chosen toolkit | The §6 spike, and an engine/UI contract that keeps the frontend replaceable |
| Scope creep (re-designing while porting) | Tier A compatibility locks behaviour. Improvements go behind negotiated features after v1 |
