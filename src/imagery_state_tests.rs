//! The Editor Layer Index load reports to the viewer when it finishes, which
//! then holds the state itself.

use crate::test_support::with_view;
use crate::ImageryLoadState;
use gpui::TestAppContext;

#[gpui::test]
fn the_viewer_holds_the_imagery_load_state_the_load_reports(cx: &mut TestAppContext) {
    with_view(cx, |view, _window, cx| {
        assert_eq!(view.imagery_load_state, ImageryLoadState::Loading);
        view.imagery_index_loaded(Err("offline".into()), cx);
        assert_eq!(view.imagery_load_state, ImageryLoadState::Failed);
        assert_eq!(view.last_imagery_load_state, Some(ImageryLoadState::Failed));
        assert_eq!(view.imagery_menu_context().2, ImageryLoadState::Failed);
        view.imagery_index_loaded(Ok(Vec::new()), cx);
        assert_eq!(view.imagery_load_state, ImageryLoadState::Ready);
        assert_eq!(view.last_imagery_load_state, Some(ImageryLoadState::Ready));
    });
}
