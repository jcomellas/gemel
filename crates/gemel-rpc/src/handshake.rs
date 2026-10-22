//! The connection header and the RPC version handshake.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Before a connection's first packet, the two sides agree on an RPC
//! version in lines of text:
//! 1. the server sends the connection header ([`CONNECTION_HEADER`]) and
//!    the versions it speaks (`VERSIONS 1`);
//! 2. the client checks the header, picks a version they both speak and
//!    sends it (`VERSION 1`);
//! 3. the server accepts it (`OK`), or refuses it with a reason
//!    (`NOK <reason>`).
//!
//! The lines are read from the buffer the connection then reads its
//! packets from ([`Reader`]), so that nothing the other side sent right
//! after them is lost. A `\r` is skipped wherever it is, as Windows ends
//! lines with it.
//!
//! The server's side is here; the client's comes later in milestone 3.

use std::pin::Pin;

use gemel_base::util::{self, Error, escaped};
use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::connection::{BUFFER_SIZE, Input, Output, lost};
use crate::protocol::RPC_SUPPORTED_VERSIONS;

/// What the handshake, then the connection, reads from (OCaml's
/// `inputBuffer`).
pub type Reader = BufReader<Input>;

/// A [`Reader`] over `input`, with OCaml's buffer size.
pub fn reader(input: Input) -> Reader {
    BufReader::with_capacity(BUFFER_SIZE, input)
}

/// `connectionHeader`: the first line a server sends.
pub const CONNECTION_HEADER: &str = "Unison RPC\n";

const RPC_OK: &str = "OK\n";
const RPC_NOK_TAG: &str = "NOK ";
const RPC_VERSIONS_TAG: &str = "VERSIONS ";
const RPC_VERSION_TAG: &str = "VERSION ";

/// `rpcSupportedVersionStr`, for messages: `"1"`, each quoted, separated
/// by commas.
fn supported_versions() -> String {
    let quoted: Vec<String> = RPC_SUPPORTED_VERSIONS
        .iter()
        .map(|v| format!("\"{v}\""))
        .collect();
    quoted.join(", ")
}

/// `rpcVersionsStr`: the line with the versions a server speaks.
pub fn rpc_versions_line() -> String {
    let versions: Vec<String> = RPC_SUPPORTED_VERSIONS
        .iter()
        .map(ToString::to_string)
        .collect();
    format!("{RPC_VERSIONS_TAG}{}\n", versions.join(" "))
}

/// `verIsSupported`.
fn is_supported(version: i64) -> bool {
    RPC_SUPPORTED_VERSIONS
        .iter()
        .any(|v| i64::from(*v) == version)
}

/// `receiveUntilSep`: the bytes up to a newline, or with `space` up to a
/// space, a tab or a newline; the separator is included with
/// `include_sep`. A `\r` is skipped.
async fn receive_until_sep(
    r: &mut Reader,
    space: bool,
    include_sep: bool,
) -> Result<Vec<u8>, Error> {
    let mut inp = Vec::new();
    loop {
        match r.read_u8().await.map_err(|_| lost())? {
            c @ (b' ' | b'\t') if space => {
                if include_sep {
                    inp.push(c);
                }
                return Ok(inp);
            }
            b'\n' => {
                if include_sep {
                    inp.push(b'\n');
                }
                return Ok(inp);
            }
            b'\r' => {}
            c => inp.push(c),
        }
    }
}

/// `peekWithoutBlocking`: what has been received and not read yet.
fn rest(r: &Reader) -> &[u8] {
    r.buffer()
}

/// `receiveUntilNewlineNb`: what has been received, up to a newline
/// (excluded, and left unread), without waiting for more.
fn receive_until_newline_nb(r: &mut Reader) -> Vec<u8> {
    let buf = rest(r);
    let len = buf.iter().position(|c| *c == b'\n').unwrap_or(buf.len());
    let line = buf[..len].to_vec();
    Pin::new(r).consume(len);
    line
}

/// A line of the handshake (`handshakeData`).
enum Line {
    /// The line had the keyword expected; this is the rest of it.
    Data(Vec<u8>),
    /// `NOK`, with the reason.
    Error(Vec<u8>),
    /// Something else: its first word.
    Unknown(Vec<u8>),
}

/// `receiveHandshakeData`: a line starting with `keyword`, or `NOK`.
async fn receive_handshake_data(r: &mut Reader, keyword: &str) -> Result<Line, Error> {
    let msg = receive_until_sep(r, true, true).await?;
    if msg == keyword.as_bytes() {
        Ok(Line::Data(receive_until_sep(r, false, false).await?))
    } else if msg == RPC_NOK_TAG.as_bytes() {
        Ok(Line::Error(receive_until_newline_nb(r)))
    } else {
        Ok(Line::Unknown(msg))
    }
}

/// Writes handshake text; an error writing it is a lost connection.
async fn send(w: &mut Output, s: &str) -> Result<(), Error> {
    async {
        w.write_all(s.as_bytes()).await?;
        w.flush().await
    }
    .await
    .map_err(|_| lost())
}

/// `parseVersion`: a version number, or why it isn't one. `side` sent it.
fn parse_version(side: &str, s: &[u8]) -> Result<i64, String> {
    let error = |e: &str| {
        format!(
            "Unknown {side} RPC version: {e}. Version received from {side}: \"{}\". \
             Supported RPC versions: {}",
            escaped(s),
            supported_versions()
        )
    };
    if s.is_empty() {
        return Err(error("invalid format"));
    }
    std::str::from_utf8(s)
        .ok()
        .and_then(util::int_of_string)
        .ok_or_else(|| error("parse error"))
}

/// The server's first lines: the header and its versions.
pub async fn send_server_header(w: &mut Output) -> Result<(), Error> {
    send(w, &format!("{CONNECTION_HEADER}{}", rpc_versions_line())).await
}

/// `checkClientVersion`: reads the version the client chose and accepts
/// it, or refuses it with `NOK` and the reason. The version, if accepted.
///
/// A refusal doesn't end the connection: as in OCaml, the server goes on
/// to serve it, and the client, which has the reason, closes it.
pub async fn check_client_version(r: &mut Reader, w: &mut Output) -> Result<Option<u32>, Error> {
    let refuse = |reason: String| format!("{RPC_NOK_TAG}{reason}\n");
    let reply = match receive_handshake_data(r, RPC_VERSION_TAG).await? {
        Line::Error(msg) => Err(refuse(format!(
            "Could not negotiate RPC version. Received unexpected error from the client: \"{}\"",
            String::from_utf8_lossy(&msg)
        ))),
        Line::Unknown(from_client) => {
            let received = [&from_client[..], rest(r)].concat();
            Err(refuse(format!(
                "Could not negotiate RPC version. Received unexpected header from the client: \"{}\"",
                escaped(&received)
            )))
        }
        Line::Data(buf) => match parse_version("client", &buf) {
            Ok(v) if is_supported(v) => Ok(v),
            Ok(v) => Err(refuse(format!(
                "Client RPC version not supported. Version received from client: \"{v}\". \
                 Supported RPC versions: {}",
                supported_versions()
            ))),
            Err(e) => Err(refuse(e)),
        },
    };
    match reply {
        Ok(v) => {
            send(w, RPC_OK).await?;
            Ok(u32::try_from(v).ok())
        }
        Err(nok) => {
            send(w, &nok).await?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        assert_eq!(rpc_versions_line(), "VERSIONS 1\n");
        assert_eq!(supported_versions(), "\"1\"");
    }

    #[test]
    fn versions_parse_as_ocaml_ints() {
        assert_eq!(parse_version("client", b"1"), Ok(1));
        assert_eq!(parse_version("client", b"0x1"), Ok(1));
        assert_eq!(
            parse_version("client", b""),
            Err(
                "Unknown client RPC version: invalid format. Version received from \
                 client: \"\". Supported RPC versions: \"1\""
                    .to_owned()
            )
        );
        assert_eq!(
            parse_version("server", b"x\"y"),
            Err(
                "Unknown server RPC version: parse error. Version received from \
                 server: \"x\\\"y\". Supported RPC versions: \"1\""
                    .to_owned()
            )
        );
    }
}
