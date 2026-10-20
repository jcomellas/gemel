//! Message framing, flow control and the RPC version handshake.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! What is here so far are the pieces a message is built from, byte for
//! byte as in OCaml: the 5-byte integers ([`encode_int`]), the tagged
//! sections that carry each marshalled value ([`encode_section`]), and the
//! header that says what a message is ([`Header`]). Packets, message ids,
//! flow control and the version handshake come with remote roots
//! (milestone 3).

use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};

/// RPC versions this implementation speaks, newest first. Must match
/// `rpcSupportedVersions` in Unison's `remote.ml`.
pub const RPC_SUPPORTED_VERSIONS: &[u32] = &[1];

/// Size of an encoded integer (`intSize`).
pub const INT_SIZE: usize = 5;

/// `intHash`: the check byte of an encoded integer.
fn int_hash(x: u64) -> u8 {
    // OCaml computes this on 63-bit ints; with x < 2^32 the product stays
    // below 2^62, so u64 gives the same result.
    (((x * 791_538_121) >> 23) + 17) as u8
}

/// `encodeInt`: 4 bytes little-endian, then a check byte.
pub fn encode_int(m: u32) -> [u8; INT_SIZE] {
    let b = m.to_le_bytes();
    [b[0], b[1], b[2], b[3], int_hash(u64::from(m))]
}

/// `decodeInt`: fails if the check byte doesn't match.
pub fn decode_int(buf: &[u8]) -> Result<u32, Error> {
    let corrupted = || {
        Error::Fatal(
            "Protocol error: corrupted message received;\nif it happens to you in a repeatable way, \n\
             please post a report on the unison-users mailing list."
                .into(),
        )
    };
    let b: [u8; INT_SIZE] = buf
        .get(..INT_SIZE)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(corrupted)?;
    let m = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    if b[4] != int_hash(u64::from(m)) {
        return Err(corrupted());
    }
    Ok(m)
}

/// `safeMarshal`: a section carrying one marshalled value: its length
/// (tag plus payload) as an encoded int, the tag, and the value framed by
/// `Umarshal.marshal_to_bytearray` (header included).
pub fn encode_section(tag: &str, write: impl FnOnce(&mut Writer)) -> Result<Vec<u8>, Error> {
    let frame = umarshal::to_bytes(write);
    let len = tag.len() + frame.len();
    let len = u32::try_from(len).map_err(|_| {
        Error::Fatal(
            "Protocol error: message data too big. This may be a bug or it\n\
             may be that your replicas are huge and the amount of updates\n\
             can't be handled by the current protocol implementation. If you\n\
             believe it is a bug then please consider reporting it.\n\
             Otherwise, try reducing the amount of updates by syncing the\n\
             replicas in smaller steps (using the \"path\" preference, for\n\
             example). You may have to do this for the initial sync only."
                .into(),
        )
    })?;
    Ok([&encode_int(len)[..], tag.as_bytes(), &frame].concat())
}

/// Splits a section off the front of `buf`: returns its body (tag and
/// frame) and the rest.
pub fn split_section(buf: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let len = decode_int(buf)? as usize;
    let body = buf
        .get(INT_SIZE..INT_SIZE + len)
        .ok_or_else(|| Error::Fatal("Protocol error: truncated message".into()))?;
    Ok((body, &buf[INT_SIZE + len..]))
}

/// `safeUnmarshal` and `unmarshalV1`: checks the tag of a section body and
/// decodes its value.
pub fn decode_section_body<T>(
    tag: &str,
    body: &[u8],
    read: impl FnOnce(&mut Reader<'_>) -> umarshal::Result<T>,
) -> Result<T, Error> {
    let Some(frame) = body.strip_prefix(tag.as_bytes()) else {
        let got = &body[..tag.len().min(body.len())];
        return Err(Error::Fatal(format!(
            "[safeUnmarshal] expected '{}' but got '{}'",
            escaped(tag.as_bytes()),
            escaped(got)
        )));
    };
    umarshal::from_bytes(frame, read)
        .map_err(|e| Error::Fatal(format!("Fatal error during unmarshaling ({e})")))
}

/// Decodes a whole section (length, tag and value).
pub fn decode_section<T>(
    tag: &str,
    buf: &[u8],
    read: impl FnOnce(&mut Reader<'_>) -> umarshal::Result<T>,
) -> Result<T, Error> {
    let (body, _) = split_section(buf)?;
    decode_section_body(tag, body, read)
}

/// `String.escaped`, for messages.
fn escaped(s: &[u8]) -> String {
    s.iter()
        .map(|&b| match b {
            b'"' => "\\\"".to_owned(),
            b'\\' => "\\\\".to_owned(),
            b'\n' => "\\n".to_owned(),
            b'\t' => "\\t".to_owned(),
            b'\r' => "\\r".to_owned(),
            0x20..=0x7e => char::from(b).to_string(),
            _ => format!("\\{b:03}"),
        })
        .collect()
}

/// What a message is (`Remote.header`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    /// The result of a request.
    NormalResult,
    /// The request failed with `Util.Transient`.
    TransientExn(String),
    /// The request failed with `Util.Fatal`.
    FatalExn(String),
    /// A request for the named command.
    Request(String),
    /// A stream chunk for the named stream.
    Stream(String),
    /// The stream was aborted.
    StreamAbort,
}

/// The tag of header sections (`"rsp"`).
pub const HEADER_TAG: &str = "rsp";

impl Header {
    /// `Remote.mheader`.
    pub fn marshal(&self, w: &mut Writer) {
        let s = |w: &mut Writer, tag, s: &String| {
            w.tag(tag);
            w.string(s.as_bytes());
        };
        match self {
            Header::NormalResult => w.tag(0),
            Header::TransientExn(m) => s(w, 1, m),
            Header::FatalExn(m) => s(w, 2, m),
            Header::Request(c) => s(w, 3, c),
            Header::Stream(c) => s(w, 4, c),
            Header::StreamAbort => w.tag(5),
        }
    }

    /// `Remote.mheader`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Header> {
        let text = |r: &mut Reader<'_>| Ok(String::from_utf8_lossy(&r.string()?).into_owned());
        Ok(match r.tag(6)? {
            0 => Header::NormalResult,
            1 => Header::TransientExn(text(r)?),
            2 => Header::FatalExn(text(r)?),
            3 => Header::Request(text(r)?),
            4 => Header::Stream(text(r)?),
            _ => Header::StreamAbort,
        })
    }

    /// A header section.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        encode_section(HEADER_TAG, |w| self.marshal(w))
    }

    /// The header for a command's outcome.
    pub fn of_result<T>(r: &Result<T, Error>) -> Header {
        match r {
            Ok(_) => Header::NormalResult,
            Err(Error::Transient(m)) => Header::TransientExn(m.clone()),
            Err(Error::Fatal(m)) => Header::FatalExn(m.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_round_trip_and_detect_corruption() {
        for m in [0, 1, 255, 1 << 31, u32::MAX] {
            assert_eq!(decode_int(&encode_int(m)), Ok(m));
        }
        let mut b = encode_int(7);
        b[4] ^= 1;
        assert!(
            decode_int(&b)
                .unwrap_err()
                .to_string()
                .starts_with("Protocol error: corrupted message")
        );
    }

    #[test]
    fn sections_check_their_tags() {
        let s = encode_section("x-args", |w| w.int(42)).unwrap();
        assert_eq!(decode_section("x-args", &s, |r| r.int()), Ok(42));
        assert_eq!(
            decode_section("y-args", &s, |r| r.int()),
            Err(Error::Fatal(
                "[safeUnmarshal] expected 'y-args' but got 'x-args'".into()
            ))
        );
        let e = decode_section("x-args", &s, |r| r.string())
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("Fatal error during unmarshaling ("), "{e}");
    }
}
