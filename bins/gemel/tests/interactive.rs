//! The `gemel` program in interactive mode, with keys typed on stdin.
//! Stdin isn't a terminal, so both programs read it a line at a time, as
//! in a dumb terminal (`dumbtty`): each line gives its keys in turn, an
//! empty one is return, and the keys are echoed. With
//! `GEMEL_REFERENCE_UNISON`, OCaml's `unison -ui text` gets the same keys
//! on a twin pair of replicas, and both runs must end the same.
//!
//! The replicas are first synced in batch mode, so that there are
//! archives: OCaml asks about a warning as soon as it comes, and then
//! shows the paths it scans over the question, which paths depending on
//! the time, where gemel asks once the scan is done.

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

#[cfg(unix)]
use common::Driven;
use common::{same_keys, twins, twins_each};
use support::sync::T0;
use support::write;
use ui_support::Pair;

fn same(g: &Pair, o: &Pair, args: &[&str], keys: &str) -> common::Run {
    same_keys(g, o, args, Some(keys.as_bytes()))
}

/// Items of every kind, from replicas synced once (`archives`), for each
/// way of reaching root `b` (see [`common::twins_each`]).
fn items_each(tag: &str) -> Vec<(Pair, Pair)> {
    let each = twins_each(tag, &["times = true"], |r, _| {
        write(r, "c", "c\n", T0);
    });
    each.into_iter().map(|(g, o)| items_in(g, o)).collect()
}

/// [`items_each`], with both roots here.
fn items(tag: &str) -> (Pair, Pair) {
    let (g, o) = twins(tag, &["times = true"], |r, _| {
        write(r, "c", "c\n", T0);
    });
    items_in(g, o)
}

fn items_in(g: Pair, o: Pair) -> (Pair, Pair) {
    same(&g, &o, &["p", "-batch"], "");
    for p in [&g, &o] {
        for side in ["a", "b"] {
            let r = if side == "a" { p.a() } else { p.b() };
            write(&r, "c", &format!("{side}'s c\n"), T0 + 1.0);
            write(&r, &format!("new-{side}"), side, T0 + 2.0);
            write(&r, &format!("dir-{side}/x"), "x", T0 + 3.0);
            write(&r, &format!("dir-{side}/y"), "y", T0 + 4.0);
        }
    }
    (g, o)
}

/// Moving through the list, the help, the details and the listings, keys
/// that don't exist, and quitting from the proceed menu.
#[test]
fn going_through_the_items() {
    // (Both roots here: differences with a remote root come with phase 5.)
    let (g, o) = items("int-moves");
    let keys = [
        // The conflict: no default, then the help, the details, and the
        // differences.
        "", "?", "x", "d", "f", "z!", "\x1b[B", // Down the list and back up.
        "nj", "p", "k", "0", "9", "6", "5", "L", "l", "R", "R", "\x7f",
        // On to the proceed menu, its help, and quitting.
        "s", "?", "w", "q",
    ];
    let r = same(&g, &o, &["p"], &(keys.join("\n") + "\n"));
    assert_eq!(r.code, Some(3), "{r:#?}");
}

/// Changing what is done with each item, alone and with matching
/// conditions, then propagating.
#[test]
fn changing_the_actions() {
    for (g, o) in twins_each("int-actions", &[], |r, side| {
        for f in ["f1", "f2", "f3", "f4", "f5", "c1", "c2"] {
            write(r, f, &format!("{f}\n"), T0);
        }
        let _ = side;
    }) {
        let args = ["a", "b", "-times"];
        same(&g, &o, &["a", "b", "-times", "-batch"], "");
        for p in [&g, &o] {
            write(&p.a(), "f1", "a's f1\n", T0 + 10.0);
            write(&p.b(), "f2", "b's f2\n", T0 + 11.0);
            write(&p.a(), "f3", "a's f3\n", T0 + 12.0);
            write(&p.b(), "f4", "b's f4\n", T0 + 13.0);
            write(&p.a(), "f5", "a's f5\n", T0 + 14.0);
            write(&p.a(), "c1", "a's c1\n", T0 + 15.0);
            write(&p.b(), "c1", "b's c1\n", T0 + 16.0);
            write(&p.a(), "c2", "a's c2\n", T0 + 18.0);
            write(&p.b(), "c2", "b's c2\n", T0 + 17.0);
            // Not the same contents: OCaml's client would copy them at
            // once, the second from the first's temporary file, where
            // gemel's copies them one at a time until phase 5.
            write(&p.b(), "n1", "new\n", T0 + 19.0);
            write(&p.b(), "n2", "new 2\n", T0 + 20.0);
        }
        let keys = [
            // c1 and c2: in favour of the newer, then of the older.
            "]", "[", // f1: the other way, inverted, skipped, reverted.
            "<", "p", "i", "p", "/", "p", "r",
            // f2: can't take the changed side, merge, revert.
            "c", "m", "p", "u", // Conditions: those to the right, and not; and two at once.
            "1", "X", "L", "2", "&", "L", "|", "L", "D", "D", "D", "C", "M", "P", "U", "U",
            // Everything after here to the left, then skipped.
            "A", "<", "L", "U", "%", "L", // Back to the start: revert all, drop f5, and go.
            "0", "*", "r", "U", "n", "n", "n", "n", "n", "n", "-", "g",
        ];
        let r = same(&g, &o, &args, &(keys.join("\n") + "\n"));
        assert_eq!(r.code, Some(1), "{r:#?}");
    }
}

/// Ignoring a path, an extension and a name for good, from a profile.
#[test]
fn ignoring_items_for_good() {
    let (g, o) = twins("int-ignore", &["times = true"], |_, _| {});
    same(&g, &o, &["p", "-batch"], "");
    for p in [&g, &o] {
        for f in ["a.tmp", "b.tmp", "keep", "sub/name", "other/name", "z"] {
            write(&p.a(), f, f, T0);
        }
    }
    let r = same(&g, &o, &["p"], "E\nn\nN\nI\nq\n");
    assert_eq!(r.code, Some(3), "{r:#?}");
    let profile = |p: &Pair| {
        let prf = std::fs::read_to_string(p.u().join("p.prf")).unwrap();
        prf.replace(p.base().to_str().unwrap(), "S")
    };
    let prf = profile(&g);
    assert!(prf.contains("ignore = Name {,.}*{.tmp}"), "{prf}");
    if common::ocaml_path().is_some() {
        assert_eq!(prf, profile(&o));
    }
}

/// The proceed menu: sorting, going through the items again, going back
/// to the last one, and proceeding.
#[test]
fn the_proceed_menu() {
    let (g, o) = twins("int-proceed", &["times = true"], |_, _| {});
    same(&g, &o, &["p", "-batch"], "");
    for p in [&g, &o] {
        write(&p.a(), "big", &"x".repeat(1000), T0);
        write(&p.a(), "small", "x", T0 + 1.0);
        write(&p.b(), "medium", &"x".repeat(100), T0 + 2.0);
    }
    let keys = [
        "s", "S", "R", "N", "W", "D", "?", "n", "", "", "", "p", "", "y",
    ];
    let r = same(&g, &o, &["p", "-auto"], &(keys.join("\n") + "\n"));
    assert_eq!(r.code, Some(0), "{r:#?}");
}

/// With `-auto`, only conflicts are asked about.
#[test]
fn auto_asks_only_about_conflicts() {
    let (g, o) = items("int-auto");
    let r = same(&g, &o, &["p", "-auto"], ">\ny\n");
    assert_eq!(r.code, Some(0), "{r:#?}");
}

/// A dangerous deletion is asked about: no stops, yes goes on.
#[test]
fn a_dangerous_deletion_is_asked_about() {
    let (g, o) = twins("int-dangerous", &["times = true"], |r, _| {
        write(r, "f", "f", T0);
        write(r, "g", "g", T0);
    });
    same(&g, &o, &["p", "-batch"], "");
    for p in [&g, &o] {
        for n in ["f", "g"] {
            std::fs::remove_file(p.a().join(n)).unwrap();
        }
    }
    let r = same(&g, &o, &["p"], "n\n");
    assert_eq!(r.code, Some(3), "{r:#?}");
    let r = same(&g, &o, &["p"], "y\n\n\ny\n");
    assert_eq!(r.code, Some(0), "{r:#?}");
}

/// With `confirmmerge`, a merge's result is kept only if the user says
/// so; otherwise the item fails.
#[cfg(unix)]
#[test]
fn a_merge_is_confirmed() {
    let lines = [
        "times = true",
        "confirmmerge = true",
        "merge = Name f -> cat CURRENT1 CURRENT2 > NEW",
    ];
    let (g, o) = twins("int-merge", &lines, |r, _| write(r, "f", "same\n", T0));
    same(&g, &o, &["p", "-batch"], "");
    for p in [&g, &o] {
        write(&p.a(), "f", "a's\n", T0 + 10.0);
        write(&p.b(), "f", "b's\n", T0 + 11.0);
    }
    let r = same(&g, &o, &["p"], "\ny\nn\n");
    assert_eq!(r.code, Some(2), "{r:#?}");
    let r = same(&g, &o, &["p"], "\ny\ny\n");
    assert_eq!(r.code, Some(0), "{r:#?}");
}

/// The input ends while the user is asked something.
#[test]
fn the_input_ends() {
    for (g, o) in items_each("int-eof") {
        let r = same(&g, &o, &["p"], "");
        assert_eq!(r.code, Some(3), "{r:#?}");
        let r = same(&g, &o, &["p"], ">\n");
        assert_eq!(r.code, Some(3), "{r:#?}");
    }
}

/// Stopping at a warning: the one that there are no archives, which
/// comes before any path is scanned.
#[test]
fn stopping_at_a_warning() {
    for (g, o) in twins_each("int-warning", &[], |r, side| {
        write(r, &format!("new-{side}"), side, T0);
    }) {
        let r = same(&g, &o, &["a", "b"], "x\n");
        assert_eq!(r.code, Some(3), "{r:#?}");
        // And the archives were left unlocked: the next run goes through.
        let r = same(&g, &o, &["a", "b", "-batch"], "");
        assert_eq!(r.code, Some(0), "{r:#?}");
    }
}

/// The archives' lock files left in the Unison directory.
#[cfg(unix)]
fn locks(p: &Pair) -> Vec<String> {
    std::fs::read_dir(p.u())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("lk"))
        .collect()
}

/// Ctrl-C while the user is asked about an item stops the program, and
/// the archives are left unlocked.
#[cfg(unix)]
#[test]
fn ctrl_c_at_a_prompt() {
    for (g, o) in items_each("int-ctrl-c") {
        let r = common::same_signals(Driven::start, &g, &o, &["p"], |p| {
            p.wait_for("c  [] ", 1);
            p.signal("INT");
        });
        assert_eq!(r.code, Some(3), "{r:#?}");
        assert_eq!(r.stderr.last().map(String::as_str), Some("Terminated!"));
        assert_eq!(locks(&g), Vec::<String>::new());
    }
}

/// A merge to confirm, after a first sync: the sync waits for the user.
#[cfg(unix)]
fn merge_to_confirm(tag: &str) -> (Pair, Pair) {
    let lines = [
        "times = true",
        "confirmmerge = true",
        "merge = Name f -> cat CURRENT1 CURRENT2 > NEW",
    ];
    let (g, o) = twins(tag, &lines, |r, _| write(r, "f", "same\n", T0));
    same(&g, &o, &["p", "-batch"], "");
    for p in [&g, &o] {
        write(&p.a(), "f", "a's\n", T0 + 10.0);
        write(&p.b(), "f", "b's\n", T0 + 11.0);
    }
    (g, o)
}

/// During a sync, Ctrl-C and `SIGTERM` abort it (Ctrl-C saying so, if
/// it is the first), and the program stops once it has ended.
#[cfg(unix)]
#[test]
fn ctrl_c_during_a_sync() {
    let (g, o) = merge_to_confirm("int-ctrl-c-sync");
    let r = common::same_signals(Driven::start, &g, &o, &["p"], |p| {
        p.wait_for("[f] ", 1);
        p.type_keys(b"\n");
        p.wait_for("Proceed with propagating updates? [] ", 1);
        p.type_keys(b"y\n");
        p.wait_for("Commit results of merge? [] ", 1);
        p.signal("INT");
        p.wait_for("Update propagation interrupted.", 1);
        p.signal("TERM");
        p.pause();
        p.type_keys(b"y\n");
    });
    assert_eq!(r.code, Some(3), "{r:#?}");
    assert_eq!(r.stderr.last().map(String::as_str), Some("Terminated!"));
    assert_eq!(locks(&g), Vec::<String>::new());
}

/// The fourth interrupt during a sync stops the program at once.
#[cfg(unix)]
#[test]
fn the_fourth_interrupt_stops_at_once() {
    let (g, o) = merge_to_confirm("int-fourth");
    let r = common::same_signals(Driven::start, &g, &o, &["p"], |p| {
        p.wait_for("[f] ", 1);
        p.type_keys(b"\n");
        p.wait_for("Proceed with propagating updates? [] ", 1);
        p.type_keys(b"y\n");
        p.wait_for("Commit results of merge? [] ", 1);
        p.signal("TERM");
        for _ in 0..3 {
            p.pause();
            p.signal("INT");
        }
    });
    assert_eq!(r.code, Some(3), "{r:#?}");
    assert!(
        r.stderr
            .iter()
            .any(|l| l.contains(" by user request") && l.starts_with("Synchronization interrupted")),
        "{r:#?}"
    );
    assert_eq!(locks(&g), Vec::<String>::new());
}

/// The profile manager (`-i`): the profiles listed with their keys, labels
/// and roots, and one chosen (by its own key, a digit given, or `d` for the
/// default); none; quitting; and an unknown key.
#[test]
fn the_profile_manager() {
    let (g, o) = twins(
        "int-profiles",
        &["times = true", "label = the first"],
        |r, side| {
            write(r, &format!("new-{side}"), side, T0);
        },
    );
    for p in [&g, &o] {
        p.profile("default", &["times = true"]);
        p.profile("keyed", &["times = true", "key = 1", "label = with a key"]);
        p.profile("other", &["times = true"]);
    }
    for keys in ["1\n", "z\nd\n", "\n", "q\n", "0\n"] {
        let r = same(&g, &o, &["-i", "-batch"], keys);
        assert!(
            r.stdout.iter().any(|l| l == "Available profiles:"),
            "{r:#?}"
        );
    }
}

/// Too many profiles to choose from, or none: the default.
#[test]
fn the_profile_manager_without_a_choice() {
    let (g, o) = twins("int-many-profiles", &["times = true"], |_, _| {});
    // Only `p`, and none chosen; then 11.
    let r = same(&g, &o, &["-i", "-batch"], "\n");
    assert_eq!(r.code, Some(0), "{r:#?}");
    for p in [&g, &o] {
        for i in 0..10 {
            p.profile(&format!("p{i}"), &[]);
        }
    }
    let r = same(&g, &o, &["-i", "-batch"], "");
    assert!(
        !r.stdout.iter().any(|l| l == "Available profiles:"),
        "{r:#?}"
    );
}
