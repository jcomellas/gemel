//! Blocking filesystem work on a host, one piece at a time.
//!
//! OCaml does a host's filesystem work (copying, backups, renames)
//! synchronously, so none of it overlaps, even with both roots on the
//! same host. Here it runs off the engine thread, under a host lock, so
//! that each piece is still atomic: two roots' backups can share a
//! directory, and renames share the commit log. A root command as a whole
//! isn't: other work can run between its pieces (OCaml's run without
//! yielding), which matters once items propagate concurrently.

use std::rc::Rc;

use gemel_rpc::host::Host;
use gemel_rpc::runtime;

/// The host's lock.
#[derive(Default)]
struct Lock(Rc<tokio::sync::Mutex<()>>);

/// Runs `f` off the engine thread once no other work of `host`'s is.
pub async fn blocking<F, R>(host: &Host, f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let lock = host.state::<Lock>().borrow().0.clone();
    let _held = lock.lock().await;
    runtime::blocking(f).await
}
