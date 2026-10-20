//! Synchronization engine: update detection, reconciliation, propagation, archives.
//!
//! The core of the port. Has no UI dependencies; frontends drive it
//! through `gemel-ui-api`.

pub mod abort;
pub mod archive;
pub mod common;
pub mod copy;
pub mod external;
pub mod files;
pub mod fpcache;
pub mod fswatch;
pub mod globals;
pub mod lock;
pub mod moves;
pub mod name_map;
pub mod negotiate;
pub mod pred;
pub mod recon;
pub mod stasher;
pub mod transfer;
pub mod transport;
pub mod tree;
pub mod update;
pub mod xferhint;

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_base::util::Error;
use gemel_rpc::registry::Rpc;

/// Registers the features of the engine and of `gemel-fs`, in OCaml's
/// order (`props`, then `common`).
pub fn register_features(features: &mut Features) -> Result<(), Error> {
    gemel_fs::props::register_features(features)?;
    common::register_features(features)
}

/// Registers the preferences of the engine modules ported so far (not
/// those of `gemel-base` and `gemel-fs`).
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    pred::register_prefs(prefs)?;
    abort::register_prefs(prefs)?;
    globals::register_prefs(prefs)?;
    fpcache::register_prefs(prefs)?;
    archive::register_prefs(prefs)
}

/// Registers the remote commands of the modules ported so far.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    rpc.registry_mut()
        .register_host(&gemel_rpc::root::CANONIZE_ON_SERVER)?;
    negotiate::register_commands(rpc)?;
    globals::register_commands(rpc)?;
    archive::register_commands(rpc)
}
