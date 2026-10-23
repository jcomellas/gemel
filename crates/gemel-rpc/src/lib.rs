//! RPC mechanism: framing, version handshake, command registry, ssh/socket connections.
//!
//! Sits *below* the engine: as in Unison, engine modules register their
//! own remote commands (`Remote.registerRootCmd` in `update.ml`,
//! `files.ml`, `copy.ml`, ...), and the server loop dispatches whatever
//! was registered.
//!
//! [`root`] has canonical roots and their resolution, [`registry`] the
//! commands and their dispatch (called directly, or over a connection),
//! [`connection`] connections (packets, message ids, flow control, an
//! in-memory one for tests), [`host`] the state a host keeps for its roots,
//! [`protocol`] the byte-exact pieces of messages, [`handshake`] the
//! version handshake, [`server`] a server's command loop, [`shell`]
//! connections through ssh, [`socket`] connections on sockets and a server
//! waiting on one, and [`runtime`] the engine thread.

pub mod connection;
pub mod handshake;
pub mod host;
pub mod protocol;
pub mod registry;
pub mod root;
pub mod runtime;
pub mod server;
pub mod shell;
pub mod socket;
pub mod terminal;
