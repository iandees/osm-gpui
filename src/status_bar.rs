//! The status bar along the bottom of the main window.
//!
//! Left side: what the app is doing or expects next — a running background
//! operation (with a spinner), else the latest transient status message,
//! else a hint for the current edit mode. Right side: pending-edit count,
//! signed-in account, cursor coordinates, and zoom level.

use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    progress::Progress,
    spinner::Spinner,
    ActiveTheme, Sizable,
};

use osm_gpui::auth;
use osm_gpui::settings_store;
use osm_gpui::tr;
use osm_gpui::tr_count;
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
    oriented: bool,
) -> SharedString {
    match (mode, progress) {
        (EditMode::Building, ModeProgress::Idle) if oriented => {
            tr!("status.hint.building_oriented_idle")
        }
        (EditMode::Building, _) if oriented => tr!("status.hint.building_oriented_started"),
        (EditMode::Select, _) if has_selection => tr!("status.hint.select_selection"),
        (EditMode::Select, _) => tr!("status.hint.select_empty"),
        (EditMode::Add, ModeProgress::Idle) => tr!("status.hint.add_idle"),
        (EditMode::Add, _) => tr!("status.hint.add_started"),
        (EditMode::Building, ModeProgress::Idle) => tr!("status.hint.building_idle"),
        (EditMode::Building, ModeProgress::Started) => tr!("status.hint.building_started"),
        (EditMode::Building, ModeProgress::EdgePlaced) => tr!("status.hint.building_edge_placed"),
        (EditMode::Extrude, _) => tr!("status.hint.extrude"),
    }
}

/// "N unsaved edit(s)", or `None` when there's nothing to upload.
pub(crate) fn pending_edits_label(count: usize) -> Option<SharedString> {
    match count {
        0 => None,
        n => Some(tr_count!("status.pending_edits", n)),
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

        let left = if let Some(progress) = self.transfer.as_ref().map(|t| t.snapshot()) {
            let cancellable = progress.is_cancellable();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1p5()
                .text_color(foreground)
                .child(Spinner::new().xsmall())
                .child(progress.label())
                // A known total (the server sent Content-Length) gets a
                // determinate bar; otherwise the spinner and byte count do.
                .children(progress.fraction().map(|fraction| {
                    Progress::new("transfer-progress")
                        .w(px(80.0))
                        .h(px(4.0))
                        .value(fraction * 100.0)
                }))
                .when(cancellable, |row| {
                    row.child(
                        Button::new("transfer-cancel")
                            .ghost()
                            .xsmall()
                            .label(tr!("transfer.download.cancel"))
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_transfer(cx))),
                    )
                })
        } else if let Some((message, _)) = self.status_message.clone() {
            div().text_color(foreground).child(message)
        } else if self.mode == EditMode::Select
            && !self.selected.is_empty()
            && self
                .selected
                .iter()
                .all(|f| f.kind == osm_gpui::selection::FeatureKind::Relation)
        {
            // Relations don't move or delete; say what does work instead.
            div().child(tr!("status.hint.select_relation"))
        } else {
            div().child(mode_hint(
                self.mode,
                self.mode_progress(),
                !self.selected.is_empty(),
                self.building_reference.is_some(),
            ))
        };

        let pending = pending_edits_label(self.pending_edit_count());
        let account =
            auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url()));
        let cursor = self
            .last_mouse_pos
            .map(|position| self.viewport.screen_to_geo(position));
        // The OSM API refuses downloads above the server's maximum area; say
        // so up front rather than only after a Download attempt fails.
        let too_large_to_download =
            crate::welcome::download_state(&self.viewport.visible_bounds(), false)
                == crate::welcome::DownloadState::ZoomIn;

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
                bar.child(segment().child(tr!("status.zoom_in_to_download")))
            })
            .children(pending.map(|label| segment().text_color(foreground).child(label)))
            .child(segment().child(match account {
                Some(name) => name.into(),
                None => tr!("status.not_signed_in"),
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
        let empty = mode_hint(EditMode::Select, ModeProgress::Idle, false, false);
        let selected = mode_hint(EditMode::Select, ModeProgress::Idle, true, false);
        assert!(empty.starts_with("Click to select"));
        assert!(selected.contains("Esc to deselect"));
    }

    #[test]
    fn drawing_hints_follow_progress() {
        assert!(mode_hint(EditMode::Add, ModeProgress::Idle, false, false).contains("start a way"));
        assert!(mode_hint(EditMode::Add, ModeProgress::Started, false, false).contains("finish"));
        assert!(
            mode_hint(EditMode::Building, ModeProgress::Idle, false, false)
                .contains("first corner")
        );
        assert!(
            mode_hint(EditMode::Building, ModeProgress::Started, false, false)
                .contains("second corner")
        );
        assert!(
            mode_hint(EditMode::Building, ModeProgress::EdgePlaced, false, false).contains("depth")
        );
    }

    #[test]
    fn oriented_building_hints() {
        let idle = mode_hint(EditMode::Building, ModeProgress::Idle, true, true);
        assert!(idle.contains("Click or drag"));
        assert!(idle.contains("aligned to the selected way"));
        let started = mode_hint(EditMode::Building, ModeProgress::Started, true, true);
        assert!(started.contains("opposite corner"));
        // Orientation is irrelevant outside Building mode.
        assert_eq!(
            mode_hint(EditMode::Select, ModeProgress::Idle, true, true),
            mode_hint(EditMode::Select, ModeProgress::Idle, true, false)
        );
    }

    #[test]
    fn every_hint_resolves_to_a_message() {
        for mode in [
            EditMode::Select,
            EditMode::Add,
            EditMode::Building,
            EditMode::Extrude,
        ] {
            for progress in [
                ModeProgress::Idle,
                ModeProgress::Started,
                ModeProgress::EdgePlaced,
            ] {
                for (has_selection, oriented) in
                    [(false, false), (true, false), (false, true), (true, true)]
                {
                    let hint = mode_hint(mode, progress, has_selection, oriented);
                    // A missing key renders as "<locale>.<key>".
                    assert!(!hint.starts_with("en.status."), "{hint}");
                    assert!(hint.contains(' '), "{hint}");
                }
            }
        }
    }

    #[test]
    fn pending_edits_label_pluralizes() {
        assert_eq!(pending_edits_label(0), None);
        assert_eq!(pending_edits_label(1).as_deref(), Some("1 unsaved edit"));
        assert_eq!(pending_edits_label(3).as_deref(), Some("3 unsaved edits"));
    }
}
