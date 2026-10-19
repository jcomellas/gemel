//! Runs a future to completion on a helper thread.
//!
//! `rfd`'s async dialogs present as window-modal sheets on macOS and leave
//! the UI event loop running; the GUI waits for the result here instead of
//! blocking the event loop.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;

struct ThreadWaker(Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Polls `fut` on the current thread until it completes.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Runs `fut` on a new thread and passes its output to `done` there.
pub fn spawn<T, F, D>(fut: F, done: D)
where
    F: Future<Output = T> + Send + 'static,
    D: FnOnce(T) + Send + 'static,
{
    std::thread::spawn(move || done(block_on(fut)));
}

#[cfg(test)]
mod tests {
    #[test]
    fn runs_to_completion() {
        let (tx, rx) = std::sync::mpsc::channel();
        super::spawn(async { 41 + 1 }, move |v| tx.send(v).unwrap());
        assert_eq!(rx.recv().unwrap(), 42);
    }
}
