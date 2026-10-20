//! Registration and dispatch of remote commands.
//!
//! Ports Unison `src/remote.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Engine modules declare their remote commands as statics: a
//! [`HostCmd`] runs on a host (`Remote.registerHostCmd`), a [`RootCmd`] on
//! the host of a root, with the root's path as an extra first argument
//! (`Remote.registerRootCmd`). Each has codecs for its argument and result
//! and an async function that runs on the host. They are registered with an
//! [`Rpc`], which dispatches calls:
//!
//! - [`Dispatch::Direct`]: a local root's command is called directly, as
//!   `registerHostCmd` does in OCaml;
//! - [`Dispatch::Loopback`]: for tests, every call goes through what a
//!   server would receive and send (the request header, the argument
//!   section, the command looked up by name, the result or error header
//!   and section), against a separate [`Host`] whose preferences come from
//!   the client's dump. The sections are byte-exact; packets, message ids
//!   and connections come with remote roots (milestone 3), as do streams
//!   and `registerRootCmdWithConnection`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use gemel_base::util::Error;
use gemel_compat::umarshal::{self, Reader, Writer};
use gemel_fs::fspath::Fspath;

use crate::host::Host;
use crate::protocol::{self, HEADER_TAG, Header};
use crate::root::{Host as RootHost, Root};

/// A future on the engine thread.
pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Writes a value.
pub type Write<T> = fn(&T, &mut Writer);
/// Reads a value.
pub type Read<T> = fn(&mut Reader<'_>) -> umarshal::Result<T>;
/// Runs a host command.
pub type HostFn<A, R> = fn(Rc<Host>, A) -> LocalBoxFuture<Result<R, Error>>;
/// Runs a root command, given the root's path.
pub type RootFn<A, R> = fn(Rc<Host>, Fspath, A) -> LocalBoxFuture<Result<R, Error>>;

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

/// A command's server side, erased: the argument section's body in, the
/// result section out.
type ServerCmd = Box<dyn Fn(Rc<Host>, Vec<u8>) -> LocalBoxFuture<Result<Vec<u8>, Error>>>;

/// The registered commands (`serverCmds`) and section tags
/// (`registeredSet`).
#[derive(Default)]
pub struct Registry {
    commands: HashMap<&'static str, ServerCmd>,
    tags: HashSet<String>,
}

fn args_tag(name: &str) -> String {
    format!("{name}-args")
}

fn res_tag(name: &str) -> String {
    format!("{name}-res")
}

impl Registry {
    /// A registry with the header tag (`"rsp"`) taken.
    pub fn new() -> Registry {
        let mut r = Registry::default();
        r.tags.insert(HEADER_TAG.to_owned());
        r
    }

    fn register_tag(&mut self, tag: String) -> Result<(), Error> {
        if !self.tags.insert(tag.clone()) {
            return Err(Error::Fatal(format!("tag {tag} is already registered")));
        }
        Ok(())
    }

    fn insert(&mut self, name: &'static str, server: ServerCmd) -> Result<(), Error> {
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
            Box::new(move |host, body| {
                Box::pin(async move {
                    let arg =
                        protocol::decode_section_body(&args_tag(cmd.name), &body, cmd.read_arg)?;
                    let res = (cmd.run)(host, arg).await?;
                    protocol::encode_section(&res_tag(cmd.name), |w| (cmd.write_res)(&res, w))
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
            Box::new(move |host, body| {
                Box::pin(async move {
                    let (fspath, arg) =
                        protocol::decode_section_body(&args_tag(cmd.name), &body, |r| {
                            Ok((Fspath::unmarshal(r)?, (cmd.read_arg)(r)?))
                        })?;
                    let res = (cmd.run)(host, fspath, arg).await?;
                    protocol::encode_section(&res_tag(cmd.name), |w| (cmd.write_res)(&res, w))
                })
            }),
        )
    }

    /// Whether a command is registered (`Remote.commandAvailable`).
    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// `processRequest`: runs the request in `msg` (header section, then
    /// argument section) on `host`, and returns the reply (header section,
    /// then result section if it succeeded).
    pub async fn process_request(&self, host: Rc<Host>, msg: &[u8]) -> Result<Vec<u8>, Error> {
        let (header, rest) = protocol::split_section(msg)?;
        let Header::Request(name) =
            protocol::decode_section_body(HEADER_TAG, header, Header::unmarshal)?
        else {
            return Err(Error::Fatal("Protocol error: expected a request".into()));
        };
        let cmd = self
            .commands
            .get(name.as_str())
            .ok_or_else(|| Error::Fatal(format!("{name} not registered!")))?;
        let (args, _) = protocol::split_section(rest)?;
        let result = cmd(host, args.to_vec()).await;
        let mut reply = Header::of_result(&result).encode()?;
        if let Ok(res) = result {
            reply.extend(res);
        }
        Ok(reply)
    }
}

/// How an [`Rpc`] runs commands on local roots.
pub enum Dispatch {
    /// Call the command directly on the client's own host.
    Direct(Rc<Host>),
    /// Encode every call and run it on a separate host (see the module
    /// documentation).
    Loopback(Rc<Host>),
}

/// Runs commands on roots.
pub struct Rpc {
    registry: Registry,
    dispatch: Dispatch,
}

impl Rpc {
    /// An RPC with no commands registered.
    pub fn new(dispatch: Dispatch) -> Rpc {
        Rpc {
            registry: Registry::new(),
            dispatch,
        }
    }

    /// The registry, to register commands with.
    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }

    /// The host that local roots' commands run on.
    pub fn local_host(&self) -> &Rc<Host> {
        match &self.dispatch {
            Dispatch::Direct(h) | Dispatch::Loopback(h) => h,
        }
    }

    fn check_local(&self, host: &RootHost, name: &str) -> Result<(), Error> {
        if let RootHost::Remote(h) = host {
            return Err(Error::Fatal(format!(
                "Remote roots are not supported yet (command {name} for host {h})"
            )));
        }
        if !self.registry.contains(name) {
            return Err(Error::Fatal(format!("{name} not registered!")));
        }
        Ok(())
    }

    /// Sends a request through the registry as a server would receive it,
    /// and decodes the reply.
    async fn loopback<R>(
        &self,
        host: &Rc<Host>,
        name: &str,
        args: Vec<u8>,
        read_res: Read<R>,
    ) -> Result<R, Error> {
        let mut msg = Header::Request(name.to_owned()).encode()?;
        msg.extend(args);
        let reply = self.registry.process_request(host.clone(), &msg).await?;
        let (header, rest) = protocol::split_section(&reply)?;
        match protocol::decode_section_body(HEADER_TAG, header, Header::unmarshal)? {
            Header::NormalResult => protocol::decode_section(&res_tag(name), rest, read_res),
            Header::TransientExn(m) => Err(Error::Transient(m)),
            Header::FatalExn(m) => Err(Error::Fatal(m)),
            other => Err(Error::Fatal(format!(
                "Protocol error: unexpected reply {other:?}"
            ))),
        }
    }

    /// Runs a host command on `host` (`registerHostCmd`'s client side).
    pub async fn call_host<A, R>(
        &self,
        cmd: &'static HostCmd<A, R>,
        host: &RootHost,
        arg: A,
    ) -> Result<R, Error> {
        self.check_local(host, cmd.name)?;
        match &self.dispatch {
            Dispatch::Direct(h) => (cmd.run)(h.clone(), arg).await,
            Dispatch::Loopback(h) => {
                let args =
                    protocol::encode_section(&args_tag(cmd.name), |w| (cmd.write_arg)(&arg, w))?;
                self.loopback(h, cmd.name, args, cmd.read_res).await
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
        self.check_local(&root.host, cmd.name)?;
        match &self.dispatch {
            Dispatch::Direct(h) => (cmd.run)(h.clone(), root.fspath.clone(), arg).await,
            Dispatch::Loopback(h) => {
                let args = protocol::encode_section(&args_tag(cmd.name), |w| {
                    root.fspath.marshal(w);
                    (cmd.write_arg)(&arg, w);
                })?;
                self.loopback(h, cmd.name, args, cmd.read_res).await
            }
        }
    }
}
