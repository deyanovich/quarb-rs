//! The `sklearn:` command library (scikit-learn through an embedded
//! Python): rows with their labels through PCA and k-means, documents
//! through tf-idf and a topic model, the synopsis at the gate and the
//! requirement refused by name. Built only with `--features sklearn`;
//! the model tests run where scikit-learn imports.
#![cfg(feature = "sklearn")]

use quarb::library::CommandLibrary;
use quarb::{QueryResult, Value};
use std::sync::Arc;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"a","x":0,"y":0},{"book":"b","x":0.1,"y":0.1},
            {"book":"c","x":5,"y":5},{"book":"d","x":5.1,"y":4.9}],
          "docs":[
            {"book":"sea","text":"the ship sailed the sea and the sea was wide"},
            {"book":"field","text":"the horse ran over the field and the field was green"}]}"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run {
        libraries: vec![Arc::new(quarb_lib_sklearn::Sklearn)],
        ..quarb::Run::ambient()
    };
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

fn installed() -> bool {
    quarb_lib_sklearn::Sklearn.available().is_ok()
}

#[test]
fn the_synopsis_is_checked_before_the_run() {
    let e = refuse("/rows/* @| sklearn:pca(-n)");
    assert!(e.contains("takes a value"), "{e}");
    let e = refuse("/rows/* @| sklearn:kmeans(--k=2)");
    assert!(
        e.contains("has no option '--k'") && e.contains("-k/--n-clusters="),
        "{e}"
    );
    // A module of the package is no command: the gate says so.
    let e = refuse("/rows/* @| sklearn:svm");
    assert!(
        e.contains("is a module of sklearn") && e.contains("svm.<name>"),
        "{e}"
    );
}

#[test]
fn the_requirement_is_refused_by_name_where_it_is_missing() {
    if installed() {
        return;
    }
    let e = refuse("/rows/* @| sklearn:pca");
    assert!(
        e.contains("library 'sklearn' is not usable here") && e.contains("scikit-learn"),
        "{e}"
    );
}

#[test]
fn rows_project_and_cluster_with_their_labels() {
    if !installed() {
        eprintln!("scikit-learn not importable; skipping");
        return;
    }
    // book is the label, x and y the columns.
    let r = run("/rows/* | %(::book; ::x; ::y) @| sklearn:kmeans(-k=2) | %(l = :label; c = :cluster) @| json").unwrap();
    assert_eq!(
        r,
        vec![r#"[{"l": "a", "c": 1}, {"l": "b", "c": 1}, {"l": "c", "c": 2}, {"l": "d", "c": 2}]"#]
    );
    let r =
        run("/rows/* | %(::book; ::x; ::y) @| sklearn:pca(-n=1 --digits=3) | :label @| join(\"\")")
            .unwrap();
    assert_eq!(r, vec!["abcd"]);
    let r =
        run("/rows/* | %(::book; ::x; ::y) @| sklearn:pca(--variance --digits=3) @| [1]").unwrap();
    assert!(r[0].starts_with("0.99") || r[0] == "1", "{r:?}");
    let r = run("/rows/* | %(::book; ::x; ::y) @| sklearn:silhouette(-k=2 --digits=2)").unwrap();
    assert!(r[0].starts_with("0.9"), "{r:?}");
}

#[test]
fn documents_give_their_terms_and_topics() {
    if !installed() {
        return;
    }
    // A document record: the `text` field is the text, the first
    // other text field the label.
    let r =
        run("/docs/* | %(::book; ::text) @| sklearn:tfidf(--top=1 --stop-words=english) | :label")
            .unwrap();
    assert_eq!(r, vec!["sea", "field"]);
    let r = run(
        "/docs/* | %(::book; ::text) @| sklearn:nmf(-n=2 --top=2 --stop-words=english) | :topic",
    )
    .unwrap();
    assert_eq!(r, vec!["1", "2"]);
    let r = run("/docs/* | %(::book; ::text) @| sklearn:nmf(-n=2 --documents --stop-words=english) | :label").unwrap();
    assert_eq!(r, vec!["sea", "field"]);
}
