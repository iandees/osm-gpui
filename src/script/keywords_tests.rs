//! Keeps `parser::KEYWORDS` and `docs/osmscript.md` in step with the parser's
//! dispatch, so the reference cannot silently drift.

use super::parser::{parse, KEYWORDS};
use std::collections::BTreeSet;

const PARSER_SRC: &str = include_str!("parser.rs");
const NOTE_OPS_SRC: &str = include_str!("note_ops.rs");

fn is_unknown_op(line: &str) -> bool {
    parse(line).is_err_and(|e| e.message.starts_with("unknown op"))
}

/// The string-literal patterns of the arms that sit at 8-space indent between
/// `start` and `end` (the arms of the one `match` on the op word).
fn arm_keywords(src: &str, start: &str, end: &str) -> BTreeSet<String> {
    let body = src.split_once(start).expect("match start not found").1;
    let body = body.split_once(end).expect("match end not found").0;
    let mut found = BTreeSet::new();
    for line in body.lines() {
        if !line.starts_with("        \"") {
            continue;
        }
        let Some((pattern, _)) = line.split_once("=>") else {
            continue;
        };
        for alt in pattern.split('|') {
            found.insert(alt.trim().trim_matches('"').to_string());
        }
    }
    found
}

#[test]
fn keywords_has_no_duplicates() {
    let set: BTreeSet<_> = KEYWORDS.iter().collect();
    assert_eq!(set.len(), KEYWORDS.len(), "duplicate entry in KEYWORDS");
}

#[test]
fn every_keyword_reaches_a_parser_arm() {
    for kw in KEYWORDS {
        // A bare keyword is a usage error for most ops and valid for the rest;
        // either way it must not fall through to the unknown-op fallback.
        assert!(!is_unknown_op(kw), "'{kw}' is in KEYWORDS but not parsed");
    }
}

#[test]
fn unknown_words_are_rejected_as_unknown_ops() {
    assert!(is_unknown_op("definitely_not_an_op 1 2"));
}

#[test]
fn keywords_match_the_arms_in_the_parser_source() {
    let mut arms = arm_keywords(
        PARSER_SRC,
        "    match head {\n",
        "        other => super::note_ops::parse_note_op",
    );
    arms.extend(arm_keywords(
        NOTE_OPS_SRC,
        "    let op = match head {",
        "        _ => return None,",
    ));
    let listed: BTreeSet<String> = KEYWORDS.iter().map(|k| k.to_string()).collect();
    assert_eq!(
        arms.difference(&listed).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "parser arms missing from KEYWORDS"
    );
    assert_eq!(
        listed.difference(&arms).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "KEYWORDS entries with no parser arm"
    );
}

/// The op word of each `| \`word ARGS\` | ... |` row in the reference.
fn documented_keywords(doc: &str) -> Vec<String> {
    doc.lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .filter_map(|code| code.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

#[test]
fn reference_documents_exactly_the_parsers_keywords() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/osmscript.md");
    let doc = std::fs::read_to_string(path).expect("docs/osmscript.md");
    let documented = documented_keywords(&doc);
    let unique: BTreeSet<&str> = documented.iter().map(String::as_str).collect();
    assert_eq!(
        unique.len(),
        documented.len(),
        "an op is documented twice in docs/osmscript.md"
    );
    let listed: BTreeSet<&str> = KEYWORDS.iter().copied().collect();
    assert_eq!(
        listed.difference(&unique).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "ops missing from docs/osmscript.md"
    );
    assert_eq!(
        unique.difference(&listed).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "docs/osmscript.md lists ops the parser rejects"
    );
}
