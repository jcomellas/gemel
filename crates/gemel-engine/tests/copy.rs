//! Copying between local roots, and backups, through both dispatch modes;
//! with `GEMEL_REFERENCE_UNISON` set, also against OCaml Unison's backup
//! names and ignore patterns.

mod support;

use std::path::Path as StdPath;

use gemel_base::case;
use gemel_base::path::{Local, Path};
use gemel_base::util::{Error, FileId};
use gemel_engine::archive::{self, ArchiveVersion, UPDATE_PREDICATES_KEY};
use gemel_engine::copy::{self, Ctx, Replacing, Source, SourceCheck};
use gemel_engine::globals;
use gemel_engine::stasher::{self, Disposition};
use gemel_fs::fileinfo::{self, FsConfig};
use gemel_fs::fspath::Fspath;
use gemel_fs::os::{self, FullFingerprint};
use gemel_fs::osx;

use support::sync::*;
use support::*;

fn local(s: &str) -> Path<Local> {
    Path::from_bytes(s.as_bytes()).unwrap()
}

impl Setup {
    fn fspath(&self, i: usize) -> Fspath {
        self.roots.list()[i].fspath.clone()
    }

    fn ctx(&self) -> Ctx {
        Ctx::of(self.host()).unwrap()
    }

    fn fs(&self) -> FsConfig {
        self.ctx().fs
    }

    /// What update detection records of a file in root `i`.
    fn source(&self, i: usize, path: &str) -> Source {
        let (fspath, path, c) = (self.fspath(i), local(path), self.fs());
        let info = fileinfo::get(true, &fspath, &path, &|_| false, &c).unwrap();
        let fp = os::fingerprint(&fspath, &path, info.typ.info_kind(), c.props.rsrc).unwrap();
        Source {
            desc: info.desc.clone(),
            fp,
            stamp: Some(info.stamp(&c)),
            ress: info.ress_stamp(),
        }
    }

    /// Copies `from` in root a to `to` in root b, replacing `real`.
    fn copy(&self, from: &str, source: &Source, to: &str, real: &str) -> Result<(), Error> {
        let roots = self.roots.list();
        block_on(copy::file(
            &self.rpc,
            &roots[0],
            &local(from),
            &roots[1],
            &self.fspath(1),
            &local(to),
            &local(real),
            Replacing::Copy,
            source,
            FileId(0),
        ))
        .map(drop)
    }

    /// The backup settings: central backups in `dir/backups`, and `prefs`.
    fn backups(&self, prefs: &[&str]) {
        let dir = self.base.join("backups");
        let mut lines = vec![format!("backupdir = {}", dir.display())];
        lines.extend(prefs.iter().map(|l| l.to_string()));
        self.set_prefs(|p| p.load_strings(&lines).unwrap());
        self.init_backups();
    }
}

fn read(p: &StdPath) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The files under `dir`, with their contents (`->target` for links).
fn listing(dir: &StdPath) -> Vec<(String, String)> {
    fn walk(root: &StdPath, d: &StdPath, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let t = e.file_type().unwrap();
            if t.is_symlink() {
                out.push((
                    rel,
                    format!("->{}", std::fs::read_link(&p).unwrap().display()),
                ));
            } else if t.is_dir() {
                out.push((format!("{rel}/"), String::new()));
                walk(root, &p, out);
            } else {
                out.push((rel, read(&p)));
            }
        }
    }
    let mut out = Vec::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// Skips tests that would write backups where the environment says.
fn backup_dir_from_env() -> bool {
    let set = ["UNISONBACKUPDIR", "UNISONMIRRORDIR"]
        .iter()
        .any(|v| std::env::var_os(v).is_some());
    if set {
        eprintln!("skipped: UNISONBACKUPDIR or UNISONMIRRORDIR is set");
    }
    set
}

#[cfg(unix)]
fn mode_of(p: &StdPath) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

fn mtime_of(p: &StdPath) -> f64 {
    std::fs::metadata(p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// ---- Copying ----

#[test]
fn copies_a_file_with_its_props() {
    for loopback in [false, true] {
        let dir = TempDir::new("copy-file");
        let s = setup(&dir, loopback);
        write(&s.a(), "x", "contents", T0);
        #[cfg(unix)]
        std::fs::set_permissions(
            s.a().join("x"),
            std::os::unix::fs::PermissionsExt::from_mode(0o640),
        )
        .unwrap();
        // An old temporary file goes first.
        write(&s.b(), "tmp", "an older attempt", T0);
        let source = s.source(0, "x");
        s.copy("x", &source, "tmp", "x").unwrap();
        let copied = s.b().join("tmp");
        assert_eq!(read(&copied), "contents");
        assert_eq!(mtime_of(&copied), T0);
        #[cfg(unix)]
        assert_eq!(mode_of(&copied), 0o640);
        // The source is untouched.
        assert_eq!(read(&s.a().join("x")), "contents");
    }
}

#[test]
fn large_files_copy_in_chunks() {
    let dir = TempDir::new("copy-large");
    let s = setup(&dir, false);
    // More than the read buffer and the progress step.
    let big: String = (0..300_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    write(&s.a(), "big", &big, T0);
    let source = s.source(0, "big");
    s.copy("big", &source, "big", "big").unwrap();
    assert_eq!(read(&s.b().join("big")), big);
    // Directly, counting the progress (the unit tests in `copy` cover
    // reading and writing, which this platform may not get to).
    let ctx = s.ctx();
    let mut seen = 0;
    let mut progress = |n| seen += n;
    let ok = || Ok(());
    copy::local_file(
        &ctx,
        &s.fspath(0),
        &local("big"),
        &s.fspath(1),
        &local("again"),
        &local("again"),
        Replacing::Copy,
        &source.desc,
        0,
        &mut copy::Progress {
            progress: &mut progress,
            check: &ok,
        },
    )
    .unwrap();
    assert_eq!(read(&s.b().join("again")), big);
    assert_eq!(seen, 300_000);
}

#[test]
fn an_abort_stops_a_copy() {
    let dir = TempDir::new("copy-abort");
    let s = setup(&dir, false);
    write(&s.a(), "x", &"x".repeat(200_000), T0);
    let source = s.source(0, "x");
    let mut seen = Vec::new();
    let mut progress = |n| seen.push(n);
    let checks = std::cell::Cell::new(0);
    let abort = || {
        checks.set(checks.get() + 1);
        Err(Error::Transient(
            gemel_engine::abort::ABORTED_BY_USER.into(),
        ))
    };
    let r = copy::local_file(
        &s.ctx(),
        &s.fspath(0),
        &local("x"),
        &s.fspath(1),
        &local("x"),
        &local("x"),
        Replacing::Copy,
        &source.desc,
        0,
        &mut copy::Progress {
            progress: &mut progress,
            check: &abort,
        },
    );
    match r {
        // Cloning by path (macOS) reports it all at once, without checking.
        Ok(()) => {
            assert_eq!((checks.get(), &seen[..]), (0, &[200_000][..]));
            #[cfg(not(target_os = "macos"))]
            panic!("only macOS clones by path");
        }
        Err(e) => {
            assert!(gemel_engine::abort::is_abort(&e), "{e}");
            assert_eq!(checks.get(), 1);
        }
    }
}

/// With `perms` restricting what is synchronised, replacing a file keeps
/// its other bits, and a new one gets the defaults'.
#[cfg(unix)]
#[test]
fn replaced_files_keep_their_unsynchronised_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("copy-perms");
    let s = setup(&dir, false);
    s.set_prefs(|p| p.set(&gemel_fs::props::PERMS, 0o700));
    write(&s.a(), "x", "new", T0);
    std::fs::set_permissions(s.a().join("x"), PermissionsExt::from_mode(0o755)).unwrap();
    write(&s.b(), "x", "old", T0);
    std::fs::set_permissions(s.b().join("x"), PermissionsExt::from_mode(0o640)).unwrap();
    let source = s.source(0, "x");
    let copy = |to: &str, replacing| {
        copy::Progress::none(|p| {
            copy::local_file(
                &s.ctx(),
                &s.fspath(0),
                &local("x"),
                &s.fspath(1),
                &local(to),
                &local("x"),
                replacing,
                &source.desc,
                0,
                p,
            )
        })
        .unwrap();
        mode_of(&s.b().join(to))
    };
    assert_eq!(copy("updated", Replacing::Update(3, 0)), 0o740);
    let default = gemel_fs::props::Props::file_default().perm.bits as u32;
    assert_eq!(copy("new", Replacing::Copy), 0o700 | (default & 0o077));
}

#[test]
fn a_source_changed_during_the_sync_fails_the_copy() {
    for loopback in [false, true] {
        let dir = TempDir::new("copy-changed");
        let s = setup(&dir, loopback);
        write(&s.a(), "x", "before", T0);
        let source = s.source(0, "x");
        write(&s.a(), "x", "after the scan", T0 + 10.0);
        let err = s.copy("x", &source, "tmp", "x").unwrap_err();
        assert_eq!(
            err,
            Error::Transient(format!(
                "The source file {}\nhas been modified during synchronization.  Transfer \
                 aborted.",
                s.fspath(0).concat(&local("x"))
            ))
        );
    }
}

#[test]
fn a_change_fast_checking_missed_is_recorded() {
    for loopback in [false, true] {
        let dir = TempDir::new("copy-missed");
        let s = setup(&dir, loopback);
        write(&s.a(), "x", "same", T0);
        write(&s.b(), "x", "same", T0);
        s.commit(&s.find());
        // The archive's fingerprint is stale, though times, length and inode
        // match: as if the file changed in place within the second.
        let mut source = s.source(0, "x");
        source.fp = FullFingerprint {
            data: gemel_fs::fingerprint::Fingerprint(md5("other")),
            ..source.fp
        };
        let err = s.copy("x", &source, "tmp", "x").unwrap_err();
        assert_eq!(
            err,
            Error::Transient(format!(
                "The source file {}\nhas been modified but the fast update detection \
                 mechanism\nfailed to detect it.  Try running once with the fastcheck\noption \
                 set to 'no'.",
                s.fspath(0).concat(&local("x"))
            ))
        );
        // The next scan looks at it again.
        assert!(matches!(
            s.archived(0, "x"),
            gemel_engine::archive::Archive::File(_, _, gemel_fs::fileinfo::Stamp::Rescan, _)
        ));
    }
}

#[test]
fn pseudo_fingerprints_are_checked_against_the_copy() {
    for loopback in [false, true] {
        let dir = TempDir::new("copy-pseudo");
        let s = setup(&dir, loopback);
        write(&s.a(), "x", "contents", T0);
        let mut source = s.source(0, "x");
        source.fp = FullFingerprint::pseudo(b"x", 8);
        s.copy("x", &source, "tmp", "x").unwrap();
        assert_eq!(read(&s.b().join("tmp")), "contents");
        // A source that isn't what was copied.
        let check = SourceCheck {
            path: local("x"),
            desc: source.desc.clone(),
            fp: source.fp.clone(),
            stamp: source.stamp,
            ress: source.ress.clone(),
            new_fp: Some(s.source(0, "x").fp),
            paranoid: false,
        };
        let roots = s.roots.list();
        block_on(
            s.rpc
                .call_root(&copy::CHECK_FOR_CHANGES_TO_SOURCE, &roots[0], check.clone()),
        )
        .unwrap();
        write(&s.a(), "x", "changed", T0);
        let err = block_on(
            s.rpc
                .call_root(&copy::CHECK_FOR_CHANGES_TO_SOURCE, &roots[0], check),
        )
        .unwrap_err();
        assert_eq!(
            err,
            Error::Transient(format!(
                "Current source file {}\n not same as transferred file.  Transfer aborted.",
                s.fspath(0).concat(&local("x"))
            ))
        );
    }
}

#[test]
fn a_bad_copy_is_kept_aside() {
    for loopback in [false, true] {
        let dir = TempDir::new("copy-bad");
        let s = setup(&dir, loopback);
        write(&s.b(), "d/.unison.f.tmp", "garbled", T0);
        let roots = s.roots.list();
        let err = block_on(s.rpc.call_root(
            &copy::SAVE_TEMP_FILE,
            &roots[1],
            (
                local("d/.unison.f.tmp"),
                local("d/f"),
                "file contents".into(),
            ),
        ))
        .unwrap_err();
        // At the root, named after the real file.
        let kept: Vec<_> = listing(&s.b())
            .into_iter()
            .filter(|(n, _)| !n.ends_with('/'))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        let (name, contents) = &kept[0];
        assert!(
            name.starts_with(".unison.f-bad") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_eq!(contents, "garbled");
        assert_eq!(
            err,
            Error::Transient(format!(
                "The file d/.unison.f.tmp was incorrectly transferred  (fingerprint mismatch \
                 in file contents) -- temp file saved as {}",
                s.fspath(1).concat(&local(name))
            ))
        );
    }
}

#[test]
fn trees_copy_recursively() {
    let dir = TempDir::new("copy-tree");
    let s = setup(&dir, false);
    build_tree(&s.a());
    copy::recursively(
        &s.ctx(),
        &s.fspath(0),
        &Path::empty(),
        &s.fspath(1),
        &local("copy"),
    )
    .unwrap();
    assert_eq!(listing(&s.b().join("copy")), listing(&s.a()));
    // Nothing there: nothing to copy.
    copy::recursively(
        &s.ctx(),
        &s.fspath(0),
        &local("gone"),
        &s.fspath(1),
        &local("gone"),
    )
    .unwrap();
    assert!(!s.b().join("gone").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn resource_forks_are_copied() {
    for loopback in [false, true] {
        let dir = TempDir::new("copy-rsrc");
        let s = setup(&dir, loopback);
        s.set_prefs(|p| osx::init(p, true));
        write(&s.a(), "x", "data", T0);
        let fork = s.a().join("x/..namedfork/rsrc");
        std::fs::write(&fork, "the resource fork").unwrap();
        let source = s.source(0, "x");
        assert_eq!(osx::ress_length(&source.ress), 17);
        s.copy("x", &source, "tmp", "x").unwrap();
        assert_eq!(read(&s.b().join("tmp")), "data");
        assert_eq!(
            read(&s.b().join("tmp/..namedfork/rsrc")),
            "the resource fork"
        );
    }
}

// ---- Backups ----

#[test]
fn backups_rotate_up_to_maxbackups() {
    if backup_dir_from_env() {
        return;
    }
    for loopback in [false, true] {
        let dir = TempDir::new("stash-rotate");
        let s = setup(&dir, loopback);
        s.backups(&["backup = Name *"]);
        let backup = |s: &Setup| {
            stasher::backup(
                s.host(),
                &s.fspath(1),
                &local("d/f"),
                Disposition::AndRemove,
                &gemel_engine::archive::Archive::NoArchive,
            )
            .unwrap();
        };
        for v in ["v1", "v2", "v3", "v4"] {
            write(&s.b(), "d/f", v, T0);
            backup(&s);
            assert!(!s.b().join("d/f").exists());
        }
        // Central: the newest has the file's name.
        assert_eq!(
            listing(&s.base.join("backups")),
            [
                ("d/".into(), String::new()),
                ("d/.bak.1.f".into(), "v3".into()),
                ("d/.bak.2.f".into(), "v2".into()),
                ("d/f".into(), "v4".into()),
            ]
        );
        // The same contents as the newest backup: none needed.
        write(&s.b(), "d/f", "v4", T0);
        backup(&s);
        assert!(!s.b().join("d/f").exists());
        assert_eq!(listing(&s.base.join("backups")).len(), 4);
        // By copying, the original stays.
        write(&s.b(), "d/f", "v5", T0);
        stasher::backup(
            s.host(),
            &s.fspath(1),
            &local("d/f"),
            Disposition::ByCopying,
            &gemel_engine::archive::Archive::NoArchive,
        )
        .unwrap();
        assert_eq!(read(&s.b().join("d/f")), "v5");
        assert_eq!(read(&s.base.join("backups/d/f")), "v5");
        // Not matched by backup: just removed.
        s.backups(&["backup = Name *", "backupnot = Name f"]);
        stasher::backup(
            s.host(),
            &s.fspath(1),
            &local("d/f"),
            Disposition::AndRemove,
            &gemel_engine::archive::Archive::NoArchive,
        )
        .unwrap();
        assert!(!s.b().join("d/f").exists());
        assert_eq!(read(&s.base.join("backups/d/f")), "v5");
    }
}

#[test]
fn local_backups_sit_next_to_the_file_and_are_ignored() {
    for loopback in [false, true] {
        let dir = TempDir::new("stash-local");
        let s = setup(&dir, loopback);
        s.backups(&[
            "backup = Name *",
            "backuploc = local",
            "backupprefix = old/$VERSION-",
            "backupsuffix = .orig",
        ]);
        for v in ["v1", "v2"] {
            write(&s.b(), "d/f", v, T0);
            stasher::backup(
                s.host(),
                &s.fspath(1),
                &local("d/f"),
                Disposition::AndRemove,
                &gemel_engine::archive::Archive::NoArchive,
            )
            .unwrap();
        }
        assert_eq!(
            listing(&s.b()),
            [
                ("d/".into(), String::new()),
                ("d/old/".into(), String::new()),
                ("d/old/0-f.orig".into(), "v2".into()),
                ("d/old/1-f.orig".into(), "v1".into()),
            ]
        );
        // The backup directories are ignored (where the hosts scan).
        let prefs = s.host().prefs().borrow();
        let ignore = globals::Ignore::new(&prefs, case::mode(&prefs)).unwrap();
        assert!(ignore.test(&local("d/old")));
        assert!(ignore.test(&local("x.unison.bak")));
        assert!(!ignore.test(&local("d/f")));
    }
}

#[test]
fn backup_names_need_a_version() {
    let dir = TempDir::new("stash-names");
    let s = setup(&dir, false);
    let init = |prefix: &str, suffix: &str| {
        s.set_prefs(|p| {
            p.load_strings(&[
                format!("backupprefix = {prefix}"),
                format!("backupsuffix = {suffix}"),
            ])
            .unwrap()
        });
        stasher::init_backups_local(s.host()).map_err(|e| e.to_string())
    };
    assert_eq!(
        init("bak/", ""),
        Err("Either backupprefix or backupsuffix must contain '$VERSION'".into())
    );
    // In the prefix's directory, it doesn't count.
    assert_eq!(
        init("$VERSION/x", ""),
        Err("Either backupprefix or backupsuffix must contain '$VERSION'".into())
    );
    assert_eq!(
        init("$VERSION.$VERSION", ""),
        Err(
            "The tag $VERSION should only appear once in the backupprefix and backupsuffix \
             preferences."
                .into()
        )
    );
    assert_eq!(init("", ".$VERSION"), Ok(()));
}

#[test]
fn the_current_version_is_stashed_when_marked_equal() {
    if backup_dir_from_env() {
        return;
    }
    for loopback in [false, true] {
        let dir = TempDir::new("stash-current");
        let s = setup(&dir, loopback);
        s.backups(&["backupcurr = Name README", "backupcurr = BelowPath src"]);
        build_tree(&s.a());
        build_tree(&s.b());
        s.commit(&s.find());
        // Changed the same way: reconciliation marks them equal, and
        // markEqual stashes them through the stasher (the default hook).
        // (The first sync marks only the root, which isn't stashed.)
        for side in ["a", "b"] {
            write(&s.root(side), "README", "read me", T0 + 20.0);
            write(
                &s.root(side),
                "src/main.c",
                "int main(void) { return 0; }",
                T0 + 20.0,
            );
            write(&s.root(side), "src/deep/x", "x", T0 + 20.0);
            for d in ["src/deep", "src", ""] {
                set_mtime(&s.root(side).join(d), T0 + 30.0);
            }
        }
        let found = s.find();
        let (items, equal, _) = block_on(gemel_engine::recon::reconcile_all(
            &s.rpc, &s.roots, false, found,
        ))
        .unwrap();
        assert!(items.is_empty() && equal, "{items:?}");
        let backups = s.base.join("backups");
        assert_eq!(read(&backups.join("README")), "read me");
        assert_eq!(
            read(&backups.join("src/main.c")),
            "int main(void) { return 0; }"
        );
        assert_eq!(read(&backups.join("src/deep/x")), "x");
        // And it can be found again.
        let fp = s.source(1, "README").fp;
        let found =
            stasher::get_recent_version(s.host(), &s.fspath(1), &local("README"), &fp).unwrap();
        assert_eq!(
            found.map(|f| f.to_string()),
            Some(
                Fspath::canonize(Some(backups.join("README").to_str().unwrap().as_bytes()))
                    .unwrap()
                    .to_string()
            )
        );
        let other = s.source(1, "src/main.c").fp;
        assert_eq!(
            stasher::get_recent_version(s.host(), &s.fspath(1), &local("README"), &other).unwrap(),
            None
        );
    }
}

// ---- Against OCaml ----

fn ocaml(unison: &std::ffi::OsStr, s: &Setup, flags: &[String]) -> String {
    let out = std::process::Command::new(unison)
        .arg(s.a())
        .arg(s.b())
        .args([
            "-batch",
            "-ui",
            "text",
            "-log=false",
            "-contactquietly",
            "-color=false",
            "-times",
        ])
        .args(flags)
        .env("UNISON", s.base.join("u"))
        .env("HOME", &s.base)
        .env_remove("UNISONBACKUPDIR")
        .env_remove("UNISONMIRRORDIR")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    all
}

/// The backup preferences of the oracle tests, as profile lines.
fn backup_prefs(dir: &StdPath) -> Vec<Vec<String>> {
    let backups = format!("backupdir = {}", dir.join("backups").display());
    [
        vec!["backup = Name *".to_owned(), backups.clone()],
        vec![
            "backup = Name *".to_owned(),
            "backuploc = local".into(),
            "maxbackups = 3".into(),
        ],
        vec![
            "backup = Name *".to_owned(),
            "backuploc = local".into(),
            "backupprefix = old/$VERSION-".into(),
            "backupsuffix = .orig".into(),
        ],
        vec![
            "backup = Name *".to_owned(),
            backups,
            "backupprefix = ".into(),
            "backupsuffix = .$VERSION.bak".into(),
        ],
    ]
    .into()
}

fn flags(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            let (k, v) = l.split_once(" = ").unwrap();
            format!("-{k}={v}")
        })
        .collect()
}

/// OCaml Unison records the same `ignore` patterns after `initBackups`.
/// Needs OCaml Unison: set `GEMEL_REFERENCE_UNISON` to its path (the CI
/// `oracle` job does).
#[test]
fn ocaml_ignores_the_same_backups() {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        eprintln!("skipped: set GEMEL_REFERENCE_UNISON to an OCaml unison");
        return;
    };
    for loopback in [false, true] {
        let dir = TempDir::new("stash-oracle-ignore");
        for (k, lines) in backup_prefs(&dir.0).into_iter().enumerate() {
            let sub = TempDir::new(&format!("stash-oracle-ignore-{k}"));
            let s = setup(&sub, loopback);
            write(&s.a(), "f", "f", T0);
            ocaml(&unison, &s, &flags(&lines));
            let file = |i: usize| {
                let (name, _) =
                    archive::archive_name(s.host(), &s.fspath(i), ArchiveVersion::MainArch)
                        .unwrap();
                s.host().file_in_unison_dir(&name)
            };
            let data = archive::load_archive_local(s.host(), &file(1), &s.this_root(1))
                .unwrap()
                .unwrap();
            let preds = data.props.props.find(&UPDATE_PREDICATES_KEY).unwrap();
            let theirs: Vec<String> = preds
                .iter()
                .find(|(n, _)| n == b"ignore")
                .unwrap()
                .1
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            s.set_prefs(|p| {
                let osx_host = cfg!(target_os = "macos");
                case::init(p, osx_host || cfg!(windows), osx_host);
                osx::init(p, osx_host);
                p.load_strings(&lines).unwrap();
            });
            s.init_backups();
            let ours = globals::IGNORE.patterns(&s.host().prefs().borrow());
            assert_eq!(ours, theirs, "{lines:?}");
        }
    }
}

/// OCaml Unison names and rotates backups the same way, overwriting a file
/// several times. Needs OCaml Unison.
#[test]
fn ocaml_backs_up_the_same_way() {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        eprintln!("skipped: set GEMEL_REFERENCE_UNISON to an OCaml unison");
        return;
    };
    let versions = ["v1", "v2", "v3", "v4", "v5"];
    let steps = |s: &Setup, lines: &[String], gemel: bool| {
        write(&s.a(), "d/f", versions[0], T0);
        write(&s.a(), "top", versions[0], T0);
        if gemel {
            std::fs::create_dir_all(s.b().join("d")).unwrap();
        } else {
            ocaml(&unison, s, &flags(lines));
        }
        for (i, v) in versions[1..].iter().enumerate() {
            let t = T0 + 10.0 * (i + 1) as f64;
            write(&s.a(), "d/f", v, t);
            write(&s.a(), "top", v, t);
            if gemel {
                // What propagation does before replacing a file.
                for f in ["d/f", "top"] {
                    write(&s.b(), f, versions[i], T0);
                    stasher::backup(
                        s.host(),
                        &s.fspath(1),
                        &local(f),
                        Disposition::AndRemove,
                        &gemel_engine::archive::Archive::NoArchive,
                    )
                    .unwrap();
                }
            } else {
                ocaml(&unison, s, &flags(lines));
            }
        }
    };
    for (k, lines) in backup_prefs(&TempDir::new("stash-oracle").0)
        .into_iter()
        .enumerate()
    {
        let theirs_dir = TempDir::new(&format!("stash-oracle-ocaml-{k}"));
        let ours_dir = TempDir::new(&format!("stash-oracle-gemel-{k}"));
        let rebase = |lines: &[String], base: &StdPath| -> Vec<String> {
            lines
                .iter()
                .map(|l| match l.strip_prefix("backupdir = ") {
                    Some(_) => format!("backupdir = {}", base.join("backups").display()),
                    None => l.clone(),
                })
                .collect()
        };
        let theirs_lines = rebase(&lines, &theirs_dir.0);
        let theirs = setup(&theirs_dir, false);
        steps(&theirs, &theirs_lines, false);
        let ours_lines = rebase(&lines, &ours_dir.0);
        let ours = setup(&ours_dir, false);
        ours.backups(&ours_lines.iter().map(String::as_str).collect::<Vec<_>>());
        steps(&ours, &ours_lines, true);
        let backups = |s: &Setup| {
            let mut l = listing(&s.base.join("backups"));
            // Local backups: what isn't the replica's own files.
            l.extend(
                listing(&s.b())
                    .into_iter()
                    .filter(|(n, _)| n != "d/" && n != "d/f" && n != "top"),
            );
            l
        };
        assert_eq!(backups(&ours), backups(&theirs), "{lines:?}");
    }
}
