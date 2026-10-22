//! A server's command loop, and what it shows on the client.
//!
//! Ports Unison `src/remote.ml` (`commandLoop`, `showWarningOnClient` and
//! `forwardMsgToClient`).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! A server serves one client on a transport: it sends its header, agrees
//! on a version with the client (see [`crate::handshake`]), then serves the
//! client's requests until the client goes away. While it does, its
//! warnings and its messages (status lines, log entries) are sent to the
//! client to show, each as a request of its own, queued at once so that it
//! goes out before the replies to the requests under way.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gemel_base::trace::{Forwarder, Message, Trace};
use gemel_base::util::Error;

use crate::connection::{self, Connection, Endpoint, Input, Output};
use crate::handshake;
use crate::registry::ServerCmd;

/// `showWarningOnClient`: shows a server's warning on the client.
pub static SHOW_WARNING_ON_CLIENT: ServerCmd<String, ()> = ServerCmd::new(
    "showWarningOnClient",
    |s, w, _| w.string(s.as_bytes()),
    |r, _| Ok(String::from_utf8_lossy(&r.string()?).into_owned()),
    |(), _, _| {},
    |_, _| Ok(()),
    |conn, s| {
        Box::pin(async move {
            conn.host().warn(&s);
            Ok(())
        })
    },
);

/// `forwardMsgToClient`: shows a server's message on the client
/// (`Trace.displayMessageLocally`).
pub static FORWARD_MSG_TO_CLIENT: ServerCmd<Message, ()> = ServerCmd::new(
    "forwardMsgToClient",
    |m, w, _| m.marshal(w),
    |r, _| Message::unmarshal(r),
    |(), _, _| {},
    |_, _| Ok(()),
    |conn, m| {
        Box::pin(async move {
            let host = conn.host();
            let prefs = host.prefs().borrow();
            let trace = host.state::<Trace>();
            let mut trace = trace.borrow_mut();
            trace.display_message_locally(&prefs, m)
        })
    },
);

thread_local! {
    /// The connections messages are forwarded on, by key: a trace's
    /// forwarder crosses threads with it, so it holds a key, not the
    /// connection (OCaml's `messageForwarder` is a global).
    static FORWARDED_TO: RefCell<HashMap<u64, Connection>> = RefCell::default();
}

static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

/// A forwarder sending messages to the client over `conn`, until it
/// closes.
fn forwarder(conn: &Connection) -> Forwarder {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    FORWARDED_TO.with(|f| f.borrow_mut().insert(key, conn.clone()));
    conn.at_close(move || {
        FORWARDED_TO.with(|f| f.borrow_mut().remove(&key));
    });
    Box::new(move |m| {
        if let Some(conn) = FORWARDED_TO.with(|f| f.borrow().get(&key).cloned()) {
            // Queued now; the reply isn't waited for.
            let _ = conn.start_call(&FORWARD_MSG_TO_CLIENT, m);
        }
    })
}

/// Sets the server's warning printer (`Util.warnPrinter`) and its trace's
/// message forwarder (`Trace.messageForwarder`) to show warnings and
/// messages on the client, until `conn` closes.
fn show_on_client(conn: &Connection) {
    let host = conn.host().clone();
    let c = conn.clone();
    host.set_warn_printer(Some(Rc::new(move |s| {
        let _ = c.start_call(&SHOW_WARNING_ON_CLIENT, s.to_owned());
    })));
    host.state::<Trace>()
        .borrow_mut()
        .set_forwarder(Some(forwarder(conn)));
    // The printer holds the connection, which holds the host.
    let h = host.clone();
    conn.at_close(move || h.set_warn_printer(None));
}

/// `commandLoop`: serves a client on `input` and `output` with
/// `endpoint`'s commands and host, whose preferences are its defaults
/// until the client sends its own. It ends quietly once the client goes
/// away; another failure is returned.
pub async fn command_loop(
    endpoint: Rc<Endpoint>,
    input: Input,
    mut output: Output,
) -> Result<(), Error> {
    endpoint
        .host()
        .state::<Trace>()
        .borrow_mut()
        .running_as_server = true;
    let mut input = handshake::reader(input);
    let version = async {
        handshake::send_server_header(&mut output).await?;
        handshake::check_client_version(&mut input, &mut output).await
    }
    .await;
    let version = match version {
        Err(e) if e == connection::lost() => return Ok(()),
        v => v?,
    };
    let conn = Connection::new(endpoint, true, Box::new(input), output);
    if let Some(v) = version {
        conn.set_version(v);
    }
    show_on_client(&conn);
    connection::serve_connection(conn).await
}
