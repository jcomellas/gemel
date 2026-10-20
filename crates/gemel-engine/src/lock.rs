//! Inter-process locks, as files.
//!
//! Ports Unison `src/lock.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The lock is a file that exists while it is held. On Unix it is taken
//! the way that also works over NFS, where `O_EXCL` is unreliable: create a
//! uniquely named file, hard-link it to the lock's name, and check that the
//! file then has two links. Elsewhere it is created with `O_EXCL`.

use std::io::ErrorKind;

use gemel_base::util::Error;
use gemel_fs::system::{self, SysError};

const PERM: u32 = 0o600;

/// `create`: whether the file was created (false if it already existed).
fn create(name: &[u8]) -> Result<bool, SysError> {
    match system::create_exclusive(name, PERM) {
        Ok(()) => Ok(true),
        Err(e) if e.error.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// `unique`: creates `name` followed by the first free number from `i`.
#[cfg(unix)]
fn unique(name: &[u8], mut i: u32) -> Result<Vec<u8>, SysError> {
    loop {
        let nm = [name, i.to_string().as_bytes()].concat();
        if create(&nm)? {
            return Ok(nm);
        }
        // Highly unlikely.
        i += 1;
    }
}

/// `rename`: links `old` to `new` and removes `old`; whether `new` was
/// taken by this link.
#[cfg(unix)]
fn rename(old: &[u8], new: &[u8]) -> Result<bool, SysError> {
    let _ = system::link(old, new);
    let res = system::link_count(old).is_ok_and(|n| n == 2);
    system::unlink(old)?;
    Ok(res)
}

/// `Lock.acquire`: takes the lock; false if it is held.
pub fn acquire(name: &[u8]) -> Result<bool, Error> {
    #[cfg(unix)]
    let r = unique(name, std::process::id()).and_then(|tmp| rename(&tmp, name));
    #[cfg(not(unix))]
    let r = create(name);
    SysError::convert("Lock.acquire", r)
}

/// `Lock.release`: releases the lock, ignoring errors.
pub fn release(name: &[u8]) {
    let _ = system::unlink(name);
}

/// `Lock.is_locked`: whether the lock is held.
pub fn is_locked(name: &[u8]) -> bool {
    system::file_exists(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquiring_and_releasing() {
        let dir = std::env::temp_dir().join(format!("gemel-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let name = system::name_bytes(dir.join("lk").as_os_str());
        assert!(!is_locked(&name));
        assert_eq!(acquire(&name), Ok(true));
        assert!(is_locked(&name));
        assert_eq!(acquire(&name), Ok(false));
        release(&name);
        assert!(!is_locked(&name));
        assert_eq!(acquire(&name), Ok(true));
        // Nothing is left behind but the lock.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        release(&name);
        let missing = system::name_bytes(dir.join("no/such/lk").as_os_str());
        let e = acquire(&missing).unwrap_err().to_string();
        assert!(e.starts_with("Error in Lock.acquire:\n"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
