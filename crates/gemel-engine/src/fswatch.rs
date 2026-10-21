//! Client side of the file-watcher protocol (`repeat = watch`).
//!
//! Ports Unison `src/fswatch.ml`, and the half of `src/fswatchold.ml` that
//! uses it.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The watcher is a helper process (`gemel-fsmonitor`, or OCaml's
//! `unison-fsmonitor`) that speaks the line protocol of
//! [`gemel_compat::fsmonitor_protocol`] over its stdin and stdout. A host
//! runs one, as OCaml does, for every replica it watches.
//!
//! # Threads
//!
//! Update detection runs off the engine thread, and tells the watcher which
//! directories it scans as it goes, each a round trip in scan order. So the
//! connection lives in a [`Slot`] behind an async mutex: a scan holds it
//! from asking for the changes through its last `DONE`, and the scans of
//! two roots on one host take turns, as OCaml's one-at-a-time scans do. A
//! thread reads the watcher's output: every line it reads is a sign of
//! changes (OCaml's `has_changes`), `CHANGES` lines answer waits only, and
//! the other lines are the answers to the commands.
//!
//! # Not ported
//!
//! The old watchers of `fswatchold.ml`, which ran `fsmonitor.py` and read
//! the changes from a file: nothing ships that script.
//!
//! # Divergence
//!
//! gemel looks for `gemel-fsmonitor`, then for OCaml's names:
//! `unison-fsmonitor-2.54` (the version of the source ported) and
//! `unison-fsmonitor`. Each is looked for next to the program, then on
//! `PATH`, as OCaml does.

use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path as StdPath, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, mpsc};

use gemel_base::path::{Global, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::fsmonitor_protocol::{Command as Cmd, PROTOCOL_VERSION, split_on_space, unquote};
use gemel_fs::fspath::Fspath;
use gemel_fs::system::SysError;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc};
use tokio::sync::{Mutex, OwnedMutexGuard, watch};

/// `watch` (`Fswatch.useWatcher`).
pub static USE_WATCHER: PrefDef<bool> = PrefDef::new(
    "watch",
    Category::Advanced(Topic::General),
    || false,
    "when set, use a file watcher process to detect changes",
    "Unison uses a file watcher process, when available, to detect filesystem \
     changes; this is used to speed up update detection. Setting this flag to \
     false disables the use of this process.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&USE_WATCHER)
}

/// The names the watcher is looked for under, in order.
const NAMES: &[&str] = &[
    "gemel-fsmonitor",
    "unison-fsmonitor-2.54",
    "unison-fsmonitor",
];

/// The watcher to run instead of looking for one, for tests.
static PROGRAM: StdMutex<Option<PathBuf>> = StdMutex::new(None);

/// Runs `program` as the watcher instead of looking for one (for tests:
/// their binaries aren't next to the watcher).
pub fn set_program(program: Option<PathBuf>) {
    *PROGRAM.lock().unwrap_or_else(|e| e.into_inner()) = program;
}

/// `watcher`: the program, next to this one or on `PATH`. The `trace`
/// gets the debug lines.
fn find_program(trace: &mut dyn FnMut(String)) -> Option<PathBuf> {
    if let Some(p) = PROGRAM.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Some(p);
    }
    trace("File monitoring helper program...\n".into());
    let exec_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(StdPath::to_path_buf));
    let path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let dirs: Vec<PathBuf> = exec_dir.into_iter().chain(path).collect();
    for name in NAMES {
        let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        for dir in &dirs {
            let p = dir.join(&name);
            let found = p.exists();
            trace(format!(
                "'{}' ...{}\n",
                p.display(),
                if found { "found" } else { "not found" }
            ));
            if found {
                return Some(p);
            }
        }
    }
    None
}

/// What the reader thread passes on: a line, or why there are no more.
type Line = Result<Vec<u8>, Error>;

/// A running watcher (OCaml's `conn`).
struct Conn {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<Line>,
    /// Counts the lines read (`has_changes`).
    changes: watch::Receiver<u64>,
    /// Cleared by the reader at the end of the watcher's output.
    alive: Arc<AtomicBool>,
}

fn died() -> Error {
    Error::Fatal("Filesystem watcher died unexpectively".into())
}

/// `reader`.
fn read_lines(
    out: impl io::Read,
    lines: mpsc::Sender<Line>,
    changes: watch::Sender<u64>,
    alive: Arc<AtomicBool>,
) {
    let mut out = BufReader::new(out);
    loop {
        let mut l = Vec::new();
        match out.read_until(b'\n', &mut l) {
            Ok(n) if n > 0 && l.last() == Some(&b'\n') => {
                l.pop();
                changes.send_modify(|n| *n += 1);
                if split_on_space(&l).0 != b"CHANGES" && lines.send(Ok(l)).is_err() {
                    return;
                }
            }
            // The end, or a last line cut short.
            _ => {
                alive.store(false, Ordering::SeqCst);
                let _ = lines.send(Err(died()));
                changes.send_modify(|n| *n += 1);
                return;
            }
        }
    }
}

/// The host's watcher, and what OCaml keeps about it in globals.
#[derive(Default)]
pub struct Slot {
    conn: Option<Conn>,
    /// The program, once looked for (OCaml's `lazy`).
    program: Option<Option<PathBuf>>,
    /// The replicas the watcher watches (`newWatchers`).
    new_watchers: BTreeSet<Vec<u8>>,
    /// Whether a `START` is under way (`scanning`), and its path.
    scanning: bool,
    start_path: Vec<u8>,
    /// Whether to send `DEBUG` (the `fswatch` debug module).
    pub debug: bool,
    /// The debug lines, for the host's trace.
    pub trace: Vec<String>,
}

impl Slot {
    fn connected(&self) -> bool {
        self.conn
            .as_ref()
            .is_some_and(|c| c.alive.load(Ordering::SeqCst))
    }

    /// `closeConnection`.
    fn close(&mut self) {
        if let Some(c) = self.conn.take() {
            let Conn {
                mut child, stdin, ..
            } = c;
            drop(stdin);
            let _ = child.wait();
        }
    }

    /// The connection, if the watcher is still there; a watcher that has
    /// gone is cleaned up.
    fn current(&mut self) -> Result<&mut Conn, Error> {
        if !self.connected() {
            self.close();
            return Err(Error::Fatal(
                "File monitoring helper program not running".into(),
            ));
        }
        Ok(self.conn.as_mut().expect("connected"))
    }

    /// `emitCmd`.
    fn emit(&mut self, cmd: &Cmd) -> Result<(), Error> {
        let line = cmd.to_line();
        if self.debug {
            self.trace
                .push(format!("<< {}", String::from_utf8_lossy(&line)));
        }
        let c = self.current()?;
        let r = c.stdin.write_all(&line).and_then(|()| c.stdin.flush());
        if let Err(e) = r {
            self.close();
            return Err(
                SysError::new("write", b"", e).to_fatal("sending command to filesystem watcher")
            );
        }
        Ok(())
    }

    /// `readLine`.
    fn read_line(&mut self) -> Result<Vec<u8>, Error> {
        let c = self.current()?;
        let l = c.lines.recv().unwrap_or_else(|_| Err(died()))?;
        if self.debug {
            self.trace
                .push(format!(">> {}\n", String::from_utf8_lossy(&l)));
        }
        Ok(l)
    }

    /// `badResponse`.
    fn bad_response(&mut self, cmd: &[u8], args: &[u8], expected: &str) -> Error {
        self.close();
        if cmd == b"ERROR" {
            let msg = unquote(args).unwrap_or_else(|_| args.to_vec());
            Error::Fatal(format!(
                "Filesystem watcher error: {}\nThe watcher can be disabled by setting preference \
                 'watch' to false",
                String::from_utf8_lossy(&msg)
            ))
        } else {
            Error::Fatal(format!(
                "Unexpected response '{} {}' from the filesystem watcher (expected {expected})",
                String::from_utf8_lossy(cmd),
                String::from_utf8_lossy(args)
            ))
        }
    }

    fn expect(&mut self, word: &[u8], expected: &str) -> Result<(), Error> {
        let l = self.read_line()?;
        let (cmd, args) = split_on_space(&l);
        if cmd != word {
            return Err(self.bad_response(cmd, args, expected));
        }
        Ok(())
    }

    /// `startProcess`: false if there is no watcher to run.
    fn start_process(&mut self) -> Result<bool, Error> {
        let program = match &self.program {
            Some(p) => p.clone(),
            None => {
                let mut lines = Vec::new();
                let p = find_program(&mut |l| lines.push(l));
                if self.debug {
                    self.trace.extend(lines);
                }
                self.program = Some(p.clone());
                p
            }
        };
        let Some(program) = program else {
            return Ok(false);
        };
        let mut child = Command::new(&program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                SysError::new("create_process", program.to_string_lossy().as_bytes(), e)
                    .to_fatal("starting filesystem watcher")
            })?;
        let stdin = child.stdin.take().expect("piped");
        let out = child.stdout.take().expect("piped");
        let (line_tx, lines) = mpsc::channel();
        let (changes_tx, changes) = watch::channel(0);
        let alive = Arc::new(AtomicBool::new(true));
        let a = alive.clone();
        std::thread::spawn(move || read_lines(out, line_tx, changes_tx, a));
        self.conn = Some(Conn {
            child,
            stdin,
            lines,
            changes,
            alive,
        });
        Ok(true)
    }

    /// `exchangeVersions`.
    fn exchange_versions(&mut self) -> Result<bool, Error> {
        let started = self.start_process()?;
        if started {
            self.emit(&Cmd::Version(PROTOCOL_VERSION))?;
            if self.debug {
                self.trace.push("debugging enabled\n".into());
                self.emit(&Cmd::Debug)?;
            }
            self.expect(b"VERSION", "VERSION")?;
        }
        Ok(started)
    }

    /// `Fswatchold.start`, with `Fswatch.start`: whether the replica is
    /// now watched.
    pub fn start(&mut self, use_watcher: bool, hash: &[u8]) -> Result<bool, Error> {
        if !use_watcher {
            return Ok(false);
        }
        let started = if !self.connected() {
            self.close();
            self.exchange_versions()?
        } else {
            self.emit(&Cmd::Reset(hash.to_vec()))?;
            true
        };
        if started {
            self.new_watchers.insert(hash.to_vec());
        }
        Ok(started)
    }

    /// `Fswatchold.running`.
    pub fn running(&mut self, hash: &[u8]) -> bool {
        if !self.new_watchers.contains(hash) {
            return false;
        }
        if self.connected() {
            true
        } else {
            self.new_watchers.remove(hash);
            false
        }
    }

    /// `Fswatchold.getChanges`: the paths that changed in the replica
    /// since it was last asked, relative to its root.
    pub fn get_changes(&mut self, hash: &[u8]) -> Result<Vec<Path<Global>>, Error> {
        if !self.connected() {
            return Err(Error::Fatal(
                "No file monitoring helper program found".into(),
            ));
        }
        self.emit(&Cmd::Changes(hash.to_vec()))?;
        let mut l = Vec::new();
        loop {
            let line = self.read_line()?;
            let (cmd, args) = split_on_space(&line);
            match cmd {
                b"CHANGES" => {}
                b"RECURSIVE" => {
                    let p = unquote(args).map_err(|e| Error::Fatal(e.to_string()))?;
                    l.push(Path::from_bytes(&p)?);
                }
                b"DONE" => return Ok(l),
                _ => return Err(self.bad_response(cmd, args, "RECURSIVE or DONE")),
            }
        }
    }

    /// `relpath`: `path` relative to where the scan started.
    fn relpath(&self, path: &[u8]) -> Vec<u8> {
        let start = &self.start_path;
        if start.is_empty() {
            path.to_vec()
        } else if start.len() == path.len() {
            Vec::new()
        } else {
            path[start.len() + 1..].to_vec()
        }
    }

    /// `startScanning`.
    pub fn start_scanning(
        &mut self,
        hash: &[u8],
        fspath: &Fspath,
        path: &[u8],
    ) -> Result<(), Error> {
        if self.connected() {
            self.emit(&Cmd::Start {
                hash: hash.to_vec(),
                fspath: fspath.as_bytes().to_vec(),
                path: path.to_vec(),
            })?;
            self.expect(b"OK", "OK")?;
            self.scanning = true;
            self.start_path = path.to_vec();
        }
        Ok(())
    }

    /// `scanDirectory`.
    pub fn scan_directory(&mut self, path: &[u8]) -> Result<(), Error> {
        if self.scanning {
            self.emit(&Cmd::Dir(self.relpath(path)))?;
            self.expect(b"OK", "OK")?;
        }
        Ok(())
    }

    /// `followLink`.
    pub fn follow_link(&mut self, path: &[u8]) -> Result<(), Error> {
        if self.scanning {
            self.emit(&Cmd::Link(self.relpath(path)))?;
            self.expect(b"OK", "OK")?;
        }
        Ok(())
    }

    /// `stopScanning`.
    pub fn stop_scanning(&mut self) -> Result<(), Error> {
        if self.scanning {
            self.scanning = false;
            self.emit(&Cmd::Done)?;
        }
        Ok(())
    }
}

/// The host's watcher.
pub struct Watch {
    slot: Arc<Mutex<Slot>>,
}

impl Default for Watch {
    fn default() -> Self {
        Watch {
            slot: Arc::new(Mutex::new(Slot::default())),
        }
    }
}

/// Takes `host`'s watcher, for a scan; other scans wait until it is given
/// back (by dropping the guard).
pub async fn take(host: &Host) -> OwnedMutexGuard<Slot> {
    let slot = host.state::<Watch>().borrow().slot.clone();
    let mut guard = slot.lock_owned().await;
    let prefs = host.prefs().borrow();
    guard.debug = gemel_base::trace::enabled(prefs.read(&gemel_base::trace::DEBUG), "fswatch");
    guard
}

/// Passes the debug lines a scan collected to `host`'s trace.
pub fn flush_trace(host: &Host, slot: &mut Slot) {
    for l in std::mem::take(&mut slot.trace) {
        crate::debug(host, "fswatch", || l);
    }
}

/// `Fswatchold.wait`: until the watcher reports a change to the replica
/// (or goes).
pub async fn wait(host: &Host, hash: &[u8]) -> Result<(), Error> {
    let mut changes = {
        let mut slot = take(host).await;
        if !slot.new_watchers.contains(hash) {
            return Err(Error::Fatal(
                "No file monitoring helper program found".into(),
            ));
        }
        // Listen before asking, so that no answer is missed.
        let mut changes = slot.current()?.changes.clone();
        changes.borrow_and_update();
        let r = slot.emit(&Cmd::Wait(hash.to_vec()));
        flush_trace(host, &mut slot);
        r?;
        changes
    };
    // An error means the reader has gone, which is a change too.
    let _ = changes.changed().await;
    Ok(())
}

/// `waitForChanges` (in OCaml's `uitext.ml`): waits for a change to the
/// replica at the root.
pub static WAIT_FOR_CHANGES: RootCmd<(), ()> = RootCmd::new(
    "waitForChanges",
    |(), w, _| w.unit(),
    |r, _| r.unit(),
    |(), w, _| w.unit(),
    |r, _| r.unit(),
    |host, fspath, ()| {
        Box::pin(async move {
            let hash = crate::archive::archive_hash(&host, &fspath)?;
            wait(&host, hash.as_bytes()).await
        })
    },
);

/// Registers this module's remote command.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    rpc.registry_mut().register_root(&WAIT_FOR_CHANGES)
}
