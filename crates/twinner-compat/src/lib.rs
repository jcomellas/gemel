//! Bit-exact formats shared with OCaml Unison: Umarshal, legacy hash, Unicode tables, fsmonitor protocol.
//!
//! Everything that must match OCaml Unison bit-for-bit lives here, so that
//! the Tier A compatibility surface is in one place and has no other
//! twinner dependencies. Golden test vectors should come from the OCaml
//! binary.

pub mod fsmonitor_protocol;
pub mod ocaml_hash;
pub mod umarshal;
pub mod unicode;
