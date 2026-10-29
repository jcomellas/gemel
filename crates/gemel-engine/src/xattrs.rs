//! A host's extended-attribute settings and its stores, for gemel-fs's
//! [`XattrCtx`]: the compiled `xattrignore` and `xattrignorenot`
//! (`Props.skipIgnoredXattr`), the host's attribute values by MD5
//! (`Propsdata.Xattr`), and its ACL texts by MD5 (`Propsdata.ACL`), which
//! its scans share.

use std::sync::{Arc, Mutex};

use gemel_base::case::Mode;
use gemel_base::prefs::Prefs;
use gemel_base::util::Error;
use gemel_fs::props::{XattrCtx, XattrFilter};
use gemel_fs::propsdata::Store;
use gemel_rpc::host::Host;

use crate::pred::Compiled;
use crate::update::{XATTR_IGNORE, XATTR_IGNORE_NOT};

/// The host's store, in its state.
#[derive(Default)]
struct HostStore(Arc<Mutex<Store>>);

/// The host's attribute values by MD5 (`Propsdata.Xattr`).
pub fn store(host: &Host) -> Arc<Mutex<Store>> {
    host.state::<HostStore>().borrow().0.clone()
}

/// The host's ACL store, in its state.
#[derive(Default)]
struct HostAclStore(Arc<Mutex<Store>>);

/// The host's ACL texts by MD5 (`Propsdata.ACL`).
pub fn acl_store(host: &Host) -> Arc<Mutex<Store>> {
    host.state::<HostAclStore>().borrow().0.clone()
}

/// What reading and setting a host's xattrs needs.
#[derive(Debug, Clone)]
pub struct XattrState {
    ignore: Arc<Compiled>,
    ignore_not: Arc<Compiled>,
    store: Arc<Mutex<Store>>,
    acl_store: Arc<Mutex<Store>>,
}

impl XattrState {
    /// `host`'s, with its preferences `prefs` under `mode`.
    pub fn of(host: &Host, prefs: &Prefs, mode: Mode) -> Result<XattrState, Error> {
        Ok(XattrState {
            ignore: XATTR_IGNORE.compiled(prefs, mode)?,
            ignore_not: XATTR_IGNORE_NOT.compiled(prefs, mode)?,
            store: store(host),
            acl_store: acl_store(host),
        })
    }

    /// Every attribute kept, and stores of its own: without a host, as in
    /// tests.
    pub fn standalone() -> XattrState {
        let none = || Arc::new(Compiled::new(Vec::new(), Mode::Sensitive).expect("no patterns"));
        XattrState {
            ignore: none(),
            ignore_not: none(),
            store: Arc::default(),
            acl_store: Arc::default(),
        }
    }

    /// The context gemel-fs's props functions take.
    pub fn ctx(&self) -> XattrCtx<'_> {
        XattrCtx {
            filter: self,
            store: &self.store,
            acl_store: &self.acl_store,
        }
    }

    /// The store.
    pub fn store(&self) -> &Mutex<Store> {
        &self.store
    }
}

impl XattrFilter for XattrState {
    fn keep(&self, name: &[u8]) -> bool {
        !self.ignore.test(name) || self.ignore_not.test(name)
    }
}
