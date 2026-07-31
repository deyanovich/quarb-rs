//! End-to-end tests: queries run through the engine against
//! text-level documents assembled from producer event streams.

use quarb_text::{Block, Container, TextModel};

fn values(model: &TextModel, query: &str) -> Vec<String> {
    match quarb::run(query, model).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn nodes(model: &TextModel, query: &str) -> Vec<String> {
    let mut got: Vec<String> = match quarb::run(query, model).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| model.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

fn outline() -> TextModel {
    TextModel::build(vec![
        Block::Paragraph {
            text: "Preamble.".into(),
        },
        Block::Heading {
            level: 1,
            lemma: "One".into(),
        },
        Block::Paragraph {
            text: "In one.".into(),
        },
        Block::Heading {
            level: 3,
            lemma: "Deep".into(),
        },
        Block::Paragraph {
            text: "In deep.".into(),
        },
        Block::Heading {
            level: 2,
            lemma: "Mid".into(),
        },
        Block::Paragraph {
            text: "In mid.".into(),
        },
        Block::Heading {
            level: 1,
            lemma: "Two".into(),
        },
        Block::Paragraph {
            text: "In two.".into(),
        },
    ])
}

/// The outline rule: a heading closes every open section at its
/// level or deeper, a skipped level (h1 → h3) nests directly, and
/// pre-heading content belongs to the document root.
#[test]
fn sections_derive_from_flat_headings() {
    let m = outline();
    assert_eq!(values(&m, "/section::lemma"), vec!["One", "Two"]);
    // The skipped-level h3 and the following h2 are siblings under
    // the h1: 2 < 3 closes "Deep", 2 > 1 keeps "One" open.
    assert_eq!(
        nodes(&m, "/section/section"),
        vec!["/section[1]/section[1]", "/section[1]/section[2]"]
    );
    assert_eq!(values(&m, "/section/section::lemma"), vec!["Deep", "Mid"]);
    assert_eq!(values(&m, "/section/section::::level"), vec!["3", "2"]);
    // Pre-heading content is root body, not a section's.
    assert_eq!(values(&m, "/paragraph::"), vec!["Preamble."]);
}

/// Bare `::` is the flattened prose of the subtree, lemma first.
#[test]
fn prose_flattens_with_lemma() {
    let m = outline();
    assert_eq!(
        values(&m, "/section[::lemma = 'Two']::"),
        vec!["Two\nIn two."]
    );
}

/// A heading inside an open container is decorative, not
/// sectioning: it lowers to a paragraph.
#[test]
fn heading_inside_container_lowers_to_paragraph() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Heading {
            level: 2,
            lemma: "Decorative".into(),
        },
        Block::Paragraph {
            text: "Quoted.".into(),
        },
        Block::Close { hypograph: None },
    ]);
    assert_eq!(nodes(&m, "//section"), Vec::<String>::new());
    assert_eq!(
        values(&m, "/blockquote/paragraph::"),
        vec!["Decorative", "Quoted."]
    );
}

/// A blockquote's hypograph is its attribution — a property, and
/// the tail of the flattened prose.
#[test]
fn blockquote_hypograph() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Paragraph {
            text: "Quoted wisdom.".into(),
        },
        Block::Close {
            hypograph: Some("— Sage".into()),
        },
    ]);
    assert_eq!(values(&m, "/blockquote::hypograph"), vec!["— Sage"]);
    assert_eq!(values(&m, "/blockquote::"), vec!["Quoted wisdom.\n— Sage"]);
}

/// Items take their flavor from the enclosing list; ordered items
/// carry taxis from the list's start; a tight item's inline text is
/// its own, a nested list is its child.
#[test]
fn lists_and_items() {
    let m = TextModel::build(vec![
        Block::Open {
            kind: Container::UnorderedList,
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "alpha".into(),
        },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "beta".into(),
        },
        Block::Open {
            kind: Container::UnorderedList,
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "beta-child".into(),
        },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::OrderedList { start: 3 },
            lemma: None,
        },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "third".into(),
        },
        Block::Close { hypograph: None },
        Block::Open {
            kind: Container::Item,
            lemma: None,
        },
        Block::Text {
            text: "fourth".into(),
        },
        Block::Close { hypograph: None },
        Block::Close { hypograph: None },
    ]);
    assert_eq!(
        values(&m, "/unordered-list/unordered-item::"),
        vec!["alpha", "beta\nbeta-child"]
    );
    assert_eq!(
        nodes(&m, "//unordered-item/unordered-list/unordered-item"),
        vec!["/unordered-list/unordered-item[2]/unordered-list/unordered-item"]
    );
    assert_eq!(values(&m, "//ordered-item::taxis"), vec!["3", "4"]);
}

/// Table denormalization: an ordered list with the `<table>` trait,
/// caption as lemma, rows as ordered items (taxis = row number),
/// cells as `Header: value` unordered items, empty cells skipped.
#[test]
fn tables_denormalize_to_nested_lists() {
    let m = TextModel::build(vec![Block::Table {
        lemma: Some("Crew".into()),
        headers: Some(vec!["Name".into(), "Role".into()]),
        rows: vec![
            vec!["Alice".into(), "captain".into()],
            vec!["Bob".into(), "".into()],
        ],
    }]);
    assert_eq!(nodes(&m, "//*<table>"), vec!["/ordered-list"]);
    assert_eq!(values(&m, "/ordered-list::lemma"), vec!["Crew"]);
    assert_eq!(values(&m, "//ordered-item::taxis"), vec!["1", "2"]);
    assert_eq!(
        values(&m, "//unordered-item::"),
        vec!["Name: Alice", "Role: captain", "Name: Bob"]
    );
}

/// A headerless table keeps bare cell text.
#[test]
fn headerless_table_keeps_bare_cells() {
    let m = TextModel::build(vec![Block::Table {
        lemma: None,
        headers: None,
        rows: vec![vec!["Alice".into(), "captain".into()]],
    }]);
    assert_eq!(
        values(&m, "//unordered-item::"),
        vec!["Alice", "captain"]
    );
}

/// Plain text: blank-line-separated paragraphs, each collapsed to
/// one line.
#[test]
fn plain_text_paragraphs() {
    let m = TextModel::parse_plain("One line\nsame paragraph.\n\n   \nSecond paragraph.\n");
    assert_eq!(
        values(&m, "/paragraph::"),
        vec!["One line same paragraph.", "Second paragraph."]
    );
    assert_eq!(nodes(&m, "//section"), Vec::<String>::new());
}
