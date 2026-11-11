//! What the pending diff contains and the order the upload document puts it in.

use super::*;
use osm_gpui::osm_upload::build_osm_change_xml;

fn road() -> [(&'static str, &'static str); 1] {
    [("highway", "residential")]
}

fn new_rel(id: i64, members: &[(MemberType, i64)]) -> OsmRelation {
    OsmRelation {
        id,
        version: 1,
        tags: HashMap::new(),
        members: members
            .iter()
            .map(|&(member_type, reference)| OsmMember {
                member_type,
                reference,
                role: String::new(),
            })
            .collect(),
    }
}

#[gpui::test]
fn a_new_relation_that_contains_a_new_relation_comes_after_it(cx: &mut TestAppContext) {
    let data = Build::new().nodes(&[1, 2]).way(10, &[1, 2], &road()).done();
    with_session(cx, data, |s| {
        s.select(&[(W, 10)]);
        assert!(s.act("new_relation"));
        let inner = -1;
        assert!(s.snap().relations.contains_key(&inner));
        s.select(&[(R, inner)]);
        assert!(s.act("new_relation"));
        let xml = s.xml();
        let a = at(&xml, "<relation id=\"-1\"");
        let b = at(&xml, "<relation id=\"-2\"");
        // The parent (-2) names -1, so -1 must be created first even though
        // -2 sorts first by id.
        assert!(a < b, "{xml}");
    });
}

#[gpui::test]
fn relation_reference_cycles_still_produce_a_document(cx: &mut TestAppContext) {
    let diff = LayerDiff {
        created_relations: vec![
            new_rel(-1, &[(MemberType::Relation, -2)]),
            new_rel(-2, &[(MemberType::Relation, -1)]),
        ],
        ..Default::default()
    };
    let xml = build_osm_change_xml(7, &[("l", diff)]);
    // Cannot be satisfied; both are emitted (the server reports it) and the
    // builder does not loop.
    assert!(xml.contains("<relation id=\"-1\"") && xml.contains("<relation id=\"-2\""));
    let _ = cx;
}

#[gpui::test]
fn deletions_go_relations_then_ways_then_nodes_after_all_modifications(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2], &road())
        .way(11, &[2, 3], &road())
        .relation(900, &[("type", "route"), ("route", "bus")], &[wm(10, "")])
        .relation(901, &[("type", "route"), ("route", "bus")], &[wm(11, "")])
        .done();
    with_session(cx, data, |s| {
        // Delete relation 900, way 10 and node 1, and modify way 11 / node 3.
        s.set_tag(W, 11, "name", "x");
        s.set_tag(N, 3, "note", "y");
        s.select(&[(R, 900), (W, 10), (N, 1)]);
        assert!(s.act("delete"));
        let xml = s.xml();
        let modify = at(&xml, "<modify>");
        let delete = at(&xml, "<delete>");
        assert!(modify < delete);
        let d = &xml[delete..];
        assert!(at(d, "<relation ") < at(d, "<way "));
        assert!(at(d, "<way ") < at(d, "<node "));
        // Modifications: node, then way.
        let m = &xml[modify..delete];
        assert!(at(m, "<node ") < at(m, "<way "));
    });
}

#[gpui::test]
fn moving_a_node_and_deleting_its_way_uploads_both(cx: &mut TestAppContext) {
    let data = Build::new()
        .nodes(&[1, 2, 3])
        .way(10, &[1, 2, 3], &road())
        .done();
    with_session(cx, data, |s| {
        s.view
            .layer_manager
            .find_layer_mut(s.layer)
            .unwrap()
            .as_editable_mut()
            .unwrap()
            .commit_node_moves(&[(2, 40.5, -74.5)]);
        s.select(&[(W, 10)]);
        assert!(s.act("delete"));
        let d = s.diff();
        assert_eq!(d.modified_nodes.len(), 1);
        assert_eq!(d.deleted_way_ids.len(), 1);
        let xml = s.xml();
        assert!(at(&xml, "<modify>") < at(&xml, "<delete>"));
    });
}

#[gpui::test]
fn created_then_deleted_in_one_session_uploads_nothing(cx: &mut TestAppContext) {
    let data = Build::new().nodes(&[1, 2]).way(10, &[1, 2], &road()).done();
    with_session(cx, data, |s| {
        // A relation made and deleted again.
        s.select(&[(W, 10)]);
        assert!(s.act("new_relation"));
        s.select(&[(R, -1)]);
        assert!(s.act("delete"));
        assert_eq!(s.counts(), (0, 0, 0));
        // A node and a way made and deleted again.
        let (n, w) = {
            let e = s
                .view
                .layer_manager
                .find_layer_mut(s.layer)
                .unwrap()
                .as_editable_mut()
                .unwrap();
            let a = e.add_node(40.1, -74.1);
            let b = e.add_node(40.2, -74.2);
            (a, e.add_way(vec![a, b], vec![]))
        };
        assert_eq!(s.counts(), (3, 0, 0));
        s.select(&[(W, w)]);
        assert!(s.act("delete"));
        s.select(&[(N, n)]);
        let _ = s.act("delete");
        let d = s.diff();
        assert!(d.created_ways.is_empty());
        assert!(
            d.deleted_way_ids.is_empty(),
            "a new way is never deleted on the server"
        );
    });
}

#[gpui::test]
fn modified_then_reverted_uploads_nothing(cx: &mut TestAppContext) {
    let data = Build::new().nodes(&[1, 2]).way(10, &[1, 2], &road()).done();
    with_session(cx, data, |s| {
        s.set_tag(W, 10, "name", "temp");
        assert_eq!(s.counts(), (0, 1, 0));
        s.view
            .layer_manager
            .find_layer_mut(s.layer)
            .unwrap()
            .as_editable_mut()
            .unwrap()
            .remove_tag(W, 10, "name");
        assert_eq!(s.counts(), (0, 0, 0));
        // Reverse twice is the same.
        s.select(&[(W, 10)]);
        assert!(s.act("reverse"));
        assert!(s.act("reverse"));
        assert_eq!(s.counts(), (0, 0, 0));
    });
}

#[gpui::test]
fn modifications_are_ordered_nodes_ways_relations_and_creates_nodes_ways_relations(
    cx: &mut TestAppContext,
) {
    let data = Build::new()
        .nodes(&[1, 2, 3, 4])
        .way(10, &[1, 2, 3, 4], &road())
        .relation(900, &[("type", "route"), ("route", "bus")], &[wm(10, "")])
        .done();
    with_session(cx, data, |s| {
        s.set_tag(N, 1, "note", "n");
        s.select(&[(N, 2), (W, 10)]);
        assert!(s.act("split"));
        s.select(&[(W, 10)]);
        assert!(s.act("new_relation"));
        let xml = s.xml();
        let create = at(&xml, "<create>");
        let modify = at(&xml, "<modify>");
        let c = &xml[create..modify];
        assert!(at(c, "<way ") < at(c, "<relation "));
        let m = &xml[modify..];
        assert!(at(m, "<node ") < at(m, "<way "));
        assert!(at(m, "<way ") < at(m, "<relation "));
    });
}
