//! What the preset picker offers before the user types a search: the presets
//! they applied recently, then a short list of common presets per geometry.
//!
//! The logic here is pure; where the recents are kept is injected through
//! [`RecentsStore`] (the app uses [`SettingsRecents`], tests an in-memory
//! store), so tests never touch the real settings file.

use crate::presets::{Geometry, Preset, PresetIndex};

/// How many recently applied presets are remembered.
pub const MAX_RECENTS: usize = 8;

/// Where the recently applied preset ids are persisted.
pub trait RecentsStore {
    /// Most recent first.
    fn load(&self) -> Vec<String>;
    fn save(&mut self, ids: &[String]);
}

/// Recents kept in the app's `settings.json` (`recent_presets`).
pub struct SettingsRecents;

impl RecentsStore for SettingsRecents {
    fn load(&self) -> Vec<String> {
        crate::settings_store::snapshot().recent_presets
    }

    fn save(&mut self, ids: &[String]) {
        let mut settings = crate::settings_store::snapshot();
        settings.recent_presets = ids.to_vec();
        crate::settings_store::update_store(settings);
    }
}

/// Move `id` to the front of `recents`, dropping any earlier occurrence and
/// anything past [`MAX_RECENTS`].
pub fn push_recent(recents: &mut Vec<String>, id: &str) {
    recents.retain(|r| r != id);
    recents.insert(0, id.to_string());
    recents.truncate(MAX_RECENTS);
}

/// Remember that the preset `id` was just applied.
pub fn record(store: &mut dyn RecentsStore, id: &str) {
    let mut recents = store.load();
    push_recent(&mut recents, id);
    store.save(&recents);
}

/// Common presets for a geometry, in display order. The iD tagging schema
/// has its own `preset_defaults`, but it is not part of the vendored data, so
/// this is a short hand-picked list; every id must exist in `presets.json`
/// and support the geometry (checked by a test).
pub fn common_preset_ids(geometry: Geometry) -> &'static [&'static str] {
    match geometry {
        Geometry::Point => &[
            "natural/tree",
            "amenity/bench",
            "amenity/waste_basket",
            "amenity/restaurant",
            "amenity/cafe",
            "amenity/fast_food",
            "shop",
            "shop/convenience",
            "amenity/parking",
            "amenity/place_of_worship",
            "amenity/post_box",
            "amenity/drinking_water",
        ],
        Geometry::Vertex => &[
            "highway/crossing",
            "highway/traffic_signals",
            "highway/stop",
            "highway/give_way",
            "barrier/gate",
            "barrier/bollard",
            "entrance",
            "highway/turning_circle",
            "highway/street_lamp",
            "natural/tree",
        ],
        Geometry::Line => &[
            "highway/residential",
            "highway/service",
            "highway/footway",
            "highway/path",
            "highway/cycleway",
            "highway/track",
            "highway/unclassified",
            "highway/tertiary",
            "barrier/fence",
            "barrier/wall",
            "waterway/stream",
            "waterway/river",
            "railway/rail",
        ],
        Geometry::Area => &[
            "building",
            "building/house",
            "building/garage",
            "building/shed",
            "building/apartments",
            "leisure/park",
            "amenity/parking",
            "landuse/residential",
            "landuse/grass",
            "landuse/forest",
            "natural/water",
            "leisure/playground",
            "leisure/pitch",
            "amenity/restaurant",
            "shop",
        ],
        Geometry::Relation => &["type/multipolygon", "type/route", "type/boundary"],
    }
}

/// Which group of the default list a preset belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Recent,
    Common,
}

/// The picker's list for an empty search: recents valid for any of
/// `geometries`, then each geometry's common presets (best geometry first),
/// without repeats.
pub fn default_list<'a>(
    index: &'a PresetIndex,
    recents: &[String],
    geometries: &[Geometry],
) -> Vec<(Section, &'a Preset)> {
    let mut out: Vec<(Section, &Preset)> = Vec::new();
    for id in recents {
        let Some(preset) = index.get(id) else {
            continue;
        };
        if preset.geometry.iter().any(|g| geometries.contains(g))
            && !out.iter().any(|(_, p)| p.id == preset.id)
        {
            out.push((Section::Recent, preset));
        }
    }
    for geometry in geometries {
        for id in common_preset_ids(*geometry) {
            let Some(preset) = index.get(id) else {
                continue;
            };
            if preset.geometry.contains(geometry) && !out.iter().any(|(_, p)| p.id == preset.id) {
                out.push((Section::Common, preset));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemStore(Vec<String>);

    impl RecentsStore for MemStore {
        fn load(&self) -> Vec<String> {
            self.0.clone()
        }
        fn save(&mut self, ids: &[String]) {
            self.0 = ids.to_vec();
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn most_recent_first_and_deduped() {
        let mut r = Vec::new();
        push_recent(&mut r, "a");
        push_recent(&mut r, "b");
        push_recent(&mut r, "a");
        assert_eq!(r, ids(&["a", "b"]));
    }

    #[test]
    fn capped() {
        let mut r = Vec::new();
        for i in 0..20 {
            push_recent(&mut r, &format!("p{i}"));
        }
        assert_eq!(r.len(), MAX_RECENTS);
        assert_eq!(r[0], "p19");
    }

    #[test]
    fn round_trip_through_injected_store() {
        let mut store = MemStore(Vec::new());
        record(&mut store, "building");
        record(&mut store, "natural/tree");
        record(&mut store, "building");
        assert_eq!(store.load(), ids(&["building", "natural/tree"]));
    }

    #[test]
    fn recents_filtered_by_geometry_and_unknown_ids_dropped() {
        let index = crate::presets::preset_index();
        let recents = ids(&[
            "highway/residential",
            "gone/away",
            "building",
            "natural/tree",
        ]);
        let list = default_list(index, &recents, &[Geometry::Area]);
        let recent: Vec<&str> = list
            .iter()
            .filter(|(s, _)| *s == Section::Recent)
            .map(|(_, p)| p.id.as_str())
            .collect();
        assert_eq!(recent, vec!["building"]);
        let list = default_list(index, &recents, &[Geometry::Point]);
        let recent: Vec<&str> = list
            .iter()
            .filter(|(s, _)| *s == Section::Recent)
            .map(|(_, p)| p.id.as_str())
            .collect();
        assert_eq!(recent, vec!["natural/tree"]);
    }

    #[test]
    fn recents_come_first_and_are_not_repeated_in_common() {
        let index = crate::presets::preset_index();
        let list = default_list(index, &ids(&["leisure/park"]), &[Geometry::Area]);
        assert_eq!(list[0].0, Section::Recent);
        assert_eq!(list[0].1.id, "leisure/park");
        assert_eq!(
            list.iter().filter(|(_, p)| p.id == "leisure/park").count(),
            1
        );
        assert!(list.iter().skip(1).all(|(s, _)| *s == Section::Common));
    }

    #[test]
    fn closed_way_gets_area_then_line_defaults() {
        let index = crate::presets::preset_index();
        let list = default_list(index, &[], &[Geometry::Area, Geometry::Line]);
        let first_line = list
            .iter()
            .position(|(_, p)| p.id == "highway/residential")
            .unwrap();
        let building = list.iter().position(|(_, p)| p.id == "building").unwrap();
        assert!(building < first_line);
    }

    #[test]
    fn every_common_id_resolves_for_its_geometry() {
        let index = crate::presets::preset_index();
        for g in [
            Geometry::Point,
            Geometry::Vertex,
            Geometry::Line,
            Geometry::Area,
            Geometry::Relation,
        ] {
            for id in common_preset_ids(g) {
                let p = index
                    .get(id)
                    .unwrap_or_else(|| panic!("unknown preset id {id}"));
                assert!(p.geometry.contains(&g), "{id} does not support {g:?}");
            }
        }
    }
}
