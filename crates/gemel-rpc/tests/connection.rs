//! Connections: the bytes a peer sees (the write token, flow control's
//! negotiation, urgent writes), requests both ways on one connection,
//! errors, lost connections, servers on other hosts, and the client's
//! table of connections.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gemel_base::clroot::Clroot;
use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_base::util::Error;
use gemel_fs::fspath::Fspath;
use gemel_rpc::connection::{self, Connection, Endpoint, HALF_DUPLEX, LOST_CONNECTION};
use gemel_rpc::host::Host;
use gemel_rpc::protocol::{self, Header};
use gemel_rpc::registry::{Dispatch, HostCmd, Registry, RootCmd, Rpc, ServerCmd};
use gemel_rpc::root::{self, CANONIZE_ON_SERVER, Root};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f)
}

/// The host's name.
static NAME: HostCmd<(), String> = HostCmd::new(
    "name",
    |(), _, _| {},
    |_, _| Ok(()),
    |s, w, _| w.string(s.as_bytes()),
    |r, _| Ok(String::from_utf8(r.string()?).unwrap()),
    |host, ()| Box::pin(async move { Ok(host.canonical_host_name()) }),
);

/// The other side's host name, asked over the connection the request came
/// on, while the request waits: a request from the server to the client.
static ASK_BACK: ServerCmd<(), String> = ServerCmd::new(
    "askBack",
    |(), _, _| {},
    |_, _| Ok(()),
    |s, w, _| w.string(s.as_bytes()),
    |r, _| Ok(String::from_utf8(r.string()?).unwrap()),
    |conn, ()| {
        Box::pin(async move {
            let theirs = conn.call(&PEER_NAME, ()).await?;
            Ok(format!(
                "{} asked {theirs}",
                conn.host().canonical_host_name()
            ))
        })
    },
);

/// The host's name, as a server command.
static PEER_NAME: ServerCmd<(), String> = ServerCmd::new(
    "peerName",
    |(), _, _| {},
    |_, _| Ok(()),
    |s, w, _| w.string(s.as_bytes()),
    |r, _| Ok(String::from_utf8(r.string()?).unwrap()),
    |conn, ()| Box::pin(async move { Ok(conn.host().canonical_host_name()) }),
);

/// Fails on request; reports the root's path otherwise. 'w' waits until
/// the connection is lost.
static PROBE: RootCmd<u8, Vec<u8>> = RootCmd::new(
    "probe",
    |a, w, _| w.char(*a),
    |r, _| r.char(),
    |s, w, _| w.string(s),
    |r, _| r.string(),
    |_, fspath, a| {
        Box::pin(async move {
            match a {
                b't' => Err(Error::Transient("transient failure".into())),
                b'f' => Err(Error::Fatal("fatal failure".into())),
                b'w' => std::future::pending().await,
                _ => Ok(fspath.as_bytes().to_vec()),
            }
        })
    },
);

/// The connection this host's side of a root command sees: the name of
/// the other side's host.
static OTHER_SIDE: ServerCmd<(), String> = ServerCmd::new(
    "otherSide",
    |(), _, _| {},
    |_, _| Ok(()),
    |s, w, _| w.string(s.as_bytes()),
    |r, _| Ok(String::from_utf8(r.string()?).unwrap()),
    |conn, ()| {
        Box::pin(async move {
            let theirs = conn.call(&PEER_NAME, ()).await?;
            Ok(format!(
                "on {} with {theirs}",
                conn.host().canonical_host_name()
            ))
        })
    },
);

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register_host(&NAME).unwrap();
    r.register_host(&CANONIZE_ON_SERVER).unwrap();
    r.register_server(&ASK_BACK).unwrap();
    r.register_server(&PEER_NAME).unwrap();
    r.register_server(&OTHER_SIDE).unwrap();
    r.register_root(&PROBE).unwrap();
    r
}

fn prefs(half_duplex: bool) -> Prefs {
    let mut p = Prefs::new();
    connection::register_prefs(&mut p).unwrap();
    gemel_fs::os::register_prefs(&mut p).unwrap();
    p.set(&HALF_DUPLEX, half_duplex);
    p
}

fn host(name: &str, half_duplex: bool) -> Rc<Host> {
    let h = Host::server(prefs(half_duplex), Features::new());
    h.set_host_name(name);
    Rc::new(h)
}

/// A client with the commands of [`registry`].
fn client(half_duplex: bool) -> Rpc {
    let mut rpc = Rpc::new(Dispatch::Direct(host("client", half_duplex)));
    *rpc.registry_mut() = registry();
    rpc
}

fn remote(name: &str) -> (Clroot, Root) {
    let clroot = Clroot::ConnectBySocket {
        host: name.into(),
        port: "1".into(),
        path: None,
    };
    let root = Root {
        host: root::Host::Remote(name.into()),
        fspath: Fspath::from_canonical(b"/r".to_vec()),
    };
    (clroot, root)
}

/// A client connected to an in-memory server named "server".
fn connected(half_duplex: bool) -> (Rpc, Root, Connection) {
    let rpc = client(half_duplex);
    let (clroot, root) = remote("server");
    let conn = rpc.connect_in_memory(clroot, root.clone(), host("server", false));
    (rpc, root, conn)
}

#[test]
fn requests_go_both_ways_on_one_connection() {
    for half_duplex in [false, true] {
        block_on(async {
            let (rpc, root, conn) = connected(half_duplex);
            assert_eq!(
                rpc.call_host(&NAME, &root.host, ()).await.unwrap(),
                "server"
            );
            assert_eq!(rpc.call_root(&PROBE, &root, b'x').await.unwrap(), b"/r");
            // Requests at once, each calling back the client while it
            // waits, and the client answering on the same connection.
            let calls = (0..20).map(|_| conn.call(&ASK_BACK, ()));
            for r in futures_join(calls).await {
                assert_eq!(r.unwrap(), "server asked client");
            }
            // The canonical root's host, through canonizeOnServer.
            let (name, _) = rpc
                .call_host(&CANONIZE_ON_SERVER, &root.host, (None, true))
                .await
                .unwrap();
            assert_eq!(name, "server");
            // Unless half duplex, both sides turned flow control off.
            assert_eq!(conn.flow_control(), half_duplex);
        });
    }
}

/// Polls the futures together, in order.
async fn futures_join<F: std::future::Future>(fs: impl Iterator<Item = F>) -> Vec<F::Output> {
    let handles: Vec<_> = fs.map(|f| Box::pin(f)).collect();
    let mut out = Vec::new();
    let mut pending: Vec<_> = handles.into_iter().map(Some).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (i, f) in pending.iter_mut().enumerate() {
            if let Some(fut) = f {
                match fut.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        results[i] = Some(v);
                        *f = None;
                    }
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    out.extend(results.into_iter().map(Option::unwrap));
    out
}

#[test]
fn errors_come_back_as_ocaml_sends_them() {
    block_on(async {
        let (rpc, root, _) = connected(false);
        assert_eq!(
            rpc.call_root(&PROBE, &root, b't').await,
            Err(Error::Transient("transient failure".into()))
        );
        assert_eq!(
            rpc.call_root(&PROBE, &root, b'f').await,
            Err(Error::Fatal("Server: fatal failure".into()))
        );
        // A command the server doesn't have.
        let conn = rpc.connection_of_root(&root).unwrap();
        assert_eq!(
            conn.request("nosuch", section("nosuch-args", |_| {})).await,
            Err(Error::Fatal("Server: nosuch not registered!".into()))
        );
        assert!(rpc.command_available(&root, "probe").await.unwrap());
        assert!(!rpc.command_available(&root, "nosuch").await.unwrap());
        // No connection to another host.
        let (_, elsewhere) = remote("elsewhere");
        assert_eq!(
            rpc.call_root(&PROBE, &elsewhere, b'x').await,
            Err(Error::Fatal("No connection with the server".into()))
        );
        // The connection is still usable.
        assert_eq!(rpc.call_root(&PROBE, &root, b'x').await.unwrap(), b"/r");
    });
}

#[test]
fn root_commands_with_the_connection() {
    block_on(async {
        let (rpc, root, _) = connected(false);
        let local = Root::local(Fspath::from_canonical(b"/l".to_vec()));
        // From a local root: runs here, with the connection to the other
        // root's host.
        assert_eq!(
            rpc.call_root_with_connection(&OTHER_SIDE, &local, &root, ())
                .await
                .unwrap(),
            "on client with server"
        );
        // From the remote root: runs there, with its connection to here.
        assert_eq!(
            rpc.call_root_with_connection(&OTHER_SIDE, &root, &local, ())
                .await
                .unwrap(),
            "on server with client"
        );
    });
}

#[test]
fn a_lost_connection_fails_what_waits_and_is_forgotten() {
    block_on(async {
        let (rpc, root, conn) = connected(false);
        let (clroot, _) = remote("server");
        let closed = Rc::new(Cell::new(0));
        let c = closed.clone();
        rpc.client_host()
            .at_conn_close(false, Rc::new(move || c.set(c.get() + 1)));
        let only_server = Rc::new(Cell::new(false));
        let s = only_server.clone();
        rpc.client_host()
            .at_conn_close(true, Rc::new(move || s.set(true)));
        assert!(rpc.connection_checked(&clroot).await.is_some());

        let waiting = rpc.call_root(&PROBE, &root, b'w');
        let close = async {
            tokio::task::yield_now().await;
            conn.close();
        };
        let (r, ()) = join2(waiting, close).await;
        assert_eq!(r, Err(Error::Fatal(LOST_CONNECTION.into())));
        assert_eq!(conn.failure(), Some(Error::Fatal(LOST_CONNECTION.into())));
        assert_eq!(closed.get(), 1);
        assert!(!only_server.get());
        assert!(rpc.connection_checked(&clroot).await.is_none());
        assert_eq!(
            rpc.call_root(&PROBE, &root, b'x').await,
            Err(Error::Fatal("No connection with the server".into()))
        );
        assert_eq!(
            conn.call(&PEER_NAME, ()).await,
            Err(Error::Fatal(LOST_CONNECTION.into()))
        );
    });
}

async fn join2<A: std::future::Future, B: std::future::Future>(
    a: A,
    b: B,
) -> (A::Output, B::Output) {
    let mut a = Box::pin(a);
    let mut b = Box::pin(b);
    let (mut ra, mut rb) = (None, None);
    std::future::poll_fn(|cx| {
        if ra.is_none()
            && let std::task::Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let std::task::Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        if ra.is_some() && rb.is_some() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    (ra.unwrap(), rb.unwrap())
}

#[test]
fn the_client_keeps_one_connection_per_server() {
    block_on(async {
        let rpc = client(false);
        let (clroot, root) = remote("server");
        let first = rpc.connect_in_memory(clroot.clone(), root.clone(), host("server", false));
        // The same server, another path: replaces the first.
        let other_path = Clroot::ConnectBySocket {
            host: "server".into(),
            port: "1".into(),
            path: Some("/elsewhere".into()),
        };
        let second = rpc.connect_in_memory(other_path, root.clone(), host("server", false));
        assert_eq!(rpc.connection_of_root(&root), Ok(second.clone()));
        assert_ne!(first, second);
        assert_eq!(
            rpc.connections().canon_root_of_clroot(&clroot),
            Some(root.clone())
        );
        // Another port is another server.
        let other_port = Clroot::ConnectBySocket {
            host: "server".into(),
            port: "2".into(),
            path: None,
        };
        assert_eq!(rpc.connections().canon_root_of_clroot(&other_port), None);
        // A second host.
        let (clroot2, root2) = remote("another");
        rpc.connect_in_memory(clroot2, root2.clone(), host("another", false));
        assert_eq!(
            rpc.call_host(&NAME, &root2.host, ()).await.unwrap(),
            "another"
        );
        assert_eq!(
            rpc.call_host(&NAME, &root.host, ()).await.unwrap(),
            "server"
        );
    });
}

// ---- What a peer sees ----

/// A section, as `protocol` encodes it.
fn section(tag: &str, write: impl FnOnce(&mut gemel_compat::umarshal::Writer)) -> Vec<u8> {
    protocol::encode_section(tag, write).unwrap()
}

fn message(id: u32, header: Header, body: Option<Vec<u8>>) -> Vec<u8> {
    let mut m = protocol::encode_int(id).to_vec();
    m.extend(header.encode().unwrap());
    m.extend(body.unwrap_or_default());
    m
}

fn token() -> Vec<u8> {
    protocol::encode_int(0).to_vec()
}

/// Reads one message: its id, header and, if it has one, its body.
async fn read_message(peer: &mut DuplexStream) -> (u32, Header, Option<Vec<u8>>) {
    let id = read_int(peer).await;
    if id == 0 {
        return (0, Header::NormalResult, None);
    }
    let header = read_packet(peer).await;
    let header = protocol::decode_section_body("rsp", &header, Header::unmarshal).unwrap();
    let body = match header {
        Header::Request(_) | Header::NormalResult | Header::Stream(_) => {
            Some(read_packet(peer).await)
        }
        _ => None,
    };
    (id, header, body)
}

async fn read_int(peer: &mut DuplexStream) -> u32 {
    let mut b = [0; 5];
    peer.read_exact(&mut b).await.unwrap();
    protocol::decode_int(&b).unwrap()
}

async fn read_packet(peer: &mut DuplexStream) -> Vec<u8> {
    let len = read_int(peer).await;
    let mut b = vec![0; len as usize];
    peer.read_exact(&mut b).await.unwrap();
    b
}

/// A gemel server, and the client's end of its pipe.
fn server(registry: Registry) -> DuplexStream {
    let (peer, ours) = tokio::io::duplex(64 * 1024);
    let (r, w) = tokio::io::split(ours);
    let endpoint = Endpoint::new(Rc::new(registry), host("server", false));
    gemel_rpc::runtime::spawn_local(async move {
        connection::serve(endpoint, Box::new(r), Box::new(w))
            .await
            .unwrap();
    });
    peer
}

fn name_request(id: u32) -> Vec<u8> {
    message(
        id,
        Header::Request("name".into()),
        Some(section("name-args", |_| {})),
    )
}

fn name_reply(id: u32) -> Vec<u8> {
    message(
        id,
        Header::NormalResult,
        Some(section("name-res", |w| w.string(b"server"))),
    )
}

/// What an OCaml client does: waits for the server's token, asks it to
/// turn flow control off and hands the token back, then sends requests
/// without tokens once the server has answered.
#[test]
fn a_server_hands_out_the_token_and_turns_flow_control_off() {
    block_on(async {
        let mut peer = server(registry());
        assert_eq!(read_int(&mut peer).await, 0, "the server's token first");
        let negotiate = message(
            2,
            Header::Request("negociateFlowControl".into()),
            Some(section("negociateFlowControl-args", |_| {})),
        );
        peer.write_all(&[negotiate, token()].concat())
            .await
            .unwrap();
        let reply = message(
            2,
            Header::NormalResult,
            Some(section("negociateFlowControl-res", |w| w.bool(false))),
        );
        let mut got = vec![0; reply.len()];
        peer.read_exact(&mut got).await.unwrap();
        assert_eq!(got, reply);
        // Full duplex now: two requests, two replies, no token.
        peer.write_all(&[name_request(3), name_request(4)].concat())
            .await
            .unwrap();
        // Replies go out as requests finish, in any order.
        let mut got = vec![0; 2 * name_reply(3).len()];
        peer.read_exact(&mut got).await.unwrap();
        let (a, b) = got.split_at(got.len() / 2);
        let mut got = [a, b];
        got.sort();
        let mut want = [name_reply(3), name_reply(4)];
        want.sort();
        assert_eq!(got, want);
        // The client goes: the server ends quietly.
        drop(peer);
    });
}

/// In half-duplex mode the client never negotiates: each side writes only
/// with the token, and hands it back after what it has written.
#[test]
fn a_half_duplex_server_writes_only_with_the_token() {
    block_on(async {
        let mut peer = server(registry());
        assert_eq!(read_int(&mut peer).await, 0);
        peer.write_all(&[name_request(2), token()].concat())
            .await
            .unwrap();
        let reply = [name_reply(2), token()].concat();
        let mut got = vec![0; reply.len()];
        peer.read_exact(&mut got).await.unwrap();
        assert_eq!(got, reply, "the reply, then the token");
        // A stream chunk for a stream the server doesn't have: aborted at
        // once, without the token, and only once.
        let chunk = |id| {
            message(
                id,
                Header::Stream("nosuchstream".into()),
                Some(section("nosuchstream-str", |_| {})),
            )
        };
        peer.write_all(&[chunk(3), chunk(3)].concat())
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut peer).await,
            (3, Header::StreamAbort, None)
        );
        // The token, then the server answers and hands it back.
        peer.write_all(&[name_request(4), token()].concat())
            .await
            .unwrap();
        let mut got = vec![0; reply.len()];
        peer.read_exact(&mut got).await.unwrap();
        assert_eq!(got, [name_reply(4), token()].concat());
    });
}

/// What an OCaml server sees of a gemel client: after its token, the
/// client's negotiation request (id 2, before a request made at once), the
/// other request and the token back; with flow control off, requests
/// without tokens.
#[test]
fn a_client_negotiates_then_writes_freely() {
    block_on(async {
        let (peer, ours) = tokio::io::duplex(64 * 1024);
        let mut peer = peer;
        let (r, w) = tokio::io::split(ours);
        let endpoint = Endpoint::new(Rc::new(registry()), host("client", false));
        let conn = connection::start_client(endpoint, Box::new(r), Box::new(w), false);
        let first = conn.start_call(&PEER_NAME, ()).unwrap();
        peer.write_all(&token()).await.unwrap();
        let (id, header, _) = read_message(&mut peer).await;
        assert_eq!(header, Header::Request("negociateFlowControl".into()));
        assert_eq!(id, 2, "OCaml's first message id");
        let (id3, header, _) = read_message(&mut peer).await;
        assert_eq!((id3, header), (3, Header::Request("peerName".into())));
        assert_eq!(read_int(&mut peer).await, 0, "the token back");
        let negotiated = message(
            id,
            Header::NormalResult,
            Some(section("negociateFlowControl-res", |w| w.bool(false))),
        );
        let name = |id, n: &[u8]| {
            message(
                id,
                Header::NormalResult,
                Some(section("peerName-res", |w| w.string(n))),
            )
        };
        peer.write_all(&[negotiated, name(3, b"ocaml")].concat())
            .await
            .unwrap();
        assert_eq!(first.await.unwrap(), "ocaml");
        let ask = conn.call(&PEER_NAME, ());
        let serve = async {
            let (id, header, _) = read_message(&mut peer).await;
            assert_eq!((id, header), (4, Header::Request("peerName".into())));
            peer.write_all(&name(id, b"again")).await.unwrap();
        };
        let (again, ()) = join2(ask, serve).await;
        assert_eq!(again.unwrap(), "again");
        assert!(!conn.flow_control());
    });
}

/// Garbage on the connection fails it, and the requests waiting with it.
#[test]
fn a_corrupted_message_fails_the_connection() {
    block_on(async {
        let (peer, ours) = tokio::io::duplex(64 * 1024);
        let mut peer = peer;
        let (r, w) = tokio::io::split(ours);
        let endpoint = Endpoint::new(Rc::new(registry()), host("client", false));
        let conn = connection::start_client(endpoint, Box::new(r), Box::new(w), true);
        let ask = conn.call(&PEER_NAME, ());
        let garbage = async {
            peer.write_all(&token()).await.unwrap();
            peer.write_all(b"garbage!").await.unwrap();
            peer
        };
        let (r, _peer) = join2(ask, garbage).await;
        let e = r.unwrap_err().to_string();
        assert!(
            e.starts_with("Protocol error: corrupted message received"),
            "{e}"
        );
        assert_eq!(conn.failure().map(|e| e.to_string()), Some(e));
    });
}

/// Closing handlers on a connection itself run once, at once if it has
/// already closed.
#[test]
fn connection_close_handlers() {
    block_on(async {
        let (_rpc, _root, conn) = connected(false);
        let runs = Rc::new(RefCell::new(Vec::new()));
        let r = runs.clone();
        conn.at_close(move || r.borrow_mut().push("before"));
        conn.close();
        // The other side sees the end of its input, then so does this one.
        while conn.failure().is_none() {
            tokio::task::yield_now().await;
        }
        let r = runs.clone();
        conn.at_close(move || r.borrow_mut().push("after"));
        assert_eq!(*runs.borrow(), ["before", "after"]);
    });
}
