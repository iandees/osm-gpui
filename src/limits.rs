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
            changeset_elements: capabilities::max_changeset_elements(base_url),
        }
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
    /// The diff as a whole has more elements than one changeset may hold.
    TooManyElements { count: usize, max: usize },
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
            Self::TooManyElements { count, max } => tr!(
                "limits.upload.changeset",
                count = group_digits(count),
                max = group_digits(max)
            )
            .to_string(),
        }
    }
}

/// Check the pending diffs against `limits`. Only created and modified
/// objects are measured: an oversized way or relation that is unchanged is
/// not part of the upload, and deleted ones carry no size. The per-object
/// limits are reported before the changeset total, since they name something
/// the user can fix.
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
    let count: usize = diffs
        .iter()
        .map(|d| {
            let (created, modified, deleted) = d.counts();
            created + modified + deleted
        })
        .sum();
    if count > limits.changeset_elements {
        return Err(LimitViolation::TooManyElements {
            count,
            max: limits.changeset_elements,
        });
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
    fn changeset_total_counts_every_layer_and_every_kind() {
        let l = limits(100, 100, 5);
        let layer = |ways: usize, deleted: usize| LayerDiff {
            created_ways: (0..ways as i64).map(|i| way(-1 - i, 2)).collect(),
            deleted_node_ids: (0..deleted as i64).map(|i| (i + 1, 1)).collect(),
            ..Default::default()
        };
        assert_eq!(check_upload(&[&layer(2, 1), &layer(1, 1)], &l), Ok(()));
        assert_eq!(
            check_upload(&[&layer(2, 1), &layer(1, 2)], &l),
            Err(LimitViolation::TooManyElements { count: 6, max: 5 })
        );
        assert_eq!(check_upload(&[], &l), Ok(()));
    }

    #[test]
    fn a_way_problem_is_reported_before_the_changeset_total() {
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
        let m = LimitViolation::TooManyElements {
            count: 12000,
            max: 10000,
        }
        .message();
        assert!(m.contains("12,000") && m.contains("10,000"), "{m}");
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
