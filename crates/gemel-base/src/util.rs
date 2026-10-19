//! Miscellaneous helpers, errors and file-size types.
//!
//! Ports Unison `src/ubase/util.ml`, Unison `src/uutil.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use std::cmp::Ordering;
use std::fmt;

/// Unison's two kinds of failure (`Util.Fatal` and `Util.Transient`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Stops the whole run.
    Fatal(String),
    /// Fails one item (or one operation); the rest of the run continues.
    Transient(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fatal(m) | Error::Transient(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Lowercases a Latin-1 (ISO 8859-1) byte: `A`–`Z`, `À`–`Ö` and `Ø`–`Þ`.
pub fn lowercase_latin1(c: u8) -> u8 {
    match c {
        b'A'..=b'Z' | 192..=214 | 216..=222 => c + 32,
        _ => c,
    }
}

/// Compares byte strings ignoring Latin-1 case (`Util.nocase_cmp`).
pub fn nocase_cmp(a: &[u8], b: &[u8]) -> Ordering {
    a.iter()
        .map(|&c| lowercase_latin1(c))
        .cmp(b.iter().map(|&c| lowercase_latin1(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_case_folding_skips_multiplication_sign() {
        assert_eq!(lowercase_latin1(b'Q'), b'q');
        assert_eq!(lowercase_latin1(0xC0), 0xE0);
        assert_eq!(lowercase_latin1(0xD7), 0xD7); // ×
        assert_eq!(lowercase_latin1(0xDE), 0xFE);
        assert_eq!(lowercase_latin1(0xDF), 0xDF); // ß
        assert_eq!(nocase_cmp(b"ABC", b"abc"), Ordering::Equal);
        assert_eq!(nocase_cmp(b"ab", b"ABC"), Ordering::Less);
    }
}
