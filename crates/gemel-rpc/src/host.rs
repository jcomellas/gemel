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
//! negotiated features, as a server would have them.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_fs::os;

/// A host's state (one per process).
pub struct Host {
    prefs: Rc<RefCell<Prefs>>,
    features: Rc<RefCell<Features>>,
    is_server: bool,
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
        Host {
            prefs,
            features,
            is_server: false,
            slots: RefCell::default(),
        }
    }

    /// A server host with its own preferences and features.
    pub fn server(prefs: Prefs, features: Features) -> Host {
        Host {
            prefs: Rc::new(RefCell::new(prefs)),
            features: Rc::new(RefCell::new(features)),
            is_server: true,
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
        prefs.load(&client_prefs.dump(1)?, 1)?;
        features.set_enabled(&client_features.get_enabled());
        Ok(Host::server(prefs, features))
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

    /// `Os.myCanonicalHostName`: on a server, its own name; on the client,
    /// `clientHostName` (which defaults to it).
    pub fn canonical_host_name(&self) -> String {
        let prefs = self.prefs.borrow();
        if !self.is_server && prefs.kind(os::CLIENT_HOST_NAME.name()).is_some() {
            prefs.read(&os::CLIENT_HOST_NAME).clone()
        } else {
            os::local_canonical_host_name()
        }
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
}
