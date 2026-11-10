//! Opt-in timing statistics for the render path and the edit-time caches.
//!
//! Off by default and free when off: [`start`] returns `None` after a single
//! relaxed-atomic-style `OnceLock` read and [`lap`] does nothing for `None`.
//! Set `OSM_GPUI_FRAME_STATS=1` to collect. Samples accumulate per phase name
//! until [`report`] prints (and clears) them: one line per phase with sample
//! count, min, median, p95, max and mean in milliseconds. The script harness
//! calls it from the `frame_stats LABEL` op, so each section of a benchmark
//! script gets its own table. See `docs/perf/` for how it was used.
//!
//! These are CPU-side wall-clock timings of the code that records draw
//! commands. GPU rasterisation and presentation are not covered.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static ENABLED: OnceLock<bool> = OnceLock::new();
static LAST: Mutex<Vec<(&'static str, Instant)>> = Mutex::new(Vec::new());
static SAMPLES: Mutex<Vec<(&'static str, Vec<u32>)>> = Mutex::new(Vec::new());

/// Whether `OSM_GPUI_FRAME_STATS` is set to anything but empty or `0`.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("OSM_GPUI_FRAME_STATS")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

/// Start a timer; `None` (and no clock read) when collection is off.
#[inline]
pub fn start() -> Option<Instant> {
    if enabled() {
        Some(Instant::now())
    } else {
        None
    }
}

/// Record the time since `started` under `name`.
#[inline]
pub fn lap(name: &'static str, started: Option<Instant>) {
    if let Some(t) = started {
        record(name, t.elapsed());
    }
}

/// Records the time from its creation to its drop, for scopes with many exits.
pub struct Scoped(&'static str, Option<Instant>);

impl Scoped {
    pub fn new(name: &'static str, started: Option<Instant>) -> Self {
        Scoped(name, started)
    }
}

impl Drop for Scoped {
    fn drop(&mut self) {
        lap(self.0, self.1);
    }
}

/// Record one sample (stored with microsecond resolution).
pub fn record(name: &'static str, d: Duration) {
    let us = d.as_micros().min(u32::MAX as u128) as u32;
    let mut all = SAMPLES.lock().unwrap();
    match all.iter_mut().find(|(n, _)| *n == name) {
        Some((_, v)) => v.push(us),
        None => all.push((name, vec![us])),
    }
}

/// Record a per-frame count (markers drawn, nodes in view). Stored like a
/// duration of `n` milliseconds, so `report` prints it as a plain number in
/// the min / med / p95 / max / mean columns.
pub fn count(name: &'static str, n: usize) {
    if enabled() {
        record(name, Duration::from_millis(n.min(u32::MAX as usize / 1000) as u64));
    }
}

/// Record the time since the previous call with the same `name` (the first
/// call only arms the timer). Measures the interval between frames.
pub fn interval(name: &'static str) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let prev = {
        let mut last = LAST.lock().unwrap();
        match last.iter_mut().find(|(n, _)| *n == name) {
            Some((_, t)) => Some(std::mem::replace(t, now)),
            None => {
                last.push((name, now));
                None
            }
        }
    };
    if let Some(prev) = prev {
        record(name, now.duration_since(prev));
    }
}

/// Summary of one phase, in milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub median: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

/// Summarise microsecond samples; `None` when empty.
pub fn summarize(samples: &[u32]) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let ms = |us: u32| us as f64 / 1000.0;
    let pick = |q: f64| ms(s[(((s.len() - 1) as f64) * q).round() as usize]);
    Some(Summary {
        n: s.len(),
        min: ms(s[0]),
        median: pick(0.5),
        p95: pick(0.95),
        max: ms(s[s.len() - 1]),
        mean: s.iter().map(|&u| u as f64).sum::<f64>() / s.len() as f64 / 1000.0,
    })
}

/// Print every phase collected since the last report as
/// `FRAMESTATS <label> <phase> n= min= med= p95= max= mean=` (ms), then clear.
/// Does nothing when collection is off.
pub fn report(label: &str) {
    if !enabled() {
        return;
    }
    LAST.lock().unwrap().clear();
    let taken = std::mem::take(&mut *SAMPLES.lock().unwrap());
    let mut rows: Vec<_> = taken
        .into_iter()
        .filter_map(|(name, v)| summarize(&v).map(|s| (name, s)))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    for (name, s) in rows {
        println!(
            "FRAMESTATS {label} {name} n={} min={:.3} med={:.3} p95={:.3} max={:.3} mean={:.3}",
            s.n, s.min, s.median, s.p95, s.max, s.mean
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_empty_is_none() {
        assert_eq!(summarize(&[]), None);
    }

    #[test]
    fn summarize_orders_and_picks_quantiles() {
        let samples: Vec<u32> = (1..=100).map(|i| i * 1000).rev().collect();
        let s = summarize(&samples).unwrap();
        assert_eq!(s.n, 100);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 100.0);
        assert!((s.median - 50.0).abs() <= 1.0);
        assert!((s.p95 - 95.0).abs() <= 1.0);
        assert!((s.mean - 50.5).abs() < 1e-9);
    }

    #[test]
    fn disabled_timer_costs_nothing() {
        // The env var is not set in tests, so nothing is recorded.
        if !enabled() {
            assert!(start().is_none());
            lap("test", None);
        }
    }
}
