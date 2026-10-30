# Milestone 5: platform polish

*Status: in progress (phase 3b, 2026-10-30) · Follows milestone 4 (GUI, PRs #47–#62) · Source: `bcpierce00/unison` at `8772d26`*

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

**Notes (phase 2a, 2026-10-29):**
- **Design decision 2, for macOS's ACLs: system calls,** in `gemel-fs`'s `acl` module: `acl_get_file`, `acl_to_text`, `acl_from_text` and `acl_set_file`, declared from `<sys/acl.h>`, since the `libc` crate lacks them. The text must be what `acl_to_text` writes, and calling it is the shortest way to get that. The stubs' rework (`postprocess_acl_os`) only drops the trailing newline on macOS.
- **No vectors from the stubs:** on macOS the text is the system's own, so the vectors would only repeat `acl_to_text`. It is checked end to end against OCaml instead: the same texts in both ways' archives, and the same ACLs on disk. Vectors stay for Windows (2b), where gemel builds the SDDL itself.
- **Ported:**
  - **`Props.ACL`:** get (read again only when the change time differs), set (last, after the permissions), check (first), `similar`, and the texts;
  - **the props:** they hold the MD5 of the text (`deflate`), as in OCaml, and the text goes in the host's ACL store, which now sits beside the xattr store so that scans can add to it;
  - **the `acl` preference,** refused only without the `Sync: ACL` feature.
- **Texts are looked up in a table for the whole process** (`inflate`), as well as kept in each host's store. OCaml has one store per process; a gemel process may run several hosts (a client and its in-process servers). A key is its text's digest, so a text any of them makes, loads or receives is right for all of them, and the table needs no plumbing into `similar` or the details pane. An unknown key, where OCaml asserts, shows as itself in hexadecimal.
- **`removeAclNames`** (comparing under `numericids`) is ported as OCaml's pattern has it. That pattern only matches FreeBSD's and Solaris's texts, whose entries end in a numeric id; macOS's end in the permissions, so there it never changes a text, as in OCaml.
- **The oracle, `--acls`, on macOS:** scenarios add ACL entries with `chmod +a` and remove ACLs with `chmod -N`. The entries only allow, since a deny entry would stop the harness changing or removing what it is on; one is inherited by what a directory gets. Snapshots show each path's extended ACL.
- **Against OCaml,** Homebrew's 2.54.0 on macOS, all agreeing:
  - `gemel-engine`'s `acls_propagate_like_ocaml`: entries added and removed on files and directories, a new file's, and one changed the other way, directly, in loopback and with a remote root;
  - the oracle with `--acls --xattrs`, locally, and with gemel as the client, the server and both, over ssh and on a socket.

  A candidate that runs unison with `-acl=false` fails every scenario.
- **CI's macOS job** now installs Homebrew's unison and runs these, since the runner has no build of the pinned source. They follow whatever release Homebrew has.
- **Not run:** ACL pairings with 2.53.8 and 2.52.1, which CI builds only on Linux, where there are no ACLs. A 2.52 server's refusal of `acl` is checked by `gemel-ui-api`'s startup test, as OCaml's validator words it.

**Notes (phase 2b, 2026-10-30):**
- **Design decision 2, for Windows' ACLs: system calls,** the stub's own: `GetNamedSecurityInfoW`, `SetNamedSecurityInfoW` and the SDDL conversions, from `windows-sys`. `exacl` has no SDDL.
- **The text** is the DACL's SDDL as the stub makes it:
  - inherited entries removed, since they come from the parent directory and can't be set;
  - the auto-inherited flag cleared;
  - empty when nothing explicit or protected is left;
  - not supported when the file has no DACL at all.

  Setting an empty text sets an empty DACL that inherits (`D:`), and protection follows the text.
- **No vectors from the stubs:** no OCaml builds on Windows here (the Risks table's MSYS2 build wasn't tried). gemel makes the text with the same system calls and the same edits as the stub, so the SDDL is the system's own conversion, as the Risks table's fallback says.
- **Tests, gemel at both ends, on CI's Windows runner:**
  - `gemel-fs`: SDDL read and set, protected and not, inherited entries left out, the auto-inherited flag dropped, an empty text, and the stub's errors;
  - `gemel-engine`'s `acls_propagate_on_windows`: ACLs set, changed and removed on files and directories, and a new file's, directly, in loopback and with a remote root, then nothing to do by the archives.

  None of these run anywhere but Windows, so this PR's CI is their first run.
- **Change times on Windows aren't trusted, as in OCaml** (`hasCorrectCTime` is false there). gemel had it true everywhere since milestone 2, since its Windows `stat` gives NTFS's change time. But setting an ACL doesn't always update that, so CI's first run found a changed ACL left unread, with the archive's kept. ACLs are now read again at every scan on Windows, as OCaml reads them.
- **`removeAclNames`** never matches SDDL, whose entries name accounts by SID, so `numericids` changes nothing there, as in OCaml.
- **Not done:** a Windows end with an OCaml end, which the oracle can't pair here: Windows replays recorded outcomes, without xattrs or ACLs.

### Phase 3: packages

- **macOS:** the `.app` with its `Info.plist` and icon, in a disk image, ad-hoc signed as the linker signs it.
- **Windows:** the `.msi`, installing `gemel-gui.exe`, `gemel.exe` and `gemel-fsmonitor.exe`, with a Start menu entry.
- **Linux:** the AppImage, and the Flatpak with its manifest and permissions.
- **CI:** a job per package, which starts the packaged program (`-version`, and the window under Xvfb on Linux); a release workflow on tags.
- **Licences:** GPL-3.0-only for the GUI's packages, with Unison's notices and the dependencies' licences (`cargo about` or similar).
- **Likely split:** 3a, macOS and Windows; 3b, Linux.

**Notes (phase 3a, 2026-10-30):**
- **Design decision 4, the tooling, as done:**
  - **macOS:** a script (`scripts/package-macos.sh`) rather than `cargo-bundle`. The bundle is a few files, and the script signs them in the right order.
  - **Windows:** WiX 5 directly (`dotnet tool install`, pinned), rather than `cargo-wix`, which drives WiX 3. Runners may no longer have WiX 3.
- **The icon** (`packaging/icon/gemel.svg`) is a plain placeholder: two interlocking rings. `scripts/make-icons.sh` builds the `.icns` and the `.ico` from it, and both are committed.
- **Notices:**
  - **`NOTICE`:** gemel's copyright, that it derives from Unison at `8772d26` and isn't an official Unison project, its licences, where a package's source is (the tag of its version), and the copyright lines of the Unison sources it ports, as their headers carry them, with OCaml's for the legacy hash;
  - **`THIRD-PARTY-LICENSES`:** written by `cargo-about` (`packaging/about.toml`), with each crate's licence. Slint's crates are accepted only under GPL-3.0-only, which records the option gemel uses.
- **macOS:**
  - **The bundle:** `Gemel.app` holds universal binaries (arm64 and x86_64) of `gemel-gui`, `gemel` and `gemel-fsmonitor`, which `gemel-gui` finds beside itself.
  - **Signing:** ad hoc, the helpers then the bundle, so that the signature covers `Info.plist` and the resources; the linker's covers only each binary. There is no hardened runtime, which only notarisation needs.
  - **The disk image** links to `/Applications`, and comes to about 31 MB.
  - **Checked locally:** run from the mounted image, the programs start, and Launch Services names the running app Gemel, from `CFBundleName`, which is what the menu bar shows.
- **Windows:** the `.msi` installs for all users in `Program Files\Gemel`, with a Start menu entry. Its UpgradeCode is fixed, so a newer version replaces an older one.
- **CI** (`.github/workflows/packages.yml`) builds and checks both on every push. The release workflow runs the same jobs on a tag and attaches the packages to a draft release.
- **Identifiers,** fixed from the first release: the bundle identifier `io.github.jcomellas.gemel`, and the installer's UpgradeCode.
- **Known, for phase 4:** `gemel-gui` is a console program on Windows, since it is also the text interface (`-ui text`), so launching it from the Start menu opens a console window beside the GUI. Hiding it (`windows_subsystem`, attaching to a parent console for the text interface) is a change to the program, not to the package.
- **Not run before this PR:** the Windows package (no Windows here) and the release workflow (no tag yet).

**Notes (phase 3b, 2026-10-30):**
- **The Windows package** first ran in #67's CI. WiX refused a component holding three unversioned files, the notices, so each has its own. CI has installed and uninstalled it since.
- **The application id** (`io.github.jcomellas.gemel`, macOS's bundle identifier) is also the window's on Linux: `gemel-gui` sets it (`slint::set_xdg_app_id`) once Slint's backend has started. Desktops match the Wayland app id and X11's `WM_CLASS` to the desktop entry of that name, for the name and icon they show. The desktop entry, the AppStream metadata and the icon are in `packaging/linux`, shared by both packages. The metadata leaves out releases, which would need editing for each version.
- **Design decision 4, the tooling, as done:**
  - **AppImage:** `scripts/package-appimage.sh`, with appimagetool 1.9.1 and the runtime of type2-runtime's 20251108 release, downloaded and checked against the SHA-256s GitHub publishes. It holds the three programs, the notices in `usr/share/doc/gemel`, and nothing of the system's: winit and glutin load X11, Wayland and GL with `dlopen`, and fontconfig is on every desktop. Built on Ubuntu 22.04, it needs glibc 2.35, which CI checks.
  - **Flatpak:** `scripts/package-flatpak.sh` and `packaging/linux/io.github.jcomellas.gemel.yml`, on the Freedesktop 26.08 runtime with its Rust extension (1.98), in a single-file bundle. Cargo fetches the crates during the build (`--share=network`), as `Cargo.lock` pins them; Flathub would instead need them as sources (`flatpak-cargo-generator`). The runtime and SDK go in the user's installation: the CI image's system-wide copy is on overlayfs, where updating it fails.
- **The Flatpak's sandbox and ssh, settled:**
  - **Files:** `--filesystem=host`. A synchronizer's replicas may be anywhere, `/media` and `/mnt` too. Without host access, a directory picked in the file chooser comes back as the document portal's path (`/run/user/…/doc/…`), which goes into the profile, where the command line and the file watcher can't use it.
  - **The Unison directory:** gemel finds it from `HOME` (`~/.unison`), never `XDG_CONFIG_HOME`, and a Flatpak keeps `HOME`. So the profiles and archives are the system's `unison` and `gemel`'s.
  - **ssh:** the runtime's own (OpenSSH 10.6p1, which CI checks is there), with `~/.ssh`'s keys, `known_hosts` and config through the host access, and the agent through `--socket=ssh-auth`. Not the system's `/etc/ssh`, or the programs a `ProxyCommand` names: the README says so. Running the system's ssh (`flatpak-spawn --host`) would need the permission to run anything outside the sandbox.
  - **Also:** the network, for remote roots; the window on Wayland, or X11 without it, with GL. The file chooser is rfd's portal backend, which works in the sandbox.
- **CI** (`packages.yml`):
  - **AppImage,** extracted, since runners may have no FUSE: the programs start, the notices are there, the desktop entry validates, and glibc 2.35 is enough.
  - **Flatpak,** in Flathub's image for building Flatpaks (`ghcr.io/flathub-infra/flatpak-github-actions:freedesktop-26.08`, privileged for bubblewrap), installed from its bundle: the programs start, ssh is there, and so are the notices. That image has no cargo, so `THIRD-PARTY-LICENSES` comes from the AppImage's job.
  - **The window, for both:** `scripts/smoke-linux-gui.sh` starts it under Xvfb on two empty roots. It waits for a window whose class is the application id, reading `WM_CLASS` through libX11 from Python, since the image has no `xwininfo`. Then it checks the program is still running 5 seconds later.
- **x86_64 only in CI,** as on Windows. Both scripts build for the machine's architecture, and were run on aarch64 here: the AppImage in the Linux Docker image, the Flatpak in CI's image (arm64), each with the window check.
- **Not checked:** the packages on a real desktop, which is the person's check in the verification section; Wayland, which Xvfb doesn't have.

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
