//! Message framing, flow control and the RPC version handshake.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.

/// RPC versions this implementation speaks, newest first. Must match
/// `rpcSupportedVersions` in Unison's `remote.ml`.
pub const RPC_SUPPORTED_VERSIONS: &[u32] = &[1];
