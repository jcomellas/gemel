//! Running an implementation on a scenario and collecting what it did.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::scenario::{self, Scenario};

/// Preferences for every run: batch mode, no colours, log or big-delete
/// prompts, one transfer at a time (so the output order is deterministic),
/// and modification times synchronised (so they can be compared).
pub const FLAGS: &[&str] = &[
    "-batch",
    "-ui",
    "text",
    "-color",
    "false",
    "-log=false",
    "-confirmbigdel=false",
    "-contactquietly",
    "-times",
    "-maxthreads",
    "1",
];

const TIMEOUT: Duration = Duration::from_secs(120);

/// One invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// What a replica looks like: each path's kind, permissions, content (or
/// link target) and, for files, modification time.
pub type Snapshot = BTreeMap<String, String>;

/// Everything collected from one implementation on one scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub first: Step,
    pub second: Step,
    pub a: Snapshot,
    pub b: Snapshot,
    /// Files in the Unison directory (archives, fingerprint caches, ...).
    pub unison_dir: Vec<String>,
    /// The reference implementation, run again on this implementation's
    /// final state: it should find nothing to do.
    pub recheck: Step,
}

fn run(bin: &Path, work: &Path) -> io::Result<Step> {
    let mut child = Command::new(bin)
        .arg(work.join("a"))
        .arg(work.join("b"))
        .args(FLAGS)
        .env("UNISON", work.join("unison"))
        .env("HOME", work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let drain = |mut r: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(Box::new(child.stdout.take().expect("piped")));
    let err = drain(Box::new(child.stderr.take().expect("piped")));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(Step {
        exit: status.and_then(|s| s.code()),
        stdout: normalize(&out.join().unwrap_or_default(), work),
        stderr: normalize(&err.join().unwrap_or_default(), work),
    })
}

/// Runs `bin` through `scenario` in `work`, which is emptied first, then
/// has `reference` recheck the result.
pub fn outcome(
    bin: &Path,
    reference: &Path,
    scenario: &Scenario,
    work: &Path,
) -> io::Result<Outcome> {
    if work.exists() {
        remove_all(work)?;
    }
    fs::create_dir_all(work.join("unison"))?;
    let (a, b) = (work.join("a"), work.join("b"));
    scenario::apply(&a, &scenario.initial_a)?;
    scenario::apply(&b, &scenario.initial_b)?;
    let first = run(bin, work)?;
    scenario::apply(&a, &scenario.changes_a)?;
    scenario::apply(&b, &scenario.changes_b)?;
    let second = run(bin, work)?;
    let (snap_a, snap_b) = (snapshot(&a)?, snapshot(&b)?);
    let mut unison_dir: Vec<String> = fs::read_dir(work.join("unison"))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    unison_dir.sort();
    let recheck = run(reference, work)?;
    Ok(Outcome {
        first,
        second,
        a: snap_a,
        b: snap_b,
        unison_dir,
        recheck,
    })
}

/// Removes a tree, restoring write permission on directories first.
pub fn remove_all(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fn fix(p: &Path) {
            if let Ok(m) = fs::symlink_metadata(p)
                && m.is_dir()
            {
                let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
                if let Ok(rd) = fs::read_dir(p) {
                    for e in rd.flatten() {
                        fix(&e.path());
                    }
                }
            }
        }
        fix(p);
    }
    fs::remove_dir_all(p)
}

fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut snap = Snapshot::new();
    walk(root, root, &mut snap)?;
    Ok(snap)
}

fn walk(root: &Path, dir: &Path, snap: &mut Snapshot) -> io::Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for p in entries {
        let rel = p
            .strip_prefix(root)
            .expect("under the root")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let m = fs::symlink_metadata(&p)?;
        let mode = mode(&m);
        let desc = if m.file_type().is_symlink() {
            format!("link -> {}", fs::read_link(&p)?.display())
        } else if m.is_dir() {
            format!("dir {mode}")
        } else {
            let mtime = m
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            format!(
                "file {mode} mtime {mtime} {:?}",
                String::from_utf8_lossy(&fs::read(&p)?)
            )
        };
        snap.insert(rel, desc);
        if m.is_dir() {
            walk(root, &p, snap)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mode(m: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!("{:o}", m.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode(m: &fs::Metadata) -> String {
    if m.permissions().readonly() {
        "ro".into()
    } else {
        "rw".into()
    }
}

const MONTHS: &[&str] = &[
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Replaces matches of `pattern`, where `9` is a digit, `*` one or more
/// digits, and anything else itself.
fn replace_digits(s: &str, pattern: &str, with: &str) -> String {
    fn match_at(s: &[u8], p: &[u8]) -> Option<usize> {
        let (mut i, mut j) = (0, 0);
        while j < p.len() {
            match p[j] {
                b'9' => {
                    if !s.get(i)?.is_ascii_digit() {
                        return None;
                    }
                    i += 1;
                }
                b'*' => {
                    let n = s[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                    if n == 0 {
                        return None;
                    }
                    i += n;
                }
                c => {
                    if *s.get(i)? != c {
                        return None;
                    }
                    i += 1;
                }
            }
            j += 1;
        }
        Some(i)
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        match match_at(&b[i..], pattern.as_bytes()) {
            Some(n) if n > 0 => {
                out.push_str(with);
                i += n;
            }
            _ => {
                let ch = s[i..].chars().next().expect("in bounds");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Removes what legitimately differs between runs: the work directory's
/// path, versions, clock times, dates and durations, and the progress
/// display (carriage-return-separated, timing dependent).
pub fn normalize(s: &str, work: &Path) -> String {
    let mut s = s.replace(&work.display().to_string(), "<W>");
    // Progress and the scanning spinner ("/ name", "- name", ...) are
    // drawn as carriage-return-separated pieces of a line, with blank
    // pieces to erase them; drop them.
    s = s
        .split('\n')
        .map(|line| {
            if !line.contains('\r') {
                return line.to_owned();
            }
            line.split('\r')
                .filter(|piece| {
                    let t = piece.trim();
                    let spinner =
                        matches!(piece.as_bytes(), [b'-' | b'\\' | b'|' | b'/', b' ', ..]);
                    !(t.is_empty() || spinner || t.contains(" ETA") || t.starts_with("Starting..."))
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n");
    // "Unison 2.54.0 (ocaml 4.14.3)" and whatever gemel prints there.
    let mut out = String::new();
    let mut rest = s.as_str();
    while let Some(i) = rest.find("Unison ") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 7..];
        match after.find(')') {
            Some(j)
                if after[..j].starts_with(|c: char| c.is_ascii_digit())
                    && !after[..j].contains('\n') =>
            {
                out.push_str("Unison <version>");
                rest = &after[j + 1..];
            }
            _ => {
                out.push_str("Unison ");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let mut s = out;
    s = replace_digits(&s, "99:99:99.99", "<time>");
    s = replace_digits(&s, "99:99:99", "<time>");
    s = replace_digits(&s, "9:99:99", "<time>");
    s = replace_digits(&s, "9999-99-99", "<date>");
    for m in MONTHS {
        s = replace_digits(&s, &format!("* {m} 9999"), "<date>");
    }
    replace_digits(&s, ", *.* s", ", <duration>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_removes_what_varies() {
        let work = Path::new("/tmp/w");
        let raw = "Starting...   Starting...\r 28%  0/2  (2 B of 5 B)  --:-- ETA \rdone\n\
                   / f  \r     \r   new file <-?-> new file   c\n\
                   Unison 2.54.0 (ocaml 4.14.3) started propagating changes at 18:38:45.70 on 19 Oct 2026\n\
                   [BGN] Copying d from /tmp/w/a to /tmp/w/b\n\
                   modified on 2026-10-19 at 18:38:44  size 2\n\
                   finished at 18:38:46 on 19 Oct 2026, 0.007 s\n\
                   2 items will be synced";
        assert_eq!(
            normalize(raw, work),
            "done\n\
                \x20  new file <-?-> new file   c\n\
             Unison <version> started propagating changes at <time> on <date>\n\
             [BGN] Copying d from <W>/a to <W>/b\n\
             modified on <date> at <time>  size 2\n\
             finished at <time> on <date>, <duration>\n\
             2 items will be synced"
        );
    }
}
