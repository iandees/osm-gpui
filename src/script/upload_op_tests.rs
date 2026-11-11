//! Parser tests for the ops that drive an upload (see `docs/live-test/`).

use super::op::*;
use super::parser::parse;
use std::time::Duration;

fn op(line: &str) -> Op {
    parse(line).unwrap().remove(0).op
}

#[test]
fn type_keeps_inner_spacing_and_punctuation() {
    assert_eq!(
        op("type osm-gpui live test:  roundtrip & \"more\" <ok>"),
        Op::Type {
            text: "osm-gpui live test:  roundtrip & \"more\" <ok>".into()
        }
    );
    assert!(parse("type").is_err());
    assert!(parse("type   ").is_err());
}

#[test]
fn set_tag_takes_the_rest_of_the_line_as_the_value() {
    assert_eq!(
        op("set_tag name  A & B <C> \"D\" caf\u{e9} \u{65e5}\u{672c}"),
        Op::SetTag {
            key: "name".into(),
            value: "A & B <C> \"D\" caf\u{e9} \u{65e5}\u{672c}".into()
        }
    );
    assert!(parse("set_tag name").is_err());
    assert!(parse("set_tag").is_err());
    assert_eq!(op("remove_tag name"), Op::RemoveTag { key: "name".into() });
    assert!(parse("remove_tag a b").is_err());
}

#[test]
fn comments_still_end_a_text_argument() {
    assert_eq!(
        op("type hello # not typed"),
        Op::Type {
            text: "hello".into()
        }
    );
}

#[test]
fn upload_buttons() {
    assert_eq!(op("upload_confirm"), Op::UploadConfirm);
    assert!(parse("upload_confirm now").is_err());
    for (word, choice) in [
        ("keep_mine", ConflictChoice::KeepMine),
        ("use_theirs", ConflictChoice::UseTheirs),
        ("drop_mine", ConflictChoice::DropMine),
        ("show", ConflictChoice::Show),
    ] {
        assert_eq!(
            op(&format!("resolve_conflict {word}")),
            Op::ResolveConflict { choice }
        );
    }
    assert!(parse("resolve_conflict nope").is_err());
    assert!(parse("resolve_conflict").is_err());
}

#[test]
fn transfer_waits() {
    assert_eq!(
        op("wait_transfer"),
        Op::WaitTransfer {
            timeout: Duration::from_secs(120)
        }
    );
    assert_eq!(
        op("wait_transfer 30s"),
        Op::WaitTransfer {
            timeout: Duration::from_secs(30)
        }
    );
    assert!(parse("wait_transfer soon").is_err());
    assert_eq!(
        op("capture_while_transferring out/p every=250ms timeout=20s"),
        Op::CaptureWhileTransferring {
            prefix: "out/p".into(),
            every: Duration::from_millis(250),
            timeout: Duration::from_secs(20)
        }
    );
    assert!(parse("capture_while_transferring").is_err());
    assert!(parse("capture_while_transferring p speed=3").is_err());
}

#[test]
fn assertions() {
    assert_eq!(op("assert_pending 0"), Op::AssertPending { count: 0 });
    assert_eq!(op("assert_pending 12"), Op::AssertPending { count: 12 });
    assert!(parse("assert_pending").is_err());
    assert!(parse("assert_pending -1").is_err());
    assert_eq!(
        op("assert_status_contains Uploaded changes (changeset"),
        Op::AssertStatusContains {
            text: "Uploaded changes (changeset".into()
        }
    );
    assert!(parse("assert_status_contains").is_err());
    assert_eq!(
        op("assert_selection way"),
        Op::AssertSelection {
            kind: SelectionKind::Way
        }
    );
    assert_eq!(
        op("assert_selection multiple"),
        Op::AssertSelection {
            kind: SelectionKind::Multiple
        }
    );
    assert!(parse("assert_selection two").is_err());
}

#[test]
fn dialog_expectations() {
    let expect = |line: &str| match op(line) {
        Op::AssertDialog { expect } => expect,
        other => panic!("{other:?}"),
    };
    assert_eq!(expect("assert_dialog none"), DialogExpect::None);
    assert_eq!(expect("assert_dialog editing"), DialogExpect::Editing);
    assert_eq!(expect("assert_dialog in_flight"), DialogExpect::InFlight);
    assert_eq!(expect("assert_dialog failed"), DialogExpect::Failed);
    assert_eq!(expect("assert_dialog sign_in"), DialogExpect::SignIn);
    assert_eq!(expect("assert_dialog notice"), DialogExpect::Notice);
    assert_eq!(
        expect("assert_dialog conflict"),
        DialogExpect::Conflict {
            kind: None,
            object: None
        }
    );
    assert_eq!(
        expect("assert_dialog conflict version_mismatch node"),
        DialogExpect::Conflict {
            kind: Some(ConflictKindName::VersionMismatch),
            object: Some(FeatureKind::Node)
        }
    );
    assert_eq!(
        expect("assert_dialog conflict already_deleted"),
        DialogExpect::Conflict {
            kind: Some(ConflictKindName::AlreadyDeleted),
            object: None
        }
    );
    assert!(parse("assert_dialog").is_err());
    assert!(parse("assert_dialog popup").is_err());
    assert!(parse("assert_dialog none extra").is_err());
    assert!(parse("assert_dialog conflict sideways").is_err());
}

#[test]
fn export_and_sign_in() {
    assert_eq!(
        op("export_layer out/stale.osm"),
        Op::ExportLayer {
            path: "out/stale.osm".into()
        }
    );
    assert!(parse("export_layer").is_err());
    assert_eq!(op("sign_in"), Op::SignIn);
    assert_eq!(op("assert_signed_in"), Op::AssertSignedIn);
    assert_eq!(
        op("wait_signed_in"),
        Op::WaitSignedIn {
            timeout: Duration::from_secs(200)
        }
    );
    assert_eq!(
        op("wait_signed_in 90s"),
        Op::WaitSignedIn {
            timeout: Duration::from_secs(90)
        }
    );
    assert!(parse("wait_signed_in 5m").is_err());
}

#[test]
fn comment_and_cancel() {
    assert_eq!(
        op("set_comment osm-gpui live test:  a & b"),
        Op::SetComment {
            text: "osm-gpui live test:  a & b".into()
        }
    );
    assert!(parse("set_comment").is_err());
    assert_eq!(op("upload_cancel"), Op::UploadCancel);
    assert!(parse("upload_cancel now").is_err());
}

#[test]
fn delete_layer_dialog_ops() {
    assert_eq!(op("confirm_delete_layer"), Op::ConfirmDeleteLayer);
    assert_eq!(op("cancel_delete_layer"), Op::CancelDeleteLayer);
    assert!(parse("confirm_delete_layer now").is_err());
    assert!(parse("cancel_delete_layer now").is_err());
    assert_eq!(
        op("assert_dialog delete_layer"),
        Op::AssertDialog {
            expect: DialogExpect::DeleteLayer
        }
    );
    assert!(parse("assert_dialog delete_layer extra").is_err());
}

#[test]
fn validation_ops() {
    assert_eq!(
        op("assert_validation errors=2 warnings=0 crossing_ways=1"),
        Op::AssertValidation {
            errors: Some(2),
            warnings: Some(0),
            checks: vec![("crossing_ways".into(), 1)]
        }
    );
    assert_eq!(
        op("assert_validation warnings=3"),
        Op::AssertValidation {
            errors: None,
            warnings: Some(3),
            checks: vec![]
        }
    );
    assert!(parse("assert_validation").is_err());
    assert!(parse("assert_validation errors").is_err());
    assert!(parse("assert_validation errors=x").is_err());
    assert!(parse("assert_validation not_a_check=1").is_err());

    assert_eq!(
        op("show_finding 2"),
        Op::ShowFinding {
            target: FindingTarget::Index(2)
        }
    );
    assert_eq!(
        op("show_finding crossing_ways"),
        Op::ShowFinding {
            target: FindingTarget::Check("crossing_ways".into())
        }
    );
    assert!(parse("show_finding").is_err());
    assert!(parse("show_finding nope").is_err());
    assert!(parse("show_finding 1 2").is_err());
    assert_eq!(
        op("assert_validation_panel shown"),
        Op::AssertValidationPanel { shown: true }
    );
    assert_eq!(
        op("assert_validation_panel hidden"),
        Op::AssertValidationPanel { shown: false }
    );
    assert!(parse("assert_validation_panel").is_err());
    assert!(parse("assert_validation_panel open").is_err());
    assert!(parse("assert_validation_panel shown hidden").is_err());
    // The modal Validate dialog is gone.
    assert!(parse("assert_dialog validate").is_err());
}

#[test]
fn find_dialog_ops() {
    assert_eq!(
        op("assert_dialog find"),
        Op::AssertDialog {
            expect: DialogExpect::Find
        }
    );
    assert_eq!(
        op("assert_selected_count 12"),
        Op::AssertSelectedCount { count: 12 }
    );
    assert!(parse("assert_selected_count").is_err());
    assert!(parse("assert_selected_count -1").is_err());
    assert_eq!(
        op("assert_find_preview 3 matches"),
        Op::AssertFindPreview {
            text: "3 matches".into()
        }
    );
    assert!(parse("assert_find_preview").is_err());
}

#[test]
fn assert_server() {
    assert_eq!(op("assert_server dev"), Op::AssertServerDev);
    assert!(parse("assert_server").is_err());
    assert!(parse("assert_server primary").is_err());
}

#[test]
fn pause_with_and_without_a_duration() {
    assert_eq!(
        op("pause Drawing the way"),
        Op::Pause {
            duration: Duration::from_secs(2),
            message: "Drawing the way".into()
        }
    );
    assert_eq!(
        op("pause 500ms Drawing the way"),
        Op::Pause {
            duration: Duration::from_millis(500),
            message: "Drawing the way".into()
        }
    );
    assert!(parse("pause").is_err());
    assert!(parse("pause 3s").is_err());
}
