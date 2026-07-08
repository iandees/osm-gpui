//! Modal dialog to search NSI brand presets and apply the matched tags to
//! the single currently-selected feature.

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

use crate::nsi::NsiEntry;
use crate::ui::style::muted_text_size;

const MAX_RESULTS: usize = 30;
const PREVIEW_TAG_COUNT: usize = 3;

/// Build the compact tag preview shown in a result row, e.g.
/// "amenity=cafe, brand=Starbucks, name=Starbucks" — up to
/// `PREVIEW_TAG_COUNT` tags, sorted by key for determinism.
pub fn format_tag_preview(entry: &NsiEntry) -> String {
    let mut kv: Vec<(&String, &String)> = entry.tags.iter().collect();
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

pub struct NsiPresetDialog {
    query: Entity<InputState>,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for NsiPresetDialog {}

impl NsiPresetDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search brands…"));
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        Self {
            query,
            selected_index: 0,
            focus_handle,
        }
    }

    /// Current search results for whatever's typed in the query box, cloned
    /// out of the global index so the dialog doesn't hold onto the Arc's
    /// borrow across renders.
    fn results(&self, cx: &Context<Self>) -> Vec<NsiEntry> {
        let Some(index) = crate::nsi::current() else {
            return Vec::new();
        };
        let text = self.query.read(cx).value().to_string();
        index
            .search(&text, MAX_RESULTS)
            .into_iter()
            .cloned()
            .collect()
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    fn submit_selected(&mut self, cx: &mut Context<Self>) {
        let results = self.results(cx);
        if let Some(entry) = results.get(self.selected_index) {
            cx.emit(DialogEvent::Submitted(entry.tags.clone()));
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

impl Focusable for NsiPresetDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NsiPresetDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let results = self.results(cx);
        self.selected_index = if results.is_empty() {
            0
        } else {
            self.selected_index.min(results.len() - 1)
        };

        let list: gpui::AnyElement = if crate::nsi::current().is_none() {
            Label::new("Downloading NSI data…")
                .text_color(muted)
                .into_any_element()
        } else if results.is_empty() {
            Label::new("No matches.")
                .text_color(muted)
                .into_any_element()
        } else {
            let selected_index = self.selected_index;
            div()
                .id("nsi-results")
                .flex()
                .flex_col()
                .h(gpui::px(240.0))
                .overflow_y_scroll()
                .children(results.iter().enumerate().map(|(i, entry)| {
                    let tags = entry.tags.clone();
                    let is_selected = i == selected_index;
                    div()
                        .id(("nsi-result", i))
                        .flex()
                        .flex_col()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(is_selected, |el| el.bg(cx.theme().accent))
                        .hover(|el| el.bg(cx.theme().accent))
                        .child(Label::new(entry.display_name.clone()))
                        .child(
                            Label::new(format_tag_preview(entry))
                                .text_size(muted_text_size())
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
                    .child("Apply NSI Preset"),
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

    fn entry(tags: &[(&str, &str)]) -> NsiEntry {
        NsiEntry {
            display_name: "Test".to_string(),
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            match_names: vec![],
        }
    }

    #[test]
    fn preview_sorts_by_key_and_caps_at_three() {
        let e = entry(&[
            ("name", "Starbucks"),
            ("amenity", "cafe"),
            ("brand", "Starbucks"),
            ("brand:wikidata", "Q37158"),
        ]);
        assert_eq!(
            format_tag_preview(&e),
            "amenity=cafe, brand=Starbucks, brand:wikidata=Q37158"
        );
    }

    #[test]
    fn preview_empty_tags_is_empty_string() {
        assert_eq!(format_tag_preview(&entry(&[])), "");
    }
}
