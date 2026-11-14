//! Modal dialog offered at startup when the previous run left autosaved
//! layers behind (it crashed, was force-quit, or lost power). An in-window
//! modal like `quit_confirm_dialog.rs`.
//!
//! Restore is the default (Enter). Escape is deliberately NOT "discard": it
//! means "ask me next time" and leaves the files where they are. Discarding
//! has to be chosen with its own button.

use gpui::{
    div, prelude::*, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window,
};
use gpui_component::{
    button::{Button, ButtonVariants as _},
    v_flex, ActiveTheme as _,
};

use crate::autosave::{InFlight, UploadMarker};
use crate::ui::modal;
use crate::ui::style::muted_text_size;
use crate::{tr, tr_count};

pub enum DialogEvent {
    /// Load every listed layer back, with its edits pending.
    Restore,
    /// Delete the autosaved files for good.
    Discard,
    /// Keep the files and ask again at the next start (also Escape).
    Later,
}

/// One autosaved layer, as the dialog lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreItem {
    pub layer_name: String,
    pub edits: usize,
    /// When it was saved, seconds since the Unix epoch.
    pub saved_at: u64,
    /// Set when the layer came from a different API server than the one in
    /// use now: the server it came from and the current one.
    pub other_server: Option<(String, String)>,
}

/// "just now", "5 minutes ago", "2 hours ago", "3 days ago".
pub fn age_phrase(now: u64, saved_at: u64) -> String {
    let secs = now.saturating_sub(saved_at);
    match secs {
        0..=59 => tr!("dialog.age.just_now").to_string(),
        60..=3599 => tr_count!("dialog.age.minutes", secs / 60).to_string(),
        3600..=86_399 => tr_count!("dialog.age.hours", secs / 3600).to_string(),
        86_400..=2_591_999 => tr_count!("dialog.age.days", secs / 86_400).to_string(),
        2_592_000..=31_535_999 => tr_count!("dialog.age.months", secs / 2_592_000).to_string(),
        _ => tr_count!("dialog.age.years", secs / 31_536_000).to_string(),
    }
}

impl RestoreItem {
    /// The list line: `Name: 12 edits, 5 minutes ago`.
    pub fn line(&self, now: u64) -> String {
        tr!(
            "dialog.restore.layer",
            layer = self.layer_name,
            edits = tr_count!("dialog.restore.edits", self.edits),
            when = age_phrase(now, self.saved_at)
        )
        .to_string()
    }

    /// The warning shown under the line when the server differs.
    pub fn server_note(&self) -> Option<String> {
        self.other_server.as_ref().map(|(from, current)| {
            tr!(
                "dialog.restore.other_server",
                server = from,
                current = current
            )
            .to_string()
        })
    }
}

/// "3 new nodes, 1 changed way, 1 deleted relation": what a marker's lists
/// hold, or `None` for a marker that lists nothing (an older build's).
pub fn in_flight_summary(flight: &InFlight) -> Option<String> {
    let c = flight.counts();
    let mut parts: Vec<String> = Vec::new();
    for (count, text) in [
        (
            c.created_nodes,
            tr_count!("dialog.restore.in_flight.created_nodes", c.created_nodes),
        ),
        (
            c.created_ways,
            tr_count!("dialog.restore.in_flight.created_ways", c.created_ways),
        ),
        (
            c.created_relations,
            tr_count!(
                "dialog.restore.in_flight.created_relations",
                c.created_relations
            ),
        ),
        (
            c.modified_nodes,
            tr_count!("dialog.restore.in_flight.modified_nodes", c.modified_nodes),
        ),
        (
            c.modified_ways,
            tr_count!("dialog.restore.in_flight.modified_ways", c.modified_ways),
        ),
        (
            c.modified_relations,
            tr_count!(
                "dialog.restore.in_flight.modified_relations",
                c.modified_relations
            ),
        ),
        (
            c.deleted_nodes,
            tr_count!("dialog.restore.in_flight.deleted_nodes", c.deleted_nodes),
        ),
        (
            c.deleted_ways,
            tr_count!("dialog.restore.in_flight.deleted_ways", c.deleted_ways),
        ),
        (
            c.deleted_relations,
            tr_count!(
                "dialog.restore.in_flight.deleted_relations",
                c.deleted_relations
            ),
        ),
    ] {
        if count > 0 {
            parts.push(text.to_string());
        }
    }
    if parts.is_empty() && flight.omitted == 0 {
        return None;
    }
    let separator = tr!("status.merge.separator");
    let mut summary = parts.join(separator.as_ref());
    if flight.omitted > 0 {
        if !summary.is_empty() {
            summary.push(' ');
        }
        summary.push_str(&tr_count!("dialog.restore.in_flight.omitted", flight.omitted));
    }
    Some(summary)
}

/// What to tell the user about a session that died while uploading: that
/// the server may already hold some of these edits, what was being sent, and
/// where to look. The first line is the warning itself; the rest is detail.
pub fn interrupted_upload_lines(marker: &UploadMarker, now: u64) -> Vec<String> {
    let mut lines = vec![tr!("dialog.restore.upload_interrupted").to_string()];
    lines.extend(interrupted_detail_lines(marker, now));
    lines
}

/// The warning once, then the detail of each interrupted upload, oldest
/// first: for the upload dialog when several crashed sessions left one.
pub fn interrupted_uploads_lines(markers: &[UploadMarker], now: u64) -> Vec<String> {
    let mut lines = vec![tr!("dialog.restore.upload_interrupted").to_string()];
    for marker in markers {
        lines.extend(interrupted_detail_lines(marker, now));
    }
    lines
}

fn interrupted_detail_lines(marker: &UploadMarker, now: u64) -> Vec<String> {
    let mut lines = Vec::new();
    if marker.objects > 0 || marker.total > 0 {
        let objects = tr_count!("dialog.restore.upload_objects", marker.objects).to_string();
        let server = marker.api_base.as_str();
        let when = age_phrase(now, marker.started_at);
        lines.push(if marker.total > 1 {
            tr!(
                "dialog.restore.upload_detail_part",
                part = marker.part,
                total = marker.total,
                objects = objects,
                server = server,
                when = when
            )
            .to_string()
        } else {
            tr!(
                "dialog.restore.upload_detail",
                objects = objects,
                server = server,
                when = when
            )
            .to_string()
        });
    }
    if let Some(summary) = in_flight_summary(&marker.in_flight) {
        lines.push(tr!("dialog.restore.upload_contents", summary = summary).to_string());
    }
    if !marker.comment.is_empty() {
        lines.push(tr!("dialog.restore.upload_comment", comment = marker.comment).to_string());
    }
    if let (Some(id), Some(url)) = (marker.changeset_id, marker.changeset_url()) {
        lines.push(tr!("dialog.restore.upload_changeset", id = id, url = url).to_string());
    }
    lines
}

pub struct RestoreDialog {
    items: Vec<RestoreItem>,
    /// One block of lines per crashed session that died mid-upload.
    upload_notices: Vec<Vec<String>>,
    focus_handle: FocusHandle,
}

impl EventEmitter<DialogEvent> for RestoreDialog {}

impl RestoreDialog {
    pub fn new(items: Vec<RestoreItem>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            items,
            upload_notices: Vec::new(),
            focus_handle,
        }
    }

    /// Warn that these sessions died with an upload in flight (see
    /// [`interrupted_upload_lines`]).
    pub fn set_upload_notices(&mut self, notices: Vec<Vec<String>>, cx: &mut Context<Self>) {
        self.upload_notices = notices;
        cx.notify();
    }

    /// The upload warnings the dialog shows.
    pub fn upload_notices(&self) -> &[Vec<String>] {
        &self.upload_notices
    }

    /// The layers the dialog lists.
    pub fn items(&self) -> &[RestoreItem] {
        &self.items
    }

    /// Press Restore.
    pub fn press_restore(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Restore);
    }

    /// Press Discard.
    pub fn press_discard(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Discard);
    }

    /// Press "Ask Me Next Time" (or Escape).
    pub fn press_later(&mut self, cx: &mut Context<Self>) {
        cx.emit(DialogEvent::Later);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match modal::classify_key(ev) {
            modal::ModalKey::Escape => self.press_later(cx),
            modal::ModalKey::Enter => self.press_restore(cx),
            modal::ModalKey::Other => {}
        }
    }
}

impl crate::ui::modal::ModalDialog for RestoreDialog {}

impl Focusable for RestoreDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RestoreDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        let footer = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(
                Button::new("restore-later")
                    .label(tr!("dialog.restore.later"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_later(cx))),
            )
            .child(
                Button::new("restore-discard")
                    .danger()
                    .label(tr!("dialog.restore.discard"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_discard(cx))),
            )
            .child(
                Button::new("restore-confirm")
                    .primary()
                    .label(tr!("dialog.restore.restore"))
                    .on_click(cx.listener(|this, _, _w, cx| this.press_restore(cx))),
            );

        let mut list = v_flex().gap_2().pl_2();
        for item in &self.items {
            let mut entry = v_flex().gap_0p5().child(
                div()
                    .text_color(cx.theme().foreground)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(item.line(now)),
            );
            if let Some(note) = item.server_note() {
                entry = entry.child(
                    div()
                        .text_size(muted_text_size())
                        .text_color(cx.theme().warning)
                        .child(note),
                );
            }
            list = list.child(entry);
        }

        let mut body = v_flex().gap_3();
        for lines in &self.upload_notices {
            let mut notice = v_flex().gap_0p5();
            for (i, line) in lines.iter().enumerate() {
                notice = notice.child(
                    div()
                        .when(i == 0, |d| d.font_weight(gpui::FontWeight::BOLD))
                        .when(i > 0, |d| d.text_size(muted_text_size()))
                        .text_color(cx.theme().warning)
                        .child(line.clone()),
                );
            }
            body = body.child(notice);
        }
        let body = body
            .child(
                div()
                    .text_color(cx.theme().foreground)
                    .child(tr!("dialog.restore.intro")),
            )
            .child(list)
            .child(
                div()
                    .text_size(muted_text_size())
                    .text_color(cx.theme().muted_foreground)
                    .child(tr!("dialog.restore.footnote")),
            );

        let frame = modal::dialog_frame(
            cx,
            gpui::px(520.0),
            tr!("dialog.restore.title"),
            body,
            footer,
        );

        modal::scrim(&self.focus_handle, cx.listener(Self::on_key_down), frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_naturally() {
        let now = 1_000_000;
        assert_eq!(age_phrase(now, now), "just now");
        assert_eq!(age_phrase(now, now - 59), "just now");
        assert_eq!(age_phrase(now, now - 60), "1 minute ago");
        assert_eq!(age_phrase(now, now - 300), "5 minutes ago");
        assert_eq!(age_phrase(now, now - 3600), "1 hour ago");
        assert_eq!(age_phrase(now, now - 7300), "2 hours ago");
        assert_eq!(age_phrase(now, now - 86_400 * 3), "3 days ago");
        assert_eq!(age_phrase(now + 86_400 * 100, now), "3 months ago");
        assert_eq!(age_phrase(now + 86_400 * 400, now), "1 year ago");
        // A clock that went backwards is not an error.
        assert_eq!(age_phrase(now, now + 500), "just now");
    }

    #[test]
    fn a_line_names_the_layer_edits_and_age() {
        let item = RestoreItem {
            layer_name: "Downtown".into(),
            edits: 12,
            saved_at: 1_000_000 - 300,
            other_server: None,
        };
        assert_eq!(item.line(1_000_000), "Downtown: 12 edits, 5 minutes ago");
        assert_eq!(item.server_note(), None);
        let one = RestoreItem { edits: 1, ..item };
        assert_eq!(one.line(1_000_000), "Downtown: 1 edit, 5 minutes ago");
    }

    #[test]
    fn an_interrupted_upload_is_named_with_what_a_person_needs_to_check_it() {
        let marker = UploadMarker {
            format: 1,
            api_base: "https://api.openstreetmap.org".into(),
            comment: "Fix bus stops".into(),
            started_at: 1_000_000 - 300,
            part: 2,
            total: 3,
            objects: 1,
            changeset_id: Some(4242),
            in_flight: InFlight {
                created_nodes: vec![-1, -2, -3],
                modified_ways: vec![7],
                deleted_relations: vec![9],
                ..Default::default()
            },
        };
        let lines = interrupted_upload_lines(&marker, 1_000_000);
        assert!(lines[0].contains("upload was in progress"), "{lines:?}");
        assert!(
            lines[0].contains("may already be on the server"),
            "{lines:?}"
        );
        assert!(lines[0].contains("recent changesets"), "{lines:?}");
        assert_eq!(
            lines[1],
            "It was sending changeset 2 of 3 (1 change) to https://api.openstreetmap.org, 5 minutes ago."
        );
        assert_eq!(
            lines[2],
            "It was sending: 3 new nodes, 1 changed way, 1 deleted relation."
        );
        assert_eq!(lines[3], "Changeset comment: \"Fix bus stops\"");
        assert_eq!(
            lines[4],
            "Changeset 4242: https://www.openstreetmap.org/changeset/4242"
        );
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn the_summary_names_what_was_in_flight_and_what_was_not_listed() {
        assert_eq!(in_flight_summary(&InFlight::default()), None);
        let flight = InFlight {
            created_nodes: vec![-1],
            deleted_nodes: vec![4, 5],
            omitted: 30,
            ..Default::default()
        };
        assert_eq!(
            in_flight_summary(&flight).unwrap(),
            "1 new node, 2 deleted nodes and 30 more that are not listed"
        );
    }

    #[test]
    fn several_interrupted_uploads_share_one_warning_and_each_keeps_its_link() {
        let at = |t: u64, id: u64| UploadMarker {
            api_base: "https://api.openstreetmap.org".into(),
            started_at: t,
            part: 1,
            total: 1,
            objects: 2,
            changeset_id: Some(id),
            ..Default::default()
        };
        let lines = interrupted_uploads_lines(&[at(10, 111), at(20, 222)], 100);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("An upload was in progress"))
                .count(),
            1
        );
        let joined = lines.join("\n");
        assert!(joined.contains("changeset/111"), "{joined}");
        assert!(joined.contains("changeset/222"), "{joined}");
    }

    #[test]
    fn an_interrupted_upload_with_nothing_known_still_warns() {
        let lines = interrupted_upload_lines(&UploadMarker::default(), 1_000_000);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("may already be on the server"));
        // One changeset, no id yet: no "part", no link.
        let single = UploadMarker {
            api_base: "https://x.test".into(),
            objects: 5,
            total: 1,
            part: 1,
            ..Default::default()
        };
        let lines = interrupted_upload_lines(&single, single.started_at);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("It was sending 5 changes to https://x.test"));
    }

    #[test]
    fn a_layer_from_another_server_says_so() {
        let item = RestoreItem {
            layer_name: "Dev test".into(),
            edits: 2,
            saved_at: 0,
            other_server: Some((
                "https://master.apis.dev.openstreetmap.org/api/0.6".into(),
                "https://api.openstreetmap.org/api/0.6".into(),
            )),
        };
        let note = item.server_note().unwrap();
        assert!(note.contains("master.apis.dev.openstreetmap.org"), "{note}");
        assert!(note.contains("https://api.openstreetmap.org"), "{note}");
    }
}
