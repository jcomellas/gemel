//! Test helpers shared by the archive tests: a test-only sketch of
//! `update.ml`'s archive type, codec and `checkArchive` (the real port is
//! phase 4a of milestone 2), and the parsing of archive files.

#![allow(dead_code)]

use std::cmp::Ordering;

use gemel_base::case::Mode;
use gemel_base::name::Name;
use gemel_base::proplist::{Key, Proplist, Registry};
use gemel_compat::ocaml_hash::{self, Value};
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fileinfo::Stamp;
use gemel_fs::os::FullFingerprint;
use gemel_fs::osx::RessStamp;
use gemel_fs::props::{Props, PropsConfig};

#[derive(Debug, PartialEq)]
/// `Update.archive` (`Absent` is OCaml's `NoArchive`).
pub enum Archive {
    Dir(Props, Vec<(Name, Archive)>),
    File(Props, FullFingerprint, Stamp, RessStamp),
    Symlink(Vec<u8>),
    Absent,
}

pub fn read_archive(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<Archive> {
    Ok(match r.tag(4)? {
        0 => {
            let props = Props::unmarshal(r, c)?;
            // MyMap.m: a list of bindings, in ascending order.
            let children = r.list(|r| Ok((Name::unmarshal(r)?, read_archive(r, c)?)))?;
            Archive::Dir(props, children)
        }
        1 => Archive::File(
            Props::unmarshal(r, c)?,
            FullFingerprint::unmarshal(r)?,
            Stamp::unmarshal(r)?,
            RessStamp::unmarshal_stamp(r)?,
        ),
        2 => Archive::Symlink(r.string()?),
        _ => Archive::Absent,
    })
}

pub fn write_archive(a: &Archive, w: &mut Writer, c: &PropsConfig) {
    match a {
        Archive::Dir(props, children) => {
            w.tag(0);
            props.marshal(w, c);
            w.list(children, |w, (n, child)| {
                n.marshal(w);
                write_archive(child, w, c);
            });
        }
        Archive::File(props, fp, stamp, ress) => {
            w.tag(1);
            props.marshal(w, c);
            fp.marshal(w);
            stamp.marshal(w);
            ress.marshal_stamp(w);
        }
        Archive::Symlink(s) => {
            w.tag(2);
            w.string(s);
        }
        Archive::Absent => w.tag(3),
    }
}

/// `Update.checkArchive`, plus the order check it does on load.
pub fn check_archive(a: &Archive, h: i64, mode: Mode, c: &PropsConfig) -> i64 {
    match a {
        Archive::Dir(props, children) => {
            for pair in children.windows(2) {
                assert_eq!(
                    pair[0].0.compare_in(&pair[1].0, mode),
                    Ordering::Less,
                    "{:?} and {:?} are not correctly ordered",
                    pair[0].0,
                    pair[1].0
                );
            }
            // NameMap.fold visits the children in descending order.
            children
                .iter()
                .rev()
                .fold(props.hash(h, c), |h, (n, child)| {
                    ocaml_hash::hash2(n.hash_in(mode), check_archive(child, h, mode, c))
                })
        }
        Archive::File(props, fp, _, _) => {
            let dig = Value::tuple([
                Value::string(fp.data.0.clone()),
                Value::string(fp.ress.0.clone()),
            ]);
            ocaml_hash::hash2(ocaml_hash::hash(&dig), props.hash(h, c))
        }
        Archive::Symlink(content) => ocaml_hash::hash2(ocaml_hash::hash_string(content), h),
        Archive::Absent => 135,
    }
}

pub fn count(a: &Archive) -> (usize, usize, usize) {
    match a {
        Archive::Dir(_, children) => children.iter().fold((1, 0, 0), |(d, f, l), (_, c)| {
            let (d2, f2, l2) = count(c);
            (d + d2, f + f2, l + l2)
        }),
        Archive::File(..) => (0, 1, 0),
        Archive::Symlink(_) => (0, 0, 1),
        Archive::Absent => (0, 0, 0),
    }
}

// The property-list keys update.ml registers ("paths" is stored
// separately, after the payload).
pub type PropsData = Vec<(Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>)>;
pub static PROPS_DATA: Key<PropsData> = Key::new(
    "props data",
    |d, w| {
        w.list(d, |w, (k, l)| {
            w.string(k);
            w.list(l, |w, (a, b)| {
                w.string(a);
                w.string(b);
            });
        })
    },
    |r| r.list(|r| Ok((r.string()?, r.list(|r| Ok((r.string()?, r.string()?)))?))),
);
pub static DIR_STAMP: Key<i64> = Key::new(
    "unchanged directory stamp",
    |n, w| w.int64(*n),
    |r| r.int64(),
);
pub static CASE_MODE: Key<Vec<u8>> = Key::new("case mode", |s, w| w.string(s), |r| r.string());
pub type Predicates = Vec<(Vec<u8>, Vec<Vec<u8>>)>;
pub fn write_predicates(p: &Predicates, w: &mut Writer) {
    w.list(p, |w, (k, l)| {
        w.string(k);
        w.list(l, |w, s| w.string(s));
    });
}
pub fn read_predicates(r: &mut Reader<'_>) -> umarshal::Result<Predicates> {
    r.list(|r| Ok((r.string()?, r.list(|r| r.string())?)))
}
pub static UPDATE_PREDICATES: Key<Predicates> =
    Key::new("update predicates", write_predicates, read_predicates);
pub static PROPS_PREDICATES: Key<Predicates> =
    Key::new("props predicates", write_predicates, read_predicates);
pub static RSRC_PREF: Key<bool> = Key::new("rsrc pref", |b, w| w.bool(*b), |r| r.bool());
pub static XATTRS_PREF: Key<bool> = Key::new("xattrs pref", |b, w| w.bool(*b), |r| r.bool());
pub static ACL_PREF: Key<bool> = Key::new("acl pref", |b, w| w.bool(*b), |r| r.bool());

pub fn registry() -> Registry {
    let mut reg = Registry::new();
    reg.register(&PROPS_DATA).unwrap();
    reg.register(&DIR_STAMP).unwrap();
    reg.register(&CASE_MODE).unwrap();
    reg.register(&UPDATE_PREDICATES).unwrap();
    reg.register(&PROPS_PREDICATES).unwrap();
    reg.register(&RSRC_PREF).unwrap();
    reg.register(&XATTRS_PREF).unwrap();
    reg.register(&ACL_PREF).unwrap();
    reg
}

/// The mode an archive's magic records (`Update.archiveMode`).
pub fn mode_of(magic: &[u8]) -> Mode {
    let desc = &magic[..magic.iter().position(|&b| b == 0).expect("NUL in magic")];
    [
        Mode::Sensitive,
        Mode::Insensitive,
        Mode::UnicodeSensitive,
        Mode::UnicodeInsensitive,
    ]
    .into_iter()
    .find(|m| m.desc().as_bytes() == desc)
    .unwrap_or_else(|| panic!("unknown case mode {:?}", String::from_utf8_lossy(desc)))
}

/// An archive file, parsed.
pub struct ArchiveFile {
    /// The props codec settings, from the features in the header.
    pub config: PropsConfig,
    /// The archive tree.
    pub archive: Archive,
    /// The `checkArchive` hash stored with it.
    pub stored_hash: i64,
    /// The magic (case mode and more).
    pub magic: Vec<u8>,
    /// The property list.
    pub props: Proplist,
    /// The payload bytes.
    pub payload: Vec<u8>,
    /// The second header line ("Archive for root ...").
    pub roots_line: Vec<u8>,
}

/// Parses an archive file: three header lines, the payload frame, and the
/// optional separately stored "paths" frame.
pub fn parse_archive_file(bytes: &[u8]) -> ArchiveFile {
    let mut at = 0;
    let mut lines = Vec::new();
    for _ in 0..3 {
        let n = bytes[at..].iter().position(|&b| b == b'\n').unwrap();
        lines.push(&bytes[at..at + n]);
        at += n + 1;
    }
    assert_eq!(lines[0], b"Unison archive format 23");
    assert!(lines[1].starts_with(b"Archive for root "));
    // The features the archive format depends on follow \x1e separators.
    // OCaml Unison 2.54 enables xattr and ACL support whenever both sides
    // have it, even with the `xattrs` and `acl` preferences off.
    let features: Vec<&[u8]> = lines[2]
        .split(|&b| b == 0x1e)
        .skip(1)
        .filter(|f| !f.is_empty())
        .collect();
    for f in &features {
        assert!(
            [&b"Sync: xattr"[..], b"Sync: ACL"].contains(f),
            "unexpected feature {:?}",
            String::from_utf8_lossy(f)
        );
    }
    let config = PropsConfig {
        xattr_enabled: features.contains(&&b"Sync: xattr"[..]),
        acl_enabled: features.contains(&&b"Sync: ACL"[..]),
        ..PropsConfig::default()
    };
    let frame = &bytes[at..];
    let size = umarshal::data_size(frame).unwrap();
    let payload = &frame[8..8 + size];
    let mut r = Reader::new(payload);
    let archive = read_archive(&mut r, &config).unwrap();
    let stored_hash = r.int().unwrap();
    let magic = r.string().unwrap();
    let props = Proplist::unmarshal(&mut r, &registry()).unwrap();
    assert!(r.remaining().is_empty(), "unread payload");
    let rest = &frame[8 + size..];
    if !rest.is_empty() {
        let size = umarshal::data_size(rest).unwrap();
        assert_eq!(rest.len(), 8 + size, "trailing bytes after paths");
    }
    ArchiveFile {
        config,
        archive,
        stored_hash,
        magic,
        props,
        payload: payload.to_vec(),
        roots_line: lines[1].to_vec(),
    }
}
