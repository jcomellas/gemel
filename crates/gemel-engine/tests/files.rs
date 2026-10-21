//! The file operations propagation applies, through both dispatch modes;
//! with `GEMEL_REFERENCE_UNISON` set, OCaml Unison then has nothing to do
//! (its archives and both replicas agree with what gemel did).

mod support;

use std::path::Path as StdPath;

#[cfg(unix)]
use gemel_base::path::Path;
use gemel_base::util::{Error, FileId};
use gemel_engine::common::ReconItem;
#[cfg(unix)]
use gemel_engine::common::{Direction, Replicas};
use gemel_engine::{files, recon, transport, update};

use support::oracle::*;
use support::sync::*;
use support::*;

impl Setup {
    /// Reconciles, and propagates every item in order (with
    /// `Transport.transportItem`, but without putting deletions last), then
    /// commits. Returns the items and each one's result.
    fn propagate(&self) -> Vec<(ReconItem, Result<(), Error>)> {
        self.propagate_after(|| {})
    }

    /// [`Setup::propagate`], running `between` after reconciling.
    fn propagate_after(&self, between: impl FnOnce()) -> Vec<(ReconItem, Result<(), Error>)> {
        settle(&[&self.a(), &self.b()]);
        let found = self.find();
        let (items, _, _) =
            block_on(recon::reconcile_all(&self.rpc, &self.roots, false, found)).unwrap();
        between();
        // As transport does at the start of a run.
        self.rpc
            .client_host()
            .state::<gemel_engine::abort::Abort>()
            .borrow_mut()
            .reset();
        let mut results = Vec::new();
        for (i, ri) in items.into_iter().enumerate() {
            let r = block_on(transport::transport_item(
                &self.rpc,
                &self.roots,
                &ri,
                FileId(i as i64),
                &mut |_, _| Box::pin(async { true }),
            ));
            results.push((ri, r));
        }
        block_on(update::commit_updates(&self.rpc, &self.roots)).unwrap();
        results
    }

    /// Propagates, expecting every item to succeed.
    fn propagate_ok(&self) -> Vec<String> {
        self.propagate()
            .into_iter()
            .map(|(ri, r)| {
                let p = String::from_utf8_lossy(ri.path1.as_bytes()).into_owned();
                r.unwrap_or_else(|e| panic!("{p}: {e}"));
                p
            })
            .collect()
    }
}

/// The replicas are the same, but for OCaml's backup files.
fn assert_mirrored(s: &Setup) {
    assert_eq!(listing(&s.a()), listing(&s.b()));
}

#[test]
fn new_files_and_trees_are_copied() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-new");
        let s = synced(&dir, loopback, &[], build_tree);
        write(&s.a(), "new", "a new file", T0 + 20.0);
        write(&s.a(), "tree/one", "1", T0 + 20.0);
        write(&s.a(), "tree/sub/two", "2", T0 + 20.0);
        write(&s.b(), "from-b", "the other way", T0 + 20.0);
        assert_eq!(s.propagate_ok(), ["from-b", "new", "tree"]);
        assert_mirrored(&s);
        ocaml_agrees(&s, &[]);
    }
}

#[test]
fn changed_files_are_updated() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-update");
        let s = synced(&dir, loopback, &[], build_tree);
        write(&s.a(), "README", "read me again, please", T0 + 20.0);
        write(
            &s.b(),
            "src/main.c",
            "int main(void) { return 1; }",
            T0 + 20.0,
        );
        assert_eq!(s.propagate_ok(), ["README", "src/main.c"]);
        assert_mirrored(&s);
        ocaml_agrees(&s, &[]);
    }
}

#[test]
fn deletions_are_propagated_and_backed_up() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-delete");
        let backups = format!("backupdir = {}", dir.0.join("backups").display());
        let prefs = ["backup = Name *", backups.as_str()];
        let s = synced(&dir, loopback, &prefs, build_tree);
        std::fs::remove_file(s.a().join("README")).unwrap();
        std::fs::remove_dir_all(s.a().join("src")).unwrap();
        assert_eq!(s.propagate_ok(), ["README", "src"]);
        assert_mirrored(&s);
        assert_eq!(
            listing(&dir.0.join("backups")),
            [
                ("README".into(), "read me".into()),
                ("src/".into(), String::new()),
                ("src/deep/".into(), String::new()),
                ("src/deep/x".into(), "x".into()),
                ("src/main.c".into(), "int main(void) { return 0; }".into()),
            ]
        );
        ocaml_agrees(&s, &prefs);
    }
}

#[cfg(unix)]
#[test]
fn props_are_set() {
    use std::os::unix::fs::PermissionsExt;
    for loopback in [false, true] {
        let dir = TempDir::new("files-props");
        let s = synced(&dir, loopback, &[], build_tree);
        std::fs::set_permissions(s.a().join("README"), PermissionsExt::from_mode(0o600)).unwrap();
        set_mtime(&s.b().join("src/main.c"), T0 + 50.0);
        assert_eq!(s.propagate_ok(), ["README", "src/main.c"]);
        let mode = |p: &StdPath| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&s.b().join("README")), 0o600);
        let mtime = |p: &StdPath| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(
            mtime(&s.a().join("src/main.c")),
            mtime(&s.b().join("src/main.c"))
        );
        ocaml_agrees(&s, &[]);
    }
}

#[test]
fn a_file_replaces_a_directory_and_back() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-replace");
        let s = synced(&dir, loopback, &[], build_tree);
        // Over a directory: moved aside first, under the commit log.
        std::fs::remove_dir_all(s.a().join("src")).unwrap();
        write(&s.a(), "src", "now a file", T0 + 20.0);
        // And a directory over a file.
        std::fs::remove_file(s.a().join("README")).unwrap();
        write(&s.a(), "README/inside", "now a directory", T0 + 20.0);
        assert_eq!(s.propagate_ok(), ["README", "src"]);
        assert_mirrored(&s);
        assert!(!s.base.join("u/DANGER.README").exists());
        ocaml_agrees(&s, &[]);
    }
}

#[cfg(unix)]
#[test]
fn links_are_made() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-links");
        let s = synced(&dir, loopback, &[], build_tree);
        std::os::unix::fs::symlink("src/main.c", s.a().join("newlink")).unwrap();
        std::fs::remove_file(s.a().join("link")).unwrap();
        std::os::unix::fs::symlink("elsewhere", s.a().join("link")).unwrap();
        assert_eq!(s.propagate_ok(), ["link", "newlink"]);
        assert_mirrored(&s);
        ocaml_agrees(&s, &[]);
    }
}

#[cfg(unix)]
#[test]
fn a_partly_failed_copy_keeps_what_worked() {
    use std::os::unix::fs::PermissionsExt;
    // Root can read anything.
    if unsafe_euid_is_root() {
        eprintln!("skipped: running as root");
        return;
    }
    for loopback in [false, true] {
        let dir = TempDir::new("files-partial");
        let s = synced(&dir, loopback, &[], build_tree);
        write(&s.a(), "tree/ok", "fine", T0 + 20.0);
        write(&s.a(), "tree/secret", "unreadable", T0 + 20.0);
        let secret = s.a().join("tree/secret");
        // Unreadable after update detection, when it is copied.
        let results = s.propagate_after(|| {
            std::fs::set_permissions(&secret, PermissionsExt::from_mode(0o000)).unwrap()
        });
        let (ri, r) = &results[0];
        assert_eq!(ri.path1.as_bytes(), b"tree");
        let err = r.as_ref().unwrap_err().to_string();
        assert!(err.contains("secret"), "{err}");
        assert_eq!(read(&s.b().join("tree/ok")), "fine");
        assert!(!s.b().join("tree/secret").exists());
        // Readable again, it is copied next time.
        std::fs::set_permissions(&secret, PermissionsExt::from_mode(0o644)).unwrap();
        assert_eq!(s.propagate_ok(), ["tree/secret"]);
        assert_mirrored(&s);
        ocaml_agrees(&s, &[]);
    }
}

#[cfg(unix)]
fn unsafe_euid_is_root() -> bool {
    std::env::var("USER").is_ok_and(|u| u == "root")
        || std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| o.stdout.starts_with(b"0\n"))
}

#[test]
fn conflicts_overridden_keep_a_copy() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-conflict");
        let prefs = ["copyonconflict = true", "prefer = a"];
        let s = synced(&dir, loopback, &prefs, build_tree);
        write(&s.a(), "README", "a's version", T0 + 20.0);
        write(&s.b(), "README", "b's version", T0 + 30.0);
        assert_eq!(s.propagate_ok(), ["README"]);
        assert_eq!(read(&s.b().join("README")), "a's version");
        let kept: Vec<_> = listing(&s.b())
            .into_iter()
            .filter(|(n, _)| n.starts_with("README (conflict_on_"))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].1, "b's version");
    }
}

#[test]
fn a_leftover_commit_log_stops_the_run() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-log");
        let s = setup(&dir, loopback);
        block_on(files::process_commit_logs(&s.rpc, &s.roots)).unwrap();
        let log = s.base.join("u/DANGER.README");
        std::fs::write(&log, "stopped").unwrap();
        let err = block_on(files::process_commit_logs(&s.rpc, &s.roots)).unwrap_err();
        assert_eq!(
            err,
            Error::Fatal(format!(
                "Warning: the previous run of unison terminated in a dangerous state.\n            \
                 Please consult the file {}, delete it, and try again.",
                String::from_utf8_lossy(&s.host().file_in_unison_dir("DANGER.README"))
            ))
        );
        // OCaml stops the same way.
        if let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") {
            let out = std::process::Command::new(unison)
                .arg(s.a())
                .arg(s.b())
                .args(["-batch", "-ui", "text", "-contactquietly"])
                .env("UNISON", s.base.join("u"))
                .env("HOME", &s.base)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let all = String::from_utf8_lossy(&out.stderr).into_owned()
                + &String::from_utf8_lossy(&out.stdout);
            assert!(all.contains(&err.to_string()), "{all}");
        }
    }
}

#[test]
fn moves_are_renames() {
    for loopback in [false, true] {
        let dir = TempDir::new("files-moves");
        let prefs = ["moves-experimental = true"];
        let s = synced(&dir, loopback, &prefs, build_tree);
        std::fs::rename(s.a().join("README"), s.a().join("READ-ME")).unwrap();
        std::fs::rename(s.b().join("src"), s.b().join("source")).unwrap();
        // The moves keep the inodes: nothing is copied.
        let ino = |p: &StdPath| {
            gemel_fs::system::stat(p.to_str().unwrap().as_bytes())
                .unwrap()
                .ino
        };
        let readme = ino(&s.b().join("README"));
        let main_c = ino(&s.a().join("src/main.c"));
        assert_eq!(s.propagate_ok(), ["README", "src"]);
        assert_mirrored(&s);
        assert_eq!(ino(&s.b().join("READ-ME")), readme);
        assert_eq!(ino(&s.a().join("source/main.c")), main_c);
        ocaml_agrees(&s, &prefs);
    }
}

/// Twin replicas with the same conflicting change to `m`, which the
/// `merge` preference says to merge with `script`.
#[cfg(unix)]
fn merge_conflict(dir: &TempDir, loopback: bool, prefs: &[&str]) -> Setup {
    let s = synced(dir, loopback, prefs, |r| write(r, "m", "base\n", T0));
    write(&s.a(), "m", "base\na\n", T0 + 20.0);
    write(&s.b(), "m", "base\nb\n", T0 + 30.0);
    s
}

/// gemel merges what OCaml merges, for each kind of result the merge
/// program can leave, and OCaml then has nothing to do.
#[cfg(unix)]
#[test]
fn merges_like_ocaml() {
    let scripts = [
        // One output.
        "cat CURRENT1 CURRENT2 > NEW",
        // Two equal outputs.
        "cat CURRENT1 CURRENT2 > NEW1; cp NEW1 NEW2",
        // Two different outputs, but success: the first wins.
        "echo one > NEW1; echo two > NEW2",
        // Two different outputs and a failure: both kept, still a conflict.
        "echo one > NEW1; echo two > NEW2; exit 1",
        // No outputs: the first input changed in place.
        "cat CURRENT2 >> CURRENT1",
        // Both changed the same way.
        "echo same > CURRENT1; echo same > CURRENT2",
        // Failures.
        "echo x > NEW; exit 1",
        "true",
        // Failures with something to show (the log is compared too).
        "echo out; echo err >&2; exit 2",
        "kill -KILL $$",
    ];
    for script in scripts {
        let merge = format!("merge = Name m -> {script}");
        let prefs = [merge.as_str()];
        for loopback in [false, true] {
            let dir = TempDir::new("files-merge");
            let s = merge_conflict(&dir, loopback, &prefs);
            settle(&[&s.a(), &s.b()]);
            let found = s.find();
            let (items, _, _) =
                block_on(recon::reconcile_all(&s.rpc, &s.roots, false, found)).unwrap();
            let [ri] = &items[..] else {
                panic!("{items:?}")
            };
            let Replicas::Different(d) = &ri.replicas else {
                panic!()
            };
            assert_eq!(d.direction, Direction::Merge);
            let roots = s.roots.list();
            let mut shown = Vec::new();
            let r = block_on(files::merge(
                &s.rpc,
                &roots[0],
                &ri.path1,
                &d.rc1.ui,
                &roots[1],
                &ri.path2,
                &d.rc2.ui,
                FileId(0),
                &mut |title, log| {
                    shown.push((title, log));
                    Box::pin(async { true })
                },
            ));
            block_on(update::commit_updates(&s.rpc, &s.roots)).unwrap();
            assert_eq!(shown[0].0, "Results of merging m", "{script}");
            // No working files are left.
            let leftovers: Vec<_> = listing(&s.a())
                .into_iter()
                .filter(|(n, _)| n.contains(".unison."))
                .collect();
            assert!(leftovers.is_empty(), "{script}: {leftovers:?}");
            let ours = listings(&s);
            if r.is_ok() && ours.0 == ours.1 {
                ocaml_agrees(&s, &prefs);
            }
            let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
                continue;
            };
            let twin_dir = TempDir::new("files-merge-ocaml");
            let twin = merge_conflict(&twin_dir, false, &prefs);
            let out = std::process::Command::new(&unison)
                .arg(twin.a())
                .arg(twin.b())
                .args([
                    "-batch",
                    "-ui",
                    "text",
                    "-log=false",
                    "-contactquietly",
                    "-times",
                ])
                .arg(format!("-merge=Name m -> {script}"))
                .env("UNISON", twin.base.join("u"))
                .env("HOME", &twin.base)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let all = String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                ours,
                listings(&twin),
                "{script} (loopback {loopback}): {all}\n{r:?}"
            );
            // gemel fails where OCaml does.
            assert_eq!(
                r.is_err(),
                all.contains("failed: m"),
                "{script}: {all}\n{r:?}"
            );
            // And shows the same log (batch mode prints it).
            let log = &shown[0].1;
            assert!(
                log.is_empty() || all.contains(log.as_str()),
                "{script}: {log:?}\n{all}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn diffs_run_the_diff_command() {
    let dir = TempDir::new("files-diff");
    let s = merge_conflict(&dir, false, &[]);
    settle(&[&s.a(), &s.b()]);
    let found = s.find();
    let (items, _, _) = block_on(recon::reconcile_all(&s.rpc, &s.roots, false, found)).unwrap();
    let Replicas::Different(d) = &items[0].replicas else {
        panic!()
    };
    let roots = s.roots.list();
    let mut shown = Vec::new();
    block_on(files::diff(
        &s.rpc,
        &roots[0],
        &items[0].path1,
        &d.rc1.ui,
        &roots[1],
        &items[0].path2,
        &d.rc2.ui,
        &mut |cmd, result| shown.push((cmd.to_owned(), result.to_owned())),
        FileId(0),
    ))
    .unwrap();
    let (cmd, result) = &shown[0];
    // b's is newer: the older (a's) first.
    assert_eq!(
        cmd,
        &format!(
            "diff -u '{}' '{}'",
            s.roots.list()[0]
                .fspath
                .concat(&Path::<gemel_base::path::Local>::from_bytes(b"m").unwrap()),
            s.roots.list()[1]
                .fspath
                .concat(&Path::<gemel_base::path::Local>::from_bytes(b"m").unwrap())
        )
    );
    assert!(result.contains("-a\n+b"), "{result}");
    assert!(result.ends_with("Exited with status 1"), "{result}");
}

#[test]
fn ls_lists_matching_entries() {
    let dir = TempDir::new("files-ls");
    for f in ["b.txt", "a.txt", "c.md"] {
        write(&dir.0, f, "", T0);
    }
    let d = dir.0.to_str().unwrap();
    assert_eq!(files::ls(d, "*.txt").unwrap(), ["a.txt", "b.txt"]);
    // `*` doesn't match a leading dot (so neither `.` nor `..`).
    assert_eq!(files::ls(d, "*").unwrap(), ["a.txt", "b.txt", "c.md"]);
    assert_eq!(files::ls(d, ".*").unwrap(), [".", ".."]);
}

/// Missing parents of a path given with `path` are created with the
/// source's props, each level its own.
#[cfg(unix)]
#[test]
fn missing_parents_get_their_props() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &StdPath| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let make = |s: &Setup| {
        write(&s.a(), "x/y/z", "deep", T0 + 20.0);
        std::fs::set_permissions(s.a().join("x/y"), PermissionsExt::from_mode(0o705)).unwrap();
        std::fs::set_permissions(s.a().join("x"), PermissionsExt::from_mode(0o750)).unwrap();
    };
    for loopback in [false, true] {
        let dir = TempDir::new("files-parents");
        let s = synced(&dir, loopback, &[], build_tree);
        s.set_prefs(|p| p.set(&gemel_engine::globals::PATHS, vec![global("x/y/z")]));
        make(&s);
        assert_eq!(s.propagate_ok(), ["x/y/z"]);
        assert_eq!(read(&s.b().join("x/y/z")), "deep");
        let ours = (mode(&s.b().join("x")), mode(&s.b().join("x/y")));
        assert_eq!(ours, (0o750, 0o705));
        if let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") {
            let twin_dir = TempDir::new("files-parents-ocaml");
            let twin = synced(&twin_dir, false, &[], build_tree);
            make(&twin);
            let all = ocaml_sync(&unison, &twin, &["-path=x/y/z".to_owned()]);
            let theirs = (mode(&twin.b().join("x")), mode(&twin.b().join("x/y")));
            assert_eq!(ours, theirs, "{all}");
        }
    }
}

/// `copyonconflict` keeps the overridden side's version under a conflict
/// name, as OCaml does, in each kind of conflict; and the next run agrees.
#[test]
fn conflict_copies_like_ocaml() {
    type Change = fn(&StdPath, &StdPath);
    let changes: [(&str, Change); 3] = [
        ("both changed", |a, b| {
            write(a, "README", "a's version", T0 + 20.0);
            write(b, "README", "b's version", T0 + 30.0);
        }),
        ("changed here, deleted there", |a, b| {
            write(a, "README", "a's version", T0 + 20.0);
            std::fs::remove_file(b.join("README")).unwrap();
        }),
        ("deleted here, changed there", |a, b| {
            std::fs::remove_file(a.join("README")).unwrap();
            write(b, "README", "b's version", T0 + 30.0);
        }),
    ];
    let prefs = ["copyonconflict = true", "prefer = a"];
    for (what, change) in changes {
        for loopback in [false, true] {
            let dir = TempDir::new("files-conflicts");
            let s = synced(&dir, loopback, &prefs, build_tree);
            change(&s.a(), &s.b());
            for (ri, r) in s.propagate() {
                r.unwrap_or_else(|e| panic!("{what}: {:?}: {e}", ri.path1));
            }
            let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
                continue;
            };
            let twin_dir = TempDir::new("files-conflicts-ocaml");
            let twin = synced(&twin_dir, false, &prefs, build_tree);
            change(&twin.a(), &twin.b());
            let all = ocaml_sync(&unison, &twin, &flags_of(&prefs));
            assert_eq!(listings(&s), listings(&twin), "{what}: {all}");
            // The next run does the same on both.
            let ours = ocaml_sync(&unison, &s, &flags_of(&prefs));
            let theirs = ocaml_sync(&unison, &twin, &flags_of(&prefs));
            assert_eq!(listings(&s), listings(&twin), "{what}:\n{ours}\n{theirs}");
        }
    }
}
