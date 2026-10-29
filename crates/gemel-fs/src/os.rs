//! Directory listing, deletion, renaming and temp-file handling.
//!
//! Ports Unison `src/os.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use gemel_base::name::Name;
use gemel_base::path::{Local, Path};
use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};
use md5::{Digest, Md5};

use crate::fileinfo::{self, FsConfig, Typ};
use crate::fingerprint::Fingerprint;
use crate::fs;
use crate::fspath::Fspath;
use crate::osx::{self, InfoKind};
use crate::system;

// ---- Host names ----

/// The host name: `UNISONLOCALHOSTNAME`, or the system's (`gethostname`).
pub fn local_canonical_host_name() -> String {
    std::env::var("UNISONLOCALHOSTNAME").unwrap_or_else(|_| system_host_name())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn system_host_name() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer and its length are valid; gethostname
    // NUL-terminates within it on success.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn system_host_name() -> String {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` has room for `len` UTF-16 units.
    if unsafe { GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut len) } == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// `clientHostName`: the host name archives are found by.
pub static CLIENT_HOST_NAME: PrefDef<String> = PrefDef::new(
    "clientHostName",
    Category::Advanced(Topic::Remote),
    local_canonical_host_name,
    "set host name of client",
    "When specified, the host name of the client will not be guessed and the provided host \
     name will be used to find the archive.",
);

/// Registers this module's preferences.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&CLIENT_HOST_NAME)
}

// ---- Temporary files ----

/// Prefix of Unison's temporary files.
pub const TEMP_FILE_PREFIX: &str = ".unison.";
/// Suffix of Unison's temporary files.
pub const TEMP_FILE_SUFFIX_FIXED: &str = ".unison.tmp";

/// Temporary file names longer than this are shortened (eCryptfs allows
/// 143 bytes, the lowest limit known).
const MAX_FILE_NAME_LENGTH: usize = 143;

/// The temporary-file suffix in use (`Os.tempFileSuffix`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempNames {
    suffix: String,
}

impl Default for TempNames {
    fn default() -> Self {
        TempNames {
            suffix: TEMP_FILE_SUFFIX_FIXED.to_owned(),
        }
    }
}

impl TempNames {
    /// `Os.includeInTempNames`: puts `s` (a host or process tag) into the
    /// suffix, so that concurrent runs don't collide.
    pub fn include(&mut self, s: &str) {
        assert!(!s.is_empty(), "Os.includeInTempNames");
        self.suffix = format!(".{s}{TEMP_FILE_SUFFIX_FIXED}");
    }

    /// The current suffix.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }
}

/// `Os.isTempFile`.
pub fn is_temp_file(file: &[u8]) -> bool {
    file.ends_with(TEMP_FILE_SUFFIX_FIXED.as_bytes())
        && file.starts_with(TEMP_FILE_PREFIX.as_bytes())
}

/// `Os.truncate_filename`: at most `l` bytes, without splitting a UTF-8
/// character.
fn truncate_filename(s: &[u8], mut l: usize) -> &[u8] {
    while l > 0 && s[l] & 0xC0 == 0x80 {
        l -= 1;
    }
    &s[..l]
}

/// `Os.genTempPath`: a temporary path next to `path`, unused if `fresh`.
pub fn gen_temp_path(
    fresh: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    prefix: &str,
    suffix: &str,
    c: &FsConfig,
) -> Result<Path<Local>, Error> {
    let (name, parent) = path.deconstruct_rev().expect("Os.genTempPath on the root");
    let name = name.as_bytes();
    for i in 0.. {
        let s = if i == 0 {
            suffix.to_owned()
        } else {
            format!("..{i:03}.{suffix}")
        };
        let max_len = MAX_FILE_NAME_LENGTH - prefix.len() - s.len();
        let short;
        let name = if name.len() <= max_len {
            name
        } else {
            let digest: String = Md5::digest(name)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let max_len = max_len - digest.len();
            assert!(max_len > 0);
            short = [truncate_filename(name, max_len), digest.as_bytes()].concat();
            &short[..]
        };
        let temp = parent.child(&Name::from_bytes(
            [prefix.as_bytes(), name, s.as_bytes()].concat(),
        )?);
        if !fresh || !exists(fspath, &temp, c)? {
            return Ok(temp);
        }
    }
    unreachable!()
}

/// `Os.tempPath`.
pub fn temp_path(
    fresh: bool,
    fspath: &Fspath,
    path: &Path<Local>,
    names: &TempNames,
    c: &FsConfig,
) -> Result<Path<Local>, Error> {
    gen_temp_path(fresh, fspath, path, TEMP_FILE_PREFIX, names.suffix(), c)
}

// ---- Querying the filesystem ----

/// `Os.exists`.
pub fn exists(fspath: &Fspath, path: &Path<Local>, c: &FsConfig) -> Result<bool, Error> {
    Ok(fileinfo::get_type(false, fspath, path, &|_| false, c)? != Typ::Absent)
}

/// `Os.readLink`: with `/` separators on Windows.
pub fn read_link(fspath: &Fspath, path: &Path<Local>) -> Result<Vec<u8>, Error> {
    let l = system::SysError::convert("reading symbolic link", fs::readlink(&fspath.concat(path)))?;
    Ok(if cfg!(windows) {
        l.iter()
            .map(|&b| if b == b'\\' { b'/' } else { b })
            .collect()
    } else {
        l
    })
}

/// `Os.isAppleDoubleFile`: `._name` files, when resource forks are synced.
pub fn is_apple_double_file(file: &[u8], rsrc: bool) -> bool {
    rsrc && file.len() > 2 && file.starts_with(b"._")
}

/// `Os.allChildrenOf`: a directory's entries; none if it is gone.
pub fn all_children_of(fspath: &Fspath, path: &Path<Local>) -> Result<Vec<Name>, Error> {
    match fs::read_dir(&fspath.concat(path)) {
        Ok(names) => names.into_iter().map(Name::from_bytes).collect(),
        // Windows reports an empty directory as missing to FindFirstFile.
        Err(e) if e.error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_transient("scanning directory")),
    }
}

/// `Os.childrenOf`: a directory's entries without AppleDouble and temporary
/// files. Temporary files with this run's suffix older than 30 days are
/// deleted.
pub fn children_of(
    fspath: &Fspath,
    path: &Path<Local>,
    names: &TempNames,
    c: &FsConfig,
) -> Result<Vec<Name>, Error> {
    let mut out = Vec::new();
    for name in all_children_of(fspath, path)? {
        let file = name.as_bytes();
        if is_apple_double_file(file, c.props.rsrc) {
            continue;
        }
        if is_temp_file(file) {
            if file.ends_with(names.suffix().as_bytes()) {
                let p = path.child(&name);
                let info = fileinfo::get_basic(false, fspath, &p, &|_| false, c)?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64());
                if info.desc.time.extract() + 2_592_000.0 < now {
                    delete(fspath, &p, c)?;
                }
            }
            continue;
        }
        out.push(name);
    }
    Ok(out)
}

// ---- Actions ----

/// `Os.delete`: deletes a file, link or directory tree, if there is one.
pub fn delete(fspath: &Fspath, path: &Path<Local>, c: &FsConfig) -> Result<(), Error> {
    let what = "deleting";
    let abs = fspath.concat(path);
    match fileinfo::get_type(false, fspath, path, &|_| false, c)? {
        Typ::Directory => {
            let _ = fs::chmod(&abs, 0o700);
            for child in all_children_of(fspath, path)? {
                delete(fspath, &path.child(&child), c)?;
            }
            system::SysError::convert(what, fs::rmdir(&abs))
        }
        Typ::File => {
            if !cfg!(unix) {
                let _ = fs::chmod(&abs, 0o600);
            }
            system::SysError::convert(what, fs::unlink(&abs))?;
            if c.props.rsrc {
                let double = abs.apple_double();
                if system::SysError::convert(what, fs::file_exists(&double))? {
                    system::SysError::convert(what, fs::unlink(&double))?;
                }
            }
            Ok(())
        }
        // chmod would follow the link.
        Typ::Symlink => system::SysError::convert(what, fs::unlink(&abs)),
        Typ::Absent => Ok(()),
    }
}

/// `Os.rename`: renames, moving the AppleDouble file along. `exdev` handles
/// a rename across filesystems (the caller copies instead).
pub fn rename(
    fname: &str,
    src_fspath: &Fspath,
    src: &Path<Local>,
    dst_fspath: &Fspath,
    dst: &Path<Local>,
    exdev: Option<&mut dyn FnMut() -> Result<(), Error>>,
    c: &FsConfig,
) -> Result<(), Error> {
    let source = src_fspath.concat(src);
    let target = dst_fspath.concat(dst);
    if source == target {
        return Err(Error::Transient(format!(
            "Rename ({fname}): identical source and target {source}"
        )));
    }
    let what = format!("renaming {source} to {target}");
    match fs::rename(&source, &target) {
        Ok(()) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::CrossesDevices && exdev.is_some() => {
            (exdev.expect("checked"))()?;
        }
        Err(e) => return Err(e.to_transient(&what)),
    }
    if c.props.rsrc {
        let (sd, td) = (source.apple_double(), target.apple_double());
        if system::SysError::convert(&what, fs::file_exists(&sd))? {
            system::SysError::convert(&what, fs::rename(&sd, &td))?;
        } else if system::SysError::convert(&what, fs::file_exists(&td))? {
            system::SysError::convert(&what, fs::unlink(&td))?;
        }
    }
    Ok(())
}

/// `Os.symlink`.
pub fn symlink(fspath: &Fspath, path: &Path<Local>, target: &[u8]) -> Result<(), Error> {
    let abs = fspath.concat(path);
    fs::symlink(target, &abs).map_err(|e| {
        if fs::has_symlink() {
            e.to_transient("writing symbolic link")
        } else {
            Error::Transient(format!(
                "Cannot create symlink \"{abs}\": symlinks are not supported on this system{}",
                if cfg!(windows) {
                    " or elevated privileges may be required"
                } else {
                    ""
                }
            ))
        }
    })
}

/// `Os.createDir`.
pub fn create_dir(fspath: &Fspath, path: &Path<Local>, perm: u32) -> Result<(), Error> {
    system::SysError::convert("creating directory", fs::mkdir(&fspath.concat(path), perm))
}

// ---- Fingerprints ----

/// `Os.fingerprint`: the data and resource-fork fingerprints.
pub fn fingerprint(
    fspath: &Fspath,
    path: &Path<Local>,
    kind: InfoKind,
    rsrc: bool,
) -> Result<FullFingerprint, Error> {
    Ok(FullFingerprint {
        data: Fingerprint::file(fspath, path)?,
        ress: osx::ress_fingerprint(fspath, path, kind, rsrc)?,
    })
}

/// `Os.safeFingerprint`: fingerprints a file, retrying (up to 10 times)
/// until it doesn't change while being read. Returns the file's info as of
/// the fingerprint.
pub fn safe_fingerprint(
    fspath: &Fspath,
    path: &Path<Local>,
    info: &fileinfo::Info,
    known: Option<Fingerprint>,
    follow: &dyn Fn(&Path<Local>) -> bool,
    c: &FsConfig,
    x: &crate::props::XattrCtx<'_>,
) -> Result<(fileinfo::Info, FullFingerprint), Error> {
    let kind = match info.typ {
        Typ::File => InfoKind::File,
        Typ::Directory => InfoKind::Dir,
        _ => InfoKind::Other,
    };
    let (mut info, mut fp, mut ress) = (info.clone(), known, None);
    for _ in 0..10 {
        let data = match fp.take() {
            Some(d) => d,
            None => Fingerprint::file(fspath, path)?,
        };
        let r = match ress.take() {
            Some(r) => r,
            None => osx::ress_fingerprint(fspath, path, kind, c.props.rsrc)?,
        };
        let (info2, data_unchanged, ress_unchanged) =
            fileinfo::unchanged(fspath, path, &info, follow, c, x)?;
        if data_unchanged && ress_unchanged {
            return Ok((info2, FullFingerprint { data, ress: r }));
        }
        fp = data_unchanged.then_some(data);
        ress = ress_unchanged.then_some(r);
        info = info2;
    }
    Err(Error::Transient(format!(
        "Failed to fingerprint file \"{}\": the file keeps on changing",
        fspath.concat(path)
    )))
}

// ---- The Unison directory ----

/// `Util.homeDir`: `HOME` on Unix (the current directory if unset, where
/// OCaml's fails), `USERPROFILE` (then `UNISON`, then `c:/`) on Windows.
pub fn home_dir() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("UNISON"))
            .map_or_else(|| "c:/".into(), Into::into)
    } else {
        std::env::var_os("HOME").map_or_else(|| ".".into(), Into::into)
    }
}

/// `Util.unisonDir`: `UNISON`, else `~/.unison`, else (on macOS, if that
/// doesn't exist) `~/Library/Application Support/Unison`.
pub fn unison_dir() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("UNISON") {
        return d.into();
    }
    let generic = home_dir().join(".unison");
    if cfg!(target_os = "macos") && !generic.exists() {
        home_dir().join("Library/Application Support/Unison")
    } else {
        generic
    }
}

/// `Os.createUnisonDir`: creates the Unison directory if needed.
pub fn create_unison_dir(dir: &std::path::Path) -> Result<(), Error> {
    if std::fs::metadata(dir).is_ok() {
        return Ok(());
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir).map_err(|e| {
        system::SysError {
            call: "mkdir",
            param: dir.display().to_string(),
            error: e,
        }
        .to_fatal(&format!("creating unison directory {}", dir.display()))
    })
}

/// `Os.fullfingerprint`: the fingerprints of the data and of the resource
/// fork.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FullFingerprint {
    /// The file's contents.
    pub data: Fingerprint,
    /// The resource fork (dummy where there is none).
    pub ress: Fingerprint,
}

impl FullFingerprint {
    /// `Os.fullfingerprint_dummy`.
    pub const DUMMY: FullFingerprint = FullFingerprint {
        data: Fingerprint::DUMMY,
        ress: Fingerprint::DUMMY,
    };

    /// `Os.pseudoFingerprint`.
    pub fn pseudo(path: &[u8], size: i64) -> FullFingerprint {
        FullFingerprint {
            data: Fingerprint::pseudo(path, size),
            ress: Fingerprint::DUMMY,
        }
    }

    /// `Os.isPseudoFingerprint`.
    pub fn is_pseudo(&self) -> bool {
        self.data.is_pseudo()
    }

    /// `Os.fullfingerprintHash` (for hash tables).
    pub fn hash(&self) -> i64 {
        self.data.hash() + 31 * self.ress.hash()
    }

    /// `Os.reasonForFingerprintMismatch`.
    pub fn reason_for_mismatch(&self, other: &FullFingerprint) -> &'static str {
        if self.data == other.data {
            "resource fork"
        } else if self.ress == other.ress {
            "file contents"
        } else {
            "both file contents and resource fork"
        }
    }

    /// `Os.mfullfingerprint`.
    pub fn marshal(&self, w: &mut Writer) {
        self.data.marshal(w);
        self.ress.marshal(w);
    }

    /// `Os.mfullfingerprint`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<FullFingerprint> {
        Ok(FullFingerprint {
            data: Fingerprint::unmarshal(r)?,
            ress: Fingerprint::unmarshal(r)?,
        })
    }
}

/// `Os.fullfingerprint_to_string`.
impl std::fmt::Display for FullFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({},{})", self.data, self.ress)
    }
}
