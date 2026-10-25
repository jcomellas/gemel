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
//! A 2.52 or 2.53 server reached on a socket, or by ssh without
//! `__new-rpc-mode`, starts in its compatibility mode for 2.51 clients: it
//! sends an old header, then a magic packet that a 2.51 client takes for a
//! harmless message. The client answers with the same packet, and both
//! start over with the handshake above (`checkServerUpgrade`).

use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use gemel_base::util::{self, Error, escaped};
use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::connection::{self, BUFFER_SIZE, Connection, Endpoint, Input, Output, lost};
use crate::protocol::{self, INT_SIZE, RPC_SUPPORTED_VERSIONS};

/// What the handshake, then the connection, reads from (OCaml's
/// `inputBuffer`).
pub type Reader = BufReader<Input>;

/// A [`Reader`] over `input`, with OCaml's buffer size.
pub fn reader(input: Input) -> Reader {
    BufReader::with_capacity(BUFFER_SIZE, input)
}

/// `connectionHeader`: the first line a server sends.
pub const CONNECTION_HEADER: &str = "Unison RPC\n";

/// `compatConnectionHeader`: what a 2.52 or 2.53 server in its
/// compatibility mode sends first.
const COMPAT_CONNECTION_HEADER: &str = "Unison 2.51 with OCaml >= 4.01.2\n";

/// `compat248ConnectionHeader`: what every release before the version
/// handshake sends first.
const COMPAT_248_CONNECTION_HEADER: &str = "Unison 2.48\n";

/// `rpcServerCmdlineOverride`: given to an ssh server, so that a 2.52 or
/// 2.53 server skips its compatibility mode.
pub const RPC_SERVER_CMDLINE_OVERRIDE: &str = "__new-rpc-mode";

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

// ---- The client's side ----

/// `handshakeFail`.
fn fail(e: String) -> Error {
    Error::Fatal(e)
}

/// `handshakeError`.
fn server_error(msg: &[u8]) -> Error {
    fail(format!(
        "Received error from the server: \"{}\".",
        String::from_utf8_lossy(msg)
    ))
}

/// `handshakeUnknown`.
fn server_unknown(msg: &[u8]) -> Error {
    fail(format!(
        "Received unexpected header from the server: \"{}\".",
        escaped(msg)
    ))
}

/// `peekWithBlocking`: what has been received and not read yet, waiting
/// for something if there is nothing; the end of the input is a lost
/// connection.
async fn peek(r: &mut Reader) -> Result<&[u8], Error> {
    use tokio::io::AsyncBufReadExt;
    match r.fill_buf().await {
        Ok([]) | Err(_) => Err(lost()),
        Ok(b) => Ok(b),
    }
}

/// `checkHeader`: reads the server's header, the current one or either
/// old one, a byte at a time, and returns it.
async fn check_header(r: &mut Reader) -> Result<&'static str, Error> {
    let mut header = CONNECTION_HEADER;
    let mut pos = 0;
    while pos < header.len() {
        let c = r.read_u8().await.map_err(|_| lost())?;
        let at = |h: &str| h.as_bytes().get(pos) == Some(&c);
        let (ok, compat_ok, compat_248_ok) = (
            at(header),
            at(COMPAT_CONNECTION_HEADER),
            at(COMPAT_248_CONNECTION_HEADER),
        );
        if !ok && !compat_ok && !compat_248_ok {
            let prefix = [&header.as_bytes()[..pos], &[c]].concat();
            let received = [&prefix[..], rest(r)].concat();
            return Err(fail(format!(
                "Received unexpected header from the server:\n expected \"{}\" but received \"{}\", \n\
                 which differs at \"{}\".\n\
                 This can happen because you have different versions of Unison\n\
                 installed on the client and server machines, or because\n\
                 your connection is failing and somebody is printing an error\n\
                 message, or because your remote login shell is printing\n\
                 something itself before starting Unison.",
                escaped(header.as_bytes()),
                escaped(&received),
                escaped(&prefix)
            )));
        }
        // The current header is almost a prefix of the old ones.
        if !ok && compat_ok {
            header = COMPAT_CONNECTION_HEADER;
        } else if !ok && compat_248_ok {
            header = COMPAT_248_CONNECTION_HEADER;
        }
        pos += 1;
    }
    Ok(header)
}

fn is_old(header: &str) -> bool {
    header == COMPAT_CONNECTION_HEADER || header == COMPAT_248_CONNECTION_HEADER
}

/// `magicId`: the id of the magic packet, which no other message has.
const MAGIC_ID: u32 = 1;

/// `magicPacket`: a stream abort, as OCaml before 4.13 marshalled it.
const MAGIC_PACKET: &[u8] = b"rsp\x84\x95\xa6\xbe\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0A";

/// `magic`: the magic packet, as a message.
fn magic() -> Vec<u8> {
    let len = u32::try_from(MAGIC_PACKET.len()).expect("a short packet");
    [
        &protocol::encode_int(MAGIC_ID)[..],
        &protocol::encode_int(len),
        MAGIC_PACKET,
    ]
    .concat()
}

/// Reads exactly `n` bytes.
async fn grab(r: &mut Reader, n: usize) -> Result<Vec<u8>, Error> {
    let mut b = vec![0; n];
    r.read_exact(&mut b).await.map_err(|_| lost())?;
    Ok(b)
}

/// `checkForMagicString`: whether the server sent the magic packet, which
/// is then read (its contents aren't checked).
async fn check_for_magic_string(r: &mut Reader) -> Result<bool, Error> {
    let b = peek(r).await?;
    if b.len() < INT_SIZE || protocol::decode_int(&b[..INT_SIZE])? != MAGIC_ID {
        return Ok(false);
    }
    grab(r, INT_SIZE).await?;
    let len = protocol::decode_int(&grab(r, INT_SIZE).await?)?;
    grab(r, len as usize).await?;
    Ok(true)
}

/// `checkServerUpgrade`: a server in its compatibility mode that sent the
/// magic packet gets it back, after its write token, and the handshake
/// starts over: the header it sends then.
async fn check_server_upgrade(
    r: &mut Reader,
    w: &mut Output,
    header: &'static str,
) -> Result<&'static str, Error> {
    if !is_old(header) || !check_for_magic_string(r).await? {
        return Ok(header);
    }
    grab(r, INT_SIZE).await?;
    async {
        w.write_all(&magic()).await?;
        w.flush().await
    }
    .await
    .map_err(|_| lost())?;
    check_header(r).await
}

/// `receiveHandshakeMsg`.
async fn receive_handshake_msg(r: &mut Reader) -> Result<Result<(), Line>, Error> {
    let msg = receive_until_sep(r, true, true).await?;
    if msg == RPC_OK.as_bytes() {
        Ok(Ok(()))
    } else if msg == RPC_NOK_TAG.as_bytes() {
        Ok(Err(Line::Error(receive_until_newline_nb(r))))
    } else {
        Ok(Err(Line::Unknown(msg)))
    }
}

/// `parseServerVersions`: the versions both sides speak, newest first.
fn parse_server_versions(inp: &[u8]) -> Result<Vec<u32>, Error> {
    let words: Vec<&[u8]> = inp.split(|c| *c == b' ').collect();
    if words == [b""] {
        parse_version("server", b"").map_err(fail)?;
    }
    let mut common = Vec::new();
    for v in words.into_iter().filter(|v| !v.is_empty()) {
        let v = parse_version("server", v).map_err(fail)?;
        if is_supported(v) {
            common.extend(u32::try_from(v).ok());
        }
    }
    common.sort_unstable_by(|a, b| b.cmp(a));
    Ok(common)
}

/// `selectServerVersion`: picks the newest version both sides speak, and
/// has the server accept it.
async fn select_server_version(r: &mut Reader, w: &mut Output) -> Result<u32, Error> {
    let versions = match receive_handshake_data(r, RPC_VERSIONS_TAG).await? {
        Line::Error(msg) => return Err(server_error(&msg)),
        Line::Unknown(from_server) => {
            return Err(server_unknown(&[&from_server[..], rest(r)].concat()));
        }
        Line::Data(versions) => versions,
    };
    let Some(&version) = parse_server_versions(&versions)?.first() else {
        return Err(fail(format!(
            "None of server's RPC versions are supported. The server may be too old or too \
             recent. Versions received from server: \"{}\". Supported RPC versions: {}",
            escaped(&versions),
            supported_versions()
        )));
    };
    send(w, &format!("{RPC_VERSION_TAG}{version}\n")).await?;
    match receive_handshake_msg(r).await? {
        Ok(()) => Ok(version),
        Err(Line::Error(reply)) => Err(server_error(&reply)),
        Err(Line::Unknown(reply)) | Err(Line::Data(reply)) => {
            Err(server_unknown(&[&reply[..], rest(r)].concat()))
        }
    }
}

/// `checkServerVersion`: an old header the server didn't upgrade from is
/// an old server.
async fn check_server_version(r: &mut Reader, w: &mut Output, header: &str) -> Result<u32, Error> {
    if is_old(header) {
        return Err(fail(format!(
            "The server runs an old and unsupported Unison version.\n\
             Version received from server: {header}\n\
             Please see the documentation about upgrading."
        )));
    }
    select_server_version(r, w).await
}

/// The client's side of the handshake (`initConnection`'s): waits for the
/// server to say something, then calls `ready` (the connection works),
/// then checks its header, upgrades it from its compatibility mode, and
/// agrees on a version: the version.
pub async fn client_handshake(
    r: &mut Reader,
    w: &mut Output,
    ready: impl FnOnce(),
) -> Result<u32, Error> {
    peek(r).await?;
    ready();
    let header = check_header(r).await?;
    let header = check_server_upgrade(r, w, header).await?;
    check_server_version(r, w, header).await
}

/// How long the handshake may take (`initConnection`'s timeout).
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(120);

/// `initConnection`: the client's side of the handshake on a transport's
/// `input` and `output`, within OCaml's 120 seconds, then the client's
/// side of a connection over them, served by `endpoint` (see
/// [`connection::start_client`]). If the handshake fails, the transport is
/// dropped, which closes it. `ready` runs once the server has written
/// something, before it is checked (`connReady`).
pub async fn init_connection(
    endpoint: Rc<Endpoint>,
    input: Input,
    mut output: Output,
    half_duplex: bool,
    ready: impl FnOnce(),
) -> Result<Connection, Error> {
    let mut input = reader(input);
    let version = tokio::time::timeout(
        NEGOTIATION_TIMEOUT,
        client_handshake(&mut input, &mut output, ready),
    )
    .await
    .unwrap_or_else(|_| {
        Err(Error::Fatal(
            "Timed out negotiating connection with the server".into(),
        ))
    })?;
    let conn = connection::start_client(endpoint, Box::new(input), output, half_duplex);
    conn.set_version(version);
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_packet_is_ocamls() {
        assert_eq!(MAGIC_PACKET.len(), 24);
        assert_eq!(&magic()[..INT_SIZE], protocol::encode_int(1));
    }

    #[test]
    fn server_versions_newest_first() {
        assert_eq!(parse_server_versions(b"1").unwrap(), [1]);
        assert_eq!(parse_server_versions(b"3 1 2").unwrap(), [1]);
        assert_eq!(parse_server_versions(b"1 ").unwrap(), [1]);
        assert_eq!(parse_server_versions(b"7").unwrap(), Vec::<u32>::new());
        assert!(parse_server_versions(b"").is_err());
        assert!(parse_server_versions(b"x").is_err());
    }

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
