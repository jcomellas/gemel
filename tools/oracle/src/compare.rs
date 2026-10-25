//! Comparing two outcomes of a scenario, and checking one on its own.

use crate::run::{Outcome, Step};

/// The first line where two texts differ, with a little context.
fn first_difference(expected: &str, actual: &str) -> String {
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let i = e
        .iter()
        .zip(&a)
        .position(|(x, y)| x != y)
        .unwrap_or(e.len().min(a.len()));
    let line = |v: &[&str]| v.get(i).copied().unwrap_or("<end>").to_owned();
    format!(
        "line {}:\n      reference: {}\n      candidate: {}",
        i + 1,
        line(&e),
        line(&a)
    )
}

/// The lines of `s`, sorted.
fn sorted_lines(s: &str) -> Vec<&str> {
    let mut v: Vec<&str> = s.lines().collect();
    v.sort_unstable();
    v
}

/// Compares two runs of a sync; with `unordered`, what they print is
/// compared without its order (items ran concurrently); with `dump`, what
/// differs is shown in full.
fn compare_step(
    name: &str,
    r: &Step,
    c: &Step,
    (unordered, dump): (bool, bool),
    out: &mut Vec<String>,
) {
    if r.exit != c.exit {
        out.push(format!("{name}: exit code {:?} vs {:?}", r.exit, c.exit));
    }
    for (stream, x, y) in [
        ("stdout", &r.stdout, &c.stdout),
        ("stderr", &r.stderr, &c.stderr),
        ("server's stderr", &r.server, &c.server),
    ] {
        let differ = if unordered {
            sorted_lines(x) != sorted_lines(y)
        } else {
            x != y
        };
        if differ {
            out.push(format!(
                "{name} {stream} differs at {}",
                first_difference(x, y)
            ));
            if dump {
                out.push(format!("--- reference:\n{x}\n--- candidate:\n{y}"));
            }
        }
    }
}

/// What differs between the reference's outcome `r` and the candidate's
/// `c`, a line each; `how` is `compare_step`'s.
pub fn compare(r: &Outcome, c: &Outcome, how: (bool, bool)) -> Vec<String> {
    let mut out = Vec::new();
    compare_step("first sync", &r.first, &c.first, how, &mut out);
    compare_step("second sync", &r.second, &c.second, how, &mut out);
    compare_left(r, c, how, &mut out);
    out
}

/// [`compare`], but for what the syncs print: only their exit codes are
/// compared, for a candidate that prints nothing (a graphical interface).
/// The reference's rechecks are still compared whole, so the candidate
/// is accepted as far as the reference's outcome is ([`check_recheck`]).
pub fn compare_state(r: &Outcome, c: &Outcome) -> Vec<String> {
    let mut out = Vec::new();
    for (name, x, y) in [
        ("first sync", &r.first, &c.first),
        ("second sync", &r.second, &c.second),
    ] {
        if x.exit != y.exit {
            out.push(format!("{name}: exit code {:?} vs {:?}", x.exit, y.exit));
        }
    }
    compare_left(r, c, (false, false), &mut out);
    out
}

/// What the two outcomes left: the replicas, the Unison directories, ssh's
/// arguments, and the reference's recheck of each.
fn compare_left(r: &Outcome, c: &Outcome, how: (bool, bool), out: &mut Vec<String>) {
    for (name, x, y) in [("replica a", &r.a, &c.a), ("replica b", &r.b, &c.b)] {
        for path in x
            .keys()
            .chain(y.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let (p, q) = (x.get(path), y.get(path));
            if p != q {
                out.push(format!("{name} {path:?}: {p:?} vs {q:?}"));
            }
        }
    }
    for (name, x, y) in [
        ("Unison directory", &r.unison_dir, &c.unison_dir),
        ("server's Unison directory", &r.server_dir, &c.server_dir),
    ] {
        if x != y {
            out.push(format!("{name}: {x:?} vs {y:?}"));
        }
    }
    if r.shell_args != c.shell_args {
        out.push(format!(
            "ssh's arguments differ at {}",
            first_difference(&r.shell_args, &c.shell_args)
        ));
    }
    compare_step("recheck by the reference", &r.recheck, &c.recheck, how, out);
}

/// The `skipped: ...` lines of a sync's summary.
fn skipped(step: &Step) -> Vec<&str> {
    let mut v: Vec<&str> = step
        .stdout
        .lines()
        .chain(step.stderr.lines())
        .map(str::trim)
        .filter(|l| l.starts_with("skipped: "))
        .collect();
    v.sort_unstable();
    v
}

/// Problems with an outcome on its own: rerun on the final state, the
/// reference must see no archive problems and nothing to do beyond the
/// conflicts the second sync skipped.
pub fn check_recheck(o: &Outcome) -> Option<String> {
    let r = &o.recheck;
    let text = format!("{}{}", r.stdout, r.stderr);
    let expected = skipped(&o.second);
    let ok = !text.contains("Warning")
        && !text.contains("Fatal")
        && if expected.is_empty() {
            r.exit == Some(0) && text.contains("Nothing to do")
        } else {
            r.exit == Some(1) && skipped(r) == expected
        };
    (!ok).then(|| {
        format!(
            "the reference does not accept the final state (exit {:?}, expected skips {expected:?}):\n{text}",
            r.exit
        )
    })
}
