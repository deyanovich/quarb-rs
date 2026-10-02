//! The corpus reading's positional index (ruling #62, phase 2) and
//! traits on a quantified group: `//token[::lower = "x"]` answers
//! from the text model's form index, exactly as the walk would, and
//! `(>token){n}<word>` filters a group's matches like a step's
//! trait does.

use quarb::{AstAdapter, QueryResult, Value};

const PROSE: &str = "One two. Three four.\n\nFive six one.\n\nSeven One eight.\n";

fn corpus() -> quarb_text::TextModel {
    let mut m = quarb_text::TextModel::parse_plain(PROSE);
    m.tokenize();
    m
}

fn values(m: &quarb_text::TextModel, q: &str) -> Vec<String> {
    match quarb::run(q, m).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns
            .iter()
            .map(|&n| m.property(n, "").map(|v| v.to_string()).unwrap_or_default())
            .collect(),
    }
}

#[test]
fn the_index_answers_what_the_walk_finds() {
    let m = corpus();
    // The hook is live for a plain word …
    let hits = m.descendants_where(m.root(), "token", "lower", &Value::Str("one".into()));
    assert_eq!(hits.map(|v| v.len()), Some(3));
    // … and declines what the walk's equality reads differently.
    assert!(
        m.descendants_where(m.root(), "token", "lower", &Value::Str("1".into()))
            .is_none()
    );
    assert!(
        m.descendants_where(m.root(), "token", "class", &Value::Str("word".into()))
            .is_none()
    );
    // Through the query: case-folded, counted, scoped to a subtree,
    // and the remaining predicates still apply.
    assert_eq!(values(&m, r#"//token[::lower = "one"] @| count"#), ["3"]);
    assert_eq!(
        values(&m, r#"//token[::lower = "one"]::"#),
        ["One", "one", "One"]
    );
    assert_eq!(
        values(&m, r#"//paragraph[2]//token[::lower = "one"] @| count"#),
        ["1"]
    );
    assert_eq!(
        values(&m, r#"//token[::lower = "one"][::::n > 1] @| count"#),
        ["2"]
    );
    assert_eq!(
        values(&m, r#"//token<word>[::lower = "one"] @| count"#),
        ["3"]
    );
    assert_eq!(
        values(&m, r#"//token[::lower = "nowhere"] @| count"#),
        ["0"]
    );
    // The same answers as the unindexed spelling.
    assert_eq!(
        values(&m, r#"//token[::lower = "one"] | ::::n"#),
        values(&m, r#"//token | [::lower = "one"] | ::::n"#)
    );
}

#[test]
fn a_trait_filters_a_quantified_group() {
    let m = corpus();
    let from_one = |q: &str| values(&m, &format!(r#"//token[::::n = 1] | {q}"#));
    // The second next token of "One" is the period: no word there.
    assert_eq!(from_one("(>token){2}<word>::"), Vec::<String>::new());
    // The hop stops at the sentence (ruling #64): "Three" opens
    // the next one, so the window of three reaches only the period.
    assert_eq!(from_one("(>token){1;3}<word>::"), ["two"]);
    assert_eq!(from_one("(>token){1;3}<punct>::"), ["."]);
    assert_eq!(from_one("\\\\?sentence>sentence/token[1]::"), ["Three"]);
    // Traits then predicates, the step's order.
    assert_eq!(
        from_one(r#"(>token){1;3}<word>[::lower != "two"]::"#),
        Vec::<String>::new()
    );
    // The spelling round-trips through the unparser.
    let defs = quarb::parse_defs("").unwrap();
    let q = r#"//token | (>token){1;3}<word>[::lower != "two"]::"#;
    assert_eq!(quarb::expand(q, &defs).unwrap(), q);
}
