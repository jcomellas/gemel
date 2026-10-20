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

/// `Uutil.File.t`: identifies an item being propagated (its line in the
/// user interface), for progress reports and aborting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub i64);

impl FileId {
    /// `Uutil.File.dummy`.
    pub const DUMMY: FileId = FileId(-1);
}

/// `Uutil.File.toString`.
impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == FileId::DUMMY {
            f.write_str("<dummy>")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

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

/// `Util.trimWhitespace`: strips spaces, tabs, CRs and LFs from both ends.
pub fn trim_whitespace(s: &str) -> &str {
    s.trim_matches([' ', '\t', '\n', '\r'])
}

/// `Util.removeTrailingCR`.
pub fn remove_trailing_cr(s: &str) -> &str {
    s.strip_suffix('\r').unwrap_or(s)
}

/// `Util.splitAtChar`: splits at the first `c`.
pub fn split_at_char(s: &str, c: char) -> (&str, Option<&str>) {
    match s.split_once(c) {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    }
}

/// `Util.findsubstring`: the first (or, with `reverse`, the last) position
/// of `needle` in `s`.
pub fn find_substring(needle: &str, s: &str, reverse: bool) -> Option<usize> {
    if reverse {
        s.rfind(needle)
    } else {
        s.find(needle)
    }
}

/// `Util.splitAtString`: splits at the first (or, with `reverse`, the last)
/// `sep`.
pub fn split_at_string<'a>(s: &'a str, sep: &str, reverse: bool) -> (&'a str, Option<&'a str>) {
    match find_substring(sep, s, reverse) {
        Some(i) => (&s[..i], Some(&s[i + sep.len()..])),
        None => (s, None),
    }
}

/// `Util.splitIntoWordsByString`: the parts between occurrences of `sep`
/// (empty parts included).
pub fn split_into_words_by_string<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut words = Vec::new();
    let mut rest = s;
    loop {
        match split_at_string(rest, sep, false) {
            (before, Some(after)) => {
                words.push(before);
                rest = after;
            }
            (last, None) => {
                words.push(last);
                return words;
            }
        }
    }
}

/// `Util.splitIntoWords`: splits at runs of `c`, with `\\` escaping the
/// next character (a final lone `\\` is dropped).
pub fn split_into_words(s: &str, c: char) -> Vec<String> {
    let mut words = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if_eq(&c).is_some() {}
        if chars.peek().is_none() {
            return words;
        }
        let mut word = String::new();
        while let Some(ch) = chars.next_if(|&ch| ch != c) {
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    word.push(escaped);
                }
            } else {
                word.push(ch);
            }
        }
        words.push(word);
    }
}

/// `Uutil.quotes`: `s` quoted for the shell (single quotes on Unix,
/// double quotes on Windows).
pub fn quotes(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `Filename.is_dir_sep` on this platform.
fn is_dir_sep(c: u8) -> bool {
    c == b'/' || (cfg!(windows) && (c == b'\\' || c == b':'))
}

/// `Filename`'s `drive_and_path` on Windows (`C:rest`); none elsewhere.
fn drive_and_path(s: &str) -> (&str, &str) {
    let b = s.as_bytes();
    if cfg!(windows) && b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        s.split_at(2)
    } else {
        ("", s)
    }
}

/// `Filename.dirname`: all but the last component (`"."` if there is
/// only one; trailing separators don't count).
pub fn filename_dirname(s: &str) -> String {
    let (drive, name) = drive_and_path(s);
    let b = name.as_bytes();
    let dir = if b.is_empty() {
        "."
    } else {
        // trailing_sep, then base, then intermediate_sep.
        let mut n = b.len() as isize - 1;
        while n >= 0 && is_dir_sep(b[n as usize]) {
            n -= 1;
        }
        if n < 0 {
            &name[..1]
        } else {
            while n >= 0 && !is_dir_sep(b[n as usize]) {
                n -= 1;
            }
            if n < 0 {
                "."
            } else {
                while n >= 0 && is_dir_sep(b[n as usize]) {
                    n -= 1;
                }
                if n < 0 {
                    &name[..1]
                } else {
                    &name[..n as usize + 1]
                }
            }
        }
    };
    format!("{drive}{dir}")
}

/// `Filename.basename`: the last component (trailing separators don't
/// count).
pub fn filename_basename(s: &str) -> String {
    let (_, name) = drive_and_path(s);
    let b = name.as_bytes();
    if b.is_empty() {
        return ".".to_owned();
    }
    let mut n = b.len() as isize - 1;
    while n >= 0 && is_dir_sep(b[n as usize]) {
        n -= 1;
    }
    if n < 0 {
        return name[..1].to_owned();
    }
    let end = n as usize + 1;
    while n >= 0 && !is_dir_sep(b[n as usize]) {
        n -= 1;
    }
    name[(n + 1) as usize..end].to_owned()
}

/// OCaml's `int_of_string` for the 63-bit `int`: an optional sign, an
/// optional `0x`/`0o`/`0b`/`0u` prefix, and digits with `_` allowed after
/// the first. Hexadecimal, octal and binary literals may use the full 63
/// bits and wrap to negative, as in OCaml.
pub fn int_of_string(s: &str) -> Option<i64> {
    const BITS: u32 = 63;
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let lower = rest.get(..2).map(str::to_ascii_lowercase);
    let (radix, digits) = match lower.as_deref() {
        Some("0x") => (16, &rest[2..]),
        Some("0o") => (8, &rest[2..]),
        Some("0b") => (2, &rest[2..]),
        Some("0u") => (10, &rest[2..]),
        _ => (10, rest),
    };
    if digits.is_empty() || digits.starts_with('_') {
        return None;
    }
    let mut n: u64 = 0;
    for ch in digits.chars().filter(|&ch| ch != '_') {
        let d = ch.to_digit(radix)?;
        n = n.checked_mul(radix.into())?.checked_add(d.into())?;
        if n >= 1 << BITS {
            return None;
        }
    }
    let max = (1u64 << (BITS - 1)) - 1;
    let n = if radix == 10 && !rest.starts_with("0u") && !rest.starts_with("0U") {
        if n > max + u64::from(neg) {
            return None;
        }
        n as i64
    } else if n > max {
        // Wrap into the negative half, as OCaml does for non-decimal input.
        (n as i64).wrapping_add(i64::MIN)
    } else {
        n as i64
    };
    Some(if neg { n.wrapping_neg() } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_helpers_match_util_ml() {
        assert_eq!(trim_whitespace(" \t a b \r\n"), "a b");
        assert_eq!(remove_trailing_cr("a\r"), "a");
        assert_eq!(split_at_char("a=b=c", '='), ("a", Some("b=c")));
        assert_eq!(split_at_char("a", '='), ("a", None));
        assert_eq!(
            split_at_string(" a -> b -> c ", " -> ", true),
            (" a -> b", Some("c "))
        );
        assert_eq!(
            split_at_string(" a -> b -> c ", " -> ", false),
            (" a", Some("b -> c "))
        );
        assert_eq!(split_at_string("a", " -> ", true), ("a", None));
        assert_eq!(
            split_into_words_by_string("a -> b ->  -> c", " -> "),
            ["a", "b", "", "c"]
        );
        assert_eq!(split_into_words_by_string("", " -> "), [""]);
        assert_eq!(
            split_into_words("  include  my\\ file ", ' '),
            ["include", "my file"]
        );
        assert_eq!(split_into_words("a\\", ' '), ["a"]);
        assert!(split_into_words("   ", ' ').is_empty());
    }

    #[test]
    fn int_of_string_follows_ocaml() {
        let max = (1i64 << 62) - 1;
        assert_eq!(int_of_string("42"), Some(42));
        assert_eq!(int_of_string("-1_000"), Some(-1000));
        assert_eq!(int_of_string("+7"), Some(7));
        assert_eq!(int_of_string("0x1F"), Some(31));
        assert_eq!(int_of_string("0o17"), Some(15));
        assert_eq!(int_of_string("0b101"), Some(5));
        assert_eq!(int_of_string(&max.to_string()), Some(max));
        assert_eq!(int_of_string(&(max + 1).to_string()), None);
        assert_eq!(int_of_string(&format!("-{}", max + 1)), Some(-max - 1));
        assert_eq!(int_of_string("0x7fffffffffffffff"), Some(-1));
        for bad in ["", "-", "_1", "1a", "0x", "0xg", " 1", "1 "] {
            assert_eq!(int_of_string(bad), None, "{bad:?}");
        }
    }

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
    #[test]
    fn filenames_split_like_ocaml() {
        for (s, dir, base) in [
            ("", ".", "."),
            ("a", ".", "a"),
            ("a/", ".", "a"),
            ("a/b", "a", "b"),
            ("a//b//", "a", "b"),
            ("/", "/", "/"),
            ("/a", "/", "a"),
            ("bak/$VERSION.", "bak", "$VERSION."),
            ("x/y/.bak.", "x/y", ".bak."),
        ] {
            assert_eq!(
                (filename_dirname(s), filename_basename(s)),
                (dir.into(), base.into()),
                "{s}"
            );
        }
    }
}
