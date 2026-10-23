//! Running an implementation on a scenario and collecting what it did.
//!
//! With a remote root, replica `b` is reached as OCaml reaches another
//! host, but on this one:
//! - over ssh, through a shim given as `-sshcmd`, which runs the server
//!   command itself instead of on a host (see [`shim`]);
//! - on a socket, from a server the harness starts for each sync.
//!
//! Either way the server has a Unison directory, a home (the work
//! directory, so that the root's path `b` is relative to it) and a host
//! name of its own, and its standard error is kept apart from the
//! client's.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::scenario::{self, Scenario};
use crate::shim;

/// Preferences for every run: batch mode, no colours, log or big-delete
/// prompts, and modification times synchronised (so they can be
/// compared).
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
];

/// Unless items run concurrently: one transfer at a time, so the output
/// order is deterministic.
const ONE_AT_A_TIME: &[&str] = &["-maxthreads", "1"];

const TIMEOUT: Duration = Duration::from_secs(120);

/// How long a socket server may take to say it has started.
const SERVER_START: Duration = Duration::from_secs(20);

/// How syncs run, besides how replica `b` is reached.
#[derive(Debug, Clone, Default)]
pub struct How {
    /// Items run concurrently (no `-maxthreads 1`).
    pub concurrent: bool,
    /// More arguments for the clients (`-debug remote+`, say).
    pub client_args: Vec<String>,
    /// The ssh shim drops `__new-rpc-mode` from the server's command, so
    /// that a 2.52 or 2.53 server starts in its compatibility mode.
    pub old_server: bool,
}

/// The host names of the two ends (`UNISONLOCALHOSTNAME`) with a remote
/// root, which archive names and the output depend on.
const CLIENT_HOST: &str = "client";
pub const SERVER_HOST: &str = "server";

/// How replica `b` is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    Ssh,
    Socket,
}

impl Remote {
    pub fn name(self) -> &'static str {
        match self {
            Remote::Ssh => "ssh",
            Remote::Socket => "socket",
        }
    }

    pub fn parse(s: &str) -> Option<Remote> {
        match s {
            "ssh" => Some(Remote::Ssh),
            "socket" => Some(Remote::Socket),
            _ => None,
        }
    }
}

/// The programs that run a sync: the client, and the server when replica
/// `b` is remote.
#[derive(Debug, Clone)]
pub struct Pairing {
    pub client: PathBuf,
    pub server: PathBuf,
}

/// One invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The server's standard error, with a remote root.
    pub server: String,
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
    /// Files in the server's Unison directory, with a remote root.
    pub server_dir: Vec<String>,
    /// The reference implementation, run again on this implementation's
    /// final state: it should find nothing to do.
    pub recheck: Step,
    /// The arguments each sync gave the ssh shim, a line each, with the
    /// server's path as `<server>`. (Not recorded.)
    pub shell_args: String,
}

/// Reads a stream to its end on a thread of its own.
fn drain(mut r: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Waits for `child`, killing it after `TIMEOUT`; `None` if it was killed
/// or died of a signal.
fn wait(child: &mut Child) -> io::Result<Option<i32>> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// A socket server, killed when dropped: the client's preferences replace
/// the server's, `killserver` among them, so it never stops by itself.
struct SocketServer {
    child: Child,
    port: u16,
    stderr: Option<thread::JoinHandle<String>>,
}

impl SocketServer {
    /// Starts `bin` serving `work`, and waits until it says so.
    fn start(bin: &Path, work: &Path) -> io::Result<SocketServer> {
        // A free port, as the system gives one; another process could take
        // it before the server does, which is unlikely enough here.
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?
            .local_addr()?
            .port();
        let mut child = Command::new(bin)
            .args(["-socket", &port.to_string(), "-listen", "127.0.0.1"])
            .current_dir(work)
            .env("UNISON", work.join("unison-server"))
            .env("HOME", work)
            .env("UNISONLOCALHOSTNAME", SERVER_HOST)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", bin.display())))?;
        // "server started" goes to standard error, which is read whole on
        // a thread once the line has come.
        let (tx, rx) = mpsc::channel();
        let mut r = BufReader::new(child.stderr.take().expect("piped"));
        let stderr = thread::spawn(move || {
            let mut seen = String::new();
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 0 {
                seen.push_str(&line);
                // A Windows server ends its lines with "\r\n".
                if line.trim_end() == "server started" {
                    let _ = tx.send(());
                }
                line.clear();
            }
            seen
        });
        let mut server = SocketServer {
            child,
            port,
            stderr: Some(stderr),
        };
        if rx.recv_timeout(SERVER_START).is_err() {
            let text = server.stop();
            return Err(io::Error::other(format!(
                "{} -socket {port} did not start:\n{text}",
                bin.display()
            )));
        }
        Ok(server)
    }

    /// Kills the server, and its standard error.
    fn stop(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stderr
            .take()
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl Drop for SocketServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// `server` as `-servercmd`, which clients split into words at spaces,
/// a backslash escaping the next character: so it must have neither, and
/// on Windows its separators are written `/`.
fn server_cmd(server: &Path) -> io::Result<String> {
    let s = server.to_str().map(|s| {
        if cfg!(windows) {
            s.replace('\\', "/")
        } else {
            s.to_owned()
        }
    });
    s.filter(|s| !s.contains([' ', '\'', '"', '\\', '$']))
        .ok_or_else(|| io::Error::other(format!("{}: not a plain path", server.display())))
}

/// The length of the ssh shim's record of the arguments it was given,
/// which grows each time it runs a server.
fn shim_runs(work: &Path) -> u64 {
    fs::metadata(work.join("remote").join("argv")).map_or(0, |m| m.len())
}

fn run(p: &Pairing, remote: Option<Remote>, how: &How, work: &Path) -> io::Result<Step> {
    let mut cmd = Command::new(&p.client);
    cmd.arg(work.join("a"));
    let mut server = None;
    let mut runs = 0;
    let server_stderr = work.join("remote").join("stderr");
    match remote {
        None => {
            cmd.arg(work.join("b"));
        }
        Some(Remote::Ssh) => {
            fs::write(&server_stderr, "")?;
            let _ = fs::remove_file(work.join("remote").join("exit"));
            runs = shim_runs(work);
            cmd.arg(format!("ssh://{SERVER_HOST}/b"))
                .arg("-sshcmd")
                .arg(shim::install(work, how.old_server)?)
                .arg("-servercmd")
                .arg(server_cmd(&p.server)?);
        }
        Some(Remote::Socket) => {
            let s = SocketServer::start(&p.server, work)?;
            cmd.arg(format!("socket://127.0.0.1:{}/b", s.port));
            server = Some(s);
        }
    }
    if remote.is_some() {
        cmd.env("UNISONLOCALHOSTNAME", CLIENT_HOST);
    }
    let mut child = cmd
        .args(FLAGS)
        .args(if how.concurrent {
            &[][..]
        } else {
            ONE_AT_A_TIME
        })
        .args(&how.client_args)
        .env("UNISON", work.join("unison"))
        .env("HOME", work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.client.display())))?;
    let out = drain(child.stdout.take().expect("piped"));
    let err = drain(child.stderr.take().expect("piped"));
    let exit = wait(&mut child)?;
    let (stdout, stderr) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    let (server, port) = match (remote, server) {
        (None, _) => (String::new(), None),
        (Some(Remote::Ssh), _) => {
            // The client can finish before the server it started has; wait
            // for the server, if the shim ran one.
            let exit = work.join("remote").join("exit");
            let start = Instant::now();
            while shim_runs(work) > runs && !exit.exists() && start.elapsed() < SERVER_START {
                thread::sleep(Duration::from_millis(20));
            }
            (fs::read_to_string(&server_stderr)?, None)
        }
        (Some(Remote::Socket), Some(mut s)) => (s.stop(), Some(s.port)),
        (Some(Remote::Socket), None) => unreachable!("started above"),
    };
    let norm = |s: &str| {
        let s = normalize(s, work);
        match port {
            Some(p) => s.replace(&format!("127.0.0.1:{p}"), "127.0.0.1:<port>"),
            None => s,
        }
    };
    Ok(Step {
        exit,
        stdout: norm(&stdout),
        stderr: norm(&stderr),
        server: norm(&server),
    })
}

fn listing(dir: &Path) -> io::Result<Vec<String>> {
    let mut v: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    Ok(v)
}

/// Runs `p` through `scenario` in `work`, which is emptied first, then has
/// `reference` recheck the result.
pub fn outcome(
    p: &Pairing,
    reference: &Pairing,
    remote: Option<Remote>,
    how: &How,
    scenario: &Scenario,
    work: &Path,
) -> io::Result<Outcome> {
    if work.exists() {
        remove_all(work)?;
    }
    fs::create_dir_all(work.join("unison"))?;
    if remote.is_some() {
        fs::create_dir_all(work.join("unison-server"))?;
        fs::create_dir_all(work.join("remote"))?;
    }
    let (a, b) = (work.join("a"), work.join("b"));
    scenario::apply(&a, &scenario.initial_a)?;
    scenario::apply(&b, &scenario.initial_b)?;
    let first = run(p, remote, how, work)?;
    scenario::apply(&a, &scenario.changes_a)?;
    scenario::apply(&b, &scenario.changes_b)?;
    let second = run(p, remote, how, work)?;
    let (snap_a, snap_b) = (snapshot(&a)?, snapshot(&b)?);
    let unison_dir = listing(&work.join("unison"))?;
    let server_dir = match remote {
        Some(_) => listing(&work.join("unison-server"))?,
        None => Vec::new(),
    };
    let recheck = run(reference, remote, how, work)?;
    let mut shell_args = fs::read_to_string(work.join("remote").join("argv")).unwrap_or_default();
    for server in [&p.server, &reference.server] {
        // (A replay has no reference.)
        if let Ok(server) = server_cmd(server)
            && !server.is_empty()
        {
            shell_args = shell_args.replace(&server, "<server>");
        }
    }
    Ok(Outcome {
        first,
        second,
        a: snap_a,
        b: snap_b,
        unison_dir,
        server_dir,
        recheck,
        shell_args,
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
    // Windows roots print with `/` (`Fspath.localString2fspath`).
    let w = work.display().to_string();
    let mut s = s.replace(&w, "<W>").replace(&w.replace('\\', "/"), "<W>");
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
    // gemel's banner is "Gemel 0.1.0", with no runtime in parentheses.
    let mut s = String::new();
    let mut rest = out.as_str();
    while let Some(i) = rest.find("Gemel ") {
        s.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let n = after
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(after.len());
        if after.starts_with(|c: char| c.is_ascii_digit()) {
            s.push_str("Unison <version>");
            rest = &after[n..];
        } else {
            s.push_str("Gemel ");
            rest = after;
        }
    }
    s.push_str(rest);
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
                   Gemel 0.1.0 finished propagating changes at 18:38:45.70 on 19 Oct 2026\n\
                   [BGN] Copying d from /tmp/w/a to /tmp/w/b\n\
                   modified on 2026-10-19 at 18:38:44  size 2\n\
                   finished at 18:38:46 on 19 Oct 2026, 0.007 s\n\
                   2 items will be synced";
        assert_eq!(
            normalize(raw, work),
            "done\n\
                \x20  new file <-?-> new file   c\n\
             Unison <version> started propagating changes at <time> on <date>\n\
             Unison <version> finished propagating changes at <time> on <date>\n\
             [BGN] Copying d from <W>/a to <W>/b\n\
             modified on <date> at <time>  size 2\n\
             finished at <time> on <date>, <duration>\n\
             2 items will be synced"
        );
    }
}
