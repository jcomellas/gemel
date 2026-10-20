//! The engine thread.
//!
//! Unison's engine is cooperative (Lwt): it changes shared state between
//! the points where it waits, without locks. gemel keeps that shape by
//! running the engine on one thread, in a single-threaded tokio runtime,
//! with its state in `Rc`/`RefCell` and futures that need not be `Send`.
//! Frontends talk to it only through channels: commands in, events out.
//! Work that blocks (directory scans, hashing) goes to [`blocking`], which
//! runs it on tokio's blocking pool.

use std::future::Future;
use std::thread::JoinHandle;

use tokio::sync::mpsc;

/// Receives the frontend's commands, on the engine thread.
pub type Commands<C> = mpsc::UnboundedReceiver<C>;
/// Sends events to the frontend, from the engine thread.
pub type Events<E> = mpsc::UnboundedSender<E>;

/// A running engine thread, as the frontend sees it.
pub struct EngineThread<C, E> {
    commands: mpsc::UnboundedSender<C>,
    events: mpsc::UnboundedReceiver<E>,
    thread: Option<JoinHandle<()>>,
}

impl<C: Send + 'static, E: Send + 'static> EngineThread<C, E> {
    /// Starts the engine thread running `engine(commands, events)`. The
    /// thread ends when the future does.
    pub fn spawn<F, Fut>(engine: F) -> std::io::Result<Self>
    where
        F: FnOnce(Commands<C>, Events<E>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        let thread = std::thread::Builder::new()
            .name("gemel-engine".into())
            .spawn(move || {
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, engine(cmd_rx, ev_tx));
            })?;
        Ok(EngineThread {
            commands: cmd_tx,
            events: ev_rx,
            thread: Some(thread),
        })
    }

    /// Sends a command; false if the engine has stopped.
    pub fn send(&self, c: C) -> bool {
        self.commands.send(c).is_ok()
    }

    /// Waits for the next event; `None` once the engine has stopped and
    /// all events are taken. For frontends without a runtime of their own.
    pub fn recv_blocking(&mut self) -> Option<E> {
        self.events.blocking_recv()
    }

    /// The next event if there is one, without waiting.
    pub fn try_recv(&mut self) -> Option<E> {
        self.events.try_recv().ok()
    }

    /// Stops sending commands and waits for the engine thread to finish.
    pub fn join(mut self) -> std::thread::Result<()> {
        let (tx, _) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut self.commands, tx));
        self.thread.take().map_or(Ok(()), JoinHandle::join)
    }
}

/// Runs blocking work off the engine thread and waits for it.
///
/// # Panics
///
/// If `f` panics, or when called outside the engine thread's runtime.
pub async fn blocking<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Starts a task on the engine thread that runs alongside the caller.
///
/// # Panics
///
/// When called outside the engine thread's runtime.
pub fn spawn_local<F>(f: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    tokio::task::spawn_local(f)
}
