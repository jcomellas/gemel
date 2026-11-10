//! Extended attributes on disk: listing, getting, setting and removing
//! them (`Fs.xattr_*`).
//!
//! Ports Unison `src/props_xattr.c`, for Linux and macOS; elsewhere,
//! Windows included, extended attributes are not supported, as in OCaml.
//!
//! Derived from Unison, Copyright 2020-2022, Tõivo Leedjärv.
//!
//! An attribute is a name and a value of bytes. Symbolic links are
//! followed. On Linux, names are mangled so that they mean the same on
//! every system: a name in the `user` namespace loses its `user.` prefix,
//! and one in any other namespace gets a `!` in front ([`name_from_os`],
//! [`name_to_os`]). System attributes are left alone, neither listed nor
//! set nor removed ([`is_system`]): on Linux those in `system.` but the
//! POSIX ACLs (`system.posix_acl_*`), on macOS the Finder information and
//! the resource fork, which Unison synchronises otherwise.

use crate::fspath::Fspath;

/// Why an attribute operation failed (`Fs.XattrNotSupported`, or OCaml's
/// `Failure` with the stub's message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Extended attributes aren't supported on the path, or at all.
    NotSupported,
    /// The stub's message, such as "Error writing extended attribute:
    /// Permission denied".
    Failure(String),
}

/// `unison_xattr_updates_ctime`: whether changing an attribute changes the
/// file's change time, so that an unchanged change time means unchanged
/// attributes. True on every system gemel supports (only Solaris differs).
pub const UPDATES_CTIME: bool = true;

/// `XN_LEN`: the most of a name the stubs copy when mangling it.
const NAME_LEN: usize = 255;

/// `val_of_attrname`: a Linux name as Unison shows it: without `user.`,
/// or with `!` in front of any other namespace.
pub fn name_from_os(name: &[u8]) -> Vec<u8> {
    match name.strip_prefix(b"user.") {
        Some(rest) => rest.to_vec(),
        None => {
            let mut out = b"!".to_vec();
            out.extend_from_slice(&name[..name.len().min(NAME_LEN)]);
            out
        }
    }
}

/// `attrname_of_val`: the Linux name for a name as Unison shows it.
pub fn name_to_os(name: &[u8]) -> Vec<u8> {
    match name.strip_prefix(b"!") {
        Some(rest) => rest.to_vec(),
        None => {
            let mut out = b"user.".to_vec();
            out.extend_from_slice(&name[..name.len().min(NAME_LEN)]);
            out
        }
    }
}

/// `unsn_is_system_attr_os` on Linux: `system.`, but not the POSIX ACLs.
pub fn is_system_linux(os_name: &[u8]) -> bool {
    os_name.starts_with(b"system.") && !os_name.starts_with(b"system.posix_acl_")
}

/// `unsn_is_system_attr_os` on macOS: the Finder information and the
/// resource fork.
pub fn is_system_macos(os_name: &[u8]) -> bool {
    os_name == b"com.apple.FinderInfo" || os_name == b"com.apple.ResourceFork"
}

/// `unsn_is_system_attr_os` on this system, for a name as the system
/// gives it.
pub fn is_system(os_name: &[u8]) -> bool {
    if cfg!(target_os = "linux") {
        is_system_linux(os_name)
    } else if cfg!(target_os = "macos") {
        is_system_macos(os_name)
    } else {
        false
    }
}

/// `unison_xattrs_list`: each attribute's name, as Unison shows it, and
/// the length of its value, in no particular order. System attributes,
/// and those whose length can't be read, are left out.
pub fn list(path: &Fspath) -> Result<Vec<(Vec<u8>, i64)>, Error> {
    os::list(path.as_bytes())
}

/// `unison_xattr_get`: the value of the attribute `name`.
pub fn get(path: &Fspath, name: &[u8]) -> Result<Vec<u8>, Error> {
    os::get(path.as_bytes(), name)
}

/// `unison_xattr_set`: sets the attribute `name` to `value`, creating it if
/// needed. A system attribute is left as it is.
pub fn set(path: &Fspath, name: &[u8], value: &[u8]) -> Result<(), Error> {
    os::set(path.as_bytes(), name, value)
}

/// `unison_xattr_remove`: removes the attribute `name`. Only "not
/// supported" is an error, as in the stub; a system attribute is left as
/// it is.
pub fn remove(path: &Fspath, name: &[u8]) -> Result<(), Error> {
    os::remove(path.as_bytes(), name)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod os {
    use super::{Error, is_system, name_from_os, name_to_os};
    use crate::system::error_message;
    use std::ffi::CString;
    use std::io;

    /// The name as the system wants it.
    fn os_name(name: &[u8]) -> Vec<u8> {
        if cfg!(target_os = "linux") {
            name_to_os(name)
        } else {
            name.to_vec()
        }
    }

    /// The name as Unison shows it.
    fn unison_name(os_name: &[u8]) -> Vec<u8> {
        if cfg!(target_os = "linux") {
            name_from_os(os_name)
        } else {
            os_name.to_vec()
        }
    }

    fn cstring(b: &[u8]) -> Result<CString, Error> {
        CString::new(b).map_err(|_| Error::Failure(fail_text("%s", libc::EINVAL)))
    }

    fn errno() -> i32 {
        io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// `unsn_xattr_fail`: the message with the system's description of
    /// `errno` in place of `%s`.
    fn fail_text(fmt: &str, errno: i32) -> String {
        fmt.replace("%s", &error_message(&io::Error::from_raw_os_error(errno)))
    }

    fn fail(fmt: &str) -> Error {
        Error::Failure(fail_text(fmt, errno()))
    }

    /// `unsn_length_xattr_os`, `unsn_get_xattr_os`: the value's length, or
    /// as much of it as fits in `buf`, or -1.
    #[allow(unsafe_code)]
    fn getxattr(path: &CString, name: &CString, buf: &mut [u8]) -> isize {
        let (p, len) = if buf.is_empty() {
            (std::ptr::null_mut(), 0)
        } else {
            (buf.as_mut_ptr().cast(), buf.len())
        };
        // SAFETY: both names are NUL-terminated, and `p` is null with a
        // length of 0 or points to `len` writable bytes.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::getxattr(path.as_ptr(), name.as_ptr(), p, len)
            }
            #[cfg(target_os = "macos")]
            {
                libc::getxattr(path.as_ptr(), name.as_ptr(), p, len, 0, 0)
            }
        }
    }

    /// `unsn_list_xattr_os`.
    #[allow(unsafe_code)]
    fn listxattr(path: &CString, buf: &mut [u8]) -> isize {
        let (p, len) = if buf.is_empty() {
            (std::ptr::null_mut(), 0)
        } else {
            (buf.as_mut_ptr().cast(), buf.len())
        };
        // SAFETY: the path is NUL-terminated, and `p` is null with a length
        // of 0 or points to `len` writable bytes.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::listxattr(path.as_ptr(), p, len)
            }
            #[cfg(target_os = "macos")]
            {
                libc::listxattr(path.as_ptr(), p, len, 0)
            }
        }
    }

    /// `unsn_set_xattr_os`.
    #[allow(unsafe_code)]
    fn setxattr(path: &CString, name: &CString, value: &[u8]) -> i32 {
        // SAFETY: both names are NUL-terminated, and the value is
        // `value.len()` readable bytes.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            }
            #[cfg(target_os = "macos")]
            {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            }
        }
    }

    /// `unsn_remove_xattr_os`.
    #[allow(unsafe_code)]
    fn removexattr(path: &CString, name: &CString) -> i32 {
        // SAFETY: both names are NUL-terminated.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::removexattr(path.as_ptr(), name.as_ptr())
            }
            #[cfg(target_os = "macos")]
            {
                libc::removexattr(path.as_ptr(), name.as_ptr(), 0)
            }
        }
    }

    pub fn list(path: &[u8]) -> Result<Vec<(Vec<u8>, i64)>, Error> {
        if path.len() > 32767 {
            return Err(Error::Failure("The path is too long".into()));
        }
        let cpath = cstring(path)?;
        let list_fail = "Error getting list of extended attributes: %s";
        let n = listxattr(&cpath, &mut []);
        if n == -1 {
            if errno() == libc::ENOTSUP {
                return Err(Error::NotSupported);
            }
            return Err(fail(list_fail));
        }
        if n == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; n as usize];
        let n = listxattr(&cpath, &mut buf);
        if n == -1 {
            return Err(fail(list_fail));
        }
        if n == 0 {
            return Ok(Vec::new());
        }
        buf.truncate(n as usize);
        // "For safety", as the stub: the last name ends the buffer.
        if let Some(last) = buf.last_mut() {
            *last = 0;
        }
        let mut out = Vec::new();
        for name in buf.split(|&b| b == 0).filter(|n| !n.is_empty()) {
            if is_system(name) {
                continue;
            }
            let Ok(cname) = CString::new(name) else {
                continue;
            };
            let len = getxattr(&cpath, &cname, &mut []);
            if len == -1 {
                continue;
            }
            out.push((unison_name(name), len as i64));
        }
        // The stub conses each name onto the list.
        out.reverse();
        Ok(out)
    }

    pub fn get(path: &[u8], name: &[u8]) -> Result<Vec<u8>, Error> {
        let cpath = cstring(path)?;
        let cname = cstring(&os_name(name))?;
        let mut tries = 0;
        loop {
            tries += 1;
            if tries > 10 {
                return Err(Error::Failure(
                    "Error reading contents of extended attribute; it keeps changing".into(),
                ));
            }
            let len = getxattr(&cpath, &cname, &mut []);
            if len == -1 {
                if errno() == libc::ENOTSUP {
                    return Err(Error::NotSupported);
                }
                return Err(fail("Error reading length of extended attribute: %s"));
            }
            if len == 0 {
                return Ok(Vec::new());
            }
            if len > 16777211 {
                return Err(Error::Failure(format!(
                    "Extended attribute value is too big ({len} bytes)"
                )));
            }
            let mut buf = vec![0u8; len as usize];
            let got = getxattr(&cpath, &cname, &mut buf);
            if got == -1 {
                match errno() {
                    libc::ENOTSUP => return Err(Error::NotSupported),
                    // The value grew in between: again.
                    libc::ERANGE => continue,
                    _ => {
                        return Err(fail("Error reading contents of extended attribute: %s"));
                    }
                }
            }
            buf.truncate(got as usize);
            return Ok(buf);
        }
    }

    pub fn set(path: &[u8], name: &[u8], value: &[u8]) -> Result<(), Error> {
        let os = os_name(name);
        if is_system(&os) {
            return Ok(());
        }
        let (cpath, cname) = (cstring(path)?, cstring(&os)?);
        if setxattr(&cpath, &cname, value) == -1 {
            if errno() == libc::ENOTSUP {
                return Err(Error::NotSupported);
            }
            return Err(fail("Error writing extended attribute: %s"));
        }
        Ok(())
    }

    pub fn remove(path: &[u8], name: &[u8]) -> Result<(), Error> {
        let os = os_name(name);
        if is_system(&os) {
            return Ok(());
        }
        let (cpath, cname) = (cstring(path)?, cstring(&os)?);
        if removexattr(&cpath, &cname) == -1 && errno() == libc::ENOTSUP {
            return Err(Error::NotSupported);
        }
        Ok(())
    }
}

/// Without extended attributes (Windows among them): everything is "not
/// supported", as the stub's fallback raises it.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod os {
    use super::Error;

    pub fn list(_: &[u8]) -> Result<Vec<(Vec<u8>, i64)>, Error> {
        Err(Error::NotSupported)
    }

    pub fn get(_: &[u8], _: &[u8]) -> Result<Vec<u8>, Error> {
        Err(Error::NotSupported)
    }

    pub fn set(_: &[u8], _: &[u8], _: &[u8]) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    pub fn remove(_: &[u8], _: &[u8]) -> Result<(), Error> {
        Err(Error::NotSupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_names_are_mangled_as_the_stub_mangles_them() {
        assert_eq!(name_from_os(b"user.mime_type"), b"mime_type");
        assert_eq!(name_from_os(b"trusted.overlay"), b"!trusted.overlay");
        assert_eq!(
            name_from_os(b"system.posix_acl_access"),
            b"!system.posix_acl_access"
        );
        assert_eq!(name_to_os(b"mime_type"), b"user.mime_type");
        assert_eq!(name_to_os(b"!trusted.overlay"), b"trusted.overlay");
        // A name in the user namespace is never written "!user.".
        assert_eq!(name_to_os(b"!user.k"), b"user.k");
        // At most 255 bytes of a name are copied.
        let long = vec![b'a'; 300];
        assert_eq!(name_to_os(&long).len(), 5 + 255);
        assert_eq!(name_from_os(&long).len(), 1 + 255);
    }

    #[test]
    fn system_attributes_are_the_stubs() {
        assert!(is_system_linux(b"system.nfs4_acl"));
        assert!(!is_system_linux(b"system.posix_acl_access"));
        assert!(!is_system_linux(b"user.system.x"));
        assert!(is_system_macos(b"com.apple.FinderInfo"));
        assert!(is_system_macos(b"com.apple.ResourceFork"));
        assert!(!is_system_macos(b"com.apple.provenance"));
    }

    /// A directory of its own for a test, in the system's temporary
    /// directory (which takes user attributes on CI's systems).
    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gemel-xattr-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fspath(p: &std::path::Path) -> Fspath {
        Fspath::canonize(Some(p.to_str().unwrap().as_bytes())).unwrap()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn attributes_are_set_listed_read_and_removed() {
        let dir = temp("rw");
        let f = dir.join("f");
        std::fs::write(&f, "x").unwrap();
        let p = fspath(&f);
        // Past the 32 and 1024 bytes `Props` tells apart, within ext4's
        // limit of about a block for a value.
        let big = vec![7u8; 3000];
        set(&p, b"gemel.small", b"v").unwrap();
        set(&p, b"gemel.big", &big).unwrap();
        set(&p, b"gemel.empty", b"").unwrap();
        let l = list(&p).unwrap();
        for (n, len) in [
            (&b"gemel.small"[..], 1),
            (b"gemel.big", 3000),
            (b"gemel.empty", 0),
        ] {
            assert!(l.contains(&(n.to_vec(), len)), "{n:?} in {l:?}");
        }
        assert_eq!(get(&p, b"gemel.small").unwrap(), b"v");
        assert_eq!(get(&p, b"gemel.big").unwrap(), big);
        assert_eq!(get(&p, b"gemel.empty").unwrap(), b"");
        // Overwritten.
        set(&p, b"gemel.small", b"w").unwrap();
        assert_eq!(get(&p, b"gemel.small").unwrap(), b"w");
        remove(&p, b"gemel.small").unwrap();
        assert!(!list(&p).unwrap().iter().any(|(n, _)| n == b"gemel.small"));
        // Removing what isn't there isn't an error, as in the stub.
        remove(&p, b"gemel.small").unwrap();
        let e = get(&p, b"gemel.small").unwrap_err();
        assert!(
            matches!(&e, Error::Failure(m) if m.starts_with("Error reading length of extended attribute: ")),
            "{e:?}"
        );
        // A directory has them too, and a symbolic link is followed.
        set(&fspath(&dir), b"gemel.dir", b"d").unwrap();
        assert_eq!(get(&fspath(&dir), b"gemel.dir").unwrap(), b"d");
        std::os::unix::fs::symlink(&f, dir.join("l")).unwrap();
        assert_eq!(get(&fspath(&dir.join("l")), b"gemel.big").unwrap(), big);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// On Linux, another namespace than `user` is named with `!`. Setting
    /// `trusted.*` needs `CAP_SYS_ADMIN`, which neither CI's runner nor
    /// Docker's root has, so the test says so and stops when refused.
    #[cfg(target_os = "linux")]
    #[test]
    fn other_namespaces_are_named_with_a_bang() {
        let dir = temp("ns");
        let f = dir.join("f");
        std::fs::write(&f, "x").unwrap();
        let p = fspath(&f);
        if let Err(Error::Failure(m)) = set(&p, b"!trusted.gemel", b"t") {
            assert!(m.ends_with("Operation not permitted"), "{m}");
            eprintln!("skipped: setting trusted.* needs CAP_SYS_ADMIN");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        assert!(list(&p).unwrap().contains(&(b"!trusted.gemel".to_vec(), 1)));
        assert_eq!(get(&p, b"!trusted.gemel").unwrap(), b"t");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn attributes_are_not_supported() {
        let dir = temp("none");
        let p = fspath(&dir);
        assert_eq!(list(&p), Err(Error::NotSupported));
        assert_eq!(get(&p, b"k"), Err(Error::NotSupported));
        assert_eq!(set(&p, b"k", b"v"), Err(Error::NotSupported));
        assert_eq!(remove(&p, b"k"), Err(Error::NotSupported));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
