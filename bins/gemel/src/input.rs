//! What the text interface waits for, and reading keys.
//!
//! The interface waits on one channel for its inputs: the session's
//! events, the keys typed and the signals caught. Keys are read on a
//! thread of their own, one read at a time when asked for: a line from a
//! dumb terminal (`input_line`), or in raw mode what one read returns
//! (`readInput`). This module also has `getInput`'s tokens and the names
//! of the keys as the help shows them.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use std::io::{self, BufRead};
use std::sync::Arc;
use std::sync::mpsc;

use gemel_fs::signal::Signal;
use gemel_fs::terminal::{self, TerminalState};
use gemel_ui_api::session::Event;

/// One of the inputs.
pub enum Input {
    /// An event from the session.
    Event(Box<Event>),
    /// The session has ended.
    Ended,
    /// What a read gave: `None` at the end of the input.
    Key(io::Result<Option<Vec<u8>>>),
    /// A signal caught.
    Signal(Signal),
}

/// How to read.
pub enum Read {
    /// A line, without its end (`input_line`).
    Line,
    /// One read of at most 9 bytes, between `startReading` and
    /// `stopReading`.
    Raw(Arc<TerminalState>),
}

/// The thread that reads stdin when asked to, and sends what it reads to
/// `inputs`.
pub fn spawn_reader(inputs: mpsc::Sender<Input>) -> io::Result<mpsc::Sender<Read>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gemel-keys".into())
        .spawn(move || {
            for r in rx {
                let got = match r {
                    Read::Line => read_line(),
                    Read::Raw(t) => {
                        let _ = t.start_reading();
                        // This should suffice to fit a complete escape
                        // sequence.
                        let mut b = [0u8; 9];
                        let n = terminal::read_stdin(&mut b);
                        let _ = t.stop_reading();
                        n.map(|n| (n > 0).then(|| b[..n].to_vec()))
                    }
                };
                if inputs.send(Input::Key(got)).is_err() {
                    return;
                }
            }
        })?;
    Ok(tx)
}

fn read_line() -> io::Result<Option<Vec<u8>>> {
    let mut l = Vec::new();
    if io::stdin().lock().read_until(b'\n', &mut l)? == 0 {
        return Ok(None);
    }
    if l.last() == Some(&b'\n') {
        l.pop();
        // A text channel on Windows.
        if cfg!(windows) && l.last() == Some(&b'\r') {
            l.pop();
        }
    }
    Ok(Some(l))
}

/// `nextInputToken`'s split: the first token of `s`, and the rest. A token
/// is a byte of US-ASCII or Latin-1, a character in UTF-8, or an escape
/// sequence, taken to run to the next escape or the end of the read.
pub fn split_token(s: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let utf8 = |n: usize| s.len() >= n && std::str::from_utf8(&s[..n]).is_ok();
    let n = match s.first() {
        None => 0,
        Some(0x1b) => s[1..]
            .iter()
            .position(|b| *b == 0x1b)
            .map_or(s.len(), |i| i + 1),
        Some(c) if *c < 0x80 => 1,
        Some(c) if *c < 0xe0 && utf8(2) => 2,
        Some(c) if *c < 0xf0 && utf8(3) => 3,
        _ if utf8(4) => 4,
        _ => 1,
    };
    (s[..n].to_vec(), s[n..].to_vec())
}

/// `getInput`'s token as a command: return is `""`, and bytes that aren't
/// UTF-8 are taken as Latin-1 (`Unicode.protect`).
pub fn token_string(t: &[u8]) -> String {
    match t {
        b"\0" => "(invalid input)".to_owned(),
        b"\n" | b"\r" => String::new(),
        t => match std::str::from_utf8(t) {
            Ok(s) => s.to_owned(),
            Err(_) => t.iter().map(|b| *b as char).collect(),
        },
    }
}

/// Escape.
pub const KEY_ESC: &str = "\x1b";
/// F1.
pub const KEY_F1: &str = "\x1bOP";
/// Delete.
pub const KEY_DELETE: &str = "\x1b[3~";
/// Home.
pub const KEY_HOME: &str = "\x1b[H";
/// End.
pub const KEY_END: &str = "\x1b[F";
/// Page up.
pub const KEY_PG_UP: &str = "\x1b[5~";
/// Page down.
pub const KEY_PG_DN: &str = "\x1b[6~";
/// Up.
pub const KEY_UP: &str = "\x1b[A";
/// Down.
pub const KEY_DN: &str = "\x1b[B";
/// Left.
pub const KEY_LEFT: &str = "\x1b[D";
/// Right.
pub const KEY_RIGHT: &str = "\x1b[C";
/// Tab.
pub const KEY_TAB: &str = "\t";
/// Shift-Tab.
pub const KEY_RV_TAB: &str = "\x1b[Z";

/// `formatname`: a key as the help names it.
pub fn format_name(n: &str) -> String {
    match n {
        "" => "<ret>".to_owned(),
        " " => "<spc>".to_owned(),
        "\x7f" | KEY_DELETE => "<del>".to_owned(),
        "\x08" => "<bsp>".to_owned(),
        KEY_TAB => "<tab>".to_owned(),
        KEY_RV_TAB => "<shift+tab>".to_owned(),
        KEY_ESC => "<esc>".to_owned(),
        KEY_UP => "<up>".to_owned(),
        KEY_DN => "<down>".to_owned(),
        KEY_LEFT => "<left>".to_owned(),
        KEY_RIGHT => "<right>".to_owned(),
        KEY_PG_UP => "<pg up>".to_owned(),
        KEY_PG_DN => "<pg down>".to_owned(),
        KEY_HOME => "<home>".to_owned(),
        KEY_END => "<end>".to_owned(),
        n if n.starts_with('\x1b') => format!("^{}", n.replace('\x1b', "[")),
        n => n.to_owned(),
    }
}

/// `String.escaped`.
pub fn escaped(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            b'\x08' => out.push_str("\\b"),
            b' '..=b'~' => out.push(b as char),
            b => out.push_str(&format!("\\{b:03}")),
        }
    }
    out
}

/// `Unix.error_message`: the system's message, without Rust's code.
pub fn error_message(e: &io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_split_into_tokens() {
        let split = |s: &[u8]| {
            let mut s = s.to_vec();
            let mut out = Vec::new();
            while !s.is_empty() {
                let (t, rest) = split_token(&s);
                out.push(t);
                s = rest;
            }
            out
        };
        assert_eq!(split(b"nnq"), [b"n", b"n", b"q"]);
        assert_eq!(split(b"\x1b[A\x1b[B"), [&b"\x1b[A"[..], b"\x1b[B"]);
        assert_eq!(split(b"\x1b[Ax"), [b"\x1b[Ax"]);
        assert_eq!(split("é!".as_bytes()), ["é".as_bytes(), b"!"]);
        assert_eq!(split(b"\xe9!"), [b"\xe9", b"!"]);
        assert_eq!(split_token(b""), (Vec::new(), Vec::new()));
    }

    #[test]
    fn tokens_are_commands() {
        assert_eq!(token_string(b"\r"), "");
        assert_eq!(token_string(b"\0"), "(invalid input)");
        assert_eq!(token_string(b"\xe9"), "é");
    }

    #[test]
    fn keys_are_named_and_escaped_as_ocaml_does() {
        assert_eq!(format_name(""), "<ret>");
        assert_eq!(format_name(KEY_PG_DN), "<pg down>");
        assert_eq!(format_name("\x1b[1;2A"), "^[[1;2A");
        assert_eq!(escaped("a\"\x01é"), "a\\\"\\001\\195\\169");
    }
}
