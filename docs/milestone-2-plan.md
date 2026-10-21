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

  **Notes (phase 4c, 2026-10-20):**
  - The rest of `update.ml` is in `update`:
    - `prepareCommit`, `commitUpdates`, and the props-data prune;
    - `updatePathInArchive`, `updateArchive`, `markEqual`, `replaceArchive` and `updateProps`;
    - `checkNoUpdates`, `markPossiblyUpdated`, the size functions, and `iterFiles`.
  - A commit stores the cached archive by reference rather than copying it.
  - `replaceArchive` decodes its archive in the order sent and sorts it under the host's case mode, since codecs have no mode. Sorting out-of-order maps is now O(n log n), and later bindings still win.
  - The stasher's hook for `markEqual` is a host slot (`set_stasher_fun`). All of a mark's backups are made before the archive changes, so a failed backup leaves the archive as it was.
  - Not ported: `inspectFilesystem`, which OCaml registers but never calls. `dumparchives` is still to come; meanwhile a hash mismatch at commit is reported without the dump.
  - Done criterion met: OCaml Unison (2.54.0 on macOS, 8772d26 on Linux) runs on archives gemel committed and accepts them. It has nothing to do, with fast checking and also with `-fastcheck false` (which checks every fingerprint). The checkArchive hash it stores when it commits again is gemel's, and it syncs on from there. CI's oracle job runs this test.

### Phase 5: reconciliation (~1k)

- **`recon`:** directions, conflicts, and `prefer`, `force`, `preferpartial` and `forcepartial`.
- **`sortri`.**
- **`moves`**, which `recon` calls: move and rename detection (`moves-experimental`).

`recon` has no Lwt, so this is plain code with table-driven tests and oracle comparisons.

**Notes (phase 5, 2026-10-20):**
- `recon`, `sortri` and `moves` are in `gemel-engine`. `sortri` was planned for `gemel-ui-api`, but `recon` sorts its items; the UI's sorting menu calls the engine's functions.
- `recon` is plain code except for `markEqual` and `commitUpdates`, which are remote calls. OCaml marks the equal paths in batches of 5000 during the walk. Here the walk keeps the batches, and marks them in the same order when it is done. That's safe because the walk only reads the local archive at paths below none already marked. As in OCaml, the "some path was equal" result comes from the last batch only.
- `moves` keeps its candidates in a value for one reconciliation rather than in global tables. It registers the `Sync: Moves` feature, and the `moves-experimental` preference is sent only when that feature is negotiated.
  - Feature validators now get the preferences, and a preference's `send` check gets the negotiated features. That second change also unblocks sending `xattrignore` only with the xattr feature (milestone 3).
- Recon's size lookups borrow the local archive (`update::LocalArchive`) rather than copying the subtree each time.
- For later phases:
  - OCaml's startup derives `links-aux` (`Fileinfo.init`), which update detection reads. Phase 7 must call it with the other inits, before `propagatePrefs`.
  - The `Sync: Moves` validator reads `moves-experimental` during negotiation, so a host that registers the engine's features must register its preferences too (phase 7's setup, and phase 8's `-server`).
  - Not ported yet: the "Reconciling changes" status and recon's debug traces (phase 7, with update detection's).
- Tests:
  - `tests/recon.rs` covers, in both dispatch modes: sorting before overriding, `prefer` and `force` (by root, prefix, suffix, `newer` and `older`, and the partial forms), the root checks, the `no*` preferences and `maxsizethreshold`, merge, atomic and props conflicts, dangerous paths, batches of equal paths, moves, links with `links = false`, and case-insensitive spellings.
  - An oracle test runs 18 scenarios through gemel, then through OCaml Unison's batch text UI on the same replicas, and compares each item line (statuses, arrows, forced arrows, moved names), the summary (items to sync and skip, and the bytes each way), and each skipped item's reason. It passes against 2.54.0 on macOS and 8772d26 on Linux, and CI's oracle job runs it.
  - The tests give directories fresh times before each scan. A directory changed within the second of its last scan keeps its time, and fast checking skips it, in OCaml as here. Times a second apart also count as equal (for FAT).

### Phase 6: propagation (~3.6k)

- **`transport`**, **`files`** and **`copy`:** local copy and cloning, setting props, atomic replacement via temporary names, deleting, and symlinks.
- **`stasher`:** backups.
- **`xferhint`:** copying from an identical local file.
- **`external`:** `diff` and `merge` programs.

Split, like phase 4, into:
- **6a:** copy's local half and the stasher;
- **6b:** `files` and `external`;
- **6c:** `transport`, and an end-to-end sync against OCaml.

**Notes (phase 6a, 2026-10-20):**
- **Copy:** only the local branch of `Copy.file` is ported.
  - A copy clones the file where the filesystem can (`clonefile` on macOS, `FICLONE` on Linux). Otherwise it copies in the kernel (`copy_file_range`, then `sendfile`, on Linux), and otherwise it reads and writes. It never uses `std::fs::copy`, which would carry over metadata that `setFileinfo` must set itself.
  - The copy is synchronous, with callbacks for progress and abort checks, so `Copy.file` runs it on a blocking task.
  - `checkForChangesToSource` and `saveTempFile` are root commands, with OCaml's codecs.
- **Deferred to milestone 3, with remote roots:**
  - transfers between hosts (rsync, compression, resuming, `copyprog`);
  - the copy from an identical local file (`tryCopyMovedFile`), which OCaml only tries for remote transfers. Their preferences are registered.
- **Not ported yet:**
  - Windows block cloning (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`, on ReFS): copies there read and write.
  - Resource forks held in AppleDouble files: an error, as in update detection.
  - Extended data (xattrs and ACLs), which copies don't carry yet.
  - Progress reporting, the timers, and aborting in the middle of a copy (phase 7, which needs an abort flag shared with the UI).
- **Stasher:** all of it.
  - `markEqual` stashes through it by default; the hook stays, so tests can replace it.
  - `initBackups` runs on each root's host, as in OCaml, so a host with both roots ignores old backup files twice. In loopback mode that is the loopback host, which stands in for the client's process. The tests now call it rather than adding the patterns themselves.
  - For phase 7: OCaml calls `initBackups` after `propagatePrefs`, which would otherwise replace the server's patterns.
  - For 6b: backups (and `backupcurr` stashes from `markEqual`) still read and copy files on the engine thread. `files` will back up every replaced file, so 6b moves them onto blocking tasks, as `checkForChangesToSource` already is.
- **Tests:** `tests/copy.rs` covers, in both dispatch modes:
  - the copy and its props, with an old temporary file;
  - large files in chunks, and by reading and writing;
  - aborting;
  - a source changed during the sync, and a change the fast check missed;
  - pseudo-fingerprints;
  - the kept bad copy;
  - trees, and resource forks on macOS;
  - backup rotation, local backups and their ignore patterns, the naming errors, `backupcurr` through `markEqual`, and `getRecentVersion`.
  
  Unit tests in `copy` cover reading and writing (the path Windows takes) and resource forks. Not tested: the fall back to reading and writing after a kernel copy fails partway.

  Two oracle tests compare four backup configurations with OCaml Unison: the `ignore` patterns its archive records after `initBackups`, and the backups' names and contents after overwriting files four times. CI's oracle job runs them.

**Notes (phase 6b, 2026-10-20):**
- **`files`:** all of it, for local roots. Every part on a root is a root command with OCaml's name and encoding.
  - Deleting, setting props (`setProp`, `updateProps`), `mkdir`, `setDirProp`, and making links.
  - Renaming with the commit log (`DANGER.README`), and `processCommitLogs`.
  - The target-path commands and the parents they create.
  - `copy`, `updateSourceArchive` and `deleteSpuriousChildren`.
  - `copyonconflict`; `move` and `moveArchive`; `diff`, `merge` and `ls`.
  - The archive updates keep OCaml's order exactly (for crash safety).
- **`external`:** running diff and merge programs through the shell, with OCaml's log format and signal numbers.
  - A divergence: on Unix the program's input is empty, where OCaml leaves it a pipe it never closes (so a program reading it hangs).
- **Threading:** OCaml does a host's filesystem work synchronously, so none of it overlaps, even with both roots on one host.
  - Here it runs off the engine thread, under a per-host lock (`exclusive`). Two roots' backups can share a directory, and renames share the commit log.
  - The stasher now takes what it needs from the host first, then works on a blocking task; `markEqual` awaits it.
  - For 6c: the lock makes each piece atomic, not each root command. OCaml's `deleteLocal`, `renameLocal` and `moveLocal` don't yield between `checkNoUpdates` and the rename; here other work can run there. That is harmless while items propagate one at a time, but `maxthreads` would expose it, so 6c keeps local items sequential or holds the lock for a whole command.
- **Copying a directory** copies its children one at a time, in name order (the order OCaml's threads run between local roots). Running transfers concurrently (`maxthreads`) comes with transport.
- **Not ported yet:**
  - remote roots for diff and merge (milestone 3);
  - the log of the merge command and its result, and merge's messages on standard error (phase 7);
  - changing directory around making a link on Windows: `os::symlink` resolves a relative target against the link's directory instead.
- **Extended data** (xattrs and ACLs) passes through unchanged (milestone 3).
- **Tests:** `tests/files.rs`, in both dispatch modes, drives each operation the way transport will:
  - new files and trees, updates, deletions with backups, props, a file over a directory and back, links;
  - a partly failed copy;
  - `copyonconflict`, the commit log, and moves (renames that keep the inode).
  
  After each, OCaml Unison finds nothing to do, with fast checking and without, so its archives agree with both replicas. On twin replicas, gemel and OCaml are compared for:
  - merges, for ten kinds of merge-program result, including the log shown (OCaml prints it in batch mode, so signal numbers are checked too);
  - the three kinds of `copyonconflict` conflict, and the run after;
  - parents created for a path given with `path`, each with its own props.
  
  OCaml stops on a leftover commit log with the same message. CI's oracle job runs these.

**Notes (phase 6c, 2026-10-20):**
- **`transport`:** all of it. Each item becomes one of the `files` operations, and a move one of `doMove`'s four cases: a rename where the moved contents are unchanged in the other replica, or copies and deletions otherwise (and after a rename across filesystems, `EXDEV`). The log entries are OCaml's: `[BGN]` and `[END]` around each operation, `[CONFLICT]` and `[ERROR]` for the items skipped, `Failed` (in the log file only) for one that fails, and the start and end of the run.
- **The client's `Trace`** is a slot of its host, which the user interface will set up (phase 7). Transport is the first part of the engine that logs. `Uutil.myNameAndVersion` is `gemel` and its version, in messages; archives and the files Unison names keep `unison`.
  - For phase 7: a `Trace` slot made by default has no Unison directory, so a relative `logfile` (the default, `unison.log`) would land in the working directory. The interface must install the client's slot, with `set_unison_dir`, before anything logs.
- **The pool:** `Transport.run`, and `Uicommon.transportItems`, which every user interface shares, so it is in `transport` rather than `gemel-ui-api` (the engine's tests need it, and can't depend on that crate).
  - The first error stops the run and aborts everything. OCaml returns it at once and gives the items still running a few chances to stop; here they finish first (they stop at their abort checks).
  - Between local roots, items run one at a time. OCaml runs up to `maxthreads` at once, but its local operations never yield, so each item runs to the end before the next starts. Here the filesystem work runs on blocking tasks, which yield; one at a time keeps OCaml's order (of the log and of archive updates) and settles the atomicity note of 6b. Items run concurrently only with a remote root.
  - `maxThreads` reads `stream` (`Remote.streamingActivated`), which comes with remote roots in milestone 3; until then it is on, its default.
- **Fixed in `gemel-fs`:** the default props (`Props.fileDefault` and the others) read the umask each time, by setting it to 0 and back, so a file another thread created in between got mode 0o666 or 0o777. The tests showed it as an occasional "New archives are not identical". It is read once now, as OCaml reads it at startup, and on Linux from `/proc/self/status`, without setting it.
- **Not ported:** the progress display and statistics of `Uitext.doTransport`, and its catching of `Transient` per item (phases 7 and 8; the tests do it themselves). Not tested: the fallback after `EXDEV`, which needs two filesystems.
- **Tests:** the `files` tests now propagate through `transportItem`. `tests/transport.rs` runs whole syncs, as the text interface does in batch mode (all but the deletions, then the deletions, then the commit), in both dispatch modes:
  - copies, updates, props, deletions and a conflict;
  - moves, in each of `doMove`'s cases and the fallback, including reverting a move and a move over a directory;
  - a merge, and a conflict where a merge was asked for but one side is gone;
  - a copy that fails, logged, while the run goes on;
  - a fatal error, which stops the run before the commit; and aborting everything, which stops dispensing.
  
  With OCaml Unison, a twin of each replica pair is synced by it, and the replicas must end the same, with the same transport log (with the roots renamed and the times left out). Then OCaml finds nothing to do on gemel's pair, with fast checking and without, so the archives agree with the replicas; or, where conflicts remain, it does the same on both pairs, with the same summary. CI's oracle job runs them.

### Phase 7: `gemel-ui-api` (~1.3k)

- **`uicommon`:** profile choice and initialisation, root checks, the transfer and stats formatting, and the summary line.
- **The session contract** modelled on `uimacbridge.ml`: `init1`/`init2`, item accessors and setters, `synchronize`, abort, ignore, and diff.
- Exit codes (already ported).

Split, like phases 4 and 6, into:
- **7a:** `uicommon`, and the startup it drives;
- **7b:** what the engine owes the interfaces: progress and update-status printers, an abort flag the copies' blocking tasks can read, the status messages and timers, and the merge's log and asynchronous confirmation;
- **7c:** the session contract, on the engine thread.

What earlier phases left for phase 7, and where each goes:

| Left by | Item | Where |
|---|---|---|
| 4a, 4b | Register the `case` and `fileinfo` preferences on every host; default `path` to the empty path; call `case::init`, `osx::init` and `fileinfo::init` (which derives `links-aux`) before `propagatePrefs` | 7a (`validateAndFixupPrefs`, `initPrefs`) |
| 5 | A host that registers the engine's features registers its preferences too | 7a: one registration of every preference, in OCaml's link order, for the client and every server host |
| 6a | `initBackups` after `propagatePrefs` | 7a (`initRoots`) |
| 6c | The client's `Trace` slot, with its Unison directory, installed before anything logs (`initPrefs` logs before the roots exist) | 7a |
| 4b, 5 | The status messages of update detection, the "Reconciling changes" status, and the debug traces of both | 7b; their events 7c |
| 6a | Progress reporting and timers; aborting in the middle of a copy, which needs an abort flag that the copy's blocking task can read | 7b |
| 6b | The merge command's log and result, and merge's messages on standard error | 7b, with the merge confirmation made asynchronous |
| 6c | `doTransport`'s statistics, and catching `Transient` for each item | 7c (the session's `synchronize`) and phase 8 (`uitext`) |

Decided now for 7c, because 7a's callbacks depend on it:
- **The session runs on the engine thread** and owns the `Rpc`, the installed roots (`None` until `initRoots`, and again after `initPrefs`, as `Globals.uninstallRoots`) and the items. Frontends reach it through `EngineThread`: commands in, events out. Events carry snapshots of items (`Send`), not the engine's own values.
- **Questions to the user are requests with a reply channel,** not events: roots when none are given, the warning panel, dangerous paths, merge confirmation, and (milestone 3) passwords. So `initPrefs`'s `promptForRoots` is async.
- **A sync runs as a task of its own** on the engine thread, so the session still reads commands during it. Abort is the one command it then accepts; it sets a flag shared with the copies' blocking tasks.
- **To decide in 7c:** whether an item's remaining bytes, added when it finishes (`bytesToTransfer - bytesTransferred`), may be negative, as for a merge in OCaml, or are clamped at 0. (Decided: as in OCaml, so that the bytes transferred end equal to the total.)

**Notes (phase 7a, 2026-10-21):**
- **`uicommon`:** all of it but the self-tests (`selftest`, whose `Test` module isn't ported) and turning on GC messages.
  - Its preferences, including `repeat` with its own codec (the setting as a string).
  - The formatting: `details2string`, `reconItem2string`, `displayPath`, the root names (`roots2niceStrings`), and `dangerousPathMsg`. Paths and root names are shown with invalid UTF-8 replaced, where OCaml prints their bytes.
  - The ignore patterns (`ignorePath`, `ignoreName`, `ignoreExt`, `addIgnorePattern`), and `showDiffs` (`uicommon`'s, which follows moves; `uimacbridge`'s copy doesn't).
  - `Stats`: the rate and time left. Where OCaml's `truncate` is undefined (a time left too long for an integer, as at a rate of 0), gemel shows the default instead.
  - The startup, in `uicommon::startup`: `scanProfiles`, `uiInitClRootsAndProfile`, `initPrefs`, `initRoots`, `connectRoots` and `validateAndFixupPrefs`, with the `architecture` root command. OCaml's globals (the command line's roots, the installed roots, the profiles found) are in `Ui`, with the client's `Rpc`.
  - `transportStart`, `transportFinish` and `transportItems` stay in `gemel_engine::transport` (see 6c).
- **Every host registers every preference** (`gemel_ui_api::register_prefs`), in OCaml's link order, which is the order of the dump sent to a server. A server refuses a preference it doesn't know, so the loopback server, and phase 8's `-server`, register the same set. The `watch` preference (`Fswatch.useWatcher`) is registered, for `repeat = watch`; the watcher is phase 9.
- **`client_rpc`** builds the client with the Trace slot installed, its Unison directory set, before anything logs.
- **Ported with it:** `Props.init` (the permission mask and owners on Windows), `Util.truncateString` and `Util.padto` (both counting as OCaml does: code points, and bytes), and `String.capitalize_ascii`.
- **Every preference of the modules milestone 2 ports is registered,** so any profile OCaml accepts is accepted, except one setting `xattrs` or `acl` (cut, see above). That includes those whose features come later: `remote`'s (`sshcmd`, `servercmd`, `killserver` and the others, in `gemel_rpc::connection`), and `dumparchives`, which does nothing yet. `maxThreads` now reads `stream`.
- **For phase 8:** `-server` must register `gemel_ui_api::register_prefs` and `register_commands` (not just the engine's), or an OCaml client's `architecture` call and preference dump fail.
- **Not ported yet:** the `ui` preference and `Uicommon.interface` (phase 8, with `main`).
- **Tests:**
  - Golden vectors from OCaml (`gen_uicommon.ml`, which copies `truncateString`, `quote` and `Stats` out of Unison's sources) for truncating, quoting patterns, and the rates and times left. The rates are compared up to rounding, since `exp` and `pow` can differ in the last bit between C libraries.
  - `tests/startup.rs`: the command line's roots and profile, asking for roots, installing them once per profile, the start of the log, and the list of profiles with their keys. Adding an ignore pattern, in loopback mode: the next scan leaves the path out (so the server got the pattern) and the profile gains it. Diffs (on Unix): of the file where it now is, for a move, and the messages for a problem and for a file against a directory. Two scenarios sync from a profile (one with `ignore` and `label`, one with a `path` ending in `*`), twice each and in both dispatch modes, on twin replicas with OCaml's `unison <profile> -batch`. They must end the same, with the same log (from its start, with the text interface's summaries left out), and with each item's details as OCaml prints them. Then OCaml finds nothing to do on gemel's replicas. CI's oracle job runs it.


**Notes (phase 7b, 2026-10-21):**
- **Printers** (`gemel_engine::uutil`, from `Uutil`): a host's progress printer and update-status printer, `Send` and `Sync`, since the blocking tasks that copy and scan call them.
  - Progress is reported on the client's host, where copies between local roots run: `"l"` as a local copy goes (so a directory's files count towards its item), and `"mov"` for a move, all at once. The reports of a copy, a move or a deletion (none) add up to its `riLength`. A merge's don't: its working copies report their bytes, as in OCaml, while its `riLength` is 0.
  - The path being scanned is shown by the host that scans, as OCaml's `showStatus` does, at most every 0.05 seconds and about every 10 KiB of files. A server's (one in loopback mode too) shows nothing, as `scanInfo.showStatus` is off on servers. Each scan counts for itself, as the two roots' scans run at once.
- **The abort flag:** whether everything is aborted is now an `AllFlag` (an atomic) in the client's `Abort`. A copy's blocking task checks it at each report, and anything can set it from any thread (`AllFlag::set`, for a signal handler or the session). `Abort::reset` clears the same flag, so one taken before a run stays current. As in OCaml, cloning a whole file by its path reports once and isn't checked.
- **Status messages and timers:** "Collecting changes" (timed, then the empty status that clears it) and "Reconciling changes"; "Transmitting file" around each copy. Timers show only with `timers`.
- **Debug traces:** those of update detection on the engine thread and on the hosts (`findOnRoot`, `findLocal`, the predicates check, `updateArchive`, `markEqual`, `replaceArchiveLocal`, `updateProps`, `markPossiblyUpdated`, `checkNoUpdates`, `commitUpdates`), and recon's `reconcileAll` and result count. `Trace.debug` on a host's trace is `gemel_engine::debug`. Not ported: those of the scan itself, which runs off the engine thread, and `root2direction`'s, which has no host to trace on.
- **Merge:** the command and its result are logged ("Merge command:", and "Merge result (exited (0)):" with the output), as OCaml logs them, and the oracle now compares those entries. What the merge made of the outputs ("One output detected" and the others) goes to stderr when log entries do (`send_log_msgs_to_stderr`), as `Util.msg`, now `Trace::msg`.
- **The merge confirmation is asynchronous** (`files::ShowMerge`, through `merge` and `transport_item`): a future, so that the session can ask the user and wait for the reply.
- **Tests:** in `tests/transport.rs`, in both dispatch modes:
  - each item's progress adds up to its length (copies, a directory, a move);
  - an abort set from another thread at the first report stops the run, with the first item done and nothing more started, and the next run starts afresh;
  - the status messages and timers come in order, and the scan's path shows in direct mode, not on a server;
  - a merge's log entries, and its explanation on stderr only when log entries go there.
  
  Copy's unit tests abort a copy midway from another thread, through the flag.

**Notes (phase 7c, 2026-10-21):**
- **`gemel_ui_api::session`:** `spawn` starts a session on its own engine thread, with the client, the installed roots and the items. Frontends send `Command`s and get `Event`s, which carry snapshots (`ItemView`, `Progress`, `Summary`); every command but `Abort` and `Log` ends with one reply event, or `Event::Error`. A command refused because a sync is running gets `Event::Refused`, which is never a reply, so a frontend waiting for the sync's can't mistake it.
- **The steps are those `uimacbridge` and `uitext` share**, and what they do differently is left to them:
  - `Start` (`uiInitClRootsAndProfile` and `scanProfiles`), `ScanProfiles`, `NewProfile` (`unisonProfileInit`), and `LoadProfile` (`initPrefs`), which replies with the settings a frontend needs;
  - `Update`: connects the roots, finds the updates (with `allowPartial` as asked), reconciles, and commits at once without archives;
  - `SetAction` (left, right, skip, merge, older, newer, revert), `Ignore` (path, name or extension, then `unisonUpdateForIgnore`), and `Diff`;
  - `Synchronize`: everything not propagated yet, deletions last, between `transportStart` and `transportFinish`, with each item's transient error its failure; `Commit`; `Abort`; and `Log`.
  
  The session shows no status text of its own beyond the engine's ("Looking for changes" is both interfaces'), and logs no summary: the Mac interface's "Everything is up to date" and the text interface's "Nothing to do", its `-repeat` commit policy, and its summary lines are theirs, with `Command::Log`. A text interface's batch run is `Start`, `LoadProfile`, `Update`, then `Synchronize` unless every item is skipped, and `Commit`.
- **Questions** (`Event::Ask`) carry a oneshot for the answer; dropping it answers no. Roots when none are given, a warning, and a merge's result with `confirmmerge` (not in batch mode; otherwise the result is just shown, `Event::Merge`).
- **Warnings** are kept, since the engine's callback is synchronous (and often runs with the preferences borrowed), and asked about at the end of the step that gave them, before anything is committed: at the end of each of `Start`, `LoadProfile`, connecting, finding updates, reconciling, a sync and a commit. A loopback server's warnings reach the session too (a real server's come with remote roots). In batch mode they are only shown. Stopping unlocks the archives and ends the session (`Event::Exit`). Logging them ("Warning: …", as the Mac interface does) is `Config::log_warnings`.
- **Quitting with a question unanswered:** `EngineThread::join` now closes the event channel and drops the events not taken, so a reply channel left in one is dropped and the question is answered no (a warning then stops the session, unlocking the archives). Before, the engine waited for the answer forever, and `join` with it.
- **A sync** is a future polled alongside the command channel and the progress reports, rather than a spawned task: the same effect without sharing the session. Abort gets through even while the sync waits for the user; other commands are refused until it ends.
- **Progress:** the reports from the copies' blocking tasks come through a channel and are applied on the engine thread; an item's own are applied before its remainder is added. `Event::Progress` comes when an item's percentage changes, an item finishes, or 0.1 seconds have passed, with the rate and the time left (`Stats`). `ItemView` has `uimacbridge`'s accessors (`unisonRiToLeft`, `…ToDirection`, `…ToProgress`, `canDiff`, `changedFromDefault`) and the text interface's line and details.
- **`Summary`:** the items transferred, skipped (with why), failed, partial and not started, whether everything was aborted, the paths still to sync (`unsynchronizedPaths`, for the next `Update`), and the text interface's exit code.
- **Not ported:** `Update`'s `paths` are passed to update detection, which scans everything until the watcher (phase 9). Remote connections (`openConnection*` and the password prompts) are milestone 3.
- **Tests:** `tests/session.rs` drives a session from the test thread:
  - the two profile scenarios of `startup.rs` (now shared, in `tests/ui_support`), through the session, against OCaml's `-batch`, in both dispatch modes;
  - profiles, and roots asked for (and not given);
  - warnings shown in batch mode, and asked about otherwise, stopping (archives unlocked, session ended) or going on;
  - each action, and reverting;
  - progress up to each item's length, outcomes, a failure (a file changed after the scan), a skipped conflict, and the summary;
  - an abort while the sync waits for a merge's confirmation (with another command refused), and a merge not confirmed. The merge itself then finishes where its result is cloned back (APFS) and is aborted where it is copied (Linux), as in OCaml;
  - ignoring an extension, diffs, status events and the scanned paths, and a command before the roots are connected;
  - quitting with a question unread: the session ends, with the archives unlocked.

### Phase 8: text UI, `bins/gemel` (~2.1k)

- **`main`:** the driver: `-server` stub, profile selection, `-batch` and `-ui text`.
- **`uitext`, batch mode first:** the summary, `-terse`/`-silent` and `-auto`.
- **Then interactive mode:** the per-item prompts and keys, the confirmations, `-repeat` with a time interval, and dumb-terminal handling.

The text UI uses the phase 7 contract rather than the engine directly, so the contract has a consumer before the GUI.

Split into:
- **8a:** `main` and the text interface's batch mode, with the preferences of both: one sync with `-batch` (and `-silent`, `-terse`, `-retry`), its output, summary, log and exit code, checked against OCaml's `unison -batch -ui text` (stdout, stderr, the log, the replicas and the exit code). `-version`; `-server`, `-socket`, `-doc`, `-prefsdocs` and `-prefsman` say they aren't supported yet.
- **8b:** interactive mode (the per-item prompts and keys, the confirmations, the terminal's raw mode and dumb-terminal handling, interrupts), `-repeat` with a time interval and safe stops, the profile manager (`-i`), and `-help`'s list of options (`Prefs.printUsage`).

Decided for 8a:
- **Who writes where:** while a session step runs, only the engine writes to stderr (through the trace), and only the frontend writes to stdout. So the session prints the wait message itself (`Config`), and the frontend's own stderr output comes before the session starts or after it ends. Before exiting, the frontend joins the session, so that the log entries it asked for are written.
- **Session additions:** `Command::Configure` (what `-silent` and `-repeat` imply: `batch`, `terse`, and whether log entries go to stderr), and `Command::Connect` on its own (`start2` connects before the warning printer changes); `ItemView` with `displayri`'s parts (against the empty path) and each root's errors; `Updates` with the roots' untruncated names (`Fspath.differentSuffix`); `Settings` with the text interface's preferences and the debug modules.

**Notes (phase 8a, 2026-10-21):**
- **`gemel_ui_api::cli`:** `uitext`'s preferences (`dumbtty`, `silent`, `color`, `i`) and `main`'s (`version`, `doc`, `prefsdocs`, `prefsman`, `server`, `socket`, `listen` with its alias `host`, `ui`), registered after `uicommon`'s, as OCaml links them (`test.ml` has none).
- **`bins/gemel`:**
  - `main`: `-version`; `-doc`, `-prefsdocs`, `-prefsman`, `-server` and `-socket` say they aren't supported yet; `-ui graphic` says this binary has only the text interface, and another value prints the usage; a bad option prints the error and the usage, and exits with 2.
  - `text`, from `uitext`: `start` and `start2`, `synchronizeUntilNoFailures` (`-retry`), `synchronizeOnce`, the items as batch mode lists them (`displayri`, the details, each root's errors), `interactAndPropagateChanges` and `doTransport` in batch mode (the "items will be synced" block, the progress line, "Failed […]" as each item fails, the summary and its "skipped", "partially transferred" and "failed" lines), `checkForDangerousPath` (aborting), `verifyMerge`, the two warning printers, `formatStatus`, `errorOut` and `handleException`, and `-silent`, `-terse` and the colours.
  - `infos`: the status line (`Util.set_infos`) on stdout, which the trace's stderr sink clears before writing and shows again after, as `Util.msg` does.
- **`-retry`** sends the failed paths to `Update`, which scans everything, as OCaml's `findLocal` does without the watcher (it uses the paths only while the watcher runs).
- **`Command::Start` no longer scans the profiles** (7c's did): `uiInitClRootsAndProfile` alone, as the text interface starts. Otherwise another profile in the Unison directory, one with a bad or a shared `key`, or one that can't be read, failed or changed every run. Frontends that list profiles send `ScanProfiles`.
- **Session additions:** `Command::Configure` and `Command::Connect`; `Config::print_wait_message`; `LogKind::Status` (`Trace.status`), and `Log` taken during a sync (the progress line goes to the log every minute in batch mode); `ItemView`'s `displayri` parts, its problem, its reason to be skipped and each root's errors; `RootNames` (short, in full, and the column headings); more `Settings`.
- **Fixed in the engine:** a merge's "Warning: 'backupcurrent' is not set for path …" on stderr (a `Util.msg` that 6b left out).
- **Not ported yet (8b):** interactive mode (the binary says so, and exits with 1, without `-batch`), `-repeat`, `-i`, `-selftest`, the option list of `-help` and of usage errors, and the timer of the whole propagation ("Transmitting all files").
- **Tests:** `bins/gemel/tests/batch.rs` runs the program, and with OCaml a twin pair with `unison … -ui text`: both must end with the same exit code, the same stdout and stderr as a terminal shows them (the progress line and the spinner overwritten), the same log and the same replicas. The programs' names and versions are normalised where they appear (the banners and the usage), and the summary's local times compared within a few seconds. Scenarios: roots on the command line (a first sync, a conflict and a deletion with `-auto`, the conflict left), a profile with `-terse` and then nothing to do, only a conflict ("No updates to propagate"), `-silent`, a dangerous deletion (abort, 3), a merge (Unix), and the errors before a sync (a missing profile, too many roots, none, a bad preference), and other profiles with bad or shared keys left alone. Not tested: a problem item, and `-retry` (no failure can be provoked from outside the program). `-version` and an unknown option are checked on their own. CI's oracle job now runs it, and the session tests of 7c, which it didn't.

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
