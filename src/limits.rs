//! The OSM server's size limits (way nodes, relation members, changeset
//! elements) as the editor enforces them: a pre-upload check over the pending
//! diff, and the one-line checks the editing operations make before they
//! would grow a way or relation past the maximum.
//!
//! The limit values come from `capabilities` (the connected server's
//! `/api/capabilities`, or the public-server defaults until that has been
//! fetched). Everything below is pure so it can be tested without a network.

use crate::capabilities;
use crate::layers::diff::LayerDiff;
use crate::tr;

/// The three limits, as one value so the pure checks can be tested with
/// non-default numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub way_nodes: usize,
    pub relation_members: usize,
    pub changeset_elements: usize,
}

impl Limits {
    pub fn for_server(base_url: &str) -> Self {
        Self {
            way_nodes: capabilities::max_way_nodes(base_url),
            relation_members: capabilities::max_relation_members(base_url),
            changeset_elements: capped_changeset_elements(
                capabilities::max_changeset_elements(base_url),
                debug_chunk_limit_env().as_deref(),
            ),
        }
    }
}

/// Debug builds only: the value of `OSM_GPUI_DEBUG_CHUNK_LIMIT`, a cap on how
/// many changes one changeset carries, so a small edit can be sent as several
/// changesets when testing against a real server. Release builds never read it.
fn debug_chunk_limit_env() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        std::env::var("OSM_GPUI_DEBUG_CHUNK_LIMIT").ok()
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// The server's changeset size limit, lowered to `cap` when that parses as a
/// positive number. Never raises the limit; anything else leaves it alone.
fn capped_changeset_elements(server: usize, cap: Option<&str>) -> usize {
    match cap.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(cap) if cap > 0 => server.min(cap),
        _ => server,
    }
}

/// Why a pending upload would be rejected by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitViolation {
    /// A created or modified way has too many nodes. `layer` indexes the
    /// diffs passed to `check_upload`.
    WayTooLong {
        layer: usize,
        id: i64,
        nodes: usize,
        max: usize,
    },
    /// A created or modified relation has too many members.
    RelationTooLarge {
        layer: usize,
        id: i64,
        members: usize,
        max: usize,
    },
}

impl LimitViolation {
    /// The status-bar text for this refusal.
    pub fn message(&self) -> String {
        match *self {
            Self::WayTooLong { id, nodes, max, .. } => tr!(
                "limits.upload.way",
                id = id,
                nodes = group_digits(nodes),
                max = group_digits(max)
            )
            .to_string(),
            Self::RelationTooLarge {
                id, members, max, ..
            } => tr!(
                "limits.upload.relation",
                id = id,
                members = group_digits(members),
                max = group_digits(max)
            )
            .to_string(),
        }
    }
}

/// Check the pending diffs against `limits`. Only created and modified
/// objects are measured: an oversized way or relation that is unchanged is
/// not part of the upload, and deleted ones carry no size. The number of
/// changes is not limited here: more than one changeset holds is uploaded as
/// several (see `upload_chunks`).
pub fn check_upload(diffs: &[&LayerDiff], limits: &Limits) -> Result<(), LimitViolation> {
    for (layer, diff) in diffs.iter().enumerate() {
        for way in diff.created_ways.iter().chain(&diff.modified_ways) {
            if way.nodes.len() > limits.way_nodes {
                return Err(LimitViolation::WayTooLong {
                    layer,
                    id: way.id,
                    nodes: way.nodes.len(),
                    max: limits.way_nodes,
                });
            }
        }
        for relation in diff
            .created_relations
            .iter()
            .chain(&diff.modified_relations)
        {
            if relation.members.len() > limits.relation_members {
                return Err(LimitViolation::RelationTooLarge {
                    layer,
                    id: relation.id,
                    members: relation.members.len(),
                    max: limits.relation_members,
                });
            }
        }
    }
    Ok(())
}

/// Edit-time: the refusal text if a way of `new_len` nodes would exceed the
/// connected server's maximum, with `message` building the text from
/// `(new_len, max)`. `None` means the edit may go ahead.
pub fn way_over_limit(
    new_len: usize,
    message: impl FnOnce(usize, usize) -> String,
) -> Option<String> {
    over(
        new_len,
        capabilities::max_way_nodes(&api_base_url()),
        message,
    )
}

/// Edit-time: as `way_over_limit`, for relation members.
pub fn relation_over_limit(
    new_len: usize,
    message: impl FnOnce(usize, usize) -> String,
) -> Option<String> {
    over(
        new_len,
        capabilities::max_relation_members(&api_base_url()),
        message,
    )
}

fn over(
    new_len: usize,
    max: usize,
    message: impl FnOnce(usize, usize) -> String,
) -> Option<String> {
    (new_len > max).then(|| message(new_len, max))
}

/// "Can't add another node" refusal for a way that is already `max` long.
pub fn way_full_message(_new_len: usize, max: usize) -> String {
    tr!("limits.edit.way_full", max = group_digits(max)).to_string()
}

/// "Can't join" refusal naming the combined size.
pub fn join_message(new_len: usize, max: usize) -> String {
    tr!(
        "limits.edit.join",
        nodes = group_digits(new_len),
        max = group_digits(max)
    )
    .to_string()
}

/// Refusal for adding members to an existing relation.
pub fn relation_add_message(new_len: usize, max: usize) -> String {
    tr!(
        "limits.edit.relation_add",
        members = group_digits(new_len),
        max = group_digits(max)
    )
    .to_string()
}

/// Refusal for creating a relation with too many members.
pub fn relation_new_message(new_len: usize, max: usize) -> String {
    tr!(
        "limits.edit.relation_new",
        members = group_digits(new_len),
        max = group_digits(max)
    )
    .to_string()
}

fn api_base_url() -> String {
    crate::settings_store::api_base_url()
}

/// `1234567` -> "1,234,567" ("1.234.567" in German).
pub fn group_digits(n: usize) -> String {
    group_digits_with(
        n,
        if crate::i18n::current_locale().starts_with("de") {
            '.'
        } else {
            ','
        },
    )
}

fn group_digits_with(n: usize, sep: char) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::{MemberType, OsmMember, OsmRelation, OsmWay};
    use std::collections::HashMap;

    #[test]
    fn chunk_cap_lowers_the_changeset_limit_only() {
        assert_eq!(capped_changeset_elements(10_000, None), 10_000);
        assert_eq!(capped_changeset_elements(10_000, Some("25")), 25);
        assert_eq!(capped_changeset_elements(10_000, Some(" 25 ")), 25);
        // Never raises the server's limit.
        assert_eq!(capped_changeset_elements(100, Some("5000")), 100);
        // Junk and zero are ignored.
        assert_eq!(capped_changeset_elements(10_000, Some("abc")), 10_000);
        assert_eq!(capped_changeset_elements(10_000, Some("0")), 10_000);
        assert_eq!(capped_changeset_elements(10_000, Some("-3")), 10_000);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn release_builds_ignore_the_chunk_limit_variable() {
        assert_eq!(debug_chunk_limit_env(), None);
    }

    fn way(id: i64, len: usize) -> OsmWay {
        OsmWay {
            id,
            version: 1,
            nodes: (1..=len as i64).collect(),
            tags: HashMap::new(),
        }
    }

    fn relation(id: i64, len: usize) -> OsmRelation {
        OsmRelation {
            id,
            version: 1,
            members: (1..=len as i64)
                .map(|n| OsmMember {
                    member_type: MemberType::Node,
                    reference: n,
                    role: String::new(),
                })
                .collect(),
            tags: HashMap::new(),
        }
    }

    fn limits(way_nodes: usize, relation_members: usize, changeset_elements: usize) -> Limits {
        Limits {
            way_nodes,
            relation_members,
            changeset_elements,
        }
    }

    #[test]
    fn ways_under_at_and_over_the_limit() {
        let l = limits(5, 100, 100);
        let diff = |n| LayerDiff {
            created_ways: vec![way(-1, n)],
            ..Default::default()
        };
        assert_eq!(check_upload(&[&diff(4)], &l), Ok(()));
        assert_eq!(check_upload(&[&diff(5)], &l), Ok(()));
        assert_eq!(
            check_upload(&[&diff(6)], &l),
            Err(LimitViolation::WayTooLong {
                layer: 0,
                id: -1,
                nodes: 6,
                max: 5
            })
        );
        // Modified ways are measured too, and the layer index is reported.
        let modified = LayerDiff {
            modified_ways: vec![way(77, 6)],
            ..Default::default()
        };
        assert_eq!(
            check_upload(&[&diff(1), &modified], &l),
            Err(LimitViolation::WayTooLong {
                layer: 1,
                id: 77,
                nodes: 6,
                max: 5
            })
        );
    }

    #[test]
    fn relations_under_at_and_over_the_limit() {
        let l = limits(100, 3, 100);
        let diff = |n, created: bool| {
            if created {
                LayerDiff {
                    created_relations: vec![relation(-9, n)],
                    ..Default::default()
                }
            } else {
                LayerDiff {
                    modified_relations: vec![relation(9, n)],
                    ..Default::default()
                }
            }
        };
        assert_eq!(check_upload(&[&diff(2, true)], &l), Ok(()));
        assert_eq!(check_upload(&[&diff(3, false)], &l), Ok(()));
        assert_eq!(
            check_upload(&[&diff(4, true)], &l),
            Err(LimitViolation::RelationTooLarge {
                layer: 0,
                id: -9,
                members: 4,
                max: 3
            })
        );
        assert!(matches!(
            check_upload(&[&diff(4, false)], &l),
            Err(LimitViolation::RelationTooLarge { id: 9, .. })
        ));
    }

    #[test]
    fn more_changes_than_a_changeset_holds_are_not_refused() {
        // They are split into several changesets instead.
        let l = limits(100, 100, 5);
        let diff = LayerDiff {
            created_ways: (0..20).map(|i| way(-1 - i, 2)).collect(),
            ..Default::default()
        };
        assert_eq!(check_upload(&[&diff], &l), Ok(()));
        assert_eq!(check_upload(&[], &l), Ok(()));
    }

    #[test]
    fn a_way_problem_is_still_reported_when_the_upload_is_large() {
        let l = limits(3, 100, 2);
        let diff = LayerDiff {
            created_ways: vec![way(-1, 9), way(-2, 2), way(-3, 2)],
            ..Default::default()
        };
        assert!(matches!(
            check_upload(&[&diff], &l),
            Err(LimitViolation::WayTooLong { id: -1, .. })
        ));
    }

    #[test]
    fn unmodified_and_deleted_objects_never_block_other_changes() {
        // The oversized way is not in the diff at all (unmodified), so only
        // the small edit is measured.
        let l = limits(5, 5, 100);
        let diff = LayerDiff {
            created_ways: vec![way(-1, 3)],
            deleted_way_ids: vec![(50, 1)],
            deleted_relation_ids: vec![(60, 1)],
            ..Default::default()
        };
        assert_eq!(check_upload(&[&diff], &l), Ok(()));
    }

    #[test]
    fn default_limits_are_the_public_servers() {
        let l = Limits::for_server("https://limits-unfetched.test");
        assert_eq!(l, limits(2000, 32000, 10000));
    }

    #[test]
    fn messages_name_the_object_and_the_counts() {
        let m = LimitViolation::WayTooLong {
            layer: 0,
            id: 123,
            nodes: 2104,
            max: 2000,
        }
        .message();
        assert_eq!(
            m,
            "Way 123 has 2,104 nodes; the server allows 2,000. Split it before uploading."
        );
        let m = LimitViolation::RelationTooLarge {
            layer: 0,
            id: 7,
            members: 40,
            max: 32,
        }
        .message();
        assert!(
            m.contains("Relation 7") && m.contains("40") && m.contains("32"),
            "{m}"
        );
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits_with(0, ','), "0");
        assert_eq!(group_digits_with(999, ','), "999");
        assert_eq!(group_digits_with(1000, '.'), "1.000");
        assert_eq!(group_digits_with(1234567, ','), "1,234,567");
    }

    #[test]
    fn edit_checks_use_strictly_greater() {
        let msg = |n, m| format!("{n}/{m}");
        assert_eq!(over(5, 5, msg), None);
        assert_eq!(over(6, 5, msg), Some("6/5".to_string()));
    }
}
