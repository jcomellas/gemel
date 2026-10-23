//! A host's filesystem work, done at once.
//!
//! OCaml does a host's filesystem work (copying, backups, renames, the
//! rescans that check nothing changed, a transfer's chunks) synchronously:
//! a root command runs without yielding until its next request to the
//! other side, so nothing else of the host's runs meanwhile, even with
//! both roots on the same host, and items propagating concurrently do
//! their work, and send their replies, in OCaml's order. Here too, on the
//! engine thread.
//!
//! Only a copy between local roots runs on another thread
//! ([`gemel_rpc::runtime::blocking`]), so that an abort reaches it as it
//! runs. Items between local roots propagate one at a time, so nothing
//! else of the host's runs meanwhile either.

/// Runs `f` at once: the future is ready when first polled, without
/// yielding.
pub async fn now<R>(f: impl FnOnce() -> R) -> R {
    f()
}
