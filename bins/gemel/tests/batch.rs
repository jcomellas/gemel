//! The `gemel` program in batch mode, run as a user runs it. With
//! `GEMEL_REFERENCE_UNISON` naming OCaml Unison, a twin of each pair of
//! replicas is synced by `unison <args> -ui text`, and both runs must end
//! the same: the same exit code, the same output (as a terminal shows it),
//! the same log, and the same replicas.

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use common::{gemel, ocaml, same, twins, twins_each};
use support::sync::T0;
use support::write;
use ui_support::Pair;

/// A first sync, then changes on both sides with a conflict and a
/// deletion, then nothing to do: from the command line's roots.
#[test]
fn roots_on_the_command_line() {
    for (g, o) in twins_each("batch-roots", &[], |r, side| {
        if side == "a" {
            write(r, "f", "one", T0);
            write(r, "sub/g", "two", T0 + 1.0);
        } else {
            write(r, "h", "three", T0 + 2.0);
        }
    }) {
        let args = ["a", "b", "-batch", "-times"];
        let r = same(&g, &o, &args);
        assert_eq!(r.code, Some(0), "{r:#?}");
        for p in [&g, &o] {
            write(&p.a(), "f", "a's", T0 + 10.0);
            write(&p.b(), "f", "b's", T0 + 11.0);
            std::fs::remove_file(p.a().join("sub/g")).unwrap();
            write(&p.b(), "new", "new in b", T0 + 12.0);
        }
        // With `-auto`, only conflicts get their details.
        let r = same(&g, &o, &["a", "b", "-batch", "-times", "-auto"]);
        assert_eq!(r.code, Some(1), "{r:#?}");
        // The conflict is left.
        let r = same(&g, &o, &args);
        assert_eq!(r.code, Some(1), "{r:#?}");
    }
}

/// From a profile, with `-terse`, then only a conflict (nothing to
/// propagate).
#[test]
fn a_profile_with_terse() {
    for (g, o) in twins_each("batch-terse", &["times = true"], |r, side| {
        write(r, "same", "same", T0);
        write(r, &format!("only-{side}"), side, T0 + 1.0);
    }) {
        same(&g, &o, &["p", "-batch", "-terse"]);
        let r = same(&g, &o, &["p", "-batch"]);
        assert!(
            r.stderr.iter().any(|l| l.starts_with("Nothing to do")),
            "{r:#?}"
        );
        for p in [&g, &o] {
            write(&p.a(), "same", "a's", T0 + 10.0);
            write(&p.b(), "same", "b's", T0 + 11.0);
        }
        let r = same(&g, &o, &["p", "-batch"]);
        assert!(
            r.stdout.iter().any(|l| l == "No updates to propagate"),
            "{r:#?}"
        );
    }
}

/// `-silent` prints nothing but errors.
#[test]
fn silent() {
    for (g, o) in twins_each("batch-silent", &["times = true"], |r, side| {
        write(r, &format!("only-{side}"), side, T0);
    }) {
        let r = same(&g, &o, &["p", "-silent"]);
        assert_eq!(r.code, Some(0));
    }
}

/// Deleting everything in one replica is dangerous: batch mode aborts.
#[test]
fn a_dangerous_deletion_aborts() {
    for (g, o) in twins_each("batch-dangerous", &["times = true"], |r, _| {
        write(r, "f", "f", T0);
        write(r, "g", "g", T0);
    }) {
        same(&g, &o, &["p", "-batch"]);
        for p in [&g, &o] {
            for n in ["f", "g"] {
                std::fs::remove_file(p.a().join(n)).unwrap();
            }
        }
        let r = same(&g, &o, &["p", "-batch"]);
        assert_eq!(r.code, Some(3), "{r:#?}");
        assert_eq!(r.stdout.last().map(String::as_str), Some("Aborting..."));
    }
}

/// A merge shows its program's output.
#[cfg(unix)]
#[test]
fn a_merge() {
    let lines = [
        "times = true",
        "merge = Name f -> cat CURRENT1 CURRENT2 > NEW",
    ];
    let (g, o) = twins("batch-merge", &lines, |r, _| write(r, "f", "same\n", T0));
    same(&g, &o, &["p", "-batch"]);
    for p in [&g, &o] {
        write(&p.a(), "f", "a's\n", T0 + 10.0);
        write(&p.b(), "f", "b's\n", T0 + 11.0);
    }
    let r = same(&g, &o, &["p", "-batch"]);
    assert_eq!(r.code, Some(0), "{r:#?}");
}

/// What goes wrong before a sync: a missing profile, too many roots, no
/// roots at all, and an error in a profile.
#[test]
fn startup_errors() {
    let (g, o) = twins("batch-errors", &[], |_, _| {});
    let r = same(&g, &o, &["missing", "-batch"]);
    assert_eq!(r.code, Some(1), "{r:#?}");
    let r = same(&g, &o, &["a", "b", "c", "d", "-batch"]);
    assert_eq!(r.code, Some(1), "{r:#?}");
    let r = same(&g, &o, &["-batch"]);
    assert_eq!(r.code, Some(1), "{r:#?}");
    // Other profiles are left alone: one with a key that isn't a digit,
    // and two with the same key.
    for p in [&g, &o] {
        p.profile("p", &["times = true"]);
        std::fs::write(p.u().join("q.prf"), "key = x\n").unwrap();
        std::fs::write(p.u().join("r.prf"), "key = 1\n").unwrap();
        std::fs::write(p.u().join("s.prf"), "key = 1\n").unwrap();
    }
    let r = same(&g, &o, &["p", "-batch"]);
    assert_eq!(r.code, Some(0), "{r:#?}");
    for p in [&g, &o] {
        std::fs::write(p.u().join("bad.prf"), "nosuchpref = 1\n").unwrap();
    }
    let r = same(&g, &o, &["bad", "-batch"]);
    assert_eq!(r.code, Some(3), "{r:#?}");
}

/// `-version`.
#[test]
fn version() {
    let pair = Pair::new("batch-version");
    let r = gemel(&pair, &["-version"]);
    assert_eq!(r.code, Some(0));
    assert_eq!(
        r.stdout,
        [format!("gemel version {}", env!("CARGO_PKG_VERSION"))]
    );
}

/// The manual (`-doc`): its topics, all of it, a topic, the first section
/// with no topic, and a topic it doesn't have; as OCaml prints them, but
/// for the program's name and the About section.
#[test]
fn the_manual() {
    let pair = Pair::new("batch-doc");
    let topics = gemel(&pair, &["-doc", "topics"]);
    assert_eq!(topics.code, Some(0));
    assert_eq!(
        topics.stdout,
        [
            "Documentation topics:",
            "          about About Gemel",
            "         people People",
            "      obtaining Obtaining Unison",
            "    development Community, Maintenance, and Development",
            "        copying Copying",
            "            ack Acknowledgements",
            "      upgrading Upgrading",
            "       tutorial Tutorial",
            "         basics Basic Concepts",
            "       failures Invariants",
            "        running Running Unison",
            "",
            "Type \"gemel -doc <topic>\" for detailed information about <topic>",
            "or \"gemel -doc all\" for the whole manual",
            "",
        ]
    );
    let about = gemel(&pair, &["-doc", "about"]);
    let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(
        about.stdout,
        ["", "Gemel File Synchronizer", &version, "", ""]
    );
    let unknown = gemel(&pair, &["-doc", "nosuch"]);
    assert_eq!(unknown.code, Some(0));
    assert_eq!(
        unknown.stdout,
        [
            "Documentation topic nosuch not recognized:",
            "Type \"gemel -doc topics\" for a list",
        ]
    );
    assert_eq!(gemel(&pair, &["-doc", ""]).stdout[..2], ["", "Overview"]);
    // Every section but the Junk, each heading first.
    let all = gemel(&pair, &["-doc", "all"]);
    let headings = [
        "Gemel File Synchronizer",
        "Overview",
        "Preface",
        "People",
        "Obtaining Unison",
        "Community, Maintenance, and Development",
        "Copying",
        "Acknowledgements",
        "Upgrading",
        "Tutorial",
        "Basic Concepts",
        "Invariants",
        "Reference Guide",
        "Running Unison",
    ];
    let mut at = 0;
    for h in headings {
        at += all.stdout[at..].iter().position(|l| l == h).expect(h);
    }
    assert!(!all.stdout.iter().any(|l| l == "Junk"));

    // OCaml must be built from the same commit: 2.54.0's manual still has
    // a paragraph on `mergebatch`, which the port's source removed with
    // the preference. That paragraph aside, the manual is the same.
    let mergebatch = [
        "   Ordinarily, external merge programs are only invoked when Unison is not",
        "   running in batch mode. To specify an external merge program that should",
        "   be used no matter the setting of the batch flag, use the mergebatch",
        "   preference instead of merge.",
        "",
    ];
    let theirs_as_ours = |ls: Vec<String>| -> Vec<String> {
        let ls: Vec<String> = ls
            .into_iter()
            .map(|l| match l.as_str() {
                "          about About Unison" => "          about About Gemel".to_owned(),
                "Unison File Synchronizer" => "Gemel File Synchronizer".to_owned(),
                "Version 2.54.0" => version.clone(),
                _ => l.replace("\"unison -doc", "\"gemel -doc"),
            })
            .collect();
        match ls.windows(mergebatch.len()).position(|w| w == mergebatch) {
            Some(i) => [&ls[..i], &ls[i + mergebatch.len()..]].concat(),
            None => ls,
        }
    };
    let mut topics: Vec<&str> = vec!["topics", "all", "nosuch", ""];
    topics.extend(gemel_ui_api::manual::sections().map(|s| s.short));
    for topic in topics {
        let Some(theirs) = ocaml(&pair, &["-doc", topic]) else {
            return;
        };
        let ours = gemel(&pair, &["-doc", topic]);
        assert_eq!(ours.code, theirs.code, "{topic:?}");
        assert_eq!(ours.stdout, theirs_as_ours(theirs.stdout), "{topic:?}");
        assert_eq!(ours.stderr, theirs.stderr, "{topic:?}");
    }
}

/// The usage and every option, from `-help` and after a usage error; the
/// options for the manual page (`-prefsman short`).
#[test]
fn the_list_of_options() {
    let pair = Pair::new("batch-help");
    for (args, code) in [
        (&["-help"][..], 2),
        (&["-nosuchoption"][..], 2),
        (&["-ui", "neither"][..], 1),
        (&["-prefsman", "short"][..], 0),
    ] {
        let r = gemel(&pair, args);
        assert_eq!(r.code, Some(code), "{args:?}");
        assert!(r.stdout.iter().any(|l| l.contains("-batch ")), "{r:#?}");
        // OCaml gets `-ui text` after the arguments, which would win.
        let Some(theirs) = ocaml(&pair, args).filter(|_| args[0] != "-ui") else {
            continue;
        };
        assert_eq!(r.code, theirs.code, "{args:?}");
        let usage = |ls: &[String]| -> Vec<String> {
            ls.iter()
                // The usage and some docs name the program.
                .map(|l| l.replace("unison", "gemel"))
                .collect()
        };
        assert_eq!(usage(&r.stdout), usage(&theirs.stdout), "{args:?}");
        // The error names the program as it was run.
        let message = |ls: &[String]| {
            ls.first()
                .and_then(|l| l.split_once(": ").map(|(_, m)| m.to_owned()))
        };
        assert_eq!(message(&r.stderr), message(&theirs.stderr), "{args:?}");
    }
}
