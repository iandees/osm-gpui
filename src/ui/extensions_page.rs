//! Settings > Extensions: the installed extensions and the ones the extension
//! index offers. An installed one's row opens to show what it may reach and
//! its own settings (its `settings` export).
//!
//! The page only shows and asks: the window gives it a [`Model`] (rows and
//! the index's state) and does what its [`PageEvent`]s ask (install, update,
//! uninstall, enable, rebuild, an extension's settings button), then hands it
//! the new model. Installing from the index first shows what the extension
//! may reach and asks again; uninstalling asks too.

use std::collections::{HashMap, HashSet};

use gpui::{div, prelude::*, px, App, Context, Entity, EventEmitter, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputState},
    label::Label,
    switch::Switch,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
};

use crate::extensions::runtime::{ButtonStyle, Element, Panel};
use crate::tr;
use crate::ui::style::muted_text_size;

/// One extension, installed or offered (or both).
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub description: String,
    pub authors: String,
    pub installed_version: Option<String>,
    /// The index's version, when the index lists it.
    pub index_version: Option<String>,
    /// What it may reach, one line each, already worded for the user.
    pub capabilities: Vec<String>,
    pub enabled: bool,
    /// Installed with Install Dev Extension (it can be rebuilt; it is never
    /// updated from the index).
    pub dev: bool,
    /// The index version runs in this app.
    pub compatible: bool,
    /// An install, update or rebuild is running.
    pub busy: bool,
    /// The last thing done to it failed.
    pub error: Option<String>,
    /// Its own settings, when it is running and has any.
    pub settings: Option<Panel>,
}

impl Row {
    /// The index has a newer version than the installed one.
    pub fn has_update(&self) -> bool {
        match (&self.installed_version, &self.index_version) {
            (Some(i), Some(a)) => {
                !self.dev
                    && self.compatible
                    && crate::extensions::registry::compare_versions(a, i).is_gt()
            }
            _ => false,
        }
    }

    fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty()
            || self.name.to_lowercase().contains(&q)
            || self.description.to_lowercase().contains(&q)
            || self.id.contains(&q)
    }
}

/// Whether the extension index could be read.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum IndexState {
    #[default]
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub rows: Vec<Row>,
    pub index: IndexState,
    /// An Install Dev Extension build is running.
    pub installing_dev: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Installed,
    Available,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PageEvent {
    Install(String),
    Update(String),
    Uninstall(String),
    SetEnabled(String, bool),
    Rebuild(String),
    InstallDev,
    /// Fetch the index again.
    Refresh,
    /// A button in an extension's settings, with the text of its inputs.
    Settings {
        extension: String,
        button: String,
        inputs: Vec<(String, String)>,
    },
}

/// A step the user is asked to confirm, for one extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Install from the index (its capabilities are shown first).
    Install(String),
    Uninstall(String),
}

impl Confirm {
    fn id(&self) -> &str {
        match self {
            Confirm::Install(id) | Confirm::Uninstall(id) => id,
        }
    }
}

pub struct ExtensionsPage {
    search: Entity<InputState>,
    tab: Tab,
    model: Model,
    /// The step being confirmed, if any.
    confirming: Option<Confirm>,
    /// Installed extensions whose details (capabilities, settings) are open.
    expanded: HashSet<String>,
    /// The text boxes of extensions' settings, by (extension, input id).
    inputs: HashMap<(String, String), Entity<InputState>>,
}

impl EventEmitter<PageEvent> for ExtensionsPage {}

impl ExtensionsPage {
    pub fn new(model: Model, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr!("dialog.extensions.search")));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        // With nothing installed, start where there is something to see.
        let tab = if model.rows.iter().any(|r| r.installed_version.is_some()) {
            Tab::Installed
        } else {
            Tab::Available
        };
        Self {
            search,
            tab,
            model,
            confirming: None,
            expanded: HashSet::new(),
            inputs: HashMap::new(),
        }
    }

    pub fn set_model(&mut self, model: Model, cx: &mut Context<Self>) {
        if let Some(confirm) = &self.confirming {
            // Installed or uninstalled meanwhile: nothing left to confirm.
            let still = model.rows.iter().any(|r| {
                r.id == confirm.id()
                    && match confirm {
                        Confirm::Install(_) => r.installed_version.is_none(),
                        Confirm::Uninstall(_) => r.installed_version.is_some(),
                    }
            });
            if !still {
                self.confirming = None;
            }
        }
        self.model = model;
        cx.notify();
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.confirming = None;
        cx.notify();
    }

    /// Ask to install `id` (shows its capabilities first).
    pub fn ask_install(&mut self, id: &str, cx: &mut Context<Self>) {
        self.confirming = Some(Confirm::Install(id.to_string()));
        cx.notify();
    }

    /// Ask whether to uninstall `id`.
    pub fn ask_uninstall(&mut self, id: &str, cx: &mut Context<Self>) {
        self.confirming = Some(Confirm::Uninstall(id.to_string()));
        cx.notify();
    }

    pub fn confirming(&self) -> Option<&Confirm> {
        self.confirming.as_ref()
    }

    /// The confirming row's confirm button: do what was asked.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        match self.confirming.take() {
            Some(Confirm::Install(id)) => cx.emit(PageEvent::Install(id)),
            Some(Confirm::Uninstall(id)) => cx.emit(PageEvent::Uninstall(id)),
            None => {}
        }
        cx.notify();
    }

    /// The confirming row's Cancel button (and Escape): back out.
    pub fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirming = None;
        cx.notify();
    }

    /// Open or close installed extension `id`'s details.
    pub fn toggle_details(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_string());
        }
        cx.notify();
    }

    /// Extension `extension`'s settings button `button` was pressed.
    fn settings_button(&mut self, extension: &str, button: &str, cx: &mut Context<Self>) {
        let inputs = self
            .inputs
            .iter()
            .filter(|((ext, _), _)| ext == extension)
            .map(|((_, id), state)| (id.clone(), state.read(cx).value().to_string()))
            .collect();
        // The boxes start again from what the extension's answer says (a
        // saved key is not left typed in).
        self.inputs.retain(|(ext, _), _| ext != extension);
        cx.emit(PageEvent::Settings {
            extension: extension.to_string(),
            button: button.to_string(),
            inputs,
        });
    }

    /// Extension `row`'s settings, as its section draws them.
    fn render_settings(
        &mut self,
        index: usize,
        row: &Row,
        panel: &Panel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut out = v_flex()
            .gap_2()
            .pt_2()
            .mt_1()
            .border_t_1()
            .border_color(theme.border);
        for (i, el) in panel.elements.iter().enumerate() {
            let key = index * 1000 + i;
            let element: gpui::AnyElement = match el {
                Element::Heading(t) => Label::new(t.clone())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .into_any_element(),
                Element::Text(t) => div()
                    .text_color(theme.foreground)
                    .child(SharedString::from(t.clone()))
                    .into_any_element(),
                Element::Muted(t) => div()
                    .text_size(muted_text_size())
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(t.clone()))
                    .into_any_element(),
                Element::Property((label, value)) => div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(label.clone())),
                    )
                    .child(SharedString::from(value.clone()))
                    .into_any_element(),
                Element::Buttons(buttons) => {
                    let mut line = div().flex().flex_row().flex_wrap().gap_2();
                    for (j, b) in buttons.iter().enumerate() {
                        let (ext, id) = (row.id.clone(), b.id.clone());
                        let button = Button::new(("ext-settings-button", key * 100 + j))
                            .small()
                            .label(b.label.clone())
                            .disabled(!b.enabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.settings_button(&ext, &id, cx)
                            }));
                        line = line.child(match b.style {
                            ButtonStyle::Primary => button.primary(),
                            ButtonStyle::Danger => button.danger(),
                            ButtonStyle::Ghost => button.ghost(),
                            ButtonStyle::Secondary => button,
                        });
                    }
                    line.into_any_element()
                }
                Element::Input(input) => {
                    let k = (row.id.clone(), input.id.clone());
                    let state = match self.inputs.get(&k) {
                        Some(s) => s.clone(),
                        None => {
                            let (placeholder, value, secret) =
                                (input.placeholder.clone(), input.value.clone(), input.secret);
                            let state = cx.new(|cx| {
                                let mut s = InputState::new(window, cx)
                                    .placeholder(placeholder)
                                    .masked(secret);
                                if !value.is_empty() {
                                    s.set_value(value, window, cx);
                                }
                                s
                            });
                            self.inputs.insert(k, state.clone());
                            state
                        }
                    };
                    v_flex()
                        .gap_1()
                        .when(!input.label.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_size(muted_text_size())
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(input.label.clone())),
                            )
                        })
                        .child(Input::new(&state))
                        .into_any_element()
                }
                Element::Link((label, url)) => {
                    let url = url.clone();
                    div()
                        .id(("ext-settings-link", key))
                        .text_color(theme.link)
                        .underline()
                        .cursor_pointer()
                        .child(SharedString::from(label.clone()))
                        .on_click(move |_, _, _| {
                            if url.starts_with("https://") || url.starts_with("http://") {
                                let _ = open::that(&url);
                            }
                        })
                        .into_any_element()
                }
                Element::Divider => div().h(px(1.0)).bg(theme.border).into_any_element(),
                // Settings show no pictures; say what it is.
                Element::Picture(p) => div()
                    .text_size(muted_text_size())
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(p.alt.clone()))
                    .into_any_element(),
            };
            out = out.child(element);
        }
        out
    }

    /// The rows the current tab and search show.
    pub fn visible_rows(&self, cx: &App) -> Vec<&Row> {
        let query = self.search.read(cx).value().to_string();
        self.model
            .rows
            .iter()
            .filter(|r| match self.tab {
                Tab::Installed => r.installed_version.is_some(),
                Tab::Available => r.installed_version.is_none() && r.index_version.is_some(),
            })
            .filter(|r| r.matches(&query))
            .collect()
    }

    fn render_row(
        &mut self,
        index: usize,
        row: &Row,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, foreground, danger) = (
            theme.muted_foreground,
            theme.border,
            theme.foreground,
            theme.danger,
        );
        let id = row.id.clone();
        let version = row
            .installed_version
            .clone()
            .or_else(|| row.index_version.clone())
            .unwrap_or_default();
        let confirming = self
            .confirming
            .as_ref()
            .filter(|c| c.id() == row.id)
            .cloned();

        let mut actions = div().flex().flex_row().items_center().gap_2();
        match self.tab {
            Tab::Installed => {
                if row.has_update() {
                    let id = id.clone();
                    actions = actions.child(
                        Button::new(("ext-update", index))
                            .small()
                            .primary()
                            .label(tr!(
                                "dialog.extensions.update_to",
                                version = row.index_version.clone().unwrap_or_default()
                            ))
                            .disabled(row.busy)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PageEvent::Update(id.clone()))
                            })),
                    );
                }
                if row.dev {
                    let id = id.clone();
                    actions = actions.child(
                        Button::new(("ext-rebuild", index))
                            .small()
                            .label(tr!("dialog.extensions.rebuild"))
                            .disabled(row.busy)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PageEvent::Rebuild(id.clone()))
                            })),
                    );
                }
                if confirming.is_none() {
                    let uninstall_id = id.clone();
                    actions = actions.child(
                        Button::new(("ext-uninstall", index))
                            .small()
                            .ghost()
                            .label(tr!("dialog.extensions.uninstall"))
                            .disabled(row.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.ask_uninstall(&uninstall_id, cx)
                            })),
                    );
                }
                let enabled = row.enabled;
                actions = actions.child(
                    Switch::new(("ext-enabled", index))
                        .checked(enabled)
                        .disabled(row.busy)
                        .tooltip(if enabled {
                            tr!("dialog.extensions.disable")
                        } else {
                            tr!("dialog.extensions.enable")
                        })
                        .on_click(cx.listener(move |_, checked: &bool, _, cx| {
                            cx.emit(PageEvent::SetEnabled(id.clone(), *checked))
                        })),
                );
            }
            Tab::Available if confirming.is_none() => {
                actions = actions.child(if row.compatible {
                    Button::new(("ext-install", index))
                        .small()
                        .primary()
                        .label(tr!("dialog.extensions.install"))
                        .disabled(row.busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.ask_install(&id, cx)))
                        .into_any_element()
                } else {
                    Label::new(tr!("dialog.extensions.incompatible"))
                        .text_size(muted_text_size())
                        .text_color(muted)
                        .into_any_element()
                });
            }
            Tab::Available => {}
        }

        let mut badges = div().flex().flex_row().gap_1();
        if row.dev {
            badges = badges.child(badge(tr!("dialog.extensions.dev"), cx));
        }
        if row.installed_version.is_some() && !row.enabled {
            badges = badges.child(badge(tr!("dialog.extensions.off"), cx));
        }

        let installed = row.installed_version.is_some();
        let expanded = installed && self.expanded.contains(&row.id);
        let toggle_id = row.id.clone();
        let title = div()
            .id(("ext-title", index))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .when(installed, |el| {
                el.cursor_pointer()
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(muted),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_details(&toggle_id, cx)),
                    )
            })
            .child(
                Label::new(row.name.clone())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(foreground),
            )
            .child(
                Label::new(version)
                    .text_size(muted_text_size())
                    .text_color(muted),
            )
            .child(badges);

        let mut block = v_flex()
            .id(("ext-row", index))
            .gap_1()
            .py_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(title)
                            .when(!row.description.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_color(foreground)
                                        .child(SharedString::from(row.description.clone())),
                                )
                            })
                            .when(!row.authors.is_empty(), |el| {
                                el.child(
                                    div().text_size(muted_text_size()).text_color(muted).child(
                                        tr!("dialog.extensions.by", authors = row.authors.clone()),
                                    ),
                                )
                            }),
                    )
                    .child(actions.flex_shrink_0()),
            );
        if expanded || confirming.is_some() {
            block = block.child(
                v_flex()
                    .gap_0p5()
                    .text_size(muted_text_size())
                    .text_color(muted)
                    .children(
                        row.capabilities
                            .iter()
                            .map(|c| div().child(SharedString::from(c.clone()))),
                    ),
            );
        }
        if expanded {
            match &row.settings {
                Some(panel) => {
                    let section = self.render_settings(index, row, panel, window, cx);
                    block = block.child(section);
                }
                None if row.enabled => {
                    block = block.child(
                        div()
                            .text_size(muted_text_size())
                            .text_color(muted)
                            .child(tr!("dialog.extensions.no_settings")),
                    );
                }
                None => {}
            }
        }
        if let Some(confirm) = &confirming {
            let (question, label) = match confirm {
                Confirm::Install(_) => (
                    None,
                    tr!("dialog.extensions.install_confirm", name = row.name.clone()),
                ),
                Confirm::Uninstall(_) => (
                    Some(tr!(
                        "dialog.extensions.uninstall_question",
                        name = row.name.clone()
                    )),
                    tr!(
                        "dialog.extensions.uninstall_confirm",
                        name = row.name.clone()
                    ),
                ),
            };
            let confirm_button = Button::new(("ext-confirm", index))
                .small()
                .label(label)
                .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)));
            let confirm_button = match confirm {
                Confirm::Install(_) => confirm_button.primary(),
                Confirm::Uninstall(_) => confirm_button.danger(),
            };
            block = block
                .children(question.map(|q| div().pt_1().text_color(foreground).child(q)))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_end()
                        .gap_2()
                        .pt_1()
                        .child(
                            Button::new(("ext-confirm-cancel", index))
                                .small()
                                .label(tr!("dialog.extensions.cancel"))
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_confirm(cx))),
                        )
                        .child(confirm_button),
                );
        }
        if row.busy {
            block = block.child(
                Label::new(tr!("dialog.extensions.working"))
                    .text_size(muted_text_size())
                    .text_color(muted),
            );
        }
        if let Some(e) = &row.error {
            block = block.child(
                div()
                    .text_size(muted_text_size())
                    .text_color(danger)
                    .child(SharedString::from(e.clone())),
            );
        }
        block
    }
}

fn badge(text: SharedString, cx: &App) -> impl IntoElement {
    div()
        .px_1p5()
        .rounded_sm()
        .bg(cx.theme().secondary)
        .text_size(muted_text_size())
        .text_color(cx.theme().secondary_foreground)
        .child(text)
}

impl Render for ExtensionsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let updates = self.model.rows.iter().filter(|r| r.has_update()).count();
        let installed = self
            .model
            .rows
            .iter()
            .filter(|r| r.installed_version.is_some())
            .count();

        let tab_button =
            |id: &'static str, tab: Tab, label: SharedString, cx: &mut Context<Self>| {
                let b = Button::new(id)
                    .small()
                    .label(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)));
                if self.tab == tab {
                    b.primary()
                } else {
                    b.ghost()
                }
            };
        let installed_label = if updates > 0 {
            tr!(
                "dialog.extensions.tab_installed_updates",
                count = installed,
                updates = updates
            )
        } else {
            tr!("dialog.extensions.tab_installed", count = installed)
        };
        let toolbar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(tab_button(
                "ext-tab-installed",
                Tab::Installed,
                installed_label,
                cx,
            ))
            .child(tab_button(
                "ext-tab-available",
                Tab::Available,
                tr!("dialog.extensions.tab_available"),
                cx,
            ))
            .child(div().flex_1().child(Input::new(&self.search).small()));

        let rows: Vec<Row> = self.visible_rows(cx).into_iter().cloned().collect();
        let mut list = v_flex();
        for (i, row) in rows.iter().enumerate() {
            let row = self.render_row(i, row, window, cx);
            list = list.child(row);
        }
        let empty_message = match (&self.tab, &self.model.index) {
            _ if !rows.is_empty() => None,
            (Tab::Available, IndexState::Loading) => Some(tr!("dialog.extensions.loading")),
            (Tab::Available, IndexState::Failed(e)) => {
                Some(tr!("dialog.extensions.index_failed", detail = e.clone()))
            }
            (Tab::Available, IndexState::Loaded) => Some(tr!("dialog.extensions.none_available")),
            (Tab::Installed, _) => Some(tr!("dialog.extensions.none_installed")),
        };
        let actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                Button::new("ext-install-dev")
                    .small()
                    .label(tr!("dialog.extensions.install_dev"))
                    .disabled(self.model.installing_dev)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PageEvent::InstallDev))),
            )
            .child(
                Button::new("ext-refresh")
                    .small()
                    .ghost()
                    .icon(IconName::Redo2)
                    .label(tr!("dialog.extensions.refresh"))
                    .disabled(self.model.index == IndexState::Loading)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PageEvent::Refresh))),
            );

        v_flex()
            .w_full()
            .gap_2()
            .child(toolbar)
            .child(list)
            .children(empty_message.map(|m| div().py_6().text_center().text_color(muted).child(m)))
            .child(actions.pt_2())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(installed: Option<&str>, index: Option<&str>) -> Row {
        Row {
            id: "x".into(),
            name: "X".into(),
            installed_version: installed.map(String::from),
            index_version: index.map(String::from),
            compatible: true,
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn an_update_is_a_newer_compatible_index_version_of_a_non_dev_install() {
        assert!(row(Some("0.1.0"), Some("0.2.0")).has_update());
        assert!(!row(Some("0.2.0"), Some("0.2.0")).has_update());
        assert!(!row(Some("0.10.0"), Some("0.9.0")).has_update());
        assert!(!row(None, Some("0.2.0")).has_update());
        let dev = Row {
            dev: true,
            ..row(Some("0.1.0"), Some("0.2.0"))
        };
        assert!(!dev.has_update());
        let incompatible = Row {
            compatible: false,
            ..row(Some("0.1.0"), Some("0.2.0"))
        };
        assert!(!incompatible.has_update());
    }

    #[test]
    fn search_matches_name_description_and_id() {
        let r = Row {
            id: "maproulette".into(),
            name: "MapRoulette".into(),
            description: "Find tasks".into(),
            ..Default::default()
        };
        assert!(r.matches(""));
        assert!(r.matches("roulette"));
        assert!(r.matches("TASKS"));
        assert!(!r.matches("lanes"));
    }
}
