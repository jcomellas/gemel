//! Filesystem operations as used by the rest of Unison.
//!
//! Ports Unison `src/fs.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The system calls of [`crate::system`] on [`Fspath`]s. Kept as a module
//! of its own, as in OCaml, so that callers name what they operate on.

use crate::fspath::Fspath;
use crate::system::{self, Stats, SysResult};

/// `Fs.stat`.
pub fn stat(f: &Fspath) -> SysResult<Stats> {
    system::stat(f.as_bytes())
}

/// `Fs.lstat`.
pub fn lstat(f: &Fspath) -> SysResult<Stats> {
    system::lstat(f.as_bytes())
}

/// `Fs.readlink`.
pub fn readlink(f: &Fspath) -> SysResult<Vec<u8>> {
    system::readlink(f.as_bytes())
}

/// `Fs.symlink`: creates `f` pointing at `target`.
pub fn symlink(target: &[u8], f: &Fspath) -> SysResult<()> {
    system::symlink(target, f.as_bytes())
}

/// `Fs.chown`.
pub fn chown(f: &Fspath, uid: i64, gid: i64) -> SysResult<()> {
    system::chown(f.as_bytes(), uid, gid)
}

/// `Fs.chmod`.
pub fn chmod(f: &Fspath, perm: i64) -> SysResult<()> {
    system::chmod(f.as_bytes(), perm)
}

/// `Fs.utimes`.
pub fn utimes(f: &Fspath, atime: f64, mtime: f64) -> SysResult<()> {
    system::utimes(f.as_bytes(), atime, mtime)
}

/// `Fs.unlink`.
pub fn unlink(f: &Fspath) -> SysResult<()> {
    system::unlink(f.as_bytes())
}

/// `Fs.rmdir`.
pub fn rmdir(f: &Fspath) -> SysResult<()> {
    system::rmdir(f.as_bytes())
}

/// `Fs.mkdir`.
pub fn mkdir(f: &Fspath, perm: u32) -> SysResult<()> {
    system::mkdir(f.as_bytes(), perm)
}

/// `Fs.rename`.
pub fn rename(from: &Fspath, to: &Fspath) -> SysResult<()> {
    system::rename(from.as_bytes(), to.as_bytes())
}

/// The entries of a directory, without `.` and `..` (`Fs.opendir` and
/// `readdir`).
pub fn read_dir(f: &Fspath) -> SysResult<Vec<Vec<u8>>> {
    system::read_dir(f.as_bytes())
}

/// `Fs.file_exists`: false only when the path (or a parent) is missing.
pub fn file_exists(f: &Fspath) -> SysResult<bool> {
    match stat(f) {
        Ok(_) => Ok(true),
        Err(e) if e.is_absent() => Ok(false),
        Err(e) => Err(e),
    }
}

/// `Fs.hasInodeNumbers`.
pub fn has_inode_numbers() -> bool {
    system::has_inode_numbers()
}

/// `Fs.hasSymlink`.
pub fn has_symlink() -> bool {
    system::has_symlink()
}

/// `Fs.hasCorrectCTime`.
pub fn has_correct_ctime() -> bool {
    system::has_correct_ctime()
}
