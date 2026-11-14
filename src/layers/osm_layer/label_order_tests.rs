//! Pins the order in which labels of every kind are offered to the collision
//! grid and which one wins a contested spot. Only `Feat::item` and
//! `Feat::score` know how a priority is built; the expectations are literals.

use super::label_pass::highway_label_rank;
use crate::layers::area_labels::{area_priority, size_bonus};
use crate::layers::labels::{
    node_priority, place, Anchor, Config, Item, Prev, HYSTERESIS_BONUS, KIND_NODE, KIND_WAY,
};
use std::collections::HashMap;

fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[derive(Clone)]
enum Class {
    Node {
        icon: bool,
        named: bool,
    },
    /// Longer side of the bounding box in pixels.
    Area(f32),
    Street,
}

#[derive(Clone)]
struct Feat {
    name: &'static str,
    id: i64,
    class: Class,
    tags: HashMap<String, String>,
    forced: bool,
}

fn feat(name: &'static str, id: i64, class: Class, t: &[(&str, &str)]) -> Feat {
    Feat {
        name,
        id,
        class,
        tags: tags(t),
        forced: false,
    }
}

impl Feat {
    fn key(&self) -> (u8, i64) {
        match self.class {
            Class::Node { .. } => (KIND_NODE, self.id),
            _ => (KIND_WAY, self.id),
        }
    }

    /// The priority without hysteresis.
    fn score(&self) -> i32 {
        match self.class {
            Class::Node { icon, named } => node_priority(&self.tags, icon, named),
            Class::Area(px) => area_priority(&self.tags) + size_bonus(px),
            Class::Street => highway_label_rank(&self.tags),
        }
    }

    fn item(&self, x: f32, y: f32) -> Item {
        Item {
            key: self.key(),
            center: (x, y),
            marker_r: 0.0,
            icon_r: None,
            text: Some((60.0, 12.0, Anchor::Center)),
            priority: self.score(),
            forced: self.forced,
        }
    }
}

fn cfg() -> Config {
    Config::for_view(400.0, 300.0)
}

/// The order `place` considers items in: forced, then score plus the bonus
/// for a previously placed one, then key.
fn offered_order(feats: &[Feat], prev: &[(u8, i64)]) -> Vec<&'static str> {
    let mut v: Vec<&Feat> = feats.iter().collect();
    v.sort_by(|a, b| {
        let eff = |f: &Feat| {
            f.score()
                + if prev.contains(&f.key()) {
                    HYSTERESIS_BONUS
                } else {
                    0
                }
        };
        b.forced
            .cmp(&a.forced)
            .then(eff(b).cmp(&eff(a)))
            .then(a.key().cmp(&b.key()))
    });
    v.iter().map(|f| f.name).collect()
}

fn node(name: &'static str, id: i64, icon: bool, named: bool, t: &[(&str, &str)]) -> Feat {
    feat(name, id, Class::Node { icon, named }, t)
}

fn area(name: &'static str, id: i64, px: f32, t: &[(&str, &str)]) -> Feat {
    feat(name, id, Class::Area(px), t)
}

fn street(name: &'static str, id: i64, t: &[(&str, &str)]) -> Feat {
    feat(name, id, Class::Street, t)
}

fn point_and_area_features() -> Vec<Feat> {
    vec![
        node("city", 1, false, true, &[("place", "city")]),
        node("town", 2, false, true, &[("place", "town")]),
        node("village", 3, false, true, &[("place", "village")]),
        node("suburb", 4, false, true, &[("place", "suburb")]),
        node("hamlet", 5, false, true, &[("place", "hamlet")]),
        node("island", 6, false, true, &[("place", "island")]),
        node("cafe", 7, true, true, &[("amenity", "cafe")]),
        node("unnamed-icon", 8, true, false, &[("amenity", "bench")]),
        node("named", 9, false, true, &[("man_made", "x")]),
        node(
            "housenumber",
            10,
            false,
            false,
            &[("addr:housenumber", "1")],
        ),
        area("big-water", 20, 800.0, &[("natural", "water")]),
        area("small-water", 21, 40.0, &[("natural", "water")]),
        area("big-park", 22, 600.0, &[("leisure", "park")]),
        area("small-park", 23, 100.0, &[("leisure", "park")]),
        area("big-building", 24, 10_000.0, &[("building", "yes")]),
        area("small-building", 25, 20.0, &[("building", "yes")]),
        area("retail", 26, 400.0, &[("landuse", "retail")]),
        area("park-tie", 27, 0.0, &[("leisure", "park")]),
        area("square", 28, 0.0, &[("place", "square")]),
    ]
}

#[test]
fn scores_of_each_kind_are_pinned() {
    let got: Vec<(&str, i32)> = point_and_area_features()
        .iter()
        .map(|f| (f.name, f.score()))
        .collect();
    assert_eq!(
        got,
        [
            ("city", 1000),
            ("town", 950),
            ("village", 900),
            ("suburb", 850),
            ("hamlet", 800),
            ("island", 700),
            ("cafe", 500),
            ("unnamed-icon", 500),
            ("named", 400),
            ("housenumber", 100),
            ("big-water", 350),
            ("small-water", 322),
            ("big-park", 330),
            ("small-park", 305),
            ("big-building", 280),
            ("small-building", 251),
            ("retail", 300),
            ("park-tie", 300),
            ("square", 340),
        ]
    );
    let streets = [
        (
            "motorway",
            street("a", 1, &[("highway", "motorway")]).score(),
        ),
        ("trunk", street("a", 1, &[("highway", "trunk")]).score()),
        ("primary", street("a", 1, &[("highway", "primary")]).score()),
        (
            "secondary",
            street("a", 1, &[("highway", "secondary")]).score(),
        ),
        (
            "tertiary",
            street("a", 1, &[("highway", "tertiary")]).score(),
        ),
        (
            "residential",
            street("a", 1, &[("highway", "residential")]).score(),
        ),
        (
            "unclassified",
            street("a", 1, &[("highway", "unclassified")]).score(),
        ),
        ("service", street("a", 1, &[("highway", "service")]).score()),
        ("track", street("a", 1, &[("highway", "track")]).score()),
        ("untagged", street("a", 1, &[("name", "x")]).score()),
    ];
    assert_eq!(
        streets.map(|s| s.1),
        [90, 90, 80, 70, 60, 50, 50, 40, 30, 20]
    );
}

#[test]
fn offered_order_across_kinds_is_pinned() {
    let feats = point_and_area_features();
    assert_eq!(
        offered_order(&feats, &[]),
        [
            "city",
            "town",
            "village",
            "suburb",
            "hamlet",
            "island",
            "cafe",
            "unnamed-icon",
            "named",
            "big-water",
            "square",
            "big-park",
            "small-water",
            "small-park",
            // equal score 300: the way keys 26 < 27 decide
            "retail",
            "park-tie",
            "big-building",
            "small-building",
            "housenumber",
        ]
    );
}

#[test]
fn offered_order_with_hysteresis_and_a_selection_is_pinned() {
    let mut feats = point_and_area_features();
    feats[9].forced = true; // the selected house number
    let prev = [
        (KIND_WAY, 28),  // square: 340 -> 380
        (KIND_WAY, 20),  // big water: 350 -> 390
        (KIND_NODE, 10), // house number: 100 -> 140 (forced anyway)
        (KIND_WAY, 25),  // small building: 251 -> 291
        (KIND_WAY, 27),  // park-tie: 300 -> 340
    ];
    assert_eq!(
        offered_order(&feats, &prev),
        [
            "housenumber",
            "city",
            "town",
            "village",
            "suburb",
            "hamlet",
            "island",
            "cafe",
            "unnamed-icon",
            "named",
            "big-water",
            "square",
            "park-tie",
            "big-park",
            "small-water",
            "small-park",
            "retail",
            "small-building",
            "big-building",
        ]
    );
}

/// The keys `place` leaves with text, for items stacked on one spot.
fn winners(feats: &[Feat], prev: &[(u8, i64)]) -> Vec<(u8, i64)> {
    let items: Vec<Item> = feats.iter().map(|f| f.item(200.0, 150.0)).collect();
    let prev: HashMap<_, _> = prev
        .iter()
        .map(|k| {
            (
                *k,
                Prev {
                    icon: false,
                    anchor: Some(Anchor::Center),
                },
            )
        })
        .collect();
    let mut won: Vec<_> = place(&items, &[], &prev, &cfg())
        .into_iter()
        .filter(|p| p.text.is_some())
        .map(|p| items[p.item].key)
        .collect();
    won.sort();
    won
}

#[test]
fn contested_spot_winners_across_kinds_are_pinned() {
    let city = node("city", 1, false, true, &[("place", "city")]);
    let named = node("named", 9, false, true, &[("man_made", "x")]);
    let num = node("num", 10, false, false, &[("addr:housenumber", "1")]);
    let water = area("water", 20, 800.0, &[("natural", "water")]);
    let building = area("bld", 24, 10_000.0, &[("building", "yes")]);
    let square = area("square", 28, 400.0, &[("place", "square")]);

    // A place beats any area; a named node beats the biggest water.
    assert_eq!(
        winners(&[water.clone(), city.clone()], &[]),
        [(KIND_NODE, 1)]
    );
    assert_eq!(
        winners(&[water.clone(), named.clone()], &[]),
        [(KIND_NODE, 9)]
    );
    // An area beats a house number.
    assert_eq!(
        winners(&[num.clone(), building.clone()], &[]),
        [(KIND_WAY, 24)]
    );
    // Hysteresis: the previous winner is worth 40, enough to reorder
    // neighbouring tiers (square 340+20=360, named 400).
    assert_eq!(
        winners(&[square.clone(), named.clone()], &[]),
        [(KIND_NODE, 9)]
    );
    assert_eq!(
        winners(&[square.clone(), named.clone()], &[(KIND_WAY, 28)]),
        [(KIND_NODE, 9)],
        "360 + 40 ties 400: the node key sorts first"
    );
    assert_eq!(
        winners(&[named.clone(), square.clone()], &[(KIND_WAY, 28)]),
        [(KIND_NODE, 9)],
        "input order does not matter"
    );
    let big_square = area("square", 28, 600.0, &[("place", "square")]); // 370
    assert_eq!(
        winners(&[named.clone(), big_square.clone()], &[(KIND_WAY, 28)]),
        [(KIND_WAY, 28)],
        "370 + 40 beats 400"
    );
    assert_eq!(
        winners(&[named.clone(), big_square.clone()], &[(KIND_NODE, 9)]),
        [(KIND_NODE, 9)]
    );
    // Hysteresis does not lift a house number over an area (100 + 40 < 251).
    assert_eq!(
        winners(&[num.clone(), building.clone()], &[(KIND_NODE, 10)]),
        [(KIND_WAY, 24)]
    );
    // Equal scores: the lower key (a node) wins, whatever the input order.
    let tie = area("tie", 30, 400.0, &[("place", "square")]); // 360
    let tie_node = node("tie", 31, false, true, &[("man_made", "x")]); // 400
    assert_eq!(
        winners(&[tie.clone(), tie_node.clone()], &[(KIND_WAY, 30)]),
        [(KIND_NODE, 31)]
    );
    let way_a = area("a", 40, 400.0, &[("leisure", "park")]);
    let way_b = area("b", 41, 400.0, &[("leisure", "park")]);
    assert_eq!(
        winners(&[way_b.clone(), way_a.clone()], &[]),
        [(KIND_WAY, 40)]
    );
    // A selection wins over everything and is never dropped.
    let mut sel = num.clone();
    sel.forced = true;
    assert_eq!(winners(&[city.clone(), sel], &[]), [(KIND_NODE, 10)]);
    let mut sel_b = building.clone();
    sel_b.forced = true;
    assert!(winners(&[city, sel_b], &[]).contains(&(KIND_WAY, 24)));
}
