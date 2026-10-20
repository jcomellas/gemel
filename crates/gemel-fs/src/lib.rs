//! OS layer: filesystem access, properties, xattrs, ACLs, macOS metadata, fingerprints.
//!
//! Platform-specific filesystem access, and the metadata values Unison
//! records about files. This is the only crate that uses `unsafe`, each
//! use in a function marked `#[allow(unsafe_code)]`: `utimensat`, user
//! and group names, `getattrlist`/`setattrlist` on macOS, `umask`,
//! `gethostname`, and the Windows file-information calls behind `stat`.
//!
//! Not ported yet: AppleDouble files (resource forks on volumes without
//! them), reading and setting xattrs and ACLs, and copy-on-write cloning
//! (`reflink`, which comes with copying in phase 6 of milestone 2).

pub mod fileinfo;
pub mod fingerprint;
pub mod fs;
pub mod fspath;
pub mod os;
pub mod osx;
pub mod props;
pub mod reflink;
pub mod system;
