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
        let prev = self.pending_tile_fetches.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "tile_fetch_finished underflow");
    }

    pub fn image_decode_started(&self) {
        self.pending_image_decodes.fetch_add(1, Ordering::SeqCst);
    }

    pub fn image_decode_finished(&self) {
        let prev = self.pending_image_decodes.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "image_decode_finished underflow");
    }

    pub fn is_idle(&self) -> bool {
        self.pending_tile_fetches.load(Ordering::SeqCst) == 0
            && self.pending_image_decodes.load(Ordering::SeqCst) == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
