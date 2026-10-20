//! Maps ordered by the case mode: archive directories (`NameMap`) and the
//! per-path archive properties (`PathMap`).
//!
//! Ports Unison `src/ubase/myMap.ml`, as `update.ml` instantiates it.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The order of the keys is the [`Mode`]'s, and it matters: archives list
//! a directory's children in ascending order, `checkArchive` hashes them in
//! descending order, and two names the mode says are equal are the same
//! file. A map is therefore built and searched under one mode, which it
//! keeps. It is a sorted vector rather than OCaml's balanced tree; the
//! iterators say which way they go.

use std::cmp::Ordering;

use gemel_base::case::Mode;
use gemel_base::name::Name;
use gemel_base::path::{Global, Path};
use gemel_compat::umarshal::{self, Reader, Writer};

/// A key ordered by the case mode.
pub trait ModeKey: Sized {
    /// Compares two keys under `mode`.
    fn compare_in(&self, other: &Self, mode: Mode) -> Ordering;
    /// Writes the key.
    fn marshal(&self, w: &mut Writer);
    /// Reads a key.
    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Self>;
}

impl ModeKey for Name {
    fn compare_in(&self, other: &Self, mode: Mode) -> Ordering {
        Name::compare_in(self, other, mode)
    }
    fn marshal(&self, w: &mut Writer) {
        Name::marshal(self, w);
    }
    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Self> {
        Name::unmarshal(r)
    }
}

impl<S> ModeKey for Path<S> {
    /// `Path.compare`.
    fn compare_in(&self, other: &Self, mode: Mode) -> Ordering {
        mode.compare(self.as_bytes(), other.as_bytes())
    }
    fn marshal(&self, w: &mut Writer) {
        Path::marshal(self, w);
    }
    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Self> {
        Path::unmarshal(r)
    }
}

/// A map whose keys are ordered by a case mode (`MyMap.Make`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModeMap<K, V> {
    mode: Mode,
    entries: Vec<(K, V)>,
}

/// A directory's children (`NameMap`).
pub type NameMap<V> = ModeMap<Name, V>;

/// Values by path (`PathMap`, of `Path.t`).
pub type PathMap<V> = ModeMap<Path<Global>, V>;

/// What [`ModeMap::validate`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Validation<'a, K> {
    /// The keys are in strictly ascending order.
    Ok,
    /// A key occurs twice.
    Duplicate(&'a K),
    /// Two keys are out of order.
    Invalid(&'a K, &'a K),
}

impl<K: ModeKey, V> ModeMap<K, V> {
    /// An empty map ordered by `mode`.
    pub fn new(mode: Mode) -> Self {
        ModeMap {
            mode,
            entries: Vec::new(),
        }
    }

    /// Builds a map by adding the bindings in turn, as `MyMap.m` reads
    /// one: a later binding for an equal key replaces the earlier one, key
    /// and value.
    pub fn from_bindings(mode: Mode, bindings: impl IntoIterator<Item = (K, V)>) -> Self {
        let mut entries: Vec<(K, V)> = bindings.into_iter().collect();
        // Archives are written in ascending order, so this is the usual
        // case.
        let ascending = entries
            .windows(2)
            .all(|w| w[0].0.compare_in(&w[1].0, mode) == Ordering::Less);
        if !ascending {
            // Stable, so the last of equal keys is the latest; keep it.
            entries.sort_by(|a, b| a.0.compare_in(&b.0, mode));
            let mut kept: Vec<(K, V)> = Vec::with_capacity(entries.len());
            for e in entries {
                match kept.last_mut() {
                    Some(last) if last.0.compare_in(&e.0, mode) == Ordering::Equal => *last = e,
                    _ => kept.push(e),
                }
            }
            entries = kept;
        }
        ModeMap { mode, entries }
    }

    /// The same bindings ordered by `mode` (as [`ModeMap::from_bindings`]).
    pub fn with_mode(self, mode: Mode) -> Self {
        if mode == self.mode {
            self
        } else {
            ModeMap::from_bindings(mode, self.entries)
        }
    }

    /// The mode the keys are ordered by.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn search(&self, k: &K) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|(x, _)| x.compare_in(k, self.mode))
    }

    /// `add`: binds `k` to `v`, replacing an equal key and its value.
    pub fn add(&mut self, k: K, v: V) {
        match self.search(&k) {
            Ok(i) => self.entries[i] = (k, v),
            Err(i) => self.entries.insert(i, (k, v)),
        }
    }

    /// `find`.
    pub fn find(&self, k: &K) -> Option<&V> {
        self.search(k).ok().map(|i| &self.entries[i].1)
    }

    /// `findi`: the key as the map spells it, and its value.
    pub fn find_key_value(&self, k: &K) -> Option<(&K, &V)> {
        self.search(k).ok().map(|i| {
            let (k, v) = &self.entries[i];
            (k, v)
        })
    }

    /// `find`, for changing the value.
    pub fn find_mut(&mut self, k: &K) -> Option<&mut V> {
        self.search(k).ok().map(|i| &mut self.entries[i].1)
    }

    /// `mem`.
    pub fn mem(&self, k: &K) -> bool {
        self.search(k).is_ok()
    }

    /// `remove`: the value that was bound to `k`, if any.
    pub fn remove(&mut self, k: &K) -> Option<V> {
        self.search(k).ok().map(|i| self.entries.remove(i).1)
    }

    /// The number of bindings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `is_empty`.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The bindings in ascending order (`iter`).
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// The bindings in descending order, the order `fold` visits them in.
    pub fn iter_desc(&self) -> impl Iterator<Item = (&K, &V)> {
        self.iter().rev()
    }

    /// The bindings in ascending order, for changing the values.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    /// The bindings in ascending order.
    pub fn into_bindings(self) -> Vec<(K, V)> {
        self.entries
    }

    /// `validate`: whether the keys are in strictly ascending order under
    /// `mode`, which may differ from the map's. The first pair out of order
    /// wins over duplicates, and the last duplicate over earlier ones, as
    /// OCaml combines what it finds.
    pub fn validate(&self, mode: Mode) -> Validation<'_, K> {
        let mut dup = None;
        for pair in self.entries.windows(2) {
            let (a, b) = (&pair[0].0, &pair[1].0);
            match a.compare_in(b, mode) {
                Ordering::Less => {}
                Ordering::Equal => dup = Some(a),
                Ordering::Greater => return Validation::Invalid(a, b),
            }
        }
        dup.map_or(Validation::Ok, Validation::Duplicate)
    }

    /// `MyMap.m`: the bindings as a list, in ascending order.
    pub fn marshal(&self, w: &mut Writer, mut write: impl FnMut(&V, &mut Writer)) {
        w.list(&self.entries, |w, (k, v)| {
            k.marshal(w);
            write(v, w);
        });
    }

    /// `MyMap.m`: reads the list and adds the bindings in turn under `mode`
    /// (see [`ModeMap::from_bindings`]).
    pub fn unmarshal<E: From<umarshal::Error>>(
        r: &mut Reader<'_>,
        mode: Mode,
        mut read: impl FnMut(&mut Reader<'_>) -> Result<V, E>,
    ) -> Result<Self, E> {
        let n = r.int()?;
        if n < 0 {
            return Err(umarshal::Error::NegativeLength(n).into());
        }
        let mut bindings = Vec::new();
        for _ in 0..n {
            let k = K::unmarshal(r)?;
            bindings.push((k, read(r)?));
        }
        Ok(ModeMap::from_bindings(mode, bindings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Name {
        Name::from_bytes(s).unwrap()
    }

    fn keys<V>(m: &NameMap<V>) -> Vec<&str> {
        m.iter()
            .map(|(k, _)| std::str::from_utf8(k.as_bytes()).unwrap())
            .collect()
    }

    #[test]
    fn bindings_are_sorted_by_the_mode_and_later_ones_win() {
        let m = NameMap::from_bindings(
            Mode::Insensitive,
            [
                (name("b"), 1),
                (name("A"), 2),
                (name("a"), 3),
                (name("C"), 4),
            ],
        );
        // "a" replaced "A", key and value.
        assert_eq!(keys(&m), ["a", "b", "C"]);
        assert_eq!(m.find(&name("A")), Some(&3));
        let desc: Vec<_> = m.iter_desc().map(|(_, v)| *v).collect();
        assert_eq!(desc, [4, 1, 3]);

        let s = NameMap::from_bindings(Mode::Sensitive, [(name("b"), 1), (name("C"), 2)]);
        assert_eq!(keys(&s), ["C", "b"]);
    }

    #[test]
    fn add_find_remove() {
        let mut m = NameMap::new(Mode::Sensitive);
        m.add(name("x"), 1);
        m.add(name("a"), 2);
        m.add(name("x"), 3);
        assert_eq!(m.len(), 2);
        assert!(m.mem(&name("a")));
        assert_eq!(m.remove(&name("x")), Some(3));
        assert_eq!(m.remove(&name("x")), None);
        assert_eq!(keys(&m), ["a"]);
    }

    #[test]
    fn validation_under_another_mode() {
        let m = NameMap::from_bindings(
            Mode::Sensitive,
            [(name("A"), ()), (name("B"), ()), (name("a"), ())],
        );
        assert_eq!(m.validate(Mode::Sensitive), Validation::Ok);
        assert_eq!(
            m.validate(Mode::Insensitive),
            Validation::Invalid(&name("B"), &name("a"))
        );
        let d = NameMap::from_bindings(Mode::Sensitive, [(name("A"), ()), (name("a"), ())]);
        assert_eq!(
            d.validate(Mode::Insensitive),
            Validation::Duplicate(&name("A"))
        );
    }

    #[test]
    fn codec_is_an_ascending_list() {
        let m = NameMap::from_bindings(Mode::Sensitive, [(name("b"), 2), (name("a"), 1)]);
        let bytes = umarshal::to_bytes(|w| m.marshal(w, |v, w| w.int(*v)));
        let expected = umarshal::to_bytes(|w| {
            w.list(&[("a", 1), ("b", 2)], |w, (k, v)| {
                w.string(k.as_bytes());
                w.int(*v);
            })
        });
        assert_eq!(bytes, expected);
        let back: NameMap<i64> = umarshal::from_bytes(&bytes, |r| {
            NameMap::unmarshal(r, Mode::Sensitive, |r| r.int())
        })
        .unwrap();
        assert_eq!(back, m);
    }
}
