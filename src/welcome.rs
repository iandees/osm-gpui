//! First-run help: the default imagery layer and the welcome card shown over
//! the map while no OSM data is loaded.
//!
//! The decisions ("add Carto?", "show the card?", "can we download here?")
//! are plain functions so they can be tested without a window.

use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme, Disableable, IconName, Sizable as _,
};
use osm_gpui::coordinates::GeoBounds;
use osm_gpui::layers::{tile_layer::TileLayer, LayerManager};
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;

use crate::{MapViewer, OpenOsmFile};

/// Name of the built-in OSM Carto imagery layer.
pub(crate) const CARTO_LAYER_NAME: &str = "OpenStreetMap Carto";

/// Whether the standard OSM Carto layer should be added: only when the layer
/// stack doesn't already have one.
pub(crate) fn needs_default_imagery(layers: &LayerManager) -> bool {
    layers.layer_named(CARTO_LAYER_NAME).is_none()
}

/// Whether the welcome card shows: until some layer holds editable OSM data
/// (downloaded or opened), and never once the user has dismissed it.
/// Imagery and grid layers don't count as data.
pub(crate) fn show_welcome(layers: &LayerManager, dismissed: bool) -> bool {
    !dismissed
        && !layers
            .layers()
            .iter()
            .any(|layer| layer.as_editable().is_some())
}

/// Hide the welcome card for good (the × button and the `DismissWelcome`
/// script action). Saved to the settings file, except in script mode where it
/// only changes the in-memory settings so a run never touches the user's file.
pub(crate) fn dismiss_welcome(cx: &mut gpui::App) {
    osm_gpui::settings_store::set_welcome_dismissed(true, !crate::script_harness::script_mode());
    cx.refresh_windows();
}

/// Script/harness action (`action DismissWelcome`): same as clicking the ×.
pub(crate) fn dismiss_welcome_action(_: &DismissWelcome, cx: &mut gpui::App) {
    dismiss_welcome(cx);
}

gpui::actions!(osm_gpui, [DismissWelcome]);

/// What the card's download button can do right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DownloadState {
    /// The view is small enough for the OSM API.
    Ready,
    /// The view covers more than the API allows; zoom in first.
    ZoomIn,
    /// A download is already running.
    Busy,
}

/// The same area check the status bar and `request_download` use: the
/// current server's advertised maximum (defaults until it is known).
pub(crate) fn download_state(visible: &GeoBounds, downloading: bool) -> DownloadState {
    if downloading {
        DownloadState::Busy
    } else if !osm_gpui::capabilities::allows_download(
        &osm_gpui::settings_store::api_base_url(),
        visible,
    ) {
        DownloadState::ZoomIn
    } else {
        DownloadState::Ready
    }
}

impl MapViewer {
    /// Put the standard OSM Carto layer under whatever else is loaded. Called
    /// once when the main window is created, never again, so a user who
    /// removes it keeps it removed.
    pub(crate) fn add_default_layers(&mut self) {
        if needs_default_imagery(&self.layer_manager) {
            let layer_id = self.layer_manager.alloc_id();
            let tile_layer = TileLayer::new(layer_id, self.tile_cache.clone());
            self.layer_manager.add_layer(Box::new(tile_layer));
        }
    }

    /// The welcome card, anchored bottom-centre of the map area, or `None`
    /// once OSM data is loaded. Only the card itself takes pointer input;
    /// everything around it still reaches the map.
    pub(crate) fn render_welcome(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let settings = osm_gpui::settings_store::snapshot();
        if !show_welcome(&self.layer_manager, settings.welcome_dismissed) {
            return None;
        }
        let state = download_state(&self.viewport.visible_bounds(), self.transfer.is_some());
        let shortcut = |id: &str| -> SharedString {
            crate::selection_actions::display_shortcut(&osm_gpui::keybindings::effective_spec(
                &settings, id,
            ))
            .into()
        };
        let muted = cx.theme().muted_foreground;

        let download_label = match state {
            DownloadState::Ready => tr!("welcome.download"),
            DownloadState::ZoomIn => tr!("welcome.zoom_in"),
            DownloadState::Busy => tr!("welcome.downloading"),
        };
        let download = Button::new("welcome-download")
            .primary()
            .label(download_label)
            .disabled(state != DownloadState::Ready)
            .on_click(cx.listener(|this, _, _window, cx| this.request_download(cx)));
        let open = Button::new("welcome-open")
            .label(tr!("welcome.open"))
            .on_click(cx.listener(|_this, _, _window, cx| {
                crate::menu::open_osm_file(&OpenOsmFile, cx);
            }));

        let with_shortcut = |button: Button, text: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(button)
                .child(
                    div()
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .child(text),
                )
        };

        let card = v_flex()
            .id("welcome-card")
            .w(px(560.0))
            .p_4()
            .gap_3()
            .bg(cx.theme().popover.opacity(0.96))
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .text_color(cx.theme().popover_foreground)
            // Clicks on the card (its buttons, or its padding) must not also
            // start a selection or pan on the map underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(tr!("welcome.title")),
                    )
                    .child(
                        Button::new("welcome-dismiss")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip(tr!("welcome.dismiss"))
                            .on_click(cx.listener(|_this, _, _window, cx| {
                                dismiss_welcome(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_color(muted)
                    .line_height(gpui::relative(1.4))
                    .child(tr!("welcome.body")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_x_4()
                    .gap_y_2()
                    .child(with_shortcut(download, shortcut("download_osm")))
                    .child(with_shortcut(open, shortcut("open_osm_file"))),
            );

        // A full-width, non-interactive row centres the card without
        // covering the map: it has no handlers, so pointer events pass
        // through to the map area behind it.
        Some(
            div()
                .absolute()
                .bottom(px(48.0))
                .left_0()
                .right_0()
                .flex()
                .flex_row()
                .justify_center()
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use osm_gpui::layers::{grid_layer::GridLayer, osm_layer::OsmLayer};

    fn osm_layer(layers: &mut LayerManager) -> osm_gpui::layers::LayerId {
        let id = layers.alloc_id();
        layers.add_layer(Box::new(OsmLayer::new(id)));
        id
    }

    #[test]
    fn welcome_shows_with_no_layers_or_only_imagery() {
        let mut layers = LayerManager::new();
        assert!(show_welcome(&layers, false));
        let id = layers.alloc_id();
        layers.add_layer(Box::new(GridLayer::new(id)));
        assert!(show_welcome(&layers, false));
    }

    #[test]
    fn welcome_is_shown_by_default_settings() {
        let layers = LayerManager::new();
        let settings = osm_gpui::settings_store::AppSettings::default();
        assert!(show_welcome(&layers, settings.welcome_dismissed));
    }

    #[test]
    fn dismissed_welcome_stays_hidden_with_no_data_layer() {
        let mut layers = LayerManager::new();
        assert!(!show_welcome(&layers, true));
        let _id = osm_layer(&mut layers);
        layers.remove_at(0);
        assert!(!show_welcome(&layers, true), "removing the last layer");
    }

    #[test]
    fn dismissal_survives_save_and_load() {
        let dir = std::env::temp_dir()
            .join("osm-gpui-welcome-tests")
            .join(format!("dismiss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let settings = osm_gpui::settings_store::AppSettings {
            welcome_dismissed: true,
            ..Default::default()
        };
        osm_gpui::settings_store::save_to(&path, &settings).unwrap();
        let loaded = osm_gpui::settings_store::load_from(&path);
        assert!(loaded.welcome_dismissed);
        assert!(!show_welcome(
            &LayerManager::new(),
            loaded.welcome_dismissed
        ));
    }

    #[test]
    fn welcome_hides_while_a_data_layer_exists_and_returns_after_removal() {
        let mut layers = LayerManager::new();
        let _id = osm_layer(&mut layers);
        assert!(!show_welcome(&layers, false));
        layers.remove_at(0);
        assert!(show_welcome(&layers, false));
    }

    #[gpui::test]
    fn default_imagery_is_added_once_and_stays_removed(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(MapViewer::new);
        window
            .update(cx, |view, _window, _cx| {
                // A fresh viewer is empty; the window setup adds Carto.
                assert!(needs_default_imagery(&view.layer_manager));
                view.add_default_layers();
                view.add_default_layers();
                let layers = view.layer_manager.layers();
                assert_eq!(layers.len(), 1, "no duplicate");
                assert_eq!(layers[0].name(), CARTO_LAYER_NAME);
                assert!(layers[0].is_visible());
                assert!(
                    show_welcome(&view.layer_manager, false),
                    "imagery is not data"
                );

                // Removing it is not undone by anything but an explicit add.
                view.layer_manager.remove_at(0);
                assert!(needs_default_imagery(&view.layer_manager));
            })
            .unwrap();
    }

    #[test]
    fn download_state_follows_area_and_activity() {
        let small = GeoBounds::new(40.70, 40.75, -74.02, -73.98);
        let large = GeoBounds::new(40.0, 41.0, -75.0, -74.0);
        assert_eq!(download_state(&small, false), DownloadState::Ready);
        assert_eq!(download_state(&large, false), DownloadState::ZoomIn);
        assert_eq!(download_state(&small, true), DownloadState::Busy);
        assert_eq!(download_state(&large, true), DownloadState::Busy);
    }

    #[test]
    fn welcome_text_resolves_in_every_state() {
        for key in [
            "welcome.title",
            "welcome.body",
            "welcome.download",
            "welcome.downloading",
            "welcome.zoom_in",
            "welcome.open",
            "welcome.dismiss",
        ] {
            let text = osm_gpui::tr!(key);
            assert!(!text.starts_with("en."), "{key}: {text}");
        }
    }
}
