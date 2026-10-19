//! Single path components.
//!
//! Ports Unison `src/name.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! # Comparing names
//!
//! Whether two names are the same file depends on the [`Mode`], which is
//! chosen at startup and can change when a frontend switches profiles. OCaml
//! reads it from a global (`Case.ops ()`); here it is an explicit argument,
//! so `Name` deliberately has no `Ord`: sorting names by bytes would give a
//! different order from Unison's in every mode but [`Mode::Sensitive`],
//! and archive order must match. `PartialEq`, `Eq` and `Hash` compare bytes,
//! which is right for exact identity; use [`Name::eq_in`] for "same file".

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

use gemel_compat::umarshal::{self, Reader, Writer};

use crate::case::Mode;
use crate::util::Error;

/// A single path component: a non-empty byte string without `/`.
///
/// The bytes are whatever the filesystem returned (UTF-8 in practice, but
/// not necessarily valid). Changing this type changes the archive format.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Name(Vec<u8>);

impl Name {
    /// `Name.fromString`: checks that the name is non-empty and has no `/`.
    pub fn from_bytes(s: impl Into<Vec<u8>>) -> Result<Name, Error> {
        let s = s.into();
        if s.is_empty() {
            // OCaml raises Invalid_argument, a programming error.
            return Err(Error::Transient("Name.fromString(empty string)".into()));
        }
        if s.contains(&b'/') {
            return Err(Error::Transient(format!(
                "Filename '{}' contains a '/'",
                String::from_utf8_lossy(&s)
            )));
        }
        Ok(Name(s))
    }

    /// `Name.toString`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Takes the bytes without checking them, as unmarshalling does in OCaml.
    pub(crate) fn from_bytes_unchecked(s: Vec<u8>) -> Name {
        Name(s)
    }

    /// `Name.compare` under `mode`.
    pub fn compare_in(&self, other: &Name, mode: Mode) -> Ordering {
        mode.compare(&self.0, &other.0)
    }

    /// `Name.eq` under `mode`: whether the names denote the same file.
    pub fn eq_in(&self, other: &Name, mode: Mode) -> bool {
        self.compare_in(other, mode) == Ordering::Equal
    }

    /// `Name.hash` under `mode`, consistent with [`Name::compare_in`]. Used in
    /// archive checksums, so it must match OCaml.
    pub fn hash_in(&self, mode: Mode) -> i64 {
        mode.hash(&self.0)
    }

    /// `Name.normalize`: the form to store for a name read from disk.
    pub fn normalize(&self, mode: Mode) -> Cow<'_, Name> {
        match mode.normalize_filename(&self.0) {
            Cow::Borrowed(_) => Cow::Borrowed(self),
            Cow::Owned(s) => Cow::Owned(Name(s)),
        }
    }

    /// `Name.badEncoding`: whether the name can't be represented in `mode`.
    pub fn bad_encoding(&self, mode: Mode) -> bool {
        mode.bad_encoding(&self.0)
    }

    /// `Name.badFile`: whether Windows can't create a file with this name.
    /// Implements `badWindowsFilenameRx`, matched case-insensitively:
    ///
    /// ```text
    /// (.*[\000-\031<>:"/\|?*].*)|((con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?)|(.*[. ])
    /// ```
    pub fn bad_file(&self) -> bool {
        bad_windows_filename(&self.0)
    }

    /// Marshals the name (`Name.m`, a string).
    pub fn marshal(&self, w: &mut Writer) {
        w.string(&self.0);
    }

    /// Unmarshals a name (`Name.m`). Like OCaml, does not validate it.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Name> {
        r.string().map(Name)
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Name({:?})", String::from_utf8_lossy(&self.0))
    }
}

pub(crate) fn bad_windows_filename(s: &[u8]) -> bool {
    let forbidden = |c: &u8| {
        matches!(
            c,
            0..=31 | b'<' | b'>' | b':' | b'"' | b'/' | b'\\' | b'|' | b'?' | b'*'
        )
    };
    let device = || {
        // `(\..*)?` accepts anything after the first dot, so only the part
        // before it needs to be a device name. Rx folds case over Latin-1,
        // but no non-ASCII letter folds to the ASCII ones used here.
        let stem = s.split(|&c| c == b'.').next().unwrap_or_default();
        match stem.to_ascii_lowercase().as_slice() {
            b"con" | b"prn" | b"aux" | b"nul" => true,
            [b'c', b'o', b'm', d] | [b'l', b'p', b't', d] => (b'1'..=b'9').contains(d),
            _ => false,
        }
    };
    s.iter().any(forbidden) || device() || matches!(s.last(), Some(b'.' | b' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_rejects_empty_names_and_slashes() {
        assert!(Name::from_bytes("a").is_ok());
        assert!(Name::from_bytes("").is_err());
        assert_eq!(
            Name::from_bytes("a/b"),
            Err(Error::Transient("Filename 'a/b' contains a '/'".into()))
        );
    }

    #[test]
    fn comparison_depends_on_the_mode() {
        let (a, b) = (
            Name::from_bytes("README").unwrap(),
            Name::from_bytes("readme").unwrap(),
        );
        assert!(!a.eq_in(&b, Mode::Sensitive));
        assert!(a.eq_in(&b, Mode::UnicodeInsensitive));
        assert_eq!(a.hash_in(Mode::Insensitive), b.hash_in(Mode::Insensitive));
    }

    /// `badWindowsFilenameRx` as Unison's Rx engine matches it.
    #[test]
    fn bad_file_matches_ocaml_rx() {
        let text = include_str!("../../gemel-compat/tests/fixtures/names.txt");
        let mut checked = 0;
        for line in text.lines().filter(|l| l.starts_with("bad_file ")) {
            let [_, input, expected] = line.split(' ').collect::<Vec<_>>()[..] else {
                panic!("bad line {line:?}");
            };
            let input = crate::test_util::unhex(input);
            assert_eq!(bad_windows_filename(&input), expected == "1", "{line}");
            checked += 1;
        }
        assert!(checked > 50);
    }

    #[test]
    fn normalize_recomposes_only_in_unicode_modes() {
        let n = Name::from_bytes("cafe\u{301}").unwrap();
        assert!(matches!(n.normalize(Mode::Sensitive), Cow::Borrowed(_)));
        assert_eq!(
            n.normalize(Mode::UnicodeSensitive).as_bytes(),
            "café".as_bytes()
        );
    }
}
