//! The app's side of tag autocomplete: the per-layer data indexes (built in
//! the background, kept current incrementally), the dropdown shown under the
//! key and value inputs of the Tags table, and the taginfo lookups behind it.
//! The suggestion logic itself is in `osm_gpui::tag_suggest` and
//! `osm_gpui::taginfo`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{
    anchored, canvas, deferred, div, prelude::*, px, Anchor, AnyElement, Bounds, Context, Pixels,
    ScrollHandle, Window,
};
use gpui_component::ActiveTheme;

use osm_gpui::layers::LayerId;
use osm_gpui::selection::FeatureRef;
use osm_gpui::settings_store;
use osm_gpui::tag_suggest::{suggest_keys, suggest_values, DataIndex, SchemaIndex, Source};
use osm_gpui::taginfo::{self, Lookup, TaginfoItem, TaginfoState, Target};
use osm_gpui::ui::style::muted_text_size;
use osm_gpui::ui::tag_edit::TagEditField;

use crate::MapViewer;

/// Most suggestions in the list (it scrolls beyond what fits).
const LIST_LIMIT: usize = 30;
const ROW_HEIGHT: f32 = 24.0;
const MAX_LIST_HEIGHT: f32 = 220.0;
const LIST_WIDTH: f32 = 260.0;
/// Space the cell's input is inset from its sizing box (see
/// `MapViewer::inline_input_cell`).
const INPUT_INSET: f32 = 8.0;

/// The bundled schema's keys and values, built once on first use.
fn schema() -> &'static SchemaIndex {
    static SCHEMA: OnceLock<SchemaIndex> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        SchemaIndex::build(
            osm_gpui::presets::preset_index(),
            osm_gpui::fields::field_index(),
        )
    })
}

/// Whether taginfo may be asked right now: the setting is on, and this is
/// neither a unit test nor a `--script` run (which never touch the network
/// for suggestions).
fn taginfo_enabled() -> bool {
    !cfg!(test)
        && !crate::script_harness::script_mode()
        && settings_store::snapshot().suggest_from_taginfo
}

/// One layer's key/value counts and the edit revision they describe.
struct LayerTagIndex {
    /// `u64::MAX` until the first build finishes.
    revision: u64,
    index: Arc<DataIndex>,
    /// The revision a background build is working on, if any.
    building: Option<u64>,
}

/// Everything the app keeps for tag autocomplete.
pub(crate) struct TagSuggestState {
    layers: HashMap<LayerId, LayerTagIndex>,
    taginfo: TaginfoState,
    /// Where the key and the value input were last painted (window
    /// coordinates), to place the dropdown above or below.
    anchors: [Rc<Cell<Option<Bounds<Pixels>>>>; 2],
}

impl TagSuggestState {
    pub(crate) fn new() -> Self {
        TagSuggestState {
            layers: HashMap::new(),
            taginfo: TaginfoState::new(),
            anchors: [Rc::new(Cell::new(None)), Rc::new(Cell::new(None))],
        }
    }

    fn anchor(&self, target: TagEditField) -> &Rc<Cell<Option<Bounds<Pixels>>>> {
        &self.anchors[(target == TagEditField::Value) as usize]
    }
}

/// The dropdown of the inline tag edit that is open.
pub(crate) struct Dropdown {
    /// The input the list belongs to; `None` while closed.
    target: Option<TagEditField>,
    items: Vec<osm_gpui::tag_suggest::Suggestion>,
    highlight: Option<usize>,
    scroll: ScrollHandle,
    /// The latest taginfo answer for what is typed.
    taginfo: Vec<TaginfoItem>,
    /// Whether the list opens under the input (else above it).
    below: bool,
}

impl Default for Dropdown {
    fn default() -> Self {
        Dropdown {
            target: None,
            items: Vec::new(),
            highlight: None,
            scroll: ScrollHandle::new(),
            taginfo: Vec::new(),
            below: true,
        }
    }
}

impl Dropdown {
    fn is_open_for(&self, target: TagEditField) -> bool {
        self.target == Some(target) && !self.items.is_empty()
    }

    fn close(&mut self) {
        self.target = None;
        self.items.clear();
        self.highlight = None;
        self.taginfo.clear();
    }
}

fn format_count(n: u64) -> String {
    match n {
        0 => String::new(),
        1..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{}k", n / 1000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

impl MapViewer {
    // --- data indexes -----------------------------------------------------

    /// Start a background rebuild of the index of every layer whose data
    /// changed since its index was built, and forget indexes of closed
    /// layers. Cheap when nothing changed (one revision compare per layer),
    /// so it is safe to call whenever a tag edit opens or its text changes.
    pub(crate) fn sync_tag_indexes(&mut self, cx: &mut Context<Self>) {
        let _ = schema();
        let live: HashSet<LayerId> = self.layer_manager.layers().iter().map(|l| l.id()).collect();
        self.tag_suggest.layers.retain(|id, _| live.contains(id));

        let mut jobs: Vec<(LayerId, u64, Arc<osm_gpui::osm::OsmData>)> = Vec::new();
        for layer in self.layer_manager.layers() {
            let id = layer.id();
            let revision = layer.edit_revision();
            let known = self.tag_suggest.layers.get(&id);
            if known.is_some_and(|e| e.revision == revision || e.building == Some(revision)) {
                continue;
            }
            // Only OSM data layers have a snapshot (a refcount bump).
            let Some((data, _baseline)) = layer.autosave_snapshot() else {
                continue;
            };
            jobs.push((id, revision, data));
        }
        for (id, revision, data) in jobs {
            let entry = self
                .tag_suggest
                .layers
                .entry(id)
                .or_insert_with(|| LayerTagIndex {
                    revision: u64::MAX,
                    index: Arc::new(DataIndex::default()),
                    building: None,
                });
            entry.building = Some(revision);
            cx.spawn(async move |this, cx| {
                let index = cx
                    .background_executor()
                    .spawn(async move { DataIndex::from_osm(&data) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(entry) = this.tag_suggest.layers.get_mut(&id) {
                        entry.index = Arc::new(index);
                        entry.revision = revision;
                        entry.building = None;
                    }
                    // The open dropdown may now know more.
                    if this.rebuild_suggestions(cx) {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// The layers whose index is up to date with their data right now.
    pub(crate) fn current_tag_index_layers(&self) -> Vec<LayerId> {
        self.layer_manager
            .layers()
            .iter()
            .filter(|l| {
                self.tag_suggest
                    .layers
                    .get(&l.id())
                    .is_some_and(|e| e.revision == l.edit_revision())
            })
            .map(|l| l.id())
            .collect()
    }

    /// Fold applied tag edits into the indexes of `current` (the layers that
    /// were up to date before the edit), so the new tags are suggested at
    /// once and no rebuild is needed. Call after the edit was applied.
    pub(crate) fn note_tag_changes(
        &mut self,
        current: &[LayerId],
        entries: &[(FeatureRef, String, Option<String>, Option<String>)],
    ) {
        for id in current {
            let Some(revision) = self
                .layer_manager
                .find_layer(*id)
                .map(|l| l.edit_revision())
            else {
                continue;
            };
            let Some(entry) = self.tag_suggest.layers.get_mut(id) else {
                continue;
            };
            let index = Arc::make_mut(&mut entry.index);
            for (feature, key, before, after) in entries {
                if feature.layer_id == *id {
                    index.apply_change(key, before.as_deref(), after.as_deref());
                }
            }
            entry.revision = revision;
        }
    }

    /// The indexes of the visible layers, for a lookup.
    fn visible_data_indexes(&self) -> Vec<Arc<DataIndex>> {
        self.layer_manager
            .layers()
            .iter()
            .filter(|l| l.is_visible())
            .filter_map(|l| self.tag_suggest.layers.get(&l.id()))
            .map(|e| e.index.clone())
            .collect()
    }

    // --- the list ---------------------------------------------------------

    /// Recompute the open dropdown's entries from the typed text, the
    /// selection, the indexes and the latest taginfo answer. Returns whether
    /// anything is shown (or was just removed).
    fn rebuild_suggestions(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(edit) = self.tag_inline_edit.as_ref() else {
            return false;
        };
        let Some(target) = edit.suggest.target else {
            return false;
        };
        let typed = match target {
            TagEditField::Value => edit.value.read(cx).value().to_string(),
            _ => edit.key.read(cx).value().to_string(),
        };
        let key_text = edit.key.read(cx).value().trim().to_string();
        let snapshots = self.feature_tag_snapshots(&edit.ctx.features);
        let indexes = self.visible_data_indexes();
        let refs: Vec<&DataIndex> = indexes.iter().map(|a| &**a).collect();

        let items = match target {
            TagEditField::Value => {
                // For several features: the values they have right now.
                let mut selection_values: Vec<String> = Vec::new();
                if snapshots.len() > 1 {
                    for (_, tags) in &snapshots {
                        if let Some((_, v)) = tags.iter().find(|(k, _)| *k == key_text) {
                            if !selection_values.contains(v) {
                                selection_values.push(v.clone());
                            }
                        }
                    }
                    selection_values.sort();
                }
                if key_text.is_empty() {
                    Vec::new()
                } else {
                    suggest_values(
                        &key_text,
                        &typed,
                        &selection_values,
                        &refs,
                        schema(),
                        &edit.suggest.taginfo,
                        LIST_LIMIT,
                    )
                }
            }
            _ => {
                if typed.trim().is_empty() {
                    Vec::new()
                } else {
                    // Keys every selected feature already has.
                    let mut exclude: HashSet<String> = HashSet::new();
                    if let Some((_, first)) = snapshots.first() {
                        exclude = first.iter().map(|(k, _)| k.clone()).collect();
                        for (_, tags) in &snapshots[1..] {
                            exclude.retain(|k| tags.iter().any(|(tk, _)| tk == k));
                        }
                    }
                    suggest_keys(
                        &typed,
                        &refs,
                        schema(),
                        &edit.suggest.taginfo,
                        &exclude,
                        LIST_LIMIT,
                    )
                }
            }
        };

        let edit = self.tag_inline_edit.as_mut().expect("checked above");
        let changed = !(edit.suggest.items.is_empty() && items.is_empty());
        edit.suggest.items = items;
        if edit
            .suggest
            .highlight
            .is_some_and(|h| h >= edit.suggest.items.len())
        {
            edit.suggest.highlight = None;
        }
        changed
    }

    /// Where the open list goes: under the input if it fits there or if
    /// there is more room below than above, else above.
    fn place_dropdown(&mut self, window: &Window) {
        let Some(edit) = self.tag_inline_edit.as_mut() else {
            return;
        };
        let Some(target) = edit.suggest.target else {
            return;
        };
        let Some(bounds) = self.tag_suggest.anchor(target).get() else {
            edit.suggest.below = true;
            return;
        };
        let list_h = (edit.suggest.items.len() as f32 * ROW_HEIGHT + 8.0).min(MAX_LIST_HEIGHT);
        let margin = 8.0;
        let below_room = f32::from(window.viewport_size().height - bounds.bottom()) - margin;
        let above_room = f32::from(bounds.top()) - margin;
        edit.suggest.below = list_h <= below_room || below_room >= above_room;
    }

    /// The text of the key or value input changed (or the list was asked
    /// for): open the dropdown for `target`, show what is known locally and
    /// queue a taginfo lookup. Highlight is cleared, so Enter still commits
    /// exactly what was typed.
    pub(crate) fn refresh_tag_suggestions(
        &mut self,
        target: TagEditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tag_inline_edit.is_none() {
            return;
        }
        self.sync_tag_indexes(cx);

        let (typed, key_text) = {
            let edit = self.tag_inline_edit.as_ref().expect("checked above");
            let typed = match target {
                TagEditField::Value => edit.value.read(cx).value().to_string(),
                _ => edit.key.read(cx).value().to_string(),
            };
            (typed, edit.key.read(cx).value().trim().to_string())
        };

        let which = if target == TagEditField::Value {
            Target::Value
        } else {
            Target::Key
        };
        let query = taginfo::plan(taginfo_enabled(), which, &key_text, &typed);
        let lookup = self.tag_suggest.taginfo.request(Instant::now(), query);

        if let Some(edit) = self.tag_inline_edit.as_mut() {
            if edit.suggest.target != Some(target) {
                edit.suggest.close();
            }
            edit.suggest.target = Some(target);
            edit.suggest.highlight = None;
            match lookup {
                Lookup::Cached(items) => edit.suggest.taginfo = items,
                // Keep the previous answer meanwhile; entries that no longer
                // match the text are filtered out when the list is built.
                Lookup::Pending => {}
                Lookup::Idle => edit.suggest.taginfo.clear(),
            }
        }
        self.rebuild_suggestions(cx);
        self.place_dropdown(window);
        if lookup_state(&self.tag_suggest.taginfo) {
            self.schedule_taginfo_poll(cx);
        }
        cx.notify();
    }

    /// Hide the dropdown (and forget any lookup that was wanted for it).
    pub(crate) fn close_tag_suggestions(&mut self) {
        if let Some(edit) = self.tag_inline_edit.as_mut() {
            edit.suggest.close();
        }
        self.tag_suggest.taginfo.request(Instant::now(), None);
    }

    // --- taginfo ------------------------------------------------------------

    fn schedule_taginfo_poll(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = self.tag_suggest.taginfo.wake_in(Instant::now()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| this.taginfo_poll(cx));
        })
        .detach();
    }

    /// Send the wanted taginfo request if its debounce has run out.
    fn taginfo_poll(&mut self, cx: &mut Context<Self>) {
        if !taginfo_enabled() || self.tag_inline_edit.is_none() {
            self.tag_suggest.taginfo.request(Instant::now(), None);
            return;
        }
        let Some(job) = self.tag_suggest.taginfo.poll(Instant::now()) else {
            return;
        };
        let query = job.query.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { taginfo::fetch(&osm_gpui::http::UreqClient::new(), &query) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let items = this.tag_suggest.taginfo.finish(Instant::now(), job, result);
                if let (Some(items), Some(edit)) = (items, this.tag_inline_edit.as_mut()) {
                    // Only if the dropdown is still the one that asked.
                    if edit.suggest.target.is_some() {
                        edit.suggest.taginfo = items;
                        this.rebuild_suggestions(cx);
                        cx.notify();
                    }
                }
                this.schedule_taginfo_poll(cx);
            });
        })
        .detach();
    }

    // --- accepting ----------------------------------------------------------

    /// Put suggestion `index` into the input it belongs to and close the
    /// list. Does not commit or move focus.
    fn fill_suggestion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.tag_inline_edit.as_mut() else {
            return;
        };
        let (Some(target), Some(item)) = (edit.suggest.target, edit.suggest.items.get(index))
        else {
            return;
        };
        let text = item.text.clone();
        let input = if target == TagEditField::Value {
            edit.value.clone()
        } else {
            edit.key.clone()
        };
        edit.suggest.close();
        input.update(cx, |state, cx| state.set_value(text, window, cx));
        self.tag_suggest.taginfo.request(Instant::now(), None);
        cx.notify();
    }

    /// A suggestion was clicked: fill it in, then do what Enter would.
    fn click_suggestion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.tag_inline_edit.as_ref() else {
            return;
        };
        let target = edit.suggest.target;
        let is_add = edit.ctx.is_add;
        let value = edit.value.clone();
        self.fill_suggestion(index, window, cx);
        if is_add && target == Some(TagEditField::Key) {
            value.update(cx, |state, cx| state.focus(window, cx));
        } else {
            self.commit_tag_inline_edit(cx);
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    // --- keys -----------------------------------------------------------------

    /// Down: open the list if it is closed, else move the highlight down.
    fn on_suggest_down(
        &mut self,
        target: TagEditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        let open = self
            .tag_inline_edit
            .as_ref()
            .is_some_and(|e| e.suggest.is_open_for(target));
        if !open {
            self.refresh_tag_suggestions(target, window, cx);
            return;
        }
        if let Some(edit) = self.tag_inline_edit.as_mut() {
            let last = edit.suggest.items.len() - 1;
            let next = edit.suggest.highlight.map_or(0, |h| (h + 1).min(last));
            edit.suggest.highlight = Some(next);
            edit.suggest.scroll.scroll_to_item(next);
        }
        cx.notify();
    }

    /// Up: move the highlight up; above the first entry it goes back to
    /// "nothing highlighted". With the list closed, Up is left alone.
    fn on_suggest_up(&mut self, target: TagEditField, cx: &mut Context<Self>) {
        let Some(edit) = self.tag_inline_edit.as_mut() else {
            return;
        };
        if !edit.suggest.is_open_for(target) {
            return;
        }
        cx.stop_propagation();
        edit.suggest.highlight = match edit.suggest.highlight {
            Some(0) | None => None,
            Some(h) => Some(h - 1),
        };
        if let Some(h) = edit.suggest.highlight {
            edit.suggest.scroll.scroll_to_item(h);
        }
        cx.notify();
    }

    /// Enter or Tab: a highlighted suggestion is put into the input first;
    /// the key then goes on to do what it always did (Enter commits or moves
    /// to the value, Tab moves on). With nothing highlighted the typed text
    /// is left exactly as it is.
    fn on_suggest_accept(
        &mut self,
        target: TagEditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.tag_inline_edit.as_ref() else {
            return;
        };
        if !edit.suggest.is_open_for(target) {
            return;
        }
        match edit.suggest.highlight {
            Some(h) => self.fill_suggestion(h, window, cx),
            None => self.close_tag_suggestions(),
        }
    }

    /// Escape: close an open list and swallow the key; with no list open
    /// Escape goes on to abandon the edit as before.
    fn on_suggest_escape(&mut self, target: TagEditField, cx: &mut Context<Self>) {
        let Some(edit) = self.tag_inline_edit.as_ref() else {
            return;
        };
        if !edit.suggest.is_open_for(target) {
            return;
        }
        cx.stop_propagation();
        self.close_tag_suggestions();
        cx.notify();
    }

    // --- rendering --------------------------------------------------------------

    /// Key handling and placement tracking for one cell of the Tags table
    /// that is an input (`target` says which). Wrap the cell with this.
    pub(crate) fn with_tag_suggest(
        &self,
        cell: gpui::Div,
        target: TagEditField,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::input::{Enter, Escape, IndentInline, MoveDown, MoveUp};

        // The input handles these keys itself, so they are caught on the
        // way down, before it sees them.
        let anchor = self.tag_suggest.anchor(target).clone();
        cell.capture_action(cx.listener(move |this, _: &MoveDown, window, cx| {
            this.on_suggest_down(target, window, cx);
        }))
        .capture_action(cx.listener(move |this, _: &MoveUp, _window, cx| {
            this.on_suggest_up(target, cx);
        }))
        .capture_action(cx.listener(move |this, _: &Enter, window, cx| {
            this.on_suggest_accept(target, window, cx);
        }))
        .capture_action(cx.listener(move |this, _: &IndentInline, window, cx| {
            this.on_suggest_accept(target, window, cx);
        }))
        .capture_action(cx.listener(move |this, _: &Escape, _window, cx| {
            this.on_suggest_escape(target, cx);
        }))
        .child(
            div().absolute().inset_0().child(
                canvas(
                    move |bounds, _window, _cx| anchor.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .size_full(),
            ),
        )
        .children(self.suggest_dropdown(target, cx))
    }

    /// The dropdown for `target`, if it is open: an overlay that is not
    /// clipped by the panel's scroll areas, anchored under the input (above
    /// it when there is no room), with its own scroll.
    fn suggest_dropdown(&self, target: TagEditField, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.tag_inline_edit.as_ref()?;
        let dd = &edit.suggest;
        if !dd.is_open_for(target) {
            return None;
        }
        let theme = cx.theme();
        let (fg, muted, accent, border, bg) = (
            theme.foreground,
            theme.muted_foreground,
            theme.accent,
            theme.border,
            theme.popover,
        );
        let translations = osm_gpui::preset_translations::active();

        let rows = dd.items.iter().enumerate().map(|(i, item)| {
            let label = item.label.as_ref().map(|english| {
                item.field_id
                    .as_deref()
                    .and_then(|f| translations.and_then(|t| t.option_label(f, &item.text)))
                    .unwrap_or(english)
                    .to_string()
            });
            let count = match item.source {
                Source::Data | Source::Taginfo => format_count(item.count),
                _ => String::new(),
            };
            div()
                .id(("tag-suggest-row", i))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(ROW_HEIGHT))
                .flex_shrink_0()
                .cursor_pointer()
                .text_color(fg)
                .when(dd.highlight == Some(i), |row| row.bg(accent))
                .hover(|row| row.bg(accent))
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(LIST_WIDTH * 0.6))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(item.text.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .children(label),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .child(count),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _ev, window, cx| {
                        this.click_suggestion(i, window, cx);
                        cx.stop_propagation();
                    }),
                )
        });

        let list = div()
            .id("tag-suggest-list")
            // Clicks and scrolls land here, not on the map or panel behind.
            .occlude()
            .w(px(LIST_WIDTH))
            .max_h(px(MAX_LIST_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&dd.scroll)
            .py_1()
            .bg(bg)
            .border_1()
            .border_color(border)
            .rounded_md()
            .shadow_md()
            .flex()
            .flex_col()
            .children(rows);

        let (corner, below) = if dd.below {
            (Anchor::TopLeft, true)
        } else {
            (Anchor::BottomLeft, false)
        };
        Some(
            div()
                .absolute()
                .left(px(-INPUT_INSET))
                .when(below, |d| d.top_full())
                .when(!below, |d| d.bottom_full())
                .child(
                    deferred(
                        anchored()
                            .anchor(corner)
                            .snap_to_window_with_margin(px(8.))
                            .child(list),
                    )
                    .with_priority(2),
                )
                .into_any_element(),
        )
    }
}

/// Whether a taginfo request is waiting for its debounce.
fn lookup_state(state: &TaginfoState) -> bool {
    state.wake_in(Instant::now()).is_some()
}
