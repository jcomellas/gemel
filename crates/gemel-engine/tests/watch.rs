//! Update detection with a file watcher, directly and through the
//! loopback: gemel's helper, or OCaml's when `GEMEL_REFERENCE_FSMONITOR`
//! names it (Linux).
//!
//! The helper is `target/<profile>/gemel-fsmonitor`, which `cargo test
//! --workspace` builds; `cargo test -p gemel-engine` alone doesn't, so the
//! tests say so if it is missing.

mod support;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gemel_base::path::Path;
use gemel_engine::common::{PrevState, UpdateContent, UpdateItem};
use gemel_engine::fswatch;
use gemel_engine::update::{self, PathUpdates, Subpaths};
use support::sync::*;
use support::*;

fn helper() -> PathBuf {
    if let Some(p) = std::env::var_os("GEMEL_REFERENCE_FSMONITOR") {
        return p.into();
    }
    // This test runs from target/<profile>/deps.
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let p = dir.join(format!("gemel-fsmonitor{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.exists(),
        "{} is missing: build it with `cargo build -p gemel-fsmonitor`",
        p.display()
    );
    p
}

/// Two replicas with the same tree, synchronised, and `watch` set.
fn synced(dir: &TempDir, via: Via) -> Setup {
    fswatch::set_program(Some(helper()));
    let s = setup(dir, via);
    build_tree(&s.a());
    build_tree(&s.b());
    let found = s.find();
    s.commit(&found);
    s.set_prefs(|p| p.set(&fswatch::USE_WATCHER, true));
    s
}

fn find_watching(s: &Setup, subpaths: Subpaths) -> Vec<PathUpdates> {
    block_on(update::find_updates(&s.rpc, &s.roots, true, subpaths)).unwrap()
}

fn reported() -> Subpaths {
    Some((Vec::new(), Vec::new()))
}

fn unsynchronized(p: &str) -> Subpaths {
    Some((vec![global(p)], Vec::new()))
}

/// `waitForChanges` on each root, until each has changed.
fn wait(s: &Setup) {
    block_on(
        s.roots
            .map(|r| s.rpc.call_root(&fswatch::WAIT_FOR_CHANGES, r, ())),
    )
    .unwrap();
    settled();
}

/// The roots share one watcher, and any line from it ends every wait, as
/// OCaml's `has_changes` does: a wait can end with the first replica's
/// report, before the other's changes are old enough to be reported. The
/// tests make their changes together, so once the first is reported, the
/// rest are after the watcher's delay.
fn settled() {
    std::thread::sleep(Duration::from_millis(700));
}

/// `waitForChanges` on root `i` alone.
fn wait_on(s: &Setup, i: usize) {
    let root = &s.roots.list()[i];
    block_on(s.rpc.call_root(&fswatch::WAIT_FOR_CHANGES, root, ())).unwrap();
    settled();
}

/// The names with updates below the root, on each side.
fn changed(found: &[PathUpdates]) -> (Vec<String>, Vec<String>) {
    let [(a, b)] = found else { panic!("one path") };
    let side = |f: &(Path<_>, UpdateItem, _)| match &f.1 {
        UpdateItem::NoUpdates => Vec::new(),
        ui => names(ui),
    };
    (side(a), side(b))
}

fn none() -> (Vec<String>, Vec<String>) {
    (Vec::new(), Vec::new())
}

fn v(l: &[&str]) -> Vec<String> {
    l.iter().map(|x| x.to_string()).collect()
}

#[test]
fn only_what_the_watcher_reports_is_scanned() {
    for via in Via::ALL {
        let dir = TempDir::new("watch-only");
        let s = synced(&dir, via);
        // The first scan with a watcher scans everything, and starts it.
        assert_eq!(changed(&find_watching(&s, None)), none());
        write(&s.a(), "new", "n", T0 + 50.0);
        let start = Instant::now();
        let found = find_watching(&s, reported());
        // The watcher reports a change once it is half a second old.
        if start.elapsed() < Duration::from_millis(400) {
            assert_eq!(changed(&found), none(), "{via}");
        }
        // The paths left unsynchronised are scanned whatever the watcher
        // says, and a scan without them scans everything.
        assert_eq!(
            changed(&find_watching(&s, unsynchronized("new"))),
            (v(&["new"]), vec![])
        );
        assert_eq!(changed(&s.find()), (v(&["new"]), vec![]));
        // Without a watcher just now, everything is scanned again, which
        // starts it again.
        assert_eq!(
            changed(&find_watching(&s, reported())),
            (v(&["new"]), vec![])
        );
        write(&s.b(), "src/other", "o", T0 + 51.0);
        wait_on(&s, 1);
        let found = find_watching(&s, reported());
        let (a, b) = changed(&found);
        assert_eq!(b, v(&["src"]), "{via}");
        // The watcher has nothing more to say about a/new.
        assert!(a.is_empty() || a == v(&["new"]), "{a:?}");
    }
}

#[test]
fn the_watcher_finds_what_a_full_scan_finds() {
    for via in Via::ALL {
        let dir = TempDir::new("watch-same");
        let s = synced(&dir, via);
        assert_eq!(changed(&find_watching(&s, None)), none());
        // Changes of each kind, on both sides.
        write(&s.a(), "README", "changed", T0 + 60.0);
        write(&s.a(), "fresh/one/two", "2", T0 + 61.0);
        // Not the only file of a directory: see below.
        std::fs::remove_file(s.a().join("src/main.c")).unwrap();
        std::fs::rename(s.b().join("src/main.c"), s.b().join("main.c")).unwrap();
        std::fs::remove_dir(s.b().join("empty")).unwrap();
        write(&s.b(), "src/deep/y", "y", T0 + 62.0);
        wait(&s);
        let watched = find_watching(&s, reported());
        let full = s.find();
        assert_eq!(shapes(&watched), shapes(&full), "{via}");
        assert_eq!(
            changed(&full),
            (
                v(&["README", "fresh", "src"]),
                v(&["empty", "main.c", "src"])
            )
        );
    }
}

/// An update, with directories' own props left out. Through a directory,
/// OCaml's `buildUpdatePathTree` gives the archive's props (`archDesc`),
/// where a whole scan gives those it reads; and each scan brings the
/// cached archive's directories up to date, which the next scan sees as
/// their previous state. Unix's fast directory check keeps these the same,
/// but Windows has none, so there the times differ.
fn shape(ui: &UpdateItem) -> String {
    let prev = |p: &PrevState| match p {
        PrevState::Dir(_) => "Dir".to_owned(),
        other => format!("{other:?}"),
    };
    match ui {
        UpdateItem::Updates(UpdateContent::Dir(_, children, perm, emptied), p) => {
            let children: Vec<String> = children
                .iter()
                .map(|(n, ui)| format!("{}: {}", String::from_utf8_lossy(n.as_bytes()), shape(ui)))
                .collect();
            format!(
                "Dir({perm:?}, {emptied}, [{}]) from {}",
                children.join(", "),
                prev(p)
            )
        }
        UpdateItem::Updates(content, p) => format!("{content:?} from {}", prev(p)),
        other => format!("{other:?}"),
    }
}

fn shapes(found: &[PathUpdates]) -> Vec<String> {
    found
        .iter()
        .map(|(a, b)| format!("{:?} {} | {:?} {}", a.0, shape(&a.1), b.0, shape(&b.1)))
        .collect()
}

/// Whether a directory's update says it was emptied.
fn emptied(ui: &UpdateItem) -> bool {
    match ui {
        UpdateItem::Updates(UpdateContent::Dir(_, _, _, e), _) => *e,
        other => panic!("not a directory: {other:?}"),
    }
}

#[test]
fn only_a_whole_scan_says_a_directory_was_emptied() {
    for via in Via::ALL {
        let dir = TempDir::new("watch-emptied");
        let s = synced(&dir, via);
        assert_eq!(changed(&find_watching(&s, None)), none());
        std::fs::remove_file(s.a().join("src/deep/x")).unwrap();
        wait_on(&s, 0);
        let watched = find_watching(&s, reported());
        let full = s.find();
        let deep = |f: &[PathUpdates]| {
            let [(a, _)] = f else { panic!("one path") };
            child(child(&a.1, "src"), "deep").clone()
        };
        // OCaml's `buildUpdatePathTree` says that a directory it goes
        // through wasn't emptied; only a scan of the whole directory says
        // it was. On Windows a deletion counts as a change to its
        // directory, as with OCaml's Windows watcher, so the watcher
        // reports src/deep itself, which is scanned whole.
        assert!(emptied(&deep(&full)), "{via}");
        assert_eq!(emptied(&deep(&watched)), cfg!(windows), "{via}");
        assert_eq!(names(&deep(&watched)), names(&deep(&full)));
    }
}

#[test]
fn directories_a_watched_scan_finds_are_watched() {
    for via in Via::ALL {
        let dir = TempDir::new("watch-new");
        let s = synced(&dir, via);
        assert_eq!(changed(&find_watching(&s, None)), none());
        std::fs::create_dir(s.a().join("made")).unwrap();
        std::fs::create_dir(s.b().join("made")).unwrap();
        wait(&s);
        // This scan of the new directories tells the watcher of them...
        assert_eq!(
            changed(&find_watching(&s, reported())),
            (v(&["made"]), v(&["made"]))
        );
        // ... so what happens in them later is reported.
        write(&s.a(), "made/inside", "i", T0 + 70.0);
        write(&s.b(), "made/inside", "i", T0 + 70.0);
        wait(&s);
        let found = find_watching(&s, reported());
        assert_eq!(changed(&found), (v(&["made"]), v(&["made"])));
        let [(a, _)] = &found[..] else { panic!() };
        assert_eq!(names(child(&a.1, "made")), v(&["inside"]));
    }
}
