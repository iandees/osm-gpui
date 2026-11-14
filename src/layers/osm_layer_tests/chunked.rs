//! Chunked upload, end to end against the fake HTTP client.

use super::{data_with, empty_tags};
use crate::http::fake::{ok, status_err, FakeClient};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::EditableLayer;
use crate::layers::LayerId;
use crate::osm::{OsmNode, OsmWay};
use crate::selection::FeatureKind;
use crate::upload_chunks::run_chunked_upload;

const BASE: &str = "https://osm.test";

fn new_node(id: i64) -> OsmNode {
    OsmNode {
        id,
        lat: 40.0,
        lon: -74.0,
        version: 0,
        tags: empty_tags(),
    }
}

fn new_way(id: i64, nodes: &[i64]) -> OsmWay {
    OsmWay {
        id,
        nodes: nodes.to_vec(),
        version: 0,
        tags: empty_tags(),
    }
}

/// A layer whose only changes are 6 new nodes (-1..-6) and 2 new
/// ways: -1 over -1,-2,-3 and -2 over -4,-5,-6. With a limit of 3 the
/// plan is [n-1 n-2 n-3] [w-1 n-4 n-5] [n-6 w-2].
fn layer_with_new_data() -> OsmLayer {
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data_with(vec![], vec![]));
    layer.set_osm_data_for_test(data_with(
        (1..=6).map(|i| new_node(-i)).collect(),
        vec![new_way(-1, &[-1, -2, -3]), new_way(-2, &[-4, -5, -6])],
    ));
    layer
}

fn nodes_result(pairs: &[(i64, i64)]) -> String {
    let mut s = String::from("<diffResult>");
    for (old, new) in pairs {
        s.push_str(&format!(
            r#"<node old_id="{old}" new_id="{new}" new_version="1"/>"#
        ));
    }
    s.push_str("</diffResult>");
    s
}

fn bodies(client: &FakeClient) -> Vec<String> {
    client
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.url.ends_with("/upload"))
        .map(|r| match &r.body {
            Some(crate::http::Body::Text(x)) => x.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn upload(
    layer: &mut OsmLayer,
    client: &FakeClient,
    max: usize,
) -> crate::upload_chunks::ChunkedUpload {
    let layers = [("L", layer.diff_for_upload())];
    // Each accepted chunk is applied as it lands, as the app does.
    run_chunked_upload(
        client,
        BASE,
        "tok",
        "edit",
        &layers,
        max,
        &mut |_, _| {},
        &mut |done| crate::upload_chunks::apply_to_layer(layer, done),
    )
    .unwrap()
}

#[test]
fn a_failure_in_the_middle_leaves_exactly_the_unsent_changes_pending() {
    let mut layer = layer_with_new_data();
    assert_eq!(layer.diff_for_upload().counts(), (8, 0, 0));
    let client = FakeClient::new(vec![
                ok(200, "101"),
                ok(200, nodes_result(&[(-1, 5001), (-2, 5002), (-3, 5003)])),
                ok(200, ""),
                ok(200, "102"),
                status_err(412, "Way -1 requires the nodes with id in (1), which either do not exist, or are not visible."),
                ok(200, ""),
            ]);
    let out = upload(&mut layer, &client, 3);
    assert_eq!(out.total, 3);
    assert_eq!(out.completed.len(), 1);
    assert!(out.failure.is_some());

    // Chunk 1 is on the server and reflected locally: real ids, v1.
    let data = layer.get_osm_data().unwrap();
    for id in [5001, 5002, 5003] {
        assert_eq!(data.nodes[&id].version, 1);
    }
    assert!(!data.nodes.contains_key(&-1));
    // Way -1 (in chunk 2) now names the real nodes.
    assert_eq!(data.ways[&-1].nodes, vec![5001, 5002, 5003]);

    // What is pending is exactly chunks 2 and 3.
    assert!(layer.is_modified());
    let rest = layer.diff_for_upload();
    let mut nodes: Vec<i64> = rest.created_nodes.iter().map(|n| n.id).collect();
    nodes.sort();
    assert_eq!(nodes, vec![-6, -5, -4]);
    let mut ways: Vec<i64> = rest.created_ways.iter().map(|w| w.id).collect();
    ways.sort();
    assert_eq!(ways, vec![-2, -1]);
    assert_eq!(rest.counts(), (5, 0, 0));
    assert!(rest.modified_nodes.is_empty() && rest.deleted_node_ids.is_empty());
}

/// Records the start of each changeset in `log`.
struct Logging {
    inner: FakeClient,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl crate::http::HttpClient for Logging {
    fn request(
        &self,
        req: crate::http::HttpRequest,
    ) -> Result<crate::http::HttpResponse, crate::http::HttpError> {
        if req.url.ends_with("/create") {
            self.log.lock().unwrap().push("create".to_string());
        }
        self.inner.request(req)
    }
}

#[test]
fn the_layer_has_server_ids_for_chunk_one_before_chunk_two_is_opened() {
    let mut layer = layer_with_new_data();
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let client = Logging {
        inner: FakeClient::new(vec![
            ok(200, "101"),
            ok(200, nodes_result(&[(-1, 5001), (-2, 5002), (-3, 5003)])),
            ok(200, ""),
            // Chunk 2 never gets as far as an upload: the sequence ends.
            crate::http::fake::transport_err("connection reset"),
        ]),
        log: log.clone(),
    };
    let layers = [("L", layer.diff_for_upload())];
    let seen = log.clone();
    let out = run_chunked_upload(
        &client,
        BASE,
        "tok",
        "edit",
        &layers,
        3,
        &mut |_, _| {},
        &mut |done| {
            crate::upload_chunks::apply_to_layer(&mut layer, done);
            let mut ids: Vec<i64> = layer
                .get_osm_data()
                .unwrap()
                .nodes
                .keys()
                .copied()
                .filter(|&id| id > 0)
                .collect();
            ids.sort();
            seen.lock().unwrap().push(format!("layer has {ids:?}"));
        },
    )
    .unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        vec!["create", "layer has [5001, 5002, 5003]", "create"]
    );

    // The sequence stopped right after chunk 1 (as if dropped): what
    // is pending is exactly chunks 2 and 3, so another upload would
    // not create chunk 1's objects again.
    assert_eq!(out.completed.len(), 1);
    let rest = layer.diff_for_upload();
    assert_eq!(rest.counts(), (5, 0, 0));
    assert!(rest.created_nodes.iter().all(|n| n.id < 0));
    assert!(layer.is_modified());
}

#[test]
fn pressing_upload_again_sends_only_the_remainder_and_finishes_clean() {
    let mut layer = layer_with_new_data();
    let client = FakeClient::new(vec![
        ok(200, "101"),
        ok(200, nodes_result(&[(-1, 5001), (-2, 5002), (-3, 5003)])),
        ok(200, ""),
        ok(200, "102"),
        status_err(409, "The changeset 102 was closed at now"),
        ok(200, ""),
    ]);
    upload(&mut layer, &client, 3);

    // Second attempt: 5 elements left -> [w-1 n-4 n-5] [n-6 w-2].
    let retry = FakeClient::new(vec![
        ok(200, "201"),
        ok(
            200,
            r#"<diffResult><node old_id="-4" new_id="5004" new_version="1"/><node old_id="-5" new_id="5005" new_version="1"/><way old_id="-1" new_id="6001" new_version="1"/></diffResult>"#,
        ),
        ok(200, ""),
        ok(200, "202"),
        ok(
            200,
            r#"<diffResult><node old_id="-6" new_id="5006" new_version="1"/><way old_id="-2" new_id="6002" new_version="1"/></diffResult>"#,
        ),
        ok(200, ""),
    ]);
    let out = upload(&mut layer, &retry, 3);
    assert!(out.failure.is_none());
    assert_eq!(out.completed.len(), 2);

    let sent = bodies(&retry);
    assert_eq!(sent.len(), 2);
    // Nothing from chunk 1 is sent again; way -1 uses its server ids.
    assert!(!sent.iter().any(|b| b.contains(r#"id="5001""#)));
    assert!(sent[0].contains(r#"<nd ref="5001"/><nd ref="5002"/><nd ref="5003"/>"#));
    // Way -2: nodes 5004/5005 from the earlier chunk, -6 its own.
    assert!(sent[1].contains(r#"<nd ref="5004"/><nd ref="5005"/><nd ref="-6"/>"#));

    // Everything is on the server and nothing is pending.
    assert!(!layer.is_modified());
    assert!(layer.diff_for_upload().is_empty());
    let data = layer.get_osm_data().unwrap();
    assert_eq!(data.nodes.len(), 6);
    assert_eq!(data.ways[&6002].nodes, vec![5004, 5005, 5006]);
    assert!(data.nodes.keys().all(|&id| id > 0));
}

#[test]
fn modified_and_deleted_objects_are_applied_per_chunk_too() {
    // Existing nodes 1, 2 (edited), 3 (deleted); limit 2 => [m1 m2] [d3].
    let mut layer = OsmLayer::new_with_data(
        LayerId(1),
        "L",
        data_with(
            vec![
                OsmNode {
                    version: 1,
                    ..new_node(1)
                },
                OsmNode {
                    version: 1,
                    ..new_node(2)
                },
                OsmNode {
                    version: 1,
                    ..new_node(3)
                },
            ],
            vec![],
        ),
    );
    layer.set_tag(FeatureKind::Node, 1, "a", "b").unwrap();
    layer.set_tag(FeatureKind::Node, 2, "a", "b").unwrap();
    layer.delete_feature(FeatureKind::Node, 3).unwrap();
    let client = FakeClient::new(vec![
        ok(200, "1"),
        ok(
            200,
            r#"<diffResult><node old_id="1" new_id="1" new_version="2"/><node old_id="2" new_id="2" new_version="2"/></diffResult>"#,
        ),
        ok(200, ""),
        ok(200, "2"),
        status_err(409, "Node 3 is still used by way 9"),
        ok(200, ""),
    ]);
    upload(&mut layer, &client, 2);
    let data = layer.get_osm_data().unwrap();
    assert_eq!(data.nodes[&1].version, 2);
    assert_eq!(data.nodes[&2].version, 2);
    let rest = layer.diff_for_upload();
    assert_eq!(rest.counts(), (0, 0, 1));
    assert_eq!(rest.deleted_node_ids, vec![(3, 1)]);
    assert!(layer.is_modified());
}
