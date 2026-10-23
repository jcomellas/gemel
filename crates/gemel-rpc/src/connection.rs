//! Connections: packets, message ids, the output queues and their flow
//! control, the thread that receives messages, and a connection's lifetime.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! A message is a message id ([`protocol::encode_int`]), then a header
//! packet ([`Header`]), then, for a request, a result or a stream chunk, a
//! packet with the argument or the result (a section, see [`protocol`]).
//! Id 0 is the write token of the flow control below. The protocol is
//! symmetric: either side sends requests, and serves the other's on the
//! same connection, with one id space and one table of waiting replies.
//!
//! Each side of a connection is an [`Endpoint`]: the commands it serves,
//! the host they run on, and its message ids. In OCaml those are a
//! process's globals; here the client and a loopback server live in one
//! process, so they are values.
//!
//! A [`Connection`] runs on the engine thread: [`Connection::receive`]
//! reads messages, wakes the requests waiting for their replies, and
//! starts a task for each request from the other side. Writes go out as
//! OCaml's do (`performOutput`), in its order and with its write token, so
//! that a peer with flow control on (`halfduplex`) gets it when it expects
//! it:
//! - a message goes into the output buffer at once, whatever its kind, if
//!   no other write is under way and this side holds the token (urgent
//!   ones need no token); the buffer is written out when it is full, at
//!   once if the output takes it, else by a task, and other writes wait;
//! - those that wait go out in turn: urgent ones (aborting a stream), then
//!   normal ones, then idle ones (stream chunks), the last two only with
//!   the token;
//! - once none waits, the output is flushed, after a yield in case more is
//!   coming; with flow control on, the token is handed to the other side
//!   first, even with nothing written.
//!
//! The server starts with the token. Then the client asks the server to
//! turn flow control off (`negociateFlowControl`), unless `halfduplex` is
//! set, and turns it off itself once the server has.
//!
//! A connection starts once a transport is open and the version handshake
//! ([`crate::handshake`]) is done; the in-memory one ([`pipe`]) needs
//! neither. The transports are ssh ([`crate::shell`]) and sockets
//! ([`crate::socket`]).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use gemel_base::prefs::{Category, PrefDef, Prefs, Topic};
use gemel_base::util::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, oneshot};

use crate::host::Host;
use crate::protocol::{self, HEADER_TAG, Header, INT_SIZE};
use crate::registry::{Registry, ServerCmd};
use crate::runtime;

/// The message a lost connection fails with (`lostConnection`). A
/// server's command loop ends quietly on it.
pub const LOST_CONNECTION: &str = "Lost connection with the server";

/// The size of the output buffer (`bufferSize`).
pub(crate) const BUFFER_SIZE: usize = 16384;

/// Message ids wrap around before this (`hugeint`).
const HUGE_INT: u32 = 1_000_000_000;

/// The size of each direction of an in-memory connection's pipe.
const PIPE_SIZE: usize = 64 * 1024;

/// What a connection reads from.
pub type Input = Box<dyn AsyncRead + Unpin>;
/// What a connection writes to.
pub type Output = Box<dyn AsyncWrite + Unpin>;

/// The process's standard input and output, for a server that ssh
/// started.
pub fn stdio() -> (Input, Output) {
    (Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout()))
}

/// A lost connection's error.
pub(crate) fn lost() -> Error {
    Error::Fatal(LOST_CONNECTION.to_owned())
}

/// One side of connections: the commands it serves and the host they run
/// on (OCaml's `serverCmds` and the process's state), and the ids of the
/// requests it sends (`ids`).
pub struct Endpoint {
    registry: Rc<Registry>,
    host: Rc<Host>,
    ids: Cell<u32>,
    streams: tokio::sync::Semaphore,
}

impl Endpoint {
    /// An endpoint serving `registry`'s commands on `host`.
    pub fn new(registry: Rc<Registry>, host: Rc<Host>) -> Rc<Endpoint> {
        Rc::new(Endpoint {
            registry,
            host,
            ids: Cell::new(1),
            streams: tokio::sync::Semaphore::new(1),
        })
    }

    /// The commands this side serves.
    pub fn registry(&self) -> &Rc<Registry> {
        &self.registry
    }

    /// The host this side's commands run on.
    pub fn host(&self) -> &Rc<Host> {
        &self.host
    }

    /// `newMsgId`: 2 and up, wrapping before [`HUGE_INT`]. (1 was the id
    /// of the 2.51 compatibility mode's magic packet, 0 is the write
    /// token.)
    pub fn new_msg_id(&self) -> u32 {
        let mut id = self.ids.get() + 1;
        if id == HUGE_INT {
            id = 2;
        }
        self.ids.set(id);
        id
    }

    /// A turn to stream (`streamReg`): one stream at a time on this side.
    pub async fn stream_turn(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.streams
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("the semaphore is never closed"))
    }
}

/// How urgently a message goes out (`kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Even without the write token, and flushed at once (`dumpUrgent`).
    Urgent,
    /// With the token (`dump`).
    Normal,
    /// With the token, after the normal ones (`dumpIdle`).
    Idle,
}

/// The output queues (`outputQueue`).
struct Queues {
    /// No write is under way (`available`).
    available: bool,
    can_write: bool,
    flow_control: bool,
    closed: bool,
    /// A flush is due (`maybeFlush`'s `pendingFlush`).
    pending_flush: bool,
    urgent: VecDeque<Queued>,
    normal: VecDeque<Queued>,
    idle: VecDeque<Queued>,
}

/// The output buffer (`outputBuffer`). The output is taken out while it is
/// written to.
struct OutputBuffer {
    output: Option<Output>,
    buf: Vec<u8>,
    /// The output was closed.
    closed: bool,
}

/// A write (`performOutput`'s action): a message, or a flush (`Last`).
enum Write {
    Message(Queued, Kind),
    Flush,
}

/// A message, and who waits for it to be written (`performOutput`'s
/// result).
type Queued = (Vec<u8>, Option<oneshot::Sender<()>>);

impl Queues {
    /// `popOutputQueues`'s choice: the next message that may go out.
    fn pop(&mut self) -> Option<(Queued, Kind)> {
        if let Some(m) = self.urgent.pop_front() {
            return Some((m, Kind::Urgent));
        }
        if !self.can_write {
            return None;
        }
        self.normal
            .pop_front()
            .map(|m| (m, Kind::Normal))
            .or_else(|| self.idle.pop_front().map(|m| (m, Kind::Idle)))
    }
}

type Reply = oneshot::Sender<Result<Vec<u8>, Error>>;

struct Inner {
    endpoint: Rc<Endpoint>,
    is_server: bool,
    version: Cell<u32>,
    input: RefCell<Option<Input>>,
    queues: RefCell<Queues>,
    out: RefCell<OutputBuffer>,
    /// Notified when a write gives the output back.
    idle: Notify,
    /// Notified when the connection is closed.
    closing: Notify,
    receivers: RefCell<HashMap<u32, Reply>>,
    failure: RefCell<Option<Error>>,
    close_handlers: RefCell<Vec<Box<dyn FnOnce()>>>,
    stream_aborted_src: Cell<u32>,
    stream_aborted_dst: Cell<bool>,
    stream_errors: RefCell<HashMap<u32, Error>>,
    emitted: Cell<u64>,
    received: Cell<u64>,
}

/// A connection with the other side (`Remote.connection`). Clones are the
/// same connection.
#[derive(Clone)]
pub struct Connection(Rc<Inner>);

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("is_server", &self.0.is_server)
            .field("failure", &self.0.failure.borrow())
            .finish_non_exhaustive()
    }
}

impl PartialEq for Connection {
    /// `connEq`.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Connection {}

impl Connection {
    /// `makeConnection`: a connection over `input` and `output`, served by
    /// `endpoint`, with flow control off until [`enable_flow_control`].
    /// Starts the task that closes the output; nothing is received until
    /// [`receive`].
    ///
    /// [`enable_flow_control`]: Connection::enable_flow_control
    /// [`receive`]: Connection::receive
    ///
    /// # Panics
    ///
    /// Outside the engine thread's runtime.
    pub fn new(endpoint: Rc<Endpoint>, is_server: bool, input: Input, output: Output) -> Self {
        let conn = Connection(Rc::new(Inner {
            endpoint,
            is_server,
            version: Cell::new(protocol::RPC_SUPPORTED_VERSIONS[0]),
            input: RefCell::new(Some(input)),
            queues: RefCell::new(Queues {
                available: true,
                can_write: true,
                flow_control: false,
                closed: false,
                pending_flush: false,
                urgent: VecDeque::new(),
                normal: VecDeque::new(),
                idle: VecDeque::new(),
            }),
            out: RefCell::new(OutputBuffer {
                output: Some(output),
                buf: Vec::with_capacity(BUFFER_SIZE),
                closed: false,
            }),
            idle: Notify::new(),
            closing: Notify::new(),
            receivers: RefCell::default(),
            failure: RefCell::default(),
            close_handlers: RefCell::default(),
            stream_aborted_src: Cell::new(0),
            stream_aborted_dst: Cell::new(false),
            stream_errors: RefCell::default(),
            emitted: Cell::new(0),
            received: Cell::new(0),
        }));
        runtime::spawn_local(conn.clone().close_output());
        conn
    }

    /// The side of the connection this end is.
    pub fn endpoint(&self) -> &Rc<Endpoint> {
        &self.0.endpoint
    }

    /// The host this end's commands run on.
    pub fn host(&self) -> &Rc<Host> {
        &self.0.endpoint.host
    }

    /// Whether this end is the server.
    pub fn is_server(&self) -> bool {
        self.0.is_server
    }

    /// The bytes written to the connection so far (`emittedBytes`, for
    /// this connection).
    pub fn bytes_emitted(&self) -> u64 {
        self.0.emitted.get()
    }

    /// The bytes read from the connection so far (`receivedBytes`).
    pub fn bytes_received(&self) -> u64 {
        self.0.received.get()
    }

    /// `connectionVersion`: the RPC version agreed on.
    pub fn version(&self) -> u32 {
        self.0.version.get()
    }

    /// `setConnectionVersion`.
    pub fn set_version(&self, version: u32) {
        self.0.version.set(version);
    }

    /// The error the connection failed with, if it has (`Lost connection
    /// with the server`, once the other side has gone).
    pub fn failure(&self) -> Option<Error> {
        self.0.failure.borrow().clone()
    }

    /// Sends a message (`dump`, `dumpIdle`, `dumpUrgent`). Messages of a
    /// kind go out in the order they are sent; nothing goes out once the
    /// connection is closed.
    pub fn send(&self, kind: Kind, message: Vec<u8>) {
        self.perform_output(kind, (message, None));
    }

    /// Sends a message and waits until it is written (into the output
    /// buffer), as OCaml's senders wait for `dump`: so that what is sent
    /// after it, of whatever kind, goes after it. Fails if the connection
    /// does first.
    pub async fn send_and_wait(&self, kind: Kind, message: Vec<u8>) -> Result<(), Error> {
        if let Some(e) = self.failure() {
            return Err(e);
        }
        let (tx, rx) = oneshot::channel();
        self.perform_output(kind, (message, Some(tx)));
        rx.await.map_err(|_| self.failure().unwrap_or_else(lost))
    }

    /// `performOutput`: writes the message now if no other write is under
    /// way and it may go out, else queues it.
    fn perform_output(&self, kind: Kind, queued: Queued) {
        let mut q = self.0.queues.borrow_mut();
        if q.closed {
            return;
        }
        if q.available && (kind == Kind::Urgent || q.can_write) {
            q.available = false;
            drop(q);
            self.write_now(Some(Write::Message(queued, kind)));
            return;
        }
        match kind {
            Kind::Urgent => q.urgent.push_back(queued),
            Kind::Normal => q.normal.push_back(queued),
            Kind::Idle => q.idle.push_back(queued),
        }
    }

    /// `performOutputRec`: does `first`, then what is queued
    /// (`popOutputQueues`), in the caller until the output has to be
    /// waited for. Once nothing is queued, a flush is scheduled.
    fn write_now(&self, first: Option<Write>) {
        let conn = self.clone();
        runtime::run_now(async move {
            let mut next = first;
            loop {
                let write = next.take().or_else(|| {
                    conn.0
                        .queues
                        .borrow_mut()
                        .pop()
                        .map(|(m, k)| Write::Message(m, k))
                });
                let Some(write) = write else {
                    conn.0.queues.borrow_mut().available = true;
                    conn.maybe_flush();
                    return;
                };
                let result = match write {
                    Write::Message((message, written), kind) => {
                        let r = conn.fill_buffer(&message).await;
                        let r = match (r, kind) {
                            (Ok(()), Kind::Urgent) => conn.flush_buffer().await,
                            (r, _) => r,
                        };
                        if let Some(written) = written {
                            let _ = written.send(());
                        }
                        r
                    }
                    Write::Flush => {
                        let token = {
                            let mut q = conn.0.queues.borrow_mut();
                            let token = q.flow_control;
                            if token {
                                q.can_write = false;
                            }
                            token
                        };
                        let r = if token {
                            conn.fill_buffer(&protocol::encode_int(0)).await
                        } else {
                            Ok(())
                        };
                        match r {
                            Ok(()) => conn.flush_buffer().await,
                            r => r,
                        }
                    }
                };
                if result.is_err() {
                    // In a task: failing runs the close handlers, which
                    // mustn't run inside whatever was sending.
                    let c = conn.clone();
                    runtime::spawn_local(async move { c.fail(lost()) });
                    return;
                }
            }
        });
    }

    /// `maybeFlush`: once the queues are empty, flushes the output after a
    /// yield, unless more is being written by then; with flow control on
    /// and the token, hands the token over first, even with nothing to
    /// flush.
    fn maybe_flush(&self) {
        {
            let mut q = self.0.queues.borrow_mut();
            let empty = self.0.out.borrow().buf.is_empty();
            if q.pending_flush || q.closed || (empty && !(q.flow_control && q.can_write)) {
                return;
            }
            q.pending_flush = true;
        }
        let conn = self.clone();
        runtime::spawn_local(async move {
            // Wait a bit, in case there are new requests being processed.
            tokio::task::yield_now().await;
            let mut q = conn.0.queues.borrow_mut();
            q.pending_flush = false;
            // If there are other writes under way, they flush.
            if !q.available || q.closed || !q.can_write {
                return;
            }
            q.available = false;
            drop(q);
            conn.write_now(Some(Write::Flush));
        });
    }

    /// `fillBuffer`: copies `s` into the output buffer, writing the buffer
    /// out each time it is full.
    async fn fill_buffer(&self, mut s: &[u8]) -> std::io::Result<()> {
        while !s.is_empty() {
            let full = self.0.out.borrow().buf.len() >= BUFFER_SIZE;
            if full {
                self.send_output().await?;
                continue;
            }
            let mut out = self.0.out.borrow_mut();
            let l = s.len().min(BUFFER_SIZE - out.buf.len());
            out.buf.extend_from_slice(&s[..l]);
            s = &s[l..];
        }
        Ok(())
    }

    /// `flushBuffer`: writes the output buffer out, all of it.
    async fn flush_buffer(&self) -> std::io::Result<()> {
        while !self.0.out.borrow().buf.is_empty() {
            self.send_output().await?;
        }
        let mut output = self.take_output().await?;
        let r = output.flush().await;
        self.put_output(output);
        r
    }

    /// `sendOutput`: one write of the output buffer; what it took goes.
    /// (What another write puts in the buffer meanwhile goes after what is
    /// left.)
    async fn send_output(&self) -> std::io::Result<()> {
        let mut output = self.take_output().await?;
        let buf = std::mem::take(&mut self.0.out.borrow_mut().buf);
        let r = output.write(&buf).await;
        let n = match r {
            Ok(0) => Err(std::io::ErrorKind::WriteZero.into()),
            r => r,
        };
        {
            let mut out = self.0.out.borrow_mut();
            let written = *n.as_ref().unwrap_or(&0);
            let added = std::mem::take(&mut out.buf);
            out.buf = [&buf[written..], &added[..]].concat();
        }
        self.put_output(output);
        let n = n?;
        self.0.emitted.set(self.0.emitted.get() + n as u64);
        Ok(())
    }

    fn put_output(&self, output: Output) {
        self.0.out.borrow_mut().output = Some(output);
        self.0.idle.notify_waiters();
    }

    /// The output, once no other write has it; none once it is closed.
    async fn take_output(&self) -> std::io::Result<Output> {
        loop {
            let idle = self.0.idle.notified();
            {
                let mut out = self.0.out.borrow_mut();
                if let Some(output) = out.output.take() {
                    return Ok(output);
                }
                if out.closed {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
            }
            idle.await;
        }
    }

    /// `enableFlowControl`, once the handshake is done: the server holds
    /// the write token first, and writes what is queued.
    pub fn enable_flow_control(&self) {
        let mut q = self.0.queues.borrow_mut();
        q.available = false;
        q.flow_control = true;
        q.can_write = self.0.is_server;
        let can_write = q.can_write;
        drop(q);
        if can_write {
            self.write_now(None);
        }
    }

    /// `disableFlowControl`.
    pub fn disable_flow_control(&self) {
        let can_write = {
            let mut q = self.0.queues.borrow_mut();
            q.flow_control = false;
            q.can_write
        };
        if !can_write {
            self.grant_writes();
        }
    }

    /// Whether flow control is on.
    pub fn flow_control(&self) -> bool {
        self.0.queues.borrow().flow_control
    }

    /// `allowWrites`: the other side has handed over the write token.
    /// OCaml asserts that this side didn't hold it. A token that crossed
    /// this side turning flow control off is ignored.
    fn allow_writes(&self) -> Result<(), Error> {
        let q = self.0.queues.borrow();
        if q.can_write {
            if !q.flow_control {
                return Ok(());
            }
            return Err(Error::Fatal(
                "Protocol error: write token received while holding it".into(),
            ));
        }
        drop(q);
        self.grant_writes();
        Ok(())
    }

    /// `allowWrites`'s effect: what is queued goes out, after a yield that
    /// lets the receiver and the requests it started run first.
    fn grant_writes(&self) {
        let mut q = self.0.queues.borrow_mut();
        q.can_write = true;
        q.available = false;
        drop(q);
        let conn = self.clone();
        runtime::spawn_local(async move {
            tokio::task::yield_now().await;
            conn.write_now(None);
        });
    }

    /// Sends a request for the command `name` with its argument section
    /// (`registerSpecialServerCmd`'s client side), and waits for the
    /// reply: the body of the result section, or the error the command
    /// failed with (a fatal one prefixed with `Server: `).
    pub async fn request(&self, name: &str, args: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.start_request(name, args)?.await
    }

    /// [`Connection::request`]'s first half, done at once: the request
    /// takes its id and its place in the queue now; the future waits for
    /// the reply.
    pub fn start_request(
        &self,
        name: &str,
        args: Vec<u8>,
    ) -> Result<impl std::future::Future<Output = Result<Vec<u8>, Error>> + 'static, Error> {
        if let Some(e) = self.failure() {
            return Err(e);
        }
        let header = Header::Request(name.to_owned()).encode()?;
        let id = self.0.endpoint.new_msg_id();
        let (tx, rx) = oneshot::channel();
        self.0.receivers.borrow_mut().insert(id, tx);
        self.send(
            Kind::Normal,
            [&protocol::encode_int(id)[..], &header, &args].concat(),
        );
        let conn = self.clone();
        Ok(async move {
            rx.await
                .unwrap_or_else(|_| Err(conn.failure().unwrap_or_else(lost)))
        })
    }

    /// `receive`: reads messages until the connection fails, then fails
    /// it (see [`Connection::fail`]) and returns the error. Call it once,
    /// after [`Connection::enable_flow_control`].
    pub async fn receive(&self) -> Error {
        let input = self.0.input.borrow_mut().take();
        let e = match input {
            Some(mut input) => self.receive_messages(&mut input).await,
            None => Error::Fatal("Internal error: the connection is already receiving".into()),
        };
        self.fail(e.clone());
        e
    }

    async fn receive_messages(&self, input: &mut Input) -> Error {
        loop {
            if let Err(e) = self.receive_message(input).await {
                return e;
            }
        }
    }

    async fn receive_message(&self, input: &mut Input) -> Result<(), Error> {
        let mut input = Counted(input, &self.0.received);
        let input = &mut input;
        let mut id = [0; INT_SIZE];
        read(input, &mut id).await?;
        let num_id = protocol::decode_int(&id)?;
        if num_id == 0 {
            return self.allow_writes();
        }
        let header = receive_packet(input).await?;
        match protocol::decode_section_body(HEADER_TAG, &header, Header::unmarshal)? {
            Header::Request(name) => {
                let args = receive_packet(input).await?;
                runtime::spawn_local(self.clone().process_request(id, name, args));
            }
            Header::NormalResult => {
                let res = receive_packet(input).await?;
                self.reply(num_id, Ok(res))?;
            }
            Header::TransientExn(s) => self.reply(num_id, Err(Error::Transient(s)))?,
            Header::FatalExn(s) => self.reply(num_id, Err(Error::Fatal(format!("Server: {s}"))))?,
            Header::Stream(name) => {
                let chunk = receive_packet(input).await?;
                self.process_stream(num_id, &name, chunk).await?;
            }
            Header::StreamAbort => self.0.stream_aborted_src.set(num_id),
        }
        Ok(())
    }

    /// Wakes the request waiting for `id` (`find_receiver`).
    fn reply(&self, id: u32, result: Result<Vec<u8>, Error>) -> Result<(), Error> {
        let receiver = self.0.receivers.borrow_mut().remove(&id);
        let receiver = receiver.ok_or_else(|| {
            Error::Fatal(format!(
                "Protocol error: reply to an unknown request (id: {id})"
            ))
        })?;
        // The request may have been given up on.
        let _ = receiver.send(result);
        Ok(())
    }

    /// `abortStream`: asks the other side to stop streaming, once.
    fn abort_stream(&self, id: u32) -> Result<(), Error> {
        if !self.0.stream_aborted_dst.replace(true) {
            let header = Header::StreamAbort.encode()?;
            self.send(
                Kind::Urgent,
                [&protocol::encode_int(id)[..], &header].concat(),
            );
        }
        Ok(())
    }

    /// `processStream`: runs a stream chunk, in order with the messages
    /// around it. Once a chunk of a stream fails, the stream is aborted,
    /// and its later chunks are dropped; its ping gets the error.
    async fn process_stream(&self, id: u32, name: &str, chunk: Vec<u8>) -> Result<(), Error> {
        if self.0.stream_errors.borrow().contains_key(&id) {
            return self.abort_stream(id);
        }
        let run = self
            .0
            .endpoint
            .registry
            .stream_fn(name)
            .map(|f| f(self.clone(), chunk));
        let result = match run {
            Some(run) => run.await,
            None => Err(Error::Fatal(format!("{name} not registered!"))),
        };
        if let Err(e) = result {
            self.0.stream_errors.borrow_mut().insert(id, e);
            self.abort_stream(id)?;
        }
        Ok(())
    }

    /// A stream's ping (`<name>Ping`'s server side): the error its chunks
    /// met, if any, which is then forgotten, and streams may be aborted
    /// again.
    pub fn stream_ping(&self, id: i64) -> Result<(), Error> {
        let e = u32::try_from(id)
            .ok()
            .and_then(|id| self.0.stream_errors.borrow_mut().remove(&id));
        match e {
            Some(e) => {
                self.0.stream_aborted_dst.set(false);
                Err(e)
            }
            None => Ok(()),
        }
    }

    /// The id of the last stream the other side aborted
    /// (`streamAbortedSrc`).
    pub fn stream_aborted_src(&self) -> u32 {
        self.0.stream_aborted_src.get()
    }

    /// `processRequest`: runs a request from the other side and sends the
    /// reply, with the request's id. A command this side doesn't have
    /// fails the request (OCaml's server stops).
    async fn process_request(self, id: [u8; INT_SIZE], name: String, args: Vec<u8>) {
        // OCaml yields first, so that the receiver keeps going; a spawned
        // task already waits for it. (Yielding here would start requests
        // that arrive together in reverse: tokio wakes tasks that yielded
        // in the same tick last first.)
        let run = self
            .0
            .endpoint
            .registry
            .server_fn(&name)
            .map(|f| f(self.clone(), args));
        let result = match run {
            Some(run) => run.await,
            None => Err(Error::Fatal(format!("{name} not registered!"))),
        };
        let header = Header::of_result(&result)
            .encode()
            .or_else(|e| Header::of_result::<()>(&Err(e)).encode());
        let Ok(header) = header else {
            return;
        };
        let mut message = [&id[..], &header].concat();
        if let Ok(res) = result {
            message.extend(res);
        }
        self.send(Kind::Normal, message);
    }

    /// Fails the connection with `e`, once (`lostConnection` and
    /// `closeConnection`): the requests waiting for replies fail with it,
    /// nothing more is written, and the close handlers run.
    pub fn fail(&self, e: Error) {
        if self.0.failure.borrow().is_some() {
            return;
        }
        *self.0.failure.borrow_mut() = Some(e.clone());
        self.shut();
        let receivers = std::mem::take(&mut *self.0.receivers.borrow_mut());
        for (_, r) in receivers {
            let _ = r.send(Err(e.clone()));
        }
        let handlers = std::mem::take(&mut *self.0.close_handlers.borrow_mut());
        for h in handlers {
            h();
        }
    }

    /// Closes the connection from this side (`closeConnection`): what was
    /// queued is written, then the output is closed, so the other side
    /// sees the connection lost, and so does this one.
    pub fn close(&self) {
        self.shut();
        if self.0.input.borrow_mut().take().is_some() {
            self.fail(lost());
        }
    }

    /// Nothing more is sent; the output is closed (see
    /// [`Connection::close_output`]).
    fn shut(&self) {
        let mut q = self.0.queues.borrow_mut();
        if !q.closed {
            q.closed = true;
            self.0.closing.notify_one();
        }
    }

    /// Once the connection is closed, and a write under way is done, flushes
    /// and closes the output. A task from the start, so that a connection
    /// dropped outside the engine thread's runtime is closed the next time
    /// it runs.
    async fn close_output(self) {
        self.0.closing.notified().await;
        let _ = self.flush_buffer().await;
        if let Ok(mut output) = self.take_output().await {
            let _ = output.shutdown().await;
        }
        self.0.out.borrow_mut().closed = true;
        self.0.idle.notify_waiters();
    }

    /// `at_conn_close'`: runs `f` when the connection fails or closes (at
    /// once if it has).
    pub fn at_close(&self, f: impl FnOnce() + 'static) {
        if self.0.failure.borrow().is_some() {
            f();
        } else {
            self.0.close_handlers.borrow_mut().push(Box::new(f));
        }
    }
}

/// The input, counting what is read from it.
struct Counted<'a>(&'a mut Input, &'a Cell<u64>);

/// Reads exactly `buf.len()` bytes (`grab`); the end of the input, or any
/// error reading it, is a lost connection.
async fn read(input: &mut Counted<'_>, buf: &mut [u8]) -> Result<(), Error> {
    input.0.read_exact(buf).await.map_err(|_| lost())?;
    input.1.set(input.1.get() + buf.len() as u64);
    Ok(())
}

/// `receivePacket`: a packet's length, then its body.
async fn receive_packet(input: &mut Counted<'_>) -> Result<Vec<u8>, Error> {
    let mut len = [0; INT_SIZE];
    read(input, &mut len).await?;
    let mut buf = vec![0; protocol::decode_int(&len)? as usize];
    read(input, &mut buf).await?;
    Ok(buf)
}

/// A client's side of a connection (`initConnection`, after the
/// handshake): flow control on, receiving, and flow control negotiated
/// unless `half_duplex`. The connection is closed when receiving fails.
pub fn start_client(
    endpoint: Rc<Endpoint>,
    input: Input,
    output: Output,
    half_duplex: bool,
) -> Connection {
    let conn = Connection::new(endpoint, false, input, output);
    conn.enable_flow_control();
    let c = conn.clone();
    runtime::spawn_local(async move {
        c.receive().await;
    });
    if !half_duplex {
        // Queued now, as OCaml does, so that it is the first request (id
        // 2); only the reply is waited for apart.
        if let Ok(reply) = conn.start_call(&NEGOCIATE_FLOW_CONTROL, ()) {
            let c = conn.clone();
            runtime::spawn_local(async move {
                if let Ok(false) = reply.await {
                    c.disable_flow_control();
                }
            });
        }
    }
    conn
}

/// `commandLoop`, without the handshake: serves the client until the
/// connection is lost, which ends it quietly; another failure is returned.
/// (With the handshake, it is [`crate::server::command_loop`].)
pub async fn serve(endpoint: Rc<Endpoint>, input: Input, output: Output) -> Result<(), Error> {
    serve_connection(Connection::new(endpoint, true, input, output)).await
}

/// [`serve`] on a server's connection.
pub(crate) async fn serve_connection(conn: Connection) -> Result<(), Error> {
    conn.enable_flow_control();
    // OCaml's server yields as it turns flow control on, which lets the
    // write token go out before anything is received.
    for _ in 0..10 {
        let holding = {
            let q = conn.0.queues.borrow();
            q.can_write && !q.closed
        };
        if !holding {
            break;
        }
        tokio::task::yield_now().await;
    }
    let e = conn.receive().await;
    // Let the writes under way end (OCaml yields a bit too).
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    if e == lost() { Ok(()) } else { Err(e) }
}

/// An in-memory connection: the client's end, with `server` serving on
/// the other end until either is closed. For a local root in loopback
/// mode, or for a server on another host.
pub fn pipe(client: Rc<Endpoint>, server: Rc<Endpoint>, half_duplex: bool) -> Connection {
    let (a, b) = tokio::io::duplex(PIPE_SIZE);
    let (ar, aw) = tokio::io::split(a);
    let (br, bw) = tokio::io::split(b);
    runtime::spawn_local(async move {
        let _ = serve(server, Box::new(br), Box::new(bw)).await;
    });
    start_client(client, Box::new(ar), Box::new(aw), half_duplex)
}

/// `negociateFlowControl`: the server turns flow control off and says it
/// isn't needed; the client then turns it off too.
pub static NEGOCIATE_FLOW_CONTROL: ServerCmd<(), bool> = ServerCmd::new(
    "negociateFlowControl",
    |(), _, _| {},
    |_, _| Ok(()),
    |b, w, _| w.bool(*b),
    |r, _| r.bool(),
    |conn, ()| {
        Box::pin(async move {
            conn.disable_flow_control();
            Ok(false)
        })
    },
);

// ---- Preferences ----

/// `sshcmd`.
pub static SSH_CMD: PrefDef<String> = PrefDef::new(
    "sshcmd",
    Category::Advanced(Topic::Remote),
    || "ssh".to_owned(),
    "path to the ssh executable",
    "This preference can be used to explicitly set the name of the ssh \
     executable (e.g., giving a full path name), if necessary.",
);

/// `sshargs`.
pub static SSH_ARGS: PrefDef<String> = PrefDef::new(
    "sshargs",
    Category::Advanced(Topic::Remote),
    String::new,
    "other arguments (if any) for remote shell command",
    "The string value of this preference will be passed as additional \
     arguments (besides the host name and the name of the Unison executable \
     on the remote system) to the \\verb|ssh| command used to invoke the \
     remote server. The backslash is an escape character.",
);

/// `servercmd`.
pub static SERVER_CMD: PrefDef<String> = PrefDef::new(
    "servercmd",
    Category::Advanced(Topic::Remote),
    String::new,
    "name of gemel executable on remote server",
    "This preference can be used to explicitly set the name of the Unison \
     executable on the remote server (e.g., giving a full path name), if \
     necessary.",
);

/// `addversionno`.
pub static ADD_VERSION_NO: PrefDef<bool> = PrefDef::new(
    "addversionno",
    Category::Advanced(Topic::Remote),
    || false,
    "add version number to name of gemel on server",
    "When this flag is set to {\\tt true}, Unison will use \
     \\texttt{unison-\\ARG{currentmajorversionnumber}} instead of just \
     \\verb|unison| as the remote server command (note that the minor version \
     number is dropped -- e.g., unison-2.51).  This allows multiple binaries \
     for different versions of unison to coexist conveniently on the same \
     server: whichever version is run on the client, the same version will \
     be selected on the server.",
);

/// `stream` (`streamingActivated`).
pub static STREAMING_ACTIVATED: PrefDef<bool> = PrefDef::new(
    "stream",
    Category::Advanced(Topic::Remote),
    || true,
    "use a streaming protocol for transferring file contents",
    "When this preference is set, Unison will use an experimental streaming \
     protocol for transferring file contents more efficiently. The default \
     value is \\texttt{true}.",
)
.deprecated();

/// `halfduplex`.
pub static HALF_DUPLEX: PrefDef<bool> = PrefDef::new(
    "halfduplex",
    Category::Advanced(Topic::Remote),
    || false,
    "force half-duplex communication with the server",
    "When this flag is set to {\\tt true}, Unison network communication is \
     forced to be half duplex (the client and the server never \
     simultaneously emit data).  If you experience unstabilities with your \
     network link, this may help.",
)
.deprecated();

/// `killserver`.
pub static KILL_SERVER: PrefDef<bool> = PrefDef::new(
    "killserver",
    Category::Advanced(Topic::Remote),
    || false,
    "kill server when done (even when using sockets)",
    "When set to \\verb|true|, this flag causes Unison to kill the remote \
     server process when the synchronization is finished.  This behavior is \
     the default for \\verb|ssh| connections, so this preference is not \
     normally needed when running over \\verb|ssh|; it is provided so that \
     socket-mode servers can be killed off after a single run of Unison, \
     rather than waiting to accept future connections.  (Some users prefer \
     to start a remote socket server for each run of Unison, rather than \
     leaving one running all the time.)",
);

/// Registers this module's preferences, in OCaml's order, and the ones it
/// removed (`rshcmd` and `rshargs`, since 2.52).
pub fn register_prefs(prefs: &mut Prefs) -> Result<(), Error> {
    prefs.register(&SSH_CMD)?;
    prefs.register(&SSH_ARGS)?;
    prefs.mark_removed("rshcmd");
    prefs.mark_removed("rshargs");
    prefs.register(&SERVER_CMD)?;
    prefs.register(&ADD_VERSION_NO)?;
    prefs.register(&STREAMING_ACTIVATED)?;
    prefs.register(&HALF_DUPLEX)?;
    prefs.register(&KILL_SERVER)?;
    // For backward compatibility.
    prefs.alias(&KILL_SERVER, "killServer")
}
