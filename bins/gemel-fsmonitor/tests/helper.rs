//! The helper driven over its stdin and stdout, on real directories.
//!
//! When `GEMEL_REFERENCE_FSMONITOR` names OCaml's `unison-fsmonitor`
//! (Linux, inotify), each scenario also runs with it, and the two must
//! report the same. The changes are compared as sets: OCaml orders them
//! by a hash table.
//!
//! What a system reports differs, so the checks do too:
//! - inotify reports each change as it happened, so on Linux the changes
//!   reported must be exactly those made;
//! - FSEvents (macOS) merges the events on a path, so a file created then
//!   deleted may still be reported;
//! - Windows reports a directory as modified when its entries change, which
//!   hides the changes below it.
//!
//! So elsewhere the changes reported only have to cover those made: each
//! path reported, or one of its directories.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use gemel_compat::fsmonitor_protocol::{Response, quote};

const TIMEOUT: Duration = Duration::from_secs(20);
const EXACT: bool = cfg!(target_os = "linux");

/// A running helper.
struct Monitor {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Monitor {
    fn start(bin: &Path) -> Monitor {
        let mut child = Command::new(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{}: {e}", bin.display()));
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for l in out.lines() {
                if tx.send(l.unwrap()).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        let mut m = Monitor {
            child,
            stdin,
            lines,
        };
        assert_eq!(m.line(), "VERSION 1");
        m.send("VERSION 1");
        m
    }

    fn send(&mut self, l: &str) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(format!("{l}\n").as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn line(&self) -> String {
        self.lines
            .recv_timeout(TIMEOUT)
            .expect("an answer from the watcher")
    }

    fn ack(&mut self, l: &str) {
        self.send(l);
        assert_eq!(self.line(), "OK", "after {l}");
    }

    /// `START` on `root` (from `path`), `DIR` for each directory, `DONE`.
    fn scan(&mut self, root: &Path, path: &str, dirs: &[&str]) {
        self.ack(&format!("START h {} {}", q(&protocol_path(root)), q(path)));
        for d in dirs {
            self.ack(&format!("DIR {}", q(d)));
        }
        self.send("DONE");
        // `DONE` has no answer. Wait until the watcher has had it, so that
        // the changes the test makes next come after the scan: during a
        // scan, a creation counts as a modification.
        self.send("CHANGES h");
        while self.line() != "DONE" {}
    }

    /// Waits until the watcher says replica `h` has changed.
    fn wait(&mut self) {
        self.send("WAIT h");
        assert_eq!(self.line(), "CHANGES h");
    }

    fn changes(&mut self) -> BTreeSet<String> {
        self.send("CHANGES h");
        let mut set = BTreeSet::new();
        loop {
            match Response::parse(self.line().as_bytes()).unwrap() {
                Response::Recursive(p) => {
                    // The watcher joins names with the system's separator.
                    set.insert(String::from_utf8(p).unwrap().replace('\\', "/"));
                }
                Response::Done => return set,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// The changes reported once the watcher says there are some. A change
    /// is reported once it is half a second old, so changes made in a burst
    /// can come in several reports when the machine is slow; and FSEvents
    /// may deliver a burst of events in several batches. So this asks again
    /// until the changes reported include (on Linux) or cover `changed`.
    fn settle(&mut self, changed: &[&str]) -> BTreeSet<String> {
        self.wait();
        let mut reported = self.changes();
        let all = |r: &BTreeSet<String>| {
            if EXACT {
                changed.iter().all(|c| r.contains(*c))
            } else {
                covers(r, changed)
            }
        };
        while !all(&reported) {
            self.wait();
            reported.extend(self.changes());
        }
        check(&reported, changed);
        reported
    }

    /// Closes stdin: the exit code, and the lines not read yet, up to the
    /// end of the watcher's output. The child may have gone before the
    /// reader has passed its last lines on.
    fn finish(mut self) -> (Option<i32>, Vec<String>) {
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        let mut rest = Vec::new();
        loop {
            match self.lines.recv_timeout(TIMEOUT) {
                Ok(l) => rest.push(l),
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("the watcher's output didn't end"),
            }
        }
        (status.code(), rest)
    }
}

fn q(s: &str) -> String {
    String::from_utf8(quote(s.as_bytes())).unwrap()
}

/// A root as Unison names it: canonical, without Windows' `\\?\`.
fn protocol_path(p: &Path) -> String {
    let s = p.to_str().unwrap().to_owned();
    s.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(s)
}

/// A fresh directory, canonical.
fn replica(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gemel-fsmonitor-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    PathBuf::from(protocol_path(&std::fs::canonicalize(&dir).unwrap()))
}

/// Each path changed is reported, or one of its directories.
fn covers(reported: &BTreeSet<String>, changed: &[&str]) -> bool {
    changed.iter().all(|c| {
        let mut p = Some(Path::new(c));
        while let Some(x) = p {
            if reported.contains(x.to_str().unwrap()) {
                return true;
            }
            p = x.parent();
        }
        reported.contains("")
    })
}

fn check(reported: &BTreeSet<String>, changed: &[&str]) {
    if EXACT {
        let changed: BTreeSet<String> = changed.iter().map(|s| s.to_string()).collect();
        assert_eq!(*reported, changed);
    } else {
        assert!(
            covers(reported, changed),
            "{reported:?} does not cover {changed:?}"
        );
    }
}

/// Runs a scenario with gemel's helper, and with OCaml's if there is one;
/// both must give the same.
fn with_each<T: PartialEq + std::fmt::Debug>(scenario: impl Fn(&Path) -> T) {
    let ours = scenario(Path::new(env!("CARGO_BIN_EXE_gemel-fsmonitor")));
    if let Some(theirs) = std::env::var_os("GEMEL_REFERENCE_FSMONITOR") {
        assert_eq!(
            scenario(Path::new(&theirs)),
            ours,
            "OCaml's watcher differs"
        );
    }
}

fn write(p: &Path) {
    std::fs::write(p, b"x").unwrap();
}

#[test]
fn changes_are_reported_once_they_settle() {
    with_each(|bin| {
        let r = replica("settle");
        std::fs::create_dir_all(r.join("a/b")).unwrap();
        let mut m = Monitor::start(bin);
        m.scan(&r, "", &["", "a", "a/b"]);
        write(&r.join("top"));
        write(&r.join("a/f"));
        std::fs::create_dir(r.join("a/d")).unwrap();
        write(&r.join("a/b/g"));
        let reported = m.settle(&["top", "a/f", "a/d", "a/b/g"]);
        m.finish();
        reported
    });
}

#[test]
fn a_file_created_then_deleted_is_not_a_change() {
    with_each(|bin| {
        let r = replica("cancel");
        std::fs::create_dir(r.join("a")).unwrap();
        let mut m = Monitor::start(bin);
        m.scan(&r, "", &["", "a"]);
        write(&r.join("a/brief"));
        std::fs::remove_file(r.join("a/brief")).unwrap();
        write(&r.join("a/kept"));
        let reported = m.settle(&["a/kept"]);
        if EXACT {
            assert!(!reported.contains("a/brief"));
        }
        m.finish();
        reported
    });
}

#[test]
fn deletions_and_renames_are_changes() {
    with_each(|bin| {
        let r = replica("delete");
        std::fs::create_dir_all(r.join("a/sub")).unwrap();
        write(&r.join("a/old"));
        write(&r.join("a/gone"));
        let mut m = Monitor::start(bin);
        m.scan(&r, "", &["", "a", "a/sub"]);
        std::fs::remove_file(r.join("a/gone")).unwrap();
        std::fs::rename(r.join("a/old"), r.join("a/new")).unwrap();
        std::fs::remove_dir(r.join("a/sub")).unwrap();
        let reported = m.settle(&["a/gone", "a/old", "a/new", "a/sub"]);
        m.finish();
        reported
    });
}

#[test]
fn directories_not_scanned_again_are_not_watched() {
    with_each(|bin| {
        let r = replica("forget");
        std::fs::create_dir_all(r.join("a/b")).unwrap();
        let mut m = Monitor::start(bin);
        m.scan(&r, "", &["", "a", "a/b"]);
        m.scan(&r, "", &["", "a"]);
        write(&r.join("a/b/f"));
        write(&r.join("a/f"));
        // On Windows, the root's watch sees a/b/f, which counts as a
        // change to a.
        let reported = m.settle(if cfg!(windows) { &["a"] } else { &["a/f"] });
        assert!(!reported.contains("a/b/f"), "{reported:?}");
        m.finish();
        reported
    });
}

#[test]
fn a_directory_deleted_then_created_again_is_watched_again() {
    with_each(|bin| {
        let r = replica("recreate");
        std::fs::create_dir_all(r.join("a/sub")).unwrap();
        let mut m = Monitor::start(bin);
        m.scan(&r, "", &["", "a", "a/sub"]);
        std::fs::remove_dir(r.join("a/sub")).unwrap();
        std::fs::create_dir(r.join("a/sub")).unwrap();
        let first = m.settle(&["a/sub"]);
        // Unison scans it again, and the new directory must be watched.
        m.scan(&r, "", &["", "a", "a/sub"]);
        write(&r.join("a/sub/new"));
        let second = m.settle(&["a/sub/new"]);
        m.finish();
        (first, second)
    });
}

#[test]
fn reset_forgets_a_replica() {
    with_each(|bin| {
        let r = replica("reset");
        let mut m = Monitor::start(bin);
        m.scan(&r, "", &[""]);
        write(&r.join("f"));
        // Settled, but forgotten.
        std::thread::sleep(Duration::from_millis(1500));
        m.send("RESET h");
        let reported = m.changes();
        assert_eq!(reported, BTreeSet::new());
        m.finish()
    });
}

#[test]
fn a_root_that_does_not_exist_is_an_error() {
    with_each(|bin| {
        let r = replica("missing").join("nowhere");
        let mut m = Monitor::start(bin);
        m.send(&format!("START h {} ", q(&protocol_path(&r))));
        let (code, lines) = m.finish();
        let expected = format!(
            "ERROR {}",
            q(&format!("Path '{}' does not exist", protocol_path(&r)))
        );
        assert_eq!((code, lines.as_slice()), (Some(1), &[expected][..]));
        code
    });
}

#[test]
fn unknown_commands_are_errors_and_the_end_of_input_stops_it() {
    with_each(|bin| {
        let mut m = Monitor::start(bin);
        m.send("NOPE");
        let stopped = m.finish();
        assert_eq!(
            stopped,
            (Some(1), vec!["ERROR unknown%20command%20'NOPE'".to_owned()])
        );
        let m = Monitor::start(bin);
        let ended = m.finish();
        assert_eq!(ended, (Some(0), vec![]));
        (stopped, ended)
    });
}
