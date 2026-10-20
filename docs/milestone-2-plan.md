# Milestone 2: local engine and text UI

*Status: proposed (2026-10-19) · Follows milestone 1 (compat and base, PRs #2–#6) · Source: `bcpierce00/unison` at `8772d26`*

Milestone 2 is step 2 of the port order in [the analysis](unison-port-analysis.md#4-architecture-recommendation): a local↔local engine plus the text UI. It ends with `gemel` synchronising two local directories, with OCaml Unison's results and archive format.

## Definition of done

On macOS, Linux and Windows, for generated replica trees and change scripts:

1. **Same reconciliation.** `gemel -batch a b` and OCaml `unison -batch a b` propose the same actions, and the final trees are identical.
2. **Archives interoperate in both directions.**
   - After OCaml syncs, gemel loads OCaml's archives, finds them correctly ordered, and recomputes the stored `checkArchive` hash.
   - After gemel syncs, OCaml loads gemel's archives and reports nothing to do and no archive inconsistency.
3. **Profiles behave the same.** A `.prf` profile works in both.
4. **The text UI is usable interactively,** and the interactive mode is covered by tests.

Out of scope, and how each behaves until it is done:

| Cut | Behaviour in M2 | When |
|---|---|---|
| Remote roots (ssh, socket) | `parse_root` accepts them; connecting fails with a clear error | M3 |
| `transfer.ml` (rsync deltas) | Not needed: local copies go through `Copy.localFile` and cloning | M3 |
| Synchronising xattrs and ACLs | The features `Sync: xattr` and `Sync: ACL` **are** registered, for their archive format (see below), but the `xattrs` and `acl` preferences are not, so nothing is synchronised | Later in M2 if time allows, else M5 |
| Move detection (feature `Sync: Moves`) | The feature is not registered | Later in M2 if time allows, else M5 |

**Correction (phase 1a, 2026-10-19):** this plan first said not to register the xattr and ACL features. Real archives showed that OCaml Unison 2.54 enables both whenever the two sides support them, even with the `xattrs` and `acl` preferences off. Its archives therefore always record the change time, xattr and ACL fields, and name both features in their header. If gemel left the features unregistered, it would refuse every archive OCaml writes.
| Watch mode / `gemel-fsmonitor` | `-repeat watch` is rejected | Last phase, optional |
| GUI | Untouched (fake engine) | M4 |

## Design decisions

These are made now because reversing them later would mean rewriting engine code.

### 1. The engine is async, structured like Lwt

The engine has to be async, for two reasons:
- `remote.ml` is not one-way. The server forwards trace messages to the client, and copying uses streaming commands, so several calls are in flight in both directions over one pipe.
- The Lwt-heavy modules are exactly the ones that talk to roots: `update` (52 uses), `copy` (43), `files` (35) and `transport` (9). Retrofitting async onto a synchronous port of those in M3 would mean rewriting them.

How:
- **Runtime:** a `current_thread` tokio runtime on one engine thread. Lwt is cooperative, and Unison mutates shared state between yield points without locks. On one thread that ports directly with `Rc`/`RefCell`, and futures need no `Send` bounds.
- **Blocking work:** directory scans and MD5 go to `spawn_blocking` with owned inputs.
- **What stays synchronous:** root commands and transport are `async`. Reconciliation (`recon` has no Lwt), archive manipulation, `checkArchive` and props comparison stay ordinary functions.
- **Frontends** reach the engine thread over channels (commands in, events out), as the analysis says.

### 2. The command boundary exists from M2

OCaml engine modules register their remote commands (`Remote.registerRootCmd`). For a local root, `registerHostCmd` just calls the function. gemel does the same from the start:
- **`RootCmd<A, R>`** in `gemel-rpc` names a command and carries the marshallers for `A` and `R` and the local function.
- **Local dispatch** calls the function directly.
- **Loopback dispatch**, for tests, marshals `A` and `R` through Umarshal and back on every call. The M2 test suite also runs in this mode, so every argument and result codec is exercised before M3, and M3 only has to add framing and the transport.

### 3. Replica state lives behind the boundary

In OCaml the archives, the fingerprint cache and the locks belong to the host that owns the root and are reached only through root commands. gemel mirrors that:
- **Per-host state** (archives, fingerprint cache, locks) is reached through `RootCmd`s.
- **The client session** holds everything else: preferences, features, case mode, trace, roots and recon items.

If archives sat in the client session in M2, M3 would have to move them.

### 4. Archive order is enforced by a type

`NameMap` order follows the case mode, and it matters in three places:
- **Marshalling.** `MyMap.m` builds the list with `fold (fun k v acc -> (k, v) :: acc)`. `fold` visits keys in descending order, so the list is **ascending**.
- **`checkArchive`.** It folds `hash2` over the children in **descending** order, and `hash2` is not commutative.
- **Loading.** `update.ml` rejects an archive whose names are "not correctly ordered".

So `NameMap` is a sorted vector that can only be built and searched under a given `Mode`. Its iterators say which direction they go. Name and path comparisons already take the mode explicitly (milestone 1).

### 5. Paths at the OS boundary

Names and paths are byte strings in the engine. They become OS paths only in `gemel-fs`:
- **Unix:** `OsStrExt`.
- **Windows:** UTF-16 with Unison's own filename mapping (`unicode::to_utf_16_filename`/`from_utf_16_filename`, already ported). Unison's long-path handling is followed.
- **Inode stamps:** Windows has them too. `Fs.hasInodeNumbers ()` is true everywhere, and Unison's `win_stat` derives the number from the file index (`IndexNumber + 155825701 × high part`); gemel does the same through `GetFileInformationByHandle`. *(Corrected in phase 1b: this plan first said Windows used `NoStamp`.)*

## Phases

There is one PR per phase, cut from `main` and merged before the next phase starts, with no stacking. Line counts are the OCaml being ported.

### Phase 0: the OCaml oracle

- **Build `unison` in Docker.** Build it from the same `8772d26` source in the existing OCaml image (`tools/ocaml-fixtures/Dockerfile`), rather than relying on distro packages. Homebrew's 2.54.0 covers macOS-specific runs.
- **A harness** (`tools/oracle/`) that:
  - generates replica trees and change scripts from a seed;
  - runs either implementation with its own `UNISON` directory;
  - collects the `-batch` output, the final trees and the archives;
  - compares them.

  The harness is the definition of done, and every later phase adds cases to it.

### Phase 1a: `gemel-fs` value types (~1.2k)

The `Props`, `Fileinfo`, `Osx` and fingerprint types, with their Umarshal codecs and `Uutil.hash`-based hashes. No OS calls yet.
- **Props:** permissions, uid/gid (numeric and named), modification time with Unison's one-second and DST tolerance, type/creator, and the xattr/ACL shapes (codec and hash; their fields are in every OCaml 2.54 archive, see the correction above).
- **Others:** `Fileinfo` stamps, `Osx` resource-fork stamps, and `Os.fullfingerprint`.
- **Tests:** OCaml golden vectors (extending `gen_fixtures.ml`).
- **First vertical slice:** decode real OCaml archives from phase 0 down to the props.

### Phase 1b: `gemel-fs` operations (~2.5k OCaml, ~2k C)

- **`system`, `fspath`, `fs`:** the portable OS calls over `std`. `gemel-fs` may use `unsafe`, for the few calls `std` lacks.
- **`os`:** directory listing, deletion, renaming, temp files and the `.unison.*` names.
- **`fileinfo`:** `lstat`, the file type, and the stamp.
- **`fingerprint`:** MD5 over data and resource fork, with `spawn_blocking`.
- **`reflink`:** clonefile, FICLONE and reflink via `reflink-copy`.
- **Tests:** on real temporary directories, in CI on APFS, ext4 and NTFS.

### Phase 2: `gemel-rpc` minimal (~0.3k of `remote.ml`)

- `Root` (`Common.root`: local or remote, plus fspath) and resolving a `Clroot` to it.
- The `RootCmd<A, R>` registry, with local and loopback dispatch.
- The per-host state container.
- The engine thread and runtime.

### Phase 3: engine foundations (~1.5k)

- **`common`:** update items, replica contents, recon items, directions and differences, with codecs.
- **`globals`:** roots and the `path`/`root` preferences.
- **`pred`:** ignore and follow predicates over `rx`.
- **`tree`** and **`abort`**.
- **`lock`:** archive lock files.
- **`fpcache`:** the fingerprint cache file.

**Notes (phase 3, 2026-10-20):**
- Command codecs now get the features in effect on their side, because `Props` and `prevState` change shape with them. The loopback encodes with the client's features.
- `negotiate` came forward from later phases, since `installRoots` needs it. In loopback mode it questions the local roots too, which enables the features on the loopback host.
- `propagatePrefs` and `installPrefs` came forward too, under the same rule: the loopback host is created before the roots are installed, so it needs the preferences set after that, as a server would get them.
- `fpcache` is an owned value per root rather than one global, so the two roots can be scanned at the same time on blocking threads.
- Predicates compile to an immutable, shareable value, cached by pattern list and case mode.
- New OCaml vectors cover every update-item constructor in every feature combination, a fingerprint-cache chunk, and `pred.ml` itself (matching and associated strings in the four case modes, and the error messages).

### Phase 4: `update.ml` (3.1k, in three PRs)

- **4a, archives:**
  - naming (`MD5("thisRoot;roots;23")`);
  - the header lines, including the features line;
  - `Umarshal` payload and the separately stored `paths`;
  - the case-mode magic;
  - `checkArchive`;
  - load, store, lock, and the cross-replica consistency check.
  - **Done when:** OCaml-written archives round-trip byte for byte.

  **Notes (phase 4a, 2026-10-20):**
  - The archive half of `update.ml` is the `archive` module; `MyMap` is `name_map`. A map is re-sorted under the host's case mode as it is read, as `MyMap.m` does, and later bindings replace earlier equal ones.
  - The case-mode preferences are registered now, and each host reads its mode from its own preferences. (`unicodeCS` is marked local, but it is still sent; local only means a server may not know it.)
  - The loopback host stands in for the client's own process, where OCaml runs local roots' commands. So it names itself as the client does (`clientHostName`), and its warnings go to the client (`Util.warn`, now on `Host`).
  - `"paths"` is kept beside the property list rather than in it. `"props data"` is carried as loaded, since gemel doesn't synchronise xattrs or ACLs yet.
  - "Byte for byte" means: an OCaml archive loaded and stored again is identical, apart from the time on the third line.
  - The real-archive fixtures moved to `gemel-engine/tests/archives/`, with `reading.rs`, now that the parser is real. There are 32: Linux and macOS, all four case modes, with and without `-path`, and each with the name OCaml gave it.
  - `prepareCommit` and `commitUpdates` stay in 4c. The `dumparchives` text dump is not ported yet.
  - Known divergence: root paths that aren't UTF-8 print lossily, so their archive names differ from OCaml's.
  - For later phases: the frontend's preference set (phase 7) must register `case` and `fileinfo` preferences, which archive commands read; and since locks are released when a host is dropped, the driver (phase 8) must call `release_held_locks` before `process::exit`.
- **4b, update detection:** scanning, fast check (inode and mtime), `dirFastCheck`, `follow` and ignore handling, fingerprinting and fpcache use.

  **Notes (phase 4b, 2026-10-20):**
  - The detection half of `update.ml` is the `update` module, with `xferhint`. A scan takes the archive, the compiled predicates and the fingerprint cache to a blocking task and brings them back, so the two roots of a local sync are scanned at the same time. What a scan leaves on the host (copying hints, the temporary-file suffix) is applied in the roots' canonical order, the order OCaml scans them in.
  - The scan updates the archive in place. Where OCaml drops the time updates under a path whose scan then fails, gemel keeps those already made; they record only what was verified.
  - `checkNoUpdatePredicateChange` gives every OCaml fixture archive back unchanged, with no rescan: the predicates (with `xattrignore`'s initial patterns), `rsrc`, and the global and per-path layout all match. This depends on the backup patterns that `Stasher.initBackups` adds to `ignore` on each root's host (so twice when both roots are local); phase 6 must port that, or OCaml's archives look as if the predicates changed.
  - For M3: `xattrignore` and `xattrignorenot` are sent to the server only when the xattr feature is enabled, since a 2.52 server doesn't know them.
  - For phase 7: the frontend must default `path` to the empty path when it is unset, as `uicommon` does, or `findUpdates` scans nothing. It must also call `case::init`, `osx::init` and `fileinfo::init` before `propagatePrefs`.
  - Not ported yet: scanning only what a file watcher reported (path trees, `subpaths`; phase 9), status messages during the scan (phase 7), and `updatePathInArchive` (4c).
  - A new test has OCaml Unison sync a tree twice, then checks that gemel finds no updates in its archives, directly and through the loopback, and finds the changes made after. CI's oracle job runs it.
- **4c, commits:** `prepareCommit` and `commit`, `markEqual`, and the update of archive props after propagation.

### Phase 5: reconciliation (~1k)

- **`recon`:** directions, conflicts, and `prefer`, `force`, `preferpartial` and `forcepartial`.
- **`sortri`.**

`recon` has no Lwt, so this is plain code with table-driven tests and oracle comparisons.

### Phase 6: propagation (~3.6k)

- **`transport`**, **`files`** and **`copy`:** local copy and cloning, setting props, atomic replacement via temporary names, deleting, and symlinks.
- **`stasher`:** backups.
- **`xferhint`:** copying from an identical local file.
- **`external`:** `diff` and `merge` programs.

### Phase 7: `gemel-ui-api` (~1.3k)

- **`uicommon`:** profile choice and initialisation, root checks, the transfer and stats formatting, and the summary line.
- **The session contract** modelled on `uimacbridge.ml`: `init1`/`init2`, item accessors and setters, `synchronize`, abort, ignore, and diff.
- Exit codes (already ported).

### Phase 8: text UI, `bins/gemel` (~2.1k)

- **`main`:** the driver: `-server` stub, profile selection, `-batch` and `-ui text`.
- **`uitext`, batch mode first:** the summary, `-terse`/`-silent` and `-auto`.
- **Then interactive mode:** the per-item prompts and keys, the confirmations, `-repeat` with a time interval, and dumb-terminal handling.

The text UI uses the phase 7 contract rather than the engine directly, so the contract has a consumer before the GUI.

### Phase 9 (optional): watch mode

- **`fswatch`** on the client side, over the ported line protocol.
- **`bins/gemel-fsmonitor`** on `notify`.

Unison has no FSEvents watcher on macOS, so gemel's helper would be an improvement there.

## Verification

- **Codecs and hashes:** OCaml golden vectors, as in milestone 1. They are regenerated with `scripts/gen-ocaml-fixtures.sh`, and must be identical on OCaml 4.13 and 5.3.
- **Behaviour:** the phase 0 oracle, on every PR from phase 4 on, comparing proposed actions, final trees and cross-loaded archives.
- **Loopback dispatch:** the whole engine suite also runs with every root call marshalled.
- **Platforms:** CI on Linux, macOS and Windows. The oracle runs on Linux and macOS, while Windows checks against recorded oracle results until a Windows OCaml build is set up.

## Risks and open questions

| Risk or question | Plan |
|---|---|
| Archive order or `checkArchive` drift silently breaks mixed use | The `NameMap` type (decision 4) and the phase 4a byte-for-byte round trip |
| Props semantics differ per platform (times, permissions, uid/gid names, Windows read-only bits) | Phase 1b tests on each filesystem; the oracle on generated trees |
| async plus `RefCell` misuse (holding a borrow across `.await`) | Clippy's `await_holding_refcell_ref` is denied |
| Windows long paths and NTFS-reserved names | Follow `system_win.ml` and `fspath.ml`. `Name::bad_file` is already ported |
| Implementation can't start until #6 (trace, proplist) merges | This plan goes out from `main`; phase 0 can start now, and phase 1a starts once #6 is merged |
| The oracle needs a `unison` build with the same source | Phase 0 builds it in the existing Docker image |
