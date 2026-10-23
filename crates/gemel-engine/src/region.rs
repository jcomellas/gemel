//! Regions that limit how much of something runs at once.
//!
//! Ports Unison `src/lwt/lwt_util.ml`'s regions (`make_region`,
//! `run_in_region`).
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! A region has a size; each task in it counts for some amount. A task
//! enters at once while the region isn't full, even if it then overflows
//! it, and waits otherwise. As each task leaves, the first waiting is let
//! in, if what is left is below the size. (OCaml's waiter then runs at
//! once, before the task leaving goes on; here it is woken to run next.)

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;

use tokio::sync::oneshot;

/// `Lwt_util.region`.
pub struct Region {
    size: Cell<usize>,
    count: Cell<usize>,
    waiters: RefCell<VecDeque<(oneshot::Sender<()>, usize)>>,
}

impl Region {
    /// `make_region`.
    pub fn new(size: usize) -> Region {
        Region {
            size: Cell::new(size),
            count: Cell::new(0),
            waiters: RefCell::default(),
        }
    }

    /// `resize_region`: a new size, for those that enter from now on.
    pub fn resize(&self, size: usize) {
        self.size.set(size);
    }

    /// `run_in_region`: runs `f`, counting for `sz`, once it may enter.
    pub async fn run<T>(&self, sz: usize, f: impl Future<Output = T>) -> T {
        if self.count.get() >= self.size.get() {
            let (tx, rx) = oneshot::channel();
            self.waiters.borrow_mut().push_back((tx, sz));
            // Let in by `leave`, which counts it.
            let _ = rx.await;
        } else {
            self.count.set(self.count.get() + sz);
        }
        let _inside = Inside(self, sz);
        f.await
    }

    /// `leave_region`: lets the first waiting in if what is left is below
    /// the size.
    fn leave(&self, sz: usize) {
        let count = self.count.get() - sz;
        if count < self.size.get() {
            while let Some((w, sz)) = self.waiters.borrow_mut().pop_front() {
                // One that has given up waiting doesn't enter.
                if w.send(()).is_ok() {
                    self.count.set(count + sz);
                    return;
                }
            }
        }
        self.count.set(count);
    }
}

/// A task in a region, which leaves it when done, or dropped.
struct Inside<'a>(&'a Region, usize);

impl Drop for Inside<'_> {
    fn drop(&mut self) {
        self.0.leave(self.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_wait_once_the_region_is_full_and_enter_in_turn() {
        gemel_rpc::runtime::run_here(async {
            let r = std::rc::Rc::new(Region::new(10));
            let order = std::rc::Rc::new(RefCell::new(Vec::new()));
            let (go_tx, go_rx) = oneshot::channel::<()>();
            let mut go = Some(go_rx);
            let mut tasks = Vec::new();
            // The first overflows the region; the others wait for it.
            for (i, sz) in [(0, 12), (1, 3), (2, 4)] {
                let (r, order) = (r.clone(), order.clone());
                let go = if i == 0 { go.take() } else { None };
                tasks.push(tokio::task::spawn_local(async move {
                    r.run(sz, async {
                        order.borrow_mut().push(format!("in {i}"));
                        if let Some(go) = go {
                            let _ = go.await;
                        }
                    })
                    .await;
                    order.borrow_mut().push(format!("out {i}"));
                }));
            }
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            assert_eq!(*order.borrow(), ["in 0"]);
            let _ = go_tx.send(());
            for t in tasks {
                t.await.unwrap();
            }
            assert_eq!(
                *order.borrow(),
                ["in 0", "out 0", "in 1", "out 1", "in 2", "out 2"]
            );
        })
        .unwrap();
    }
}
