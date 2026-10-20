//! Extensible property lists stored in archives.
//!
//! Ports Unison `src/ubase/proplist.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! An archive header carries a property list: values of different types,
//! each under a registered name (`"case mode"`, `"paths"`, ...). On disk it
//! is an `int` count followed by `(name, value)` pairs in name order (OCaml
//! iterates a `StringMap`), each value encoded with its key's codec.
//!
//! Keys are declared as statics ([`Key`]). Reading a list needs a
//! [`Registry`] of the keys that may appear in it; OCaml keeps that table in
//! a global.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use gemel_compat::umarshal::{self, Reader, Writer};

use crate::util::Error;

/// Writes a value of type `T`.
pub type WriteFn<T> = fn(&T, &mut Writer);
/// Reads a value of type `T`.
pub type ReadFn<T> = fn(&mut Reader<'_>) -> umarshal::Result<T>;

/// A typed key (`'a Proplist.key`): a name and the value's codec.
pub struct Key<T> {
    name: &'static str,
    write: WriteFn<T>,
    read: ReadFn<T>,
}

impl<T> Key<T> {
    /// A key named `name` (`Proplist.register name m`).
    pub const fn new(name: &'static str, write: WriteFn<T>, read: ReadFn<T>) -> Self {
        Key { name, write, read }
    }

    /// The key's name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

type Value = Arc<dyn Any + Send + Sync>;
type ErasedWrite = Arc<dyn Fn(&Value, &mut Writer) + Send + Sync>;
type ErasedRead = Arc<dyn Fn(&mut Reader<'_>) -> umarshal::Result<Value> + Send + Sync>;

fn erase_write<T: Send + Sync + 'static>(write: WriteFn<T>) -> ErasedWrite {
    Arc::new(move |v, w| write(v.downcast_ref().expect("value of the key's type"), w))
}

/// The keys that may be read back (OCaml's `names` table).
#[derive(Default)]
pub struct Registry {
    keys: HashMap<&'static str, (ErasedRead, ErasedWrite)>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Proplist.register`.
    pub fn register<T: Send + Sync + 'static>(
        &mut self,
        key: &'static Key<T>,
    ) -> Result<(), Error> {
        if self.keys.contains_key(key.name) {
            return Err(Error::Fatal(format!(
                "Property lists: {} already registered!",
                key.name
            )));
        }
        let read = key.read;
        let reader: ErasedRead = Arc::new(move |r| Ok(Arc::new(read(r)?) as Value));
        self.keys.insert(key.name, (reader, erase_write(key.write)));
        Ok(())
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.keys.keys()).finish()
    }
}

/// A property list (`Proplist.t`). Cloning is cheap.
#[derive(Clone, Default)]
pub struct Proplist {
    entries: BTreeMap<String, (Value, ErasedWrite)>,
}

impl fmt::Debug for Proplist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.entries.keys()).finish()
    }
}

impl Proplist {
    /// `Proplist.empty`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Proplist.mem`.
    pub fn mem<T>(&self, key: &Key<T>) -> bool {
        self.entries.contains_key(key.name)
    }

    /// `Proplist.find`, with `None` for OCaml's `Not_found`.
    pub fn find<T: 'static>(&self, key: &Key<T>) -> Option<&T> {
        self.entries
            .get(key.name)
            .map(|(v, _)| v.downcast_ref().expect("value of the key's type"))
    }

    /// `Proplist.add`: sets the value under `key`.
    pub fn add<T: Send + Sync + 'static>(&mut self, key: &Key<T>, value: T) {
        self.entries.insert(
            key.name.to_owned(),
            (Arc::new(value), erase_write(key.write)),
        );
    }

    /// `Proplist.remove`.
    pub fn remove<T>(&mut self, key: &Key<T>) {
        self.entries.remove(key.name);
    }

    /// Number of properties.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no properties.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `Proplist.m`: marshals the list, in name order.
    pub fn marshal(&self, w: &mut Writer) {
        w.int(i64::try_from(self.entries.len()).expect("property count fits in an int"));
        for (name, (value, write)) in &self.entries {
            w.string(name.as_bytes());
            write(value, w);
        }
    }

    /// `Proplist.m`: unmarshals a list. Every name must be registered.
    pub fn unmarshal(r: &mut Reader<'_>, registry: &Registry) -> Result<Proplist, Error> {
        Proplist::read(r, registry).map_err(|e| match e {
            ReadError::Umarshal(e) => Error::Fatal(format!("Property lists: {e}")),
            ReadError::Fatal(e) => e,
        })
    }

    /// Like [`Proplist::unmarshal`], but keeps decoding errors apart from
    /// unregistered names (which OCaml raises as `Util.Fatal`).
    pub fn read(r: &mut Reader<'_>, registry: &Registry) -> Result<Proplist, ReadError> {
        let n = r.int()?;
        let mut list = Proplist::new();
        for _ in 0..n {
            let name = r.string()?;
            let name = String::from_utf8_lossy(&name).into_owned();
            let Some((read, write)) = registry.keys.get(name.as_str()) else {
                return Err(ReadError::Fatal(Error::Fatal(format!(
                    "Property lists: {name} not yet registered!"
                ))));
            };
            let value = read(r)?;
            list.entries.insert(name, (value, write.clone()));
        }
        Ok(list)
    }
}

/// Why [`Proplist::read`] failed.
#[derive(Debug)]
pub enum ReadError {
    /// The encoding is invalid (`Umarshal.Error`, or `End_of_file`).
    Umarshal(umarshal::Error),
    /// A name isn't registered.
    Fatal(Error),
}

impl From<umarshal::Error> for ReadError {
    fn from(e: umarshal::Error) -> Self {
        ReadError::Umarshal(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static CASE: Key<String> = Key::new(
        "case mode",
        |s, w| w.string(s.as_bytes()),
        |r| Ok(String::from_utf8_lossy(&r.string()?).into_owned()),
    );
    static STAMP: Key<i64> = Key::new(
        "unchanged directory stamp",
        |n, w| w.int64(*n),
        |r| r.int64(),
    );

    fn registry() -> Registry {
        let mut reg = Registry::new();
        reg.register(&CASE).unwrap();
        reg.register(&STAMP).unwrap();
        reg
    }

    #[test]
    fn add_find_remove() {
        let mut p = Proplist::new();
        assert!(!p.mem(&CASE));
        p.add(&CASE, "case sensitive".to_owned());
        assert_eq!(p.find(&CASE).map(String::as_str), Some("case sensitive"));
        p.remove(&CASE);
        assert_eq!(p.find(&CASE), None);
        let mut reg = registry();
        assert_eq!(
            reg.register(&CASE),
            Err(Error::Fatal(
                "Property lists: case mode already registered!".into()
            ))
        );
    }

    #[test]
    fn marshals_in_name_order_and_round_trips() {
        let mut p = Proplist::new();
        p.add(&STAMP, 300);
        p.add(&CASE, "x".to_owned());
        let mut w = Writer::new();
        p.marshal(&mut w);
        let bytes = w.into_payload();
        // 2 entries; "case mode" sorts before "unchanged directory stamp".
        let mut expected = vec![1, 2, 1, 9];
        expected.extend_from_slice(b"case mode");
        expected.extend_from_slice(&[1, 1, b'x', 1, 25]);
        expected.extend_from_slice(b"unchanged directory stamp");
        expected.extend_from_slice(&[2, 1, 44]);
        assert_eq!(bytes, expected);

        let back = Proplist::unmarshal(&mut Reader::new(&bytes), &registry()).unwrap();
        assert_eq!(back.find(&STAMP), Some(&300));
        assert_eq!(back.find(&CASE).map(String::as_str), Some("x"));
        let err = Proplist::unmarshal(&mut Reader::new(&bytes), &Registry::new()).unwrap_err();
        assert_eq!(
            err,
            Error::Fatal("Property lists: case mode not yet registered!".into())
        );
    }
}
