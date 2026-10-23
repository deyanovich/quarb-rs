//! `map(command; --to=…)` (spec: The Table, "Mapping over the
//! cells"): the command, read as after a pipe, applied to every
//! value one level deep; `??` broadcasting over a structure and
//! standing after a pipe; the comparisons after a pipe.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"n": 1}"#).unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn one(q: &str) -> String {
    let v = run(q).unwrap();
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

const T: &str = "= #(@(a; b); %(1.26; null); %(-2.5; 4); %(null; 6))";

fn table(rows: &[&str]) -> String {
    format!("#(@(a; b);\n  {})", rows.join(";\n  "))
}

#[test]
fn map_applies_the_command_to_every_cell() {
    assert_eq!(
        one(&format!("{T} | map(round(1))")),
        table(&["%(1.3; null)", "%(-2.5; 4)", "%(null; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(abs)")),
        table(&["%(1.26; null)", "%(2.5; 4)", "%(null; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(?? 0)")),
        table(&["%(1.26; 0)", "%(-2.5; 4)", "%(0; 6)"])
    );
    // the pipe may be written; the cell stands at any position
    assert_eq!(
        one(&format!("{T} | map(| ?? 0)")),
        one(&format!("{T} | map(?? 0)"))
    );
    assert_eq!(
        one("= #(@(a; b); %(1; null); %(-2; 4)) | map(+ 3)"),
        "#(@(a; b);\n  %(4; null);\n  %(1; 7))"
    );
    assert_eq!(
        one("= #(@(a; b); %(1; null); %(-2; 4)) | map(3 + $_)"),
        "#(@(a; b);\n  %(4; null);\n  %(1; 7))"
    );
    assert_eq!(
        one("= #(@(a; b); %(1; null); %(-2; 4)) | map(> 0 ? $_ : null)"),
        "#(@(a; b);\n  %(1; null);\n  %(null; 4))"
    );
    assert_eq!(
        one("= #(@(a; b); %(1; null); %(-2; 4)) | map(round(1) | * 2)"),
        "#(@(a; b);\n  %(2; null);\n  %(-4; 8))"
    );
    // a list, a record, a scalar; a structure in a cell is one value
    assert_eq!(one("= @(1; null; 3) | map(?? 0)"), "@(1; 0; 3)");
    assert_eq!(one("= %(a = null; b = 2) | map(?? 0)"), "%(a = 0; b = 2)");
    assert_eq!(one("= 5 | map(+ 3)"), "8");
    assert_eq!(one("= @(@(1; null); 2) | map(?? 0)"), "@(@(1; 0); 2)");
    // a table re-types its columns
    assert_eq!(
        one("= #(@(a); %(\"1\"); %(\"2\")) | map(number) | type"),
        "int"
    );
}

#[test]
fn to_restricts_the_mapping() {
    assert_eq!(
        one(&format!("{T} | map(?? 0; --to=:b)")),
        table(&["%(1.26; 0)", "%(-2.5; 4)", "%(null; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(?? 0; --to=(:a; :b))")),
        table(&["%(1.26; 0)", "%(-2.5; 4)", "%(0; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(?? 0; --to=[1])")),
        table(&["%(1.26; 0)", "%(-2.5; 4)", "%(null; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(?? 0; --to=[2..3])")),
        table(&["%(1.26; null)", "%(-2.5; 4)", "%(0; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(?? 0; --to=[@(1; 3)])")),
        table(&["%(1.26; 0)", "%(-2.5; 4)", "%(0; 6)"])
    );
    assert_eq!(
        one(&format!("{T} | map(?? 0; --to=[..2]:b)")),
        table(&["%(1.26; 0)", "%(-2.5; 4)", "%(null; 6)"])
    );
    assert_eq!(
        one("= @(1; null; 3; null) | map(?? 0; --to=[2..3])"),
        "@(1; 0; 3; null)"
    );
    assert_eq!(
        one("= %(a = null; b = 2; c = null) | map(?? 0; --to=:c)"),
        "%(a = null; b = 2; c = 0)"
    );
    assert_eq!(
        one("= %(a = null; b = 2; c = null) | map(?? 0; --to=[1])"),
        "%(a = 0; b = 2; c = null)"
    );
    let e = run(&format!("{T} | map(?? 0; --to=:zz)")).unwrap_err().to_string();
    assert!(e.contains("no column 'zz'"), "{e}");
}

#[test]
fn the_null_default_broadcasts_and_stands_after_a_pipe() {
    assert_eq!(
        one("= #(@(a; b); %(1; null); %(null; 4)) | ?? 0"),
        "#(@(a; b);\n  %(1; 0);\n  %(0; 4))"
    );
    assert_eq!(one("= @(1; null; 3) | ?? 0"), "@(1; 0; 3)");
    assert_eq!(one("= %(a = null; b = 2) | ?? 0"), "%(a = 0; b = 2)");
    assert_eq!(one("= null | ?? 0"), "0");
    assert_eq!(one("= 5 | ?? 0"), "5");
    assert_eq!(one("= @(1; null) | $_ ?? 0"), "@(1; 0)");
    // one level deep
    assert_eq!(one("= @(@(1; null); null) | ?? 0"), "@(@(1; null); 0)");
    // the comparisons after a pipe take the topic as the left side
    assert_eq!(one("= 5 | > 3 ? $_ : null"), "5");
    assert_eq!(run("= 2 | > 3 ? $_ : null").unwrap(), vec!["null"]);
    assert_eq!(one("= 5 | = 5"), "true");
}
