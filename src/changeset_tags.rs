//! The tags of the changeset(s) an upload creates, and the small pure pieces
//! the upload dialog builds them from: hashtag extraction, the quick-add
//! source values and the list of recent comments.
//!
//! Everything here is pure, so the tags an upload would carry can be checked
//! without a dialog, a server or the settings file.

use crate::CREATED_BY;

/// The longest value the OSM API accepts for a tag, in characters (not
/// bytes). The same limit applies to changeset tags.
pub const MAX_VALUE_CHARS: usize = 255;

/// How many distinct recent changeset comments are remembered.
pub const MAX_RECENT_COMMENTS: usize = 10;

/// Common `source` values offered as quick-add buttons. These are the
/// conventional plain values (see the OSM wiki's `source` key); they are
/// tag values, so they are not translated.
pub const SOURCE_CHIPS: [&str; 5] = [
    "survey",
    "local knowledge",
    "aerial imagery",
    "GPS",
    "street-level imagery",
];

/// What the changeset tags are made from.
#[derive(Debug, Clone, Copy)]
pub struct TagInputs<'a> {
    /// The changeset comment (hashtags in it are also written as `hashtags`).
    pub comment: &'a str,
    /// The sources field, written as `source`.
    pub source: &'a str,
    /// Written as `review_requested=yes` when true.
    pub review_requested: bool,
    /// Names of the visible imagery layers, written as `imagery_used`.
    pub imagery: &'a [String],
    /// The UI language, written as `locale`.
    pub locale: &'a str,
}

/// An ordered list of changeset tags. Built by [`build_changeset_tags`]; the
/// same value is used for every changeset of a split upload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangesetTags {
    pairs: Vec<(String, String)>,
}

impl ChangesetTags {
    /// The tags in the order they are written.
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    /// The value of tag `key`, if it is written.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Just `created_by` and the comment.
impl From<&str> for ChangesetTags {
    fn from(comment: &str) -> Self {
        build_changeset_tags(&TagInputs {
            comment,
            source: "",
            review_requested: false,
            imagery: &[],
            locale: "",
        })
    }
}

impl From<&ChangesetTags> for ChangesetTags {
    fn from(tags: &ChangesetTags) -> Self {
        tags.clone()
    }
}

/// Cut `s` to at most [`MAX_VALUE_CHARS`] characters.
pub fn truncate_value(s: &str) -> String {
    let cut: String = s.chars().take(MAX_VALUE_CHARS).collect();
    cut.trim_end().to_string()
}

/// Whether `s` (as it would be written, trimmed) is longer than the API allows.
pub fn value_too_long(s: &str) -> bool {
    s.trim().chars().count() > MAX_VALUE_CHARS
}

/// How many characters `s` would take as a tag value.
pub fn value_chars(s: &str) -> usize {
    s.trim().chars().count()
}

/// Join `items` with `;`, leaving out the entries that would take the value
/// over [`MAX_VALUE_CHARS`] (whole entries only, never a cut-off one).
fn join_semicolon_capped<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for item in items {
        let extra = item.chars().count() + usize::from(!out.is_empty());
        if out.chars().count() + extra > MAX_VALUE_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(item);
    }
    out
}

/// The `#hashtags` in `comment`, in order of first appearance, without
/// duplicates (compared ignoring case), each with its leading `#`.
///
/// The rule follows iD: a `#` followed by one or more characters that are
/// Unicode letters, digits, `_` or `-` starts a hashtag, which ends at the
/// first other character (space, punctuation). Differences from "any run of
/// non-space characters": trailing punctuation (`#fix,`) is not part of the
/// tag. A `#` with nothing valid after it, or followed only by `-`/`_`, is
/// not a hashtag. Digit-only tags such as `#123` are accepted (iD does not
/// require a letter). A `#` in the middle of a word still starts a hashtag.
pub fn extract_hashtags(comment: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut chars = comment.char_indices();
    while let Some((i, c)) = chars.next() {
        if c != '#' {
            continue;
        }
        let rest = &comment[i + 1..];
        let body: String = rest
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-')
            .collect();
        if !body.chars().any(|ch| ch.is_alphanumeric() || ch == '_') {
            continue;
        }
        // Skip what was consumed so `#a#b` yields both but `#abc` only once.
        for _ in 0..body.chars().count() {
            chars.next();
        }
        let tag = format!("#{body}");
        if !found.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
            found.push(tag);
        }
    }
    found
}

/// Names of imagery layers as written in `imagery_used`: trimmed, without
/// `;` (it separates the names), empty and repeated ones left out.
fn clean_imagery_names(imagery: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in imagery {
        let name = name.replace(';', ",").trim().to_string();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The ordered tags for a changeset:
///
/// 1. `created_by` (always),
/// 2. `comment`,
/// 3. `source`,
/// 4. `imagery_used` (names separated by `;`),
/// 5. `locale`,
/// 6. `hashtags` (from the comment, separated by `;`),
/// 7. `review_requested` = `yes`.
///
/// Empty values are not written. Typed values (comment, source) are cut at
/// [`MAX_VALUE_CHARS`] characters as a last resort (the dialog refuses to
/// upload them that long); the lists drop whole entries instead.
pub fn build_changeset_tags(inputs: &TagInputs) -> ChangesetTags {
    let mut pairs: Vec<(String, String)> = vec![("created_by".to_string(), CREATED_BY.to_string())];
    let mut push = |key: &str, value: String| {
        if !value.is_empty() {
            pairs.push((key.to_string(), value));
        }
    };
    push("comment", truncate_value(inputs.comment.trim()));
    push("source", truncate_value(inputs.source.trim()));
    let names = clean_imagery_names(inputs.imagery);
    push(
        "imagery_used",
        join_semicolon_capped(names.iter().map(String::as_str)),
    );
    push("locale", truncate_value(inputs.locale.trim()));
    let hashtags = extract_hashtags(inputs.comment);
    push(
        "hashtags",
        join_semicolon_capped(hashtags.iter().map(String::as_str)),
    );
    if inputs.review_requested {
        push("review_requested", "yes".to_string());
    }
    ChangesetTags { pairs }
}

/// `source` with `value` added at the end after `; `, unless the field
/// already lists it (compared ignoring case and surrounding spaces). A blank
/// field just becomes `value`.
pub fn add_source_value(source: &str, value: &str) -> String {
    let value = value.trim();
    let trimmed = source.trim().trim_end_matches(';').trim_end();
    if value.is_empty() {
        return source.to_string();
    }
    let present = trimmed
        .split(';')
        .any(|part| part.trim().eq_ignore_ascii_case(value));
    if present {
        return source.to_string();
    }
    if trimmed.is_empty() {
        value.to_string()
    } else {
        format!("{trimmed}; {value}")
    }
}

/// Whether the field lists `value` (see [`add_source_value`]).
pub fn source_has_value(source: &str, value: &str) -> bool {
    source
        .split(';')
        .any(|part| part.trim().eq_ignore_ascii_case(value.trim()))
}

/// Remember `comment` as the most recent one: moved to the front when it was
/// already there (compared after trimming), the list cut to
/// [`MAX_RECENT_COMMENTS`]. A blank comment is not remembered.
pub fn push_recent_comment(recent: &mut Vec<String>, comment: &str) {
    let comment = comment.trim();
    if comment.is_empty() {
        return;
    }
    recent.retain(|c| c.trim() != comment);
    recent.insert(0, comment.to_string());
    recent.truncate(MAX_RECENT_COMMENTS);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>() -> TagInputs<'a> {
        TagInputs {
            comment: "Add a cafe",
            source: "",
            review_requested: false,
            imagery: &[],
            locale: "",
        }
    }

    fn keys(tags: &ChangesetTags) -> Vec<&str> {
        tags.pairs().iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn comment_only_is_what_was_written_before() {
        let tags = build_changeset_tags(&inputs());
        assert_eq!(
            tags.pairs(),
            &[
                ("created_by".to_string(), CREATED_BY.to_string()),
                ("comment".to_string(), "Add a cafe".to_string()),
            ]
        );
        assert_eq!(ChangesetTags::from("Add a cafe"), tags);
    }

    #[test]
    fn every_extra_is_written_in_order() {
        let imagery = vec!["Esri World Imagery".to_string(), "Bing".to_string()];
        let tags = build_changeset_tags(&TagInputs {
            comment: "Map the park #mapathon",
            source: "survey; GPS",
            review_requested: true,
            imagery: &imagery,
            locale: "de",
        });
        assert_eq!(
            keys(&tags),
            [
                "created_by",
                "comment",
                "source",
                "imagery_used",
                "locale",
                "hashtags",
                "review_requested"
            ]
        );
        assert_eq!(tags.get("source"), Some("survey; GPS"));
        assert_eq!(tags.get("imagery_used"), Some("Esri World Imagery;Bing"));
        assert_eq!(tags.get("locale"), Some("de"));
        assert_eq!(tags.get("hashtags"), Some("#mapathon"));
        assert_eq!(tags.get("review_requested"), Some("yes"));
    }

    #[test]
    fn each_extra_alone() {
        let base = inputs();
        let t = build_changeset_tags(&TagInputs {
            source: "survey",
            ..base
        });
        assert_eq!(keys(&t), ["created_by", "comment", "source"]);
        let t = build_changeset_tags(&TagInputs {
            review_requested: true,
            ..base
        });
        assert_eq!(keys(&t), ["created_by", "comment", "review_requested"]);
        let imagery = vec!["Bing".to_string()];
        let t = build_changeset_tags(&TagInputs {
            imagery: &imagery,
            ..base
        });
        assert_eq!(keys(&t), ["created_by", "comment", "imagery_used"]);
        let t = build_changeset_tags(&TagInputs {
            locale: "en",
            ..base
        });
        assert_eq!(keys(&t), ["created_by", "comment", "locale"]);
    }

    #[test]
    fn review_not_requested_writes_nothing() {
        let t = build_changeset_tags(&inputs());
        assert_eq!(t.get("review_requested"), None);
    }

    #[test]
    fn empty_and_blank_values_are_not_written() {
        let imagery = vec![" ".to_string(), String::new()];
        let t = build_changeset_tags(&TagInputs {
            comment: "  ",
            source: "   ",
            review_requested: false,
            imagery: &imagery,
            locale: " ",
        });
        assert_eq!(keys(&t), ["created_by"]);
    }

    #[test]
    fn values_are_trimmed() {
        let t = build_changeset_tags(&TagInputs {
            comment: "  Fix  ",
            source: " survey ",
            ..inputs()
        });
        assert_eq!(t.get("comment"), Some("Fix"));
        assert_eq!(t.get("source"), Some("survey"));
    }

    #[test]
    fn imagery_names_are_cleaned_and_deduplicated() {
        let imagery = vec!["A;B".to_string(), " Bing ".to_string(), "Bing".to_string()];
        let t = build_changeset_tags(&TagInputs {
            imagery: &imagery,
            ..inputs()
        });
        assert_eq!(t.get("imagery_used"), Some("A,B;Bing"));
    }

    #[test]
    fn typed_values_are_cut_at_255_characters_as_a_last_resort() {
        let long = "ä".repeat(300);
        let t = build_changeset_tags(&TagInputs {
            comment: &long,
            source: &long,
            ..inputs()
        });
        assert_eq!(t.get("comment").unwrap().chars().count(), 255);
        assert_eq!(t.get("source").unwrap().chars().count(), 255);
        // Exactly 255 characters is left alone.
        let exact = "x".repeat(255);
        let t = build_changeset_tags(&TagInputs {
            source: &exact,
            ..inputs()
        });
        assert_eq!(t.get("source"), Some(exact.as_str()));
    }

    #[test]
    fn too_long_is_measured_in_characters_after_trimming() {
        assert!(!value_too_long(&format!("  {}  ", "ß".repeat(255))));
        assert!(value_too_long(&"ß".repeat(256)));
        assert_eq!(value_chars(" abc "), 3);
    }

    #[test]
    fn lists_drop_whole_entries_instead_of_cutting_one() {
        let names: Vec<String> = (0..40).map(|i| format!("Imagery number {i:02}")).collect();
        let t = build_changeset_tags(&TagInputs {
            imagery: &names,
            ..inputs()
        });
        let value = t.get("imagery_used").unwrap();
        assert!(value.chars().count() <= 255);
        assert!(value.split(';').all(|n| names.iter().any(|x| x == n)));
        assert!(value.starts_with("Imagery number 00;"));

        let comment: String = (0..60).map(|i| format!("#tag{i:03} ")).collect();
        let t = build_changeset_tags(&TagInputs {
            comment: &comment,
            ..inputs()
        });
        let value = t.get("hashtags").unwrap();
        assert!(value.chars().count() <= 255);
        assert!(value
            .split(';')
            .all(|h| h.len() == 7 && h.starts_with("#tag")));
        // The comment itself is cut at 255 too.
        assert!(t.get("comment").unwrap().chars().count() <= 255);
    }

    #[test]
    fn hashtags_basic() {
        assert_eq!(
            extract_hashtags("Fix roads #hotosm-project-123 and #MissingMaps"),
            ["#hotosm-project-123", "#MissingMaps"]
        );
        assert!(extract_hashtags("no tags here").is_empty());
    }

    #[test]
    fn hashtags_are_unicode_aware_and_stop_at_punctuation() {
        assert_eq!(
            extract_hashtags("Straße #Müllabfuhr, #été! #日本語."),
            ["#Müllabfuhr", "#été", "#日本語"]
        );
        assert_eq!(
            extract_hashtags("(#tag) #a_b #c-d"),
            ["#tag", "#a_b", "#c-d"]
        );
    }

    #[test]
    fn lone_hash_and_dash_only_are_not_hashtags() {
        assert!(extract_hashtags("#").is_empty());
        assert!(extract_hashtags("fix # of lanes").is_empty());
        assert!(extract_hashtags("#-").is_empty());
        assert!(extract_hashtags("## ").is_empty());
        assert!(extract_hashtags("C# is a language").is_empty());
    }

    #[test]
    fn digit_only_hashtags_are_accepted_like_id() {
        assert_eq!(extract_hashtags("see #123"), ["#123"]);
    }

    #[test]
    fn hashtags_are_deduplicated_ignoring_case_and_keep_first_spelling() {
        assert_eq!(
            extract_hashtags("#Foo #bar #foo #FOO #bar"),
            ["#Foo", "#bar"]
        );
    }

    #[test]
    fn adjacent_hashtags_are_separate() {
        assert_eq!(extract_hashtags("#a#b"), ["#a", "#b"]);
        assert_eq!(extract_hashtags("##a"), ["#a"]);
    }

    #[test]
    fn hashtags_come_from_the_comment_only() {
        let t = build_changeset_tags(&TagInputs {
            comment: "plain",
            source: "#survey",
            ..inputs()
        });
        assert_eq!(t.get("hashtags"), None);
    }

    #[test]
    fn source_values_append_with_semicolons_without_duplicates() {
        assert_eq!(add_source_value("", "survey"), "survey");
        assert_eq!(add_source_value("  ", "survey"), "survey");
        assert_eq!(add_source_value("survey", "GPS"), "survey; GPS");
        assert_eq!(add_source_value("survey; GPS", "gps"), "survey; GPS");
        assert_eq!(add_source_value("survey;GPS", "GPS"), "survey;GPS");
        assert_eq!(add_source_value("survey;", "GPS"), "survey; GPS");
        assert_eq!(add_source_value("survey; ", "GPS"), "survey; GPS");
        assert_eq!(add_source_value("Bing", "survey"), "Bing; survey");
        assert!(source_has_value("survey; GPS", "GPS"));
        assert!(!source_has_value("surveys", "survey"));
    }

    #[test]
    fn recent_comments_are_most_recent_first_distinct_and_capped() {
        let mut recent: Vec<String> = Vec::new();
        push_recent_comment(&mut recent, "one");
        push_recent_comment(&mut recent, "two");
        assert_eq!(recent, ["two", "one"]);
        // Repeating moves it to the front instead of adding it again.
        push_recent_comment(&mut recent, " one ");
        assert_eq!(recent, ["one", "two"]);
        // Blank is ignored.
        push_recent_comment(&mut recent, "   ");
        assert_eq!(recent, ["one", "two"]);
        for i in 0..20 {
            push_recent_comment(&mut recent, &format!("c{i}"));
        }
        assert_eq!(recent.len(), MAX_RECENT_COMMENTS);
        assert_eq!(recent[0], "c19");
        assert_eq!(recent[9], "c10");
    }
}
