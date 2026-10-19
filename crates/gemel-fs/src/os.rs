//! Directory listing, deletion, renaming and temp-file handling.
//!
//! Ports Unison `src/os.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! This module has the full fingerprint type so far; the directory
//! operations are phase 1b of milestone 2.

use gemel_compat::umarshal::{self, Reader, Writer};

use crate::fingerprint::Fingerprint;

/// `Os.fullfingerprint`: the fingerprints of the data and of the resource
/// fork.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FullFingerprint {
    /// The file's contents.
    pub data: Fingerprint,
    /// The resource fork (dummy where there is none).
    pub ress: Fingerprint,
}

impl FullFingerprint {
    /// `Os.fullfingerprint_dummy`.
    pub const DUMMY: FullFingerprint = FullFingerprint {
        data: Fingerprint::DUMMY,
        ress: Fingerprint::DUMMY,
    };

    /// `Os.pseudoFingerprint`.
    pub fn pseudo(path: &[u8], size: i64) -> FullFingerprint {
        FullFingerprint {
            data: Fingerprint::pseudo(path, size),
            ress: Fingerprint::DUMMY,
        }
    }

    /// `Os.isPseudoFingerprint`.
    pub fn is_pseudo(&self) -> bool {
        self.data.is_pseudo()
    }

    /// `Os.fullfingerprintHash` (for hash tables).
    pub fn hash(&self) -> i64 {
        self.data.hash() + 31 * self.ress.hash()
    }

    /// `Os.reasonForFingerprintMismatch`.
    pub fn reason_for_mismatch(&self, other: &FullFingerprint) -> &'static str {
        if self.data == other.data {
            "resource fork"
        } else if self.ress == other.ress {
            "file contents"
        } else {
            "both file contents and resource fork"
        }
    }

    /// `Os.mfullfingerprint`.
    pub fn marshal(&self, w: &mut Writer) {
        self.data.marshal(w);
        self.ress.marshal(w);
    }

    /// `Os.mfullfingerprint`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<FullFingerprint> {
        Ok(FullFingerprint {
            data: Fingerprint::unmarshal(r)?,
            ress: Fingerprint::unmarshal(r)?,
        })
    }
}

/// `Os.fullfingerprint_to_string`.
impl std::fmt::Display for FullFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({},{})", self.data, self.ress)
    }
}
