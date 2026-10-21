//! Archives: what each replica looked like when it was last synchronised.
//!
//! Ports the archive half of Unison `src/update.ml`: the archive type and
//! its codec, archive names, `checkArchive`, loading and storing archive
//! files, the in-memory archive caches, case conversion, archive locks and
//! crash recovery. Update detection and commits build on it.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! # Hosts
//!
//! The archives of a root belong to the host that owns the root, as in
//! OCaml, and are reached through root commands. Each host keeps them in a
//! [`Host`] slot, keyed by the root's global name ([`this_roots_global_name`]).
//! The client-side functions ([`load_archives`], [`lock_archives`],
//! [`do_archive_crash_recovery`], ...) run those commands on every root.
//!
//! # Differences from OCaml
//!
//! - The `"paths"` property (per-path properties) is kept beside the
//!   property list, in [`ArchiveProps::paths`], rather than in it. OCaml
//!   stores it in a frame of its own after the payload anyway, so that
//!   Unison ≤ 2.53.3 can still read the archive, and its values are
//!   property lists themselves.
//! - The `"props data"` property (data shared between xattrs and ACLs) is
//!   kept as loaded. gemel doesn't synchronise xattrs or ACLs yet, so it has
//!   no data of its own to add.
//! - An archive is decoded with the features its file records, without
//!   enabling them on the host meanwhile.
//! - A truncated archive is reported as damaged, where OCaml stops with an
//!   uncaught `End_of_file`.
//! - The `dumparchives` preference and the `dumpArchive` command (a text
//!   dump through OCaml's `Format`) are not ported yet.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::LazyLock;

use gemel_base::case::{self, Mode};
use gemel_base::features::{self, Id};
use gemel_base::name::Name;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, Internal, PrefDef, Prefs, Topic};
use gemel_base::proplist::{self, Key, Proplist, Registry};
use gemel_base::util::{self, Error};
use gemel_compat::ocaml_hash::{self, Value};
use gemel_compat::umarshal::{self, HEADER_SIZE, Reader, Writer};
use gemel_fs::fileinfo::{self, Stamp};
use gemel_fs::fspath::Fspath;
use gemel_fs::os::FullFingerprint;
use gemel_fs::osx::RessStamp;
use gemel_fs::props::{self, FEATURE_ACL, FEATURE_XATTR, Props, PropsConfig};
use gemel_fs::system::{self, Kind, Stats, SysError};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc};
use gemel_rpc::root::{Host as RootHost, Root};
use md5::{Digest, Md5};

use crate::fpcache::FpCache;
use crate::globals::Roots;
use crate::lock;
use crate::name_map::{NameMap, PathMap, Validation};

/// `Uutil.myName`.
pub(crate) const MY_NAME: &str = "unison";

// ---- Preferences ----

/// `ignorearchives`.
pub static IGNORE_ARCHIVES: PrefDef<bool> = PrefDef::new(
    "ignorearchives",
    Category::Advanced(Topic::Archive),
    || false,
    "ignore existing archive files",
    "When this preference is set, Unison will ignore any existing \
     archive files and behave as though it were being run for the first \
     time on these replicas.  It is \
     not a good idea to set this option in a profile: it is intended for \
     command-line use.",
);

/// `rootsName`: the canonical names of the roots, which archive names are
/// derived from. The client sets it ([`store_roots_name`]) and sends it to
/// the server with the other preferences.
pub static ROOTS_NAME: PrefDef<String> = PrefDef::new(
    "rootsName",
    Category::Internal(Internal::Pseudo),
    String::new,
    "*Canonical root names",
    "",
);

/// `rootalias`: `old -> new` rules applied to root names before archive
/// names are computed.
pub static ROOT_ALIASES: PrefDef<Vec<String>> = PrefDef::new(
    "rootalias",
    Category::Advanced(Topic::General),
    Vec::new,
    "register alias for canonical root names",
    "When calculating the name of the archive files for a given pair of roots, \
     Unison replaces any roots matching the left-hand side of any rootalias \
     rule by the corresponding right-hand side.",
);

/// `showarchive`.
pub static SHOW_ARCHIVE_NAME: PrefDef<bool> = PrefDef::new(
    "showarchive",
    Category::Advanced(Topic::General),
    || false,
    "show 'true names' (for rootalias) of roots and archive",
    "When this preference is set, Unison will print out the 'true names'\
     of the roots, in the same form as is expected by the {\\tt rootalias} \
     preference.",
);

/// `ignorelocks`.
pub static IGNORE_LOCKS: PrefDef<bool> = PrefDef::new(
    "ignorelocks",
    Category::Advanced(Topic::General),
    || false,
    "ignore locks left over from previous run (dangerous!)",
    "When this preference is set, Unison will ignore any lock files \
     that may have been left over from a previous run of Unison that \
     was interrupted while reading or writing archive files; by default, \
     when Unison sees these lock files it will stop and request manual \
     intervention.  This \
     option should be set only if you are {\\em positive} that no other \
     instance of Unison might be concurrently accessing the same archive \
     files (e.g., because there was only one instance of unison running \
     and it has just crashed or you have just killed it).  It is probably \
     not a good idea to set this option in a profile: it is intended for \
     command-line use.",
);

/// `dumparchives`. Accepted, but the dump (`dumpArchiveOnRoot`) isn't
/// ported.
pub static DUMP_ARCHIVES: PrefDef<bool> = PrefDef::new(
    "dumparchives",
    Category::Expert,
    || false,
    "dump contents of archives just after loading",
    "When this preference is set, Unison will create a file unison.dump on \
     each host, containing a text summary of the archive, immediately after \
     loading it.",
)
.cli_only();

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&IGNORE_ARCHIVES)?;
    prefs.register(&ROOTS_NAME)?;
    prefs.register(&ROOT_ALIASES)?;
    prefs.register(&SHOW_ARCHIVE_NAME)?;
    prefs.alias(&SHOW_ARCHIVE_NAME, "showArchiveName")?;
    prefs.register(&DUMP_ARCHIVES)?;
    prefs.register(&IGNORE_LOCKS)
}

// ---- The archive ----

/// `Update.archiveFormat`: the archive format version. It is in archive
/// headers and archive names, so changing it discards every archive.
pub const ARCHIVE_FORMAT: u32 = 23;

/// `Update.archive`: the state of a path when it was last synchronised.
/// For directories, only the permissions in the props are synchronised.
#[derive(Debug, Clone, PartialEq)]
pub enum Archive {
    /// A directory and its children.
    Dir(Props, NameMap<Archive>),
    /// A file: its props, fingerprints, stamp and resource-fork stamp.
    File(Props, FullFingerprint, Stamp, RessStamp),
    /// A symbolic link and its target.
    Symlink(Vec<u8>),
    /// Nothing (only at the root of an archive).
    NoArchive,
}

impl Archive {
    /// `marchive`.
    pub fn marshal(&self, w: &mut Writer, c: &PropsConfig) {
        match self {
            Archive::Dir(props, children) => {
                w.tag(0);
                props.marshal(w, c);
                children.marshal(w, |a, w| a.marshal(w, c));
            }
            Archive::File(props, fp, stamp, ress) => {
                w.tag(1);
                props.marshal(w, c);
                fp.marshal(w);
                stamp.marshal(w);
                ress.marshal_stamp(w);
            }
            Archive::Symlink(target) => {
                w.tag(2);
                w.string(target);
            }
            Archive::NoArchive => w.tag(3),
        }
    }

    /// `marchive`. Directory children are sorted under `mode` as they are
    /// read (see [`NameMap::unmarshal`]).
    pub fn unmarshal(r: &mut Reader<'_>, c: &PropsConfig, mode: Mode) -> umarshal::Result<Archive> {
        Ok(match r.tag(4)? {
            0 => {
                let props = Props::unmarshal(r, c)?;
                Archive::Dir(
                    props,
                    NameMap::unmarshal(r, mode, |r| Archive::unmarshal(r, c, mode))?,
                )
            }
            1 => Archive::File(
                Props::unmarshal(r, c)?,
                FullFingerprint::unmarshal(r)?,
                Stamp::unmarshal(r)?,
                RessStamp::unmarshal_stamp(r)?,
            ),
            2 => Archive::Symlink(r.string()?),
            _ => Archive::NoArchive,
        })
    }

    /// The archive with every directory's children ordered by `mode`
    /// (for an archive decoded under another mode).
    pub fn with_mode(self, mode: Mode) -> Archive {
        match self {
            Archive::Dir(desc, mut children) if children.mode() == mode => {
                for (_, a) in children.iter_mut() {
                    *a = std::mem::replace(a, Archive::NoArchive).with_mode(mode);
                }
                Archive::Dir(desc, children)
            }
            Archive::Dir(desc, children) => {
                let bindings: Vec<_> = children
                    .into_bindings()
                    .into_iter()
                    .map(|(n, a)| (n, a.with_mode(mode)))
                    .collect();
                Archive::Dir(desc, NameMap::from_bindings(mode, bindings))
            }
            a => a,
        }
    }

    /// `archive2string`.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Archive::Dir(..) => "ArchiveDir",
            Archive::File(..) => "ArchiveFile",
            Archive::Symlink(_) => "ArchiveSymlink",
            Archive::NoArchive => "NoArchive",
        }
    }
}

// ---- Archive names ----
//
// The canonical name of a root is its canonical host name and path, and
// that of a set of roots is theirs in sorted order. Each root has an
// archive, named by the hash of the root's name, the roots' names and the
// archive format.

/// `root2stringOrAlias`: the root's name (`//host/path`), after the
/// `rootalias` rules.
pub fn root_to_string_or_alias(prefs: &Prefs, root: &Root) -> Result<String, Error> {
    let r = root.to_string();
    let mut aliases = Vec::new();
    for s in prefs.read(&ROOT_ALIASES) {
        match util::split_into_words_by_string(s, " -> ")[..] {
            [n, n2] => aliases.push((util::trim_whitespace(n), util::trim_whitespace(n2))),
            _ => {
                return Err(Error::Fatal(format!(
                    "rootalias {s} should be two strings separated by ' -> '"
                )));
            }
        }
    }
    Ok(aliases
        .iter()
        .find(|(n, _)| *n == r)
        .map_or(r, |(_, n2)| (*n2).to_owned()))
}

/// `storeRootsName`: sets `rootsName` on the client, from the roots' names
/// (a local root named after this host), aliased and sorted. It must be
/// set before the preferences are sent to the hosts.
pub fn store_roots_name(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    let client = rpc.client_host();
    let host_name = client.canonical_host_name();
    let mut names = {
        let prefs = client.prefs().borrow();
        roots
            .in_canonical_order()
            .into_iter()
            .map(|r| {
                let r = match r.host {
                    RootHost::Local => Root {
                        host: RootHost::Remote(host_name.clone()),
                        fspath: r.fspath,
                    },
                    RootHost::Remote(_) => r,
                };
                root_to_string_or_alias(&prefs, &r)
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    names.sort();
    client
        .prefs()
        .borrow_mut()
        .set(&ROOTS_NAME, names.join(", "));
    Ok(())
}

/// `thisRootsGlobalName`: the name of the root at `fspath` on `host`.
pub fn this_roots_global_name(host: &Host, fspath: &Fspath) -> Result<String, Error> {
    let root = Root {
        host: RootHost::Remote(host.canonical_host_name()),
        fspath: fspath.clone(),
    };
    root_to_string_or_alias(&host.prefs().borrow(), &root)
}

/// `Update.archiveVersion`: which of a root's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveVersion {
    /// The archive (`ar`).
    MainArch,
    /// A committed archive not yet copied to the main one (`tm`).
    NewArch,
    /// An archive being written (`sc`).
    ScratchArch,
    /// The lock (`lk`).
    Lock,
    /// The fingerprint cache (`fp`).
    FpCache,
}

impl ArchiveVersion {
    fn prefix(self) -> &'static str {
        match self {
            ArchiveVersion::MainArch => "ar",
            ArchiveVersion::NewArch => "tm",
            ArchiveVersion::ScratchArch => "sc",
            ArchiveVersion::Lock => "lk",
            ArchiveVersion::FpCache => "fp",
        }
    }

    /// `marchiveVersion`.
    pub fn marshal(self, w: &mut Writer) {
        w.tag(match self {
            ArchiveVersion::MainArch => 0,
            ArchiveVersion::NewArch => 1,
            ArchiveVersion::ScratchArch => 2,
            ArchiveVersion::Lock => 3,
            ArchiveVersion::FpCache => 4,
        });
    }

    /// `marchiveVersion`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Self> {
        Ok(match r.tag(5)? {
            0 => ArchiveVersion::MainArch,
            1 => ArchiveVersion::NewArch,
            2 => ArchiveVersion::ScratchArch,
            3 => ArchiveVersion::Lock,
            _ => ArchiveVersion::FpCache,
        })
    }
}

/// `archiveHash`: the hash in the names of the root's files, of
/// `"<this root>;<rootsName>;23"`.
pub fn archive_hash(host: &Host, fspath: &Fspath) -> Result<String, Error> {
    let this_root = this_roots_global_name(host, fspath)?;
    let prefs = host.prefs().borrow();
    let n = format!("{this_root};{};{ARCHIVE_FORMAT}", prefs.read(&ROOTS_NAME));
    let d: String = Md5::digest(n.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if *prefs.read(&SHOW_ARCHIVE_NAME) {
        eprintln!("Archive name is {n}; hashcode is {d}");
    }
    Ok(d)
}

/// `archiveName`: the name of one of the root's files, and the root's
/// global name.
pub fn archive_name(
    host: &Host,
    fspath: &Fspath,
    v: ArchiveVersion,
) -> Result<(String, String), Error> {
    let n = archive_hash(host, fspath)?;
    Ok((
        format!("{}{n}", v.prefix()),
        this_roots_global_name(host, fspath)?,
    ))
}

/// `archiveNameOnRoot`: the name of one of the root's files, the host's
/// name, and whether the file exists.
pub static ARCHIVE_NAME: RootCmd<ArchiveVersion, (String, String, bool)> = RootCmd::new(
    "archiveName",
    |v, w, _| v.marshal(w),
    |r, _| ArchiveVersion::unmarshal(r),
    |(name, host, exists), w, _| {
        w.string(name.as_bytes());
        w.string(host.as_bytes());
        w.bool(*exists);
    },
    |r, _| Ok((read_text(r)?, read_text(r)?, r.bool()?)),
    |host, fspath, v| {
        Box::pin(async move {
            let (name, _) = archive_name(&host, &fspath, v)?;
            let exists = system::file_exists(&host.file_in_unison_dir(&name));
            Ok((name, host.canonical_host_name(), exists))
        })
    },
);

fn read_text(r: &mut Reader<'_>) -> umarshal::Result<String> {
    Ok(String::from_utf8_lossy(&r.string()?).into_owned())
}

// ---- Sanity checks ----

/// `checkArchive`: checks that no directory has a name twice or names out
/// of order (under `mode`), and returns the archive's hash. Archives with
/// the same tree have the same hash, which is how both hosts check that
/// they stored the same thing.
pub fn check_archive(archive: &Archive, mode: Mode, c: &PropsConfig) -> Result<i64, Error> {
    check(&mut Vec::new(), archive, 0, mode, c)
}

fn check(
    path: &mut Vec<Name>,
    archive: &Archive,
    h: i64,
    mode: Mode,
    c: &PropsConfig,
) -> Result<i64, Error> {
    match archive {
        Archive::Dir(desc, children) => {
            let at = |path: &[Name]| {
                let p = path.iter().fold(Path::<Local>::empty(), |p, n| p.child(n));
                String::from_utf8_lossy(p.as_bytes()).into_owned()
            };
            let name = |n: &Name| String::from_utf8_lossy(n.as_bytes()).into_owned();
            match children.validate(mode) {
                Validation::Ok => {}
                Validation::Duplicate(n) => {
                    return Err(Error::Fatal(format!(
                        "Corrupted archive: the file {} occurs twice in path {}",
                        name(n),
                        at(path)
                    )));
                }
                Validation::Invalid(n, n2) => {
                    return Err(Error::Fatal(format!(
                        "Corrupted archive: the files {} and {} are not correctly \
                         ordered in directory {}",
                        name(n),
                        name(n2),
                        at(path)
                    )));
                }
            }
            // NameMap.fold: the children in descending order.
            let mut h = desc.hash(h, c);
            for (n, a) in children.iter_desc() {
                path.push(n.clone());
                let ha = check(path, a, h, mode, c);
                path.pop();
                h = ocaml_hash::hash2(n.hash_in(mode), ha?);
            }
            Ok(h)
        }
        Archive::File(desc, dig, _, _) => {
            let dig = Value::tuple([
                Value::string(dig.data.0.clone()),
                Value::string(dig.ress.0.clone()),
            ]);
            Ok(ocaml_hash::hash2(ocaml_hash::hash(&dig), desc.hash(h, c)))
        }
        Archive::Symlink(content) => Ok(ocaml_hash::hash2(ocaml_hash::hash_string(content), h)),
        Archive::NoArchive => Ok(135),
    }
}

/// `archivesIdentical`: whether every element is the same, and none is
/// `None`.
pub fn archives_identical<T: PartialEq>(l: &[Option<T>]) -> bool {
    match l {
        [h, rest @ ..] => h.is_some() && rest.iter().all(|x| x == h),
        [] => true,
    }
}

// ---- Archive properties ----

/// Data shared between props (`Props.Data`): for `"xattr"` and `"ACL"`,
/// the values by key.
pub type PropsData = Vec<(Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>)>;

/// The predicates update detection depended on (`updatePredicates`,
/// `propsPredicates`): each predicate's name and patterns.
pub type Predicates = Vec<(Vec<u8>, Vec<Vec<u8>>)>;

fn write_predicates(p: &Predicates, w: &mut Writer) {
    w.list(p, |w, (k, l)| {
        w.string(k);
        w.list(l, |w, s| w.string(s));
    });
}

fn read_predicates(r: &mut Reader<'_>) -> umarshal::Result<Predicates> {
    r.list(|r| Ok((r.string()?, r.list(Reader::string)?)))
}

fn write_bool(b: &bool, w: &mut Writer) {
    w.bool(*b);
}

fn read_bool(r: &mut Reader<'_>) -> umarshal::Result<bool> {
    r.bool()
}

/// `propsDataKey`: the data shared by the archive's props.
pub static PROPS_DATA_KEY: Key<PropsData> = Key::new(
    "props data",
    |d, w| {
        w.list(d, |w, (k, l)| {
            w.string(k);
            w.list(l, |w, (a, b)| {
                w.string(a);
                w.string(b);
            });
        });
    },
    |r| r.list(|r| Ok((r.string()?, r.list(|r| Ok((r.string()?, r.string()?)))?))),
);

/// `dirStampKey`: the stamp marking directories unchanged since the last
/// scan.
pub static DIR_STAMP_KEY: Key<props::DirChangedStamp> = Key::new(
    "unchanged directory stamp",
    |n, w| w.int64(*n),
    |r| r.int64(),
);

/// `caseKey`: the description of the case mode the archive was made in.
pub static CASE_KEY: Key<Vec<u8>> = Key::new("case mode", |s, w| w.string(s), |r| r.string());

/// `predKey`: the update predicates the archive was scanned with.
pub static UPDATE_PREDICATES_KEY: Key<Predicates> =
    Key::new("update predicates", write_predicates, read_predicates);

/// `rsrcKey`: the `rsrc` preference the archive was scanned with.
pub static RSRC_KEY: Key<bool> = Key::new("rsrc pref", write_bool, read_bool);

/// `pred2Key`: the props predicates the archive was scanned with.
pub static PROPS_PREDICATES_KEY: Key<Predicates> =
    Key::new("props predicates", write_predicates, read_predicates);

/// `xattrsKey`: the `xattrs` preference the archive was scanned with.
pub static XATTRS_KEY: Key<bool> = Key::new("xattrs pref", write_bool, read_bool);

/// `aclKey`: the `acl` preference the archive was scanned with.
pub static ACL_KEY: Key<bool> = Key::new("acl pref", write_bool, read_bool);

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let mut reg = Registry::new();
    let registered = (|| {
        reg.register(&PROPS_DATA_KEY)?;
        reg.register(&DIR_STAMP_KEY)?;
        reg.register(&CASE_KEY)?;
        reg.register(&UPDATE_PREDICATES_KEY)?;
        reg.register(&RSRC_KEY)?;
        reg.register(&PROPS_PREDICATES_KEY)?;
        reg.register(&XATTRS_KEY)?;
        reg.register(&ACL_KEY)
    })();
    registered.expect("archive property names are unique");
    reg
});

/// The property names an archive's lists may contain (all but `"paths"`,
/// which is stored on its own).
pub fn registry() -> &'static Registry {
    &REGISTRY
}

/// An archive's properties (`getArchiveProps`).
#[derive(Debug, Clone)]
pub struct ArchiveProps {
    /// The property list.
    pub props: Proplist,
    /// Properties by path (OCaml's `"paths"` property).
    pub paths: PathMap<Proplist>,
}

impl ArchiveProps {
    /// No properties.
    pub fn empty(mode: Mode) -> ArchiveProps {
        ArchiveProps {
            props: Proplist::new(),
            paths: PathMap::new(mode),
        }
    }
}

// ---- Archive files ----

/// `formatString`: the first line of an archive file.
pub const FORMAT_STRING: &str = "Unison archive format 23";

/// `verboseArchiveName`: the second line of an archive file.
pub fn verbose_archive_name(this_root: &str, roots_name: &str) -> String {
    format!("Archive for root {this_root} synchronizing roots {roots_name}")
}

/// The contents of an archive file.
#[derive(Debug, Clone)]
pub struct ArchiveData {
    /// The archive.
    pub archive: Archive,
    /// Its [`check_archive`] hash when it was stored.
    pub hash: i64,
    /// The case mode description, a NUL, and the time and process that
    /// stored it (`"<mode>\0<time>.<pid>"`), shared by both roots' archives.
    pub magic: Vec<u8>,
    /// The properties.
    pub props: ArchiveProps,
}

/// The contents of an archive file, borrowed.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveRef<'a> {
    /// The archive.
    pub archive: &'a Archive,
    /// Its [`check_archive`] hash.
    pub hash: i64,
    /// The magic (see [`ArchiveData::magic`]).
    pub magic: &'a [u8],
    /// The properties.
    pub props: &'a ArchiveProps,
}

impl ArchiveData {
    /// The contents, borrowed.
    pub fn as_ref(&self) -> ArchiveRef<'_> {
        ArchiveRef {
            archive: &self.archive,
            hash: self.hash,
            magic: &self.magic,
            props: &self.props,
        }
    }
}

/// `input_line`: the bytes up to the next newline (or the end), and
/// `None` at the end.
fn input_line<'a>(bytes: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    let rest = bytes.get(*at..).filter(|r| !r.is_empty())?;
    let n = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    *at += (n + 1).min(rest.len());
    Some(&rest[..n])
}

fn damaged(why: &str) -> Error {
    Error::Fatal(format!(
        "Archive file seems damaged ({why}): use the -ignorearchives option, \
         or throw away archives on both machines and try again"
    ))
}

fn read_error(e: proplist::ReadError) -> Error {
    match e {
        proplist::ReadError::Umarshal(e) => damaged(&e.to_string()),
        proplist::ReadError::Fatal(e) => e,
    }
}

/// The parsing half of `loadArchiveLocal`: checks the header of the
/// archive file `file` (whose contents are `bytes`) and decodes it.
///
/// The format line and the roots line (`expected_roots`, from
/// [`verbose_archive_name`]) must match, or `warn` is told why the archive
/// will be discarded and the result is `None`. The archive must not need
/// features missing from `available` (`Features.all`). Directory children
/// are sorted under `mode`.
pub fn parse_archive_file(
    bytes: &[u8],
    file: &str,
    expected_roots: &str,
    available: &[Id],
    mode: Mode,
    warn: &dyn Fn(&str),
) -> Result<Option<ArchiveData>, Error> {
    let mut at = 0;
    let eof = || damaged(&umarshal::Error::EndOfInput.to_string());
    let header = input_line(bytes, &mut at).ok_or_else(eof)?;
    if header != FORMAT_STRING.as_bytes() {
        warn(&format!(
            "Archive format mismatch: found\n '{}'\nbut expected\n '{FORMAT_STRING}'.\n\
             I will delete the old archive and start from scratch.\n",
            String::from_utf8_lossy(header)
        ));
        return Ok(None);
    }
    let roots = input_line(bytes, &mut at).ok_or_else(eof)?;
    if roots != expected_roots.as_bytes() {
        warn(&format!(
            "Archive mismatch: found\n '{}'\nbut expected\n '{expected_roots}'.\n\
             I will delete the old archive and start from scratch.\n",
            String::from_utf8_lossy(roots)
        ));
        return Ok(None);
    }
    // The first part of the third line is informative; the features the
    // archive needs follow, after \x1e separators.
    let stored: Vec<Id> = input_line(bytes, &mut at)
        .ok_or_else(eof)?
        .split(|&b| b == 0x1e)
        .skip(1)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    let common = features::inter(&stored, available);
    if stored.len() != common.len() {
        let missing: Vec<&str> = stored
            .iter()
            .filter(|f| !common.contains(f))
            .map(String::as_str)
            .collect();
        return Err(Error::Fatal(format!(
            "Archive format mismatch: the archive was stored with features that \
             are currently not available.\nMissing features: {}\nArchive file: {file}\n\
             You should either upgrade Unison or invoke Unison once with \
             -ignorearchives flag and then try again.",
            missing.join(", ")
        )));
    }
    let c = PropsConfig {
        xattr_enabled: common.iter().any(|f| f == FEATURE_XATTR),
        acl_enabled: common.iter().any(|f| f == FEATURE_ACL),
        ..PropsConfig::default()
    };
    // Umarshal.from_channel skips the header and reads the value from what
    // follows, whatever size the header says.
    let frame = bytes.get(at + HEADER_SIZE..).ok_or_else(eof)?;
    let mut r = Reader::new(frame);
    let um = |e: umarshal::Error| damaged(&e.to_string());
    let archive = Archive::unmarshal(&mut r, &c, mode).map_err(um)?;
    let hash = r.int().map_err(um)?;
    let magic = r.string().map_err(um)?;
    let props = Proplist::read(&mut r, registry()).map_err(read_error)?;
    // "paths" follows in a frame of its own, if there are any; a truncated
    // frame is as good as none.
    let rest = &frame[r.position()..];
    let paths = match rest.get(HEADER_SIZE..) {
        None => PathMap::new(mode),
        Some(frame) => {
            let mut r = Reader::new(frame);
            match PathMap::unmarshal(&mut r, mode, |r| Proplist::read(r, registry())) {
                Ok(paths) => paths,
                Err(proplist::ReadError::Umarshal(umarshal::Error::EndOfInput)) => {
                    PathMap::new(mode)
                }
                Err(e) => return Err(read_error(e)),
            }
        }
    };
    Ok(Some(ArchiveData {
        archive,
        hash,
        magic,
        props: ArchiveProps { props, paths },
    }))
}

/// The writing half of `storeArchiveLocal`: the contents of an archive
/// file. `roots_line` comes from [`verbose_archive_name`], `written` is the
/// informative start of the third line (`"Written at <time> - <mode>
/// mode"`), and `features` are those the archive format depends on
/// (`Features.changingArchiveFormat`).
pub fn archive_file_bytes(
    data: &ArchiveData,
    roots_line: &str,
    written: &str,
    features: &[Id],
    c: &PropsConfig,
) -> Vec<u8> {
    file_bytes(data.as_ref(), roots_line, written, features, c)
}

fn file_bytes(
    data: ArchiveRef<'_>,
    roots_line: &str,
    written: &str,
    features: &[Id],
    c: &PropsConfig,
) -> Vec<u8> {
    let mut out = Vec::new();
    for line in [FORMAT_STRING, roots_line] {
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(written.as_bytes());
    out.push(0x1e);
    out.extend_from_slice(features.join("\x1e").as_bytes());
    out.push(b'\n');
    out.extend(umarshal::to_bytes(|w| {
        data.archive.marshal(w, c);
        w.int(data.hash);
        w.string(data.magic);
        data.props.props.marshal(w);
    }));
    // A frame of its own, so that Unison ≤ 2.53.3 (which would fail with
    // "paths not yet registered!") can still read the archive.
    if !data.props.paths.is_empty() {
        out.extend(umarshal::to_bytes(|w| {
            data.props.paths.marshal(w, Proplist::marshal);
        }));
    }
    out
}

fn path_string(f: &[u8]) -> String {
    String::from_utf8_lossy(f).into_owned()
}

/// `loadArchiveLocal`: loads the archive file `file` of the root named
/// `this_root`, if it exists and its header matches. Mismatches are
/// warnings, and the archive is then discarded.
pub fn load_archive_local(
    host: &Host,
    file: &[u8],
    this_root: &str,
) -> Result<Option<ArchiveData>, Error> {
    let bytes = match std::fs::read(system::os_path(file)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SysError {
                call: "open",
                param: path_string(file),
                error,
            }
            .to_fatal("loading archive"));
        }
    };
    let expected = verbose_archive_name(this_root, host.prefs().borrow().read(&ROOTS_NAME));
    let available = host.features().borrow().all();
    let mode = case::mode(&host.prefs().borrow());
    parse_archive_file(
        &bytes,
        &path_string(file),
        &expected,
        &available,
        mode,
        &|s| host.warn(s),
    )
}

/// `storeArchiveLocal`: writes the archive file `file` of the root named
/// `this_root`, encoded with the host's features.
pub fn store_archive_local(
    host: &Host,
    file: &[u8],
    this_root: &str,
    data: &ArchiveData,
) -> Result<(), Error> {
    store_archive_ref(host, file, this_root, data.as_ref())
}

/// [`store_archive_local`], from borrowed parts.
pub fn store_archive_ref(
    host: &Host,
    file: &[u8],
    this_root: &str,
    data: ArchiveRef<'_>,
) -> Result<(), Error> {
    let (roots_line, mode) = {
        let prefs = host.prefs().borrow();
        (
            verbose_archive_name(this_root, prefs.read(&ROOTS_NAME)),
            case::mode(&prefs),
        )
    };
    let (c, features) = {
        let f = host.features().borrow();
        (
            PropsConfig::default().with_features(&f),
            f.changing_archive_format(),
        )
    };
    let written = format!(
        "Written at {} - {} mode",
        system::time2string(system::gettimeofday()),
        mode.desc()
    );
    let bytes = file_bytes(data, &roots_line, &written, &features, &c);
    write_file(file, &bytes).map_err(|e| e.to_fatal("saving archive"))
}

/// Writes `bytes` to `file`, created with permissions 0o600 if need be
/// (`open_out_gen [Open_wronly; Open_creat; Open_trunc; Open_binary]
/// 0o600`).
fn write_file(file: &[u8], bytes: &[u8]) -> Result<(), SysError> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let sys = |call, error| SysError {
        call,
        param: path_string(file),
        error,
    };
    let mut f = o.open(system::os_path(file)).map_err(|e| sys("open", e))?;
    f.write_all(bytes).map_err(|e| sys("write", e))
}

/// `removeArchiveLocal`: removes one of the root's files, if it exists.
pub fn remove_archive_local(host: &Host, fspath: &Fspath, v: ArchiveVersion) -> Result<(), Error> {
    let (name, _) = archive_name(host, fspath, v)?;
    match system::unlink(&host.file_in_unison_dir(&name)) {
        Err(e) if !e.is_absent() => Err(e.to_fatal("removing archive")),
        _ => Ok(()),
    }
}

/// `removeArchiveOnRoot`.
pub static REMOVE_ARCHIVE: RootCmd<ArchiveVersion, ()> = RootCmd::new(
    "removeArchive",
    |v, w, _| v.marshal(w),
    |r, _| ArchiveVersion::unmarshal(r),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, v| Box::pin(async move { remove_archive_local(&host, &fspath, v) }),
);

/// `commitArchiveLocal`: renames the scratch archive to the new one.
pub fn commit_archive_local(host: &Host, fspath: &Fspath) -> Result<(), Error> {
    let (from, _) = archive_name(host, fspath, ArchiveVersion::ScratchArch)?;
    let (to, _) = archive_name(host, fspath, ArchiveVersion::NewArch)?;
    system::rename(
        &host.file_in_unison_dir(&from),
        &host.file_in_unison_dir(&to),
    )
    .map_err(|e| e.to_fatal("committing"))
}

/// `commitArchiveOnRoot`.
pub static COMMIT_ARCHIVE: RootCmd<(), ()> = RootCmd::new(
    "commitArchive",
    |(), _, _| {},
    |_, _| Ok(()),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, ()| Box::pin(async move { commit_archive_local(&host, &fspath) }),
);

/// `getArchiveInfo`: the archive file's status, `None` if it is missing.
pub fn get_archive_info(f: &[u8]) -> Result<Option<Stats>, Error> {
    match system::stat(f) {
        Ok(st) => Ok(Some(st)),
        Err(e) if e.is_absent() => Ok(None),
        Err(e) => Err(e.to_transient("querying file information")),
    }
}

/// `postCommitArchiveLocal`: makes the new archive the main one, as a hard
/// link or else a copy.
pub fn post_commit_archive_local(host: &Host, fspath: &Fspath) -> Result<(), Error> {
    let (from, _) = archive_name(host, fspath, ArchiveVersion::NewArch)?;
    let (to, this_root) = archive_name(host, fspath, ArchiveVersion::MainArch)?;
    let ffrom = host.file_in_unison_dir(&from);
    let fto = host.file_in_unison_dir(&to);
    let fatal = |e: SysError| e.to_fatal("copying archive");
    match system::unlink(&fto) {
        Err(e) if !e.is_absent() => return Err(fatal(e)),
        _ => {}
    }
    if system::link(&ffrom, &fto).is_err() {
        let bytes = std::fs::read(system::os_path(&ffrom)).map_err(|error| {
            fatal(SysError {
                call: "open",
                param: path_string(&ffrom),
                error,
            })
        })?;
        write_file(&fto, &bytes).map_err(fatal)?;
        // In case the file already existed.
        let _ = system::chmod(&fto, 0o600);
    }
    let info = get_archive_info(&fto)?;
    cache(host).borrow_mut().info.insert(this_root, info);
    Ok(())
}

/// `postCommitArchiveOnRoot`.
pub static POST_COMMIT_ARCHIVE: RootCmd<(), ()> = RootCmd::new(
    "postCommitArchive",
    |(), _, _| {},
    |_, _| Ok(()),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, ()| Box::pin(async move { post_commit_archive_local(&host, &fspath) }),
);

// ---- The archive cache ----

/// A host's archives, by root (`archiveCache`, `archivePropCache`,
/// `archiveInfoCache`, and `Props.Data`'s store).
#[derive(Default)]
struct ArchiveCache {
    archives: HashMap<String, Archive>,
    props: HashMap<String, ArchiveProps>,
    info: HashMap<String, Option<Stats>>,
    props_data: PropsData,
}

fn cache(host: &Host) -> Rc<std::cell::RefCell<ArchiveCache>> {
    host.state::<ArchiveCache>()
}

/// `getArchive`: runs `f` on the root's archive, if it is loaded.
pub fn with_archive<R>(host: &Host, this_root: &str, f: impl FnOnce(&Archive) -> R) -> Option<R> {
    cache(host).borrow().archives.get(this_root).map(f)
}

/// Runs `f` on the root's archive in the cache, to change it (the root
/// gets `NoArchive` first if it had none).
pub fn with_archive_mut<R>(host: &Host, this_root: &str, f: impl FnOnce(&mut Archive) -> R) -> R {
    let cache = cache(host);
    let mut cache = cache.borrow_mut();
    f(cache
        .archives
        .entry(this_root.to_owned())
        .or_insert(Archive::NoArchive))
}

/// `setArchiveLocal`: replaces the root's archive in the cache.
pub fn set_archive_local(host: &Host, this_root: &str, archive: Archive) {
    cache(host)
        .borrow_mut()
        .archives
        .insert(this_root.to_owned(), archive);
}

/// `getArchiveProps`.
pub fn archive_props(host: &Host, this_root: &str) -> Option<ArchiveProps> {
    cache(host).borrow().props.get(this_root).cloned()
}

/// `setArchivePropsLocal`.
pub fn set_archive_props_local(host: &Host, this_root: &str, props: ArchiveProps) {
    cache(host)
        .borrow_mut()
        .props
        .insert(this_root.to_owned(), props);
}

/// The host's shared props data (`Props.Data`), as last loaded.
pub fn props_data(host: &Host) -> PropsData {
    cache(host).borrow().props_data.clone()
}

/// Takes the root's archive out of the cache (leaving `NoArchive`), to
/// update it off the engine thread; [`set_archive_local`] puts it back.
pub fn take_archive(host: &Host, this_root: &str) -> Archive {
    cache(host)
        .borrow_mut()
        .archives
        .get_mut(this_root)
        .map_or(Archive::NoArchive, |a| {
            std::mem::replace(a, Archive::NoArchive)
        })
}

/// `externArchivePropsdata`: records in `props` the shared props data the
/// archive still uses (`prunePropsdata`). Only ACLs keep data; keys with
/// none are skipped (OCaml asserts there). If nothing is kept, `props` is
/// left as it was.
pub fn extern_archive_propsdata(host: &Host, archive: &Archive, props: &mut Proplist) {
    fn keys<'a>(a: &'a Archive, out: &mut HashSet<&'a [u8]>) {
        let acl = |p: &'a Props| p.acl.as_deref().filter(|s| !s.is_empty());
        match a {
            Archive::Dir(desc, children) => {
                out.extend(acl(desc));
                for (_, c) in children.iter() {
                    keys(c, out);
                }
            }
            Archive::File(desc, ..) => out.extend(acl(desc)),
            Archive::Symlink(_) | Archive::NoArchive => {}
        }
    }
    let mut used = HashSet::new();
    keys(archive, &mut used);
    let cache = cache(host);
    let cache = cache.borrow();
    let kept: Vec<(Vec<u8>, Vec<u8>)> = cache
        .props_data
        .iter()
        .filter(|(k, _)| k == b"ACL")
        .flat_map(|(_, l)| l.iter())
        .filter(|(k, _)| used.contains(k.as_slice()))
        .cloned()
        .collect();
    if !kept.is_empty() {
        props.add(&PROPS_DATA_KEY, vec![(b"ACL".to_vec(), kept)]);
    }
}

/// `Props.Data.merge`: adds another host's shared props data to this
/// host's. A key must not have two values.
pub fn merge_props_data(host: &Host, data: &PropsData) -> Result<(), Error> {
    let cache = cache(host);
    let mut cache = cache.borrow_mut();
    for (kind, entries) in data {
        if kind != b"xattr" && kind != b"ACL" {
            continue;
        }
        let store = match cache.props_data.iter().position(|(k, _)| k == kind) {
            Some(i) => &mut cache.props_data[i].1,
            None => {
                cache.props_data.push((kind.clone(), Vec::new()));
                &mut cache.props_data.last_mut().expect("just pushed").1
            }
        };
        for (key, value) in entries {
            match store.iter().find(|(k, _)| k == key) {
                None => store.push((key.clone(), value.clone())),
                Some((_, v)) if v == value => {}
                Some((_, v)) => {
                    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
                    return Err(Error::Fatal(format!(
                        "Internal integrity error (propsdata). Key {} returns different \
                         results:\n  (existing) {}\nand\n  (new) {}\n",
                        s(key),
                        s(v),
                        s(value)
                    )));
                }
            }
        }
    }
    Ok(())
}

/// `Props.Data.extern \`New`: the shared props data this host added since
/// it was loaded. gemel doesn't synchronise xattrs or ACLs yet, so there is
/// none.
pub fn new_props_data(_host: &Host) -> PropsData {
    PropsData::new()
}

/// `fileUnchanged`: whether two statuses are of the same regular file,
/// unmodified.
pub fn file_unchanged(prefs: &Prefs, old: Option<&Stats>, new: Option<&Stats>) -> bool {
    match (old, new) {
        (Some(o), Some(n)) => {
            o.kind == Kind::File
                && n.kind == Kind::File
                && o.mtime == n.mtime
                && o.size == n.size
                && (o.ino == n.ino
                    || *prefs.read(&fileinfo::IGNORE_INODE_NUMBERS)
                    || !system::has_inode_numbers())
        }
        _ => false,
    }
}

/// `archiveUnchanged`: whether the root's archive file is as it was when
/// it was loaded or stored.
pub fn archive_unchanged(host: &Host, this_root: &str, new: Option<&Stats>) -> bool {
    let cache = cache(host);
    let cache = cache.borrow();
    match cache.info.get(this_root) {
        Some(old) => file_unchanged(&host.prefs().borrow(), old.as_ref(), new),
        None => false,
    }
}

/// `internArchivePropsdata`: makes the archive's shared props data the
/// host's.
fn intern_archive_propsdata(host: &Host, props: &Proplist) {
    cache(host).borrow_mut().props_data = props.find(&PROPS_DATA_KEY).cloned().unwrap_or_default();
}

// ---- Case conversion ----

/// `makeCaseSensitiveRec`: re-sorts an archive made in case-sensitive mode
/// under `mode`, dropping names it can't represent and names that become
/// equal.
pub fn make_case_sensitive_rec(archive: &Archive, mode: Mode) -> Archive {
    match archive {
        Archive::Dir(desc, children) => {
            let mut dups = Vec::new();
            let mut out = NameMap::new(mode);
            for (n, child) in children.iter_desc() {
                if n.bad_encoding(mode) {
                    continue;
                }
                if out.mem(n) {
                    dups.push(n.clone());
                }
                out.add(n.clone(), make_case_sensitive_rec(child, mode));
            }
            for n in &dups {
                out.remove(n);
            }
            Archive::Dir(desc.clone(), out)
        }
        Archive::File(..) | Archive::Symlink(_) | Archive::NoArchive => archive.clone(),
    }
}

/// `makeCaseSensitive`: converts the root's archive to the host's mode, and
/// marks every directory possibly changed so that they are all rescanned.
pub fn make_case_sensitive(host: &Host, this_root: &str) {
    let mode = case::mode(&host.prefs().borrow());
    let cache = cache(host);
    let mut cache = cache.borrow_mut();
    if let Some(a) = cache.archives.get(this_root) {
        let a = make_case_sensitive_rec(a, mode);
        cache.archives.insert(this_root.to_owned(), a);
    }
    let props = cache
        .props
        .entry(this_root.to_owned())
        .or_insert_with(|| ArchiveProps::empty(mode));
    props.props.add(&CASE_KEY, mode.desc().as_bytes().to_vec());
    props.props.add(&DIR_STAMP_KEY, props::fresh_dir_stamp());
}

/// `makeCaseSensitiveOnRoot`.
pub static MAKE_CASE_SENSITIVE: RootCmd<(), ()> = RootCmd::new(
    "makeCaseSensitive",
    |(), _, _| {},
    |_, _| Ok(()),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, ()| {
        Box::pin(async move {
            make_case_sensitive(&host, &this_roots_global_name(&host, &fspath)?);
            Ok(())
        })
    },
);

/// `archiveMode`: the case mode an archive's magic records. A new archive
/// is in the current mode; an archive from before modes were recorded
/// can't be Unicode case insensitive.
pub fn archive_mode(magic: &[u8], current: Mode) -> Vec<u8> {
    if magic.is_empty() {
        return current.desc().as_bytes().to_vec();
    }
    match magic.iter().position(|&b| b == 0) {
        Some(i) => magic[..i].to_vec(),
        None if current == Mode::UnicodeInsensitive => b"some non-Unicode".to_vec(),
        None => current.desc().as_bytes().to_vec(),
    }
}

/// `checkArchiveCaseSensitivity`: makes sure the archives were made in the
/// current case mode. Archives made in case-sensitive mode are converted;
/// for any other difference, the user has to act.
pub async fn check_archive_case_sensitivity(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    let local = roots
        .local_root()
        .ok_or_else(|| Error::Fatal("Internal error: no roots are installed".into()))?;
    let host = rpc.local_host();
    let root = this_roots_global_name(host, &local.fspath)?;
    let cur = case::mode(&rpc.client_host().prefs().borrow()).desc();
    let arch = archive_props(host, &root)
        .and_then(|p| p.props.find(&CASE_KEY).cloned())
        .ok_or_else(|| Error::Fatal(format!("Internal error: no archive loaded for {root}")))?;
    if arch == cur.as_bytes() {
        return Ok(());
    }
    if arch == case::CASE_SENSITIVE_MODE_DESC.as_bytes() {
        return roots
            .iter(|r| rpc.call_root(&MAKE_CASE_SENSITIVE, r, ()))
            .await;
    }
    let names = roots
        .map(|r| rpc.call_root(&ARCHIVE_NAME, r, ArchiveVersion::MainArch))
        .await?;
    let mut lines = vec![
        "Warning: incompatible case sensitivity settings.".to_owned(),
        format!("Unison is currently in {cur} mode,"),
        format!(
            "while the archives were created in {} mode.",
            String::from_utf8_lossy(&arch)
        ),
        "You should either change Unison's setup or delete".into(),
        "the following archives from the .unison directories:".into(),
    ];
    lines.extend(
        names
            .iter()
            .map(|(name, host, _)| format!("    archive {name} on host {host}")),
    );
    lines.push("(or invoke Unison once with -ignorearchives flag).".into());
    lines.push("Then, try again.".into());
    Err(Error::Fatal(lines.join("\n")))
}

/// `populateCacheFromArchive`: fills the root's fingerprint cache from its
/// archive, so that converting the archive doesn't mean fingerprinting
/// everything again.
fn populate_cache_from_archive(
    host: &Host,
    fspath: &Fspath,
    archive: &Archive,
) -> Result<(), Error> {
    fn walk(cache: &mut FpCache, path: &Path<Local>, a: &Archive) {
        match a {
            Archive::Dir(_, children) => {
                for (n, child) in children.iter() {
                    walk(cache, &path.child(n), child);
                }
            }
            Archive::File(desc, dig, stamp, ress) => {
                cache.save(path, (desc.clone(), dig.clone(), *stamp, ress.clone()));
            }
            Archive::Symlink(_) | Archive::NoArchive => {}
        }
    }
    let (name, _) = archive_name(host, fspath, ArchiveVersion::FpCache)?;
    let file = system::os_path(&host.file_in_unison_dir(&name));
    let ignore = *host.prefs().borrow().read(&IGNORE_ARCHIVES);
    let c = PropsConfig::default().with_features(&host.features().borrow());
    let mut cache = FpCache::init(true, ignore, &file, c);
    walk(&mut cache, &Path::empty(), archive);
    cache.finish();
    Ok(())
}

// ---- Loading archives ----

/// What [`LOAD_ARCHIVE`] returns: the stored hash and magic, `(0, "")` for
/// an archive that is new or already loaded, or `None` if an optimistic
/// load failed.
pub type Checksum = Option<(i64, Vec<u8>)>;

fn set_archive_data(
    host: &Host,
    this_root: &str,
    fspath: &Fspath,
    data: ArchiveData,
    info: Option<Stats>,
) -> Result<Checksum, Error> {
    let cur = case::mode(&host.prefs().borrow());
    let arch_mode = archive_mode(&data.magic, cur);
    let mut props = data.props;
    props.props.add(&CASE_KEY, arch_mode.clone());
    intern_archive_propsdata(host, &props.props);
    if arch_mode != cur.desc().as_bytes() {
        populate_cache_from_archive(host, fspath, &data.archive)?;
    }
    {
        let cache = cache(host);
        let mut cache = cache.borrow_mut();
        cache.archives.insert(this_root.to_owned(), data.archive);
        cache.props.insert(this_root.to_owned(), props);
        cache.info.insert(this_root.to_owned(), info);
    }
    Ok(Some((data.hash, data.magic)))
}

fn clear_archive_data(host: &Host, this_root: &str) -> Checksum {
    let mode = case::mode(&host.prefs().borrow());
    let mut props = ArchiveProps::empty(mode);
    props.props.add(&CASE_KEY, mode.desc().as_bytes().to_vec());
    intern_archive_propsdata(host, &props.props);
    let cache = cache(host);
    let mut cache = cache.borrow_mut();
    cache
        .archives
        .insert(this_root.to_owned(), Archive::NoArchive);
    cache.props.insert(this_root.to_owned(), props);
    cache.info.remove(this_root);
    Some((0, Vec::new()))
}

/// `foundArchives`, as a host slot.
struct FoundArchives(bool);

impl Default for FoundArchives {
    fn default() -> Self {
        FoundArchives(true)
    }
}

fn set_found_archives(host: &Host, found: bool) {
    host.state::<FoundArchives>().borrow_mut().0 = found;
}

/// `foundArchives`: false if the last load ignored the archives, or crash
/// recovery found none. (OCaml keeps one flag per process; here the
/// client and the host of local roots each have one. Loads set the host's,
/// recovery sets both.)
pub fn found_archives(rpc: &Rpc) -> bool {
    rpc.client_host().state::<FoundArchives>().borrow().0
        && rpc.local_host().state::<FoundArchives>().borrow().0
}

async fn load_archive_on_root(
    host: &Host,
    fspath: &Fspath,
    optimistic: bool,
) -> Result<Checksum, Error> {
    let (arc_name, this_root) = archive_name(host, fspath, ArchiveVersion::MainArch)?;
    let arc_file = host.file_in_unison_dir(&arc_name);
    if *host.prefs().borrow().read(&IGNORE_ARCHIVES) {
        set_found_archives(host, false);
        return Ok(clear_archive_data(host, &this_root));
    }
    if !optimistic {
        return match load_archive_local(host, &arc_file, &this_root)? {
            Some(data) => {
                let info = get_archive_info(&arc_file)?;
                set_archive_data(host, &this_root, fspath, data, info)
            }
            None => Ok(clear_archive_data(host, &this_root)),
        };
    }
    // If the archive is not in a stable state, it needs recovery, and the
    // optimistic load fails.
    let (new_name, _) = archive_name(host, fspath, ArchiveVersion::NewArch)?;
    let (lock_name, _) = archive_name(host, fspath, ArchiveVersion::Lock)?;
    if system::file_exists(&host.file_in_unison_dir(&new_name))
        || lock::is_locked(&host.file_in_unison_dir(&lock_name))
    {
        return Ok(None);
    }
    let info = get_archive_info(&arc_file)?;
    if archive_unchanged(host, &this_root, info.as_ref()) {
        return Ok(Some((0, Vec::new())));
    }
    match load_archive_local(host, &arc_file, &this_root)? {
        Some(data) => {
            let info2 = get_archive_info(&arc_file)?;
            if file_unchanged(&host.prefs().borrow(), info.as_ref(), info2.as_ref()) {
                set_archive_data(host, &this_root, fspath, data, info)
            } else {
                // The archive changed while it was being loaded.
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

/// `loadArchiveOnRoot`: loads the root's archive into the host's cache.
///
/// An optimistic load fails (`None`) unless the archive is in a stable
/// state, and does nothing if the cached archive is still that on disk
/// (`(0, "")`; such a result can't be compared with one read from disk, so
/// the archives may then be loaded again, as in OCaml).
pub static LOAD_ARCHIVE: RootCmd<bool, Checksum> = RootCmd::new(
    "loadArchive",
    |b, w, _| w.bool(*b),
    |r, _| r.bool(),
    |c, w, _| {
        w.option(c.as_ref(), |w, (h, m)| {
            w.int(*h);
            w.string(m);
        });
    },
    |r, _| r.option(|r| Ok((r.int()?, r.string()?))),
    |host, fspath, optimistic| {
        Box::pin(async move { load_archive_on_root(&host, &fspath, optimistic).await })
    },
);

/// `loadArchives`: loads every root's archive; returns whether they are
/// identical, and the checksums in the user's order of the roots. A
/// non-optimistic load fails if they differ.
pub async fn load_archives(
    rpc: &Rpc,
    roots: &Roots,
    optimistic: bool,
) -> Result<(bool, Vec<Checksum>), Error> {
    let checksums = roots
        .map(|r| rpc.call_root(&LOAD_ARCHIVE, r, optimistic))
        .await?;
    let identical = archives_identical(&checksums);
    if !(optimistic || identical) {
        return Err(Error::Fatal(format!(
            "Internal error: On-disk archives are not identical.\n\
             \n\
             This can happen when both machines have the same hostname.\n\
             It can also happen when one copy of Unison has been compiled with\n\
             OCaml version 3 and one with OCaml version 4.\n\
             \n\
             If this is not the case and you get this message repeatedly, please:\n  \
             a) Send a bug report to unison-users@seas.upenn.edu (you may need\n     \
             to join the group before you will be allowed to post).\n     \
             For information, see https://github.com/bcpierce00/unison/wiki\n  \
             b) Move the archive files on each machine to some other directory\n     \
             (in case they may be useful for debugging).\n     \
             The archive files on this machine are in the directory\n       \
             {}\n     \
             and have names of the form\n       \
             arXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\n     \
             where the X's are hexadecimal numbers.\n  \
             c) Run unison again to synchronize from scratch.\n",
            rpc.client_host().unison_dir().display()
        )));
    }
    Ok((identical, checksums))
}

// ---- Locks ----

/// The locks a host holds (`heldLocks`), released when the host goes away
/// (OCaml releases them at exit).
#[derive(Default)]
struct HeldLocks(HashSet<Vec<u8>>);

impl Drop for HeldLocks {
    fn drop(&mut self) {
        for l in self.0.drain() {
            lock::release(&l);
        }
    }
}

/// `releaseHeldLocks`: releases every lock the host holds.
pub fn release_held_locks(host: &Host) {
    let locks: Vec<_> = host.state::<HeldLocks>().borrow_mut().0.drain().collect();
    for l in locks {
        lock::release(&l);
    }
}

fn acquire_lock(host: &Host, name: Vec<u8>) -> Result<bool, Error> {
    if lock::acquire(&name)? {
        host.state::<HeldLocks>().borrow_mut().0.insert(name);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn release_lock(host: &Host, name: &[u8]) {
    host.state::<HeldLocks>().borrow_mut().0.remove(name);
    lock::release(name);
}

/// `lockArchiveLocal`: takes the root's lock; if it is held, what the user
/// should do about it.
pub fn lock_archive_local(host: &Host, fspath: &Fspath) -> Result<Option<String>, Error> {
    let (name, _) = archive_name(host, fspath, ArchiveVersion::Lock)?;
    let file = host.file_in_unison_dir(&name);
    if acquire_lock(host, file.clone())? {
        Ok(None)
    } else {
        Ok(Some(format!(
            "The file {} on host {} should be deleted",
            path_string(&file),
            host.canonical_host_name()
        )))
    }
}

/// `lockArchiveOnRoot`.
pub static LOCK_ARCHIVE: RootCmd<(), Option<String>> = RootCmd::new(
    "lockArchive",
    |(), _, _| {},
    |_, _| Ok(()),
    |s, w, _| w.option(s.as_ref(), |w, s| w.string(s.as_bytes())),
    |r, _| r.option(read_text),
    |host, fspath, ()| Box::pin(async move { lock_archive_local(&host, &fspath) }),
);

/// `unlockArchiveLocal`.
pub fn unlock_archive_local(host: &Host, fspath: &Fspath) -> Result<(), Error> {
    let (name, _) = archive_name(host, fspath, ArchiveVersion::Lock)?;
    release_lock(host, &host.file_in_unison_dir(&name));
    Ok(())
}

/// `unlockArchiveOnRoot`.
pub static UNLOCK_ARCHIVE: RootCmd<(), ()> = RootCmd::new(
    "unlockArchive",
    |(), _, _| {},
    |_, _| Ok(()),
    |(), _, _| {},
    |_, _| Ok(()),
    |host, fspath, ()| Box::pin(async move { unlock_archive_local(&host, &fspath) }),
);

/// `locked`: whether this client holds the archive locks.
#[derive(Default)]
struct Locked(bool);

/// `lockArchives`: locks every root's archive. If any lock is held
/// elsewhere (and `ignorelocks` is off), releases those it got and fails
/// with what to delete.
pub async fn lock_archives(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    let locked = rpc.client_host().state::<Locked>();
    if locked.borrow().0 {
        return Err(Error::Fatal(
            "Internal error: the archives are already locked".into(),
        ));
    }
    let result = roots.map(|r| rpc.call_root(&LOCK_ARCHIVE, r, ())).await?;
    let ignore = *rpc.client_host().prefs().borrow().read(&IGNORE_LOCKS);
    if result.iter().any(Option::is_some) && !ignore {
        roots
            .iter2(
                |r, st: Option<String>| async move {
                    match st {
                        None => rpc.call_root(&UNLOCK_ARCHIVE, r, ()).await,
                        Some(_) => Ok(()),
                    }
                },
                result.clone(),
            )
            .await?;
        let mut lines = vec![
            "Warning: the archives are locked.  ".to_owned(),
            format!("If no other instance of {MY_NAME} is running, the locks should be removed."),
        ];
        lines.extend(result.into_iter().flatten());
        lines.push("Please delete lock files as appropriate and try again.".into());
        return Err(Error::Fatal(lines.join("\n")));
    }
    locked.borrow_mut().0 = true;
    Ok(())
}

/// `unlockArchives`: releases the locks [`lock_archives`] took.
pub async fn unlock_archives(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    let locked = rpc.client_host().state::<Locked>();
    if !locked.borrow().0 {
        return Ok(());
    }
    roots
        .iter(|r| rpc.call_root(&UNLOCK_ARCHIVE, r, ()))
        .await?;
    locked.borrow_mut().0 = false;
    Ok(())
}

// ---- Crash recovery ----
//
// Archives are written with a joint commit, so that an interrupted run
// leaves them consistent: every host writes a scratch archive and renames it
// to a new one; then every new archive is copied to the main one and
// removed. So if every host has a new archive, those are coherent;
// otherwise the main archives are.

/// `archivesExistOnRoot`: whether the root's main and new archives exist.
pub static ARCHIVES_EXIST: RootCmd<(), (bool, bool)> = RootCmd::new(
    "archivesExist",
    |(), _, _| {},
    |_, _| Ok(()),
    |(old, new), w, _| {
        w.bool(*old);
        w.bool(*new);
    },
    |r, _| Ok((r.bool()?, r.bool()?)),
    |host, fspath, ()| {
        Box::pin(async move {
            let exists = |v| -> Result<bool, Error> {
                let (name, _) = archive_name(&host, &fspath, v)?;
                Ok(system::file_exists(&host.file_in_unison_dir(&name)))
            };
            Ok((
                exists(ArchiveVersion::MainArch)?,
                exists(ArchiveVersion::NewArch)?,
            ))
        })
    },
);

/// `doArchiveCrashRecovery`: finishes or rolls back an interrupted commit,
/// and checks that every root has an archive, or none does.
pub async fn do_archive_crash_recovery(rpc: &Rpc, roots: &Roots) -> Result<(), Error> {
    let client = rpc.client_host();
    let exl = roots.map(|r| rpc.call_root(&ARCHIVES_EXIST, r, ())).await?;
    let (old, new): (Vec<bool>, Vec<bool>) = exl.into_iter().unzip();
    if new.iter().all(|&b| b) {
        client.warn(&format!(
            "Warning: {MY_NAME} may have terminated abnormally last time.\n\
             A new archive exists on all hosts: I'll use them.\n"
        ));
        roots
            .iter(|r| rpc.call_root(&POST_COMMIT_ARCHIVE, r, ()))
            .await?;
        roots
            .iter(|r| rpc.call_root(&REMOVE_ARCHIVE, r, ArchiveVersion::NewArch))
            .await?;
    } else if new.iter().any(|&b| b) {
        client.warn(&format!(
            "Warning: {MY_NAME} may have terminated abnormally last time.\n\
             A new archive exists on some hosts only; it will be ignored.\n"
        ));
        roots
            .iter(|r| rpc.call_root(&REMOVE_ARCHIVE, r, ArchiveVersion::NewArch))
            .await?;
    }
    if old.iter().all(|&b| b) {
        set_found_archives(client, true);
        set_found_archives(rpc.local_host(), true);
        Ok(())
    } else if old.iter().any(|&b| b) {
        let names = roots
            .map(|r| rpc.call_root(&ARCHIVE_NAME, r, ArchiveVersion::MainArch))
            .await?;
        let mut lines = vec![
            "Warning: inconsistent state.  ".to_owned(),
            "The archive file is missing on some hosts.".into(),
            "For safety, the remaining copies should be deleted.".into(),
        ];
        lines.extend(names.iter().map(|(name, host, exists)| {
            format!(
                "  Archive {name} on host {host} {}",
                if *exists {
                    "should be DELETED"
                } else {
                    "is MISSING"
                }
            )
        }));
        lines.push("Please delete archive files as appropriate and try again".into());
        lines.push("or invoke Unison with -ignorearchives flag.".into());
        Err(Error::Fatal(lines.join("\n")))
    } else {
        set_found_archives(client, false);
        set_found_archives(rpc.local_host(), false);
        let expected: Vec<String> = roots.list().iter().map(Root::to_string).collect();
        client.warn(&format!(
            "No archive files were found for these roots, whose canonical names are:\n\t{}\n\
             This can happen either\n\
             because this is the first time you have synchronized these roots, \n\
             or because you have upgraded Unison to a new version with a different\n\
             archive format.  \n\n\
             Update detection may take a while on this run if the replicas are \n\
             large.\n\n\
             Unison will assume that the 'last synchronized state' of both replicas\n\
             was completely empty.  This means that any files that are different\n\
             will be reported as conflicts, and any files that exist only on one\n\
             replica will be judged as new and propagated to the other replica.\n\
             If the two replicas are identical, then no changes will be reported.\n\n\
             If you see this message repeatedly, it may be because one of your machines\n\
             is getting its address from DHCP, which is causing its host name to change\n\
             between synchronizations.  See the documentation for the UNISONLOCALHOSTNAME\n\
             environment variable for advice on how to correct this.\n\
             \n",
            expected.join("\n\t")
        ));
        Ok(())
    }
}

/// The archive half of `findUpdatesOnPaths`: loads the archives,
/// optimistically first; if that fails, recovers from an interrupted
/// commit and loads them again under the locks. Then checks their case
/// mode. Returns the checksums.
pub async fn load_archives_safely(rpc: &Rpc, roots: &Roots) -> Result<Vec<Checksum>, Error> {
    let (ok, checksums) = load_archives(rpc, roots, true).await?;
    let checksums = if ok {
        checksums
    } else {
        lock_archives(rpc, roots).await?;
        let loaded = match do_archive_crash_recovery(rpc, roots).await {
            Ok(()) => load_archives(rpc, roots, false).await,
            Err(e) => Err(e),
        };
        // Remote.Thread.unwindProtect: unlock, then pass the error on.
        unlock_archives(rpc, roots).await?;
        loaded?.1
    };
    check_archive_case_sensitivity(rpc, roots).await?;
    Ok(checksums)
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    let r = rpc.registry_mut();
    r.register_root(&ARCHIVE_NAME)?;
    r.register_root(&REMOVE_ARCHIVE)?;
    r.register_root(&COMMIT_ARCHIVE)?;
    r.register_root(&POST_COMMIT_ARCHIVE)?;
    r.register_root(&MAKE_CASE_SENSITIVE)?;
    r.register_root(&LOAD_ARCHIVE)?;
    r.register_root(&LOCK_ARCHIVE)?;
    r.register_root(&UNLOCK_ARCHIVE)?;
    r.register_root(&ARCHIVES_EXIST)
}
