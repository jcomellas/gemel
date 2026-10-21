//! What the watchers have in common: the directories watched for each
//! replica, the changes seen in them, and the protocol's commands.
//!
//! Ports Unison `src/fsmonitor/watchercommon.ml`.
//!
//! Derived from Unison, Copyright 2012, Benjamin C. Pierce.
//!
//! There are no system calls here: watching goes through a [`Backend`],
//! events come in as [`Event`]s, and the time is passed to each call, so
//! the tests can run it all without a filesystem or a clock. Answers are
//! collected in [`Core::out`], and [`Core::timer`] says when [`Core::tick`]
//! is due (OCaml's `Lwt_unix.sleep delay`).
//!
//! Divergence: OCaml finds the directory an event concerns from the
//! inotify watch it came from, or, on Windows, from the path below the
//! root. Here every backend gives paths. On Linux and macOS an event
//! concerns the watched directory holding the path and, if the path is
//! itself watched, that directory ([`Resolve::Watched`]); on Windows it
//! follows OCaml's Windows watcher ([`Resolve::BelowRoot`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use gemel_compat::fsmonitor_protocol::{Command, ParseError, Response, split_on_space};

/// Changes are reported once they are this old, in seconds, so that a
/// burst of them is reported together.
pub const DELAY: f64 = 0.5;

/// A watched directory (OCaml's `t`), by number.
pub type FileId = usize;

/// What an event did to a name (OCaml's `` `CREAT ``, `` `MODIF `` and
/// `` `DEL ``; Windows' `` `MOVED `` counts as a modification).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Creat,
    Modif,
    Del,
}

/// What a backend saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Something happened to this path.
    Path(PathBuf, Kind),
    /// Events were lost (inotify's `Q_OVERFLOW`, an empty Windows
    /// notification, FSEvents' rescan flags, or a backend error).
    Overflow,
}

/// Why a watch couldn't be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchError {
    /// The path is gone (OCaml's `Already_lost`).
    Lost,
    /// Unison reports this itself when it scans the path (`EACCES`,
    /// `ENOTDIR`, `ELOOP`): no watch, and no error.
    Unwatchable,
    /// The watcher stops with this message.
    Fatal(String),
}

/// Adding and removing watches (OCaml's `add_watch` and `release_watch`).
pub trait Backend {
    /// Starts watching `path` for `file`; `root` says whether it is where
    /// a replica's scan starts, and `follow` whether it is a symbolic link
    /// Unison followed.
    fn add_watch(&mut self, path: &Path, root: bool, follow: bool) -> Result<(), WatchError>;
    /// Stops watching `path`, which `add_watch` was given.
    fn release_watch(&mut self, path: &Path, root: bool);
    /// How events are matched with watched directories.
    fn resolve(&self) -> Resolve;
    /// The long form of `rel` below `root` (Windows' `longpathname`).
    fn long_name(&self, _root: &Path, _rel: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
        None
    }
}

/// How an event's path is matched with the directories watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolve {
    /// Each watched directory reports its children (inotify), or each root
    /// reports everything below it but only watched directories count
    /// (FSEvents).
    Watched,
    /// Each root reports everything below it, and an event in a directory
    /// that isn't known counts as a change to the nearest one that is
    /// (`fsmonitor/windows/watcher.ml`).
    BelowRoot,
}

/// The watcher must stop: it sends `ERROR msg` and exits with code 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fatal(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Modified,
    Created,
}

#[derive(Debug, Clone)]
enum Parent {
    Root { fspath: Vec<u8>, path: Vec<u8> },
    Dir { name: Vec<u8>, dir: FileId },
}

#[derive(Debug)]
struct File {
    generation: i64,
    /// The path watched, if any.
    watch: Option<PathBuf>,
    subdirs: BTreeMap<Vec<u8>, FileId>,
    parent: Parent,
    archive_hash: Vec<u8>,
    changed: bool,
    changed_children: BTreeMap<Vec<u8>, (Status, f64)>,
}

/// A replica's root: the archive hash, the replica's root and where the
/// scan started below it.
type RootKey = (Vec<u8>, Vec<u8>, Vec<u8>);

/// When each name changed in each directory, for one replica.
type ChangeTable = HashMap<(FileId, Vec<u8>), f64>;

/// An unfinished `START`: the directory it started at, and its location.
struct Scan {
    start: FileId,
    fspath: Vec<u8>,
}

/// The watcher's state.
pub struct Core<B> {
    pub backend: B,
    files: HashMap<FileId, File>,
    last_file_id: FileId,
    roots: BTreeMap<RootKey, FileId>,
    current_gen: i64,
    /// For each replica, the names changed in each directory, and when.
    changes: BTreeMap<Vec<u8>, ChangeTable>,
    waiting_for_changes: BTreeSet<Vec<u8>>,
    active_wait: bool,
    /// The files watching each path.
    watched: HashMap<PathBuf, BTreeSet<FileId>>,
    scan: Option<Scan>,
    previous_event: Option<Event>,
    pub debug: bool,
    /// When [`Core::tick`] is due.
    pub timer: Option<f64>,
    /// What to send to Unison.
    pub out: Vec<u8>,
}

/// `Filename.concat`, on this system.
fn filename_concat(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let sep = |c: u8| {
        if cfg!(windows) {
            matches!(c, b'/' | b'\\' | b':')
        } else {
            c == b'/'
        }
    };
    let mut out = dir.to_vec();
    if !(dir.is_empty() || sep(dir[dir.len() - 1])) {
        out.push(if cfg!(windows) { b'\\' } else { b'/' });
    }
    out.extend_from_slice(name);
    out
}

/// `concat`: `path` below `fspath`, which it is if `path` is empty.
pub fn concat(fspath: &[u8], path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        fspath.to_vec()
    } else {
        filename_concat(fspath, path)
    }
}

/// A protocol path as a path of this system.
pub fn os_path(p: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(p))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(p).into_owned())
    }
}

/// A path's components as protocol names.
fn names(p: &Path) -> Vec<Vec<u8>> {
    p.components()
        .map(|c| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                c.as_os_str().as_bytes().to_vec()
            }
            #[cfg(not(unix))]
            {
                c.as_os_str().to_string_lossy().into_owned().into_bytes()
            }
        })
        .collect()
}

fn show(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// OCaml's `format_exc` of the exceptions a bad line raises.
fn uncaught(e: &str) -> Fatal {
    Fatal(format!(
        "error while communicating with Unison: uncaugth exception {e}\n"
    ))
}

impl<B: Backend> Core<B> {
    pub fn new(backend: B) -> Self {
        Core {
            backend,
            files: HashMap::new(),
            last_file_id: 0,
            roots: BTreeMap::new(),
            current_gen: 0,
            changes: BTreeMap::new(),
            waiting_for_changes: BTreeSet::new(),
            active_wait: false,
            watched: HashMap::new(),
            scan: None,
            previous_event: None,
            debug: false,
            timer: None,
            out: Vec::new(),
        }
    }

    fn file(&self, id: FileId) -> &File {
        &self.files[&id]
    }

    fn file_mut(&mut self, id: FileId) -> &mut File {
        self.files.get_mut(&id).expect("a live file")
    }

    fn is_root(&self, id: FileId) -> bool {
        matches!(self.file(id).parent, Parent::Root { .. })
    }

    /// `dir_path`: where a directory is, with `path` below it.
    fn dir_path(&self, id: FileId, path: &[u8]) -> Vec<u8> {
        match &self.file(id).parent {
            Parent::Root { fspath, path: p } => concat(fspath, &concat(p, path)),
            Parent::Dir { name, dir } => self.dir_path(*dir, &concat(name, path)),
        }
    }

    fn send(&mut self, r: Response) {
        self.out.extend(r.to_line());
    }

    // ---- Changes ----

    fn change_table(&mut self, hash: &[u8]) -> &mut ChangeTable {
        self.changes.entry(hash.to_vec()).or_default()
    }

    fn signal_changes(&mut self, replicas: BTreeSet<Vec<u8>>) {
        self.waiting_for_changes.clear();
        self.send(Response::Changes(replicas.into_iter().collect()));
    }

    fn replicas_with_changes(&self, watched: &BTreeSet<Vec<u8>>, time: f64) -> BTreeSet<Vec<u8>> {
        let mut changed: BTreeSet<Vec<u8>> = self
            .roots
            .iter()
            .filter(|((hash, _, _), r)| self.file(**r).changed && watched.contains(hash))
            .map(|((hash, _, _), _)| hash.clone())
            .collect();
        for hash in watched {
            if !changed.contains(hash)
                && self
                    .changes
                    .get(hash)
                    .is_some_and(|t| t.values().any(|t| time - t > DELAY))
            {
                changed.insert(hash.clone());
            }
        }
        changed
    }

    fn has_impending_changes(&self, watched: &BTreeSet<Vec<u8>>) -> bool {
        watched
            .iter()
            .any(|h| self.changes.get(h).is_some_and(|t| !t.is_empty()))
    }

    fn wait_for_changes(&mut self, watched: BTreeSet<Vec<u8>>, time: f64) {
        if !watched.is_empty() {
            let changed = self.replicas_with_changes(&watched, time);
            if !changed.is_empty() {
                self.signal_changes(changed);
            } else if self.has_impending_changes(&watched) {
                self.signal_impending_changes(time);
            }
        }
    }

    fn signal_impending_changes(&mut self, time: f64) {
        if !(self.waiting_for_changes.is_empty() || self.active_wait) {
            self.active_wait = true;
            self.timer = Some(time + DELAY);
        }
    }

    /// The end of `signal_impending_changes`'s sleep.
    pub fn tick(&mut self, time: f64) {
        if self.timer.is_some_and(|t| t <= time) {
            self.timer = None;
            self.active_wait = false;
            let waiting = self.waiting_for_changes.clone();
            self.wait_for_changes(waiting, time);
        }
    }

    fn wait(&mut self, hash: Vec<u8>, time: f64) {
        self.waiting_for_changes.insert(hash.clone());
        self.wait_for_changes(BTreeSet::from([hash]), time);
    }

    fn add_change(&mut self, dir: FileId, nm: &[u8], time: f64) {
        let hash = self.file(dir).archive_hash.clone();
        self.change_table(&hash).insert((dir, nm.to_vec()), time);
        self.signal_impending_changes(time);
    }

    fn remove_change(&mut self, dir: FileId, nm: &[u8]) {
        let hash = self.file(dir).archive_hash.clone();
        self.change_table(&hash).remove(&(dir, nm.to_vec()));
    }

    fn clear_changes(&mut self, hash: &[u8], time: f64) {
        let roots: Vec<FileId> = self
            .roots
            .iter()
            .filter(|((h, _, _), _)| h == hash)
            .map(|(_, r)| *r)
            .collect();
        for r in roots {
            self.file_mut(r).changed = false;
            self.clear_rec(r, time);
        }
    }

    fn clear_rec(&mut self, f: FileId, time: f64) {
        let old: Vec<Vec<u8>> = self
            .file(f)
            .changed_children
            .iter()
            .filter(|(_, (_, t))| time - t > DELAY)
            .map(|(nm, _)| nm.clone())
            .collect();
        for nm in old {
            self.file_mut(f).changed_children.remove(&nm);
            self.remove_change(f, &nm);
        }
        let subdirs: Vec<FileId> = self.file(f).subdirs.values().copied().collect();
        for s in subdirs {
            self.clear_rec(s, time);
        }
    }

    /// `signal_change`: `nm` in `dir` (or `dir` itself) changed.
    fn signal_change(&mut self, time: f64, dir: FileId, nm: Option<&[u8]>, kind: Kind) {
        match nm {
            Some(nm) => {
                let generation = self.file(dir).generation;
                let current = self.current_gen;
                match self.file(dir).changed_children.get(nm).copied() {
                    Some((st, _)) => {
                        if st == Status::Created
                            && kind == Kind::Del
                            && !self.file(dir).subdirs.contains_key(nm)
                        {
                            if self.debug {
                                eprintln!("Deleted: {}", show(&self.dir_path(dir, nm)));
                            }
                            self.file_mut(dir).changed_children.remove(nm);
                            self.remove_change(dir, nm);
                        } else {
                            self.file_mut(dir)
                                .changed_children
                                .insert(nm.to_vec(), (st, time));
                            self.add_change(dir, nm, time);
                        }
                    }
                    None => {
                        let st = if kind == Kind::Creat && generation != current {
                            if self.debug {
                                eprintln!("Created: {}", show(&self.dir_path(dir, nm)));
                            }
                            Status::Created
                        } else {
                            if self.debug {
                                eprintln!("Modified: {}", show(&self.dir_path(dir, nm)));
                            }
                            Status::Modified
                        };
                        self.file_mut(dir)
                            .changed_children
                            .insert(nm.to_vec(), (st, time));
                        self.add_change(dir, nm, time);
                    }
                }
            }
            None => match self.file(dir).parent.clone() {
                Parent::Root { .. } => {
                    self.file_mut(dir).changed = true;
                    self.signal_impending_changes(time);
                }
                Parent::Dir { name, dir: parent } => {
                    self.signal_change(time, parent, Some(&name), kind)
                }
            },
        }
    }

    fn signal_overflow(&mut self) {
        let roots: Vec<FileId> = self.roots.values().copied().collect();
        for r in roots {
            self.file_mut(r).changed = true;
        }
        if !self.waiting_for_changes.is_empty() {
            let waiting = self.waiting_for_changes.clone();
            self.signal_changes(waiting);
        }
    }

    // ---- Events ----

    /// An event from the backend.
    pub fn event(&mut self, ev: Event, time: f64) {
        // Identical events in a row are reported once (inotify repeats
        // them).
        if self.previous_event.as_ref() == Some(&ev) {
            return;
        }
        self.previous_event = Some(ev.clone());
        match ev {
            Event::Overflow => {
                if self.debug {
                    eprintln!("OVERFLOW");
                }
                self.signal_overflow();
            }
            Event::Path(p, kind) => match self.backend.resolve() {
                Resolve::Watched => {
                    let name = p.file_name().map(|n| names(Path::new(n)).concat());
                    if let (Some(parent), Some(name)) = (p.parent(), name) {
                        for f in self.watching(parent) {
                            self.signal_change(time, f, Some(&name), kind);
                        }
                    }
                    for f in self.watching(&p) {
                        self.signal_change(time, f, None, kind);
                    }
                }
                Resolve::BelowRoot => self.event_below_roots(&p, kind, time),
            },
        }
    }

    fn watching(&self, p: &Path) -> Vec<FileId> {
        self.watched
            .get(p)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// OCaml's Windows watcher: each root's watch reports paths below it.
    fn event_below_roots(&mut self, p: &Path, kind: Kind, time: f64) {
        let roots: Vec<FileId> = self.roots.values().copied().collect();
        for r in roots {
            let Some(root_path) = self.file(r).watch.clone() else {
                continue;
            };
            let Ok(rel) = p.strip_prefix(&root_path) else {
                continue;
            };
            let rel = names(rel);
            if rel.is_empty() {
                continue;
            }
            // `get_win_path`: the event's own directory if every directory
            // above it is known (in long form), else the nearest known
            // directory, as modified. A deleted name can't be expanded.
            let exact = if kind == Kind::Del {
                None
            } else {
                let long = self
                    .backend
                    .long_name(&root_path, &rel)
                    .unwrap_or(rel.clone());
                self.follow_names(r, &long[..long.len() - 1])
                    .map(|d| (d, long[long.len() - 1].clone()))
            };
            match exact {
                Some((dir, nm)) => self.signal_change(time, dir, Some(&nm), kind),
                None => {
                    let dir = self.nearest(r, &root_path, &rel[..rel.len() - 1]);
                    self.signal_change(time, dir, None, Kind::Modif);
                }
            }
        }
    }

    fn follow_names(&self, mut dir: FileId, names: &[Vec<u8>]) -> Option<FileId> {
        for nm in names {
            dir = *self.file(dir).subdirs.get(nm)?;
        }
        Some(dir)
    }

    /// `follow_win_path_parent`: the deepest known directory along `names`,
    /// trying each name as it is and then in long form.
    fn nearest(&self, mut dir: FileId, root: &Path, names: &[Vec<u8>]) -> FileId {
        let mut at = root.to_path_buf();
        for nm in names {
            let next = self.file(dir).subdirs.get(nm).copied().or_else(|| {
                let long = self.backend.long_name(&at, std::slice::from_ref(nm))?;
                self.file(dir).subdirs.get(long.last()?).copied()
            });
            match next {
                Some(d) => {
                    at.push(os_path(nm));
                    dir = d;
                }
                None => break,
            }
        }
        dir
    }

    // ---- The tree ----

    fn new_file(&mut self, hash: &[u8], parent: Parent) -> FileId {
        let id = self.last_file_id;
        self.last_file_id += 1;
        self.files.insert(
            id,
            File {
                generation: -1,
                watch: None,
                subdirs: BTreeMap::new(),
                parent,
                archive_hash: hash.to_vec(),
                changed: false,
                changed_children: BTreeMap::new(),
            },
        );
        id
    }

    fn new_root(&mut self, hash: &[u8], fspath: &[u8], path: &[u8]) -> FileId {
        if self.debug {
            eprintln!("ROOT {} {}", show(fspath), show(path));
        }
        let r = self.new_file(
            hash,
            Parent::Root {
                fspath: fspath.to_vec(),
                path: path.to_vec(),
            },
        );
        self.roots
            .insert((hash.to_vec(), fspath.to_vec(), path.to_vec()), r);
        r
    }

    fn dir_child(&mut self, dir: FileId, name: &[u8]) -> FileId {
        if let Some(d) = self.file(dir).subdirs.get(name) {
            return *d;
        }
        let hash = self.file(dir).archive_hash.clone();
        let d = self.new_file(
            &hash,
            Parent::Dir {
                name: name.to_vec(),
                dir,
            },
        );
        self.file_mut(dir).subdirs.insert(name.to_vec(), d);
        d
    }

    fn find_root(&self, hash: &[u8], fspath: &[u8], path: &[u8]) -> Option<Vec<u8>> {
        let key = (hash.to_vec(), fspath.to_vec(), path.to_vec());
        if self.roots.contains_key(&key) {
            Some(path.to_vec())
        } else if let Some(i) = path.iter().rposition(|&c| c == b'/') {
            self.find_root(hash, fspath, &path[..i])
        } else if path.is_empty() {
            None
        } else {
            self.find_root(hash, fspath, b"")
        }
    }

    /// `follow_path`, for `DIR` and `LINK`: every directory above the last
    /// one must already be known.
    fn follow_path(&mut self, mut dir: FileId, path: &[u8]) -> Result<FileId, Fatal> {
        if path.is_empty() {
            return Ok(dir);
        }
        let parts: Vec<&[u8]> = path.split(|&c| c == b'/').collect();
        let (last, above) = parts.split_last().expect("split gives one part");
        for nm in above {
            dir = *self
                .file(dir)
                .subdirs
                .get(*nm)
                .ok_or_else(|| uncaught("Assert_failure(\"watchercommon.ml\", 434, 6)"))?;
        }
        Ok(self.dir_child(dir, last))
    }

    /// `follow_fspath`: a directory not known yet on the way starts a new
    /// root.
    fn follow_fspath(
        &mut self,
        hash: &[u8],
        fspath: &[u8],
        mut dir: FileId,
        path: &[u8],
        mut pos: usize,
    ) -> FileId {
        loop {
            match path[pos..].iter().position(|&c| c == b'/') {
                Some(n) => match self.file(dir).subdirs.get(&path[pos..pos + n]) {
                    Some(d) => {
                        dir = *d;
                        pos += n + 1;
                    }
                    None => return self.new_root(hash, fspath, path),
                },
                None => return self.dir_child(dir, &path[pos..]),
            }
        }
    }

    fn find_start(&mut self, hash: &[u8], fspath: &[u8], path: &[u8]) -> FileId {
        match self.find_root(hash, fspath, path) {
            None => self.new_root(hash, fspath, path),
            Some(root_path) => {
                let root = self.roots[&(hash.to_vec(), fspath.to_vec(), root_path.clone())];
                if path == root_path.as_slice() {
                    root
                } else {
                    let pos = if root_path.is_empty() {
                        0
                    } else {
                        root_path.len() + 1
                    };
                    self.follow_fspath(hash, fspath, root, path, pos)
                }
            }
        }
    }

    fn clear_file_changes(&mut self, f: FileId) {
        let names: Vec<Vec<u8>> = self.file(f).changed_children.keys().cloned().collect();
        for nm in names {
            self.remove_change(f, &nm);
        }
        let file = self.file_mut(f);
        file.changed_children.clear();
        file.changed = false;
    }

    fn set_watch(&mut self, f: FileId, path: Option<PathBuf>) {
        if let Some(old) = self.file_mut(f).watch.take()
            && let Some(s) = self.watched.get_mut(&old)
        {
            s.remove(&f);
            if s.is_empty() {
                self.watched.remove(&old);
            }
        }
        if let Some(p) = &path {
            self.watched.entry(p.clone()).or_default().insert(f);
        }
        self.file_mut(f).watch = path;
    }

    fn release_watch(&mut self, f: FileId) {
        let root = self.is_root(f);
        if let Some(p) = self.file(f).watch.clone() {
            self.set_watch(f, None);
            // Another file may watch the same path.
            if !self.watched.contains_key(&p) {
                self.backend.release_watch(&p, root);
            }
        }
    }

    fn remove_file(&mut self, f: FileId) {
        if self.debug {
            eprintln!("REMOVING {}", show(&self.dir_path(f, b"")));
        }
        let subdirs: Vec<FileId> = self.file(f).subdirs.values().copied().collect();
        for s in subdirs {
            self.remove_file(s);
        }
        self.release_watch(f);
        self.clear_file_changes(f);
        if let Some(file) = self.files.remove(&f)
            && let Parent::Dir { name, dir } = file.parent
            && let Some(p) = self.files.get_mut(&dir)
        {
            p.subdirs.remove(&name);
        }
    }

    fn remove_old_files(&mut self, f: FileId) {
        if self.file(f).generation != self.current_gen {
            self.remove_file(f);
        } else {
            let subdirs: Vec<FileId> = self.file(f).subdirs.values().copied().collect();
            for s in subdirs {
                self.remove_old_files(s);
            }
            let file = self.file(f);
            if file.watch.is_none() && file.subdirs.is_empty() && !self.is_root(f) {
                self.remove_file(f);
            }
        }
    }

    /// `watch_path`.
    ///
    /// The backend is asked again for a path already watched, as OCaml's
    /// inotify watcher asks inotify: the directory there may be a new one,
    /// which the old watch doesn't see.
    fn watch_path(&mut self, path: &[u8], f: FileId, follow: bool) -> Result<(), Fatal> {
        let p = os_path(path);
        let same = self.file(f).watch.as_ref() == Some(&p);
        let root = self.is_root(f);
        let r = self.backend.add_watch(&p, root, follow);
        if r.is_err() || !same {
            self.release_watch(f);
        }
        match r {
            Ok(()) => {
                if !same {
                    self.set_watch(f, Some(p));
                }
                Ok(())
            }
            Err(WatchError::Unwatchable) => Ok(()),
            Err(WatchError::Fatal(m)) => Err(Fatal(m)),
            Err(WatchError::Lost) if root => {
                Err(Fatal(format!("Path '{}' does not exist", show(path))))
            }
            Err(WatchError::Lost) => {
                // Most likely, a directory was deleted during the scan: a
                // change Unison will look at.
                self.signal_change(0., f, None, Kind::Del);
                Ok(())
            }
        }
    }

    // ---- Commands ----

    fn start_watching(&mut self, hash: &[u8], fspath: &[u8], path: &[u8]) -> Result<(), Fatal> {
        let start = self.find_start(hash, fspath, path);
        self.clear_file_changes(start);
        self.file_mut(start).generation = self.current_gen;
        let fspath = concat(fspath, path);
        if self.is_root(start) {
            self.watch_path(&fspath, start, false)?;
        }
        self.send(Response::Ok);
        self.scan = Some(Scan { start, fspath });
        Ok(())
    }

    fn add_directory(&mut self, path: &[u8], follow: bool) -> Result<(), Fatal> {
        let scan = self.scan.as_ref().expect("scanning");
        let (start, full) = (scan.start, concat(&scan.fspath, path));
        let f = self.follow_path(start, path)?;
        self.clear_file_changes(f);
        self.file_mut(f).generation = self.current_gen;
        self.watch_path(&full, f, follow)?;
        self.send(Response::Ok);
        Ok(())
    }

    fn reset(&mut self, hash: &[u8]) {
        let keys: Vec<RootKey> = self.roots.keys().filter(|k| k.0 == hash).cloned().collect();
        for k in keys {
            let f = self.roots[&k];
            self.remove_file(f);
            self.roots.remove(&k);
        }
        self.previous_event = None;
        self.changes.remove(hash);
    }

    fn gather_changes(&mut self, hash: &[u8], time: f64) -> Vec<Vec<u8>> {
        self.previous_event = None;
        let mut l = Vec::new();
        for ((h, _, path), r) in &self.roots {
            if h != hash {
                continue;
            }
            if self.file(*r).changed {
                l.push(path.clone());
            }
            self.gather_rec(path, *r, time, &mut l);
        }
        l
    }

    fn gather_rec(&self, path: &[u8], r: FileId, time: f64, l: &mut Vec<Vec<u8>>) {
        let f = self.file(r);
        let old: BTreeSet<&Vec<u8>> = f
            .changed_children
            .iter()
            .filter(|(_, (_, t))| time - t > DELAY)
            .map(|(nm, _)| nm)
            .collect();
        l.extend(old.iter().map(|nm| concat(path, nm)));
        for (nm, d) in &f.subdirs {
            if !old.contains(nm) {
                self.gather_rec(&concat(path, nm), *d, time, l);
            }
        }
    }

    fn output_changes(&mut self, hash: &[u8], time: f64) {
        let paths = self.gather_changes(hash, time);
        self.clear_changes(hash, time);
        for p in paths {
            self.send(Response::Recursive(p));
        }
        self.send(Response::Done);
    }

    /// A line from Unison, without its newline.
    pub fn line(&mut self, l: &[u8], time: f64) -> Result<(), Fatal> {
        let parse = |l: &[u8]| match Command::parse(l) {
            Ok(c) => Ok(c),
            Err(ParseError::BadQuoting) => Err(uncaught("Invalid_argument(\"unquote\")")),
            Err(ParseError::Unknown(w)) => Err(Fatal(format!("unknown command '{}'", show(&w)))),
        };
        let unknown = || Fatal(format!("unknown command '{}'", show(split_on_space(l).0)));
        if self.scan.is_some() {
            // `add_directories`.
            return match parse(l)? {
                Command::Dir(p) => self.add_directory(&p, false),
                Command::Link(p) => self.add_directory(&p, true),
                Command::Done => {
                    let start = self.scan.take().expect("scanning").start;
                    // Directories no longer watched, and those that are
                    // not watched in fact, are forgotten.
                    self.remove_old_files(start);
                    self.current_gen += 1;
                    Ok(())
                }
                _ => Err(unknown()),
            };
        }
        // Any command cancels the waits.
        if split_on_space(l).0 != b"WAIT" {
            self.waiting_for_changes.clear();
        }
        match parse(l)? {
            Command::Version(_) => {}
            Command::Debug => self.debug = true,
            Command::Start { hash, fspath, path } => self.start_watching(&hash, &fspath, &path)?,
            Command::Wait(h) => self.wait(h, time),
            Command::Changes(h) => self.output_changes(&h, time),
            Command::Reset(h) => self.reset(&h),
            Command::Dir(_) | Command::Link(_) | Command::Done => return Err(unknown()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Watches nothing, and records what it was asked.
    #[derive(Default)]
    struct Fake {
        resolve: Option<Resolve>,
        watching: BTreeSet<PathBuf>,
        log: Vec<String>,
        lost: BTreeSet<PathBuf>,
    }

    impl Backend for Fake {
        fn add_watch(&mut self, path: &Path, root: bool, _follow: bool) -> Result<(), WatchError> {
            if self.lost.contains(path) {
                return Err(WatchError::Lost);
            }
            self.log.push(format!("add {} {root}", slashes(path)));
            self.watching.insert(path.to_path_buf());
            Ok(())
        }

        fn release_watch(&mut self, path: &Path, _root: bool) {
            self.log.push(format!("release {}", slashes(path)));
            self.watching.remove(path);
        }

        fn resolve(&self) -> Resolve {
            self.resolve.unwrap_or(Resolve::Watched)
        }
    }

    /// Paths with `/` on every system: Windows joins names with `\`, as
    /// `Filename.concat` does there.
    fn slashes(p: &Path) -> String {
        p.display().to_string().replace('\\', "/")
    }

    fn core() -> Core<Fake> {
        Core::new(Fake::default())
    }

    fn send(c: &mut Core<Fake>, lines: &[&str], time: f64) -> String {
        for l in lines {
            c.line(l.as_bytes(), time).unwrap();
        }
        String::from_utf8(std::mem::take(&mut c.out))
            .unwrap()
            .replace("%5C", "/")
    }

    /// A replica `/r` with `a` and `a/b` scanned, at time 0.
    fn scanned() -> Core<Fake> {
        let mut c = core();
        let out = send(
            &mut c,
            &["START h /r ", "DIR ", "DIR a", "DIR a/b", "DONE"],
            0.,
        );
        assert_eq!(out, "OK\nOK\nOK\nOK\n");
        c
    }

    fn ev(c: &mut Core<Fake>, p: &str, kind: Kind, time: f64) {
        c.event(Event::Path(PathBuf::from(p), kind), time);
    }

    fn changes(c: &mut Core<Fake>, time: f64) -> String {
        send(c, &["CHANGES h"], time)
    }

    #[test]
    fn scanning_watches_each_directory() {
        let mut c = scanned();
        // The root, by `START` and by `DIR`.
        assert_eq!(
            c.backend.log,
            [
                "add /r true",
                "add /r true",
                "add /r/a false",
                "add /r/a/b false"
            ]
        );
        assert_eq!(c.backend.watching.len(), 3);
        // A directory scanned again is watched again: it may be a new one.
        c.backend.log.clear();
        send(&mut c, &["START h /r ", "DIR a/b", "DONE"], 1.);
        assert!(
            c.backend.log.contains(&"add /r/a/b false".to_owned()),
            "{:?}",
            c.backend.log
        );
    }

    #[test]
    fn changes_are_reported_once_old_enough() {
        let mut c = scanned();
        ev(&mut c, "/r/a/f", Kind::Modif, 10.);
        assert_eq!(changes(&mut c, 10.2), "DONE\n");
        assert_eq!(changes(&mut c, 10.6), "RECURSIVE a/f\nDONE\n");
        // Reported once.
        assert_eq!(changes(&mut c, 11.), "DONE\n");
    }

    #[test]
    fn a_changed_directory_hides_the_changes_below_it() {
        let mut c = scanned();
        ev(&mut c, "/r/a/b/f", Kind::Modif, 1.);
        ev(&mut c, "/r/a/b", Kind::Modif, 1.);
        assert_eq!(changes(&mut c, 2.), "RECURSIVE a/b\nDONE\n");
    }

    #[test]
    fn a_file_created_then_deleted_is_not_a_change() {
        let mut c = scanned();
        ev(&mut c, "/r/a/new", Kind::Creat, 1.);
        ev(&mut c, "/r/a/new", Kind::Del, 1.1);
        assert_eq!(changes(&mut c, 2.), "DONE\n");
        // A directory created then deleted is reported: Unison watches it.
        ev(&mut c, "/r/a/b", Kind::Creat, 3.);
        ev(&mut c, "/r/a/b", Kind::Del, 3.1);
        assert_eq!(changes(&mut c, 4.), "RECURSIVE a/b\nDONE\n");
    }

    #[test]
    fn creations_during_a_scan_count_as_modifications() {
        let mut c = core();
        send(&mut c, &["START h /r ", "DIR "], 0.);
        ev(&mut c, "/r/new", Kind::Creat, 0.1);
        send(&mut c, &["DONE"], 0.2);
        ev(&mut c, "/r/new", Kind::Del, 0.3);
        assert_eq!(changes(&mut c, 1.), "RECURSIVE new\nDONE\n");
    }

    #[test]
    fn a_change_to_the_root_itself_reports_the_scan_path() {
        let mut c = core();
        send(&mut c, &["START h /r sub", "DIR ", "DONE"], 0.);
        ev(&mut c, "/r/sub", Kind::Modif, 1.);
        assert_eq!(changes(&mut c, 1.), "RECURSIVE sub\nDONE\n");
        // An overflow reports every root.
        c.event(Event::Overflow, 2.);
        assert_eq!(changes(&mut c, 2.), "RECURSIVE sub\nDONE\n");
    }

    #[test]
    fn directories_not_scanned_again_are_forgotten() {
        let mut c = scanned();
        send(&mut c, &["START h /r ", "DIR ", "DIR a", "DONE"], 1.);
        assert_eq!(c.backend.log.last().unwrap(), "release /r/a/b");
        ev(&mut c, "/r/a/b/f", Kind::Modif, 2.);
        assert_eq!(changes(&mut c, 3.), "DONE\n");
    }

    #[test]
    fn a_scan_below_the_root_keeps_the_rest() {
        let mut c = scanned();
        send(&mut c, &["START h /r a", "DIR ", "DONE"], 1.);
        assert_eq!(c.backend.watching.len(), 2, "{:?}", c.backend.log);
        ev(&mut c, "/r/a/f", Kind::Modif, 2.);
        assert_eq!(changes(&mut c, 3.), "RECURSIVE a/f\nDONE\n");
    }

    #[test]
    fn waits_are_answered_once_changes_settle() {
        let mut c = scanned();
        assert_eq!(send(&mut c, &["WAIT h", "WAIT other"], 0.), "");
        ev(&mut c, "/r/a/f", Kind::Modif, 1.);
        assert_eq!(c.timer, Some(1.5));
        c.tick(1.4);
        assert!(c.out.is_empty());
        c.tick(1.5);
        // Not older than the delay yet: wait again.
        assert!(c.out.is_empty());
        assert_eq!(c.timer, Some(2.));
        c.tick(2.);
        assert_eq!(
            String::from_utf8(std::mem::take(&mut c.out)).unwrap(),
            "CHANGES h\n"
        );
        // Any other command cancels the waits.
        send(&mut c, &["WAIT h", "CHANGES h"], 3.);
        ev(&mut c, "/r/a/g", Kind::Modif, 4.);
        assert_eq!(c.timer, None);
    }

    #[test]
    fn a_wait_with_changes_ready_is_answered_at_once() {
        let mut c = scanned();
        ev(&mut c, "/r/a/f", Kind::Modif, 1.);
        assert_eq!(send(&mut c, &["WAIT h"], 2.), "CHANGES h\n");
    }

    #[test]
    fn reset_forgets_a_replica() {
        let mut c = scanned();
        ev(&mut c, "/r/a/f", Kind::Modif, 1.);
        send(&mut c, &["RESET h"], 2.);
        assert!(c.backend.watching.is_empty());
        assert_eq!(changes(&mut c, 3.), "DONE\n");
    }

    #[test]
    fn a_directory_lost_during_the_scan_is_a_change() {
        let mut c = core();
        c.backend.lost.insert(PathBuf::from("/r/gone"));
        send(&mut c, &["START h /r ", "DIR ", "DIR gone", "DONE"], 0.);
        // At time 0, so it is old enough once the clock passes the delay.
        assert_eq!(changes(&mut c, 0.4), "DONE\n");
        assert_eq!(changes(&mut c, 0.6), "RECURSIVE gone\nDONE\n");
        // A root that is lost is an error.
        let mut c = core();
        c.backend.lost.insert(PathBuf::from("/x"));
        assert_eq!(
            c.line(b"START h /x ", 0.),
            Err(Fatal("Path '/x' does not exist".into()))
        );
    }

    #[test]
    fn bad_commands_stop_the_watcher() {
        let mut c = core();
        assert_eq!(
            c.line(b"DIR a", 0.),
            Err(Fatal("unknown command 'DIR'".into()))
        );
        assert_eq!(
            c.line(b"NOPE", 0.),
            Err(Fatal("unknown command 'NOPE'".into()))
        );
        send(&mut c, &["START h /r "], 0.);
        assert_eq!(
            c.line(b"WAIT h", 0.),
            Err(Fatal("unknown command 'WAIT'".into()))
        );
        assert!(
            c.line(b"DIR %", 0.)
                .unwrap_err()
                .0
                .contains("Invalid_argument")
        );
    }

    #[test]
    fn below_roots_an_unknown_directory_counts_as_its_nearest_known_one() {
        let mut c = core();
        c.backend.resolve = Some(Resolve::BelowRoot);
        send(&mut c, &["START h /r ", "DIR ", "DIR a", "DONE"], 0.);
        // Only the root is watched there; the others are just known.
        ev(&mut c, "/r/a/f", Kind::Modif, 1.);
        assert_eq!(changes(&mut c, 2.), "RECURSIVE a/f\nDONE\n");
        // A directory not known counts as its nearest known one, changed.
        ev(&mut c, "/r/a/x/y", Kind::Creat, 2.);
        assert_eq!(changes(&mut c, 3.), "RECURSIVE a\nDONE\n");
        // A deletion can't be followed: its directory counts as modified.
        ev(&mut c, "/r/a/f", Kind::Del, 3.);
        assert_eq!(changes(&mut c, 4.), "RECURSIVE a\nDONE\n");
        ev(&mut c, "/r/top", Kind::Del, 5.);
        assert_eq!(changes(&mut c, 6.), "RECURSIVE \nDONE\n");
    }
}
