//! What the program's tests share: running `gemel` (and OCaml's
//! `unison -ui text`) as a user runs it, with keys on stdin, and checking
//! that both runs end the same.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path as StdPath;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gemel_base::util::{MY_NAME_AND_VERSION, capitalize_ascii};

use crate::support::{listing, settle};
use crate::ui_support::Pair;

/// How a program run ended.
#[derive(Debug, PartialEq, Eq)]
pub struct Run {
    pub code: Option<i32>,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
}

/// What a terminal shows of `bytes`: each `\r` goes back to the start of
/// the line, and what follows overwrites it; spaces at the ends of lines
/// are left out.
pub fn screen(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|l| {
            let mut shown: Vec<char> = Vec::new();
            let mut col = 0;
            for c in l.chars() {
                if c == '\r' {
                    col = 0;
                } else {
                    if col < shown.len() {
                        shown[col] = c;
                    } else {
                        shown.push(c);
                    }
                    col += 1;
                }
            }
            shown.into_iter().collect::<String>().trim_end().to_owned()
        })
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// The command for `prog` with `args` in `pair`'s directory, with
/// `pair`'s Unison directory, and nothing in the environment that changes
/// the output.
pub fn command(prog: &OsStr, pair: &Pair, args: &[&str]) -> Command {
    let mut c = Command::new(prog);
    c.args(args)
        .current_dir(pair.base())
        .env("UNISON", pair.u())
        .env("HOME", pair.base())
        .env_remove("EMACS")
        .env_remove("NO_COLOR")
        .env_remove("TERM")
        .env_remove("PAGER");
    // On Windows raw mode depends on the console (`CONIN$`), not on
    // stdin: keys on a pipe are read a line at a time only with
    // `dumbtty`, which `EMACS` sets.
    if cfg!(windows) {
        c.env("EMACS", "t");
    }
    c
}

/// Runs `prog` with `args`, and `keys` on stdin (nothing without).
pub fn run(prog: &OsStr, pair: &Pair, args: &[&str], keys: Option<&[u8]>) -> Run {
    let mut c = command(prog, pair, args);
    c.stdin(if keys.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(keys) = keys {
        let mut stdin = child.stdin.take().unwrap();
        let keys = keys.to_vec();
        // The program may stop before it has read them all.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&keys);
        });
    }
    let out = child.wait_with_output().unwrap();
    Run {
        code: out.status.code(),
        stdout: screen(&out.stdout),
        stderr: screen(&out.stderr),
    }
}

pub fn gemel_path() -> &'static OsStr {
    OsStr::new(env!("CARGO_BIN_EXE_gemel"))
}

pub fn gemel(pair: &Pair, args: &[&str]) -> Run {
    run(gemel_path(), pair, args, None)
}

/// OCaml Unison, if `GEMEL_REFERENCE_UNISON` names it.
pub fn ocaml_path() -> Option<std::ffi::OsString> {
    std::env::var_os("GEMEL_REFERENCE_UNISON")
}

/// OCaml Unison, with the text interface; `None` without
/// `GEMEL_REFERENCE_UNISON`.
pub fn ocaml(pair: &Pair, args: &[&str]) -> Option<Run> {
    let unison = ocaml_path()?;
    Some(run(&unison, pair, &ocaml_args(args), None))
}

/// OCaml's arguments: the text interface.
pub fn ocaml_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut args = args.to_vec();
    args.extend(["-ui", "text"]);
    args
}

/// OCaml's name and version, as its messages start with them.
fn ocaml_banner() -> Option<String> {
    let out = Command::new(ocaml_path()?)
        .arg("-version")
        .output()
        .unwrap();
    let v = String::from_utf8_lossy(&out.stdout);
    let v = v.trim().strip_prefix("unison version ")?;
    Some(format!("Unison {v}"))
}

/// A line with the pair's directory shown as `S`, the programs' names and
/// versions as `NAME`, and times as `T`. The summary's time is kept apart
/// (`clock`), to be compared within a few seconds.
pub fn normalized(line: &str, pair: &Pair, banners: &[String]) -> (String, Option<u32>) {
    let canonical = std::fs::canonicalize(pair.base()).unwrap();
    let mut l = line.to_owned();
    for n in [canonical.to_str().unwrap(), pair.base().to_str().unwrap()] {
        l = l.replace(n, "S");
    }
    for b in banners {
        if let Some(rest) = l.strip_prefix(b.as_str()) {
            l = format!("NAME{rest}");
        }
    }
    // The usage names the program.
    if [
        "Usage: ",
        "    or ",
        "For a",
        "For other",
        "To start interactive",
        "unison was invoked",
    ]
    .iter()
    .any(|p| l.starts_with(p))
    {
        l = l.replace("unison", "gemel");
    }
    // So do the help and the messages of the interactive mode.
    for (theirs, ours) in [
        ("unison's ", "gemel's "),
        ("exit unison", "exit gemel"),
        ("restart unison", "restart gemel"),
    ] {
        l = l.replace(theirs, ours);
    }
    if l.contains(" started propagating changes at ") {
        return ("(start)".to_owned(), None);
    }
    if l.contains(" finished propagating changes at ") {
        return ("(finish)".to_owned(), None);
    }
    if l.contains(" log started at ") {
        return ("(log started)".to_owned(), None);
    }
    if let Some(rest) = l.strip_prefix("Synchronization ") {
        if let Some((word, rest)) = rest.split_once(" at ") {
            let (t, rest) = rest.split_at(8.min(rest.len()));
            let secs = t
                .split(':')
                .map(|n| n.parse::<u32>().ok())
                .collect::<Option<Vec<_>>>()
                .filter(|v| v.len() == 3)
                .map(|v| v[0] * 3600 + v[1] * 60 + v[2]);
            return (format!("Synchronization {word} at T{rest}"), secs);
        }
    }
    (l, None)
}

/// The lines, normalized, and the summaries' times.
pub fn lines(ls: &[String], pair: &Pair, banners: &[String]) -> (Vec<String>, Vec<u32>) {
    let mut out = Vec::new();
    let mut clocks = Vec::new();
    for l in ls {
        let (l, c) = normalized(l, pair, banners);
        out.push(l);
        clocks.extend(c);
    }
    (out, clocks)
}

pub fn log(pair: &Pair) -> Vec<String> {
    pair.log().lines().map(str::to_owned).collect()
}

pub fn banners() -> Vec<String> {
    let mut b = vec![capitalize_ascii(MY_NAME_AND_VERSION)];
    b.extend(ocaml_banner());
    b
}

/// Both runs ended the same, with the same log and replicas.
pub fn compare(g: &Pair, o: &Pair, what: &str, ours: &Run, theirs: &Run) {
    let b = banners();
    let what = format!("{what}\n--- gemel:\n{ours:#?}\n--- OCaml:\n{theirs:#?}");
    assert_eq!(ours.code, theirs.code, "{what}");
    let (out1, _) = lines(&ours.stdout, g, &b);
    let (out2, _) = lines(&theirs.stdout, o, &b);
    assert_eq!(out1, out2, "stdout: {what}");
    let (err1, c1) = lines(&ours.stderr, g, &b);
    let (err2, c2) = lines(&theirs.stderr, o, &b);
    assert_eq!(err1, err2, "stderr: {what}");
    // Local times, within a few seconds of each other.
    assert_eq!(c1.len(), c2.len(), "{what}");
    for (a, b) in c1.iter().zip(&c2) {
        let d = (*a as i64 - *b as i64).rem_euclid(86_400);
        assert!(d.min(86_400 - d) <= 10, "{a} and {b}: {what}");
    }
    let (log1, _) = lines(&log(g), g, &b);
    let (log2, _) = lines(&log(o), o, &b);
    assert_eq!(log1, log2, "log: {what}");
    assert_eq!(listing(&g.a()), listing(&o.a()), "{what}");
    assert_eq!(listing(&g.b()), listing(&o.b()), "{what}");
}

/// Runs gemel on `g` and OCaml on `o` with `args`; with OCaml, they must
/// end the same. Gemel's run.
pub fn same(g: &Pair, o: &Pair, args: &[&str]) -> Run {
    same_keys(g, o, args, None)
}

/// [`same`], with `keys` typed on stdin.
pub fn same_keys(g: &Pair, o: &Pair, args: &[&str], keys: Option<&[u8]>) -> Run {
    settle(&[&g.a(), &g.b(), &o.a(), &o.b()]);
    let ours = run(gemel_path(), g, args, keys);
    let Some(unison) = ocaml_path() else {
        return ours;
    };
    let theirs = run(&unison, o, &ocaml_args(args), keys);
    compare(g, o, &format!("{args:?} {keys:?}"), &ours, &theirs);
    ours
}

/// Twin pairs, made with `build(root, side)`, and the profile `p` with
/// `lines` in both.
pub fn twins(tag: &str, lines: &[&str], build: fn(&StdPath, &str)) -> (Pair, Pair) {
    let (g, o) = (
        Pair::new(&format!("{tag}-g")),
        Pair::new(&format!("{tag}-o")),
    );
    for p in [&g, &o] {
        p.profile("p", lines);
        build(&p.a(), "a");
        build(&p.b(), "b");
    }
    (g, o)
}

/// A program running, driven step by step: keys are typed once what it
/// prints shows it is waiting for them, and signals sent at given points.
pub struct Driven {
    child: std::process::Child,
    stdin: Option<Box<dyn Write + Send>>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

/// Starts a program to drive.
pub type Start = fn(&OsStr, &Pair, &[&str]) -> Driven;

fn collect(
    mut r: impl Read + Send + 'static,
    into: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut b = [0u8; 4096];
        loop {
            match r.read(&mut b) {
                Ok(0) | Err(_) => return,
                Ok(n) => into.lock().unwrap().extend_from_slice(&b[..n]),
            }
        }
    })
}

impl Driven {
    /// With pipes for stdin, stdout and stderr.
    pub fn start(prog: &OsStr, pair: &Pair, args: &[&str]) -> Driven {
        let mut child = command(prog, pair, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (stdin, stdout, stderr) = (
            child.stdin.take().unwrap(),
            child.stdout.take().unwrap(),
            child.stderr.take().unwrap(),
        );
        Driven::new(child, Box::new(stdin), Box::new(stdout), Box::new(stderr))
    }

    /// `child`, written to with `stdin`, its output read from `stdout`
    /// and `stderr`.
    pub fn new(
        child: std::process::Child,
        stdin: Box<dyn Write + Send>,
        stdout: Box<dyn Read + Send>,
        stderr: Box<dyn Read + Send>,
    ) -> Driven {
        let (out, err) = (Arc::default(), Arc::default());
        let readers = vec![
            collect(stdout, Arc::clone(&out)),
            collect(stderr, Arc::clone(&err)),
        ];
        Driven {
            stdin: Some(stdin),
            child,
            stdout: out,
            stderr: err,
            readers,
        }
    }

    /// Waits until stdout shows `s` for the `n`th time.
    pub fn wait_for(&self, s: &str, n: usize) {
        let t0 = Instant::now();
        loop {
            let out = String::from_utf8_lossy(&self.stdout.lock().unwrap()).into_owned();
            if out.matches(s).count() >= n {
                return;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "waiting for {s:?} ({n}) in:\n{out}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Types `keys`; nothing if the program has ended.
    pub fn type_keys(&mut self, keys: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        let _ = stdin.write_all(keys).and_then(|()| stdin.flush());
    }

    /// Sends `sig` (`INT`, `TERM`...) with the shell's `kill`.
    pub fn signal(&self, sig: &str) {
        let ok = Command::new("sh")
            .arg("-c")
            .arg(format!("kill -{sig} {}", self.child.id()))
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    /// Waits a little: for a signal to be handled, say.
    pub fn pause(&self) {
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Waits for the program to end, giving it the end of the input only
    /// if it hasn't after a while.
    pub fn finish(mut self) -> Run {
        let t0 = Instant::now();
        while self.child.try_wait().unwrap().is_none() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(self.stdin.take());
        let status = self.child.wait().unwrap();
        for r in self.readers.drain(..) {
            r.join().unwrap();
        }
        Run {
            code: status.code(),
            stdout: screen(&self.stdout.lock().unwrap()),
            stderr: screen(&self.stderr.lock().unwrap()),
        }
    }
}

/// Runs `drive` on gemel with `g`, and on OCaml with `o` if there is one;
/// both runs must end the same. Gemel's run.
pub fn same_driven(g: &Pair, o: &Pair, args: &[&str], drive: fn(&mut Driven)) -> Run {
    same_driven_by(Driven::start, g, o, args, drive)
}

/// [`same_driven`], started with `start`.
pub fn same_driven_by(
    start: Start,
    g: &Pair,
    o: &Pair,
    args: &[&str],
    drive: fn(&mut Driven),
) -> Run {
    same_driven_if(true, start, g, o, args, drive)
}

/// OCaml's runtime is OCaml 5 or later. OCaml 4.14's handles a signal
/// that comes during a read of stdin only once the read has returned;
/// gemel, as OCaml 5's and as `uitext` expects, at once.
pub fn ocaml_handles_signals_at_once() -> bool {
    ocaml_path().is_some_and(|u| {
        let out = Command::new(u).arg("-version").output().unwrap();
        let v = String::from_utf8_lossy(&out.stdout);
        v.split("(ocaml ")
            .nth(1)
            .and_then(|r| r.split('.').next())
            .and_then(|m| m.parse::<u32>().ok())
            .is_some_and(|m| m >= 5)
    })
}

/// [`same_driven_by`] for signals: compared with OCaml only if its
/// runtime handles them at once ([`ocaml_handles_signals_at_once`]).
pub fn same_signals(
    start: Start,
    g: &Pair,
    o: &Pair,
    args: &[&str],
    drive: fn(&mut Driven),
) -> Run {
    same_driven_if(ocaml_handles_signals_at_once(), start, g, o, args, drive)
}

fn same_driven_if(
    compared: bool,
    start: Start,
    g: &Pair,
    o: &Pair,
    args: &[&str],
    drive: fn(&mut Driven),
) -> Run {
    settle(&[&g.a(), &g.b(), &o.a(), &o.b()]);
    let mut ours = start(gemel_path(), g, args);
    drive(&mut ours);
    let ours = ours.finish();
    let Some(unison) = ocaml_path().filter(|_| compared) else {
        return ours;
    };
    let mut theirs = start(&unison, o, &ocaml_args(args));
    drive(&mut theirs);
    let theirs = theirs.finish();
    compare(g, o, &format!("{args:?}"), &ours, &theirs);
    ours
}
