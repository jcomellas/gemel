//! MD5 file fingerprints.
//!
//! Ports Unison `src/fingerprint.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! This module has the value type so far; hashing file contents is phase 1b
//! of milestone 2.

use std::fmt;

use gemel_compat::umarshal::{self, Reader, Writer};
use md5::{Digest, Md5};

use crate::props::Filesize;

/// A fingerprint (`Fingerprint.t`): the raw MD5 of a file's contents, a
/// pseudo-fingerprint (`LEN<size>@<MD5 of the path>`, for files whose
/// contents haven't been read), or empty ([`Fingerprint::DUMMY`]).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub Vec<u8>);

const PSEUDO_PREFIX: &[u8] = b"LEN";

impl Fingerprint {
    /// `Fingerprint.dummy`.
    pub const DUMMY: Fingerprint = Fingerprint(Vec::new());

    /// `Fingerprint.pseudo`: stands in for the contents of the file at
    /// `path` (a replica-relative path) of size `len`.
    pub fn pseudo(path: &[u8], len: Filesize) -> Fingerprint {
        let mut v = format!("LEN{len}@").into_bytes();
        v.extend_from_slice(&Md5::digest(path));
        Fingerprint(v)
    }

    /// `Fingerprint.ispseudo`.
    pub fn is_pseudo(&self) -> bool {
        self.0.starts_with(PSEUDO_PREFIX)
    }

    /// `Fingerprint.hash` (for hash tables, not `checkArchive`): the first
    /// three bytes, or a constant for the dummy.
    pub fn hash(&self) -> i64 {
        match &self.0[..] {
            [] => 1_234_577,
            [a, b, c, ..] => i64::from(*a) + (i64::from(*b) << 8) + (i64::from(*c) << 16),
            _ => panic!("fingerprint shorter than 3 bytes"),
        }
    }

    /// `Fingerprint.m`.
    pub fn marshal(&self, w: &mut Writer) {
        w.string(&self.0);
    }

    /// `Fingerprint.m`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Fingerprint> {
        r.string().map(Fingerprint)
    }
}

/// `Fingerprint.toString`: lowercase hex, or a pseudo-fingerprint as is.
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_pseudo() {
            f.write_str(&String::from_utf8_lossy(&self.0))
        } else {
            self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
        }
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_pseudo_and_hash() {
        let fp = Fingerprint(Md5::digest(b"hi\n").to_vec());
        assert_eq!(fp.to_string(), "764efa883dda1e11db47671c4a3bbd9e");
        assert_eq!(fp.hash(), 0x76 + (0x4e << 8) + (0xfa << 16));
        assert_eq!(Fingerprint::DUMMY.hash(), 1_234_577);
        let p = Fingerprint::pseudo(b"a/b", 12);
        assert!(p.is_pseudo() && p.0.starts_with(b"LEN12@") && p.0.len() == 6 + 16);
    }
}
