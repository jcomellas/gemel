//! Connections over TCP and Unix-domain sockets: the client's side, and a
//! server waiting for clients on a port or a path.
//!
//! Ports Unison `src/remote.ml` (`buildSocket`, `buildConnectSocket`,
//! `buildListenSocket`, `buildSocketConnection` and `waitOnPort`).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! A socket root is `socket://host:port/path`, or `socket://{path}/path`
//! for a Unix-domain socket. The client tries the host's addresses in the
//! resolver's order; one whose family or protocol the system lacks is
//! skipped, and any other failure ends the attempt at once, as in OCaml.
//!
//! A server (`-socket`) listens on a port, on every address of each host
//! given by `-listen` (by default on all of them), or on a path. It serves
//! its clients one at a time, each with its preferences at their defaults
//! until the client sends its own, and stops after one if the client sets
//! `killserver`. It removes the socket files it made as it stops.
//!
//! The TCP method has no authentication or encryption: anyone who can
//! reach the port can read and write the server's files as its user.

use std::future::{Future, poll_fn};
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::rc::Rc;
use std::task::Poll;

use gemel_base::util::{self, Error};
use gemel_fs::net;
use gemel_fs::system::{SysError, error_message};
use socket2::{Domain, SockAddr, SockRef, Socket, Type};
use tokio::net::{TcpListener, TcpStream};

use crate::connection::{Connection, Endpoint, Input, KILL_SERVER, Output};
use crate::{handshake, runtime, server};

/// What makes a socket's address unusable here, so that the next one is
/// tried (`EAFNOSUPPORT`, `EPROTONOSUPPORT` and `EINVAL`).
fn skipped(e: &io::Error) -> bool {
    #[cfg(unix)]
    let codes = [libc::EAFNOSUPPORT, libc::EPROTONOSUPPORT, libc::EINVAL];
    // WSAEAFNOSUPPORT, WSAEPROTONOSUPPORT, WSAEINVAL.
    #[cfg(not(unix))]
    let codes = [10047, 10043, 10022];
    e.raw_os_error().is_some_and(|c| codes.contains(&c))
}

/// A Unix-domain socket's path too long for its address: OCaml fails
/// with `ENAMETOOLONG` where the standard library has no OS error.
#[cfg(unix)]
fn path_error(e: io::Error) -> io::Error {
    if e.raw_os_error().is_none() && e.kind() == io::ErrorKind::InvalidInput {
        io::Error::from_raw_os_error(libc::ENAMETOOLONG)
    } else {
        e
    }
}

/// `printAddr`.
fn print_addr(host: &str, addr: &SocketAddr) -> String {
    format!("{host}[{}]:{}", addr.ip(), addr.port())
}

// ---- The client ----

/// `buildSocketConnection`: connects to the server at `host` and `port`
/// (or at the path `host` gives in braces, for a Unix-domain socket) and,
/// once the handshake is done, the client's side of a connection served
/// by `endpoint`.
pub async fn connect(
    endpoint: Rc<Endpoint>,
    host: &str,
    port: &str,
    half_duplex: bool,
) -> Result<Connection, Error> {
    let (input, output) = connect_socket(host, port).await?;
    handshake::init_connection(endpoint, input, output, half_duplex).await
}

/// `buildConnectSocket`.
async fn connect_socket(host: &str, port: &str) -> Result<(Input, Output), Error> {
    if host.is_empty() || host.starts_with('{') {
        return connect_unix(host).await;
    }
    const ERR: &str = "Failed to connect to the server on host ";
    let (h, p) = (host.to_owned(), port.to_owned());
    let addrs = runtime::blocking(move || net::getaddrinfo(&h, &p, false)).await;
    for addr in addrs {
        match connect_tcp(addr).await {
            Ok(stream) => {
                let (r, w) = stream.into_split();
                return Ok((Box::new(r), Box::new(w)));
            }
            Err(e) if skipped(&e) => {}
            Err(e) => {
                return Err(Error::Fatal(format!(
                    "{ERR}{}: {}\n",
                    print_addr(host, &addr),
                    error_message(&e)
                )));
            }
        }
    }
    Err(Error::Fatal(format!("{ERR}{host}:{port}")))
}

/// Connects to `addr` as Unison's Lwt does (`connect`, then
/// `check_socket`): once a connection under way can be written to, it is
/// made if the socket has a peer, and otherwise reading says why not. So
/// the errors are OCaml's: on macOS, a refused connection has no peer
/// with `EINVAL`, which skips to the next address, where Linux says
/// `ENOTCONN` and reading then says it was refused.
#[cfg(unix)]
async fn connect_tcp(addr: SocketAddr) -> io::Result<TcpStream> {
    let socket = Socket::new(
        Domain::for_address(addr),
        Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_nonblocking(true)?;
    match socket.connect(&addr.into()) {
        Ok(()) => {}
        Err(e)
            if e.raw_os_error() == Some(libc::EINPROGRESS)
                || e.kind() == io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e),
    }
    let stream = TcpStream::from_std(std::net::TcpStream::from(socket))?;
    loop {
        stream.writable().await?;
        match stream.peer_addr() {
            Ok(_) => return Ok(stream),
            Err(e) if e.raw_os_error() != Some(libc::ENOTCONN) => return Err(e),
            Err(_) => match stream.try_read(&mut [0; 1]) {
                // Still under way: wait until it can be written again.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let _ = stream.try_io(tokio::io::Interest::WRITABLE, || {
                        Err::<(), _>(io::ErrorKind::WouldBlock.into())
                    });
                }
                Err(e) => return Err(e),
                Ok(_) => return Ok(stream),
            },
        }
    }
}

/// Windows: as tokio connects (phase 6).
#[cfg(not(unix))]
async fn connect_tcp(addr: SocketAddr) -> io::Result<TcpStream> {
    let socket = match addr {
        SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
        SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
    }?;
    socket.connect(addr).await
}

/// `buildConnectSocketUnix`: `host` is the path, in braces.
async fn connect_unix(host: &str) -> Result<(Input, Output), Error> {
    const ERR: &str = "Can't connect to Unix domain socket on path ";
    let path = host
        .strip_prefix('{')
        .and_then(|h| h.strip_suffix('}'))
        .unwrap_or(host);
    #[cfg(unix)]
    match tokio::net::UnixStream::connect(path)
        .await
        .map_err(path_error)
    {
        Ok(stream) => {
            let (r, w) = stream.into_split();
            Ok((Box::new(r), Box::new(w)))
        }
        Err(e) if skipped(&e) => Err(Error::Fatal(format!("{ERR}{path}"))),
        Err(e) => Err(Error::Fatal(format!(
            "{ERR}{path}: {}\n",
            error_message(&e)
        ))),
    }
    // Unix-domain sockets on Windows come with phase 6.
    #[cfg(not(unix))]
    Err(Error::Fatal(format!("{ERR}{path}")))
}

// ---- The server ----

/// A socket the server listens on.
enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
}

/// A client a [`Listener`] accepted.
enum Client {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl Client {
    fn split(self) -> (Input, Output) {
        match self {
            Client::Tcp(s) => {
                let (r, w) = s.into_split();
                (Box::new(r), Box::new(w))
            }
            #[cfg(unix)]
            Client::Unix(s) => {
                let (r, w) = s.into_split();
                (Box::new(r), Box::new(w))
            }
        }
    }
}

/// The socket files a server made, removed as it stops
/// (`unixSocketCleanup`), however it stops.
#[derive(Default)]
struct Cleanup(Vec<std::path::PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A listening socket on `addr` (`buildSocket`'s `Bind`): IPv6 only for
/// an IPv6 address, so that the IPv4 one can be bound too, addresses
/// reused, and one pending client at most.
fn bind(addr: &SockAddr, v6: bool) -> io::Result<Socket> {
    let socket = Socket::new(addr.domain(), Type::STREAM, None)?;
    if v6 {
        socket.set_only_v6(true)?;
    }
    if addr.domain() != Domain::UNIX {
        socket.set_reuse_address(true)?;
    }
    socket.bind(addr)?;
    socket.listen(1)?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// `buildListenSocket`: the sockets to listen on, on `port` at each of
/// `hosts`' addresses (none means all of them), or at the path `port` if
/// it isn't a number.
fn listen(hosts: &[String], port: &str, cleanup: &mut Cleanup) -> Result<Vec<Listener>, Error> {
    if util::int_of_string(port).is_none() {
        return listen_unix(port, cleanup);
    }
    let addrs: Vec<SocketAddr> = hosts
        .iter()
        .flat_map(|h| net::getaddrinfo(h, port, true))
        .collect();
    // Every address is tried; the first failure is the one reported.
    let mut listeners = Vec::new();
    let mut failure = None;
    for addr in addrs {
        let bound = bind(&addr.into(), addr.is_ipv6())
            .and_then(|s| TcpListener::from_std(std::net::TcpListener::from(s)));
        match bound {
            Ok(l) => listeners.push(Listener::Tcp(l)),
            Err(e) if skipped(&e) => {}
            Err(e) => {
                failure.get_or_insert_with(|| {
                    Error::Fatal(format!(
                        "Can't bind socket to port {port} at address [{}]: {}\n",
                        addr.ip(),
                        error_message(&e)
                    ))
                });
            }
        }
    }
    if let Some(e) = failure {
        return Err(e);
    }
    if listeners.is_empty() {
        return Err(Error::Fatal(format!("Can't bind socket to port {port}")));
    }
    Ok(listeners)
}

/// `buildListenSocketUnix`.
fn listen_unix(path: &str, cleanup: &mut Cleanup) -> Result<Vec<Listener>, Error> {
    let none = || Error::Fatal(format!("Can't bind Unix domain socket on path {path}"));
    #[cfg(unix)]
    {
        let bound = SockAddr::unix(path)
            .map_err(path_error)
            .and_then(|a| bind(&a, false))
            .and_then(|s| {
                tokio::net::UnixListener::from_std(std::os::unix::net::UnixListener::from(s))
            });
        match bound {
            Ok(l) => {
                cleanup.0.push(path.into());
                Ok(vec![Listener::Unix(l)])
            }
            Err(e) if skipped(&e) => Err(none()),
            Err(e) => Err(Error::Fatal(format!(
                "Can't bind socket to path '{path}': {}\n",
                error_message(&e)
            ))),
        }
    }
    // Unix-domain sockets on Windows come with phase 6.
    #[cfg(not(unix))]
    {
        let _ = cleanup;
        Err(none())
    }
}

/// Whether accepting a client failed for that client only, or for a
/// while, so that the server goes on (`accept_retry`).
fn transient(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        [
            libc::ECONNABORTED,
            libc::EPERM,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOBUFS,
            libc::ENOMEM,
            libc::ENETUNREACH,
            libc::EHOSTUNREACH,
            libc::ENETDOWN,
            libc::EHOSTDOWN,
            libc::ETIMEDOUT,
        ]
        .contains(&e.raw_os_error().unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        matches!(
            e.kind(),
            io::ErrorKind::ConnectionAborted
                | io::ErrorKind::PermissionDenied
                | io::ErrorKind::OutOfMemory
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::NetworkDown
                | io::ErrorKind::TimedOut
        )
    }
}

/// The next client on any of `listeners` (`accept_retry` on each, the
/// first to come), with keepalive on for TCP.
async fn accept(listeners: &[Listener]) -> Result<Client, Error> {
    loop {
        let accepted = poll_fn(|cx| {
            for l in listeners {
                let r = match l {
                    Listener::Tcp(l) => l.poll_accept(cx).map_ok(|(s, _)| Client::Tcp(s)),
                    #[cfg(unix)]
                    Listener::Unix(l) => l.poll_accept(cx).map_ok(|(s, _)| Client::Unix(s)),
                };
                if r.is_ready() {
                    return r;
                }
            }
            Poll::Pending
        })
        .await;
        match accepted {
            Ok(client) => {
                match &client {
                    Client::Tcp(s) => {
                        let _ = SockRef::from(s).set_keepalive(true);
                    }
                    #[cfg(unix)]
                    Client::Unix(_) => {}
                }
                return Ok(client);
            }
            Err(e) if transient(&e) => eprintln!(
                "server: continuing after receiving an error when accepting client \
                 connection: {}",
                error_message(&e)
            ),
            Err(e) => return Err(SysError::new("accept", b"", e).to_fatal("waiting on port")),
        }
    }
}

/// `waitOnPort`: listens on `port` at `hosts` (see [`listen`]) and serves
/// each client in turn with a new endpoint from `endpoint`, until one sets
/// `killserver` or `stop` completes. Between clients the server's close
/// handlers run.
pub async fn wait_on_port(
    hosts: &[String],
    port: &str,
    endpoint: impl Fn() -> Result<Rc<Endpoint>, Error>,
    stop: impl Future<Output = ()>,
) -> Result<(), Error> {
    let mut cleanup = Cleanup::default();
    let hosts = if hosts.is_empty() {
        &[String::new()][..]
    } else {
        hosts
    };
    let listeners = listen(hosts, port, &mut cleanup)?;
    eprintln!("server started");
    let serve = async {
        loop {
            let (input, output) = accept(&listeners).await?.split();
            let endpoint = endpoint()?;
            server::command_loop(endpoint.clone(), input, output).await?;
            // The client has closed its end of the connection.
            let host = endpoint.host();
            if *host.prefs().borrow().read(&KILL_SERVER) {
                return Ok(());
            }
            host.run_conn_close_handlers(true);
        }
    };
    let (mut serve, mut stop) = (pin!(serve), pin!(stop));
    poll_fn(|cx| {
        if stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(()));
        }
        serve.as_mut().poll(cx)
    })
    .await
}
