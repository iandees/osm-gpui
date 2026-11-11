//! The list of validator findings shown in the side panel's Validation
//! section: errors first, then warnings, each a compact row with a severity
//! marker, the message and, muted below it, the objects it concerns. Clicking
//! a row selects those objects. The list renders at most [`MAX_ROWS`] rows.

use gpui::{div, prelude::*, px, Context, SharedString};
use gpui_component::{
    h_flex, label::Label, tooltip::Tooltip, v_flex, ActiveTheme as _, Icon, IconName, Sizable as _,
};

use crate::selection::FeatureRef;
use crate::ui::style::{interactive_row, muted_text_size, PANEL_ROW_HEIGHT};
use crate::validator::{Entry, Report, Severity};
use crate::{tr, tr_count};

/// The most rows drawn; the rest are summarised as "and N more".
pub const MAX_ROWS: usize = 200;

/// What identifies a finding across re-runs of the validator: its check and
/// the objects it concerns. (Not the message, which can change as the object
/// does, and not the row position, which shifts as other problems are fixed.)
pub type FindingKey = (&'static str, Vec<FeatureRef>);

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

/// The identity of `entry`, see [`FindingKey`].
pub fn finding_key(entry: &Entry) -> FindingKey {
    (entry.finding.check.id(), feature_refs(entry))
}

/// The position of the finding with identity `key` in `report`, if it is
/// still there.
pub fn position_of(report: &Report, key: &FindingKey) -> Option<usize> {
    report.entries.iter().position(|e| finding_key(e) == *key)
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

/// The rows of the findings in `report`, as a column. `highlighted` is the
/// index of the row to mark as the current one. `on_select(view, index, cx)`
/// is called when row `index` (into `report.entries`) is clicked.
pub fn findings_rows<V: 'static>(
    report: &Report,
    id: &'static str,
    highlighted: Option<usize>,
    cx: &mut Context<V>,
    on_select: fn(&mut V, usize, &mut Context<V>),
) -> gpui::Div {
    let theme = cx.theme().clone();
    let (shown, hidden) = rows_shown(report.entries.len());
    let mut list = v_flex().gap_px();
    for (index, entry) in report.entries.iter().take(shown).enumerate() {
        // The shape differs as well as the colour: a cross in a circle for
        // an error, a triangle for a warning.
        let (icon, color, severity) = match entry.finding.severity {
            Severity::Error => (
                IconName::CircleX,
                theme.danger,
                tr!("validator.severity.error"),
            ),
            Severity::Warning => (
                IconName::TriangleAlert,
                theme.warning,
                tr!("validator.severity.warning"),
            ),
        };
        let full: SharedString =
            format!("{severity}: {}\n{}", entry.finding.message, entry.label).into();
        let row = interactive_row((id, index), highlighted == Some(index), cx)
            .h_auto()
            .min_h(px(PANEL_ROW_HEIGHT))
            .items_start()
            .py_1()
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .pt(px(2.0))
                    .child(Icon::new(icon).xsmall().text_color(color)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    // The message wraps (two lines at most); the object label
                    // is one muted line, cut with an ellipsis. The whole text
                    // is in the tooltip.
                    .child(
                        div()
                            .line_clamp(2)
                            .text_color(theme.foreground)
                            .child(entry.finding.message.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(muted_text_size())
                            .text_color(theme.muted_foreground)
                            .child(entry.label.clone()),
                    ),
            )
            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _window, cx| on_select(this, index, cx)));
        list = list.child(row);
    }
    if hidden > 0 {
        list = list.child(
            div().px_2().child(
                Label::new(tr_count!("validator.more", hidden))
                    .text_size(muted_text_size())
                    .text_color(theme.muted_foreground),
            ),
        );
    }
    list
}

/// A one-line note in the panel's style (the "no problems" message).
pub fn note_row<V: 'static>(text: SharedString, cx: &mut Context<V>) -> gpui::Div {
    h_flex().px_2().py_1().child(
        Label::new(text)
            .text_size(muted_text_size())
            .text_color(cx.theme().muted_foreground),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::LayerId;
    use crate::selection::FeatureKind;
    use crate::validator::{Check, Finding};

    fn report(errors: usize, warnings: usize) -> Report {
        let mut report = Report::default();
        let mut push = |check: Check, id: i64| {
            report.entries.push(Entry {
                layer: LayerId(3),
                finding: Finding {
                    severity: check.severity(),
                    check,
                    message: "m".into(),
                    features: vec![(FeatureKind::Way, id), (FeatureKind::Node, 6)],
                },
                label: "l".into(),
            })
        };
        for i in 0..errors {
            push(Check::WayTooShort, 5 + i as i64);
        }
        for i in 0..warnings {
            push(Check::UntaggedWay, 50 + i as i64);
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
        assert_eq!(tr_count!("validator.more", 7), "…and 7 more not shown");
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

    #[test]
    fn a_finding_is_found_again_by_check_and_features_after_others_go() {
        let before = report(2, 2);
        let key = finding_key(&before.entries[3]);
        assert_eq!(key.0, "untagged_way");
        assert_eq!(position_of(&before, &key), Some(3));
        // Both errors are fixed: the row moves up but is still the same one.
        let after = report(0, 2);
        assert_eq!(position_of(&after, &key), Some(1));
        // Fixing the finding itself removes it.
        let gone = report(2, 1);
        assert_eq!(position_of(&gone, &key), None);
    }
}
