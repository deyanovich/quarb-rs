//! Reflected commands (spec: Command Libraries, "Reflected
//! commands"): numpy, pandas and the rest of scipy and scikit-learn
//! by their own signatures; the library value kept between commands
//! and read by `| @` and `| %`; the gate on a reflected synopsis.
//! Built only with the python features; runs where the packages
//! import (PYTHONPATH may point at them).
#![cfg(all(feature = "numpy", feature = "pandas", feature = "scipy", feature = "sklearn"))]

use quarb::library::CommandLibrary;
use quarb::{QueryResult, Value};
use std::sync::Arc;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"a","x":1,"y":2.0},
            {"book":"b","x":3,"y":1.0},
            {"book":"c","x":5,"y":0.0},
            {"book":"d","x":6,"y":0.5}],
          "v":[1,2,3,4,10],
          "kv":[{"k":"p","v":1},{"k":"q","v":2},{"k":"p","v":4}],
          "csv":[{"sex":"F","tip":"1.5"},{"sex":"M","tip":"2.5"},{"sex":"F","tip":"3.5"}],
          "series":["1","2","3","4","5","6"]}"#,
    )
    .unwrap()
}

fn libs() -> quarb::Run {
    quarb::Run {
        libraries: vec![
            Arc::new(quarb_lib_numpy::Numpy),
            Arc::new(quarb_lib_pandas::Pandas),
            Arc::new(quarb_lib_scipy::Scipy),
            Arc::new(quarb_lib_sklearn::Sklearn),
        ],
        ..quarb::Run::ambient()
    }
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &libs())?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

fn installed() -> bool {
    quarb_lib_numpy::Numpy.available().is_ok()
        && quarb_lib_pandas::Pandas.available().is_ok()
        && quarb_lib_scipy::Scipy.available().is_ok()
        && quarb_lib_sklearn::Sklearn.available().is_ok()
}

#[test]
fn the_gate_reads_the_signature() {
    if !installed() {
        eprintln!("python packages not importable; skipping");
        return;
    }
    // An unknown name: the package's nearest names.
    let e = refuse("/v/* | :: @| np:meen");
    assert!(e.contains("have no 'meen'") && e.contains("nearest: mean"), "{e}");
    // An option the function does not take, refused with its synopsis.
    let e = refuse("/v/* | :: @| np:mean(--foo=1)");
    assert!(e.contains("has no option '--foo'") && e.contains("--axis="), "{e}");
    // A method chain's unknown tail opens the gate: Python judges.
    let e = refuse(r#"/rows/* | %(::book; ::x) @| pd:groupby.nothing(book)"#);
    assert!(e.contains("nothing"), "{e}");
    // A library command is pure: what reads or writes outside the
    // query is withheld, at the gate, by module, name or parameter.
    for q in [
        r#"/v/* | :: @| pd:read_csv("x.csv")"#,
        r#"/v/* | :: @| np:save("x.npy")"#,
        r#"/v/* | :: @| sp:io.loadmat("x.mat")"#,
        r#"/v/* | :: @| sklearn:datasets.load_iris"#,
        r#"/rows/* | %(::book; ::x) @| pd:to_csv("x.csv")"#,
        r#"/rows/* | %(::book; ::x) @| pd:to_pickle("x.pkl")"#,
    ] {
        let e = refuse(q);
        assert!(e.contains("is withheld") && e.contains("outside the query"), "{q}: {e}");
    }
    // The pure converters stay.
    assert!(run(r#"/rows/* | %(::book; ::x) @| pd:to_dict"#).is_ok());
    // A library value has no truth value and no reading: a predicate
    // on one refuses, and so does a comparison.
    let model = r#"/rows/* | %(::book; ::x; ::y) @| sklearn:naive_bayes.GaussianNB.fit((^/rows/* | ::book))"#;
    let e = refuse(&format!("{model} | [$_] | count"));
    assert!(e.contains("library value"), "{e}");
    let e = refuse(r#"/rows/* | %(::book; ::x; ::y) @| sklearn:naive_bayes.GaussianNB.fit((^/rows/* | ::book)) | [$_ > 1] | count"#);
    assert!(e.contains("a comparison") && e.contains("library value"), "{e}");
}

#[test]
fn numpy_functions_over_the_stream() {
    if !installed() {
        return;
    }
    assert_eq!(run("/v/* | :: @| np:mean").unwrap(), vec!["4"]);
    assert_eq!(run("/v/* | :: @| np:percentile(50)").unwrap(), vec!["3"]);
    // A matrix from records: numeric fields, labels dropped; the
    // array stays a library value; `| @` reads its rows.
    // After `|` the rows are one list value; after `@|` they are the
    // stream.
    assert_eq!(
        run(r#"/rows/* | %(::book; ::x; ::y) @| np:transpose | @"#).unwrap(),
        vec!["@(@(1; 3; 5; 6); @(2; 1; 0; 0.5))"]
    );
    assert_eq!(
        run(r#"/rows/* | %(::book; ::x; ::y) @| np:transpose @| @"#).unwrap(),
        vec!["@(1; 3; 5; 6)", "@(2; 1; 0; 0.5)"]
    );
    // A one-dimensional array comes back as a sample (a one-column
    // table without a header), which reads back to its values; a
    // two-dimensional one as an array table.
    assert_eq!(
        run("/v/* | :: @| np:cumsum").unwrap(),
        vec!["#(%(1);\n  %(3);\n  %(6);\n  %(10);\n  %(20))"]
    );
    assert_eq!(run("/v/* | :: @| np:cumsum @| @").unwrap(), vec!["1", "3", "6", "10", "20"]);
    assert_eq!(run("/v/* | :: @| np:cumsum | [2]").unwrap(), vec!["3"]);
    // a list literal on the pipe is the sample itself
    assert_eq!(run("= @(-1; 1) @| np:log @| @").unwrap(), vec!["nan", "0"]);
    assert_eq!(
        run("/rows/* | %(::book; ::x; ::y) @| np:transpose").unwrap(),
        vec!["#(%(1; 3; 5; 6);\n  %(2; 1; 0; 0.5))"]
    );
    // Chaining: the sample is the input of the next function, on
    // either pipe.
    assert_eq!(run("/v/* | :: @| np:cumsum @| np:max").unwrap(), vec!["20"]);
    assert_eq!(run("/v/* | :: @| np:cumsum | np:max").unwrap(), vec!["20"]);
    // A library value in a stdlib command is refused by kind.
    let e = refuse(r#"/rows/* | %(::book; ::x; ::y) @| sklearn:naive_bayes.GaussianNB.fit((^/rows/* | ::book)) | upper"#);
    assert!(e.contains("is a library value") && e.contains("'| @'"), "{e}");
}

#[test]
fn numeric_text_enters_arrays_and_frames_as_numbers() {
    if !installed() {
        return;
    }
    // The table rule: a value with a numeric reading is the number
    // it reads as (CSV fields arrive as text), so arrays compute and
    // a frame's column is numeric when every value reads as one.
    assert_eq!(run("/csv/* | ::tip @| np:mean").unwrap(), vec!["2.5"]);
    assert_eq!(run("/csv/* | ::tip @| np:percentile(50)").unwrap(), vec!["2.5"]);
    let out = run(r#"/csv/* | %(::sex; ::tip) @| pd:groupby.mean(sex) @| @"#).unwrap();
    assert_eq!(out, vec![r#"%(sex = "F"; tip = 2.5)"#, r#"%(sex = "M"; tip = 2.5)"#]);
    assert_eq!(run(r#"/csv/* | %(::sex; ::tip) @| pd:tip.dtype"#).unwrap(), vec!["float64"]);
    // A class first on the path is constructed from the input when
    // its first parameter is its data; the operands go on.
    let a = run("/series/* | :: @| pd:Series.rolling.mean(3) @| @ @| [3]").unwrap();
    let b = run("/series/* | :: @| pd:rolling.mean(3) @| @ @| [3]").unwrap();
    assert_eq!(a, vec!["2"]);
    assert_eq!(a, b);
}

#[test]
fn pandas_frames_chain_and_export() {
    if !installed() {
        return;
    }
    // describe, rounded, read by columns: a record per column keyed
    // by the statistic.
    // (describe's index is the statistic: the first column of the
    // table coming back, so the column reading keys rows by it)
    let out = run(r#"/rows/* | %(::book; ::x; ::y) @| pd:describe | pd:round(2) | [2] | :x"#)
        .unwrap();
    assert_eq!(out, vec!["3.75"]);
    let out = run(r#"/rows/* | %(::book; ::x; ::y) @| pd:describe | pd:round(2) | header"#).unwrap();
    assert_eq!(out, vec![r#"@("index"; "x"; "y")"#]);
    // The row reading carries the index back as the first field.
    let out = run(r#"/rows/* | %(::book; ::x; ::y) @| pd:corr(--method=spearman) @| @ @| [1]"#)
        .unwrap();
    assert_eq!(out, vec![r#"%(index = "x"; x = 1; y = -0.8)"#]);
    // The label is the index; the frame's text columns stay.
    let out = run(r#"/rows/* | %(::book; ::x; ::y) @| pd:sort_values(x; --ascending=false) @| @ @| [1]"#)
        .unwrap();
    assert_eq!(out, vec![r#"%(book = "d"; x = 6; y = 0.5)"#]);
    // A scalar back from a method is a number.
    assert_eq!(run(r#"/rows/* | %(::book; ::x; ::y) @| pd:x.sum"#).unwrap(), vec!["15"]);
    // An intermediate object takes its method on the path.
    let out = run(r#"/kv/* | %(::k; ::v) @| pd:groupby.sum(k) | @"#).unwrap();
    assert_eq!(out, vec![r#"@(%(k = "p"; v = 5); %(k = "q"; v = 2))"#]);
    // The frame serializes by its rows.
    let out = run(r#"/rows/*[::book = "a"] | %(::book; ::x) @| pd:round(0) | json"#).unwrap();
    assert_eq!(out, vec![r#"[{"book": "a", "x": 1}]"#]);
}

#[test]
fn scipy_and_sklearn_by_reflection_beside_the_declared_commands() {
    if !installed() {
        return;
    }
    // The declared command, and the raw function under its own name.
    let declared = run(r#"/rows/* | %(::x) @| sp:shapiro(--digits=3) | :pvalue"#).unwrap();
    let raw = run(r#"/rows/* | ::x @| sp:stats.shapiro | :pvalue | round(3)"#).unwrap();
    assert_eq!(declared, raw);
    // A function of a submodule, a result bunch as a record.
    let out = run(r#"= @(1; 2; 3; 4) | sp:stats.describe | :mean"#).unwrap();
    assert_eq!(out, vec!["2.5"]);
    // A module instance's method with its options.
    assert_eq!(run("= 0 | sp:stats.norm.cdf(--loc=0 --scale=1)").unwrap(), vec!["0.5"]);
    // An estimator: constructed with its options, the method over the
    // matrix; the labels come back by position through `| @`.
    let out = run(r#"/rows/* | %(::book; ::x; ::y) @| sklearn:cluster.KMeans.fit_predict(2; --random-state=0 --n-init=1) @| @ @| count"#)
        .unwrap();
    assert_eq!(out, vec!["4"]);
    // An attribute of the fitted result.
    let out = run(r#"/rows/* | %(::book; ::x; ::y) @| sklearn:decomposition.PCA.fit.explained_variance_ratio_(--n-components=2) @| @ @| count"#)
        .unwrap();
    // A callable instance on the path is walked into, or called
    // when given arguments: the frozen distribution.
    assert_eq!(run("= 0 | sp:stats.norm(0; 1).cdf").is_err(), true);
    assert_eq!(run("= 1.96 | sp:stats.norm.cdf | round(3)").unwrap(), vec!["0.975"]);
    assert_eq!(out, vec!["2"]);
}

/// An operand of a library command keeps the kind its expression
/// yields (spec: "Reflected commands", the operand sentence): a
/// subcontext yielding records is a table read by the table rule,
/// one yielding numbers a sample, one yielding a library value the
/// object itself. The input fills the first parameter named as data
/// (`X`), a fitted estimator on the pipe is the receiver of its
/// methods, and a bare constructor that takes no data is built from
/// its options alone.
#[test]
fn operands_keep_their_kind_and_the_input_fills_the_data_slot() {
    if !installed() {
        return;
    }
    let x = "/rows/* | %(::book; ::x; ::y)";
    let xr = "(^/rows/* | %(::book; ::x; ::y))";
    let y = "(^/rows/* | ::book)";
    // y as a list operand: a sample of labels
    let r = run(&format!("{x} @| sklearn:naive_bayes.GaussianNB.fit({y})")).unwrap();
    assert_eq!(r, vec!["GaussianNB()"]);
    // the fitted estimator is the receiver of predict and score; X
    // the operand, a table
    let r = run(&format!(
        "{x} @| sklearn:naive_bayes.GaussianNB.fit({y}) @| sklearn:naive_bayes.GaussianNB.predict({xr}) @| @"
    ))
    .unwrap();
    assert_eq!(r, vec!["a", "b", "c", "d"]);
    let r = run(&format!(
        "{x} @| sklearn:naive_bayes.GaussianNB.fit({y}) @| sklearn:naive_bayes.GaussianNB.score({xr}; {y})"
    ))
    .unwrap();
    assert_eq!(r, vec!["1"]);
    // a constructor without a data parameter: the input passes it by
    let r = run("= 1 @| sklearn:naive_bayes.GaussianNB(--var-smoothing=0.5)").unwrap();
    assert_eq!(r, vec!["GaussianNB(var_smoothing=0.5)"]);
    // the input fills X, the estimator operand fills the first slot
    // (two classes of two, so two folds hold)
    let r = run(&format!(
        "{x} @| sklearn:model_selection.cross_val_score((1 | sklearn:naive_bayes.GaussianNB); (^/rows/* | (::x < 4 ? \"lo\" : \"hi\")); --cv=2) @| @ @| count"
    ))
    .unwrap();
    assert_eq!(r, vec!["2"]);
    let e = refuse(&format!("{x} @| sklearn:model_selection.cross_val_score(--cv=2)"));
    assert!(
        e.contains("needs 1 operand") && e.contains("cross_val_score(estimator; [X = $_]"),
        "{e}"
    );
    // a second table as the operand: the distance matrix of rows
    // against rows
    let r = run(&format!(
        "/rows/*[::x < 4] | %(::book; ::x; ::y) @| sp:spatial.distance.cdist({xr}) @| @ @| count"
    ))
    .unwrap();
    assert_eq!(r, vec!["2"]);
    // a boolean field reads 1 or 0: a column, not a gap
    let r = run("/rows/* | %(::book; ::x; big = (::x > 2)) @| np:transpose @| @ @| [2]").unwrap();
    assert_eq!(r, vec!["@(0; 1; 1; 1)"]);
    let r = run("/rows/* | %(::book; ::x; big = (::x > 2)) @| pd:DataFrame | % | :big").unwrap();
    assert_eq!(r, vec!["@(0; 1; 1; 1)"]);
    let r = run("/rows/* | %(::book; big = (::x > 2)) @| sp:describe(--digits=2) | :mean").unwrap();
    assert_eq!(r, vec!["0.75"]);
    // numeric text in an operand list reads as numbers, as the input's
    let r = run("/csv/* | ::tip @| np:add((^/csv/* | ::tip)) @| @").unwrap();
    assert_eq!(r, vec!["3", "5", "7"]);
    // a hole in an operand table refuses as the input's would
    let e = refuse(&format!(
        "{x} @| sklearn:naive_bayes.GaussianNB.fit({y}) @| sklearn:naive_bayes.GaussianNB.predict((^/kv/* | %(::k; v = ::v; w = (::k = \"p\" ? ::v : null))))"
    ));
    assert!(e.contains("row q: field 'w'") && e.contains("hole"), "{e}");
}

/// A table at the boundary: as the input or an operand of a reflected
/// command it crosses as its rows, so a frame becomes the DataFrame
/// or the matrix the construction makes, an array the matrix.
#[test]
fn a_table_crosses_the_boundary_as_its_rows() {
    if !installed() {
        return;
    }
    let r = run("= #(%(1; 2); %(3; 4)) @| np:add((#(%(10; 20); %(30; 40)))) | @").unwrap();
    assert_eq!(r, vec!["@(@(11; 22); @(33; 44))"]);
    let r = run("/rows/* | %(::book; ::x; ::y) @| # @| sklearn:naive_bayes.GaussianNB.fit((^/rows/* | ::book))").unwrap();
    assert_eq!(r, vec!["GaussianNB()"]);
    // The three constructors stand as operands and mean the value
    // they build: a table literal, a record as a one-row table, a
    // list as a sample.
    let r = run("/rows/* | %(::book; ::x; ::y) @| sklearn:naive_bayes.GaussianNB.fit((^/rows/* | ::book)) @| sklearn:naive_bayes.GaussianNB.predict(#(@(book; x; y); %(\"z\"; 1; 2))) @| @").unwrap();
    assert_eq!(r, vec!["a"]);
    let r = run("/rows/* | %(::book; ::x; ::y) @| sklearn:naive_bayes.GaussianNB.fit((^/rows/* | ::book)) @| sklearn:naive_bayes.GaussianNB.predict(%(x = 6; y = 0.5)) @| @").unwrap();
    assert_eq!(r, vec!["d"]);
    let r = run("/rows/* | %(::book; ::x; ::y) @| sp:spatial.distance.cdist(#(@(x; y); %(1; 2))) | dim").unwrap();
    assert_eq!(r, vec!["@(4; 1)"]);
    let r = run("= 1 @| np:append(@(2; 3)) @| @").unwrap();
    assert_eq!(r, vec!["1", "2", "3"]);
    // a Series on a named index comes back as a two-column frame
    let r = run("/rows/* | %(::book; ::x; ::y) @| # @| pd:DataFrame.mean(--numeric-only=true) | @").unwrap();
    assert_eq!(r, vec![r#"@(%(index = "x"; value = 3.75); %(index = "y"; value = 0.875))"#]);
}
