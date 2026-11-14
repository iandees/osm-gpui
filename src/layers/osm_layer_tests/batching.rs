//! Draw-batch ordering and z-keys.

use super::super::{batch_index, finish_sorted, z_key, Batch, BatchKind};
use gpui::{point, px};
use std::collections::{HashMap, HashSet};

const KINDS: [BatchKind; 4] = [
    BatchKind::Fill,
    BatchKind::Overlay,
    BatchKind::Casing,
    BatchKind::Stroke,
];

fn with_triangle(mut b: Batch) -> Batch {
    super::super::push_triangle(
        &mut b.path,
        &mut b.bounds,
        point(px(0.0), px(0.0)),
        point(px(1.0), px(0.0)),
        point(px(0.0), px(1.0)),
    );
    b
}

#[test]
fn z_key_orders_fractional_and_negative_indices() {
    let zs = [-2.0f32, -0.5, -0.125, 0.0, 0.125, 0.5, 1.0, 1.5, 10.0];
    let keys: Vec<i32> = zs.iter().map(|&z| z_key(z)).collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "{keys:?}");
    assert_eq!(z_key(0.5), 4);
    assert_eq!(z_key(-0.5), -4);
    assert_eq!(z_key(0.0), 0);
}

#[test]
fn z_key_clamps_extremes() {
    assert_eq!(z_key(f32::MAX), z_key(1.0e6));
    assert_eq!(z_key(f32::MIN), z_key(-1.0e6));
    assert_eq!(z_key(f32::INFINITY), z_key(1.0e6));
    assert_eq!(z_key(f32::NEG_INFINITY), z_key(-1.0e6));
}

#[test]
fn batch_keys_do_not_collide() {
    let hints = HashMap::new();
    let colours = [
        0x0000_0000u32,
        0x0000_00ff,
        0x0000_ff00,
        0x00ff_0000,
        0xff00_0000,
        0xffff_ffff,
        0x1234_5678,
        0x1234_56ff,
        0x1234_5600,
    ];
    let zs = [-8_000_000, -1000, -8, -1, 0, 1, 8, 1000, 8_000_000];
    let mut seen = HashSet::new();
    let mut n = 0;
    for kind in KINDS {
        for &rgba in &colours {
            for &z in &zs {
                seen.insert(Batch::new(kind, rgba, z, &hints).key);
                n += 1;
            }
        }
    }
    assert_eq!(seen.len(), n);
}

#[test]
fn batch_index_reuses_by_z_and_colour_only() {
    let hints = HashMap::new();
    let mut index = HashMap::new();
    let mut batches = Vec::new();
    let mut idx =
        |z, rgba| batch_index(&mut index, &mut batches, BatchKind::Stroke, z, rgba, &hints);
    let a = idx(0, 0xff);
    assert_eq!(idx(0, 0xff), a);
    assert_ne!(idx(1, 0xff), a);
    assert_ne!(idx(0, 0xfe), a);
    assert_eq!(batches.len(), 3);
}

#[test]
fn finish_sorted_orders_by_z_then_first_seen() {
    let hints = HashMap::new();
    let mut index = HashMap::new();
    let mut batches = Vec::new();
    // First-seen order: (z 8, A), (z -4, B), (z 8, C), (z -4, D).
    for (z, rgba) in [(8, 0xA), (-4, 0xB), (8, 0xC), (-4, 0xD)] {
        batch_index(&mut index, &mut batches, BatchKind::Fill, z, rgba, &hints);
    }
    let batches: Vec<Batch> = batches.into_iter().map(with_triangle).collect();
    let mut next = HashMap::new();
    let out = finish_sorted(batches, &mut next);
    let order: Vec<u32> = out.iter().map(|(c, _)| *c).collect();
    assert_eq!(order, [0xB, 0xD, 0xA, 0xC]);
    assert_eq!(next.len(), 4);
}

#[test]
fn finish_sorted_drops_empty_batches() {
    let hints = HashMap::new();
    let empty = Batch::new(BatchKind::Fill, 1, 0, &hints);
    let full = with_triangle(Batch::new(BatchKind::Fill, 2, 0, &hints));
    let mut next = HashMap::new();
    let out = finish_sorted(vec![empty, full], &mut next);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, 2);
}

#[test]
fn a_way_bbox_intersects_the_view_when_it_overlaps_or_touches() {
    use super::super::WayBbox;
    let b = WayBbox {
        min_x: 0.0,
        max_x: 10.0,
        min_y: 0.0,
        max_y: 10.0,
    };
    assert!(b.intersects((5.0, 6.0, 5.0, 6.0)));
    assert!(b.intersects((10.0, 20.0, 10.0, 20.0)));
    assert!(!b.intersects((10.5, 20.0, 0.0, 10.0)));
    assert!(!b.intersects((0.0, 10.0, -5.0, -0.5)));
    let flat = WayBbox {
        max_x: 0.0,
        max_y: 0.0,
        ..b
    };
    assert!(flat.intersects((-1.0, 0.0, -1.0, 0.0)));
}
