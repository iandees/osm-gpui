//! Writing a layer's data as a JOSM-style `.osm` file.
//!
//! The file holds the full dataset, not a diff. Pending local edits are
//! recorded the way JOSM records them, so the file can be reopened in JOSM
//! or in this app (see `OsmParser::parse_file_with_edits`) and uploaded
//! later:
//!
//! - a modified element carries `action="modify"`;
//! - a locally created element has a negative id, `action="modify"`, and
//!   no `version`;
//! - a locally deleted element is still written, with `action="delete"`
//!   and without its tags.
//!
//! Which elements those are comes from diffing the layer's server baseline
//! against its current data, the same way an upload is built.

use crate::layers::diff::diff_osm_data;
use crate::osm::{OsmData, OsmNode, OsmRelation, OsmWay};
use crate::osm_upload::xml_escape;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

/// The `action` attribute to write for an element, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    None,
    Modify,
    Delete,
}

impl Action {
    fn attr(self) -> &'static str {
        match self {
            Action::None => "",
            Action::Modify => " action=\"modify\"",
            Action::Delete => " action=\"delete\"",
        }
    }
}

/// ` version="N"` for an element the server knows; nothing for a locally
/// created one (negative id), which has no server version yet.
fn version_attr(id: i64, version: i32) -> String {
    if id < 0 {
        String::new()
    } else {
        format!(" version=\"{version}\"")
    }
}

/// `<tag>` children in key order (so output is deterministic), each on its
/// own line at `indent`.
fn write_tags(out: &mut String, tags: &HashMap<String, String>) {
    let mut sorted: Vec<(&String, &String)> = tags.iter().collect();
    sorted.sort();
    for (key, value) in sorted {
        let _ = writeln!(
            out,
            "    <tag k=\"{}\" v=\"{}\"/>",
            xml_escape(key),
            xml_escape(value)
        );
    }
}

fn write_node(out: &mut String, node: &OsmNode, action: Action) {
    let _ = write!(
        out,
        "  <node id=\"{}\"{}{} visible=\"true\" lat=\"{}\" lon=\"{}\"",
        node.id,
        action.attr(),
        version_attr(node.id, node.version),
        node.lat,
        node.lon
    );
    if node.tags.is_empty() || action == Action::Delete {
        out.push_str("/>\n");
    } else {
        out.push_str(">\n");
        write_tags(out, &node.tags);
        out.push_str("  </node>\n");
    }
}

fn write_way(out: &mut String, way: &OsmWay, action: Action) {
    let _ = writeln!(
        out,
        "  <way id=\"{}\"{}{} visible=\"true\">",
        way.id,
        action.attr(),
        version_attr(way.id, way.version)
    );
    for node_ref in &way.nodes {
        let _ = writeln!(out, "    <nd ref=\"{node_ref}\"/>");
    }
    if action != Action::Delete {
        write_tags(out, &way.tags);
    }
    out.push_str("  </way>\n");
}

fn write_relation(out: &mut String, relation: &OsmRelation, action: Action) {
    let _ = writeln!(
        out,
        "  <relation id=\"{}\"{}{} visible=\"true\">",
        relation.id,
        action.attr(),
        version_attr(relation.id, relation.version)
    );
    for member in &relation.members {
        let _ = writeln!(
            out,
            "    <member type=\"{}\" ref=\"{}\" role=\"{}\"/>",
            member.member_type,
            member.reference,
            xml_escape(&member.role)
        );
    }
    write_tags(out, &relation.tags);
    out.push_str("  </relation>\n");
}

/// Serialize a layer as a JOSM-style `.osm` document: everything in
/// `current`, plus the elements deleted since `original` (the server
/// baseline), with `action` attributes marking the pending edits. Elements
/// are written in id order within each kind.
pub fn to_osm_xml(original: &OsmData, current: &OsmData) -> String {
    let diff = diff_osm_data(original, current);
    let changed_nodes: HashSet<i64> = diff
        .created_nodes
        .iter()
        .chain(&diff.modified_nodes)
        .map(|n| n.id)
        .collect();
    let changed_ways: HashSet<i64> = diff
        .created_ways
        .iter()
        .chain(&diff.modified_ways)
        .map(|w| w.id)
        .collect();

    let changed_relations: HashSet<i64> = diff
        .created_relations
        .iter()
        .chain(&diff.modified_relations)
        .map(|r| r.id)
        .collect();

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<osm version=\"0.6\" generator=\"{}\">",
        xml_escape(crate::CREATED_BY)
    );
    // One `<bounds>` per downloaded rectangle, so the known area survives a
    // save and reload. (Uploads use osmChange, which has none.)
    for bounds in &current.bounds {
        let _ = writeln!(
            out,
            "  <bounds minlat=\"{}\" minlon=\"{}\" maxlat=\"{}\" maxlon=\"{}\"/>",
            bounds.min_lat, bounds.min_lon, bounds.max_lat, bounds.max_lon
        );
    }

    // Live and deleted elements interleaved in id order.
    let mut nodes: Vec<(&OsmNode, Action)> = current
        .nodes
        .values()
        .map(|node| {
            let action = if changed_nodes.contains(&node.id) {
                Action::Modify
            } else {
                Action::None
            };
            (node, action)
        })
        .chain(
            diff.deleted_node_ids
                .iter()
                .filter_map(|(id, _)| original.nodes.get(id))
                .map(|node| (node, Action::Delete)),
        )
        .collect();
    nodes.sort_by_key(|(node, _)| node.id);
    for (node, action) in nodes {
        write_node(&mut out, node, action);
    }

    let mut ways: Vec<(&OsmWay, Action)> = current
        .ways
        .values()
        .map(|way| {
            let action = if changed_ways.contains(&way.id) {
                Action::Modify
            } else {
                Action::None
            };
            (way, action)
        })
        .chain(
            diff.deleted_way_ids
                .iter()
                .filter_map(|(id, _)| original.ways.get(id))
                .map(|way| (way, Action::Delete)),
        )
        .collect();
    ways.sort_by_key(|(way, _)| way.id);
    for (way, action) in ways {
        write_way(&mut out, way, action);
    }

    let mut relations: Vec<(&OsmRelation, Action)> = current
        .relations
        .iter()
        .map(|relation| {
            let action = if changed_relations.contains(&relation.id) {
                Action::Modify
            } else {
                Action::None
            };
            (relation, action)
        })
        .chain(
            diff.deleted_relation_ids
                .iter()
                .filter_map(|(id, _)| original.relations.iter().find(|r| r.id == *id))
                .map(|relation| (relation, Action::Delete)),
        )
        .collect();
    relations.sort_by_key(|(relation, _)| relation.id);
    for (relation, action) in relations {
        write_relation(&mut out, relation, action);
    }

    out.push_str("</osm>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::diff::baseline_for_loaded_file;
    use crate::osm::{MemberType, OsmMember, OsmParser};

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn node(id: i64, lat: f64, lon: f64, version: i32, t: &[(&str, &str)]) -> OsmNode {
        OsmNode {
            id,
            lat,
            lon,
            version,
            tags: tags(t),
        }
    }

    fn way(id: i64, nodes: Vec<i64>, version: i32, t: &[(&str, &str)]) -> OsmWay {
        OsmWay {
            id,
            nodes,
            version,
            tags: tags(t),
        }
    }

    fn data(nodes: Vec<OsmNode>, ways: Vec<OsmWay>) -> OsmData {
        OsmData {
            nodes: nodes.into_iter().map(|n| (n.id, n)).collect(),
            ways: ways.into_iter().map(|w| (w.id, w)).collect(),
            relations: Vec::new(),
            bounds: Vec::new(),
        }
    }

    /// A baseline with three nodes and a way, and a current state where
    /// node 2 moved, node 3 was deleted, and node -1 and way -1 were added.
    fn edited() -> (OsmData, OsmData) {
        let original = data(
            vec![
                node(
                    1,
                    40.5,
                    -74.25,
                    3,
                    &[("amenity", "cafe"), ("name", "A & B")],
                ),
                node(2, 40.0, -74.0, 5, &[]),
                node(3, 41.0, -75.0, 2, &[("note", "gone")]),
            ],
            vec![way(10, vec![1, 2], 4, &[("highway", "path")])],
        );
        let mut current = original.clone();
        current.nodes.get_mut(&2).unwrap().lat = 40.125;
        current.nodes.remove(&3);
        current.nodes.insert(-1, node(-1, 42.0, -76.0, 1, &[]));
        current
            .ways
            .insert(-1, way(-1, vec![2, -1], 1, &[("building", "yes")]));
        (original, current)
    }

    #[test]
    fn writes_each_kind_of_element_with_the_right_action() {
        let (original, current) = edited();
        let xml = to_osm_xml(&original, &current);

        let expected_body = concat!(
            // Created: negative id, action, no version.
            "  <node id=\"-1\" action=\"modify\" visible=\"true\" lat=\"42\" lon=\"-76\"/>\n",
            // Untouched, with tags in key order and escaped.
            "  <node id=\"1\" version=\"3\" visible=\"true\" lat=\"40.5\" lon=\"-74.25\">\n",
            "    <tag k=\"amenity\" v=\"cafe\"/>\n",
            "    <tag k=\"name\" v=\"A &amp; B\"/>\n",
            "  </node>\n",
            // Modified: action plus the base version.
            "  <node id=\"2\" action=\"modify\" version=\"5\" visible=\"true\" lat=\"40.125\" lon=\"-74\"/>\n",
            // Deleted: still written, without its tags.
            "  <node id=\"3\" action=\"delete\" version=\"2\" visible=\"true\" lat=\"41\" lon=\"-75\"/>\n",
            "  <way id=\"-1\" action=\"modify\" visible=\"true\">\n",
            "    <nd ref=\"2\"/>\n",
            "    <nd ref=\"-1\"/>\n",
            "    <tag k=\"building\" v=\"yes\"/>\n",
            "  </way>\n",
            "  <way id=\"10\" version=\"4\" visible=\"true\">\n",
            "    <nd ref=\"1\"/>\n",
            "    <nd ref=\"2\"/>\n",
            "    <tag k=\"highway\" v=\"path\"/>\n",
            "  </way>\n",
            "</osm>\n",
        );
        assert!(xml.ends_with(expected_body), "unexpected output:\n{xml}");
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<osm version=\"0.6\""));
    }

    #[test]
    fn unedited_data_has_no_action_attributes() {
        let (original, _) = edited();
        let xml = to_osm_xml(&original, &original);
        assert!(!xml.contains("action="), "{xml}");
    }

    #[test]
    fn exported_edits_survive_a_round_trip_through_the_parser() {
        let (original, current) = edited();
        let xml = to_osm_xml(&original, &current);

        let (reloaded, edits) = OsmParser::new().parse_str_with_edits(&xml).unwrap();
        // The reloaded data is the current state, tag text intact.
        assert_eq!(reloaded.nodes, current.nodes);
        assert_eq!(reloaded.ways, current.ways);
        assert_eq!(reloaded.nodes[&1].tags["name"], "A & B");

        // And the reconstructed baseline yields the same pending upload.
        let baseline = baseline_for_loaded_file(&reloaded, &edits);
        let before = diff_osm_data(&original, &current);
        let after = diff_osm_data(&baseline, &reloaded);
        assert_eq!(after.counts(), before.counts());
        assert_eq!(after.deleted_node_ids, before.deleted_node_ids);
        assert_eq!(after.modified_nodes, before.modified_nodes);

        // Exporting the reloaded layer again is byte-for-byte stable.
        assert_eq!(to_osm_xml(&baseline, &reloaded), xml);
    }

    #[test]
    fn a_membership_edit_is_exported_as_modify_and_restored_on_reopen() {
        let relation = |members: &[(i64, &str)]| OsmRelation {
            id: 5,
            members: members
                .iter()
                .map(|&(reference, role)| OsmMember {
                    member_type: MemberType::Way,
                    reference,
                    role: role.to_string(),
                })
                .collect(),
            version: 2,
            tags: tags(&[("type", "route")]),
        };
        let mut original = data(
            vec![node(1, 1.0, 1.0, 1, &[]), node(2, 2.0, 2.0, 1, &[])],
            vec![way(10, vec![1, 2], 1, &[]), way(-1, vec![1, 2], 1, &[])],
        );
        original.ways.remove(&-1);
        original.relations.push(relation(&[(10, "forward")]));
        let mut current = original.clone();
        current
            .ways
            .insert(-1, way(-1, vec![2, 1], 1, &[("name", "new half")]));
        current.relations[0] = relation(&[(10, "forward"), (-1, "forward")]);

        let xml = to_osm_xml(&original, &current);
        assert!(
            xml.contains(concat!(
                "  <relation id=\"5\" action=\"modify\" version=\"2\" visible=\"true\">\n",
                "    <member type=\"way\" ref=\"10\" role=\"forward\"/>\n",
                "    <member type=\"way\" ref=\"-1\" role=\"forward\"/>\n",
                "    <tag k=\"type\" v=\"route\"/>\n",
                "  </relation>\n",
            )),
            "{xml}"
        );

        let (reloaded, edits) = OsmParser::new().parse_str_with_edits(&xml).unwrap();
        assert_eq!(edits.modified_relations, HashSet::from([5]));
        assert_eq!(reloaded.relations, current.relations);

        // The reopened layer still has the relation edit pending, with the
        // same base version, and re-exports byte for byte.
        let baseline = baseline_for_loaded_file(&reloaded, &edits);
        let diff = diff_osm_data(&baseline, &reloaded);
        assert_eq!(diff.modified_relations, current.relations);
        assert_eq!(diff.modified_relations[0].version, 2);
        assert_eq!(to_osm_xml(&baseline, &reloaded), xml);

        // An untouched relation carries no action.
        let plain = to_osm_xml(&original, &original);
        assert!(
            plain.contains("<relation id=\"5\" version=\"2\""),
            "{plain}"
        );
    }

    #[test]
    fn created_and_deleted_relations_round_trip_through_a_file() {
        let relation = |id: i64, version: i32, members: &[(MemberType, i64)]| OsmRelation {
            id,
            version,
            tags: tags(&[("type", "route")]),
            members: members
                .iter()
                .map(|&(member_type, reference)| OsmMember {
                    member_type,
                    reference,
                    role: "r".to_string(),
                })
                .collect(),
        };
        let mut original = data(vec![node(1, 1.0, 1.0, 1, &[])], vec![]);
        original
            .relations
            .push(relation(5, 2, &[(MemberType::Node, 1)]));
        original
            .relations
            .push(relation(6, 3, &[(MemberType::Node, 1)]));
        let mut current = original.clone();
        current.relations.remove(1); // delete 6
        current
            .relations
            .push(relation(-1, 0, &[(MemberType::Relation, 5)]));
        current.relations.push(relation(
            -2,
            0,
            &[(MemberType::Relation, -1), (MemberType::Node, 1)],
        ));

        let xml = to_osm_xml(&original, &current);
        assert!(
            xml.contains("  <relation id=\"-1\" action=\"modify\" visible=\"true\">\n"),
            "{xml}"
        );
        assert!(
            xml.contains("  <relation id=\"-2\" action=\"modify\" visible=\"true\">\n"),
            "{xml}"
        );
        assert!(
            xml.contains(
                "  <relation id=\"6\" action=\"delete\" version=\"3\" visible=\"true\">\n"
            ),
            "{xml}"
        );
        assert!(xml.contains("<relation id=\"5\" version=\"2\""), "{xml}");

        let (reloaded, edits) = OsmParser::new().parse_str_with_edits(&xml).unwrap();
        assert_eq!(
            edits.deleted_relations.keys().copied().collect::<Vec<_>>(),
            [6]
        );
        assert!(reloaded.relations.iter().all(|r| r.id != 6));

        let baseline = baseline_for_loaded_file(&reloaded, &edits);
        let diff = diff_osm_data(&baseline, &reloaded);
        let mut created: Vec<i64> = diff.created_relations.iter().map(|r| r.id).collect();
        created.sort_unstable();
        assert_eq!(created, vec![-2, -1]);
        assert_eq!(diff.deleted_relation_ids, vec![(6, 3)]);
        assert!(diff.modified_relations.is_empty());
        assert_eq!(diff.counts(), (2, 0, 1));
        assert_eq!(to_osm_xml(&baseline, &reloaded), xml);
    }

    #[test]
    fn several_bounds_round_trip_in_order() {
        let mut current = data(vec![node(1, 1.0, 2.0, 1, &[])], vec![]);
        current.bounds = vec![
            crate::osm::OsmBounds {
                min_lat: 1.0,
                max_lat: 2.0,
                min_lon: 3.0,
                max_lon: 4.0,
            },
            crate::osm::OsmBounds {
                min_lat: 2.0,
                max_lat: 3.0,
                min_lon: 3.5,
                max_lon: 5.0,
            },
        ];
        let xml = to_osm_xml(&current, &current);
        assert_eq!(xml.matches("<bounds ").count(), 2);
        let (reloaded, _) = OsmParser::new().parse_str_with_edits(&xml).unwrap();
        assert_eq!(reloaded.bounds, current.bounds);
    }

    #[test]
    fn no_bounds_are_written_when_none_are_known() {
        let current = data(vec![node(1, 1.0, 2.0, 1, &[])], vec![]);
        assert!(!to_osm_xml(&current, &current).contains("<bounds"));
    }

    #[test]
    fn relations_and_bounds_are_carried_through() {
        let mut current = data(vec![node(1, 1.0, 2.0, 1, &[])], vec![]);
        current.bounds = vec![crate::osm::OsmBounds {
            min_lat: 1.0,
            max_lat: 2.0,
            min_lon: 3.0,
            max_lon: 4.0,
        }];
        current.relations.push(OsmRelation {
            id: 7,
            members: vec![OsmMember {
                member_type: MemberType::Node,
                reference: 1,
                role: "stop \"A\"".to_string(),
            }],
            version: 2,
            tags: tags(&[("type", "route")]),
        });

        let xml = to_osm_xml(&current, &current);

        assert!(xml.contains("  <bounds minlat=\"1\" minlon=\"3\" maxlat=\"2\" maxlon=\"4\"/>\n"));
        assert!(xml.contains(concat!(
            "  <relation id=\"7\" version=\"2\" visible=\"true\">\n",
            "    <member type=\"node\" ref=\"1\" role=\"stop &quot;A&quot;\"/>\n",
            "    <tag k=\"type\" v=\"route\"/>\n",
            "  </relation>\n",
        )));
        let (reloaded, _) = OsmParser::new().parse_str_with_edits(&xml).unwrap();
        assert_eq!(reloaded, current);
    }
}
