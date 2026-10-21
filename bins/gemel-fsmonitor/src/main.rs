//! `gemel-fsmonitor`: the file-watcher helper used by `repeat = watch`.
//!
//! Replaces Unison's `src/fsmonitor/` watchers (inotify, Windows) with one
//! portable implementation, and speaks the same line protocol
//! (`gemel_compat::fsmonitor_protocol`) so that OCaml Unison can use it
//! too when installed as `unison-fsmonitor`.
//!
//! Derived from Unison, Copyright 2012, Benjamin C. Pierce.
//!
//! Unison writes commands to its stdin and reads the answers from its
//! stdout. One loop handles the commands and the system's events, from a
//! channel that a thread reading stdin and the backend feed, with a
//! timeout for the delay before changes are reported ([`core::DELAY`]).

mod backend;
mod core;

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gemel_compat::fsmonitor_protocol::{PROTOCOL_VERSION, Response};

use crate::core::{Core, Fatal};

enum Input {
    /// A line from Unison, without its newline; `None` at the end.
    Line(io::Result<Option<Vec<u8>>>),
    Notify(notify::Result<notify::Event>),
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64())
}

/// Sends `ERROR msg` and stops (`Watchercommon.error`).
fn error(msg: &str) -> ExitCode {
    let mut out = io::stdout().lock();
    let _ = out.write_all(&Response::Error(msg.as_bytes().to_vec()).to_line());
    let _ = out.flush();
    ExitCode::FAILURE
}

fn read_stdin(tx: mpsc::Sender<Input>) {
    let mut stdin = io::stdin().lock();
    loop {
        let mut l = Vec::new();
        let r = match stdin.read_until(b'\n', &mut l) {
            Ok(0) => Ok(None),
            Ok(_) => {
                if l.last() == Some(&b'\n') {
                    l.pop();
                }
                Ok(Some(l))
            }
            Err(e) => Err(e),
        };
        let end = !matches!(r, Ok(Some(_)));
        if tx.send(Input::Line(r)).is_err() || end {
            return;
        }
    }
}

fn main() -> ExitCode {
    let (tx, rx) = mpsc::channel();
    let events = tx.clone();
    let backend = match backend::Notify::new(move |r| {
        let _ = events.send(Input::Notify(r));
    }) {
        Ok(b) => b,
        Err(m) => return error(&m),
    };
    std::thread::spawn(move || read_stdin(tx));
    let mut core = Core::new(backend);
    let mut stdout = io::stdout().lock();
    core.out = format!("VERSION {PROTOCOL_VERSION}\n").into_bytes();
    loop {
        // Unison has gone (EPIPE): stop quietly, as OCaml does.
        if !core.out.is_empty() {
            if stdout
                .write_all(&core.out)
                .and_then(|()| stdout.flush())
                .is_err()
            {
                return ExitCode::SUCCESS;
            }
            core.out.clear();
        }
        let input = match core.timer {
            Some(t) => match rx.recv_timeout(Duration::from_secs_f64((t - now()).max(0.))) {
                Ok(i) => Some(i),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return ExitCode::SUCCESS,
            },
            None => match rx.recv() {
                Ok(i) => Some(i),
                Err(_) => return ExitCode::SUCCESS,
            },
        };
        let r = match input {
            None => {
                core.tick(now());
                Ok(())
            }
            Some(Input::Line(Ok(Some(l)))) => core.line(&l, now()),
            Some(Input::Line(Ok(None))) => return ExitCode::SUCCESS,
            Some(Input::Line(Err(e))) => Err(Fatal(format!(
                "error while communicating with Unison: {e}\n"
            ))),
            Some(Input::Notify(r)) => {
                for ev in backend::events(r) {
                    if core.debug {
                        eprintln!("{ev:?}");
                    }
                    core.event(ev, now());
                }
                Ok(())
            }
        };
        if let Err(Fatal(m)) = r {
            let _ = stdout.write_all(&core.out);
            drop(stdout);
            return error(&m);
        }
    }
}
