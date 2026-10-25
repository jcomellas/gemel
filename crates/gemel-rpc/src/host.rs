//! The state a host keeps for the roots it serves.
//!
//! In OCaml, the archives, fingerprint caches and locks of a root live in
//! the globals of the process that owns the root, and other hosts reach
//! them only through root commands. A [`Host`] is that process-wide state:
//! the preferences and features in effect there, and typed slots where
//! engine modules keep their own state (keyed by root inside, since one
//! host serves both roots of a local sync).
//!
//! In a local run the client is its own host and shares its preferences
//! and features. In loopback mode (see [`crate::registry`]) the host is
//! separate, with preferences loaded from the client's dump and the
//! negotiated features, as a server would have them. It still runs the
//! client's local roots, which OCaml serves in the client's own process, so
//! it names itself as the client does (`clientHostName`) and its warnings
//! go to the client. A host standing for a server on another host
//! ([`crate::registry::Rpc::connect_in_memory`]) names itself after that
//! host ([`Host::set_host_name`]).

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_fs::{os, system};

/// Shows a warning (`Util.warnPrinter`).
pub type WarnPrinter = Rc<dyn Fn(&str)>;

/// Runs when a connection closes (`Remote.at_conn_close`).
pub type CloseHandler = Rc<dyn Fn()>;

/// What a host's connections have written and read so far, all together
/// (`Remote.emittedBytes` and `Remote.receivedBytes`, which OCaml keeps
/// for the whole process). It can be read from any thread, for a
/// frontend's statistics.
#[derive(Debug, Default)]
pub struct Traffic {
    emitted: AtomicU64,
    received: AtomicU64,
}

impl Traffic {
    /// The bytes written.
    pub fn emitted(&self) -> u64 {
        self.emitted.load(Ordering::Relaxed)
    }

    /// The bytes read.
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    pub(crate) fn add_emitted(&self, n: u64) {
        self.emitted.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn add_received(&self, n: u64) {
        self.received.fetch_add(n, Ordering::Relaxed);
    }
}

/// A host's state (one per process).
pub struct Host {
    prefs: Rc<RefCell<Prefs>>,
    features: Rc<RefCell<Features>>,
    is_server: bool,
    runs_client_roots: bool,
    unison_dir: RefCell<Option<PathBuf>>,
    host_name: RefCell<Option<String>>,
    warn_printer: RefCell<Option<WarnPrinter>>,
    close_handlers: RefCell<Vec<(bool, CloseHandler)>>,
    slots: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
    traffic: RefCell<Arc<Traffic>>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("is_server", &self.is_server)
            .finish_non_exhaustive()
    }
}

impl Host {
    /// The client acting as its own host: shares the client's preferences
    /// and features.
    pub fn client(prefs: Rc<RefCell<Prefs>>, features: Rc<RefCell<Features>>) -> Host {
        Host::new(prefs, features, false, true)
    }

    /// A server host with its own preferences and features.
    pub fn server(prefs: Prefs, features: Features) -> Host {
        Host::new(
            Rc::new(RefCell::new(prefs)),
            Rc::new(RefCell::new(features)),
            true,
            false,
        )
    }

    fn new(
        prefs: Rc<RefCell<Prefs>>,
        features: Rc<RefCell<Features>>,
        is_server: bool,
        runs_client_roots: bool,
    ) -> Host {
        let default_printer: WarnPrinter = Rc::new(|s| eprint!("Warning: {s}"));
        Host {
            prefs,
            features,
            is_server,
            runs_client_roots,
            unison_dir: RefCell::default(),
            host_name: RefCell::default(),
            warn_printer: RefCell::new(Some(default_printer)),
            close_handlers: RefCell::default(),
            slots: RefCell::default(),
            traffic: RefCell::default(),
        }
    }

    /// The host a server would be for this client: `prefs` and `features`
    /// are freshly registered (as a server process starts), then get the
    /// client's preference dump and negotiated features. Preferences that
    /// aren't sent (`cli_only`, or local to the client) keep their defaults,
    /// as on a real server.
    pub fn loopback_server(
        client_prefs: &Prefs,
        client_features: &Features,
        mut prefs: Prefs,
        mut features: Features,
    ) -> Result<Host, gemel_base::util::Error> {
        prefs.load(&client_prefs.dump(1, client_features)?, 1)?;
        features.set_enabled(&client_features.get_enabled());
        let mut host = Host::server(prefs, features);
        host.runs_client_roots = true;
        Ok(host)
    }

    /// The preferences in effect on this host.
    pub fn prefs(&self) -> &Rc<RefCell<Prefs>> {
        &self.prefs
    }

    /// The features in effect on this host.
    pub fn features(&self) -> &Rc<RefCell<Features>> {
        &self.features
    }

    /// Whether this host is serving another (`Trace.runningasserver`).
    pub fn is_server(&self) -> bool {
        self.is_server
    }

    /// `Os.myCanonicalHostName`: on a server, its own name; on the client
    /// (and the loopback host), `clientHostName` (which defaults to it). A
    /// name set with [`Host::set_host_name`] replaces the system's.
    pub fn canonical_host_name(&self) -> String {
        if let Some(name) = &*self.host_name.borrow() {
            return name.clone();
        }
        let prefs = self.prefs.borrow();
        if self.runs_client_roots && prefs.kind(os::CLIENT_HOST_NAME.name()).is_some() {
            prefs.read(&os::CLIENT_HOST_NAME).clone()
        } else {
            os::local_canonical_host_name()
        }
    }

    /// Names the host `name` rather than after the system's host, for a
    /// host standing for another machine.
    pub fn set_host_name(&self, name: impl Into<String>) {
        *self.host_name.borrow_mut() = Some(name.into());
    }

    /// `Remote.at_conn_close`: runs `f` whenever a connection closes, or,
    /// with `only_server`, only on a server (see
    /// [`Host::run_conn_close_handlers`]).
    pub fn at_conn_close(&self, only_server: bool, f: CloseHandler) {
        self.close_handlers.borrow_mut().push((only_server, f));
    }

    /// `runConnCloseHandlers`, newest first: on the client (`is_server`
    /// false) when it
    /// closes a connection, the handlers not only for servers; on a socket
    /// server when its client goes, all of them.
    pub fn run_conn_close_handlers(&self, is_server: bool) {
        let handlers = self.close_handlers.borrow().clone();
        for (only_server, f) in handlers.into_iter().rev() {
            if !only_server || is_server {
                f();
            }
        }
    }

    /// Sets the Unison directory, where archives and locks are kept (by
    /// default [`os::unison_dir`]).
    pub fn set_unison_dir(&self, dir: impl Into<PathBuf>) {
        *self.unison_dir.borrow_mut() = Some(dir.into());
    }

    /// `Util.unisonDir`.
    pub fn unison_dir(&self) -> PathBuf {
        self.unison_dir
            .borrow()
            .clone()
            .unwrap_or_else(os::unison_dir)
    }

    /// `Util.fileInUnisonDir`: `name` in the Unison directory
    /// (`Filename.concat`).
    pub fn file_in_unison_dir(&self, name: &str) -> Vec<u8> {
        let mut f = system::name_bytes(self.unison_dir().as_os_str());
        let sep_end = |b: &u8| *b == b'/' || (cfg!(windows) && (*b == b'\\' || *b == b':'));
        if !f.last().is_some_and(sep_end) {
            f.push(if cfg!(windows) { b'\\' } else { b'/' });
        }
        f.extend_from_slice(name.as_bytes());
        f
    }

    /// `Util.warn`: shows a warning with the printer set, if any (by
    /// default on standard error, prefixed with `Warning: `).
    pub fn warn(&self, message: &str) {
        let printer = self.warn_printer.borrow().clone();
        if let Some(p) = printer {
            p(message);
        }
    }

    /// Sets `Util.warnPrinter`; `None` drops warnings.
    pub fn set_warn_printer(&self, printer: Option<WarnPrinter>) {
        *self.warn_printer.borrow_mut() = printer;
    }

    /// What the connections this host's commands are served on have
    /// written and read.
    pub fn traffic(&self) -> Arc<Traffic> {
        self.traffic.borrow().clone()
    }

    /// Counts what the connections made from now on write and read into
    /// `traffic`.
    pub fn set_traffic(&self, traffic: Arc<Traffic>) {
        *self.traffic.borrow_mut() = traffic;
    }

    /// The slot of type `T`, created empty on first use. Don't hold a borrow
    /// of it across an `.await`.
    pub fn state<T: Default + 'static>(&self) -> Rc<RefCell<T>> {
        let mut slots = self.slots.borrow_mut();
        let slot = slots
            .entry(TypeId::of::<RefCell<T>>())
            .or_insert_with(|| Rc::new(RefCell::new(T::default())) as Rc<dyn Any>)
            .clone();
        slot.downcast::<RefCell<T>>()
            .unwrap_or_else(|_| unreachable!("slot keyed by its type"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_typed_and_shared() {
        let h = Host::server(Prefs::new(), Features::new());
        h.state::<Vec<u32>>().borrow_mut().push(1);
        h.state::<Vec<u32>>().borrow_mut().push(2);
        assert_eq!(*h.state::<Vec<u32>>().borrow(), [1, 2]);
        assert!(h.state::<String>().borrow().is_empty());
        assert!(h.is_server());
    }

    #[test]
    fn files_in_the_unison_dir_and_warnings() {
        let h = Host::server(Prefs::new(), Features::new());
        let sep = if cfg!(windows) { "\\" } else { "/" };
        h.set_unison_dir(format!("u{sep}"));
        assert_eq!(
            h.file_in_unison_dir("ar1"),
            format!("u{sep}ar1").into_bytes()
        );
        h.set_unison_dir("u");
        assert_eq!(
            h.file_in_unison_dir("ar1"),
            format!("u{sep}ar1").into_bytes()
        );
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        h.set_warn_printer(Some(Rc::new(move |m| s.borrow_mut().push(m.to_owned()))));
        h.warn("one");
        h.set_warn_printer(None);
        h.warn("two");
        assert_eq!(*seen.borrow(), ["one"]);
    }
}
