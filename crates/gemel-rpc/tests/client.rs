//! The client's side of the handshake: against gemel's server, and against
//! a server in its compatibility mode (2.52 and 2.53 on a socket, or by
//! ssh without `__new-rpc-mode`), which is upgraded as OCaml upgrades it.
//! The messages for servers that aren't right are checked against OCaml's
//! in `bins/gemel/tests/client.rs`.

use std::rc::Rc;

use gemel_base::features::Features;
use gemel_base::prefs::Prefs;
use gemel_base::util::Error;
use gemel_rpc::connection::{self, Endpoint, Output};
use gemel_rpc::handshake;
use gemel_rpc::host::Host;
use gemel_rpc::protocol;
use gemel_rpc::registry::{HostCmd, Registry};
use gemel_rpc::server;
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

fn endpoint(name: &str) -> Rc<Endpoint> {
    let mut r = Registry::new();
    r.register_host(&NAME).unwrap();
    let mut p = Prefs::new();
    connection::register_prefs(&mut p).unwrap();
    gemel_fs::os::register_prefs(&mut p).unwrap();
    gemel_base::trace::register_prefs(&mut p).unwrap();
    let host = Host::server(p, Features::new());
    host.set_host_name(name);
    Endpoint::new(Rc::new(r), Rc::new(host))
}

/// The client's end of a pipe, split, the handshake's way.
fn client_end(d: DuplexStream) -> (handshake::Reader, Output) {
    let (r, w) = tokio::io::split(d);
    (handshake::reader(Box::new(r)), Box::new(w))
}

/// A gemel client and a gemel server agree on version 1, then a request
/// goes through.
#[test]
fn a_gemel_client_and_server_agree_then_talk() {
    block_on(async {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(theirs);
        let server = endpoint("server");
        gemel_rpc::runtime::spawn_local(async move {
            server::command_loop(server, Box::new(sr), Box::new(sw))
                .await
                .unwrap();
        });
        let (mut r, mut w) = client_end(ours);
        let mut ready = false;
        let v = handshake::client_handshake(&mut r, &mut w, || ready = true)
            .await
            .unwrap();
        assert_eq!(v, 1);
        assert!(ready);
        let conn = connection::start_client(endpoint("client"), Box::new(r), w, false);
        assert_eq!(conn.call_host(&NAME, ()).await.unwrap(), "server");
        conn.close();
    });
}

async fn read_exactly(peer: &mut DuplexStream, n: usize) -> Vec<u8> {
    let mut b = vec![0; n];
    peer.read_exact(&mut b).await.unwrap();
    b
}

/// `magic`, as OCaml sends it: id 1, the length, then a stream abort as
/// OCaml before 4.13 marshalled it.
fn magic() -> Vec<u8> {
    let packet = b"rsp\x84\x95\xa6\xbe\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0A";
    [
        &protocol::encode_int(1)[..],
        &protocol::encode_int(packet.len() as u32),
        packet,
    ]
    .concat()
}

/// A server in its compatibility mode sends its old header, the magic
/// packet and its write token; the client sends the magic packet back, and
/// the handshake starts over.
#[test]
fn a_server_in_its_compatibility_mode_is_upgraded() {
    for old in [
        &b"Unison 2.51 with OCaml >= 4.01.2\n"[..],
        &b"Unison 2.48\n"[..],
    ] {
        block_on(async {
            let (ours, mut peer) = tokio::io::duplex(64 * 1024);
            let (mut r, mut w) = client_end(ours);
            let token = protocol::encode_int(0);
            peer.write_all(&[old, &magic(), &token].concat())
                .await
                .unwrap();
            let client = gemel_rpc::runtime::spawn_local(async move {
                handshake::client_handshake(&mut r, &mut w, || {}).await
            });
            assert_eq!(read_exactly(&mut peer, magic().len()).await, magic());
            peer.write_all(b"Unison RPC\nVERSIONS 1\n").await.unwrap();
            assert_eq!(read_exactly(&mut peer, 10).await, b"VERSION 1\n");
            peer.write_all(b"OK\n").await.unwrap();
            assert_eq!(client.await.unwrap(), Ok(1));
        });
    }
}

/// An old header that isn't followed by the magic packet is an old server
/// (bytes that aren't a message id at all are a corrupted message, as in
/// OCaml).
#[test]
fn an_old_server_without_the_magic_packet_is_refused() {
    block_on(async {
        let (ours, mut peer) = tokio::io::duplex(64 * 1024);
        let (mut r, mut w) = client_end(ours);
        peer.write_all(&[&b"Unison 2.48\n"[..], &protocol::encode_int(2)].concat())
            .await
            .unwrap();
        let e = handshake::client_handshake(&mut r, &mut w, || {})
            .await
            .unwrap_err();
        assert_eq!(
            e,
            Error::Fatal(
                "The server runs an old and unsupported Unison version.\n\
                 Version received from server: Unison 2.48\n\n\
                 Please see the documentation about upgrading."
                    .into()
            )
        );
    });
}
