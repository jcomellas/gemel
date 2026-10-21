//! Thin portable wrapper over OS calls.
//!
//! Ports Unison `src/system.ml`, Unison `src/system/system_intf.ml`, Unison `src/system/system_generic.ml`, Unison `src/system/system_win.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Paths here are the byte strings Unison uses (`System.fspath`): native
//! bytes on Unix, UTF-8 on Windows (OCaml's runtime converts to and from
//! UTF-16, replacing what doesn't convert). [`Stats`] reproduces what
//! OCaml's `Unix.LargeFile.stat` returns, including Unison's own `win_stat`
//! on Windows (whole-second times, emulated permission bits, an inode
//! number derived from the file index).
//!
//! Errors carry the call and its argument ([`SysError`]), so that
//! [`SysError::convert`] can produce Unison's messages.

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use gemel_base::util::Error;

/// A failed system call (`Unix.Unix_error`).
#[derive(Debug)]
pub struct SysError {
    /// The call, as OCaml names it (`"lstat"`, `"rename"`, ...).
    pub call: &'static str,
    /// Its argument.
    pub param: String,
    /// What happened.
    pub error: io::Error,
}

impl SysError {
    /// The error `error` of `call` on `path`.
    pub fn new(call: &'static str, path: &[u8], error: io::Error) -> Self {
        SysError {
            call,
            param: String::from_utf8_lossy(path).into_owned(),
            error,
        }
    }

    /// The OS's description of the error (`Unix.error_message`).
    pub fn message(&self) -> String {
        let s = self.error.to_string();
        match s.rfind(" (os error ") {
            Some(i) => s[..i].to_owned(),
            None => s,
        }
    }

    /// Whether the path (or a parent) doesn't exist (`ENOENT`/`ENOTDIR`).
    pub fn is_absent(&self) -> bool {
        self.error.kind() == io::ErrorKind::NotFound
            || self.error.kind() == io::ErrorKind::NotADirectory
            || self.error.raw_os_error() == Some(not_dir_errno())
    }

    /// `Util.convertUnixErrorsToTransient m`: `"Error in m:\n<message>
    /// [<call>(<param>)]"`.
    pub fn to_transient(&self, what: &str) -> Error {
        Error::Transient(self.text(what))
    }

    /// Like [`SysError::to_transient`], for errors OCaml raises as
    /// `Sys_error` (from channels: `open_in`, `input`, ...):
    /// `"Error in m:\n<param>: <message>"`.
    pub fn to_transient_sys(&self, what: &str) -> Error {
        Error::Transient(format!(
            "Error in {what}:\n{}: {}",
            self.param,
            self.message()
        ))
    }

    /// `Util.convertUnixErrorsToFatal m`.
    pub fn to_fatal(&self, what: &str) -> Error {
        Error::Fatal(self.text(what))
    }

    fn text(&self, what: &str) -> String {
        format!(
            "Error in {what}:\n{} [{}({})]",
            self.message(),
            self.call,
            self.param
        )
    }

    /// Converts a result for `what`, as `convertUnixErrorsToTransient`.
    pub fn convert<T>(what: &str, r: Result<T, SysError>) -> Result<T, Error> {
        r.map_err(|e| e.to_transient(what))
    }
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}({})]", self.message(), self.call, self.param)
    }
}

impl std::error::Error for SysError {}

#[cfg(unix)]
fn not_dir_errno() -> i32 {
    libc::ENOTDIR
}

#[cfg(not(unix))]
fn not_dir_errno() -> i32 {
    // ERROR_DIRECTORY: "The directory name is invalid."
    267
}

/// Result of a system call.
pub type SysResult<T> = Result<T, SysError>;

/// Converts Unison's path bytes to an OS path. On Windows, paths with a
/// drive letter or a UNC prefix get the `\\?\` prefix that lifts the
/// length limit (`System.extendedPath`).
pub fn os_path(p: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(p))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(extended_path(&String::from_utf8_lossy(p)))
    }
}

/// The long form of a path, without 8.3 short names (`GetLongPathNameW`,
/// as `Lwt_win.longpathname` does for the file watcher); `None` if there
/// is none, as when the path is gone.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn long_path_name(p: &std::path::Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = vec![0u16; 260];
    loop {
        // SAFETY: `wide` is NUL-terminated, and `buf` is as long as said.
        let n =
            unsafe { GetLongPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            buf.truncate(n);
            return Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
        }
        buf.resize(n, 0);
    }
}

/// `System.extendedPath` (Windows): `C:/x` becomes `\\?\C:\x`,
/// `//host/share/x` becomes `\\?\UNC\host\share\x`, and `//?/...` paths
/// just get backslashes.
pub fn extended_path(f: &str) -> String {
    let fix = |s: &str| s.replace('/', "\\");
    let b = f.as_bytes();
    let sep = |c: u8| c == b'/' || c == b'\\';
    let win_root = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && sep(b[2]);
    let unc = || {
        // [/\][/\][^?/\]+[/\][^/\]+[/\].*
        if b.len() < 2 || !sep(b[0]) || !sep(b[1]) {
            return false;
        }
        let rest = &b[2..];
        let host = rest.iter().take_while(|&&c| !(c == b'?' || sep(c))).count();
        if host == 0 || host >= rest.len() || !sep(rest[host]) {
            return false;
        }
        let rest = &rest[host + 1..];
        let share = rest.iter().take_while(|&&c| !sep(c)).count();
        share > 0 && share < rest.len()
    };
    if win_root {
        fix(&format!("\\\\?\\{f}"))
    } else if unc() {
        fix(&format!("\\\\?\\UNC{}", &f[1..]))
    } else if f.len() > 4 && f.starts_with("//?/") {
        fix(f)
    } else {
        f.to_owned()
    }
}

/// Converts an OS file name back to Unison's bytes.
pub fn name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        name.to_string_lossy().into_owned().into_bytes()
    }
}

/// The kind of a file (`Unix.file_kind`, as far as Unison cares).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Symlink,
    /// Anything else (device, pipe, socket).
    Other,
}

/// What `stat` returns (`Unix.LargeFile.stats`), in OCaml's units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    /// The file's kind.
    pub kind: Kind,
    /// The permission bits (`st_perm`, `mode & 0o7777`).
    pub perm: i64,
    /// The inode number.
    pub ino: i64,
    /// The device.
    pub dev: i64,
    /// The owner (0 on Windows).
    pub uid: i64,
    /// The group (0 on Windows).
    pub gid: i64,
    /// The size in bytes.
    pub size: i64,
    /// The modification time, in seconds.
    pub mtime: f64,
    /// The status change time, in seconds.
    pub ctime: f64,
}

/// `sec + nsec / 1e9` as OCaml's Unix library computes it, never rounding
/// up to the next second.
///
/// On macOS, OCaml's runtime (as built for Homebrew's unison, the reference
/// there) has no sub-second `stat` times, so neither does gemel: otherwise
/// each would see the other's recorded times as changes and rescan.
#[cfg(unix)]
fn timestamp(sec: i64, nsec: i64) -> f64 {
    let nsec = if cfg!(target_os = "macos") { 0 } else { nsec };
    let s = sec as f64;
    let t = s + nsec as f64 / 1e9;
    if t == s + 1.0 {
        // nextafter(t, s)
        if t > 0.0 {
            f64::from_bits(t.to_bits() - 1)
        } else {
            f64::from_bits(t.to_bits() + 1)
        }
    } else {
        t
    }
}

#[cfg(unix)]
fn stats_of(m: &fs::Metadata) -> Stats {
    use std::os::unix::fs::MetadataExt;
    let ft = m.file_type();
    let kind = if ft.is_file() {
        Kind::File
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    };
    Stats {
        kind,
        perm: i64::from(m.mode() & 0o7777),
        ino: m.ino() as i64,
        dev: m.dev() as i64,
        uid: i64::from(m.uid()),
        gid: i64::from(m.gid()),
        size: m.size() as i64,
        mtime: timestamp(m.mtime(), m.mtime_nsec()),
        ctime: timestamp(m.ctime(), m.ctime_nsec()),
    }
}

/// `System.stat`: follows symbolic links.
pub fn stat(p: &[u8]) -> SysResult<Stats> {
    stat_impl(p, false)
}

/// `System.lstat`: doesn't follow symbolic links.
pub fn lstat(p: &[u8]) -> SysResult<Stats> {
    stat_impl(p, true)
}

#[cfg(unix)]
fn stat_impl(p: &[u8], link: bool) -> SysResult<Stats> {
    let path = os_path(p);
    let m = if link {
        fs::symlink_metadata(&path)
    } else {
        fs::metadata(&path)
    };
    m.map(|m| stats_of(&m))
        .map_err(|e| SysError::new(if link { "lstat" } else { "stat" }, p, e))
}

#[cfg(windows)]
fn stat_impl(p: &[u8], link: bool) -> SysResult<Stats> {
    windows::stat(p, link).map_err(|e| SysError::new(if link { "lstat" } else { "stat" }, p, e))
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    //! Unison's `win_stat` (system_win_stubs.c).

    use std::fs::OpenOptions;
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    };

    use super::{Kind, Stats, os_path};

    /// Seconds since the epoch, truncated (`WINTIME_TO_TIME`).
    fn wintime(t: i64) -> f64 {
        ((t as u64).wrapping_sub(116_444_736_000_000_000) / 10_000_000) as f64
    }

    pub fn stat(p: &[u8], link: bool) -> io::Result<Stats> {
        let mut opts = OpenOptions::new();
        opts.access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(
                FILE_FLAG_BACKUP_SEMANTICS
                    | if link {
                        FILE_FLAG_OPEN_REPARSE_POINT
                    } else {
                        0
                    },
            );
        let f = opts.open(os_path(p))?;
        let h = f.as_raw_handle() as HANDLE;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(h, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
        let have_basic = unsafe {
            GetFileInformationByHandleEx(
                h,
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        } != 0;
        let symlink = link
            && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            && std::fs::symlink_metadata(os_path(p))?
                .file_type()
                .is_symlink();
        if link && !symlink {
            drop(f);
            return stat(p, false);
        }
        let high = u64::from(info.nFileIndexHigh);
        let index = (high << 32) | u64::from(info.nFileIndexLow);
        let ino = index.wrapping_add(155_825_701u64.wrapping_mul(high)) as i64;
        let attrs = if have_basic {
            basic.FileAttributes
        } else {
            info.dwFileAttributes
        };
        let dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
        let mut mode = 0o444;
        if attrs & FILE_ATTRIBUTE_READONLY == 0 {
            mode |= 0o222;
        }
        if dir {
            mode |= 0o111;
        }
        let kind = if symlink {
            mode |= 0o111 | 0o444;
            Kind::Symlink
        } else if dir {
            Kind::Dir
        } else {
            Kind::File
        };
        let size = if symlink {
            // win_stat gives the target's length in characters; nothing in
            // Unison uses a link's size.
            0
        } else {
            ((u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow)) as i64
        };
        let (mtime, ctime) = if have_basic {
            let ctime = if basic.ChangeTime != 0 {
                basic.ChangeTime
            } else {
                basic.CreationTime
            };
            (wintime(basic.LastWriteTime), wintime(ctime))
        } else {
            let ft = |t: windows_sys::Win32::Foundation::FILETIME| {
                ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as i64
            };
            (
                wintime(ft(info.ftLastWriteTime)),
                wintime(ft(info.ftCreationTime)),
            )
        };
        Ok(Stats {
            kind,
            perm: mode,
            ino,
            dev: i64::from(info.dwVolumeSerialNumber),
            uid: 0,
            gid: 0,
            size,
            mtime,
            ctime,
        })
    }
}

/// `System.readdir` over a whole directory: the entries' names, without
/// `.` and `..`, in the order the OS returns them.
pub fn read_dir(p: &[u8]) -> SysResult<Vec<Vec<u8>>> {
    let err = |e| SysError::new("opendir", p, e);
    fs::read_dir(os_path(p))
        .map_err(err)?
        .map(|e| e.map(|e| name_bytes(&e.file_name())).map_err(err))
        .collect()
}

/// `System.mkdir`.
pub fn mkdir(p: &[u8], perm: u32) -> SysResult<()> {
    #[cfg(unix)]
    let b = {
        let mut b = fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut b, perm);
        b
    };
    #[cfg(not(unix))]
    let b = {
        let _ = perm;
        fs::DirBuilder::new()
    };
    b.create(os_path(p))
        .map_err(|e| SysError::new("mkdir", p, e))
}

/// `System.rmdir`.
pub fn rmdir(p: &[u8]) -> SysResult<()> {
    fs::remove_dir(os_path(p)).map_err(|e| SysError::new("rmdir", p, e))
}

/// `System.unlink`.
pub fn unlink(p: &[u8]) -> SysResult<()> {
    fs::remove_file(os_path(p)).map_err(|e| SysError::new("unlink", p, e))
}

/// `System.link`: a hard link.
pub fn link(from: &[u8], to: &[u8]) -> SysResult<()> {
    fs::hard_link(os_path(from), os_path(to)).map_err(|e| SysError::new("link", to, e))
}

/// `System.openfile p [O_WRONLY; O_CREAT; O_EXCL] perm`, closing the file
/// at once: creates `p`, failing if it exists.
pub fn create_exclusive(p: &[u8], perm: u32) -> SysResult<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, perm);
    #[cfg(not(unix))]
    let _ = perm;
    opts.open(os_path(p))
        .map(drop)
        .map_err(|e| SysError::new("open", p, e))
}

/// `open_in_bin p`: opens `p` to read. (OCaml raises `Sys_error`; see
/// [`SysError::to_transient_sys`].)
pub fn open_in_bin(p: &[u8]) -> SysResult<fs::File> {
    fs::File::open(os_path(p)).map_err(|e| SysError::new("open", p, e))
}

/// `Copy.openFileOut'` for a data fork: creates `p` with `perm` to write
/// it, failing if it exists. On Unix, where `O_EXCL` isn't supported
/// (some NFS mounts), it truncates instead.
pub fn open_out_new(p: &[u8], perm: u32) -> SysResult<fs::File> {
    let open = |exclusive: bool| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true);
        if exclusive {
            opts.create_new(true);
        } else {
            opts.create(true).truncate(true);
        }
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, perm);
        #[cfg(not(unix))]
        let _ = perm;
        opts.open(os_path(p))
    };
    match open(true) {
        #[cfg(unix)]
        Err(e) if matches!(e.raw_os_error(), Some(libc::EOPNOTSUPP | 524)) => open(false),
        r => r,
    }
    .map_err(|e| SysError::new("open", p, e))
}

/// The number of hard links to `p` (`st_nlink`).
#[cfg(unix)]
pub fn link_count(p: &[u8]) -> SysResult<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(os_path(p))
        .map(|m| m.nlink())
        .map_err(|e| SysError::new("stat", p, e))
}

/// `System.rename`. On Windows, like Unison, a read-only target is made
/// writable to be replaced, and sharing violations are retried for about a
/// second (an indexer or a virus scanner may hold the file).
pub fn rename(from: &[u8], to: &[u8]) -> SysResult<()> {
    let attempt = || fs::rename(os_path(from), os_path(to));
    #[cfg(windows)]
    let attempt = || {
        let r = attempt();
        match &r {
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(80) =>
            {
                let Ok(st) = lstat(to) else { return r };
                if st.perm & 0o200 != 0 || !matches!(st.kind, Kind::File | Kind::Symlink) {
                    return r;
                }
                if chmod(to, st.perm | 0o200).is_err() {
                    return r;
                }
                let r2 = fs::rename(os_path(from), os_path(to));
                if r2.is_err() {
                    let _ = chmod(to, st.perm);
                }
                r2
            }
            _ => r,
        }
    };
    let mut delay = 0.01;
    loop {
        match attempt() {
            Err(e)
                if cfg!(windows)
                    && delay < 1.0
                    && (e.kind() == io::ErrorKind::PermissionDenied
                        || e.raw_os_error() == Some(32)) =>
            {
                std::thread::sleep(std::time::Duration::from_secs_f64(delay));
                delay *= 2.0;
            }
            r => return r.map_err(|e| SysError::new("rename", from, e)),
        }
    }
}

/// `System.chmod`.
pub fn chmod(p: &[u8], perm: i64) -> SysResult<()> {
    let path = os_path(p);
    #[cfg(unix)]
    let perms = {
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(perm as u32)
    };
    #[cfg(not(unix))]
    let perms = {
        // The OCaml runtime's chmod on Windows only toggles read-only.
        let mut perms = fs::metadata(&path)
            .map_err(|e| SysError::new("chmod", p, e))?
            .permissions();
        perms.set_readonly(perm & 0o200 == 0);
        perms
    };
    fs::set_permissions(path, perms).map_err(|e| SysError::new("chmod", p, e))
}

/// `System.chown` (`ENOSYS` on Windows); `-1` leaves an id unchanged.
pub fn chown(p: &[u8], uid: i64, gid: i64) -> SysResult<()> {
    #[cfg(unix)]
    {
        let id = |i: i64| u32::try_from(i).ok();
        std::os::unix::fs::chown(os_path(p), id(uid), id(gid))
            .map_err(|e| SysError::new("chown", p, e))
    }
    #[cfg(not(unix))]
    {
        let _ = (uid, gid);
        Err(SysError::new(
            "chown",
            p,
            io::Error::from(io::ErrorKind::Unsupported),
        ))
    }
}

/// `System.utimes`: sets the access and modification times (seconds).
pub fn utimes(p: &[u8], atime: f64, mtime: f64) -> SysResult<()> {
    utimes_impl(p, atime, mtime).map_err(|e| SysError::new("utimes", p, e))
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn utimes_impl(p: &[u8], atime: f64, mtime: f64) -> io::Result<()> {
    let ts = |t: f64| {
        let sec = t.floor();
        libc::timespec {
            tv_sec: sec as libc::time_t,
            tv_nsec: ((t - sec) * 1e9) as _,
        }
    };
    let path =
        std::ffi::CString::new(p).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let times = [ts(atime), ts(mtime)];
    // SAFETY: `path` is a NUL-terminated string and `times` has two entries.
    let r = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn utimes_impl(p: &[u8], atime: f64, mtime: f64) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let t = |s: f64| {
        if s >= 0.0 {
            std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(s)
        } else {
            std::time::UNIX_EPOCH - std::time::Duration::from_secs_f64(-s)
        }
    };
    // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS for directories.
    let f = fs::OpenOptions::new()
        .access_mode(0x100)
        .custom_flags(0x0200_0000)
        .open(os_path(p))?;
    f.set_times(
        fs::FileTimes::new()
            .set_accessed(t(atime))
            .set_modified(t(mtime)),
    )
}

/// `System.symlink`: creates `p` pointing at `target`.
pub fn symlink(target: &[u8], p: &[u8]) -> SysResult<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(os_path(target), os_path(p))
            .map_err(|e| SysError::new("symlink", p, e))
    }
    #[cfg(windows)]
    {
        let t = PathBuf::from(String::from_utf8_lossy(target).replace('/', "\\"));
        let dir = fs::metadata(
            os_path(p)
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(&t),
        )
        .is_ok_and(|m| m.is_dir());
        let r = if dir {
            std::os::windows::fs::symlink_dir(&t, os_path(p))
        } else {
            std::os::windows::fs::symlink_file(&t, os_path(p))
        };
        r.map_err(|e| SysError::new("symlink", p, e))
    }
}

/// `System.readlink`.
pub fn readlink(p: &[u8]) -> SysResult<Vec<u8>> {
    fs::read_link(os_path(p))
        .map(|t| name_bytes(t.as_os_str()))
        .map_err(|e| SysError::new("readlink", p, e))
}

/// `System.hasInodeNumbers`: true everywhere (Windows has best-effort
/// numbers from the file index).
pub fn has_inode_numbers() -> bool {
    true
}

/// `System.hasSymlink`.
pub fn has_symlink() -> bool {
    true
}

/// `System.hasCorrectCTime`: whether `ctime` is the status change time.
pub fn has_correct_ctime() -> bool {
    true
}

/// The name of a user or group id, if it has one (`getpwuid`/`getgrgid`).
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn id_name(id: i64, user: bool) -> Option<Vec<u8>> {
    let id = u32::try_from(id).ok()?;
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: the out-pointers are valid for the duration of the call,
        // and `buf` is the scratch space of the size passed.
        let (r, name) = unsafe {
            if user {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out: *mut libc::passwd = std::ptr::null_mut();
                let r = libc::getpwuid_r(id, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out);
                (
                    r,
                    (!out.is_null())
                        .then(|| std::ffi::CStr::from_ptr(pw.pw_name).to_bytes().to_vec()),
                )
            } else {
                let mut gr: libc::group = std::mem::zeroed();
                let mut out: *mut libc::group = std::ptr::null_mut();
                let r = libc::getgrgid_r(id, &mut gr, buf.as_mut_ptr().cast(), buf.len(), &mut out);
                (
                    r,
                    (!out.is_null())
                        .then(|| std::ffi::CStr::from_ptr(gr.gr_name).to_bytes().to_vec()),
                )
            }
        };
        if r == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return name;
    }
}

/// The id of a user or group name (`getpwnam`/`getgrnam`).
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn id_of_name(name: &[u8], user: bool) -> Option<i64> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: as in `id_name`.
        let (r, id) = unsafe {
            if user {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out: *mut libc::passwd = std::ptr::null_mut();
                let r = libc::getpwnam_r(
                    cname.as_ptr(),
                    &mut pw,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    &mut out,
                );
                (r, (!out.is_null()).then_some(i64::from(pw.pw_uid)))
            } else {
                let mut gr: libc::group = std::mem::zeroed();
                let mut out: *mut libc::group = std::ptr::null_mut();
                let r = libc::getgrnam_r(
                    cname.as_ptr(),
                    &mut gr,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    &mut out,
                );
                (r, (!out.is_null()).then_some(i64::from(gr.gr_gid)))
            }
        };
        if r == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return id;
    }
}

/// No user or group names on Windows.
#[cfg(not(unix))]
pub fn id_name(_id: i64, _user: bool) -> Option<Vec<u8>> {
    None
}

/// No user or group names on Windows.
#[cfg(not(unix))]
pub fn id_of_name(_name: &[u8], _user: bool) -> Option<i64> {
    None
}

/// `Sys.file_exists`.
pub fn file_exists(p: &[u8]) -> bool {
    stat(p).is_ok()
}

/// A local date and time (the fields of `Unix.tm` that Unison prints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    /// The year, e.g. 2026.
    pub year: i32,
    /// The month, 1 to 12.
    pub month: u32,
    /// The day of the month, 1 to 31.
    pub day: u32,
    /// Hours, minutes and seconds.
    pub hour: u32,
    /// Minutes.
    pub min: u32,
    /// Seconds.
    pub sec: u32,
}

/// `Unix.localtime`: the local time of `t` (seconds since the epoch), or
/// `None` where the system can't represent it.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn localtime(t: f64) -> Option<LocalTime> {
    // OCaml converts the float to time_t by truncation.
    let secs = t as libc::time_t;
    // SAFETY: an all-zero `tm` is valid, and localtime_r only writes it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    let field = |v: libc::c_int| u32::try_from(v).ok();
    Some(LocalTime {
        year: tm.tm_year + 1900,
        month: field(tm.tm_mon)? + 1,
        day: field(tm.tm_mday)?,
        hour: field(tm.tm_hour)?,
        min: field(tm.tm_min)?,
        sec: field(tm.tm_sec)?,
    })
}

/// `Unix.localtime`: the local time of `t` (seconds since the epoch), or
/// `None` where the system can't represent it.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn localtime(t: f64) -> Option<LocalTime> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // 100 ns intervals since 1601.
    let secs = t.trunc() as i64;
    let ticks = u64::try_from(secs.checked_add(11_644_473_600)?.checked_mul(10_000_000)?).ok()?;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    // SAFETY: an all-zero SYSTEMTIME is valid; the calls only write them.
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: all pointers are valid for the calls; a null time zone means
    // the current one.
    let ok = unsafe {
        FileTimeToSystemTime(&ft, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
    };
    ok.then(|| LocalTime {
        year: i32::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
        hour: u32::from(local.wHour),
        min: u32::from(local.wMinute),
        sec: u32::from(local.wSecond),
    })
}

/// `Util.time2string`: `t` as local time, e.g. `2026-10-20 at  9:05:00`.
pub fn time2string(t: f64) -> String {
    match localtime(t) {
        Some(tm) => format!(
            "{:4}-{:02}-{:02} at {:2}:{:02}:{:02}",
            tm.year, tm.month, tm.day, tm.hour, tm.min, tm.sec
        ),
        None => "(invalid date)".into(),
    }
}

/// `Unix.gettimeofday`.
pub fn gettimeofday() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_print_as_unison_does() {
        let s = time2string(gettimeofday());
        // "YYYY-MM-DD at hh:mm:ss", the hour padded with a space.
        assert_eq!(s.len(), 22, "{s}");
        assert_eq!(&s[10..14], " at ");
        assert_eq!(&s[19..20], ":");
        let tm = localtime(86_400.0 * 365.0).unwrap();
        assert!((1970..=1971).contains(&tm.year), "{tm:?}");
    }

    #[test]
    fn extended_paths() {
        assert_eq!(extended_path("C:/x/y"), r"\\?\C:\x\y");
        assert_eq!(extended_path("//host/share/x"), r"\\?\UNC\host\share\x");
        assert_eq!(extended_path("//?/Volume{1}/x"), r"\\?\Volume{1}\x");
        assert_eq!(extended_path("relative/x"), "relative/x");
        assert_eq!(extended_path("//host"), "//host");
    }

    #[cfg(unix)]
    #[test]
    fn timestamps_never_round_up() {
        assert_eq!(timestamp(10, 0), 10.0);
        let half = if cfg!(target_os = "macos") {
            10.0
        } else {
            10.5
        };
        assert_eq!(timestamp(10, 500_000_000), half);
        let t = timestamp(1_600_000_000, 999_999_999);
        assert!((1_600_000_000.0..1_600_000_001.0).contains(&t));
    }

    #[test]
    fn stat_and_errors() {
        let dir = std::env::temp_dir().join(format!("gemel-system-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let d = name_bytes(dir.as_os_str());
        mkdir(&d, 0o755).unwrap();
        let f = [&d[..], b"/f"].concat();
        fs::write(os_path(&f), b"abc").unwrap();
        let st = stat(&f).unwrap();
        assert_eq!((st.kind, st.size), (Kind::File, 3));
        assert_eq!(stat(&d).unwrap().kind, Kind::Dir);
        assert_eq!(read_dir(&d).unwrap(), vec![b"f".to_vec()]);
        let missing = [&d[..], b"/missing"].concat();
        let e = lstat(&missing).unwrap_err();
        assert!(e.is_absent());
        assert!(
            e.to_transient("querying file information")
                .to_string()
                .starts_with("Error in querying file information:\n")
        );
        assert!(
            e.to_string()
                .ends_with(&format!("[lstat({})]", String::from_utf8_lossy(&missing)))
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
