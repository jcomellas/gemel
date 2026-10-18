//! Synchronization engine: update detection, reconciliation, propagation, archives.
//!
//! The core of the port. Has no UI dependencies; frontends drive it
//! through `twinner-ui-api`.

pub mod abort;
pub mod common;
pub mod copy;
pub mod external;
pub mod files;
pub mod fpcache;
pub mod fswatch;
pub mod globals;
pub mod lock;
pub mod moves;
pub mod negotiate;
pub mod pred;
pub mod recon;
pub mod stasher;
pub mod transfer;
pub mod transport;
pub mod tree;
pub mod update;
pub mod xferhint;
