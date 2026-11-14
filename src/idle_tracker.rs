//! Tracks outstanding async work so `wait_idle` can know when the map has settled.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct IdleTracker {
    pending_tile_fetches: AtomicUsize,
    pending_image_decodes: AtomicUsize,
}

impl IdleTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn tile_fetch_started(&self) {
        self.pending_tile_fetches.fetch_add(1, Ordering::SeqCst);
    }

    pub fn tile_fetch_finished(&self) {
        saturating_decrement(&self.pending_tile_fetches, "tile_fetch_finished");
    }

    pub fn image_decode_started(&self) {
        self.pending_image_decodes.fetch_add(1, Ordering::SeqCst);
    }

    pub fn image_decode_finished(&self) {
        saturating_decrement(&self.pending_image_decodes, "image_decode_finished");
    }

    pub fn is_idle(&self) -> bool {
        self.pending_tile_fetches.load(Ordering::SeqCst) == 0
            && self.pending_image_decodes.load(Ordering::SeqCst) == 0
    }
}

/// Decrement without ever wrapping below zero: a wrapped counter would make
/// `is_idle` false forever and hang `wait_idle`. Returns false if it was
/// already zero (the decrement is skipped).
fn try_decrement(counter: &AtomicUsize) -> bool {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        if current == 0 {
            return false;
        }
        match counter.compare_exchange_weak(
            current,
            current - 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

fn saturating_decrement(counter: &AtomicUsize, what: &str) {
    if !try_decrement(counter) {
        debug_assert!(false, "{what} underflow");
        eprintln!("IdleTracker: {what} called with nothing pending; ignoring");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_decrement_never_goes_below_zero() {
        let c = AtomicUsize::new(1);
        assert!(try_decrement(&c));
        assert!(!try_decrement(&c));
        assert!(!try_decrement(&c));
        assert_eq!(c.load(Ordering::SeqCst), 0);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn unbalanced_finish_leaves_tracker_idle() {
        let t = IdleTracker::new();
        t.tile_fetch_finished();
        t.image_decode_finished();
        assert!(t.is_idle());
        t.tile_fetch_started();
        assert!(!t.is_idle());
    }

    #[test]
    fn new_tracker_is_idle() {
        assert!(IdleTracker::new().is_idle());
    }

    #[test]
    fn tile_fetch_toggles_idle() {
        let t = IdleTracker::new();
        t.tile_fetch_started();
        assert!(!t.is_idle());
        t.tile_fetch_finished();
        assert!(t.is_idle());
    }

    #[test]
    fn image_decode_toggles_idle() {
        let t = IdleTracker::new();
        t.image_decode_started();
        assert!(!t.is_idle());
        t.image_decode_finished();
        assert!(t.is_idle());
    }

    #[test]
    fn both_counters_must_be_zero() {
        let t = IdleTracker::new();
        t.tile_fetch_started();
        t.image_decode_started();
        t.tile_fetch_finished();
        assert!(!t.is_idle());
        t.image_decode_finished();
        assert!(t.is_idle());
    }

    #[test]
    fn concurrent_increments_balance() {
        use std::thread;
        let t = IdleTracker::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let t = t.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    t.tile_fetch_started();
                    t.tile_fetch_finished();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(t.is_idle());
    }
}
