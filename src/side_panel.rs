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

use crate::imagery_offset_ui::{AdjustLayerImageryOffset, ResetLayerImageryOffset};
use crate::{
    DeleteLayer, MapViewer, MoveLayer, PendingTagEditOpen, SetActiveLayer,
    ToggleLayerVisibilityAction,
};

use crate::side_panel_layout as layout;

/// Index of the History (undo) section in `MapViewer::side_panel_open`.
pub(crate) const HISTORY_SECTION: usize = 4;

/// What a Layers row needs of a layer: id, name, visible, modified, is an OSM
/// layer, and (imagery layers only) its imagery offset.
type LayerRowInfo = (
    LayerId,
    String,
    bool,
    bool,
    bool,
    Option<osm_gpui::imagery_offset::ImageryOffset>,
);

/// One membership of the selected feature in a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Membership {
    pub relation_id: i64,
    /// The relation's display name.
    pub name: String,
    pub role: String,
    /// Where the feature is in the relation's member list.
    pub index: usize,
}

/// Whether the Selection header offers the change-feature-type control, and
/// if so whether it is enabled: shown for exactly one selected feature (the
/// case the Fields section is shown for), disabled while an upload freezes
/// the layers.
pub(crate) fn header_change_type_control(selected: usize, edits_blocked: bool) -> Option<bool> {
    (selected == 1).then_some(!edits_blocked)
}

impl MapViewer {
    const SELECTION_MAX_VISIBLE_ROWS: usize = 10;
    /// A selection of tens of thousands (Edit > Select All, Edit > Find)
    /// lists only this many features; the rest are summarized.
    const SELECTION_LIST_CAP: usize = 200;
    /// Above this many selected features the Tags section stops combining
    /// their tags (that work is repeated on every frame).
    const TAGS_AGGREGATE_LIMIT: usize = 5000;

    /// The right pane: Layers, Selection, and Tags sections stacked
    /// top-to-bottom, each collapsible and sized to its content (the whole
    /// pane scrolls).
    pub(crate) fn render_side_panel(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let layer_info: Vec<LayerRowInfo> = self
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
                    layer.imagery_offset(),
                )
            })
            .collect();

        // How tall Layers, Validation and History lists may be (see
        // `side_panel_layout`); the middle zone gets the rest.
        let panel_height = (window.viewport_size().height
            - px(crate::status_bar::STATUS_BAR_HEIGHT)
            - crate::menu::top_chrome_height())
        .as_f32();
        let rows = |n: usize| n.max(1) as f32 * (PANEL_ROW_HEIGHT + 4.0);
        let plan = layout::plan(
            panel_height,
            layout::Want {
                open: self.side_panel_open[0],
                content: rows(layer_info.len()),
            },
            self.validation_want(),
            layout::Want {
                open: self.side_panel_open[HISTORY_SECTION],
                content: self.undo_stack.actions.len().max(1) as f32 * PANEL_ROW_HEIGHT,
            },
        );

        let layers_section = self.render_layers_section(&layer_info, plan.layers, cx);
        let selection_section = self.render_selection_section(cx);
        let fields_section = self.render_fields_section(window, cx);
        let tags_section = self.render_tags_section(cx);
        let history_section = self.render_history_section(plan.history, cx);
        let validation_section =
            self.render_validation_section(plan.validation.unwrap_or_default(), cx);
        let relations_section = self.render_relations_section(cx);
        let open_relations = self.side_panel_open[5];
        let members_section = self.render_members_section(window, cx);
        let open_members = self.side_panel_open[6];

        let open_layers = self.side_panel_open[0];
        let open_fields = self.side_panel_open[2];
        let open_tags = self.side_panel_open[3];
        let open_history = self.side_panel_open[HISTORY_SECTION];
        // Collapsed, the header says how many steps are in it.
        let steps = self.undo_stack.actions.len();
        let history_steps = (!open_history && steps > 0).then(|| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(muted_text_size())
                .text_color(cx.theme().muted_foreground)
                .child(tr_count!("panel.history.steps", steps))
                .into_any_element()
        });

        let selected_count = self.selected.len();
        let muted = cx.theme().muted_foreground;
        let selection_header = self.render_selection_header(cx);

        // An open note takes the Selection zone's place.
        let note_section = self
            .render_note_section(cx)
            .or_else(|| self.render_photo_section(cx));
        let note_open = note_section.is_some();
        let object_photos = self.render_object_photos_section(cx);
        let (note_title, note_status, note_body) = match note_section {
            Some((title, status, body)) => (Some(title), Some(status), Some(body)),
            None => (None, None, None),
        };
        let selection_title = match (note_title, self.selected.len()) {
            (Some(title), _) => title,
            (None, 0 | 1) => tr!("panel.selection.title"),
            (None, n) => tr_count!("panel.selection.title_count", n),
        };

        // The selection header stays put while there is room for it and the
        // scrolling part below; otherwise it scrolls with that part.
        let header_shown = selected_count == 1 && !note_open;
        let (pinned_header, scrolled_header) = match (header_shown, plan.pins_selection_header()) {
            (true, true) => (Some(selection_header), None),
            (true, false) => (None, Some(selection_header)),
            (false, _) => (None, None),
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
            // Stacked zones (see `side_panel_layout`): Layers keeps its
            // height up to a cap; the middle zone takes the rest and
            // scrolls; Validation and History are bounded lists under
            // always-visible headers.
            .child(
                self.collapsible_section(
                    tr!("panel.layers.title"),
                    0,
                    open_layers,
                    layers_section,
                    cx,
                )
                .flex_shrink_0(),
            )
            // The middle zone, the flexible one: a title strip and (for an
            // open note, or one selected feature when there is room) its
            // header, both pinned, then a lighter surface holding everything
            // else, which scrolls.
            .child(
                div()
                    .id("side-panel-middle")
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.zone_strip(selection_title, cx))
                    .children(note_status.map(|status| div().flex_shrink_0().child(status)))
                    .children(pinned_header.map(|header| {
                        div()
                            .flex_shrink_0()
                            .bg(cx.theme().background)
                            .child(header)
                    }))
                    .child(
                        div()
                            // A note and a selection keep separate scroll
                            // positions.
                            .id(if note_open {
                                "side-panel-note-scroll"
                            } else {
                                "side-panel-scroll"
                            })
                            .flex_1()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .bg(cx.theme().background)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_shrink_0()
                                    // Nothing selected: one friendly message instead
                                    // of empty Fields and Tags sections.
                                    .children(scrolled_header)
                                    .children(note_body)
                                    .when(selected_count == 0 && !note_open, |group| {
                                        group.child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .items_center()
                                                .gap_1()
                                                .px_4()
                                                .py_6()
                                                .child(
                                                    Label::new(tr!("panel.selection.nothing"))
                                                        .text_color(muted),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(muted_text_size())
                                                        .text_color(muted)
                                                        .text_center()
                                                        .child(tr!("panel.selection.nothing_hint")),
                                                ),
                                        )
                                    })
                                    .when(selected_count > 0 && !note_open, |group| {
                                        group
                                            .when(selected_count > 1, |group| {
                                                group.child(
                                                    div().px_2().py_1().child(selection_section),
                                                )
                                            })
                                            .when(selected_count == 1, |group| {
                                                group.child(self.collapsible_section(
                                                    tr!("panel.fields.title"),
                                                    2,
                                                    open_fields,
                                                    fields_section,
                                                    cx,
                                                ))
                                            })
                                            .children(object_photos)
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
                                    }),
                            ),
                    ),
            )
            // Validation, only after a validation was run: directly above
            // History, and like it able to shrink to its header.
            .children(validation_section)
            // Pinned to the bottom. It is the only zone besides the
            // selection and Validation that may shrink (to its header) when
            // the window is very short.
            .child(
                self.collapsible_section_with(
                    tr!("panel.history.title"),
                    HISTORY_SECTION,
                    open_history,
                    history_steps,
                    history_section,
                    cx,
                )
                .flex_shrink(1.)
                .min_h(px(32.))
                .overflow_hidden(),
            )
            // Last, so it sits on top of the scrolling content.
            .child(resize_handle)
    }

    /// The relations the single selected feature belongs to, one entry per
    /// membership. Empty unless exactly one feature is selected and a
    /// relation has it as a member.
    pub(crate) fn selected_memberships(&self) -> Vec<Membership> {
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
            for (index, member) in relation.members.iter().enumerate() {
                if member.member_type == member_type && member.reference == feature.id {
                    memberships.push(Membership {
                        relation_id,
                        name: name.clone(),
                        role: member.role.clone(),
                        index,
                    });
                }
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
    fn render_members_section(
        &self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Option<(usize, gpui::AnyElement)> {
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

        let sort_relation_id = relation.id;
        let sort_row = div().flex().flex_row().child(
            Button::new("members-sort")
                .label(tr!("panel.members.sort"))
                .tooltip(tr!("panel.members.sort_hint"))
                .ghost()
                .small()
                .disabled(total < 2)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sort_relation_members(layer_id, sort_relation_id);
                    cx.notify();
                })),
        );

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
                // The preset name may be cut short, but the id never is, so
                // rows of one kind stay distinguishable at the narrowest
                // panel width.
                let (title, ident) = osm_gpui::relations::member_label_parts(
                    member.member_type,
                    member.reference,
                    loaded,
                    described.as_deref(),
                );
                let label = div()
                    .flex()
                    .flex_row()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .when_some(title, |this, title| {
                        this.child(
                            div()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(title),
                        )
                        .child(div().flex_shrink_0().whitespace_nowrap().child(" · "))
                    })
                    .child(div().flex_shrink_0().whitespace_nowrap().child(ident));
                let letter: String = osm_gpui::relations::kind_label(member.member_type)
                    .chars()
                    .take(1)
                    .collect();

                let name = div()
                    .id(("member-name", i))
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
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
                // Quiet until the row is hovered or focus is on one of its
                // buttons (still in the layout and clickable, so the row
                // doesn't shift and the buttons stay reachable). The three
                // buttons are ordinary tab stops; this container's handle is
                // not one, it only groups them so `contains_focused` can say
                // whether focus is inside the row. (gpui's `in_focus` style
                // means the opposite: "inside a focused element".)
                let group_name = SharedString::from(format!("member-row-{i}"));
                let controls_focus = {
                    let mut handles = self.member_controls_focus.borrow_mut();
                    while handles.len() <= i {
                        handles.push(cx.focus_handle());
                    }
                    handles[i].clone()
                };
                let focus_in_row = controls_focus.contains_focused(window, cx);
                let controls = div()
                    .id(("member-controls", i))
                    .track_focus(&controls_focus)
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .opacity(if focus_in_row { 1. } else { 0. })
                    .group_hover(group_name.clone(), |s| s.opacity(1.))
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
                    .group(group_name)
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
                            .px_2()
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
                            .pl(px(26.))
                            .pr_2()
                            .pb_1()
                            .child(role_line),
                    )
            });

        let mut column = div().flex().flex_col().gap_0p5().child(sort_row);
        column = column.child(
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
                    |(
                        i,
                        Membership {
                            relation_id,
                            name,
                            role,
                            index,
                        },
                    )| {
                        let member_ref = crate::MemberRef {
                            layer_id,
                            relation_id,
                            index,
                        };
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
                            // Takes this feature out of that relation. (A
                            // Button swallows the mouse-down, so it doesn't
                            // also select the relation.)
                            .child(
                                Button::new(("relation-remove", i))
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .tooltip(tr!("panel.relations.remove"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_relation_member(member_ref);
                                        cx.notify();
                                    })),
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
    fn render_history_section(&self, max_height: f32, cx: &mut Context<Self>) -> gpui::AnyElement {
        // Collapsed: the rows are not drawn, so they are not built either.
        if !self.side_panel_open[HISTORY_SECTION] {
            return div().into_any_element();
        }
        if self.undo_stack.actions.is_empty() {
            return Label::new(tr!("panel.history.empty"))
                .text_color(cx.theme().muted_foreground)
                .into_any_element();
        }

        let cursor = self.undo_stack.cursor;
        div()
            .id("history-list")
            .flex()
            .flex_col()
            // A long history scrolls here rather than pushing everything
            // else off screen.
            .max_h(px(max_height))
            .overflow_y_scroll()
            .children(
                self.undo_stack
                    .actions
                    .iter()
                    .enumerate()
                    .map(|(i, action)| {
                        let is_current = i + 1 == cursor;
                        let is_future = i >= cursor;
                        // Long descriptions wrap onto a second line instead
                        // of running off the panel.
                        let mut row = panel_row(("history-row", i))
                            .h_auto()
                            .min_h(px(PANEL_ROW_HEIGHT))
                            .py_0p5()
                            .child(div().flex_1().min_w(px(0.)).child(action.description()));
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

    /// The title strip of a zone that does not collapse (the selection): the
    /// same tinted full-width bar as the Layers and History headers, with
    /// its text lined up with theirs.
    fn zone_strip(&self, title: impl Into<SharedString>, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(px(32.))
            .pl(px(24.))
            .pr_2()
            .bg(cx.theme().secondary)
            .child(
                Label::new(title)
                    .text_size(muted_text_size())
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::SEMIBOLD),
            )
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
    ) -> gpui::Div {
        self.collapsible_section_with(title, index, open, None, content, cx)
    }

    /// `collapsible_section` with `trailing` (if any) in the header after the
    /// title; it should take the rest of the header's width (`flex_1`).
    pub(crate) fn collapsible_section_with(
        &self,
        title: impl Into<gpui::SharedString>,
        index: usize,
        open: bool,
        trailing: Option<gpui::AnyElement>,
        content: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // Layers (0), History (4) and Validation (7) are zones of their own,
        // like the selection: their headers are full-width tinted strips (see
        // `zone_strip`). Sections inside the selection zone keep the plain
        // header and a hairline.
        let document_level =
            index == 0 || index == HISTORY_SECTION || index == crate::validation_panel::SECTION;
        let title_label = if document_level {
            Label::new(title)
                .text_size(muted_text_size())
                .text_color(cx.theme().foreground)
        } else {
            Label::new(title)
        };
        let header = div()
            .id(("section-header", index))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .when(document_level, |this| this.px_2())
            .when(!document_level, |this| this.px_3())
            .h(px(32.))
            .cursor_pointer()
            .when(document_level, |this| {
                this.bg(cx.theme().secondary)
                    .hover(|this| this.bg(cx.theme().secondary_hover))
            })
            .when(!document_level, |this| {
                this.hover(|this| this.bg(cx.theme().accent))
            })
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(cx.theme().muted_foreground),
            )
            .child(title_label.font_weight(gpui::FontWeight::SEMIBOLD))
            .children(trailing)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.side_panel_open[index] = !this.side_panel_open[index];
                cx.notify();
            }));

        let border = cx.theme().border;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            // Selection-level sections are divided by a hairline.
            .when(!document_level, |this| {
                this.border_t_1().border_color(border)
            })
            .child(header)
            .when(open, |this| {
                // The zones' rows carry their own inset; the selection
                // sections share one (12px) so Fields, Tags, Relations and
                // Members line up.
                this.child(
                    div()
                        // Validation may shrink in a short window: its body
                        // gives up height too, and its list scrolls.
                        .when(index == crate::validation_panel::SECTION, |this| {
                            this.flex().flex_col().flex_shrink(1.).min_h(px(0.))
                        })
                        .when(document_level, |this| this.px_2())
                        .when(!document_level, |this| this.px_3())
                        .pt_0p5()
                        .pb_2()
                        .child(content),
                )
            })
    }

    /// Resolve the friendly `(name, icon_svg_path)` for a selected feature,
    /// or `None` if the feature's layer/tags/geometry can't be found (e.g.
    /// it was deleted since selection). `icon_svg_path` is `None` when the
    /// matched preset has no icon or the icon file isn't vendored.
    pub(crate) fn describe_selected_feature(
        &self,
        feat: &osm_gpui::selection::FeatureRef,
    ) -> Option<(String, Option<SharedString>)> {
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
        let icon_path = osm_gpui::presets::icon_path_or_fallback(icon_name.as_deref(), geometry);
        Some((name, icon_path))
    }

    /// The anchor of the panel when exactly one feature is selected: its
    /// preset icon and name in large type, with its kind and id as secondary
    /// text. An empty element otherwise.
    fn render_selection_header(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui_component::Disableable as _;
        use osm_gpui::selection::FeatureKind;

        let [feat] = self.selected.as_slice() else {
            return div().into_any_element();
        };
        let kind_label = match feat.kind {
            FeatureKind::Node => tr!("panel.selection.node"),
            FeatureKind::Way => tr!("panel.selection.way"),
            FeatureKind::Relation => tr!("panel.selection.relation"),
        };
        let described = self.describe_selected_feature(feat);
        // The feature's own name (if it has one) leads; the preset name moves
        // down beside the kind and id.
        let own_name = self
            .layer_manager
            .find_layer(feat.layer_id)
            .and_then(|layer| layer.as_editable())
            .and_then(|editable| editable.feature_tags(feat))
            .and_then(|tags| {
                tags.into_iter()
                    .find(|(k, v)| k == "name" && !v.trim().is_empty())
                    .map(|(_, v)| v)
            });
        let plain = tr!("panel.selection.row", kind = kind_label, id = feat.id);
        let (title, detail, icon_path) = match (described, own_name) {
            (Some((preset, path)), Some(name)) => (
                name,
                tr!(
                    "panel.selection.row_named",
                    name = preset,
                    kind = kind_label,
                    id = feat.id
                ),
                path,
            ),
            (Some((preset, path)), None) => (preset, plain, path),
            (None, Some(name)) => (name, plain, None),
            (None, None) => (plain.to_string(), SharedString::default(), None),
        };
        let muted = cx.theme().muted_foreground;
        let body = osm_gpui::ui::style::current_text_scale().body;
        // Who last edited it, once its server history has been fetched (the
        // download itself does not keep that).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let last_edit = self.last_edit_for(feat).map(|edit| {
            osm_gpui::ui::object_history_view::last_edit_line(&edit, &self.web_base(), now, cx)
        });
        // Object History is in Edit > Show Object History and the feature's
        // context menu; the header only offers changing the feature type.
        let change_type_button = header_change_type_control(
            self.selected.len(),
            osm_gpui::transfer::blocks_edits(self.transfer.as_ref()),
        )
        .map(|enabled| {
            Button::new("change-feature-type")
                .ghost()
                .xsmall()
                .icon(Icon::empty().path(osm_gpui::assets::PENCIL_ICON_PATH))
                .disabled(!enabled)
                .tooltip(tr!("panel.fields.change_type"))
                .on_click(cx.listener(|_this, _, window, cx| {
                    window.dispatch_action(Box::new(crate::ChangeFeatureType), cx);
                }))
        });

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2p5()
            // The icon slot is always reserved (empty when even the geometry
            // fallback is missing) so the title never shifts between features.
            .child(
                div()
                    .size(px(24.0))
                    .flex_shrink_0()
                    .children(icon_path.map(|path| {
                        osm_gpui::presets::icon_element(
                            path,
                            24.0,
                            cx.theme().foreground,
                            cx.theme().is_dark(),
                        )
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(body + px(3.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(detail),
                    )
                    .children(last_edit),
            )
            .children(change_type_button)
            .into_any_element()
    }

    /// The Selection accordion section for a multi-selection: a scrollable
    /// list of the selected features (max ~10 rows visible, then scrolls).
    /// Clicking a row narrows the selection to just that feature.
    fn render_selection_section(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use osm_gpui::selection::FeatureKind;

        if self.selected.is_empty() {
            return div().into_any_element();
        }

        let visible_rows = self.selected.len().min(Self::SELECTION_MAX_VISIBLE_ROWS);
        let list_height = px(visible_rows as f32 * PANEL_ROW_HEIGHT);
        let hidden = self.selected.len().saturating_sub(Self::SELECTION_LIST_CAP);

        div()
            .id("selection-list")
            .flex()
            .flex_col()
            .h(list_height)
            .overflow_y_scroll()
            .when(hidden > 0, |list| {
                list.child(
                    div()
                        .px_2()
                        .h(px(PANEL_ROW_HEIGHT))
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr_count!(
                            "panel.selection.more",
                            hidden,
                            shown = Self::SELECTION_LIST_CAP
                        )),
                )
            })
            .children(
                self.selected
                    .iter()
                    .take(Self::SELECTION_LIST_CAP)
                    .enumerate()
                    .map(|(i, feat)| {
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

                        // Reserve the icon slot even without an icon so the row
                        // text lines up across rows.
                        row = row.child(div().size(px(14.0)).flex_shrink_0().children(
                            icon_path.map(|path| {
                                osm_gpui::presets::icon_element(
                                    path,
                                    14.0,
                                    cx.theme().foreground,
                                    cx.theme().is_dark(),
                                )
                            }),
                        ));

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
                    }),
            )
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
        layer_info: &[LayerRowInfo],
        max_height: f32,
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
            .id("layers-list")
            .flex()
            .flex_col()
            .gap_1()
            // Many layers scroll here rather than crowding out the selection.
            .max_h(px(max_height))
            .overflow_y_scroll()
            .children(
                layer_info
                    .iter()
                    .enumerate()
                    .map(
                        |(
                            index,
                            (layer_id, name, is_visible, is_modified, is_osm, imagery_offset),
                        )| {
                            let layer_id = *layer_id;
                            let is_imagery = imagery_offset.is_some();
                            // Imagery drawn shifted says so, so nobody traces
                            // on moved imagery without knowing.
                            let shifted = imagery_offset.filter(|o| !o.is_zero());
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

                            // GPX / GeoJSON layers carry their format as a tag.
                            let format_tag = self
                                .layer_manager
                                .layers()
                                .get(index)
                                .and_then(|l| {
                                    l.as_any().downcast_ref::<osm_gpui::layers::reference_layer::ReferenceLayer>()
                                })
                                .map(|l| l.format().tag());

                            row.gap_2()
                                .child(eye)
                                .child(name_label)
                                .children(format_tag.map(|tag| {
                                    div()
                                        .id(("layer-format-tag", index))
                                        .flex_shrink_0()
                                        .px_1()
                                        .rounded_sm()
                                        .text_size(muted_text_size())
                                        .bg(cx.theme().muted)
                                        .text_color(muted)
                                        .child(tag)
                                }))
                                .children(shifted.map(|offset| {
                                    div()
                                        .id(("layer-offset-tag", index))
                                        .flex_shrink_0()
                                        .px_1()
                                        .rounded_sm()
                                        .text_size(muted_text_size())
                                        .bg(cx.theme().warning.opacity(0.35))
                                        .text_color(cx.theme().foreground)
                                        .child(tr!(
                                            "panel.layers.offset_tag",
                                            east = osm_gpui::imagery_offset::format_metres(
                                                offset.east_m
                                            ),
                                            north = osm_gpui::imagery_offset::format_metres(
                                                offset.north_m
                                            )
                                        ))
                                }))
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
                                    if is_imagery {
                                        menu = menu.separator().menu(
                                            tr!("panel.layers.adjust_offset"),
                                            Box::new(AdjustLayerImageryOffset { index }),
                                        );
                                        if shifted.is_some() {
                                            menu = menu.menu(
                                                tr!("panel.layers.reset_offset"),
                                                Box::new(ResetLayerImageryOffset { index }),
                                            );
                                        }
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
    ///
    /// `target` says which input this is, for the autocomplete list that
    /// drops down from it (see `tag_suggest_ui`).
    fn tag_input_cell(
        &self,
        input: &gpui::Entity<gpui_component::input::InputState>,
        sizing_text: impl Into<SharedString>,
        target: osm_gpui::ui::tag_edit::TagEditField,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let cell = self.inline_input_cell(input, sizing_text, Self::cancel_tag_inline_edit, cx);
        self.with_tag_suggest(cell, target, cx)
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
            // Not shown at all when nothing is selected.
            return div().into_any_element();
        }

        if self.selected.len() > Self::TAGS_AGGREGATE_LIMIT {
            return div()
                .px_2()
                .py_1()
                .text_color(cx.theme().muted_foreground)
                .child(tr_count!(
                    "panel.tags.too_many",
                    self.selected.len(),
                    limit = Self::TAGS_AGGREGATE_LIMIT
                ))
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
                // Medium cells (8px inset, ~28px rows) match the Fields
                // controls' inset and height.
                .with_size(Size::Medium);

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
                        .tag_input_cell(&edit.key, k.clone(), TagEditField::Key, cx)
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
                        .tag_input_cell(&edit.value, value_text.clone(), TagEditField::Value, cx)
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
                        self.tag_input_cell(
                            &edit.key,
                            tr!("panel.tags.key_placeholder"),
                            TagEditField::Key,
                            cx,
                        )
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
                                TagEditField::Value,
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
mod header_control_tests {
    use super::header_change_type_control;

    #[test]
    fn change_type_is_offered_for_a_single_selection_only() {
        assert_eq!(header_change_type_control(0, false), None);
        assert_eq!(header_change_type_control(1, false), Some(true));
        assert_eq!(header_change_type_control(2, false), None);
    }

    #[test]
    fn change_type_is_disabled_while_an_upload_runs() {
        assert_eq!(header_change_type_control(1, true), Some(false));
        assert_eq!(header_change_type_control(3, true), None);
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
            bounds: Vec::new(),
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
