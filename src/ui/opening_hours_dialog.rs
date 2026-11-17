//! The opening-hours editor: day groups (toggle chips for `Mo`–`Su` and `PH`)
//! each with its time ranges or "closed", a box to paste hours copied from a
//! website and convert them, and a live preview of the `opening_hours` value.

use gpui::{
    div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    SharedString, Subscription, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    v_flex, ActiveTheme as _, IconName, Sizable as _,
};

use crate::opening_hours::{self, Rule, Schedule, TimeRange, DAY_CODES};
use crate::tr;
use crate::ui::modal;
use crate::ui::style::muted_text_size;

pub enum DialogEvent {
    /// The new tag value (empty: remove the tag).
    Submitted(String),
    Cancelled,
}

struct RangeInputs {
    start: Entity<InputState>,
    end: Entity<InputState>,
    _subscriptions: [Subscription; 2],
}

struct DayGroup {
    days: [bool; 8],
    closed: bool,
    ranges: Vec<RangeInputs>,
}

pub struct OpeningHoursDialog {
    title: SharedString,
    groups: Vec<DayGroup>,
    paste: Entity<InputState>,
    /// What the last Convert said, and whether it is an error.
    paste_message: Option<(SharedString, bool)>,
    /// The starting value could not be shown as day groups.
    unsupported: bool,
    /// The paste box (rather than the dialog) takes focus on opening: there
    /// were no hours to show.
    focus_paste: bool,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for OpeningHoursDialog {}

impl OpeningHoursDialog {
    /// An editor titled `title` (the field's label) starting from `value`.
    pub fn new(
        title: impl Into<SharedString>,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let paste = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 8)
                .placeholder(tr!("dialog.opening_hours.paste_placeholder"))
        });
        let parsed = opening_hours::parse_osm(value);
        let mut this = Self {
            title: title.into(),
            groups: Vec::new(),
            paste,
            paste_message: None,
            unsupported: parsed.is_none(),
            focus_paste: false,
            focus_handle: cx.focus_handle(),
        };
        match parsed {
            Some(schedule) if !schedule.rules.is_empty() => {
                this.load(&schedule, window, cx);
                window.focus(&this.focus_handle, cx);
            }
            // Nothing set yet (or nothing the editor can show): one group
            // to fill in, weekdays picked as the usual starting point, and
            // the paste box focused, since pasting is the quickest start.
            _ => {
                let mut days = [false; 8];
                days[..5].fill(true);
                this.push_group(days, &[], window, cx);
                this.focus_paste = true;
                this.paste.update(cx, |state, cx| state.focus(window, cx));
            }
        }
        this
    }

    fn range_inputs(
        range: Option<TimeRange>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> RangeInputs {
        let mut input = |value: Option<u16>, placeholder: &str| {
            let placeholder = placeholder.to_string();
            let state = cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                if let Some(minutes) = value {
                    state.set_value(opening_hours::format_time(minutes), window, cx);
                }
                state
            });
            let subscription = cx.subscribe(&state, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
            (state, subscription)
        };
        let (start, s1) = input(range.map(|r| r.start), "09:00");
        let (end, s2) = input(range.map(|r| r.end), "17:00");
        RangeInputs {
            start,
            end,
            _subscriptions: [s1, s2],
        }
    }

    fn push_group(
        &mut self,
        days: [bool; 8],
        times: &[TimeRange],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let closed = false;
        let mut ranges: Vec<RangeInputs> = times
            .iter()
            .map(|t| Self::range_inputs(Some(*t), window, cx))
            .collect();
        if ranges.is_empty() {
            ranges.push(Self::range_inputs(None, window, cx));
        }
        self.groups.push(DayGroup {
            days,
            closed,
            ranges,
        });
    }

    /// Replace the groups with `schedule`'s.
    fn load(&mut self, schedule: &Schedule, window: &mut Window, cx: &mut Context<Self>) {
        self.groups.clear();
        for rule in schedule.normalized().rules {
            self.push_group(rule.days, &rule.times, window, cx);
            if rule.times.is_empty() {
                if let Some(group) = self.groups.last_mut() {
                    group.closed = true;
                }
            }
        }
    }

    /// The schedule the groups describe, or what is wrong with them. A group
    /// with no days, and a time row left blank, are ignored.
    fn schedule(&self, cx: &App) -> Result<Schedule, SharedString> {
        let mut rules = Vec::new();
        for group in &self.groups {
            if !group.days.iter().any(|d| *d) {
                continue;
            }
            if group.closed {
                rules.push(Rule {
                    days: group.days,
                    times: Vec::new(),
                });
                continue;
            }
            let mut times = Vec::new();
            for range in &group.ranges {
                let start = range.start.read(cx).value().to_string();
                let end = range.end.read(cx).value().to_string();
                if start.trim().is_empty() && end.trim().is_empty() {
                    continue;
                }
                let bad = |text: &str| tr!("dialog.opening_hours.error.time", time = text.trim());
                let s = opening_hours::parse_time(&start).ok_or_else(|| bad(&start))?;
                let mut e = opening_hours::parse_time(&end).ok_or_else(|| bad(&end))?;
                if e == 0 {
                    e = opening_hours::DAY_MINUTES;
                }
                if s >= opening_hours::DAY_MINUTES || s == e {
                    return Err(tr!(
                        "dialog.opening_hours.error.range",
                        start = start.trim(),
                        end = end.trim()
                    ));
                }
                times.push(TimeRange::new(s, e));
            }
            if !times.is_empty() {
                rules.push(Rule {
                    days: group.days,
                    times,
                });
            }
        }
        Ok(Schedule { rules })
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if let Ok(schedule) = self.schedule(cx) {
            cx.emit(DialogEvent::Submitted(schedule.to_osm()));
        }
    }

    fn convert_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.paste.read(cx).value().to_string();
        // On failure the groups are left as they were.
        self.paste_message = Some(match opening_hours::parse_text(&text) {
            Ok(schedule) => {
                self.load(&schedule, window, cx);
                self.unsupported = false;
                (tr!("dialog.opening_hours.converted"), false)
            }
            Err(opening_hours::TextError::NothingFound) => {
                (tr!("dialog.opening_hours.not_understood"), true)
            }
            Err(opening_hours::TextError::Partial) => {
                (tr!("dialog.opening_hours.partly_understood"), true)
            }
        });
        cx.notify();
    }

    /// Turn `day` on or off in group `g`; a day turned on leaves every
    /// other group, so each day has one set of hours.
    fn toggle_day(&mut self, g: usize, day: usize, cx: &mut Context<Self>) {
        let on = !self.groups[g].days[day];
        if on {
            for group in &mut self.groups {
                group.days[day] = false;
            }
        }
        self.groups[g].days[day] = on;
        cx.notify();
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => cx.emit(DialogEvent::Cancelled),
            // Enter in the paste box is a new line.
            modal::ModalKey::Enter if !self.paste.read(cx).focus_handle(cx).is_focused(window) => {
                self.submit(cx)
            }
            _ => {}
        }
    }

    fn render_group(&self, g: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let group = &self.groups[g];
        let chips = h_flex()
            .gap_1()
            .flex_wrap()
            .children(DAY_CODES.iter().enumerate().map(|(d, code)| {
                let button = Button::new(("oh-day", g * 8 + d))
                    .label(*code)
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_day(g, d, cx)));
                if group.days[d] {
                    button.primary()
                } else {
                    button.outline()
                }
            }));
        let header = h_flex().justify_between().gap_2().child(chips).child(
            h_flex()
                .gap_2()
                .child(
                    Checkbox::new(("oh-closed", g))
                        .checked(group.closed)
                        .label(tr!("dialog.opening_hours.closed"))
                        .text_size(crate::ui::style::body_text_size())
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.groups[g].closed = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(("oh-remove-group", g))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip(tr!("dialog.opening_hours.remove_group"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.groups.remove(g);
                            cx.notify();
                        })),
                ),
        );

        let mut column = v_flex()
            .gap_2()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(header);
        if !group.closed {
            let muted = cx.theme().muted_foreground;
            column = column
                .children(group.ranges.iter().enumerate().map(|(r, range)| {
                    h_flex()
                        .gap_2()
                        .child(div().w(px(84.)).child(Input::new(&range.start).small()))
                        .child(Label::new("–").text_color(muted))
                        .child(div().w(px(84.)).child(Input::new(&range.end).small()))
                        .child(
                            Button::new(("oh-remove-range", g * 100 + r))
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip(tr!("dialog.opening_hours.remove_time"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.groups[g].ranges.remove(r);
                                    cx.notify();
                                })),
                        )
                }))
                .child(
                    h_flex().child(
                        Button::new(("oh-add-range", g))
                            .label(tr!("dialog.opening_hours.add_time"))
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let inputs = Self::range_inputs(None, window, cx);
                                this.groups[g].ranges.push(inputs);
                                cx.notify();
                            })),
                    ),
                );
        }
        column
    }
}

impl crate::ui::modal::ModalDialog for OpeningHoursDialog {
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        if self.focus_paste {
            self.paste.read(cx).focus_handle(cx)
        } else {
            self.focus_handle.clone()
        }
    }
}

impl Focusable for OpeningHoursDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OpeningHoursDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let section_label = |text: SharedString| {
            Label::new(text)
                .text_size(muted_text_size())
                .text_color(muted)
        };

        let mut body = v_flex().gap_3();
        if self.unsupported {
            body = body.child(
                Label::new(tr!("dialog.opening_hours.unsupported"))
                    .text_size(muted_text_size())
                    .text_color(cx.theme().warning),
            );
        }

        // Paste-and-convert.
        let mut paste = v_flex()
            .gap_1()
            .child(section_label(tr!("dialog.opening_hours.paste_label")))
            .child(Input::new(&self.paste))
            .child(
                h_flex().child(
                    Button::new("oh-convert")
                        .label(tr!("dialog.opening_hours.convert"))
                        .small()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.convert_paste(window, cx)),
                        ),
                ),
            );
        if let Some((message, is_error)) = self.paste_message.clone() {
            paste = paste.child(
                Label::new(message)
                    .text_size(muted_text_size())
                    .text_color(if is_error { cx.theme().danger } else { muted }),
            );
        }
        body = body.child(paste);

        // Day groups.
        body = body
            .child(section_label(tr!("dialog.opening_hours.groups_label")))
            .children((0..self.groups.len()).map(|g| self.render_group(g, cx).into_any_element()))
            .child(
                h_flex().child(
                    Button::new("oh-add-group")
                        .label(tr!("dialog.opening_hours.add_group"))
                        .icon(IconName::Plus)
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, window, cx| {
                            let taken: Vec<bool> = (0..8)
                                .map(|d| this.groups.iter().any(|g| g.days[d]))
                                .collect();
                            // Start with the weekend if it is still free.
                            let mut days = [false; 8];
                            if !taken[5] && !taken[6] {
                                days[5] = true;
                                days[6] = true;
                            }
                            this.push_group(days, &[], window, cx);
                            cx.notify();
                        })),
                ),
            );

        // Preview.
        let schedule = self.schedule(cx);
        let preview = match &schedule {
            Ok(s) => {
                let value = s.to_osm();
                if value.is_empty() {
                    Label::new(tr!("dialog.opening_hours.empty")).text_color(muted)
                } else {
                    Label::new(value)
                }
            }
            Err(message) => Label::new(message.clone()).text_color(cx.theme().danger),
        };
        body = body.child(
            v_flex()
                .gap_1()
                .child(section_label(tr!("dialog.opening_hours.result")))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(cx.theme().muted)
                        .child(preview),
                ),
        );

        let footer = modal::footer_buttons(
            Button::new("cancel")
                .label(tr!("dialog.cancel"))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Cancelled))),
            gpui_component::Disableable::disabled(
                Button::new("apply")
                    .primary()
                    .label(tr!("dialog.opening_hours.apply")),
                schedule.is_err(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
        );
        let frame = modal::card(cx, px(520.0), self.title.clone(), body)
            .footer(footer)
            .scroll_body("oh-body", px(560.0))
            .build();
        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialog mounted in a window under a `Root`, as the app mounts it
    /// (focusing an input needs one).
    struct Handle {
        window: gpui::WindowHandle<gpui_component::Root>,
        dialog: Entity<OpeningHoursDialog>,
    }

    impl Handle {
        fn update<R>(
            &self,
            cx: &mut gpui::TestAppContext,
            f: impl FnOnce(&mut OpeningHoursDialog, &mut Window, &mut Context<OpeningHoursDialog>) -> R,
        ) -> anyhow::Result<R> {
            let dialog = self.dialog.clone();
            self.window.update(cx, |_, window, cx| {
                dialog.update(cx, |d, cx| f(d, window, cx))
            })
        }
    }

    fn dialog(value: &str, cx: &mut gpui::TestAppContext) -> Handle {
        cx.update(gpui_component::init);
        let value = value.to_string();
        let window = cx.add_window(|window, cx| {
            let dialog = cx.new(|cx| OpeningHoursDialog::new("Hours", &value, window, cx));
            gpui_component::Root::new(dialog, window, cx)
        });
        let dialog = window
            .update(cx, |root, _, _| root.view().clone())
            .unwrap()
            .downcast()
            .unwrap();
        Handle { window, dialog }
    }

    fn value(d: &Handle, cx: &mut gpui::TestAppContext) -> String {
        d.update(cx, |d, _, cx| d.schedule(cx).unwrap().to_osm())
            .unwrap()
    }

    #[gpui::test]
    fn loads_and_writes_back_an_existing_value(cx: &mut gpui::TestAppContext) {
        let d = dialog("Mo-Fr 09:00-12:00,13:00-17:00; Sa 10:00-14:00; Su off", cx);
        d.update(cx, |d, _, _| {
            assert_eq!(d.groups.len(), 3);
            assert!(d.groups[2].closed);
            assert!(!d.unsupported);
        })
        .unwrap();
        assert_eq!(
            value(&d, cx),
            "Mo-Fr 09:00-12:00,13:00-17:00; Sa 10:00-14:00; Su off"
        );
    }

    #[gpui::test]
    fn a_new_value_starts_blank(cx: &mut gpui::TestAppContext) {
        let d = dialog("", cx);
        assert_eq!(value(&d, cx), "");
    }

    #[gpui::test]
    fn an_unsupported_value_is_flagged(cx: &mut gpui::TestAppContext) {
        let d = dialog("Jan-Mar Mo 09:00-12:00", cx);
        d.update(cx, |d, _, _| assert!(d.unsupported)).unwrap();
    }

    #[gpui::test]
    fn a_day_belongs_to_one_group(cx: &mut gpui::TestAppContext) {
        let d = dialog("Mo-Fr 09:00-17:00; Sa 10:00-14:00", cx);
        d.update(cx, |d, _, cx| d.toggle_day(1, 0, cx)).unwrap();
        assert_eq!(value(&d, cx), "Mo,Sa 10:00-14:00; Tu-Fr 09:00-17:00");
    }

    #[gpui::test]
    fn a_bad_time_is_reported(cx: &mut gpui::TestAppContext) {
        let d = dialog("Mo 09:00-17:00", cx);
        let err = d
            .update(cx, |d, window, cx| {
                d.groups[0].ranges[0]
                    .end
                    .update(cx, |s, cx| s.set_value("27:00", window, cx));
                d.schedule(cx).unwrap_err()
            })
            .unwrap();
        assert!(err.contains("27:00"), "{err}");
    }

    #[gpui::test]
    fn text_it_cannot_fully_read_changes_nothing(cx: &mut gpui::TestAppContext) {
        let d = dialog("Mo 08:00-09:00", cx);
        let is_error = d
            .update(cx, |d, window, cx| {
                d.paste.update(cx, |p, cx| {
                    p.set_value("Monday-Thursday 10:00 am to Close", window, cx)
                });
                d.convert_paste(window, cx);
                d.paste_message.as_ref().map(|(_, is_error)| *is_error)
            })
            .unwrap();
        assert_eq!(is_error, Some(true));
        assert_eq!(value(&d, cx), "Mo 08:00-09:00");
    }

    #[gpui::test]
    fn converting_pasted_text_replaces_the_groups(cx: &mut gpui::TestAppContext) {
        let d = dialog("Mo 08:00-09:00", cx);
        d.update(cx, |d, window, cx| {
            d.paste.update(cx, |p, cx| {
                p.set_value("Mon-Fri 9am-5pm\nSat 10-2\nSun closed", window, cx)
            });
            d.convert_paste(window, cx);
        })
        .unwrap();
        assert_eq!(value(&d, cx), "Mo-Fr 09:00-17:00; Sa 10:00-14:00; Su off");
    }
}
