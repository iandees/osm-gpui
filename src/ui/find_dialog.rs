//! Edit > Find: a modal with a query field, a mode choice, a live match count
//! and a collapsible syntax hint. It never touches the map itself: it parses
//! the query (to show errors), asks the app for the count through
//! [`DialogEvent::Preview`] and hands the final query back in
//! [`DialogEvent::Submitted`].

use std::time::Duration;

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Subscription, Task, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex, ActiveTheme as _, Selectable as _, Sizable as _,
};

use crate::search::{ParseError, ParseErrorKind, SearchMode};
use crate::ui::modal;
use crate::ui::style::muted_text_size;
use crate::{tr, tr_count};

/// How long typing must pause before the count is refreshed.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// Recent queries listed under an empty field.
const RECENT_SHOWN: usize = 5;

pub enum DialogEvent {
    /// The (parsable) query to count matches for; answer with
    /// [`FindDialog::set_count`].
    Preview {
        query: String,
        mode: SearchMode,
    },
    /// Enter or the Find button: apply the query.
    Submitted {
        query: String,
        mode: SearchMode,
    },
    Cancelled,
}

/// What the line under the mode buttons says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Nothing typed.
    Blank,
    /// Typed, count not yet known.
    Pending,
    Count(usize),
    Error(ParseError),
}

pub struct FindDialog {
    query: Entity<InputState>,
    mode: SearchMode,
    /// Earlier queries, newest first.
    history: Vec<String>,
    /// Which history entry the field currently shows (arrow keys).
    history_pos: Option<usize>,
    /// What was typed before the arrow keys started replacing it.
    draft: String,
    hint_open: bool,
    preview: Preview,
    /// The query text `preview` was computed for.
    preview_for: String,
    debounce: Option<Task<()>>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<DialogEvent> for FindDialog {}

impl FindDialog {
    pub fn new(
        mode: SearchMode,
        history: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("dialog.find.placeholder")));
        let focus_handle = cx.focus_handle();
        query.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe(&query, |this, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.history_pos = None;
                this.schedule_preview(cx);
            }
        });
        Self {
            query,
            mode,
            history,
            history_pos: None,
            draft: String::new(),
            hint_open: false,
            preview: Preview::Blank,
            preview_for: String::new(),
            debounce: None,
            focus_handle,
            _subscription: subscription,
        }
    }

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    pub fn hint_open(&self) -> bool {
        self.hint_open
    }

    pub fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.schedule_preview(cx);
            cx.notify();
        }
    }

    pub fn toggle_hint(&mut self, cx: &mut Context<Self>) {
        self.hint_open = !self.hint_open;
        cx.notify();
    }

    /// The app's answer to [`DialogEvent::Preview`]. Ignored when the field
    /// has changed since.
    pub fn set_count(&mut self, query: &str, count: usize, cx: &mut Context<Self>) {
        if self.preview_for == query {
            self.preview = Preview::Count(count);
            cx.notify();
        }
    }

    /// The text shown under the mode buttons.
    pub fn preview_text(&self) -> SharedString {
        match &self.preview {
            Preview::Blank => SharedString::default(),
            Preview::Pending => tr!("dialog.find.counting"),
            Preview::Count(0) => tr!("dialog.find.no_matches"),
            Preview::Count(n) => tr_count!("dialog.find.matches", *n as u64),
            Preview::Error(e) => describe_error(e),
        }
    }

    /// (Re)start the debounce: after a pause, parse the field and either show
    /// the error or ask the app for the count.
    fn schedule_preview(&mut self, cx: &mut Context<Self>) {
        let text = self.query_text(cx);
        self.preview_for = text.clone();
        if text.trim().is_empty() {
            self.preview = Preview::Blank;
            self.debounce = None;
            cx.notify();
            return;
        }
        if !matches!(self.preview, Preview::Count(_)) {
            self.preview = Preview::Pending;
        }
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.run_preview(&text, cx));
        }));
        cx.notify();
    }

    /// Parse the settled query; errors are shown here, the rest goes to the
    /// app to count.
    pub fn run_preview(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.preview_for != text {
            return;
        }
        match crate::search::parse(text) {
            Err(e) => {
                self.preview = Preview::Error(e);
                cx.notify();
            }
            Ok(_) => cx.emit(DialogEvent::Preview {
                query: text.to_string(),
                mode: self.mode,
            }),
        }
    }

    pub fn press_apply(&mut self, cx: &mut Context<Self>) {
        let query = self.query_text(cx);
        if query.trim().is_empty() {
            return;
        }
        // A query that does not parse stays open, with the reason showing.
        match crate::search::parse(&query) {
            Err(e) => {
                self.preview_for = query;
                self.preview = Preview::Error(e);
                cx.notify();
            }
            Ok(_) => cx.emit(DialogEvent::Submitted {
                query,
                mode: self.mode,
            }),
        }
    }

    pub fn press_cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Cancelled);
    }

    /// Show `text` in the field (history recall) and refresh the count.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |state, cx| {
            state.set_value(text.to_string(), window, cx)
        });
        self.schedule_preview(cx);
    }

    fn recall(&mut self, older: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, older) {
            (None, true) => {
                self.draft = self.query_text(cx);
                Some(0)
            }
            (None, false) => return,
            (Some(i), true) => Some((i + 1).min(self.history.len() - 1)),
            (Some(0), false) => None,
            (Some(i), false) => Some(i - 1),
        };
        let text = match next {
            Some(i) => self.history[i].clone(),
            None => self.draft.clone(),
        };
        self.set_query(&text, window, cx);
        // `set_query` ends in a Change-less update; keep the position.
        self.history_pos = next;
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.press_cancel(cx),
            modal::ModalKey::Enter => self.press_apply(cx),
            modal::ModalKey::Other => match k.key.as_str() {
                "up" => self.recall(true, window, cx),
                "down" => self.recall(false, window, cx),
                "f1" => self.toggle_hint(cx),
                // Cmd/Ctrl+1..4 pick the mode (Tab would move focus away
                // from the field).
                key @ ("1" | "2" | "3" | "4") if k.modifiers.secondary() => {
                    let i = key.parse::<usize>().unwrap_or(1) - 1;
                    self.set_mode(SearchMode::ALL[i], cx);
                    cx.stop_propagation();
                }
                _ => {}
            },
        }
    }
}

/// A parse error as one readable line with its position.
pub fn describe_error(e: &ParseError) -> SharedString {
    let position = e.column();
    let message = match &e.kind {
        ParseErrorKind::Empty => tr!("dialog.find.error.empty"),
        ParseErrorKind::ExpectedExpression => tr!("dialog.find.error.expected_expression"),
        ParseErrorKind::UnexpectedCloseParen => tr!("dialog.find.error.unexpected_close_paren"),
        ParseErrorKind::UnclosedParen => tr!("dialog.find.error.unclosed_paren"),
        ParseErrorKind::UnterminatedQuote => tr!("dialog.find.error.unterminated_quote"),
        ParseErrorKind::EmptyQuotes => tr!("dialog.find.error.empty_quotes"),
        ParseErrorKind::MissingKey => tr!("dialog.find.error.missing_key"),
        ParseErrorKind::InvalidNumber(t) => tr!("dialog.find.error.invalid_number", text = t),
        ParseErrorKind::InvalidRange(t) => tr!("dialog.find.error.invalid_range", text = t),
        ParseErrorKind::UnknownType(t) => tr!("dialog.find.error.unknown_type", text = t),
        ParseErrorKind::TooDeep => tr!("dialog.find.error.too_deep"),
    };
    tr!(
        "dialog.find.error.at",
        position = position,
        message = message
    )
}

fn mode_label(mode: SearchMode) -> SharedString {
    match mode {
        SearchMode::Replace => tr!("dialog.find.mode.replace"),
        SearchMode::Add => tr!("dialog.find.mode.add"),
        SearchMode::Remove => tr!("dialog.find.mode.remove"),
        SearchMode::FindInSelection => tr!("dialog.find.mode.in_selection"),
    }
}

impl modal::ModalDialog for FindDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl Focusable for FindDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FindDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let modes =
            div()
                .flex()
                .flex_row()
                .gap_1()
                .children(SearchMode::ALL.iter().enumerate().map(|(i, mode)| {
                    let mode = *mode;
                    let selected = mode == self.mode;
                    let button = Button::new(("find-mode", i))
                        .small()
                        .compact()
                        .label(mode_label(mode))
                        .selected(selected)
                        .on_click(cx.listener(move |this, _, _w, cx| this.set_mode(mode, cx)));
                    if selected {
                        button.primary()
                    } else {
                        button.outline()
                    }
                }));

        let status_color = match self.preview {
            Preview::Error(_) => cx.theme().danger,
            _ => muted,
        };
        let status = div()
            .h(gpui::px(20.0))
            .child(Label::new(self.preview_text()).text_color(status_color));

        // Under an empty field: the last few queries, to click.
        let recent = (self.preview == Preview::Blank && !self.history.is_empty()).then(|| {
            v_flex()
                .gap_0p5()
                .child(
                    Label::new(tr!("dialog.find.recent"))
                        .text_size(muted_text_size())
                        .text_color(muted),
                )
                .children(
                    self.history
                        .iter()
                        .take(RECENT_SHOWN)
                        .enumerate()
                        .map(|(i, q)| {
                            let q_click = q.clone();
                            div()
                                .id(("find-recent", i))
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .cursor_pointer()
                                .hover(|el| el.bg(cx.theme().accent))
                                .child(Label::new(q.clone()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_query(&q_click, window, cx);
                                }))
                        }),
                )
        });

        let examples: [(&str, SharedString); 6] = [
            ("amenity=cafe", tr!("dialog.find.hint.equals")),
            ("name=Star*", tr!("dialog.find.hint.wildcard")),
            ("starbucks", tr!("dialog.find.hint.word")),
            ("highway=* -name=*", tr!("dialog.find.hint.negation")),
            ("type:way nodes:10-", tr!("dialog.find.hint.counts")),
            ("modified OR new", tr!("dialog.find.hint.states")),
        ];
        let hint_toggle = Button::new("find-hint-toggle")
            .small()
            .compact()
            .outline()
            .label(if self.hint_open {
                tr!("dialog.find.hint.hide")
            } else {
                tr!("dialog.find.hint.show")
            })
            .on_click(cx.listener(|this, _, _w, cx| this.toggle_hint(cx)));
        let hint = self.hint_open.then(|| {
            v_flex()
                .gap_0p5()
                .children(examples.into_iter().map(|(code, text)| {
                    div()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .child(
                            div()
                                .w(gpui::px(170.0))
                                .flex_shrink_0()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(muted_text_size())
                                .child(code),
                        )
                        .child(
                            Label::new(text)
                                .text_size(muted_text_size())
                                .text_color(muted),
                        )
                }))
                .child(
                    Label::new(tr!("dialog.find.hint.keys"))
                        .text_size(muted_text_size())
                        .text_color(muted),
                )
        });

        // A fixed width (the frame's 680 less the body's padding): the body
        // slot sizes to its content, which squeezes the input to nothing
        // when the rows around it are narrow or the syntax help is open.
        let body = v_flex()
            .w(gpui::px(644.0))
            .gap_3()
            .child(div().w_full().child(Input::new(&self.query).w_full()))
            .child(modes)
            .child(status)
            .children(recent)
            .child(hint_toggle)
            .children(hint);

        let footer = modal::footer_row(
            "find-cancel",
            tr!("dialog.cancel"),
            cx.listener(|this, _, _w, cx| this.press_cancel(cx)),
            "find-apply",
            tr!("dialog.find.apply"),
            cx.listener(|this, _, _w, cx| this.press_apply(cx)),
        );
        let frame =
            modal::dialog_frame(cx, gpui::px(680.0), tr!("dialog.find.title"), body, footer);
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}
