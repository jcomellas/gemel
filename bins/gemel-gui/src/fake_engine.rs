//! A fake engine: deterministic recon rows and a background progress feed.
//!
//! Until the engine exists, this is the GUI's only data source. It stands in
//! for the `gemel-ui-api` session: [`generate_rows`] plays the result of
//! update detection and reconciliation, and the [`Feed`] thread plays the
//! transfer loop, sending batches of [`ProgressUpdate`]s over a channel at a
//! fixed rate. Replace it with the real session events; it must never
//! depend on `gemel-engine` directly.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::recon_view::{Direction, ProgressUpdate, ReplicaStatus, Row};

/// A small deterministic PRNG (xorshift64*), so runs are repeatable without
/// a `rand` dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Generates `n` plausible recon rows.
pub fn generate_rows(n: usize) -> Vec<Row> {
    const TOP: [&str; 6] = ["Documents", "Photos", "src", "Music", "Projects", ".config"];
    const MID: [&str; 8] = [
        "archive", "drafts", "2025", "2026", "client-a", "client-b", "build", "notes",
    ];
    const EXT: [&str; 7] = ["txt", "jpg", "rs", "ml", "pdf", "flac", "json"];
    let mut rng = Rng(0x0067_656d_656c); // "gemel" in ASCII
    (0..n)
        .map(|i| {
            let status = |rng: &mut Rng| match rng.below(10) {
                0..=2 => ReplicaStatus::Unchanged,
                3..=5 => ReplicaStatus::Modified,
                6..=7 => ReplicaStatus::Created,
                8 => ReplicaStatus::Deleted,
                _ => ReplicaStatus::PropsChanged,
            };
            let mut left = status(&mut rng);
            let mut right = status(&mut rng);
            if left == ReplicaStatus::Unchanged && right == ReplicaStatus::Unchanged {
                left = ReplicaStatus::Modified;
            }
            // Most items change on one side only; a few are conflicts.
            if rng.below(8) != 0 {
                if rng.below(2) == 0 {
                    right = ReplicaStatus::Unchanged;
                } else {
                    left = ReplicaStatus::Unchanged;
                }
            }
            let direction = match (left, right) {
                (ReplicaStatus::Unchanged, _) => Direction::RightToLeft,
                (_, ReplicaStatus::Unchanged) => Direction::LeftToRight,
                _ => Direction::Conflict,
            };
            let path = format!(
                "{}/{}/{}/file-{i:05}.{}",
                TOP[rng.below(TOP.len() as u64) as usize],
                MID[rng.below(MID.len() as u64) as usize],
                MID[rng.below(MID.len() as u64) as usize],
                EXT[rng.below(EXT.len() as u64) as usize],
            );
            // Log-uniform sizes from 1 B to ~1 GiB.
            let size = 1u64 << rng.below(30);
            let size = size + rng.below(size);
            Row {
                path,
                left,
                right,
                direction,
                size,
                progress: 0.0,
            }
        })
        .collect()
}

/// A running progress feed. Stops when dropped.
pub struct Feed {
    rx: Receiver<Vec<ProgressUpdate>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Feed {
    /// Starts a feed over `rows` rows sending `rate` updates per second.
    ///
    /// Sixteen simulated transfers run at once; each update moves one of
    /// them forward 10%, and a finished transfer picks the next row with a
    /// large stride, so updates land both inside and outside the visible
    /// part of the table. The GUI drains the channel on a timer.
    pub fn spawn(rows: usize, rate: u32) -> Feed {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("fake-engine".into())
            .spawn(move || {
                if rows == 0 || rate == 0 {
                    return;
                }
                const SLOTS: usize = 16;
                const STRIDE: usize = 7919;
                let mut next_row = 0usize;
                let mut pick = || {
                    let id = next_row;
                    next_row = (next_row + STRIDE) % rows;
                    id as u32
                };
                let mut slots: Vec<(u32, f32)> = (0..SLOTS).map(|_| (pick(), 0.0)).collect();
                let mut slot = 0;
                let start = Instant::now();
                let mut sent: u64 = 0;
                while !stop_flag.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10));
                    let due = (start.elapsed().as_secs_f64() * f64::from(rate)) as u64;
                    let mut batch = Vec::with_capacity((due - sent) as usize);
                    while sent < due {
                        let (id, progress) = &mut slots[slot];
                        *progress = (*progress + 0.1).min(1.0);
                        batch.push(ProgressUpdate {
                            id: *id,
                            progress: *progress,
                        });
                        if *progress >= 1.0 {
                            slots[slot] = (pick(), 0.0);
                        }
                        slot = (slot + 1) % SLOTS;
                        sent += 1;
                    }
                    if !batch.is_empty() && tx.send(batch).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn fake engine thread");
        Feed {
            rx,
            stop,
            thread: Some(thread),
        }
    }

    /// Takes every batch that has arrived, without blocking.
    pub fn drain(&self) -> Vec<ProgressUpdate> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(batch) => out.extend(batch),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return out,
            }
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_deterministic_and_consistent() {
        let a = generate_rows(1000);
        assert_eq!(a, generate_rows(1000));
        for row in &a {
            assert!(row.size >= 1);
            match row.direction {
                Direction::LeftToRight => assert_eq!(row.right, ReplicaStatus::Unchanged),
                Direction::RightToLeft => assert_eq!(row.left, ReplicaStatus::Unchanged),
                Direction::Conflict => {
                    assert_ne!(row.left, ReplicaStatus::Unchanged);
                    assert_ne!(row.right, ReplicaStatus::Unchanged);
                }
                Direction::Skip => panic!("generated a skip"),
            }
        }
        assert!(a.iter().any(|r| r.direction == Direction::Conflict));
    }

    #[test]
    fn feed_sends_valid_updates() {
        // No timing assertions: CI runners are too noisy for them.
        let feed = Feed::spawn(100, 1000);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = Vec::new();
        while got.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            got = feed.drain();
        }
        assert!(!got.is_empty());
        assert!(
            got.iter()
                .all(|u| u.id < 100 && (0.0..=1.0).contains(&u.progress))
        );
    }
}
