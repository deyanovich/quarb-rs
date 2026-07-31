//! Rendering the text vocabulary back to markup.

use quarb::AstAdapter;
use quarb_text::{Block, Container, Render, TextModel, render_node};

fn doc() -> TextModel {
    TextModel::build(vec![
        Block::Heading {
            level: 1,
            lemma: "Guide".into(),
        },
        Block::Paragraph {
            text: "Intro <text>.".into(),
        },
        Block::Heading {
            level: 2,
            lemma: "Usage".into(),
        },
        Block::Open {
            kind: Container::Blockquote,
            lemma: None,
        },
        Block::Paragraph {
            text: "Quoted wisdom.".into(),
        },
        Block::Close {
            hypograph: Some("Sage".into()),
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
        Block::Verbatim {
            lang: Some("rust".into()),
            text: "fn main() {}".into(),
        },
    ])
}

#[test]
fn markdown_render() {
    let m = doc();
    assert_eq!(
        render_node(&m, m.root(), Render::Markdown),
        "\
# Guide

Intro <text>.

## Usage

> Quoted wisdom.
>
> — Sage

- alpha
- beta
  - beta-child

```rust
fn main() {}
```"
    );
}

#[test]
fn html_render() {
    let m = doc();
    assert_eq!(
        render_node(&m, m.root(), Render::Html),
        "\
<h1>Guide</h1>

<p>Intro &lt;text&gt;.</p>

<h2>Usage</h2>

<blockquote>
<p>Quoted wisdom.</p>
<footer>Sage</footer>
</blockquote>

<ul>
<li>alpha</li>
<li>beta
<ul>
<li>beta-child</li>
</ul></li>
</ul>

<pre><code class=\"language-rust\">fn main() {}</code></pre>"
    );
}

#[test]
fn plain_render() {
    let m = doc();
    assert_eq!(
        render_node(&m, m.root(), Render::Plain),
        "\
Guide

Intro <text>.

Usage

Quoted wisdom.
— Sage

- alpha
- beta
  - beta-child

fn main() {}"
    );
}

/// Rendering a single result subtree — a section — not the root.
#[test]
fn subtree_render() {
    let m = doc();
    let usage = *quarb::run("//section[::lemma = 'Usage']", &m)
        .ok()
        .and_then(|r| match r {
            quarb::QueryResult::Nodes(ns) => Some(ns),
            _ => None,
        })
        .unwrap()
        .first()
        .unwrap();
    let md = render_node(&m, usage, Render::Markdown);
    assert!(md.starts_with("## Usage\n"));
    assert!(md.contains("> Quoted wisdom."));
}

/// A table-derived list renders as the nested list it became.
#[test]
fn table_renders_as_list() {
    let m = TextModel::build(vec![Block::Table {
        lemma: None,
        headers: Some(vec!["Name".into(), "Role".into()]),
        rows: vec![vec!["Alice".into(), "captain".into()]],
    }]);
    assert_eq!(
        render_node(&m, m.root(), Render::Markdown),
        "1. - Name: Alice\n   - Role: captain"
    );
}
