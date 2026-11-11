//! The list of validator findings shown in the upload dialog and in the
//! Validate dialog: errors first, then warnings, each row with its severity,
//! message, the objects it concerns and a Show button. The list scrolls
//! inside a fixed maximum height and renders at most [`MAX_ROWS`] rows.

use gpui::{div, prelude::*, px, Context, Pixels, SharedString};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    v_flex, ActiveTheme as _, Sizable as _,
};

use crate::selection::FeatureRef;
use crate::ui::style::muted_text_size;
use crate::validator::{Entry, Report, Severity};
use crate::{tr, tr_count};

/// The most rows drawn; the rest are summarised as "and N more".
pub const MAX_ROWS: usize = 200;

/// The objects of `entry` as references into its layer.
pub fn feature_refs(entry: &Entry) -> Vec<FeatureRef> {
    entry
        .finding
        .features
        .iter()
        .map(|&(kind, id)| FeatureRef {
            layer_id: entry.layer,
            kind,
            id,
        })
        .collect()
}

/// "2 errors, 3 warnings" (either part is left out when it is zero), or
/// "No problems found." when there is nothing.
pub fn summary_text(report: &Report) -> SharedString {
    match (report.errors(), report.warnings()) {
        (0, 0) => tr!("validator.none"),
        (e, 0) => tr_count!("validator.errors", e),
        (0, w) => tr_count!("validator.warnings", w),
        (e, w) => tr!(
            "validator.summary_both",
            errors = tr_count!("validator.errors", e),
            warnings = tr_count!("validator.warnings", w)
        ),
    }
}

/// How many rows are drawn for `total` findings, and how many are left out.
pub fn rows_shown(total: usize) -> (usize, usize) {
    let shown = total.min(MAX_ROWS);
    (shown, total - shown)
}

/// The scrolling list of findings. `on_show(view, index, cx)` is called when
/// the Show button of finding `index` (into `report.entries`) is pressed.
pub fn findings_list<V: 'static>(
    report: &Report,
    id: &'static str,
    max_height: Pixels,
    cx: &mut Context<V>,
    on_show: fn(&mut V, usize, &mut Context<V>),
) -> impl IntoElement {
    let theme = cx.theme().clone();
    let (shown, hidden) = rows_shown(report.entries.len());
    let mut list = v_flex()
        .id(id)
        .gap_1()
        .max_h(max_height)
        .overflow_y_scroll()
        .pr_1();
    for (index, entry) in report.entries.iter().take(shown).enumerate() {
        let (label, color) = match entry.finding.severity {
            Severity::Error => (tr!("validator.severity.error"), theme.danger),
            Severity::Warning => (tr!("validator.severity.warning"), theme.warning),
        };
        let row = h_flex()
            .gap_2()
            .items_start()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.list_hover.opacity(0.5))
            .child(
                div().w(px(64.0)).flex_shrink_0().child(
                    Label::new(label)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(color),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(entry.finding.message.clone()).text_color(theme.foreground))
                    .child(
                        Label::new(entry.label.clone())
                            .text_size(muted_text_size())
                            .text_color(theme.muted_foreground),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-show-{index}")))
                    .ghost()
                    .xsmall()
                    .label(tr!("validator.show"))
                    .on_click(cx.listener(move |this, _, _window, cx| on_show(this, index, cx))),
            );
        list = list.child(row);
    }
    if hidden > 0 {
        list = list.child(
            Label::new(tr_count!("validator.more", hidden))
                .text_size(muted_text_size())
                .text_color(theme.muted_foreground),
        );
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::LayerId;
    use crate::selection::FeatureKind;
    use crate::validator::{Check, Finding};

    fn report(errors: usize, warnings: usize) -> Report {
        let mut report = Report::default();
        let mut push = |check: Check| {
            report.entries.push(Entry {
                layer: LayerId(3),
                finding: Finding {
                    severity: check.severity(),
                    check,
                    message: "m".into(),
                    features: vec![(FeatureKind::Way, 5), (FeatureKind::Node, 6)],
                },
                label: "l".into(),
            })
        };
        for _ in 0..errors {
            push(Check::WayTooShort);
        }
        for _ in 0..warnings {
            push(Check::UntaggedWay);
        }
        report
    }

    #[test]
    fn summary_names_each_nonzero_count() {
        assert_eq!(summary_text(&report(0, 0)), "No problems found.");
        assert_eq!(summary_text(&report(1, 0)), "1 error");
        assert_eq!(summary_text(&report(2, 0)), "2 errors");
        assert_eq!(summary_text(&report(0, 1)), "1 warning");
        assert_eq!(summary_text(&report(0, 4)), "4 warnings");
        assert_eq!(summary_text(&report(2, 1)), "2 errors, 1 warning");
    }

    #[test]
    fn long_lists_are_cut_at_the_row_cap() {
        assert_eq!(rows_shown(0), (0, 0));
        assert_eq!(rows_shown(MAX_ROWS), (MAX_ROWS, 0));
        assert_eq!(rows_shown(MAX_ROWS + 7), (MAX_ROWS, 7));
    }

    #[test]
    fn a_finding_maps_to_feature_refs_in_its_layer() {
        let r = report(1, 0);
        let refs = feature_refs(&r.entries[0]);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|f| f.layer_id == LayerId(3)));
        assert_eq!((refs[0].kind, refs[0].id), (FeatureKind::Way, 5));
        assert_eq!((refs[1].kind, refs[1].id), (FeatureKind::Node, 6));
    }
}
