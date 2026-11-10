//! The status bar along the bottom of the main window.
//!
//! Left side: what the app is doing or expects next — a running background
//! operation (with a spinner), else the latest transient status message,
//! else a hint for the current edit mode. Right side: pending-edit count,
//! signed-in account, cursor coordinates, and zoom level.

use gpui::{div, prelude::*, px, AnyElement, Context};
use gpui_component::{spinner::Spinner, ActiveTheme, Sizable};

use osm_gpui::auth;
use osm_gpui::settings_store;
use osm_gpui::ui::style::muted_text_size;

use crate::{EditMode, MapViewer};

/// Fixed height of the status bar, shared with the map-size math in
/// `MapViewer::render`.
pub(crate) const STATUS_BAR_HEIGHT: f32 = 24.0;

/// How far along the current mode's multi-click gesture is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeProgress {
    /// Nothing placed yet.
    Idle,
    /// Add: a way is being drawn. Building: the first corner is placed.
    Started,
    /// Building only: both corners of the first edge are placed.
    EdgePlaced,
}

/// One-line description of what the mouse and keyboard do right now.
pub(crate) fn mode_hint(
    mode: EditMode,
    progress: ModeProgress,
    has_selection: bool,
) -> &'static str {
    match (mode, progress) {
        (EditMode::Select, _) if has_selection => {
            "Drag the selection to move it · Delete to remove · Esc to deselect"
        }
        (EditMode::Select, _) => {
            "Click to select · Shift-click to add to the selection · drag to box-select · Space-drag or right-drag to pan"
        }
        (EditMode::Add, ModeProgress::Idle) => {
            "Click to place a node or start a way · Ctrl-click to place without snapping · Esc to exit"
        }
        (EditMode::Add, _) => "Click to continue the way · Enter or Esc to finish",
        (EditMode::Building, ModeProgress::Idle) => {
            "Click to place the building's first corner · Esc to exit"
        }
        (EditMode::Building, ModeProgress::Started) => {
            "Click to place the second corner · Esc to cancel"
        }
        (EditMode::Building, ModeProgress::EdgePlaced) => {
            "Move to set the depth, then click to finish · Esc to cancel"
        }
        (EditMode::Extrude, _) => {
            "Drag a way segment to extrude it · double-click a segment to add a node · Esc to exit"
        }
    }
}

/// "N unsaved edit(s)", or `None` when there's nothing to upload.
pub(crate) fn pending_edits_label(count: usize) -> Option<String> {
    match count {
        0 => None,
        1 => Some("1 unsaved edit".to_string()),
        n => Some(format!("{n} unsaved edits")),
    }
}

impl MapViewer {
    fn mode_progress(&self) -> ModeProgress {
        match self.mode {
            EditMode::Add if self.add_progress.is_some() => ModeProgress::Started,
            EditMode::Building => match self.building_progress {
                None => ModeProgress::Idle,
                Some(progress) if progress.corner_b.is_none() => ModeProgress::Started,
                Some(_) => ModeProgress::EdgePlaced,
            },
            _ => ModeProgress::Idle,
        }
    }

    /// Total created + modified + deleted objects across all layers. The
    /// underlying diff walks the whole dataset, so it's cached and only
    /// recomputed after an edit, undo/redo, upload, or layer change.
    fn pending_edit_count(&mut self) -> usize {
        let key = (self.undo_stack.revision, self.data_generation);
        if let Some((cached_key, count)) = self.pending_edits {
            if cached_key == key {
                return count;
            }
        }
        let count = self
            .layer_manager
            .layers()
            .iter()
            .filter(|layer| layer.is_modified())
            .map(|layer| {
                let (created, modified, deleted) = layer.diff_for_upload().counts();
                created + modified + deleted
            })
            .sum();
        self.pending_edits = Some((key, count));
        count
    }

    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;

        let left = if let Some(activity) = self.activity.clone() {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .text_color(foreground)
                .child(Spinner::new().xsmall())
                .child(activity)
        } else if let Some((message, _)) = self.status_message.clone() {
            div().text_color(foreground).child(message)
        } else {
            div().child(mode_hint(
                self.mode,
                self.mode_progress(),
                !self.selected.is_empty(),
            ))
        };

        let pending = pending_edits_label(self.pending_edit_count());
        let account =
            auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
        let cursor = self
            .last_mouse_pos
            .map(|position| self.viewport.screen_to_geo(position));
        // The OSM API refuses downloads above a maximum area; say so up
        // front rather than only after a Download attempt fails.
        let too_large_to_download =
            osm_gpui::osm_api::check_area(&self.viewport.visible_bounds()).is_err();

        let segment = || div().flex_shrink_0();

        div()
            .id("status-bar")
            .h(px(STATUS_BAR_HEIGHT))
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .px_3()
            .bg(cx.theme().sidebar)
            .border_t_1()
            .border_color(cx.theme().border)
            .text_size(muted_text_size())
            .text_color(muted)
            .child(
                left.flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis(),
            )
            .when(too_large_to_download, |bar| {
                bar.child(segment().child("Zoom in to download"))
            })
            .children(pending.map(|label| segment().text_color(foreground).child(label)))
            .child(segment().child(match account {
                Some(name) => name,
                None => "Not signed in".to_string(),
            }))
            .children(cursor.map(|(lat, lon)| {
                // Fixed width so the segments to its left don't jitter as
                // the digits change.
                segment()
                    .w(px(150.0))
                    .text_right()
                    .child(format!("{lat:.5}, {lon:.5}"))
            }))
            .child(segment().child(format!("z{:.1}", self.viewport.zoom_level())))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_hint_depends_on_selection() {
        let empty = mode_hint(EditMode::Select, ModeProgress::Idle, false);
        let selected = mode_hint(EditMode::Select, ModeProgress::Idle, true);
        assert!(empty.starts_with("Click to select"));
        assert!(selected.contains("Esc to deselect"));
    }

    #[test]
    fn drawing_hints_follow_progress() {
        assert!(mode_hint(EditMode::Add, ModeProgress::Idle, false).contains("start a way"));
        assert!(mode_hint(EditMode::Add, ModeProgress::Started, false).contains("finish"));
        assert!(mode_hint(EditMode::Building, ModeProgress::Idle, false).contains("first corner"));
        assert!(
            mode_hint(EditMode::Building, ModeProgress::Started, false).contains("second corner")
        );
        assert!(mode_hint(EditMode::Building, ModeProgress::EdgePlaced, false).contains("depth"));
    }

    #[test]
    fn pending_edits_label_pluralizes() {
        assert_eq!(pending_edits_label(0), None);
        assert_eq!(pending_edits_label(1).as_deref(), Some("1 unsaved edit"));
        assert_eq!(pending_edits_label(3).as_deref(), Some("3 unsaved edits"));
    }
}
