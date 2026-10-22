//! A server's command loop: the handshake's server side, byte for byte as
//! OCaml's, and the warnings and messages it sends the client.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_base::trace::{self, Trace};
use gemel_base::util::Error;
use gemel_rpc::connection::{self, Connection, Endpoint};
use gemel_rpc::host::Host;
use gemel_rpc::protocol::{self, Header};
use gemel_rpc::registry::{Registry, ServerCmd};
use gemel_rpc::server;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::task::JoinHandle;

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f)
}

/// Warns, shows a message and logs, on the host it runs on, then answers.
static CHATTY: ServerCmd<(), String> = ServerCmd::new(
    "chatty",
    |(), _, _| {},
    |_, _| Ok(()),
    |s, w, _| w.string(s.as_bytes()),
    |r, _| Ok(String::from_utf8(r.string()?).unwrap()),
    |conn, ()| {
        Box::pin(async move {
            let host = conn.host();
            host.warn("careful\n");
            let prefs = host.prefs().borrow();
            let t = host.state::<Trace>();
            t.borrow_mut().message(&prefs, "hello from the server")?;
            t.borrow_mut().log(&prefs, "logged on the server\n")?;
            Ok("done".to_owned())
        })
    },
);

fn prefs() -> Prefs {
    let mut p = Prefs::new();
    connection::register_prefs(&mut p).unwrap();
    gemel_fs::os::register_prefs(&mut p).unwrap();
    trace::register_prefs(&mut p).unwrap();
    p
}

fn endpoint(name: &str) -> Rc<Endpoint> {
    let mut r = Registry::new();
    r.register_server(&CHATTY).unwrap();
    let host = Host::server(prefs(), Features::new());
    host.set_host_name(name);
    Endpoint::new(Rc::new(r), Rc::new(host))
}

/// A server's command loop, and the client's end of its pipe.
fn server() -> (DuplexStream, JoinHandle<Result<(), Error>>) {
    let (peer, ours) = tokio::io::duplex(64 * 1024);
    let (r, w) = tokio::io::split(ours);
    let endpoint = endpoint("server");
    let served = gemel_rpc::runtime::spawn_local(async move {
        server::command_loop(endpoint, Box::new(r), Box::new(w)).await
    });
    (peer, served)
}

const HEADER: &[u8] = b"Unison RPC\nVERSIONS 1\n";

async fn read_exactly(peer: &mut DuplexStream, n: usize) -> Vec<u8> {
    let mut b = vec![0; n];
    peer.read_exact(&mut b).await.unwrap();
    b
}

fn token() -> Vec<u8> {
    protocol::encode_int(0).to_vec()
}

/// The server sends its header and versions, accepts version 1 (a `\r`
/// before the newline is skipped), hands out the write token, then serves.
#[test]
fn a_server_says_what_it_speaks_then_accepts_version_1() {
    for version in ["VERSION 1\n", "VERSION 1\r\n", "VERSION 0x1\n"] {
        block_on(async {
            let (mut peer, served) = server();
            assert_eq!(read_exactly(&mut peer, HEADER.len()).await, HEADER);
            peer.write_all(version.as_bytes()).await.unwrap();
            assert_eq!(read_exactly(&mut peer, 3).await, b"OK\n", "{version:?}");
            assert_eq!(read_exactly(&mut peer, 5).await, token(), "{version:?}");
            drop(peer);
            served.await.unwrap().unwrap();
        });
    }
}

/// A version the server doesn't speak, or anything else, gets `NOK` with
/// OCaml's reason; the server then goes on (it hands out the token) until
/// the client hangs up.
#[test]
fn a_server_refuses_with_ocamls_reasons() {
    let cases: &[(&[u8], &str)] = &[
        (
            b"VERSION 2\n",
            "Client RPC version not supported. Version received from client: \"2\". \
             Supported RPC versions: \"1\"",
        ),
        (
            b"VERSION \n",
            "Unknown client RPC version: invalid format. Version received from client: \"\". \
             Supported RPC versions: \"1\"",
        ),
        (
            b"VERSION one\n",
            "Unknown client RPC version: parse error. Version received from client: \"one\". \
             Supported RPC versions: \"1\"",
        ),
        (
            b"NOK oops\n",
            "Could not negotiate RPC version. Received unexpected error from the client: \"oops\"",
        ),
        (
            b"HELLO\nmore",
            "Could not negotiate RPC version. Received unexpected header from the client: \
             \"HELLO\\nmore\"",
        ),
        (
            b"VERSIONS\t1\n",
            "Could not negotiate RPC version. Received unexpected header from the client: \
             \"VERSIONS\\t1\\n\"",
        ),
    ];
    for (input, reason) in cases {
        block_on(async {
            let (mut peer, served) = server();
            read_exactly(&mut peer, HEADER.len()).await;
            peer.write_all(input).await.unwrap();
            let nok = format!("NOK {reason}\n");
            let got = read_exactly(&mut peer, nok.len()).await;
            assert_eq!(String::from_utf8_lossy(&got), nok);
            assert_eq!(read_exactly(&mut peer, 5).await, token());
            drop(peer);
            served.await.unwrap().unwrap();
        });
    }
}

/// A client that goes away during the handshake ends the server quietly.
#[test]
fn a_client_gone_during_the_handshake_ends_the_server_quietly() {
    block_on(async {
        let (mut peer, served) = server();
        peer.write_all(b"VERS").await.unwrap();
        drop(peer);
        served.await.unwrap().unwrap();
    });
}

/// Output that tests can read back.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

/// A client's side of the handshake, then its connection.
async fn connect(mut peer: DuplexStream, client: Rc<Endpoint>) -> Connection {
    assert_eq!(read_exactly(&mut peer, HEADER.len()).await, HEADER);
    peer.write_all(b"VERSION 1\n").await.unwrap();
    assert_eq!(read_exactly(&mut peer, 3).await, b"OK\n");
    let (r, w) = tokio::io::split(peer);
    connection::start_client(client, Box::new(r), Box::new(w), false)
}

/// What the server warns and shows goes to the client, each as a request
/// of its own, before the reply to the request that caused it.
#[test]
fn warnings_and_messages_reach_the_client_before_the_reply() {
    block_on(async {
        let (peer, served) = server();
        let client = endpoint("client");
        let warnings = Rc::new(RefCell::new(Vec::new()));
        let w = warnings.clone();
        client
            .host()
            .set_warn_printer(Some(Rc::new(move |s| w.borrow_mut().push(s.to_owned()))));
        let stderr = Shared::default();
        *client.host().state::<Trace>().borrow_mut() =
            Trace::with_sinks(Box::new(std::io::sink()), Box::new(stderr.clone()));
        let dir = tempdir();
        client.host().prefs().borrow_mut().set_unison_dir(&dir);
        client
            .host()
            .state::<Trace>()
            .borrow_mut()
            .set_unison_dir(&dir);
        let conn = connect(peer, client).await;
        assert_eq!(conn.call(&CHATTY, ()).await.unwrap(), "done");
        assert_eq!(*warnings.borrow(), ["careful\n"]);
        assert_eq!(
            stderr.text(),
            "hello from the server\nlogged on the server\n"
        );
        // The log entry is written on the client, as OCaml's is.
        let log = std::fs::read_to_string(dir.join("unison.log")).unwrap();
        assert_eq!(log, "logged on the server\n");
        conn.close();
        served.await.unwrap().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    });
}

fn tempdir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("gemel-rpc-server-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// After `OK` and the token come packets: a warning goes out as a request
/// of the server's, as an OCaml client reads it.
#[test]
fn after_the_handshake_the_server_speaks_packets() {
    block_on(async {
        let (mut peer, served) = server();
        read_exactly(&mut peer, HEADER.len()).await;
        peer.write_all(b"VERSION 1\n").await.unwrap();
        read_exactly(&mut peer, 3 + 5).await;
        let request = [
            &protocol::encode_int(2)[..],
            &Header::Request("chatty".into()).encode().unwrap(),
            &protocol::encode_section("chatty-args", |_| {}).unwrap(),
            &token(),
        ]
        .concat();
        peer.write_all(&request).await.unwrap();
        // The warning, as a request of the server's.
        let mut id = [0; 5];
        peer.read_exact(&mut id).await.unwrap();
        let len = protocol::decode_int(&read_exactly(&mut peer, 5).await).unwrap();
        let header = read_exactly(&mut peer, len as usize).await;
        let header = protocol::decode_section_body("rsp", &header, Header::unmarshal).unwrap();
        assert_eq!(header, Header::Request("showWarningOnClient".into()));
        drop(peer);
        served.await.unwrap().unwrap();
    });
}
