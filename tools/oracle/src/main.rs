//! Differential test harness for gemel: runs a candidate implementation and
//! OCaml Unison (the reference) on identical generated replicas, and
//! compares what they print, the replicas they leave behind, the files in
//! their Unison directories, and whether the reference accepts the
//! candidate's archives.
//!
//! ```text
//! gemel-oracle --reference PATH [--candidate PATH] [--seed N] [--count K]
//!              [--flavour unix|windows] [--record FILE] [--work DIR] [--keep]
//!              [--remote ssh|socket] [--reference-server PATH] [--candidate-server PATH]
//!              [--concurrent [--ordered]] [--old-server] [--client-args ARGS] [--dump]
//!              [--ssh-config FILE] [--xattrs [--refused]]
//! gemel-oracle --replay FILE --candidate PATH [--candidate-server PATH] [--work DIR] [--keep]
//! ```
//!
//! The candidate defaults to the reference, which tests the harness itself
//! (every scenario must then agree). With `--remote`, replica `b` is on a
//! server, which is each side's client unless `--*-server` says otherwise. Each implementation runs in the same
//! work directory in turn, because archive names depend on the roots'
//! absolute paths. `--concurrent` lets items run concurrently, and then
//! what the syncs print is compared without its order, unless `--ordered`
//! says to keep it. `--client-args` gives the clients more arguments
//! (split at spaces), and `--dump` prints what the syncs printed in full
//! where it differs, for tracing (`--client-args '-debug remote+'`). `--old-server`
//! starts ssh servers without `__new-rpc-mode`, so that a 2.52 or 2.53
//! server starts in its compatibility mode. `--ssh-config` goes through a
//! real ssh and sshd with that client configuration, for host `server`
//! (`scripts/start-sshd.sh`). `--xattrs` sets and removes extended
//! attributes in the scenarios, and synchronises them; with `--refused`,
//! every sync must refuse them instead, as with a server that has no
//! xattr support. `--record` saves the reference's outcomes, and
//! `--replay` compares a candidate with them where there is no reference
//! (Windows). See `tools/oracle/README.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gemel_oracle::compare::{check_recheck, compare};
use gemel_oracle::record::{self, Recording};
use gemel_oracle::run::{self, Outcome, Pairing, Remote};
use gemel_oracle::scenario::{self, Flavour, Scenario};
use gemel_oracle::shim;

struct Args {
    reference: PathBuf,
    candidate: Option<PathBuf>,
    seed: u64,
    count: u64,
    flavour: Flavour,
    record: Option<PathBuf>,
    replay: Option<PathBuf>,
    work: Option<PathBuf>,
    keep: bool,
    remote: Option<Remote>,
    reference_server: Option<PathBuf>,
    candidate_server: Option<PathBuf>,
    concurrent: bool,
    ordered: bool,
    old_server: bool,
    client_args: Vec<String>,
    dump: bool,
    ssh_config: Option<PathBuf>,
    xattrs: bool,
    refused: bool,
}

impl Args {
    fn how(&self) -> run::How {
        run::How {
            concurrent: self.concurrent,
            client_args: self.client_args.clone(),
            old_server: self.old_server,
            ssh_config: self.ssh_config.clone(),
            harness: std::env::current_exe().unwrap_or_default(),
            xattrs: self.xattrs,
        }
    }

    fn reference(&self) -> Pairing {
        Pairing {
            client: self.reference.clone(),
            server: self
                .reference_server
                .clone()
                .unwrap_or_else(|| self.reference.clone()),
        }
    }

    /// The candidate, which defaults to the reference.
    fn candidate(&self) -> Pairing {
        match &self.candidate {
            None => Pairing {
                client: self.reference.clone(),
                server: self
                    .candidate_server
                    .clone()
                    .unwrap_or_else(|| self.reference().server),
            },
            Some(c) => Pairing {
                client: c.clone(),
                server: self.candidate_server.clone().unwrap_or_else(|| c.clone()),
            },
        }
    }
}

const USAGE: &str = "usage: gemel-oracle --reference PATH [--candidate PATH] [--seed N] \
                     [--count K] [--flavour unix|windows] [--record FILE] [--work DIR] [--keep]\n                    \
                     [--remote ssh|socket] [--reference-server PATH] [--candidate-server PATH] \
                     [--concurrent [--ordered]] [--old-server]\n                    \
                     [--client-args ARGS] [--dump] [--ssh-config FILE] [--xattrs [--refused]]\n   \
                     or: gemel-oracle --replay FILE --candidate PATH [--candidate-server PATH] \
                     [--work DIR] [--keep]";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        reference: PathBuf::new(),
        candidate: None,
        seed: 0,
        count: 20,
        flavour: Flavour::host(),
        record: None,
        replay: None,
        work: None,
        keep: false,
        remote: None,
        reference_server: None,
        candidate_server: None,
        concurrent: false,
        ordered: false,
        old_server: false,
        client_args: Vec::new(),
        dump: false,
        ssh_config: None,
        xattrs: false,
        refused: false,
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
            "--flavour" => {
                let v = value()?;
                args.flavour =
                    Flavour::parse(&v).ok_or_else(|| format!("--flavour: unknown {v:?}"))?;
            }
            "--record" => args.record = Some(value()?.into()),
            "--replay" => args.replay = Some(value()?.into()),
            "--work" => args.work = Some(value()?.into()),
            "--keep" => args.keep = true,
            "--remote" => {
                let v = value()?;
                args.remote =
                    Some(Remote::parse(&v).ok_or_else(|| format!("--remote: unknown {v:?}"))?);
            }
            "--reference-server" => args.reference_server = Some(value()?.into()),
            "--candidate-server" => args.candidate_server = Some(value()?.into()),
            "--concurrent" => args.concurrent = true,
            "--ordered" => args.ordered = true,
            "--client-args" => {
                args.client_args = value()?
                    .split(' ')
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "--dump" => args.dump = true,
            "--old-server" => args.old_server = true,
            "--ssh-config" => args.ssh_config = Some(value()?.into()),
            "--xattrs" => args.xattrs = true,
            "--refused" => args.refused = true,
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    // A replay's recording says whether the root is remote.
    let ok = if args.replay.is_some() {
        args.candidate.is_some()
            && args.reference.as_os_str().is_empty()
            && args.record.is_none()
            && args.remote.is_none()
            && args.reference_server.is_none()
            && !args.concurrent
            && !args.old_server
            && args.client_args.is_empty()
            && args.ssh_config.is_none()
            && !args.xattrs
    } else {
        !args.reference.as_os_str().is_empty()
            && !(args.concurrent && args.record.is_some())
            && !(args.ordered && !args.concurrent)
            && (args.ssh_config.is_none()
                || (args.remote == Some(Remote::Ssh) && args.record.is_none() && cfg!(unix)))
            // Recordings don't say whether scenarios have xattrs, and
            // Windows has none.
            && !(args.xattrs && (args.record.is_some() || args.flavour == Flavour::Windows))
    } && (args.xattrs || !args.refused);
    if !ok {
        return Err(USAGE.into());
    }
    // A socket server runs in the work directory, where a relative path
    // to a program no longer leads to it.
    let absolute = |p: &mut PathBuf| -> Result<(), String> {
        if !p.as_os_str().is_empty() {
            *p = std::path::absolute(&*p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(())
    };
    absolute(&mut args.reference)?;
    for p in [
        &mut args.candidate,
        &mut args.reference_server,
        &mut args.candidate_server,
        &mut args.ssh_config,
    ]
    .into_iter()
    .flatten()
    {
        absolute(p)?;
    }
    Ok(args)
}

/// What one scenario found, and the reference's outcome, to record.
struct Ran {
    problems: Vec<String>,
    reference: Option<Outcome>,
    /// Whether the replicas either side left have any of the scenarios'
    /// extended attributes.
    xattrs: bool,
}

/// What a peer without xattr support makes a client say (`Props`'s
/// feature validation).
const REFUSAL: &str = "preference) but the server does not support this.";

/// With `--refused`, problems with an outcome on its own: every sync must
/// stop with the refusal.
fn check_refused(o: &Outcome) -> Vec<String> {
    [
        ("first sync", &o.first),
        ("second sync", &o.second),
        ("recheck", &o.recheck),
    ]
    .into_iter()
    .filter(|(_, s)| s.exit == Some(0) || !format!("{}{}", s.stdout, s.stderr).contains(REFUSAL))
    .map(|(name, s)| {
        format!(
            "the {name} does not refuse xattrs (exit {:?}):\n{}{}",
            s.exit, s.stdout, s.stderr
        )
    })
    .collect()
}

/// Runs a scenario with the reference and the candidate, or, given a
/// recorded outcome, with the candidate alone. With no reference to
/// recheck its final state, the candidate rechecks its own.
fn run_scenario(
    args: &Args,
    base: &Path,
    seed: u64,
    (flavour, remote): (Flavour, Option<Remote>),
    recorded: Option<&Outcome>,
) -> Result<Ran, String> {
    let scenario = Scenario::generate_with(seed, flavour, args.xattrs);
    let work = base.join("work");
    let candidate = args.candidate();
    let io = |e: std::io::Error| format!("scenario {seed}: {e}");
    let (reference, rechecker) = match recorded {
        Some(o) => (o.clone(), candidate.clone()),
        None => {
            let r = args.reference();
            let o = run::outcome(&r, &r, remote, &args.how(), &scenario, &work).map_err(io)?;
            std::fs::rename(&work, base.join(format!("{seed}-reference"))).map_err(io)?;
            (o, r)
        }
    };
    let candidate = run::outcome(
        &candidate,
        &rechecker,
        remote,
        &args.how(),
        &scenario,
        &work,
    )
    .map_err(io)?;
    std::fs::rename(&work, base.join(format!("{seed}-candidate"))).map_err(io)?;
    let mut problems = if recorded.is_some() {
        compare(
            &record::portable(&reference, flavour),
            &record::portable(&candidate, flavour),
            (false, args.dump),
        )
    } else {
        compare(
            &reference,
            &candidate,
            (args.concurrent && !args.ordered, args.dump),
        )
    };
    // A disagreement inside the reference itself is a harness problem; one
    // in the candidate's recheck is a real failure.
    for (side, o) in [("reference", &reference), ("candidate", &candidate)] {
        let found = if args.refused {
            check_refused(o)
        } else {
            check_recheck(o).into_iter().collect()
        };
        problems.extend(found.into_iter().map(|p| format!("({side}) {p}")));
    }
    let xattrs = run::has_xattrs(&reference) || run::has_xattrs(&candidate);
    if problems.is_empty() && !args.keep {
        for side in ["reference", "candidate"] {
            let dir = base.join(format!("{seed}-{side}"));
            if dir.exists() {
                run::remove_all(&dir).map_err(io)?;
            }
        }
    } else if !problems.is_empty() {
        problems.insert(0, scenario.to_string());
    }
    Ok(Ran {
        problems,
        reference: recorded.is_none().then_some(reference),
        xattrs,
    })
}

/// Scenarios to run: each seed, with its recorded outcome when replaying.
type Seeds = Vec<(u64, Option<Outcome>)>;

/// How scenarios are generated, and how replica `b` is reached.
type Kind = (Flavour, Option<Remote>);

/// The scenarios to run, and their kind.
fn plan(args: &Args) -> Result<(Kind, Seeds), String> {
    match &args.replay {
        Some(file) => {
            let text = std::fs::read_to_string(file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let r = record::parse(&text).map_err(|e| format!("{}: {e}", file.display()))?;
            let seeds = r.outcomes.into_iter().map(|(s, o)| (s, Some(o))).collect();
            Ok(((r.flavour, r.remote), seeds))
        }
        None => Ok((
            (args.flavour, args.remote),
            (args.seed..args.seed + args.count)
                .map(|s| (s, None))
                .collect(),
        )),
    }
}

/// A path without Windows' `\\?\` prefix, which `canonicalize` adds and
/// which neither program prints back.
fn plain(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(s) if !s.starts_with("UNC") => PathBuf::from(s),
        _ => p,
    }
}

fn main() -> ExitCode {
    // Started by a client as ssh.
    if let Some((dir, work)) = shim::running_as() {
        return shim::main(&dir, &work);
    }
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
    let base = plain(std::fs::canonicalize(&base).unwrap_or(base));
    let (kind, seeds) = match plan(&args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let total = seeds.len();
    let mut failed = 0;
    let mut with_xattrs = 0;
    let mut recording = Recording {
        flavour: kind.0,
        remote: kind.1,
        outcomes: Vec::new(),
    };
    for (seed, recorded) in seeds {
        match run_scenario(&args, &base, seed, kind, recorded.as_ref()) {
            Ok(ran) => {
                with_xattrs += usize::from(ran.xattrs);
                if ran.problems.is_empty() {
                    println!("scenario {seed}: ok");
                } else {
                    failed += 1;
                    println!("scenario {seed}: DIFFERS");
                    for p in ran.problems {
                        println!("  {}", p.replace('\n', "\n  "));
                    }
                }
                recording.outcomes.extend(ran.reference.map(|o| (seed, o)));
            }
            Err(e) => {
                failed += 1;
                println!("{e}");
            }
        }
    }
    println!(
        "{} of {total} scenarios agree (work directory {})",
        total - failed,
        base.display()
    );
    // Agreement on replicas without attributes would say nothing: the file
    // system may not keep them.
    if args.xattrs && !args.refused && total > 0 && with_xattrs == 0 {
        println!("no scenario left any extended attributes: is the file system keeping them?");
        failed = total;
    }
    if let Some(file) = &args.record {
        if failed > 0 {
            eprintln!("not recording {}: scenarios differ", file.display());
            return ExitCode::FAILURE;
        }
        if let Err(e) = std::fs::write(file, record::write(&recording)) {
            eprintln!("cannot write {}: {e}", file.display());
            return ExitCode::from(2);
        }
        println!("recorded {total} scenarios in {}", file.display());
    }
    if failed == 0 {
        if !args.keep {
            let _ = std::fs::remove_dir(&base);
        }
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
