//! Parser tests for the ops that drive the upload dialog's changeset extras.

use super::op::*;
use super::parser::parse;

fn op(line: &str) -> Op {
    parse(line).unwrap().remove(0).op
}

fn cmd(c: UploadDialogCmd) -> Op {
    Op::UploadDialog(c)
}

#[test]
fn source_and_chip_take_the_rest_of_the_line() {
    assert_eq!(
        op("set_source survey; aerial imagery"),
        cmd(UploadDialogCmd::SetSource("survey; aerial imagery".into()))
    );
    // No text clears the field.
    assert_eq!(
        op("set_source"),
        cmd(UploadDialogCmd::SetSource(String::new()))
    );
    assert_eq!(
        op("press_source_chip street-level imagery"),
        cmd(UploadDialogCmd::PressSourceChip(
            "street-level imagery".into()
        ))
    );
    assert!(parse("press_source_chip").is_err());
}

#[test]
fn switches_want_on_or_off() {
    assert_eq!(
        op("set_review_requested on"),
        cmd(UploadDialogCmd::SetReviewRequested(true))
    );
    assert_eq!(
        op("set_review_requested off"),
        cmd(UploadDialogCmd::SetReviewRequested(false))
    );
    assert_eq!(
        op("clear_imagery_used on"),
        cmd(UploadDialogCmd::SetImageryCleared(true))
    );
    assert!(parse("set_review_requested").is_err());
    assert!(parse("set_review_requested yes").is_err());
    assert!(parse("clear_imagery_used on off").is_err());
}

#[test]
fn recent_comment_ops() {
    assert_eq!(
        op("open_upload_dialog_preview"),
        cmd(UploadDialogCmd::OpenPreview)
    );
    assert!(parse("open_upload_dialog_preview now").is_err());
    assert_eq!(
        op("toggle_recent_comments"),
        cmd(UploadDialogCmd::ToggleRecentComments)
    );
    assert_eq!(
        op("add_recent_comment Add a cafe"),
        cmd(UploadDialogCmd::AddRecentComment("Add a cafe".into()))
    );
    assert!(parse("add_recent_comment").is_err());
    assert_eq!(
        op("pick_recent_comment 2"),
        cmd(UploadDialogCmd::PickRecentComment(2))
    );
    assert!(parse("pick_recent_comment").is_err());
    assert!(parse("pick_recent_comment -1").is_err());
}

#[test]
fn changeset_tag_assertions() {
    assert_eq!(
        op("assert_changeset_tag source survey; GPS"),
        Op::AssertChangesetTag {
            key: "source".into(),
            value: Some("survey; GPS".into())
        }
    );
    assert_eq!(
        op("assert_no_changeset_tag review_requested"),
        Op::AssertChangesetTag {
            key: "review_requested".into(),
            value: None
        }
    );
    assert!(parse("assert_changeset_tag source").is_err());
    assert!(parse("assert_changeset_tag").is_err());
    assert!(parse("assert_no_changeset_tag").is_err());
    assert!(parse("assert_no_changeset_tag a b").is_err());
}

#[test]
fn a_backslash_hash_is_a_literal_hash_not_a_comment() {
    assert_eq!(
        op("set_comment Map the park \\#mapathon # a comment"),
        Op::SetComment {
            text: "Map the park #mapathon".into()
        }
    );
    assert_eq!(
        op("assert_changeset_tag hashtags \\#a;\\#b"),
        Op::AssertChangesetTag {
            key: "hashtags".into(),
            value: Some("#a;#b".into())
        }
    );
    // An unescaped hash still starts a comment.
    assert_eq!(
        op("set_comment keep # drop"),
        Op::SetComment {
            text: "keep".into()
        }
    );
}
