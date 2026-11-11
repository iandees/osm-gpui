//! The app's GPUI asset source: gpui-component's own assets plus the preset
//! icons, which are embedded in the binary (see `presets::PRESET_ICONS`) and
//! served from memory under `preset-icons/<name>.svg`.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// The pencil icon (lucide, ISC) that gpui-component's bundle lacks; used for
/// the Selection header's change-feature-type control.
pub const PENCIL_ICON_PATH: &str = "icons/pencil.svg";
const PENCIL_ICON_SVG: &str = include_str!("../assets/icons/pencil.svg");

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(crate::presets::ICON_ASSET_DIR) {
            return Ok(crate::presets::icon_asset(path).map(Cow::Borrowed));
        }
        if path == PENCIL_ICON_PATH {
            return Ok(Some(Cow::Borrowed(PENCIL_ICON_SVG.as_bytes())));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path.starts_with(crate::presets::ICON_ASSET_DIR) {
            return Ok(crate::presets::PRESET_ICONS
                .iter()
                .map(|(name, _)| format!("{}{name}.svg", crate::presets::ICON_ASSET_DIR).into())
                .collect());
        }
        gpui_component_assets::Assets.list(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_preset_icons_from_memory() {
        let bytes = AppAssets
            .load("preset-icons/maki-cafe.svg")
            .unwrap()
            .expect("embedded icon");
        assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
        assert!(AppAssets.load("preset-icons/nope.svg").unwrap().is_none());
    }

    #[test]
    fn serves_the_pencil_icon() {
        let bytes = AppAssets.load(PENCIL_ICON_PATH).unwrap().expect("pencil");
        assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
    }

    #[test]
    fn lists_every_preset_icon() {
        let all = AppAssets.list("preset-icons/").unwrap();
        assert_eq!(all.len(), crate::presets::PRESET_ICONS.len());
    }

    #[test]
    fn falls_through_to_the_component_assets() {
        // gpui-component's own icons keep working through the composite.
        let bytes = AppAssets.load("icons/check.svg").unwrap();
        assert!(bytes.is_some());
    }

    #[test]
    fn every_preset_icon_resolves_through_the_asset_source() {
        for preset in crate::presets::preset_index().iter() {
            let Some(icon) = preset.icon.as_deref() else {
                continue;
            };
            let path = crate::presets::icon_path(icon)
                .unwrap_or_else(|| panic!("{} -> {icon} not embedded", preset.id));
            assert!(
                AppAssets.load(&path).unwrap().is_some(),
                "{path} not served"
            );
        }
    }
}
