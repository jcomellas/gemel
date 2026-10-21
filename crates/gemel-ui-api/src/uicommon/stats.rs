//! The progress rate and time left during propagation (`Uicommon.Stats`).
//!
//! Ports the `Stats` module of Unison `src/uicommon.ml`.
//!
//! Derived from Unison, Copyright 1999-2020, Benjamin C. Pierce.
//!
//! Progress isn't tracked accurately (hashing before and after a copy
//! isn't counted, and continuing an interrupted transfer changes the total),
//! so the rate is smoothed twice, ignoring small changes but following
//! large ones quickly.

use gemel_fs::props::Filesize;

/// `calcETA`: `rem` bytes at `rate` bytes a second, less precisely the
/// longer it is; empty if either is unknown. (OCaml's `truncate` of a time
/// too long for an integer, as at a rate of 0, is undefined; here it shows
/// nothing.)
fn calc_eta(rem: f64, rate: f64) -> String {
    if rate.is_nan() || rem.is_nan() || rem < 0. {
        return String::new();
    }
    let secs = rem / rate + 0.5;
    if !secs.is_finite() || secs.abs() >= 1e18 {
        return String::new();
    }
    let t = secs.trunc() as i64;
    if t >= 86220 {
        let u = t + 180;
        return format!(
            "{}d {:02}:{:02}:00",
            u / 86400,
            (u % 86400) / 3600,
            ((u % 3600) / 300) * 5
        );
    }
    let (h, m, s) = if t >= 3420 {
        let u = t + 180;
        (u / 3600, ((u % 3600) / 300) * 5, 0)
    } else if t >= 2640 {
        (0, ((t + 180) / 300) * 5, 0)
    } else if t >= 1800 {
        (0, ((t + 119) / 120) * 2, 0)
    } else if t >= 120 {
        let u = t + 15;
        (0, u / 60, ((u % 60) / 30) * 30)
    } else {
        (0, t / 60, t % 60)
    };
    format!("{h:02}:{m:02}:{s:02}")
}

/// `Float.min`: NaN if either is.
fn float_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

/// `Float.min_num`: ignores a NaN.
fn float_min_num(a: f64, b: f64) -> f64 {
    a.min(b)
}

/// `movAvg`: the moving average over `avg_period` seconds, `delta_time`
/// after `prev`, weighted by `c`.
fn mov_avg(curr: f64, prev: f64, c: f64, delta_time: f64, avg_period: f64) -> f64 {
    if prev.is_nan() {
        return curr;
    }
    let a = c * float_min(1. - (-delta_time / avg_period).exp(), 1.);
    prev + a * (curr - prev)
}

const AVG_PERIOD_S: f64 = 4.0;
const AVG_PERIOD_D: f64 = 3.5;
const CALC_PERIOD: f64 = 0.25;

/// `Uicommon.Stats.t`.
#[derive(Debug, Clone)]
pub struct Stats {
    t0: f64,
    t: f64,
    total_to_complete: Filesize,
    completed: Filesize,
    cur_rate: f64,
    avg_rate_s: f64,
    avg_rate_double_s_gauss: f64,
}

impl Stats {
    /// `init`: nothing done yet of `total_to_transfer` bytes.
    pub fn new(total_to_transfer: Filesize) -> Stats {
        Stats {
            t0: 0.,
            t: 0.,
            total_to_complete: total_to_transfer,
            completed: 0,
            cur_rate: f64::NAN,
            avg_rate_s: f64::NAN,
            avg_rate_double_s_gauss: f64::NAN,
        }
    }

    fn calc_avg_rate(&mut self, tot_time: f64, delta_completed: Filesize, delta_time: f64) {
        let cur_rate = delta_completed as f64 / delta_time;
        let avg_rate_s = mov_avg(
            cur_rate,
            self.avg_rate_s,
            1.,
            delta_time,
            float_min_num(tot_time, AVG_PERIOD_S),
        );
        let cpr = (avg_rate_s - self.avg_rate_double_s_gauss) / self.avg_rate_double_s_gauss;
        let gauss_c = 2. * 0.025f64.powf(2.);
        let c = 1. - (-cpr.powf(2.) / gauss_c).exp();
        let avg_rate_double_s_gauss = mov_avg(
            avg_rate_s,
            self.avg_rate_double_s_gauss,
            c,
            delta_time,
            float_min_num(tot_time, AVG_PERIOD_D),
        );
        self.cur_rate = cur_rate;
        self.avg_rate_s = avg_rate_s;
        self.avg_rate_double_s_gauss = avg_rate_double_s_gauss;
    }

    /// `update`: `total_completed` bytes done at time `t1`. The first
    /// progress only starts the clock (the time before it, starting
    /// transport, would skew the rate); after that, the rates change at
    /// most four times a second.
    pub fn update(&mut self, t1: f64, total_completed: Filesize) {
        let delta_time = t1 - self.t;
        if self.completed == 0 {
            self.completed = total_completed;
            self.t0 = t1;
            self.t = t1;
        } else if delta_time >= CALC_PERIOD {
            let delta_completed = total_completed.wrapping_sub(self.completed);
            self.completed = total_completed;
            self.t = t1;
            self.calc_avg_rate(t1 - self.t0, delta_completed, delta_time);
        }
    }

    /// `curRate`: the latest rate, which changes a lot.
    pub fn cur_rate(&self) -> f64 {
        self.cur_rate
    }

    /// `avgRate1`: its moving average, steadier.
    pub fn avg_rate1(&self) -> f64 {
        self.avg_rate_s
    }

    /// `avgRate2`: the moving average of that, steadier still.
    pub fn avg_rate2(&self) -> f64 {
        self.avg_rate_double_s_gauss
    }

    /// `eta`: the time left at `rate` (by default [`Stats::avg_rate2`]), or
    /// `default` if it isn't known.
    pub fn eta(&self, rate: Option<f64>, default: &str) -> String {
        let rem = self.total_to_complete.wrapping_sub(self.completed) as f64;
        let eta = calc_eta(rem, rate.unwrap_or(self.avg_rate_double_s_gauss));
        if eta.is_empty() {
            default.to_owned()
        } else {
            eta
        }
    }
}
