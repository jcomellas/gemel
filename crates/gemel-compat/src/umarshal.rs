//! Schema-less binary codec used for archives and RPC payloads.
//!
//! Ports Unison `src/ubase/umarshal.ml`.
//!
//! Derived from Unison, Copyright 2020, Stéphane Glondu.
//!
//! # Format
//!
//! A marshalled value is an 8-byte big-endian payload length followed by the
//! payload. The payload carries no type information; both sides must agree on
//! the shape. Primitive encodings:
//!
//! | OCaml | Encoding |
//! |---|---|
//! | `unit` | nothing |
//! | `bool` | one byte, `0` or `1` |
//! | `int`, `int64` | a size byte (`0`, `1`, `2`, `4` or `8`) then that many big-endian two's-complement bytes; `0` has size 0 |
//! | `float` | its IEEE-754 bits, encoded as an `int64` |
//! | `string`, `bytearray` | an `int` length, then the bytes |
//! | `int32bigarray` | an `int` length, then 4 big-endian bytes per element |
//! | `'a list` | an `int` length, then each element |
//! | sums (`sum2`..`sum6`, `option`) | a tag byte, then the payload of that case |
//! | products (`prod2`..`prod6`) | the fields in order |
//!
//! # Mapping to Rust
//!
//! The OCaml module builds codecs from combinators. Here the same structure is
//! written directly against [`Writer`] and [`Reader`]: a product is a sequence
//! of calls, a sum is [`Writer::tag`]/[`Reader::tag`] followed by the case's
//! fields, and `cond`/`switch` are plain `if`s in the caller. OCaml strings
//! are byte strings, so they are `&[u8]`/`Vec<u8>` here, never `str`.

use std::fmt;

/// Size of the length header in front of every marshalled value.
pub const HEADER_SIZE: usize = 8;

/// Largest OCaml `int` on a 64-bit platform (`max_int`, 2⁶² − 1).
pub const OCAML_MAX_INT: i64 = (1 << 62) - 1;

/// Smallest OCaml `int` on a 64-bit platform (`min_int`, −2⁶²).
pub const OCAML_MIN_INT: i64 = -(1 << 62);

/// A decoding error. OCaml raises `Umarshal.Error` for the same conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Fewer than [`HEADER_SIZE`] bytes were available for the header.
    HeaderTooShort,
    /// The header declared a negative payload size.
    NegativeSize,
    /// The header declared a payload larger than this platform can address.
    PayloadTooLarge,
    /// The payload ended in the middle of a value.
    EndOfInput,
    /// A `bool` byte other than 0 or 1.
    InvalidBool(u8),
    /// A sum tag outside the number of cases.
    InvalidTag {
        /// Number of cases the reader expected.
        cases: u8,
        /// The tag that was read.
        tag: u8,
    },
    /// An integer size byte other than 0, 1, 2, 4 or 8.
    InvalidIntSize(u8),
    /// An `int` outside OCaml's 63-bit range.
    IntTooLarge(i64),
    /// A negative length for a string, array or list.
    NegativeLength(i64),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::HeaderTooShort => write!(f, "data_size: header too short"),
            Error::NegativeSize => write!(f, "data_size: negative size"),
            Error::PayloadTooLarge => write!(f, "data_size: payload too large"),
            Error::EndOfInput => write!(f, "end of input"),
            Error::InvalidBool(b) => write!(f, "bool: invalid value ({b})"),
            Error::InvalidTag { cases, tag } => write!(f, "sum{cases}: invalid tag ({tag})"),
            Error::InvalidIntSize(n) => write!(f, "int64.read: unexpected size ({n})"),
            Error::IntTooLarge(n) => write!(f, "int.read: too large ({n})"),
            Error::NegativeLength(n) => write!(f, "negative length ({n})"),
        }
    }
}

impl std::error::Error for Error {}

/// Result of a decoding operation.
pub type Result<T> = std::result::Result<T, Error>;

/// Reads the payload size from a header (`Umarshal.data_size`).
///
/// `header` must start with the [`HEADER_SIZE`]-byte header; anything after it
/// is ignored.
pub fn data_size(header: &[u8]) -> Result<usize> {
    let bytes: [u8; HEADER_SIZE] = header
        .get(..HEADER_SIZE)
        .and_then(|h| h.try_into().ok())
        .ok_or(Error::HeaderTooShort)?;
    let n = i64::from_be_bytes(bytes);
    if n < 0 {
        return Err(Error::NegativeSize);
    }
    usize::try_from(n).map_err(|_| Error::PayloadTooLarge)
}

/// Marshals a value with its header (`Umarshal.to_string`).
///
/// ```
/// use gemel_compat::umarshal;
/// let bytes = umarshal::to_bytes(|w| w.string(b"a"));
/// assert_eq!(bytes, [0, 0, 0, 0, 0, 0, 0, 3, 1, 1, b'a']);
/// ```
pub fn to_bytes(write: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer {
        buf: vec![0; HEADER_SIZE],
    };
    write(&mut w);
    let size = (w.buf.len() - HEADER_SIZE) as u64;
    w.buf[..HEADER_SIZE].copy_from_slice(&size.to_be_bytes());
    w.buf
}

/// Unmarshals a value that starts with its header (`Umarshal.from_bytes`).
///
/// Reading stops at the end of the payload the header declares, or at the end
/// of `bytes` if that comes first. Bytes the reader leaves unread are not an
/// error, as in OCaml.
pub fn from_bytes<T>(bytes: &[u8], read: impl FnOnce(&mut Reader<'_>) -> Result<T>) -> Result<T> {
    let size = data_size(bytes)?;
    let payload = &bytes[HEADER_SIZE..];
    let end = size.min(payload.len());
    read(&mut Reader::new(&payload[..end]))
}

/// Encoder for a payload (no header). See [`to_bytes`].
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// Creates a writer for a bare payload.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the payload written so far.
    pub fn into_payload(self) -> Vec<u8> {
        self.buf
    }

    /// Appends raw bytes, as the OCaml `send` callback does.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// `Umarshal.unit`: writes nothing.
    pub fn unit(&mut self) {}

    /// `Umarshal.bool`.
    pub fn bool(&mut self, b: bool) {
        self.buf.push(u8::from(b));
    }

    /// A single byte (`Umarshal.char`, internal in OCaml).
    pub fn char(&mut self, c: u8) {
        self.buf.push(c);
    }

    /// The tag of a sum case (`I21` is 0, `I22` is 1, and so on).
    pub fn tag(&mut self, tag: u8) {
        self.buf.push(tag);
    }

    /// `Umarshal.int`. The value must fit in OCaml's 63-bit `int`
    /// ([`OCAML_MIN_INT`]..=[`OCAML_MAX_INT`]); OCaml readers reject anything
    /// larger.
    pub fn int(&mut self, n: i64) {
        debug_assert!(
            (OCAML_MIN_INT..=OCAML_MAX_INT).contains(&n),
            "{n} does not fit in an OCaml int"
        );
        self.int64(n);
    }

    /// `Umarshal.int64`: the shortest of the 0/1/2/4/8-byte encodings.
    pub fn int64(&mut self, n: i64) {
        if n == 0 {
            self.buf.push(0);
        } else if let Ok(b) = i8::try_from(n) {
            self.buf.push(1);
            self.buf.extend_from_slice(&b.to_be_bytes());
        } else if let Ok(b) = i16::try_from(n) {
            self.buf.push(2);
            self.buf.extend_from_slice(&b.to_be_bytes());
        } else if let Ok(b) = i32::try_from(n) {
            self.buf.push(4);
            self.buf.extend_from_slice(&b.to_be_bytes());
        } else {
            self.buf.push(8);
            self.buf.extend_from_slice(&n.to_be_bytes());
        }
    }

    /// `Umarshal.float`: the IEEE-754 bits as an `int64`.
    pub fn float(&mut self, x: f64) {
        self.int64(x.to_bits() as i64);
    }

    /// `Umarshal.string`: a length then the bytes.
    pub fn string(&mut self, s: &[u8]) {
        self.length(s.len());
        self.buf.extend_from_slice(s);
    }

    /// `Umarshal.bytearray`: same encoding as [`Writer::string`].
    pub fn bytearray(&mut self, bytes: &[u8]) {
        self.string(bytes);
    }

    /// `Umarshal.int32bigarray`: a length then 4 big-endian bytes each.
    pub fn int32bigarray(&mut self, items: &[i32]) {
        self.length(items.len());
        for i in items {
            self.buf.extend_from_slice(&i.to_be_bytes());
        }
    }

    /// `Umarshal.option`: tag 0 for `None`, tag 1 then the value for `Some`.
    pub fn option<T>(&mut self, value: Option<&T>, write: impl FnOnce(&mut Self, &T)) {
        match value {
            None => self.tag(0),
            Some(v) => {
                self.tag(1);
                write(self, v);
            }
        }
    }

    /// `Umarshal.list`: a length then each element.
    pub fn list<T>(&mut self, items: &[T], mut write: impl FnMut(&mut Self, &T)) {
        self.length(items.len());
        for item in items {
            write(self, item);
        }
    }

    fn length(&mut self, len: usize) {
        self.int(i64::try_from(len).expect("length fits in an OCaml int"));
    }
}

/// Decoder for a payload (no header). See [`from_bytes`].
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Creates a reader over a bare payload.
    pub fn new(payload: &'a [u8]) -> Self {
        Self {
            buf: payload,
            pos: 0,
        }
    }

    /// Number of bytes consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Bytes not yet consumed.
    pub fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    /// Takes the next `n` raw bytes, as the OCaml `recv` callback does.
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::EndOfInput)?;
        let bytes = self.buf.get(self.pos..end).ok_or(Error::EndOfInput)?;
        self.pos = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.raw(N)?.try_into().expect("raw returns N bytes"))
    }

    /// `Umarshal.unit`: reads nothing.
    pub fn unit(&mut self) -> Result<()> {
        Ok(())
    }

    /// `Umarshal.bool`.
    pub fn bool(&mut self) -> Result<bool> {
        match self.char()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(Error::InvalidBool(b)),
        }
    }

    /// A single byte (`Umarshal.char`, internal in OCaml).
    pub fn char(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    /// The tag of a sum with `cases` cases, checked to be in range.
    pub fn tag(&mut self, cases: u8) -> Result<u8> {
        let tag = self.char()?;
        if tag < cases {
            Ok(tag)
        } else {
            Err(Error::InvalidTag { cases, tag })
        }
    }

    /// `Umarshal.int`: an `int64` checked to fit OCaml's 63-bit `int`.
    pub fn int(&mut self) -> Result<i64> {
        let n = self.int64()?;
        if (OCAML_MIN_INT..=OCAML_MAX_INT).contains(&n) {
            Ok(n)
        } else {
            Err(Error::IntTooLarge(n))
        }
    }

    /// `Umarshal.int64`. Accepts any of the sizes, not just the shortest.
    pub fn int64(&mut self) -> Result<i64> {
        Ok(match self.char()? {
            0 => 0,
            1 => i8::from_be_bytes(self.array()?).into(),
            2 => i16::from_be_bytes(self.array()?).into(),
            4 => i32::from_be_bytes(self.array()?).into(),
            8 => i64::from_be_bytes(self.array()?),
            n => return Err(Error::InvalidIntSize(n)),
        })
    }

    /// `Umarshal.float`.
    pub fn float(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.int64()? as u64))
    }

    /// `Umarshal.string`, borrowing from the input.
    pub fn string_slice(&mut self) -> Result<&'a [u8]> {
        let len = self.length()?;
        self.raw(len)
    }

    /// `Umarshal.string`.
    pub fn string(&mut self) -> Result<Vec<u8>> {
        self.string_slice().map(<[u8]>::to_vec)
    }

    /// `Umarshal.bytearray`.
    pub fn bytearray(&mut self) -> Result<Vec<u8>> {
        self.string()
    }

    /// `Umarshal.int32bigarray`.
    pub fn int32bigarray(&mut self) -> Result<Vec<i32>> {
        let len = self.length()?;
        let bytes = self.raw(len.checked_mul(4).ok_or(Error::EndOfInput)?)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|c| i32::from_be_bytes(c.try_into().expect("chunks of 4")))
            .collect())
    }

    /// `Umarshal.option`.
    pub fn option<T>(&mut self, read: impl FnOnce(&mut Self) -> Result<T>) -> Result<Option<T>> {
        match self.tag(2)? {
            0 => Ok(None),
            _ => read(self).map(Some),
        }
    }

    /// `Umarshal.list`.
    pub fn list<T>(&mut self, mut read: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let len = self.length()?;
        // `len` comes from the input, so don't let it size the allocation:
        // every element except `unit` takes at least one byte.
        let mut items = Vec::with_capacity(len.min(self.remaining().len()));
        for _ in 0..len {
            items.push(read(self)?);
        }
        Ok(items)
    }

    fn length(&mut self) -> Result<usize> {
        let n = self.int()?;
        usize::try_from(n).map_err(|_| Error::NegativeLength(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(write: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut w = Writer::new();
        write(&mut w);
        w.into_payload()
    }

    #[test]
    fn int64_roundtrips_at_size_boundaries() {
        let mut cases = vec![0, i64::MIN, i64::MAX];
        for bits in [7, 15, 31, 63] {
            let edge = 1i64.checked_shl(bits).unwrap_or(i64::MIN);
            for n in [edge, edge.wrapping_neg()] {
                cases.extend([n.wrapping_sub(1), n, n.wrapping_add(1)]);
            }
        }
        for n in cases {
            let bytes = payload(|w| w.int64(n));
            assert_eq!(Reader::new(&bytes).int64(), Ok(n), "{n}");
        }
    }

    #[test]
    fn int_rejects_values_outside_63_bits() {
        let bytes = payload(|w| w.int64(OCAML_MAX_INT + 1));
        assert_eq!(
            Reader::new(&bytes).int(),
            Err(Error::IntTooLarge(OCAML_MAX_INT + 1))
        );
    }

    #[test]
    fn reader_rejects_malformed_input() {
        assert_eq!(Reader::new(&[2]).bool(), Err(Error::InvalidBool(2)));
        assert_eq!(Reader::new(&[3]).int64(), Err(Error::InvalidIntSize(3)));
        assert_eq!(Reader::new(&[4, 0, 0]).int64(), Err(Error::EndOfInput));
        assert_eq!(
            Reader::new(&[2]).tag(2),
            Err(Error::InvalidTag { cases: 2, tag: 2 })
        );
        assert_eq!(Reader::new(&[1, 5, b'a']).string(), Err(Error::EndOfInput));
        assert_eq!(
            Reader::new(&[1, 0xff]).string(),
            Err(Error::NegativeLength(-1))
        );
        // A huge declared length must fail on the data, not on allocation.
        assert_eq!(
            Reader::new(&[8, 0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1]).list(|r| r.bool()),
            Err(Error::EndOfInput)
        );
    }

    #[test]
    fn header_is_checked() {
        assert_eq!(data_size(&[0; 7]), Err(Error::HeaderTooShort));
        assert_eq!(data_size(&[0xff; 8]), Err(Error::NegativeSize));
        assert_eq!(data_size(&[0, 0, 0, 0, 0, 0, 1, 0, 0xaa]), Ok(256));
    }

    #[test]
    fn from_bytes_stops_at_the_declared_size() {
        let mut bytes = to_bytes(|w| w.string(b"ab"));
        bytes.extend_from_slice(&[1, 1]);
        assert_eq!(from_bytes(&bytes, |r| r.string()), Ok(b"ab".to_vec()));
        assert_eq!(
            from_bytes(&bytes, |r| {
                r.string()?;
                r.bool()
            }),
            Err(Error::EndOfInput)
        );
    }
}
