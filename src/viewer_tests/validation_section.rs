//! The validation section of the side panel.

use super::{make_pending_change, status_text, validation_view, CENTER_LAT, CENTER_LON};
use crate::undo::UndoableAction;
use crate::{side_panel, side_panel_layout, validation_panel, MapViewer};
use gpui::TestAppContext;

#[gpui::test]
fn tools_validate_shows_the_side_panel_section(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let (view, vcx) = validation_view(cx);
    // Never validated: no section, the panel is as before.
    view.update_in(vcx, |view, _window, _cx| {
        assert!(!view.validation_shown());
    });
    // Nothing pending: the section appears (collapsed by default, like
    // History) and says so.
    view.update_in(vcx, |view, _window, cx| {
        assert!(!view.side_panel_open[validation_panel::SECTION]);
        assert!(!view.side_panel_open[side_panel::HISTORY_SECTION]);
        view.show_validation(cx);
        assert!(!view.side_panel_open[validation_panel::SECTION]);
        let report = view.shown_validation().expect("shown");
        assert!(!report.checked_changes);
        assert_eq!(
            status_text(view),
            Some("There are no pending changes to check.")
        );
        assert!(!view.select_finding(0, cx), "no such row");
    });
    // A pending change with a problem: counts, status, and a click on the
    // row selects the node while the section stays.
    view.update_in(vcx, |view, _window, cx| {
        make_pending_change(view);
        view.show_validation(cx);
        assert_eq!(view.shown_validation().unwrap().warnings(), 1);
        assert_eq!(status_text(view), Some("1 warning"));
        assert!(view.highlighted_finding().is_none());
        assert!(view.select_finding(0, cx));
        assert!(view.validation_shown());
        assert_eq!(view.selected.len(), 1);
        assert_eq!(view.selected[0].kind, FeatureKind::Node);
        assert!(view.selected[0].id < 0, "the new node");
        assert_eq!(view.highlighted_finding(), Some(0));
        // Selecting something else drops the highlight.
        view.set_selection(vec![]);
        assert_eq!(view.highlighted_finding(), None);
    });
    // The close button removes it.
    view.update_in(vcx, |view, _window, cx| {
        view.close_validation(cx);
        assert!(!view.validation_shown());
    });
}

#[gpui::test]
fn the_validation_section_keeps_the_users_choice_of_open_or_collapsed(cx: &mut TestAppContext) {
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, _window, cx| {
        make_pending_change(view);
        view.show_validation(cx);
        assert!(!view.side_panel_open[validation_panel::SECTION]);
        // The user expands it (what a click on its header does): later
        // runs, and a close and reopen, leave it expanded.
        view.side_panel_open[validation_panel::SECTION] = true;
        view.show_validation(cx);
        assert!(view.side_panel_open[validation_panel::SECTION]);
        view.close_validation(cx);
        view.show_validation(cx);
        assert!(view.side_panel_open[validation_panel::SECTION]);
        // Collapsed by the user: stays collapsed too.
        view.side_panel_open[validation_panel::SECTION] = false;
        view.show_validation(cx);
        assert!(!view.side_panel_open[validation_panel::SECTION]);
        // The explicit "Show in side panel" expands it.
        view.show_validation_expanded(cx);
        assert!(view.side_panel_open[validation_panel::SECTION]);
    });
}

#[gpui::test]
fn the_collapsed_header_counts_follow_edits(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, _window, cx| {
        make_pending_change(view);
        record_edit(view);
        view.show_validation(cx);
        assert!(!view.side_panel_open[validation_panel::SECTION]);
        assert_eq!(view.shown_validation().unwrap().warnings(), 1);
        assert!(view.select_finding(0, cx));
    });
    vcx.run_until_parked();
    // Fix the problem while the section is collapsed: the header's
    // counts (read from the report) still refresh by themselves.
    view.update_in(vcx, |view, _window, cx| {
        let node = view.selected[0].id;
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .set_tag(FeatureKind::Node, node, "amenity", "cafe")
            .unwrap();
        record_edit(view);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.executor()
        .advance_clock(validation_panel::REFRESH_DELAY * 2);
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| {
        assert!(!view.side_panel_open[validation_panel::SECTION]);
        assert_eq!(view.shown_validation().unwrap().warnings(), 0);
    });
}

/// Stand-in for the bookkeeping every real edit does: the history step
/// bumps `undo_stack.revision`, which is what the watcher looks at.
fn record_edit(view: &mut MapViewer) {
    view.undo_stack
        .push(UndoableAction::set_tags(vec![]));
}

#[gpui::test]
fn the_validation_section_refreshes_itself_after_edits(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind;
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, _window, cx| {
        make_pending_change(view);
        record_edit(view);
        view.show_validation(cx);
        assert_eq!(view.shown_validation().unwrap().warnings(), 1);
        assert!(view.select_finding(0, cx));
        assert_eq!(view.highlighted_finding(), Some(0));
    });
    vcx.run_until_parked();

    // Fix the problem (tag the new node). Nothing happens at once: the
    // refresh waits for the debounce.
    let node = view.update_in(vcx, |view, _window, cx| {
        let node = view.selected[0].id;
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .set_tag(FeatureKind::Node, node, "amenity", "cafe")
            .unwrap();
        record_edit(view);
        cx.notify();
        node
    });
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| {
        assert_eq!(view.shown_validation().unwrap().warnings(), 1, "not yet");
    });
    vcx.executor()
        .advance_clock(validation_panel::REFRESH_DELAY / 2);
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| {
        assert_eq!(
            view.shown_validation().unwrap().warnings(),
            1,
            "still waiting"
        );
    });
    vcx.executor()
        .advance_clock(validation_panel::REFRESH_DELAY);
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| {
        let report = view.shown_validation().unwrap();
        assert_eq!(report.warnings(), 0, "the fixed problem is gone");
        assert!(report.checked_changes);
        assert_eq!(view.highlighted_finding(), None);
    });

    // A new problem appears by itself; a burst of edits is one refresh.
    view.update_in(vcx, |view, _w, cx| {
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .set_tag(FeatureKind::Node, node, "note", "")
            .unwrap();
        record_edit(view);
        cx.notify();
    });
    vcx.run_until_parked();
    for _ in 0..3 {
        vcx.executor()
            .advance_clock(validation_panel::REFRESH_DELAY / 2);
        vcx.run_until_parked();
        view.update_in(vcx, |view, _w, cx| {
            record_edit(view);
            cx.notify();
        });
        vcx.run_until_parked();
    }
    view.update_in(vcx, |view, _w, _cx| {
        assert_eq!(
            view.shown_validation().unwrap().errors(),
            0,
            "edits keep pushing the refresh back"
        );
    });
    vcx.executor()
        .advance_clock(validation_panel::REFRESH_DELAY * 2);
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| {
        assert_eq!(view.shown_validation().unwrap().errors(), 1);
    });
}

#[gpui::test]
fn the_validation_section_ignores_changes_while_hidden_and_keeps_the_highlight(
    cx: &mut TestAppContext,
) {
    use osm_gpui::selection::FeatureKind;
    let (view, vcx) = validation_view(cx);
    view.update_in(vcx, |view, _window, cx| {
        // Two warnings: a stray node and another.
        make_pending_change(view);
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .create_node(CENTER_LAT + 0.0005, CENTER_LON, None)
            .unwrap();
        record_edit(view);
        view.show_validation(cx);
        assert_eq!(view.shown_validation().unwrap().warnings(), 2);
        // Click the second row; the first is then fixed (deleted).
        assert!(view.select_finding(1, cx));
        assert_eq!(view.highlighted_finding(), Some(1));
    });
    view.update_in(vcx, |view, _w, cx| {
        let first = view.shown_validation().unwrap().entries[0].finding.features[0];
        let id = view.active_layer.unwrap();
        view.layer_manager
            .find_layer_mut(id)
            .and_then(|l| l.as_editable_mut())
            .unwrap()
            .delete_feature(FeatureKind::Node, first.1)
            .expect("deleted");
        record_edit(view);
        cx.notify();
    });
    vcx.executor()
        .advance_clock(validation_panel::REFRESH_DELAY * 2);
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| {
        assert_eq!(view.shown_validation().unwrap().warnings(), 1);
        // The clicked finding moved up to row 0 and is still the
        // highlighted one: findings are matched by check and objects.
        assert_eq!(view.highlighted_finding(), Some(0));
    });
    // Closed: edits do not schedule anything.
    view.update_in(vcx, |view, _w, cx| {
        view.close_validation(cx);
        record_edit(view);
        cx.notify();
    });
    vcx.run_until_parked();
    view.update_in(vcx, |view, _w, _cx| assert!(!view.validation_shown()));
}

#[test]
fn the_findings_list_gets_a_bounded_share_of_the_window() {
    use side_panel_layout::validation_cap;
    assert_eq!(validation_cap(800.0), 224.0);
    assert_eq!(validation_cap(300.0), 84.0);
    assert_eq!(validation_cap(2000.0), 260.0);
}
