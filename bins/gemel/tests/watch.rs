//! `-repeat watch`: the `gemel` program syncs whenever the file watcher
//! reports changes, until it is asked to stop between syncs (`SIGUSR2`).
//! With `GEMEL_REFERENCE_UNISON` and `GEMEL_REFERENCE_FSMONITOR`, OCaml's
//! `unison -ui text` runs the same scenario with its own watcher on a twin
//! pair of replicas.
//!
//! How many syncs a scenario takes depends on when the watcher reports
//! what (each sync's own writes are reported too), so the runs are
//! compared on what they do: the actions taken, the replicas left, and the
//! exit code, not on everything they print.
//!
//! gemel finds `gemel-fsmonitor` next to itself; `cargo test --workspace`
//! builds it, and the tests say so if it is missing. OCaml looks next to
//! itself, then on `PATH`, where the tests put its watcher as
//! `unison-fsmonitor`.

#![cfg(unix)]

mod common;
#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{Driven, Run, banners, command, gemel_path, normalized, ocaml_args, ocaml_path};
use support::sync::T0;
use support::write;
use ui_support::Pair;

const TIMEOUT: Duration = Duration::from_secs(30);

fn until(what: &str, ok: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < TIMEOUT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// gemel's watcher, next to it.
fn gemel_watcher() -> PathBuf {
    let p = Path::new(gemel_path()).with_file_name("gemel-fsmonitor");
    assert!(
        p.exists(),
        "{} is missing: build it with `cargo build -p gemel-fsmonitor`",
        p.display()
    );
    p
}

/// A program to run, how, and with what watcher.
struct Prog {
    exe: OsString,
    args: Vec<String>,
    /// `PATH` for it.
    path: OsString,
}

/// A directory of `pair`'s, made empty.
fn dir(pair: &Pair, name: &str) -> PathBuf {
    let d = pair.base().join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An executable script that records its process id in `pids`, then runs
/// `real`.
fn recording(script: &Path, real: &Path, pids: &Path) {
    std::fs::write(
        script,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nexec '{}'\n",
            pids.display(),
            real.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// gemel, as built (its watcher next to it), or copied to a directory of
/// its own with `watcher` there (`None`: no watcher at all).
fn gemel(pair: &Pair, args: &[&str], watcher: Option<Option<&Path>>) -> Prog {
    let empty = dir(pair, "empty-path");
    let exe = match watcher {
        None => {
            gemel_watcher();
            gemel_path().to_owned()
        }
        Some(w) => {
            let d = dir(pair, "gemel-bin");
            let exe = d.join("gemel");
            std::fs::copy(gemel_path(), &exe).unwrap();
            if let Some(w) = w {
                std::fs::copy(w, d.join("gemel-fsmonitor")).unwrap();
            }
            exe.into_os_string()
        }
    };
    Prog {
        exe,
        args: args.iter().map(|s| s.to_string()).collect(),
        path: empty.into_os_string(),
    }
}

/// OCaml's watcher, if there is one to compare with.
fn ocaml_watcher() -> Option<PathBuf> {
    ocaml_path()?;
    std::env::var_os("GEMEL_REFERENCE_FSMONITOR").map(PathBuf::from)
}

/// OCaml, with `watcher` first on `PATH` as `unison-fsmonitor`, if any
/// (`None` when there is no OCaml to compare with).
fn ocaml(pair: &Pair, args: &[&str], watcher: Option<&Path>) -> Option<Prog> {
    let exe = ocaml_path()?;
    let d = dir(pair, "ocaml-bin");
    if let Some(w) = watcher {
        std::fs::copy(w, d.join("unison-fsmonitor")).unwrap();
    }
    Some(Prog {
        exe,
        args: ocaml_args(args).iter().map(|s| s.to_string()).collect(),
        path: d.into_os_string(),
    })
}

fn start(prog: &Prog, pair: &Pair) -> Driven {
    let args: Vec<&str> = prog.args.iter().map(String::as_str).collect();
    let spawn = || {
        command(&prog.exe, pair, &args)
            .env("PATH", &prog.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    // A program just copied can be busy for a moment on Linux: a process
    // another test forks meanwhile holds it open until it runs its own.
    let mut tries = 0;
    let mut child = loop {
        match spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            r => break r.unwrap(),
        }
    };
    let (stdin, stdout, stderr) = (
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        child.stderr.take().unwrap(),
    );
    Driven::new(child, Box::new(stdin), Box::new(stdout), Box::new(stderr))
}

/// What a run did: its exit code, the actions it took (`[BGN]` lines), and
/// the replicas it left.
#[derive(Debug, PartialEq)]
struct Did {
    code: Option<i32>,
    actions: Vec<String>,
    a: Vec<(String, String)>,
    b: Vec<(String, String)>,
}

fn did(run: &Run, pair: &Pair) -> Did {
    let banners = banners();
    let actions = run
        .stdout
        .iter()
        .chain(&run.stderr)
        .filter(|l| l.contains("[BGN]"))
        .map(|l| normalized(&l[l.find("[BGN]").unwrap()..], pair, &banners).0)
        .collect();
    Did {
        code: run.code,
        actions,
        a: support::listing(&pair.a()),
        b: support::listing(&pair.b()),
    }
}

/// Runs `scenario` with gemel on `g`, and with OCaml on `o` if there is one
/// to compare with; both must do the same.
fn same(
    g: &Pair,
    o: &Pair,
    gemel: Prog,
    ocaml: Option<Prog>,
    scenario: impl Fn(&Pair, &mut Driven),
) -> Did {
    let run = |pair: &Pair, prog: &Prog| {
        let mut p = start(prog, pair);
        scenario(pair, &mut p);
        let r = p.finish();
        let d = did(&r, pair);
        (d, r)
    };
    let (ours, ours_run) = run(g, &gemel);
    if let Some(prog) = ocaml {
        let (theirs, theirs_run) = run(o, &prog);
        assert_eq!(
            ours, theirs,
            "gemel's run:\n{ours_run:#?}\nOCaml's:\n{theirs_run:#?}"
        );
    }
    ours
}

fn replicas(tag: &str) -> (Pair, Pair) {
    common::twins(tag, &["times = true"], |r, side| {
        if side == "a" {
            write(r, "first", "1", T0);
        }
    })
}

/// New files and deletions propagate as the watcher reports them; asked to
/// stop, the program does so with the last sync's exit code. Also with root
/// `b` on a server, which runs its own watcher for it.
#[test]
fn syncs_whenever_the_watcher_reports_changes() {
    let each = common::twins_each("watch-sync", &["times = true"], |r, side| {
        if side == "a" {
            write(r, "first", "1", T0);
        }
    });
    for (g, o) in each {
        syncs_whenever_the_watcher_reports_changes_on(&g, &o);
    }
}

fn syncs_whenever_the_watcher_reports_changes_on(g: &Pair, o: &Pair) {
    let args = ["p", "-repeat", "watch"];
    let d = same(
        g,
        o,
        gemel(g, &args, None),
        ocaml_watcher().and_then(|w| ocaml(o, &args, Some(&w))),
        |pair, p| {
            until("the first sync", || pair.b().join("first").exists());
            std::thread::sleep(Duration::from_millis(1500));
            write(&pair.a(), "second", "2", T0 + 10.0);
            until("the new file to propagate", || {
                pair.b().join("second").exists()
            });
            std::fs::remove_file(pair.b().join("first")).unwrap();
            until("the deletion to propagate", || {
                !pair.a().join("first").exists()
            });
            p.signal("USR2");
        },
    );
    assert_eq!(d.code, Some(0), "{d:#?}");
    assert_eq!(d.actions.len(), 3, "{d:#?}");
}

/// Without a watcher, the first sync is a whole one, then waiting for
/// changes is an error. (OCaml must not have `fsmonitor.py` next to it
/// either: its old watcher would run that script, which gemel doesn't
/// port. OCaml's source tree has one beside the binary it builds.)
#[test]
fn a_watcher_that_is_not_found() {
    let (g, o) = replicas("watch-none");
    let args = ["p", "-repeat", "watch"];
    let run = |prog: Prog, pair: &Pair| {
        let p = start(&prog, pair);
        p.finish()
    };
    let ours = run(gemel(&g, &args, Some(None)), &g);
    assert_eq!(ours.code, Some(3), "{ours:#?}");
    assert_eq!(
        ours.stderr.last().map(String::as_str),
        Some("Error: No file monitoring helper program found"),
        "{ours:#?}"
    );
    assert!(g.b().join("first").exists());
    if let Some(prog) = ocaml(&o, &args, None) {
        let theirs = run(prog, &o);
        assert_eq!(
            (theirs.code, theirs.stderr.last()),
            (ours.code, ours.stderr.last()),
            "{theirs:#?}"
        );
    }
}

/// Waits until the program waits for changes: the protocol trace ends
/// with its `WAIT`s, and stays so for a second.
fn waiting(p: &Driven) {
    until("a wait for changes", || {
        let ends_waiting = |t: &str| {
            t.trim_end()
                .lines()
                .last()
                .is_some_and(|l| l.contains("<< WAIT"))
        };
        let before = p.stderr_text();
        if !ends_waiting(&before) {
            return false;
        }
        std::thread::sleep(Duration::from_secs(1));
        p.stderr_text() == before
    });
}

/// A watcher that goes while the program waits for changes ends the wait,
/// and the next sync scans everything and starts another, as in OCaml.
/// (One that goes at another time stops the program, also as in OCaml:
/// "File monitoring helper program not running".)
#[test]
fn a_watcher_that_dies_is_started_again() {
    let (g, o) = replicas("watch-dies");
    let args = ["p", "-repeat", "watch", "-debug", "fswatch+"];
    let wrap = |pair: &Pair, real: &Path, name: &str| {
        let script = pair.base().join(format!("{name}-script"));
        recording(&script, real, &pair.base().join("pids"));
        script
    };
    let ours = {
        let w = wrap(&g, &gemel_watcher(), "gemel");
        gemel(&g, &args, Some(Some(&w)))
    };
    let theirs = ocaml_watcher().and_then(|w| {
        let script = wrap(&o, &w, "ocaml");
        ocaml(&o, &args, Some(&script))
    });
    let pids = |pair: &Pair| -> Vec<String> {
        std::fs::read_to_string(pair.base().join("pids"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    let d = same(&g, &o, ours, theirs, |pair, p| {
        until("the first sync", || pair.b().join("first").exists());
        until("the watcher", || pids(pair).len() == 1);
        waiting(p);
        let pid = pids(pair)[0].clone();
        let ok = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill {pid}"))
            .status()
            .unwrap()
            .success();
        assert!(ok);
        until("another watcher", || pids(pair).len() == 2);
        std::thread::sleep(Duration::from_millis(1500));
        write(&pair.a(), "second", "2", T0 + 10.0);
        until("the new file to propagate", || {
            pair.b().join("second").exists()
        });
        p.signal("USR2");
    });
    assert_eq!(d.code, Some(0), "{d:#?}");
}

/// After a restart, both replicas are watched again: a change to the one
/// scanned second propagates too. (OCaml takes that one for watched by the
/// new watcher, which was never told of it: gemel alone is checked.)
#[test]
fn a_new_watcher_watches_every_replica() {
    let (g, _) = replicas("watch-restart");
    let args = ["p", "-repeat", "watch", "-debug", "fswatch+"];
    let pids_file = g.base().join("pids");
    let script = g.base().join("gemel-script");
    recording(&script, &gemel_watcher(), &pids_file);
    let p = start(&gemel(&g, &args, Some(Some(&script))), &g);
    let pids = || -> Vec<String> {
        std::fs::read_to_string(&pids_file)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    until("the first sync", || g.b().join("first").exists());
    until("the watcher", || pids().len() == 1);
    waiting(&p);
    let ok = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill {}", pids()[0]))
        .status()
        .unwrap()
        .success();
    assert!(ok);
    until("another watcher", || pids().len() == 2);
    waiting(&p);
    write(&g.a(), "from-a", "a", T0 + 10.0);
    write(&g.b(), "from-b", "b", T0 + 11.0);
    until("both changes to propagate", || {
        g.b().join("from-a").exists() && g.a().join("from-b").exists()
    });
    p.signal("USR2");
    let r = p.finish();
    assert_eq!(r.code, Some(0), "{r:#?}");
}
