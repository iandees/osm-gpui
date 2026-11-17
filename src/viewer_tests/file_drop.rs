//! Files dropped on the window from Finder.

use super::{select, setup_network};
use crate::MapViewer;
use gpui::{point, px, size, FileDropEvent, TestAppContext};

/// gpui turns a drop into a left release with no press. Over the map it must
/// not also act as a click of the map: here, applying a Rotate in progress.
#[gpui::test]
fn a_dropped_file_does_not_click_the_map(cx: &mut TestAppContext) {
    use osm_gpui::selection::FeatureKind::Way;
    let window = setup_network(cx);
    window
        .update(cx, |view, _, cx| {
            select(view, Way, &[100]);
            view.run_selection_action("rotate", cx);
            view.pointer.rotate_session.as_mut().unwrap().moved = true;
        })
        .unwrap();
    let vcx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    let root = window.root(vcx).unwrap();
    vcx.draw(
        point(px(0.0), px(0.0)),
        size(
            gpui::AvailableSpace::Definite(px(1200.0)),
            gpui::AvailableSpace::Definite(px(800.0)),
        ),
        |_, _| gpui::AnyView::from(root),
    );
    let pos = point(px(MapViewer::MODE_PANEL_WIDTH + 200.0), px(200.0));
    let paths =
        gpui::ExternalPaths(vec![std::path::PathBuf::from("/nonexistent/dropped.gpx")].into());
    vcx.simulate_event(FileDropEvent::Entered {
        position: pos,
        paths,
    });
    vcx.simulate_event(FileDropEvent::Pending { position: pos });
    vcx.simulate_event(FileDropEvent::Submit { position: pos });
    window
        .update(vcx, |view, _, cx| {
            assert!(
                view.pointer.rotate_session.is_some(),
                "the drop applied the rotation"
            );
            assert_eq!(view.undo_stack.actions.len(), 0);
            assert!(!cx.has_active_drag());
        })
        .unwrap();
}
