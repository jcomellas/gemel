//! RPC mechanism: framing, version handshake, command registry, ssh/socket connections.
//!
//! Sits *below* the engine: as in Unison, engine modules register their
//! own remote commands (`Remote.registerRootCmd` in `update.ml`,
//! `files.ml`, `copy.ml`, ...), and the server loop dispatches whatever
//! was registered.

pub mod connection;
pub mod protocol;
pub mod registry;
pub mod terminal;
