//! The engine/frontend contract shared by the text UI and the GUI.
//!
//! Frontends depend only on this crate. The contract is modelled on
//! `uimacbridge.ml`: profile initialisation, update detection, per-item
//! accessors and direction setters, synchronization, progress, ignore
//! and diff. Commands go in and events come out over channels.

pub mod exit_code;
pub mod session;
pub mod sortri;
pub mod uicommon;
