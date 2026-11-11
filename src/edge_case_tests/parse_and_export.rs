//! `.osm` reading and writing: odd ids, actions, escaping, round trips.

use super::*;
use osm_gpui::osm::OsmParser;
use osm_gpui::osm_export::to_osm_xml;

fn parse(xml: &str) -> Result<OsmData, osm_gpui::osm::OsmParseError> {
    OsmParser::new().parse_str(xml)
}

#[test]
fn negative_ids_are_read_as_given_and_references_resolve() {
    let data = parse(
        r#"<osm version="0.6">
  <node id="-2" lat="1" lon="2"/><node id="-1" lat="1.5" lon="2.5"/>
  <way id="-3"><nd ref="-1"/><nd ref="-2"/><tag k="highway" v="path"/></way>
  <relation id="-4"><member type="way" ref="-3" role="outer"/><member type="relation" ref="-5" role=""/></relation>
</osm>"#,
    )
    .unwrap();
    assert_eq!(data.ways[&-3].nodes, vec![-1, -2]);
    assert_eq!(data.relations[0].members[0].reference, -3);
    assert_eq!(
        data.relations[0].members[1].member_type,
        MemberType::Relation
    );
    // New objects default to version 1 when the file gives none.
    assert_eq!(data.nodes[&-1].version, 1);
}

#[test]
fn deleted_elements_are_kept_apart_from_the_data_and_modified_ones_noted() {
    let (data, edits) = OsmParser::new()
        .parse_str_with_edits(
            r#"<osm version="0.6">
  <node id="1" version="2" lat="1" lon="2" action="delete"/>
  <node id="2" version="4" lat="1" lon="2" action="modify"><tag k="a" v="b"/></node>
  <way id="3" version="1" action="delete"><nd ref="1"/><nd ref="2"/></way>
  <relation id="4" version="1" action="delete"><member type="node" ref="2" role="x"/></relation>
</osm>"#,
        )
        .unwrap();
    assert!(!data.nodes.contains_key(&1) && data.nodes.contains_key(&2));
    assert!(data.ways.is_empty() && data.relations.is_empty());
    assert!(edits.deleted_nodes.contains_key(&1));
    assert!(edits.deleted_ways.contains_key(&3));
    assert!(edits.deleted_relations.contains_key(&4));
    assert!(edits.modified_nodes.contains(&2));
}

#[test]
fn objects_without_tags_or_members_and_missing_versions_are_accepted() {
    let data = parse(
        r#"<osm><node id="1" lat="0" lon="0"/><way id="2"><nd ref="1"/></way><relation id="3"/></osm>"#,
    )
    .unwrap();
    assert!(data.nodes[&1].tags.is_empty());
    assert_eq!(data.ways[&2].version, 1);
    assert!(data.relations[0].members.is_empty());
}

#[test]
fn malformed_input_is_an_error_not_a_guess() {
    // Missing id, missing ref, unknown member type, tag without a key.
    assert!(parse(r#"<osm><node lat="0" lon="0"/></osm>"#).is_err());
    assert!(parse(r#"<osm><way id="1"><nd/></way></osm>"#).is_err());
    assert!(
        parse(r#"<osm><relation id="1"><member type="area" ref="1"/></relation></osm>"#).is_err()
    );
    assert!(parse(r#"<osm><node id="1" lat="0" lon="0"><tag v="x"/></node></osm>"#).is_err());
    assert!(parse(r#"<osm><node id="1" lat="abc" lon="0"/></osm>"#).is_err());
}

#[test]
fn an_empty_tag_value_and_duplicate_keys_are_handled() {
    let data = parse(
        r#"<osm><node id="1" lat="0" lon="0"><tag k="a" v=""/><tag k="b" v="1"/><tag k="b" v="2"/></node></osm>"#,
    )
    .unwrap();
    assert_eq!(data.nodes[&1].tags["a"], "");
    // Last one wins.
    assert_eq!(data.nodes[&1].tags["b"], "2");
}

fn odd_data() -> OsmData {
    Build::new()
        .node(
            1,
            &[
                ("name", "Barnes & Noble <\"best\"> 'quoted'"),
                ("note", "line one\nline two\r\n\ttabbed"),
                ("description", "  padded  "),
                ("name:ja", "東京タワー"),
            ],
        )
        .node(2, &[])
        .way(10, &[1, 2, 1], &[("area", "yes"), ("a&b", "c<d")])
        .relation(
            900,
            &[("type", "multipolygon"), ("note", "x\ny")],
            &[
                wm(10, "ou\"ter"),
                nm(1, ""),
                rm(901, "a&b"),
                wm(10, "outer"),
            ],
        )
        .relation(901, &[("type", "route")], &[])
        .done()
}

#[test]
fn awkward_characters_survive_a_write_and_read() {
    let data = odd_data();
    let xml = to_osm_xml(&data, &data);
    let back = parse(&xml).unwrap();
    assert_eq!(back.nodes, data.nodes);
    assert_eq!(back.ways, data.ways);
    // Member order, repeated members and roles are exact.
    assert_eq!(back.relations, data.relations);
}

#[test]
fn pending_edits_round_trip_through_the_action_attributes() {
    let original = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &[("highway", "path")])
        .relation(900, &[("type", "route")], &[wm(10, "")])
        .done();
    let mut current = original.clone();
    current.nodes.get_mut(&1).unwrap().lat = 41.0;
    current.nodes.remove(&3);
    current.ways.get_mut(&10).unwrap().nodes = vec![1, 2];
    current.ways.insert(
        -1,
        OsmWay {
            id: -1,
            nodes: vec![1, 2],
            version: 1,
            tags: HashMap::new(),
        },
    );
    current.relations[0].members.push(OsmMember {
        member_type: MemberType::Way,
        reference: -1,
        role: "x".into(),
    });
    let xml = to_osm_xml(&original, &current);
    let (data, edits) = OsmParser::new().parse_str_with_edits(&xml).unwrap();
    assert!(edits.modified_nodes.contains(&1));
    assert!(edits.deleted_nodes.contains_key(&3));
    assert!(edits.modified_ways.contains(&10) && edits.modified_ways.contains(&-1));
    assert!(edits.modified_relations.contains(&900));
    assert_eq!(data.relations[0].members.len(), 2);
    assert_eq!(data.ways[&-1].nodes, vec![1, 2]);
}
