//! Upload conflicts, end to end against the fake HTTP client.

use super::{data_with, empty_tags};
use crate::http::fake::{ok, status_err, FakeClient};
use crate::layers::osm_layer::OsmLayer;
use crate::layers::LayerId;
use crate::layers::{EditableLayer, MapLayer};
use crate::osm::{OsmNode, OsmWay};
use crate::osm_api::{fetch_element_with, ServerElement};
use crate::osm_upload::{run_upload, ConflictKind, UploadConflict, UploadError};
use crate::selection::{FeatureKind, FeatureRef};

const BASE: &str = "https://osm.test";
const MISMATCH: &str = "Version mismatch: Provided 1, server had: 2 of Node 1";

fn node(id: i64, lat: f64, version: i32) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon: -74.0,
        version,
        tags: empty_tags(),
    }
}

/// A layer with node 1 (v1) and way 10 (v1) over nodes 1, 2; node 1 is
/// edited locally (tagged).
fn edited_layer() -> OsmLayer {
    let way = OsmWay {
        id: 10,
        nodes: vec![1, 2],
        version: 1,
        tags: empty_tags(),
    };
    let data = data_with(vec![node(1, 40.0, 1), node(2, 40.001, 1)], vec![way]);
    let mut layer = OsmLayer::new_with_data(LayerId(1), "L", data);
    layer.set_tag(FeatureKind::Node, 1, "amenity", "cafe");
    layer
}

/// Upload `layer`'s diff against `client`; the conflict it is refused with.
fn upload(layer: &OsmLayer, client: &FakeClient) -> Result<(), UploadConflict> {
    let diff = layer.diff_for_upload();
    match run_upload(client, BASE, "tok", "edit", &[("L", diff)], &mut |_| {}) {
        Ok(_) => Ok(()),
        Err(f) => match f.error {
            UploadError::Conflict(c) => Err(c),
            other => panic!("unexpected {other:?}"),
        },
    }
}

/// The osmChange body of the second request (the upload) `client` saw.
fn sent_xml(client: &FakeClient, upload_index: usize) -> String {
    let requests = client.requests.lock().unwrap();
    let uploads: Vec<_> = requests
        .iter()
        .filter(|r| r.url.ends_with("/upload"))
        .collect();
    match &uploads[upload_index].body {
        Some(crate::http::Body::Text(xml)) => xml.clone(),
        other => panic!("no xml body: {other:?}"),
    }
}

fn refused_session() -> Vec<Result<crate::http::HttpResponse, crate::http::HttpError>> {
    vec![ok(200, "42"), status_err(409, MISMATCH), ok(200, "")]
}

fn server_node_xml(version: i32, lat: f64) -> String {
    format!(
        r#"<osm version="0.6"><node id="1" visible="true" version="{version}" lat="{lat}" lon="-74.0"><tag k="shop" v="bakery"/></node></osm>"#
    )
}

#[test]
fn keep_mine_makes_the_retry_overwrite_the_servers_version() {
    let mut layer = edited_layer();
    let client = FakeClient::new(refused_session());
    let conflict = upload(&layer, &client).unwrap_err();
    assert_eq!(
        conflict.kind,
        ConflictKind::VersionMismatch {
            provided: 1,
            server: 2
        }
    );
    assert!(sent_xml(&client, 0).contains(r#"version="1" changeset="42""#));

    // Fetch the server's version number and adopt it as the base.
    let (kind, id) = conflict.object.unwrap();
    let fetch = FakeClient::new(vec![ok(200, server_node_xml(2, 40.0))]);
    let server = fetch_element_with(&fetch, BASE, None, kind, id, false).unwrap();
    let version = server.version(kind, id).unwrap();
    assert!(EditableLayer::set_base_version(
        &mut layer, kind, id, version
    ));

    // The edit is still there, now against version 2.
    assert_eq!(
        layer.feature_tags(&FeatureRef {
            layer_id: LayerId(1),
            kind,
            id
        }),
        Some(vec![("amenity".to_string(), "cafe".to_string())])
    );
    let retry = FakeClient::new(vec![
        ok(200, "43"),
        ok(
            200,
            r#"<diffResult><node old_id="1" new_id="1" new_version="3"/></diffResult>"#,
        ),
        ok(200, ""),
    ]);
    upload(&layer, &retry).unwrap();
    let xml = sent_xml(&retry, 0);
    assert!(
        xml.contains(r#"<node id="1" lat="40" lon="-74" version="2" changeset="43">"#),
        "{xml}"
    );
    assert!(xml.contains(r#"<tag k="amenity" v="cafe"/>"#), "{xml}");
}

#[test]
fn use_theirs_replaces_the_object_and_the_retry_leaves_it_out() {
    let mut layer = edited_layer();
    // A second, unrelated edit that must survive.
    layer.set_tag(FeatureKind::Way, 10, "highway", "path");
    let client = FakeClient::new(refused_session());
    let conflict = upload(&layer, &client).unwrap_err();
    let (kind, id) = conflict.object.unwrap();

    let fetch = FakeClient::new(vec![ok(200, server_node_xml(2, 40.5))]);
    let server = fetch_element_with(&fetch, BASE, None, kind, id, false).unwrap();
    assert!(EditableLayer::adopt_server_element(
        &mut layer, kind, id, server
    ));

    let tags = layer
        .feature_tags(&FeatureRef {
            layer_id: LayerId(1),
            kind,
            id,
        })
        .unwrap();
    assert_eq!(tags, vec![("shop".to_string(), "bakery".to_string())]);
    assert_eq!(layer.node_lat_lon(1), Some((40.5, -74.0)));

    let retry = FakeClient::new(vec![
        ok(200, "43"),
        ok(
            200,
            r#"<diffResult><way old_id="10" new_id="10" new_version="2"/></diffResult>"#,
        ),
        ok(200, ""),
    ]);
    upload(&layer, &retry).unwrap();
    let xml = sent_xml(&retry, 0);
    assert!(!xml.contains(r#"<node id="1""#), "node 1 dropped: {xml}");
    assert!(xml.contains(r#"<way id="10" version="1""#), "{xml}");
    assert!(layer.is_modified());
}

#[test]
fn use_theirs_that_leaves_nothing_to_upload_clears_the_modified_flag() {
    let mut layer = edited_layer();
    assert!(layer.is_modified());
    let fetch = FakeClient::new(vec![ok(200, server_node_xml(2, 40.0))]);
    let server = fetch_element_with(&fetch, BASE, None, FeatureKind::Node, 1, false).unwrap();
    assert!(EditableLayer::adopt_server_element(
        &mut layer,
        FeatureKind::Node,
        1,
        server
    ));
    assert!(layer.diff_for_upload().is_empty());
    assert!(!layer.is_modified());
}

#[test]
fn use_theirs_on_an_object_deleted_on_the_server_removes_it_locally() {
    let mut layer = edited_layer();
    // The way uses node 1, so edit the way instead: delete-on-server
    // for way 10 after a local tag edit.
    layer.set_tag(FeatureKind::Way, 10, "highway", "path");
    let client = FakeClient::new(vec![
        ok(200, "42"),
        status_err(410, "The way with the id 10 has already been deleted"),
        ok(200, ""),
    ]);
    let conflict = upload(&layer, &client).unwrap_err();
    assert_eq!(conflict.kind, ConflictKind::AlreadyDeleted);
    let (kind, id) = conflict.object.unwrap();
    assert_eq!((kind, id), (FeatureKind::Way, 10));

    let fetch = FakeClient::new(vec![status_err(410, "")]);
    let server = fetch_element_with(&fetch, BASE, None, kind, id, true).unwrap();
    assert_eq!(server, ServerElement::Gone);
    assert!(EditableLayer::adopt_server_element(
        &mut layer, kind, id, server
    ));
    assert!(!EditableLayer::has_feature(&layer, kind, id));
    assert!(layer.way_node_ids(10).is_none());

    let diff = layer.diff_for_upload();
    assert!(diff.modified_ways.is_empty() && diff.deleted_way_ids.is_empty());
    assert_eq!(diff.modified_nodes.len(), 1, "the node edit stays");
}

#[test]
fn drop_my_change_forgets_a_delete_the_server_already_did() {
    let mut layer = edited_layer();
    // Delete way 10 locally.
    layer.delete_feature(FeatureKind::Way, 10).unwrap();
    assert!(EditableLayer::is_locally_deleted(
        &layer,
        FeatureKind::Way,
        10
    ));
    assert_eq!(layer.diff_for_upload().deleted_way_ids, vec![(10, 1)]);

    assert!(EditableLayer::forget_local_delete(
        &mut layer,
        FeatureKind::Way,
        10
    ));
    assert!(layer.diff_for_upload().deleted_way_ids.is_empty());
    assert!(!EditableLayer::is_locally_deleted(
        &layer,
        FeatureKind::Way,
        10
    ));
    // Nothing to forget for an object that is simply there.
    assert!(!EditableLayer::forget_local_delete(
        &mut layer,
        FeatureKind::Node,
        1
    ));
}

#[test]
fn keep_mine_on_a_locally_deleted_object_updates_the_delete_version() {
    let mut layer = edited_layer();
    layer.delete_feature(FeatureKind::Way, 10).unwrap();
    assert!(EditableLayer::set_base_version(
        &mut layer,
        FeatureKind::Way,
        10,
        5
    ));
    assert_eq!(layer.diff_for_upload().deleted_way_ids, vec![(10, 5)]);
}
