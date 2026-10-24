//! The oracle's harness as a library: generating scenarios, running an
//! implementation through them, and comparing what it did with what
//! another did. The `gemel-oracle` program runs it; `gemel-gui`'s tests
//! run it with the graphical interface as one side.
//!
//! See the program's documentation and `tools/oracle/README.md`.

pub mod compare;
pub mod record;
pub mod run;
pub mod scenario;
pub mod shim;
