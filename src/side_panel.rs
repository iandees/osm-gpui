//! The right-hand side panel: Layers, Selection, Tags, and History sections.

use gpui::{div, prelude::*, px, Context, MouseDownEvent, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    description_list::{DescriptionItem, DescriptionList},
    input::Input,
    label::Label,
    menu::ContextMenuExt,
    ActiveTheme, Icon, IconName, Sizable,
};

use osm_gpui::layers::LayerId;
use osm_gpui::ui::style::{interactive_row, muted_text_size, panel_row, PANEL_ROW_HEIGHT};
use osm_gpui::{tr, tr_count};

use crate::{
    DeleteLayer, MapViewer, MoveLayer, PendingTagEditOpen, SetActiveLayer,
    ToggleLayerVisibilityAction,
};

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
        let relations_section = self.render_relations_section(cx);
        let open_relations = self.side_panel_open[5];
        let members_section = self.render_members_section(cx);
        let open_members = self.side_panel_open[6];

        let open_layers = self.side_panel_open[0];
        let open_selection = self.side_panel_open[1];
        let open_fields = self.side_panel_open[2];
        let open_tags = self.side_panel_open[3];
        let open_history = self.side_panel_open[4];

        let selection_title = match self.selected.len() {
            0 => tr!("panel.selection.title"),
            n => tr_count!("panel.selection.title_count", n),
        };

        // A thin strip along the panel's left edge that drags its width.
        // The drag itself is tracked on the window root (see `render`), so
        // it keeps following the pointer once it leaves this strip.
        let handle_hover = cx.theme().border;
        let resize_handle = div()
            .id("side-panel-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(5.0))
            .cursor_col_resize()
            .hover(move |this| this.bg(handle_hover))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    this.start_side_panel_resize(ev.position.x.as_f32(), cx);
                    cx.stop_propagation();
                }),
            );

        div()
            .w(px(self.side_panel_width))
            .flex_shrink_0()
            .h_full()
            .relative()
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
                    .child(self.collapsible_section(
                        tr!("panel.layers.title"),
                        0,
                        open_layers,
                        layers_section,
                        cx,
                    ))
                    .child(self.collapsible_section(
                        selection_title,
                        1,
                        open_selection,
                        selection_section,
                        cx,
                    ))
                    .child(self.collapsible_section(
                        tr!("panel.fields.title"),
                        2,
                        open_fields,
                        fields_section,
                        cx,
                    ))
                    .child(self.collapsible_section(
                        tr!("panel.tags.title"),
                        3,
                        open_tags,
                        tags_section,
                        cx,
                    ))
                    .children(members_section.map(|(count, section)| {
                        self.collapsible_section(
                            tr_count!("panel.members.title_count", count),
                            6,
                            open_members,
                            section,
                            cx,
                        )
                    }))
                    .children(relations_section.map(|section| {
                        self.collapsible_section(
                            tr!("panel.relations.title"),
                            5,
                            open_relations,
                            section,
                            cx,
                        )
                    }))
                    .child(self.collapsible_section(
                        tr!("panel.history.title"),
                        4,
                        open_history,
                        history_section,
                        cx,
                    )),
            )
            // Last, so it sits on top of the scrolling content.
            .child(resize_handle)
    }

    /// The relations the single selected feature belongs to, one entry per
    /// membership: `(relation id, display name, role)`. Empty unless exactly
    /// one feature is selected and a relation has it as a member.
    pub(crate) fn selected_memberships(&self) -> Vec<(i64, String, String)> {
        use osm_gpui::osm::MemberType;
        use osm_gpui::selection::FeatureKind;

        let [feature] = self.selected.as_slice() else {
            return Vec::new();
        };
        let Some(editable) = self
            .layer_manager
            .find_layer(feature.layer_id)
            .and_then(|layer| layer.as_editable())
        else {
            return Vec::new();
        };
        let (member_type, relation_ids) = match feature.kind {
            FeatureKind::Node => (
                MemberType::Node,
                editable.relations_containing_node(feature.id),
            ),
            FeatureKind::Way => (
                MemberType::Way,
                editable.relations_containing_way(feature.id),
            ),
            // The Relations section lists what the selected node or way
            // belongs to; a relation's own contents are in its Members section.
            FeatureKind::Relation => return Vec::new(),
        };
        let mut memberships = Vec::new();
        for relation_id in relation_ids {
            let Some(relation) = editable.relation(relation_id) else {
                continue;
            };
            let name = osm_gpui::relations::display_name(&relation);
            for role in osm_gpui::relations::roles_of(&relation, member_type, feature.id) {
                memberships.push((relation_id, name.clone(), role.to_string()));
            }
        }
        memberships
    }

    /// How many members the Members list renders before "Show all": a route
    /// can have hundreds, and every row is several elements.
    const MEMBERS_INITIAL_ROWS: usize = 100;

    /// The Members accordion section for a single selected relation, with
    /// its member count (for the title): the ordered member list, each with
    /// a name that selects it, an editable role, and move/remove buttons.
    /// `None` unless exactly one relation is selected.
    fn render_members_section(&self, cx: &mut Context<Self>) -> Option<(usize, gpui::AnyElement)> {
        use crate::MemberRef;
        use gpui_component::Disableable as _;
        use osm_gpui::osm::MemberType;
        use osm_gpui::selection::{FeatureKind, FeatureRef};

        let [feature] = self.selected.as_slice() else {
            return None;
        };
        if feature.kind != FeatureKind::Relation {
            return None;
        }
        let layer_id = feature.layer_id;
        let editable = self.layer_manager.find_layer(layer_id)?.as_editable()?;
        let relation = editable.relation(feature.id)?;
        let total = relation.members.len();
        let muted = cx.theme().muted_foreground;
        if total == 0 {
            return Some((
                0,
                Label::new(tr!("panel.members.empty"))
                    .text_color(muted)
                    .into_any_element(),
            ));
        }
        let shown = if self.members_show_all {
            total
        } else {
            total.min(Self::MEMBERS_INITIAL_ROWS)
        };

        let rows = relation
            .members
            .iter()
            .enumerate()
            .take(shown)
            .map(|(i, member)| {
                let member_ref = MemberRef {
                    layer_id,
                    relation_id: relation.id,
                    index: i,
                };
                let (kind, loaded) = match member.member_type {
                    MemberType::Node => (
                        FeatureKind::Node,
                        editable.node_lat_lon(member.reference).is_some(),
                    ),
                    MemberType::Way => (
                        FeatureKind::Way,
                        editable.way_node_ids(member.reference).is_some(),
                    ),
                    MemberType::Relation => (
                        FeatureKind::Relation,
                        editable.relation(member.reference).is_some(),
                    ),
                };
                let target = FeatureRef {
                    layer_id,
                    kind,
                    id: member.reference,
                };
                let described = loaded
                    .then(|| self.describe_selected_feature(&target))
                    .flatten()
                    .map(|(name, _)| name);
                let label = osm_gpui::relations::member_label(
                    member.member_type,
                    member.reference,
                    loaded,
                    described.as_deref(),
                );
                let letter: String = osm_gpui::relations::kind_label(member.member_type)
                    .chars()
                    .take(1)
                    .collect();

                let name = div()
                    .id(("member-name", i))
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label);
                let name = if loaded {
                    let hover_bg = cx.theme().list_hover;
                    name.cursor_pointer()
                        .rounded_sm()
                        .hover(move |this| this.bg(hover_bg))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                this.set_selection(vec![target]);
                                cx.notify();
                            }),
                        )
                } else {
                    name.italic().text_color(muted)
                };

                let button = |id: &'static str, icon: IconName, tip: SharedString| {
                    Button::new((id, i))
                        .icon(icon)
                        .ghost()
                        .xsmall()
                        .tooltip(tip)
                };
                let controls = div()
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .child(
                        button("member-up", IconName::ArrowUp, tr!("panel.members.move_up"))
                            .disabled(i == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_relation_member(member_ref, -1);
                                cx.notify();
                            })),
                    )
                    .child(
                        button(
                            "member-down",
                            IconName::ArrowDown,
                            tr!("panel.members.move_down"),
                        )
                        .disabled(i + 1 == total)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.move_relation_member(member_ref, 1);
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "member-remove",
                            IconName::Close,
                            tr!("panel.members.remove"),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_relation_member(member_ref);
                            cx.notify();
                        })),
                    );

                let role = member.role.clone();
                let role_line = match self.role_edit.as_ref().filter(|e| e.member == member_ref) {
                    Some(edit) => {
                        let sizing = if role.is_empty() {
                            tr!("panel.members.role")
                        } else {
                            SharedString::from(role.clone())
                        };
                        self.inline_input_cell(&edit.input, sizing, Self::cancel_role_edit, cx)
                            .into_any_element()
                    }
                    None => {
                        let text = if role.is_empty() {
                            div().italic().child(tr!("panel.relations.no_role"))
                        } else {
                            div().child(role.clone())
                        };
                        text.id(("member-role", i))
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .cursor_text()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                    this.pending_role_edit_open = Some((member_ref, role.clone()));
                                    cx.notify();
                                }),
                            )
                            .into_any_element()
                    }
                };

                div()
                    .id(("member-row", i))
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .h(px(PANEL_ROW_HEIGHT))
                            .px_1()
                            .child(
                                div()
                                    .w(px(14.))
                                    .flex_shrink_0()
                                    .text_size(muted_text_size())
                                    .text_color(muted)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(letter),
                            )
                            .child(name)
                            .child(controls),
                    )
                    // The role sits under the name, indented past the
                    // type letter, so a narrow panel has room for both.
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .pl(px(24.))
                            .pr_1()
                            .pb_1()
                            .child(role_line),
                    )
            });

        let mut column = div().flex().flex_col().gap_0p5().child(
            div()
                .id("members-list")
                .flex()
                .flex_col()
                .max_h(px(360.))
                .overflow_y_scroll()
                .children(rows),
        );
        if shown < total {
            column = column.child(
                div().flex().flex_row().child(
                    Button::new("members-show-all")
                        .label(tr!("panel.members.show_all", count = total))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.members_show_all = true;
                            cx.notify();
                        })),
                ),
            );
        }
        Some((total, column.into_any_element()))
    }

    /// The Relations accordion section: one read-only row per relation the
    /// selected feature is a member of, with its role. `None` (no section at
    /// all) when there is nothing to list.
    fn render_relations_section(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let memberships = self.selected_memberships();
        if memberships.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let layer_id = self.selected[0].layer_id;
        Some(
            div()
                .flex()
                .flex_col()
                .children(memberships.into_iter().enumerate().map(
                    |(i, (relation_id, name, role))| {
                        let role_text = if role.is_empty() {
                            div().italic().child(tr!("panel.relations.no_role"))
                        } else {
                            div().child(role)
                        };
                        // Clicking the row selects the relation.
                        interactive_row(("relation-row", i), false, cx)
                            .gap_2()
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                    this.select_relation(layer_id, relation_id);
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name),
                            )
                            .child(
                                role_text
                                    .flex_shrink_0()
                                    .text_size(muted_text_size())
                                    .text_color(muted),
                            )
                    },
                ))
                .into_any_element(),
        )
    }

    /// The History accordion section: a passive list of every undoable
    /// action in order. The most recently applied action (the stack's
    /// current position) is highlighted; anything after it is available to
    /// redo but not currently applied, and renders dimmed.
    fn render_history_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.undo_stack.actions.is_empty() {
            return Label::new(tr!("panel.history.empty"))
                .text_color(cx.theme().muted_foreground)
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
            .child(Label::new(title).font_weight(gpui::FontWeight::SEMIBOLD))
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
    pub(crate) fn describe_selected_feature(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(String, Option<std::path::PathBuf>)> {
        let layer = self.layer_manager.find_layer(feat.layer_id)?;
        let editable = layer.as_editable()?;
        let tags: std::collections::HashMap<String, String> =
            editable.feature_tags(feat)?.into_iter().collect();
        let geometry = editable.feature_geometry(feat, osm_gpui::presets::area_keys())?;
        let (mut name, icon_name) =
            osm_gpui::presets::describe_feature(osm_gpui::presets::preset_index(), &tags, geometry);
        // Only the catch-all "Relation" preset matches a relation of a kind
        // the schema doesn't know; its `type` says more than that does.
        if feat.kind == osm_gpui::selection::FeatureKind::Relation {
            let matched = osm_gpui::presets::preset_index().match_feature(&tags, geometry);
            if matched.is_none_or(|p| p.id == "relation") {
                if let Some(kind) = tags.get("type").filter(|t| !t.trim().is_empty()) {
                    name = kind.clone();
                }
            }
        }
        let icon_path = icon_name.and_then(|n| osm_gpui::presets::icon_path(&n));
        Some((name, icon_path))
    }

    /// The Selection accordion section: a scrollable list of the selected
    /// features (max ~10 rows visible, then scrolls). Clicking a row narrows
    /// the selection to just that feature.
    fn render_selection_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::selection::FeatureKind;

        if self.selected.is_empty() {
            return Label::new(tr!("panel.selection.empty"))
                .text_color(cx.theme().muted_foreground)
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
                    FeatureKind::Node => tr!("panel.selection.node"),
                    FeatureKind::Way => tr!("panel.selection.way"),
                    FeatureKind::Relation => tr!("panel.selection.relation"),
                };
                let row_feat = *feat;
                let described = self.describe_selected_feature(feat);
                let row_text = match &described {
                    Some((name, _)) => tr!(
                        "panel.selection.row_named",
                        name = name,
                        kind = kind_label,
                        id = feat.id
                    ),
                    None => tr!("panel.selection.row", kind = kind_label, id = feat.id),
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

    /// The Layers accordion section: one row per layer, with an eye toggle
    /// for visibility on the left and, for OSM layers, click-to-activate on
    /// the rest of the row. The active layer — the one the drawing modes
    /// write into — is the highlighted row carrying the "editing" badge. A
    /// right-click context menu offers the same actions plus reordering and
    /// delete.
    ///
    /// Both click targets are plain `on_mouse_down` listeners rather than
    /// `Button`/`Checkbox::on_click`: `.context_menu(...)` wraps the row in
    /// an extra hitbox layer, and gpui's paired mouse-down/mouse-up click
    /// detection through it proved fragile (first click ignored, menu
    /// dismissing itself on mouse move).
    fn render_layers_section(
        &self,
        layer_info: &[(LayerId, String, bool, bool, bool)],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let total = layer_info.len();
        if total == 0 {
            return Label::new(tr!("panel.layers.empty"))
                .text_color(cx.theme().muted_foreground)
                .into_any_element();
        }

        let muted = cx.theme().muted_foreground;
        let foreground = cx.theme().foreground;

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
                            let is_visible = *is_visible;

                            let eye = div()
                                .id(("layer-eye", index))
                                .flex_shrink_0()
                                .cursor_pointer()
                                .child(
                                    Icon::new(if is_visible {
                                        IconName::Eye
                                    } else {
                                        IconName::EyeOff
                                    })
                                    .small()
                                    .text_color(if is_visible { foreground } else { muted }),
                                )
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                        this.toggle_layer_visibility(layer_id);
                                        // Don't also activate the layer.
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                );

                            let name_label = div()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(if is_visible { foreground } else { muted })
                                .when(is_active, |this| {
                                    this.font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .child(if *is_modified {
                                    format!("{} \u{2022}", name)
                                } else {
                                    name.clone()
                                });

                            // Only OSM layers can be activated, so only
                            // their rows look and act clickable.
                            let row = if is_osm {
                                interactive_row(("layer-row", index), is_active, cx).on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(move |this, _ev: &MouseDownEvent, _, cx| {
                                        this.active_layer = Some(layer_id);
                                        cx.notify();
                                    }),
                                )
                            } else {
                                panel_row(("layer-row", index))
                            };

                            row.gap_2()
                                .child(eye)
                                .child(name_label)
                                .when(is_active, |this| {
                                    this.child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(muted_text_size())
                                            .text_color(muted)
                                            .child(tr!("panel.layers.editing")),
                                    )
                                })
                                .context_menu(move |menu, _window, _cx| {
                                    let mut menu = menu;
                                    if is_osm && !is_active {
                                        menu = menu.menu(
                                            tr!("panel.layers.set_active"),
                                            Box::new(SetActiveLayer { index }),
                                        );
                                    }
                                    if index > 0 {
                                        menu = menu.menu(
                                            tr!("panel.layers.move_up"),
                                            Box::new(MoveLayer { index, delta: -1 }),
                                        );
                                    }
                                    if index + 1 < total {
                                        menu = menu.menu(
                                            tr!("panel.layers.move_down"),
                                            Box::new(MoveLayer { index, delta: 1 }),
                                        );
                                    }
                                    menu.separator()
                                        .menu(
                                            if is_visible {
                                                tr!("panel.layers.hide")
                                            } else {
                                                tr!("panel.layers.show")
                                            },
                                            Box::new(ToggleLayerVisibilityAction { index }),
                                        )
                                        .separator()
                                        .menu(
                                            tr!("panel.layers.delete"),
                                            Box::new(DeleteLayer { index }),
                                        )
                                })
                                .into_any_element()
                        },
                    )
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    /// One cell of the Tags table while it is being edited in place.
    ///
    /// The cell must not change size when it switches between showing text
    /// and editing it, so the layout is still driven by text: `sizing_text`
    /// (the cell's text as it was before the edit, or a placeholder for a
    /// new row) is laid out invisibly, and the input is positioned on top
    /// of it, filling exactly that box. The input is borderless, set in the
    /// table's own text size, and shifted left by its built-in padding so
    /// its text starts where the plain text did; a ring drawn as a shadow
    /// (which takes no layout space) marks it as the field being edited.
    ///
    /// The wrapper also turns Escape into "abandon the edit". (Enter and
    /// loss of focus commit; those arrive as `InputEvent`s — see
    /// `MapViewer::on_tag_input_event`.)
    fn tag_input_cell(
        &self,
        input: &gpui::Entity<gpui_component::input::InputState>,
        sizing_text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        self.inline_input_cell(input, sizing_text, Self::cancel_tag_inline_edit, cx)
    }

    /// `tag_input_cell` for any inline edit: `cancel` is what Escape does.
    fn inline_input_cell(
        &self,
        input: &gpui::Entity<gpui_component::input::InputState>,
        sizing_text: impl Into<SharedString>,
        cancel: fn(&mut Self, &mut gpui::Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::Size;

        // `Size::Size(s)` renders input text at `s * 0.875` with 8px of
        // horizontal padding; pick `s` so the text matches the body size.
        let body = osm_gpui::ui::style::current_text_scale().body;
        let input_padding = px(8.0);
        let ring = gpui::BoxShadow {
            color: cx.theme().ring,
            offset: gpui::point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.5),
            // Inset, so the table cell's clipping can't cut the ring off.
            inset: true,
        };

        div()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .on_key_down(
                cx.listener(move |this, ev: &gpui::KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "escape" {
                        cancel(this, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(div().invisible().child(sizing_text.into()))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(-input_padding)
                    .right_0()
                    .rounded_sm()
                    .bg(cx.theme().background)
                    .shadow(vec![ring])
                    .child(
                        Input::new(input)
                            .with_size(Size::Size(body / 0.875))
                            .appearance(false)
                            .h_full(),
                    ),
            )
    }

    /// The Tags accordion section: tags aggregated across every selected
    /// feature. A key shows its value only if every selected feature has
    /// that exact same value (a feature missing the key counts as its own
    /// distinct state); otherwise it shows "<N values>".
    ///
    /// Tags are edited in place: clicking a key or a value turns that cell
    /// into a text input (Enter or clicking away commits, Escape abandons),
    /// the trailing "x" removes the tag immediately, and "Add tag" appends
    /// a row with empty key and value inputs.
    fn render_tags_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::ui::tag_edit::TagEditField;

        if self.selected.is_empty() {
            return Label::new(tr!("panel.tags.no_selection"))
                .text_color(cx.theme().muted_foreground)
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

        // The row (by key) and cell being edited in place, if any; the add
        // row is handled separately below.
        let editing = self
            .tag_inline_edit
            .as_ref()
            .filter(|edit| !edit.ctx.is_add);
        let adding = self.tag_inline_edit.as_ref().filter(|edit| edit.ctx.is_add);

        // Keys get about a third of the panel, within sensible bounds.
        let key_width = px((self.side_panel_width * 0.34).clamp(90.0, 190.0));

        use gpui_component::Size;

        let mut list = div().flex().flex_col().gap_2();

        if aggregated.is_empty() && adding.is_none() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("panel.tags.empty")),
            );
        } else {
            let mut description_list = DescriptionList::new()
                .columns(1)
                .label_width(key_width)
                .bordered(true)
                .with_size(Size::Small);

            for (k, v) in aggregated.into_iter() {
                let value_text = match v {
                    osm_gpui::selection::TagValue::Single(s) => s,
                    osm_gpui::selection::TagValue::Multiple(n) => format!("<{} values>", n),
                };
                let editing_field = editing
                    .filter(|edit| edit.ctx.original_key == k)
                    .map(|edit| (edit.field, edit));

                // Clicking a cell asks for it to become an input.
                let open_edit = |select: TagEditField| {
                    let features = selection.clone();
                    let original_key = k.clone();
                    let original_value = value_text.clone();
                    move |this: &mut Self,
                          _ev: &MouseDownEvent,
                          _window: &mut gpui::Window,
                          cx: &mut Context<Self>| {
                        this.pending_tag_edit_open = Some(PendingTagEditOpen {
                            features: features.clone(),
                            original_key: original_key.clone(),
                            original_value: original_value.clone(),
                            select,
                            is_add: false,
                        });
                        cx.notify();
                    }
                };

                let key_element = match editing_field {
                    Some((TagEditField::Key, edit)) => self
                        .tag_input_cell(&edit.key, k.clone(), cx)
                        .into_any_element(),
                    _ => div()
                        .id(SharedString::from(format!("tag-key-{k}")))
                        .cursor_text()
                        .child(k.clone())
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(open_edit(TagEditField::Key)),
                        )
                        .into_any_element(),
                };

                let value_cell = match editing_field {
                    Some((TagEditField::Value, edit)) => self
                        .tag_input_cell(&edit.value, value_text.clone(), cx)
                        .into_any_element(),
                    _ => div()
                        .id(SharedString::from(format!("tag-value-{k}")))
                        .flex_1()
                        // Flex items default to a min-width equal to
                        // their content's natural width, which blocks
                        // wrapping for long unbroken values (e.g. a
                        // URL) and forces them to overflow instead.
                        // Zero it out so long values wrap.
                        .min_w(px(0.))
                        .cursor_text()
                        .child(value_text.clone())
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(open_edit(TagEditField::Value)),
                        )
                        .into_any_element(),
                };

                let key_for_delete = k.clone();
                let value_element = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_1()
                    .w_full()
                    .child(value_cell)
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
                    .into_any_element();

                description_list = description_list.child(
                    DescriptionItem::new(key_element)
                        .value(value_element)
                        .span(1),
                );
            }

            if let Some(edit) = adding {
                // The add row is a real table row whose two cells are
                // inputs, so it has exactly the size and position the new
                // tag's row will have once committed.
                description_list = description_list.child(
                    DescriptionItem::new(
                        self.tag_input_cell(&edit.key, tr!("panel.tags.key_placeholder"), cx)
                            .into_any_element(),
                    )
                    .value(
                        div()
                            .flex()
                            .flex_row()
                            .w_full()
                            .child(self.tag_input_cell(
                                &edit.value,
                                tr!("panel.tags.value_placeholder"),
                                cx,
                            ))
                            .into_any_element(),
                    )
                    .span(1),
                );
            }

            list = list.child(description_list);
        }

        if adding.is_some() {
            return list.into_any_element();
        }

        let add_selection = selection.clone();
        list.child(
            div().flex().flex_row().child(
                Button::new("add-tag")
                    .label(tr!("panel.tags.add"))
                    .icon(IconName::Plus)
                    .ghost()
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
            ),
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
