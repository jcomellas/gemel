//! The engine/frontend contract shared by the text UI and the GUI.
//!
//! Frontends depend only on this crate. The contract is modelled on
//! `uimacbridge.ml`: profile initialisation, update detection, per-item
//! accessors and direction setters, synchronization, progress, ignore
//! and diff. Commands go in and events come out over channels.

pub mod exit_code;
pub mod session;
pub mod uicommon;

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_base::util::Error;
use gemel_engine::{
    abort, archive, copy, files, fpcache, fswatch, globals, moves, pred, recon, sortri, stasher,
    transport, update, xferhint,
};
use gemel_rpc::registry::Rpc;

/// Registers every preference ported so far, in the order OCaml links the
/// modules that define them (the order of the dump sent to a server).
/// Every host registers them all, client or server: the client sends its
/// preferences, and a server refuses one it doesn't know.
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    gemel_base::trace::register_prefs(prefs)?;
    gemel_base::case::register_prefs(prefs)?;
    pred::register_prefs(prefs)?;
    abort::register_prefs(prefs)?;
    gemel_fs::osx::register_prefs(prefs)?;
    fswatch::register_prefs(prefs)?;
    gemel_fs::props::register_prefs(prefs)?;
    gemel_fs::fileinfo::register_prefs(prefs)?;
    gemel_fs::os::register_prefs(prefs)?;
    xferhint::register_prefs(prefs)?;
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
    transport::register_prefs(prefs)?;
    uicommon::register_prefs(prefs)
}

/// Preferences with everything registered, at their defaults.
pub fn new_prefs() -> Result<Prefs, Error> {
    let mut p = Prefs::new();
    register_prefs(&mut p)?;
    Ok(p)
}

/// Features with everything registered, none enabled yet.
pub fn new_features() -> Result<Features, Error> {
    let mut f = Features::new();
    gemel_engine::register_features(&mut f)?;
    Ok(f)
}

/// Registers every remote command: the engine's, and `architecture`.
pub fn register_commands(rpc: &mut Rpc) -> Result<(), Error> {
    gemel_engine::register_commands(rpc)?;
    rpc.registry_mut()
        .register_root(&uicommon::startup::ARCHITECTURE)
}
