//! Copy-on-write cloning (clonefile, FICLONE, reflink).
//!
//! Ports Unison `src/copy_stubs.c`.
//!
//! Derived from Unison, Copyright 2021-2025, Tõivo Leedjärv.
//!
//! [`clone_path`] and [`clone_file`] clone a whole file or do nothing;
//! [`copy_file`] copies a range in the kernel. Where the OS can't, they
//! report so and copying falls back to reading and writing. Not ported:
//! block cloning on Windows (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`, on ReFS),
//! so [`copy_file`] always falls back there; FreeBSD's `copy_file_range`
//! and Solaris's `sendfile`, which gemel doesn't build for.

use std::fs::File;
use std::io;

use crate::fspath::Fspath;

/// `System.clone_path`: clones the file `from` to the new file `to`
/// (macOS `clonefile`, without following a link or copying the owner).
/// False if it can't, for whatever reason.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn clone_path(from: &Fspath, to: &Fspath) -> bool {
    use std::ffi::{CString, c_char, c_int};
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;
    unsafe extern "C" {
        fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> c_int;
    }
    let (Ok(src), Ok(dst)) = (CString::new(from.as_bytes()), CString::new(to.as_bytes())) else {
        return false;
    };
    // SAFETY: both are NUL-terminated strings that outlive the call.
    unsafe {
        clonefile(
            src.as_ptr(),
            dst.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOOWNERCOPY,
        ) == 0
    }
}

/// `System.clone_path`: unsupported here.
#[cfg(not(target_os = "macos"))]
pub fn clone_path(_from: &Fspath, _to: &Fspath) -> bool {
    false
}

/// `System.clone_file`: makes `dst` a clone of `src` (Linux `FICLONE`),
/// wholly or not at all. False if it can't.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn clone_file(src: &File, dst: &File) -> bool {
    use std::os::fd::AsRawFd;
    // _IOW(0x94, 9, int)
    const FICLONE: libc::c_ulong = 0x4004_9409;
    // SAFETY: both descriptors are open for the duration of the call.
    unsafe { libc::ioctl(dst.as_raw_fd(), FICLONE as _, src.as_raw_fd()) == 0 }
}

/// `System.clone_file`: unsupported here.
#[cfg(not(target_os = "linux"))]
pub fn clone_file(_src: &File, _dst: &File) -> bool {
    false
}

/// `System.copy_file`: copies up to `len` bytes of `src` from `src_offs`
/// to `dst` at its current position, which advances; `src`'s doesn't.
/// Returns the number copied (0 at the end of `src`). Linux tries
/// `copy_file_range`, then `sendfile`. An error means falling back to
/// reading and writing.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn copy_file(src: &File, dst: &File, src_offs: u64, len: usize) -> io::Result<usize> {
    use std::os::fd::AsRawFd;
    let (in_fd, out_fd) = (src.as_raw_fd(), dst.as_raw_fd());
    let mut off = libc::off64_t::try_from(src_offs)
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: the descriptors are open and `off` outlives the call.
    let mut ret =
        unsafe { libc::copy_file_range(in_fd, &mut off, out_fd, std::ptr::null_mut(), len, 0) };
    if ret == -1 {
        let errno = io::Error::last_os_error().raw_os_error();
        if matches!(errno, Some(libc::ENOSYS | libc::EBADF | libc::EXDEV)) {
            // SAFETY: as above.
            ret = unsafe { libc::sendfile64(out_fd, in_fd, &mut off, len) };
        }
    }
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret as usize)
    }
}

/// `System.copy_file`: unsupported here (`ENOSYS`).
#[cfg(not(target_os = "linux"))]
pub fn copy_file(_src: &File, _dst: &File, _src_offs: u64, _len: usize) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy_file: not supported",
    ))
}
