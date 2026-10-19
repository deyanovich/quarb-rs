//! The `sp:` command library (scipy through an embedded Python):
//! the table rule (a list of records as a labelled matrix, a sample
//! per label, a table read by its entries), the one result shape of
//! the tests, the synopsis checked at the gate, and the requirement
//! refused by name. Built only with `--features scipy`; the
//! statistical tests run where scipy imports (PYTHONPATH may point at
//! it).
#![cfg(feature = "scipy")]

use quarb::library::CommandLibrary;
use quarb::{QueryResult, Value};
use std::sync::Arc;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"a","century":"18","mean":51,"semi":16.7},
            {"book":"b","century":"18","mean":36.9,"semi":12.8},
            {"book":"c","century":"18","mean":26.2,"semi":8.7},
            {"book":"d","century":"19","mean":17.1,"semi":11.4},
            {"book":"e","century":"19","mean":16.1,"semi":13.3},
            {"book":"f","century":"19","mean":15.4,"semi":4.9}],
          "holes":[
            {"hp":130,"mpg":18},{"hp":165,"mpg":15},{"hp":null,"mpg":25},{"hp":150,"mpg":16},{"hp":90,"mpg":30}],
          "pairs":[
            {"a":"x","b":"y","d":0.6},{"a":"x","b":"z","d":1.4},{"a":"y","b":"z","d":1.3}]}"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run {
        libraries: vec![Arc::new(quarb_lib_scipy::Scipy)],
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
    quarb_lib_scipy::Scipy.available().is_ok()
}

#[test]
fn the_synopsis_is_checked_before_the_run() {
    let e = refuse("/rows/* | ::mean @| sp:ttest(--tail=1)");
    assert!(
        e.contains("'sp:ttest' has no option '--tail'") && e.contains("--popmean="),
        "{e}"
    );
    let e = refuse("/rows/* | ::mean | sp:describe");
    assert!(e.contains("rides the aggregation pipe"), "{e}");
    // A name the declared commands lack is looked for in scipy
    // itself; where scipy has none either, the refusal names the
    // package and its nearest names.
    let e = refuse("/rows/* | ::mean @| sp:median");
    assert!(e.contains("scipy has no 'median'"), "{e}");
    let e = refuse("/rows/* | ::mean @| sp:fcluster(-t)");
    assert!(e.contains("takes a value"), "{e}");
}

#[test]
fn a_stream_of_one_list_is_the_sample() {
    if !installed() {
        return;
    }
    let out = run("= @(1; 2; 3; 4; 5; 6) @| sp:describe(--digits=3)").unwrap();
    assert!(out[0].starts_with("%(nobs = 6; mean = 3.5;"), "{out:?}");
    // A table of several rows and columns is no sample.
    let e = refuse(r#"/rows/* | %(::mean; ::semi) @| sp:describe"#);
    assert!(e.contains("name the column"), "{e}");
}

#[test]
fn omit_drops_the_holes_pairwise() {
    if !installed() {
        return;
    }
    let with =
        run(r#"/holes/* | %(::hp; ::mpg) @| sp:pearsonr(--nan-policy=omit --digits=3)"#).unwrap();
    let filtered =
        run(r#"/holes/*[::hp != null] | %(::hp; ::mpg) @| sp:pearsonr(--digits=3)"#).unwrap();
    assert_eq!(with, filtered);
    assert!(with[0].starts_with("%(statistic = -0.9"), "{with:?}");
    let e = refuse(r#"/holes/* | %(::hp; ::mpg) @| sp:pearsonr"#);
    assert!(e.contains("--nan-policy=omit"), "{e}");
}

#[test]
fn the_requirement_is_refused_by_name_where_it_is_missing() {
    if installed() {
        return;
    }
    let e = refuse("/rows/* | ::mean @| sp:describe");
    assert!(
        e.contains("library 'sp' is not usable here") && e.contains("scipy"),
        "{e}"
    );
}

#[test]
fn a_sample_per_label_and_one_result_shape() {
    if !installed() {
        eprintln!("scipy not importable; skipping");
        return;
    }
    // One label and one value per record: a sample per label, in the
    // order the labels first appear. The century column is numeric
    // text, which reads as a number, so the label is spelled out.
    let r = run(r#"/rows/* | %(c = (::century = "18" ? "old" : "new"); v = ::mean) @| sp:mannwhitneyu(--digits=4)"#).unwrap();
    assert_eq!(r, vec!["%(statistic = 9; pvalue = 0.1)"]);
    let r = run(r#"/rows/* | %(c = (::century = "18" ? "old" : "new"); v = ::mean) @| sp:ttest(--digits=3)"#).unwrap();
    assert!(
        r[0].starts_with("%(statistic = 3.03; pvalue = 0.0387; df = 4)"),
        "{r:?}"
    );
    // Numeric text reads as a number: century and mean are the two
    // columns of the correlation.
    let r = run("/rows/* | %(::century; ::mean) @| sp:spearmanr(--digits=3)").unwrap();
    assert!(r[0].starts_with("%(statistic = -0.8"), "{r:?}");
    let r = run("/rows/* | %(::mean; ::semi) @| sp:linregress(--digits=3) | :slope").unwrap();
    assert_eq!(r.len(), 1);
    // The sample as numbers; the operand as the second sample.
    let r = run("/rows/* | ::mean @| sp:describe(--digits=3) | :nobs").unwrap();
    assert_eq!(r, vec!["6"]);
}

#[test]
fn a_hole_is_refused_by_name() {
    if !installed() {
        return;
    }
    let e = refuse(
        r#"/rows/* | %(c = (::century = "18" ? "old" : "new"); v = (::mean > 20 ? ::mean : null)) @| sp:kruskal"#,
    );
    assert!(e.contains("hole") || e.contains("null"), "{e}");
    let r = run(r#"/rows/* | %(c = (::century = "18" ? "old" : "new"); v = (::mean > 20 ? ::mean : null)) @| sp:kruskal(--nan-policy=omit --digits=3) | :pvalue"#).unwrap();
    assert_eq!(r.len(), 1);
}

#[test]
fn rows_cluster_and_pairs_read_by_position() {
    if !installed() {
        return;
    }
    let r = run("/rows/* | %(::book; ::mean; ::semi) @| sp:fcluster(-t=2 --method=ward) | :cluster @| join(\"\")").unwrap();
    assert_eq!(r, vec!["111222"]);
    let r =
        run("/rows/* | %(::book; ::mean; ::semi) @| sp:linkage(--method=single --digits=3) @| [1]")
            .unwrap();
    assert_eq!(
        r,
        vec!["%(a = @(\"d\"); b = @(\"e\"); distance = 2.15; size = 2)"]
    );
    // The pairs table by position: row, column, entry.
    let r = run("/pairs/* | %(::a; ::b; ::d) @| sp:leaves(--pairs) @| join(\" \")").unwrap();
    assert_eq!(r, vec!["z x y"]);
    let r = run("/pairs/* | %(::a; ::b; ::d) @| sp:linkage(--pairs --method=average) @| count")
        .unwrap();
    assert_eq!(r, vec!["2"]);
}
