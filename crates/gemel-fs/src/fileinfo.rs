//! File type, properties and change stamps.
//!
//! Ports Unison `src/fileinfo.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use gemel_base::path::{Local, Path};
use gemel_base::prefs::{BoolWithDefault, Category, Internal, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};

use crate::fs;
use crate::fspath::Fspath;
use crate::osx::{self, InfoKind};
use crate::props::{Props, PropsConfig, SetKind};
use crate::system::{Kind, Stats, SysError};

/// `links`: synchronise symbolic links (`true`/`false`/`default`).
pub static ALLOW_SYMLINKS: PrefDef<BoolWithDefault> = PrefDef::new(
    "links",
    Category::Advanced(Topic::Sync),
    BoolWithDefault::default,
    "allow the synchronization of symbolic links (true/false/default)",
    "When set to {\\tt true}, this flag causes Unison to synchronize \
     symbolic links.  When the flag is set to {\\tt false}, symbolic \
     links will be ignored during update detection.  \
     Ordinarily, when the flag is set to {\\tt default}, symbolic \
     links are synchronized except when one of the hosts is running \
     Windows.  On a Windows client, Unison makes an attempt to detect \
     if symbolic links are supported and allowed by user privileges.  \
     You may have to get elevated privileges to create symbolic links.  \
     When the flag is set to {\\tt default} and symbolic links can't be \
     synchronized then an error is produced during update detection.",
);

/// `links-aux`: the resolved `links`.
pub static SYMLINKS_ALLOWED: PrefDef<bool> = PrefDef::new(
    "links-aux",
    Category::Internal(Internal::Pseudo),
    || true,
    "*Pseudo-preference for internal use only",
    "",
);

/// `Fileinfo.init`: resolves `links` once it is known whether a host runs
/// Windows.
pub fn init(prefs: &mut Prefs, some_host_running_windows: bool) {
    let l = *prefs.read(&ALLOW_SYMLINKS);
    prefs.set(
        &SYMLINKS_ALLOWED,
        l == BoolWithDefault::True
            || (l == BoolWithDefault::Default && (!some_host_running_windows || fs::has_symlink())),
    );
}

/// The settings the filesystem functions depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsConfig {
    /// For the props.
    pub props: PropsConfig,
    /// `links`.
    pub allow_symlinks: BoolWithDefault,
    /// `links-aux`.
    pub symlinks_allowed: bool,
    /// `ignoreinodenumbers`.
    pub ignore_inode_numbers: bool,
    /// `dontchmod`.
    pub dont_chmod: bool,
}

impl FsConfig {
    /// Reads the settings from registered preferences (see [`register_prefs`]
    /// and [`crate::props::register_prefs`]).
    pub fn from_prefs(prefs: &Prefs) -> FsConfig {
        FsConfig {
            props: PropsConfig::from_prefs(prefs),
            allow_symlinks: *prefs.read(&ALLOW_SYMLINKS),
            symlinks_allowed: *prefs.read(&SYMLINKS_ALLOWED),
            ignore_inode_numbers: *prefs.read(&IGNORE_INODE_NUMBERS),
            dont_chmod: *prefs.read(&crate::props::DONT_CHMOD),
        }
    }
}

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
    prefs.register(&ALLOW_SYMLINKS)?;
    prefs.register(&SYMLINKS_ALLOWED)?;
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

/// A file's type (`Fileinfo.typ`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Typ {
    /// Nothing there.
    Absent,
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
}

impl Typ {
    /// `Fileinfo.mtyp`.
    pub fn marshal(self, w: &mut Writer) {
        w.tag(match self {
            Typ::Absent => 0,
            Typ::File => 1,
            Typ::Directory => 2,
            Typ::Symlink => 3,
        });
    }

    /// `Fileinfo.mtyp`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Typ> {
        Ok(match r.tag(4)? {
            0 => Typ::Absent,
            1 => Typ::File,
            2 => Typ::Directory,
            _ => Typ::Symlink,
        })
    }

    /// `Fileinfo.type2string`.
    pub fn as_str(self) -> &'static str {
        match self {
            Typ::Absent => "nonexistent",
            Typ::File => "file",
            Typ::Directory => "dir",
            Typ::Symlink => "symlink",
        }
    }

    /// The kind of Finder information a file of this type has.
    pub fn info_kind(self) -> InfoKind {
        match self {
            Typ::File => InfoKind::File,
            Typ::Directory => InfoKind::Dir,
            _ => InfoKind::Other,
        }
    }
}

/// What update detection knows about a file (`Fileinfo.t`).
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// Its type.
    pub typ: Typ,
    /// The inode number, truncated to 30 bits (as OCaml stores it).
    pub inode: i64,
    /// Its props.
    pub desc: Props,
    /// Its Finder information and resource fork.
    pub os_x: osx::Info,
}

impl Info {
    /// `Fileinfo.m`.
    pub fn marshal(&self, w: &mut Writer, c: &PropsConfig) {
        self.typ.marshal(w);
        w.int(self.inode);
        self.desc.marshal(w, c);
        self.os_x.marshal(w);
    }

    /// `Fileinfo.m`.
    pub fn unmarshal(r: &mut Reader<'_>, c: &PropsConfig) -> umarshal::Result<Info> {
        Ok(Info {
            typ: Typ::unmarshal(r)?,
            inode: r.int()?,
            desc: Props::unmarshal(r, c)?,
            os_x: osx::Info::unmarshal(r)?,
        })
    }

    /// `Fileinfo.stamp`.
    pub fn stamp(&self, c: &FsConfig) -> Stamp {
        if c.ignore_inode_numbers || !fs::has_inode_numbers() {
            Stamp::NoStamp
        } else {
            Stamp::Inode(self.inode)
        }
    }

    /// `Fileinfo.ressStamp`.
    pub fn ress_stamp(&self) -> osx::RessStamp {
        self.os_x.ress_info.stamp()
    }
}

/// The message suffix for a symbolic link where links aren't allowed. It
/// is also matched by [`should_ignore`], so it must not change.
pub const SYMLINK_ERR: &str = " is a symbolic link";

/// `Fileinfo.shouldIgnore`: whether an error is about a symbolic link
/// that `links = false` says to skip silently.
pub fn should_ignore(message: &str, c: &FsConfig) -> bool {
    c.allow_symlinks == BoolWithDefault::False
        && message.len() > SYMLINK_ERR.len()
        && message.ends_with(SYMLINK_ERR)
}

/// `Fileinfo.statFn`: `lstat`, or `stat` for a symbolic link the `follow`
/// preference matches (only when scanning from the root).
fn stat_fn(
    from_root: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    follow: &dyn Fn(&Path<Local>) -> bool,
) -> Result<Stats, StatError> {
    let full = fspath.concat(path);
    let st = fs::lstat(&full).map_err(StatError::Sys)?;
    if st.kind == Kind::Symlink && from_root && follow(path) {
        return fs::stat(&full).map_err(|e| {
            if e.is_absent() {
                StatError::Transient(Error::Transient(format!(
                    "Path {full} is marked 'follow' but its target is missing"
                )))
            } else {
                StatError::Sys(e)
            }
        });
    }
    Ok(st)
}

enum StatError {
    Sys(SysError),
    Transient(Error),
}

/// `Fileinfo.getAux`: stats the path and builds its info, computing the
/// props with `props`. A missing path is [`Typ::Absent`].
fn get_aux(
    from_root: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    follow: &dyn Fn(&Path<Local>) -> bool,
    c: &FsConfig,
    props: &dyn Fn(&Stats, &osx::Info) -> Props,
) -> Result<Info, Error> {
    let what = "querying file information";
    let absent = || -> Result<Info, Error> {
        Ok(Info {
            typ: Typ::Absent,
            inode: 0,
            desc: Props::dummy(),
            os_x: osx::get_file_infos(fspath, path, InfoKind::Other, c.props.rsrc)?,
        })
    };
    let st = match stat_fn(from_root, fspath, path, follow) {
        Ok(st) => st,
        Err(StatError::Sys(e)) if e.is_absent() => return absent(),
        Err(StatError::Sys(e)) => return Err(e.to_transient(what)),
        Err(StatError::Transient(e)) => return Err(e),
    };
    let full = fspath.concat(path);
    let typ = match st.kind {
        Kind::File => Typ::File,
        Kind::Dir => Typ::Directory,
        Kind::Symlink if !from_root || c.symlinks_allowed => Typ::Symlink,
        Kind::Symlink => {
            // A Transient passes through convertUnixErrorsToTransient
            // unchanged, so there is no "Error in ..." prefix.
            return Err(Error::Transient(format!("path {full}{SYMLINK_ERR}")));
        }
        Kind::Other => {
            return Err(Error::Transient(format!(
                "path {full} has unknown file type"
            )));
        }
    };
    let os_x = osx::get_file_infos(fspath, path, typ.info_kind(), c.props.rsrc)?;
    Ok(Info {
        typ,
        // Truncated to fit a 31-bit OCaml int, as archives store it.
        inode: st.ino & 0x3FFF_FFFF,
        desc: props(&st, &os_x),
        os_x,
    })
}

/// `Fileinfo.getType`.
pub fn get_type(
    from_root: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    follow: &dyn Fn(&Path<Local>) -> bool,
    c: &FsConfig,
) -> Result<Typ, Error> {
    Ok(get_aux(from_root, fspath, path, follow, c, &|_, _| Props::dummy())?.typ)
}

/// `Fileinfo.getBasic`: with the props `stat` gives.
pub fn get_basic(
    from_root: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    follow: &dyn Fn(&Path<Local>) -> bool,
    c: &FsConfig,
) -> Result<Info, Error> {
    get_aux(from_root, fspath, path, follow, c, &|st, _| {
        Props::get_basic(st, &c.props)
    })
}

/// `Fileinfo.get`: with all the props.
pub fn get(
    from_root: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    follow: &dyn Fn(&Path<Local>) -> bool,
    c: &FsConfig,
) -> Result<Info, Error> {
    get_aux(from_root, fspath, path, follow, c, &|st, os_x| {
        Props::get(st, os_x, &c.props)
    })
}

/// `Fileinfo.check`: the props on disk after setting them.
pub fn check(
    fspath: &Fspath,
    path: &Path<Local>,
    props: &Props,
    c: &FsConfig,
) -> Result<(), Error> {
    let st = match stat_fn(false, fspath, path, &|_| false) {
        Ok(st) => st,
        Err(StatError::Sys(e)) => return Err(e.to_transient("checking file information")),
        Err(StatError::Transient(e)) => return Err(e),
    };
    props.check(fspath, path, &st, &c.props)
}

/// What [`set`] starts from.
#[derive(Debug, Clone)]
pub enum SetAction<'a> {
    /// A new file: these defaults, overridden by the new props.
    Set(&'a Props),
    /// A copy of the file at this path: its props, overridden.
    Copy(&'a Path<Local>),
    /// An existing file whose props were these: only what changed.
    Update(&'a Props),
}

/// `Fileinfo.set`: sets props on disk, then checks them.
pub fn set(
    fspath: &Fspath,
    path: &Path<Local>,
    action: SetAction<'_>,
    new: &Props,
    c: &FsConfig,
) -> Result<(), Error> {
    let p = &c.props;
    let (kind, props) = match action {
        SetAction::Set(defaults) => (SetKind::Set, defaults.override_with(new, p)),
        SetAction::Copy(old) => (
            SetKind::Set,
            get(false, fspath, old, &|_| false, c)?
                .desc
                .override_with(new, p),
        ),
        SetAction::Update(old) => (
            SetKind::Update,
            get(false, fspath, path, &|_| false, c)?
                .desc
                .override_with(&old.diff(new, p), p),
        ),
    };
    props.set(fspath, path, kind, p, c.dont_chmod)?;
    check(fspath, path, &props, c)
}

/// `Fileinfo.unchanged`: rereads the info and says whether the data and the
/// resource fork are unchanged. A file modified within the current second
/// is reported as changed (after a one-second pause), since a further
/// change in the same second would be invisible.
pub fn unchanged(
    fspath: &Fspath,
    path: &Path<Local>,
    info: &Info,
    follow: &dyn Fn(&Path<Local>) -> bool,
    c: &FsConfig,
) -> Result<(Info, bool, bool), Error> {
    // The time must be taken before the stat.
    let t0 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let info2 = get(true, fspath, path, follow, c)?;
    let data_unchanged = info.desc.same_time(&info2.desc, &c.props)
        && info.stamp(c) == info2.stamp(c)
        && if info2.desc.time.extract() == t0 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            false
        } else {
            true
        };
    let ress_unchanged = osx::ress_unchanged(
        &info.os_x.ress_info,
        &info2.os_x.ress_info,
        Some(t0),
        data_unchanged,
    );
    Ok((info2, data_unchanged, ress_unchanged))
}
