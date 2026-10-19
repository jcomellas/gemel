//! Parsing of root specifications (local, ssh://, socket://).
//!
//! Ports Unison `src/clroot.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Roots on the command line and in profiles follow a URI-like grammar:
//!
//! ```text
//! replica  ::= [protocol:]//[user@][host][:port][/path]
//!           |  path
//! protocol ::= file | socket | ssh
//! user     ::= [-_a-zA-Z0-9.%@]+
//! host     ::= [-_a-zA-Z0-9%.]+
//!           |  \[ [a-f0-9:.]+ zone? \]     IPv6 literal
//!           |  { [^}]+ }                   Unix domain socket (socket only)
//! zone     ::= %[-_a-zA-Z0-9~%.]+
//! port     ::= [^/]+
//! ```
//!
//! The OCaml uses `Str` regular expressions; the matchers below reproduce
//! what those expressions match, including their backtracking.

use std::fmt;

use crate::util::trim_whitespace;

/// A root as given by the user (`Clroot.clroot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clroot {
    /// A local directory (`None` is the current directory).
    ConnectLocal(Option<String>),
    /// A remote replica reached by running a shell command.
    ConnectByShell {
        /// The command, `"ssh"`.
        shell: String,
        /// The host.
        host: String,
        /// The user to log in as.
        user: Option<String>,
        /// The port.
        port: Option<String>,
        /// The replica's root on the host.
        path: Option<String>,
    },
    /// A remote replica reached over a TCP or Unix domain socket.
    ConnectBySocket {
        /// The host, or `{path}` for a Unix domain socket.
        host: String,
        /// The port; empty for a Unix domain socket.
        port: String,
        /// The replica's root on the host.
        path: Option<String>,
    },
}

/// Why a root specification was rejected, with the exception OCaml raises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClrootError {
    /// `Util.Fatal`.
    Fatal(String),
    /// `Util.Transient`.
    Transient(String),
    /// `Prefs.IllegalValue`.
    IllegalValue(String),
    /// `Invalid_argument`: a deprecated or unknown protocol.
    InvalidArgument(String),
}

impl fmt::Display for ClrootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClrootError::Fatal(m)
            | ClrootError::Transient(m)
            | ClrootError::IllegalValue(m)
            | ClrootError::InvalidArgument(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ClrootError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    File,
    Socket,
    Ssh,
}

/// Length of the longest prefix of `s` whose characters satisfy `class`.
fn run(s: &str, class: impl Fn(u8) -> bool) -> usize {
    s.bytes().take_while(|&c| class(c)).count()
}

fn letters(s: &str) -> usize {
    run(s, |c| c.is_ascii_alphabetic())
}

/// `getProtocolSlashSlash`.
fn get_protocol_slash_slash(s: &str) -> Result<Option<(Protocol, &str)>, ClrootError> {
    let n = letters(s);
    // [a-zA-Z]+://
    if n > 0 && s[n..].starts_with("://") {
        let protocol = match &s[..n] {
            "file" => Protocol::File,
            "socket" => Protocol::Socket,
            "ssh" => Protocol::Ssh,
            "rsh" => {
                return Err(ClrootError::InvalidArgument(
                    "protocol rsh has been deprecated, use ssh instead (optionally specifying a \
                     different sshcmd preference)"
                        .into(),
                ));
            }
            "unison" => {
                return Err(ClrootError::InvalidArgument(
                    "protocol unison has been deprecated, use file, ssh, or socket instead".into(),
                ));
            }
            name => {
                return Err(ClrootError::InvalidArgument(format!(
                    "\"{s}\": unrecognized protocol {name}"
                )));
            }
        };
        return Ok(Some((protocol, &s[n + 3..])));
    }
    // //
    if let Some(rest) = s.strip_prefix("//") {
        return Ok(Some((Protocol::File, rest)));
    }
    // [a-zA-Z]+:
    if n > 0 && s[n..].starts_with(':') {
        let matched = &s[..=n];
        if matches!(matched, "file:" | "ssh:" | "socket:") {
            return Err(ClrootError::Fatal(format!(
                "ill-formed root specification \"{s}\" ({matched} must be followed by //)"
            )));
        }
    }
    Ok(None)
}

/// `getUser`: `[-_a-zA-Z0-9.%@]+@`, which backtracks to the last `@` that
/// has at least one character before it.
fn get_user(s: &str) -> (Option<String>, &str) {
    let n = run(s, |c| c.is_ascii_alphanumeric() || b"-_.%@".contains(&c));
    match s[..n].rfind('@') {
        Some(at) if at >= 1 => (Some(s[..at].to_owned()), &s[at + 1..]),
        _ => (None, s),
    }
}

/// `getHost`: a host name, `{socket path}` or `[IPv6 literal]` (returned
/// without the brackets).
fn get_host(s: &str) -> (Option<String>, &str) {
    let name = run(s, |c| c.is_ascii_alphanumeric() || b"-_%.".contains(&c));
    if name > 0 {
        return (Some(s[..name].to_owned()), &s[name..]);
    }
    if let Some(inner) = s.strip_prefix('{') {
        let n = run(inner, |c| c != b'}');
        if n > 0 && inner[n..].starts_with('}') {
            return (Some(s[..n + 2].to_owned()), &s[n + 2..]);
        }
    }
    if let Some(inner) = s.strip_prefix('[') {
        let mut n = run(
            inner,
            |c| matches!(c, b'a'..=b'f' | b'0'..=b'9' | b':' | b'.'),
        );
        if n > 0 {
            if inner[n..].starts_with('%') {
                let zone = run(&inner[n + 1..], |c| {
                    c.is_ascii_alphanumeric() || b"-_~%.".contains(&c)
                });
                if zone > 0 && inner[n + 1 + zone..].starts_with(']') {
                    n += 1 + zone;
                }
            }
            if inner[n..].starts_with(']') {
                return (Some(inner[..n].to_owned()), &s[n + 2..]);
            }
        }
    }
    (None, s)
}

/// `getPort`: `:[^/]+`.
fn get_port(s: &str) -> (Option<String>, &str) {
    if let Some(rest) = s.strip_prefix(':') {
        let n = run(rest, |c| c != b'/');
        if n > 0 {
            return (Some(rest[..n].to_owned()), &rest[n..]);
        }
    }
    (None, s)
}

type Uri = (
    Protocol,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// `parseUri`: protocol, user, host, port and path (non-empty if present).
fn parse_uri(s: &str) -> Result<Uri, ClrootError> {
    let s = trim_whitespace(s);
    let Some((protocol, s0)) = get_protocol_slash_slash(s)? else {
        return Ok((Protocol::File, None, None, None, Some(s.to_owned())));
    };
    let (user, s1) = get_user(s0);
    let (host, s2) = get_host(s1);
    let (port, s3) = get_port(s2);
    let path = match s3.strip_prefix('/') {
        _ if s3.is_empty() => None,
        Some("") => None,
        Some(p) => Some(p.to_owned()),
        None => {
            return Err(ClrootError::Fatal(format!(
                "ill-formed root specification {s}"
            )));
        }
    };
    Ok((protocol, user, host, port, path))
}

/// `parseHostPort`: splits `host:port`.
fn parse_host_port(s: &str) -> Result<(String, Option<String>), ClrootError> {
    let (host, s1) = get_host(s);
    let (port, s2) = get_port(s1);
    if !s2.is_empty() {
        return Err(ClrootError::Transient(format!(
            "ill-formed host specification {s}"
        )));
    }
    Ok((host.unwrap_or_default(), port))
}

/// `Clroot.parseRoot`.
pub fn parse_root(string: &str) -> Result<Clroot, ClrootError> {
    let illegal = |s: &str| Err(ClrootError::IllegalValue(format!("\"{string}\": {s}")));
    let (protocol, user, host, port, path) = parse_uri(string)?;
    use Protocol::*;
    match (protocol, &user, &host, &port) {
        (_, _, None, Some(_))
        | (_, Some(_), None, None)
        | (Socket, _, None, None)
        | (Ssh, _, None, _) => illegal("missing host"),
        (File, _, _, Some(_)) => illegal("ill-formed (cannot use a port number with file)"),
        (File, _, Some(h), None) => {
            let prefix = format!("//{h}/");
            Ok(Clroot::ConnectLocal(Some(match path {
                None => prefix,
                Some(p) => prefix + &p,
            })))
        }
        (File, None, None, None) => Ok(Clroot::ConnectLocal(path)),
        (Socket, None, Some(h), Some(p)) if !h.starts_with('{') => Ok(Clroot::ConnectBySocket {
            host: h.clone(),
            port: p.clone(),
            path,
        }),
        (Socket, None, Some(h), None) if h.starts_with('{') => Ok(Clroot::ConnectBySocket {
            host: h.clone(),
            port: String::new(),
            path,
        }),
        (Socket, Some(_), _, _) => illegal("ill-formed (cannot use a user with socket)"),
        (Socket, _, _, None) => illegal("ill-formed (must give a port number with socket)"),
        (Socket, _, Some(_), Some(_)) => {
            illegal("ill-formed (must not give a port number with Unix domain socket)")
        }
        (Ssh, _, Some(h), _) => Ok(Clroot::ConnectByShell {
            shell: "ssh".into(),
            host: h.clone(),
            user,
            port,
            path,
        }),
    }
}

/// `Clroot.fixHost`: splits a port out of a host given without one (as
/// happens for roots built from separate host and port settings).
pub fn fix_host(root: Clroot) -> Result<Clroot, ClrootError> {
    Ok(match root {
        Clroot::ConnectBySocket { host, port, path } if port.is_empty() => {
            let (host, port) = parse_host_port(&host)?;
            Clroot::ConnectBySocket {
                host,
                port: port.unwrap_or_default(),
                path,
            }
        }
        Clroot::ConnectByShell {
            shell,
            host,
            user,
            port: None,
            path,
        } => {
            let (host, port) = parse_host_port(&host)?;
            Clroot::ConnectByShell {
                shell,
                host,
                user,
                port,
                path,
            }
        }
        other => other,
    })
}

/// `Clroot.clroot2string`: the root as it would be written.
impl fmt::Display for Clroot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Clroot::ConnectLocal(None) => f.write_str("."),
            Clroot::ConnectLocal(Some(s)) if s.is_empty() => f.write_str("."),
            Clroot::ConnectLocal(Some(s)) => {
                // cannotAbbrevFileRx and networkNameRx
                if ["file:", "ssh:", "socket:"]
                    .iter()
                    .any(|p| s.starts_with(p))
                {
                    if s.starts_with("//") {
                        write!(f, "file:{s}")
                    } else {
                        write!(f, "file:///{s}")
                    }
                } else {
                    f.write_str(s)
                }
            }
            Clroot::ConnectBySocket { host, port, path } => {
                let port = if port.is_empty() {
                    String::new()
                } else {
                    format!(":{port}")
                };
                let host = if host.contains(':') && !host.starts_with('{') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                write!(f, "socket://{host}{port}/{}", path.as_deref().unwrap_or(""))
            }
            Clroot::ConnectByShell {
                shell,
                host,
                user,
                port,
                path,
            } => {
                let user = user.as_ref().map_or(String::new(), |u| format!("{u}@"));
                let port = port.as_ref().map_or(String::new(), |p| format!(":{p}"));
                let host = if host.contains(':') {
                    format!("[{host}]")
                } else {
                    host.clone()
                };
                write!(
                    f,
                    "{shell}://{user}{host}{port}/{}",
                    path.as_deref().unwrap_or("")
                )
            }
        }
    }
}

/// Removed preferences that clroot.ml marks (`sshversion`, gone since 2.52).
pub const REMOVED_PREFS: &[&str] = &["sshversion"];
