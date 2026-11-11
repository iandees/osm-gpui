//! Search by query, in the spirit of JOSM's Edit > Search.
//!
//! A query such as `highway=residential type:way -name` is parsed by
//! [`parse`] into a [`Query`] and run over one layer's data by [`search`],
//! which returns every matching node, way and relation. Nothing in here
//! knows about the UI; the Find dialog and the script harness both call it.
//!
//! Supported syntax (see the table in the Find dialog's hint and in the
//! tests):
//!
//! | Query | Matches |
//! |---|---|
//! | `key=value` | tag `key` has exactly `value` (`*` is a wildcard) |
//! | `key=*` | tag `key` is present |
//! | `key=` , `-key=*` | tag `key` is absent |
//! | `key~pattern` | value contains `pattern` (`*` wildcard) |
//! | `key:value` | value of `key` contains `value` |
//! | `word` | any key or value contains `word` |
//! | `"two words"` | quoted key, value or word |
//! | `a b`, `a OR b`, `a \| b`, `-a`, `NOT a`, `( )` | boolean logic (AND binds tighter than OR) |
//! | `type:node`, `type:way`, `type:relation` | element type |
//! | `modified`, `new`, `selected`, `untagged`, `closed`, `incomplete`, `inview` | state filters |
//! | `id:N`, `version:N` | id / version (`version` takes ranges) |
//! | `nodes:N`, `tags:N`, `members:N` | counts; `N`, `N-M`, `N-`, `-M` |
//! | `child expr`, `parent expr` | children / parents of what `expr` matches |
//!
//! All text comparison is case-insensitive.

mod matcher;
mod parser;
#[cfg(test)]
mod tests;

pub use matcher::{search, GeoBounds, SearchContext};
pub use parser::{parse, ParseError, ParseErrorKind};

use crate::selection::{FeatureKind, FeatureRef};
use std::collections::HashSet;

/// A parsed search expression, ready to run with [`search`].
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub(crate) expr: Expr,
    /// How many `child` / `parent` sub-expressions the query holds.
    pub(crate) structural: usize,
}

/// One node of the expression tree.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    /// Objects that are children (way nodes, relation members) of objects
    /// matching `expr`. `index` names the precomputed result set.
    Child {
        index: usize,
        expr: Box<Expr>,
    },
    /// Objects having a child that matches `expr`.
    Parent {
        index: usize,
        expr: Box<Expr>,
    },
    /// A tag whose key matches `key` and whose value satisfies `value`.
    Tag {
        key: Pat,
        value: ValueTest,
    },
    /// Any key or value contains the text.
    Word(Pat),
    Type(FeatureKind),
    Modified,
    New,
    Selected,
    Untagged,
    Closed,
    Incomplete,
    InView,
    Id(i64),
    Version(Range),
    Nodes(Range),
    Tags(Range),
    Members(Range),
}

/// What a tag's value must satisfy.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ValueTest {
    /// `key=*`: any value.
    Present,
    /// `key=`: no such tag (or an empty value).
    Absent,
    Matches(Pat),
}

/// A case-insensitive text pattern. Text is stored lower-cased.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pat {
    Exact(String),
    Contains(String),
    /// A `*` wildcard pattern; `lead` / `trail` mean the pattern starts /
    /// ends with a `*` (so it is not anchored there).
    Glob {
        parts: Vec<String>,
        lead: bool,
        trail: bool,
    },
}

/// An inclusive count range; `None` is unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Range {
    pub(crate) min: Option<u64>,
    pub(crate) max: Option<u64>,
}

impl Range {
    pub(crate) fn contains(&self, n: u64) -> bool {
        self.min.is_none_or(|m| n >= m) && self.max.is_none_or(|m| n <= m)
    }
}

/// How the search result combines with the current selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// The result becomes the selection.
    #[default]
    Replace,
    /// The result is added to the selection.
    Add,
    /// The result is taken out of the selection.
    Remove,
    /// Only the selected features that match stay selected.
    FindInSelection,
}

impl SearchMode {
    pub const ALL: [SearchMode; 4] = [
        SearchMode::Replace,
        SearchMode::Add,
        SearchMode::Remove,
        SearchMode::FindInSelection,
    ];

    /// The mode after this one, wrapping, for cycling with the keyboard.
    pub fn next(self) -> SearchMode {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The mode before this one, wrapping.
    pub fn prev(self) -> SearchMode {
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// The selection after applying a search `found` to the `current` one in
/// `mode`. Order is kept (current features first, then new ones) and nothing
/// appears twice. For [`SearchMode::FindInSelection`] `found` is expected to
/// already be restricted to the selection, but is intersected regardless.
pub fn combine(mode: SearchMode, current: &[FeatureRef], found: &[FeatureRef]) -> Vec<FeatureRef> {
    match mode {
        SearchMode::Replace => found.to_vec(),
        SearchMode::Add => {
            let mut seen: HashSet<FeatureRef> = current.iter().copied().collect();
            let mut out = current.to_vec();
            for f in found {
                if seen.insert(*f) {
                    out.push(*f);
                }
            }
            out
        }
        SearchMode::Remove => {
            let gone: HashSet<FeatureRef> = found.iter().copied().collect();
            current
                .iter()
                .copied()
                .filter(|f| !gone.contains(f))
                .collect()
        }
        SearchMode::FindInSelection => {
            let keep: HashSet<FeatureRef> = found.iter().copied().collect();
            current
                .iter()
                .copied()
                .filter(|f| keep.contains(f))
                .collect()
        }
    }
}

/// The history of the last few distinct queries, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryHistory {
    entries: Vec<String>,
}

impl QueryHistory {
    pub const CAPACITY: usize = 10;

    /// Remember `query` (trimmed) as the newest entry; blank queries are
    /// ignored and a repeated one moves to the front.
    pub fn push(&mut self, query: &str) {
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        self.entries.retain(|e| e != q);
        self.entries.insert(0, q.to_string());
        self.entries.truncate(Self::CAPACITY);
    }

    /// Newest first.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use crate::layers::LayerId;

    fn f(kind: FeatureKind, id: i64) -> FeatureRef {
        FeatureRef {
            layer_id: LayerId(1),
            kind,
            id,
        }
    }

    #[test]
    fn combine_modes() {
        let cur = [f(FeatureKind::Node, 1), f(FeatureKind::Node, 2)];
        let found = [f(FeatureKind::Node, 2), f(FeatureKind::Way, 9)];
        assert_eq!(combine(SearchMode::Replace, &cur, &found), found.to_vec());
        assert_eq!(
            combine(SearchMode::Add, &cur, &found),
            vec![
                f(FeatureKind::Node, 1),
                f(FeatureKind::Node, 2),
                f(FeatureKind::Way, 9)
            ]
        );
        assert_eq!(
            combine(SearchMode::Remove, &cur, &found),
            vec![f(FeatureKind::Node, 1)]
        );
        assert_eq!(
            combine(SearchMode::FindInSelection, &cur, &found),
            vec![f(FeatureKind::Node, 2)]
        );
    }

    #[test]
    fn mode_cycles_both_ways() {
        assert_eq!(SearchMode::Replace.next(), SearchMode::Add);
        assert_eq!(SearchMode::FindInSelection.next(), SearchMode::Replace);
        assert_eq!(SearchMode::Replace.prev(), SearchMode::FindInSelection);
    }

    #[test]
    fn history_dedupes_caps_and_ignores_blanks() {
        let mut h = QueryHistory::default();
        h.push("  ");
        assert!(h.is_empty());
        h.push("a");
        h.push("b");
        h.push(" a ");
        assert_eq!(h.entries(), ["a", "b"]);
        for i in 0..20 {
            h.push(&format!("q{i}"));
        }
        assert_eq!(h.len(), QueryHistory::CAPACITY);
        assert_eq!(h.entries()[0], "q19");
    }
}
