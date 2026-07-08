//! The right-hand side panel: Layers, Selection, Tags, and History sections.

use gpui::{div, prelude::*, px, Context, MouseDownEvent, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    label::Label,
    menu::ContextMenuExt,
    ActiveTheme, Icon, IconName, Sizable,
};

use osm_gpui::layers::LayerId;
use osm_gpui::ui::style::{interactive_row, panel_row, PANEL_ROW_HEIGHT, SIDE_PANEL_WIDTH};

use crate::{DeleteLayer, MapViewer, MoveLayer, PendingTagEditOpen};

impl MapViewer {
    const SELECTION_MAX_VISIBLE_ROWS: usize = 10;

    /// The right pane: Layers, Selection, and Tags sections stacked
    /// top-to-bottom, each collapsible and sized to its content (the whole
    /// pane scrolls).
    pub(crate) fn render_side_panel(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let layer_info: Vec<(LayerId, String, bool, bool, bool)> = self
            .layer_manager
            .layers()
            .iter()
            .map(|layer| {
                let is_osm = layer.as_editable().is_some();
                (
                    layer.id(),
                    layer.name().to_string(),
                    layer.is_visible(),
                    layer.is_modified(),
                    is_osm,
                )
            })
            .collect();

        let layers_section = self.render_layers_section(&layer_info, cx);
        let selection_section = self.render_selection_section(cx);
        let fields_section = self.render_fields_section(window, cx);
        let tags_section = self.render_tags_section(cx);
        let history_section = self.render_history_section(cx);

        let open_layers = self.side_panel_open[0];
        let open_selection = self.side_panel_open[1];
        let open_fields = self.side_panel_open[2];
        let open_tags = self.side_panel_open[3];
        let open_history = self.side_panel_open[4];

        let selection_title = match self.selected.len() {
            0 => "Selection".to_string(),
            1 => "Selection (1 item)".to_string(),
            n => format!("Selection ({} items)", n),
        };

        div()
            .w(px(SIDE_PANEL_WIDTH))
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
                    .child(self.collapsible_section("Fields", 2, open_fields, fields_section, cx))
                    .child(self.collapsible_section("Tags", 3, open_tags, tags_section, cx))
                    .child(self.collapsible_section(
                        "History",
                        4,
                        open_history,
                        history_section,
                        cx,
                    )),
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
            .children(
                self.undo_stack
                    .actions
                    .iter()
                    .enumerate()
                    .map(|(i, action)| {
                        let is_current = i + 1 == cursor;
                        let is_future = i >= cursor;
                        let mut row = panel_row(("history-row", i)).child(action.description());
                        if is_current {
                            row = row.bg(cx.theme().list_active);
                        } else if is_future {
                            row = row.text_color(cx.theme().muted_foreground).italic();
                        }
                        row
                    }),
            )
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

        div().flex().flex_col().child(header).when(open, |this| {
            this.child(div().px_2().py_1p5().child(content))
        })
    }

    /// Resolve the friendly `(name, icon_svg_path)` for a selected feature,
    /// or `None` if the feature's layer/tags/geometry can't be found (e.g.
    /// it was deleted since selection). `icon_svg_path` is `None` when the
    /// matched preset has no icon or the icon file isn't vendored.
    fn describe_selected_feature(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(String, Option<std::path::PathBuf>)> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let (name, icon_name) =
            osm_gpui::presets::describe_feature(osm_gpui::presets::preset_index(), &tags, geometry);
        let icon_path = icon_name.and_then(|n| osm_gpui::presets::icon_path(&n));
        Some((name, icon_path))
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
        let list_height = px(visible_rows as f32 * PANEL_ROW_HEIGHT);

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
                let row_feat = *feat;
                let described = self.describe_selected_feature(feat);
                let row_text = match &described {
                    Some((name, _)) => format!("{} · {} {}", name, kind_label, feat.id),
                    None => format!("{} {}", kind_label, feat.id),
                };
                let icon_path = described.and_then(|(_, path)| path);

                let mut row = interactive_row(("selection-row", i), false, cx)
                    .text_color(cx.theme().foreground);

                if let Some(path) = icon_path {
                    row = row.child(
                        gpui::svg()
                            .external_path(path.to_string_lossy().to_string())
                            .size(px(14.0))
                            .text_color(cx.theme().foreground),
                    );
                }

                row.child(row_text).on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                        this.selected = vec![row_feat];
                        this.fields_text_inputs.clear();
                        this.fields_text_subscribed.clear();
                        this.fields_open_combo = None;
                        this.fields_promoted_more_fields.clear();
                        cx.notify();
                    }),
                )
            }))
            .into_any_element()
    }

    /// The Layers accordion section: a Checkbox row per layer with a right-click
    /// context menu offering Move up / Move down / Delete.
    ///
    /// The toggle is driven by a plain `on_mouse_down` on a wrapping row
    /// rather than `Checkbox::on_click` + `.context_menu(...)` on the
    /// `Checkbox` itself: `Checkbox::on_click` relies on gpui's paired
    /// mouse-down/mouse-up click detection on one hitbox, but
    /// `.context_menu(...)` wraps that same element in an *extra* hitbox
    /// layer (`ContextMenu::prepaint` inserts its own hitbox covering the
    /// whole row). That combination is fragile — it manifested as the first
    /// click on a layer checkbox doing nothing (only the second registered)
    /// and the right-click menu dismissing itself when the cursor moved.
    /// `Checkbox` is kept purely for its visual (box + check icon + label);
    /// the wrapping row owns both the click-to-toggle and the context menu.
    fn render_layers_section(
        &self,
        layer_info: &[(LayerId, String, bool, bool, bool)],
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
                    .map(
                        |(index, (layer_id, name, is_visible, is_modified, is_osm))| {
                            let layer_id = *layer_id;
                            let is_osm = *is_osm;
                            let is_active = self.active_layer == Some(layer_id);
                            let label = if *is_modified {
                                format!("{} \u{2022}", name)
                            } else {
                                name.clone()
                            };
                            interactive_row(("layer-row", index), is_active, cx)
                                .child(
                                    Checkbox::new(("layer", index))
                                        .checked(*is_visible)
                                        .label(label),
                                )
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                        this.toggle_layer_visibility(layer_id);
                                        if is_osm {
                                            this.active_layer = Some(layer_id);
                                        }
                                        cx.notify();
                                    }),
                                )
                                .context_menu(move |menu, _window, _cx| {
                                    let mut menu = menu;
                                    if index > 0 {
                                        menu = menu.menu(
                                            "Move up",
                                            Box::new(MoveLayer { index, delta: -1 }),
                                        );
                                    }
                                    if index + 1 < total {
                                        menu = menu.menu(
                                            "Move down",
                                            Box::new(MoveLayer { index, delta: 1 }),
                                        );
                                    }
                                    menu.separator()
                                        .menu("Delete", Box::new(DeleteLayer { index }))
                                })
                                .into_any_element()
                        },
                    )
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
                    .find_layer(sel.layer_id)
                    .and_then(|layer| layer.as_editable())
                    .and_then(|editable| editable.feature_tags(sel))
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

                panel_row(SharedString::from(format!("tag-row-{k}")))
                    .gap_2()
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
                        // Spec deviation: the design spec calls for a danger
                        // hover treatment on this delete icon, but
                        // gpui-component's Button doesn't support it cleanly
                        // in combination with `.ghost()`. `ButtonVariant::Custom`
                        // (via `ButtonCustomVariant`) fixes the foreground color
                        // across all states rather than only on hover, and its
                        // `.hover()` background color is actually unused by
                        // `ButtonVariant::hovered()` for the non-outline case
                        // (it re-derives the hover background from `color`
                        // instead). Layering a second `.hover(...)` directly on
                        // the `Button` via `Styled`/`InteractiveElement` would
                        // also conflict with the `.hover()` call `Button::render`
                        // already makes internally on the same `Interactivity`,
                        // which panics in debug builds (`hover style already
                        // set`). Keeping `.ghost()` until upstream exposes a
                        // composable per-state foreground/hover API.
                        Button::new(SharedString::from(format!("tag-delete-{k}")))
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.delete_tag(&key_for_delete, cx);
                            })),
                    )
                    .into_any_element()
            }));
        }

        let add_selection = selection.clone();
        list.child(
            Button::new("add-tag")
                .label("Add tag")
                .icon(IconName::Plus)
                .primary()
                .small()
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

#[cfg(test)]
mod preset_label_tests {
    use osm_gpui::layers::osm_layer::OsmLayer;
    use osm_gpui::layers::LayerManager;
    use osm_gpui::osm::{OsmData, OsmNode};
    use osm_gpui::selection::{FeatureKind, FeatureRef};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn manager_with_cafe_node() -> (LayerManager, FeatureRef) {
        let mut manager = LayerManager::new();
        let layer_id = manager.alloc_id();
        let mut tags = HashMap::new();
        tags.insert("amenity".to_string(), "cafe".to_string());
        let node = OsmNode {
            id: 1,
            lat: 40.0,
            lon: -74.0,
            version: 1,
            tags,
        };
        let data = Arc::new(OsmData {
            nodes: HashMap::from([(1, node)]),
            ways: HashMap::new(),
            relations: Vec::new(),
            bounds: None,
        });
        let layer = OsmLayer::new_with_data(layer_id, "L", data);
        manager.add_layer(Box::new(layer));
        let feature = FeatureRef {
            layer_id,
            kind: FeatureKind::Node,
            id: 1,
        };
        (manager, feature)
    }

    // MapViewer::describe_selected_feature needs a full `MapViewer` (a GPUI
    // `Context`-bound struct), which isn't practical to construct outside a
    // running app. Test the same lookup path directly instead, exercising
    // exactly what describe_selected_feature does internally.
    #[test]
    fn cafe_node_resolves_to_cafe_label() {
        let (manager, feature) = manager_with_cafe_node();
        let layer = manager.find_layer(feature.layer_id).unwrap();
        let editable = layer.as_editable().unwrap();
        let tags: HashMap<String, String> = editable
            .feature_tags(&feature)
            .unwrap()
            .into_iter()
            .collect();
        let geometry = editable
            .feature_geometry(&feature, osm_gpui::presets::area_keys())
            .unwrap();
        let (name, _icon) =
            osm_gpui::presets::describe_feature(osm_gpui::presets::preset_index(), &tags, geometry);
        assert_eq!(name, "Cafe");
    }
}
