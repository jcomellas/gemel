//! Data shared between props: extended attributes' values and ACLs, by key
//! (`Propsdata`).
//!
//! Ports Unison `src/propsdata.ml`.
//!
//! Derived from Unison, Copyright 2020-2022, Tõivo Leedjärv.
//!
//! A host keeps one [`Store`] per kind of data. The xattr store is a cache
//! of attribute values by their MD5, which update detection fills and
//! copies read from; the ACL store holds ACL texts by key, which archives
//! keep. What a host adds is also recorded as new, to be sent to the other
//! hosts, and what its archives still use is recorded as kept, to be
//! written with them.

use std::collections::HashMap;

/// Which of a store's entries ([`Store::get`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Every entry.
    All,
    /// Those added on this host since the store was last set.
    New,
    /// Those kept for the archives.
    Kept,
}

/// `KVStore`: entries by key, with the new and kept ones apart.
#[derive(Debug, Clone, Default)]
pub struct Store {
    main: HashMap<Vec<u8>, Vec<u8>>,
    new: HashMap<Vec<u8>, Vec<u8>>,
    keep: HashMap<Vec<u8>, Vec<u8>>,
}

/// One entry's key and value.
pub type Entry = (Vec<u8>, Vec<u8>);

impl Store {
    /// `exists`.
    pub fn exists(&self, key: &[u8]) -> bool {
        self.main.contains_key(key)
    }

    /// `find_opt`.
    pub fn find_opt(&self, key: &[u8]) -> Option<&Vec<u8>> {
        self.main.get(key)
    }

    /// `add`: adds an entry this host made, unless the key is known.
    pub fn add(&mut self, key: &[u8], value: &[u8]) {
        if !self.exists(key) {
            self.main.insert(key.to_vec(), value.to_vec());
            self.new.insert(key.to_vec(), value.to_vec());
        }
    }

    /// `get`: the entries of `kind`, in no particular order.
    pub fn get(&self, kind: Kind) -> Vec<Entry> {
        let store = match kind {
            Kind::All => &self.main,
            Kind::New => &self.new,
            Kind::Kept => &self.keep,
        };
        store.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// `set`: these entries and no others, none of them new.
    pub fn set(&mut self, d: &[Entry]) {
        self.main.clear();
        self.new.clear();
        for (k, v) in d {
            self.main.insert(k.clone(), v.clone());
        }
    }

    /// `merge`: adds another host's entries. A key known with another
    /// value is an internal error.
    pub fn merge(&mut self, d: &[Entry]) -> Result<(), String> {
        for (key, value) in d {
            match self.main.get(key) {
                None => {
                    self.main.insert(key.clone(), value.clone());
                }
                Some(v) if v == value => {}
                Some(v) => {
                    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
                    return Err(format!(
                        "Internal integrity error (propsdata). Key {} returns different \
                         results:\n  (existing) {}\nand\n  (new) {}\n",
                        s(key),
                        s(v),
                        s(value)
                    ));
                }
            }
        }
        Ok(())
    }

    /// `clear \`Kept`.
    pub fn clear_kept(&mut self) {
        self.keep.clear();
    }

    /// `keep`: records that the archives still use `key`, which must be
    /// known.
    pub fn keep(&mut self, key: &[u8]) {
        if !self.keep.contains_key(key) {
            let v = self.main.get(key).expect("propsdata: kept key is known");
            self.keep.insert(key.to_vec(), v.clone());
        }
    }

    /// `length`: how many entries there are.
    pub fn len(&self) -> usize {
        self.main.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.main.is_empty()
    }
}

/// `Props.Data`'s stores on one host.
#[derive(Debug, Clone, Default)]
pub struct PropsData {
    /// `Propsdata.Xattr`: attribute values by MD5.
    pub xattr: Store,
    /// `Propsdata.ACL`: ACL texts by key.
    pub acl: Store,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<Entry>) -> Vec<Entry> {
        v.sort();
        v
    }

    #[test]
    fn entries_are_added_set_merged_and_kept() {
        let mut s = Store::default();
        s.add(b"k", b"v");
        s.add(b"k", b"other");
        assert_eq!(s.find_opt(b"k").unwrap(), b"v");
        assert_eq!(s.get(Kind::New), [(b"k".to_vec(), b"v".to_vec())]);
        // Merged entries aren't new; the same value twice is fine.
        s.merge(&[
            (b"m".to_vec(), b"w".to_vec()),
            (b"k".to_vec(), b"v".to_vec()),
        ])
        .unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.get(Kind::New).len(), 1);
        let e = s.merge(&[(b"k".to_vec(), b"x".to_vec())]).unwrap_err();
        assert!(
            e.starts_with("Internal integrity error (propsdata). Key k"),
            "{e}"
        );
        s.keep(b"m");
        assert_eq!(s.get(Kind::Kept), [(b"m".to_vec(), b"w".to_vec())]);
        s.clear_kept();
        assert!(s.get(Kind::Kept).is_empty());
        // Set: nothing new any more.
        s.set(&[(b"a".to_vec(), b"1".to_vec())]);
        assert_eq!(sorted(s.get(Kind::All)), [(b"a".to_vec(), b"1".to_vec())]);
        assert!(s.get(Kind::New).is_empty());
    }
}
