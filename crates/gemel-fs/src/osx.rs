//! macOS resource forks and Finder information.
//!
//! Ports Unison `src/osx.ml`, Unison `src/osxsupport.c`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//! Derived from Unison, Copyright 1999-2008.
//!
//! On macOS, Finder information and resource-fork lengths come from
//! `getattrlist`, as in `osxsupport.c`. Not ported yet: AppleDouble (`._`)
//! files, which carry this information on filesystems without it (a macOS
//! host syncing to a FAT or Linux volume); where one exists it is reported
//! as an error rather than silently ignored. Resource forks are read and
//! written through their named-fork paths (`..namedfork/rsrc`).

use gemel_base::prefs::{BoolWithDefault, Category, Internal, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};

use crate::fspath::Fspath;
use crate::props::Filesize;
use crate::system::SysError;
use gemel_base::path::Path;

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

/// What `Osx.getFileInfos` returns: where the resource fork is (with the
/// AppleDouble file and offset, if it is in one) and the Finder
/// information, trimmed (`"F"` + type, creator and flags for files, `"D"` +
/// flags for directories).
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// The resource fork.
    pub ress_info: RessInfo<(Fspath, i64)>,
    /// The Finder information.
    pub finfo: Vec<u8>,
}

impl Info {
    /// `Osx.minfo`.
    pub fn marshal(&self, w: &mut Writer) {
        self.ress_info.marshal(w, |w, (f, off)| {
            f.marshal(w);
            w.int64(*off);
        });
        w.string(&self.finfo);
    }

    /// `Osx.minfo`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Info> {
        Ok(Info {
            ress_info: RessInfo::unmarshal(r, |r| Ok((Fspath::unmarshal(r)?, r.int64()?)))?,
            finfo: r.string()?,
        })
    }
}

/// Whether [`get_file_infos`] is for a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// Anything else, or nothing.
    Other,
}

/// `Osx.defaultInfos`.
pub fn default_infos(kind: InfoKind) -> Info {
    Info {
        ress_info: RessInfo::NoRess,
        finfo: match kind {
            InfoKind::File => b"F".to_vec(),
            InfoKind::Dir => b"D".to_vec(),
            InfoKind::Other => Vec::new(),
        },
    }
}

/// `Osx.extractInfo`: the parts of the 32-byte Finder information that are
/// synchronised, without trailing zeroes.
pub fn extract_info(kind: InfoKind, info: &[u8; 32]) -> Vec<u8> {
    let mut flags = [info[8], info[9]];
    // Ignore the hasBeenInited flag.
    flags[0] &= 0xfe;
    // Clear the extended flags if they are to be ignored.
    let xflags = if info[24] & 0x80 != 0 {
        [0, 0]
    } else {
        [info[24], info[25]]
    };
    let mut out = match kind {
        InfoKind::File => [&b"F"[..], &info[..8], &flags, &xflags].concat(),
        _ => [&b"D"[..], &flags, &xflags].concat(),
    };
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

/// `Osx.getFileInfos`: the Finder information and resource fork of
/// `fspath/path`, if `rsrc` is on.
pub fn get_file_infos(
    fspath: &Fspath,
    path: &Path<gemel_base::path::Local>,
    kind: InfoKind,
    rsrc: bool,
) -> Result<Info, Error> {
    if !rsrc || kind == InfoKind::Other {
        return Ok(default_infos(kind));
    }
    let abspath = fspath.concat(path);
    match native_infos(abspath.as_bytes(), kind == InfoKind::File) {
        Ok((finfo, rsrc_length)) => Ok(Info {
            ress_info: if rsrc_length == 0 {
                RessInfo::NoRess
            } else {
                RessInfo::HfsRess(rsrc_length)
            },
            finfo: extract_info(kind, &finfo),
        }),
        Err(NativeError::Unsupported) => {
            // Not an HFS-style volume: OCaml looks for an AppleDouble file.
            match crate::fs::file_exists(&abspath.apple_double()) {
                Ok(false) | Err(_) => Ok(default_infos(kind)),
                Ok(true) => Err(Error::Transient(format!(
                    "AppleDouble file {} found: AppleDouble files are not supported yet",
                    abspath.apple_double()
                ))),
            }
        }
        Err(NativeError::Sys(e)) => Err(e.to_transient("getting file information")),
    }
}

/// `Osx.ressFingerprint`: the MD5 of the resource fork, or the dummy
/// fingerprint where there is none.
pub fn ress_fingerprint(
    fspath: &Fspath,
    path: &Path<gemel_base::path::Local>,
    kind: InfoKind,
    rsrc: bool,
) -> Result<crate::fingerprint::Fingerprint, Error> {
    use crate::fingerprint::Fingerprint;
    match get_file_infos(fspath, path, kind, rsrc)?.ress_info {
        RessInfo::NoRess => Ok(Fingerprint::DUMMY),
        RessInfo::HfsRess(_) => Fingerprint::file(fspath, &ress_path(path)?),
        RessInfo::AppleDoubleRess(_, _, _, len, (f, offset)) => {
            Fingerprint::subfile(&f, offset as u64, len)
        }
    }
}

/// `Osx.ressPath`: the path of a file's resource fork
/// (`path/..namedfork/rsrc`).
pub fn ress_path(
    path: &Path<gemel_base::path::Local>,
) -> Result<Path<gemel_base::path::Local>, Error> {
    use gemel_base::name::Name;
    Ok(path
        .child(&Name::from_bytes("..namedfork")?)
        .child(&Name::from_bytes("rsrc")?))
}

/// The error for a resource fork that only an AppleDouble file could hold.
fn apple_double_unsupported(what: &str, abspath: &Fspath) -> Error {
    Error::Transient(format!(
        "Error in {what}:
the volume of {abspath} has no native resource forks, and \
         AppleDouble files are not supported yet"
    ))
}

fn is_missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// `Osx.openRessIn`: opens a file's resource fork to read it. (Volumes
/// without native forks need AppleDouble files, not supported yet.)
pub fn open_ress_in(
    fspath: &Fspath,
    path: &Path<gemel_base::path::Local>,
) -> Result<std::fs::File, Error> {
    const WHAT: &str = "reading resource fork";
    let p = fspath.concat(&ress_path(path)?);
    match std::fs::File::open(crate::system::os_path(p.as_bytes())) {
        Ok(f) => Ok(f),
        Err(e) if is_missing(&e) => Err(apple_double_unsupported(WHAT, &fspath.concat(path))),
        Err(e) => Err(SysError::new("open", p.as_bytes(), e).to_transient(WHAT)),
    }
}

/// `Osx.openRessOut`: opens (creating) a file's resource fork to write it.
pub fn open_ress_out(
    fspath: &Fspath,
    path: &Path<gemel_base::path::Local>,
) -> Result<std::fs::File, Error> {
    const WHAT: &str = "writing resource fork";
    let p = fspath.concat(&ress_path(path)?);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    match opts.open(crate::system::os_path(p.as_bytes())) {
        Ok(f) => Ok(f),
        Err(e) if is_missing(&e) => Err(apple_double_unsupported(WHAT, &fspath.concat(path))),
        Err(e) => Err(SysError::new("open", p.as_bytes(), e).to_transient(WHAT)),
    }
}

/// `Osx.ressLength`.
pub fn ress_length<T>(r: &RessInfo<T>) -> Filesize {
    match r {
        RessInfo::NoRess => 0,
        RessInfo::HfsRess(len) | RessInfo::AppleDoubleRess(_, _, _, len, _) => *len,
    }
}

/// `Osx.ressUnchanged`: whether a resource fork is unchanged, given whether
/// the data fork is and the time `t0` the check started.
pub fn ress_unchanged<T, U>(
    info: &RessInfo<T>,
    info2: &RessInfo<U>,
    t0: Option<f64>,
    data_unchanged: bool,
) -> bool {
    match (info, info2) {
        (RessInfo::NoRess, RessInfo::NoRess) => true,
        (RessInfo::HfsRess(len), RessInfo::HfsRess(len2)) => data_unchanged && len == len2,
        (
            RessInfo::AppleDoubleRess(ino, mt, ct, _, _),
            RessInfo::AppleDoubleRess(ino2, mt2, ct2, _, _),
        ) => {
            ino == ino2 && mt == mt2 && ct == ct2 && {
                if t0 != Some(*mt2) {
                    true
                } else {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    false
                }
            }
        }
        _ => false,
    }
}

/// `Osx.insertInfo`: puts the synchronised parts of `info` (as from
/// [`extract_info`]) into the full 32-byte Finder information.
pub fn insert_info(full: &mut [u8; 32], info: &[u8]) {
    let info = [info, &[0u8; 13]].concat();
    let is_file = info[0] == b'F';
    let offset = if is_file { 9 } else { 1 };
    if is_file {
        full[..8].copy_from_slice(&info[1..9]);
    }
    full[8..10].copy_from_slice(&info[offset..offset + 2]);
    full[24..26].copy_from_slice(&info[offset + 2..offset + 4]);
}

/// `Osx.setFileInfos`: sets the Finder information of `abspath`.
pub fn set_file_infos(abspath: &Fspath, finfo: &[u8]) -> Result<(), Error> {
    assert!(!finfo.is_empty(), "Osx.setFileInfos");
    let p = abspath.as_bytes();
    let unsupported = || {
        Err(Error::Transient(format!(
            "Error in setting file information:\nthe volume of {abspath} has no native Finder \
             information, and AppleDouble files are not supported yet"
        )))
    };
    match native_infos(p, false) {
        Ok((mut full, _)) => {
            insert_info(&mut full, finfo);
            native_set(p, &full).map_err(|e| e.to_transient("setting file information"))
        }
        Err(NativeError::Unsupported) => unsupported(),
        Err(NativeError::Sys(e)) if e.error.raw_os_error() == Some(93) => unsupported(),
        Err(NativeError::Sys(e)) => Err(e.to_transient("setting file information")),
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn native_set(path: &[u8], full: &[u8; 32]) -> Result<(), SysError> {
    use std::io;
    let err = |e: io::Error| SysError {
        call: "setattrlist",
        param: String::from_utf8_lossy(path).into_owned(),
        error: e,
    };
    let cpath = std::ffi::CString::new(path)
        .map_err(|_| err(io::Error::from(io::ErrorKind::InvalidInput)))?;
    let set = || {
        let mut list = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: libc::ATTR_CMN_FNDRINFO,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buf = *full;
        // SAFETY: `cpath` is NUL-terminated and `buf` holds the 32 bytes
        // passed.
        let r = unsafe {
            libc::setattrlist(
                cpath.as_ptr(),
                (&mut list as *mut libc::attrlist).cast(),
                buf.as_mut_ptr().cast(),
                32,
                0,
            )
        };
        if r == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    match set() {
        // Read-only files can't take Finder information; make them
        // writable for a moment, as osxsupport.c does.
        Err(e) if e.raw_os_error() == Some(libc::EACCES) => {
            let st = crate::system::stat(path)?;
            crate::system::chmod(path, st.perm | 0o200)?;
            let r = set();
            let _ = crate::system::chmod(path, st.perm);
            r.map_err(err)
        }
        r => r.map_err(err),
    }
}

#[cfg(not(target_os = "macos"))]
fn native_set(_path: &[u8], _full: &[u8; 32]) -> Result<(), SysError> {
    unreachable!("native_infos is unsupported off macOS")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum NativeError {
    /// `EOPNOTSUPP` or `ENOSYS` (what OCaml catches): no native Finder
    /// information here.
    Unsupported,
    Sys(SysError),
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn native_infos(path: &[u8], need_size: bool) -> Result<([u8; 32], Filesize), NativeError> {
    use std::io;

    #[repr(C, packed)]
    struct Buf {
        length: u32,
        finder_info: [u8; 32],
        rsrc_length: libc::off_t,
    }
    let fail = |e: io::Error| {
        if matches!(e.raw_os_error(), Some(libc::EOPNOTSUPP | libc::ENOSYS)) {
            NativeError::Unsupported
        } else {
            NativeError::Sys(SysError {
                call: "getattrlist",
                param: String::from_utf8_lossy(path).into_owned(),
                error: e,
            })
        }
    };
    let cpath = std::ffi::CString::new(path)
        .map_err(|_| fail(io::Error::from(io::ErrorKind::InvalidInput)))?;
    let mut list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_FNDRINFO,
        volattr: 0,
        dirattr: 0,
        fileattr: if need_size {
            libc::ATTR_FILE_RSRCLENGTH
        } else {
            0
        },
        forkattr: 0,
    };
    let mut buf = Buf {
        length: 0,
        finder_info: [0; 32],
        rsrc_length: 0,
    };
    // SAFETY: `cpath` is NUL-terminated, and `buf` is a writable buffer of
    // the size passed.
    let r = unsafe {
        libc::getattrlist(
            cpath.as_ptr(),
            (&mut list as *mut libc::attrlist).cast(),
            (&mut buf as *mut Buf).cast(),
            std::mem::size_of::<Buf>(),
            libc::FSOPT_REPORT_FULLSIZE,
        )
    };
    if r == -1 {
        return Err(fail(io::Error::last_os_error()));
    }
    let expected = if need_size {
        std::mem::size_of::<Buf>()
    } else {
        4 + 32
    };
    if buf.length as usize != expected {
        return Err(fail(io::Error::from_raw_os_error(libc::EINVAL)));
    }
    let finder_info = buf.finder_info;
    let rsrc_length = if need_size { buf.rsrc_length } else { 0 };
    Ok((finder_info, rsrc_length))
}

#[cfg(not(target_os = "macos"))]
fn native_infos(_path: &[u8], _need_size: bool) -> Result<([u8; 32], Filesize), NativeError> {
    Err(NativeError::Unsupported)
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
