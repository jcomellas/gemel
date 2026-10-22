//! Reconciliation, sorting and overrides, through both dispatch modes;
//! with `GEMEL_REFERENCE_UNISON` set, also against OCaml Unison's listing
//! of the same changes.

mod support;

use std::path::Path as StdPath;

use gemel_base::case::{self, Mode};
use gemel_base::path::{Global, Path};
use gemel_base::util::Error;
use gemel_engine::common::{Direction, ReconItem, ReplicaContent, Replicas, Status};
use gemel_engine::recon::{self, Choice, Strength};
use gemel_engine::{archive, globals, moves, sortri};
use gemel_fs::fileinfo::Typ;
use gemel_fs::osx;

use support::oracle::{ocaml_command, via_of};
use support::sync::*;
use support::*;

type Reconciled = (Vec<ReconItem>, bool, Vec<Path<Global>>);

impl Setup {
    /// Finds the updates and reconciles them.
    fn reconcile(&self) -> Reconciled {
        self.try_reconcile().unwrap()
    }

    fn try_reconcile(&self) -> Result<Reconciled, Error> {
        settle(&[&self.a(), &self.b()]);
        let found = self.find();
        block_on(recon::reconcile_all(&self.rpc, &self.roots, false, found))
    }

    /// The listing of a reconciliation, as OCaml's text UI prints it.
    fn listing(&self) -> Vec<String> {
        let found = archive::found_archives(&self.rpc);
        self.reconcile()
            .0
            .iter()
            .map(|ri| line(ri, found))
            .collect()
    }

    /// What OCaml's startup sets for these hosts, and `prefs` (profile
    /// lines).
    fn init_prefs(&self, prefs: &[&str]) {
        self.set_prefs(|p| {
            let osx_host = cfg!(target_os = "macos");
            case::init(p, osx_host || cfg!(windows), osx_host);
            osx::init(p, osx_host);
            let lines: Vec<String> = prefs.iter().map(|l| l.to_string()).collect();
            p.load_strings(&lines).unwrap();
            // links-aux, from links.
            gemel_fs::fileinfo::init(p, cfg!(windows));
        });
        self.init_backups();
    }

    /// A root as given (for `prefer` and the like).
    fn raw(&self, side: &str) -> String {
        self.root(side).to_str().unwrap().to_owned()
    }
}

// ---- The text UI's listing (`Uicommon.reconItem2stringList`) ----

/// `replicaContent2shortString`; `found` is `Update.foundArchives`.
fn short(rc: &ReplicaContent, found: bool) -> &'static str {
    let choose = |s1, s2| if found { s1 } else { s2 };
    match (rc.typ, &rc.status) {
        (_, Status::Unchanged) => "        ",
        (Typ::Absent, Status::Deleted) => "deleted ",
        (Typ::File, Status::Created) => choose("new file", "file    "),
        (Typ::File, Status::Modified) => "changed ",
        (Typ::File, Status::PropsChanged) => "props   ",
        (Typ::File, Status::MovedOut(..) | Status::MovedIn(..)) => "renamed ",
        (Typ::Symlink, Status::Created) => choose("new link", "link    "),
        (Typ::Symlink, Status::Modified) => "chgd lnk",
        (Typ::Directory, Status::Created) => choose("new dir ", "dir     "),
        (Typ::Directory, Status::Modified) => "chgd dir",
        (Typ::Directory, Status::PropsChanged) => "props   ",
        (Typ::Directory, Status::MovedOut(..) | Status::MovedIn(..)) => "rnmd dir",
        other => panic!("impossible replica content {other:?}"),
    }
}

/// `overwriteWarning`.
fn overwrite_warning(rc: &ReplicaContent) -> String {
    match &rc.status {
        Status::MovedOut(_, _, other) if other.typ != Typ::Absent => format!(
            " <will overwrite a {} in the other replica>",
            other.typ.as_str()
        ),
        _ => String::new(),
    }
}

fn path_str(p: &Path<Global>) -> String {
    String::from_utf8_lossy(p.as_bytes()).into_owned()
}

/// `Uitext.displayri`'s line (without colours).
fn line(ri: &ReconItem, found: bool) -> String {
    let path = path_str(&ri.path1);
    let Replicas::Different(d) = &ri.replicas else {
        return format!("         error            {path}  ");
    };
    let partial = !d.errors1.is_empty() || !d.errors2.is_empty();
    let forced = d.direction != d.default_direction;
    let moved =
        |rc: &ReplicaContent| matches!(rc.status, Status::MovedOut(..) | Status::MovedIn(..));
    let onemoved = if moved(&d.rc1) && moved(&d.rc2) {
        String::new()
    } else {
        match (&d.rc1.status, &d.rc2.status) {
            (Status::MovedOut(n, _, _), _) => {
                format!(
                    " (--> new name: {}){}",
                    path_str(n),
                    overwrite_warning(&d.rc1)
                )
            }
            (_, Status::MovedOut(n, _, _)) => {
                format!(
                    " (--> new name: {}){}",
                    path_str(n),
                    overwrite_warning(&d.rc2)
                )
            }
            (Status::MovedIn(n, _, _), _) | (_, Status::MovedIn(n, _, _)) => {
                format!(" (<-- previous name: {})", path_str(n))
            }
            _ => String::new(),
        }
    };
    let action = match (&d.direction, partial, forced) {
        (Direction::Conflict(_), _, false) => "<-?->",
        (Direction::Conflict(_), _, true) => "<=?=>",
        (Direction::Replica1ToReplica2, false, false) => "---->",
        (Direction::Replica1ToReplica2, false, true) => "====>",
        (Direction::Replica1ToReplica2, true, false) => "--?->",
        (Direction::Replica1ToReplica2, true, true) => "==?=>",
        (Direction::Replica2ToReplica1, false, false) => "<----",
        (Direction::Replica2ToReplica1, false, true) => "<====",
        (Direction::Replica2ToReplica1, true, false) => "<-?--",
        (Direction::Replica2ToReplica1, true, true) => "<=?==",
        (Direction::Merge, _, false) => "<-M->",
        (Direction::Merge, _, true) => "<=M=>",
    };
    format!(
        "{} {action} {}   {path}{onemoved}  ",
        short(&d.rc1, found),
        short(&d.rc2, found)
    )
}

/// The conflicts and problems, and why, as OCaml's summary lists them
/// (`skipped: path (reason)`).
fn skipped(items: &[ReconItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|ri| match &ri.replicas {
            Replicas::Different(d) => match &d.direction {
                Direction::Conflict(r) => Some(format!("skipped: {} ({r})", path_str(&ri.path1))),
                _ => None,
            },
            Replicas::Problem(r) => Some(format!("skipped: {} ({r})", path_str(&ri.path1))),
        })
        .collect()
}

/// `Util.bytes2string`.
fn bytes2string(v: i64) -> String {
    let (kib, mib, gib) = (1024., 1024. * 1024., 1024. * 1024. * 1024.);
    let f = v as f64;
    if v > 1_048_051_711 {
        format!("{:.2} GiB", f / gib)
    } else if v > 104_805_171 {
        format!("{:.0} MiB", f / mib)
    } else if v > 1_023_487 {
        format!("{:.1} MiB", f / mib)
    } else if v > 102_348 {
        format!("{:.0} KiB", f / kib)
    } else if v > 999 {
        format!("{:.1} KiB", f / kib)
    } else {
        format!("{v} B")
    }
}

/// The text UI's summary before propagating: how many items will be
/// synced and skipped (problems and conflicts), and the bytes each way.
/// The summary of `items`, between the roots named `a` and `b` (or
/// `local` and the server's name, with a remote root, as OCaml names them).
fn summary(items: &[ReconItem], via: Via) -> Vec<String> {
    let (a, b) = if via == Via::Remote {
        ("local", SERVER)
    } else {
        ("a", "b")
    };
    let (mut todo, mut skip, mut to1, mut to2) = (0, 0, 0, 0);
    for ri in items {
        if gemel_engine::common::problematic(ri) {
            skip += 1;
            continue;
        }
        todo += 1;
        match direction(ri) {
            Direction::Replica1ToReplica2 => to2 += gemel_engine::common::ri_length(ri),
            Direction::Replica2ToReplica1 => to1 += gemel_engine::common::ri_length(ri),
            Direction::Conflict(_) | Direction::Merge => {}
        }
    }
    vec![
        format!("{todo} items will be synced, {skip} skipped"),
        format!("{} to be synced from {a} to {b}", bytes2string(to2)),
        format!("{} to be synced from {b} to {a}", bytes2string(to1)),
    ]
}

fn direction(ri: &ReconItem) -> &Direction {
    match &ri.replicas {
        Replicas::Different(d) => &d.direction,
        Replicas::Problem(p) => panic!("problem: {p}"),
    }
}

fn paths(items: &[ReconItem]) -> Vec<String> {
    items.iter().map(|ri| path_str(&ri.path1)).collect()
}

fn conflict(reason: &str) -> Direction {
    Direction::Conflict(reason.into())
}

// ---- Scenarios ----

/// A replica in sync on both sides: the trees given, then reconciled,
/// which marks them equal and commits.
/// Then a new client, as OCaml runs a new process each time (which
/// matters for `foundArchives`), with `prefs`.
fn synced(dir: &TempDir, via: Via, prefs: &[&str], build: fn(&StdPath)) -> Setup {
    let s = setup(dir, via);
    s.init_prefs(&[]);
    build(&s.a());
    build(&s.b());
    let (items, equal, dangerous) = s.reconcile();
    assert!(items.is_empty(), "{items:?}");
    assert!(equal && dangerous.is_empty());
    drop(s);
    let s = setup(dir, via);
    s.init_prefs(prefs);
    s
}

/// The tree of OCaml's quick check: files, and a directory.
fn small(root: &StdPath) {
    write(root, "f", "x", T0);
    write(root, "g", "y", T0);
    write(root, "d/h", "z", T0);
    set_mtime(&root.join("d"), T0 + 10.0);
}

/// Changes on both sides: `f` changed on both (a conflict), `g` deleted
/// on b, `n` new on b, `d` moved to `e` on a.
fn small_changes(a: &StdPath, b: &StdPath) {
    write(a, "f", "1", T0 + 20.0);
    write(b, "f", "2", T0 + 30.0);
    std::fs::remove_file(b.join("g")).unwrap();
    write(b, "n", "new", T0 + 40.0);
    std::fs::rename(a.join("d"), a.join("e")).unwrap();
}

#[test]
fn identical_replicas_are_marked_equal_and_committed() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-equal");
        let s = synced(&dir, via, &[], build_tree);
        // Committed: nothing changed since.
        assert_eq!(s.reconcile(), (Vec::new(), false, Vec::new()));
        // Changed the same way on both sides: equal again.
        for side in ["a", "b"] {
            write(&s.root(side), "README", "same", T0 + 50.0);
        }
        assert_eq!(s.reconcile(), (Vec::new(), true, Vec::new()));
        assert_eq!(s.reconcile(), (Vec::new(), false, Vec::new()));
    }
}

#[test]
fn proposals_are_sorted_then_overridden() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-small");
        let s = synced(&dir, via, &[], small);
        small_changes(&s.a(), &s.b());
        assert_eq!(
            s.listing(),
            [
                "changed  <-?-> changed    f  ",
                "deleted  ---->            d  ",
                "new dir  ---->            e  ",
                "         <---- deleted    g  ",
                "         <---- new file   n  ",
            ]
        );
        // OCaml's quick check: d and e become conflicts after sorting.
        let b = s.raw("b");
        s.set_prefs(|p| {
            for pref in [&recon::NO_UPDATE, &recon::NO_CREATION, &recon::NO_DELETION] {
                p.set(pref, vec![b.clone()]);
            }
        });
        let (items, _, _) = s.reconcile();
        assert_eq!(paths(&items), ["f", "d", "e", "g", "n"]);
        assert_eq!(
            skipped(&items),
            [
                "skipped: f (contents changed on both sides)",
                "skipped: d (would update a file with noupdate or noupdatepartial set)",
                "skipped: e (would create a file with nocreation or nocreationpartial set)",
            ]
        );
        assert_eq!(
            items.iter().map(|ri| line(ri, true)).collect::<Vec<_>>()[1],
            "deleted  <=?=>            d  "
        );
    }
}

#[test]
fn prefer_and_force_pick_directions() {
    let dir = TempDir::new("recon-prefer");
    let s = synced(&dir, Via::Direct, &[], small);
    small_changes(&s.a(), &s.b());
    let directions = |prefs: &[&str]| {
        s.set_prefs(|p| {
            for pref in [&recon::PREFER_ROOT, &recon::FORCE_ROOT] {
                p.set(pref, String::new());
            }
            for pred in [&recon::PREFER_ROOT_PARTIAL, &recon::FORCE_ROOT_PARTIAL] {
                pred.intern(p, Vec::new());
            }
            let lines: Vec<String> = prefs.iter().map(|l| l.to_string()).collect();
            p.load_strings(&lines).unwrap();
        });
        let (items, _, _) = s.reconcile();
        items
            .iter()
            .map(|ri| (path_str(&ri.path1), direction(ri).clone()))
            .collect::<Vec<_>>()
    };
    use Direction::{Replica1ToReplica2 as R12, Replica2ToReplica1 as R21};
    let all = |dirs: [Direction; 5]| {
        ["f", "d", "e", "g", "n"]
            .into_iter()
            .map(String::from)
            .zip(dirs)
            .collect::<Vec<_>>()
    };
    let c = conflict("contents changed on both sides");
    assert_eq!(directions(&[]), all([c.clone(), R12, R12, R21, R21]));
    // prefer only settles conflicts, by root, by suffix, or by time.
    let prefer_b = format!("prefer = {}", s.raw("b"));
    assert_eq!(directions(&[&prefer_b]), all([R21, R12, R12, R21, R21]));
    assert_eq!(directions(&["prefer = a"]), all([R12, R12, R12, R21, R21]));
    assert_eq!(
        directions(&["prefer = newer"]),
        all([R21, R12, R12, R21, R21])
    );
    assert_eq!(
        directions(&["prefer = older"]),
        all([R12, R12, R12, R21, R21])
    );
    // force settles everything.
    assert_eq!(directions(&["force = b"]), all([R21, R21, R21, R21, R21]));
    // Equal times leave items alone; deletions only move in conflicts.
    assert_eq!(
        directions(&["force = newer"]),
        all([R21, R12, R12, R21, R21])
    );
    // The partial forms override the others for their paths.
    assert_eq!(
        directions(&["force = b", "forcepartial = Name {f,n} -> a"]),
        all([R12, R21, R21, R21, R12])
    );
    assert_eq!(
        directions(&["preferpartial = Path f -> a"]),
        all([R12, R12, R12, R21, R21])
    );
}

#[test]
fn roots_are_named_by_prefix_or_suffix() {
    let dir = TempDir::new("recon-roots");
    let s = setup(&dir, Via::Direct);
    let prefs = s.rpc.client_host().prefs().borrow();
    let dir = |r: &str| recon::root_to_direction(&prefs, r);
    assert_eq!(dir("").unwrap(), None);
    assert_eq!(dir("older").unwrap(), Some(Choice::Older));
    assert_eq!(dir("newer").unwrap(), Some(Choice::Newer));
    assert_eq!(dir(&s.raw("a")).unwrap(), Some(Choice::Replica1ToReplica2));
    assert_eq!(dir("b").unwrap(), Some(Choice::Replica2ToReplica1));
    let err = dir("/").unwrap_err().to_string();
    assert_eq!(
        err,
        format!(
            "/\nis not uniquely identifying one of the current roots:\n  {}\n  {}",
            s.raw("a"),
            s.raw("b")
        )
    );
}

#[test]
fn preferred_roots_are_checked_up_front() {
    let dir = TempDir::new("recon-check");
    let s = setup(&dir, Via::Direct);
    let check = |lines: &[&str]| {
        let mut prefs = new_prefs();
        globals::set_raw_roots(&mut prefs, &[s.raw("a"), s.raw("b")]);
        prefs.set(&gemel_fs::props::TIMES, true);
        let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        prefs.load_strings(&lines).unwrap();
        recon::check_that_preferred_root_is_valid(&prefs).map_err(|e| e.to_string())
    };
    assert_eq!(check(&["prefer = b", "nodeletion = a"]), Ok(()));
    assert_eq!(check(&["force = newer", "prefer = older"]), Ok(()));
    assert_eq!(
        check(&["times = false", "prefer = older"]),
        Err("The 'prefer=older' preference can only be used with 'times=true'".into())
    );
    // Not checked for newer, as in OCaml.
    assert_eq!(check(&["times = false", "prefer = newer"]), Ok(()));
    assert_eq!(
        check(&["nocreation = newer"]),
        Err(
            "Argument to preference 'nocreation': newer must not be keyword 'older' or 'newer'."
                .into()
        )
    );
    assert_eq!(
        check(&["noupdatepartial = Path x -> /"]),
        Err(format!(
            "Argument to preference 'noupdatepartial': /\nis not uniquely identifying one of \
             the current roots:\n  {}\n  {}",
            s.raw("a"),
            s.raw("b")
        ))
    );
}

#[test]
fn set_direction_follows_the_strength() {
    let dir = TempDir::new("recon-set");
    let s = synced(&dir, Via::Direct, &[], small);
    small_changes(&s.a(), &s.b());
    let (mut items, _, _) = s.reconcile();
    let prefs = s.rpc.client_host().prefs();
    // A proposal only changes when forced.
    let g = &mut items[3];
    let set = |ri: &mut ReconItem, c, strength| {
        recon::set_direction(&prefs.borrow(), ri, c, strength).unwrap();
    };
    set(g, Choice::Replica1ToReplica2, Strength::Prefer);
    assert_eq!(*direction(g), Direction::Replica2ToReplica1);
    set(g, Choice::Replica1ToReplica2, Strength::Force);
    assert_eq!(*direction(g), Direction::Replica1ToReplica2);
    recon::revert_to_default_direction(g);
    assert_eq!(*direction(g), Direction::Replica2ToReplica1);
    // Merging needs a path to merge.
    let f = &mut items[0];
    set(f, Choice::Merge, Strength::Prefer);
    assert!(direction(f).is_conflict());
    globals::MERGE.intern(&mut prefs.borrow_mut(), vec!["Name f -> cat".into()]);
    set(f, Choice::Merge, Strength::Prefer);
    assert_eq!(*direction(f), Direction::Merge);
}

#[test]
fn merge_atomic_and_props_conflicts() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-kinds");
        let s = synced(
            &dir,
            via,
            &["merge = Name m -> cat", "atomic = Name at"],
            |r| {
                for f in ["m", "p", "at/x", "at/y", "same"] {
                    write(r, f, f, T0);
                }
                set_mtime(&r.join("at"), T0);
            },
        );
        // Times a second apart count as the same (for FAT).
        write(&s.a(), "m", "1", T0 + 5.0);
        write(&s.b(), "m", "2", T0 + 5.0);
        write(&s.a(), "at/x", "1", T0 + 5.0);
        write(&s.b(), "at/y", "2", T0 + 5.0);
        for (side, t) in [("a", T0 + 5.0), ("b", T0 + 10.0)] {
            set_mtime(&s.root(side).join("p"), t);
            write(&s.root(side), "same", "both", t);
        }
        let (items, _, _) = s.reconcile();
        assert_eq!(
            items
                .iter()
                .map(|ri| (path_str(&ri.path1), direction(ri).clone()))
                .collect::<Vec<_>>(),
            [
                ("at".into(), conflict("atomic directory")),
                ("p".into(), conflict("properties changed on both sides")),
                ("same".into(), conflict("properties changed on both sides")),
                ("m".into(), Direction::Merge),
            ]
        );
        assert_eq!(
            items.iter().map(|ri| line(ri, true)).collect::<Vec<_>>(),
            [
                "chgd dir <-?-> chgd dir   at  ",
                "props    <-?-> props      p  ",
                "props    <-?-> props      same  ",
                "changed  <-M-> changed    m  ",
            ]
        );
    }
}

#[test]
fn no_prefs_and_size_threshold_make_conflicts() {
    let dir = TempDir::new("recon-no");
    let s = synced(&dir, Via::Direct, &[], |r| {
        write(r, "keep/x", "x", T0);
        write(r, "big", "small", T0);
    });
    write(&s.a(), "big", &"x".repeat(3000), T0 + 1.0);
    std::fs::remove_file(s.a().join("keep/x")).unwrap();
    write(&s.a(), "keep/y", "y", T0 + 1.0);
    let reasons = |prefs: &[&str]| {
        s.set_prefs(|p| {
            for pref in [&recon::NO_DELETION, &recon::NO_CREATION, &recon::NO_UPDATE] {
                p.set(pref, Vec::new());
            }
            for pred in [
                &recon::NO_DELETION_PARTIAL,
                &recon::NO_CREATION_PARTIAL,
                &recon::NO_UPDATE_PARTIAL,
            ] {
                pred.intern(p, Vec::new());
            }
            p.set(&recon::MAX_SIZE_THRESHOLD, -1);
            let lines: Vec<String> = prefs.iter().map(|l| l.to_string()).collect();
            p.load_strings(&lines).unwrap();
        });
        let (items, _, _) = s.reconcile();
        skipped(&items)
    };
    assert!(reasons(&[]).is_empty());
    assert_eq!(
        reasons(&["maxsizethreshold = 2"]),
        ["skipped: big (would transfer a file of size greater than maxsizethreshold)"]
    );
    assert!(reasons(&["maxsizethreshold = 4"]).is_empty());
    assert_eq!(
        reasons(&["nodeletionpartial = BelowPath keep -> b"]),
        ["skipped: keep/x (would delete a file with nodeletion or nodeletionpartial set)"]
    );
    assert_eq!(
        reasons(&["nocreation = a", "nocreationpartial = Name y -> b"]),
        ["skipped: keep/y (would create a file with nocreation or nocreationpartial set)"]
    );
    assert_eq!(
        reasons(&["noupdate = b"]),
        [
            "skipped: big (would update a file with noupdate or noupdatepartial set)",
            "skipped: keep/x (would update a file with noupdate or noupdatepartial set)",
        ]
    );
}

#[test]
fn sorting_follows_the_preferences() {
    let dir = TempDir::new("recon-sort");
    let s = synced(&dir, Via::Direct, &[], |r| write(r, "z", "z", T0));
    for (f, content) in [("a/b", "12345"), ("a-b", "1"), ("B", "123"), ("z", "zz")] {
        write(&s.a(), f, content, T0 + 1.0);
    }
    write(&s.b(), "z", "zzz", T0 + 1.0);
    let order = || {
        let (items, _, _) = s.reconcile();
        paths(&items)
    };
    // Conflicts first, then byte order of the path.
    assert_eq!(order(), ["z", "B", "a", "a-b"]);
    s.set_prefs(|p| {
        sortri::SORT_FIRST.intern(p, vec!["Name a-b".into()]);
        sortri::SORT_LAST.intern(p, vec!["Name B".into()]);
    });
    assert_eq!(order(), ["z", "a-b", "a", "B"]);
    sortri::sort_by_size(s.rpc.client_host());
    assert_eq!(order(), ["z", "a-b", "B", "a"]);
    sortri::sort_by_name(s.rpc.client_host());
    assert_eq!(order(), ["z", "B", "a", "a-b"]);
    sortri::restore_default_settings(s.rpc.client_host());
    assert_eq!(order(), ["z", "a-b", "a", "B"]);
    // New first: only a-b, B and a are new, and z is a conflict.
    sortri::sort_new_first(s.rpc.client_host());
    assert_eq!(order(), ["z", "a-b", "a", "B"]);
}

#[test]
fn dangerous_paths_are_those_emptied_on_one_side() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-dangerous");
        let s = synced(&dir, via, &[], |r| {
            write(r, "d/x", "x", T0);
            write(r, "e/y", "y", T0);
            write(r, "f/z", "z", T0);
        });
        s.set_prefs(|p| p.set(&globals::PATHS, vec![global("d"), global("e"), global("f")]));
        std::fs::remove_file(s.a().join("d/x")).unwrap();
        std::fs::remove_dir_all(s.b().join("e")).unwrap();
        std::fs::remove_file(s.a().join("f/z")).unwrap();
        std::fs::remove_file(s.b().join("f/z")).unwrap();
        let found = s.find();
        let scanned: Vec<_> = found
            .iter()
            .map(|(a, _)| a.0.clone().make_global())
            .collect();
        let (_, _, dangerous) =
            block_on(recon::reconcile_all(&s.rpc, &s.roots, false, found)).unwrap();
        // Last first; f was emptied on both sides.
        let mut expected: Vec<_> = scanned.into_iter().filter(|p| *p != global("f")).collect();
        expected.reverse();
        assert_eq!(dangerous, expected);
        assert_eq!(dangerous.len(), 2);
    }
}

#[test]
fn equal_paths_are_marked_in_batches() {
    let dir = TempDir::new("recon-batches");
    let s = synced(&dir, Via::Direct, &[], |_| {});
    // 4999 files and their directory: 5000 equal paths, all marked by the
    // batch, which leaves the last one empty (so false, as in OCaml).
    for side in ["a", "b"] {
        for i in 0..4999 {
            write(&s.root(side), &format!("many/{i:04}"), "same", T0);
        }
    }
    assert_eq!(s.reconcile(), (Vec::new(), false, Vec::new()));
    assert_eq!(s.reconcile(), (Vec::new(), false, Vec::new()));
    // One more makes a second batch.
    for side in ["a", "b"] {
        write(&s.root(side), "many/x", "same", T0);
        write(&s.root(side), "y", "same", T0);
    }
    assert_eq!(s.reconcile(), (Vec::new(), true, Vec::new()));
    assert_eq!(s.reconcile(), (Vec::new(), false, Vec::new()));
}

#[test]
fn moves_are_detected_when_asked() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-moves");
        let s = synced(&dir, via, &[], |r| {
            write(r, "old", "contents", T0);
            write(r, "dir/x", "x", T0);
            write(r, "dir/y", "y", T0);
        });
        std::fs::rename(s.a().join("old"), s.a().join("new")).unwrap();
        std::fs::rename(s.b().join("dir"), s.b().join("moved")).unwrap();
        assert_eq!(
            s.listing(),
            [
                "         <---- deleted    dir  ",
                "         <---- new dir    moved  ",
                "new file ---->            new  ",
                "deleted  ---->            old  ",
            ]
        );
        s.set_prefs(|p| p.set(&moves::DETECT_MOVES, true));
        assert_eq!(
            s.listing(),
            [
                "         <---- rnmd dir   dir (--> new name: moved)  ",
                "renamed  ---->            old (--> new name: new)  ",
            ]
        );
        let (items, _, _) = s.reconcile();
        let Replicas::Different(d) = &items[1].replicas else {
            panic!()
        };
        let Status::MovedOut(to, here, there) = &d.rc1.status else {
            panic!("{d:?}")
        };
        assert_eq!(*to, global("new"));
        assert_eq!(
            (here.status.clone(), there.typ),
            (Status::Created, Typ::Absent)
        );
        assert_eq!(
            gemel_engine::moves::original_rc(&d.rc1).status,
            Status::Deleted
        );
    }
}

#[test]
fn moves_need_the_feature() {
    let mut f = new_features();
    let mut prefs = new_prefs();
    let all = f.all();
    f.set_enabled(&all);
    prefs.set(&moves::DETECT_MOVES, true);
    assert!(f.validate(&all, &prefs).is_ok());
    let without: Vec<String> = all
        .iter()
        .filter(|n| *n != moves::FEATURE_MOVES)
        .cloned()
        .collect();
    let err = f.validate(&without, &prefs).unwrap_err().to_string();
    assert!(err.contains("the server does not support this"), "{err}");
    // The preference is sent only with the feature.
    let sent = |f: &gemel_base::features::Features| {
        prefs
            .dump(1, f)
            .unwrap()
            .iter()
            .any(|(n, _, _)| n == "moves-experimental")
    };
    assert!(sent(&f));
    f.set_enabled(&without);
    assert!(!sent(&f));
}

#[cfg(unix)]
#[test]
fn links_are_problems_when_not_synchronised() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-links");
        let s = synced(&dir, via, &["links = false"], |r| write(r, "f", "f", T0));
        std::os::unix::fs::symlink("f", s.a().join("l")).unwrap();
        std::os::unix::fs::symlink("f", s.a().join("both")).unwrap();
        write(&s.b(), "both", "file", T0);
        let (items, _, _) = s.reconcile();
        assert_eq!(paths(&items), ["both"]);
        assert_eq!(
            items[0].replicas,
            Replicas::Problem(
                "Syncing symbolic links is disabled, but this path represents a symbolic link \
                 in one of the replicas and a non-link in the other replica."
                    .into()
            )
        );
    }
}

#[test]
fn case_insensitive_spellings_pair_up() {
    for via in Via::ALL {
        let dir = TempDir::new("recon-case");
        let s = setup_in(&dir, via, Mode::Insensitive);
        write(&s.a(), "Dir/File", "same", T0);
        write(&s.b(), "dir/file", "same", T0);
        set_mtime(&s.a().join("Dir"), T0);
        set_mtime(&s.b().join("dir"), T0);
        let (items, equal, _) = s.reconcile();
        assert!(items.is_empty() && equal, "{items:?}");
        // Changed on one side: both paths are that side's spelling, as in
        // OCaml.
        write(&s.a(), "Dir/File", "a", T0 + 1.0);
        let (items, _, _) = s.reconcile();
        assert_eq!(paths(&items), ["Dir/File"]);
        assert_eq!(path_str(&items[0].path2), "Dir/File");
        // On both: each side's.
        write(&s.b(), "dir/file", "b", T0 + 2.0);
        let (items, _, _) = s.reconcile();
        assert_eq!(paths(&items), ["Dir/File"]);
        assert_eq!(path_str(&items[0].path2), "dir/file");
        assert!(direction(&items[0]).is_conflict());
    }
}

// ---- Against OCaml ----

/// A scenario for the oracle: preferences, the tree both sides start
/// with, and the changes.
struct Scenario {
    name: &'static str,
    prefs: &'static [&'static str],
    build: fn(&StdPath),
    change: fn(&StdPath, &StdPath),
}

fn scenarios() -> Vec<Scenario> {
    fn tree(r: &StdPath) {
        small(r);
        write(r, "m", "m", T0);
        write(r, "p", "p", T0);
        write(r, "big", "small", T0);
        write(r, "keep/x", "x", T0);
        write(r, "at/x", "x", T0);
        write(r, "at/y", "y", T0);
        write(r, "moved/one", "one", T0);
        write(r, "moved/two", "two", T0);
        write(r, "renamed", "renamed contents", T0);
        for d in ["at", "keep", "moved", "d"] {
            set_mtime(&r.join(d), T0 + 10.0);
        }
    }
    fn changes(a: &StdPath, b: &StdPath) {
        small_changes(a, b);
        write(a, "m", "1", T0 + 5.0);
        write(b, "m", "2", T0 + 10.0);
        set_mtime(&a.join("p"), T0 + 15.0);
        set_mtime(&b.join("p"), T0 + 20.0);
        write(a, "big", &"x".repeat(3000), T0 + 5.0);
        std::fs::remove_file(a.join("keep/x")).unwrap();
        write(a, "keep/y", "y", T0 + 5.0);
        write(a, "at/x", "1", T0 + 5.0);
        write(b, "at/y", "2", T0 + 5.0);
        std::fs::rename(b.join("moved"), b.join("elsewhere")).unwrap();
        std::fs::rename(a.join("renamed"), a.join("keep/renamed")).unwrap();
        write(a, "a-b", "dash", T0 + 5.0);
        write(a, "a/b", "slash", T0 + 5.0);
        write(b, "B", "upper", T0 + 6.0);
        std::fs::create_dir(a.join("dirfile")).unwrap();
        write(b, "dirfile", "file", T0 + 7.0);
        write(a, "same", "same", T0 + 8.0);
        write(b, "same", "same", T0 + 9.0);
    }
    let s = |name, prefs| Scenario {
        name,
        prefs,
        build: tree,
        change: changes,
    };
    vec![
        s("plain", &[]),
        s(
            "merge-atomic",
            &["merge = Name m -> true", "atomic = Name at"],
        ),
        s("prefer-b", &["prefer = b"]),
        s("prefer-newer", &["prefer = newer"]),
        s("prefer-older", &["prefer = older"]),
        s("force-a", &["force = a"]),
        s("force-newer", &["force = newer"]),
        s("force-older", &["force = older"]),
        s(
            "partial",
            &[
                "force = b",
                "forcepartial = Name {f,n} -> a",
                "preferpartial = Path m -> older",
            ],
        ),
        s(
            "no-b",
            &["noupdate = b", "nocreation = b", "nodeletion = b"],
        ),
        s(
            "no-a",
            &["noupdate = a", "nocreation = a", "nodeletion = a"],
        ),
        s(
            "no-partial",
            &[
                "nodeletionpartial = BelowPath keep -> b",
                "nocreationpartial = Name y -> b",
                "noupdatepartial = Name n -> a",
            ],
        ),
        s("size", &["maxsizethreshold = 2", "force = b"]),
        s(
            "sorting",
            &[
                "sortfirst = Name n",
                "sortlast = Name f",
                "sortbysize = true",
            ],
        ),
        s("new-first", &["sortnewfirst = true"]),
        s("moves", &["moves-experimental = true"]),
        s("moves-forced", &["moves-experimental = true", "force = b"]),
        // A link where the other side has a file is a problem.
        #[cfg(unix)]
        Scenario {
            name: "links",
            prefs: &["links = false"],
            build: tree,
            change: |a, b| {
                changes(a, b);
                std::os::unix::fs::symlink("f", a.join("link")).unwrap();
                std::os::unix::fs::symlink("f", a.join("both")).unwrap();
                write(b, "both", "file", T0 + 5.0);
            },
        },
    ]
}

/// The run's item lines, the summary before propagating, and the skipped
/// items, from OCaml's batch output.
fn ocaml_listing(out: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let arrows = [
        "<-?->", "<=?=>", "---->", "====>", "--?->", "==?=>", "<----", "<====", "<-?--", "<=?==",
        "<-M->", "<=M=>", "error",
    ];
    // Progress messages end with carriage returns.
    let lines: Vec<&str> = out
        .lines()
        .map(|l| l.rsplit('\r').next().unwrap_or(l))
        .collect();
    let items = lines
        .iter()
        .filter(|l| l.len() > 24 && l.is_char_boundary(9) && arrows.contains(&&l[9..14]))
        .map(|l| l.to_string())
        .collect();
    let skipped = lines
        .iter()
        .filter_map(|l| l.strip_prefix("  skipped: "))
        .map(|l| format!("skipped: {l}"))
        .collect();
    let summary = lines
        .iter()
        .position(|l| l.contains(" items will be synced, "))
        .map(|i| lines[i..i + 3].iter().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    (items, summary, skipped)
}

/// OCaml Unison proposes what gemel does, for each scenario. Needs OCaml
/// Unison: set `GEMEL_REFERENCE_UNISON` to its path (the CI `oracle` job
/// does).
#[test]
fn ocaml_proposes_the_same() {
    let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") else {
        eprintln!("skipped: set GEMEL_REFERENCE_UNISON to an OCaml unison");
        return;
    };
    for via in Via::ALL {
        for sc in scenarios() {
            let dir = TempDir::new("recon-oracle");
            let s = synced(&dir, via, sc.prefs, sc.build);
            (sc.change)(&s.a(), &s.b());
            // gemel first: it commits only paths updated the same way, which
            // neither lists.
            let (items, _, _) = s.reconcile();
            let found = archive::found_archives(&s.rpc);
            let ours: Vec<String> = items.iter().map(|ri| line(ri, found)).collect();
            let out = ocaml_command(&unison, &s.base, via_of(&s))
                .args([
                    "-batch",
                    "-ui",
                    "text",
                    "-log=false",
                    "-contactquietly",
                    "-color=false",
                    "-times",
                ])
                .args(sc.prefs.iter().map(|l| {
                    let (k, v) = l.split_once(" = ").unwrap();
                    format!("-{k}={v}")
                }))
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let all = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            for bad in ["Warning", "mismatch", "No archive files", "Fatal"] {
                assert!(!all.contains(bad), "{}: {all}", sc.name);
            }
            let (theirs, their_summary, their_skipped) = ocaml_listing(&all);
            assert!(theirs.len() > 10, "{}: {all}", sc.name);
            assert_eq!(summary(&items, via), their_summary, "{}: {all}", sc.name);
            if sc.name == "links" {
                assert!(theirs.iter().any(|l| l.contains(" error ")), "{all}");
            }
            assert_eq!(ours, theirs, "{} ({via})", sc.name);
            assert_eq!(skipped(&items), their_skipped, "{}: {all}", sc.name);
        }
    }
}
