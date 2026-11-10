//! Modal dialog to search vendored iD tagging schema presets and apply the
//! matched tags to the single currently-selected feature — lets a user
//! deliberately change a feature's type, not just accept whatever
//! `PresetIndex::match_feature` auto-matched.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Window,
};
use gpui_component::{
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};
use std::collections::HashMap;

use crate::preset_recents::{self, Section};
use crate::preset_translations::PresetTranslations;
use crate::presets::{Geometry, Preset};
use crate::tr;
use crate::ui::style::muted_text_size;

const MAX_RESULTS: usize = 30;
const PREVIEW_TAG_COUNT: usize = 3;
const ROW_ICON_SIZE: f32 = 20.0;

/// The icon asset for a result row: the preset's own icon, else the generic
/// icon for the geometry being edited.
fn icon_for_preset(icon_name: Option<&str>, geometry: Geometry) -> Option<gpui::SharedString> {
    crate::presets::icon_path_or_fallback(icon_name, geometry)
}

/// Build the compact tag preview shown in a result row, e.g.
/// "amenity=cafe, cuisine=coffee_shop" — up to `PREVIEW_TAG_COUNT` tags,
/// sorted by key for determinism. Mirrors
/// `crate::ui::nsi_dialog::format_tag_preview`, duplicated here since that
/// one is specific to `NsiEntry` rather than any `HashMap<String, String>`.
pub fn format_tag_preview(tags: &HashMap<String, String>) -> String {
    let mut kv: Vec<(&String, &String)> = tags.iter().collect();
    kv.sort_by(|a, b| a.0.cmp(b.0));
    kv.into_iter()
        .take(PREVIEW_TAG_COUNT)
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(", ")
}

pub enum DialogEvent {
    /// The chosen preset's tags and the geometries it applies to (the
    /// caller needs the latter to keep a closed way's `area=yes` right).
    Submitted {
        tags: HashMap<String, String>,
        geometry: Vec<Geometry>,
        /// The chosen preset's schema id, remembered as a recent use.
        preset_id: String,
    },
    Cancelled,
}

/// One row of the picker: the preset's id, display name, tags, geometries
/// and icon name, plus the group heading it sits under (only in the default
/// list shown before the user types a search).
struct PickerRow {
    id: String,
    name: String,
    tags: HashMap<String, String>,
    geometry: Vec<Geometry>,
    icon: Option<String>,
    section: Option<Section>,
}

impl PickerRow {
    fn from_preset(
        p: &Preset,
        translations: Option<&PresetTranslations>,
        section: Option<Section>,
    ) -> Self {
        Self {
            id: p.id.clone(),
            name: p.display_name(translations).to_string(),
            tags: p.tags.clone(),
            geometry: p.geometry.clone(),
            icon: p.icon.clone(),
            section,
        }
    }

    fn submitted(&self) -> DialogEvent {
        DialogEvent::Submitted {
            tags: self.tags.clone(),
            geometry: self.geometry.clone(),
            preset_id: self.id.clone(),
        }
    }
}

pub struct PresetPickerDialog {
    query: Entity<InputState>,
    /// Geometries to offer presets for, best first (a closed way gets both
    /// area and line).
    geometries: Vec<Geometry>,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for PresetPickerDialog {}

impl PresetPickerDialog {
    pub fn new(geometries: Vec<Geometry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("dialog.preset_picker.search_placeholder"))
        });
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        Self {
            query,
            geometries,
            selected_index: 0,
            focus_handle,
        }
    }

    /// Current search results for whatever's typed in the query box, cloned
    /// out of the global index so the dialog doesn't hold onto a
    /// `&'static Preset` borrow across renders.
    fn results(&self, cx: &Context<Self>) -> Vec<PickerRow> {
        let text = self.query.read(cx).value().to_string();
        let translations = crate::preset_translations::active();
        let index = crate::presets::preset_index();
        if text.trim().is_empty() {
            // Before any search: recents, then common presets.
            let recents = preset_recents::RecentsStore::load(&preset_recents::SettingsRecents);
            return preset_recents::default_list(index, &recents, &self.geometries)
                .into_iter()
                .take(MAX_RESULTS)
                .map(|(section, p)| PickerRow::from_preset(p, translations, Some(section)))
                .collect();
        }
        index
            .search_geometries_in(translations, &text, &self.geometries, MAX_RESULTS)
            .into_iter()
            .map(|p| PickerRow::from_preset(p, translations, None))
            .collect()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn submit_selected(&mut self, cx: &mut Context<Self>) {
        let results = self.results(cx);
        if let Some(row) = results.get(self.selected_index) {
            cx.emit(row.submitted());
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => self.cancel(cx),
            "enter" => self.submit_selected(cx),
            "down" => {
                let count = self.results(cx).len();
                if count > 0 {
                    self.selected_index = (self.selected_index + 1).min(count - 1);
                    cx.notify();
                }
            }
            "up" => {
                self.selected_index = self.selected_index.saturating_sub(1);
                cx.notify();
            }
            _ => {}
        }
    }
}

impl Focusable for PresetPickerDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PresetPickerDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let results = self.results(cx);
        self.selected_index = if results.is_empty() {
            0
        } else {
            self.selected_index.min(results.len() - 1)
        };

        let list: gpui::AnyElement = if results.is_empty() {
            Label::new(tr!("dialog.no_matches"))
                .text_color(muted)
                .into_any_element()
        } else {
            let selected_index = self.selected_index;
            // Rows without an icon of their own show the first (best)
            // geometry's generic icon.
            let fallback_geometry = self.geometries.first().copied().unwrap_or(Geometry::Point);
            div()
                .id("preset-picker-results")
                .flex()
                .flex_col()
                .h(gpui::px(240.0))
                .overflow_y_scroll()
                .children(results.iter().enumerate().flat_map(|(i, row)| {
                    let mut items: Vec<gpui::AnyElement> = Vec::new();
                    // A heading (not selectable, not a row) starts each group.
                    if row.section.is_some() && (i == 0 || results[i - 1].section != row.section) {
                        let label = match row.section {
                            Some(Section::Recent) => tr!("dialog.preset_picker.recent"),
                            _ => tr!("dialog.preset_picker.common"),
                        };
                        items.push(
                            div()
                                .id(("preset-picker-heading", i))
                                .px_2()
                                .pt_2()
                                .pb_1()
                                .child(
                                    Label::new(label)
                                        .text_size(muted_text_size())
                                        .text_color(muted)
                                        .font_weight(gpui::FontWeight::BOLD),
                                )
                                .into_any_element(),
                        );
                    }
                    items.push({
                        let name = &row.name;
                        let tags = &row.tags;
                        let is_selected = i == selected_index;
                        let icon = icon_for_preset(row.icon.as_deref(), fallback_geometry);
                        let (click_tags, click_geometry, click_id) =
                            (row.tags.clone(), row.geometry.clone(), row.id.clone());
                        div()
                            .id(("preset-picker-result", i))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .when(is_selected, |el| el.bg(cx.theme().accent))
                            .hover(|el| el.bg(cx.theme().accent))
                            // The icon slot is reserved even without an
                            // icon so names line up down the list.
                            .child(
                                div()
                                    .size(gpui::px(ROW_ICON_SIZE))
                                    .flex_shrink_0()
                                    .children(icon.map(|path| {
                                        gpui::svg()
                                            .path(path)
                                            .size(gpui::px(ROW_ICON_SIZE))
                                            .text_color(cx.theme().foreground)
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(gpui::px(0.))
                                    .child(Label::new(name.clone()))
                                    .child(
                                        Label::new(format_tag_preview(tags))
                                            .text_size(muted_text_size())
                                            .text_color(muted),
                                    ),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |_this, _ev, _, cx| {
                                    cx.emit(DialogEvent::Submitted {
                                        tags: click_tags.clone(),
                                        geometry: click_geometry.clone(),
                                        preset_id: click_id.clone(),
                                    });
                                }),
                            )
                            .into_any_element()
                    });
                    items
                }))
                .into_any_element()
        };

        let body = v_flex().gap_3().child(Input::new(&self.query)).child(list);

        let frame = v_flex()
            .w(gpui::px(420.0))
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(tr!("dialog.preset_picker.title")),
            )
            .child(div().p_4().child(body));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .inset_0()
            // Without this, GPUI lets mouse events fall through to whatever's
            // behind the modal — the map area's own mouse-down handler then
            // steals window focus back from the dialog on every click.
            .occlude()
            .bg(crate::ui::style::scrim_color())
            .flex()
            .justify_center()
            .items_center()
            .child(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn preview_sorts_by_key_and_caps_at_three() {
        let t = tags(&[
            ("name", "Starbucks"),
            ("amenity", "cafe"),
            ("brand", "Starbucks"),
            ("brand:wikidata", "Q37158"),
        ]);
        assert_eq!(
            format_tag_preview(&t),
            "amenity=cafe, brand=Starbucks, brand:wikidata=Q37158"
        );
    }

    #[test]
    fn preview_empty_tags_is_empty_string() {
        assert_eq!(format_tag_preview(&tags(&[])), "");
    }
}
