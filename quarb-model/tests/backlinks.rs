//! A model over a base that cannot walk its links back (spec: The
//! adapter contract): what the model declares it walks back
//! itself, under the declaration's own label; every other backward
//! hop needs the base's backlinks and is refused as over the base
//! alone.

use quarb::{AstAdapter, NodeId};
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model};

/// A base whose links are read off the node they leave: no
/// backlinks, and it says so.
struct NoBack<A>(A);

impl<A: AstAdapter> AstAdapter for NoBack<A> {
    quarb::forward_ast_adapter!(0; except backlinks, backlink_rows, backlinks_unsupported);
    fn backlinks(&self, _node: NodeId) -> Vec<(String, NodeId)> {
        Vec::new()
    }
    fn backlink_rows(&self, _node: NodeId) -> Vec<(String, NodeId, Option<NodeId>)> {
        Vec::new()
    }
    fn backlinks_unsupported(&self, _node: NodeId, _label: Option<&str>) -> bool {
        true
    }
}

fn library(statements: &str) -> ModelAdapter<NoBack<JsonAdapter>> {
    let base = JsonAdapter::parse(
        r#"{"books": {
              "sawyer": {"title": "Tom Sawyer",       "author": "twain"},
              "finn":   {"title": "Huckleberry Finn", "author": "twain"},
              "emma":   {"title": "Emma",             "author": "austen"}},
            "authors": {
              "twain":  {"id": "twain",  "name": "Mark Twain"},
              "austen": {"id": "austen", "name": "Jane Austen"},
              "homer":  {"id": "homer",  "name": "Homer"}}}"#,
    )
    .unwrap();
    ModelAdapter::new(NoBack(base), parse_model(statements).unwrap())
}

fn values(a: &impl AstAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    }
}

fn refused(a: &impl AstAdapter, q: &str) -> String {
    match quarb::run(q, a) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("{q} was answered"),
    }
}

#[test]
fn the_model_walks_back_what_it_declares() {
    let a = library("ref /books/*::author --> authors;");
    // the declared reference, back under its own label: the fabric
    // holds every source, the base is never asked
    assert_eq!(
        values(&a, "/authors/twain<-books::title"),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
    assert_eq!(values(&a, "/authors/twain--books::title").len(), 2);
    // an author nobody wrote under: nothing is there, and the
    // empty answer is the true one
    assert_eq!(values(&a, "/authors/homer<-books"), [] as [&str; 0]);
    // forward is the base's and the model's as before
    assert_eq!(values(&a, "/books/emma->authors::name"), ["Jane Austen"]);
}

#[test]
fn any_other_backward_hop_is_the_bases_and_refused() {
    let a = library("ref /books/*::author --> authors;");
    for q in [
        // a wildcard would need the base's own links walked back
        "/authors/twain<-*",
        "/authors/twain--*",
        "/authors/twain~*",
        // a label the model does not walk back under
        "/authors/twain<-wrote",
        // a pattern and a selection name no one label
        "/authors/twain<-b*",
        "/authors/twain<-(books || wrote)",
    ] {
        let e = refused(&a, q);
        assert!(e.contains("does not support backlinks"), "{q}: {e}");
    }
    // `as NAME` moves the label, and the answer with it
    let b = library("ref /books/*::author --> authors as wrote;");
    assert_eq!(
        values(&b, "/authors/twain<-wrote::title"),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
    assert!(refused(&b, "/authors/twain<-books").contains("does not support backlinks"));
}

/// The edges TABLE over a node set is answered from the forward
/// links of the set (the user, 2026-11-10): it holds the edges among
/// the nodes given, and every such edge leaves one of them, so a
/// source that cannot walk its links back is short of nothing. It
/// was refused, as the walk forms are and stay.
#[test]
fn the_edges_table_reads_the_sets_forward_links() {
    let a = library("ref /books/*::author --> authors as wrote;");
    // (the base's own link stands under the property's name too; the
    // wildcard is the hop that would have needed it walked back)
    let into = values(&a, "/*/* @| edges(<-*) | #(:source; :target) @| csv");
    assert_eq!(
        into,
        [
            "source,target\n/books/sawyer,/authors/twain\n/books/finn,/authors/twain\n/books/emma,/authors/austen"
        ]
    );
    // either way and both ways, the same three edges
    assert_eq!(
        values(&a, "/*/* @| edges(->*) | dim"),
        values(&a, "/*/* @| edges(<-*) | dim")
    );
    assert_eq!(
        values(&a, "/*/* @| edges(--*) | dim"),
        values(&a, "/*/* @| edges(<-*) | dim")
    );
    // a set no edge runs within has none
    assert_eq!(values(&a, "/authors/* @| edges(<-*) | dim"), ["0, 4"]);
    // the walk leaves the set, and is refused as before
    assert!(refused(&a, "/authors/twain<-*").contains("does not support backlinks"));
}

#[test]
fn a_declaration_with_no_row_yet_still_answers() {
    // the label is the declaration's whether or not a row carries
    // it: an empty scope answers nothing, it is not refused
    let a = library("ref /plays/*::author --> authors as staged;");
    assert_eq!(values(&a, "/authors/twain<-staged"), [] as [&str; 0]);
    // a relation walks back under the role of the scope it leaves
    let a = library("rel /authors/* -> /books/* [::author = $$0::id];");
    assert_eq!(values(&a, "/books/emma<-authors::name"), ["Jane Austen"]);
}

#[test]
fn a_role_node_is_the_fabrics_alone() {
    // a derived node has no links but the fabric's: nothing of the
    // base's is missing there, so nothing is refused, wildcard
    // included
    let a = library(
        "node /writers/author: /authors/*;\n\
         ref /books/*::author --> /writers/author;",
    );
    assert_eq!(
        values(&a, r#"/writers/author[:name = "twain"]<-*::title"#),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
}
