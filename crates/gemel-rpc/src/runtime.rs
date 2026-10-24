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
    /// thread ends when the future does. Blocking work still under way
    /// then is left to end with the process: on Windows, tokio reads a
    /// shell's output there, and the read ends only once the server does,
    /// which may wait for the client to exit.
    pub fn spawn<F, Fut>(engine: F) -> std::io::Result<Self>
    where
        F: FnOnce(Commands<C>, Events<E>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let thread = std::thread::Builder::new()
            .name("gemel-engine".into())
            .spawn(move || {
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, engine(cmd_rx, ev_tx));
                drop(local);
                runtime.shutdown_background();
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

    /// Hands the events to `f` from now on, on a thread of their own, for
    /// a frontend that waits for other things too (keys, signals): then
    /// `None` once the engine has stopped. Once `f` returns false the
    /// events are dropped, as by [`join`].
    ///
    /// [`join`]: EngineThread::join
    pub fn forward_events(
        &mut self,
        mut f: impl FnMut(Option<E>) -> bool + Send + 'static,
    ) -> std::io::Result<()> {
        let (_, closed) = mpsc::unbounded_channel();
        let mut events = std::mem::replace(&mut self.events, closed);
        std::thread::Builder::new()
            .name("gemel-events".into())
            .spawn(move || {
                while let Some(e) = events.blocking_recv() {
                    if !f(Some(e)) {
                        events.close();
                        while events.try_recv().is_ok() {}
                        return;
                    }
                }
                f(None);
            })?;
        Ok(())
    }

    /// Stops sending commands and waits for the engine thread to finish.
    /// The events not taken, and any sent from now on, are dropped: so an
    /// engine waiting for an answer carried by one (a channel it holds the
    /// other end of) sees it dropped, and doesn't wait forever.
    pub fn join(mut self) -> std::thread::Result<()> {
        let (tx, _) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut self.commands, tx));
        self.events.close();
        while self.events.try_recv().is_ok() {}
        self.thread.take().map_or(Ok(()), JoinHandle::join)
    }
}

/// Runs `engine` with this thread as the engine thread, for a process
/// that is only an engine (a server). Blocking work still under way when
/// it ends (reading standard input, say) is left to end with the process.
pub fn run_here<F: Future>(engine: F) -> std::io::Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    let output = local.block_on(&runtime, engine);
    drop(local);
    runtime.shutdown_background();
    Ok(output)
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

/// Runs `f` now, in the caller, until it has to wait, and leaves the rest
/// to a task on the engine thread: as Lwt runs a thread until it blocks.
/// What `f` waits for must be polled again to wake it (as I/O is), since
/// the first poll has no one to wake.
///
/// # Panics
///
/// When `f` has to wait, outside the engine thread's runtime.
pub fn run_now(f: impl Future<Output = ()> + 'static) {
    let mut f = Box::pin(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    if f.as_mut().poll(&mut cx).is_pending() {
        spawn_local(f);
    }
}
