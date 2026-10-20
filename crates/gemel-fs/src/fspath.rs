//! Absolute filesystem paths of replica roots.
//!
//! Ports Unison `src/fspath.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! An [`Fspath`] is an absolute path in Unison's byte form: `/`-separated
//! everywhere (Windows paths are converted), without a trailing slash
//! except for root directories (`/`, `C:/`, `//host/share/`), which always
//! have one. Canonical root names (see [`Fspath::canonize`]) go into archive
//! names, so they must come out as OCaml Unison's do.

use std::sync::OnceLock;

use gemel_base::name::Name;
use gemel_base::path::Path;
use gemel_base::rx::Rx;
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};

use crate::system;

/// An absolute path to a replica root or something in it (`Fspath.t`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fspath(Vec<u8>);

impl std::fmt::Debug for Fspath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fspath({:?})", String::from_utf8_lossy(&self.0))
    }
}

/// `Fspath.toPrintString`.
impl std::fmt::Display for Fspath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

/// Whether paths follow Windows conventions (`winStylePaths`).
const WIN_STYLE: bool = cfg!(windows);

/// `winRootRx`: drive roots, UNC shares and `//?/` volume roots.
fn win_root_rx() -> &'static Rx {
    static RX: OnceLock<Rx> = OnceLock::new();
    RX.get_or_init(|| {
        Rx::regex(
            "(([a-zA-Z]:)?/|//[^?/]+/[^/]+/|//[?]/[Uu][Nn][Cc]/[^/]+/[^/]+/)|//[?]/([^Uu][^/]*|[Uu]|[Uu][^Nn][^/]*|[Uu][Nn]|[Uu][Nn][^Cc][^/]*|[Uu][Nn][Cc][^/]+)/",
        )
        .expect("valid regex")
    })
}

/// `winFNsPrefixRx`: a `\\?\` prefix and its first component.
fn win_fns_prefix_rx() -> &'static Rx {
    static RX: OnceLock<Rx> = OnceLock::new();
    RX.get_or_init(|| Rx::regex("[\\/][\\/][?][\\/][^\\/]+").expect("valid regex"))
}

/// `Fspath.isRootDir`.
pub fn is_root_dir(d: &[u8]) -> bool {
    d == b"/" || (WIN_STYLE && win_root_rx().match_string(d))
}

fn backslashes_to_slashes(s: &[u8]) -> Vec<u8> {
    s.iter()
        .map(|&c| if c == b'\\' { b'/' } else { c })
        .collect()
}

fn remove_trailing_slashes(mut s: &[u8]) -> &[u8] {
    while let [rest @ .., b'/'] = s {
        s = rest;
    }
    s
}

/// `Fspath.isRootDirLocalString`.
fn is_root_dir_local_string(d: &[u8]) -> bool {
    let d = if WIN_STYLE {
        backslashes_to_slashes(d)
    } else {
        d.to_vec()
    };
    is_root_dir(&[remove_trailing_slashes(&d), b"/"].concat())
}

/// `Fspath.winRootFix`: adds the trailing slash a root needs.
fn win_root_fix(d: Vec<u8>) -> Vec<u8> {
    let with = [&d[..], b"/"].concat();
    if win_root_rx().match_string(&with) {
        with
    } else {
        d
    }
}

/// `Fspath.isInvalidWinPath`: a bare `\\?\x` with nothing after it.
fn is_invalid_win_path(p: &[u8]) -> bool {
    win_fns_prefix_rx().match_string(p)
}

/// OCaml's `Filename.dirname` (Unix rules, or Windows rules on Windows).
fn dirname(p: &[u8]) -> Vec<u8> {
    let sep = |c: u8| c == b'/' || (WIN_STYLE && c == b'\\');
    // Windows: keep a drive prefix.
    let (drive, p) = if WIN_STYLE && p.len() >= 2 && p[1] == b':' && p[0].is_ascii_alphabetic() {
        (&p[..2], &p[2..])
    } else {
        (&b""[..], p)
    };
    let mut n = p.len();
    while n > 0 && sep(p[n - 1]) {
        n -= 1;
    }
    if n == 0 {
        return [drive, if p.is_empty() { b"." } else { &p[..1] }].concat();
    }
    while n > 0 && !sep(p[n - 1]) {
        n -= 1;
    }
    if n == 0 {
        return [drive, b"."].concat();
    }
    while n > 0 && sep(p[n - 1]) {
        n -= 1;
    }
    if n == 0 {
        [drive, &p[..1]].concat()
    } else {
        [drive, &p[..n]].concat()
    }
}

/// OCaml's `Filename.basename`.
fn basename(p: &[u8]) -> Vec<u8> {
    let sep = |c: u8| c == b'/' || (WIN_STYLE && c == b'\\');
    let p = if WIN_STYLE && p.len() >= 2 && p[1] == b':' {
        &p[2..]
    } else {
        p
    };
    let mut end = p.len();
    while end > 0 && sep(p[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && !sep(p[start - 1]) {
        start -= 1;
    }
    p[start..end].to_vec()
}

/// `Fspath.winSafeDirname`: `dirname` that copes with `\\?\` prefixes.
fn win_safe_dirname(p: &[u8]) -> Vec<u8> {
    if !WIN_STYLE {
        return dirname(p);
    }
    match win_fns_prefix_rx().match_prefix(p, 0) {
        None => dirname(p),
        Some(pos) => [&p[..pos], &dirname(&p[pos..])].concat(),
    }
}

/// The working directory after `chdir p` (`getcwd`), as Unison's bytes.
fn physical_dir(p: &[u8]) -> std::io::Result<Vec<u8>> {
    let path = system::os_path(p);
    if !std::fs::metadata(&path)?.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotADirectory));
    }
    let c = std::fs::canonicalize(&path)?;
    let bytes = system::name_bytes(c.as_os_str());
    if WIN_STYLE {
        // canonicalize returns \\?\C:\x or \\?\UNC\host\share\x; getcwd
        // returns C:\x and \\host\share\x.
        if let Some(rest) = bytes.strip_prefix(b"\\\\?\\UNC\\") {
            return Ok([&b"\\\\"[..], rest].concat());
        }
        if let Some(rest) = bytes.strip_prefix(b"\\\\?\\")
            && rest.len() >= 2
            && rest[1] == b':'
        {
            return Ok(rest.to_vec());
        }
    }
    Ok(bytes)
}

/// `Sys_error`'s message for a failed `chdir`: `"<path>: <reason>"`.
fn chdir_error(p: &[u8], e: &std::io::Error) -> String {
    let s = e.to_string();
    let reason = match s.rfind(" (os error ") {
        Some(i) => &s[..i],
        None => &s,
    };
    format!("{}: {reason}", String::from_utf8_lossy(p))
}

impl Fspath {
    /// The bytes (`Fspath.toString`).
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// `Fspath.m`.
    pub fn marshal(&self, w: &mut Writer) {
        w.string(&self.0);
    }

    /// `Fspath.m`.
    pub fn unmarshal(r: &mut Reader<'_>) -> umarshal::Result<Fspath> {
        r.string().map(Fspath)
    }

    /// Wraps bytes that are already in canonical form (from the peer or an
    /// archive).
    pub fn from_canonical(b: Vec<u8>) -> Fspath {
        Fspath(b)
    }

    /// `Fspath.localString2fspath`: normalises separators and trailing
    /// slashes, without resolving anything.
    pub fn from_local_string(s: &[u8]) -> Result<Fspath, Error> {
        let s = if WIN_STYLE {
            win_root_fix(backslashes_to_slashes(s))
        } else {
            s.to_vec()
        };
        if is_root_dir(&s) {
            Ok(Fspath(s))
        } else if !s.is_empty() {
            let t = remove_trailing_slashes(&s);
            Ok(Fspath(if t.is_empty() {
                b"/".to_vec()
            } else {
                t.to_vec()
            }))
        } else {
            Err(Error::Transient("Os.localString2fspath".into()))
        }
    }

    /// `Fspath.canonize`: the canonical name of a root, with symbolic
    /// links in directories resolved. A root that isn't a directory is
    /// named by its canonical parent and its own name.
    pub fn canonize(p: Option<&[u8]>) -> Result<Fspath, Error> {
        let p = match p {
            None | Some(b"") => &b"."[..],
            Some(s) => s,
        };
        let resolved = match physical_dir(p) {
            Ok(d) => d,
            Err(why) => {
                let why = chdir_error(p, &why);
                if is_root_dir_local_string(p) || is_invalid_win_path(p) {
                    return Err(Error::Fatal(format!(
                        "Cannot find canonical name of root directory {}\n({why}){}",
                        String::from_utf8_lossy(p),
                        if is_invalid_win_path(p) {
                            "\nMaybe you need to add a backslash at end of the root path?"
                        } else {
                            ""
                        }
                    )));
                }
                let parent = win_safe_dirname(p);
                let parent_resolved = physical_dir(&parent).map_err(|why2| {
                    Error::Fatal(format!(
                        "Cannot find canonical name of {}: unable to cd either to it ({why})\nor to its parent {}\n({})",
                        String::from_utf8_lossy(p),
                        String::from_utf8_lossy(&parent),
                        chdir_error(&parent, &why2)
                    ))
                })?;
                let bn = basename(p);
                if bn.is_empty() {
                    parent_resolved
                } else {
                    let name = Name::from_bytes(bn)?;
                    Fspath::from_local_string(&parent_resolved)?.child(&name).0
                }
            }
        };
        Fspath::from_local_string(&resolved)
    }

    /// `Fspath.child`.
    pub fn child(&self, n: &Name) -> Fspath {
        if is_root_dir(&self.0) {
            Fspath([&self.0[..], n.as_bytes()].concat())
        } else {
            Fspath([&self.0[..], b"/", n.as_bytes()].concat())
        }
    }

    /// `Fspath.concat`.
    pub fn concat<S>(&self, path: &Path<S>) -> Fspath {
        let p = path.as_bytes();
        if p.is_empty() {
            self.clone()
        } else if is_root_dir(&self.0) {
            Fspath([&self.0[..], p].concat())
        } else {
            Fspath([&self.0[..], b"/", p].concat())
        }
    }

    /// `Fspath.appleDouble`: the `._name` file that holds a file's
    /// resource fork on filesystems without one.
    pub fn apple_double(&self) -> Fspath {
        assert!(!is_root_dir(&self.0), "Fspath.appleDouble");
        let i = self
            .0
            .iter()
            .rposition(|&c| c == b'/')
            .expect("an absolute path")
            + 1;
        Fspath([&self.0[..i], b"._", &self.0[i..]].concat())
    }

    /// `Fspath.rsrc`: a file's resource fork on macOS.
    pub fn rsrc(&self) -> Fspath {
        assert!(!is_root_dir(&self.0), "Fspath.rsrc");
        Fspath([&self.0[..], b"/..namedfork/rsrc"].concat())
    }

    /// `Fspath.differentSuffix`: the shortest trailing parts that tell two
    /// roots apart, for display.
    pub fn different_suffix(&self, other: &Fspath) -> (Vec<u8>, Vec<u8>) {
        let (f1, f2) = (&self.0, &other.0);
        if is_root_dir(f1) || is_root_dir(f2) {
            return (f1.clone(), f2.clone());
        }
        let (len1, len2) = (f1.len() as isize, f2.len() as isize);
        let mut n: isize = 1;
        loop {
            let (i1, i2) = (len1 - n, len2 - n);
            if i1 < 0 || i2 < 0 || f1[i1 as usize] != f2[i2 as usize] {
                break;
            }
            n += 1;
        }
        let suffix = |f: &Vec<u8>, len: isize| -> Vec<u8> {
            if n > len {
                return f.clone();
            }
            match f[..=((len - n) as usize)].iter().rposition(|&c| c == b'/') {
                Some(j) => f[j + 1..].to_vec(),
                None => f.clone(),
            }
        };
        (suffix(f1, len1), suffix(f2, len2))
    }

    /// `Fspath.findWorkingDir`: the directory containing `path` and its
    /// name there. With `follow` (the path matches the `follow`
    /// preference), symbolic links are followed first, up to 100 deep.
    pub fn find_working_dir<S>(
        &self,
        path: &Path<S>,
        follow: bool,
    ) -> Result<(Fspath, Path<S>), Error> {
        let abspath = self.concat(path).0;
        let mut real = abspath.clone();
        if follow {
            let mut n = 0;
            loop {
                if n >= 100 {
                    return Err(Error::Transient(format!(
                        "Too many symbolic links from {}",
                        String::from_utf8_lossy(&abspath)
                    )));
                }
                let Ok(link) = system::readlink(&real) else {
                    break;
                };
                let relative =
                    !(link.starts_with(b"/") || (WIN_STYLE && is_root_dir_local_string(&link)));
                real = if relative {
                    let dir = win_safe_dirname(&real);
                    let joined = if dir.ends_with(b"/") {
                        [&dir[..], &link[..]].concat()
                    } else {
                        [&dir[..], b"/", &link[..]].concat()
                    };
                    if WIN_STYLE {
                        match Fspath::canonize(Some(&joined)) {
                            Ok(f) => f.0,
                            Err(_) => break,
                        }
                    } else {
                        joined
                    }
                } else {
                    link
                };
                n += 1;
            }
        }
        if is_root_dir_local_string(&real) {
            return Err(Error::Transient(format!(
                "The path {} is a root directory",
                String::from_utf8_lossy(&abspath)
            )));
        }
        let name = basename(&real);
        Ok((
            Fspath::from_local_string(&win_safe_dirname(&real))?,
            Path::from_bytes(&name)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(s: &str) -> Fspath {
        Fspath::from_local_string(s.as_bytes()).unwrap()
    }

    #[test]
    fn local_strings_and_roots() {
        assert_eq!(fs("/a/b//").as_bytes(), b"/a/b");
        assert_eq!(fs("///").as_bytes(), b"/");
        assert_eq!(fs("/").as_bytes(), b"/");
        assert!(Fspath::from_local_string(b"").is_err());
        assert!(is_root_dir(b"/"));
        assert_eq!(is_root_dir(b"C:/"), WIN_STYLE);
        assert_eq!(is_root_dir(b"//host/share/"), WIN_STYLE);
    }

    #[test]
    fn concat_child_and_suffixes() {
        let n = Name::from_bytes("x").unwrap();
        assert_eq!(fs("/a").child(&n).as_bytes(), b"/a/x");
        assert_eq!(fs("/").child(&n).as_bytes(), b"/x");
        let p: Path<gemel_base::path::Local> = Path::from_bytes(b"b/c").unwrap();
        assert_eq!(fs("/a").concat(&p).as_bytes(), b"/a/b/c");
        assert_eq!(fs("/a/b").apple_double().as_bytes(), b"/a/._b");
        assert_eq!(
            fs("/home/me/docs").different_suffix(&fs("/backup/me/docs")),
            (b"home/me/docs".to_vec(), b"backup/me/docs".to_vec())
        );
        assert_eq!(
            fs("/x/a").different_suffix(&fs("/x/b")),
            (b"a".to_vec(), b"b".to_vec())
        );
    }

    #[test]
    fn dirname_and_basename_follow_ocaml() {
        for (p, d, b) in [
            ("a/b", "a", "b"),
            ("a", ".", "a"),
            ("/a", "/", "a"),
            ("/", "/", ""),
            ("a/b/", "a", "b"),
            ("", ".", ""),
        ] {
            assert_eq!(dirname(p.as_bytes()), d.as_bytes(), "dirname {p:?}");
            assert_eq!(basename(p.as_bytes()), b.as_bytes(), "basename {p:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn canonize_resolves_links_in_directories() {
        let base = std::env::temp_dir().join(format!("gemel-fspath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let canon_base = std::fs::canonicalize(&base).unwrap();
        let b = |p: &std::path::Path| system::name_bytes(p.as_os_str());
        let c = Fspath::canonize(Some(&b(&base.join("link")))).unwrap();
        assert_eq!(c.as_bytes(), &b(&canon_base.join("real"))[..]);
        // A root that doesn't exist yet is named through its parent.
        let c = Fspath::canonize(Some(&b(&base.join("link/new")))).unwrap();
        assert_eq!(c.as_bytes(), &b(&canon_base.join("real/new"))[..]);
        let e = Fspath::canonize(Some(&b(&base.join("nope/new")))).unwrap_err();
        assert!(
            e.to_string().starts_with("Cannot find canonical name of "),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
