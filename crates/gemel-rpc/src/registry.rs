//! Registration and dispatch of remote commands.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Engine modules declare their remote commands as statics: a
//! [`HostCmd`] runs on a host (`Remote.registerHostCmd`), a [`RootCmd`] on
//! the host of a root, with the root's path as an extra first argument
//! (`Remote.registerRootCmd`), and a [`ServerCmd`] with the connection it
//! came on (`Remote.registerServerCmd`, and
//! `registerRootCmdWithConnection`, which sends the same). Each has codecs
//! for its argument and result and an async function that runs on the
//! host. The codecs get the features in effect on their side, since some
//! encodings depend on them (`Props` and `prevState`, for example).
//! Commands are registered with an [`Rpc`], which dispatches calls:
//!
//! - a remote root's go over the connection to its host, which
//!   [`ClientConns`] keeps (one per server, shared by the roots there);
//! - a local root's are called directly ([`Dispatch::Direct`]), as
//!   `registerHostCmd` does in OCaml, or, for tests, over an in-memory
//!   connection to a separate [`Host`] whose preferences come from the
//!   client's dump ([`Dispatch::Loopback`]): packets, message ids, flow
//!   control and the server's command loop, everything a server would
//!   receive and send but the handshake and the transport. A fatal error
//!   comes back without the `Server: ` a remote root's gets, as for the
//!   local roots it stands for.
//!
//! An in-memory connection can also stand for a server on another host
//! ([`Rpc::connect_in_memory`]), whose roots are then remote.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};

use gemel_base::clroot::Clroot;
use gemel_base::features::Features;
use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fspath::Fspath;

use crate::connection::{self, Connection, Endpoint, HALF_DUPLEX, NEGOCIATE_FLOW_CONTROL};
use crate::host::Host;
use crate::protocol::{self, HEADER_TAG};
use crate::root::{Host as RootHost, Root};
use crate::server::{FORWARD_MSG_TO_CLIENT, SHOW_WARNING_ON_CLIENT};

/// A future on the engine thread.
pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Writes a value, given the features in effect.
pub type Write<T> = fn(&T, &mut Writer, &Features);
/// Reads a value, given the features in effect.
pub type Read<T> = fn(&mut Reader<'_>, &Features) -> umarshal::Result<T>;
/// Runs a host command.
pub type HostFn<A, R> = fn(Rc<Host>, A) -> LocalBoxFuture<Result<R, Error>>;
/// Runs a root command, given the root's path.
pub type RootFn<A, R> = fn(Rc<Host>, Fspath, A) -> LocalBoxFuture<Result<R, Error>>;
/// Runs a server command, given the connection it came on (whose host is
/// the one it runs on).
pub type ServerFn<A, R> = fn(Connection, A) -> LocalBoxFuture<Result<R, Error>>;

/// A command that runs on a host (`Remote.registerHostCmd`).
pub struct HostCmd<A, R> {
    name: &'static str,
    write_arg: Write<A>,
    read_arg: Read<A>,
    write_res: Write<R>,
    read_res: Read<R>,
    run: HostFn<A, R>,
}

impl<A, R> HostCmd<A, R> {
    /// A command named `name` (its wire name).
    pub const fn new(
        name: &'static str,
        write_arg: Write<A>,
        read_arg: Read<A>,
        write_res: Write<R>,
        read_res: Read<R>,
        run: HostFn<A, R>,
    ) -> Self {
        HostCmd {
            name,
            write_arg,
            read_arg,
            write_res,
            read_res,
            run,
        }
    }

    /// The command's name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// A command that runs on the host of a root (`Remote.registerRootCmd`):
/// the root's path is sent first.
pub struct RootCmd<A, R> {
    name: &'static str,
    write_arg: Write<A>,
    read_arg: Read<A>,
    write_res: Write<R>,
    read_res: Read<R>,
    run: RootFn<A, R>,
}

impl<A, R> RootCmd<A, R> {
    /// A command named `name` (its wire name).
    pub const fn new(
        name: &'static str,
        write_arg: Write<A>,
        read_arg: Read<A>,
        write_res: Write<R>,
        read_res: Read<R>,
        run: RootFn<A, R>,
    ) -> Self {
        RootCmd {
            name,
            write_arg,
            read_arg,
            write_res,
            read_res,
            run,
        }
    }

    /// The command's name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// A command that runs with the connection it came on
/// (`Remote.registerServerCmd`). `registerRootCmdWithConnection` sends the
/// same; [`Rpc::call_root_with_connection`] is its client side.
pub struct ServerCmd<A, R> {
    name: &'static str,
    write_arg: Write<A>,
    read_arg: Read<A>,
    write_res: Write<R>,
    read_res: Read<R>,
    run: ServerFn<A, R>,
}

impl<A, R> ServerCmd<A, R> {
    /// Decodes an argument section's body (as received), with `features`.
    pub fn decode_arg(&self, body: &[u8], features: &Features) -> Result<A, Error> {
        protocol::decode_section_body(&args_tag(self.name), body, |r| (self.read_arg)(r, features))
    }

    /// Encodes an argument section, with `features`.
    pub fn encode_arg(&self, a: &A, features: &Features) -> Result<Vec<u8>, Error> {
        protocol::encode_section(&args_tag(self.name), |w| (self.write_arg)(a, w, features))
    }

    /// Decodes a result section's body (as received), with `features`.
    pub fn decode_res(&self, body: &[u8], features: &Features) -> Result<R, Error> {
        protocol::decode_section_body(&res_tag(self.name), body, |r| (self.read_res)(r, features))
    }

    /// Encodes a result section, with `features`.
    pub fn encode_res(&self, r: &R, features: &Features) -> Result<Vec<u8>, Error> {
        protocol::encode_section(&res_tag(self.name), |w| (self.write_res)(r, w, features))
    }

    /// A command named `name` (its wire name).
    pub const fn new(
        name: &'static str,
        write_arg: Write<A>,
        read_arg: Read<A>,
        write_res: Write<R>,
        read_res: Read<R>,
        run: ServerFn<A, R>,
    ) -> Self {
        ServerCmd {
            name,
            write_arg,
            read_arg,
            write_res,
            read_res,
            run,
        }
    }

    /// The command's name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// Writes a stream command's argument as raw bytes.
pub type RawWrite<A> = fn(&A, &mut Vec<u8>);
/// Reads a stream command's argument from raw bytes.
pub type RawRead<A> = fn(&[u8]) -> Result<A, Error>;
/// Runs a stream command on a chunk, given the connection it came on.
pub type StreamFn<A> = fn(Connection, A) -> LocalBoxFuture<Result<(), Error>>;

/// A command whose arguments are streamed: sent without waiting for a
/// reply, then checked for errors with a ping (`Remote.registerStreamCmd`).
/// Its argument is raw bytes, not a marshalled value. Registering it also
/// registers the ping (`<name>Ping`), and the command itself as a request
/// with no result, for when streaming is off (the `stream` preference).
pub struct StreamCmd<A> {
    name: &'static str,
    ping: &'static str,
    write: RawWrite<A>,
    read: RawRead<A>,
    run: StreamFn<A>,
}

impl<A> StreamCmd<A> {
    /// Decodes a chunk's section body (as received).
    pub fn decode_chunk(&self, body: &[u8]) -> Result<A, Error> {
        (self.read)(protocol::strip_tag(&str_tag(self.name), body)?)
    }

    /// Encodes a chunk's section.
    pub fn encode_chunk(&self, v: &A) -> Result<Vec<u8>, Error> {
        let mut payload = Vec::new();
        (self.write)(v, &mut payload);
        protocol::encode_raw_section(&str_tag(self.name), &payload)
    }

    /// A command named `name`, whose ping is `ping` (`<name>Ping`).
    pub const fn new(
        name: &'static str,
        ping: &'static str,
        write: RawWrite<A>,
        read: RawRead<A>,
        run: StreamFn<A>,
    ) -> Self {
        StreamCmd {
            name,
            ping,
            write,
            read,
            run,
        }
    }

    /// The command's name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

fn str_tag(name: &str) -> String {
    format!("{name}-str")
}

/// `commandAvailable`: whether the host of a root has a command. (A root
/// command on the wire; it runs with the connection, to see its
/// commands.)
pub static COMMAND_AVAILABLE: ServerCmd<(Fspath, String), bool> = ServerCmd::new(
    "commandAvailable",
    |(f, s), w, _| {
        f.marshal(w);
        w.string(s.as_bytes());
    },
    |r, _| {
        Ok((
            Fspath::unmarshal(r)?,
            String::from_utf8_lossy(&r.string()?).into_owned(),
        ))
    },
    |b, w, _| w.bool(*b),
    |r, _| r.bool(),
    |conn, (_, name)| Box::pin(async move { Ok(conn.endpoint().registry().contains(&name)) }),
);

/// A command's server side, erased: the connection the request came on and
/// the argument section's body in, the result section out.
pub type ErasedFn = Box<dyn Fn(Connection, Vec<u8>) -> LocalBoxFuture<Result<Vec<u8>, Error>>>;

/// A stream command's server side, erased: the connection and the chunk's
/// section body in.
pub type ErasedStreamFn = Box<dyn Fn(Connection, Vec<u8>) -> LocalBoxFuture<Result<(), Error>>>;

/// The registered commands (`serverCmds`), stream commands
/// (`serverStreams`) and section tags (`registeredSet`).
pub struct Registry {
    commands: HashMap<&'static str, ErasedFn>,
    streams: HashMap<&'static str, ErasedStreamFn>,
    tags: HashSet<String>,
}

fn args_tag(name: &str) -> String {
    format!("{name}-args")
}

fn res_tag(name: &str) -> String {
    format!("{name}-res")
}

impl Default for Registry {
    fn default() -> Self {
        Registry::new()
    }
}

impl Registry {
    /// A registry with the header tag (`"rsp"`) taken, and `remote.ml`'s
    /// own commands: `commandAvailable`, `negociateFlowControl`,
    /// `showWarningOnClient` and `forwardMsgToClient`.
    pub fn new() -> Registry {
        let mut r = Registry {
            commands: HashMap::new(),
            streams: HashMap::new(),
            tags: HashSet::from([HEADER_TAG.to_owned()]),
        };
        for cmd in [
            &COMMAND_AVAILABLE as &dyn Registrable,
            &NEGOCIATE_FLOW_CONTROL,
            &SHOW_WARNING_ON_CLIENT,
            &FORWARD_MSG_TO_CLIENT,
        ] {
            cmd.register(&mut r)
                .unwrap_or_else(|e| unreachable!("a fresh registry: {e}"));
        }
        r
    }

    fn register_tag(&mut self, tag: String) -> Result<(), Error> {
        if !self.tags.insert(tag.clone()) {
            return Err(Error::Fatal(format!("tag {tag} is already registered")));
        }
        Ok(())
    }

    fn insert(&mut self, name: &'static str, server: ErasedFn) -> Result<(), Error> {
        if self.commands.contains_key(name) {
            return Err(Error::Fatal(format!("{name} already registered!")));
        }
        self.register_tag(args_tag(name))?;
        self.register_tag(res_tag(name))?;
        self.commands.insert(name, server);
        Ok(())
    }

    /// Registers a host command.
    pub fn register_host<A: 'static, R: 'static>(
        &mut self,
        cmd: &'static HostCmd<A, R>,
    ) -> Result<(), Error> {
        self.insert(
            cmd.name,
            Box::new(move |conn, body| {
                Box::pin(async move {
                    let host = conn.host().clone();
                    let arg = decode_args(&host, cmd.name, &body, |r, f| (cmd.read_arg)(r, f))?;
                    let res = (cmd.run)(host.clone(), arg).await?;
                    encode_res(&host, cmd.name, |w, f| (cmd.write_res)(&res, w, f))
                })
            }),
        )
    }

    /// Registers a root command.
    pub fn register_root<A: 'static, R: 'static>(
        &mut self,
        cmd: &'static RootCmd<A, R>,
    ) -> Result<(), Error> {
        self.insert(
            cmd.name,
            Box::new(move |conn, body| {
                Box::pin(async move {
                    let host = conn.host().clone();
                    let (fspath, arg) = decode_args(&host, cmd.name, &body, |r, f| {
                        Ok((Fspath::unmarshal(r)?, (cmd.read_arg)(r, f)?))
                    })?;
                    let res = (cmd.run)(host.clone(), fspath, arg).await?;
                    encode_res(&host, cmd.name, |w, f| (cmd.write_res)(&res, w, f))
                })
            }),
        )
    }

    /// Registers a server command.
    pub fn register_server<A: 'static, R: 'static>(
        &mut self,
        cmd: &'static ServerCmd<A, R>,
    ) -> Result<(), Error> {
        self.insert(
            cmd.name,
            Box::new(move |conn, body| {
                Box::pin(async move {
                    let host = conn.host().clone();
                    let arg = decode_args(&host, cmd.name, &body, |r, f| (cmd.read_arg)(r, f))?;
                    let res = (cmd.run)(conn, arg).await?;
                    encode_res(&host, cmd.name, |w, f| (cmd.write_res)(&res, w, f))
                })
            }),
        )
    }

    /// Registers a stream command, its ping, and the command as a request
    /// for when streaming is off.
    pub fn register_stream<A: 'static>(&mut self, cmd: &'static StreamCmd<A>) -> Result<(), Error> {
        // The request, with the same raw argument and no result.
        self.insert(
            cmd.name,
            Box::new(move |conn, body| {
                Box::pin(async move {
                    let arg = (cmd.read)(protocol::strip_tag(&args_tag(cmd.name), &body)?)?;
                    (cmd.run)(conn.clone(), arg).await?;
                    encode_res(conn.host(), cmd.name, |_, _| {})
                })
            }),
        )?;
        // The ping: the error the stream met, if any.
        self.insert(
            cmd.ping,
            Box::new(move |conn, body| {
                Box::pin(async move {
                    let id = decode_args(conn.host(), cmd.ping, &body, |r, _| r.int())?;
                    conn.stream_ping(id)?;
                    encode_res(conn.host(), cmd.ping, |_, _| {})
                })
            }),
        )?;
        if self.streams.contains_key(cmd.name) {
            return Err(Error::Fatal(format!("{} already registered!", cmd.name)));
        }
        self.register_tag(str_tag(cmd.name))?;
        self.streams.insert(
            cmd.name,
            Box::new(move |conn, body| {
                Box::pin(async move {
                    let arg = (cmd.read)(protocol::strip_tag(&str_tag(cmd.name), &body)?)?;
                    (cmd.run)(conn, arg).await
                })
            }),
        );
        Ok(())
    }

    /// The server side of the stream command `name`.
    pub fn stream_fn(&self, name: &str) -> Option<&ErasedStreamFn> {
        self.streams.get(name)
    }

    /// Whether a command is registered (`Remote.commandAvailable`).
    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// The server side of the command `name`.
    pub fn server_fn(&self, name: &str) -> Option<&ErasedFn> {
        self.commands.get(name)
    }
}

/// A command a registry can register (for `remote.ml`'s own).
trait Registrable {
    fn register(&'static self, r: &mut Registry) -> Result<(), Error>;
}

impl<A: 'static, R: 'static> Registrable for ServerCmd<A, R> {
    fn register(&'static self, r: &mut Registry) -> Result<(), Error> {
        r.register_server(self)
    }
}

/// Decodes an argument section's body with `host`'s features.
fn decode_args<T>(
    host: &Host,
    name: &str,
    body: &[u8],
    read: impl FnOnce(&mut Reader<'_>, &Features) -> umarshal::Result<T>,
) -> Result<T, Error> {
    let f = host.features().borrow();
    protocol::decode_section_body(&args_tag(name), body, |r| read(r, &f))
}

/// Encodes a result section with `host`'s features.
fn encode_res(
    host: &Host,
    name: &str,
    write: impl FnOnce(&mut Writer, &Features),
) -> Result<Vec<u8>, Error> {
    let f = host.features().borrow();
    protocol::encode_section(&res_tag(name), |w| write(w, &f))
}

/// Encodes an argument section with `host`'s features.
fn encode_args(
    host: &Host,
    name: &str,
    write: impl FnOnce(&mut Writer, &Features),
) -> Result<Vec<u8>, Error> {
    let f = host.features().borrow();
    protocol::encode_section(&args_tag(name), |w| write(w, &f))
}

impl Connection {
    /// Sends a request with its argument section, and decodes the result
    /// with this side's features.
    async fn call_encoded<R>(
        &self,
        name: &str,
        args: Vec<u8>,
        read_res: Read<R>,
    ) -> Result<R, Error> {
        let body = self.request(name, args).await?;
        let f = self.host().features().borrow();
        protocol::decode_section_body(&res_tag(name), &body, |r| read_res(r, &f))
    }

    /// [`Connection::call`], with the request sent at once and the reply
    /// waited for by the future.
    pub fn start_call<A, R: 'static>(
        &self,
        cmd: &'static ServerCmd<A, R>,
        arg: A,
    ) -> Result<impl Future<Output = Result<R, Error>> + 'static, Error> {
        let args = encode_args(self.host(), cmd.name, |w, f| (cmd.write_arg)(&arg, w, f))?;
        let reply = self.start_request(cmd.name, args)?;
        let host = self.host().clone();
        Ok(async move {
            let body = reply.await?;
            let f = host.features().borrow();
            protocol::decode_section_body(&res_tag(cmd.name), &body, |r| (cmd.read_res)(r, &f))
        })
    }

    /// Runs a host command on the other side's host, over this connection
    /// (as OCaml calls `canonizeOnServer` on the connection it just made).
    pub async fn call_host<A, R>(&self, cmd: &'static HostCmd<A, R>, arg: A) -> Result<R, Error> {
        let args = encode_args(self.host(), cmd.name, |w, f| (cmd.write_arg)(&arg, w, f))?;
        self.call_encoded(cmd.name, args, cmd.read_res).await
    }

    /// Runs a server command on the other side (`registerServerCmd`'s
    /// client side).
    pub async fn call<A, R>(&self, cmd: &'static ServerCmd<A, R>, arg: A) -> Result<R, Error> {
        let args = encode_args(self.host(), cmd.name, |w, f| (cmd.write_arg)(&arg, w, f))?;
        self.call_encoded(cmd.name, args, cmd.read_res).await
    }
}

/// Sends a stream's chunks (`registerStreamCmd`'s client side), from
/// [`Connection::stream`].
pub struct StreamSender<A: 'static> {
    conn: Connection,
    cmd: &'static StreamCmd<A>,
    /// The stream's id; none when streaming is off, and each chunk is a
    /// request.
    id: Option<u32>,
}

impl<A> StreamSender<A> {
    /// Sends a chunk, once the earlier ones are written; with streaming
    /// off, as a request, and waits for it to be done. Fails once the
    /// other side has aborted the stream.
    pub async fn send(&self, v: &A) -> Result<(), Error> {
        let mut payload = Vec::new();
        (self.cmd.write)(v, &mut payload);
        let name = self.cmd.name;
        match self.id {
            None => {
                let args = protocol::encode_raw_section(&args_tag(name), &payload)?;
                self.conn.call_encoded(name, args, |_, _| Ok(())).await
            }
            Some(id) => {
                if self.conn.stream_aborted_src() == id {
                    return Err(Error::Transient("Streaming aborted".into()));
                }
                let message = [
                    &protocol::encode_int(id)[..],
                    &protocol::Header::Stream(name.to_owned()).encode()?,
                    &protocol::encode_raw_section(&str_tag(name), &payload)?,
                ]
                .concat();
                self.conn
                    .send_and_wait(connection::Kind::Idle, message)
                    .await
            }
        }
    }
}

impl Connection {
    /// Runs `f` with a sender for `cmd`'s chunks (`registerStreamCmd`'s
    /// client side). Streaming (the `stream` preference, as the caller
    /// reads it), one stream at a time on this side; once `f` is done, a
    /// ping fails with the error the other side met, if any. Without
    /// streaming, each chunk is a request.
    pub async fn stream<A, T, F, Fut>(
        &self,
        cmd: &'static StreamCmd<A>,
        streaming: bool,
        f: F,
    ) -> Result<T, Error>
    where
        F: FnOnce(StreamSender<A>) -> Fut,
        Fut: Future<Output = Result<T, Error>>,
    {
        if !streaming {
            return f(StreamSender {
                conn: self.clone(),
                cmd,
                id: None,
            })
            .await;
        }
        let id = self.endpoint().new_msg_id();
        let result = {
            let _one = self.endpoint().stream_turn().await;
            f(StreamSender {
                conn: self.clone(),
                cmd,
                id: Some(id),
            })
            .await
        };
        let ping = |id: u32| async move {
            let args = encode_args(self.host(), cmd.ping, |w, _| w.int(i64::from(id)))?;
            self.call_encoded(cmd.ping, args, |_, _| Ok(())).await
        };
        match result {
            Ok(v) => {
                ping(id).await?;
                Ok(v)
            }
            Err(e) => {
                if self.stream_aborted_src() == id {
                    ping(id).await?;
                }
                Err(e)
            }
        }
    }
}

/// `ClientConn.clrootEq`: whether two root specifications reach the same
/// server, whatever their paths.
fn same_server(a: &Clroot, b: &Clroot) -> bool {
    match (a, b) {
        (
            Clroot::ConnectByShell {
                shell,
                host,
                user,
                port,
                ..
            },
            Clroot::ConnectByShell {
                shell: shell2,
                host: host2,
                user: user2,
                port: port2,
                ..
            },
        ) => shell == shell2 && host == host2 && user == user2 && port == port2,
        (
            Clroot::ConnectBySocket { host, port, .. },
            Clroot::ConnectBySocket {
                host: host2,
                port: port2,
                ..
            },
        ) => host == host2 && port == port2,
        _ => false,
    }
}

struct ClientConn {
    clroot: Clroot,
    root: Root,
    conn: Connection,
}

/// The client's connections (`ClientConn`): one per server, found by the
/// root specification that reached it or by its canonical root.
#[derive(Default)]
pub struct ClientConns(Vec<ClientConn>);

impl ClientConns {
    /// `register`: replaces the connection to the same server.
    pub fn register(&mut self, clroot: Clroot, root: Root, conn: Connection) {
        self.0.retain(|c| !same_server(&c.clroot, &clroot));
        self.0.insert(0, ClientConn { clroot, root, conn });
    }

    /// `unregister`.
    pub fn unregister(&mut self, conn: &Connection) {
        self.0.retain(|c| c.conn != *conn);
    }

    /// `ofRootOpt`.
    pub fn of_root(&self, root: &Root) -> Option<Connection> {
        self.0
            .iter()
            .find(|c| c.root == *root)
            .map(|c| c.conn.clone())
    }

    /// The connection to a host, for host commands.
    pub fn of_host(&self, host: &str) -> Option<Connection> {
        self.0
            .iter()
            .find(|c| matches!(&c.root.host, RootHost::Remote(h) if h == host))
            .map(|c| c.conn.clone())
    }

    /// `canonRootOfClroot`: the canonical root of the server a root
    /// specification reaches, if connected.
    pub fn canon_root_of_clroot(&self, clroot: &Clroot) -> Option<Root> {
        self.0
            .iter()
            .find(|c| same_server(&c.clroot, clroot))
            .map(|c| c.root.clone())
    }

    /// `ofClrootConncheck`'s lookup.
    fn of_clroot(&self, clroot: &Clroot) -> Option<Connection> {
        self.0
            .iter()
            .find(|c| same_server(&c.clroot, clroot))
            .map(|c| c.conn.clone())
    }

    fn all(&self) -> Vec<Connection> {
        self.0.iter().map(|c| c.conn.clone()).collect()
    }
}

/// How an [`Rpc`] runs commands on local roots.
pub enum Dispatch {
    /// Call the command directly on the client's own host.
    Direct(Rc<Host>),
    /// Send every call over an in-memory connection to a separate host
    /// (see the module documentation).
    Loopback {
        /// The client, whose features encode arguments and decode results.
        client: Rc<Host>,
        /// The host the commands run on.
        server: Rc<Host>,
    },
}

/// The loopback host and its connection, made on first use.
struct Loopback {
    server: Rc<Host>,
    conn: OnceCell<Connection>,
}

/// Runs commands on roots.
pub struct Rpc {
    registry: Rc<Registry>,
    client: Rc<Host>,
    loopback: Option<Loopback>,
    endpoint: OnceCell<Rc<Endpoint>>,
    conns: Rc<RefCell<ClientConns>>,
}

impl Drop for Rpc {
    fn drop(&mut self) {
        if let Some(c) = self.loopback.as_ref().and_then(|l| l.conn.get()) {
            c.close();
        }
        let conns = self.conns.borrow().all();
        for c in conns {
            c.close();
        }
    }
}

impl Rpc {
    /// An RPC with `remote.ml`'s commands registered. In loopback mode the
    /// server host's warnings are shown on the client, as a server sends
    /// them (`showWarningOnClient`).
    pub fn new(dispatch: Dispatch) -> Rpc {
        let (client, loopback) = match dispatch {
            Dispatch::Direct(client) => (client, None),
            Dispatch::Loopback { client, server } => {
                let c = Rc::downgrade(&client);
                server.set_warn_printer(Some(Rc::new(move |s| {
                    if let Some(c) = c.upgrade() {
                        c.warn(s);
                    }
                })));
                let conn = OnceCell::new();
                (client, Some(Loopback { server, conn }))
            }
        };
        Rpc {
            registry: Rc::new(Registry::new()),
            client,
            loopback,
            endpoint: OnceCell::new(),
            conns: Rc::default(),
        }
    }

    /// The registry, to register commands with.
    ///
    /// # Panics
    ///
    /// Once a connection has been made: commands are registered first.
    pub fn registry_mut(&mut self) -> &mut Registry {
        Rc::get_mut(&mut self.registry).expect("commands are registered before any connection")
    }

    /// The registered commands.
    pub fn registry(&self) -> &Rc<Registry> {
        &self.registry
    }

    /// The host that local roots' commands run on.
    pub fn local_host(&self) -> &Rc<Host> {
        self.loopback.as_ref().map_or(&self.client, |l| &l.server)
    }

    /// The client's host: the same as [`Rpc::local_host`] in direct mode.
    pub fn client_host(&self) -> &Rc<Host> {
        &self.client
    }

    /// Whether local roots' commands go over a connection
    /// ([`Dispatch::Loopback`]). Code that skips a root command for local
    /// roots, as OCaml does, calls it anyway in this mode, so that it is
    /// exercised.
    pub fn is_loopback(&self) -> bool {
        self.loopback.is_some()
    }

    /// The client's side of its connections.
    pub fn endpoint(&self) -> &Rc<Endpoint> {
        self.endpoint
            .get_or_init(|| Endpoint::new(self.registry.clone(), self.client.clone()))
    }

    /// Whether the client asks for half-duplex connections (`halfduplex`).
    fn half_duplex(&self) -> bool {
        let prefs = self.client.prefs().borrow();
        prefs.kind(HALF_DUPLEX.name()).is_some() && *prefs.read(&HALF_DUPLEX)
    }

    /// The connection a host's commands go over: none for a local root in
    /// direct mode, the loopback connection in loopback mode (made on first
    /// use), and a remote host's from [`ClientConns`].
    pub fn connection(&self, host: &RootHost) -> Result<Option<Connection>, Error> {
        match host {
            RootHost::Local => Ok(self.loopback.as_ref().map(|l| {
                l.conn
                    .get_or_init(|| {
                        let server = Endpoint::new(self.registry.clone(), l.server.clone());
                        connection::pipe(self.endpoint().clone(), server, self.half_duplex())
                    })
                    .clone()
            })),
            RootHost::Remote(h) => self
                .conns
                .borrow()
                .of_host(h)
                .map(Some)
                .ok_or_else(no_connection),
        }
    }

    /// `Remote.connectionOfRoot`: the connection to a remote root's host.
    pub fn connection_of_root(&self, root: &Root) -> Result<Connection, Error> {
        self.conns.borrow().of_root(root).ok_or_else(no_connection)
    }

    /// `ClientConn.register` and `registerConnCleanup`: `conn` reaches the
    /// server `clroot` names, whose canonical root is `root`. Once it
    /// fails or closes, it is forgotten, and the client's close handlers
    /// run (`clientConnClose`).
    pub fn register_connection(&self, clroot: Clroot, root: Root, conn: Connection) {
        let conns: Weak<RefCell<ClientConns>> = Rc::downgrade(&self.conns);
        let client = Rc::downgrade(&self.client);
        let c = conn.clone();
        conn.at_close(move || {
            if let Some(conns) = conns.upgrade() {
                conns.borrow_mut().unregister(&c);
            }
            if let Some(client) = client.upgrade() {
                client.run_conn_close_handlers(false);
            }
        });
        self.conns.borrow_mut().register(clroot, root, conn);
    }

    /// `ClientConn.register` alone, for a connection already registered
    /// (with [`Rpc::register_connection`]): `conn` reaches the server
    /// `clroot` names, whose canonical root is `root`.
    pub fn name_connection(&self, clroot: Clroot, root: Root, conn: Connection) {
        self.conns.borrow_mut().register(clroot, root, conn);
    }

    /// The client's connections.
    pub fn connections(&self) -> std::cell::Ref<'_, ClientConns> {
        self.conns.borrow()
    }

    /// `ClientConn.ofClrootConncheck`: the connection to the server
    /// `clroot` names, if it is still up. (The receiving task sees a lost
    /// connection; yielding a bit lets it.)
    pub async fn connection_checked(&self, clroot: &Clroot) -> Option<Connection> {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        let conn = self.conns.borrow().of_clroot(clroot)?;
        if conn.failure().is_some() {
            self.conns.borrow_mut().unregister(&conn);
            return None;
        }
        Some(conn)
    }

    /// `clientCloseRootConnection`: closes the connection to a remote
    /// root's host; for a local root, runs the client's close handlers.
    pub fn close_root_connection(&self, root: &Root) {
        match &root.host {
            RootHost::Local => self.client.run_conn_close_handlers(false),
            RootHost::Remote(_) => {
                let conn = self.conns.borrow().of_root(root);
                if let Some(c) = conn {
                    c.close();
                }
            }
        }
    }

    /// An in-memory connection to `server`, standing for a server on
    /// another host that `clroot` reaches with canonical root `root`
    /// (whose host is then remote), registered as such.
    pub fn connect_in_memory(&self, clroot: Clroot, root: Root, server: Rc<Host>) -> Connection {
        let server = Endpoint::new(self.registry.clone(), server);
        let conn = connection::pipe(self.endpoint().clone(), server, self.half_duplex());
        self.register_connection(clroot, root, conn.clone());
        conn
    }

    /// `Remote.commandAvailable`: whether `root`'s host has the command.
    /// (For a local root, OCaml looks in its own table, as here in direct
    /// mode.)
    pub async fn command_available(&self, root: &Root, name: &str) -> Result<bool, Error> {
        match self.connection(&root.host)? {
            None => Ok(self.registry.contains(name)),
            Some(conn) => {
                let arg = (root.fspath.clone(), name.to_owned());
                self.over(&root.host, conn.call(&COMMAND_AVAILABLE, arg))
                    .await
            }
        }
    }

    fn check_registered(&self, name: &str) -> Result<(), Error> {
        if !self.registry.contains(name) {
            return Err(Error::Fatal(format!("{name} not registered!")));
        }
        Ok(())
    }

    /// Awaits a call that went over the connection to `host`: in loopback
    /// mode, a fatal error loses the `Server: ` that marks a server's, as
    /// for a local root.
    async fn over<R>(
        &self,
        host: &RootHost,
        call: impl Future<Output = Result<R, Error>>,
    ) -> Result<R, Error> {
        match call.await {
            Err(Error::Fatal(m)) if *host == RootHost::Local => Err(Error::Fatal(
                m.strip_prefix("Server: ").map_or(m.clone(), str::to_owned),
            )),
            r => r,
        }
    }

    /// Runs a host command on `host` (`registerHostCmd`'s client side).
    pub async fn call_host<A, R>(
        &self,
        cmd: &'static HostCmd<A, R>,
        host: &RootHost,
        arg: A,
    ) -> Result<R, Error> {
        self.check_registered(cmd.name)?;
        match self.connection(host)? {
            None => (cmd.run)(self.client.clone(), arg).await,
            Some(conn) => {
                let args = encode_args(&self.client, cmd.name, |w, f| (cmd.write_arg)(&arg, w, f))?;
                self.over(host, conn.call_encoded(cmd.name, args, cmd.read_res))
                    .await
            }
        }
    }

    /// Runs a root command on `root`'s host (`registerRootCmd`'s client
    /// side).
    pub async fn call_root<A, R>(
        &self,
        cmd: &'static RootCmd<A, R>,
        root: &Root,
        arg: A,
    ) -> Result<R, Error> {
        self.check_registered(cmd.name)?;
        match self.connection(&root.host)? {
            None => (cmd.run)(self.client.clone(), root.fspath.clone(), arg).await,
            Some(conn) => {
                let args = encode_args(&self.client, cmd.name, |w, f| {
                    root.fspath.marshal(w);
                    (cmd.write_arg)(&arg, w, f);
                })?;
                self.over(&root.host, conn.call_encoded(cmd.name, args, cmd.read_res))
                    .await
            }
        }
    }

    /// `registerRootCmdWithConnection`'s client side: runs `cmd` on
    /// `local_root`'s host with the connection to `remote_root`'s. If
    /// `local_root` is local, it runs here, with the connection to the
    /// other root's host; otherwise it is sent over the connection to
    /// `local_root`'s host, which runs it with that connection.
    pub async fn call_root_with_connection<A, R>(
        &self,
        cmd: &'static ServerCmd<A, R>,
        local_root: &Root,
        remote_root: &Root,
        arg: A,
    ) -> Result<R, Error> {
        self.check_registered(cmd.name)?;
        if local_root.is_local() {
            (cmd.run)(self.connection_of_root(remote_root)?, arg).await
        } else {
            self.connection_of_root(local_root)?.call(cmd, arg).await
        }
    }
}

fn no_connection() -> Error {
    Error::Fatal("No connection with the server".into())
}
