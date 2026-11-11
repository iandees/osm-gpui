//! Measurements of the current selection for the side panel: the length of
//! a way, the area and perimeter of a closed one, the distance between two
//! nodes, a node's coordinates, the area of a multipolygon.
//!
//! Method and accuracy:
//!
//! - Lengths are haversine (great-circle) distances on a sphere of radius
//!   6 371 008.8 m, summed over a way's segments. Against the WGS 84
//!   ellipsoid the error is up to about 0.3 % (it depends on latitude and
//!   direction), the same for a building and for a city-sized way.
//! - Areas use the Lambert cylindrical equal-area projection of the same
//!   sphere (x = R * delta-longitude, y = R * sin(latitude)), which preserves
//!   area exactly, and the shoelace formula on the projected ring. Polygon
//!   edges are treated as straight lines in that projection (a constant
//!   latitude edge is exact; a slanted edge differs from the geodesic by a
//!   sliver far below 0.01 % for a building and below about 0.1 % for a
//!   city-sized ring). The sphere-versus-ellipsoid difference (up to about
//!   0.7 % in area) is the larger error and is not corrected.
//! - Nothing here is meant for surveying: it is for judging sizes while
//!   mapping.

use std::collections::HashMap;

use crate::geometry::LatLon;
use crate::layers::EditableLayer;
use crate::multipolygon;
use crate::osm::MemberType;
use crate::selection::{FeatureKind, FeatureRef};

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// A selection of more than this many features is not measured.
pub const MAX_FEATURES: usize = 1000;

/// A selection whose ways have more than this many nodes in total is not
/// measured.
pub const MAX_NODES: usize = 50_000;

/// Great-circle distance in metres between two (lat, lon) points in degrees.
pub fn haversine_m(a: LatLon, b: LatLon) -> f64 {
    let (lat1, lat2) = (a.0.to_radians(), b.0.to_radians());
    let dlat = lat2 - lat1;
    let dlon = (b.1 - a.1).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_M * h.sqrt().min(1.0).asin()
}

/// Length in metres of the path through `points`.
pub fn path_length_m(points: &[LatLon]) -> f64 {
    points.windows(2).map(|w| haversine_m(w[0], w[1])).sum()
}

/// Longitude difference in degrees, wrapped to [-180, 180].
fn wrap_lon(d: f64) -> f64 {
    let d = (d + 180.0).rem_euclid(360.0) - 180.0;
    if d == -180.0 && d != 0.0 {
        180.0
    } else {
        d
    }
}

/// Position in metres in the equal-area plane around longitude `lon0`.
fn equal_area_xy(lon0: f64, p: LatLon) -> (f64, f64) {
    (
        EARTH_RADIUS_M * wrap_lon(p.1 - lon0).to_radians(),
        EARTH_RADIUS_M * p.0.to_radians().sin(),
    )
}

/// Area in square metres enclosed by `points` (a ring; the closing duplicate
/// is optional), whichever way it winds. Zero for fewer than three points.
pub fn ring_area_m2(points: &[LatLon]) -> f64 {
    let ring = match points {
        [first, .., last] if first == last => &points[..points.len() - 1],
        _ => points,
    };
    if ring.len() < 3 {
        return 0.0;
    }
    let lon0 = ring[0].1;
    let xy: Vec<(f64, f64)> = ring.iter().map(|&p| equal_area_xy(lon0, p)).collect();
    multipolygon::ring_area(&xy)
}

/// What the side panel shows for a selection.
#[derive(Debug, Clone, PartialEq)]
pub enum Measurement {
    /// One node.
    Point { lat: f64, lon: f64 },
    /// Two nodes.
    Distance { metres: f64 },
    /// One or more ways, none of them a single closed way: total length.
    Length { ways: usize, metres: f64 },
    /// A single closed way.
    Ring { area_m2: f64, perimeter_m: f64 },
    /// A multipolygon relation: outers minus inners.
    Area { area_m2: f64 },
    /// The selection is too large to measure.
    TooMany,
}

/// Measure the selection `selection` of the layer `layer_id` as held by
/// `editable`. `overrides` gives nodes' positions in place of the layer's
/// (the nodes being dragged, so the measurement follows the drag).
///
/// `None` when there is nothing to say: an empty selection, a mix of kinds,
/// features of several layers, a relation that is not an area or cannot be
/// assembled, or a way with an unresolvable node.
pub fn measure_selection(
    editable: &dyn EditableLayer,
    layer_id: crate::layers::LayerId,
    selection: &[FeatureRef],
    overrides: &HashMap<i64, LatLon>,
) -> Option<Measurement> {
    if selection.is_empty() || selection.iter().any(|f| f.layer_id != layer_id) {
        return None;
    }
    if selection.len() > MAX_FEATURES {
        return Some(Measurement::TooMany);
    }
    let position = |id: i64| {
        overrides
            .get(&id)
            .copied()
            .or_else(|| editable.node_lat_lon(id))
    };
    let kind = selection[0].kind;
    if selection.iter().any(|f| f.kind != kind) {
        return None;
    }
    match kind {
        FeatureKind::Node => match selection {
            [a] => {
                let (lat, lon) = position(a.id)?;
                Some(Measurement::Point { lat, lon })
            }
            [a, b] => Some(Measurement::Distance {
                metres: haversine_m(position(a.id)?, position(b.id)?),
            }),
            _ => None,
        },
        FeatureKind::Way => {
            let mut paths: Vec<Vec<LatLon>> = Vec::with_capacity(selection.len());
            let mut nodes = 0usize;
            for f in selection {
                let ids = editable.way_node_ids(f.id)?;
                nodes += ids.len();
                if nodes > MAX_NODES {
                    return Some(Measurement::TooMany);
                }
                paths.push(ids.iter().map(|&id| position(id)).collect::<Option<_>>()?);
            }
            if let [path] = paths.as_slice() {
                if crate::geometry::is_closed_ring(path) {
                    return Some(Measurement::Ring {
                        area_m2: ring_area_m2(path),
                        perimeter_m: path_length_m(path),
                    });
                }
            }
            Some(Measurement::Length {
                ways: paths.len(),
                metres: paths.iter().map(|p| path_length_m(p)).sum(),
            })
        }
        FeatureKind::Relation => {
            let [feature] = selection else {
                return None;
            };
            let relation = editable.relation(feature.id)?;
            if !multipolygon::is_area_relation(&relation.tags) {
                return None;
            }
            let mut way_nodes: Vec<(i64, String, Option<Vec<i64>>)> = Vec::new();
            let mut nodes = 0usize;
            for m in relation
                .members
                .iter()
                .filter(|m| m.member_type == MemberType::Way)
            {
                let ids = editable.way_node_ids(m.reference);
                nodes += ids.as_ref().map_or(0, Vec::len);
                if nodes > MAX_NODES {
                    return Some(Measurement::TooMany);
                }
                way_nodes.push((m.reference, m.role.clone(), ids));
            }
            let lon0 = way_nodes
                .iter()
                .filter_map(|(_, _, ids)| ids.as_ref()?.first().copied())
                .find_map(&position)?
                .1;
            let mut coords: HashMap<i64, (f64, f64)> = HashMap::new();
            for id in way_nodes
                .iter()
                .filter_map(|(_, _, ids)| ids.as_ref())
                .flatten()
            {
                if let Some(p) = position(*id) {
                    coords.insert(*id, equal_area_xy(lon0, p));
                }
            }
            let members: Vec<multipolygon::WayMember> = way_nodes
                .iter()
                .map(|(way_id, role, ids)| multipolygon::WayMember {
                    way_id: *way_id,
                    role,
                    nodes: ids.as_deref(),
                })
                .collect();
            let assembled = multipolygon::assemble(&members, &|id| coords.get(&id).copied());
            assembled
                .ok()
                .map(|mp| Measurement::Area { area_m2: mp.area() })
        }
    }
}

/// A length in metres: one decimal below 100 m, whole metres up to a
/// kilometre, then kilometres with two decimals.
pub fn format_length(metres: f64) -> String {
    if metres >= 1000.0 {
        format!("{:.2} km", metres / 1000.0)
    } else if metres >= 100.0 {
        format!("{metres:.0} m")
    } else {
        format!("{metres:.1} m")
    }
}

/// An area in square metres: m² below 10 000 m², hectares from there up to
/// a square kilometre, then km².
pub fn format_area(m2: f64) -> String {
    if m2 >= 1_000_000.0 {
        format!("{:.2} km²", m2 / 1_000_000.0)
    } else if m2 >= 10_000.0 {
        format!("{:.2} ha", m2 / 10_000.0)
    } else if m2 >= 100.0 {
        format!("{m2:.0} m²")
    } else {
        format!("{m2:.1} m²")
    }
}

/// A latitude or longitude with seven decimals.
pub fn format_degrees(v: f64) -> String {
    format!("{v:.7}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square of `side` metres on the ground whose south-west corner is at
    /// `lat`: built from the exact meridian and parallel lengths on the same
    /// sphere, so its true area is `side` squared.
    fn square(lat: f64, side: f64) -> Vec<LatLon> {
        let dlat = (side / EARTH_RADIUS_M).to_degrees();
        let top = lat + dlat;
        // Rows at lat and top have different parallel lengths; use the
        // latitude whose parallel gives the mean width so the rectangle in
        // degrees has area close to side squared.
        let mid = (lat + top) / 2.0;
        let dlon = (side / (EARTH_RADIUS_M * mid.to_radians().cos())).to_degrees();
        vec![
            (lat, 8.0),
            (lat, 8.0 + dlon),
            (top, 8.0 + dlon),
            (top, 8.0),
            (lat, 8.0),
        ]
    }

    fn within(value: f64, expected: f64, fraction: f64) -> bool {
        (value - expected).abs() <= expected * fraction
    }

    #[test]
    fn hundred_metre_square_is_ten_thousand_square_metres() {
        for lat in [0.0, 45.0, 60.0, -33.0, 80.0] {
            let ring = square(lat, 100.0);
            let area = ring_area_m2(&ring);
            assert!(within(area, 10_000.0, 0.005), "lat {lat}: {area}");
            let perimeter = path_length_m(&ring);
            assert!(within(perimeter, 400.0, 0.005), "lat {lat}: {perimeter}");
            // Winding and the closing duplicate do not matter.
            let mut reversed = ring.clone();
            reversed.reverse();
            assert!((ring_area_m2(&reversed) - area).abs() < 1e-6);
            assert!((ring_area_m2(&ring[..4]) - area).abs() < 1e-6);
        }
    }

    #[test]
    fn city_sized_areas_match_the_exact_spherical_values() {
        // A 1 degree by 1 degree cell on the equator: R^2 * dlon * (sin 1 deg).
        let cell = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
        let exact = EARTH_RADIUS_M.powi(2) * 1f64.to_radians() * 1f64.to_radians().sin();
        assert!(within(ring_area_m2(&cell), exact, 1e-9));
        // A 10 km square at 50 degrees, exact for a lat/lon rectangle.
        let (lat, dlat, dlon) = (50.0_f64, 0.09_f64, 0.14_f64);
        let rect = [
            (lat, 8.0),
            (lat, 8.0 + dlon),
            (lat + dlat, 8.0 + dlon),
            (lat + dlat, 8.0),
        ];
        let exact = EARTH_RADIUS_M.powi(2)
            * dlon.to_radians()
            * ((lat + dlat).to_radians().sin() - lat.to_radians().sin());
        assert!(within(ring_area_m2(&rect), exact, 1e-9));
    }

    #[test]
    fn haversine_matches_known_distances() {
        // One degree of latitude is about 111.195 km on this sphere.
        assert!(within(
            haversine_m((0.0, 0.0), (1.0, 0.0)),
            111_195.0,
            0.001
        ));
        // One degree of longitude shrinks with the cosine of the latitude.
        assert!(within(
            haversine_m((60.0, 10.0), (60.0, 11.0)),
            55_597.0,
            0.001
        ));
        // Antimeridian and identical points.
        assert!(within(
            haversine_m((0.0, 179.5), (0.0, -179.5)),
            111_195.0,
            0.001
        ));
        assert_eq!(haversine_m((12.0, 34.0), (12.0, 34.0)), 0.0);
        assert!(haversine_m((90.0, 0.0), (-90.0, 0.0)).is_finite());
    }

    #[test]
    fn degenerate_input_is_zero_not_nan() {
        let p = (47.0, 8.0);
        assert_eq!(ring_area_m2(&[]), 0.0);
        assert_eq!(ring_area_m2(&[p, p]), 0.0);
        assert_eq!(ring_area_m2(&[p, p, p, p]), 0.0);
        assert_eq!(ring_area_m2(&[p, (47.001, 8.0), (47.002, 8.0)]), 0.0);
        assert_eq!(path_length_m(&[]), 0.0);
        assert_eq!(path_length_m(&[p]), 0.0);
    }

    #[test]
    fn area_across_the_antimeridian_is_the_same() {
        let a = square(10.0, 100.0);
        let shifted: Vec<LatLon> = a.iter().map(|&(lat, lon)| (lat, lon + 171.99)).collect();
        let crossing: Vec<LatLon> = shifted
            .iter()
            .map(|&(lat, lon)| (lat, if lon > 180.0 { lon - 360.0 } else { lon }))
            .collect();
        assert!(within(ring_area_m2(&crossing), ring_area_m2(&a), 1e-6));
    }

    fn layer_with_shapes() -> (crate::layers::osm_layer::OsmLayer, [i64; 6]) {
        use crate::layers::osm_layer::OsmLayer;
        let mut layer = OsmLayer::new(crate::layers::LayerId(1));
        // A 100 m square at 45 degrees, and a 20 m square hole inside it.
        let outer_pts = square(45.0, 100.0);
        let hole_pts: Vec<LatLon> = square(45.0, 20.0)
            .into_iter()
            .map(|(lat, lon)| (lat + 0.0002, lon + 0.0003))
            .collect();
        let ring = |pts: &[LatLon], layer: &mut OsmLayer| {
            let mut ids: Vec<i64> = pts[..pts.len() - 1]
                .iter()
                .map(|&(lat, lon)| layer.add_node(lat, lon))
                .collect();
            ids.push(ids[0]);
            layer.add_way(ids, Vec::new())
        };
        let outer = ring(&outer_pts, &mut layer);
        let hole = ring(&hole_pts, &mut layer);
        let a = layer.add_node(47.0, 8.0);
        let b = layer.add_node(47.001, 8.0);
        let line = layer.add_way(vec![a, b], Vec::new());
        (layer, [outer, hole, line, a, b, 0])
    }

    fn fref(kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: crate::layers::LayerId(1),
            kind,
            id,
        }
    }

    #[test]
    fn selection_measurements_by_shape() {
        let (mut layer, [outer, hole, line, a, b, _]) = layer_with_shapes();
        let id = crate::layers::LayerId(1);
        let none = HashMap::new();
        let measure = |layer: &crate::layers::osm_layer::OsmLayer, sel: &[FeatureRef]| {
            measure_selection(layer, id, sel, &none)
        };

        // One way, closed: area and perimeter.
        let Some(Measurement::Ring {
            area_m2,
            perimeter_m,
        }) = measure(&layer, &[fref(FeatureKind::Way, outer)])
        else {
            panic!("expected a ring");
        };
        assert!(within(area_m2, 10_000.0, 0.005), "{area_m2}");
        assert!(within(perimeter_m, 400.0, 0.005), "{perimeter_m}");
        // One open way: length (111.2 m for 0.001 degrees of latitude).
        let Some(Measurement::Length { ways: 1, metres }) =
            measure(&layer, &[fref(FeatureKind::Way, line)])
        else {
            panic!("expected a length");
        };
        assert!(within(metres, 111.2, 0.002), "{metres}");
        // Several ways: the total, closed ones by perimeter.
        let Some(Measurement::Length { ways: 2, metres }) = measure(
            &layer,
            &[fref(FeatureKind::Way, outer), fref(FeatureKind::Way, line)],
        ) else {
            panic!("expected a total");
        };
        assert!(within(metres, 400.0 + 111.2, 0.005), "{metres}");
        // One node, two nodes.
        assert_eq!(
            measure(&layer, &[fref(FeatureKind::Node, a)]),
            Some(Measurement::Point {
                lat: 47.0,
                lon: 8.0
            })
        );
        let Some(Measurement::Distance { metres }) = measure(
            &layer,
            &[fref(FeatureKind::Node, a), fref(FeatureKind::Node, b)],
        ) else {
            panic!("expected a distance");
        };
        assert!(within(metres, 111.2, 0.002));
        // Nothing, a mix, and three nodes say nothing.
        assert_eq!(measure(&layer, &[]), None);
        assert_eq!(
            measure(
                &layer,
                &[fref(FeatureKind::Node, a), fref(FeatureKind::Way, line)]
            ),
            None
        );

        // A multipolygon: outer minus the hole.
        let tags = vec![("type".to_string(), "multipolygon".to_string())];
        let member = |way: i64, role: &str| crate::osm::OsmMember {
            member_type: MemberType::Way,
            reference: way,
            role: role.to_string(),
        };
        let rel = layer
            .create_relation(tags, vec![member(outer, "outer"), member(hole, "inner")])
            .unwrap();
        let Some(Measurement::Area { area_m2 }) =
            measure(&layer, &[fref(FeatureKind::Relation, rel)])
        else {
            panic!("expected an area");
        };
        assert!(within(area_m2, 10_000.0 - 400.0, 0.006), "{area_m2}");
        // A relation that is not an area has no measurement.
        let route = layer
            .create_relation(
                vec![("type".to_string(), "route".to_string())],
                vec![member(line, "")],
            )
            .unwrap();
        assert_eq!(measure(&layer, &[fref(FeatureKind::Relation, route)]), None);
    }

    #[test]
    fn overrides_follow_a_drag_and_big_selections_are_skipped() {
        let (layer, [_, _, line, _, b, _]) = layer_with_shapes();
        let id = crate::layers::LayerId(1);
        // Dragging the far node 0.001 degrees further north doubles the way.
        let moved = HashMap::from([(b, (47.002, 8.0))]);
        let Some(Measurement::Length { metres, .. }) =
            measure_selection(&layer, id, &[fref(FeatureKind::Way, line)], &moved)
        else {
            panic!("expected a length");
        };
        assert!(within(metres, 222.4, 0.002), "{metres}");
        let many: Vec<FeatureRef> = (0..=MAX_FEATURES as i64)
            .map(|i| fref(FeatureKind::Way, i))
            .collect();
        assert_eq!(
            measure_selection(&layer, id, &many, &HashMap::new()),
            Some(Measurement::TooMany)
        );
    }

    #[test]
    fn length_formatting() {
        assert_eq!(format_length(0.0), "0.0 m");
        assert_eq!(format_length(42.34), "42.3 m");
        assert_eq!(format_length(99.96), "100.0 m");
        assert_eq!(format_length(123.4), "123 m");
        assert_eq!(format_length(999.4), "999 m");
        assert_eq!(format_length(1500.0), "1.50 km");
    }

    #[test]
    fn area_formatting() {
        assert_eq!(format_area(12.34), "12.3 m²");
        assert_eq!(format_area(2500.0), "2500 m²");
        assert_eq!(format_area(10_000.0), "1.00 ha");
        assert_eq!(format_area(250_000.0), "25.00 ha");
        assert_eq!(format_area(2_500_000.0), "2.50 km²");
        assert_eq!(format_degrees(40.71234567891), "40.7123457");
    }
}
