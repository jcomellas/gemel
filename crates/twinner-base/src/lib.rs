//! Foundation: preferences and profiles, CLI flags, names and paths, case handling, feature registry.
//!
//! Ports Unison's `ubase` library plus the low-level modules that
//! everything else depends on. `Name` and `Path` ordering depends on
//! `Case`, which depends on the Unicode tables and the legacy hash, so
//! they live here rather than in `twinner-fs`.

pub mod case;
pub mod clroot;
pub mod features;
pub mod name;
pub mod path;
pub mod prefs;
pub mod proplist;
pub mod rx;
pub mod trace;
pub mod uarg;
pub mod util;
