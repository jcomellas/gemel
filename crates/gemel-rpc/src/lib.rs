//! RPC mechanism: framing, version handshake, command registry, ssh/socket connections.
//!
//! Sits *below* the engine: as in Unison, engine modules register their
//! own remote commands (`Remote.registerRootCmd` in `update.ml`,
//! `files.ml`, `copy.ml`, ...), and the server loop dispatches whatever
//! was registered.
//!
//! So far (milestone 2) this covers local roots: [`root`] (canonical roots
//! and their resolution), [`registry`] (commands, called directly or in a
//! loopback mode that encodes every call as a server would see it),
//! [`host`] (the state a host keeps for its roots), [`protocol`] (the
//! byte-exact pieces of messages) and [`runtime`] (the engine thread).
//! Connections, packets and the handshake come with remote roots
//! (milestone 3).

pub mod connection;
pub mod host;
pub mod protocol;
pub mod registry;
pub mod root;
pub mod runtime;
pub mod terminal;
