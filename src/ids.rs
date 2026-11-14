//! Typed OSM element ids. Nodes, ways and relations number their objects
//! separately (node 5 and way 5 are different objects), so each kind gets its
//! own type and a way id cannot be used to look up a node. Ids of objects not
//! yet uploaded are negative.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! element_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        #[repr(transparent)]
        pub struct $name(pub i64);

        impl $name {
            /// The bare number, for the OSM API, file formats and code that
            /// still handles ids of mixed kinds as `i64`.
            #[inline]
            pub const fn get(self) -> i64 {
                self.0
            }

            /// Whether the object has not been uploaded yet (negative id).
            #[inline]
            pub const fn is_new(self) -> bool {
                self.0 < 0
            }

            /// The bare numbers of `ids`, for the planners that still work on
            /// `i64` ids. Each use marks a place not yet converted.
            pub fn to_raw_vec(ids: &[Self]) -> Vec<i64> {
                ids.iter().map(|id| id.0).collect()
            }

            /// The inverse of [`Self::to_raw_vec`].
            pub fn from_raw_vec(ids: &[i64]) -> Vec<Self> {
                ids.iter().copied().map(Self).collect()
            }
        }

        impl From<i64> for $name {
            #[inline]
            fn from(id: i64) -> Self {
                Self(id)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

element_id!(
    /// The id of a node.
    NodeId
);
element_id!(
    /// The id of a way.
    WayId
);
element_id!(
    /// The id of a relation.
    RelationId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_prints_the_bare_number() {
        assert_eq!(NodeId(42).to_string(), "42");
        assert_eq!(WayId(-7).to_string(), "-7");
        assert_eq!(format!("{:>4}", RelationId(5)), "   5");
    }

    #[test]
    fn orders_like_the_number() {
        assert!(NodeId(-2) < NodeId(-1));
        assert!(WayId(-1) < WayId(3));
        let mut ids = vec![RelationId(5), RelationId(-3), RelationId(1)];
        ids.sort();
        assert_eq!(ids, vec![RelationId(-3), RelationId(1), RelationId(5)]);
    }

    #[test]
    fn get_from_and_is_new() {
        assert_eq!(NodeId::from(9).get(), 9);
        assert!(NodeId(-1).is_new());
        assert!(!WayId(0).is_new());
        assert!(!RelationId(1).is_new());
    }

    #[test]
    fn serialises_as_the_bare_number() {
        assert_eq!(serde_json::to_string(&NodeId(-12)).unwrap(), "-12");
        assert_eq!(
            serde_json::to_string(&vec![WayId(1), WayId(2)]).unwrap(),
            "[1,2]"
        );
        let id: RelationId = serde_json::from_str("77").unwrap();
        assert_eq!(id, RelationId(77));
        let old_shape: Vec<i64> = serde_json::from_str("[1,-2]").unwrap();
        let typed: Vec<NodeId> = serde_json::from_str("[1,-2]").unwrap();
        assert_eq!(typed, old_shape.into_iter().map(NodeId).collect::<Vec<_>>());
    }

    #[test]
    fn same_size_as_i64() {
        assert_eq!(std::mem::size_of::<NodeId>(), std::mem::size_of::<i64>());
    }
}
