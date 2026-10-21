//! Watching with `notify`, as OCaml's watchers watch.
//!
//! Ports the system parts of Unison `src/fsmonitor/inotify/watcher.ml` and
//! `src/fsmonitor/windows/watcher.ml`.
//!
//! Derived from Unison, Copyright 2012, Benjamin C. Pierce.
//!
//! - Linux: each directory scanned is watched on its own (inotify), as
//!   OCaml's inotify watcher does.
//! - Windows: each root is watched with everything below it, and the
//!   directories scanned are only recorded, as OCaml's Windows watcher
//!   does, so no handle is held on them.
//! - macOS, which OCaml has no watcher for: each root is watched with
//!   everything below it (FSEvents), and events count only in the
//!   directories scanned, as on Linux.
//!
//! As in OCaml's Windows watcher, a symbolic link's target is not watched
//! on Windows and macOS unless it is below a root.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::core::{Backend, Event, Kind, Resolve, WatchError};

/// Each root recursively (Windows, macOS), or each directory on its own
/// (Linux).
const RECURSIVE: bool = !cfg!(target_os = "linux");

pub struct Notify {
    watcher: RecommendedWatcher,
    watching: HashSet<PathBuf>,
}

/// An error's message, as `Unix.error_message` gives it.
fn error_message(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

impl Notify {
    /// Starts the system's watcher, which hands its events to `handler`.
    pub fn new(
        handler: impl FnMut(notify::Result<notify::Event>) + Send + 'static,
    ) -> Result<Notify, String> {
        let watcher = notify::recommended_watcher(handler).map_err(|e| match e.kind {
            notify::ErrorKind::Io(ref io)
                if cfg!(target_os = "linux") && io.raw_os_error() == Some(24) =>
            {
                // EMFILE.
                "unable to start inotify: system limit reached (you can do a web search for \
                 \"inotify max_user_instances\" to understand the reasons and mitigations \
                 for this error)"
                    .to_owned()
            }
            _ => format!("unable to start watching for changes: {e}"),
        })?;
        Ok(Notify {
            watcher,
            watching: HashSet::new(),
        })
    }
}

/// The inotify watcher's `add_watch` errors.
fn watch_error(e: notify::Error) -> WatchError {
    const ENOSPC: &str = "cannot add a watcher: system limit reached (you can do a web search \
                          for \"inotify max_user_watches\" to understand the reasons and \
                          mitigations for this error)";
    match e.kind {
        notify::ErrorKind::PathNotFound => WatchError::Lost,
        notify::ErrorKind::MaxFilesWatch => WatchError::Fatal(ENOSPC.into()),
        notify::ErrorKind::Io(ref io) => match io.kind() {
            io::ErrorKind::NotFound => WatchError::Lost,
            io::ErrorKind::PermissionDenied | io::ErrorKind::NotADirectory => {
                WatchError::Unwatchable
            }
            // ELOOP and ENOSPC on Linux.
            _ if cfg!(target_os = "linux") && io.raw_os_error() == Some(40) => {
                WatchError::Unwatchable
            }
            _ if cfg!(target_os = "linux") && io.raw_os_error() == Some(28) => {
                WatchError::Fatal(ENOSPC.into())
            }
            _ => WatchError::Fatal(format!(
                "unexpected error while adding a watcher: {}",
                error_message(io)
            )),
        },
        _ => WatchError::Fatal(format!("unexpected error while adding a watcher: {e}")),
    }
}

impl Backend for Notify {
    fn add_watch(&mut self, path: &Path, root: bool, _follow: bool) -> Result<(), WatchError> {
        // inotify is asked again for a directory it watches: it may be a
        // new one at that path. A recursive watch follows the path.
        if RECURSIVE && (!root || self.watching.contains(path)) {
            return Ok(());
        }
        // notify's Windows watcher doesn't say a path is missing.
        if path
            .symlink_metadata()
            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            return Err(WatchError::Lost);
        }
        let mode = if RECURSIVE {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        match self.watcher.watch(path, mode) {
            Ok(()) => {
                self.watching.insert(path.to_path_buf());
                Ok(())
            }
            Err(e) if RECURSIVE => match watch_error(e) {
                // The Windows watcher's message.
                WatchError::Fatal(m) => Err(WatchError::Fatal(format!(
                    "Error while starting to watch for changes: {m}\n"
                ))),
                other => Err(other),
            },
            Err(e) => Err(watch_error(e)),
        }
    }

    fn release_watch(&mut self, path: &Path, _root: bool) {
        if self.watching.remove(path) {
            // Fails if the directory is gone.
            let _ = self.watcher.unwatch(path);
        }
    }

    fn resolve(&self) -> Resolve {
        if cfg!(windows) {
            Resolve::BelowRoot
        } else {
            Resolve::Watched
        }
    }

    #[cfg(windows)]
    fn long_name(&self, root: &Path, rel: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
        let full = rel
            .iter()
            .fold(root.to_path_buf(), |p, n| p.join(crate::core::os_path(n)));
        let long = gemel_fs::system::long_path_name(&full)?;
        let long_root = gemel_fs::system::long_path_name(root).unwrap_or(root.to_path_buf());
        let rest = long.strip_prefix(long_root).ok()?;
        Some(
            rest.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned().into_bytes())
                .collect(),
        )
    }
}

/// What a `notify` event means for the watcher.
pub fn events(res: notify::Result<notify::Event>) -> Vec<Event> {
    let ev = match res {
        Ok(ev) if !ev.need_rescan() => ev,
        // Lost events, or an error reading them: everything may have
        // changed.
        _ => return vec![Event::Overflow],
    };
    let kind = |p: &Path| match ev.kind {
        EventKind::Create(_) => Some(Kind::Creat),
        EventKind::Remove(_) => Some(Kind::Del),
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => Some(Kind::Del),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => Some(Kind::Modif),
        // Both names also come on their own.
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => None,
        // FSEvents doesn't say which side of a rename a path is.
        EventKind::Modify(ModifyKind::Name(_)) => Some(if p.symlink_metadata().is_ok() {
            Kind::Modif
        } else {
            Kind::Del
        }),
        EventKind::Modify(_) | EventKind::Any => Some(Kind::Modif),
        // Reads and closes, as inotify's `` `OTHER `` events.
        EventKind::Access(_) | EventKind::Other => None,
    };
    ev.paths
        .iter()
        .filter_map(|p| kind(p).map(|k| Event::Path(p.clone(), k)))
        .collect()
}
