//! File type, properties and change stamps.
//!
//! Ports Unison `src/fileinfo.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! This module has the stamp type so far; `lstat` and the file type are
//! phase 1b of milestone 2.

use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};

/// `ignoreinodenumbers` (alias `pretendwin`): don't use inode numbers in
/// fast update detection.
pub static IGNORE_INODE_NUMBERS: PrefDef<bool> = PrefDef::new(
    "ignoreinodenumbers",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "ignore inode number changes when detecting updates",
    "When set to true, this preference makes Unison not take advantage \
     of inode numbers during fast update detection. \
     This switch should be used with care, as it \
     is less safe than the standard update detection method, but it \
     can be useful with filesystems which do not support inode numbers.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&IGNORE_INODE_NUMBERS)?;
    prefs.alias(&IGNORE_INODE_NUMBERS, "pretendwin")
}

/// `Fileinfo.stamp`: what fast update detection compares besides the
/// modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stamp {
    /// The inode number (Unix).
    Inode(i64),
    /// No stamp (Windows, or `ignoreinodenumbers`).
    NoStamp,
    /// Rescan the file next time (for example after a failed transfer).
    Rescan,
}

impl Stamp {
    /// `Fileinfo.mstamp`.
    pub fn marshal(self, w: &mut Writer) {
        match self {
            Stamp::Inode(i) => {
                w.tag(0);
                w.int(i);
            }
            Stamp::NoStamp => w.tag(1),
            Stamp::Rescan => w.tag(2),
        }
    }

    /// `Fileinfo.mstamp`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Stamp> {
        Ok(match r.tag(3)? {
            0 => Stamp::Inode(r.int()?),
            1 => Stamp::NoStamp,
            _ => Stamp::Rescan,
        })
    }
}
