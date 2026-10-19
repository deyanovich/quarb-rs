//! Trait declarations: `trait <name>: STEP;` — a node carries the
//! declared trait exactly when it would match STEP as a step of a
//! path, decided at the node. A real trait: it takes part in the
//! trait algebra and `:traits` lists it; an earlier declaration is
//! visible to a later one, never the reverse; a built-in or a
//! container's trait cannot be redefined; an undefined trait still
//! matches nothing.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

const DOC: &str = r#"{"items": [
    {"kind": "book", "pages": 320, "title": "Pride and Prejudice"},
    {"kind": "book", "pages": 48,  "title": "The Whitewash"},
    {"kind": "map",  "pages": 1,   "title": "Jackson's Island"}
]}"#;

fn model(text: &str) -> ModelAdapter<JsonAdapter> {
    let m = ModelAdapter::new(JsonAdapter::parse(DOC).unwrap(), parse_model(text).unwrap());
    m.check_declared_traits().unwrap();
    m
}

fn values(a: &ModelAdapter<JsonAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    }
}

#[test]
fn a_declared_trait_is_decided_at_the_node() {
    let a = model(
        r#"
        trait <entry>: items;
        trait <long>: [::pages > 100];
        trait <titled>: <object>[::title];
        trait <short-book>: <object && !long>[::kind = "book"];
        "#,
    );
    // `entry` by name: the node's own name is what the matcher tests.
    assert_eq!(values(&a, "//*<entry> @| count"), ["1"]);
    // Predicates, trait expressions, and combinations.
    assert_eq!(values(&a, "//*<long>::title"), ["Pride and Prejudice"]);
    assert_eq!(values(&a, "//*<titled> @| count"), ["3"]);
    assert_eq!(values(&a, "//*<short-book>::title"), ["The Whitewash"]);
    // The algebra, and the listing.
    assert_eq!(values(&a, "//*<titled && !long> @| count"), ["2"]);
    assert_eq!(
        values(&a, "/items/0 | :traits"),
        ["object, long, titled"].map(|s| s.to_string())
    );
    // Undefined stays silent.
    assert_eq!(values(&a, "//*<nothing> @| count"), ["0"]);
}

#[test]
fn an_alternation_and_the_implicit_wildcard() {
    let a = model(
        r#"
        trait <leafish>: (pages|title);
        trait <counted>: [::pages];
        "#,
    );
    assert_eq!(values(&a, "//*<leafish> @| count"), ["6"]);
    assert_eq!(values(&a, "//*<counted> @| count"), ["3"]);
}

#[test]
fn order_settles_references() {
    // `b` is declared after `a`, so `a` does not see it.
    let a = model("trait <a>: <b>;\ntrait <b>: [::pages];");
    assert_eq!(values(&a, "//*<a> @| count"), ["0"]);
    assert_eq!(values(&a, "//*<b> @| count"), ["3"]);
    let c = model("trait <b>: [::pages];\ntrait <a>: <b>[::pages > 100];");
    assert_eq!(values(&c, "//*<a> @| count"), ["1"]);
}

#[test]
fn declared_traits_reach_derived_nodes_and_aliases() {
    let a = model(
        r#"
        node /kinds/kind: /items/*::kind | unique;
        trait <k>: <kind>;
        alias <kk> <k>;
        "#,
    );
    assert_eq!(values(&a, "//*<k>::"), ["book", "map"]);
    assert_eq!(values(&a, "//*<kk> @| count"), ["2"]);
}

#[test]
fn refusals() {
    // A period prefix, a duplicate, a bad body, more than one step.
    assert!(parse_model("trait <.x>: (a|b);").is_err());
    assert!(parse_model("trait <x>: (a|b);\ntrait <x>: c;").is_err());
    assert!(parse_model("trait <x>: /a;").is_err());
    assert!(parse_model("trait <x>: a | count;").is_err());
    assert!(parse_model("trait <x>: a/b;").is_err());
    // A positional predicate has no meaning at a single node.
    assert!(parse_model("trait <x>: a[1];").is_err());
    assert!(parse_model("trait <x>: a[2..3];").is_err());
    // A built-in trait, and a derived container's trait.
    let m = ModelAdapter::new(
        JsonAdapter::parse(DOC).unwrap(),
        parse_model("trait <object>: [::pages];").unwrap(),
    );
    assert!(m.check_declared_traits().is_err());
    let m = ModelAdapter::new(
        JsonAdapter::parse(DOC).unwrap(),
        parse_model("node /kinds/kind: /items/*::kind | unique;\ntrait <kind>: [::pages];")
            .unwrap(),
    );
    assert!(m.check_declared_traits().is_err());
}
