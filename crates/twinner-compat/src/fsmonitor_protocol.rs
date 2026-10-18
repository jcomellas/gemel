//! Line protocol spoken between Unison and the `unison-fsmonitor` helper.
//!
//! Ports Unison `src/fswatch.ml`, Unison `src/fsmonitor/watchercommon.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//! Derived from Unison, Copyright 2012, Benjamin C. Pierce.

/// Protocol version exchanged in the `VERSION` handshake with the watcher
/// (see `fswatch.ml`).
pub const PROTOCOL_VERSION: u32 = 1;
