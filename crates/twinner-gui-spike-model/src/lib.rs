//! Throwaway support code for the GUI spike in §6 of
//! `docs/unison-port-analysis.md`.
//!
//! Both spike frontends (`twinner-gui-spike-egui` and
//! `twinner-gui-spike-slint`) render the same state from this crate, so the
//! comparison measures the toolkits rather than two different models. It
//! holds a fake engine feed, the recon row model with sorting and a
//! multi-selection keyed by stable row id, frame-time statistics and the
//! shared command-line flags. It has no toolkit dependencies.
//!
//! None of this is the real engine/frontend contract: that will live in
//! `twinner-ui-api`. Delete this crate once the spike is decided.

pub mod args;
pub mod engine;
pub mod stats;
pub mod table;
pub mod task;
