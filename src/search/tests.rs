use super::*;
use crate::osm::{MemberType, OsmData, OsmMember, OsmNode, OsmRelation, OsmWay};
use crate::selection::FeatureKind::{Node, Relation, Way};
use std::collections::{HashMap, HashSet};

fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn node(id: i64, lat: f64, lon: f64, t: &[(&str, &str)]) -> OsmNode {
    OsmNode {
        id,
        lat,
        lon,
        version: 1,
        tags: tags(t),
    }
}

fn way(id: i64, nodes: &[i64], t: &[(&str, &str)]) -> OsmWay {
    OsmWay {
        id,
        nodes: nodes.to_vec(),
        version: 2,
        tags: tags(t),
    }
}

fn member(t: MemberType, reference: i64, role: &str) -> OsmMember {
    OsmMember {
        member_type: t,
        reference,
        role: role.into(),
    }
}

fn fixture() -> OsmData {
    let mut nodes = HashMap::new();
    for n in [
        node(
            1,
            0.0,
            0.0,
            &[("name", "Starbucks"), ("amenity", "cafe"), ("brand", "S")],
        ),
        node(2, 0.0, 1.0, &[]),
        node(3, 1.0, 1.0, &[]),
        node(
            4,
            5.0,
            5.0,
            &[("name", "Main Street Cafe"), ("amenity", "cafe")],
        ),
        node(5, 1.0, 0.0, &[]),
        node(
            6,
            9.0,
            9.0,
            &[("name", "Müller Straße"), ("addr:housenumber", "12")],
        ),
        node(-1, 2.0, 2.0, &[("shop", "bakery")]),
    ] {
        nodes.insert(n.id, n);
    }
    let mut ways = HashMap::new();
    for w in [
        way(
            10,
            &[1, 2, 3],
            &[("highway", "residential"), ("name", "Main Street")],
        ),
        way(11, &[2, 3, 5, 2], &[("building", "yes")]),
        way(12, &[1, 99], &[("highway", "service")]),
    ] {
        ways.insert(w.id, w);
    }
    OsmData {
        nodes,
        ways,
        relations: vec![OsmRelation {
            id: 20,
            members: vec![
                member(MemberType::Way, 10, "street"),
                member(MemberType::Node, 1, "stop"),
                member(MemberType::Way, 77, ""),
            ],
            version: 1,
            tags: tags(&[("type", "route"), ("route", "bus")]),
        }],
        bounds: Vec::new(),
    }
}

fn run_ctx(q: &str, data: &OsmData, ctx: &SearchContext<'_>) -> Vec<(FeatureKind, i64)> {
    let query = parse(q).unwrap_or_else(|e| panic!("{q:?} should parse: {e}"));
    search(&query, data, ctx)
}

fn run(q: &str) -> Vec<(FeatureKind, i64)> {
    run_ctx(q, &fixture(), &SearchContext::default())
}

fn err(q: &str) -> ParseError {
    parse(q).expect_err(&format!("{q:?} should not parse"))
}

#[test]
fn exact_value_and_presence() {
    assert_eq!(run("amenity=cafe"), vec![(Node, 1), (Node, 4)]);
    assert_eq!(run("name=Starbucks"), vec![(Node, 1)]);
    // Case-insensitive.
    assert_eq!(run("NAME=starbucks"), vec![(Node, 1)]);
    assert_eq!(run("highway=*"), vec![(Way, 10), (Way, 12)], "key present");
    assert!(run("amenity=caf").is_empty(), "= is exact");
}

#[test]
fn absence_forms() {
    // `key=` and `-key=*` both mean the key is absent.
    let a = run("type:node -amenity=*");
    let b = run("type:node amenity=");
    assert_eq!(a, b);
    assert!(!a.contains(&(Node, 1)));
    assert!(a.contains(&(Node, 2)));
}

#[test]
fn wildcards_and_tilde() {
    assert_eq!(run("name=Star*"), vec![(Node, 1)]);
    assert_eq!(run("name=*bucks"), vec![(Node, 1)]);
    assert_eq!(run("name=*Street*"), vec![(Node, 4), (Way, 10)]);
    assert_eq!(run("name=S*s"), vec![(Node, 1)]);
    assert!(run("name=S*x").is_empty());
    // `~` matches anywhere.
    assert_eq!(run("name~tree"), vec![(Node, 4), (Way, 10)]);
    assert_eq!(run("name~Main*Cafe"), vec![(Node, 4)]);
    assert_eq!(run("addr:*=12"), vec![(Node, 6)], "wildcard in the key");
    assert_eq!(
        run("highway~"),
        vec![(Way, 10), (Way, 12)],
        "~ alone is presence"
    );
}

#[test]
fn colon_is_a_substring_test_and_bare_words_search_everything() {
    assert_eq!(run("name:street"), vec![(Node, 4), (Way, 10)]);
    assert_eq!(run("highway:"), vec![(Way, 10), (Way, 12)]);
    assert_eq!(run("starbucks"), vec![(Node, 1)]);
    // A bare word also matches keys.
    assert_eq!(run("brand"), vec![(Node, 1)]);
    assert_eq!(run("BUS"), vec![(Relation, 20)]);
}

#[test]
fn quoting() {
    assert_eq!(run(r#"name="Main Street""#), vec![(Way, 10)]);
    assert_eq!(run(r#""Main Street""#), vec![(Node, 4), (Way, 10)]);
    assert_eq!(run(r#""addr:housenumber"=12"#), vec![(Node, 6)]);
    assert_eq!(run(r#""addr:housenumber":1"#), vec![(Node, 6)]);
    assert_eq!(run(r#"addr:housenumber=12"#), vec![(Node, 6)]);
    assert_eq!(run(r#"name:"Main Street""#), vec![(Node, 4), (Way, 10)]);
    // Quoted text takes `*` and keywords literally.
    assert!(run(r#"name="Star*""#).is_empty());
    assert!(run(r#""new""#).is_empty(), "the word new, not the state");
    // Escapes.
    assert!(parse(r#"name="say \"hi\"""#).is_ok());
}

#[test]
fn unicode_matching() {
    assert_eq!(run("name=müller*"), vec![(Node, 6)]);
    assert_eq!(run("MÜLLER"), vec![(Node, 6)]);
    assert_eq!(run("name:straße"), vec![(Node, 6)]);
    assert_eq!(run("name=*ß*"), vec![(Node, 6)]);
}

#[test]
fn boolean_logic() {
    assert_eq!(run("amenity=cafe name:street"), vec![(Node, 4)]);
    assert_eq!(run("amenity=cafe AND name:street"), vec![(Node, 4)]);
    assert_eq!(run("amenity=cafe & name:street"), vec![(Node, 4)]);
    assert_eq!(
        run("shop=bakery OR building=yes"),
        vec![(Node, -1), (Way, 11)]
    );
    assert_eq!(
        run("shop=bakery | building=yes"),
        vec![(Node, -1), (Way, 11)]
    );
    assert_eq!(run("shop=bakery|building=yes"), vec![(Node, -1), (Way, 11)]);
    // AND binds tighter than OR.
    assert_eq!(
        run("shop=bakery OR building=yes type:node"),
        vec![(Node, -1)],
        "shop=bakery OR (building=yes AND type:node)"
    );
    assert_eq!(
        run("(shop=bakery OR building=yes) type:way"),
        vec![(Way, 11)]
    );
    assert_eq!(run("type:way -highway=*"), vec![(Way, 11)]);
    assert_eq!(run("type:way NOT highway=*"), vec![(Way, 11)]);
    assert_eq!(
        run("type:way -(highway=service OR highway=residential)"),
        vec![(Way, 11)]
    );
    assert_eq!(run("--shop=bakery"), vec![(Node, -1)]);
}

#[test]
fn type_filters() {
    assert_eq!(run("type:way").len(), 3);
    assert_eq!(run("type:relation"), vec![(Relation, 20)]);
    assert_eq!(run("TYPE:Node amenity=cafe"), vec![(Node, 1), (Node, 4)]);
}

#[test]
fn state_filters() {
    assert_eq!(run("new"), vec![(Node, -1)]);
    assert_eq!(run("closed"), vec![(Way, 11)]);
    assert_eq!(
        run("untagged type:node"),
        vec![(Node, 2), (Node, 3), (Node, 5)]
    );
    assert_eq!(run("incomplete"), vec![(Way, 12), (Relation, 20)]);
    assert_eq!(run("id:4"), vec![(Node, 4)]);
    assert_eq!(run("id:-1"), vec![(Node, -1)]);
    assert_eq!(run("id:10"), vec![(Way, 10)]);
    assert_eq!(run("version:2"), vec![(Way, 10), (Way, 11), (Way, 12)]);
    assert_eq!(run("type:node version:2-").len(), 0);
}

#[test]
fn modified_compares_with_the_baseline() {
    let data = fixture();
    let mut original = data.clone();
    original.nodes.remove(&-1);
    original.nodes.get_mut(&4).unwrap().lat = 4.0;
    original
        .ways
        .get_mut(&10)
        .unwrap()
        .tags
        .insert("name".into(), "Old Name".into());
    original.relations[0]
        .tags
        .insert("route".into(), "tram".into());
    let ctx = SearchContext {
        original: Some(&original),
        ..Default::default()
    };
    assert_eq!(
        run_ctx("modified", &data, &ctx),
        vec![(Node, -1), (Node, 4), (Way, 10), (Relation, 20)]
    );
    // Without a baseline only new objects count.
    assert_eq!(run("modified"), vec![(Node, -1)]);
}

#[test]
fn selected_and_find_in_selection() {
    let data = fixture();
    let selected: HashSet<(FeatureKind, i64)> = [(Node, 1), (Node, 2), (Way, 10)].into();
    let ctx = SearchContext {
        selected: Some(&selected),
        ..Default::default()
    };
    assert_eq!(
        run_ctx("selected", &data, &ctx),
        vec![(Node, 1), (Node, 2), (Way, 10)]
    );
    assert!(run("selected").is_empty(), "nothing selected without a set");
    let ctx = SearchContext {
        selected: Some(&selected),
        only_selected: true,
        ..Default::default()
    };
    assert_eq!(
        run_ctx("type:node", &data, &ctx),
        vec![(Node, 1), (Node, 2)]
    );
}

#[test]
fn counts() {
    assert_eq!(run("nodes:3"), vec![(Way, 10)]);
    assert_eq!(run("nodes:2-3"), vec![(Way, 10), (Way, 12)]);
    assert_eq!(run("nodes:4-"), vec![(Way, 11)]);
    assert_eq!(run("nodes:-2"), vec![(Way, 12)]);
    assert_eq!(run("tags:3"), vec![(Node, 1)]);
    assert_eq!(run("tags:0 type:way"), Vec::new());
    assert_eq!(
        run("tags:2-"),
        vec![(Node, 1), (Node, 4), (Node, 6), (Way, 10), (Relation, 20)]
    );
    assert_eq!(run("members:3"), vec![(Relation, 20)]);
    assert_eq!(run("members:1-2"), Vec::new());
}

#[test]
fn child_and_parent() {
    // Children of the street: its nodes.
    assert_eq!(
        run("child name=\"Main Street\" type:node"),
        vec![(Node, 1), (Node, 2), (Node, 3)]
    );
    // Ways that have a cafe among their nodes.
    assert_eq!(
        run("parent amenity=cafe"),
        vec![(Way, 10), (Way, 12), (Relation, 20)]
    );
    // Ways holding a child of a relation (node 1 is also a relation member).
    assert_eq!(
        run("type:way parent child type:relation"),
        vec![(Way, 10), (Way, 12)]
    );
    assert_eq!(run("child type:relation"), vec![(Node, 1), (Way, 10)]);
    assert_eq!(
        run("type:node child(type:way highway=residential)"),
        vec![(Node, 1), (Node, 2), (Node, 3)]
    );
    // Nested.
    assert_eq!(
        run("child child type:relation type:node"),
        vec![(Node, 1), (Node, 2), (Node, 3)]
    );
    // Negated.
    assert_eq!(
        run("type:node -child type:way"),
        vec![(Node, -1), (Node, 4), (Node, 6)]
    );
}

#[test]
fn inview() {
    let data = fixture();
    let ctx = SearchContext {
        view: Some(GeoBounds {
            min_lat: -0.5,
            max_lat: 1.5,
            min_lon: -0.5,
            max_lon: 1.5,
        }),
        ..Default::default()
    };
    assert_eq!(
        run_ctx("inview type:node", &data, &ctx),
        vec![(Node, 1), (Node, 2), (Node, 3), (Node, 5)]
    );
    assert_eq!(
        run_ctx("inview type:way", &data, &ctx),
        vec![(Way, 10), (Way, 11), (Way, 12)]
    );
    assert_eq!(
        run_ctx("inview type:relation", &data, &ctx),
        vec![(Relation, 20)]
    );
    assert!(run("inview").is_empty(), "no view, nothing in view");
}

#[test]
fn parse_errors_have_positions() {
    let e = err("(a b");
    assert_eq!((e.pos, &e.kind), (0, &ParseErrorKind::UnclosedParen));
    let e = err("a b)");
    assert_eq!((e.pos, &e.kind), (3, &ParseErrorKind::UnexpectedCloseParen));
    let e = err(")");
    assert_eq!(e.kind, ParseErrorKind::UnexpectedCloseParen);
    let e = err("a OR");
    assert_eq!((e.pos, &e.kind), (4, &ParseErrorKind::ExpectedExpression));
    let e = err("a |");
    assert_eq!(e.kind, ParseErrorKind::ExpectedExpression);
    let e = err("OR a");
    assert_eq!((e.pos, &e.kind), (0, &ParseErrorKind::ExpectedExpression));
    let e = err("a AND");
    assert_eq!(e.kind, ParseErrorKind::ExpectedExpression);
    let e = err("a -");
    assert_eq!((e.pos, &e.kind), (2, &ParseErrorKind::ExpectedExpression));
    let e = err("NOT");
    assert_eq!(e.kind, ParseErrorKind::ExpectedExpression);
    let e = err("child");
    assert_eq!(e.kind, ParseErrorKind::ExpectedExpression);
    let e = err("()");
    assert_eq!((e.pos, &e.kind), (1, &ParseErrorKind::ExpectedExpression));
    let e = err(r#"name="oops"#);
    assert_eq!((e.pos, &e.kind), (5, &ParseErrorKind::UnterminatedQuote));
    let e = err(r#""""#);
    assert_eq!(e.kind, ParseErrorKind::EmptyQuotes);
    let e = err("=foo");
    assert_eq!(e.kind, ParseErrorKind::MissingKey);
    let e = err(r#""" = x"#);
    assert_eq!(e.kind, ParseErrorKind::EmptyQuotes);
    let e = err(r#"""=x"#);
    assert_eq!(e.kind, ParseErrorKind::MissingKey);
    let e = err("type:wat");
    assert_eq!(
        (e.pos, e.kind),
        (5, ParseErrorKind::UnknownType("wat".into()))
    );
    let e = err("id:abc");
    assert_eq!(e.kind, ParseErrorKind::InvalidNumber("abc".into()));
    let e = err("nodes:5-2");
    assert_eq!(e.kind, ParseErrorKind::InvalidRange("5-2".into()));
    let e = err("nodes:-");
    assert_eq!(e.kind, ParseErrorKind::InvalidNumber("-".into()));
    let e = err("   ");
    assert_eq!(e.kind, ParseErrorKind::Empty);
    let e = err("");
    assert_eq!(e.kind, ParseErrorKind::Empty);
    let e = err(":foo");
    assert_eq!(e.kind, ParseErrorKind::MissingKey);
}

#[test]
fn error_text_is_readable_and_positioned() {
    let e = err("a b)");
    assert_eq!(e.column(), 4);
    assert_eq!(e.to_string(), "at position 4: unexpected ')'");
    // Positions count characters, not bytes.
    let e = err("ü )");
    assert_eq!(e.pos, 2);
}

#[test]
fn odd_input_never_panics() {
    let nasty = [
        "",
        " ",
        "(",
        ")",
        "((",
        "))",
        "()",
        "(()",
        "\"",
        "\"\"",
        "\\",
        "\\\"",
        "-",
        "--",
        "- -",
        "|",
        "||",
        "| |",
        "&",
        "OR",
        "AND",
        "NOT",
        "NOT NOT",
        "child",
        "parent child",
        "=",
        "~",
        ":",
        "::",
        "=~",
        "a=",
        "a~",
        "a:",
        "*",
        "**",
        "a=*",
        "=*",
        "name=\"",
        "name=\"\"",
        "\"\"=\"\"",
        "type:",
        "id:",
        "id:99999999999999999999",
        "nodes:",
        "nodes:1-2-3",
        "nodes:99999999999999999999",
        "version:-",
        "ünï=çødé",
        "日本語",
        "😀",
        "a\u{0}b",
        "name=\u{202e}x",
        "((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((((a",
        "a b c d e f g",
        "child child child child a",
        "a OR OR b",
        "a AND AND b",
        "( ) ( )",
        "name=a=b",
        "a~b~c",
        "\n\t",
        "name=\"a\"b",
        "\"a\"\"b\"",
        "\"a\"b",
        "x:\"",
        "x:\"y",
    ];
    let data = fixture();
    for q in nasty {
        if let Ok(query) = parse(q) {
            let _ = search(&query, &data, &SearchContext::default());
        }
    }
    // Deep nesting is refused rather than overflowing the stack.
    let deep = format!("{}a{}", "(".repeat(5000), ")".repeat(5000));
    assert_eq!(err(&deep).kind, ParseErrorKind::TooDeep);
    let deep = format!("{}a", "-".repeat(5000));
    assert_eq!(err(&deep).kind, ParseErrorKind::TooDeep);
    // A moderately long conjunction is fine.
    let long = vec!["a"; 2000].join(" ");
    assert!(parse(&long).is_ok());
}

#[test]
fn keys_named_like_keywords_still_work() {
    let mut data = fixture();
    data.nodes.get_mut(&2).unwrap().tags = tags(&[("new", "yes"), ("child", "x"), ("OR", "1")]);
    let ctx = SearchContext::default();
    assert_eq!(run_ctx("new=yes", &data, &ctx), vec![(Node, 2)]);
    assert_eq!(run_ctx("child=x", &data, &ctx), vec![(Node, 2)]);
    assert_eq!(run_ctx("OR=1", &data, &ctx), vec![(Node, 2)]);
    assert_eq!(run_ctx("child:x", &data, &ctx), vec![(Node, 2)]);
}

#[test]
fn glob_edge_cases() {
    let g = |pat: &str, text: &str| -> bool {
        let q = parse(&format!("k={pat}")).unwrap();
        let mut d = fixture();
        d.nodes.clear();
        d.ways.clear();
        d.relations.clear();
        d.nodes.insert(1, node(1, 0.0, 0.0, &[("k", text)]));
        !search(&q, &d, &SearchContext::default()).is_empty()
    };
    assert!(g("a*b*c", "aXXbYYc"));
    assert!(!g("a*b*c", "aXXbYY"));
    assert!(g("a*a", "aa"));
    assert!(!g("a*a", "a"), "prefix and suffix must not overlap");
    assert!(g("*", "anything"));
    assert!(g("**x**", "ax"));
    assert!(g("ab*", "ab"));
    assert!(!g("ab*", "a"));
    assert!(g("*ab", "xab"));
    assert!(!g("ab", "abc"));
}

#[test]
fn matching_a_big_layer_is_quick() {
    // Not a benchmark assertion (debug builds are slow): just exercise the
    // path with a large layer so a quadratic slip would time out the suite.
    let mut data = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    for i in 1..=100_000i64 {
        let t: &[(&str, &str)] = if i % 10 == 0 {
            &[("amenity", "cafe"), ("name", "Some Cafe")]
        } else {
            &[]
        };
        data.nodes.insert(i, node(i, 0.0, 0.0, t));
    }
    let q = parse("amenity=cafe").unwrap();
    assert_eq!(search(&q, &data, &SearchContext::default()).len(), 10_000);
}

/// `cargo test --release search::tests::timing -- --ignored --nocapture`
#[test]
#[ignore]
fn timing_on_a_300k_node_layer() {
    let mut data = OsmData {
        nodes: HashMap::new(),
        ways: HashMap::new(),
        relations: Vec::new(),
        bounds: Vec::new(),
    };
    for i in 1..=300_000i64 {
        let t: &[(&str, &str)] = match i % 20 {
            0 => &[("amenity", "cafe"), ("name", "Some Cafe")],
            1 => &[("natural", "tree"), ("species", "Quercus robur")],
            2 => &[("addr:housenumber", "12"), ("addr:street", "Main Street")],
            _ => &[],
        };
        data.nodes.insert(i, node(i, 0.0, 0.0, t));
    }
    for i in 0..50_000i64 {
        let ids: Vec<i64> = (1..=6).map(|k| i * 5 + k).collect();
        data.ways
            .insert(i + 1, way(i + 1, &ids, &[("highway", "residential")]));
    }
    for q in [
        "amenity=cafe",
        "cafe",
        "name:Cafe type:node",
        "highway=residential nodes:6",
        "-name=* type:node tags:0",
        "modified OR new",
        "addr:*=* OR natural=tree",
        "parent amenity=cafe",
    ] {
        let query = parse(q).unwrap();
        let start = std::time::Instant::now();
        let found = search(&query, &data, &SearchContext::default());
        println!("{q:40} {:>8} matches {:?}", found.len(), start.elapsed());
    }
}
