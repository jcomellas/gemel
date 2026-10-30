//! ACLs on disk, as text: reading and setting them (`Fs.acl_get_text`,
//! `Fs.acl_set_text`).
//!
//! Ports Unison `src/props_acl.c`, for macOS and Windows; elsewhere ACLs
//! are not supported, as in the stub's fallback (Linux among them, where
//! POSIX ACLs travel as xattrs).
//!
//! Derived from Unison, Copyright 2020-2022, Tõivo Leedjärv.
//!
//! The text goes between hosts and into archives as it is. On macOS it is
//! what the system's `acl_to_text` gives for the extended ACL, without its
//! trailing newline, such as
//!
//! ```text
//! !#acl 1
//! user:CF568982-EAC2-450F-946F-BACAFF5C6355:jo:501:allow:read,write
//! ```
//!
//! On Windows it is the file's discretionary ACL in SDDL, without the
//! entries it inherits (which come from the parent directory and can't be
//! set) and without the auto-inherited flag, such as `D:P(A;;FA;;;WD)`.
//!
//! An empty text is a trivial ACL: reading gives it for a file without an
//! extended ACL (on Windows, without entries of its own and not protected
//! from inheritance), and setting it removes the extended ACL (on Windows,
//! sets an empty one that inherits). Symbolic links are followed.

use crate::fspath::Fspath;

/// `unison_acl_to_text`: the ACL of `path` as text, empty if it is trivial,
/// or `None` if ACLs aren't supported there (the stub's `"-1"`). An error
/// is the stub's message, such as "Error getting ACL: Permission denied".
pub fn get_text(path: &Fspath) -> Result<Option<Vec<u8>>, String> {
    os::get_text(path.as_bytes())
}

/// `unison_acl_from_text`: sets the ACL of `path` from `text`, removing
/// the extended ACL if `text` is empty. Where ACLs aren't supported, does
/// nothing, as the stub's fallback.
pub fn set_text(path: &Fspath, text: &[u8]) -> Result<(), String> {
    os::set_text(path.as_bytes(), text)
}

#[cfg(target_os = "macos")]
mod os {
    use crate::system::error_message;
    use std::ffi::{CStr, CString, c_char, c_int, c_void};
    use std::io;

    /// `acl_t`.
    type AclT = *mut c_void;

    /// `ACL_TYPE_EXTENDED`, the only type macOS has (`unsn_path_acl_type`).
    const ACL_TYPE_EXTENDED: c_int = 0x100;

    // Declared in <sys/acl.h>, in libSystem; the libc crate lacks them.
    #[allow(unsafe_code)]
    unsafe extern "C" {
        fn acl_get_file(path: *const c_char, ty: c_int) -> AclT;
        fn acl_set_file(path: *const c_char, ty: c_int, acl: AclT) -> c_int;
        fn acl_from_text(text: *const c_char) -> AclT;
        fn acl_to_text(acl: AclT, len: *mut isize) -> *mut c_char;
        fn acl_free(obj: *mut c_void) -> c_int;
    }

    fn errno() -> i32 {
        io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    /// `unsn_acl_fail`: the message with the system's description of
    /// `errno` in place of `%s`.
    fn fail(fmt: &str, errno: i32) -> String {
        fmt.replace("%s", &error_message(&io::Error::from_raw_os_error(errno)))
    }

    fn cstring(b: &[u8]) -> Result<CString, String> {
        CString::new(b).map_err(|_| fail("Error getting ACL: %s", libc::EINVAL))
    }

    /// An ACL from the system, freed when dropped.
    struct Acl(AclT);

    impl Drop for Acl {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the ACL came from acl_get_file or acl_from_text and
            // is freed once.
            unsafe {
                acl_free(self.0);
            }
        }
    }

    /// `acl_from_text`, or `None` if the text isn't an ACL.
    #[allow(unsafe_code)]
    fn from_text(text: &CStr) -> Option<Acl> {
        // SAFETY: the text is NUL-terminated.
        let acl = unsafe { acl_from_text(text.as_ptr()) };
        (!acl.is_null()).then_some(Acl(acl))
    }

    /// `acl_set_file`, with `errno` on failure.
    #[allow(unsafe_code)]
    fn set_file(path: &CStr, acl: &Acl) -> Result<(), i32> {
        // SAFETY: the path is NUL-terminated and the ACL is valid.
        let r = unsafe { acl_set_file(path.as_ptr(), ACL_TYPE_EXTENDED, acl.0) };
        if r == -1 { Err(errno()) } else { Ok(()) }
    }

    /// `unsn_get_acl_os`: the extended ACL, `None` if there is none, or
    /// `errno`. macOS sets `ENOENT` for a file without an extended ACL as
    /// for a missing file, so `stat` tells them apart.
    #[allow(unsafe_code)]
    fn get_file(path: &CStr) -> Result<Option<Acl>, i32> {
        // SAFETY: the path is NUL-terminated. errno is cleared first, as
        // the stub does, since only errno says whether the call failed.
        let acl = unsafe {
            *libc::__error() = 0;
            acl_get_file(path.as_ptr(), ACL_TYPE_EXTENDED)
        };
        let e = errno();
        let acl = (!acl.is_null()).then_some(Acl(acl));
        if e == 0 {
            return Ok(acl);
        }
        if e == libc::ENOENT {
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: the path is NUL-terminated and `st` has room for the
            // result, which isn't read.
            let found = unsafe { libc::stat(path.as_ptr(), st.as_mut_ptr()) } == 0
                || errno() != libc::ENOENT;
            if found {
                return Ok(None);
            }
        }
        Err(e)
    }

    /// `unsn_acl_to_text_os`, with `postprocess_acl_os`: the system's text
    /// without its trailing newline.
    #[allow(unsafe_code)]
    fn to_text(acl: &Acl) -> Option<Vec<u8>> {
        // SAFETY: the ACL is valid; the text it gives is NUL-terminated
        // and freed once, after being copied.
        let mut text = unsafe {
            let p = acl_to_text(acl.0, std::ptr::null_mut());
            if p.is_null() {
                return None;
            }
            let text = CStr::from_ptr(p).to_bytes().to_vec();
            acl_free(p.cast());
            text
        };
        if text.last() == Some(&b'\n') {
            text.pop();
        }
        Some(text)
    }

    pub fn get_text(path: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let cpath = cstring(path)?;
        match get_file(&cpath) {
            Err(e) if e == libc::ENOSYS || e == libc::EOPNOTSUPP => Ok(None),
            Err(e) => Err(fail("Error getting ACL: %s", e)),
            Ok(None) => Ok(Some(Vec::new())),
            Ok(Some(acl)) => to_text(&acl)
                .map(Some)
                .ok_or_else(|| "Error converting ACL to text".to_owned()),
        }
    }

    pub fn set_text(path: &[u8], text: &[u8]) -> Result<(), String> {
        let cpath = cstring(path)?;
        if text.is_empty() {
            // `unsn_remove_acl_os`: an empty extended ACL, errors ignored.
            if let Some(empty) = from_text(c"!#acl 1") {
                let _ = set_file(&cpath, &empty);
            }
            return Ok(());
        }
        let ctext = CString::new(text).map_err(|_| "Error converting ACL from text".to_owned())?;
        let acl = from_text(&ctext).ok_or_else(|| "Error converting ACL from text".to_owned())?;
        set_file(&cpath, &acl).map_err(|e| fail("Error setting ACL: %s", e))
    }
}

#[cfg(windows)]
mod os {
    use crate::system::{error_message, os_path};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
        DeleteAce, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, INHERITED_ACE, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, SE_DACL_AUTO_INHERITED, SE_DACL_PROTECTED,
        SetSecurityDescriptorControl, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    /// `unsn_acl_fail`: `msg` with the error's code and the system's text
    /// for it, without its final period, cut to the stub's 159 bytes.
    fn fail(msg: &str, err: u32) -> String {
        let sys = error_message(&std::io::Error::from_raw_os_error(err as i32));
        let sys = sys.trim_end().trim_end_matches('.');
        let mut s = format!("{msg} (Windows error code: {}) {sys}", err as i32);
        if s.len() > 159 {
            let mut end = 159;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
        s
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(Some(0)).collect()
    }

    /// A security descriptor the system allocated, freed when dropped.
    struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Drop for Descriptor {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: the descriptor came from the system, which allocated
            // it with LocalAlloc, and is freed once.
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    /// `unison_acl_to_text`'s work on the descriptor: removes the inherited
    /// entries and the auto-inherited flag, and gives the rest as SDDL, or
    /// `None` if nothing explicit or protected is left.
    #[allow(unsafe_code)]
    fn explicit_sddl(sd: &Descriptor, dacl: *mut ACL) -> Result<Option<Vec<u16>>, String> {
        // SAFETY: `dacl` points into `sd`, which is valid and writable;
        // each ACE GetAce gives is read as its header; the SDDL the system
        // allocates is NUL-terminated, copied, then freed.
        unsafe {
            let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
            if GetAclInformation(
                dacl,
                (&raw mut info).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ) == 0
            {
                return Err(fail(
                    "Error getting ACL information",
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32,
                ));
            }
            let mut count = info.AceCount;
            for i in (0..info.AceCount).rev() {
                let mut ace: *mut c_void = std::ptr::null_mut();
                if GetAce(dacl, i, &mut ace) != 0
                    && u32::from((*ace.cast::<ACE_HEADER>()).AceFlags) & INHERITED_ACE != 0
                    && DeleteAce(dacl, i) != 0
                {
                    count -= 1;
                }
            }
            SetSecurityDescriptorControl(sd.0, SE_DACL_AUTO_INHERITED, 0);
            if count == 0 {
                let (mut control, mut revision) = (0u16, 0u32);
                if GetSecurityDescriptorControl(sd.0, &mut control, &mut revision) == 0 {
                    return Err(fail(
                        "Error getting ACL control information",
                        std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32,
                    ));
                }
                if control & SE_DACL_PROTECTED == 0 {
                    return Ok(None);
                }
            }
            let mut text: *mut u16 = std::ptr::null_mut();
            let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd.0,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            );
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
            if ok == 0 || text.is_null() {
                return Err(fail("Error converting ACL to text", err));
            }
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let out = std::slice::from_raw_parts(text, len).to_vec();
            LocalFree(text.cast());
            Ok(Some(out))
        }
    }

    #[allow(unsafe_code)]
    pub fn get_text(path: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let wpath = wide(os_path(path).as_os_str());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the path is NUL-terminated; the descriptor the system
        // allocates is freed by `Descriptor`, and `dacl` points into it.
        let res = unsafe {
            GetNamedSecurityInfoW(
                wpath.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if res != 0 || sd.is_null() {
            return Err(fail("Error getting ACL", res));
        }
        let sd = Descriptor(sd);
        if dacl.is_null() {
            return Ok(None);
        }
        Ok(Some(match explicit_sddl(&sd, dacl)? {
            None => Vec::new(),
            Some(w) => String::from_utf16_lossy(&w).into_bytes(),
        }))
    }

    #[allow(unsafe_code)]
    pub fn set_text(path: &[u8], text: &[u8]) -> Result<(), String> {
        let wpath = wide(os_path(path).as_os_str());
        // The SDDL of an empty ACL.
        let text = if text.is_empty() { &b"D:"[..] } else { text };
        let wtext: Vec<u16> = String::from_utf8_lossy(text)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the text is NUL-terminated; the descriptor the system
        // allocates is freed by `Descriptor`.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wtext.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
            return Err(fail("Error converting ACL from text", err));
        }
        let sd = Descriptor(sd);
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: the descriptor is valid, and `dacl` points into it.
        let ok = unsafe {
            GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) != 0
                && GetSecurityDescriptorControl(sd.0, &mut control, &mut revision) != 0
        };
        if !ok || present == 0 {
            return Err("Error converting ACL from text (no ACL info present?)".into());
        }
        let si = DACL_SECURITY_INFORMATION
            | if control & SE_DACL_PROTECTED != 0 {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
        // SAFETY: the path is NUL-terminated, and the DACL is valid while
        // the descriptor is.
        let res = unsafe {
            SetNamedSecurityInfoW(
                wpath.as_ptr(),
                SE_FILE_OBJECT,
                si,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        match res {
            0 => Ok(()),
            ERROR_ACCESS_DENIED => Err("Error setting ACL: access denied. The process may require \
                                        Administrator or \"Restore files\" privileges to set the ACL"
                .into()),
            e => Err(fail("Error setting ACL", e)),
        }
    }
}

/// Without ACLs (Linux among them): reading says they aren't supported,
/// and setting does nothing, as the stub's fallback.
#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    pub fn get_text(_: &[u8]) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    pub fn set_text(_: &[u8], _: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::Command;

    struct Dir(std::path::PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            // A deny-delete entry would keep the files.
            let _ = Command::new("chmod").arg("-RN").arg(&self.0).status();
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fspath(p: &std::path::Path) -> Fspath {
        Fspath::canonize(Some(p.to_str().unwrap().as_bytes())).unwrap()
    }

    fn chmod_a(entry: &str, p: &std::path::Path) {
        assert!(
            Command::new("chmod")
                .arg("+a")
                .arg(entry)
                .arg(p)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn acls_are_read_and_set_as_text() {
        let d = Dir(std::env::temp_dir().join(format!("gemel-acl-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d.0);
        std::fs::create_dir_all(&d.0).unwrap();
        let (f, g) = (d.0.join("f"), d.0.join("g"));
        std::fs::write(&f, b"x").unwrap();
        std::fs::write(&g, b"y").unwrap();
        // No extended ACL: trivial.
        assert_eq!(get_text(&fspath(&f)).unwrap(), Some(Vec::new()));
        chmod_a("everyone deny delete", &f);
        chmod_a("group:staff allow read,write", &f);
        let text = get_text(&fspath(&f)).unwrap().unwrap();
        let s = String::from_utf8(text.clone()).unwrap();
        let lines: Vec<&str> = s.split('\n').collect();
        assert_eq!(lines[0], "!#acl 1", "{s}");
        assert!(
            lines[1].starts_with("group:") && lines[1].ends_with(":everyone:12:deny:delete"),
            "{s}"
        );
        assert!(lines[2].ends_with(":staff:20:allow:read,write"), "{s}");
        assert!(!s.ends_with('\n'));
        // The text sets the same ACL elsewhere, and empty removes it.
        set_text(&fspath(&g), &text).unwrap();
        assert_eq!(get_text(&fspath(&g)).unwrap().unwrap(), text);
        set_text(&fspath(&g), b"").unwrap();
        assert_eq!(get_text(&fspath(&g)).unwrap(), Some(Vec::new()));
        // A directory's, with inheritance.
        let sub = d.0.join("sub");
        std::fs::create_dir(&sub).unwrap();
        chmod_a(
            "group:staff allow list,add_file,file_inherit,directory_inherit",
            &sub,
        );
        let t = String::from_utf8(get_text(&fspath(&sub)).unwrap().unwrap()).unwrap();
        assert!(
            t.contains(":staff:20:allow,file_inherit,directory_inherit:"),
            "{t}"
        );
        // Errors are the stub's.
        let e = set_text(&fspath(&g), b"not an acl").unwrap_err();
        assert_eq!(e, "Error converting ACL from text");
        let e = get_text(&fspath(&d.0.join("missing"))).unwrap_err();
        assert_eq!(e, "Error getting ACL: No such file or directory");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn fspath(p: &std::path::Path) -> Fspath {
        Fspath::canonize(Some(p.to_str().unwrap().as_bytes())).unwrap()
    }

    fn text(p: &std::path::Path) -> String {
        String::from_utf8(get_text(&fspath(p)).unwrap().unwrap()).unwrap()
    }

    #[test]
    fn acls_are_read_and_set_as_sddl() {
        let d = std::env::temp_dir().join(format!("gemel-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let (f, g) = (d.join("f"), d.join("g"));
        std::fs::write(&f, b"x").unwrap();
        std::fs::write(&g, b"y").unwrap();
        // Only inherited entries: trivial.
        assert_eq!(text(&f), "");
        // Protected, with an entry of its own; Everyone keeps full access,
        // so the files can still be removed.
        set_text(&fspath(&f), b"D:P(A;;FA;;;WD)(A;;FR;;;BU)").unwrap();
        let t = text(&f);
        assert!(t.starts_with("D:P(") && t.contains("(A;;FA;;;WD)"), "{t}");
        assert!(!t.contains("AI"), "the auto-inherited flag is dropped: {t}");
        // The text sets the same ACL elsewhere.
        set_text(&fspath(&g), t.as_bytes()).unwrap();
        assert_eq!(text(&g), t);
        // An entry of its own, not protected: the inherited ones are left
        // out.
        let sub = d.join("sub");
        set_text(&fspath(&sub), b"D:(A;OICI;FA;;;WD)").unwrap();
        let s = text(&sub);
        assert!(s.starts_with("D:(") && s.contains(";;;WD)"), "{s}");
        assert!(!s.contains("ID;"), "no inherited entries: {s}");
        // Empty: inheriting again, with nothing of its own.
        set_text(&fspath(&g), b"").unwrap();
        assert_eq!(text(&g), "");
        let e = set_text(&fspath(&g), b"not sddl").unwrap_err();
        assert!(
            e.starts_with("Error converting ACL from text (Windows error code: "),
            "{e}"
        );
        let e = get_text(&fspath(&d.join("missing"))).unwrap_err();
        assert!(
            e.starts_with("Error getting ACL (Windows error code: 2) "),
            "{e}"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }
}
