//! Whole syncs: update detection, reconciliation, transport and commit, as
//! the text user interface runs them in batch mode, through both dispatch
//! modes. With `GEMEL_REFERENCE_UNISON` set, OCaml Unison syncs a twin of
//! the replicas, and both end the same, with the same transport log.

mod support;

use std::cell::RefCell;
use std::path::Path as StdPath;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use gemel_base::util::{Error, FileId};
use gemel_engine::abort::Abort;
use gemel_engine::common::{self, ReconItem};
use gemel_engine::transport::{self, Task};
use gemel_engine::{recon, update, uutil};

use support::oracle::*;
use support::sync::*;
use support::*;

/// How a sync went: the items that failed (`path: message`), or the error
/// that stopped it.
type Outcome = Result<Vec<String>, Error>;

impl Setup {
    /// A whole sync, as `Uitext` runs one in batch mode: transport (all but
    /// the deletions, then the deletions) between `logStart` and
    /// `logFinish`, then the commit; or only the commit, if every item is
    /// skipped. A failed item is noted (with the roots named `A` and `B`),
    /// and the rest go on; an error that stops the run skips the commit.
    fn sync(&self) -> Outcome {
        self.sync_items().1
    }

    /// [`Setup::sync`], and the items it propagated.
    fn sync_items(&self) -> (Vec<ReconItem>, Outcome) {
        settle(&[&self.a(), &self.b()]);
        let found = self.find();
        let items = RefCell::new(Vec::new());
        let outcome = block_on(async {
            let (found_items, _, _) =
                recon::reconcile_all(&self.rpc, &self.roots, false, found).await?;
            *items.borrow_mut() = found_items.clone();
            let items = found_items;
            if items.iter().all(common::problematic) {
                update::commit_updates(&self.rpc, &self.roots).await?;
                return Ok(Vec::new());
            }
            transport::log_start(&self.rpc)?;
            let failed = RefCell::new(Vec::new());
            for deletions in [false, true] {
                transport::transport_items(
                    &self.rpc,
                    &self.roots,
                    &items,
                    |ri| common::is_deletion(ri) == deletions,
                    |i, ri| -> Task<'_, Result<(), Error>> {
                        let failed = &failed;
                        Box::pin(async move {
                            let r = transport::transport_item(
                                &self.rpc,
                                &self.roots,
                                ri,
                                FileId(i as i64),
                                &mut |_, _| Box::pin(async { true }),
                            )
                            .await;
                            match r {
                                Err(Error::Transient(m)) => {
                                    let p = String::from_utf8_lossy(ri.path1.as_bytes());
                                    let m = normalized(&format!("{p}: {m}"), self);
                                    failed.borrow_mut().push(m);
                                    Ok(())
                                }
                                r => r,
                            }
                        })
                    },
                )
                .await?;
            }
            transport::log_finish(&self.rpc)?;
            update::commit_updates(&self.rpc, &self.roots).await?;
            Ok(failed.into_inner())
        });
        (items.into_inner(), outcome)
    }

    /// The root names in log entries, and what to show instead.
    /// How the roots appear in messages, and what to show instead: as
    /// roots (`//server//...` when remote), then as paths.
    fn root_names(&self) -> [(String, &'static str); 4] {
        let r = self.roots.list();
        [
            (r[0].to_string(), "A"),
            (r[1].to_string(), "B"),
            (r[0].fspath.to_string(), "A"),
            (r[1].fspath.to_string(), "B"),
        ]
    }
}

/// The entries transport writes, from a log file, with the roots named
/// `A` and `B` and the times left out.
fn transport_log(log: &str, s: &Setup) -> Vec<String> {
    let mut out = Vec::new();
    let mut detail = false;
    for line in log.lines() {
        let keep = detail
            || [
                "[BGN] ",
                "[END] ",
                "[CONFLICT] ",
                "[ERROR] ",
                "Failed [",
                "Merge command: ",
                "Merge result (",
            ]
            .iter()
            .any(|p| line.starts_with(p));
        detail = ["[CONFLICT] ", "[ERROR] ", "Failed ["]
            .iter()
            .any(|p| line.starts_with(p));
        if line.contains(" started propagating changes at ") {
            out.push("(start)".to_owned());
        } else if line.contains(" finished propagating changes at ") {
            out.push("(finish)".to_owned());
        } else if keep {
            out.push(normalized(line, s));
        }
    }
    out
}

/// `m`, with the roots named `A` and `B`, and the hashes in temporary
/// names (which depend on the root) left out.
fn normalized(m: &str, s: &Setup) -> String {
    let mut m = m.to_owned();
    for (name, shown) in s.root_names() {
        m = m.replace(&name, shown);
    }
    let tag = ".unison.new.";
    let mut out = String::new();
    let mut rest = m.as_str();
    while let Some(i) = rest.find(tag) {
        let (head, tail) = rest.split_at(i + tag.len());
        out.push_str(head);
        let hash = tail.bytes().take_while(u8::is_ascii_hexdigit).count();
        out.push_str(if hash == 32 { "HASH" } else { "" });
        rest = &tail[if hash == 32 { hash } else { 0 }..];
    }
    out + rest
}

fn read_log(path: &StdPath) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

const NOTHING_TO_DO: &str = "Nothing to do: replicas have not changed since last sync.";

/// The summary of an OCaml run: what it would do and what it did, without
/// the times.
fn summary(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| {
            if l.contains(" items will be synced, ")
                || ["  skipped: ", "  failed: ", "  partially transferred: "]
                    .iter()
                    .any(|p| l.starts_with(p))
            {
                Some(l.to_owned())
            } else if l.starts_with("Synchronization ") {
                let (word, rest) = l.split_once(" at ")?;
                Some(format!("{word} {}", &rest[rest.find('(')?..]))
            } else {
                None
            }
        })
        .collect()
}

/// The changes of a scenario, made to both replicas of a sync.
type Change = fn(&StdPath, &StdPath);

/// Syncs replicas built with `build`, then changed with `change`. With OCaml
/// Unison, a twin is synced by it, and the replicas and the transport log
/// must be the same; then each runs OCaml once more, to the same effect.
fn sync_like_ocaml(what: &str, prefs: &[&str], build: fn(&StdPath), change: Change) -> Outcome {
    sync_like_ocaml_via(&Via::ALL, what, prefs, build, change)
}

/// The transport log without the order of its `[END]` lines, which OCaml
/// interleaves with the next items' when a root is remote, where it runs
/// items concurrently; gemel runs them one at a time until phase 5c of
/// milestone 3.
fn ends_unordered(log: &[String]) -> (Vec<&String>, Vec<&String>) {
    let (mut ends, rest): (Vec<_>, Vec<_>) = log.iter().partition(|l| l.starts_with("[END] "));
    ends.sort();
    (rest, ends)
}

/// [`sync_like_ocaml`], with only some of the ways.
fn sync_like_ocaml_via(
    ways: &[Via],
    what: &str,
    prefs: &[&str],
    build: fn(&StdPath),
    change: Change,
) -> Outcome {
    let mut outcome = None;
    for &via in ways {
        let dir = TempDir::new("transport");
        let s = synced(&dir, via, prefs, build);
        change(&s.a(), &s.b());
        let ours_log = s.base.join("u/unison.log");
        let _ = std::fs::remove_file(&ours_log);
        let r = s.sync();
        let ours = transport_log(&read_log(&ours_log), &s);
        if let Some(unison) = std::env::var_os("GEMEL_REFERENCE_UNISON") {
            let twin_dir = TempDir::new("transport-ocaml");
            let twin = synced(&twin_dir, via, prefs, build);
            change(&twin.a(), &twin.b());
            settle(&[&twin.a(), &twin.b()]);
            let theirs_log = twin.base.join("ocaml.log");
            let mut flags = vec![
                "-log=true".to_owned(),
                format!("-logfile={}", theirs_log.display()),
            ];
            flags.extend(flags_of(prefs));
            let all = ocaml_run(&unison, &twin, &flags);
            let theirs = transport_log(&read_log(&theirs_log), &twin);
            assert_eq!(listings(&s), listings(&twin), "{what}: {r:?}\n{all}");
            if via == Via::Remote {
                assert_eq!(
                    ends_unordered(&ours),
                    ends_unordered(&theirs),
                    "{what}: {r:?}\n{all}"
                );
            } else {
                assert_eq!(ours, theirs, "{what}: {r:?}\n{all}");
            }
            // OCaml then finds nothing to do on both (so the archives agree
            // with the replicas), or the same to do, to the same effect.
            let theirs_next = ocaml_sync(&unison, &twin, &flags_of(prefs));
            if theirs_next.contains(NOTHING_TO_DO) {
                ocaml_agrees(&s, prefs);
            } else {
                let ours_next = ocaml_sync(&unison, &s, &flags_of(prefs));
                assert_eq!(
                    summary(&ours_next),
                    summary(&theirs_next),
                    "{what}:\n{ours_next}\n{theirs_next}"
                );
                assert_eq!(
                    listings(&s),
                    listings(&twin),
                    "{what}:\n{ours_next}\n{theirs_next}"
                );
            }
        }
        // A remote root's errors are worded as a transfer's, not a local
        // copy's, as in OCaml: it is held to OCaml's own run above.
        if via == Via::Remote {
            continue;
        }
        if let Some(o) = &outcome {
            assert_eq!(o, &r, "{what}: the dispatch modes differ");
        }
        outcome = Some(r);
    }
    outcome.unwrap()
}

/// Copies, updates, props, deletions (last), and a conflict skipped.
#[test]
fn a_sync_propagates_everything_like_ocaml() {
    let change: Change = |a, b| {
        write(a, "new-a", "new in a", T0 + 20.0);
        write(a, "t/u/v", "a new tree", T0 + 20.0);
        write(b, "src/main.c", "int main(void) { return 1; }", T0 + 30.0);
        std::fs::remove_file(a.join("src/deep/x")).unwrap();
        std::fs::remove_dir(b.join("empty")).unwrap();
        write(a, "README", "a's version", T0 + 20.0);
        write(b, "README", "b's version", T0 + 30.0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(a.join("src"), PermissionsExt::from_mode(0o700)).unwrap();
        }
    };
    let failed = sync_like_ocaml("everything", &[], build_tree, change).unwrap();
    assert_eq!(failed, Vec::<String>::new());
}

/// Moves are renames where the moved contents are unchanged in the other
/// replica (each of `doMove`'s cases), and copies and deletions otherwise.
#[test]
fn moves_propagate_like_ocaml() {
    let moves = ["moves-experimental = true"];
    let scenarios: [(&str, &[&str], Change); 9] = [
        ("a file and a directory", &[], |a, b| {
            std::fs::rename(a.join("README"), a.join("READ-ME")).unwrap();
            std::fs::rename(b.join("src"), b.join("source")).unwrap();
        }),
        ("to a name sorted first", &[], |a, _| {
            std::fs::rename(a.join("README"), a.join("AAA")).unwrap();
        }),
        ("reverted", &["force = b"], |a, _| {
            std::fs::rename(a.join("README"), a.join("ZZZ")).unwrap();
        }),
        (
            "reverted, from a name sorted first",
            &["force = b"],
            |a, _| {
                std::fs::rename(a.join("README"), a.join("AAA")).unwrap();
            },
        ),
        ("changed in the other replica", &[], |a, b| {
            std::fs::rename(a.join("README"), a.join("READ-ME")).unwrap();
            write(b, "README", "b's version", T0 + 30.0);
        }),
        (
            "changed in the other replica, and forced",
            &["force = a"],
            |a, b| {
                std::fs::rename(a.join("src"), a.join("source")).unwrap();
                write(b, "src/main.c", "b's version", T0 + 30.0);
            },
        ),
        // A move over something else is made on its new path.
        ("over a directory", &[], |a, _| {
            std::fs::remove_dir(a.join("empty")).unwrap();
            std::fs::rename(a.join("README"), a.join("empty")).unwrap();
        }),
        (
            "over a directory the other replica deleted, reverted",
            &["force = b"],
            |a, b| {
                std::fs::remove_dir(a.join("empty")).unwrap();
                std::fs::rename(a.join("README"), a.join("empty")).unwrap();
                std::fs::remove_dir(b.join("empty")).unwrap();
            },
        ),
        ("over a file the other replica deleted", &[], |a, b| {
            std::fs::remove_file(b.join("src/main.c")).unwrap();
            std::fs::rename(a.join("README"), a.join("src/main.c")).unwrap();
        }),
    ];
    for (what, prefs, change) in scenarios {
        let prefs: Vec<&str> = moves.iter().chain(prefs).copied().collect();
        sync_like_ocaml(what, &prefs, build_tree, change).unwrap();
    }
}

/// A merge runs the merge program.
#[cfg(unix)]
#[test]
fn merges_propagate_like_ocaml() {
    let prefs = ["merge = Name README -> cat CURRENT1 CURRENT2 > NEW"];
    let ways = Via::ALL;
    let failed = sync_like_ocaml_via(&ways, "both changed", &prefs, build_tree, |a, b| {
        write(a, "README", "a's version\n", T0 + 20.0);
        write(b, "README", "b's version\n", T0 + 30.0);
        write(a, "new-a", "new in a", T0 + 20.0);
    })
    .unwrap();
    assert_eq!(failed, Vec::<String>::new());
    // Reconciliation proposes no merge when a side is deleted (it is a
    // conflict, skipped), but the user may ask for one.
    sync_like_ocaml_via(&ways, "one deleted", &prefs, build_tree, |a, b| {
        write(a, "README", "a's version\n", T0 + 20.0);
        std::fs::remove_file(b.join("README")).unwrap();
    })
    .unwrap();
}

/// Merging a file with nothing fails.
#[test]
fn only_files_are_merged() {
    for via in Via::ALL {
        let dir = TempDir::new("transport-merge");
        let s = synced(&dir, via, &[], build_tree);
        write(&s.a(), "README", "a's version", T0 + 20.0);
        std::fs::remove_file(s.b().join("README")).unwrap();
        settle(&[&s.a(), &s.b()]);
        let found = s.find();
        let (mut items, _, _) =
            block_on(recon::reconcile_all(&s.rpc, &s.roots, false, found)).unwrap();
        let [ri] = &mut items[..] else {
            panic!("{items:?}")
        };
        let common::Replicas::Different(d) = &mut ri.replicas else {
            panic!("{ri:?}")
        };
        assert!(d.direction.is_conflict(), "{d:?}");
        d.direction = common::Direction::Merge;
        let r = block_on(transport::transport_item(
            &s.rpc,
            &s.roots,
            ri,
            FileId(0),
            &mut |_, _| Box::pin(async { true }),
        ));
        assert_eq!(
            r,
            Err(Error::Transient("Can only merge two existing files".into()))
        );
    }
}

#[cfg(unix)]
fn euid_is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| o.stdout.starts_with(b"0\n"))
}

/// A copy into a directory that can't be written fails, is logged, and the
/// run goes on.
#[cfg(unix)]
#[test]
fn a_failed_item_is_logged_like_ocaml() {
    use std::os::unix::fs::PermissionsExt;
    if euid_is_root() {
        eprintln!("skipped: running as root");
        return;
    }
    let build: fn(&StdPath) = |root| {
        build_tree(root);
        write(root, "ro/old", "old", T0);
        set_mtime(&root.join("ro"), T0 + 10.0);
        std::fs::set_permissions(root.join("ro"), PermissionsExt::from_mode(0o555)).unwrap();
    };
    let change: Change = |a, b| {
        let a_ro = a.join("ro");
        std::fs::set_permissions(&a_ro, PermissionsExt::from_mode(0o755)).unwrap();
        write(a, "ro/new", "new", T0 + 20.0);
        std::fs::set_permissions(&a_ro, PermissionsExt::from_mode(0o555)).unwrap();
        write(b, "new-b", "new in b", T0 + 30.0);
    };
    let failed = sync_like_ocaml("unwritable", &[], build, change).unwrap();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].starts_with("ro/new: "), "{failed:?}");
}

/// A fatal error stops the run: nothing more starts, everything is
/// aborted, and nothing is committed.
#[test]
fn a_fatal_error_stops_the_run() {
    for via in Via::ALL {
        let dir = TempDir::new("transport-fatal");
        let s = synced(&dir, via, &[], build_tree);
        write(&s.a(), "README", "a's version", T0 + 20.0);
        std::fs::remove_dir_all(s.a().join("src")).unwrap();
        write(&s.a(), "src", "a file", T0 + 20.0);
        write(&s.a(), "t", "new", T0 + 20.0);
        // Replacing a directory writes the commit log, which mustn't exist.
        std::fs::write(s.base.join("u/DANGER.README"), "stopped").unwrap();
        let before = archives(&s);
        let err = s.sync().unwrap_err();
        assert!(matches!(err, Error::Fatal(_)), "{err:?}");
        assert!(s.rpc.client_host().state::<Abort>().borrow().is_all());
        assert_eq!(read(&s.b().join("README")), "a's version");
        assert!(s.b().join("src/main.c").exists());
        assert!(!s.b().join("t").exists());
        let log = read_log(&s.base.join("u/unison.log"));
        let err = normalized(&err.to_string(), &s);
        let mut err = err.lines();
        assert_eq!(
            transport_log(&log, &s),
            [
                "(start)".to_owned(),
                "[BGN] Updating file README from A to B".to_owned(),
                "[END] Updating file README".to_owned(),
                "[BGN] Copying src from A to B".to_owned(),
                format!("Failed [src]: {}", err.next().unwrap()),
                err.next().unwrap_or_default().to_owned(),
            ],
            "{log}"
        );
        assert_eq!(archives(&s), before, "committed");
    }
}

/// The archive files, and their contents.
fn archives(s: &Setup) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(s.base.join("u"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().starts_with("ar"))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    assert!(!out.is_empty());
    out
}

/// Once everything is aborted, nothing more starts.
#[test]
fn aborting_everything_stops_dispensing() {
    let dir = TempDir::new("transport-abort");
    let s = setup(&dir, Via::Direct);
    let items = [0, 1, 2, 3];
    let ran = RefCell::new(Vec::new());
    block_on(transport::transport_items(
        &s.rpc,
        &s.roots,
        &items,
        |_| true,
        |_, &n| -> Task<'_, Result<(), Error>> {
            let (ran, rpc) = (&ran, &s.rpc);
            Box::pin(async move {
                ran.borrow_mut().push(n);
                if n == 1 {
                    rpc.client_host().state::<Abort>().borrow_mut().all();
                }
                Ok(())
            })
        },
    ))
    .unwrap();
    assert_eq!(ran.into_inner(), [0, 1]);
    // And an aborted run starts nothing.
    let ran = RefCell::new(Vec::new());
    block_on(transport::transport_items(
        &s.rpc,
        &s.roots,
        &items,
        |_| true,
        |_, &n| -> Task<'_, Result<(), Error>> {
            let ran = &ran;
            Box::pin(async move {
                ran.borrow_mut().push(n);
                Ok(())
            })
        },
    ))
    .unwrap();
    assert_eq!(ran.into_inner(), Vec::<i32>::new());
}

/// What the progress printer got: the item, the bytes and the tag.
type Reports = Arc<Mutex<Vec<(FileId, i64, &'static str)>>>;

/// Installs a progress printer on the client that records what it gets.
fn record_progress(s: &Setup) -> Reports {
    let seen = Reports::default();
    let r = seen.clone();
    uutil::set_progress_printer(
        s.rpc.client_host(),
        Some(Arc::new(move |id, n, tag| {
            r.lock().unwrap().push((id, n, tag))
        })),
    );
    seen
}

/// Each item's progress adds up to what propagating it transfers
/// (`riLength`): a copy as it goes, a move all at once.
#[test]
fn progress_adds_up_to_what_each_item_transfers() {
    for via in Via::ALL {
        let dir = TempDir::new("transport-progress");
        let s = synced(&dir, via, &["moves-experimental = true"], build_tree);
        write(&s.a(), "big", &"x".repeat(300_000), T0 + 20.0);
        write(&s.a(), "new/f1", "one", T0 + 20.0);
        write(&s.a(), "new/f2", "two two", T0 + 20.0);
        write(&s.b(), "README", "b's own version", T0 + 20.0);
        std::fs::rename(s.a().join("src/main.c"), s.a().join("src/moved.c")).unwrap();
        std::fs::remove_file(s.b().join("src/deep/x")).unwrap();
        let seen = record_progress(&s);
        let (items, outcome) = s.sync_items();
        assert_eq!(outcome.unwrap(), Vec::<String>::new(), "{via}");
        let mut by_item: BTreeMap<i64, (i64, Vec<&str>)> = BTreeMap::new();
        for (id, n, tag) in seen.lock().unwrap().iter() {
            let e = by_item.entry(id.0).or_default();
            e.0 += n;
            if !e.1.contains(tag) {
                e.1.push(tag);
            }
        }
        let shown: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, ri)| {
                let (n, tags) = by_item.remove(&(i as i64)).unwrap_or_default();
                let p = String::from_utf8_lossy(ri.path1.as_bytes()).into_owned();
                (p, n, common::ri_length(ri), tags)
            })
            .collect();
        for (p, n, len, _) in &shown {
            assert_eq!(n, len, "{p} ({via}): {shown:?}");
        }
        let tags = |p: &str| &shown.iter().find(|s| s.0 == p).expect(p).3;
        // Copied here, or transferred between hosts.
        let copied = if via == Via::Remote { "r" } else { "l" };
        assert_eq!(tags("big"), &[copied]);
        assert_eq!(tags("new"), &[copied]);
        assert_eq!(tags("README"), &[copied]);
        // A move is shown at its original path.
        assert_eq!(tags("src/main.c"), &["mov"]);
        // A deletion transfers nothing, and reports nothing.
        assert_eq!(tags("src/deep/x"), &[] as &[&str]);
        assert!(by_item.is_empty(), "{by_item:?}");
    }
}

/// Aborting everything from another thread (a signal handler's, say)
/// reaches the run, through the flag the copies read: nothing more starts.
/// The next run starts afresh.
#[test]
fn aborting_from_another_thread_stops_the_run() {
    for via in Via::ALL {
        let dir = TempDir::new("transport-abort-flag");
        let s = synced(&dir, via, &[], build_tree);
        for n in ["a1", "a2", "a3"] {
            write(&s.a(), n, n, T0 + 20.0);
        }
        // Taken before the run, whose start resets it in place.
        let flag = s.rpc.client_host().state::<Abort>().borrow().all_flag();
        uutil::set_progress_printer(
            s.rpc.client_host(),
            Some(Arc::new(move |_, _, _| {
                let flag = flag.clone();
                std::thread::spawn(move || flag.set()).join().unwrap();
            })),
        );
        assert_eq!(s.sync().unwrap(), Vec::<String>::new());
        let there = |n: &str| s.b().join(n).exists();
        assert_eq!(
            (there("a1"), there("a2"), there("a3")),
            (true, false, false)
        );
        uutil::set_progress_printer(s.rpc.client_host(), None);
        assert_eq!(s.sync().unwrap(), Vec::<String>::new());
        assert_eq!((there("a1"), there("a2"), there("a3")), (true, true, true));
    }
}

/// A merge logs its command and result, and says what it made of the
/// outputs on stderr, unless log entries aren't shown there.
#[cfg(unix)]
#[test]
fn a_merge_is_logged_and_explained() {
    for to_stderr in [true, false] {
        let dir = TempDir::new("transport-merge-log");
        let prefs = ["merge = Name README -> cat CURRENT1 CURRENT2 > NEW"];
        let s = synced(&dir, Via::Direct, &prefs, build_tree);
        write(&s.a(), "README", "a's version\n", T0 + 20.0);
        write(&s.b(), "README", "b's version\n", T0 + 30.0);
        s.output.take();
        s.rpc
            .client_host()
            .state::<gemel_base::trace::Trace>()
            .borrow_mut()
            .send_log_msgs_to_stderr = to_stderr;
        assert_eq!(s.sync().unwrap(), Vec::<String>::new());
        let out = s.output.take();
        assert_eq!(out.contains("One output detected \n"), to_stderr, "{out}");
        assert_eq!(out.contains("Merge command: cat "), to_stderr, "{out}");
        let log = read_log(&s.base.join("u/unison.log"));
        assert!(log.contains("Merge command: cat "), "{log}");
        assert!(log.contains("Merge result (exited (0)):\n"), "{log}");
        assert!(!log.contains("One output detected"), "{log}");
    }
}

/// Update detection and reconciliation show their status, and with
/// `timers`, how long the scan and each copy took. The path being scanned
/// is shown now and then, by the host that scans: a server shows nothing.
#[test]
fn the_scan_and_reconciliation_show_their_status() {
    for via in Via::ALL {
        let dir = TempDir::new("transport-status");
        let s = synced(&dir, via, &[], build_tree);
        for i in 0..40 {
            write(&s.a(), &format!("many/f{i:02}"), "f", T0 + 20.0);
        }
        let shown = Arc::new(Mutex::new(Vec::<String>::new()));
        let d = shown.clone();
        s.rpc
            .client_host()
            .state::<gemel_base::trace::Trace>()
            .borrow_mut()
            .set_displayer(Some(Box::new(move |m: &str| {
                d.lock().unwrap().push(m.to_owned())
            })));
        s.set_prefs(|p| p.set(&gemel_base::trace::TIMERS, true));
        let scanned = Arc::new(Mutex::new(Vec::<String>::new()));
        let sc = scanned.clone();
        uutil::set_update_status_printer(
            s.rpc.client_host(),
            Some(Arc::new(move |p: &str| {
                sc.lock().unwrap().push(p.to_owned())
            })),
        );
        assert_eq!(s.sync().unwrap(), Vec::<String>::new());
        let shown = shown.lock().unwrap().clone();
        let at = |pred: &dyn Fn(&str) -> bool| {
            shown
                .iter()
                .position(|m| pred(m))
                .unwrap_or_else(|| panic!("{shown:?}"))
        };
        let collecting = at(&|m| m == "Collecting changes...");
        let collected = at(&|m| m.starts_with("Collecting changes (") && m.ends_with(" seconds)"));
        let done = at(&|m| m == " ");
        let reconciling = at(&|m| m == "Reconciling changes ");
        let copying = at(&|m| m == "Transmitting file...");
        assert!(
            collecting < collected
                && collected < done
                && done < reconciling
                && reconciling < copying,
            "{shown:?}"
        );
        let mut scanned = scanned.lock().unwrap().clone();
        // With a server, once the client's own scan has ended: an empty
        // status, then that it waits for the server (OCaml's waiting
        // action).
        let waiting = shown
            .iter()
            .any(|m| m.contains("Waiting for changes from server"));
        assert_eq!(waiting, via == Via::Remote, "{shown:?}");
        if via == Via::Remote {
            assert_eq!(scanned.pop().as_deref(), Some(""), "{scanned:?}");
        }
        if via == Via::Loopback {
            assert_eq!(scanned, Vec::<String>::new());
        } else {
            assert!(!scanned.is_empty());
            assert!(scanned.iter().all(|p| !p.is_empty()), "{scanned:?}");
        }
    }
}
