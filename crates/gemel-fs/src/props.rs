//! Permissions, ownership, times, xattrs and ACLs.
//!
//! Ports Unison `src/props.ml`, Unison `src/props_xattr.c`, Unison `src/props_acl.c`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//! Derived from Unison, Copyright 2020-2022, Tõivo Leedjärv.
//!
//! [`Props`] is a file's (or directory's) metadata as Unison stores it in
//! archives and compares it between replicas. Each part has the same set of
//! operations as in OCaml:
//!
//! - `similar`: equal as far as synchronisation is concerned;
//! - `override`: the result of propagating the other side's value here;
//! - `strip`: drop the parts that aren't synchronised;
//! - `diff`: what changed, for setting only that;
//! - `hash`: the `checkArchive` contribution, which must match OCaml;
//! - `to_string`: the text the user interfaces show.
//!
//! The OCaml functions read preferences; here they take a [`PropsConfig`]
//! built from them. This module holds only values; reading and setting
//! metadata on disk is phase 1b of milestone 2.
//!
//! xattrs and ACLs: their archive format is here, their synchronisation
//! isn't yet. OCaml Unison 2.54 enables the `Sync: xattr` and `Sync: ACL`
//! features whenever both sides support them, even with the `xattrs` and
//! `acl` preferences off, so its archives carry the change time, xattr and
//! ACL fields ([`PropsConfig::xattr_enabled`], [`PropsConfig::acl_enabled`]),
//! and gemel must read and write them. Not ported yet: reading and setting
//! xattrs and ACLs, the `xattrs` and `acl` preferences (so
//! [`PropsConfig::sync_xattrs`] and [`PropsConfig::sync_acl`] stay off),
//! ACL comparison under `numericids`, and the "props data" store that ACL
//! strings refer to.

use gemel_base::features::Features;
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::ocaml_hash::{self, Value};
use gemel_compat::umarshal::{self, Reader, Writer};

use gemel_base::path::{Local, Path};

use crate::fs;
use crate::fspath::Fspath;
use crate::osx;
use crate::system::{self, Kind, Stats};

/// `Uutil.Filesize.t`: a size in bytes.
pub type Filesize = i64;

/// `Uutil.Filesize.dummy`.
pub const FILESIZE_DUMMY: Filesize = -1;

// ---- Preferences ----

/// `perms`: the permission bits that are synchronised.
pub static PERMS: PrefDef<i64> = PrefDef::new(
    "perms",
    Category::Basic(Topic::Sync),
    || 0o777 + 0o1000,
    "part of the permissions which is synchronized",
    "The integer value of this preference is a mask indicating which \
     permission bits should be synchronized.  It is set by default to \
     $0o1777$: all bits but the set-uid and set-gid bits are \
     synchronised (synchronizing these latter bits can be a security \
     hazard).  If you want to synchronize all bits, you can set the \
     value of this preference to $-1$.  If one of the replica is on \
     a FAT [Windows] filesystem, you should consider using the \
     {\\tt fat} preference instead of this preference.  If you need \
     Unison not to set permissions at all, set the value of this \
     preference to $0$ and set the preference {\\tt dontchmod} to {\\tt true}.",
);

/// `dontchmod`: never call chmod.
pub static DONT_CHMOD: PrefDef<bool> = PrefDef::new(
    "dontchmod",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "when set, never use the chmod system call",
    "By default, Unison uses the 'chmod' system call to set the permission bits \
     of files after it has copied them.  But in some circumstances (and under  \
     some operating systems), the chmod call always fails.  Setting this  \
     preference completely prevents Unison from ever calling chmod.",
);

/// `numericids`: synchronise uids and gids as numbers, not names.
pub static NUMERIC_IDS: PrefDef<bool> = PrefDef::new(
    "numericids",
    Category::Advanced(Topic::Syncprocess),
    || false,
    "don't map uid/gid values by user/group names",
    "When this flag is set to \\verb|true|, groups and users are \
     synchronized numerically, rather than by name. \n\
     \n\
     The special uid 0 and the special group 0 are never mapped via \
     user/group names even if this preference is not set.",
);

/// `owner`: synchronise file owners.
pub static OWNER: PrefDef<bool> = PrefDef::new(
    "owner",
    Category::Basic(Topic::Sync),
    || false,
    "synchronize owner",
    "When this flag is set to \\verb|true|, the owner attributes \
     of the files are synchronized.  \
     Whether the owner names or the owner identifiers are synchronized\
     depends on the preference \\texttt{numerids}.",
);

/// `group`: synchronise file groups.
pub static GROUP: PrefDef<bool> = PrefDef::new(
    "group",
    Category::Basic(Topic::Sync),
    || false,
    "synchronize group attributes",
    "When this flag is set to \\verb|true|, the group attributes \
     of the files are synchronized.  \
     Whether the group names or the group identifiers are synchronized \
     depends on the preference \\texttt{numerids}.",
);

/// `times`: synchronise file modification times.
pub static TIMES: PrefDef<bool> = PrefDef::new(
    "times",
    Category::Basic(Topic::Sync),
    || false,
    "synchronize modification times",
    "When this flag is set to \\verb|true|, \
     file modification times (but not directory modtimes) are propagated.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&PERMS)?;
    prefs.register(&DONT_CHMOD)?;
    prefs.register(&NUMERIC_IDS)?;
    prefs.alias(&NUMERIC_IDS, "numericIds")?;
    prefs.register(&OWNER)?;
    prefs.register(&GROUP)?;
    prefs.register(&TIMES)
}

/// The feature that adds the change time and xattrs to props.
pub const FEATURE_XATTR: &str = "Sync: xattr";

/// The feature that adds the change time and the ACL to props.
pub const FEATURE_ACL: &str = "Sync: ACL";

/// Registers this module's features, in OCaml's order. Both change the
/// archive format. (OCaml's validators check the `xattrs` and `acl`
/// preferences, which aren't ported, so there are none.)
pub fn register_features(features: &mut Features) -> Result<(), Error> {
    features.register(FEATURE_XATTR, true, None)?;
    features.register(FEATURE_ACL, true, None)?;
    Ok(())
}

/// The preferences and negotiated features the props operations depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropsConfig {
    /// `perms`.
    pub perm_mask: i64,
    /// `owner`.
    pub sync_owner: bool,
    /// `group`.
    pub sync_group: bool,
    /// `numericids`.
    pub numeric_ids: bool,
    /// `times`.
    pub sync_times: bool,
    /// `rsrc-aux`, the resolved `rsrc` (see [`osx`]).
    pub rsrc: bool,
    /// `xattrs`.
    pub sync_xattrs: bool,
    /// `acl`.
    pub sync_acl: bool,
    /// Feature `Sync: xattr` negotiated.
    pub xattr_enabled: bool,
    /// Feature `Sync: ACL` negotiated.
    pub acl_enabled: bool,
}

impl Default for PropsConfig {
    /// Unison's defaults, without xattrs or ACLs.
    fn default() -> Self {
        PropsConfig {
            perm_mask: 0o1777,
            sync_owner: false,
            sync_group: false,
            numeric_ids: false,
            sync_times: false,
            rsrc: false,
            sync_xattrs: false,
            sync_acl: false,
            xattr_enabled: false,
            acl_enabled: false,
        }
    }
}

impl PropsConfig {
    /// Reads the configuration from preferences registered with
    /// [`register_prefs`] and [`osx::register_prefs`]. The caller sets the
    /// feature flags from negotiation; xattr and ACL synchronisation stays
    /// off (see the module documentation).
    pub fn from_prefs(prefs: &Prefs) -> Self {
        PropsConfig {
            perm_mask: *prefs.read(&PERMS),
            sync_owner: *prefs.read(&OWNER),
            sync_group: *prefs.read(&GROUP),
            numeric_ids: *prefs.read(&NUMERIC_IDS),
            sync_times: *prefs.read(&TIMES),
            rsrc: *prefs.read(&osx::RSRC_AUX),
            ..PropsConfig::default()
        }
    }

    /// The configuration with the feature flags from `features`.
    pub fn with_features(self, features: &Features) -> Self {
        PropsConfig {
            xattr_enabled: features.enabled_by_name(FEATURE_XATTR),
            acl_enabled: features.enabled_by_name(FEATURE_ACL),
            ..self
        }
    }

    /// `Perm.validatePrefs`.
    pub fn validate(prefs: &Prefs) -> Result<(), Error> {
        if *prefs.read(&DONT_CHMOD) && *prefs.read(&PERMS) != 0 {
            return Err(Error::Fatal(
                "If the 'dontchmod' preference is set, the 'perms' preference should be 0".into(),
            ));
        }
        Ok(())
    }
}

// ---- Permissions ----

/// `Perm.t`: the permission bits and the mask of the bits that are known.
/// `(0, 0)` ([`Perm::DUMMY`]) is the "never synchronised" permission, which
/// is similar to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Perm {
    /// The permission bits.
    pub bits: i64,
    /// Which bits are meaningful.
    pub mask: i64,
}

/// Only the write bit can be changed on Windows (`wind_mask`).
pub const WINDOWS_PERM_MASK: i64 = 0o200;

impl Perm {
    /// `Perm.dummy`.
    pub const DUMMY: Perm = Perm { bits: 0, mask: 0 };

    /// The bits, as for chmod (`Perm.extract`).
    pub fn extract(self) -> i64 {
        self.bits
    }

    fn marshal(self, w: &mut Writer) {
        w.int(self.bits);
        w.int(self.mask);
    }

    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Perm> {
        Ok(Perm {
            bits: r.int()?,
            mask: r.int()?,
        })
    }

    fn hash(self, h: i64) -> i64 {
        ocaml_hash::hash2(self.bits & self.mask, ocaml_hash::hash2(self.mask, h))
    }

    fn similar(self, other: Perm, c: &PropsConfig) -> bool {
        let m = c.perm_mask;
        self.mask & m == m && other.mask & m == m && self.bits & m == other.bits & m
    }

    fn override_with(self, other: Perm, c: &PropsConfig) -> Perm {
        let m = c.perm_mask & other.mask;
        Perm {
            bits: (self.bits & !m) | (other.bits & m),
            mask: m,
        }
    }

    fn strip(self, c: &PropsConfig) -> Perm {
        Perm {
            bits: self.bits,
            mask: self.mask & c.perm_mask,
        }
    }

    fn diff(self, other: Perm) -> Perm {
        Perm {
            bits: other.bits,
            mask: (self.bits ^ other.bits) & self.mask & other.mask,
        }
    }

    fn bits_to_string(self, m: i64) -> String {
        let bit = |mb: i64, unknown: &'static str, off: &'static str, on: &'static str| {
            if mb & m == 0 {
                unknown
            } else if self.bits & mb != 0 {
                on
            } else {
                off
            }
        };
        [
            bit(0o4000, "", "-", "S"),
            bit(0o2000, "", "-", "s"),
            bit(0o1000, "?", "", "t"),
            bit(0o0400, "?", "-", "r"),
            bit(0o0200, "?", "-", "w"),
            bit(0o0100, "?", "-", "x"),
            bit(0o0040, "?", "-", "r"),
            bit(0o0020, "?", "-", "w"),
            bit(0o0010, "?", "-", "x"),
            bit(0o0004, "?", "-", "r"),
            bit(0o0002, "?", "-", "w"),
            bit(0o0001, "?", "-", "x"),
        ]
        .concat()
    }

    /// `Perm.toString`, e.g. `"rw-r--r--"`.
    pub fn to_string(self, c: &PropsConfig) -> String {
        if self.mask == 0 {
            "unknown permissions".into()
        } else if c.perm_mask == WINDOWS_PERM_MASK {
            if self.bits & WINDOWS_PERM_MASK != 0 {
                "read-write"
            } else {
                "read-only"
            }
            .into()
        } else {
            self.bits_to_string(c.perm_mask)
        }
    }

    /// `Perm.syncedPartsToString`: like [`Perm::to_string`], by the perm's
    /// own mask.
    pub fn synced_parts_to_string(self) -> String {
        if self.mask == 0 {
            "unknown permissions".into()
        } else {
            self.bits_to_string(self.mask)
        }
    }
}

// ---- User and group ids ----

/// `Uid.t` and `Gid.t`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Id {
    /// Not synchronised.
    Ignored,
    /// By name.
    Named(Vec<u8>),
    /// By number.
    Numeric(i64),
}

/// Which id an [`Id`] is, for its `sync` preference and its printed name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    /// The owner.
    User,
    /// The group.
    Group,
}

impl IdKind {
    fn synced(self, c: &PropsConfig) -> bool {
        match self {
            IdKind::User => c.sync_owner,
            IdKind::Group => c.sync_group,
        }
    }

    fn name(self) -> &'static str {
        match self {
            IdKind::User => "user",
            IdKind::Group => "group",
        }
    }
}

impl Id {
    fn marshal(&self, w: &mut Writer) {
        match self {
            Id::Ignored => w.tag(0),
            Id::Named(n) => {
                w.tag(1);
                w.string(n);
            }
            Id::Numeric(i) => {
                w.tag(2);
                w.int(*i);
            }
        }
    }

    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Id> {
        Ok(match r.tag(3)? {
            0 => Id::Ignored,
            1 => Id::Named(r.string()?),
            _ => Id::Numeric(r.int()?),
        })
    }

    fn hash(&self, h: i64) -> i64 {
        let v = match self {
            Id::Ignored => -1,
            Id::Numeric(i) => *i,
            Id::Named(n) => ocaml_hash::hash_string(n),
        };
        ocaml_hash::hash2(v, h)
    }

    fn similar(&self, other: &Id, kind: IdKind, c: &PropsConfig) -> bool {
        !kind.synced(c) || (*self != Id::Ignored && *other != Id::Ignored && self == other)
    }

    fn strip(&self, kind: IdKind, c: &PropsConfig) -> Id {
        if kind.synced(c) {
            self.clone()
        } else {
            Id::Ignored
        }
    }

    fn diff(&self, other: &Id, kind: IdKind, c: &PropsConfig) -> Id {
        if self.similar(other, kind, c) {
            Id::Ignored
        } else {
            other.clone()
        }
    }

    /// `Uid.toString` / `Gid.toString`, e.g. `" user=me"`.
    pub fn to_string(&self, kind: IdKind) -> String {
        match self {
            Id::Ignored => String::new(),
            Id::Numeric(i) => format!(" {}={i}", kind.name()),
            Id::Named(n) => format!(" {}={}", kind.name(), String::from_utf8_lossy(n)),
        }
    }
}

// ---- Modification time ----

/// `Time.t`: a modification time, and whether it is synchronised.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Time {
    /// Synchronised (`times` set when it was recorded).
    Synced(f64),
    /// Recorded but not synchronised.
    NotSynced(f64),
}

impl Time {
    /// `Time.dummy`.
    pub const DUMMY: Time = Time::NotSynced(0.0);

    /// The time, in seconds since the epoch (`Time.extract`).
    pub fn extract(self) -> f64 {
        match self {
            Time::Synced(v) | Time::NotSynced(v) => v,
        }
    }

    fn marshal(self, w: &mut Writer) {
        match self {
            Time::Synced(v) => {
                w.tag(0);
                w.float(v);
            }
            Time::NotSynced(v) => {
                w.tag(1);
                w.float(v);
            }
        }
    }

    fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Time> {
        Ok(match r.tag(2)? {
            0 => Time::Synced(r.float()?),
            _ => Time::NotSynced(r.float()?),
        })
    }

    fn hash(self, h: i64) -> i64 {
        // One-second differences are ignored, so the hash can only say
        // whether the time is synchronised.
        ocaml_hash::hash2(i64::from(matches!(self, Time::Synced(_))), h)
    }

    /// `Time.similar`: within a second of each other (FAT has two-second
    /// granularity, and sub-second times are rounded unpredictably).
    pub fn similar(self, other: Time, c: &PropsConfig) -> bool {
        !c.sync_times
            || match (self, other) {
                // Int64.of_float truncates, like `as`.
                (Time::Synced(v), Time::Synced(v2)) => {
                    matches!((v as i64).wrapping_sub(v2 as i64), -1..=1)
                }
                (Time::NotSynced(_), Time::NotSynced(_)) => true,
                _ => false,
            }
    }

    fn override_with(self, other: Time) -> Time {
        match (self, other) {
            (_, Time::Synced(_)) => other,
            (Time::Synced(v), _) => Time::NotSynced(v),
            _ => self,
        }
    }

    /// `Time.replace`: the same kind of time, with another value.
    pub fn replace(self, v: f64) -> Time {
        match self {
            Time::Synced(_) => Time::Synced(v),
            Time::NotSynced(_) => Time::NotSynced(v),
        }
    }

    fn strip(self, c: &PropsConfig) -> Time {
        match self {
            Time::Synced(v) if !c.sync_times => Time::NotSynced(v),
            _ => self,
        }
    }

    fn diff(self, other: Time, c: &PropsConfig) -> Time {
        if self.similar(other, c) {
            Time::NotSynced(other.extract())
        } else {
            other
        }
    }

    /// `Time.same`: similar, or (when not both synchronised) equal or an
    /// hour apart (a DST change).
    pub fn same(self, other: Time, c: &PropsConfig) -> bool {
        match (self, other) {
            (Time::Synced(_), Time::Synced(_)) => self.similar(other, c),
            _ => {
                let delta = self.extract() - other.extract();
                delta == 0.0 || delta == 3600.0 || delta == -3600.0
            }
        }
    }
}

// ---- Type and creator (macOS Finder information) ----

/// `TypeCreator.t`: the Finder information, if any.
pub type TypeCreator = Option<Vec<u8>>;

fn type_creator_hash(t: &TypeCreator, h: i64) -> i64 {
    let v = Value::option(t.as_ref().map(|s| Value::string(s.clone())));
    ocaml_hash::hash2(ocaml_hash::hash(&v), h)
}

fn type_creator_similar(t: &TypeCreator, t2: &TypeCreator, c: &PropsConfig) -> bool {
    !c.rsrc || t == t2
}

/// `String.escaped` for the bytes shown here.
fn escaped(s: &[u8]) -> String {
    let mut out = String::new();
    for &b in s {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            0x08 => out.push_str("\\b"),
            0x20..=0x7e => out.push(char::from(b)),
            _ => out.push_str(&format!("\\{b:03}")),
        }
    }
    out
}

/// `TypeCreator.toString`: `" TYPE CRTR"` for Finder information that
/// starts with `F` and has a non-zero type or creator.
pub fn type_creator_to_string(t: &TypeCreator) -> String {
    const ZEROES: [u8; 8] = [0; 8];
    match t {
        Some(s) if s.first() == Some(&b'F') => {
            let s = [&s[..], &ZEROES].concat();
            if s[1..9] == ZEROES {
                return String::new();
            }
            format!(" {} {}", escaped(&s[1..5]), escaped(&s[5..9]))
        }
        _ => String::new(),
    }
}

// ---- Extended attributes ----

/// One extended attribute's value as stored (`Props.attrvalue`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttrValue {
    /// The value itself (small values).
    String(Vec<u8>),
    /// The MD5 of the value.
    Hash(Vec<u8>),
    /// The value and its MD5.
    Loaded(Vec<u8>, Vec<u8>),
}

/// `Xattr.t`: the attributes and their total size; `None` where xattrs
/// are not supported.
pub type Xattrs = Option<(Vec<(Vec<u8>, AttrValue)>, Filesize)>;

fn attr_value_marshal(v: &AttrValue, w: &mut Writer) {
    match v {
        AttrValue::String(s) => {
            w.tag(0);
            w.string(s);
        }
        AttrValue::Hash(s) => {
            w.tag(1);
            w.string(s);
        }
        AttrValue::Loaded(a, b) => {
            w.tag(2);
            w.string(a);
            w.string(b);
        }
    }
}

fn attr_value_unmarshal(r: &mut Reader<'_>) -> umarshal::Result<AttrValue> {
    Ok(match r.tag(3)? {
        0 => AttrValue::String(r.string()?),
        1 => AttrValue::Hash(r.string()?),
        _ => AttrValue::Loaded(r.string()?, r.string()?),
    })
}

fn xattrs_marshal(x: &Xattrs, w: &mut Writer) {
    w.option(x.as_ref(), |w, (attrs, size)| {
        w.list(attrs, |w, (name, value)| {
            w.string(name);
            attr_value_marshal(value, w);
        });
        w.int64(*size);
    });
}

fn xattrs_unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Xattrs> {
    r.option(|r| {
        Ok((
            r.list(|r| Ok((r.string()?, attr_value_unmarshal(r)?)))?,
            r.int64()?,
        ))
    })
}

/// The OCaml runtime value of an `Xattr.t`, for hashing.
fn xattrs_value(x: &Xattrs) -> Value {
    Value::option(x.as_ref().map(|(attrs, size)| {
        let attrs = Value::list(attrs.iter().map(|(n, v)| {
            let v = match v {
                AttrValue::String(s) => Value::Block(0, vec![Value::string(s.clone())]),
                AttrValue::Hash(s) => Value::Block(1, vec![Value::string(s.clone())]),
                AttrValue::Loaded(a, b) => Value::Block(
                    2,
                    vec![Value::tuple([
                        Value::string(a.clone()),
                        Value::string(b.clone()),
                    ])],
                ),
            };
            Value::tuple([Value::string(n.clone()), v])
        }));
        Value::tuple([attrs, Value::Int64(*size)])
    }))
}

fn xattrs_hash(x: &Xattrs, h: i64, c: &PropsConfig) -> i64 {
    if c.sync_xattrs {
        ocaml_hash::hash2(ocaml_hash::hash(&xattrs_value(x)), h)
    } else {
        h
    }
}

/// `Xattr.toString`: a summary such as `" 2 xattrs (10 bytes)"`.
pub fn xattrs_to_string(x: &Xattrs) -> String {
    match x {
        None => String::new(),
        Some((l, _)) if l.is_empty() => " 0 xattrs".into(),
        Some((l, z)) if l.len() == 1 => format!(" 1 xattr ({z} bytes)"),
        Some((l, z)) => format!(" {} xattrs ({z} bytes)", l.len()),
    }
}

fn md5(s: &[u8]) -> Vec<u8> {
    use md5::{Digest, Md5};
    Md5::digest(s).to_vec()
}

/// `attrEqual`: same name and value, comparing by hash where needed.
fn attr_equal((n, v): &(Vec<u8>, AttrValue), (n2, v2): &(Vec<u8>, AttrValue)) -> bool {
    use AttrValue::*;
    n == n2
        && match (v, v2) {
            (String(a), String(b) | Loaded(b, _))
            | (Hash(a), Hash(b) | Loaded(_, b))
            | (Loaded(a, _), String(b))
            | (Loaded(_, a), Hash(b) | Loaded(_, b)) => a == b,
            (String(s), Hash(h)) | (Hash(h), String(s)) => *h == md5(s),
        }
}

fn xattrs_similar(x: &Xattrs, x2: &Xattrs, c: &PropsConfig) -> bool {
    !c.sync_xattrs
        || match (x, x2) {
            (None, None) => true,
            (Some((l, z)), Some((l2, z2))) => {
                z == z2
                    && l.len() == l2.len()
                    && l.iter().all(|a| l2.iter().any(|b| attr_equal(b, a)))
            }
            _ => false,
        }
}

// ---- ACLs ----

/// `ACL.t`: the ACL as text, `Some("")` for a trivial ACL, `None` where ACLs
/// are not supported.
pub type Acl = Option<Vec<u8>>;

fn acl_hash(a: &Acl, h: i64, c: &PropsConfig) -> i64 {
    if c.sync_acl {
        let v = Value::option(a.as_ref().map(|s| Value::string(s.clone())));
        ocaml_hash::hash2(ocaml_hash::hash(&v), h)
    } else {
        h
    }
}

fn acl_similar(a: &Acl, a2: &Acl, c: &PropsConfig) -> bool {
    // OCaml also compares the ACLs without user and group names when
    // `numericids` is set; that comes with the ACL feature.
    !c.sync_acl || a == a2
}

// ---- Properties ----

/// `Props.t`: everything Unison records about a file or directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Props {
    /// Permissions.
    pub perm: Perm,
    /// Owner.
    pub uid: Id,
    /// Group.
    pub gid: Id,
    /// Modification time.
    pub time: Time,
    /// macOS Finder information.
    pub type_creator: TypeCreator,
    /// Size of a regular file (0 for others).
    pub length: Filesize,
    /// Change time, only recorded with xattr or ACL synchronisation.
    pub ctime: f64,
    /// Extended attributes.
    pub xattr: Xattrs,
    /// ACL.
    pub acl: Acl,
}

/// `CTime.dummy`.
pub const CTIME_DUMMY: f64 = -1.0;

impl Props {
    /// `Props.template`: dummy values with the given permissions.
    pub fn template(perm: Perm) -> Props {
        Props {
            perm,
            uid: Id::Ignored,
            gid: Id::Ignored,
            time: Time::DUMMY,
            type_creator: None,
            length: FILESIZE_DUMMY,
            ctime: CTIME_DUMMY,
            xattr: None,
            acl: None,
        }
    }

    /// `Props.dummy`.
    pub fn dummy() -> Props {
        Props::template(Perm::DUMMY)
    }

    /// `Props.m`. The change time, xattrs and ACL are only present when
    /// their features are enabled.
    pub fn marshal(&self, w: &mut Writer, c: &PropsConfig) {
        self.perm.marshal(w);
        self.uid.marshal(w);
        self.gid.marshal(w);
        self.time.marshal(w);
        w.option(self.type_creator.as_ref(), |w, s| w.string(s));
        w.int64(self.length);
        if c.xattr_enabled || c.acl_enabled {
            w.float(self.ctime);
        }
        if c.xattr_enabled {
            xattrs_marshal(&self.xattr, w);
        }
        if c.acl_enabled {
            w.option(self.acl.as_ref(), |w, s| w.string(s));
        }
    }

    /// `Props.m`.
    pub fn unmarshal(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<Props> {
        let perm = Perm::unmarshal(r)?;
        let uid = Id::unmarshal(r)?;
        let gid = Id::unmarshal(r)?;
        let time = Time::unmarshal(r)?;
        let type_creator = r.option(Reader::string)?;
        let length = r.int64()?;
        let ctime = if c.xattr_enabled || c.acl_enabled {
            r.float()?
        } else {
            CTIME_DUMMY
        };
        let xattr = if c.xattr_enabled {
            xattrs_unmarshal(r)?
        } else {
            None
        };
        let acl = if c.acl_enabled {
            r.option(Reader::string)?
        } else {
            None
        };
        Ok(Props {
            perm,
            uid,
            gid,
            time,
            type_creator,
            length,
            ctime,
            xattr,
            acl,
        })
    }

    /// `Props.hash`: this part of the `checkArchive` checksum.
    pub fn hash(&self, h: i64, c: &PropsConfig) -> i64 {
        let h = acl_hash(&self.acl, h, c);
        let h = xattrs_hash(&self.xattr, h, c);
        let h = type_creator_hash(&self.type_creator, h);
        let h = self.time.hash(h);
        let h = self.gid.hash(h);
        let h = self.uid.hash(h);
        self.perm.hash(h)
    }

    /// `Props.similar`.
    pub fn similar(&self, other: &Props, c: &PropsConfig) -> bool {
        self.perm.similar(other.perm, c)
            && self.uid.similar(&other.uid, IdKind::User, c)
            && self.gid.similar(&other.gid, IdKind::Group, c)
            && self.time.similar(other.time, c)
            && type_creator_similar(&self.type_creator, &other.type_creator, c)
            && xattrs_similar(&self.xattr, &other.xattr, c)
            && acl_similar(&self.acl, &other.acl, c)
    }

    /// `Props.override`: these props after propagating `other` onto them.
    pub fn override_with(&self, other: &Props, c: &PropsConfig) -> Props {
        Props {
            perm: self.perm.override_with(other.perm, c),
            uid: other.uid.clone(),
            gid: other.gid.clone(),
            time: self.time.override_with(other.time),
            type_creator: other.type_creator.clone(),
            length: other.length,
            // See CTime.override in props.ml: the on-disk ctime changes
            // anyway when the props are set.
            ctime: self.ctime,
            xattr: other.xattr.clone(),
            acl: other.acl.clone(),
        }
    }

    /// `Props.strip`: drops what isn't synchronised.
    pub fn strip(&self, c: &PropsConfig) -> Props {
        Props {
            perm: self.perm.strip(c),
            uid: self.uid.strip(IdKind::User, c),
            gid: self.gid.strip(IdKind::Group, c),
            time: self.time.strip(c),
            type_creator: self.type_creator.clone(),
            length: self.length,
            ctime: self.ctime,
            xattr: if c.sync_xattrs {
                self.xattr.clone()
            } else {
                None
            },
            acl: if c.sync_acl { self.acl.clone() } else { None },
        }
    }

    /// `Props.diff`: what differs from these props in `other`.
    pub fn diff(&self, other: &Props, c: &PropsConfig) -> Props {
        Props {
            perm: self.perm.diff(other.perm),
            uid: self.uid.diff(&other.uid, IdKind::User, c),
            gid: self.gid.diff(&other.gid, IdKind::Group, c),
            time: self.time.diff(other.time, c),
            type_creator: if type_creator_similar(&self.type_creator, &other.type_creator, c) {
                None
            } else {
                other.type_creator.clone()
            },
            length: other.length,
            ctime: other.ctime,
            xattr: if xattrs_similar(&self.xattr, &other.xattr, c) {
                None
            } else {
                other.xattr.clone()
            },
            acl: if acl_similar(&self.acl, &other.acl, c) {
                None
            } else {
                other.acl.clone()
            },
        }
    }

    /// `Props.toString`, e.g. `"modified on 2026-10-19 at 18:38:44  size 2
    /// rw-r--r--"`. `time2string` formats a time (`Util.time2string`,
    /// which uses local time).
    pub fn to_string(&self, c: &PropsConfig, time2string: &dyn Fn(f64) -> String) -> String {
        format!(
            "modified on {}  size {:<9.0} {}{}{}{}{}{}",
            time2string(self.time.extract()),
            self.length as f64,
            self.perm.to_string(c),
            self.uid.to_string(IdKind::User),
            self.gid.to_string(IdKind::Group),
            xattrs_to_string(&self.xattr),
            type_creator_to_string(&self.type_creator),
            acl_to_string(&self.acl, c),
        )
    }

    /// `Props.length`.
    pub fn length(&self) -> Filesize {
        self.length
    }

    /// `Props.same_time`.
    pub fn same_time(&self, other: &Props, c: &PropsConfig) -> bool {
        self.time.same(other.time, c)
    }
}

// ---- On disk ----

/// How props are being set (`` [`Set | `Update] ``): on a new file, or
/// changing an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetKind {
    /// A new file: permissions are always set.
    Set,
    /// An existing file: only what changed (a non-empty mask) is set.
    Update,
}

impl Perm {
    /// `Perm.get`.
    pub fn get(st: &Stats, c: &PropsConfig) -> Perm {
        Perm {
            bits: st.perm,
            mask: c.perm_mask,
        }
    }

    /// `Perm.set`.
    pub fn set(self, abspath: &Fspath, kind: SetKind, dont_chmod: bool) -> Result<(), Error> {
        if (self.mask == 0 && kind != SetKind::Set) || dont_chmod {
            return Ok(());
        }
        fs::chmod(abspath, self.bits).map_err(|e| {
            let msg = e.to_transient("setting permissions").to_string();
            #[cfg(unix)]
            if e.error.raw_os_error() == Some(libc::EOPNOTSUPP) {
                return Error::Transient(format!(
                    "{msg}. You can use preference \"fat\",or else set preference \"perms\" \
                     to 0 and preference \"dontchmod\" to true to avoid this error"
                ));
            }
            Error::Transient(msg)
        })
    }

    /// `Perm.check`: the permissions on disk after setting them.
    pub fn check(self, abspath: &Fspath, st: &Stats, c: &PropsConfig) -> Result<(), Error> {
        let disk = Perm {
            bits: st.perm,
            mask: self.mask,
        };
        if self.bits & self.mask == disk.bits & self.mask {
            return Ok(());
        }
        Err(Error::Transient(format!(
            "Failed to set permissions of file {abspath} to {}: the permissions was set to {} \
             instead. The filesystem probably does not support all permission bits. If this is a \
             FAT filesystem, you should set the \"fat\" option to true. Otherwise, you should \
             probably set the \"perms\" option to 0o{:o} (or to 0 if you don't need to \
             synchronize permissions).",
            self.synced_parts_to_string(),
            disk.synced_parts_to_string(),
            c.perm_mask & !(self.bits ^ st.perm)
        )))
    }
}

impl Id {
    /// `Uid.get` / `Gid.get`: by name, except for id 0 and with
    /// `numericids`.
    pub fn get(st: &Stats, kind: IdKind, c: &PropsConfig) -> Id {
        if !kind.synced(c) {
            return Id::Ignored;
        }
        let id = match kind {
            IdKind::User => st.uid,
            IdKind::Group => st.gid,
        };
        if id == 0 || c.numeric_ids {
            return Id::Numeric(id);
        }
        match system::id_name(id, kind == IdKind::User) {
            Some(n) => Id::Named(n),
            None => Id::Numeric(id),
        }
    }

    /// `extern`: the numeric id to set, or `None` for "leave it".
    fn external(&self, kind: IdKind) -> Result<Option<i64>, Error> {
        match self {
            Id::Ignored => Ok(None),
            Id::Numeric(i) => Ok(Some(*i)),
            Id::Named(n) => {
                let name = String::from_utf8_lossy(n);
                let id = system::id_of_name(n, kind == IdKind::User)
                    .ok_or_else(|| Error::Transient(format!("No {} {name}", kind.name())))?;
                if id == 0 {
                    return Err(Error::Transient(format!(
                        "Trying to map the non-root {} {name} to {} 0",
                        kind.name(),
                        kind.name()
                    )));
                }
                Ok(Some(id))
            }
        }
    }

    /// `Uid.set` / `Gid.set`.
    pub fn set(&self, abspath: &Fspath, kind: IdKind) -> Result<(), Error> {
        let Some(id) = self.external(kind)? else {
            return Ok(());
        };
        let (uid, gid) = match kind {
            IdKind::User => (id, -1),
            IdKind::Group => (-1, id),
        };
        fs::chown(abspath, uid, gid).map_err(|e| e.to_transient("setting file ownership"))
    }
}

impl Time {
    /// `Time.get`: synchronised for regular files when `times` is set.
    pub fn get(st: &Stats, c: &PropsConfig) -> Time {
        if st.kind == Kind::File && c.sync_times {
            Time::Synced(st.mtime)
        } else {
            Time::NotSynced(st.mtime)
        }
    }

    /// `Time.set`: sets a synchronised time as both access and modification
    /// time.
    pub fn set(self, abspath: &Fspath) -> Result<(), Error> {
        match self {
            // utimes with both times 0 would mean "now"; a tiny non-zero
            // atime keeps an mtime of 0 (bcpierce00/unison#223).
            Time::Synced(v) => fs::utimes(abspath, if v == 0.0 { 1e-12 } else { v }, v)
                .map_err(|e| e.to_transient("setting modification time")),
            Time::NotSynced(_) => Ok(()),
        }
    }

    /// `Time.check`.
    pub fn check(self, abspath: &Fspath, st: &Stats, c: &PropsConfig) -> Result<(), Error> {
        let Time::Synced(_) = self else { return Ok(()) };
        let disk = Time::Synced(st.mtime);
        if self.similar(disk, c) {
            return Ok(());
        }
        Err(Error::Transient(format!(
            "Failed to set modification time of file {abspath} to {}: the time was set to {} instead",
            self.synced_parts_to_string(),
            disk.synced_parts_to_string()
        )))
    }

    /// `Time.syncedPartsToString`, with the time as seconds (OCaml also
    /// prints it as a local date).
    pub fn synced_parts_to_string(self) -> String {
        match self {
            Time::Synced(v) => format!("{v:.6}"),
            Time::NotSynced(_) => String::new(),
        }
    }
}

/// `TypeCreator.get`: the Finder information of files and directories,
/// when `rsrc` is on.
pub fn type_creator_get(st: &Stats, info: &osx::Info, c: &PropsConfig) -> TypeCreator {
    (c.rsrc && matches!(st.kind, Kind::File | Kind::Dir)).then(|| info.finfo.clone())
}

impl Props {
    /// `Props.get'`: what `stat` alone gives.
    pub fn get_basic(st: &Stats, c: &PropsConfig) -> Props {
        Props {
            perm: Perm::get(st, c),
            uid: Id::get(st, IdKind::User, c),
            gid: Id::get(st, IdKind::Group, c),
            time: Time::get(st, c),
            type_creator: None,
            length: if st.kind == Kind::File { st.size } else { 0 },
            ctime: CTIME_DUMMY,
            xattr: None,
            acl: None,
        }
    }

    /// `Props.get`: everything, including the change time and the Finder
    /// information. xattrs and ACLs are not read (see the module
    /// documentation).
    pub fn get(st: &Stats, info: &osx::Info, c: &PropsConfig) -> Props {
        Props {
            type_creator: type_creator_get(st, info, c),
            ctime: st.ctime,
            ..Props::get_basic(st, c)
        }
    }

    /// `Props.getWithRess`.
    pub fn get_with_ress(st: &Stats, info: &osx::Info, c: &PropsConfig) -> Props {
        Props {
            type_creator: type_creator_get(st, info, c),
            ..Props::get_basic(st, c)
        }
    }

    /// `Props.set`: owner, group, Finder information, time, then
    /// permissions.
    pub fn set(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
        kind: SetKind,
        c: &PropsConfig,
        dont_chmod: bool,
    ) -> Result<(), Error> {
        let abspath = fspath.concat(path);
        let _ = c;
        self.uid.set(&abspath, IdKind::User)?;
        self.gid.set(&abspath, IdKind::Group)?;
        if let Some(t) = &self.type_creator {
            osx::set_file_infos(&abspath, t)?;
        }
        self.time.set(&abspath)?;
        self.perm.set(&abspath, kind, dont_chmod)
    }

    /// `Props.check`: the props on disk after setting them.
    pub fn check(
        &self,
        fspath: &Fspath,
        path: &Path<Local>,
        st: &Stats,
        c: &PropsConfig,
    ) -> Result<(), Error> {
        let abspath = fspath.concat(path);
        self.time.check(&abspath, st, c)?;
        self.perm.check(&abspath, st, c)
    }

    /// `Props.fileDefault`: what a new file gets when nothing is known.
    pub fn file_default() -> Props {
        Props::template(Perm {
            bits: 0o666 & !umask(),
            mask: -1,
        })
    }

    /// `Props.dirDefault`.
    pub fn dir_default() -> Props {
        Props::template(Perm {
            bits: 0o777 & !umask(),
            mask: -1,
        })
    }

    /// `Props.fileSafe`: like [`Props::file_default`], private.
    pub fn file_safe() -> Props {
        Props::template(Perm {
            bits: 0o600 & !umask(),
            mask: -1,
        })
    }
}

/// The process umask (0o077 on Windows, where OCaml uses rw------- and
/// rwx------ defaults).
#[cfg(unix)]
#[allow(unsafe_code)]
fn umask() -> i64 {
    // SAFETY: umask has no preconditions; it is set back at once. Not
    // thread-safe with respect to other umask callers, as in OCaml.
    let u = unsafe { libc::umask(0) };
    unsafe { libc::umask(u) };
    i64::from(u)
}

#[cfg(not(unix))]
fn umask() -> i64 {
    0o077
}

fn acl_to_string(a: &Acl, c: &PropsConfig) -> String {
    match a {
        Some(s) if s.is_empty() => " <trivial ACL>".into(),
        Some(s) => format!(" A={}", String::from_utf8_lossy(s)),
        None if c.sync_acl => " !No ACL support!".into(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perm(bits: i64) -> Perm {
        Perm { bits, mask: 0o1777 }
    }

    #[test]
    fn perm_similarity_override_and_diff() {
        let c = PropsConfig::default();
        assert!(perm(0o644).similar(perm(0o644 | 0o4000), &c));
        assert!(!perm(0o644).similar(perm(0o600), &c));
        assert!(!Perm::DUMMY.similar(Perm::DUMMY, &c));
        let o = perm(0o4600).override_with(perm(0o644), &c);
        assert_eq!(
            o,
            Perm {
                bits: 0o4644,
                mask: 0o1777
            }
        );
        assert_eq!(
            perm(0o644).diff(perm(0o600)),
            Perm {
                bits: 0o600,
                mask: 0o044
            }
        );
        assert_eq!(perm(0o644).to_string(&c), "rw-r--r--");
        assert_eq!(perm(0o1755).to_string(&c), "trwxr-xr-x");
        assert_eq!(Perm::DUMMY.to_string(&c), "unknown permissions");
        let win = PropsConfig {
            perm_mask: WINDOWS_PERM_MASK,
            ..c
        };
        assert_eq!(perm(0o444).to_string(&win), "read-only");
    }

    #[test]
    fn times_ignore_one_second_and_dst() {
        let c = PropsConfig {
            sync_times: true,
            ..PropsConfig::default()
        };
        assert!(Time::Synced(100.9).similar(Time::Synced(101.2), &c));
        assert!(!Time::Synced(100.0).similar(Time::Synced(102.0), &c));
        assert!(!Time::Synced(100.0).similar(Time::NotSynced(100.0), &c));
        assert!(Time::NotSynced(0.0).same(Time::NotSynced(3600.0), &c));
        assert_eq!(
            Time::Synced(5.0).override_with(Time::NotSynced(7.0)),
            Time::NotSynced(5.0)
        );
        assert_eq!(
            Time::Synced(5.0).strip(&PropsConfig::default()),
            Time::NotSynced(5.0)
        );
    }

    #[test]
    fn ids_and_type_creator() {
        let c = PropsConfig {
            sync_owner: true,
            ..PropsConfig::default()
        };
        let me = Id::Named(b"me".to_vec());
        assert!(me.similar(&me, IdKind::User, &c));
        assert!(!Id::Ignored.similar(&Id::Ignored, IdKind::User, &c));
        assert!(Id::Ignored.similar(&me, IdKind::Group, &c));
        assert_eq!(me.to_string(IdKind::User), " user=me");
        assert_eq!(
            type_creator_to_string(&Some(b"FTEXTttxt".to_vec())),
            " TEXT ttxt"
        );
        assert_eq!(type_creator_to_string(&Some(b"F".to_vec())), "");
    }

    #[test]
    fn props_codec_round_trips_with_and_without_features() {
        let p = Props {
            perm: perm(0o644),
            uid: Id::Numeric(501),
            gid: Id::Named(b"staff".to_vec()),
            time: Time::Synced(1_600_000_010.0),
            type_creator: Some(b"F\0\0\0\0\0\0\0\0".to_vec()),
            length: 38,
            ctime: 1.5,
            xattr: Some((
                vec![(b"user.a".to_vec(), AttrValue::String(b"v".to_vec()))],
                1,
            )),
            acl: Some(Vec::new()),
        };
        for c in [
            PropsConfig::default(),
            PropsConfig {
                xattr_enabled: true,
                acl_enabled: true,
                ..PropsConfig::default()
            },
        ] {
            let bytes = umarshal::to_bytes(|w| p.marshal(w, &c));
            let back = umarshal::from_bytes(&bytes, |r| Props::unmarshal(r, &c)).unwrap();
            let expected = if c.xattr_enabled {
                p.clone()
            } else {
                Props {
                    ctime: CTIME_DUMMY,
                    xattr: None,
                    acl: None,
                    ..p.clone()
                }
            };
            assert_eq!(back, expected);
        }
    }
}
