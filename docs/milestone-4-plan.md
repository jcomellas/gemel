# Milestone 4: GUI

*Status: in progress (phase 1a, 2026-10-24) · Follows milestone 3 (remote roots, PRs #30–#46) · Source: `bcpierce00/unison` at `8772d26`*

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
   - what ssh prints is shown, and a password, passphrase or host-key question gets the password dialog, recognised as `terminal.ml` recognises them;
   - Quit there ends the GUI as `uigtk3`'s does.
5. **`gemel-gui` is a whole gemel,** as `unison-gui` is a whole unison:
   - `-ui text` runs the text interface;
   - on X11 and Wayland systems (Linux and the BSDs), with no `DISPLAY` or `WAYLAND_DISPLAY`, it warns and runs the text interface, as `uigtk3` does;
   - `-server` and `-socket` serve.

   `gemel` stays without GUI dependencies (`check-no-gui-deps.sh`). With `-ui graphic`, it already does what OCaml's text-only build does: it says it only has the text interface, and runs it.

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
- **Likely split:** 3a, the manager and the wizard; 3b, the editor and the manual.

### Phase 4: each system, and the rest

- **Each system's conventions:** shortcuts (⌘ on macOS), the menu bar (native on macOS, in the window elsewhere), and `rfd`'s pickers on each system.
- **By eye:** screenshots on macOS, on Linux under Xvfb, and on Windows. They need an unlocked session and a person (see Risks).
- **The spike's checks, through the session:** 50k items, and progress at 500 updates a second.
- **Odds and ends** found on the way, each with a decision, as in milestone 3's phase 6b.

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
| Checking by eye and with screen readers needs an unlocked session and a person | Screenshots in phase 4. The VoiceOver and Narrator pass moves to M5 (decided 2026-10-24) |
| Slint is pinned at 1.18.1 | It stays pinned unless a phase needs a newer release; that upgrade is a commit of its own |
