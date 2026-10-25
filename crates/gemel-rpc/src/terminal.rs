//! ssh under a pseudo-terminal, so that a graphical interface can answer
//! its prompts: what ssh prints there, and the answers sent back.
//!
//! Ports Unison `src/terminal.ml`, Unison `src/pty.c`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The patterns for OpenSSH's messages ([`password`], [`passphrase`],
//! [`authenticity`]) are the Mac interface's: `uigtk3` shows whatever ssh
//! prints that isn't blank.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use gemel_base::rx::Rx;

// ---- OpenSSH's messages ----

/// Compiles one of the patterns below, once.
fn pattern(cell: &'static OnceLock<Rx>, re: &str) -> &'static Rx {
    cell.get_or_init(|| Rx::regex(re).expect("a valid pattern"))
}

/// Whether `s` is a password prompt (`passwordRx`).
pub fn password(s: &[u8]) -> bool {
    static RX: OnceLock<Rx> = OnceLock::new();
    pattern(&RX, ".*assword:[ ]*").match_string(s)
}

/// Whether `s` asks for a key's passphrase (`passphraseRx`).
pub fn passphrase(s: &[u8]) -> bool {
    static RX: OnceLock<Rx> = OnceLock::new();
    pattern(&RX, "Enter passphrase for key.*").match_string(s)
}

/// Whether `s` asks whether to trust a host's key (`authenticityRx`).
pub fn authenticity(s: &[u8]) -> bool {
    static RX: OnceLock<Rx> = OnceLock::new();
    pattern(
        &RX,
        "The authenticity of host .* continue connecting \\(yes/no\\)\\? ",
    )
    .match_string(s)
}

// ---- Control sequences ----

/// Where [`process_escapes`] is in a control sequence (`controlSt`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    No,
    Escape,
    EscapeSeq,
    Csi,
    Osc,
    StringSeq,
    OscEsc,
    StringEsc,
}

const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const ESC: u8 = 0x1b;

/// The most spaces a cursor-forward sequence stands for. OCaml has no
/// limit, and would try to add as many as the count says.
const MAX_FORWARD: i64 = 1000;

/// `parseCtrlSeq` (`processEscapes`): `s` without its control sequences,
/// as a terminal would show it. A very small parser of ANSI X3.64 and
/// ECMA-48's 7-bit sequences: they are dropped, but for a cursor forward
/// (CSI n C), which stands for n spaces, and control characters inside
/// them, which are kept.
pub fn process_escapes(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut params = Vec::new();
    let mut st = Control::No;
    // `parseEsc`.
    let escape = |ch: u8, params: &mut Vec<u8>| {
        params.clear();
        match ch {
            0x20..=0x2f => Control::EscapeSeq,
            b'[' => Control::Csi,
            b']' => Control::Osc,
            b'X' | b'^' | b'_' => Control::StringSeq,
            _ => Control::No,
        }
    };
    for &ch in s {
        st = match st {
            Control::No if ch == ESC => Control::Escape,
            Control::No => {
                out.push(ch);
                Control::No
            }
            Control::Escape => escape(ch, &mut params),
            Control::EscapeSeq => match ch {
                CAN | SUB => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::Escape,
                0x30..=0xff => Control::No,
                _ => st,
            },
            Control::Csi => match ch {
                CAN | SUB => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::Escape,
                0x40..=0x7e => {
                    if ch == b'C' {
                        let n = std::str::from_utf8(&params)
                            .ok()
                            .and_then(gemel_base::util::int_of_string)
                            .unwrap_or(1);
                        out.extend(std::iter::repeat_n(b' ', n.clamp(0, MAX_FORWARD) as usize));
                    }
                    Control::No
                }
                0x7f..=0xff => Control::No,
                _ => {
                    params.push(ch);
                    st
                }
            },
            Control::Osc => match ch {
                CAN | SUB | BEL => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::OscEsc,
                _ => st,
            },
            Control::OscEsc | Control::StringEsc => match ch {
                b'\\' => Control::No,
                _ => escape(ch, &mut params),
            },
            Control::StringSeq => match ch {
                CAN | SUB => Control::No,
                0x00..=0x19 => {
                    out.push(ch);
                    st
                }
                ESC => Control::StringEsc,
                _ => st,
            },
        };
    }
    out
}

// ---- Sessions ----

/// Where the answer to what ssh printed goes.
pub type Respond = Box<dyn FnOnce(String)>;

/// What the user is told and asked while ssh runs at a terminal
/// (`termInteract`).
pub struct Interact {
    /// `userInput`: ssh printed this, maybe a question; the answer, if
    /// any, goes to the function given.
    pub user_input: Box<dyn FnMut(String, Respond)>,
    /// `endInput`: ssh won't be answered any more (the connection is up,
    /// or has failed).
    pub end_input: Box<dyn FnMut()>,
}

/// Makes the [`Interact`] for a root, given as the user wrote it
/// (`termInteract rootName`).
pub type InteractFor = Rc<dyn Fn(&str) -> Interact>;

/// What a terminal session's reader sends: what was read, until the
/// terminal fails or closes.
type Chunks = tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>;

/// The controlling process's end of a session's terminal.
pub struct Terminal {
    /// Where answers are written.
    output: RefCell<Option<std::fs::File>>,
    /// What the terminal shows, read on a thread of its own: tokio can't
    /// be relied on to wake for a terminal on macOS, and a pseudo-console's
    /// pipes can't be waited for at all.
    input: RefCell<Option<Chunks>>,
    /// The parent's own slave, kept until the session is closed, as OCaml
    /// keeps it: ssh closes its own before it opens `/dev/tty`, and the
    /// master would fail meanwhile.
    #[cfg(unix)]
    slave: RefCell<Option<std::os::fd::OwnedFd>>,
    /// The pseudo-console, closed with the session (`win_closepty`), while
    /// its output is still being read: closing it can wait for that.
    #[cfg(windows)]
    console: RefCell<Option<gemel_fs::conpty::Console>>,
}

impl Terminal {
    /// The terminal whose output is `shown` and input `typed`: what it
    /// shows is read on a thread of its own.
    fn new(mut shown: std::fs::File, typed: std::fs::File) -> std::io::Result<Terminal> {
        use std::io::Read;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("gemel-terminal".into())
            .spawn(move || {
                let mut buf = vec![0; 10_000];
                while let Ok(n) = shown.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            })?;
        Ok(Terminal {
            output: RefCell::new(Some(typed)),
            input: RefCell::new(Some(rx)),
            #[cfg(unix)]
            slave: RefCell::new(None),
            #[cfg(windows)]
            console: RefCell::new(None),
        })
    }

    /// Writes `s` and a newline (`sendResponse`), unless the session has
    /// been closed. Errors are dropped: ssh will say what went wrong.
    fn answer(&self, s: &str) {
        use std::io::Write;
        if let Some(out) = self.output.borrow_mut().as_mut() {
            let _ = out.write_all(format!("{s}\n").as_bytes());
        }
    }

    /// `close_session`'s terminal half: the master and the parent's slave
    /// close (on Windows, the pseudo-console), which ends the reader.
    pub fn close(&self) {
        self.output.borrow_mut().take();
        self.input.borrow_mut().take();
        #[cfg(unix)]
        self.slave.borrow_mut().take();
        #[cfg(windows)]
        self.console.borrow_mut().take();
    }
}

/// A shell's process.
pub enum Child {
    /// Started by tokio: under a pseudo-terminal, or plain.
    Spawned(Box<tokio::process::Child>),
    /// Started on a pseudo-console.
    #[cfg(windows)]
    OnConsole(gemel_fs::conpty::Process),
}

impl Child {
    /// Its process id, while it runs.
    pub fn id(&self) -> Option<u32> {
        match self {
            Child::Spawned(c) => c.id(),
            #[cfg(windows)]
            Child::OnConsole(p) => Some(p.id()),
        }
    }
}

/// A shell started: its terminal, if it has one, its standard input and
/// output, and its process.
pub struct Session {
    /// Its terminal.
    pub term: Option<Rc<Terminal>>,
    /// Its standard input.
    pub stdin: tokio::process::ChildStdin,
    /// Its standard output.
    pub stdout: tokio::process::ChildStdout,
    /// Its process.
    pub child: Child,
}

/// `create_session`: starts `cmd`, whose standard input and output are
/// piped, with a terminal of its own if one can be had (a pseudo-terminal,
/// or on Windows a pseudo-console), its standard error going there.
/// Otherwise, as `fallback_session`, a plain process.
pub fn create_session(mut cmd: std::process::Command) -> std::io::Result<Session> {
    #[cfg(unix)]
    if let Ok(pty) = gemel_fs::pty::open() {
        gemel_fs::pty::controlled_by(&mut cmd, &pty.slave)?;
        let child = tokio::process::Command::from(cmd).spawn()?;
        let master = std::fs::File::from(pty.master);
        let term = Terminal::new(master.try_clone()?, master)?;
        *term.slave.borrow_mut() = Some(pty.slave);
        return spawned(Some(Rc::new(term)), child);
    }
    #[cfg(windows)]
    {
        let args: Vec<std::ffi::OsString> = cmd.get_args().map(|a| a.to_owned()).collect();
        if let Ok(started) = gemel_fs::conpty::spawn(cmd.get_program(), &args) {
            let term = Terminal::new(started.output, started.input)?;
            *term.console.borrow_mut() = Some(started.console);
            return Ok(Session {
                term: Some(Rc::new(term)),
                stdin: tokio::process::ChildStdin::from_std(started.stdin.into())?,
                stdout: tokio::process::ChildStdout::from_std(started.stdout.into())?,
                child: Child::OnConsole(started.process),
            });
        }
    }
    cmd.stderr(std::process::Stdio::inherit());
    spawned(None, tokio::process::Command::from(cmd).spawn()?)
}

/// The session of `child`, started by tokio with its standard input and
/// output piped.
fn spawned(
    term: Option<Rc<Terminal>>,
    mut child: tokio::process::Child,
) -> std::io::Result<Session> {
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(std::io::Error::other("the shell's pipes"));
    };
    Ok(Session {
        term,
        stdin,
        stdout,
        child: Child::Spawned(Box::new(child)),
    })
}

/// `close_session`'s child half (`safe_waitpid`): if `child` hasn't ended
/// half a second on, it is asked to stop, and two seconds on, killed (on
/// Windows, it is ended half a second on). In the background.
pub fn end_session(child: Child) {
    match child {
        Child::Spawned(mut child) => {
            crate::runtime::spawn_local(async move {
                let ms = std::time::Duration::from_millis;
                if tokio::time::timeout(ms(500), child.wait()).await.is_ok() {
                    return;
                }
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    gemel_fs::signal::terminate(pid);
                }
                if tokio::time::timeout(ms(1500), child.wait()).await.is_ok() {
                    return;
                }
                let _ = child.kill().await;
            });
        }
        #[cfg(windows)]
        Child::OnConsole(p) => {
            crate::runtime::spawn_local(crate::runtime::blocking(move || {
                if !p.wait(500) {
                    p.terminate();
                }
            }));
        }
    }
}

/// What [`handle_password_requests`] keeps.
struct Requests {
    term: Rc<Terminal>,
    interact: RefCell<Interact>,
    /// What ssh printed and hasn't been shown yet.
    scrollback: RefCell<Vec<u8>>,
    /// No more prompts: the connection is up, or has failed.
    ended: Cell<bool>,
    /// The reader has stopped prompting.
    done: Cell<bool>,
    done_notify: tokio::sync::Notify,
    /// The connection failed: what is left is for the warning, not for
    /// standard error.
    failed: Cell<bool>,
}

impl Requests {
    fn extract(&self) -> Vec<u8> {
        std::mem::take(&mut *self.scrollback.borrow_mut())
    }

    /// `closeInput`.
    fn close_input(&self) {
        if !self.ended.replace(true) {
            (self.interact.borrow_mut().end_input)();
        }
    }

    /// `promptUser`: what ssh printed since, unless it is blank.
    fn prompt_user(self: &Rc<Self>) {
        let query = self.extract();
        if matches!(query.as_slice(), b"\r\n" | b"\n" | b"\r") {
            return;
        }
        let text = process_escapes(&query);
        if text.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let me = self.clone();
        let respond: Respond = Box::new(move |s: String| {
            if !me.ended.get() {
                me.term.answer(&s);
            }
        });
        let text = String::from_utf8_lossy(&text).into_owned();
        (self.interact.borrow_mut().user_input)(text, respond);
    }
}

/// The ways to stop asking: the connection is up, or it failed.
pub struct PasswordRequests(Rc<Requests>);

impl PasswordRequests {
    /// `connectionReady`: no more prompts; what ssh printed and wasn't
    /// shown, for standard error. What it prints from now on goes there.
    pub fn ready(&self) -> String {
        self.0.close_input();
        String::from_utf8_lossy(&self.0.extract()).into_owned()
    }

    /// `extractRemainingOutput`: no more prompts; what ssh printed, after
    /// a last chance to print more (0.3 s), for the warning.
    pub async fn remaining(&self) -> String {
        let r = &self.0;
        r.failed.set(true);
        r.close_input();
        if !r.done.get() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(300),
                r.done_notify.notified(),
            )
            .await;
        }
        let text = process_escapes(&r.extract());
        String::from_utf8_lossy(&text).trim().to_owned()
    }
}

/// `handlePasswordRequests`: what ssh prints at `term` is shown with
/// `interact` (50 ms after it comes, unless the connection is up by
/// then), and the answers written back, until [`PasswordRequests::ready`]
/// or [`PasswordRequests::remaining`]. After `ready`, what ssh prints
/// goes to standard error (`forwardShellStderr`).
pub fn handle_password_requests(term: Rc<Terminal>, interact: Interact) -> PasswordRequests {
    let r = Rc::new(Requests {
        term: term.clone(),
        interact: RefCell::new(interact),
        scrollback: RefCell::default(),
        ended: Cell::new(false),
        done: Cell::new(false),
        done_notify: tokio::sync::Notify::new(),
        failed: Cell::new(false),
    });
    let Some(mut chunks) = term.input.borrow_mut().take() else {
        return PasswordRequests(r);
    };
    let me = r.clone();
    crate::runtime::spawn_local(async move {
        let r = me;
        while let Some(chunk) = chunks.recv().await {
            r.scrollback.borrow_mut().extend_from_slice(&chunk);
            if r.ended.get() {
                break;
            }
            let later = r.clone();
            crate::runtime::spawn_local(async move {
                // Time for the connection to be found up.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                if !later.ended.get() {
                    later.prompt_user();
                }
            });
        }
        r.done.set(true);
        r.done_notify.notify_waiters();
        if r.failed.get() {
            return;
        }
        // `forwardShellStderr`: from now on, the client's standard error.
        use std::io::Write;
        let _ = std::io::stderr().write_all(&r.extract());
        while let Some(chunk) = chunks.recv().await {
            let _ = std::io::stderr().write_all(&chunk);
        }
    });
    PasswordRequests(r)
}
