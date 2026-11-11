//! The object history view: the versions the OSM server has of the selected
//! node, way or relation, newest first, with what each one changed.
//!
//! A card floating over the top-left of the map rather than a modal: it does
//! not dim or block the map, so a mapper can keep editing (and selecting
//! other things) while it is open. It scrolls on its own, and shows only the
//! newest versions until "Show more" is pressed, so a history of hundreds of
//! versions stays cheap to draw.
//!
//! Not the app's undo history (the side panel's "History" section).

use std::collections::BTreeMap;

use gpui::{
    div, prelude::*, px, relative, AnyElement, App, Context, ElementId, EventEmitter, FocusHandle,
    Focusable, SharedString, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};

use crate::object_history::{
    build_entries, format_date, format_distance, redacted_text, web_changeset_url, web_history_url,
    web_user_url, HistoryEntry, HistoryError, HistoryVersion, LastEdit, ObjectHistory, ObjectState,
    StateChange, TagChange, INITIAL_VERSIONS_SHOWN, SHOW_MORE_STEP,
};
use crate::selection::FeatureKind;
use crate::ui::restore_dialog::age_phrase;
use crate::ui::style::{body_text_size, muted_text_size};
use crate::{tr, tr_count};

pub enum ViewEvent {
    /// Closed by the user (this also cancels a running request).
    Close,
    /// The Retry button after a network failure.
    Retry,
    /// "Revert tags to this version": set the object's tags to these.
    RevertTags {
        version: u32,
        tags: BTreeMap<String, String>,
    },
}

/// Where the view is in loading the history.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadState {
    Loading,
    Loaded(ObjectHistory),
    Failed(HistoryError),
}

pub struct ObjectHistoryView {
    kind: FeatureKind,
    id: i64,
    state: LoadState,
    /// The object as it is in the editor now, for "your unsaved changes".
    local: Option<ObjectState>,
    entries: Vec<HistoryEntry>,
    /// How many of `entries` are drawn.
    shown: usize,
    /// The website matching the API server, for links.
    web_base: String,
    /// The API server, for messages.
    server: String,
    focus_handle: FocusHandle,
}

impl EventEmitter<ViewEvent> for ObjectHistoryView {}

impl Focusable for ObjectHistoryView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A clickable piece of text that opens `url` in the browser.
fn link(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    url: String,
    cx: &App,
) -> AnyElement {
    crate::ui::style::link(id, text, cx)
        .on_click(move |_, _, _| {
            let _ = open::that(&url);
        })
        .into_any_element()
}

/// The widest the last-edit line's user name gets before it is cut short
/// (the full name is in its tooltip).
const LAST_EDIT_USER_MAX_WIDTH: f32 = 140.0;

/// "changeset 123" as one unit: a no-break space keeps the word and its
/// number on the same line.
pub fn changeset_text(label: &str, changeset: i64) -> String {
    format!("{label}\u{a0}{changeset}")
}

/// The Selection header's muted "last edit" line: `v7 · Name · 3 months ago
/// · changeset 123`, with the user and the changeset as links to the
/// website. Each part is a unit that never splits, so the line wraps between
/// parts (two lines at most); a long user name is cut with its full text in a
/// tooltip.
pub fn last_edit_line(edit: &LastEdit, web_base: &str, now: i64, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let (link_color, link_hover) = (cx.theme().link, cx.theme().link_hover);
    let mut units: Vec<AnyElement> = vec![div()
        .child(tr!(
            "object_history.last_edit.version",
            version = edit.version
        ))
        .into_any_element()];
    units.push(match &edit.user {
        Some(user) => {
            let url = web_user_url(web_base, user);
            let full = SharedString::from(user.clone());
            div()
                .id("last-edit-user")
                .max_w(px(LAST_EDIT_USER_MAX_WIDTH))
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .cursor_pointer()
                .text_color(link_color)
                .hover(move |this| this.text_color(link_hover).underline())
                .tooltip({
                    let full = full.clone();
                    move |window, cx| Tooltip::new(full.clone()).build(window, cx)
                })
                .on_click(move |_, _, _| {
                    let _ = open::that(&url);
                })
                .child(full)
                .into_any_element()
        }
        None => div()
            .child(tr!("object_history.unknown_user"))
            .into_any_element(),
    });
    if let Some(ts) = edit.timestamp {
        units.push(
            div()
                .child(age_phrase(now.max(0) as u64, ts.max(0) as u64))
                .into_any_element(),
        );
    }
    if let Some(changeset) = edit.changeset {
        units.push(link(
            "last-edit-changeset",
            changeset_text(&tr!("object_history.last_edit.changeset"), changeset),
            web_changeset_url(web_base, changeset),
            cx,
        ));
    }
    let last = units.len() - 1;
    h_flex()
        .flex_wrap()
        .gap_x_1()
        .text_size(muted_text_size())
        .text_color(muted)
        .children(units.into_iter().enumerate().map(|(i, unit)| {
            h_flex()
                .gap_x_1()
                .whitespace_nowrap()
                .min_w_0()
                .child(unit)
                .when(i != last, |this| this.child("·"))
        }))
        .into_any_element()
}

impl ObjectHistoryView {
    pub fn new(
        kind: FeatureKind,
        id: i64,
        web_base: String,
        server: String,
        cx: &mut Context<Self>,
    ) -> Self {
        ObjectHistoryView {
            kind,
            id,
            state: LoadState::Loading,
            local: None,
            entries: Vec::new(),
            shown: INITIAL_VERSIONS_SHOWN,
            web_base,
            server,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn kind_and_id(&self) -> (FeatureKind, i64) {
        (self.kind, self.id)
    }

    pub fn state(&self) -> &LoadState {
        &self.state
    }

    /// How many server versions the loaded history has (0 until loaded).
    pub fn version_count(&self) -> usize {
        match &self.state {
            LoadState::Loaded(h) => h.versions.len(),
            _ => 0,
        }
    }

    /// Whether the "Your unsaved changes" row is showing.
    pub fn has_local_row(&self) -> bool {
        self.entries
            .iter()
            .any(|e| matches!(e, HistoryEntry::Local { .. }))
    }

    pub fn set_loading(&mut self, cx: &mut Context<Self>) {
        self.state = LoadState::Loading;
        self.entries.clear();
        cx.notify();
    }

    pub fn set_loaded(
        &mut self,
        history: ObjectHistory,
        local: Option<ObjectState>,
        cx: &mut Context<Self>,
    ) {
        self.local = local;
        self.entries = build_entries(&history, self.local.as_ref());
        self.shown = INITIAL_VERSIONS_SHOWN;
        self.state = LoadState::Loaded(history);
        cx.notify();
    }

    pub fn set_failed(&mut self, error: HistoryError, cx: &mut Context<Self>) {
        self.state = LoadState::Failed(error);
        self.entries.clear();
        cx.notify();
    }

    /// The object changed locally (a revert, an undo): redo the diff.
    pub fn set_local(&mut self, local: Option<ObjectState>, cx: &mut Context<Self>) {
        if self.local == local {
            return;
        }
        self.local = local;
        if let LoadState::Loaded(history) = &self.state {
            self.entries = build_entries(history, self.local.as_ref());
        }
        cx.notify();
    }

    pub fn press_close(&mut self, cx: &mut Context<Self>) {
        cx.emit(ViewEvent::Close);
    }

    pub fn press_retry(&mut self, cx: &mut Context<Self>) {
        cx.emit(ViewEvent::Retry);
    }

    pub fn press_show_more(&mut self, cx: &mut Context<Self>) {
        self.shown += SHOW_MORE_STEP;
        cx.notify();
    }

    pub fn press_revert(&mut self, version: u32, cx: &mut Context<Self>) {
        if let LoadState::Loaded(history) = &self.state {
            if let Some(v) = history.version(version).filter(|v| v.visible) {
                cx.emit(ViewEvent::RevertTags {
                    version,
                    tags: v.state.tags.clone(),
                });
            }
        }
    }

    fn change_lines(&self, change: &StateChange, cx: &App) -> Vec<AnyElement> {
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        let size = muted_text_size();
        let mut lines: Vec<AnyElement> = Vec::new();
        let mut line = |color: Option<gpui::Hsla>, text: String| {
            let mut d = div().text_size(size).min_w(px(0.)).child(text);
            if let Some(c) = color {
                d = d.text_color(c);
            }
            lines.push(d.into_any_element());
        };
        for t in &change.tags {
            match t {
                TagChange::Added { key, value } => {
                    line(Some(success), format!("+ {key} = {value}"))
                }
                TagChange::Removed { key, value } => {
                    line(Some(danger), format!("\u{2212} {key} = {value}"))
                }
                TagChange::Changed { key, old, new } => {
                    line(None, format!("{key}: {old} \u{2192} {new}"))
                }
            }
        }
        if change.unchanged_tags > 0 {
            line(
                Some(muted),
                tr_count!("object_history.unchanged_tags", change.unchanged_tags).to_string(),
            );
        }
        if let Some(m) = change.moved_m {
            line(
                None,
                tr!("object_history.moved", distance = format_distance(m)).to_string(),
            );
        }
        if change.nodes_added > 0 {
            line(
                Some(success),
                tr_count!("object_history.nodes_added", change.nodes_added).to_string(),
            );
        }
        if change.nodes_removed > 0 {
            line(
                Some(danger),
                tr_count!("object_history.nodes_removed", change.nodes_removed).to_string(),
            );
        }
        if change.nodes_reordered {
            line(None, tr!("object_history.nodes_reordered").to_string());
        }
        if let Some((before, after)) = change.member_counts {
            line(
                None,
                tr!(
                    "object_history.members_count",
                    before = before,
                    after = after
                )
                .to_string(),
            );
        } else if change.members_changed {
            line(None, tr!("object_history.members_changed").to_string());
        }
        if change.is_empty() && change.unchanged_tags == 0 {
            line(Some(muted), tr!("object_history.no_changes").to_string());
        }
        lines
    }

    fn version_row(
        &self,
        index: usize,
        v: &HistoryVersion,
        change: &StateChange,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let size = muted_text_size();

        let mut when = String::new();
        if let Some(ts) = v.timestamp {
            when = format!(
                "{} \u{00b7} {}",
                format_date(ts),
                age_phrase(now.max(0) as u64, ts.max(0) as u64)
            );
        }
        let mut title = h_flex().gap_2().items_baseline().child(
            Label::new(tr!("object_history.version", version = v.version))
                .font_weight(gpui::FontWeight::SEMIBOLD),
        );
        if !v.visible {
            title = title.child(
                div()
                    .text_color(danger)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(tr!("object_history.deleted")),
            );
        }
        title = title.child(div().text_size(size).text_color(muted).child(when));

        let mut who = h_flex()
            .flex_wrap()
            .gap_x_1()
            .text_size(size)
            .text_color(muted);
        who = who.child(match &v.user {
            Some(user) => link(
                ("history-user", index),
                user.clone(),
                web_user_url(&self.web_base, user),
                cx,
            ),
            None => div()
                .child(tr!("object_history.unknown_user"))
                .into_any_element(),
        });
        if let Some(changeset) = v.changeset {
            who = who.child("\u{00b7}").child(link(
                ("history-changeset", index),
                tr!("object_history.changeset", id = changeset),
                web_changeset_url(&self.web_base, changeset),
                cx,
            ));
        }

        let mut col = v_flex().gap_0p5().pb_2().child(title).child(who);
        if v.visible || !change.is_empty() {
            col = col.children(self.change_lines(change, cx));
        }
        let version = v.version;
        let can_revert = v.visible
            && self
                .local
                .as_ref()
                .is_none_or(|local| local.tags != v.state.tags);
        if can_revert {
            col = col.child(
                div().pt_1().flex().flex_row().child(
                    Button::new(("history-revert", index))
                        .ghost()
                        .small()
                        .label(tr!("object_history.revert_tags"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.press_revert(version, cx);
                        })),
                ),
            );
        }
        col.into_any_element()
    }

    fn list(&mut self, history: &ObjectHistory, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut version_rows = 0usize;
        let mut hidden = 0usize;
        for (i, entry) in self.entries.iter().enumerate() {
            if i >= self.shown {
                if matches!(entry, HistoryEntry::Version { .. }) {
                    hidden += 1;
                }
                continue;
            }
            let row = match entry {
                HistoryEntry::Local { change } => {
                    let mut col = v_flex().gap_0p5().pb_2().child(
                        Label::new(tr!("object_history.unsaved"))
                            .font_weight(gpui::FontWeight::SEMIBOLD),
                    );
                    col = col.children(self.change_lines(change, cx));
                    col.into_any_element()
                }
                HistoryEntry::Version { index, change } => {
                    version_rows += 1;
                    match history.versions.get(*index) {
                        Some(v) => self.version_row(*index, v, change, now, cx),
                        None => continue,
                    }
                }
                HistoryEntry::Redacted { from, to } => div()
                    .pb_2()
                    .text_size(muted_text_size())
                    .text_color(muted)
                    .italic()
                    .child(redacted_text(*from, *to))
                    .into_any_element(),
            };
            rows.push(
                div()
                    .border_b_1()
                    .border_color(border)
                    .pt_2()
                    .child(row)
                    .into_any_element(),
            );
        }
        let _ = version_rows;
        if hidden > 0 {
            rows.push(
                div()
                    .py_2()
                    .child(
                        Button::new("history-show-more")
                            .ghost()
                            .small()
                            .label(tr_count!("object_history.show_more", hidden))
                            .on_click(cx.listener(|this, _, _, cx| this.press_show_more(cx))),
                    )
                    .into_any_element(),
            );
        }
        div()
            .id("object-history-list")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .px_3()
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    }
}

impl Render for ObjectHistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_secs();
        let muted = cx.theme().muted_foreground;
        let kind_label = match self.kind {
            FeatureKind::Node => tr!("panel.selection.node"),
            FeatureKind::Way => tr!("panel.selection.way"),
            FeatureKind::Relation => tr!("panel.selection.relation"),
        };

        let header = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(tr!("object_history.title", kind = kind_label, id = self.id)),
            )
            .child(
                Button::new("history-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip(tr!("object_history.close"))
                    .on_click(cx.listener(|this, _, _, cx| this.press_close(cx))),
            );

        let body: AnyElement = match self.state.clone() {
            LoadState::Loading => div()
                .p_3()
                .text_color(muted)
                .child(tr!("object_history.loading"))
                .into_any_element(),
            LoadState::Failed(error) => {
                let mut col = v_flex().p_3().gap_2().child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(error.message(&self.server)),
                );
                if error.is_retryable() {
                    col = col.child(
                        div().child(
                            Button::new("history-retry")
                                .small()
                                .label(tr!("object_history.retry"))
                                .on_click(cx.listener(|this, _, _, cx| this.press_retry(cx))),
                        ),
                    );
                }
                col.into_any_element()
            }
            LoadState::Loaded(history) => self.list(&history, now, cx),
        };

        let footer = h_flex()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(div().text_size(body_text_size()).child(link(
                "history-open-web",
                tr!("object_history.open_web"),
                web_history_url(&self.web_base, self.kind, self.id),
                cx,
            )));

        v_flex()
            .id("object-history-view")
            .track_focus(&self.focus_handle)
            .absolute()
            .top_4()
            .left_4()
            .w(px(440.))
            .max_h(relative(0.9))
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .overflow_hidden()
            .occlude()
            .child(header)
            .child(body)
            .child(footer)
    }
}

#[cfg(test)]
mod last_edit_tests {
    use super::changeset_text;

    #[test]
    fn the_changeset_word_and_number_cannot_be_split() {
        let text = changeset_text("changeset", 134000005);
        assert_eq!(text, "changeset\u{a0}134000005");
        assert!(!text.contains(' '));
    }
}
