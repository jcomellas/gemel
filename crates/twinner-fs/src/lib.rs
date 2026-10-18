//! OS layer: filesystem access, properties, xattrs, ACLs, macOS metadata, fingerprints.
//!
//! Platform-specific filesystem access. This is the only crate expected
//! to need `unsafe` (FFI for xattrs, ACLs and clonefile); opt in with a
//! crate-level `#![allow(unsafe_code)]` when that code arrives.

pub mod fileinfo;
pub mod fingerprint;
pub mod fs;
pub mod fspath;
pub mod os;
pub mod osx;
pub mod props;
pub mod reflink;
pub mod system;
