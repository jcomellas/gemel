//! The session contract, driven as a frontend drives it: commands in from
//! another thread, events out. Whole syncs from a profile are checked
//! against OCaml Unison when `GEMEL_REFERENCE_UNISON` names it.

#[path = "../../gemel-engine/tests/support/mod.rs"]
mod support;
mod ui_support;

use std::sync::Arc;

use gemel_base::trace::Trace;
use gemel_rpc::runtime::EngineThread;
use gemel_ui_api::exit_code::ExitCode;
use gemel_ui_api::session::{
    self, Command, Config, Event, IgnoreKind, ItemView, Outcome, Question, SetAction, SortKind,
    Summary, Traffic, Updates,
};
use gemel_ui_api::uicommon::Action;
use support::sync::{Output, T0};
use support::{settle, write};
use ui_support::Pair;

type Session = EngineThread<Command, Event>;

/// A session on `pair`, with `args`; its trace prints to the output.
fn start(pair: &Pair, args: &[&str], loopback: bool) -> (Session, Output) {
    start_counting(pair, args, loopback, None)
}

/// [`start`], counting the connections' bytes in `traffic`.
fn start_counting(
    pair: &Pair,
    args: &[&str],
    loopback: bool,
    traffic: Option<Arc<Traffic>>,
) -> (Session, Output) {
    let output = Output::default();
    let trace = Trace::with_sinks(Box::new(output.clone()), Box::new(output.clone()));
    let s = session::spawn(Config {
        unison_dir: pair.u(),
        args: args.iter().map(|s| s.to_string()).collect(),
        trace,
        loopback,
        status_events: true,
        scan_status: true,
        log_warnings: false,
        print_wait_message: false,
        complete_progress: true,
        traffic,
    })
    .unwrap();
    (s, output)
}

/// Whether `e` ends command `c`.
fn is_reply(c: &Command, e: &Event) -> bool {
    match e {
        Event::Started(_)
        | Event::Profiles(_)
        | Event::ProfileLoaded(_)
        | Event::ProfileUnchanged(_)
        | Event::Connected(_)
        | Event::Updates(_)
        | Event::Items { .. }
        | Event::Synchronized(_)
        | Event::Committed
        | Event::Diff { .. }
        | Event::Error(_)
        | Event::Interrupted
        | Event::Exit(_) => true,
        Event::Item(..) => matches!(c, Command::SetAction(..)),
        _ => false,
    }
}

/// Sends `c`, and takes the events up to its reply, with `answer`
/// answering the questions: the events before, and the reply.
fn call(
    s: &mut Session,
    c: Command,
    answer: &mut dyn FnMut(&mut Session, Question),
) -> (Vec<Event>, Event) {
    let shown = format!("{c:?}");
    assert!(s.send(c.clone()), "{shown}: the session has stopped");
    let mut before = Vec::new();
    loop {
        let e = s
            .recv_blocking()
            .unwrap_or_else(|| panic!("{shown}: no reply"));
        match e {
            Event::Ask(q) => answer(s, q),
            e if is_reply(&c, &e) => return (before, e),
            e => before.push(e),
        }
    }
}

/// Answers nothing: no question is expected.
fn no_questions(_: &mut Session, q: Question) {
    panic!("unexpected question: {q:?}");
}

/// Goes on at every warning.
fn go_on(_: &mut Session, q: Question) {
    match q {
        Question::Warning(_, r) => r.send(true),
        q => panic!("unexpected question: {q:?}"),
    }
}

fn ok(s: &mut Session, c: Command, answer: &mut dyn FnMut(&mut Session, Question)) -> Event {
    match call(s, c, answer).1 {
        Event::Error(e) => panic!("{e}"),
        e => e,
    }
}

/// Loads the profile the command line names.
fn load(s: &mut Session, answer: &mut dyn FnMut(&mut Session, Question)) {
    let Event::Started(profile) = ok(s, Command::Start, answer) else {
        panic!()
    };
    let profile = profile.expect("a profile");
    ok(s, Command::LoadProfile(profile), answer);
}

fn updates(s: &mut Session, answer: &mut dyn FnMut(&mut Session, Question)) -> Updates {
    let c = Command::Update {
        allow_partial: true,
        want_watcher: false,
        paths: None,
    };
    match ok(s, c, answer) {
        Event::Updates(u) => u,
        e => panic!("{e:?}"),
    }
}

fn synchronize(
    s: &mut Session,
    answer: &mut dyn FnMut(&mut Session, Question),
) -> (Vec<Event>, Summary) {
    match call(s, Command::Synchronize, answer) {
        (before, Event::Synchronized(summary)) => (before, summary),
        (_, e) => panic!("{e:?}"),
    }
}

/// `gemel <args>` in batch mode on `pair`, through a session, as the text
/// interface runs it: propagates and commits, unless every item is
/// skipped (then only commits). The items' details.
fn gemel(pair: &Pair, args: &[&str], loopback: bool) -> Vec<String> {
    let (mut s, _) = start(pair, args, loopback);
    load(&mut s, &mut no_questions);
    let u = updates(&mut s, &mut no_questions);
    let details = u.items.iter().map(|i| i.details.clone()).collect();
    if u.items.iter().all(|i| i.problematic) {
        if u.found_archives {
            ok(&mut s, Command::Commit, &mut no_questions);
        }
    } else {
        synchronize(&mut s, &mut no_questions);
        ok(&mut s, Command::Commit, &mut no_questions);
    }
    s.join().unwrap();
    details
}

#[test]
fn a_profile_syncs_like_ocaml() {
    ui_support::a_profile_syncs_like_ocaml(gemel);
}

#[test]
fn wildcard_paths_sync_like_ocaml() {
    ui_support::wildcard_paths_sync_like_ocaml(gemel);
}

/// A pair with the profile `p` (and `lines`), synced once in batch mode
/// with `build` on both replicas.
fn synced_pair(tag: &str, lines: &[&str], build: fn(&std::path::Path)) -> Pair {
    synced_pair_on(Pair::new(tag), lines, build, false)
}

/// [`synced_pair`], on `pair`, in loopback mode or not.
fn synced_pair_on(pair: Pair, lines: &[&str], build: fn(&std::path::Path), loopback: bool) -> Pair {
    pair.profile("p", lines);
    build(&pair.a());
    build(&pair.b());
    settle(&[&pair.a(), &pair.b()]);
    gemel(&pair, &["p", "-batch"], loopback);
    pair
}

/// A pair made for each way the session reaches root `b`: here, in
/// loopback mode, and on an OCaml server (only with OCaml Unison, on
/// Unix); with whether it is in loopback mode.
fn ways(tag: &str) -> Vec<(Pair, bool)> {
    let mut ways = vec![(Pair::new(tag), false), (Pair::new(tag), true)];
    if let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON").filter(|_| cfg!(unix)) {
        ways.push((Pair::remote(tag, &unison), false));
    }
    ways
}

fn item<'a>(items: &'a [ItemView], path: &str) -> (usize, &'a ItemView) {
    items
        .iter()
        .enumerate()
        .find(|(_, i)| i.path_string == path)
        .unwrap_or_else(|| panic!("{path}: {items:?}"))
}

/// The profiles are listed; a profile without roots asks for them, and
/// keeps them. Without an answer, it fails.
#[test]
fn profiles_are_listed_and_roots_asked_for() {
    let pair = Pair::new("session-roots");
    pair.profile("other", &["label = another"]);
    std::fs::write(pair.u().join("empty.prf"), "").unwrap();
    let (mut s, _) = start(&pair, &[], false);
    let Event::Started(profile) = ok(&mut s, Command::Start, &mut no_questions) else {
        panic!()
    };
    assert_eq!(profile, None);
    let Event::Profiles(profiles) = ok(&mut s, Command::ScanProfiles, &mut no_questions) else {
        panic!()
    };
    let names: Vec<_> = profiles.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["empty", "other"]);
    // Without an answer.
    let (_, e) = call(&mut s, Command::LoadProfile("empty".into()), &mut |_, q| {
        drop(q)
    });
    assert!(
        matches!(&e, Event::Error(m) if m.contains("no roots given")),
        "{e:?}"
    );
    let (a, b) = (pair.root("a"), pair.root("b"));
    let roots = (a.clone(), b.clone());
    let Event::ProfileLoaded(settings) = ok(
        &mut s,
        Command::LoadProfile("empty".into()),
        &mut |_, q| match q {
            Question::Roots(r) => r.send(Some(roots.clone())),
            q => panic!("{q:?}"),
        },
    ) else {
        panic!()
    };
    assert_eq!(settings.roots, [a.clone(), b.clone()]);
    assert_eq!(settings.profile, "empty");
    assert!(!settings.batch);
    let text = std::fs::read_to_string(pair.u().join("empty.prf")).unwrap();
    assert!(text.contains(&format!("root = {a}")), "{text}");
    // A new profile.
    let Event::Profiles(profiles) = ok(
        &mut s,
        Command::NewProfile {
            name: "made".into(),
            root1: a.clone(),
            root2: b.clone(),
        },
        &mut no_questions,
    ) else {
        panic!()
    };
    let (_, made) = profiles.iter().find(|(n, _)| n == "made").unwrap();
    assert_eq!(made.roots, [a, b]);
    s.join().unwrap();
}

/// Without archives, OCaml warns. In batch mode the warning is shown;
/// otherwise the user is asked, and may stop: then the archives are
/// unlocked and the session ends.
#[test]
fn warnings_are_shown_or_asked_about() {
    let build = |r: &std::path::Path| write(r, "f", "f", T0);
    // Batch mode.
    let pair = Pair::new("session-warn-batch");
    pair.profile("p", &[]);
    build(&pair.a());
    let (mut s, _) = start(&pair, &["p", "-batch"], false);
    load(&mut s, &mut no_questions);
    let c = Command::Update {
        allow_partial: true,
        want_watcher: false,
        paths: None,
    };
    let (before, _) = call(&mut s, c, &mut no_questions);
    assert!(
        before
            .iter()
            .any(|e| matches!(e, Event::Warning(w) if w.starts_with("No archive files"))),
        "{before:?}"
    );
    s.join().unwrap();
    // Stopping.
    let pair = Pair::new("session-warn-stop");
    pair.profile("p", &[]);
    build(&pair.a());
    let (mut s, _) = start(&pair, &["p"], false);
    load(&mut s, &mut no_questions);
    let c = Command::Update {
        allow_partial: true,
        want_watcher: false,
        paths: None,
    };
    let mut asked = Vec::new();
    let (_, e) = call(&mut s, c, &mut |_, q| match q {
        Question::Warning(w, r) => {
            asked.push(w);
            r.send(false)
        }
        q => panic!("{q:?}"),
    });
    assert!(matches!(e, Event::Exit(ExitCode::Fatal)), "{e:?} {asked:?}");
    assert_eq!(asked.len(), 1);
    assert!(s.recv_blocking().is_none(), "the session has ended");
    s.join().unwrap();
    let locks: Vec<_> = std::fs::read_dir(pair.u())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("lk"))
        .collect();
    assert_eq!(locks, Vec::<String>::new());
    // Going on.
    let (mut s, _) = start(&pair, &["p"], false);
    load(&mut s, &mut no_questions);
    let u = updates(&mut s, &mut go_on);
    assert_eq!(u.items.len(), 1);
    assert!(!u.found_archives);
    s.join().unwrap();
}

/// What a frontend shows of the items, and their actions changed and set
/// back.
#[test]
fn actions_can_be_changed_and_reverted() {
    for (pair, loopback) in ways("session-actions") {
        let pair = synced_pair_on(
            pair,
            &["times = true"],
            |r| {
                write(r, "both", "same", T0);
            },
            loopback,
        );
        write(&pair.a(), "both", "a's", T0 + 10.0);
        write(&pair.b(), "both", "b's, newer", T0 + 20.0);
        write(&pair.a(), "new", "new in a", T0 + 10.0);
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p", "-batch"], loopback);
        load(&mut s, &mut no_questions);
        let u = updates(&mut s, &mut no_questions);
        let (i, both) = item(&u.items, "both");
        assert_eq!(
            (both.left, both.right, both.action, both.conflict),
            ("Modified", "Modified", Action::Skip(false), true)
        );
        assert!(both.can_diff && both.problematic && !both.changed_from_default);
        assert!(both.line.contains("<-?->"), "{}", both.line);
        let (_, new) = item(&u.items, "new");
        assert_eq!(
            (new.left, new.right, new.action, new.length),
            ("Created", "", Action::LtoR(false), 8)
        );
        let set = |s: &mut Session, a| match ok(s, Command::SetAction(i, a), &mut no_questions) {
            Event::Item(j, v) => {
                assert_eq!(j, i);
                v
            }
            e => panic!("{e:?}"),
        };
        let v = set(&mut s, SetAction::Right);
        assert_eq!(v.action, Action::LtoR(false));
        assert!(v.changed_from_default && !v.problematic);
        assert_eq!(set(&mut s, SetAction::Revert).action, Action::Skip(false));
        assert_eq!(set(&mut s, SetAction::Older).action, Action::LtoR(false));
        assert_eq!(set(&mut s, SetAction::Newer).action, Action::RtoL(false));
        assert_eq!(set(&mut s, SetAction::Skip).action, Action::Skip(false));
        assert_eq!(set(&mut s, SetAction::Left).action, Action::RtoL(false));
        let (_, summary) = synchronize(&mut s, &mut no_questions);
        assert_eq!(summary.transferred, 2);
        assert_eq!(summary.exit_code(), ExitCode::Perfect);
        ok(&mut s, Command::Commit, &mut no_questions);
        s.join().unwrap();
        assert_eq!(support::read(&pair.a().join("both")), "b's, newer");
        assert_eq!(support::read(&pair.b().join("new")), "new in a");
    }
}

/// A sync reports each item's progress up to its length, then how it
/// went; a summary counts what was skipped and what failed.
#[test]
fn progress_and_outcomes_are_reported() {
    for (pair, loopback) in ways("session-progress") {
        let pair = synced_pair_on(
            pair,
            &[],
            |r| {
                write(r, "conflict", "same", T0);
            },
            loopback,
        );
        write(&pair.a(), "big", &"x".repeat(300_000), T0 + 10.0);
        write(&pair.a(), "small", "small", T0 + 10.0);
        write(&pair.a(), "changing", "first", T0 + 10.0);
        write(&pair.a(), "conflict", "a's", T0 + 10.0);
        write(&pair.b(), "conflict", "b's", T0 + 10.0);
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p", "-batch"], loopback);
        load(&mut s, &mut no_questions);
        let u = updates(&mut s, &mut no_questions);
        // Changed after the scan: its copy fails.
        write(&pair.a(), "changing", "second, longer", T0 + 20.0);
        let (before, summary) = synchronize(&mut s, &mut no_questions);
        let progress: Vec<_> = before
            .iter()
            .filter_map(|e| match e {
                Event::Progress(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        let last = progress.last().expect("progress");
        let length: i64 = u.items.iter().map(|i| i.length).sum();
        assert_eq!((last.transferred, last.length), (length, length));
        assert_eq!((last.items_done, last.items), (3, 3));
        let (big, _) = item(&u.items, "big");
        assert!(
            progress
                .iter()
                .any(|p| p.item == big && p.item_progress == "100%")
        );
        let done: Vec<_> = before
            .iter()
            .filter_map(|e| match e {
                Event::Item(i, v) => Some((*i, v.clone())),
                _ => None,
            })
            .collect();
        let outcome = |p: &str| {
            let (i, _) = item(&u.items, p);
            let (_, v) = done.iter().find(|(j, _)| *j == i).unwrap();
            (v.outcome.clone(), v.progress.clone())
        };
        assert_eq!(outcome("big"), (Some(Outcome::Succeeded), "done".into()));
        assert_eq!(outcome("small"), (Some(Outcome::Succeeded), "done".into()));
        let (failed, progress) = outcome("changing");
        assert!(
            matches!(&failed, Some(Outcome::Failed(m)) if m.contains("modified during synchronization")),
            "{failed:?}"
        );
        assert_eq!(progress, "FAILED");
        // The conflict is skipped, as transport logs it.
        assert_eq!(outcome("conflict").1, "");
        let (changing, _) = item(&u.items, "changing");
        let (conflict, _) = item(&u.items, "conflict");
        assert_eq!(summary.transferred, 2);
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].0, changing);
        assert_eq!(
            summary.skipped,
            [(conflict, "contents changed on both sides".into())]
        );
        assert_eq!((summary.not_started, summary.aborted), (0, false));
        assert_eq!(summary.exit_code(), ExitCode::Failed);
        let paths: Vec<_> = summary
            .unsynchronized
            .iter()
            .map(|p| String::from_utf8_lossy(p.as_bytes()).into_owned())
            .collect();
        assert_eq!(paths, ["changing", "conflict"]);
        ok(&mut s, Command::Commit, &mut no_questions);
        s.join().unwrap();
    }
}

/// While a sync waits for the user to confirm a merge, an abort gets
/// through (and other commands are refused): once answered, nothing more
/// starts. A merge not confirmed fails.
#[cfg(unix)]
#[test]
fn a_sync_can_be_aborted_while_it_waits_for_the_user() {
    let lines = [
        "merge = Name README -> cat CURRENT1 CURRENT2 > NEW",
        "confirmmerge = true",
    ];
    for keep in [true, false] {
        let pair = synced_pair("session-abort", &lines, |r| {
            write(r, "README", "read me\n", T0);
        });
        write(&pair.a(), "README", "a's\n", T0 + 10.0);
        write(&pair.b(), "README", "b's\n", T0 + 20.0);
        for n in ["later1", "later2"] {
            write(&pair.a(), n, n, T0 + 10.0);
        }
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p"], false);
        load(&mut s, &mut no_questions);
        let u = updates(&mut s, &mut no_questions);
        assert_eq!(item(&u.items, "README").1.action, Action::Merge);
        let mut refused = None;
        let (before, summary) = synchronize(&mut s, &mut |s, q| match q {
            Question::Merge { title, text, reply } => {
                assert_eq!(title, "Results of merging README");
                assert_eq!(text, "");
                if keep {
                    assert!(s.send(Command::Abort));
                    assert!(s.send(Command::Commit));
                    // The refusal comes before the merge goes on.
                    refused = s.recv_blocking();
                }
                reply.send(keep);
            }
            q => panic!("{q:?}"),
        });
        if keep {
            assert!(
                matches!(&refused, Some(Event::Refused(Command::Commit))),
                "{refused:?}"
            );
            // The merge goes on, unless it reaches an abort check while
            // it copies its result back: cloning a whole file (as on
            // APFS) has none, copying does.
            let readme = support::read(&pair.b().join("README"));
            match &summary.failed[..] {
                [] => {
                    assert_eq!(readme, "a's\nb's\n");
                    assert_eq!(summary.transferred, 1);
                    assert_eq!(summary.exit_code(), ExitCode::Skippy);
                }
                [(_, m)] => {
                    assert_eq!(m, "Aborted by user request");
                    assert_eq!(readme, "b's\n");
                    assert_eq!(summary.exit_code(), ExitCode::Failed);
                }
                f => panic!("{f:?}"),
            }
            assert_eq!(summary.not_started, 2);
            assert!(summary.aborted);
            assert!(!pair.b().join("later1").exists());
        } else {
            let failed: Vec<_> = summary.failed.iter().map(|(_, m)| m.as_str()).collect();
            assert_eq!(failed, ["Merge command canceled by the user"]);
            assert_eq!(summary.transferred, 2);
            assert!(pair.b().join("later1").exists());
        }
        assert!(
            !before.iter().any(|e| matches!(e, Event::Merge { .. })),
            "confirmed, not just shown"
        );
        ok(&mut s, Command::Commit, &mut no_questions);
        s.join().unwrap();
    }
}

/// Ignoring an item drops it (and what else is now ignored), and adds the
/// pattern to the profile.
#[test]
fn ignoring_an_item_drops_it() {
    let pair = synced_pair("session-ignore", &[], |_| {});
    for n in ["a.log", "b.txt", "c.log", "d.txt"] {
        write(&pair.a(), n, n, T0 + 10.0);
    }
    settle(&[&pair.a(), &pair.b()]);
    let (mut s, _) = start(&pair, &["p", "-batch"], true);
    load(&mut s, &mut no_questions);
    let u = updates(&mut s, &mut no_questions);
    let (i, _) = item(&u.items, "b.txt");
    let Event::Items { items, next } = ok(
        &mut s,
        Command::Ignore(i, IgnoreKind::Ext),
        &mut no_questions,
    ) else {
        panic!()
    };
    let names: Vec<_> = items.iter().map(|i| i.path_string.as_str()).collect();
    assert_eq!(names, ["a.log", "c.log"]);
    assert_eq!(next, 1);
    let text = std::fs::read_to_string(pair.u().join("p.prf")).unwrap();
    assert!(text.contains("ignore = Name {,.}*{.txt}"), "{text}");
    // And the next scan leaves them out.
    let u = updates(&mut s, &mut no_questions);
    let names: Vec<_> = u.items.iter().map(|i| i.path_string.as_str()).collect();
    assert_eq!(names, ["a.log", "c.log"]);
    s.join().unwrap();
}

/// A diff is shown with its title; a problem, as an error.
#[cfg(unix)]
#[test]
fn diffs_are_shown() {
    let pair = synced_pair("session-diff", &["diff = echo"], |r| {
        write(r, "f", "same", T0);
    });
    write(&pair.a(), "f", "a's", T0 + 10.0);
    write(&pair.b(), "f", "b's", T0 + 10.0);
    settle(&[&pair.a(), &pair.b()]);
    let (mut s, _) = start(&pair, &["p", "-batch"], false);
    load(&mut s, &mut no_questions);
    let u = updates(&mut s, &mut no_questions);
    let (i, _) = item(&u.items, "f");
    let Event::Diff { shown, errors } = ok(&mut s, Command::Diff(i), &mut no_questions) else {
        panic!()
    };
    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(shown.len(), 1);
    assert!(shown[0].0.starts_with("echo "), "{shown:?}");
    s.join().unwrap();
}

/// Status messages and the path being scanned come as events; commands
/// before the roots are connected fail.
#[test]
fn status_comes_as_events() {
    for (pair, loopback) in ways("session-status") {
        let pair = synced_pair_on(
            pair,
            &[],
            |r| {
                for i in 0..40 {
                    write(r, &format!("d/f{i:02}"), "f", T0);
                }
            },
            loopback,
        );
        write(&pair.a(), "new", "new", T0 + 10.0);
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p", "-batch"], loopback);
        load(&mut s, &mut no_questions);
        let (_, e) = call(&mut s, Command::Commit, &mut no_questions);
        assert!(
            matches!(&e, Event::Error(m) if m == "The roots are not connected"),
            "{e:?}"
        );
        let c = Command::Update {
            allow_partial: true,
            want_watcher: false,
            paths: None,
        };
        let (before, _) = call(&mut s, c, &mut no_questions);
        let statuses: Vec<_> = before
            .iter()
            .filter_map(|e| match e {
                Event::Status(m) => Some(m.as_str()),
                _ => None,
            })
            .collect();
        assert!(statuses.contains(&"Looking for changes "), "{statuses:?}");
        assert!(statuses.contains(&"Reconciling changes "), "{statuses:?}");
        let scanned: Vec<_> = before
            .iter()
            .filter_map(|e| match e {
                Event::Scanning(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        // The client shows what it scans, then an empty status; in
        // loopback mode the roots' host scans, and shows nothing. With a
        // server, once the client's own scan has ended, the status also
        // says it waits for the server.
        let waiting = statuses
            .iter()
            .any(|m| m.contains("Waiting for changes from server"));
        assert_eq!(waiting, pair.is_remote(), "{statuses:?}");
        if loopback {
            assert_eq!(scanned, [""]);
        } else {
            assert!(scanned.len() >= 2 || pair.is_remote(), "{scanned:?}");
            assert_eq!(scanned.last(), Some(&""));
        }
        s.join().unwrap();
    }
}

/// A frontend that quits with a question unanswered doesn't hang: the
/// question is answered no, so the session stops (unlocking the archives).
#[test]
fn quitting_answers_what_is_left_unanswered() {
    let pair = Pair::new("session-quit");
    pair.profile("p", &[]);
    write(&pair.a(), "f", "f", T0);
    let (mut s, _) = start(&pair, &["p"], false);
    load(&mut s, &mut no_questions);
    // The first scan warns that there are no archives; the question is
    // left unread.
    assert!(s.send(Command::Update {
        allow_partial: true,
        want_watcher: false,
        paths: None,
    }));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(s.join().is_ok()).unwrap());
    let joined = rx.recv_timeout(std::time::Duration::from_secs(20));
    assert_eq!(joined, Ok(true), "the session didn't end");
    let locks: Vec<_> = std::fs::read_dir(pair.u())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("lk"))
        .collect();
    assert_eq!(locks, Vec::<String>::new());
}

fn items(e: Event) -> Vec<ItemView> {
    match e {
        Event::Items { items, next } => {
            assert_eq!(next, 0);
            items
        }
        e => panic!("{e:?}"),
    }
}

fn names(items: &[ItemView]) -> Vec<&str> {
    items.iter().map(|v| v.path_string.as_str()).collect()
}

/// The items can be put in another order, some left out, and sorted; a
/// sync propagates only those kept.
#[test]
fn items_can_be_arranged_and_sorted() {
    let pair = synced_pair("session-arrange", &["times = true"], |_| {});
    write(&pair.a(), "a", &"x".repeat(100), T0);
    write(&pair.a(), "b", "x", T0);
    write(&pair.a(), "c", &"x".repeat(10), T0);
    settle(&[&pair.a(), &pair.b()]);
    let (mut s, _) = start(&pair, &["p", "-batch"], false);
    load(&mut s, &mut no_questions);
    let u = updates(&mut s, &mut no_questions);
    assert_eq!(names(&u.items), ["a", "b", "c"]);
    let kept = items(ok(&mut s, Command::Arrange(vec![2, 0]), &mut no_questions));
    assert_eq!(names(&kept), ["c", "a"]);
    let by_size = items(ok(&mut s, Command::Sort(SortKind::Size), &mut no_questions));
    assert_eq!(names(&by_size), ["c", "a"]);
    let by_name = items(ok(&mut s, Command::Sort(SortKind::Name), &mut no_questions));
    assert_eq!(names(&by_name), ["a", "c"]);
    let back = items(ok(
        &mut s,
        Command::Sort(SortKind::Default),
        &mut no_questions,
    ));
    assert_eq!(names(&back), ["a", "c"]);
    // An item given twice, or one that doesn't exist.
    for order in [vec![0, 0], vec![5]] {
        let (_, e) = call(&mut s, Command::Arrange(order), &mut no_questions);
        assert!(matches!(e, Event::Error(_)), "{e:?}");
    }
    let (_, summary) = synchronize(&mut s, &mut no_questions);
    assert_eq!(summary.transferred, 2);
    ok(&mut s, Command::Commit, &mut no_questions);
    s.join().unwrap();
    assert!(pair.b().join("a").exists() && pair.b().join("c").exists());
    assert!(!pair.b().join("b").exists());
}

/// Older and newer "if a conflict" change only a conflict, or an item
/// whose default is one.
#[test]
fn conflicts_are_resolved_only_if_they_are_conflicts() {
    for (pair, loopback) in ways("session-if-conflict") {
        let pair = synced_pair_on(
            pair,
            &["times = true"],
            |r| {
                write(r, "both", "same", T0);
            },
            loopback,
        );
        write(&pair.a(), "both", "a's", T0 + 10.0);
        write(&pair.b(), "both", "b's, newer", T0 + 20.0);
        write(&pair.a(), "new", "new in a", T0 + 10.0);
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p", "-batch"], loopback);
        load(&mut s, &mut no_questions);
        let u = updates(&mut s, &mut no_questions);
        let set = |s: &mut Session, i, a| match ok(s, Command::SetAction(i, a), &mut no_questions) {
            Event::Item(_, v) => v.action,
            e => panic!("{e:?}"),
        };
        let (both, _) = item(&u.items, "both");
        let (new, _) = item(&u.items, "new");
        assert_eq!(
            set(&mut s, new, SetAction::OlderIfConflict),
            Action::LtoR(false)
        );
        assert_eq!(
            set(&mut s, both, SetAction::NewerIfConflict),
            Action::RtoL(false)
        );
        // No longer a conflict, but its default is one.
        assert_eq!(
            set(&mut s, both, SetAction::OlderIfConflict),
            Action::LtoR(false)
        );
        s.join().unwrap();
    }
}

/// Whether the profile is as it was loaded: not once its file changes.
#[test]
fn a_changed_profile_is_told() {
    let pair = Pair::new("session-profile-changed");
    pair.profile("p", &["times = true"]);
    let (mut s, _) = start(&pair, &["p", "-batch"], false);
    load(&mut s, &mut no_questions);
    let unchanged = |s: &mut Session| match ok(s, Command::ProfileUnchanged, &mut no_questions) {
        Event::ProfileUnchanged(u) => u,
        e => panic!("{e:?}"),
    };
    assert!(unchanged(&mut s));
    pair.profile("p", &["times = true", "fastcheck = false"]);
    assert!(!unchanged(&mut s));
    ok(
        &mut s,
        Command::LoadProfile("p".to_owned()),
        &mut no_questions,
    );
    assert!(unchanged(&mut s));
    s.join().unwrap();
}

/// The bytes the client's connections write and read are counted where
/// the frontend says: over the loopback or to a server; locally, nothing
/// goes over a connection.
#[test]
fn traffic_is_counted() {
    for (pair, loopback) in ways("session-traffic") {
        let pair = synced_pair_on(pair, &[], |r| write(r, "f", "same", T0), loopback);
        write(&pair.a(), "new", "new in a", T0 + 10.0);
        settle(&[&pair.a(), &pair.b()]);
        let traffic = Arc::new(Traffic::default());
        let (mut s, _) = start_counting(&pair, &["p", "-batch"], loopback, Some(traffic.clone()));
        load(&mut s, &mut no_questions);
        updates(&mut s, &mut no_questions);
        synchronize(&mut s, &mut no_questions);
        ok(&mut s, Command::Commit, &mut no_questions);
        s.join().unwrap();
        let counted = (traffic.emitted(), traffic.received());
        if loopback || pair.is_remote() {
            assert!(counted.0 > 0 && counted.1 > 0, "{counted:?}");
        } else {
            assert_eq!(counted, (0, 0));
        }
    }
}

/// `uigtk3`'s actions: preferring a version changes only an item whose
/// default is a conflict, merging only a path to merge, and its skip
/// gives no reason. Its column headings name the roots in 15 characters.
#[test]
fn uigtk3s_actions_prefer_merge_and_skip() {
    for (pair, loopback) in ways("session-uigtk3") {
        let pair = synced_pair_on(
            pair,
            &["times = true", "merge = Name mergeable -> cp CURRENT1 NEW"],
            |r| {
                write(r, "both", "same", T0);
                write(r, "mergeable", "same", T0);
            },
            loopback,
        );
        write(&pair.a(), "both", "a's", T0 + 10.0);
        write(&pair.b(), "both", "b's, newer", T0 + 20.0);
        write(&pair.a(), "mergeable", "a's", T0 + 10.0);
        write(&pair.b(), "mergeable", "b's, newer", T0 + 20.0);
        write(&pair.a(), "new", "new in a", T0 + 10.0);
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p", "-batch"], loopback);
        load(&mut s, &mut no_questions);
        let Event::Connected(names) = ok(&mut s, Command::Connect, &mut no_questions) else {
            panic!()
        };
        let narrow = if pair.is_remote() {
            ("local".to_owned(), ui_support::SERVER.to_owned())
        } else {
            ("a".to_owned(), "b".to_owned())
        };
        assert_eq!(names.narrow, narrow);
        let u = updates(&mut s, &mut no_questions);
        let set = |s: &mut Session, i, a| match ok(s, Command::SetAction(i, a), &mut no_questions) {
            Event::Item(_, v) => v,
            e => panic!("{e:?}"),
        };
        let (both, _) = item(&u.items, "both");
        let (mergeable, m) = item(&u.items, "mergeable");
        assert_eq!(m.action, Action::Merge);
        let (new, _) = item(&u.items, "new");
        use session::Favor;
        for (f, action) in [
            (Favor::First, Action::LtoR(false)),
            (Favor::Second, Action::RtoL(false)),
            (Favor::Older, Action::LtoR(false)),
            (Favor::Newer, Action::RtoL(false)),
        ] {
            assert_eq!(set(&mut s, both, SetAction::Prefer(f)).action, action);
            // Not a conflict by default: left alone.
            assert_eq!(
                set(&mut s, new, SetAction::Prefer(Favor::Second)).action,
                Action::LtoR(false)
            );
        }
        let skipped = set(&mut s, mergeable, SetAction::Conflict);
        assert_eq!(
            (skipped.action, skipped.skip_reason.as_deref()),
            (Action::Skip(false), Some(""))
        );
        assert_eq!(
            set(&mut s, mergeable, SetAction::MergeDefault).action,
            Action::Merge
        );
        // Not a path to merge: left alone.
        assert_eq!(
            set(&mut s, both, SetAction::MergeDefault).action,
            Action::RtoL(false)
        );
        s.join().unwrap();
    }
}

/// What the matching conditions and "in favour of the changed" need: only
/// properties changed, and contents changed on one side only.
#[cfg(unix)]
#[test]
fn props_changes_are_told_apart() {
    for (pair, loopback) in ways("session-props") {
        use std::os::unix::fs::PermissionsExt;
        let pair = synced_pair_on(
            pair,
            &["times = true"],
            |r| {
                write(r, "props", "same", T0);
                write(r, "mixed", "same", T0);
            },
            loopback,
        );
        let chmod = |p: std::path::PathBuf| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap()
        };
        chmod(pair.a().join("props"));
        write(&pair.a(), "mixed", "a's", T0 + 10.0);
        chmod(pair.b().join("mixed"));
        settle(&[&pair.a(), &pair.b()]);
        let (mut s, _) = start(&pair, &["p", "-batch"], loopback);
        load(&mut s, &mut no_questions);
        let u = updates(&mut s, &mut no_questions);
        let (_, props) = item(&u.items, "props");
        assert!(
            props.props_only && props.favor_changed.is_none(),
            "{props:?}"
        );
        let (_, mixed) = item(&u.items, "mixed");
        assert!(!mixed.props_only, "{mixed:?}");
        assert_eq!(mixed.favor_changed, Some(SetAction::Right));
        s.join().unwrap();
    }
}

/// An interrupt stops what the session is doing, a question left
/// unanswered included (answering it later does nothing), and the
/// session goes on; once it ends, the archives are unlocked.
#[test]
fn an_interrupt_stops_a_question_unanswered() {
    let pair = Pair::new("session-interrupt");
    pair.profile("p", &[]);
    write(&pair.a(), "f", "f", T0);
    settle(&[&pair.a()]);
    let (mut s, _) = start(&pair, &["p"], false);
    load(&mut s, &mut no_questions);
    // Nothing running.
    assert!(matches!(
        ok(&mut s, Command::Interrupt, &mut no_questions),
        Event::Interrupted
    ));
    let mut held = None;
    let c = Command::Update {
        allow_partial: true,
        want_watcher: false,
        paths: None,
    };
    let (_, e) = call(&mut s, c, &mut |s, q| match q {
        // No archives: the warning.
        Question::Warning(_, r) => {
            assert!(s.send(Command::Interrupt));
            held = Some(r);
        }
        q => panic!("{q:?}"),
    });
    assert!(matches!(e, Event::Interrupted), "{e:?}");
    held.expect("asked").send(false);
    let (_, e) = call(&mut s, Command::Start, &mut no_questions);
    assert!(matches!(e, Event::Started(_)), "{e:?}");
    s.join().unwrap();
    let locks: Vec<_> = std::fs::read_dir(pair.u())
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.starts_with("lk").then_some(n)
        })
        .collect();
    assert_eq!(locks, Vec::<String>::new());
}
