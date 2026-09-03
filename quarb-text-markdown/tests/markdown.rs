//! End-to-end tests: queries run through the engine against a
//! Markdown document lowered to the text level natively (no HTML
//! round-trip).

use quarb_text::TextModel;

const DOC: &str = "Preamble.

# Guide

Intro paragraph with *emphasis* and `code`.

## Usage

> Quoted wisdom.

- alpha
- beta
  - beta-child

3. third
4. fourth

```rust
fn main() {}
```

| Name | Role |
| ---- | ---- |
| Alice | captain |
| Bob | |

Text after.
";

fn model() -> TextModel {
    quarb_text_markdown::parse(DOC)
}

fn values(query: &str) -> Vec<String> {
    let model = model();
    match quarb::run(query, &model).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

fn nodes(query: &str) -> Vec<String> {
    let model = model();
    let mut got: Vec<String> = match quarb::run(query, &model).unwrap() {
        quarb::QueryResult::Nodes(ns) => ns.into_iter().map(|n| model.locator(n)).collect(),
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    };
    got.sort();
    got
}

/// Heading levels come straight from the source; the section tree
/// derives; pre-heading content is root body.
#[test]
fn sections_derive() {
    assert_eq!(values("/paragraph::"), vec!["Preamble."]);
    assert_eq!(values("/section::lemma"), vec!["Guide"]);
    assert_eq!(values("/section/section::lemma"), vec!["Usage"]);
    assert_eq!(values("/section/section::::level"), vec!["2"]);
    // Everything after the h2 belongs to Usage — including the
    // trailing paragraph.
    assert!(
        values("/section/section::")
            .remove(0)
            .ends_with("Text after.")
    );
}

/// Inline markup flattens to its text.
#[test]
fn inline_flattens() {
    assert_eq!(
        values("/section/paragraph::"),
        vec!["Intro paragraph with emphasis and code."]
    );
}

/// A quote block mounts as a blockquote with paragraph children.
#[test]
fn blockquote() {
    assert_eq!(values("//blockquote/paragraph::"), vec!["Quoted wisdom."]);
}

/// Tight items carry their own text; nesting is real; the ordered
/// list keeps its source start.
#[test]
fn lists() {
    // Anchored under the section: the table's derived cell lists
    // sit deeper (under ordered items) and stay out of scope.
    assert_eq!(
        values("/section/section/unordered-list/unordered-item::"),
        vec!["alpha", "beta\nbeta-child"]
    );
    assert_eq!(
        nodes("//unordered-item/unordered-list/unordered-item").len(),
        1
    );
    // The ol starts at 3; the table's rows follow at 1, 2.
    assert_eq!(values("//ordered-item::taxis"), vec!["3", "4", "1", "2"]);
}

/// A fenced block becomes verbatim with its fence language, kept
/// as authored.
#[test]
fn verbatim() {
    assert_eq!(values("//verbatim::::lang"), vec!["rust"]);
    assert_eq!(values("//verbatim::"), vec!["fn main() {}"]);
}

/// The pipe table denormalizes with its syntactic header row;
/// Bob's empty cell is skipped.
#[test]
fn table_denormalizes() {
    assert_eq!(nodes("//*<table>").len(), 1);
    assert_eq!(
        values("//*<table>//unordered-item::"),
        vec!["Name: Alice", "Role: captain", "Name: Bob"]
    );
}

/// The round trip: render the model to Markdown, re-parse it, and
/// the text-level reading is unchanged (the one deliberate loss is
/// the `<table>` trait — a rendered table IS a nested list).
#[test]
fn markdown_round_trips() {
    use quarb::AstAdapter as _;
    let m = model();
    let md = quarb_text::render_node(&m, m.root(), quarb_text::Render::Markdown);
    let m2 = quarb_text_markdown::parse(&md);
    for q in [
        "//section::lemma",
        "//paragraph::",
        "//ordered-item::taxis",
        "//unordered-item::",
        "::",
    ] {
        let a = quarb::run(q, &m).unwrap();
        let b = quarb::run(q, &m2).unwrap();
        let show = |r: quarb::QueryResult| match r {
            quarb::QueryResult::Values(vs) => {
                vs.iter().map(|v| v.to_string()).collect::<Vec<_>>()
            }
            quarb::QueryResult::Nodes(_) => panic!("expected values"),
        };
        assert_eq!(show(a), show(b), "round trip diverged on {q}");
    }
}

/// The markdown footnote extension ([^name] callouts, [^name]:
/// bodies) lands in the shared apparatus: footnote-family nodes
/// both ends, clean prose, ->footnote edges (ruling #35's
/// recorded markdown amendment).
#[test]
fn footnotes_join_the_apparatus() {
    let m = quarb_text_markdown::parse(
        "# The war\n\nThe emus advanced.[^count] They kept coming.\n\n\
         [^count]: Twenty thousand of them.\n",
    );
    let q = |q: &str| -> Vec<String> {
        match quarb::run(q, &m).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| m.locator(*n)).collect(),
        }
    };
    assert_eq!(q("//footnote @| count"), ["2"]);
    assert_eq!(q("//*<deixis>->footnote::"), ["Twenty thousand of them."]);
    assert_eq!(
        q("//section/paragraph::"),
        ["The emus advanced. They kept coming."]
    );
    assert_eq!(q(r"//*<note><-footnote\*::"), ["The emus advanced. They kept coming."]);
    assert_eq!(q("//*<dangling> @| count"), ["0"]);
}

#[test]
fn pandoc_bracketed_citations() {
    let model = quarb_text_markdown::parse(
        "The classic treatment [@knuth84] holds; compare\n\
         [see @lamport94, pp. 3-5; -@knuth84].\n\n\
         Mail [x@y.com] or ping @someone about it.\n",
    );
    let vals = |q: &str| -> Vec<String> {
        match quarb::run(q, &model).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
        }
    };
    // Every bracketed @key is a cit mark — the locator, prefix,
    // and author-suppressed forms included; keys repeat as cited.
    assert_eq!(vals("//cit::target"), ["knuth84", "lamport94", "knuth84"]);
    // The bare narrative @key is deliberately not read: an email
    // in brackets and an @mention stay prose.
    assert_eq!(vals("//cit @| count"), ["3"]);
    // The bracket text stays in the prose as authored.
    assert_eq!(
        vals("//paragraph[2]::"),
        ["Mail [x@y.com] or ping @someone about it."]
    );
}
