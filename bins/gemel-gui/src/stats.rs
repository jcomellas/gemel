//! The statistics window's figures: the rates and bytes sent to and
//! received from the server, with a graph of each rate, and the file data
//! written on each side.
//!
//! Ports `uigtk3.ml`'s `statistics`, its `stats` graph, `rate2str` and
//! `kib2str`. The window samples the session's traffic counters twice a
//! second, from when update detection or a sync starts until a little
//! after it ends (`startStats`, `stopStats`).

use std::sync::Arc;

use gemel_ui_api::session::Traffic;

/// How often the figures are sampled, in seconds (`delay`).
pub const DELAY: f64 = 0.5;

/// The graphs' smoothing (`a`).
const A: f64 = 0.5;
/// The table's (`b`).
const B: f64 = 0.8;

/// The samples taken after a sync or a scan ends (`stopStats`).
const AFTER: i32 = 10;

/// `rate2str`: a rate in bytes per second, or a blank below 100 B/s.
pub fn rate2str(v: f64) -> String {
    if v > 9.9e3 {
        if v > 9.9e6 {
            format!("{:.0} MiB/s", v / 1e6)
        } else if v > 999e3 {
            format!("{:.1} MiB/s", v / 1e6)
        } else {
            format!("{:.0} KiB/s", v / 1e3)
        }
    } else if v > 990. {
        format!("{:.1} KiB/s", v / 1e3)
    } else if v > 99. {
        format!("{:.2} KiB/s", v / 1e3)
    } else {
        " ".to_owned()
    }
}

/// `kib2str`: a number of bytes.
pub fn kib2str(v: f64) -> String {
    const MIB: f64 = 1024. * 1024.;
    if v > 100_000_000. {
        format!("{:.0} MiB", v / MIB)
    } else if v > 1_000_000. {
        format!("{:.1} MiB", v / MIB)
    } else if v > 1024. {
        format!("{:.1} KiB", v / 1024.)
    } else {
        format!("{v:.0} B")
    }
}

/// A graph of a rate over the last samples (`stats`), drawn as a filled
/// path.
#[derive(Debug, Clone)]
pub struct Graph {
    values: Vec<f64>,
    maxim: f64,
}

impl Graph {
    /// A graph `width` samples wide, all zero.
    pub fn new(width: usize) -> Graph {
        Graph {
            values: vec![0.; width],
            maxim: 0.,
        }
    }

    /// Adds a sample on the right (`push`).
    pub fn push(&mut self, v: f64) {
        let need_max = self.values[0] == self.maxim;
        self.values.remove(0);
        self.values.push(v);
        if need_max {
            self.maxim = self.values.iter().copied().fold(0., f64::max);
        } else {
            self.maxim = self.maxim.max(v);
        }
    }

    /// The scale: 1, times 1.5 until it is at least the largest sample.
    fn scale(&self) -> f64 {
        let mut scale = 1.;
        while self.maxim > scale {
            scale *= 1.5;
        }
        scale
    }

    /// The graph `height` high, as an SVG path, one unit per sample.
    pub fn commands(&self, height: f64) -> String {
        let scale = self.scale();
        let w = self.values.len();
        let mut s = format!("M 0 {height}");
        for (i, v) in self.values.iter().enumerate() {
            let y = height - height * v / scale;
            s += &format!(" L {i} {y:.2} L {} {y:.2}", i + 1);
        }
        s += &format!(" L {w} {height} Z");
        s
    }
}

/// What the statistics window shows, and the sampling.
#[derive(Debug)]
pub struct Stats {
    traffic: Arc<Traffic>,
    emitted: f64,
    received: f64,
    emit_rate: f64,
    emit_rate2: f64,
    receive_rate: f64,
    receive_rate2: f64,
    /// Samples still to take: negative while a sync or scan runs, 0 when
    /// sampling has stopped (`stopCounter`).
    stop_counter: i32,
    /// File data written on the client's side and on the server's
    /// (`clientWritten`, `serverWritten`).
    pub client_written: f64,
    /// The same, on the server's.
    pub server_written: f64,
    /// The rate of what is sent.
    pub emission: Graph,
    /// The rate of what is received.
    pub reception: Graph,
}

/// The graphs' width, in samples.
pub const GRAPH_WIDTH: usize = 320;

impl Stats {
    /// The figures for `traffic`, before any sampling.
    pub fn new(traffic: Arc<Traffic>) -> Stats {
        Stats {
            traffic,
            emitted: 0.,
            received: 0.,
            emit_rate: 0.,
            emit_rate2: 0.,
            receive_rate: 0.,
            receive_rate2: 0.,
            stop_counter: 0,
            client_written: 0.,
            server_written: 0.,
            emission: Graph::new(GRAPH_WIDTH),
            reception: Graph::new(GRAPH_WIDTH),
        }
    }

    /// `startStats`: sampling goes on until [`Stats::stop`]; whether it
    /// must be started (it wasn't running).
    pub fn start(&mut self) -> bool {
        let start = self.stop_counter == 0;
        if start {
            self.emitted = self.traffic.emitted() as f64;
            self.received = self.traffic.received() as f64;
        }
        self.stop_counter = -1;
        start
    }

    /// `stopStats`: a few more samples, then sampling stops.
    pub fn stop(&mut self) {
        self.stop_counter = AFTER;
    }

    /// A sample (`timeout`): whether sampling goes on.
    pub fn sample(&mut self) -> bool {
        let (emitted, received) = (
            self.traffic.emitted() as f64,
            self.traffic.received() as f64,
        );
        let sent = (emitted - self.emitted) / DELAY;
        let got = (received - self.received) / DELAY;
        self.emit_rate = A * self.emit_rate + (1. - A) * sent;
        self.emit_rate2 = B * self.emit_rate2 + (1. - B) * sent;
        self.emission.push(self.emit_rate);
        self.receive_rate = A * self.receive_rate + (1. - A) * got;
        self.receive_rate2 = B * self.receive_rate2 + (1. - B) * got;
        self.reception.push(self.receive_rate);
        self.emitted = emitted;
        self.received = received;
        if self.stop_counter > 0 {
            self.stop_counter -= 1;
        }
        if self.stop_counter == 0 {
            self.emit_rate2 = 0.;
            self.receive_rate2 = 0.;
        }
        self.stop_counter != 0
    }

    /// The table's rows (`updateTable`): the reception rate, the data
    /// received and the file data written, each on the client, the server
    /// and in all.
    pub fn table(&self) -> [[String; 3]; 3] {
        let three = |c: f64, s: f64, f: fn(f64) -> String| [f(c), f(s), f(c + s)];
        [
            three(self.receive_rate2, self.emit_rate2, rate2str),
            three(self.received, self.emitted, kib2str),
            three(self.client_written, self.server_written, kib2str),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_sizes_as_uigtk3_writes_them() {
        assert_eq!(rate2str(50.), " ");
        assert_eq!(rate2str(500.), "0.50 KiB/s");
        assert_eq!(rate2str(5_000.), "5.0 KiB/s");
        assert_eq!(rate2str(50_000.), "50 KiB/s");
        assert_eq!(rate2str(5_000_000.), "5.0 MiB/s");
        assert_eq!(rate2str(50_000_000.), "50 MiB/s");
        assert_eq!(kib2str(0.), "0 B");
        assert_eq!(kib2str(1024.), "1024 B");
        assert_eq!(kib2str(1536.), "1.5 KiB");
        assert_eq!(kib2str(2_097_152.), "2.0 MiB");
        assert_eq!(kib2str(209_715_200.), "200 MiB");
    }

    #[test]
    fn a_graph_scales_to_its_largest_sample() {
        let mut g = Graph::new(3);
        assert_eq!(
            g.commands(10.),
            "M 0 10 L 0 10.00 L 1 10.00 L 1 10.00 L 2 10.00 L 2 10.00 L 3 10.00 L 3 10 Z"
        );
        g.push(2.);
        // 1.5, then 2.25: the sample is at 2/2.25 of the height.
        assert!(
            g.commands(9.).contains("L 2 1.00 L 3 1.00"),
            "{}",
            g.commands(9.)
        );
        g.push(0.);
        g.push(0.);
        g.push(0.);
        assert_eq!(g.maxim, 0.);
    }

    #[test]
    fn sampling_smooths_and_stops() {
        let traffic = Arc::new(Traffic::default());
        let mut s = Stats::new(traffic);
        assert!(s.start());
        assert!(!s.start());
        assert!(s.sample());
        s.stop();
        for _ in 0..AFTER - 1 {
            assert!(s.sample());
        }
        assert!(!s.sample());
        assert_eq!(s.table()[1], ["0 B", "0 B", "0 B"]);
    }
}
