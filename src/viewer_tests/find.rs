//! The Find dialog and applying a query to the selection.

use super::{setup, CENTER_LAT, CENTER_LON};
use crate::{Find, MapViewer};
use gpui::TestAppContext;

fn selected_kind_ids(view: &MapViewer) -> Vec<(osm_gpui::selection::FeatureKind, i64)> {
    view.selected.iter().map(|f| (f.kind, f.id)).collect()
}

#[gpui::test]
fn find_applies_each_mode_to_the_selection(cx: &mut TestAppContext) {
    use osm_gpui::search::SearchMode;
    use osm_gpui::selection::FeatureKind::{Node, Way};
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            assert_eq!(view.apply_find("type:node", SearchMode::Replace), Ok(2));
            assert_eq!(selected_kind_ids(view), vec![(Node, 1), (Node, 2)]);
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("2 features selected")
            );

            assert_eq!(view.apply_find("type:way", SearchMode::Add), Ok(1));
            assert_eq!(
                selected_kind_ids(view),
                vec![(Node, 1), (Node, 2), (Way, 10)]
            );

            assert_eq!(view.apply_find("id:1", SearchMode::Remove), Ok(1));
            assert_eq!(selected_kind_ids(view), vec![(Node, 2), (Way, 10)]);

            // Only what is selected is searched.
            assert_eq!(
                view.apply_find("type:way", SearchMode::FindInSelection),
                Ok(1)
            );
            assert_eq!(selected_kind_ids(view), vec![(Way, 10)]);

            assert_eq!(view.apply_find("type:node", SearchMode::Replace), Ok(2));
            assert_eq!(
                view.apply_find("selected", SearchMode::FindInSelection),
                Ok(2)
            );
            assert_eq!(view.selected.len(), 2);
        })
        .unwrap();
}

#[gpui::test]
fn find_with_no_match_says_so_and_a_bad_query_changes_nothing(cx: &mut TestAppContext) {
    use osm_gpui::search::{ParseErrorKind, SearchMode};
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            view.apply_find("type:node", SearchMode::Replace).unwrap();
            assert_eq!(view.apply_find("nothing=here", SearchMode::Replace), Ok(0));
            assert!(view.selected.is_empty());
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("No matches")
            );

            view.apply_find("type:node", SearchMode::Replace).unwrap();
            let err = view
                .apply_find("(type:node", SearchMode::Replace)
                .unwrap_err();
            assert_eq!(err.kind, ParseErrorKind::UnclosedParen);
            assert_eq!(view.selected.len(), 2, "a bad query leaves the selection");
        })
        .unwrap();
}

#[gpui::test]
fn find_moves_the_view_only_when_nothing_selected_is_in_view(cx: &mut TestAppContext) {
    use osm_gpui::search::SearchMode;
    let window = setup(cx);
    window
        .update(cx, |view, _window, _cx| {
            let before = view.viewport.center();
            view.apply_find("type:node", SearchMode::Replace).unwrap();
            assert_eq!(view.viewport.center(), before, "already in view");

            view.viewport.pan_to(CENTER_LAT + 5.0, CENTER_LON + 5.0);
            let far = view.viewport.center();
            view.apply_find("type:node", SearchMode::Replace).unwrap();
            assert_ne!(view.viewport.center(), far);
            let (lat, lon) = view.viewport.center();
            assert!((lat - CENTER_LAT).abs() < 0.01 && (lon - CENTER_LON).abs() < 0.01);
        })
        .unwrap();
}

#[gpui::test]
fn find_without_a_data_layer_explains_in_the_status_bar(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let window = cx.add_window(MapViewer::new);
    window
        .update(cx, |view, window, cx| {
            view.on_find(&Find, window, cx);
            assert!(view.find_dialog.is_none());
            assert_eq!(
                view.status_message.as_ref().map(|(m, _)| m.as_str()),
                Some("There is no data layer to search")
            );
        })
        .unwrap();
}

#[gpui::test]
fn find_dialog_applies_on_submit_and_leaves_the_selection_on_cancel(cx: &mut TestAppContext) {
    use osm_gpui::search::SearchMode;
    let window = setup(cx);
    window
        .update(cx, |view, window, cx| {
            view.apply_find("type:way", SearchMode::Replace).unwrap();
            view.on_find(&Find, window, cx);
            let dialog = view.find_dialog.clone().expect("Find opens a dialog");
            dialog.update(cx, |d, cx| d.set_query("type:node", window, cx));
            dialog.update(cx, |d, cx| d.press_cancel(cx));
        })
        .unwrap();
    window
        .update(cx, |view, window, cx| {
            assert!(view.find_dialog.is_none());
            assert_eq!(view.selected.len(), 1, "cancel leaves the selection alone");
            assert!(view.find_history.is_empty());

            view.on_find(&Find, window, cx);
            let dialog = view.find_dialog.clone().unwrap();
            dialog.update(cx, |d, cx| {
                d.set_mode(SearchMode::Add, cx);
                d.set_query("type:node", window, cx);
                d.press_apply(cx);
            });
        })
        .unwrap();
    window
        .update(cx, |view, _window, _cx| {
            assert!(view.find_dialog.is_none());
            assert_eq!(view.selected.len(), 3, "added to the way");
            assert_eq!(view.find_history.entries(), ["type:node"]);
            assert_eq!(view.find_mode, SearchMode::Add);
        })
        .unwrap();
}

#[gpui::test]
fn find_dialog_stays_open_on_a_bad_query_and_counts_matches(cx: &mut TestAppContext) {
    use osm_gpui::search::SearchMode;
    use osm_gpui::ui::find_dialog::Preview;
    let window = setup(cx);
    // One update: a window with the dialog open can't be drawn in a
    // test (its input needs the app's root view).
    window
        .update(cx, |view, window, cx| {
            assert_eq!(
                view.find_match_count("type:node", SearchMode::Replace),
                Some(2)
            );
            assert_eq!(
                view.find_match_count("(type:node", SearchMode::Replace),
                None
            );
            assert_eq!(
                view.find_match_count("type:node", SearchMode::FindInSelection),
                Some(0),
                "nothing is selected to search in"
            );

            view.on_find(&Find, window, cx);
            let dialog = view.find_dialog.clone().unwrap();
            dialog.update(cx, |d, cx| {
                d.set_query("(type:node", window, cx);
                d.press_apply(cx);
                assert!(matches!(d.preview(), Preview::Error(_)));
                assert!(d.preview_text().starts_with("Position 1:"));
                d.set_query("type:node", window, cx);
                d.set_count("type:node", 2, cx);
                assert_eq!(d.preview_text().as_str(), "2 matches");
                // A stale answer is ignored.
                d.set_count("other", 9, cx);
                assert_eq!(d.preview_text().as_str(), "2 matches");
                d.press_cancel(cx);
            });
        })
        .unwrap();
}

#[gpui::test]
fn find_opened_from_the_menu_gets_focus_in_its_query_field(cx: &mut TestAppContext) {
    let window = setup(cx);
    // One update: a window with the dialog open can't be drawn in a test.
    window
        .update(cx, |view, window, cx| {
            view.on_find(&Find, window, cx);
            let map = view.focus_handle.clone();
            window.focus(&map, cx);
            let dialog = view.find_dialog.clone().expect("Find opens a dialog");
            let query = osm_gpui::ui::modal::ModalDialog::initial_focus(dialog.read(cx), cx);
            assert!(!query.is_focused(window), "the map took focus");
            // A window with the dialog open can't be drawn in a test (its
            // input needs the app's root view), so the host's rule runs
            // directly, as `render` does.
            view.drawn_once = true;
            assert!(view.keep_dialog_focus(window, cx));
            assert!(query.is_focused(window), "typing reaches the query");
            // Focus already inside: left alone.
            assert!(!view.keep_dialog_focus(window, cx));
            // Close it again so the window can be drawn at the end of
            // this update. The Escape keystroke can't be sent either (it
            // draws): the delete-layer test above covers the keystroke,
            // the script run covers Find's.
            view.find_dialog = None;
        })
        .unwrap();
}
