//! Modal dialog to pick one of a layer's relations (by name, with a search
//! box): "Add to Relation" appends the selected features to the chosen one.

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Window,
};
use gpui_component::{
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _,
};

use crate::relations::{matching_choices, RelationChoice};
use crate::ui::modal;
use crate::ui::style::muted_text_size;
use crate::{tr, tr_count};

pub enum DialogEvent {
    /// The id of the chosen relation.
    Submitted(i64),
    Cancelled,
}

pub struct RelationPickerDialog {
    query: Entity<InputState>,
    choices: Vec<RelationChoice>,
    selected_index: usize,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for RelationPickerDialog {}

impl RelationPickerDialog {
    /// A picker over `choices` (see `relations::relation_choices`).
    pub fn new(choices: Vec<RelationChoice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("dialog.relation_picker.search_placeholder"))
        });
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        Self {
            query,
            choices,
            selected_index: 0,
            focus_handle,
        }
    }

    /// The ids of the choices matching what is typed, in list order.
    fn matching_ids(&self, cx: &Context<Self>) -> Vec<i64> {
        let text = self.query.read(cx).value().to_string();
        matching_choices(&self.choices, &text)
            .into_iter()
            .map(|c| c.id)
            .collect()
    }

    fn submit_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.matching_ids(cx).get(self.selected_index) {
            cx.emit(DialogEvent::Submitted(*id));
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => cx.emit(DialogEvent::Cancelled),
            modal::ModalKey::Enter => self.submit_selected(cx),
            modal::ModalKey::Other => {
                let key = ev.keystroke.key.as_str();
                if matches!(key, "up" | "down") {
                    let count = self.matching_ids(cx).len();
                    if let Some(i) = modal::move_selection(self.selected_index, count, key) {
                        self.selected_index = i;
                        cx.notify();
                    }
                }
            }
        }
    }
}

impl crate::ui::modal::ModalDialog for RelationPickerDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Focusable for RelationPickerDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RelationPickerDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let text = self.query.read(cx).value().to_string();
        let results: Vec<RelationChoice> = matching_choices(&self.choices, &text)
            .into_iter()
            .cloned()
            .collect();
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
            div()
                .id("relation-picker-results")
                .flex()
                .flex_col()
                .h(gpui::px(240.0))
                .overflow_y_scroll()
                .children(results.iter().enumerate().map(|(i, choice)| {
                    let id = choice.id;
                    let detail = tr!(
                        "dialog.relation_picker.detail",
                        id = choice.id,
                        members = tr_count!("dialog.relation_picker.members", choice.member_count)
                    );
                    div()
                        .id(("relation-picker-result", i))
                        .flex()
                        .flex_col()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .when(i == selected_index, |el| el.bg(cx.theme().accent))
                        .hover(|el| el.bg(cx.theme().accent))
                        .child(Label::new(choice.name.clone()))
                        .child(
                            Label::new(detail)
                                .text_size(muted_text_size())
                                .text_color(muted),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |_, _ev, _, cx| {
                                cx.emit(DialogEvent::Submitted(id));
                            }),
                        )
                }))
                .into_any_element()
        };

        let body = v_flex().gap_3().child(Input::new(&self.query)).child(list);

        let frame = modal::card(
            cx,
            gpui::px(420.0),
            tr!("dialog.relation_picker.title"),
            body,
        )
        .build();

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
