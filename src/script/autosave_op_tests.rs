//! Parser tests for the ops that exercise autosave and crash recovery.

use super::op::*;
use super::parser::parse;

fn op(line: &str) -> Op {
    parse(line).unwrap().remove(0).op
}

#[test]
fn autosave_ops_parse() {
    assert_eq!(op("autosave_now"), Op::AutosaveNow);
    assert_eq!(op("restore_autosave"), Op::RestoreAutosave);
    assert_eq!(op("discard_autosave"), Op::DiscardAutosave);
    assert_eq!(op("crash"), Op::Crash);
    for line in [
        "autosave_now soon",
        "restore_autosave all",
        "discard_autosave 1",
        "crash now",
    ] {
        assert!(parse(line).is_err(), "{line}");
    }
}

#[test]
fn assert_autosave_exists_takes_an_optional_count() {
    assert_eq!(
        op("assert_autosave_exists"),
        Op::AssertAutosaveExists { count: None }
    );
    assert_eq!(
        op("assert_autosave_exists 0"),
        Op::AssertAutosaveExists { count: Some(0) }
    );
    assert_eq!(
        op("assert_autosave_exists 3"),
        Op::AssertAutosaveExists { count: Some(3) }
    );
    assert!(parse("assert_autosave_exists -1").is_err());
    assert!(parse("assert_autosave_exists 1 2").is_err());
}

#[test]
fn the_restore_dialog_can_be_asserted() {
    assert_eq!(
        op("assert_dialog restore"),
        Op::AssertDialog {
            expect: DialogExpect::Restore
        }
    );
    assert!(parse("assert_dialog restore now").is_err());
}
