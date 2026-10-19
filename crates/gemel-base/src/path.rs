//! Relative paths within a replica.
//!
//! Ports Unison `src/path.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! A path is the `/`-separated names from a replica root, stored as one byte
//! string (`""` is the root itself). As in OCaml, the type records which
//! replica it is valid for: a [`Path<Global>`] means the same file on both
//! replicas (and is compared case-insensitively in case-insensitive mode),
//! while a [`Path<Local>`] is spelled as on one particular filesystem.
//! [`Path::force_local`] and [`Path::make_global`] convert between them.
//!
//! As with [`Name`], comparison takes the [`Mode`] explicitly and `Path` has
//! no `Ord`; see the `name` module.
//!
//! `followLink` (the `follow` preference) moves to the predicate module when
//! `pred.ml` is ported.

use std::cmp::Ordering;
use std::fmt;
use std::marker::PhantomData;

use gemel_compat::umarshal::{self, Reader, Writer};

use crate::case::Mode;
use crate::name::Name;
use crate::util::Error;

/// Marker for paths valid on both replicas (`[`Global] path`, `Path.t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Global {}

/// Marker for paths specific to one replica (`[`Local] path`, `Path.local`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Local {}

/// A relative path; `S` is [`Global`] or [`Local`].
pub struct Path<S> {
    bytes: Vec<u8>,
    scope: PhantomData<S>,
}

const SEPARATOR: u8 = b'/';

impl<S> Path<S> {
    fn new(bytes: Vec<u8>) -> Self {
        Path {
            bytes,
            scope: PhantomData,
        }
    }

    /// `Path.empty`: the replica root.
    pub fn empty() -> Self {
        Path::new(Vec::new())
    }

    /// `Path.toString`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `Path.child`: appends a name.
    pub fn child(&self, name: &Name) -> Self {
        Path::new(concat(&self.bytes, name.as_bytes()))
    }

    /// `Path.deconstruct`: splits off the first name.
    pub fn deconstruct(&self) -> Option<(Name, Self)> {
        let p = &self.bytes;
        match p.iter().position(|&c| c == SEPARATOR) {
            Some(i) => Some((name(&p[..i]), Path::new(p[i + 1..].to_vec()))),
            None if p.is_empty() => None,
            None => Some((name(p), Path::empty())),
        }
    }

    /// `Path.fromString`, following the conventions of the platform this runs
    /// on (on Windows, `\` separates too and drive letters are absolute).
    ///
    /// Rejects absolute paths and `..`; drops `.` components, empty
    /// components and trailing slashes.
    pub fn from_bytes(s: &[u8]) -> Result<Self, Error> {
        Self::from_bytes_for(s, cfg!(windows))
    }

    /// [`Path::from_bytes`] with an explicit platform, for testing.
    fn from_bytes_for(s0: &[u8], windows: bool) -> Result<Self, Error> {
        let converted;
        let s = if windows {
            converted = s0
                .iter()
                .map(|&c| if c == b'\\' { b'/' } else { c })
                .collect::<Vec<_>>();
            &converted[..]
        } else {
            s0
        };
        if is_absolute(s, windows) {
            return Err(Error::Transient(format!(
                "The path '{}' is not a relative path",
                String::from_utf8_lossy(s)
            )));
        }
        let parent_ref = || {
            Error::Transient(format!(
                "Reference to parent directory '..' not allowed in path '{}'",
                String::from_utf8_lossy(s0)
            ))
        };
        let s = remove_trailing_slashes(s);
        let mut path = Path::empty();
        for component in s.split(|&c| c == SEPARATOR) {
            match component {
                b".." => return Err(parent_ref()),
                b"" | b"." => {}
                n => path = path.child(&name(n)),
            }
        }
        Ok(path)
    }

    /// Marshals the path (`Path.mpath`, a string).
    pub fn marshal(&self, w: &mut Writer) {
        w.string(&self.bytes);
    }

    /// Unmarshals a path (`Path.mpath`). Like OCaml, does not validate it.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Self> {
        r.string().map(Path::new)
    }
}

impl Path<Global> {
    /// `Path.length`. Note that this is the number of separators, as in
    /// OCaml: 0 for both the root and a single name.
    pub fn length(&self) -> usize {
        self.bytes.iter().filter(|&&c| c == SEPARATOR).count()
    }

    /// `Path.finalName`: the last name, or `None` for the root.
    pub fn final_name(&self) -> Option<Name> {
        let p = &self.bytes;
        match p.iter().rposition(|&c| c == SEPARATOR) {
            Some(i) => Some(name(&p[i + 1..])),
            None if p.is_empty() => None,
            None => Some(name(p)),
        }
    }

    /// `Path.toNames`: the names from left to right.
    pub fn to_names(&self) -> Result<Vec<Name>, Error> {
        str_split(&self.bytes)
            .map(|n| Name::from_bytes(n.to_vec()))
            .collect()
    }

    /// `Path.compare` under `mode`.
    pub fn compare_in(&self, other: &Self, mode: Mode) -> Ordering {
        mode.compare(&self.bytes, &other.bytes)
    }

    /// `Path.forceLocal`.
    pub fn force_local(self) -> Path<Local> {
        Path::new(self.bytes)
    }
}

impl Path<Local> {
    /// `Path.isEmpty`: whether this is the root.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// `Path.parent`: the path without its last name (the root's parent is
    /// the root).
    pub fn parent(&self) -> Self {
        let p = &self.bytes;
        match p.iter().rposition(|&c| c == SEPARATOR) {
            Some(i) => Path::new(p[..i].to_vec()),
            None => Path::empty(),
        }
    }

    /// `Path.deconstructRev`: splits off the last name.
    pub fn deconstruct_rev(&self) -> Option<(Name, Self)> {
        let p = &self.bytes;
        match p.iter().rposition(|&c| c == SEPARATOR) {
            Some(i) => Some((name(&p[i + 1..]), Path::new(p[..i].to_vec()))),
            None if p.is_empty() => None,
            None => Some((name(p), Path::empty())),
        }
    }

    /// `Path.toDebugString`: the names separated by `" / "`.
    pub fn to_debug_string(&self) -> String {
        let names: Vec<_> = str_split(&self.bytes)
            .map(String::from_utf8_lossy)
            .collect();
        names.join(" / ")
    }

    /// `Path.addSuffixToFinalName`.
    pub fn add_suffix_to_final_name(&self, suffix: &[u8]) -> Self {
        Path::new([&self.bytes[..], suffix].concat())
    }

    /// `Path.addToFinalName`: inserts `suffix` before the final name's
    /// extension, if it has one.
    ///
    /// # Panics
    ///
    /// On the root, like OCaml's `assert`.
    pub fn add_to_final_name(&self, suffix: &[u8]) -> Self {
        let p = &self.bytes;
        assert!(!p.is_empty(), "Path.addToFinalName on the root");
        let slash = p.iter().rposition(|&c| c == SEPARATOR);
        let dot = p.iter().rposition(|&c| c == b'.');
        let j = match (dot, slash) {
            (Some(j), Some(i)) if j > i => j,
            (Some(j), None) => j,
            _ => p.len(),
        };
        Path::new([&p[..j], suffix, &p[j..]].concat())
    }

    /// `Path.addPrefixToFinalName`.
    ///
    /// # Panics
    ///
    /// On the root, like OCaml's `assert`.
    pub fn add_prefix_to_final_name(&self, prefix: &[u8]) -> Self {
        let p = &self.bytes;
        let i = match p.iter().rposition(|&c| c == SEPARATOR) {
            Some(i) => i + 1,
            None => {
                assert!(!p.is_empty(), "Path.addPrefixToFinalName on the root");
                0
            }
        };
        Path::new([&p[..i], prefix, &p[i..]].concat())
    }

    /// `Path.makeGlobal`.
    pub fn make_global(self) -> Path<Global> {
        Path::new(self.bytes)
    }
}

impl<S> Clone for Path<S> {
    fn clone(&self) -> Self {
        Path::new(self.bytes.clone())
    }
}

impl<S> PartialEq for Path<S> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<S> Eq for Path<S> {}

impl<S> std::hash::Hash for Path<S> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl<S> fmt::Debug for Path<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Path({:?})", String::from_utf8_lossy(&self.bytes))
    }
}

/// Names inside a path are non-empty and slash-free by construction.
fn name(bytes: &[u8]) -> Name {
    Name::from_bytes_unchecked(bytes.to_vec())
}

fn concat(p: &[u8], p2: &[u8]) -> Vec<u8> {
    if p.is_empty() {
        p2.to_vec()
    } else if p2.is_empty() {
        p.to_vec()
    } else {
        [p, &[SEPARATOR], p2].concat()
    }
}

fn remove_trailing_slashes(mut s: &[u8]) -> &[u8] {
    while let [rest @ .., SEPARATOR] = s {
        s = rest;
    }
    s
}

/// `Path.is_absolute` (`winAbspathRx` or `unixAbspathRx`).
fn is_absolute(s: &[u8], windows: bool) -> bool {
    if windows {
        let sep = |c: &u8| matches!(c, b'/' | b'\\');
        match s {
            [c, ..] if sep(c) => true,
            [d, b':', c, ..] => d.is_ascii_alphabetic() && sep(c),
            _ => false,
        }
    } else {
        s.first() == Some(&b'/')
    }
}

/// `Str.split (Str.regexp "/")`: a separator at the start or end produces no
/// empty component, but two in a row do.
fn str_split(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    let s = s.strip_prefix(b"/").unwrap_or(s);
    let s = s.strip_suffix(b"/").unwrap_or(s);
    s.split(|&c| c == SEPARATOR).filter(move |_| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(s: &str) -> Path<Global> {
        Path::from_bytes_for(s.as_bytes(), false).unwrap()
    }

    fn local(s: &str) -> Path<Local> {
        global(s).force_local()
    }

    fn n(s: &str) -> Name {
        Name::from_bytes(s).unwrap()
    }

    #[test]
    fn from_bytes_normalises_relative_paths() {
        for (input, expected) in [
            ("", ""),
            (".", ""),
            ("a", "a"),
            ("a/b", "a/b"),
            ("a//b", "a/b"),
            ("./a/./b/.", "a/b"),
            ("a/b///", "a/b"),
            ("c:", "c:"),
            ("a\\b", "a\\b"),
        ] {
            assert_eq!(global(input).as_bytes(), expected.as_bytes(), "{input:?}");
        }
    }

    #[test]
    fn from_bytes_rejects_absolute_paths_and_parent_references() {
        for input in ["/", "/a", "..", "a/..", "../a", "a/../b"] {
            assert!(
                Path::<Global>::from_bytes_for(input.as_bytes(), false).is_err(),
                "{input:?}"
            );
        }
        assert_eq!(
            Path::<Global>::from_bytes_for(b"a/../b", false),
            Err(Error::Transient(
                "Reference to parent directory '..' not allowed in path 'a/../b'".into()
            ))
        );
    }

    #[test]
    fn from_bytes_follows_windows_conventions_on_windows() {
        let win = |s: &str| Path::<Global>::from_bytes_for(s.as_bytes(), true);
        assert_eq!(win("a\\b\\").unwrap().as_bytes(), b"a/b");
        for input in ["c:/", "C:\\x", "\\x", "/x"] {
            assert!(win(input).is_err(), "{input:?}");
        }
        assert_eq!(win("c:").unwrap().as_bytes(), b"c:");
        assert_eq!(win("1:/a").unwrap().as_bytes(), b"1:/a");
    }

    #[test]
    fn global_accessors() {
        let p = global("a/b/c");
        assert_eq!(p.length(), 2);
        assert_eq!(global("a").length(), 0);
        assert_eq!(p.final_name(), Some(n("c")));
        assert_eq!(Path::<Global>::empty().final_name(), None);
        assert_eq!(p.to_names().unwrap(), [n("a"), n("b"), n("c")]);
        assert!(Path::<Global>::empty().to_names().unwrap().is_empty());
        assert_eq!(p.deconstruct(), Some((n("a"), global("b/c"))));
        assert_eq!(global("a").deconstruct(), Some((n("a"), global(""))));
        assert_eq!(Path::<Global>::empty().deconstruct(), None);
        assert_eq!(global("a").child(&n("b")), global("a/b"));
        assert_eq!(Path::<Global>::empty().child(&n("b")), global("b"));
    }

    #[test]
    fn local_accessors() {
        let p = local("a/b.tar/c.txt");
        assert_eq!(p.parent(), local("a/b.tar"));
        assert_eq!(local("a").parent(), local(""));
        assert_eq!(p.deconstruct_rev(), Some((n("c.txt"), local("a/b.tar"))));
        assert_eq!(p.to_debug_string(), "a / b.tar / c.txt");
        assert_eq!(p.add_suffix_to_final_name(b"~"), local("a/b.tar/c.txt~"));
        assert_eq!(p.add_to_final_name(b"-1"), local("a/b.tar/c-1.txt"));
        assert_eq!(
            local("a/b.tar/c").add_to_final_name(b"-1"),
            local("a/b.tar/c-1")
        );
        assert_eq!(local("x.y").add_to_final_name(b"-1"), local("x-1.y"));
        assert_eq!(p.add_prefix_to_final_name(b"."), local("a/b.tar/.c.txt"));
        assert_eq!(local("c").add_prefix_to_final_name(b"."), local(".c"));
    }

    #[test]
    fn str_split_matches_ocaml_str() {
        fn split(s: &str) -> Vec<&[u8]> {
            str_split(s.as_bytes()).collect()
        }
        assert!(split("").is_empty());
        assert!(split("/").is_empty());
        assert_eq!(split("a/b"), [&b"a"[..], b"b"]);
        assert_eq!(split("/a/b/"), [&b"a"[..], b"b"]);
        assert_eq!(split("a//b"), [&b"a"[..], b"", b"b"]);
    }

    /// `winAbspathRx` and `unixAbspathRx` as Unison's Rx engine matches them.
    #[test]
    fn is_absolute_matches_ocaml_rx() {
        let text = include_str!("../../gemel-compat/tests/fixtures/names.txt");
        let mut checked = 0;
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let [check, input, expected] = line.split(' ').collect::<Vec<_>>()[..] else {
                panic!("bad line {line:?}");
            };
            let windows = match check {
                "win_absolute" => true,
                "unix_absolute" => false,
                _ => continue,
            };
            let input = crate::test_util::unhex(input);
            assert_eq!(is_absolute(&input, windows), expected == "1", "{line}");
            checked += 1;
        }
        assert!(checked > 100);
    }

    #[test]
    fn compare_depends_on_the_mode() {
        let (a, b) = (global("Dir/File"), global("dir/file"));
        assert_ne!(a.compare_in(&b, Mode::Sensitive), Ordering::Equal);
        assert_eq!(a.compare_in(&b, Mode::Insensitive), Ordering::Equal);
    }
}
