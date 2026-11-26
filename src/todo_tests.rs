//! The to-do list in the window: adding the selection, visiting entries and
//! marking them done (the list itself is tested in `osm_gpui::todo`).

use crate::test_support::{with_session, Build, N, W};
use gpui::TestAppContext;
use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::transfer::{TransferHandle, TransferKind, TransferPhase};

fn data() -> osm_gpui::osm::OsmData {
    Build::new()
        .nodes(&[1, 2, 3, 4, 5, 6])
        .way(10, &[1, 2], &[("highway", "path"), ("name", "First")])
        .way(11, &[3, 4], &[("highway", "path")])
        .way(12, &[5, 6], &[("highway", "path")])
        .done()
}

fn listed(s: &crate::test_support::Session<'_, '_>) -> Vec<i64> {
    s.view.todo.list.items().iter().map(|f| f.id).collect()
}

fn selected(s: &crate::test_support::Session<'_, '_>) -> Vec<(FeatureKind, i64)> {
    s.view.selected.iter().map(|f| (f.kind, f.id)).collect()
}

#[gpui::test]
fn adding_the_selection_lists_it_and_shows_the_section(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        assert!(!s.view.shows_todo());
        s.select(&[(W, 10), (W, 11)]);
        assert!(!s.act("todo_add"), "no undo step: the list is not an edit");
        assert_eq!(listed(s), [10, 11]);
        assert!(s.view.shows_todo());
        assert!(s
            .view
            .side_panel_open
            .is_open(crate::side_panel::SidePanelSection::Todo));
        // Adding again: nothing new, and the action no longer applies.
        s.select(&[(W, 10)]);
        s.act("todo_add");
        assert_eq!(listed(s), [10, 11]);
        assert_eq!(
            s.status(),
            osm_gpui::tr!("action.todo_add.hint").to_string()
        );
    });
}

#[gpui::test]
fn done_moves_on_to_the_next_entry_and_ends_with_nothing_selected(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        s.select(&[(W, 10), (W, 11), (W, 12)]);
        s.act("todo_add");

        // Done & Next with no entry selected starts at the first.
        s.select(&[(N, 1)]);
        s.view.todo_done_and_next(s.cx);
        assert_eq!(selected(s), [(W, 10)]);

        s.act("todo_done");
        assert_eq!(listed(s), [11, 12]);
        assert_eq!(selected(s), [(W, 11)]);

        s.view.todo_done_and_next(s.cx);
        assert_eq!(selected(s), [(W, 12)]);
        s.view.todo_done_and_next(s.cx);
        assert!(s.view.todo.list.is_empty());
        assert!(s.view.selected.is_empty());
        assert_eq!(
            s.status(),
            osm_gpui::tr!("todo.status.all_done").to_string()
        );
    });
}

#[gpui::test]
fn the_to_do_actions_run_during_an_upload(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        s.view.transfer = Some(TransferHandle::new(
            TransferKind::Upload,
            TransferPhase::OpeningChangeset,
        ));
        s.select(&[(W, 10)]);
        s.act("todo_add");
        assert_eq!(listed(s), [10]);
        s.act("todo_done");
        assert!(s.view.todo.list.is_empty());
    });
}

#[gpui::test]
fn a_deleted_feature_stays_listed_until_done_and_is_not_visited(cx: &mut TestAppContext) {
    with_session(cx, data(), |s| {
        s.select(&[(W, 11), (W, 12)]);
        s.act("todo_add");
        s.select(&[(W, 11)]);
        assert!(s.act("delete"));
        s.view.set_selection(Vec::new());
        // Starting at the first entry, which is gone: it is not selected.
        s.view.todo_done_and_next(s.cx);
        assert!(s.view.selected.is_empty());
        assert_eq!(s.status(), osm_gpui::tr!("todo.status.gone").to_string());
        assert_eq!(listed(s), [11, 12]);
        // Its row's Done takes it off.
        let gone = FeatureRef {
            layer_id: s.layer,
            kind: FeatureKind::Way,
            id: 11,
        };
        s.view.todo.list.mark_done(&[gone]);
        assert_eq!(listed(s), [12]);
    });
}
