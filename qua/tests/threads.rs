//! The thread tree (spec: The Pipeline, `@| threads`): the lineage
//! the run records when the query asks for it, drawn as text — every
//! step of the expanded query a level, a thread's line its cursor
//! after the step (the position relative to the line above: `…`
//! unchanged, `…` the position above with the hop's literal part and
//! then what the wildcards matched, whole otherwise), the forks
//! children, each aggregation a paragraph.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"books":[
            {"title":"Pride and Prejudice","year":1813,"tags":["love","wit"]},
            {"title":"Hamlet","year":1603,"tags":["ghost"]},
            {"title":"Tom Sawyer","year":1876,"tags":[]}]}"#,
    )
    .unwrap()
}

fn text(q: &str) -> String {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .run(&doc(), &run)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    match out.result {
        QueryResult::Values(vs) => match vs.as_slice() {
            [Value::Str(s)] => s.clone(),
            other => panic!("{q}: {other:?}"),
        },
        QueryResult::Nodes(ns) => panic!("{q}: nodes {ns:?}"),
    }
}

/// The page's example: a fork, a projection, a predicate that ends
/// one thread, a push, then a fold as a paragraph of its own.
#[test]
fn every_step_is_a_level_and_a_fold_a_paragraph() {
    assert_eq!(
        text("/books/* | ::year | [$_ > 1800] | .y @|{.y} count @| threads"),
        "\
root  (/; ∅)
└─ /books/*
   ├─ (…/0; ∅)
   │  └─ ::year  (…; 1813)
   │     └─ [$_ > 1800]  (…; 1813)
   │        └─ .y  (…; 1813)
   ├─ (…/1; ∅)
   │  └─ ::year  (…; 1603)
   └─ (…/2; ∅)
      └─ ::year  (…; 1876)
         └─ [$_ > 1800]  (…; 1876)
            └─ .y  (…; 1876)
@|{.y} count
├─ .y = 1813  (∅; 1)
└─ .y = 1876  (∅; 1)"
    );
}

/// A hop inside a branch is a level of its own; a mark on the walk a
/// step; a thread exhausted at a hop has no line under its last step.
#[test]
fn hops_within_a_branch_and_an_exhausted_thread() {
    assert_eq!(
        text("/books/* .b /tags/* @| threads"),
        "\
root  (/; ∅)
└─ /books/*
   ├─ (…/0; ∅)
   │  └─ .b  (…; ∅)
   │     └─ /tags/*
   │        ├─ (…/0; ∅)
   │        └─ (…/1; ∅)
   ├─ (…/1; ∅)
   │  └─ .b  (…; ∅)
   │     └─ /tags/*
   │        └─ (…/0; ∅)
   └─ (…/2; ∅)
      └─ .b  (…; ∅)"
    );
}

/// A reduction is a paragraph with one answer standing nowhere; the
/// steps after it nest under the answer.
#[test]
fn a_reduction_is_a_paragraph_with_one_answer() {
    assert_eq!(
        text("/books/* | ::year @| sum | .s @| threads"),
        "\
root  (/; ∅)
└─ /books/*
   ├─ (…/0; ∅)
   │  └─ ::year  (…; 1813)
   ├─ (…/1; ∅)
   │  └─ ::year  (…; 1603)
   └─ (…/2; ∅)
      └─ ::year  (…; 1876)
@| sum
└─ (∅; 5292)
   └─ .s  (∅; 5292)"
    );
}

/// Alike threads merging at a hop (the parent of both tags is the
/// one tags node): the first continues with the count, the others
/// end with the mark. A branch's elements are one hop level.
#[test]
fn alike_threads_merge_at_a_hop() {
    assert_eq!(
        text("/books/0/tags/* | \\ @| threads"),
        "\
root  (/; ∅)
└─ /books/0/tags/*
   ├─ (…/0; ∅)
   │  └─ \\
   │     └─ (/books/0/tags; ∅)  (2 merged)
   └─ (…/1; ∅)
      └─ \\
         └─ (/books/0/tags; ∅)  ⤴ merged"
    );
}

/// `-d` counts the steps within a paragraph; `-c` adds the capsa.
#[test]
fn depth_and_capsa_options() {
    assert_eq!(
        text("/books/* | ::year | .y @| threads(-d=1)"),
        "\
root  (/; ∅)
└─ /books/*
   ├─ (…/0; ∅)
   ├─ (…/1; ∅)
   └─ (…/2; ∅)"
    );
    assert_eq!(
        text("/books/0 | .t(::title) | ::year @| threads(-c)"),
        "\
root  (/; ∅)
└─ /books/0
   └─ (…; ∅)
      └─ .t(::title)  (…; ∅)
         └─ ::year  (…; 1813)  [.t = \"Pride and Prejudice\"]"
    );
}
