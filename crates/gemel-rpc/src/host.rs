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
//! go to the client.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_fs::{os, system};

/// Shows a warning (`Util.warnPrinter`).
pub type WarnPrinter = Rc<dyn Fn(&str)>;

/// A host's state (one per process).
pub struct Host {
    prefs: Rc<RefCell<Prefs>>,
    features: Rc<RefCell<Features>>,
    is_server: bool,
    runs_client_roots: bool,
    unison_dir: RefCell<Option<PathBuf>>,
    warn_printer: RefCell<Option<WarnPrinter>>,
    slots: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
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
            warn_printer: RefCell::new(Some(default_printer)),
            slots: RefCell::default(),
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
    /// (and the loopback host), `clientHostName` (which defaults to it).
    pub fn canonical_host_name(&self) -> String {
        let prefs = self.prefs.borrow();
        if self.runs_client_roots && prefs.kind(os::CLIENT_HOST_NAME.name()).is_some() {
            prefs.read(&os::CLIENT_HOST_NAME).clone()
        } else {
            os::local_canonical_host_name()
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
