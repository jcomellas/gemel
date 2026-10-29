//! Synchronization engine: update detection, reconciliation, propagation, archives.
//!
//! The core of the port. Has no UI dependencies; frontends drive it
//! through `gemel-ui-api`.

pub mod abort;
pub mod archive;
pub mod common;
pub mod copy;
pub mod exclusive;
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
pub mod region;
pub mod sortri;
pub mod stasher;
pub mod transfer;
pub mod transport;
pub mod tree;
pub mod update;
pub mod uutil;
pub mod xattrs;
pub mod xferhint;

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_base::trace::{Timer, Trace};
use gemel_base::util::Error;
use gemel_rpc::host::Host;
use gemel_rpc::registry::Rpc;

/// Registers the features of the engine and of `gemel-fs`, in OCaml's
/// order (`props`, `common`, then `moves`).
pub fn register_features(features: &mut Features) -> Result<(), Error> {
    gemel_fs::props::register_features(features)?;
    common::register_features(features)?;
    moves::register_features(features)
}

/// Registers the preferences of the engine modules ported so far, and
/// `remote`'s (in `gemel-rpc`), in OCaml's order; not those of
/// `gemel-base` and `gemel-fs`.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    pred::register_prefs(prefs)?;
    abort::register_prefs(prefs)?;
    fswatch::register_prefs(prefs)?;
    xferhint::register_prefs(prefs)?;
    gemel_rpc::connection::register_prefs(prefs)?;
    globals::register_prefs(prefs)?;
    fpcache::register_prefs(prefs)?;
    archive::register_prefs(prefs)?;
    update::register_prefs(prefs)?;
    moves::register_prefs(prefs)?;
    copy::register_prefs(prefs)?;
    stasher::register_prefs(prefs)?;
    files::register_prefs(prefs)?;
    sortri::register_prefs(prefs)?;
    recon::register_prefs(prefs)?;
    transport::register_prefs(prefs)
}

/// Runs `f` with the client's [`Trace`] (a slot of its host, which the
/// user interface sets up) and preferences.
pub fn with_trace<R>(rpc: &Rpc, f: impl FnOnce(&mut Trace, &Prefs) -> R) -> R {
    let host = rpc.client_host();
    let trace = host.state::<Trace>();
    let prefs = host.prefs().borrow();
    f(&mut trace.borrow_mut(), &prefs)
}

/// `Trace.debug`, on `host`'s trace: the client's, or a server's for what
/// runs there.
pub fn debug(host: &Host, modname: &str, msg: impl FnOnce() -> String) {
    let trace = host.state::<Trace>();
    let prefs = host.prefs().borrow();
    trace.borrow_mut().debug(&prefs, modname, msg);
}

/// `Trace.startTimer`, on the client's trace.
pub fn start_timer(rpc: &Rpc, desc: &str) -> Result<Timer, Error> {
    with_trace(rpc, |t, p| t.start_timer(p, desc))
}

/// `Trace.showTimer`, on the client's trace.
pub fn show_timer(rpc: &Rpc, timer: &Timer) -> Result<(), Error> {
    with_trace(rpc, |t, p| t.show_timer(p, timer))
}

/// Registers the remote commands of the modules ported so far.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    rpc.registry_mut()
        .register_host(&gemel_rpc::root::CANONIZE_ON_SERVER)?;
    negotiate::register_commands(rpc)?;
    globals::register_commands(rpc)?;
    archive::register_commands(rpc)?;
    update::register_commands(rpc)?;
    copy::register_commands(rpc)?;
    stasher::register_commands(rpc)?;
    files::register_commands(rpc)?;
    fswatch::register_commands(rpc)
}
