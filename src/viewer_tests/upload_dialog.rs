//! The upload dialog: validation, changeset tags and size limits.

use super::{
    long_way_nodes, make_pending_change, setup, setup_network, setup_network_with, validation_view,
    way_fixture, CENTER_LAT, CENTER_LON,
};
use crate::{validation_panel, MapViewer};
use gpui::prelude::*;
use gpui::TestAppContext;
use osm_gpui::layers::osm_layer::OsmLayer;
use osm_gpui::layers::tile_layer::TileLayer;
use osm_gpui::settings_store;
use std::sync::Arc;

#[gpui::test]
fn upload_is_refused_for_an_oversized_way_and_selects_it(cx: &mut TestAppContext) {
    let window = setup_network(cx);
    window
        .update(cx, |view, window, cx| {
            let layer_id = view.active_layer.unwrap();
            view.layer_manager
                .find_layer_mut(layer_id)
                .and_then(|l| l.as_editable_mut())
                .unwrap()
                .set_way_nodes(100, long_way_nodes(2104));
            view.open_upload_dialog_as(Some("me"), window, cx);
            assert!(view.upload_dialog.is_none() && view.sign_in_dialog.is_none());
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some(
                    "Way 100 has 2,104 nodes; the server allows 2,000. Split it before uploading."
                )
            );
            assert_eq!(view.selected.len(), 1);
            assert_eq!(view.selected[0].id, 100);
            assert_eq!(view.selected[0].layer_id, layer_id);
            // The refusal is also a dialog naming the object and limit,
            // and it is shown to a signed-out user too (signing in would
            // not help).
            let notice = view.notice_dialog.clone().expect("refusal dialog");
            assert!(notice
                .read(cx)
                .message()
                .contains("Way 100 has 2,104 nodes"));
            assert!(notice.read(cx).message().contains("allows 2,000"));
            view.notice_dialog = None;
            view.open_upload_dialog_as(None, window, cx);
            assert!(view.notice_dialog.is_some() && view.sign_in_dialog.is_none());
        })
        .unwrap();
}

#[gpui::test]
fn an_unmodified_oversized_way_does_not_block_other_changes(cx: &mut TestAppContext) {
    let window = setup_network_with(cx, |data| {
        data.ways.get_mut(&100).unwrap().nodes = long_way_nodes(2500);
    });
    window
        .update(cx, |view, _window, _cx| {
            make_pending_change(view);
            let layer_id = view.active_layer.unwrap();
            let diff = view
                .layer_manager
                .find_layer(layer_id)
                .unwrap()
                .diff_for_upload();
            assert!(diff.modified_ways.is_empty());
            let limits = osm_gpui::limits::Limits::for_server(&settings_store::api_base_url());
            assert_eq!(osm_gpui::limits::check_upload(&[&diff], &limits), Ok(()));
        })
        .unwrap();
}

#[gpui::test]
fn upload_without_changes_shows_a_status_message(cx: &mut TestAppContext) {
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.open_upload_dialog_as(Some("me"), window, cx);
            assert!(view.upload_dialog.is_none() && view.sign_in_dialog.is_none());
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("Nothing to upload")
            );
        })
        .unwrap();
}

#[gpui::test]
fn the_upload_comment_survives_closing_the_dialog(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(|window, cx| {
        let view = cx.new(|cx| MapViewer::new(window, cx));
        gpui_component::Root::new(view, window, cx)
    });
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let root = window.root(vcx).unwrap();
    let view: gpui::Entity<MapViewer> = root
        .read_with(vcx, |root, _| root.view().clone())
        .downcast::<MapViewer>()
        .unwrap();
    view.update_in(vcx, |view, window, cx| {
        let layer_id = view.layer_manager.alloc_id();
        let layer = OsmLayer::new_with_data(
            layer_id,
            "L".to_string(),
            Arc::new(way_fixture(CENTER_LAT, CENTER_LON)),
        );
        view.layer_manager.add_layer(Box::new(layer));
        view.active_layer = Some(layer_id);
        make_pending_change(view);

        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        dialog.update(cx, |d, cx| {
            d.set_draft_comment("Add a cafe".into(), window, cx)
        });
        view.close_upload_dialog_keeping_draft(cx);
        assert!(view.upload_dialog.is_none());
        assert_eq!(view.upload_comment_draft, "Add a cafe");

        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog reopens");
        assert_eq!(dialog.read(cx).draft_comment(cx), "Add a cafe");
    });
}

#[gpui::test]
fn the_upload_dialog_validates_once_and_only_errors_block(cx: &mut TestAppContext) {
    use osm_gpui::validator::Check;
    let (view, vcx) = validation_view(cx);
    // A new, untagged, unconnected node: a warning. The fixture's own
    // untagged way 10 is an old problem and is not reported.
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        dialog.update(cx, |d, cx| {
            d.set_draft_comment("Add a node".into(), window, cx)
        });
        let d = dialog.read(cx);
        assert_eq!((d.validation().errors(), d.validation().warnings()), (0, 1));
        assert_eq!(d.validation().count_of(Check::UntaggedNode), 1);
        assert_eq!(d.validation().count_of(Check::UntaggedWay), 0);
        assert_eq!(d.confirm_blocker(cx), None, "warnings do not block");
        view.close_upload_dialog_keeping_draft(cx);
    });
    // Touch a node of way 10 so that it gets an empty tag value: an error.
    view.update_in(vcx, |view, window, cx| {
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .set_tag(osm_gpui::selection::FeatureKind::Node, 1, "note", "");
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog reopens");
        let d = dialog.read(cx);
        assert_eq!(d.validation().errors(), 1);
        assert_eq!(d.validation().count_of(Check::TagEmptyValue), 1);
        assert!(
            d.confirm_blocker(cx)
                .is_some_and(|why| why.contains("validator")),
            "an error blocks Upload"
        );
        // The dialog shows a one-line summary, not the list; its button
        // hands the list to the side panel.
        assert!(!view.validation_shown());
        dialog.update(cx, |d, cx| assert!(d.press_show_in_panel(cx)));
    });
    // "Show in side panel" closes the dialog (keeping the comment) and
    // opens the Validation section with the same findings.
    view.update_in(vcx, |view, _window, _cx| {
        assert!(view.upload_dialog.is_none());
        assert_eq!(view.upload_comment_draft, "Add a node");
        let report = view.shown_validation().expect("the section opens");
        assert_eq!((report.errors(), report.warnings()), (1, 1));
        assert!(view.side_panel_open[validation_panel::SECTION]);
    });
}

fn tag(
    dialog: &gpui::Entity<osm_gpui::ui::upload_dialog::UploadDialog>,
    key: &str,
    cx: &gpui::App,
) -> Option<String> {
    dialog
        .read(cx)
        .changeset_tags(cx)
        .get(key)
        .map(str::to_string)
}

#[gpui::test]
fn the_dialog_writes_source_review_locale_and_hashtags(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        // Nothing beyond created_by, the comment and the language.
        dialog.update(cx, |d, cx| {
            d.set_draft_comment("Add a cafe #cafe #Cafe #pizza".into(), window, cx)
        });
        assert_eq!(
            tag(&dialog, "comment", cx).as_deref(),
            Some("Add a cafe #cafe #Cafe #pizza")
        );
        assert_eq!(tag(&dialog, "source", cx), None);
        assert_eq!(tag(&dialog, "review_requested", cx), None);
        assert_eq!(tag(&dialog, "imagery_used", cx), None);
        assert_eq!(
            tag(&dialog, "hashtags", cx).as_deref(),
            Some("#cafe;#pizza")
        );
        assert_eq!(
            tag(&dialog, "locale", cx).as_deref(),
            Some(osm_gpui::i18n::current_locale().as_str())
        );

        // Chips append with "; " and never twice.
        dialog.update(cx, |d, cx| {
            d.press_source_chip("survey", window, cx);
            d.press_source_chip("GPS", window, cx);
            d.press_source_chip("survey", window, cx);
        });
        assert_eq!(dialog.read(cx).draft_source(cx), "survey; GPS");
        assert_eq!(tag(&dialog, "source", cx).as_deref(), Some("survey; GPS"));

        dialog.update(cx, |d, cx| d.set_review_requested(true, cx));
        assert_eq!(tag(&dialog, "review_requested", cx).as_deref(), Some("yes"));
        dialog.update(cx, |d, cx| d.set_review_requested(false, cx));
        assert_eq!(tag(&dialog, "review_requested", cx), None);
        assert_eq!(dialog.read(cx).confirm_blocker(cx), None);
    });
}

#[gpui::test]
/// A dialog that is not hooked to the app: pressing Upload only emits an
/// event that this test collects, so nothing can start an upload.
#[gpui::test]
fn the_upload_event_carries_the_same_tags_the_dialog_shows(cx: &mut TestAppContext) {
    use osm_gpui::ui::upload_dialog::{DialogEvent, UploadDialog};
    let (view, vcx) = validation_view(cx);
    let seen: std::rc::Rc<std::cell::RefCell<Vec<(String, String, String)>>> = Default::default();
    view.update_in(vcx, |view, window, cx| {
        let dialog = cx.new(|cx| UploadDialog::new(window, cx, Vec::new()));
        // Keep it alive until the event has been delivered.
        view.upload_dialog = Some(dialog.clone());
        let sink = seen.clone();
        cx.subscribe(&dialog, move |_this, _d, event: &DialogEvent, _cx| {
            if let DialogEvent::Upload {
                comment,
                source,
                tags,
            } = event
            {
                let review = tags.get("review_requested").unwrap_or("-").to_string();
                sink.borrow_mut().push((
                    comment.clone(),
                    format!("{source}|{}", tags.get("source").unwrap_or("-")),
                    review,
                ));
            }
        })
        .detach();
        dialog.update(cx, |d, cx| {
            d.set_draft_comment("  Fix roads ".into(), window, cx);
            d.set_draft_source(" survey ".into(), window, cx);
            d.set_review_requested(true, cx);
            assert_eq!(d.confirm_blocker(cx), None);
            d.press_confirm(cx);
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        *seen.borrow(),
        vec![(
            "Fix roads".to_string(),
            "survey|survey".to_string(),
            "yes".to_string()
        )]
    );
}

#[gpui::test]
fn the_preview_dialog_can_never_upload(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_preview(window, cx);
        let dialog = view.upload_dialog.clone().expect("preview opens");
        assert!(dialog.read(cx).is_preview());
        dialog.update(cx, |d, cx| {
            d.set_draft_comment("Add a node".into(), window, cx)
        });
        assert!(dialog
            .read(cx)
            .confirm_blocker(cx)
            .is_some_and(|why| why.contains("preview")));
        // Pressing Upload does nothing (the dialog stays editable and
        // no upload is under way).
        dialog.update(cx, |d, cx| d.press_confirm(cx));
        assert!(view.transfer.is_none());
        assert!(matches!(
            dialog.read(cx).phase(),
            osm_gpui::ui::upload_dialog::Phase::Editing
        ));
    });
}

#[gpui::test]
fn recent_comments_are_offered_and_picking_one_replaces_the_comment(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        dialog.update(cx, |d, cx| {
            // Nothing to open while there are no recent comments.
            d.toggle_recent_comments(cx);
            assert!(!d.recent_comments_open());
            d.set_recent_comments(vec!["Newest".into(), "Older".into()], cx);
            d.set_draft_comment("typing".into(), window, cx);
            d.toggle_recent_comments(cx);
            assert!(d.recent_comments_open());
            assert!(!d.pick_recent_comment(5, window, cx));
            assert!(d.pick_recent_comment(1, window, cx));
            assert!(!d.recent_comments_open());
            assert_eq!(d.draft_comment(cx), "Older");
        });
    });
}

#[gpui::test]
fn imagery_is_written_unless_cleared_and_only_visible_imagery_counts(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        // A visible and a hidden imagery layer, and a shifted one; the
        // OSM Carto map itself is not imagery.
        for (name, visible, east) in [
            ("Esri World Imagery", true, 0.0),
            ("Hidden Imagery", false, 0.0),
            ("Shifted Imagery", true, 3.0),
        ] {
            let id = view.layer_manager.alloc_id();
            let mut layer = TileLayer::new_with_name(id, name.to_string(), view.tile_cache.clone())
                .with_imagery_id(name)
                .with_offset(osm_gpui::imagery_offset::ImageryOffset::new(east, 0.0));
            osm_gpui::layers::MapLayer::set_visible(&mut layer, visible);
            view.layer_manager.add_layer(Box::new(layer));
        }
        let id = view.layer_manager.alloc_id();
        view.layer_manager
            .add_layer(Box::new(TileLayer::new(id, view.tile_cache.clone())));
        assert_eq!(
            view.visible_imagery_names(),
            vec!["Esri World Imagery", "Shifted Imagery"]
        );

        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        assert_eq!(
            tag(&dialog, "imagery_used", cx).as_deref(),
            Some("Esri World Imagery;Shifted Imagery")
        );
        // A shifted layer adds no extra tags.
        let tags = dialog.read(cx).changeset_tags(cx);
        let keys: Vec<&str> = tags.pairs().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["created_by", "imagery_used", "locale"]);
        dialog.update(cx, |d, cx| d.set_imagery_cleared(true, cx));
        assert_eq!(tag(&dialog, "imagery_used", cx), None);
        dialog.update(cx, |d, cx| d.set_imagery_cleared(false, cx));
        assert!(tag(&dialog, "imagery_used", cx).is_some());
    });
}

#[gpui::test]
fn a_comment_or_source_over_255_characters_blocks_upload(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        dialog.update(cx, |d, cx| {
            d.set_draft_comment("ok".into(), window, cx);
            d.set_draft_source("s".repeat(255), window, cx);
        });
        assert_eq!(dialog.read(cx).confirm_blocker(cx), None);
        dialog.update(cx, |d, cx| d.set_draft_source("s".repeat(256), window, cx));
        assert!(dialog
            .read(cx)
            .confirm_blocker(cx)
            .is_some_and(|why| why.contains("255")));
        // Even then, the tag that would be written is cut.
        assert_eq!(tag(&dialog, "source", cx).unwrap().chars().count(), 255);
        dialog.update(cx, |d, cx| d.set_draft_source(String::new(), window, cx));
        dialog.update(cx, |d, cx| d.set_draft_comment("c".repeat(256), window, cx));
        assert!(dialog.read(cx).confirm_blocker(cx).is_some());
    });
}

#[gpui::test]
fn the_source_is_prefilled_from_the_draft_and_the_review_box_is_not_kept(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        make_pending_change(view);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        dialog.update(cx, |d, cx| {
            d.press_source_chip("survey", window, cx);
            d.set_review_requested(true, cx);
        });
        view.close_upload_dialog_keeping_draft(cx);
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog reopens");
        assert_eq!(dialog.read(cx).draft_source(cx), "survey");
        assert!(!dialog.read(cx).review_requested());
    });
}

#[gpui::test]
fn the_upload_dialog_has_nothing_to_show_in_the_panel_without_findings(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, window, cx| {
        // An old, untouched untagged way is not reported; a tagged new
        // node is clean.
        let id = view.active_layer.unwrap();
        let layer = view
            .layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap();
        let node = layer.create_node(CENTER_LAT, CENTER_LON, None).unwrap();
        layer.set_tag(
            osm_gpui::selection::FeatureKind::Node,
            node,
            "amenity",
            "cafe",
        );
        view.open_upload_dialog_as(Some("me"), window, cx);
        let dialog = view.upload_dialog.clone().expect("dialog opens");
        assert!(dialog.read(cx).validation().is_empty());
        dialog.update(cx, |d, cx| assert!(!d.press_show_in_panel(cx)));
        assert!(view.upload_dialog.is_some());
        assert!(!view.validation_shown());
    });
}
