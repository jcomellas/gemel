//! `gemel-fsmonitor`: the file-watcher helper used by `repeat = watch`.
//!
//! Replaces Unison's `src/fsmonitor/` watchers (inotify, Windows) with one
//! portable implementation, and speaks the same line protocol
//! (`gemel_compat::fsmonitor_protocol`) so that OCaml Unison can use it
//! too when installed as `unison-fsmonitor`.
//!
//! Derived from Unison, Copyright 2012, Benjamin C. Pierce.

use gemel_compat::fsmonitor_protocol::PROTOCOL_VERSION;

fn main() -> std::process::ExitCode {
    eprintln!(
        "gemel-fsmonitor {} (protocol version {PROTOCOL_VERSION}): not implemented yet",
        env!("CARGO_PKG_VERSION")
    );
    std::process::ExitCode::FAILURE
}
