//! Modal dialog to search vendored iD tagging schema presets and apply the
//! matched tags to the single currently-selected feature — lets a user
//! deliberately change a feature's type, not just accept whatever
//! `PresetIndex::match_feature` auto-matched.

use gpui::{
    div, prelude::*, rgba, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, Window,
};
use gpui_component::{
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};
use std::collections::HashMap;

use crate::presets::Geometry;

const MAX_RESULTS: usize = 30;
const PREVIEW_TAG_COUNT: usize = 3;

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
    Submitted(HashMap<String, String>),
    Cancelled,
}

pub struct PresetPickerDialog {
    query: Entity<InputState>,
    geometry: Geometry,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for PresetPickerDialog {}

impl PresetPickerDialog {
    pub fn new(geometry: Geometry, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search feature types…"));
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        Self {
            query,
            geometry,
            selected_index: 0,
            focus_handle,
        }
    }

    /// Current search results for whatever's typed in the query box: (name,
    /// id, tags) triples cloned out of the global index so the dialog
    /// doesn't hold onto a `&'static Preset` borrow across renders.
    fn results(&self, cx: &Context<Self>) -> Vec<(String, String, HashMap<String, String>)> {
        let text = self.query.read(cx).value().to_string();
        crate::presets::preset_index()
            .search(&text, self.geometry, MAX_RESULTS)
            .into_iter()
            .map(|p| (p.name.clone(), p.id.clone(), p.tags.clone()))
            .collect()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn submit_selected(&mut self, cx: &mut Context<Self>) {
        let results = self.results(cx);
        if let Some((_, _, tags)) = results.get(self.selected_index) {
            cx.emit(DialogEvent::Submitted(tags.clone()));
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
            Label::new("No matches.")
                .text_sm()
                .text_color(muted)
                .into_any_element()
        } else {
            let selected_index = self.selected_index;
            div()
                .id("preset-picker-results")
                .flex()
                .flex_col()
                .h(gpui::px(240.0))
                .overflow_y_scroll()
                .children(results.iter().enumerate().map(|(i, (name, _id, tags))| {
                    let tags = tags.clone();
                    let is_selected = i == selected_index;
                    div()
                        .id(("preset-picker-result", i))
                        .flex()
                        .flex_col()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(is_selected, |el| el.bg(cx.theme().accent))
                        .hover(|el| el.bg(cx.theme().accent))
                        .child(Label::new(name.clone()).text_sm())
                        .child(
                            Label::new(format_tag_preview(&tags))
                                .text_xs()
                                .text_color(muted),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev, _, cx| {
                                cx.emit(DialogEvent::Submitted(tags.clone()));
                                let _ = this;
                            }),
                        )
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
                    .child("Change Feature Type"),
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
            .bg(rgba(0x00000099))
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
