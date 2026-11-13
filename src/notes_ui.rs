//! OSM notes in the window: View > Show Notes, fetching, the pins, the Note
//! section of the side panel, writes, and the New Note flow.
//!
//! The model, the API calls and the layer live in the library
//! (`osm_gpui::notes`, `osm_gpui::layers::notes_layer`); this is the part that
//! needs the window. Notes are fetched in the background (anonymously unless a
//! login is already in memory, so it never raises the keychain prompt), never
//! block editing, and report failures in the status bar. A write (comment,
//! close, reopen, create) needs a signed-in user, runs in the background, is
//! never retried, and shows its failure inline in the note panel (or the New
//! Note dialog) with the typed text kept.

use std::time::{Duration, Instant};

use gpui::{actions, div, prelude::*, px, AnyElement, Context, Entity, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _,
};
use osm_gpui::auth;
use osm_gpui::coordinates::GeoBounds;
use osm_gpui::layers::notes_layer::{self, NotesLayer};
use osm_gpui::layers::LayerId;
use osm_gpui::notes::{
    self, FetchPlan, Fetched, Note, NoteStatus, NoteStore, NoteWrite, NotesError,
};
use osm_gpui::script::{NoteCheck, NoteOp};
use osm_gpui::settings_store;
use osm_gpui::ui::note_dialog::{DialogEvent, NewNoteDialog};
use osm_gpui::ui::sign_in_dialog::SignInPurpose;
use osm_gpui::ui::style::muted_text_size;
use osm_gpui::{tr, tr_count};

use crate::{script_harness, EditMode, MapViewer};

actions!(osm_gpui, [ToggleNotes, RefreshNotes, NewNote]);

/// Colour of the status dot in the note panel; the same hues as the pins.
const OPEN_DOT: u32 = 0xd9480f;
const CLOSED_DOT: u32 = 0x7c8794;
/// How long a viewport-only fetch waits before following a pan.
const VIEWPORT_REFETCH_DELAY: Duration = Duration::from_millis(1500);

/// The note open in the side panel.
pub(crate) struct OpenNote {
    note: Note,
    input: Entity<InputState>,
    /// The last write's failure, shown above the text box.
    error: Option<SharedString>,
}

/// Everything about notes that the window holds.
#[derive(Default)]
pub(crate) struct NotesState {
    /// Shared with the Notes layer, which draws from it.
    store: NoteStore,
    /// The Notes layer, while View > Show Notes is on.
    layer: Option<LayerId>,
    fetching: bool,
    /// Bumped to discard the answer of a fetch that is no longer wanted.
    epoch: u64,
    /// The notes came from a local file (a script), so nothing is fetched.
    local_only: bool,
    /// For a viewport-only fetch: the area it covered and when it started.
    viewport_fetch: Option<(GeoBounds, Instant)>,
    open: Option<OpenNote>,
    /// A pin was clicked; the panel opens on the next frame (it needs the
    /// window).
    pending_open: Option<i64>,
    writing: bool,
    /// New Note was chosen: the next click on the map places the note.
    armed: bool,
    /// Where the click landed; the dialog opens on the next frame.
    pending_new: Option<(f64, f64)>,
    new_dialog: Option<Entity<NewNoteDialog>>,
    /// Empty the text box on the next frame (after a successful write).
    clear_input: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PanelAction {
    Comment,
    Close,
    Reopen,
}

/// `b` grown by `factor` of its size on every side.
fn expand(b: &GeoBounds, factor: f64) -> GeoBounds {
    let (dw, dh) = (b.width() * factor, b.height() * factor);
    GeoBounds::new(
        (b.min_lat - dh).max(-85.0),
        (b.max_lat + dh).min(85.0),
        (b.min_lon - dw).max(-180.0),
        (b.max_lon + dw).min(180.0),
    )
}

fn contains(outer: &GeoBounds, inner: &GeoBounds) -> bool {
    inner.min_lat >= outer.min_lat
        && inner.max_lat <= outer.max_lat
        && inner.min_lon >= outer.min_lon
        && inner.max_lon <= outer.max_lon
}

impl MapViewer {
    /// Whether View > Show Notes is on (drives its menu checkmark).
    pub(crate) fn shows_notes(&self) -> bool {
        self.notes.layer.is_some()
    }

    fn refresh_menus_for_notes(&mut self, cx: &mut Context<Self>) {
        osm_gpui::notes::set_menu_shows_notes(self.shows_notes());
        let (lat, lon, state) = self.imagery_menu_context();
        crate::menu::rebuild_menus(
            &mut *cx,
            lat,
            lon,
            state,
            self.show_downloaded_area,
            self.has_uploadable_changes(),
        );
    }

    /// Turn the Notes layer on or off. Turning it on fetches the notes.
    pub(crate) fn set_show_notes(&mut self, on: bool, cx: &mut Context<Self>) {
        if on == self.shows_notes() {
            return;
        }
        if on {
            let id = self.layer_manager.alloc_id();
            let layer = NotesLayer::new(
                id,
                tr!("notes.layer_name").to_string(),
                self.notes.store.clone(),
            );
            self.layer_manager.add_layer(Box::new(layer));
            self.notes.layer = Some(id);
            self.refresh_menus_for_notes(cx);
            self.refresh_notes(cx);
        } else {
            if let Some(id) = self.notes.layer {
                if let Some(i) = self
                    .layer_manager
                    .layers()
                    .iter()
                    .position(|l| l.id() == id)
                {
                    self.layer_manager.remove_at(i);
                }
            }
            self.notes_off();
            self.refresh_menus_for_notes(cx);
        }
        cx.notify();
    }

    /// Forget everything about shown notes (the layer is already gone).
    fn notes_off(&mut self) {
        self.notes.layer = None;
        self.notes.store.clear();
        self.notes.open = None;
        self.notes.fetching = false;
        self.notes.epoch += 1;
        self.notes.local_only = false;
        self.notes.viewport_fetch = None;
        self.notes.pending_open = None;
        self.notes.armed = false;
        self.notes.pending_new = None;
        self.notes.new_dialog = None;
    }

    pub(crate) fn on_toggle_notes(
        &mut self,
        _: &ToggleNotes,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_show_notes(!self.shows_notes(), cx);
    }

    pub(crate) fn on_refresh_notes(
        &mut self,
        _: &RefreshNotes,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_notes(cx);
    }

    /// Fetch the notes for the downloaded area (cut to the server's note
    /// area limit), or for the visible map when there is no downloaded area
    /// or it is too large to cover. Runs in the background.
    pub(crate) fn refresh_notes(&mut self, cx: &mut Context<Self>) {
        if !self.shows_notes() {
            self.set_status(tr!("notes.state.off"));
            cx.notify();
            return;
        }
        if self.notes.local_only || self.notes.fetching {
            return;
        }
        let base_url = settings_store::api_base_url();
        osm_gpui::capabilities::ensure_fetched(&base_url);
        let max_area = osm_gpui::capabilities::max_note_area(&base_url);

        let downloaded: Vec<GeoBounds> = self
            .layer_manager
            .layers()
            .iter()
            .filter(|l| l.is_visible())
            .flat_map(|l| l.download_rects().iter().map(notes::merc_to_geo))
            .collect();
        let view = self.viewport.visible_bounds();
        // A viewport-only fetch covers a margin around the screen so a small
        // pan does not need another one.
        let wide = expand(&view, 0.5);
        let (cells, covered) =
            match notes::plan_fetch(&downloaded, &wide, max_area, notes::MAX_FETCH_REQUESTS) {
                FetchPlan::Downloaded(cells) => (cells, None),
                FetchPlan::Viewport(cells) => (cells, Some(wide)),
                FetchPlan::TooLarge => {
                    match notes::plan_fetch(&downloaded, &view, max_area, notes::MAX_FETCH_REQUESTS)
                    {
                        FetchPlan::Downloaded(cells) => (cells, None),
                        FetchPlan::Viewport(cells) => (cells, Some(view)),
                        FetchPlan::TooLarge => {
                            self.set_status(tr!("notes.state.too_large"));
                            cx.notify();
                            return;
                        }
                    }
                }
            };

        self.notes.fetching = true;
        self.notes.epoch += 1;
        let epoch = self.notes.epoch;
        self.notes.viewport_fetch = covered.map(|b| (b, Instant::now()));
        self.set_status(tr!("notes.state.loading"));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Reading notes works anonymously, so it never waits on the
                    // system keychain: a token is used only if it is already in
                    // memory, as with the map download.
                    let token = auth::fresh_token_if_loaded(&auth::oauth_base_for(&base_url))
                        .map(|t| t.access_token);
                    notes::fetch_notes(&base_url, &cells, token.as_deref())
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_note_fetch(epoch, result, cx));
        })
        .detach();
    }

    fn finish_note_fetch(
        &mut self,
        epoch: u64,
        result: Result<Fetched, NotesError>,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.notes.epoch || !self.shows_notes() {
            return;
        }
        self.notes.fetching = false;
        match result {
            Ok(found) => {
                let count = found.notes.len();
                self.notes.store.replace(found.notes);
                if !self.notes.writing {
                    if let Some(open) = &mut self.notes.open {
                        if let Some(fresh) = self.notes.store.get(open.note.id) {
                            open.note = fresh;
                        }
                    }
                }
                if found.truncated {
                    self.set_status(tr!("notes.state.truncated", count = notes::NOTE_LIMIT));
                } else {
                    self.set_status(tr_count!("notes.state.loaded", count));
                }
            }
            Err(e) => {
                self.set_status(tr!("notes.state.fetch_failed", detail = e.user_message()));
            }
        }
        cx.notify();
    }

    /// Per-frame upkeep, called from `render`: follows the layer being
    /// deleted from the Layers list, keeps the pins above the data, closes
    /// the note panel when a feature is selected, and does what the last
    /// click asked for.
    pub(crate) fn notes_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.notes.layer {
            match self
                .layer_manager
                .layers()
                .iter()
                .position(|l| l.id() == id)
            {
                None => {
                    // Deleted from the Layers list.
                    self.notes_off();
                    self.refresh_menus_for_notes(cx);
                }
                Some(i) => {
                    let last = self.layer_manager.layers().len() - 1;
                    if i != last {
                        self.layer_manager.move_layer(i, last);
                    }
                }
            }
        }
        if !self.selected.is_empty() && self.notes.open.is_some() {
            self.close_note_panel();
        }
        if let Some(id) = self.notes.pending_open.take() {
            self.open_note(id, window, cx);
        }
        if let Some((lat, lon)) = self.notes.pending_new.take() {
            self.open_new_note_dialog(lat, lon, window, cx);
        }
        if std::mem::take(&mut self.notes.clear_input) {
            if let Some(open) = &self.notes.open {
                open.input
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
        // A viewport-only fetch follows the map once it leaves the area it
        // covered.
        if self.shows_notes() && !self.notes.local_only && !self.notes.fetching {
            if let Some((covered, at)) = self.notes.viewport_fetch {
                if !self.viewport.is_dragging
                    && at.elapsed() >= VIEWPORT_REFETCH_DELAY
                    && !contains(&covered, &self.viewport.visible_bounds())
                {
                    self.refresh_notes(cx);
                }
            }
        }
    }

    /// A click on the map at `pos` (map-local pixels): places a new note when
    /// New Note is armed, otherwise opens the note whose pin was clicked.
    /// Returns whether the click was used.
    pub(crate) fn notes_click(&mut self, pos: gpui::Point<gpui::Pixels>) -> bool {
        if self.notes.armed {
            self.notes.armed = false;
            self.notes.pending_new = Some(self.viewport.screen_to_geo(pos));
            return true;
        }
        if self.mode != EditMode::Select {
            return false;
        }
        let Some(layer_id) = self.notes.layer else {
            return false;
        };
        if !self
            .layer_manager
            .find_layer(layer_id)
            .is_some_and(|l| l.is_visible())
        {
            return false;
        }
        match notes_layer::hit_test(&self.notes.store, &self.viewport, pos) {
            Some(id) => {
                self.notes.pending_open = Some(id);
                true
            }
            None => false,
        }
    }

    /// Escape while New Note is armed cancels it. Returns whether it did.
    pub(crate) fn cancel_armed_note(&mut self) -> bool {
        if !std::mem::take(&mut self.notes.armed) {
            return false;
        }
        self.set_status(tr!("notes.new.cancelled"));
        true
    }

    pub(crate) fn notes_armed(&self) -> bool {
        self.notes.armed
    }

    /// Open note `id` (which must be loaded) in the side panel, replacing the
    /// Selection there.
    pub(crate) fn open_note(
        &mut self,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(note) = self.notes.store.get(id) else {
            return false;
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 8)
                .placeholder(tr!("notes.panel.placeholder"))
        });
        self.set_selection(Vec::new());
        self.close_photo_panel();
        self.notes.store.set_selected(Some(id));
        self.notes.open = Some(OpenNote {
            note,
            input,
            error: None,
        });
        cx.notify();
        true
    }

    pub(crate) fn close_note_panel(&mut self) {
        self.notes.open = None;
        self.notes.store.set_selected(None);
    }

    // -- New Note ---------------------------------------------------------

    fn signed_in_for_notes(&self) -> bool {
        auth::stored_display_name(&auth::oauth_base_for(&settings_store::api_base_url())).is_some()
    }

    /// Tools > New Note: arm a one-shot click that places the note.
    pub(crate) fn on_new_note(&mut self, _: &NewNote, window: &mut Window, cx: &mut Context<Self>) {
        if !self.signed_in_for_notes() {
            // Notes are attributed to a user: offer the sign-in flow.
            self.show_sign_in_dialog(SignInPurpose::Notes, window, cx);
            return;
        }
        self.set_show_notes(true, cx);
        self.mode = EditMode::Select;
        self.add_progress = None;
        self.notes.armed = true;
        self.set_status(tr!("notes.new.armed"));
        cx.notify();
    }

    pub(crate) fn open_new_note_dialog(
        &mut self,
        lat: f64,
        lon: f64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.notes.new_dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| NewNoteDialog::new(lat, lon, window, cx));
        cx.subscribe(
            &dialog,
            move |this: &mut Self, dialog, event: &DialogEvent, cx| match event {
                DialogEvent::Cancelled => {
                    this.notes.new_dialog = None;
                    cx.notify();
                }
                DialogEvent::Submit(text) => {
                    dialog.update(cx, |d, cx| d.begin(cx));
                    if let Err(why) =
                        this.start_note_write(NoteWrite::Create { lat, lon }, text.clone(), cx)
                    {
                        dialog.update(cx, |d, cx| d.fail(why.into(), cx));
                    }
                }
            },
        )
        .detach();
        self.notes.new_dialog = Some(dialog);
        cx.notify();
    }

    pub(crate) fn new_note_dialog_element(&self) -> Option<Entity<NewNoteDialog>> {
        self.notes.new_dialog.clone()
    }

    // -- Writes -------------------------------------------------------------

    /// Start a write in the background. `Err` says why it could not start
    /// (nothing was sent). Never retried: a failed write is shown, and the
    /// user decides.
    fn start_note_write(
        &mut self,
        write: NoteWrite,
        text: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let base_url = settings_store::api_base_url();
        // A scripted run only ever writes to the development server.
        if let Some(refusal) = script_harness::scripted_note_write_refusal(&base_url) {
            return Err(refusal);
        }
        if osm_gpui::transfer::blocks_edits(self.transfer.as_ref()) {
            return Err(tr!("notes.panel.upload_running").to_string());
        }
        if self.notes.writing {
            return Err(tr!("notes.panel.working").to_string());
        }
        if !self.signed_in_for_notes() {
            return Err(SignInPurpose::Notes.title());
        }
        self.notes.writing = true;
        if let Some(open) = &mut self.notes.open {
            open.error = None;
        }
        cx.notify();

        let oauth_base = auth::oauth_base_for(&base_url);
        cx.spawn(async move |this, cx| {
            let result: Result<Note, String> = cx
                .background_executor()
                .spawn(async move {
                    // May wait on the system keychain's access prompt the first
                    // time; this is the background thread, and "Working…" shows.
                    let token = auth::ensure_fresh_token(&oauth_base).map_err(|e| e.to_string())?;
                    notes::write_note(&base_url, &token.access_token, write, &text)
                        .map_err(|e| e.user_message())
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_note_write(write, result, cx));
        })
        .detach();
        Ok(())
    }

    fn finish_note_write(
        &mut self,
        write: NoteWrite,
        result: Result<Note, String>,
        cx: &mut Context<Self>,
    ) {
        self.notes.writing = false;
        match result {
            Ok(note) => {
                let id = note.id;
                if self.shows_notes() {
                    self.notes.store.upsert(note.clone());
                }
                match write {
                    NoteWrite::Create { .. } => {
                        self.notes.new_dialog = None;
                        self.notes.pending_open = Some(id);
                        self.set_status(tr!("notes.state.created", id = id));
                    }
                    _ => {
                        if let Some(open) = &mut self.notes.open {
                            if open.note.id == id {
                                open.note = note;
                                open.error = None;
                                self.notes.clear_input = true;
                            }
                        }
                        self.set_status(tr!("notes.state.updated", id = id));
                    }
                }
            }
            Err(message) => match write {
                NoteWrite::Create { .. } => {
                    if let Some(dialog) = &self.notes.new_dialog {
                        let message = message.clone();
                        dialog.update(cx, |d, cx| d.fail(message.into(), cx));
                    }
                    self.set_status(message);
                }
                _ => {
                    if let Some(open) = &mut self.notes.open {
                        open.error = Some(message.into());
                    }
                }
            },
        }
        cx.notify();
    }

    /// One of the panel's buttons: send the text box as a comment, a closing
    /// comment or a reopening comment. Failures to start show in the panel
    /// too. Signed out, nothing is sent: the sign-in prompt opens over the
    /// panel (the typed text stays in the box) and `Err` says so for scripts.
    fn note_panel_action(
        &mut self,
        action: PanelAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(open) = &self.notes.open else {
            return Err("no note is open".into());
        };
        let id = open.note.id;
        let text = open.input.read(cx).value().to_string();
        if !self.signed_in_for_notes() {
            self.show_sign_in_dialog(SignInPurpose::Notes, window, cx);
            return Err("not signed in: the sign-in prompt was opened".into());
        }
        let write = match action {
            PanelAction::Comment => NoteWrite::Comment(id),
            PanelAction::Close => NoteWrite::Close(id),
            PanelAction::Reopen => NoteWrite::Reopen(id),
        };
        let outcome = if action == PanelAction::Comment && text.trim().is_empty() {
            Err(tr!("notes.panel.comment_empty").to_string())
        } else {
            self.start_note_write(write, text, cx)
        };
        if let Err(why) = &outcome {
            if let Some(open) = &mut self.notes.open {
                open.error = Some(why.clone().into());
            }
            cx.notify();
        }
        outcome
    }

    // -- Side panel ---------------------------------------------------------

    /// The Note section while a note is open: its title, its status row
    /// (pinned) and the rest (thread, comment box, buttons; scrolled).
    pub(crate) fn render_note_section(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(SharedString, AnyElement, AnyElement)> {
        let open = self.notes.open.as_ref()?;
        let note = &open.note;
        let id = note.id;
        let busy = self.notes.writing;
        let theme = cx.theme();
        let (muted, border, foreground, danger) = (
            theme.muted_foreground,
            theme.border,
            theme.foreground,
            theme.danger,
        );
        let now = notes::now_unix();

        let dot = match note.status {
            NoteStatus::Open => OPEN_DOT,
            _ => CLOSED_DOT,
        };
        let when = match (note.status, note.closed, note.created) {
            (NoteStatus::Closed, Some(t), _) => {
                tr!("notes.panel.closed_on", when = notes::when_phrase(now, t)).to_string()
            }
            (_, _, Some(t)) => {
                tr!("notes.panel.created", when = notes::when_phrase(now, t)).to_string()
            }
            _ => String::new(),
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .w(px(10.0))
                    .h(px(10.0))
                    .rounded_full()
                    .bg(gpui::rgb(dot)),
            )
            .child(
                Label::new(note.status.label())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(foreground),
            )
            .child(
                div().flex_1().child(
                    Label::new(when)
                        .text_size(muted_text_size())
                        .text_color(muted),
                ),
            )
            .child(
                Button::new("note-close-panel")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip(tr!("notes.panel.close"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.close_note_panel();
                        cx.notify();
                    })),
            );

        let thread = v_flex().children(note.comments.iter().enumerate().map(|(i, c)| {
            let author = c
                .user
                .clone()
                .unwrap_or_else(|| tr!("notes.anonymous").to_string());
            let when = c
                .date
                .map(|t| notes::when_phrase(now, t))
                .unwrap_or_default();
            v_flex()
                .px_2()
                .py_1()
                .gap_0p5()
                .border_t_1()
                .border_color(border)
                .child(
                    div()
                        .id(("note-comment-head", i))
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Label::new(author)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(foreground),
                        )
                        .child(Label::new(c.action.label()).text_color(muted))
                        .child(
                            Label::new(when)
                                .text_size(muted_text_size())
                                .text_color(muted),
                        ),
                )
                .when(!c.text.trim().is_empty(), |el| {
                    el.child(
                        div()
                            .text_color(foreground)
                            .child(SharedString::from(c.text.trim().to_string())),
                    )
                })
        }));

        let is_open = note.is_open();
        let controls = v_flex()
            .gap_2()
            .px_2()
            .py_2()
            .border_t_1()
            .border_color(border)
            .children(open.error.clone().map(|e| Label::new(e).text_color(danger)))
            .child(Input::new(&open.input).disabled(busy))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("note-comment")
                            .small()
                            .label(tr!("notes.panel.comment"))
                            .disabled(busy || !is_open)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let _ = this.note_panel_action(PanelAction::Comment, window, cx);
                            })),
                    )
                    .child(if is_open {
                        Button::new("note-close")
                            .small()
                            .primary()
                            .label(tr!("notes.panel.close_note"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let _ = this.note_panel_action(PanelAction::Close, window, cx);
                            }))
                    } else {
                        Button::new("note-reopen")
                            .small()
                            .primary()
                            .label(tr!("notes.panel.reopen_note"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let _ = this.note_panel_action(PanelAction::Reopen, window, cx);
                            }))
                    })
                    .children(busy.then(|| {
                        Label::new(tr!("notes.panel.working"))
                            .text_size(muted_text_size())
                            .text_color(muted)
                    })),
            );

        Some((
            tr!("notes.panel.title", id = id),
            // The status row stays pinned; the side panel scrolls the rest.
            header.into_any_element(),
            v_flex()
                .flex_shrink_0()
                .child(thread)
                .child(controls)
                .into_any_element(),
        ))
    }

    // -- Script ---------------------------------------------------------------

    /// Run a note script op. Writes have already passed the dev-server guard
    /// (and `start_note_write` checks it again).
    pub(crate) fn script_note_op(
        &mut self,
        op: &NoteOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        match op {
            NoteOp::Show(on) => {
                self.set_show_notes(*on, cx);
                Ok(())
            }
            NoteOp::LoadFile(path) => {
                let loaded = notes::load_notes_file(std::path::Path::new(path))?;
                self.notes.local_only = true;
                self.notes.fetching = false;
                self.notes.epoch += 1;
                self.set_show_notes(true, cx);
                self.notes.store.replace(loaded);
                cx.notify();
                Ok(())
            }
            NoteOp::Open(id) => {
                if self.open_note(*id, window, cx) {
                    Ok(())
                } else {
                    Err(format!("note {id} is not loaded"))
                }
            }
            NoteOp::Comment(text) => {
                self.script_panel_write(PanelAction::Comment, Some(text), window, cx)
            }
            NoteOp::Close(text) => {
                self.script_panel_write(PanelAction::Close, text.as_deref(), window, cx)
            }
            NoteOp::Reopen(text) => {
                self.script_panel_write(PanelAction::Reopen, text.as_deref(), window, cx)
            }
            NoteOp::New { lat, lon, text } => {
                self.set_show_notes(true, cx);
                self.start_note_write(
                    NoteWrite::Create {
                        lat: *lat,
                        lon: *lon,
                    },
                    text.clone(),
                    cx,
                )
            }
        }
    }

    fn script_panel_write(
        &mut self,
        action: PanelAction,
        text: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(open) = &self.notes.open else {
            return Err("no note is open (run `open_note ID` first)".into());
        };
        if let Some(text) = text {
            open.input.update(cx, |state, cx| {
                state.set_value(text.to_string(), window, cx)
            });
        }
        self.note_panel_action(action, window, cx)
    }

    /// For tests: the open note's typed text, whether it shows an inline
    /// error, and whether a write is running (`None` with no note open).
    #[cfg(test)]
    pub(crate) fn note_panel_state(&self, cx: &gpui::App) -> Option<(String, bool, bool)> {
        let open = self.notes.open.as_ref()?;
        Some((
            open.input.read(cx).value().to_string(),
            open.error.is_some(),
            self.notes.writing,
        ))
    }

    /// Evaluate a note assertion.
    pub(crate) fn script_note_check(&self, check: &NoteCheck) -> Result<(), String> {
        match check {
            NoteCheck::Count(want) => {
                let got = self.notes.store.len();
                if got == *want {
                    Ok(())
                } else {
                    Err(format!("expected {want} notes loaded, found {got}"))
                }
            }
            NoteCheck::Status(want) => {
                let got = self.notes.open.as_ref().map(|o| o.note.is_open());
                if got == *want {
                    Ok(())
                } else {
                    let name = |s: Option<bool>| match s {
                        Some(true) => "open",
                        Some(false) => "closed",
                        None => "none",
                    };
                    Err(format!(
                        "expected the open note to be {}, it is {}",
                        name(*want),
                        name(got)
                    ))
                }
            }
            NoteCheck::Comments(want) => match &self.notes.open {
                Some(o) if o.note.comments.len() == *want => Ok(()),
                Some(o) => Err(format!(
                    "expected {want} comments, found {}",
                    o.note.comments.len()
                )),
                None => Err("no note is open".into()),
            },
            NoteCheck::Idle => {
                if self.notes.fetching || self.notes.writing {
                    Err("a notes fetch or write is running".into())
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_grows_every_side_and_stays_on_the_map() {
        let b = GeoBounds::new(10.0, 11.0, 20.0, 22.0);
        let e = expand(&b, 0.5);
        assert_eq!(
            (e.min_lat, e.max_lat, e.min_lon, e.max_lon),
            (9.5, 11.5, 19.0, 23.0)
        );
        let polar = expand(&GeoBounds::new(80.0, 85.0, 170.0, 180.0), 1.0);
        assert!(polar.max_lat <= 85.0 && polar.max_lon <= 180.0);
    }

    #[test]
    fn containment_is_inclusive() {
        let outer = GeoBounds::new(0.0, 2.0, 0.0, 2.0);
        assert!(contains(&outer, &GeoBounds::new(0.0, 1.0, 0.5, 2.0)));
        assert!(!contains(&outer, &GeoBounds::new(-0.1, 1.0, 0.5, 2.0)));
    }
}
