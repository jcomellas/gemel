//! MD5 file fingerprints.
//!
//! Ports Unison `src/fingerprint.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

use std::fmt;

use gemel_compat::umarshal::{self, Reader, Writer};
use md5::{Digest, Md5};

use crate::fspath::Fspath;
use crate::props::Filesize;
use crate::system;
use gemel_base::path::Path;
use gemel_base::util::Error;

/// A fingerprint (`Fingerprint.t`): the raw MD5 of a file's contents, a
/// pseudo-fingerprint (`LEN<size>@<MD5 of the path>`, for files whose
/// contents haven't been read), or empty ([`Fingerprint::DUMMY`]).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub Vec<u8>);

const PSEUDO_PREFIX: &[u8] = b"LEN";

impl Fingerprint {
    /// `Fingerprint.dummy`.
    pub const DUMMY: Fingerprint = Fingerprint(Vec::new());

    /// `Fingerprint.pseudo`: stands in for the contents of the file at
    /// `path` (a replica-relative path) of size `len`.
    pub fn pseudo(path: &[u8], len: Filesize) -> Fingerprint {
        let mut v = format!("LEN{len}@").into_bytes();
        v.extend_from_slice(&Md5::digest(path));
        Fingerprint(v)
    }

    /// `Fingerprint.ispseudo`.
    pub fn is_pseudo(&self) -> bool {
        self.0.starts_with(PSEUDO_PREFIX)
    }

    /// `Fingerprint.hash` (for hash tables, not `checkArchive`): the first
    /// three bytes, or a constant for the dummy.
    pub fn hash(&self) -> i64 {
        match &self.0[..] {
            [] => 1_234_577,
            [a, b, c, ..] => i64::from(*a) + (i64::from(*b) << 8) + (i64::from(*c) << 16),
            _ => panic!("fingerprint shorter than 3 bytes"),
        }
    }

    /// `Fingerprint.m`.
    pub fn marshal(&self, w: &mut Writer) {
        w.string(&self.0);
    }

    /// `Fingerprint.m`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Fingerprint> {
        r.string().map(Fingerprint)
    }
}

/// The MD5 of a reader's contents, up to `len` bytes if given.
fn digest(mut r: impl std::io::Read, len: Option<u64>) -> std::io::Result<(Vec<u8>, u64)> {
    let mut md5 = Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let want = match len {
            Some(l) => (l - total).min(buf.len() as u64) as usize,
            None => buf.len(),
        };
        if want == 0 {
            break;
        }
        let n = r.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        md5.update(&buf[..n]);
        total += n as u64;
    }
    Ok((md5.finalize().to_vec(), total))
}

impl Fingerprint {
    /// `Fingerprint.file`: the MD5 of the contents of `fspath/path`.
    pub fn file<S>(fspath: &Fspath, path: &Path<S>) -> Result<Fingerprint, Error> {
        let f = fspath.concat(path);
        let what = format!("digesting {f}");
        // OCaml reads through a channel, so errors are Sys_errors.
        let sys = |call, e| system::SysError {
            call,
            param: f.to_string(),
            error: e,
        };
        let file = std::fs::File::open(system::os_path(f.as_bytes()))
            .map_err(|e| sys("open", e).to_transient_sys(&what))?;
        let (d, _) = digest(file, None).map_err(|e| sys("read", e).to_transient_sys(&what))?;
        Ok(Fingerprint(d))
    }

    /// `Fingerprint.subfile`: the MD5 of `len` bytes at `offset` in `path`.
    pub fn subfile(path: &Fspath, offset: u64, len: Filesize) -> Result<Fingerprint, Error> {
        use std::io::{Seek, SeekFrom};
        let len = u64::try_from(len)
            .map_err(|_| Error::Transient(format!("File '{path}' too big for fingerprinting")))?;
        let sys = |call, e| system::SysError {
            call,
            param: path.to_string(),
            error: e,
        };
        let mut f = std::fs::File::open(system::os_path(path.as_bytes()))
            .map_err(|e| sys("open", e).to_transient_sys("digesting subfile"))?;
        f.seek(SeekFrom::Start(offset))
            .map_err(|e| sys("lseek", e).to_transient_sys("digesting subfile"))?;
        let (d, n) =
            digest(f, Some(len)).map_err(|e| sys("read", e).to_transient("digesting subfile"))?;
        if n < len {
            return Err(Error::Transient(format!(
                "Error in digesting subfile '{path}': truncated file"
            )));
        }
        Ok(Fingerprint(d))
    }
}

/// `Fingerprint.toString`: lowercase hex, or a pseudo-fingerprint as is.
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_pseudo() {
            f.write_str(&String::from_utf8_lossy(&self.0))
        } else {
            self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
        }
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_pseudo_and_hash() {
        let fp = Fingerprint(Md5::digest(b"hi\n").to_vec());
        assert_eq!(fp.to_string(), "764efa883dda1e11db47671c4a3bbd9e");
        assert_eq!(fp.hash(), 0x76 + (0x4e << 8) + (0xfa << 16));
        assert_eq!(Fingerprint::DUMMY.hash(), 1_234_577);
        let p = Fingerprint::pseudo(b"a/b", 12);
        assert!(p.is_pseudo() && p.0.starts_with(b"LEN12@") && p.0.len() == 6 + 16);
    }
}
