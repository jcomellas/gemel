//! Generated scenarios: an initial pair of replicas, a first sync, changes on
//! either side, and a second sync. Everything is derived from a seed, so a
//! failing scenario can be rerun by number.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// SplitMix64: small, fast and the same everywhere.
pub struct Rng(u64);

impl Rng {
    /// The generator for `seed`.
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// The next number.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// True `percent` times in a hundred.
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    /// One of `items`.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Names to build trees from. None differ only in case (macOS and Windows
/// filesystems are usually case-insensitive) or end in a dot or space
/// (Windows), and two are non-ASCII.
const NAMES: &[&str] = &[
    "a",
    "b",
    "c",
    "notes.txt",
    "main.o",
    "src",
    "doc",
    "with space",
    "café",
    "日本",
];

/// The extended attributes scenarios with them set and remove, as Unison
/// shows their names (on Linux, `user.gemel.a` and so on). Replicas'
/// snapshots show these and no others, since systems add their own.
pub const XATTR_NAMES: &[&str] = &["gemel.a", "gemel.b", "gemel.c"];

/// The prefix [`XATTR_NAMES`] share.
pub const XATTR_PREFIX: &str = "gemel.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
}

/// One change to a replica. Paths are relative, with `/` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // The fields say what they are.
pub enum Op {
    /// Makes a directory.
    Mkdir(String),
    /// Creates or overwrites a file and sets its modification time.
    Write {
        path: String,
        content: Vec<u8>,
        mtime: u64,
    },
    /// Deletes a file, link or whole directory.
    Delete(String),
    /// Renames a file, link or directory.
    Rename { from: String, to: String },
    /// Sets permissions; Unix only.
    Chmod { path: String, mode: u32 },
    /// Makes a symbolic link; Unix only.
    Symlink { path: String, target: String },
    /// Sets an extended attribute of a file or directory; only in
    /// scenarios with them.
    SetXattr {
        path: String,
        name: String,
        value: Vec<u8>,
    },
    /// Removes an extended attribute, if it is there.
    RemoveXattr { path: String, name: String },
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Op::Mkdir(p) => write!(f, "mkdir {p:?}"),
            Op::Write {
                path,
                content,
                mtime,
            } => {
                write!(f, "write {path:?} ({} bytes, mtime {mtime})", content.len())
            }
            Op::Delete(p) => write!(f, "delete {p:?}"),
            Op::Rename { from, to } => write!(f, "rename {from:?} -> {to:?}"),
            Op::Chmod { path, mode } => write!(f, "chmod {mode:o} {path:?}"),
            Op::Symlink { path, target } => write!(f, "symlink {path:?} -> {target:?}"),
            Op::SetXattr { path, name, value } => {
                write!(f, "setxattr {path:?} {name} ({} bytes)", value.len())
            }
            Op::RemoveXattr { path, name } => write!(f, "rmxattr {path:?} {name}"),
        }
    }
}

/// The model of a replica that ops are generated against.
#[derive(Default, Clone)]
struct Tree(BTreeMap<String, Kind>);

impl Tree {
    fn dirs(&self) -> Vec<String> {
        std::iter::once(String::new())
            .chain(
                self.0
                    .iter()
                    .filter(|(_, k)| **k == Kind::Dir)
                    .map(|(p, _)| p.clone()),
            )
            .collect()
    }

    fn of_kind(&self, kind: Kind) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, k)| **k == kind)
            .map(|(p, _)| p.clone())
            .collect()
    }

    fn join(dir: &str, name: &str) -> String {
        if dir.is_empty() {
            name.to_owned()
        } else {
            format!("{dir}/{name}")
        }
    }

    /// A name not yet used in `dir`, if there is one.
    fn fresh(&self, rng: &mut Rng, dir: &str) -> Option<String> {
        let free: Vec<String> = NAMES
            .iter()
            .map(|n| Self::join(dir, n))
            .filter(|p| !self.0.contains_key(p))
            .collect();
        (!free.is_empty()).then(|| rng.pick(&free).clone())
    }

    fn remove(&mut self, path: &str) {
        let prefix = format!("{path}/");
        self.0.retain(|p, _| p != path && !p.starts_with(&prefix));
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::Mkdir(p) => {
                self.0.insert(p.clone(), Kind::Dir);
            }
            Op::Write { path, .. } => {
                self.0.insert(path.clone(), Kind::File);
            }
            Op::Delete(p) => self.remove(p),
            Op::Rename { from, to } => {
                let kind = self.0[from];
                self.remove(from);
                self.0.insert(to.clone(), kind);
            }
            Op::Chmod { .. } | Op::SetXattr { .. } | Op::RemoveXattr { .. } => {}
            Op::Symlink { path, .. } => {
                self.0.insert(path.clone(), Kind::Link);
            }
        }
    }
}

/// Which filesystem a scenario is generated for. Windows scenarios have no
/// permission changes or symbolic links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    /// With permission changes and symbolic links.
    Unix,
    /// Without them.
    Windows,
}

impl Flavour {
    /// This system's flavour.
    pub fn host() -> Flavour {
        if cfg!(unix) {
            Flavour::Unix
        } else {
            Flavour::Windows
        }
    }

    /// Its name on the command line and in recordings.
    pub fn name(self) -> &'static str {
        match self {
            Flavour::Unix => "unix",
            Flavour::Windows => "windows",
        }
    }

    /// The flavour named `s`.
    pub fn parse(s: &str) -> Option<Flavour> {
        match s {
            "unix" => Some(Flavour::Unix),
            "windows" => Some(Flavour::Windows),
            _ => None,
        }
    }
}

/// Generates ops, keeping file modification times distinct and increasing.
struct Gen<'a> {
    rng: &'a mut Rng,
    clock: &'a mut u64,
    flavour: Flavour,
    /// Whether to set and remove extended attributes too.
    xattrs: bool,
}

impl Gen<'_> {
    fn content(&mut self) -> Vec<u8> {
        let len = self.rng.below(40);
        (0..len).map(|_| b"abcdefgh\n"[self.rng.below(9)]).collect()
    }

    fn mtime(&mut self) -> u64 {
        *self.clock += 10;
        *self.clock
    }

    fn write(&mut self, path: String) -> Op {
        Op::Write {
            path,
            content: self.content(),
            mtime: self.mtime(),
        }
    }

    /// Sets an attribute of `path`, to a value that is kept in the props
    /// (32 bytes or less), by its digest, or too long to cache (1024 bytes
    /// or more).
    fn set_xattr(&mut self, path: String) -> Op {
        let len = *self.rng.pick(&[0, 3, 32, 33, 100, 1500]);
        let c = b"xyz"[self.rng.below(3)];
        Op::SetXattr {
            path,
            name: (*self.rng.pick(XATTR_NAMES)).to_owned(),
            value: vec![c; len],
        }
    }

    /// An attribute op on a file or directory of `tree`, if it has one.
    fn xattr_op(&mut self, tree: &Tree) -> Option<Op> {
        let targets: Vec<String> = tree
            .0
            .iter()
            .filter(|(_, k)| **k != Kind::Link)
            .map(|(p, _)| p.clone())
            .collect();
        if targets.is_empty() {
            return None;
        }
        let path = self.rng.pick(&targets).clone();
        Some(if self.rng.chance(70) {
            self.set_xattr(path)
        } else {
            Op::RemoveXattr {
                path,
                name: (*self.rng.pick(XATTR_NAMES)).to_owned(),
            }
        })
    }

    /// Builds a random tree from nothing.
    fn initial(&mut self, tree: &mut Tree) -> Vec<Op> {
        let mut ops = Vec::new();
        for _ in 0..3 + self.rng.below(10) {
            let dirs = tree.dirs();
            let dir = self.rng.pick(&dirs).clone();
            if dir.matches('/').count() >= 2 {
                continue;
            }
            let Some(path) = tree.fresh(self.rng, &dir) else {
                continue;
            };
            let op = if self.rng.chance(30) {
                Op::Mkdir(path.clone())
            } else {
                self.write(path.clone())
            };
            tree.apply(&op);
            ops.push(op);
            if self.xattrs && self.rng.chance(50) {
                ops.push(self.set_xattr(path));
            }
        }
        ops
    }

    /// One random change to an existing tree, if one applies.
    fn change(&mut self, tree: &Tree) -> Option<Op> {
        let files = tree.of_kind(Kind::File);
        let dirs = tree.dirs();
        let any: Vec<String> = tree.0.keys().cloned().collect();
        let kinds = if self.flavour == Flavour::Unix { 8 } else { 6 };
        let roll = self.rng.below(kinds + if self.xattrs { 3 } else { 0 });
        if roll >= kinds {
            return self.xattr_op(tree);
        }
        Some(match roll {
            0 | 1 => {
                let dir = self.rng.pick(&dirs).clone();
                let path = tree.fresh(self.rng, &dir)?;
                self.write(path)
            }
            2 => {
                if files.is_empty() {
                    return None;
                }
                let f = self.rng.pick(&files).clone();
                self.write(f)
            }
            3 => {
                if any.is_empty() {
                    return None;
                }
                Op::Delete(self.rng.pick(&any).clone())
            }
            4 => {
                let dir = self.rng.pick(&dirs).clone();
                Op::Mkdir(tree.fresh(self.rng, &dir)?)
            }
            5 => {
                if files.is_empty() {
                    return None;
                }
                let from = self.rng.pick(&files).clone();
                let dir = self.rng.pick(&dirs).clone();
                Op::Rename {
                    to: tree.fresh(self.rng, &dir)?,
                    from,
                }
            }
            6 => {
                if any.is_empty() {
                    return None;
                }
                let path = self.rng.pick(&any).clone();
                if tree.0[&path] == Kind::Link {
                    return None;
                }
                let mode = *self.rng.pick(&[0o644, 0o600, 0o755, 0o640, 0o444]);
                // Keep directories traversable and writable, so cleanup works.
                let mode = if tree.0[&path] == Kind::Dir {
                    mode | 0o700
                } else {
                    mode
                };
                Op::Chmod { path, mode }
            }
            _ => {
                let dir = self.rng.pick(&dirs).clone();
                let target =
                    (*self.rng.pick(&["a", "../b", "nowhere", "src/notes.txt"])).to_owned();
                Op::Symlink {
                    path: tree.fresh(self.rng, &dir)?,
                    target,
                }
            }
        })
    }

    fn changes(&mut self, tree: &mut Tree, n: usize) -> Vec<Op> {
        let mut ops = Vec::new();
        for _ in 0..n * 3 {
            if ops.len() == n {
                break;
            }
            if let Some(op) = self.change(tree) {
                tree.apply(&op);
                ops.push(op);
            }
        }
        ops
    }
}

/// A complete scenario.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// The seed it was generated from.
    pub seed: u64,
    /// Replica `a` before the first sync.
    pub initial_a: Vec<Op>,
    /// Replica `b` before the first sync.
    pub initial_b: Vec<Op>,
    /// What changes on `a` between the two syncs.
    pub changes_a: Vec<Op>,
    /// What changes on `b` between the two syncs.
    pub changes_b: Vec<Op>,
}

impl Scenario {
    /// The scenario for `seed`, for a filesystem of `flavour`.
    pub fn generate(seed: u64, flavour: Flavour) -> Scenario {
        Self::generate_with(seed, flavour, false)
    }

    /// [`Scenario::generate`], with extended attributes set and removed
    /// too if `xattrs` (Unix only).
    pub fn generate_with(seed: u64, flavour: Flavour, xattrs: bool) -> Scenario {
        let mut rng = Rng::new(seed);
        // Start well in the past, so no time is within a second of the syncs.
        let mut clock = 1_600_000_000;
        let mut g = Gen {
            rng: &mut rng,
            clock: &mut clock,
            flavour,
            xattrs,
        };
        let mut a = Tree::default();
        let initial_a = g.initial(&mut a);
        // b starts empty, as a copy of a, or as a copy of a with changes of
        // its own (conflicts on the first sync).
        let mut b = Tree::default();
        let initial_b = match g.rng.below(3) {
            0 => Vec::new(),
            1 => {
                b = a.clone();
                initial_a.clone()
            }
            _ => {
                b = a.clone();
                let mut ops = initial_a.clone();
                let n = 1 + g.rng.below(3);
                ops.extend(g.changes(&mut b, n));
                ops
            }
        };
        // After the first sync both replicas hold the union of the two, as
        // far as the model is concerned; generate changes against that.
        let mut synced = a.clone();
        for (p, k) in &b.0 {
            synced.0.entry(p.clone()).or_insert(*k);
        }
        let (mut sa, mut sb) = (synced.clone(), synced);
        let n = g.rng.below(6);
        let changes_a = g.changes(&mut sa, n);
        let n = g.rng.below(6);
        let changes_b = g.changes(&mut sb, n);
        Scenario {
            seed,
            initial_a,
            initial_b,
            changes_a,
            changes_b,
        }
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "scenario {}", self.seed)?;
        for (title, ops) in [
            ("initial a", &self.initial_a),
            ("initial b", &self.initial_b),
            ("changes a", &self.changes_a),
            ("changes b", &self.changes_b),
        ] {
            writeln!(f, "  {title}:")?;
            for op in ops {
                writeln!(f, "    {op}")?;
            }
        }
        Ok(())
    }
}

fn native(root: &Path, rel: &str) -> std::path::PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
}

/// Applies ops to a replica on disk. Ops generated for a model that has
/// drifted from the disk (after a sync resolved a conflict differently)
/// may fail; those are skipped, the same way for every implementation.
pub fn apply(root: &Path, ops: &[Op]) -> io::Result<()> {
    fs::create_dir_all(root)?;
    for op in ops {
        let _ = apply_one(root, op);
    }
    Ok(())
}

fn apply_one(root: &Path, op: &Op) -> io::Result<()> {
    match op {
        Op::Mkdir(p) => fs::create_dir(native(root, p)),
        Op::Write {
            path,
            content,
            mtime,
        } => {
            let p = native(root, path);
            fs::write(&p, content)?;
            let f = fs::OpenOptions::new().write(true).open(&p)?;
            f.set_modified(UNIX_EPOCH + Duration::from_secs(*mtime))
        }
        Op::Delete(p) => {
            let p = native(root, p);
            if fs::symlink_metadata(&p)?.is_dir() {
                fs::remove_dir_all(p)
            } else {
                fs::remove_file(p)
            }
        }
        Op::Rename { from, to } => fs::rename(native(root, from), native(root, to)),
        #[cfg(unix)]
        Op::Chmod { path, mode } => {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(native(root, path), fs::Permissions::from_mode(*mode))
        }
        #[cfg(unix)]
        Op::Symlink { path, target } => std::os::unix::fs::symlink(target, native(root, path)),
        #[cfg(not(unix))]
        Op::Chmod { .. } | Op::Symlink { .. } => Ok(()),
        Op::SetXattr { path, name, value } => {
            gemel_fs::xattr::set(&fspath(root, path)?, name.as_bytes(), value).map_err(xattr_error)
        }
        Op::RemoveXattr { path, name } => {
            gemel_fs::xattr::remove(&fspath(root, path)?, name.as_bytes()).map_err(xattr_error)
        }
    }
}

/// `rel` under `root`, as gemel-fs takes it.
fn fspath(root: &Path, rel: &str) -> io::Result<gemel_fs::fspath::Fspath> {
    fspath_of(&native(root, rel))
}

/// `p`, as gemel-fs takes it.
pub fn fspath_of(p: &Path) -> io::Result<gemel_fs::fspath::Fspath> {
    let bytes = gemel_fs::system::name_bytes(p.as_os_str());
    gemel_fs::fspath::Fspath::canonize(Some(&bytes)).map_err(|e| io::Error::other(e.to_string()))
}

/// An attribute operation's error, as an I/O error.
pub fn xattr_error(e: gemel_fs::xattr::Error) -> io::Error {
    io::Error::other(format!("{e:?}"))
}

/// Seconds since the epoch, for messages.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_scenarios_have_no_modes_or_links() {
        for seed in 0..200 {
            let s = Scenario::generate(seed, Flavour::Windows);
            for op in s
                .initial_a
                .iter()
                .chain(&s.initial_b)
                .chain(&s.changes_a)
                .chain(&s.changes_b)
            {
                assert!(!matches!(op, Op::Chmod { .. } | Op::Symlink { .. }), "{op}");
            }
        }
    }

    #[test]
    fn xattrs_change_only_scenarios_with_them() {
        let ops = |s: &Scenario| {
            s.initial_a
                .iter()
                .chain(&s.initial_b)
                .chain(&s.changes_a)
                .chain(&s.changes_b)
                .cloned()
                .collect::<Vec<_>>()
        };
        let is_xattr = |op: &Op| matches!(op, Op::SetXattr { .. } | Op::RemoveXattr { .. });
        let mut with = 0;
        for seed in 0..100 {
            let plain = Scenario::generate(seed, Flavour::Unix);
            assert!(!ops(&plain).iter().any(is_xattr));
            let s = Scenario::generate_with(seed, Flavour::Unix, true);
            with += ops(&s).iter().filter(|op| is_xattr(op)).count();
        }
        assert!(with > 100, "{with}");
    }

    #[test]
    fn generation_is_deterministic() {
        for seed in 0..50 {
            for f in [Flavour::Unix, Flavour::Windows] {
                let (s1, s2) = (Scenario::generate(seed, f), Scenario::generate(seed, f));
                assert_eq!(s1.to_string(), s2.to_string());
            }
        }
        assert_ne!(
            Scenario::generate(1, Flavour::Unix).to_string(),
            Scenario::generate(2, Flavour::Unix).to_string()
        );
    }
}
