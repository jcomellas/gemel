//! Differential test harness for gemel: runs a candidate implementation and
//! OCaml Unison (the reference) on identical generated replicas, and
//! compares what they print, the replicas they leave behind, the files in
//! their Unison directories, and whether the reference accepts the
//! candidate's archives.
//!
//! ```text
//! gemel-oracle --reference PATH [--candidate PATH] [--seed N] [--count K]
//!              [--work DIR] [--keep]
//! ```
//!
//! The candidate defaults to the reference, which tests the harness itself
//! (every scenario must then agree). Each implementation runs in the same
//! work directory in turn, because archive names depend on the roots'
//! absolute paths. See `tools/oracle/README.md`.

mod run;
mod scenario;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use run::{Outcome, Step};
use scenario::Scenario;

struct Args {
    reference: PathBuf,
    candidate: Option<PathBuf>,
    seed: u64,
    count: u64,
    work: Option<PathBuf>,
    keep: bool,
}

const USAGE: &str = "usage: gemel-oracle --reference PATH [--candidate PATH] [--seed N] \
                     [--count K] [--work DIR] [--keep]";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        reference: PathBuf::new(),
        candidate: None,
        seed: 0,
        count: 20,
        work: None,
        keep: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{a} needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--reference" => args.reference = value()?.into(),
            "--candidate" => args.candidate = Some(value()?.into()),
            "--seed" => args.seed = value()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--count" => args.count = value()?.parse().map_err(|e| format!("--count: {e}"))?,
            "--work" => args.work = Some(value()?.into()),
            "--keep" => args.keep = true,
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if args.reference.as_os_str().is_empty() {
        return Err(USAGE.into());
    }
    Ok(args)
}

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

fn compare_step(name: &str, r: &Step, c: &Step, out: &mut Vec<String>) {
    if r.exit != c.exit {
        out.push(format!("{name}: exit code {:?} vs {:?}", r.exit, c.exit));
    }
    for (stream, x, y) in [
        ("stdout", &r.stdout, &c.stdout),
        ("stderr", &r.stderr, &c.stderr),
    ] {
        if x != y {
            out.push(format!(
                "{name} {stream} differs at {}",
                first_difference(x, y)
            ));
        }
    }
}

fn compare(r: &Outcome, c: &Outcome) -> Vec<String> {
    let mut out = Vec::new();
    compare_step("first sync", &r.first, &c.first, &mut out);
    compare_step("second sync", &r.second, &c.second, &mut out);
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
    if r.unison_dir != c.unison_dir {
        out.push(format!(
            "Unison directory: {:?} vs {:?}",
            r.unison_dir, c.unison_dir
        ));
    }
    compare_step("recheck by the reference", &r.recheck, &c.recheck, &mut out);
    out
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
fn check_recheck(o: &Outcome) -> Option<String> {
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

fn run_scenario(args: &Args, base: &Path, seed: u64) -> Result<Vec<String>, String> {
    let scenario = Scenario::generate(seed);
    let work = base.join("work");
    let candidate = args.candidate.as_deref().unwrap_or(&args.reference);
    let io = |e: std::io::Error| format!("scenario {seed}: {e}");
    let reference = run::outcome(&args.reference, &args.reference, &scenario, &work).map_err(io)?;
    std::fs::rename(&work, base.join(format!("{seed}-reference"))).map_err(io)?;
    let candidate = run::outcome(candidate, &args.reference, &scenario, &work).map_err(io)?;
    std::fs::rename(&work, base.join(format!("{seed}-candidate"))).map_err(io)?;
    let mut problems = compare(&reference, &candidate);
    // A disagreement inside the reference itself is a harness problem; one
    // in the candidate's recheck is a real failure.
    if let Some(p) = check_recheck(&reference) {
        problems.push(format!("(reference) {p}"));
    }
    if let Some(p) = check_recheck(&candidate) {
        problems.push(format!("(candidate) {p}"));
    }
    if problems.is_empty() && !args.keep {
        for side in ["reference", "candidate"] {
            run::remove_all(&base.join(format!("{seed}-{side}"))).map_err(io)?;
        }
    } else if !problems.is_empty() {
        problems.insert(0, scenario.to_string());
    }
    Ok(problems)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let base = args.work.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "gemel-oracle-{}-{}",
            std::process::id(),
            scenario::now()
        ))
    });
    if let Err(e) = std::fs::create_dir_all(&base) {
        eprintln!("cannot create {}: {e}", base.display());
        return ExitCode::from(2);
    }
    // Unison prints canonical root paths; use them everywhere.
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    let mut failed = 0;
    for seed in args.seed..args.seed + args.count {
        match run_scenario(&args, &base, seed) {
            Ok(problems) if problems.is_empty() => println!("scenario {seed}: ok"),
            Ok(problems) => {
                failed += 1;
                println!("scenario {seed}: DIFFERS");
                for p in problems {
                    println!("  {}", p.replace('\n', "\n  "));
                }
            }
            Err(e) => {
                failed += 1;
                println!("{e}");
            }
        }
    }
    println!(
        "{} of {} scenarios agree (work directory {})",
        args.count - failed,
        args.count,
        base.display()
    );
    if failed == 0 {
        if !args.keep {
            let _ = std::fs::remove_dir(&base);
        }
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
