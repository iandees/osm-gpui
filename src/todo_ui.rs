//! The side panel's To-do section (`osm_gpui::todo` holds the list itself).
//!
//! Features get on the list from the selection: Tools > Add to To-do List,
//! the map's context menu, or the section's own Add Selected button, after
//! picking them on the map or with Edit > Find. A click on an entry selects
//! it and zooms to it. Done (per row, Mark To-do Done, or Done & Next in the
//! header) takes entries off the list and goes on to the next one.
//!
//! The section is a zone of its own above Validation. It appears with the
//! first entry added and stays until its close button; View > To-do List
//! brings it back, with the list as it was.

use gpui::{actions, div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
};

use osm_gpui::selection::{FeatureKind, FeatureRef};
use osm_gpui::todo::TodoList;
use osm_gpui::ui::style::{interactive_row, muted_text_size, PANEL_ROW_HEIGHT};
use osm_gpui::ui::validation_list::note_row;
use osm_gpui::{tr, tr_count};

use crate::MapViewer;

actions!(osm_gpui, [ToggleTodoList]);

/// Index of the To-do section in `MapViewer::side_panel_open`.
pub(crate) const SECTION: usize = 10;

/// About how tall one entry's row is.
const ROW_HEIGHT_ESTIMATE: f32 = PANEL_ROW_HEIGHT + 4.0;

#[derive(Default)]
pub(crate) struct TodoState {
    pub(crate) list: TodoList,
    /// The section is in the side panel.
    pub(crate) shown: bool,
}

impl MapViewer {
    /// Whether View > To-do List is checked.
    pub(crate) fn shows_todo(&self) -> bool {
        self.todo.shown
    }

    /// What the To-do section asks of the side panel layout, or `None` when
    /// it is not showing.
    pub(crate) fn todo_want(&self) -> Option<crate::side_panel_layout::Want> {
        self.todo.shown.then(|| crate::side_panel_layout::Want {
            open: self.side_panel_open[SECTION],
            content: self.todo.list.len().max(1) as f32 * ROW_HEIGHT_ESTIMATE,
        })
    }

    fn set_todo_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.todo.shown == shown {
            return;
        }
        self.todo.shown = shown;
        let (center, state) = self.imagery_menu_context();
        crate::menu::rebuild_menus(&mut *cx, center, state, self.menu_toggles());
        cx.notify();
    }

    pub(crate) fn on_toggle_todo_list(
        &mut self,
        _: &ToggleTodoList,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown = !self.todo.shown;
        if shown {
            self.side_panel_open[SECTION] = true;
        }
        self.set_todo_shown(shown, cx);
    }

    /// Per-frame upkeep: entries of a deleted layer go with it.
    pub(crate) fn todo_frame(&mut self) {
        if self.todo.list.is_empty() {
            return;
        }
        let layers = &self.layer_manager;
        self.todo
            .list
            .retain_layers(|id| layers.find_layer(id).is_some());
    }

    /// Add to To-do List: put the selected features on the list and show it.
    pub(crate) fn add_selection_to_todo(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let added = self.todo.list.add(&self.selected);
        let total = self.todo.list.len();
        self.set_status(if added == 0 {
            tr!("todo.status.already")
        } else {
            tr_count!("todo.status.added", added, total = total)
        });
        self.side_panel_open[SECTION] = true;
        self.set_todo_shown(true, cx);
        cx.notify();
    }

    /// Mark To-do Done: take the selected features off the list and go to
    /// the next entry.
    pub(crate) fn mark_selection_todo_done(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected.clone();
        let (removed, next) = self.todo.list.mark_done(&selected);
        if removed == 0 {
            return;
        }
        self.after_todo_done(next, cx);
    }

    /// Done & Next: mark the selected entries done, or, when none is
    /// selected, start at the first entry.
    pub(crate) fn todo_done_and_next(&mut self, cx: &mut Context<Self>) {
        if self.selected.iter().any(|f| self.todo.list.contains(f)) {
            self.mark_selection_todo_done(cx);
        } else if let Some(first) = self.todo.list.items().first().copied() {
            self.visit_todo(&first, cx);
        }
    }

    /// The Done button of row `index`.
    fn todo_row_done(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.todo.list.items().get(index).copied() else {
            return;
        };
        let was_current = self.selected == [item];
        let next = self.todo.list.remove_at(index);
        if was_current {
            self.after_todo_done(next, cx);
        } else {
            self.set_status(tr_count!("todo.status.left", self.todo.list.len()));
            cx.notify();
        }
    }

    fn after_todo_done(&mut self, next: Option<FeatureRef>, cx: &mut Context<Self>) {
        match next {
            Some(next) => {
                self.set_status(tr_count!("todo.status.left", self.todo.list.len()));
                self.visit_todo(&next, cx);
            }
            None => {
                self.set_selection(Vec::new());
                self.set_status(tr!("todo.status.all_done"));
                cx.notify();
            }
        }
    }

    /// Select `item` and zoom to it, when it still exists.
    fn visit_todo(&mut self, item: &FeatureRef, cx: &mut Context<Self>) {
        if self.todo_label(item).1 {
            self.show_features(std::slice::from_ref(item), cx);
        } else {
            self.set_status(tr!("todo.status.gone"));
            cx.notify();
        }
    }

    /// The row text for `item`, and whether the feature still exists.
    fn todo_label(&self, item: &FeatureRef) -> (String, bool) {
        let layer = self.layer_manager.find_layer(item.layer_id);
        let exists = layer
            .and_then(|l| l.as_editable())
            .is_some_and(|e| e.has_feature(item.kind, item.id));
        let label = match layer
            .and_then(|l| l.as_osm())
            .and_then(|l| l.get_osm_data())
        {
            Some(data) if exists => osm_gpui::validator::feature_label(&data, item.kind, item.id),
            _ => {
                let kind = match item.kind {
                    FeatureKind::Node => tr!("validator.feature.node"),
                    FeatureKind::Way => tr!("validator.feature.way"),
                    FeatureKind::Relation => tr!("validator.feature.relation"),
                };
                tr!("todo.row.gone", kind = kind, id = item.id).to_string()
            }
        };
        (label, exists)
    }

    /// The whole To-do section for the side panel, or `None` when it is not
    /// showing.
    pub(crate) fn render_todo_section(
        &self,
        list_max_height: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        if !self.todo.shown {
            return None;
        }
        let theme = cx.theme().clone();
        let open = self.side_panel_open[SECTION];
        let count = self.todo.list.len();
        let can_add = self.selected.iter().any(|f| !self.todo.list.contains(f));

        let trailing = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_1()
            .child(
                div().min_w_0().truncate().child(
                    Label::new(tr_count!("todo.count", count))
                        .text_size(muted_text_size())
                        .text_color(theme.muted_foreground),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("todo-add")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .tooltip(tr!("todo.add_selected"))
                            .disabled(!can_add)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                cx.stop_propagation();
                                this.add_selection_to_todo(cx);
                            })),
                    )
                    .child(
                        Button::new("todo-next")
                            .label(tr!("todo.done_next"))
                            .outline()
                            .xsmall()
                            .disabled(count == 0)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                cx.stop_propagation();
                                this.todo_done_and_next(cx);
                            })),
                    )
                    .child(
                        Button::new("todo-close")
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .tooltip(tr!("todo.close"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                cx.stop_propagation();
                                this.set_todo_shown(false, cx);
                            })),
                    ),
            );

        let body = if !open {
            div().into_any_element()
        } else if count == 0 {
            note_row(tr!("todo.empty"), cx).into_any_element()
        } else {
            let mut rows = gpui_component::v_flex().gap_px();
            for (index, item) in self.todo.list.items().iter().enumerate() {
                let (label, exists) = self.todo_label(item);
                let current = self.selected.contains(item);
                let item_for_click = *item;
                let row = interactive_row(("todo-row", index), current, cx)
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if exists {
                                theme.foreground
                            } else {
                                theme.muted_foreground
                            })
                            .child(SharedString::from(label)),
                    )
                    .child(
                        Button::new(("todo-done", index))
                            .icon(IconName::Check)
                            .ghost()
                            .xsmall()
                            .tooltip(tr!("todo.done"))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                cx.stop_propagation();
                                this.todo_row_done(index, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.visit_todo(&item_for_click, cx);
                    }));
                rows = rows.child(row);
            }
            div()
                .id("todo-list")
                .flex_shrink(1.)
                .min_h(px(0.))
                .max_h(px(list_max_height))
                .overflow_y_scroll()
                .child(rows)
                .into_any_element()
        };

        Some(
            self.collapsible_section_with(
                tr!("todo.title"),
                SECTION,
                open,
                Some(trailing.into_any_element()),
                div()
                    .flex()
                    .flex_col()
                    .min_h(px(0.))
                    .child(body)
                    .into_any_element(),
                cx,
            )
            .flex_shrink(1.)
            .min_h(px(32.))
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border),
        )
    }
}
