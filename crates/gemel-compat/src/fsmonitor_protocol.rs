//! Line protocol spoken between Unison and the `unison-fsmonitor` helper.
//!
//! Ports Unison `src/fswatch.ml`, Unison `src/fsmonitor/watchercommon.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//! Derived from Unison, Copyright 2012, Benjamin C. Pierce.
//!
//! Unison starts the watcher as a child process and talks to it over its
//! stdin and stdout. Every message is one line: a word, a space, and
//! space-separated arguments, each percent-encoded ([`quote`]). Both sides
//! start by sending `VERSION 1`. Then, from Unison ([`Command`]):
//!
//! - `START hash fspath path`: start watching part of a replica, answered
//!   with `OK`; then `DIR path` and `LINK path` (each answered with `OK`)
//!   for the directories and followed links scanned, and `DONE`;
//! - `CHANGES hash`: answered with `RECURSIVE path` lines and `DONE`;
//! - `WAIT hash`: the watcher sends `CHANGES hash...` once some of the
//!   replicas waited for have changed; any other command cancels the waits;
//! - `RESET hash`: stop watching a replica;
//! - `DEBUG`: enable debugging output.
//!
//! The watcher can send `ERROR msg` at any time ([`Response`]). The
//! arguments are byte strings (hashes, and paths that need not be UTF-8).

use std::fmt;

/// Protocol version exchanged in the `VERSION` handshake with the watcher
/// (see `fswatch.ml`). Neither side checks the other's number.
pub const PROTOCOL_VERSION: u32 = 1;

/// Bytes sent as they are; everything else is percent-encoded.
fn allowed(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"-_.~!*'();:@&=+$,/?#[]".contains(&c)
}

/// Percent-encodes an argument (`quote`), with uppercase hex digits.
pub fn quote(s: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(s.len());
    for &c in s {
        if allowed(c) {
            out.push(c);
        } else {
            out.extend_from_slice(&[b'%', HEX[usize::from(c >> 4)], HEX[usize::from(c & 15)]]);
        }
    }
    out
}

/// A malformed percent-encoding (OCaml raises `Invalid_argument`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadQuoting;

impl fmt::Display for BadQuoting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("malformed percent-encoding")
    }
}

impl std::error::Error for BadQuoting {}

/// Decodes an argument (`unquote`).
///
/// Follows the OCaml exactly: the output is as long as the input minus two
/// bytes per `%`, and decoding stops there. So a `%` too close to the end
/// truncates the result instead of failing (`"a%4"` gives `"a"`), and more
/// `%`s than fit is an error.
pub fn unquote(s: &[u8]) -> Result<Vec<u8>, BadQuoting> {
    let n = s.iter().filter(|&&c| c == b'%').count();
    let len = s.len().checked_sub(2 * n).ok_or(BadQuoting)?;
    let hex = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(BadQuoting),
    };
    let mut out = Vec::with_capacity(len);
    let mut j = 0;
    while out.len() < len {
        if s[j] == b'%' {
            let (hi, lo) = (
                *s.get(j + 1).ok_or(BadQuoting)?,
                *s.get(j + 2).ok_or(BadQuoting)?,
            );
            out.push((hex(hi)? << 4) + hex(lo)?);
            j += 3;
        } else {
            out.push(s[j]);
            j += 1;
        }
    }
    Ok(out)
}

/// Splits at the first space (`split_on_space`); the rest is empty if there
/// is none.
pub fn split_on_space(s: &[u8]) -> (&[u8], &[u8]) {
    match s.iter().position(|&c| c == b' ') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, b""),
    }
}

/// A message from Unison to the watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `VERSION n`.
    Version(u32),
    /// `DEBUG`.
    Debug,
    /// `START hash fspath path`.
    Start {
        /// The archive hash identifying the replica.
        hash: Vec<u8>,
        /// The replica's root.
        fspath: Vec<u8>,
        /// Where scanning starts, relative to `fspath`.
        path: Vec<u8>,
    },
    /// `DIR path`, relative to the `START` location.
    Dir(Vec<u8>),
    /// `LINK path`, relative to the `START` location.
    Link(Vec<u8>),
    /// `DONE`: the end of a `START` sequence.
    Done,
    /// `WAIT hash`.
    Wait(Vec<u8>),
    /// `CHANGES hash`.
    Changes(Vec<u8>),
    /// `RESET hash`.
    Reset(Vec<u8>),
}

/// A message from the watcher to Unison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// `VERSION n`.
    Version(u32),
    /// `OK`.
    Ok,
    /// `ERROR msg`.
    Error(Vec<u8>),
    /// `RECURSIVE path`: a changed path, relative to the replica root.
    Recursive(Vec<u8>),
    /// `DONE`: the end of the answer to `CHANGES hash`.
    Done,
    /// `CHANGES hash...`: these waited-for replicas have changed.
    Changes(Vec<Vec<u8>>),
}

fn line(word: &str, args: &[&[u8]]) -> Vec<u8> {
    let mut out = word.as_bytes().to_vec();
    for a in args {
        out.push(b' ');
        out.extend(quote(a));
    }
    out.push(b'\n');
    out
}

/// Why a line couldn't be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// An unknown first word (the watcher answers `ERROR unknown command`).
    Unknown(Vec<u8>),
    /// A malformed argument.
    BadQuoting,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Unknown(w) => write!(f, "unknown command '{}'", String::from_utf8_lossy(w)),
            ParseError::BadQuoting => BadQuoting.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<BadQuoting> for ParseError {
    fn from(_: BadQuoting) -> Self {
        ParseError::BadQuoting
    }
}

/// The version number in `VERSION n`; neither side checks it, so anything
/// unparsable counts as 0.
fn version(args: &[u8]) -> u32 {
    std::str::from_utf8(args)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Removes the line terminator, if any.
fn chomp(l: &[u8]) -> &[u8] {
    l.strip_suffix(b"\n").unwrap_or(l)
}

impl Command {
    /// The line to send, with its newline.
    pub fn to_line(&self) -> Vec<u8> {
        match self {
            Command::Version(n) => format!("VERSION {n}\n").into_bytes(),
            Command::Debug => b"DEBUG\n".to_vec(),
            Command::Start { hash, fspath, path } => line("START", &[hash, fspath, path]),
            Command::Dir(p) => line("DIR", &[p]),
            Command::Link(p) => line("LINK", &[p]),
            Command::Done => b"DONE\n".to_vec(),
            Command::Wait(h) => line("WAIT", &[h]),
            Command::Changes(h) => line("CHANGES", &[h]),
            Command::Reset(h) => line("RESET", &[h]),
        }
    }

    /// Parses a line (with or without its newline), as the watcher does.
    pub fn parse(l: &[u8]) -> Result<Command, ParseError> {
        let (cmd, args) = split_on_space(chomp(l));
        Ok(match cmd {
            b"VERSION" => Command::Version(version(args)),
            b"DEBUG" => Command::Debug,
            b"START" => {
                let (hash, rem) = split_on_space(args);
                let (fspath, path) = split_on_space(rem);
                Command::Start {
                    hash: unquote(hash)?,
                    fspath: unquote(fspath)?,
                    path: unquote(path)?,
                }
            }
            b"DIR" => Command::Dir(unquote(args)?),
            b"LINK" => Command::Link(unquote(args)?),
            b"DONE" => Command::Done,
            b"WAIT" => Command::Wait(unquote(args)?),
            b"CHANGES" => Command::Changes(unquote(args)?),
            b"RESET" => Command::Reset(unquote(args)?),
            other => return Err(ParseError::Unknown(other.to_vec())),
        })
    }
}

impl Response {
    /// The line to send, with its newline.
    pub fn to_line(&self) -> Vec<u8> {
        match self {
            Response::Version(n) => format!("VERSION {n}\n").into_bytes(),
            Response::Ok => b"OK\n".to_vec(),
            Response::Error(m) => line("ERROR", &[m]),
            Response::Recursive(p) => line("RECURSIVE", &[p]),
            Response::Done => b"DONE\n".to_vec(),
            Response::Changes(hashes) => {
                // "CHANGES %s" with the quoted hashes joined by spaces, so an
                // empty set still has the space.
                let quoted: Vec<Vec<u8>> = hashes.iter().map(|h| quote(h)).collect();
                [b"CHANGES ".to_vec(), quoted.join(&b' '), b"\n".to_vec()].concat()
            }
        }
    }

    /// Parses a line (with or without its newline), as Unison does.
    pub fn parse(l: &[u8]) -> Result<Response, ParseError> {
        let (cmd, args) = split_on_space(chomp(l));
        Ok(match cmd {
            b"VERSION" => Response::Version(version(args)),
            b"OK" => Response::Ok,
            b"ERROR" => Response::Error(unquote(args)?),
            b"RECURSIVE" => Response::Recursive(unquote(args)?),
            b"DONE" => Response::Done,
            b"CHANGES" => Response::Changes(
                args.split(|&c| c == b' ')
                    .filter(|h| !h.is_empty())
                    .map(unquote)
                    .collect::<Result<_, _>>()?,
            ),
            other => return Err(ParseError::Unknown(other.to_vec())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_round_trips_every_byte() {
        let all: Vec<u8> = (0..=255).collect();
        let q = quote(&all);
        assert!(!q.contains(&b' ') && !q.contains(&b'\n'));
        assert_eq!(unquote(&q), Ok(all));
        assert_eq!(quote(b"a b\n%"), b"a%20b%0A%25");
    }

    #[test]
    fn unquote_keeps_ocamls_truncation() {
        assert_eq!(unquote(b"a%4"), Ok(b"a".to_vec()));
        assert_eq!(unquote(b"%"), Err(BadQuoting));
        assert_eq!(unquote(b"%zz"), Err(BadQuoting));
        assert_eq!(unquote(b"%e9"), Ok(vec![0xe9]));
    }

    #[test]
    fn commands_and_responses_round_trip() {
        let commands = [
            Command::Version(1),
            Command::Debug,
            Command::Start {
                hash: b"abc".to_vec(),
                fspath: b"/home/me/My Docs".to_vec(),
                path: b"".to_vec(),
            },
            Command::Dir(b"a b/c".to_vec()),
            Command::Link(b"l".to_vec()),
            Command::Done,
            Command::Wait(b"h".to_vec()),
            Command::Changes(b"h".to_vec()),
            Command::Reset(b"h".to_vec()),
        ];
        for c in commands {
            assert_eq!(Command::parse(&c.to_line()), Ok(c));
        }
        let responses = [
            Response::Version(1),
            Response::Ok,
            Response::Error(b"no inotify".to_vec()),
            Response::Recursive(b"x/y z".to_vec()),
            Response::Done,
            Response::Changes(vec![b"h1".to_vec(), b"h 2".to_vec()]),
            Response::Changes(vec![]),
        ];
        assert_eq!(Response::Changes(vec![]).to_line(), b"CHANGES \n");
        for r in responses {
            assert_eq!(Response::parse(&r.to_line()), Ok(r));
        }
        assert_eq!(
            Command::Start {
                hash: b"h".to_vec(),
                fspath: b"/r".to_vec(),
                path: b"p q".to_vec()
            }
            .to_line(),
            b"START h /r p%20q\n"
        );
        assert_eq!(
            Command::parse(b"FOO x"),
            Err(ParseError::Unknown(b"FOO".to_vec()))
        );
    }
}
