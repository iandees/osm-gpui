//! Links and Find queries for a tag, behind the right-click menu on the tag
//! table's key and value cells.

const WIKI_BASE: &str = "https://wiki.openstreetmap.org/wiki/";
const TAGINFO_BASE: &str = "https://taginfo.openstreetmap.org/";

/// Percent-encode `s` for a URL path segment, keeping the characters the OSM
/// wiki and taginfo leave readable (`:`, `=`, `_`, ...).
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'=' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The wiki page for a key (`Key:name`), or for a tag (`Tag:key=value`).
pub fn wiki_url(key: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("{WIKI_BASE}{}", encode(&format!("Tag:{key}={v}"))),
        None => format!("{WIKI_BASE}{}", encode(&format!("Key:{key}"))),
    }
}

/// The taginfo page for a key, or for a tag.
pub fn taginfo_url(key: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("{TAGINFO_BASE}tags/{}", encode(&format!("{key}={v}"))),
        None => format!("{TAGINFO_BASE}keys/{}", encode(key)),
    }
}

/// A double-quoted Find term (`\` and `"` escaped).
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The Find query matching every feature with `key` (any value), or with
/// the exact tag `key=value`.
pub fn find_query(key: &str, value: Option<&str>) -> String {
    match value {
        Some(v) => format!("{}={}", quote(key), quote(v)),
        None => format!("{}=*", quote(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{parse, Query};

    #[test]
    fn wiki_urls() {
        assert_eq!(
            wiki_url("highway", None),
            "https://wiki.openstreetmap.org/wiki/Key:highway"
        );
        assert_eq!(
            wiki_url("amenity", Some("cafe")),
            "https://wiki.openstreetmap.org/wiki/Tag:amenity=cafe"
        );
        assert_eq!(
            wiki_url("name", Some("A B/C")),
            "https://wiki.openstreetmap.org/wiki/Tag:name=A%20B%2FC"
        );
    }

    #[test]
    fn taginfo_urls() {
        assert_eq!(
            taginfo_url("addr:street", None),
            "https://taginfo.openstreetmap.org/keys/addr:street"
        );
        assert_eq!(
            taginfo_url("shop", Some("bakery")),
            "https://taginfo.openstreetmap.org/tags/shop=bakery"
        );
    }

    #[test]
    fn find_queries_parse() {
        let tag = find_query("addr:street", Some("Main \"St\" *"));
        assert_eq!(tag, r#""addr:street"="Main \"St\" *""#);
        let _: Query = parse(&tag).unwrap();
        let key = find_query("highway", None);
        assert_eq!(key, r#""highway"=*"#);
        let _: Query = parse(&key).unwrap();
    }
}
