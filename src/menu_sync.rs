//! What the application menus were last built from, so they are rebuilt only
//! when it changes.

use crate::ImageryLoadState;
use osm_gpui::coordinates::LatLon;

pub(crate) struct MenuSync {
    /// Last (lat, lon) the Imagery menu was rebuilt for. None forces a rebuild.
    pub(crate) last_menu_center: Option<LatLon>,
    /// Where the Editor Layer Index load stands.
    pub(crate) imagery_load_state: ImageryLoadState,
    /// The load state the Imagery menu was last rebuilt for.
    pub(crate) last_imagery_load_state: Option<ImageryLoadState>,
    /// Whether the menus were last built with File > Upload to OSM enabled.
    pub(crate) upload_menu_enabled: bool,
}

impl Default for MenuSync {
    fn default() -> Self {
        Self {
            last_menu_center: None,
            imagery_load_state: ImageryLoadState::Loading,
            last_imagery_load_state: None,
            upload_menu_enabled: false,
        }
    }
}
