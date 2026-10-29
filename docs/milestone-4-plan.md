# Milestone 4: GUI

*Status: done (2026-10-29) · Follows milestone 3 (remote roots, PRs #30–#46) · Source: `bcpierce00/unison` at `8772d26`*

Milestone 4 is step 4 of the port order in [the analysis](unison-port-analysis.md#4-architecture-recommendation): the GUI. It ends with `gemel-gui` doing what OCaml's GTK interface (`uigtk3.ml`, ~4.8k) does, on the engine through the session contract, with ssh's prompts in it, on macOS, Linux and Windows. Today `gemel-gui` is the Slint spike of §6 of the analysis, fed by a fake engine.

## Definition of done

1. **The GUI does what `uigtk3` does,** through `gemel-ui-api`'s session, on macOS, Linux and Windows:
   - **Starting:** roots and a profile from the command line as `uicommon` takes them, the profile manager otherwise or when a profile fails, and asking for roots when there are none (`getFirstRoot`, `getSecondRoot`).
   - **Scanning** with the file watcher, as `uigtk3` does (`findUpdates ~wantWatcher:true`), so that a rescan looks only at what changed.
   - **The main window:**
     - the items, with `uigtk3`'s columns (each replica's status, the action's icon, the item's status and progress, the path) and its multiple selection;
     - the details pane;
     - the menus (Synchronization, Actions, Ignore, Sort, Help, and Expert when `expert` is set), the toolbar and their keys;
     - the profile keys `0`–`9` (`profileKeymap`).
   - **Actions** on the selection: left to right, right to left, skip, merge, newer, older, revert, and the conflict-only variants; ignoring a path, a name or an extension; sorting (`sortri`).
   - **Syncing:** progress per item and overall, with rate and time left (`Uicommon.Stats`), the statistics window (bytes from and to the server), and the summary.
   - **The rest:** the diff window, warnings and errors as `uigtk3` shows them (`warnBox`, `fatalError`, back to the profile manager after a fatal error), confirming big deletions and merges, rescanning, and the "contacting server" state.
2. **Syncs from the GUI end as the text interface's do.** For generated scenarios, the GUI, driven headless with the default actions, leaves the same trees and archives as `gemel -batch` with the same roots: local, over ssh and on a socket.
3. **Profiles:**
   - the profile manager (`getProfile`);
   - the new-profile wizard (`createProfile`: local, ssh or socket, with native directory pickers);
   - the profile editor (`editProfile`, `editPreference`, `addPreference`), typed as `Prefs` types the preferences;
   - the documentation window.

   Profiles are written as `uigtk3` writes them, and OCaml Unison loads them.
4. **ssh's prompts through the GUI** (`termInteract`), as `uigtk3` shows them:
   - ssh runs under a pseudo-terminal on Unix and ConPTY on Windows (`terminal.ml`, `pty.c`, `win_openpty`);
   - what ssh prints at its terminal is shown in the password dialog, which takes the answer, as `uigtk3` shows it. `uigtk3` doesn't tell prompts apart by `terminal.ml`'s patterns for passwords, passphrases and host keys; only the Mac interface uses them. They are ported all the same;
   - Quit there ends the GUI as `uigtk3`'s does.
5. **`gemel-gui` is a whole gemel,** as `unison-gui` is a whole unison:
   - `-ui text` runs the text interface;
   - on X11 and Wayland systems (Linux and the BSDs), with no `DISPLAY` or `WAYLAND_DISPLAY`, it warns and runs the text interface, as `uigtk3` does;
   - `-server` and `-socket` serve.

   `gemel` stays without GUI dependencies (`check-no-gui-deps.sh`). With `-ui graphic`, it already does what OCaml's text-only build does: it says it only has the text interface, and runs it.

**Met (close-out, 2026-10-29):**
- **1, `uigtk3`'s flows:** phases 1 to 3 port them on the session, with the session's gaps filled in `gemel-ui-api`, each with tests there. `bins/gemel-gui/tests/window.rs` drives each flow headless on Slint's testing backend, on macOS, Linux and Windows in CI: starting and asking for roots, the items and their actions, syncing with progress, the statistics and the summary, the dialogs, diffs, ignoring, sorting, the Expert menu, the profile keys, Rescan and Recheck, the profile manager, the assistant, the editor, the manual, and closing the window. Phase 4a times the window through the session at 50,000 items, within the spike's pass lines. Phase 4b's screenshots draw every card and window on each system in CI, and check that a card's controls lie within it. A person looked at the real window on macOS, Linux and Windows (phases 4b to 4d; macOS's look and its own Quit were fixed on the way).
  - **Scanning with the watcher** is asked for on every detection, and Rescan scans only what it reports. The engine's tests check the watcher's scans; no window test runs with a watcher, since none is found next to the tests' programs.
- **2:** `bins/gemel-gui/tests/scenarios.rs` runs the oracle's generated scenarios through the window with the default actions, and through `gemel-gui -ui text -batch` (`gemel`'s program, phase 0), and compares what each leaves as the oracle compares it: 8 with local roots, 4 over ssh and 4 on a socket (phases 1a and 2b), on each system in CI.
- **3:** the profile manager, the assistant, the editor and the manual's window (phases 3a to 3c, and 4c's Help for the second root). `gemel-ui-api`'s session tests check that profiles are written as `uigtk3` writes them, and that OCaml loads and syncs with them (with `GEMEL_REFERENCE_UNISON`, against 2.54.0). `gemel -doc` is compared with OCaml's (`bins/gemel/tests/batch.rs`).
- **4:** the pseudo-terminal on Unix and ConPTY on Windows are in `gemel-fs`, `terminal.ml`'s parser and patterns in `gemel-ui-api` against golden vectors from `terminal.ml`, and the password dialog in the window (phases 2a to 2c). Window tests on Unix answer a shim's password prompt, wrongly then rightly, and Quit; the terminal tests run on Windows through ConPTY, and the scenarios over ssh run the shim on a pseudo-console there.
- **5:** `bins/gemel-gui/tests/program.rs` checks `-ui text`, the fallback without a display, `-server` and `-socket`; `scripts/check-no-gui-deps.sh` keeps `gemel` without GUI dependencies (phase 0).
- **Left, by decision:**
  - not ported: GTK's type-ahead search in lists, dragging a list's values to reorder them (Up and Down do it), the busy cursor (Slint has none that leaves clicks to the widgets), and "Show memory/GC stats" (Rust has no garbage collector);
  - on macOS, logging out is cancelled while gemel-gui works, as its Quit asks first (phase 4b).

Out of scope, and how each behaves until it is done:

| Cut | Behaviour in M4 | When |
|---|---|---|
| Installers, signing and notarisation (`.app`, `.msi`, AppImage or Flatpak) | `cargo build` and `cargo run` only | M5 (decided 2026-10-24) |
| A VoiceOver and Narrator pass | The screen-reader tree is checked by tests only, as in the spike | M5 (decided 2026-10-24) |
| What only `uimac` (Cocoa) has | Not ported: on macOS, `gemel-gui` is Slint's window with a native menu bar | Not planned |
| `-repeat` and notifications in the GUI (§2 of the analysis lists them, from `uimac`) | As in `uigtk3`, which has neither; it does scan with the file watcher (see the definition of done) | Not planned |
| Synchronising xattrs and ACLs | As in milestone 3 | M5 |

## Design decisions

These are made now because reversing them later would mean rewriting the GUI's plumbing.

### 1. `uigtk3` is the reference

- **Decided 2026-10-24:** the GUI follows `uigtk3`'s flows: what it asks, in which order, with which menus, keys and dialogs. `uigtk3` is OCaml's cross-platform GUI.
- **Each system's conventions:**
  - Slint's menu bar is native on macOS and drawn in the window elsewhere;
  - shortcuts use ⌘ on macOS where `uigtk3` uses Ctrl;
  - dialogs that `uigtk3` opens as windows of their own (diff, statistics, documentation) are windows of their own.
- **Where it departs from the spike:**
  - the columns are `uigtk3`'s, not the spike's (path first, size, progress);
  - sorting goes through the Sort menu and `sortri`, as `uigtk3`'s headers aren't clickable.

### 2. Only through the session

- **`gemel-gui` talks to the engine only through `gemel-ui-api`,** as now. The session (`session.rs`, ported from `uimacbridge.ml`) runs on the engine thread, and the GUI holds snapshots (`ItemView`).
- **`uigtk3` calls the engine directly** (`Recon.setDirection`, `Prefs.set`, `Remote.emittedBytes`, ...). Each call maps onto a session command. Where none exists, the session gains one, tested in `gemel-ui-api` without a toolkit. Known gaps:
  - ssh's prompts (design decision 4);
  - the bytes each side sent, for the statistics window;
  - listing preferences with their types and documentation, and writing profiles, for the editor;
  - the manual's sections (`Strings.docs`), for the documentation window.
- **The items:**
  - `recon_view`'s placeholder rows become `ItemView`s;
  - the selection stays in the GUI;
  - sorting is the session's `Sort` (`sortri`).
- **The fake engine** goes. A development flag keeps a fake source of session events (not rows), so that the 50k-row and progress-rate checks of the spike still run through the same path.

### 3. `gemel-gui` is a whole gemel

- **`bins/gemel` gains a library target:** the text interface, server mode and startup. `gemel-gui` depends on it, as `unison-gui` links `uitext` and the server.
- **The dependency goes one way,** so `gemel` stays headless, which `check-no-gui-deps.sh` keeps checking.
- **Choosing the interface:**
  - `gemel-gui` picks the GUI by default, and the text interface with `-ui text` or without a display, as `uigtk3`'s `Body.start` does;
  - **without a display** means without `DISPLAY` or `WAYLAND_DISPLAY` on X11 and Wayland systems only. `uigtk3` checks them everywhere but Windows, so a literal port would run the text interface on every Mac without XQuartz. This departs from `uigtk3` on macOS;
  - `gemel` already takes `-ui graphic` as OCaml's text-only build does.

### 4. Pseudo-terminals live below the GUI

- **Where:** `terminal.ml` is ported in `gemel-rpc` (`terminal.rs`, a stub today), beside `shell.rs`, which starts ssh. `pty.c` and the system calls go in `gemel-fs`, the only crate allowed `unsafe`, behind safe functions. The GUI only shows prompts and sends answers.
- **The session asks** through `Event::Ask`, as `termInteract`'s `userInput` and `endInput`: a question with what ssh printed, an answer, and a note that input has ended. The text interface passes no `termInteract`, as OCaml's doesn't.
- **New `unsafe`,** in `gemel-fs`, each in a function marked `#[allow(unsafe_code)]`, as its crate documentation lists them:
  - `openpty` and the child's session setup, on Unix;
  - `CreatePseudoConsole` on Windows, which needs windows-sys's `Win32_System_Console`.

### 5. The manual is data

- **The source:** `strings.ml` (~3.2k) is the manual, generated upstream from the documentation's sources.
- **The port:** a script turns `8772d26`'s `strings.ml` into a data file in `gemel-ui-api`.
- **Its uses:**
  - `gemel -doc`, which `bins/gemel` refuses today;
  - the GUI's documentation window and Help menu.

## Phases

There is one PR per phase, cut from `main` and merged before the next phase starts, with no stacking. A phase that grows past a reviewable size is split, as phases 2 to 6 of milestone 3 were. Line counts are the OCaml being ported.

### Phase 0: a whole gemel, and the GUI harness

- **A whole gemel** (design decision 3):
  - `bins/gemel`'s library target;
  - `gemel-gui`'s `-ui text`, no-display fallback, `-server` and `-socket`.
- **The harness:** a test support module in `gemel-gui` that drives the window through Slint's testing backend, with a real session on real roots: it finds elements by accessible role and label, clicks, types and waits for events.
- **Scenarios:**
  - the oracle's scenario generator (`tools/oracle/src/scenario.rs`) becomes usable from tests;
  - the harness runs a scenario through the GUI with the default actions, then through `gemel -batch`, one after the other in the same work directory, as the oracle runs its reference and candidate (archive names hash the roots' absolute paths), and compares trees and archives. It is the base for the definition of done's item 2, from phase 1 on.
- **CI** runs the GUI's tests headless on all three systems, as the spike's test does now.

**Notes (phase 0, 2026-10-24):**
- **A whole gemel:** `bins/gemel` is a library, the program, and a binary that calls it. `gemel::main` takes the graphical interface as an argument.
  - `gemel-gui` passes its window, which is then the default interface.
  - `-ui text` runs the text interface.
  - On X11 and Wayland systems without `DISPLAY` or `WAYLAND_DISPLAY`, the text interface runs, after `uigtk3`'s warning on standard error (`Util.warn`'s default printer).
  - `-server` and `-socket` serve.
  - `gemel` itself is unchanged, `-ui graphic` included.
  - `bins/gemel-gui/tests/program.rs` checks each of these.
- **The development flags** `--rows` and `--rate` are taken out of the command line before Unison reads it. The window takes no roots or profile yet.
- **`gemel-gui` is a library too:** the program with its window, so that integration tests can drive the window. The tests need the built program (`CARGO_BIN_EXE_gemel-gui`), which only a package's own integration tests get.
- **The oracle's harness is a library:**
  - it holds the scenarios, runs, recordings, the ssh shim and the comparisons, which move out of the program into `compare.rs`;
  - the shim is copied from the program the caller names (`How::harness`), not from the running one, which in a test would be the test;
  - scenarios through the GUI with a remote root will need the built `gemel-oracle` passed in, as `GEMEL_REFERENCE_UNISON` passes OCaml.
- **Driving the window** (`bins/gemel-gui/tests/support`):
  - Slint's testing backend has a platform per thread, so window tests run in parallel, and on one thread one after the other.
  - Its clock is a mock one that only the harness moves. `wait_until` moves it a frame at a time, against a deadline in real time, since what the timers wait for (another thread, a process) takes real time. A test checks that the window's timer applies what another thread sends.
  - The spike's window test moved onto the harness.
- **Scenarios through the harness:** each runs through one side, then through `gemel-gui -ui text`, in the same work directory. They are compared as the oracle compares them: output, trees, the Unison directory's files, and a recheck by the text interface. With no session in the window yet, both sides are the text interface, as a self-test (5 of 5). The window becomes one side in phase 1a.
- **The Linux image** (`docker/linux/Dockerfile`) gains netbase: `gemel-fs`'s test of ports by service name needs `/etc/services`, which CI's Ubuntu runner has. The image's CI checks had failed there since milestone 3's phase 6a.
- **ConPTY needs no new windows-sys feature:** `CreatePseudoConsole` is in `Win32_System_Console`, which the console modes already enable.

### Phase 1: the main window on the session (`uigtk3.ml`'s `createToplevelWindow`, ~1.7k)

- **Starting** (`start`, `uiInitClRootsAndProfile`, `getFirstRoot`, `getSecondRoot`): roots and profiles from the command line, and asking for roots (`Question::Roots`). The profile manager comes in phase 3; until then, a profile that isn't named is an error, and a fatal error ends the GUI after showing it.
- **Scanning** with the file watcher (`Command::Update`'s `want_watcher`), as `uigtk3` does.
- **The items** (`displayMain`, `columnsOf`, `getArrow`, `getStatusIcon`): the columns, icons and colours, the selection, `nextInteresting`, and the details pane (`updateDetails`, `ItemView.details`).
- **Actions, ignoring and sorting:** the Actions, Ignore and Sort menus, the toolbar, their keys, and which are enabled when (`updateButtons`, `grSet`).
- **Syncing** (`synchronize`, `detectUpdatesAndReconcile`, `commitUpdates`):
  - progress per item and overall (`showProgress`, `displayGlobalProgress`, `Uicommon.Stats`);
  - the statistics window (`statistics`), which needs the session to report the bytes each side sent;
  - the summary (`summaryBox`), failures, and rescanning, including the failed paths only.
- **Dialogs:** `warnBox`, `fatalError`, `okBox` and `twoBox`, confirming big deletions (`Globals.confirmBigDeletes`) and merges (`Question::Merge`).
- **The diff window** (`diffCmd`, `Command::Diff`), the Expert menu and the profile keys `0`–`9`.
- **The session's gaps** for this phase, each its own commit with tests in `gemel-ui-api`.
- **Likely split:**
  - 1a: starting, the items, actions and details;
  - 1b: syncing, progress, statistics and dialogs;
  - 1c: diff, ignoring, sorting, the Expert menu and the profile keys.

**Notes (phase 1a, 2026-10-24):**
- **The split, as done:** the scenarios need the window to sync, so 1a also has Go: `Synchronize`, then `Commit`, as `synchronize` commits. It has the dialogs that starting and syncing need as well. That leaves 1b with:
  - the progress bar, with rate and time left;
  - the statistics window and the summary;
  - Stop, Rescan, "Detect Updates and Proceed" and "Recheck Unsynchronized Items";
  - the busy cursor.
- **The session's gaps:** `uigtk3` sets directions directly in three ways the session couldn't, now `SetAction`s:
  - `Prefer` lets one version win only where the default is a conflict ("Resolve Conflicts in Favor of …");
  - `MergeDefault` merges only a path that `merge` names ("Revert to the Merging Default, if Available");
  - `Conflict` skips with no reason given (`questionAction`), which the log shows.

  `RootNames::narrow` is `roots2niceStrings 15`, for the column headings. `session::Handle` names a running session, and `gemel-ui-api` re-exports what a frontend needs of the crates below (`Trace`, `Path`, `Subpaths`, `clroot`, `unison_dir`), so `gemel-gui` depends on it alone.
- **Starting:**
  - The window sends `Start`, `LoadProfile`, `Connect`, then `Update`: with `allow_partial`, `want_watcher`, and the paths left unsynchronised.
  - A profile without roots gets `getFirstRoot`'s and `getSecondRoot`'s dialogs. Both have a text field and `rfd`'s directory picker; the second one's Help button isn't ported yet.
  - Without a profile or roots named, the window says it can't choose a profile yet, and Quit ends the program (the profile manager is phase 3).
  - Before the first items are shown, an error ends the program once its dialog is closed, since there is no profile manager to go back to. After that, it is `fatalErrorHandler`'s dialog, with Quit and Close.
  - With `-batch`, the window syncs once after detecting, as `detectCmd` does.
- **The items:**
  - `uigtk3`'s columns and multiple selection; `nextInteresting`, which with `auto` goes to conflicts only; the details pane; Detailed Information.
  - The action column draws the text interface's arrows (`action2niceString`) in `getArrow`'s colours, not GTK's pixmaps: green, orange when partial, red for a skip or an error, light blue when the action isn't the default.
  - The status column has a tick or a cross once an item is done, and its progress until then. A failed item's path has `uigtk3`'s note.
- **Actions:**
  - The Actions menu has all of `uigtk3`'s items but Show Diffs (1c); the toolbar has Left to Right, Skip, Right to Left, Merge, Details and Go.
  - Keys: `>` and `.` propagate left to right, `<`, `,` and the arrow keys as `uigtk3`'s, `/` skips, and `m`, `i`, `g` and `q` merge, show details, go and quit. Ctrl-A, Shift-Ctrl-A and Ctrl-Q are in the menus, with ⌘ on macOS.
  - The keys without modifiers are taken by the item list, not by native menu accelerators, which would take them from text fields too.
- **Dialogs** are cards over the window (see Risks):
  - `warnBox`: Close in batch mode; otherwise OK, or Quit, which unlocks the archives and ends the program with 0;
  - `fatalError` and `messageBox`;
  - confirming a merge (`twoBoxAdvanced`, without its "View details" button);
  - confirming big deletions (`confirmbigdeletes`);
  - `safeExit`'s "Premature exit" while working.
- **The harness drives the window as a user would:** OK at each warning, then Go if there is anything to propagate.
  - The window gets the oracle's flags, but not `-batch` or `-ui text`.
  - Its exit code is the text interface's for the same summary (`Summary::exit_code`).
  - The oracle runs it in the test's process (`run::outcome_with`) and compares only what it leaves (`compare::compare_state`): the trees, the Unison directory, the reference's recheck, and the exit codes. The reference's output is checked as before (`check_recheck`).
  - 8 of 8 scenarios agree with local roots. Remote roots through the window wait for phase 2: archive names depend on `UNISONLOCALHOSTNAME`, which the window in the test's process would need set, and tests run in parallel.
- **The window's tests** (`tests/window.rs`):
  - a sync through the session: the items, the details, an action, then Go;
  - `-batch`, which syncs without Go;
  - big deletions confirmed, in batch mode too: Quit ends the program, and OK leaves Go to press;
  - a merge confirmed (on Unix, where the merge command is `cat`);
  - roots asked for;
  - no profile;
  - the spike's check of 50,000 items: virtualised, selectable, and accessible. It runs without a session (`App::detached`), fed events (`App::handle`). The commands it would send are kept (`App::sent`).
- **The fake engine** and its flags, `--rows` and `--rate`, are gone. Phase 4's development flag will feed fake session events, for the check of 500 updates a second.
- **The real backend** started under Xvfb in the Linux image, on two roots, and was still running after 15 seconds, with nothing on standard error but Slint's note that the container has no D-Bus session. The testing backend never runs the event loop, the native menu bar or closing the window, so that is all this checks; looking at the window is phase 4's.
- **The file watcher** is asked for on every detection. No test runs with one yet: none is found next to the tests' programs or on CI's `PATH`. Rescans that use it come with 1b's Rescan.

**Notes (phase 1b, 2026-10-24):**
- **The session's gaps:**
  - **The bytes on the connection:** `uigtk3`'s statistics read `Remote.emittedBytes` and `receivedBytes` twice a second, even during a sync or a scan, when the session takes no commands. So the counts are kept where the window's thread can read them: a host's `Traffic`, in atomics, which every connection served on the host adds to. The client's host counts its connections to servers; `Config::traffic` names the `Traffic` to count into. OCaml keeps one count for the process; gemel's is per host, so that tests running in parallel keep theirs apart. Tested against the connections' own counts, and in the session locally (nothing), in loopback mode and against OCaml's server.
  - **`Command::ProfileUnchanged`** (`Prefs.profileUnchanged`), for Rescan.
  - **`Command::Update`'s `only`** scans only some paths, as `paths` would name them, then sets `paths` back, an interrupt included (`uigtk3`'s Recheck).
- **The progress bar** (`displayGlobalProgress`) pulses while updates are detected, then shows the fraction transferred, with the time left and the rate twice a second.
- **The statistics window** (`statistics`) is a window of its own:
  - the graphs of the rates sent and received, drawn as filled paths, without `uigtk3`'s grey edges;
  - the table of the reception rate, the data received and the file data written, on the client, the server and in all;
  - sampled from when a scan or a sync starts until 10 samples after it ends, with `uigtk3`'s smoothing.

  The file data written is worked out from each item's progress, by its direction and whether that root is local, as `showProgress` does.
- **The summary box** (`summaryBox`) comes when the replicas aren't fully synchronized, with its items under "Show details". Its message is `uigtk3`'s, checked case by case. The count of items not started is the session's: when a sync is aborted, `uigtk3` also counts conflicts it never reached.
- **Stop** aborts the sync. Without a session, a test checks that it is enabled only while a sync runs; the session's tests check what an abort does.
- **Rescan** loads the profile again only if it has changed (or never loaded), which scans everything again; otherwise it connects the roots and keeps the paths left unsynchronised. With `watch` set (off by default, as in OCaml), only those and what the watcher reports are scanned. The engine's tests check the watcher's scans. No window test runs with a watcher, since none is found next to the tests' programs.
- **"Detect Updates and Proceed (Without Waiting)"** sets batch mode once the profile is loaded again, and syncs after detecting. **"Recheck Unsynchronized Items"** looks only at the items not propagated, failed, skipped or with a problem. Big deletions aren't confirmed then, and the next detection is a whole one.
- **Working:** Rescan and Recheck while working say "Synchronizer is busy, please wait..", as `getLock` does. The busy cursor isn't ported: Slint has no cursor for the whole window that leaves clicks to the widgets under it, and Stop must stay clickable.
- **The harness** presses OK at the summary, as a user would.
- **New window tests:**
  - a sync without a session: Stop, the progress bar's fraction and text, the file data written, and the summary;
  - Rescan and Recheck on a real session;
  - the statistics against a server on a socket.

**Notes (phase 1c, 2026-10-25):**
- **The session's gaps:**
  - `Command::Ignore` takes several items. `addRegExpByPath` folds the patterns of every item selected into a set (sorted, each once), adds each, then drops the items now ignored. One item at a time wouldn't do, since dropping items renumbers the rest.
  - `Overrides::debug` sets the modules debugged, as `Trace.enable` changes them. `gemel-ui-api` re-exports `Trace.enabled` as `debugging`.
  - `ItemView::both_files`: `uigtk3` enables Diff for an item whose replicas are both files, whatever changed. `ItemView::can_diff` (the Mac interface's) also leaves out changes to properties only.
- **Show Diffs** (`diffCmd`) shows each diff in a window of its own: added lines green, removed ones red, locations cyan. Why one couldn't be shown goes to the status line. The progress bar doesn't move while a diff fetches a remote file, since the session reports no progress then.
- **The Ignore menu** ignores the paths, extensions or names of every item selected. **The Sort menu** sorts by name or size, puts new entries first, or goes back to the default. Both keep the selected rows by index, as `displayMain` does.
- **`getLock`:** a diff, an ignore or a sort holds it until the session replies, since the items' indices may be about to change. Nothing else that names an item is sent meanwhile, and the menus' keys say "Synchronizer is busy, please wait..".
- **The Expert menu**, shown with `expert`, turns debugging of all, verbose and update on and off, without scanning the items again. Every `Configure` now says what follows its reply, since that reply is also `LoadProfile`'s. "Show memory/GC stats" isn't ported: Rust has no garbage collector.
- **The profile keys** 0 to 9 (`profileKeymap`):
  - the profiles are scanned before the first one is loaded, as `startGUI` does;
  - the Synchronization menu lists the profiles bound to keys;
  - a key loads its profile, unless its file has gone ("Profile NAME not found").
- **New tests:**
  - in the session: several items ignored at once, the modules debugged changed, `both_files`;
  - in the window, without a session: an ignore and a sort hold the items until they are done;
  - in the window, on a real session: the Expert menu, the profile keys, ignoring, and a diff window (on Unix).
- **Phase 1 is done** apart from what other phases own:
  - the profile manager, the way back to it after a fatal error, and the Help menu (phase 3);
  - ssh's prompts and remote roots in the harness (phase 2);
  - checking each system's conventions by eye (phase 4). Slint's menus already take `Control` as ⌘ on macOS.

### Phase 2: ssh's prompts in the GUI (`terminal.ml`, `pty.c`, ~0.6k; `uigtk3`'s password dialogs)

- **Pseudo-terminals** (design decision 4):
  - `openpty` and starting ssh on it, on Unix (`unix_create_session`);
  - ConPTY on Windows (`win_create_session`);
  - the fallback without one (`fallback_session`);
  - closing a session (`close_session`).
- **What ssh prints:**
  - escape sequences (`parseCtrlSeq`, `processEscapes`);
  - the password, passphrase and host-key patterns (`passwordRx`, `passphraseRx`, `authenticityRx`);
  - `handlePasswordRequests`.
- **The session:** prompts as questions (design decision 4); the connection goes on once ssh stops asking.
- **The dialog** (`createPasswordDialog`, `getPassword`, `disablePassword`, `waitForPasswordWindowClosing`): text appended as ssh prints it, the field disabled once input has ended, and Quit ending the GUI (`safeExit`).
- **The harness with remote roots:** scenarios through the window over ssh and on a socket, as the definition of done's item 2 asks; phase 1a's are local only. The window runs in the test's process, and archive names depend on the client's host name, which only `UNISONLOCALHOSTNAME` overrides today. So the session takes the host name in its `Config` (a gap), and tests running in parallel needn't set the environment.
- **Tests:**
  - golden vectors for the patterns and the escape handling, generated from `terminal.ml` with milestone 1's fixture scripts (`gen_terminal.ml`);
  - a shim standing in for ssh asks for a password on its terminal, as `bins/gemel/tests/terminal.rs` does, and the GUI's dialog answers it, on Unix;
  - on Windows, the same through ConPTY, which needs no console. Whether CI's runner allows it is checked in this phase. If it doesn't, the plan says so.

**Notes (phase 2a, 2026-10-25):**
- **The split:**
  - **2a:** what is below the window, on Unix: the pseudo-terminal, `terminal.ml`, and the session's questions.
  - **2b:** the window's password dialog, and remote roots in the harness.
  - **2c:** ConPTY, which only Windows CI can test. Until then, on Windows, ssh runs as a plain process, as `fallback_session` runs it.
- **What ssh prints:**
  - `terminal.ml`'s parser of control sequences (`parseCtrlSeq`) and its patterns for OpenSSH's messages are ported, on bytes. Golden vectors come from `terminal.ml` itself (`tools/ocaml-fixtures/gen_terminal.ml`), and are the same on OCaml 4.13 and 5.3.
  - A cursor forward stands for at most 1000 spaces, where OCaml has no limit.
- **The pseudo-terminal** is in `gemel-fs` (`pty.rs`, the only new `unsafe`):
  - `openpty`, with both ends closed on exec;
  - the child's session: its standard error is the terminal, then `setsid`, `TIOCSCTTY` and echo off.

  `gemel-fs`'s documentation lists the calls.
- **`terminal.rs`:**
  - `create_session` reads the terminal on a thread of its own, since tokio can't be relied on to wake for a terminal on macOS.
  - `handle_password_requests` shows what ssh prints 50 ms after it comes, unless the connection is up by then, and writes the answers back.
  - Once the server has written something (`initConnection`'s `connReady`, now a hook of `init_connection`), nothing more is asked, and what ssh prints goes to the client's standard error.
  - If the connection fails, ssh's last words become a warning.
- **The shell:** an ssh started under a terminal is also ended as the program exits, if it still runs (`end_shells`), rather than by `safe_waitpid` when its connection closes. A connection's close handlers may run as the engine thread ends, when nothing can be started.
- **The session:**
  - `Config::prompts` sets `Rpc::set_term_interact`.
  - What ssh prints comes as `Question::Password`, with the root as the user wrote it (`canonize_root` and `open_connection` now take the name), and `Event::InputEnded` says it won't be answered any more.
  - The text interface and, until 2b, the window don't ask for prompts.
- **Tests:**
  - the vectors;
  - the pseudo-terminal: `/dev/tty` and `-echo`;
  - with a shim standing in for ssh: a password asked for until the answer is right; a banner after the connection is up not shown; ssh's last words kept when it fails; nothing written once input has ended;
  - in `gemel`'s tests, a session with prompts against a shim that asks twice, then runs the server: `gemel`, or OCaml's with `GEMEL_REFERENCE_UNISON`.
- **Generating the vectors** on OCaml 4.13 fails at `gen_transfer.ml`, which needs `Digest.MD5` (OCaml 5.2). That failure predates this phase: the terminal vectors were checked on 4.13 on their own.

**Notes (phase 2b, 2026-10-25):**
- **The password dialog** (`createPasswordDialog`):
  - the window's session runs ssh with `prompts`;
  - the dialog shows "Connecting to 'ROOT'...", what ssh printed, and a hidden entry;
  - OK sends the password, if it is still asked for. Quit ends the program, as `safeExit` does while the roots are being connected (and Escape, as closing the dialog does);
  - more of what ssh prints for the same root goes to the dialog already open (`getPassword`), whose answer is the one that counts;
  - once input has ended, the entry goes (`disablePassword`).

  `waitForPasswordWindowClosing` belongs to the profile manager (phase 3).
- **Remote roots in the harness:**
  - `Config::host_name` names the client's host for archives, instead of `UNISONLOCALHOSTNAME`, which a test can't set for one window while other tests run.
  - The oracle sets a client's run up for a client that isn't a program (`run::client_run`, `ClientRun::finish`).
  - The oracle's program has a test of its own, so that the workspace's tests build it: `gemel-gui`'s tests copy it as their ssh shim.
  - Over ssh, the window runs the shim under a terminal of its own, as it runs ssh.
  - 4 scenarios agree over ssh and 4 on a socket, besides the 8 with local roots: the definition of done's item 2 is met.
- **A leak the harness found:** a registered connection's close handler held the connection, which holds its handlers. When a session ended in a process that goes on, the connection and its transport were never freed, and an ssh server never saw its input end. The handler holds a `WeakConnection` now, and a test checks that a client lets go of its connections.
- **Window tests on Unix:** a shim asks for a password at its terminal, then runs `gemel-gui -server`. A wrong password is asked again, then the right one connects; and Quit.
- **Left for 2c:** ConPTY on Windows.

**Notes (phase 2c, 2026-10-25):**
- **ConPTY** is in `gemel-fs` (`conpty.rs`; `pty.c`'s `win_openpty`, `win_closepty` and `w_create_process_pty`). Rust's `CommandExt::raw_attribute` is still unstable, so ssh is started with `CreateProcessW` directly. Its attributes:
  - the pseudo-console, 80×25, on two pipes of its own (read-write pipes deadlock with one);
  - standard input and output on pipes, which tokio wraps;
  - standard error null, as OCaml has it, so that ssh built for Cygwin or MSYS2 finds its terminal;
  - our priority.

  The program is found with `SearchPathW`. The command line quotes arguments that have a space or a quote as OCaml's `Filename.quote` does (`make_cmdline`), trailing backslashes after the closing quote included. windows-sys gains `Win32_Security` and `Win32_System_Pipes`.
- **`terminal::create_session`** returns the session's pieces: its terminal, standard input and output, and a process that, on Windows, isn't tokio's.
  - The console closes with the session, while its output is still being read, since closing it can wait for that.
  - The process is ended half a second on, as `safe_waitpid` does on Windows.
  - Without a pseudo-console, ssh runs as a plain process.
  - `win_alloc_console` isn't ported: gemel-gui is a console program.
- **Tests:** the terminal tests run on Windows too, with a Python shim that asks at the console and reads the answer a key at a time (`msvcrt.getwch`) until a return or a newline, as Windows OpenSSH reads a password (`_getch`). A pseudo-console may draw the screen again, so there the prompt may show more than twice. The GUI's scenarios over ssh run the oracle's shim on a pseudo-console on Windows.
- **On CI's Windows runner:**
  - ConPTY works: what a program prints at the console comes out of the pseudo-console.
  - A first shim read its answer a line at a time (`set /p`), which a bare `"\n"` doesn't end. That is how Windows reads lines, not how ssh reads passwords, so the answer stays `"\n"`, as OCaml writes it.

### Phase 3: profiles (`uigtk3.ml`'s profile manager, wizard and editor, ~1.7k; `strings.ml` as data)

- **The profile manager** (`getProfile`): profiles with their roots and labels (`Command::ScanProfiles`), opening, creating, editing and deleting them. Starting without a profile opens it, and so does a fatal error, as in `uigtk3`'s `initLoop`.
- **The wizard** (`createProfile`): local, ssh or socket, with the host, user, port and directories, and native directory pickers (`rfd`). The file is written as `uigtk3` writes it.
- **The editor** (`editProfile`, `editPreference`, `addPreference`, `documentPreference`):
  - preferences by type (`Prefs.typ`), with their documentation and topics;
  - adding, changing and removing them;
  - writing the profile back as `uigtk3` does.
- **The manual** (design decision 5): the data file, `gemel -doc`, the documentation window (`documentation`) and the Help menu (`addDocSection`).
- **Tests:**
  - the profiles written, compared with what `uigtk3`'s code writes (its format strings);
  - each written profile loaded by OCaml Unison and by `gemel`;
  - `gemel -doc` compared with OCaml's.
- **Split:** 3a, the manager and the wizard; 3b, the manual; 3c, the editor. The editor (~900 lines of `uigtk3`, and the session's commands for it) was too much to go with the manual in one PR.

**Notes (phase 3a, 2026-10-25):**
- **The session's gaps:**
  - `CreateProfile` writes a profile as the assistant's `saveProfile` does: `# Unison preferences`, the label if any, the two roots, `sshargs = -C` and `fat = true` as chosen, in a file that mustn't exist, mode 0600.
  - `DeleteProfile` removes one, an error doing so dropped, as there.
  - `ForgetClRoots` is `clearClRoots`.

  Each replies with the profiles, scanned again. Tests check:
  - the text, with and without its optional lines;
  - a profile not written over;
  - loading it, and OCaml syncing with it (with `GEMEL_REFERENCE_UNISON`, checked against 2.54.0);
  - deleting one;
  - a profile loaded with its own roots once the command line's are forgotten.
- **The profile manager** (`getProfile`) is a card over the window. It has:
  - the profiles in byte order, with their descriptions, and the roots of the one selected if it has two;
  - Open, by button, double click or the list's default action;
  - Add, which opens the assistant and then selects what it made;
  - Delete, which asks first;
  - Quit or Cancel.

  It opens:
  - when the command line names no profile or roots (Quit exits 0);
  - after an error before the first items, once its dialog is closed, with the command line's roots forgotten (`initLoop`);
  - after a later error with no profile loaded (Quit exits 1);
  - from Change Profile: the toolbar, the Synchronization menu and `p`, with Cancel.

  Edit comes with 3c, and Help with 3b.
- **Startup scans the profiles first** (`updateProfileKeyMenu`), as before. A profile chosen in the manager is loaded with the command line's roots unless they were forgotten, as `initPrefs` does.
- **The assistant** (`createProfile`) is a card, a page at a time:
  - the name and description;
  - the connection (local, ssh or a TCP socket, with compression for ssh);
  - the two directories or files;
  - FAT, skipped under Windows;
  - then Apply.

  Its rules for when a page is complete, and for the second root (`makeRemoteRoot`), are ported as pure functions with unit tests. As in OCaml, a socket's port isn't checked once entered. Where its text names Unison, the program, it says Gemel, and the command to start a server is `gemel -socket xxx`. A text field next to `rfd`'s picker stands in for GTK's chooser buttons, so that a root can be typed, and tests can set it.
- **What each profile command's reply is for** is kept in order: a profile that can't be saved is said so ("Could not save profile") rather than being a fatal error, and an error deleting one is dropped. The other commands' replies are told apart by the window's state, as in phase 1; the profile commands are sent only while the manager is open, when nothing else is waiting.
- **Errors before the first items:** an error before the first items, the session's failing to start included, gets `fatalError`'s Quit and Close, and Close goes to the manager. If the session failed to start, Close ends the program, since there is no manager to go to.
- **A bug fixed:** since phase 1a, once the items were shown, an error dialog's Close ended the program and its Quit carried on. A test now presses both.
- **Window tests:**
  - the manager with no profile: its order, the selected profile's roots, and Quit;
  - the assistant making a profile, which opens and syncs;
  - Delete;
  - Change Profile to another profile;
  - an unknown profile on the command line, whose error leads to the manager;
  - a profile that can't be saved, and the error dialog's buttons.

**Notes (phase 3b, 2026-10-28):**
- **The manual as data** (design decision 5): `tools/ocaml-fixtures/gen_manual.ml`, linked against `strings.ml`, writes `Strings.docs` as Rust source, `gemel-ui-api/src/manual/docs.rs`, which `scripts/gen-ocaml-fixtures.sh` regenerates. It is OCaml's own list that is written out, not `strings.ml`'s text that is parsed: its escapes and its continued lines are OCaml's to read. Every section is UTF-8, so the texts are raw string literals. `.gitattributes` keeps the file's bytes as generated.
- **About** names gemel and its version (`Gemel File Synchronizer`, `Version 0.1.0`), and is titled "About Gemel": upstream makes it from the version it builds. The rest is Unison's manual as it is, Unison named in it.
- **`gemel -doc`**, as `main.ml` prints it:
  - `topics` lists the sections with a topic;
  - `all` prints the manual but for the Junk;
  - a topic prints the first section it names, `""` the Overview;
  - a topic that isn't one is said so, with exit code 0.

  A closed pipe ends the output quietly, as in OCaml. `verify_stdout` matters only for a Windows build without a console, which gemel doesn't have.
- **Compared with OCaml:** every topic, but for the program's name and About. Against `8772d26`'s build (in the Linux image, `target/oracle`) the rest is the same. Homebrew's 2.54.0 still has a paragraph on `mergebatch`, which `8772d26` removed with the preference; the test leaves that paragraph out of OCaml's output when it is there.
- **The documentation window** (`documentation`) is a window of its own:
  - a tab for each section with a topic, About first;
  - the section chosen, in the monospaced font, unwrapped, in a list that shows only the lines in view (Running Unison is over 2,000 lines), each page as far down as it was, as a page of GTK's notebook keeps it;
  - Close.

  There is one window. Asked for again while it is open, it goes to the section asked for, or stays where it is for a section it hasn't. Once closed, it starts again at About, each page at its top.

  Unlike GTK's, its text can't be selected, as it is a list of lines, and its tabs are chosen with the pointer or a screen reader, not the keyboard. It hasn't been looked at on a real renderer yet (phase 4): the testing backend's fonts are fake, so empty lines' heights and the wrapped tabs are still to be seen.
- **The Help menu** (`addDocSection`) has an item for each of those sections. **Help in the profile manager** opens the window at About, the page that `uigtk3`'s `""` finds.
- **Window test:** the Help menu's sections; Help in the manager; a section asked for; a tab chosen as a screen reader chooses it; each page's place; a section the window hasn't; and Close.
- **A test fixed:** `gemel-fs`'s pseudo-terminal test (phase 2a) closed the master once the child's last words had come, which hung the child up if it hadn't exited yet. It failed now and then on macOS, with the child's output whole; the master is now kept until the child has gone. Another failure was seen once, before that fix, and isn't explained: the output stopped short of the child's last words, so the reading ended early. The test now retries an interrupted read, and its assertions say how the reading ended, should it happen again.
- **Another test fixed:** `gemel-fsmonitor`'s tests (milestone 2) took the watcher's last lines once it had exited, but only those already passed on by the thread that reads them, so an error line was now and then missed (on macOS CI). They now read the watcher's output to its end.

**Notes (phase 3c, 2026-10-29):**
- **The session's gaps** (`gemel-ui-api`'s `profile_editor`, without a toolkit):
  - the preferences' types (`Prefs.typ`, with `UNKNOWN` for a name nothing registers), `nameOfType` and `defaultValue`;
  - their documentation, and `addPreference`'s tree;
  - a profile's settings as `editProfile` reads them (`ReadProfile`): includes expanded, comments left out, aliases canonical, by name in byte order, each preference's values in the order they came, `normalizeValue`'s last value for a single one, a three-state boolean at `default` or `auto` left out;
  - the text `applyCommand` writes (`WriteProfile`): `# Unison preferences`, then `name = value` for each value, as they are, over the file, which is its owner's alone if it is made;
  - `formatDoc`, a preference's documentation as GTK markup.

  As `Prefs.readAFile` does, reading a profile for the editor records its files, so that a profile loaded since is taken to have changed if they change.
- **Tests:**
  - the session reads a profile with an include, comments, an alias, values to normalize and a three-state boolean at its default, and writes it back as `uigtk3` does;
  - what it wrote reads back the same, loads, and OCaml syncs with it (with `GEMEL_REFERENCE_UNISON`, checked against 2.54.0);
  - the tree, against `Prefs.list false`;
  - `formatDoc` on examples, and no LaTeX left in any preference's documentation (`uigtk3`'s own check, left commented out there).
- **The tree** has the basic preferences by topic, then `key` and `label`; the advanced ones by topic, General last; and the expert ones. The basic General topic goes, as its preferences are all for the command line. Only the first group starts expanded, as in `uigtk3`.
- **The editor** (`editProfile`), from Edit in the profile manager, is a card over it:
  - the profile's preferences with their types and values, by name, either way from the Name header (as GTK collates, case aside), written in the order read, then added;
  - Add (`addPreference`, a card of its own), Edit or a double click (`editPreference`, another), and Delete, once asked;
  - the documentation of the preference selected;
  - Help, the manual at Running Unison;
  - Apply, OK and Cancel.

  Closed, the editor leaves the manager listing the profiles again, with its profile selected (`fillLst`).
- **A preference edited** (`editPreference`): True or False, a line of text, or a list edited a value at a time ("Edited item"). Its rules are ported as they are:
  - Add is always there, and Enter in the entry adds;
  - Edit is there when the entry differs from the value selected;
  - Up and Down are there as the value's place allows;
  - Remove selects the next value, or the one before at the end.

  OK with no values deletes the preference, once asked; a preference added needs a value. `yes` is read as true, and written back as `true` on OK.
- **The documentation** (`documentPreference`): `formatDoc`'s markup is shown in Slint's `StyledText`, as markdown:
  - `<i>` in italics, `<tt>` as code;
  - each line a line of its own;
  - punctuation escaped.

  A test checks that every documented preference's text parses, and shows the text `uigtk3` would.
- **Departures:**
  - **Deleting from the edit dialog:** emptying a list deletes the preference edited. `uigtk3` deletes the row selected, which differs when Add found the preference already set.
  - **A failed save:** a profile that can't be saved is said so ("Could not save profile") over the editor, which stays open with what was edited. On OK, `uigtk3` shows the error and closes, losing the edits.
  - **Apostrophes:** `Glib.Markup.escape_text` writes them `&#39;`, which `uigtk3`'s markup reader doesn't decode, so it shows the entity. They are apostrophes here.
  - **A profile that can't be read** for the editor is said so in a message ("Error"). In `uigtk3`, the exception goes to GTK's handler, which only prints it.
  - **Not ported:** dragging a list's values to reorder them (Up and Down do it), and the tree's and lists' keys. Their rows are chosen with the pointer or a screen reader; keys come with phase 4.
- **Window test:**
  - Edit from the manager, and the order either way;
  - a boolean changed, its buttons chosen as a screen reader chooses them;
  - a line of text, which Enter takes;
  - a list added from the tree;
  - a preference deleted, once asked;
  - Help;
  - Apply, with the file checked;
  - OK, back to the manager with the profile selected;
  - a profile that can't be saved.
- **True or False** takes the choice from the radio group's `selected`: a button's `toggled` doesn't come when a screen reader chooses it.
- **Not yet seen by eye** (phase 4): the styled text's fonts, and the cards' layout. It wraps, as a `ScrollView`'s content is as wide as the view unless one word is wider.

### Phase 4: each system, and the rest

- **Each system's conventions:** shortcuts (⌘ on macOS), the menu bar (native on macOS, in the window elsewhere), and `rfd`'s pickers on each system.
- **By eye:** screenshots on macOS, on Linux under Xvfb, and on Windows. They need an unlocked session and a person (see Risks).
- **The spike's checks, through the session:** 50k items, and progress at 500 updates a second.
- **Odds and ends** found on the way, each with a decision, as in milestone 3's phase 6b. Left for this phase by phase 3:
  - keys for the documentation window's tabs, the editor's tree and its lists, which take the pointer and a screen reader only;
  - by eye: the documentation window (empty lines, the wrapped tabs, scrolling), the editor's cards, and its styled text's fonts.
- **Split:** 4a, what can be checked headless (the spike's checks, the keys, the close button); 4b, the look on each system, which needs a person: the conventions as they show (the native menu bar, ⌘Q, the pickers), and the screenshots.

**Notes (phase 4a, 2026-10-29):**
- **The spike's checks, through the session:** `examples/bench.rs` in `gemel-gui`, an example rather than the development flag the plan had, so that the program has none. Events come from `gemel_gui::fake`, which the tests use too.
  - **Without a session**, it times the main thread's work per frame as the spike did: Slint's software renderer at 1000×700 and 60 frames a second, scrolling 7 rows a frame through 50,000 items, progress at 500 or 5,000 updates a second, each frame taking the events due as the window's ticks take them. The frames that carry select all, a sort, `>` on every item, and the replies to the last two, are reported one by one.
  - **With `--session`**, the window runs on a session over 50,000 new files in a temporary directory: finding and showing them, `<` on every item with the replies, and a sort, through the engine.
- **Pass lines,** set before measuring: 16.7 ms at the 99th percentile per frame; 100 ms for a frame carrying a one-off operation or its replies; no frame over 16.7 ms once the items are shown, through the session.
- **What the benchmark found:** each reply to an action on an item selected showed the details pane again, and with every item selected that looked at all of them. The frame that took the replies to `>` on 50,000 items took 1,981 ms. The pane is now shown once the events taken with a tick are applied: 32 ms. No session command was needed; sending only the changed items, the fallback, isn't.
- **The numbers** (release build, Apple M4 Max, macOS; Linux and Windows not measured):

  | | 500 updates/s | 5,000 updates/s | The spike |
  |---|---|---|---|
  | Work per frame, p50 / p99 / max | 3.4 / 5.4 / 6.2 ms | 3.6 / 4.9 / 5.6 ms | 3.9 / 6.9 / 7–11 ms |
  | Select all / `>` / its replies | 6.7 / 4.5 / 32 ms | 7.4 / 7.1 / 36 ms | 4–8 / 5–6 ms / (none) |
  | Sort / its reply | 6.2 / 32 ms | 4.4 / 32 ms | 19–20 ms |

  Through the session: 50,000 items found and shown in 1.5 s, with no frame over 3.8 ms; `<` on all of them, with the replies, in 0.11 s; a sort in 0.09 s, no frame over 6 ms. The engine's detection is in the first figure; the window's share is the frames.
- **Keys** (`KeyedList`, a frame and keys around a `ListView`):
  - The profile manager's, the editor's and the edit dialog's lists, and the tree preferences are added from: Up and Down choose the row before or after, which is kept in view. Enter opens the profile, edits the preference or adds it.
  - A list takes the keys when its card opens, and when a row is clicked. Escape still closes the card.
  - The tree: Right expands a row, or goes into it; Left collapses it, or goes to the row it is under (`lst_expand_by_keyboard`). GTK's type-ahead search isn't ported.
  - The documentation window: Up and Down, or Ctrl+Page Up and Page Down, go to the tab before or after; Escape closes it.
- **The close button** already went through `safeExit`; a test now checks it, while idle and while working.
- **Window tests:** the keys in the manager, the editor, the edit dialog, the tree and the documentation window; Escape over a list; the close button.

**Notes (phase 4b, 2026-10-29):**
- **The screenshots:** `tests/screenshots.rs` in `gemel-gui` drives the window through `uigtk3`'s flows, as the window tests do, on Slint's testing backend with its software renderer (`support::init_drawn`), in the system's fonts. It writes 48 PNGs to `target/tmp/screenshots`:
  - the main window: the items, an action changed, all selected, a sync's progress, synced;
  - each card: a warning, an error, "Premature exit", the summary with its details, ssh's password, the roots (local, ssh, socket), the profile manager, the assistant's pages, and the editor's (its list, each kind of value, the tree, the longest documentation scrolled to its end, deleting);
  - the other windows: a diff, the statistics, and each page of the manual.

  CI keeps them for each system (`screenshots-<os>`). Each shot waits for the style's animations to end, and checks that it isn't blank and that a card's controls (buttons, fields, lists, choices) lie within the card. The menu bar is Slint's own, drawn in the window as on Linux and Windows; macOS's native one and `rfd`'s pickers aren't drawn.
- **What they showed, fixed:**
  - **The lists over their cards:** `KeyedList` (4a) is a `FocusScope`, which isn't a layout, so its framed list had no height in its card's layout; the profile manager's, the editor's and the tree's lists were drawn over their labels. It now has the list's height. Checked: the manager's list lies between its label and the summary.
  - **Cards cut short:** in a layout in a card, Slint 1.18 counts a word-wrapped text as one line, so the assistant's ssh page put its buttons below the card. Such texts are `Para`s now, at least as tall as their own preferred height (dialog 1's message already was).
  - **The second root's fields:** its row of kind, host, user and port was wider than the card. `uigtk3` has the kind's radio buttons in a row of their own; here the kind, then the host and the port, then the user, each a row.
  - **The progress text** was drawn over the style's progress bar, a thin line. It is above it now.
- **macOS's own Quit:** AppKit quits with `terminate:` (the application menu's Quit, which Slint always adds with Cmd-Q; the Dock's; logging out). winit's delegate has no `applicationShouldTerminate:`, so AppKit ended the program at once: measured while contacting a server, exit 0 with no question, and mid-sync it would leave the archives locked. `src/macos.rs` adds the method to the delegate's class: while nothing is going on, AppKit ends the program at once, as Quit would; otherwise it cancels, and the window's Quit runs (`safeExit`), which asks. It is gemel-gui's only `unsafe` (decided 2026-10-29), through `objc2`, which `rfd` and Slint's menus already use. Measured again: while busy, "Premature exit" comes up and the program goes on (seen in its window); while idle, it ends with 0, nothing locked. Logging out is cancelled only while gemel-gui works.
- **Checked, nothing to change:**
  - a card's text fields keep Ctrl-A (Cmd-A): Select All is disabled whenever such a card can be up (a test);
  - the longest preference documentation scrolls to its end.
- **Left for a person,** on each system, with the real backend:
  - the look against the PNGs (fonts, high-DPI);
  - macOS's menu bar: the application menu (its name, About), and which Quit a typed Cmd-Q reaches (both are Cmd-Q, and both ask now);
  - `rfd`'s pickers (Browse… in the roots and the assistant);
  - a run on Windows, and on Linux under X11 and Wayland.

**Notes (phase 4c, 2026-10-29):**
- **The second root's Help** (`getSecondRoot`'s, left by phase 1a): "Picking roots", `helpmessage`, in a card over the root selection's. As in the assistant, the command it names to start a server is `gemel -socket xxx`. The root selection's fields are now emptied as a root is asked for, not each time its card is shown, so that what was typed is still there once Help is closed. Checked in the window test of the roots, and drawn.
- **CI's screenshots** from phase 4b's PR, on Linux (DejaVu) and Windows (Segoe UI), were looked at: the cards fit and the text wraps as on macOS. Nothing to change.
- **Left** before the milestone is done: phase 4b's checks by a person, on each system with the real backend.

**Notes (phase 4d, 2026-10-29):** from the first look by a person, on macOS.
- **Build warnings:** the statistics, diff and manual windows had a callback named `close`, which shadows Slint's own `close()`; the Slint compiler warned at each build. It is `close-window` now.
- **macOS's look:** every system had Slint's Fluent style, Windows' look. On macOS, `build.rs` now picks Slint's `cupertino` style (`SLINT_STYLE` still wins); elsewhere it stays Fluent, which Slint picks without Qt. The lists are the window's own, so they follow the style by its palette: a selected row is in the style's accent, with its text in the accent's foreground, as Slint's own list items are in the macOS style (Fluent's accent is close to the selection colour it had); and the list boxes, the details pane and the documentation have the style's control background. CI draws the macOS screenshots in that style.
- **Messages cut off:** a message card's text was the only child of its `ScrollView`, which Slint centres, so a line wider than the view (the roots' canonical names in the warning that there are no archives) was cut off on both sides. The message, and ssh's text in the password card, are now laid out in their views. Seen on the real window; the screenshots' warning now has a long path, and a check that the message starts within its card.

## Verification

- **The session's additions:** tests in `gemel-ui-api`, without a toolkit; prompts against an ssh shim. The bytes each side sent are checked against the bytes on the connection, not against OCaml, whose text interface doesn't print them.
- **The GUI:** Slint's testing backend, headless, on all three systems in CI:
  - elements found by accessible role;
  - the screen-reader tree checked as in the spike.
- **Behaviour:** the harness compares syncs through the GUI with `gemel -batch`'s, which the oracle compares with OCaml's.
- **Prompts:**
  - golden vectors from `terminal.ml`;
  - pseudo-terminal tests on Unix;
  - ConPTY on Windows, if the runner allows it (phase 2).
- **Profiles and the manual:** compared with what `uigtk3` writes and with OCaml's `-doc`, and loaded by OCaml.
- **By eye:** checked locally on each system, with an unlocked session; not in CI.

## Risks and open questions

| Risk or question | Plan |
|---|---|
| No oracle compares GUIs, so the GUI can drift from `uigtk3` | The definition of done lists `uigtk3`'s flows. The harness checks every sync against `gemel -batch`, and each dialog against `uigtk3`'s code |
| Slint has no in-window modal | The spike's scrim and card where `uigtk3` blocks the main window. Windows of their own where `uigtk3` opens them |
| ConPTY behaves differently from Unix pseudo-terminals | `terminal.ml`'s Windows code is the reference. A Windows test runs if CI's runner allows it (phase 2) |
| Snapshots of 50k items, and sorting them on the engine thread | The spike's numbers were for 50k rows in the GUI. Phase 4 measures them through the session; sending only the changed items is the fallback |
| Pseudo-terminals add `unsafe` | Kept in `gemel-fs`, behind safe functions, with tests on each system |
| Checking by eye and with screen readers needs an unlocked session and a person | Screenshots drawn headless by the tests, on each system in CI (phase 4b); the real backends looked at by a person. The VoiceOver and Narrator pass moves to M5 (decided 2026-10-24) |
| Slint is pinned at 1.18.1 | It stays pinned unless a phase needs a newer release; that upgrade is a commit of its own |
