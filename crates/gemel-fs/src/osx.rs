//! macOS resource forks and Finder information.
//!
//! Ports Unison `src/osx.ml`, Unison `src/osxsupport.c`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//! Derived from Unison, Copyright 1999-2008.
//!
//! This module has the value types so far; reading and writing resource
//! forks and Finder information is phase 1b of milestone 2.

use gemel_base::prefs::{BoolWithDefault, Category, Internal, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};

use crate::props::Filesize;

/// `rsrc`: synchronise resource forks and HFS metadata
/// (`true`/`false`/`default`; by default, when either host runs macOS).
pub static RSRC: PrefDef<BoolWithDefault> = PrefDef::new(
    "rsrc",
    Category::Advanced(Topic::Sync),
    BoolWithDefault::default,
    "synchronize resource forks (true/false/default)",
    "When set to {\\tt true}, this flag causes Unison to synchronize \
     resource forks and HFS meta-data.  On filesystems that do not \
     natively support resource forks, this data is stored in \
     Carbon-compatible .\\_ AppleDouble files.  When the flag is set \
     to {\\tt false}, Unison will not synchronize these data.  \
     Ordinarily, the flag is set to {\\tt default}, and these data are\n     \
     automatically synchronized if either host is running OSX.  In \
     rare circumstances it is useful to set the flag manually.",
);

/// `rsrc-aux`: the resolved `rsrc`, a pseudo-preference so that it is sent
/// to the server.
pub static RSRC_AUX: PrefDef<bool> = PrefDef::new(
    "rsrc-aux",
    Category::Internal(Internal::Pseudo),
    || false,
    "*synchronize resource forks and HFS meta-data",
    "",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&RSRC)?;
    prefs.register(&RSRC_AUX)
}

/// `Osx.init`: resolves `rsrc` once it is known whether a host runs macOS.
pub fn init(prefs: &mut Prefs, some_host_running_osx: bool) {
    let r = *prefs.read(&RSRC);
    prefs.set(
        &RSRC_AUX,
        r == BoolWithDefault::True || (r == BoolWithDefault::Default && some_host_running_osx),
    );
}

/// `Osx.ressInfo`: where a file's resource fork is.
#[derive(Debug, Clone, PartialEq)]
pub enum RessInfo<T> {
    /// No resource fork.
    NoRess,
    /// A native (HFS) resource fork of this length.
    HfsRess(Filesize),
    /// In an AppleDouble `._` file: its inode, mtime, ctime, the fork's
    /// length, and `T`.
    AppleDoubleRess(i64, f64, f64, Filesize, T),
}

/// `Osx.ressStamp`: what archives record about the resource fork.
pub type RessStamp = RessInfo<()>;

impl<T> RessInfo<T> {
    /// `Osx.mressInfo`, with `T`'s own codec.
    pub fn marshal(&self, w: &mut Writer, m: impl FnOnce(&mut Writer, &T)) {
        match self {
            RessInfo::NoRess => w.tag(0),
            RessInfo::HfsRess(len) => {
                w.tag(1);
                w.int64(*len);
            }
            RessInfo::AppleDoubleRess(ino, mtime, ctime, len, x) => {
                w.tag(2);
                w.int(*ino);
                w.float(*mtime);
                w.float(*ctime);
                w.int64(*len);
                m(w, x);
            }
        }
    }

    /// `Osx.mressInfo`.
    pub fn unmarshal(
        r: &mut Reader<'_>,
        m: impl FnOnce(&mut Reader<'_>) -> umarshal::Result<T>,
    ) -> umarshal::Result<RessInfo<T>> {
        Ok(match r.tag(3)? {
            0 => RessInfo::NoRess,
            1 => RessInfo::HfsRess(r.int64()?),
            _ => RessInfo::AppleDoubleRess(r.int()?, r.float()?, r.float()?, r.int64()?, m(r)?),
        })
    }

    /// The stamp of this information (`Osx.stamp`).
    pub fn stamp(&self) -> RessStamp {
        match self {
            RessInfo::NoRess => RessInfo::NoRess,
            RessInfo::HfsRess(len) => RessInfo::HfsRess(*len),
            RessInfo::AppleDoubleRess(ino, mtime, ctime, len, _) => {
                RessInfo::AppleDoubleRess(*ino, *mtime, *ctime, *len, ())
            }
        }
    }
}

impl RessStamp {
    /// `Osx.mressStamp`.
    pub fn marshal_stamp(&self, w: &mut Writer) {
        self.marshal(w, |_, ()| {});
    }

    /// `Osx.mressStamp`.
    pub fn unmarshal_stamp(r: &mut Reader<'_>) -> umarshal::Result<RessStamp> {
        RessInfo::unmarshal(r, |_| Ok(()))
    }

    /// `Osx.ressStampToString`.
    pub fn stamp_to_string(&self) -> String {
        match self {
            RessInfo::NoRess => "NoRess".into(),
            RessInfo::HfsRess(len) => format!("Hfs({len})"),
            RessInfo::AppleDoubleRess(ino, mtime, ctime, len, ()) => {
                format!("Hfs({ino},{mtime:.6},{ctime:.6},{len})")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_round_trip() {
        for s in [
            RessStamp::NoRess,
            RessStamp::HfsRess(10),
            RessStamp::AppleDoubleRess(7, 1.5, 2.5, 30, ()),
        ] {
            let b = umarshal::to_bytes(|w| s.marshal_stamp(w));
            assert_eq!(umarshal::from_bytes(&b, RessStamp::unmarshal_stamp), Ok(s));
        }
        assert_eq!(RessStamp::HfsRess(3).stamp_to_string(), "Hfs(3)");
    }

    #[test]
    fn init_resolves_rsrc() {
        let mut p = Prefs::new();
        register_prefs(&mut p).unwrap();
        init(&mut p, true);
        assert!(*p.read(&RSRC_AUX));
        init(&mut p, false);
        assert!(!*p.read(&RSRC_AUX));
    }
}
