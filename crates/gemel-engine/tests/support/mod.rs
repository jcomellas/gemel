//! Helpers shared by the archive and update-detection tests.

#![allow(dead_code)]

pub mod oracle;
pub mod sync;

use std::path::{Path as StdPath, PathBuf};

use gemel_base::case::{self, Mode, Settings};
use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_fs::system;

/// How a suite's client reaches its roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// Both roots here, their commands called directly.
    Direct,
    /// Both roots here, their commands sent over an in-memory connection to
    /// a separate host, as a server would get them.
    Loopback,
    /// Root `b` on another host, "server", behind an in-memory connection
    /// (as `ssh://server//...`); root `a` here.
    Remote,
}

impl Via {
    /// Every way.
    pub const ALL: [Via; 3] = [Via::Direct, Via::Loopback, Via::Remote];
}

impl std::fmt::Display for Via {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Via::Direct => "direct",
            Via::Loopback => "loopback",
            Via::Remote => "remote",
        })
    }
}

/// The host name of [`Via::Remote`]'s server.
pub const SERVER: &str = "server";

/// A directory removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("gemel-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.0).is_err() {
            // A test left a directory read-only.
            #[cfg(unix)]
            fn writable(d: &StdPath) {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(d, PermissionsExt::from_mode(0o755));
                for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        writable(&e.path());
                    }
                }
            }
            #[cfg(unix)]
            writable(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Runs `f` on this thread's current-thread runtime, as the engine thread
/// does: tasks one call starts (a loopback connection's) live on into the
/// next.
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    thread_local! {
        static ENGINE: (tokio::runtime::Runtime, tokio::task::LocalSet) = (
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
            tokio::task::LocalSet::new(),
        );
    }
    ENGINE.with(|(rt, local)| local.block_on(rt, f))
}

/// Every preference the engine reads.
pub fn new_prefs() -> Prefs {
    let mut p = Prefs::new();
    gemel_base::trace::register_prefs(&mut p).unwrap();
    case::register_prefs(&mut p).unwrap();
    gemel_fs::os::register_prefs(&mut p).unwrap();
    gemel_fs::fileinfo::register_prefs(&mut p).unwrap();
    gemel_engine::update::register_props_prefs(&mut p).unwrap();
    gemel_fs::osx::register_prefs(&mut p).unwrap();
    gemel_engine::register_prefs(&mut p).unwrap();
    p
}

pub fn new_features() -> Features {
    let mut f = Features::new();
    gemel_engine::register_features(&mut f).unwrap();
    f
}

/// Features with xattr and ACL support enabled, as both sides of an OCaml
/// 2.54 sync negotiate them.
pub fn enabled_features() -> Features {
    let mut f = new_features();
    let all = f.all();
    f.set_enabled(&all);
    f
}

/// The pseudo-preferences that select `mode`.
pub fn settings(mode: Mode) -> Settings {
    Settings {
        some_host_is_insensitive: matches!(mode, Mode::Insensitive | Mode::UnicodeInsensitive),
        unicode_encoding: mode == Mode::UnicodeInsensitive,
        unicode_case_sensitive: mode == Mode::UnicodeSensitive,
    }
}

pub fn mode_of_desc(desc: &str) -> Mode {
    [
        Mode::Sensitive,
        Mode::Insensitive,
        Mode::UnicodeSensitive,
        Mode::UnicodeInsensitive,
    ]
    .into_iter()
    .find(|m| m.desc() == desc)
    .unwrap_or_else(|| panic!("unknown case mode {desc:?}"))
}

/// Sets a file's or directory's modification time.
pub fn set_mtime(p: &StdPath, t: f64) {
    system::utimes(&system::name_bytes(p.as_os_str()), t, t).unwrap();
}

/// Writes a file (creating its directory) with a modification time.
pub fn write(root: &StdPath, rel: &str, content: &str, t: f64) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
    set_mtime(&p, t);
}

/// Gives every directory a new modification time. A directory changed in
/// the second its last scan saw keeps its time, and fast checking skips it
/// (as in OCaml); this makes scans look inside.
pub fn settle(roots: &[&StdPath]) {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = sync::T0 + 1000.0 + 10.0 * f64::from(n);
    fn walk(d: &StdPath, t: f64) {
        for e in std::fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), t);
            }
        }
        set_mtime(d, t);
    }
    for r in roots {
        walk(r, t);
    }
}

pub fn read(p: &StdPath) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The files under `dir`, with their contents (`->target` for links).
pub fn listing(dir: &StdPath) -> Vec<(String, String)> {
    fn walk(root: &StdPath, d: &StdPath, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let t = e.file_type().unwrap();
            if t.is_symlink() {
                out.push((
                    rel,
                    format!("->{}", std::fs::read_link(&p).unwrap().display()),
                ));
            } else if t.is_dir() {
                out.push((format!("{rel}/"), String::new()));
                walk(root, &p, out);
            } else {
                out.push((rel, read(&p)));
            }
        }
    }
    let mut out = Vec::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}
