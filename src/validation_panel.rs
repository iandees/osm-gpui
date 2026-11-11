//! The side panel's Validation section: the validator's findings for the
//! pending changes, kept next to the map so problems can be fixed one after
//! another without reopening anything.
//!
//! The section exists only after Tools > Validate (or the upload dialog's
//! "Show in side panel") and until its close button. While it exists it is
//! re-run, debounced, whenever the edited data changed (see [`Signal`]).

use std::time::Duration;

use gpui::{div, prelude::*, px, Context};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    label::Label,
    ActiveTheme as _, IconName, Sizable as _,
};

use osm_gpui::layers::LayerId;
use osm_gpui::tr;
use osm_gpui::ui::style::muted_text_size;
use osm_gpui::ui::validation_list::{
    feature_refs, finding_key, findings_rows, note_row, position_of, summary_text, FindingKey,
};
use osm_gpui::validator::Report;

use crate::MapViewer;

/// How long after the last change the list is refreshed.
pub(crate) const REFRESH_DELAY: Duration = Duration::from_millis(300);

/// Index of the Validation section in `MapViewer::side_panel_open`.
pub(crate) const SECTION: usize = 7;

/// What "the edited data changed" means: `(undo stack revision, data
/// generation, active layer)`. The revision is bumped by every edit, undo and
/// redo; the data generation by what changes data outside the history
/// (an upload being applied, reconciliation, a layer added or deleted).
pub(crate) type Signal = (u64, u64, Option<LayerId>);

pub(crate) struct ValidationPanel {
    pub report: Report,
    /// The signal the report was computed at.
    ran_at: Signal,
    /// The latest signal seen by the watcher; a refresh is scheduled when it
    /// changes.
    seen: Signal,
    /// Counts the refreshes scheduled; only the latest one runs.
    generation: u64,
    /// The finding last clicked, so its row stays highlighted across
    /// refreshes while it still exists and its objects stay selected.
    current: Option<FindingKey>,
}

impl ValidationPanel {
    pub(crate) fn current(&self) -> Option<&FindingKey> {
        self.current.as_ref()
    }
}

/// About how tall one finding's row is (a title and a line of objects).
const ROW_HEIGHT_ESTIMATE: f32 = 48.0;

impl MapViewer {
    /// What the Validation section asks of the side panel layout, or `None`
    /// when it is not showing.
    pub(crate) fn validation_want(&self) -> Option<crate::side_panel_layout::Want> {
        let panel = self.validation.as_ref()?;
        Some(crate::side_panel_layout::Want {
            open: self.side_panel_open[SECTION],
            content: panel.report.entries.len().max(1) as f32 * ROW_HEIGHT_ESTIMATE,
        })
    }

    fn validation_signal(&self) -> Signal {
        (
            self.undo_stack.revision,
            self.data_generation,
            self.active_layer,
        )
    }

    /// Whether the Validation section is in the side panel.
    pub(crate) fn validation_shown(&self) -> bool {
        self.validation.is_some()
    }

    /// The report the Validation section shows, if it is showing.
    pub(crate) fn shown_validation(&self) -> Option<&Report> {
        self.validation.as_ref().map(|p| &p.report)
    }

    /// "Show in side panel": the explicit request to see the list, so the
    /// section is shown expanded.
    pub(crate) fn show_validation_expanded(&mut self, cx: &mut Context<Self>) {
        self.show_validation(cx);
        self.side_panel_open[SECTION] = true;
        cx.notify();
    }

    /// Tools > Validate: run the checks and show the section, whatever the
    /// result. It is collapsed (its header carries the counts) until the user
    /// expands it, and stays as the user left it for the rest of the session
    /// (a later Validate does not change it).
    pub(crate) fn show_validation(&mut self, cx: &mut Context<Self>) {
        let report = self.validation_report();
        self.set_status(if report.checked_changes {
            summary_text(&report)
        } else {
            tr!("validator.nothing_pending")
        });
        let signal = self.validation_signal();
        let current = self.validation.take().and_then(|p| p.current);
        self.validation = Some(ValidationPanel {
            report,
            ran_at: signal,
            seen: signal,
            generation: 0,
            current,
        });
        cx.notify();
    }

    /// The close button: remove the section.
    pub(crate) fn close_validation(&mut self, cx: &mut Context<Self>) {
        self.validation = None;
        cx.notify();
    }

    /// Re-run the validator now and keep the current finding if it is still
    /// reported.
    fn rerun_validation(&mut self) {
        let report = self.validation_report();
        let signal = self.validation_signal();
        if let Some(panel) = self.validation.as_mut() {
            if panel
                .current
                .as_ref()
                .is_some_and(|key| position_of(&report, key).is_none())
            {
                panel.current = None;
            }
            panel.report = report;
            panel.ran_at = signal;
            panel.seen = signal;
        }
    }

    /// Called every frame: when the edited data changed since the section was
    /// last filled, schedule one refresh [`REFRESH_DELAY`] after the last
    /// change. A burst of edits keeps pushing that back, and the work is
    /// skipped if the data is back where the last run saw it.
    pub(crate) fn watch_validation(&mut self, cx: &mut Context<Self>) {
        let signal = self.validation_signal();
        let Some(panel) = self.validation.as_mut() else {
            return;
        };
        if signal == panel.seen {
            return;
        }
        panel.seen = signal;
        panel.generation += 1;
        let generation = panel.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DELAY).await;
            let _ = this.update(cx, |this, cx| this.refresh_validation(generation, cx));
        })
        .detach();
    }

    /// The scheduled refresh: runs only if no later change replaced it and the
    /// data differs from what the shown report was computed from.
    fn refresh_validation(&mut self, generation: u64, cx: &mut Context<Self>) {
        let signal = self.validation_signal();
        let Some(panel) = self.validation.as_ref() else {
            return;
        };
        if panel.generation != generation || panel.ran_at == signal {
            return;
        }
        self.rerun_validation();
        cx.notify();
    }

    /// A click on row `index`: select the finding's objects and zoom to
    /// them. The list stays where it is.
    pub(crate) fn select_finding(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(panel) = self.validation.as_mut() else {
            return false;
        };
        let Some(entry) = panel.report.entries.get(index) else {
            return false;
        };
        let features = feature_refs(entry);
        panel.current = Some(finding_key(entry));
        self.show_features(&features, cx);
        true
    }

    /// Index of the row to highlight: the clicked finding while its objects
    /// are still what is selected.
    pub(crate) fn highlighted_finding(&self) -> Option<usize> {
        let panel = self.validation.as_ref()?;
        let key = panel.current()?;
        let index = position_of(&panel.report, key)?;
        let refs = &key.1;
        let same =
            refs.len() == self.selected.len() && refs.iter().all(|f| self.selected.contains(f));
        same.then_some(index)
    }

    /// The whole Validation section for the side panel, or `None` when no
    /// validation has been run.
    pub(crate) fn render_validation_section(
        &self,
        list_max_height: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let panel = self.validation.as_ref()?;
        let report = &panel.report;
        let theme = cx.theme().clone();
        let open = self.side_panel_open[SECTION];

        // The counts sit in the header so they show when it is collapsed.
        let counts = if report.is_empty() {
            None
        } else {
            Some(
                Label::new(summary_text(report))
                    .text_size(muted_text_size())
                    .text_color(if report.has_errors() {
                        theme.danger
                    } else {
                        theme.warning
                    }),
            )
        };
        let trailing = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_1()
            .child(div().min_w_0().truncate().children(counts))
            .child(
                Button::new("validation-close")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .tooltip(tr!("validator.close"))
                    .on_click(cx.listener(|this, _, _window, cx| {
                        cx.stop_propagation();
                        this.close_validation(cx);
                    })),
            );

        // Collapsed, only the header (with the counts) is drawn, so the
        // rows are not built.
        let body = if !open {
            div().into_any_element()
        } else if !report.checked_changes {
            note_row(tr!("validator.nothing_pending"), cx).into_any_element()
        } else if report.is_empty() {
            note_row(tr!("validator.none"), cx).into_any_element()
        } else {
            let rows = findings_rows(
                report,
                "validation-row",
                self.highlighted_finding(),
                cx,
                |this: &mut MapViewer, index, cx| {
                    this.select_finding(index, cx);
                },
            );
            div()
                .id("validation-list")
                .flex_shrink(1.)
                .min_h(px(0.))
                .max_h(px(list_max_height))
                .overflow_y_scroll()
                .child(rows)
                .into_any_element()
        };

        // Under the Selection zone and above History: the one zone besides
        // History that gives up height (down to its header) in a short window,
        // its list scrolling inside whatever is left.
        Some(
            self.collapsible_section_with(
                tr!("validator.title"),
                SECTION,
                open,
                Some(trailing.into_any_element()),
                div()
                    .flex()
                    .flex_col()
                    .min_h(px(0.))
                    .child(body)
                    .into_any_element(),
                cx,
            )
            .flex_shrink(1.)
            .min_h(px(32.))
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border),
        )
    }
}
