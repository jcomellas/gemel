//! `-server`: serving a client on standard input and output, as ssh starts
//! a server on the remote host; and `-socket`: serving clients on a socket.
//!
//! Ports Unison `src/main.ml`'s server modes (`catch_all`) and
//! `src/remote.ml`'s `beAServer`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! The server is a host of its own, with every preference and command
//! registered and the preferences at their defaults: the client sends its
//! own once connected. A socket server makes a new one for each client.
//! What a server prints goes to standard error.

use std::rc::Rc;
use std::sync::Arc;

use gemel_base::trace::Trace;
use gemel_base::util::Error;
use gemel_fs::os;
use gemel_fs::signal::{self, Signal};
use gemel_fs::system::SysError;
use gemel_rpc::connection::{self, Endpoint};
use gemel_rpc::host::Host;
use gemel_rpc::registry::{Dispatch, Rpc};
use gemel_rpc::{runtime, server, socket};
use tokio::sync::Notify;

/// `catch_all`, around creating the Unison directory and `beAServer`: the
/// exit code. A failure is shown, and exits with 1.
pub fn run() -> i32 {
    catch_all(be_a_server)
}

/// `catch_all`, around creating the Unison directory and `waitOnPort`:
/// the exit code. Unlike `-server`, it stays in the current directory.
pub fn run_socket(hosts: &[String], port: &str) -> i32 {
    catch_all(|| wait_on_port(hosts, port))
}

/// `catch_all`: creates the Unison directory and runs `f`; the exit code.
/// A failure is shown, and exits with 1.
fn catch_all(f: impl FnOnce() -> Result<(), Error>) -> i32 {
    let result = os::create_unison_dir(&os::unison_dir()).and_then(|()| f());
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!(
                "Unison server failed: {}",
                gemel_ui_api::uicommon::exn2string(&e)
            );
            1
        }
    }
}

/// `beAServer`: changes to the home directory, then serves the client on
/// standard input and output until it goes away.
fn be_a_server() -> Result<(), Error> {
    match std::env::var_os("HOME") {
        Some(home) => std::env::set_current_dir(&home).map_err(|e| {
            let bytes = gemel_fs::system::name_bytes(&home);
            SysError::new("chdir", &bytes, e).to_fatal_sys("changing working directory")
        })?,
        None => {
            eprintln!("Environment variable HOME unbound: executing server in current directory")
        }
    }
    let served = runtime::run_here(async {
        let (input, output) = connection::stdio();
        server::command_loop(endpoint()?, input, output).await
    });
    served.map_err(|e| Error::Fatal(e.to_string()))?
}

/// `waitOnPort`, until a client sets `killserver` or Ctrl-C
/// (`Sys.catch_break`), which ends it quietly.
fn wait_on_port(hosts: &[String], port: &str) -> Result<(), Error> {
    let interrupted = Arc::new(Notify::new());
    let i = interrupted.clone();
    if signal::listen(move |s| {
        if s == Signal::Interrupt {
            i.notify_one();
        }
    })
    .is_ok()
    {
        let _ = signal::catch(Signal::Interrupt);
    }
    let served = runtime::run_here(socket::wait_on_port(
        hosts,
        port,
        endpoint,
        interrupted.notified(),
    ));
    served.map_err(|e| Error::Fatal(e.to_string()))?
}

/// The server's host and commands.
fn endpoint() -> Result<Rc<Endpoint>, Error> {
    let unison_dir = os::unison_dir();
    let mut prefs = gemel_ui_api::new_prefs()?;
    prefs.set_unison_dir(&unison_dir);
    let host = Rc::new(Host::server(prefs, gemel_ui_api::new_features()?));
    host.set_unison_dir(&unison_dir);
    let mut trace = Trace::with_sinks(Box::new(std::io::stderr()), Box::new(std::io::stderr()));
    trace.set_unison_dir(&unison_dir);
    *host.state::<Trace>().borrow_mut() = trace;
    let mut rpc = Rpc::new(Dispatch::Direct(host.clone()));
    gemel_ui_api::register_commands(&mut rpc)?;
    Ok(Endpoint::new(rpc.registry().clone(), host))
}
