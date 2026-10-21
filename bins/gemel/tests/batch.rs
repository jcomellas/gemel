//! The `gemel` program in batch mode, run as a user runs it. With
//! `GEMEL_REFERENCE_UNISON` naming OCaml Unison, a twin of each pair of
//! replicas is synced by `unison <args> -ui text`, and both runs must end
//! the same: the same exit code, the same output (as a terminal shows it),
//! the same log, and the same replicas.

#[path = "../../../crates/gemel-engine/tests/support/mod.rs"]
mod support;
#[path = "../../../crates/gemel-ui-api/tests/ui_support/mod.rs"]
mod ui_support;

use std::ffi::OsStr;
use std::path::Path as StdPath;

use gemel_base::util::{MY_NAME_AND_VERSION, capitalize_ascii};
use support::sync::T0;
use support::{listing, settle, write};
use ui_support::Pair;

/// How a program run ended.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    code: Option<i32>,
    stdout: Vec<String>,
    stderr: Vec<String>,
}

/// What a terminal shows of `bytes`: each `\r` goes back to the start of
/// the line, and what follows overwrites it; spaces at the ends of lines
/// are left out.
fn screen(bytes: &[u8]) -> Vec<String> {
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

/// Runs `prog` with `args` in `pair`'s directory, with `pair`'s Unison
/// directory, and nothing in the environment that changes the output.
fn run(prog: &OsStr, pair: &Pair, args: &[&str]) -> Run {
    let out = std::process::Command::new(prog)
        .args(args)
        .current_dir(pair.base())
        .env("UNISON", pair.u())
        .env("HOME", pair.base())
        .env_remove("EMACS")
        .env_remove("NO_COLOR")
        .env_remove("TERM")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    Run {
        code: out.status.code(),
        stdout: screen(&out.stdout),
        stderr: screen(&out.stderr),
    }
}

fn gemel(pair: &Pair, args: &[&str]) -> Run {
    run(OsStr::new(env!("CARGO_BIN_EXE_gemel")), pair, args)
}

/// OCaml Unison, with the text interface; `None` without
/// `GEMEL_REFERENCE_UNISON`.
fn ocaml(pair: &Pair, args: &[&str]) -> Option<Run> {
    let unison = std::env::var_os("GEMEL_REFERENCE_UNISON")?;
    let mut args = args.to_vec();
    args.extend(["-ui", "text"]);
    Some(run(&unison, pair, &args))
}

/// OCaml's name and version, as its messages start with them.
fn ocaml_banner() -> Option<String> {
    let unison = std::env::var_os("GEMEL_REFERENCE_UNISON")?;
    let out = std::process::Command::new(unison)
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
fn normalized(line: &str, pair: &Pair, banners: &[String]) -> (String, Option<u32>) {
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
fn lines(ls: &[String], pair: &Pair, banners: &[String]) -> (Vec<String>, Vec<u32>) {
    let mut out = Vec::new();
    let mut clocks = Vec::new();
    for l in ls {
        let (l, c) = normalized(l, pair, banners);
        out.push(l);
        clocks.extend(c);
    }
    (out, clocks)
}

fn log(pair: &Pair) -> Vec<String> {
    pair.log().lines().map(str::to_owned).collect()
}

fn banners() -> Vec<String> {
    let mut b = vec![capitalize_ascii(MY_NAME_AND_VERSION)];
    b.extend(ocaml_banner());
    b
}

/// Runs gemel on `g` and OCaml on `o` with `args`; with OCaml, they must
/// end the same. Gemel's run.
fn same(g: &Pair, o: &Pair, args: &[&str]) -> Run {
    settle(&[&g.a(), &g.b(), &o.a(), &o.b()]);
    let ours = gemel(g, args);
    let Some(theirs) = ocaml(o, args) else {
        return ours;
    };
    let b = banners();
    let what = format!("{args:?}\n--- gemel:\n{ours:#?}\n--- OCaml:\n{theirs:#?}");
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
    ours
}

/// Twin pairs, made with `build(root, side)`, and the profile `p` with
/// `lines` in both.
fn twins(tag: &str, lines: &[&str], build: fn(&StdPath, &str)) -> (Pair, Pair) {
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

/// A first sync, then changes on both sides with a conflict and a
/// deletion, then nothing to do: from the command line's roots.
#[test]
fn roots_on_the_command_line() {
    let (g, o) = twins("batch-roots", &[], |r, side| {
        if side == "a" {
            write(r, "f", "one", T0);
            write(r, "sub/g", "two", T0 + 1.0);
        } else {
            write(r, "h", "three", T0 + 2.0);
        }
    });
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

/// From a profile, with `-terse`, then only a conflict (nothing to
/// propagate).
#[test]
fn a_profile_with_terse() {
    let (g, o) = twins("batch-terse", &["times = true"], |r, side| {
        write(r, "same", "same", T0);
        write(r, &format!("only-{side}"), side, T0 + 1.0);
    });
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

/// `-silent` prints nothing but errors.
#[test]
fn silent() {
    let (g, o) = twins("batch-silent", &["times = true"], |r, side| {
        write(r, &format!("only-{side}"), side, T0);
    });
    let r = same(&g, &o, &["p", "-silent"]);
    assert_eq!(r.code, Some(0));
}

/// Deleting everything in one replica is dangerous: batch mode aborts.
#[test]
fn a_dangerous_deletion_aborts() {
    let (g, o) = twins("batch-dangerous", &["times = true"], |r, _| {
        write(r, "f", "f", T0);
        write(r, "g", "g", T0);
    });
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

/// `-version`, and options that don't exist (OCaml then lists every
/// option: only the first line is compared).
#[test]
fn version_and_unknown_options() {
    let pair = Pair::new("batch-version");
    let r = gemel(&pair, &["-version"]);
    assert_eq!(r.code, Some(0));
    assert_eq!(
        r.stdout,
        [format!("gemel version {}", env!("CARGO_PKG_VERSION"))]
    );
    let r = gemel(&pair, &["-nosuchoption"]);
    assert_eq!(r.code, Some(2));
    let first = r.stderr[0]
        .rsplit_once('/')
        .map_or(&r.stderr[0][..], |(_, l)| l);
    let first = first.rsplit_once('\\').map_or(first, |(_, l)| l);
    if let Some(theirs) = ocaml(&pair, &["-nosuchoption"]) {
        assert_eq!(r.code, theirs.code);
        let t = &theirs.stderr[0];
        let t = t.rsplit_once('/').map_or(&t[..], |(_, l)| l);
        assert_eq!(
            first.split_once(':').map(|x| x.1),
            t.split_once(':').map(|x| x.1)
        );
    } else {
        assert!(
            first.ends_with(": unknown option `-nosuchoption'."),
            "{first}"
        );
    }
}
