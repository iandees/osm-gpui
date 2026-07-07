//! The right-hand side panel: Layers, Selection, Tags, and History sections.

use gpui::{div, prelude::*, px, Context, MouseDownEvent, SharedString};
use gpui_component::{
    ActiveTheme, Icon, IconName, Sizable,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    label::Label,
    menu::ContextMenuExt,
};

use crate::{DeleteLayer, MapViewer, MoveLayer, PendingTagEditOpen};

impl MapViewer {
    const SELECTION_ROW_HEIGHT: f32 = 22.0;
    const SELECTION_MAX_VISIBLE_ROWS: usize = 10;

    /// The right pane: Layers, Selection, and Tags sections stacked
    /// top-to-bottom, each collapsible and sized to its content (the whole
    /// pane scrolls).
    pub(crate) fn render_side_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layer_info: Vec<(String, bool, bool)> = self
            .layer_manager
            .layers()
            .iter()
            .map(|layer| (layer.name().to_string(), layer.is_visible(), layer.is_modified()))
            .collect();

        let layers_section = self.render_layers_section(&layer_info, cx);
        let selection_section = self.render_selection_section(cx);
        let tags_section = self.render_tags_section(cx);
        let history_section = self.render_history_section(cx);

        let open_layers = self.side_panel_open[0];
        let open_selection = self.side_panel_open[1];
        let open_tags = self.side_panel_open[2];
        let open_history = self.side_panel_open[3];

        let selection_title = match self.selected.len() {
            0 => "Selection".to_string(),
            1 => "Selection (1 item)".to_string(),
            n => format!("Selection ({} items)", n),
        };

        div()
            .w(px(280.0))
            .h_full()
            .bg(cx.theme().sidebar)
            .border_l_1()
            .border_color(cx.theme().border)
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .id("side-panel-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(self.collapsible_section("Layers", 0, open_layers, layers_section, cx))
                    .child(self.collapsible_section(
                        selection_title,
                        1,
                        open_selection,
                        selection_section,
                        cx,
                    ))
                    .child(self.collapsible_section("Tags", 2, open_tags, tags_section, cx))
                    .child(self.collapsible_section("History", 3, open_history, history_section, cx)),
            )
    }

    /// The History accordion section: a passive list of every undoable
    /// action in order. The most recently applied action (the stack's
    /// current position) is highlighted; anything after it is available to
    /// redo but not currently applied, and renders dimmed.
    fn render_history_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.undo_stack.actions.is_empty() {
            return Label::new("No actions yet.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let cursor = self.undo_stack.cursor;
        div()
            .flex()
            .flex_col()
            .children(self.undo_stack.actions.iter().enumerate().map(|(i, action)| {
                let is_current = i + 1 == cursor;
                let is_future = i >= cursor;
                let mut row = div()
                    .px_1()
                    .py_0p5()
                    .text_sm()
                    .child(action.description());
                if is_current {
                    row = row.bg(cx.theme().accent);
                } else if is_future {
                    row = row.text_color(cx.theme().muted_foreground).italic();
                }
                row
            }))
            .into_any_element()
    }

    /// A single collapsible section: a clickable header (chevron + title) that
    /// toggles `side_panel_open[index]`, with its content rendered below when
    /// open. Sizes to content so sections stack instead of splitting the height.
    fn collapsible_section(
        &self,
        title: impl Into<gpui::SharedString>,
        index: usize,
        open: bool,
        content: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = div()
            .id(("section-header", index))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .cursor_pointer()
            .border_b_1()
            .border_color(cx.theme().border)
            .hover(|this| this.bg(cx.theme().accent))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(cx.theme().muted_foreground),
            )
            .child(
                Label::new(title)
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD),
            )
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.side_panel_open[index] = !this.side_panel_open[index];
                cx.notify();
            }));

        div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |this| this.child(div().px_2().py_1p5().child(content)))
    }

    /// The Selection accordion section: a scrollable list of the selected
    /// features (max ~10 rows visible, then scrolls). Clicking a row narrows
    /// the selection to just that feature.
    fn render_selection_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::selection::FeatureKind;

        if self.selected.is_empty() {
            return Label::new("Click or drag to select.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let visible_rows = self.selected.len().min(Self::SELECTION_MAX_VISIBLE_ROWS);
        let list_height = px(visible_rows as f32 * Self::SELECTION_ROW_HEIGHT);

        div()
            .id("selection-list")
            .flex()
            .flex_col()
            .h(list_height)
            .overflow_y_scroll()
            .children(self.selected.iter().enumerate().map(|(i, feat)| {
                let kind_label = match feat.kind {
                    FeatureKind::Node => "Node",
                    FeatureKind::Way => "Way",
                };
                let row_feat = feat.clone();
                div()
                    .id(("selection-row", i))
                    .flex_shrink_0()
                    .h(px(Self::SELECTION_ROW_HEIGHT))
                    .px_1()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|this| this.bg(cx.theme().accent))
                    .child(format!("{} {}", kind_label, feat.id))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                            this.selected = vec![row_feat.clone()];
                            cx.notify();
                        }),
                    )
            }))
            .into_any_element()
    }

    /// The Layers accordion section: a Checkbox row per layer with a right-click
    /// context menu offering Move up / Move down / Delete.
    fn render_layers_section(
        &self,
        layer_info: &[(String, bool, bool)],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let total = layer_info.len();
        if total == 0 {
            return Label::new("No layers yet. Add one from the menu.").into_any_element();
        }

        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                layer_info
                    .iter()
                    .enumerate()
                    .map(|(index, (name, is_visible, is_modified))| {
                        let layer_name = name.clone();
                        let label = if *is_modified {
                            format!("{} \u{2022}", name)
                        } else {
                            name.clone()
                        };
                        Checkbox::new(("layer", index))
                            .checked(*is_visible)
                            .label(label)
                            .on_click(cx.listener(move |this, _checked: &bool, _, cx| {
                                this.toggle_layer_visibility(&layer_name);
                                cx.notify();
                            }))
                            .context_menu(move |menu, _window, _cx| {
                                let mut menu = menu;
                                if index > 0 {
                                    menu = menu
                                        .menu("Move up", Box::new(MoveLayer { index, delta: -1 }));
                                }
                                if index + 1 < total {
                                    menu = menu
                                        .menu("Move down", Box::new(MoveLayer { index, delta: 1 }));
                                }
                                menu.separator()
                                    .menu("Delete", Box::new(DeleteLayer { index }))
                            })
                    })
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    /// The Tags accordion section: tags aggregated across every selected
    /// feature. A key shows its value only if every selected feature has
    /// that exact same value (a feature missing the key counts as its own
    /// distinct state); otherwise it shows "<N values>". Double-
    /// clicking the key or value opens the tag-edit dialog with that field
    /// pre-selected; the trailing "x" removes the tag immediately. An "Add
    /// tag" button below the list opens the same dialog with empty fields.
    fn render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::ui::tag_edit_dialog::TagEditField;

        if self.selected.is_empty() {
            return Label::new("No selection.")
                .text_color(cx.theme().muted_foreground)
                .text_sm()
                .into_any_element();
        }

        let per_feature: Vec<Vec<(String, String)>> = self
            .selected
            .iter()
            .filter_map(|sel| {
                self.layer_manager
                    .find_layer(&sel.layer_name)
                    .and_then(|layer| layer.feature_tags(sel))
            })
            .collect();

        let aggregated = osm_gpui::selection::aggregate_tags(&per_feature);
        let selection = self.selected.clone();

        let mut list = div().flex().flex_col();

        if aggregated.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("(no tags)"),
            );
        } else {
            list = list.children(aggregated.into_iter().map(|(k, v)| {
                let value_text = match v {
                    osm_gpui::selection::TagValue::Single(s) => s,
                    osm_gpui::selection::TagValue::Multiple(n) => format!("<{} values>", n),
                };

                let key_for_key_click = k.clone();
                let value_for_key_click = value_text.clone();
                let selection_for_key_click = selection.clone();

                let key_for_value_click = k.clone();
                let value_for_value_click = value_text.clone();
                let selection_for_value_click = selection.clone();

                let key_for_delete = k.clone();

                div()
                    .id(SharedString::from(format!("tag-row-{k}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .cursor_pointer()
                            .child(k.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_key_click.clone(),
                                            original_key: key_for_key_click.clone(),
                                            original_value: value_for_key_click.clone(),
                                            select: TagEditField::Key,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .cursor_pointer()
                            .child(value_text.clone())
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                                    if ev.click_count == 2 {
                                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                                            features: selection_for_value_click.clone(),
                                            original_key: key_for_value_click.clone(),
                                            original_value: value_for_value_click.clone(),
                                            select: TagEditField::Value,
                                            is_add: false,
                                        });
                                        cx.notify();
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tag-delete-{k}")))
                            .cursor_pointer()
                            .text_color(cx.theme().muted_foreground)
                            .hover(|this| this.text_color(cx.theme().danger))
                            .child("x")
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _window, cx| {
                                    this.delete_tag(&key_for_delete, cx);
                                }),
                            ),
                    )
                    .into_any_element()
            }));
        }

        let add_selection = selection.clone();
        list.child(
            Button::new("add-tag")
                .label("Add tag")
                .primary()
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.pending_tag_edit_open = Some(PendingTagEditOpen {
                        features: add_selection.clone(),
                        original_key: String::new(),
                        original_value: String::new(),
                        select: TagEditField::None,
                        is_add: true,
                    });
                    cx.notify();
                })),
        )
        .into_any_element()
    }
}
