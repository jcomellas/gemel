//! Feature negotiation with the roots' hosts.
//!
//! Ports Unison `src/negotiate.ml`.
//!
//! Derived from Unison, Copyright 2021, Tõivo Leedjärv.
//!
//! The client proposes the features it supports to each root's host, which
//! answers with those it supports too; what is left is enabled on both
//! sides. A local root needs no question; in loopback mode it is asked
//! anyway, which also enables the features on the loopback host.

use std::rc::Rc;

use gemel_base::features::{self, Id};
use gemel_base::util::Error;
use gemel_rpc::host::Host;
use gemel_rpc::registry::{RootCmd, Rpc};
use gemel_rpc::root::Root;

/// `getCommonFeaturesLocal`, on the host: enables the proposed features it
/// supports, and returns them.
fn common_features_local(host: &Host, proposed: &[Id]) -> Result<Vec<Id>, Error> {
    let mut f = host.features().borrow_mut();
    f.reset_enabled();
    let common = features::inter(proposed, &f.all());
    f.validate(&common, &host.prefs().borrow())?;
    f.set_enabled(&common);
    Ok(common)
}

/// `negotiateFeatures`.
pub static NEGOTIATE_FEATURES: RootCmd<Vec<Id>, Vec<Id>> = RootCmd::new(
    "negotiateFeatures",
    |l, w, _| w.list(l, |w, s| w.string(s.as_bytes())),
    |r, _| read_ids(r),
    |l, w, _| w.list(l, |w, s| w.string(s.as_bytes())),
    |r, _| read_ids(r),
    |host: Rc<Host>, _, proposed| Box::pin(async move { common_features_local(&host, &proposed) }),
);

fn read_ids(r: &mut gemel_compat::umarshal::Reader<'_>) -> gemel_compat::umarshal::Result<Vec<Id>> {
    r.list(|r| Ok(String::from_utf8_lossy(&r.string()?).into_owned()))
}

/// `commonFeatures`: narrows `proposed` to what `root`'s host supports.
async fn common_features(rpc: &Rpc, root: &Root, proposed: Vec<Id>) -> Result<Vec<Id>, Error> {
    let common = if root.is_local() && !rpc.is_loopback() {
        proposed
    } else {
        rpc.call_root(&NEGOTIATE_FEATURES, root, proposed).await?
    };
    let client = rpc.client_host();
    client
        .features()
        .borrow()
        .validate(&common, &client.prefs().borrow())?;
    Ok(common)
}

/// `Negotiate.features`: negotiates with the hosts of `roots` (in canonical
/// order) and enables the common features on the client.
pub async fn features(rpc: &Rpc, roots: &[Root]) -> Result<(), Error> {
    let supported = {
        let mut f = rpc.client_host().features().borrow_mut();
        f.reset_enabled();
        f.all()
    };
    let mut supported_by_all = true;
    for r in roots {
        supported_by_all &= rpc.command_available(r, NEGOTIATE_FEATURES.name())?;
    }
    let common = if supported_by_all {
        let mut common = supported;
        for r in roots {
            common = common_features(rpc, r, common).await?;
        }
        common
    } else {
        // The server does not support features.
        Vec::new()
    };
    rpc.client_host()
        .features()
        .borrow_mut()
        .set_enabled(&common);
    Ok(())
}

/// Registers this module's commands.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    rpc.registry_mut().register_root(&NEGOTIATE_FEATURES)
}
