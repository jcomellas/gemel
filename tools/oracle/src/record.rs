//! Recorded outcomes: the reference's results on a set of scenarios, saved
//! where OCaml Unison runs, and replayed where it doesn't (Windows).
//!
//! The format is line based:
//!
//! ```text
//! flavour windows
//! remote socket            (with a remote root only)
//! scenario 0
//! first exit 0
//! stdout 3
//! <three lines>
//! stderr 1
//! <one line>
//! server 1                 (with a remote root only: the server's stderr)
//! <one line>
//! second exit 0
//! ...
//! recheck exit 0
//! ...
//! replica a 2
//! <path>\t<description>
//! replica b 2
//! ...
//! unison 5
//! <file name>
//! server 3                 (with a remote root only)
//! <file name>
//! ssh 3                    (over ssh only: the shim's arguments)
//! <line>
//! ```
//!
//! A stream's text is split on `\n`, so a count of one with an empty line
//! is an empty stream, and a final newline shows as a last empty line.

use std::fmt::Write as _;

use crate::run::{Outcome, Remote, Snapshot, Step};
use crate::scenario::Flavour;

/// Outcomes recorded from the reference, to replay where there is none.
pub struct Recording {
    /// The scenarios' flavour.
    pub flavour: Flavour,
    /// How replica `b` was reached.
    pub remote: Option<Remote>,
    /// Each scenario's seed and outcome.
    pub outcomes: Vec<(u64, Outcome)>,
}

fn write_text(out: &mut String, name: &str, text: &str) {
    let lines: Vec<&str> = text.split('\n').collect();
    let _ = writeln!(out, "{name} {}", lines.len());
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
}

fn write_step(out: &mut String, name: &str, s: &Step, remote: bool) {
    match s.exit {
        Some(c) => {
            let _ = writeln!(out, "{name} exit {c}");
        }
        None => {
            let _ = writeln!(out, "{name} exit none");
        }
    }
    write_text(out, "stdout", &s.stdout);
    write_text(out, "stderr", &s.stderr);
    if remote {
        write_text(out, "server", &s.server);
    }
}

fn write_names(out: &mut String, name: &str, names: &[String]) {
    let _ = writeln!(out, "{name} {}", names.len());
    for f in names {
        let _ = writeln!(out, "{f}");
    }
}

/// A recording as text.
pub fn write(r: &Recording) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "flavour {}", r.flavour.name());
    if let Some(m) = r.remote {
        let _ = writeln!(out, "remote {}", m.name());
    }
    let remote = r.remote.is_some();
    for (seed, o) in &r.outcomes {
        let _ = writeln!(out, "scenario {seed}");
        write_step(&mut out, "first", &o.first, remote);
        write_step(&mut out, "second", &o.second, remote);
        write_step(&mut out, "recheck", &o.recheck, remote);
        for (name, snap) in [("a", &o.a), ("b", &o.b)] {
            let _ = writeln!(out, "replica {name} {}", snap.len());
            for (path, desc) in snap {
                let _ = writeln!(out, "{path}\t{desc}");
            }
        }
        write_names(&mut out, "unison", &o.unison_dir);
        if remote {
            write_names(&mut out, "server", &o.server_dir);
        }
        if r.remote == Some(Remote::Ssh) {
            write_text(&mut out, "ssh", &o.shell_args);
        }
    }
    out
}

struct Lines<'a> {
    it: std::iter::Enumerate<std::str::Split<'a, char>>,
}

impl<'a> Lines<'a> {
    fn next(&mut self) -> Result<(usize, &'a str), String> {
        self.it
            .next()
            .map(|(i, l)| (i + 1, l))
            .ok_or_else(|| "unexpected end of the recording".to_owned())
    }

    /// The next line, which must be `keyword` followed by a value.
    fn field(&mut self, keyword: &str) -> Result<&'a str, String> {
        let (n, l) = self.next()?;
        l.strip_prefix(keyword)
            .and_then(|r| r.strip_prefix(' '))
            .ok_or_else(|| format!("line {n}: expected {keyword:?}, found {l:?}"))
    }

    fn count(&mut self, keyword: &str) -> Result<usize, String> {
        let v = self.field(keyword)?;
        v.parse().map_err(|e| format!("{keyword} {v:?}: {e}"))
    }

    fn text(&mut self, keyword: &str) -> Result<String, String> {
        let n = self.count(keyword)?;
        let mut lines = Vec::with_capacity(n);
        for _ in 0..n {
            lines.push(self.next()?.1);
        }
        Ok(lines.join("\n"))
    }

    fn step(&mut self, name: &str, remote: bool) -> Result<Step, String> {
        let exit = match self.field(name)?.strip_prefix("exit ") {
            Some("none") => None,
            Some(c) => Some(c.parse().map_err(|e| format!("{name} exit {c:?}: {e}"))?),
            None => return Err(format!("{name}: expected an exit code")),
        };
        Ok(Step {
            exit,
            stdout: self.text("stdout")?,
            stderr: self.text("stderr")?,
            server: if remote {
                self.text("server")?
            } else {
                String::new()
            },
        })
    }

    fn names(&mut self, keyword: &str) -> Result<Vec<String>, String> {
        let k = self.count(keyword)?;
        let mut v = Vec::with_capacity(k);
        for _ in 0..k {
            v.push(self.next()?.1.to_owned());
        }
        Ok(v)
    }

    fn snapshot(&mut self, name: &str) -> Result<Snapshot, String> {
        let n = self.count(&format!("replica {name}"))?;
        let mut snap = Snapshot::new();
        for _ in 0..n {
            let (i, l) = self.next()?;
            let (path, desc) = l
                .split_once('\t')
                .ok_or_else(|| format!("line {i}: expected a path and a description"))?;
            snap.insert(path.to_owned(), desc.to_owned());
        }
        Ok(snap)
    }
}

/// A recording from its text.
pub fn parse(text: &str) -> Result<Recording, String> {
    let mut lines = Lines {
        it: text.split('\n').enumerate(),
    };
    let f = lines.field("flavour")?;
    let flavour = Flavour::parse(f).ok_or_else(|| format!("unknown flavour {f:?}"))?;
    let mut remote = None;
    let mut outcomes = Vec::new();
    loop {
        let (n, l) = lines.next()?;
        if let Some(m) = l.strip_prefix("remote ")
            && outcomes.is_empty()
            && remote.is_none()
        {
            remote =
                Some(Remote::parse(m).ok_or_else(|| format!("line {n}: unknown remote {m:?}"))?);
            continue;
        }
        if l.is_empty() {
            // The final newline.
            break;
        }
        let seed = l
            .strip_prefix("scenario ")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("line {n}: expected a scenario, found {l:?}"))?;
        let r = remote.is_some();
        let first = lines.step("first", r)?;
        let second = lines.step("second", r)?;
        let recheck = lines.step("recheck", r)?;
        let a = lines.snapshot("a")?;
        let b = lines.snapshot("b")?;
        let unison_dir = lines.names("unison")?;
        let server_dir = if r {
            lines.names("server")?
        } else {
            Vec::new()
        };
        let shell_args = if remote == Some(Remote::Ssh) {
            lines.text("ssh")?
        } else {
            String::new()
        };
        outcomes.push((
            seed,
            Outcome {
                first,
                second,
                a,
                b,
                unison_dir,
                server_dir,
                recheck,
                shell_args,
            },
        ));
    }
    Ok(Recording {
        flavour,
        remote,
        outcomes,
    })
}

/// Replaces each run of hexadecimal digits after `prefix` at the start of
/// a name.
fn hide_hash(name: &str) -> String {
    for prefix in ["ar", "fp", "lk"] {
        if let Some(rest) = name.strip_prefix(prefix)
            && !rest.is_empty()
            && rest.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return format!("{prefix}<hash>");
        }
    }
    name.to_owned()
}

/// `rwxr-xr-x` and friends, as OCaml prints permissions on Unix, become
/// `read-write` or `read-only`, as it prints them on Windows.
fn windows_perms(line: &str) -> String {
    line.split(' ')
        .map(|w| {
            let b = w.as_bytes();
            let perms = b.len() == 9
                && b.iter().all(|c| b"rwx-sStT?".contains(c))
                && b[0] != b'-'
                && b.contains(&b'-');
            match (perms, b.get(1)) {
                (true, Some(b'w')) => "read-write",
                (true, _) => "read-only",
                (false, _) => w,
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What legitimately differs between the system an outcome was recorded
/// on and the one it is replayed on, removed from both: line endings,
/// the padding of local times, separators in the roots, the way permissions are printed and stored,
/// and the archives' hashes (of the roots' paths and the host's name).
pub fn portable(o: &Outcome, flavour: Flavour) -> Outcome {
    let text = |s: &str| {
        // Hours are padded to two places with a space, and whether they need
        // it depends on the time zone.
        let s = s.replace("\r\n", "\n").replace("  <time>", " <time>");
        let mut out = String::new();
        let mut rest = s.as_str();
        // "<W>\a\x" from Windows roots, "<W>/a/x" elsewhere.
        while let Some(i) = rest.find("<W>") {
            out.push_str(&rest[..i + 3]);
            rest = &rest[i + 3..];
            let n = rest
                .find(|c: char| c.is_whitespace() || c == ',' || c == ')')
                .unwrap_or(rest.len());
            out.push_str(&rest[..n].replace('\\', "/"));
            rest = &rest[n..];
        }
        out.push_str(rest);
        if flavour == Flavour::Windows {
            out.split('\n')
                .map(windows_perms)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            out
        }
    };
    let step = |s: &Step| Step {
        exit: s.exit,
        stdout: text(&s.stdout),
        stderr: text(&s.stderr),
        server: text(&s.server),
    };
    let snap = |s: &Snapshot| -> Snapshot {
        s.iter()
            .map(|(p, d)| {
                let d = match (flavour, d.split_once(' ')) {
                    (Flavour::Windows, Some((kind @ ("file" | "dir"), rest))) => {
                        let (mode, tail) = rest.split_once(' ').unwrap_or((rest, ""));
                        let mode = match u32::from_str_radix(mode, 8) {
                            Ok(m) if m & 0o200 == 0 => "ro",
                            Ok(_) => "rw",
                            Err(_) => mode,
                        };
                        format!(
                            "{kind} {mode}{}{tail}",
                            if tail.is_empty() { "" } else { " " }
                        )
                    }
                    _ => d.clone(),
                };
                (p.clone(), d)
            })
            .collect()
    };
    let names = |v: &[String]| {
        let mut v: Vec<String> = v.iter().map(|n| hide_hash(n)).collect();
        v.sort();
        v
    };
    Outcome {
        first: step(&o.first),
        second: step(&o.second),
        a: snap(&o.a),
        b: snap(&o.b),
        unison_dir: names(&o.unison_dir),
        server_dir: names(&o.server_dir),
        recheck: step(&o.recheck),
        shell_args: o.shell_args.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> Outcome {
        let step = |exit, stdout: &str| Step {
            exit,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            server: String::new(),
        };
        let mut a = Snapshot::new();
        a.insert("d".into(), "dir 755".into());
        a.insert("d/f".into(), "file 444 mtime 10 \"x\\n\"".into());
        Outcome {
            first: step(Some(0), "one\n\ntwo\n"),
            second: step(None, ""),
            a,
            b: Snapshot::new(),
            unison_dir: vec!["ar0123abcd".into(), "default.prf".into()],
            server_dir: Vec::new(),
            recheck: step(Some(1), "skipped: f"),
            shell_args: String::new(),
        }
    }

    #[test]
    fn a_recording_reads_back() {
        let r = Recording {
            flavour: Flavour::Windows,
            remote: None,
            outcomes: vec![(3, outcome()), (7, outcome())],
        };
        let back = parse(&write(&r)).unwrap();
        assert_eq!(back.flavour, Flavour::Windows);
        assert_eq!(back.remote, None);
        assert_eq!(back.outcomes, r.outcomes);
    }

    /// The recordings in the repository read back and write out byte for
    /// byte, so the format still reads the older one.
    #[test]
    fn the_recordings_read_back() {
        for (text, remote) in [
            (include_str!("../recorded/windows.txt"), None),
            (
                include_str!("../recorded/windows-socket.txt"),
                Some(Remote::Socket),
            ),
            (
                include_str!("../recorded/windows-ssh.txt"),
                Some(Remote::Ssh),
            ),
        ] {
            let r = parse(text).unwrap();
            assert_eq!(r.remote, remote);
            assert_eq!(r.outcomes.len(), 100);
            assert!(write(&r) == text);
        }
    }

    #[test]
    fn a_remote_recording_reads_back() {
        for remote in [Remote::Socket, Remote::Ssh] {
            let mut o = outcome();
            o.first.server = "server started\n".into();
            o.server_dir = vec!["ar4567".into()];
            // Only ssh's are recorded.
            if remote == Remote::Ssh {
                o.shell_args = "server -e none <server> -server\n".into();
            }
            let r = Recording {
                flavour: Flavour::Unix,
                remote: Some(remote),
                outcomes: vec![(1, o)],
            };
            let text = write(&r);
            let head = format!("flavour unix\nremote {}\nscenario 1\n", remote.name());
            assert!(text.starts_with(&head));
            let back = parse(&text).unwrap();
            assert_eq!(back.remote, Some(remote));
            assert_eq!(back.outcomes, r.outcomes);
        }
    }

    #[test]
    fn portable_hides_what_depends_on_the_system() {
        let mut o = outcome();
        o.first.stdout =
            "[BGN] Copying f from <W>\\a to <W>\\b\r\n  file  rw-r--r--  r--r--r--  -rw\n".into();
        let p = portable(&o, Flavour::Windows);
        assert_eq!(
            p.first.stdout,
            "[BGN] Copying f from <W>/a to <W>/b\n  file  read-write  read-only  -rw\n"
        );
        assert_eq!(p.a["d"], "dir rw");
        assert_eq!(p.a["d/f"], "file ro mtime 10 \"x\\n\"");
        assert_eq!(p.unison_dir, ["ar<hash>", "default.prf"]);
        // Unix outcomes keep their permissions.
        assert_eq!(portable(&o, Flavour::Unix).a["d"], "dir 755");
    }
}
