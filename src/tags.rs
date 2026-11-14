//! The tags of one OSM element: a set of key/value pairs with unique keys and
//! no inherent order.
//!
//! The data model has always held tags in a hash map, and everything that
//! needs an order (XML and osmChange output, the tag table, undo snapshots,
//! the clipboard) sorts by key at the point of use. `Tags` keeps exactly that
//! contract: [`Tags::iter`] visits pairs in unspecified order, and
//! [`Tags::sorted`] / [`Tags::to_sorted_vec`] give the deterministic key order.
//! Two `Tags` are equal when they hold the same pairs, whatever order they
//! were built in.
//!
//! Keys and values stay plain `String`s. Rows being edited in the tag table
//! (ordered, possibly with duplicate or empty keys) and lists of tag changes
//! are not an element's tags and keep their own pair-vector shape; convert at
//! the boundary with [`Tags::from_pairs`] and [`Tags::to_sorted_vec`].

use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, SerializeMap, Serializer};
use std::collections::HashMap;
use std::ops::Index;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags(HashMap<String, String>);

impl Tags {
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of `key`, if the element has that tag.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Whether the element has the tag `key=value`.
    #[inline]
    pub fn is(&self, key: &str, value: &str) -> bool {
        self.get(key) == Some(value)
    }

    /// Set `key` to `value`, returning the value it replaced.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> Option<String> {
        self.0.insert(key.into(), value.into())
    }

    /// Remove `key`, returning its value if it was present.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.0.remove(key)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Keep only the pairs for which `keep` returns true.
    pub fn retain(&mut self, mut keep: impl FnMut(&str, &str) -> bool) {
        self.0.retain(|k, v| keep(k, v));
    }

    /// All pairs, in unspecified order. Use [`Tags::sorted`] where the order
    /// is visible (output, display, comparison of ordered lists).
    pub fn iter(&self) -> std::collections::hash_map::Iter<'_, String, String> {
        self.0.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.keys()
    }

    /// All pairs ordered by key.
    pub fn sorted(&self) -> Vec<(&String, &String)> {
        let mut pairs: Vec<_> = self.0.iter().collect();
        pairs.sort();
        pairs
    }

    /// All pairs, owned, ordered by key.
    pub fn to_sorted_vec(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<_> = self.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        pairs.sort();
        pairs
    }

    /// The pairs, owned, ordered by key; consumes `self`.
    pub fn into_sorted_vec(self) -> Vec<(String, String)> {
        let mut pairs: Vec<_> = self.0.into_iter().collect();
        pairs.sort();
        pairs
    }

    /// Build tags from a list of pairs. Where a key repeats, the last value
    /// wins (as when collecting into a map).
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        pairs.into_iter().collect()
    }
}

impl FromIterator<(String, String)> for Tags {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Tags(iter.into_iter().collect())
    }
}

impl<'a> FromIterator<(&'a str, &'a str)> for Tags {
    fn from_iter<I: IntoIterator<Item = (&'a str, &'a str)>>(iter: I) -> Self {
        Tags(
            iter.into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

impl Extend<(String, String)> for Tags {
    fn extend<I: IntoIterator<Item = (String, String)>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl From<Vec<(String, String)>> for Tags {
    fn from(pairs: Vec<(String, String)>) -> Self {
        pairs.into_iter().collect()
    }
}

impl From<Tags> for Vec<(String, String)> {
    /// Sorted by key, so the result is deterministic.
    fn from(tags: Tags) -> Self {
        tags.into_sorted_vec()
    }
}

impl From<&[(&str, &str)]> for Tags {
    fn from(pairs: &[(&str, &str)]) -> Self {
        pairs.iter().copied().collect()
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Tags {
    fn from(pairs: [(&str, &str); N]) -> Self {
        pairs.into_iter().collect()
    }
}

impl<const N: usize> From<&[(&str, &str); N]> for Tags {
    fn from(pairs: &[(&str, &str); N]) -> Self {
        pairs.iter().copied().collect()
    }
}

impl IntoIterator for Tags {
    type Item = (String, String);
    type IntoIter = std::collections::hash_map::IntoIter<String, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Tags {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::hash_map::Iter<'a, String, String>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// `tags["name"]`; panics when the tag is absent, like indexing a map.
impl Index<&str> for Tags {
    type Output = String;
    fn index(&self, key: &str) -> &String {
        &self.0[key]
    }
}

/// Serialises as a plain JSON object with keys in sorted order, the shape a
/// `HashMap<String, String>` had, so the output is deterministic.
impl Serialize for Tags {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (k, v) in self.sorted() {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Tags {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HashMap::<String, String>::deserialize(deserializer).map(Tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_from_arrays_slices_and_pairs() {
        let a = Tags::from([("highway", "residential"), ("name", "Main St")]);
        let slice: &[(&str, &str)] = &[("name", "Main St"), ("highway", "residential")];
        let b = Tags::from(slice);
        let c = Tags::from(&[("highway", "residential"), ("name", "Main St")]);
        let d = Tags::from_pairs(vec![
            ("name".to_string(), "Main St".to_string()),
            ("highway".to_string(), "residential".to_string()),
        ]);
        assert_eq!(a.len(), 2);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
    }

    #[test]
    fn lookup_insert_remove() {
        let mut t = Tags::new();
        assert!(t.is_empty());
        assert_eq!(t.insert("a", "1"), None);
        assert_eq!(t.insert("a", "2"), Some("1".to_string()));
        assert_eq!(t.get("a"), Some("2"));
        assert!(t.contains_key("a"));
        assert!(t.is("a", "2"));
        assert!(!t.is("a", "1"));
        assert!(!t.is("b", "2"));
        assert_eq!(t["a"], "2");
        assert_eq!(t.remove("a"), Some("2".to_string()));
        assert_eq!(t.remove("a"), None);
        assert_eq!(t.get("a"), None);
    }

    #[test]
    fn sorted_iteration_is_by_key() {
        let t = Tags::from([("name", "x"), ("amenity", "cafe"), ("shop", "bakery")]);
        let keys: Vec<&str> = t.sorted().into_iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["amenity", "name", "shop"]);
        assert_eq!(
            t.to_sorted_vec(),
            vec![
                ("amenity".to_string(), "cafe".to_string()),
                ("name".to_string(), "x".to_string()),
                ("shop".to_string(), "bakery".to_string()),
            ]
        );
        assert_eq!(Vec::<(String, String)>::from(t.clone()), t.to_sorted_vec());
        assert_eq!(t.clone().into_sorted_vec(), t.to_sorted_vec());
    }

    #[test]
    fn equality_ignores_insertion_order() {
        let mut a = Tags::new();
        a.insert("a", "1");
        a.insert("b", "2");
        a.insert("c", "3");
        let mut b = Tags::new();
        b.insert("c", "3");
        b.insert("b", "2");
        b.insert("a", "1");
        assert_eq!(a, b);
        b.insert("a", "other");
        assert_ne!(a, b);
    }

    #[test]
    fn duplicate_keys_keep_the_last_value() {
        let t: Tags = vec![
            ("k".to_string(), "first".to_string()),
            ("k".to_string(), "last".to_string()),
        ]
        .into();
        assert_eq!(t.len(), 1);
        assert_eq!(t.get("k"), Some("last"));
    }

    #[test]
    fn iterates_and_retains() {
        let mut t = Tags::from([("a", "1"), ("b", "2"), ("c", "3")]);
        assert_eq!((&t).into_iter().count(), 3);
        t.retain(|k, _| k != "b");
        let mut owned: Vec<_> = t.into_iter().collect();
        owned.sort();
        assert_eq!(
            owned,
            vec![
                ("a".to_string(), "1".to_string()),
                ("c".to_string(), "3".to_string())
            ]
        );
    }

    #[test]
    fn serialises_as_a_sorted_json_object() {
        let t = Tags::from([("name", "Main St"), ("highway", "residential")]);
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"highway":"residential","name":"Main St"}"#
        );
        assert_eq!(serde_json::to_string(&Tags::new()).unwrap(), "{}");
    }

    #[test]
    fn deserialises_the_old_map_shape() {
        let old: HashMap<String, String> = serde_json::from_str(r#"{"b":"2","a":"1"}"#).unwrap();
        let t: Tags = serde_json::from_str(r#"{"b":"2","a":"1"}"#).unwrap();
        assert_eq!(t, old.into_iter().collect::<Tags>());
        assert_eq!(t, Tags::from([("a", "1"), ("b", "2")]));
        let round: Tags = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(round, t);
    }
}
