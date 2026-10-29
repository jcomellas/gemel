# Milestone 5: platform polish

*Status: in progress (phase 1b, 2026-10-29) · Follows milestone 4 (GUI, PRs #47–#62) · Source: `bcpierce00/unison` at `8772d26`*

Milestone 5 is step 5 of the port order in [the analysis](unison-port-analysis.md#4-architecture-recommendation): platform polish. It takes what earlier milestones moved here:
- synchronising extended attributes and ACLs, cut since milestone 2;
- packages for `gemel-gui` on each system, and the screen-reader pass, both moved from milestone 4 (decided 2026-10-24).

It ends with gemel synchronising xattrs and ACLs as OCaml Unison does, wherever OCaml does, with OCaml at the other end or gemel; and with `gemel-gui` installable from a package on macOS, Windows and Linux.

## Definition of done

1. **Extended attributes are synchronised as OCaml synchronises them,** with `xattrs = true`:
   - **where OCaml's `props_xattr.c` has them:** Linux and macOS, among the systems gemel builds for. Windows has none, as in OCaml, and says so as OCaml does;
   - **what is synchronised:** each attribute's name and value, less those `xattrignore` matches unless `xattrignorenot` does. The initial patterns are OCaml's: Linux's `security` and `trusted` namespaces, and the POSIX ACLs in `system.posix_acl_*`, which `xattrignorenot` can let through;
   - **how:** the values go between hosts as OCaml sends them (`propsExtData`), are stored in the archive as OCaml stores them (the props data, pruned, externed and interned), and count in update detection and reconciliation as in `props.ml` (the hash, `similar`, `check`, and the change time where writing xattrs updates it).
2. **ACLs are synchronised as OCaml synchronises them,** with `acl = true`:
   - **where OCaml's `props_acl.c` has them:** macOS and Windows, among the systems gemel builds for. Linux has none as ACLs, as in OCaml: there, POSIX ACLs travel as xattrs (item 1);
   - **their text** is what OCaml's stubs make: `acl_to_text`'s on macOS, as the stubs rework it (`postprocess_acl_os`), and the security descriptor's SDDL on Windows. It goes between hosts and into archives as it is, so a gemel end and an OCaml end agree on it;
   - their props, hash and checks are `props.ml`'s.
3. **With OCaml at the other end:** syncs that set, change and remove xattrs and ACLs end as OCaml's, in the oracle's pairings: a gemel client with an OCaml server, an OCaml client with a gemel server, and gemel at both ends, over ssh and on a socket, against `8772d26` and 2.53.8. A 2.52 peer, which has neither feature, refuses `xattrs` and `acl` as OCaml refuses them ("... but the server does not support this"), and syncs without them.
4. **Packages for `gemel-gui`:**
   - **macOS:** an application bundle (`.app`), with its `Info.plist` naming the application menu, in a disk image;
   - **Windows:** an installer (`.msi`);
   - **Linux:** an AppImage and a Flatpak (decided 2026-10-29).

   CI builds each package and checks that the program in it starts. Each carries the GPL-3.0-only licence and Unison's notices (§7 of the analysis). The packages aren't signed or notarised (decided 2026-10-29). `gemel` stays a plain binary without GUI dependencies (`check-no-gui-deps.sh`).
5. **The screen-reader pass:** a person goes through `uigtk3`'s flows in `gemel-gui` with VoiceOver on macOS and Narrator on Windows, and what can't be reached or read is fixed or listed.

Out of scope, and how each behaves until it is done:

| Cut | Behaviour in M5 | When |
|---|---|---|
| Signing and notarisation (Developer ID, Authenticode) | Unsigned packages: Gatekeeper and SmartScreen warn on first run | After M5 (decided 2026-10-29) |
| `dumpArchive`, `inspectFilesystem` (debugging) | Not registered | Not planned (decided 2026-10-29) |
| xattrs and ACLs on the BSDs and Solaris | gemel isn't built or tested there; OCaml's code for them isn't ported | Not planned |
| External copy programs (`copyprog`, `copyprogrest`, `copythreshold`) | As in milestone 3: accepted, with a warning that they are ignored | Not planned |
| `rsh` (`rshcmd`, `rshargs`) | Marked removed, as in OCaml since 2.52 | Never |
| The self-test commands (`test.ml`) | Not registered | Never |
| Unix-domain sockets on Windows | As in milestone 3 | Not planned (decided 2026-10-23) |
| What only `uimac` (Cocoa) has | As in milestone 4 | Not planned |

## Design decisions

### 1. OCaml's C stubs are the reference

- **The stubs decide** what is read and written on each system, and in what form: `props_xattr.c` (~0.8k) and `props_acl.c` (~0.8k). gemel follows their `#if`s for Linux, macOS and Windows, and the errors they raise, which `props.ml` turns into messages ("You can set preference \"xattrs\" to false to avoid this error").
- **Golden vectors** come from the stubs themselves, with milestone 1's fixture scripts (`tools/ocaml-fixtures`, as `gen_terminal.ml` did): on each system, a file with known xattrs and ACLs, and what OCaml reads from it and writes back. They settle the next decision.

### 2. Crates or system calls

- **The analysis's table** maps the stubs to the `xattr` and `exacl` crates. It didn't bind milestone 4, whose pseudo-terminal wasn't `portable-pty`.
- **The ACL text is the question:** it must be the text OCaml's stubs make, on the wire and in archives. `exacl` has a text form of its own, and no SDDL. So ACLs are likely system calls in `gemel-fs`, with the stubs' rework of the text (`acl_get_file` and `acl_to_text` on macOS; `GetNamedSecurityInfoW` and `ConvertSecurityDescriptorToStringSecurityDescriptorW` on Windows), unless the vectors show a crate gives the same text.
- **xattrs** may be the `xattr` crate, if it calls what the stubs call, with their options (`getxattr` and `listxattr`; on macOS with options 0, which follows symbolic links), or system calls otherwise.
- **New `unsafe`** goes in `gemel-fs` only, each in a function marked `#[allow(unsafe_code)]`, as its crate documentation lists them.

### 3. The data travels as OCaml's does

- **In archives:** the props hold a key into the props data (`Propsdata`), which the archive stores beside them. Milestone 2 already carries it as loaded. Now gemel prunes it, externs it when writing and interns it when loading, as `update.ml` does.
- **Between hosts:** the data a copy needs is asked for with `propsExtData` (`copy.ml`), which gemel registers now, and copies and transport carry it.
- **Preferences:** `xattrs`, `acl`, `xattrignore` and `xattrignorenot` are sent only when their feature is negotiated, as since milestone 3. The refusal of `xattrs` and `acl` stays for a peer without the feature, and goes otherwise. The archive's `xattrs pref` key makes a change of the preference rescan, as `checkNoUpdatePredicateChange` does.

### 4. Packages are built by CI, from tags

- **Tooling:** `cargo-bundle` or a script for the `.app` and disk image, WiX (through `cargo-wix`) for the `.msi`, `appimagetool` for the AppImage, and a Flatpak manifest built with `flatpak-builder`. Each is a CI job on all pushes, and a release on tags uploads them.
- **The application's name** in the menu bar and the task bar is Gemel, from the bundle's `Info.plist` and the installer's metadata, not the binary's name. This was on milestone 4's checklist for a person.
- **The Flatpak's sandbox** needs access to the files gemel synchronises and to the user's ssh: the manifest grants home and `--talk-name` as needed, and runs the system's ssh, or ships one. Phase 3 settles which, and says so.

## Phases

There is one PR per phase, cut from `main` and merged before the next phase starts, with no stacking. A phase that grows past a reviewable size is split. Line counts are the OCaml being ported.

### Phase 1: extended attributes (`props.ml`'s `Xattr`, ~0.4k; `props_xattr.c`, ~0.8k; `propsdata.ml`, ~0.1k)

- **Reading and writing** in `gemel-fs`: listing, getting, setting and removing, on Linux and macOS; not supported on Windows, as the stubs say.
- **`Props.Xattr`:** get (with the ignore patterns, and the cache of values by digest), set, check after setting, the hash, `similar`, `override`, the string for the details pane, and the change time (`ctimeDetect`).
- **`Propsdata`'s stores** for xattrs, and the archive's props data: pruned, externed and interned.
- **Preferences:** `xattrs`, `xattrignore` and `xattrignorenot` as OCaml has them, with the `xattrs pref` key.
- **Between hosts:** `propsExtData`, and copies carrying the data.
- **The oracle:**
  - the scenario generator sets, changes and removes xattrs (user ones, as the runners' file systems allow);
  - `compare_state` compares them;
  - pairings with `xattrs = true` against `8772d26` and 2.53.8, and the refusal against 2.52.1.
- **Likely split:** 1a, `gemel-fs`, the vectors and `Props.Xattr` on one host; 1b, the props data, `propsExtData`, copies and the oracle.

**Notes (phase 1a, 2026-10-29):**
- **Design decision 2, for xattrs: system calls,** in `gemel-fs`'s `xattr` module, not the `xattr` crate. The stubs follow symbolic links, read a value again while it keeps changing, take `ENOTSUP` as not supported, and mangle Linux's names; the crate's plain functions don't follow links.
- **No vectors from the stubs for xattrs,** where design decision 1 had them. Their behaviour is a few rules, ported and tested from the C: Linux's names (`user.` taken off, `!` before another namespace), and the system attributes left alone (Linux's `system.` but the POSIX ACLs; macOS's Finder information and resource fork). They are checked end to end against OCaml instead (below). Vectors stay for ACLs, whose text is what is at stake.
- **Ported:**
  - **`Props.Xattr`:** get (regular files and directories), set, check, `readAll`, `getAll`, `purge`, `wrapFail`, the texts, and the cache of values;
  - **`Propsdata`'s store,** one per host, which the host's scans share;
  - **`Props.get`** reading attributes again only when the change time differs from the archive's (`ctimeDetect`), with the archive's props passed as `buildUpdateRec` passes them;
  - **the `xattrs` preference,** refused only without the `Sync: xattr` feature, as OCaml's validator refuses it.
- **The split, as done:** loading the values before props are set (`propsExtDataOnRoot`) is one path for local copies and transfers, so the `propsExtData` server command is here too, and the test runs with a remote root in loopback. Left for 1b:
  - sending the values a host's scan stored to the other hosts (the `propsdata` command's new data, empty so far);
  - the oracle's scenarios with xattrs, and the pairings with 2.53.8 and against 2.52.1, which refuses `xattrs`.
- **Change times:** on macOS they are whole seconds, as OCaml's `stat` gives them there. So an attribute changed in the second of the last scan is missed, as in OCaml. The test waits a second before changing any.
- **Where xattrs can be tested:**
  - **Docker:** the image's `/tmp` (overlayfs) takes user attributes;
  - **`trusted.*`:** setting it needs `CAP_SYS_ADMIN`, which neither CI's runner nor Docker's root has, so the test for other namespaces says so and stops; the mangling's unit test runs everywhere;
  - **macOS** adds `com.apple.provenance` to files on its own. It is synchronised as OCaml synchronises it, and the tests look only at their own attributes.
- **Against OCaml** (`gemel-engine`'s `xattrs_propagate_like_ocaml`): attributes set, changed and removed on a file whose contents are the same, large ones, a new file's, a directory's, the other way, and one that `xattrignore` leaves alone, directly, in loopback and with a remote root. OCaml syncs a twin to the same attributes; it finds nothing to do on gemel's replicas, with fast checking and without; and gemel finds nothing to do on OCaml's. On macOS against 2.54.0, and on Linux against `8772d26`.

**Notes (phase 1b, 2026-10-29):**
- **A bug from 1a, found by the first mixed pairing:** `prepareCommit` hashed the archive with the default props settings, which leave xattrs out, where OCaml's `Props.hash` counts them when they are synchronised. So with `xattrs = true`, every sync between an OCaml end and a gemel end stopped at the commit ("New archives are not identical"). Phase 1a's test had OCaml check gemel's replicas and archives, but never had both at the ends of one sync; gemel at both ends left the xattrs out on both sides. Fixed, and the oracle's mixed pairings now check it.
- **The props data is `Propsdata`'s stores:**
  - the host's xattr store, which its scans share;
  - an ACL store, a host state of its own: the archive being written out is borrowed from the archive cache, so the store can't live there.

  Loading an archive sets them, and empties the xattr store, which archives don't keep. Writing one keeps the ACL texts it uses. After update detection, the `propsdata` command sends each host's new values to the other, so a copy finds cached values without `propsExtData`.
- **An archive that hasn't changed on disk isn't loaded again,** as in OCaml (`archiveUnchanged`), so the xattr store lasts from one sync to the next in a session.
- **The oracle, `--xattrs`:**
  - scenarios set and remove `gemel.a`, `gemel.b` and `gemel.c` on files and directories, with values empty, kept in the props, kept by digest, and too long to cache;
  - snapshots show those attributes only, since macOS's `com.apple.provenance` differs by the program that made the file;
  - the changes wait a second after the first sync, for macOS's whole-second change times;
  - a run that ends with no attributes at all fails.

  Without the flag, scenarios and snapshots are unchanged, and so are the recordings. `--refused` expects every sync to be refused instead.
- **Pairings,** all agreeing:
  - **Linux, in Docker:** `8772d26` against itself, and with gemel as the client, the server and both; gemel with 2.53.8 at the other end, both ways; locally, over ssh and on a socket. A gemel client with a 2.52.1 server is refused as OCaml's client is.
  - **macOS,** against Homebrew's 2.54.0: the same, but 2.53.8 and 2.52.1.
  - **CI's oracle job** runs these pairings, with 2.53.8 against the pinned source too.
- **Not done:** a 2.52.1 client with a gemel server, since 2.52 has no `xattrs` preference to ask with.

### Phase 2: ACLs (`props.ml`'s `ACL`, ~0.2k; `props_acl.c`, ~0.8k)

- **Reading and writing** in `gemel-fs`, as text: macOS and Windows; not supported on Linux, as the stubs say.
- **`Props.ACL`:** get, set, check, the hash, `similar` (without the names, as `removeAclNames` does), and their props data.
- **The preference** `acl`, and the feature's validation.
- **The oracle:** ACLs on macOS against Homebrew's OCaml, locally, and in CI's macOS job if an OCaml can be built there. Windows has no OCaml build: there, gemel at both ends, round trips through the archive, and the vectors (see Risks).
- **Likely split:** 2a, macOS; 2b, Windows.

### Phase 3: packages

- **macOS:** the `.app` with its `Info.plist` and icon, in a disk image, ad-hoc signed as the linker signs it.
- **Windows:** the `.msi`, installing `gemel-gui.exe`, `gemel.exe` and `gemel-fsmonitor.exe`, with a Start menu entry.
- **Linux:** the AppImage, and the Flatpak with its manifest and permissions.
- **CI:** a job per package, which starts the packaged program (`-version`, and the window under Xvfb on Linux); a release workflow on tags.
- **Licences:** GPL-3.0-only for the GUI's packages, with Unison's notices and the dependencies' licences (`cargo about` or similar).
- **Likely split:** 3a, macOS and Windows; 3b, Linux.

### Phase 4: the screen-reader pass, and the rest

- **VoiceOver and Narrator,** by a person: the main window's items and actions, each dialog, the profile manager, the assistant, the editor and the manual. `scripts/macos-ax-dump.swift` dumps the real window's tree on macOS; what the testing backend can't check is checked there.
- **Fixes** to what can't be reached or read, each with a window test where the testing backend can check it.
- **Odds and ends** found on the way, each with a decision.

## Verification

- **xattrs and ACLs:**
  - golden vectors from OCaml's stubs, on each system that has them;
  - `gemel-fs` tests on real files, on each system in CI;
  - the oracle's pairings, against `8772d26` and 2.53.8 with the features, and 2.52.1 without;
  - OCaml rechecks the archives a gemel end wrote, as in milestone 3.
- **Packages:** built and started in CI; installed and run by a person on each system once.
- **Screen readers:** a person, with VoiceOver and Narrator; the testing backend's checks for what it can see.

## Risks and open questions

| Risk or question | Plan |
|---|---|
| CI's file systems may not support user xattrs (Docker's overlayfs, the runners' volumes) | Checked first, in phase 1a. The tests use a directory on a file system that does (a tmpfs, or the runner's own disk), and say where they can't run |
| macOS adds xattrs to files on its own (`com.apple.provenance`, quarantine) | Checked in phase 1a. The vectors and tests list what the system added, and compare what OCaml and gemel do with it, rather than assume none |
| No OCaml build on Windows, to compare ACLs with | Vectors from the stubs, if OCaml can be built on Windows once (MSYS2); otherwise gemel at both ends, and the SDDL from the system's own conversion, checked against Windows' documentation |
| The ACL text differs between `exacl` and OCaml's stubs | Design decision 2: the vectors decide; system calls if needed |
| Windows' ACL text names accounts that don't exist on the other host | As in OCaml: the text goes as it is, and setting it fails as OCaml's does |
| The Flatpak's sandbox and ssh | Phase 3 settles whether the Flatpak runs the system's ssh or ships one, and how it reaches the files to synchronise |
| Unsigned packages | Gatekeeper and SmartScreen warn on first run; the README says how to open them. Signing after M5 (decided 2026-10-29) |
| Screen-reader checks need an unlocked session and a person | As in milestone 4: the person checks, and the testing backend keeps what it can |
