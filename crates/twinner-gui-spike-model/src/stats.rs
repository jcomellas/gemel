//! Frame statistics for the `--bench` runs.

use std::time::{Duration, Instant};

/// Records the interval between successive presented frames.
#[derive(Debug, Default)]
pub struct FrameStats {
    last: Option<Instant>,
    intervals_ms: Vec<f64>,
    /// Progress updates applied to the model during the run.
    pub updates_applied: u64,
}

impl FrameStats {
    /// Records that a frame was presented now.
    pub fn frame(&mut self) {
        self.frame_at(Instant::now());
    }

    /// Records that a frame was presented at `at`.
    pub fn frame_at(&mut self, at: Instant) {
        if let Some(last) = self.last {
            self.intervals_ms
                .push(at.duration_since(last).as_secs_f64() * 1000.0);
        }
        self.last = Some(at);
    }

    /// Number of frames recorded.
    pub fn frames(&self) -> usize {
        self.intervals_ms.len() + usize::from(self.last.is_some())
    }

    /// Records the main-thread work for one headless frame.
    pub fn work(&mut self, took: Duration) {
        self.intervals_ms.push(took.as_secs_f64() * 1000.0);
    }

    /// A one-paragraph summary: frame count, mean fps, p50/p99/max interval
    /// and the number of intervals over 33 ms and 50 ms (visible stalls).
    pub fn report(&self, toolkit: &str, wall: Duration) -> String {
        let mut sorted = self.intervals_ms.clone();
        sorted.sort_by(f64::total_cmp);
        let pct = |p: f64| {
            if sorted.is_empty() {
                0.0
            } else {
                sorted[((sorted.len() - 1) as f64 * p).round() as usize]
            }
        };
        let over = |ms: f64| sorted.iter().filter(|&&x| x > ms).count();
        format!(
            "[{toolkit}] bench {:.1}s: {} frames ({:.1} fps), interval p50 {:.2} ms, p99 {:.2} ms, \
             max {:.2} ms, >33ms {}, >50ms {}, progress updates applied {} ({:.0}/s)",
            wall.as_secs_f64(),
            self.frames(),
            self.frames() as f64 / wall.as_secs_f64(),
            pct(0.5),
            pct(0.99),
            sorted.last().copied().unwrap_or(0.0),
            over(33.0),
            over(50.0),
            self.updates_applied,
            self.updates_applied as f64 / wall.as_secs_f64(),
        )
    }
}

/// A one-off operation timed during a headless bench, together with the
/// frame that shows its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Select all 50k rows.
    SelectAll,
    /// Sort by status (the view order changes completely).
    SortByStatus,
    /// `>` on every selected row.
    DirectionAll,
}

/// Drives a headless bench at 60 Hz for `duration`: `frame` must apply the
/// pending engine updates, auto-scroll and lay out (and, where the toolkit
/// allows it without a window, render) one frame, after performing `op` if
/// given. Ordinary frames are summarised as percentiles; the frames that
/// carry an [`Op`] are reported one by one, since those are where a hitch
/// would show.
pub fn headless_bench(
    toolkit: &str,
    duration: Duration,
    mut frame: impl FnMut(Option<Op>) -> usize,
) -> String {
    const TICK: Duration = Duration::from_micros(16_667);
    let schedule = [
        (120, Op::SelectAll),
        (240, Op::SortByStatus),
        (360, Op::DirectionAll),
    ];
    let mut stats = FrameStats::default();
    let mut ops = Vec::new();
    let start = Instant::now();
    let mut next = start;
    let mut n = 0;
    while start.elapsed() < duration {
        next += TICK;
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let op = schedule.iter().find(|(at, _)| *at == n).map(|(_, op)| *op);
        let t = Instant::now();
        stats.updates_applied += frame(op) as u64;
        let took = t.elapsed();
        match op {
            Some(op) => ops.push(format!("{op:?} {:.2} ms", took.as_secs_f64() * 1000.0)),
            None => stats.work(took),
        }
        n += 1;
    }
    let mut sorted = stats.intervals_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize];
    format!(
        "[{toolkit}] headless {:.1}s: {} frames, work per frame p50 {:.2} ms, p99 {:.2} ms, \
         max {:.2} ms; one-off frames: {}; progress updates applied {}",
        start.elapsed().as_secs_f64(),
        n,
        pct(0.5),
        pct(0.99),
        sorted.last().copied().unwrap_or(0.0),
        ops.join(", "),
        stats.updates_applied,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_intervals() {
        let t0 = Instant::now();
        let mut s = FrameStats::default();
        for ms in [0, 16, 32, 100] {
            s.frame_at(t0 + Duration::from_millis(ms));
        }
        assert_eq!(s.frames(), 4);
        let r = s.report("t", Duration::from_secs(1));
        assert!(r.contains(">33ms 1, >50ms 1"), "{r}");
        assert!(r.contains("max 68.00 ms"), "{r}");
    }

    #[test]
    fn headless_reports_ops() {
        let mut seen = Vec::new();
        let r = headless_bench("t", Duration::from_millis(6200), |op| {
            seen.extend(op);
            1
        });
        assert_eq!(seen, [Op::SelectAll, Op::SortByStatus, Op::DirectionAll]);
        assert!(r.contains("SelectAll"), "{r}");
    }
}
