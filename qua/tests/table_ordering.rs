//! Ordering a table (spec: The Table): a keyed aggregate whose one
//! member is a table ranks its rows, each keyed as the cursor
//! record, and answers a table under the same header; `--desc`
//! (`-d`) flips the whole key list on `sort_by` and `sort`; the
//! ordering reductions read a table's rows (`reverse`, `unique` a
//! table, `first` / `last` a row), `sort` refuses with the pointer.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"name":"Iliad","pages":683,"year":-750},
            {"name":"Emma","pages":474,"year":1815},
            {"name":"Hamlet","pages":342,"year":1603},
            {"name":"Emma","pages":474,"year":1815}]}"#,
    )
    .unwrap()
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

const TABLE: &str = "/rows/*[1..3] | %(::name; ::pages; ::year) @| #";
const DUPES: &str = "/rows/* | %(::name; ::pages; ::year) @| #";

const ILIAD: &str = "%(\"Iliad\"; 683; -750)";
const EMMA: &str = "%(\"Emma\"; 474; 1815)";
const HAMLET: &str = "%(\"Hamlet\"; 342; 1603)";

fn table(rows: &[&str]) -> String {
    format!("#(@(name; pages; year);\n  {})", rows.join(";\n  "))
}

#[test]
fn a_keyed_aggregate_ranks_the_rows_of_one_table() {
    assert_eq!(one(TABLE), table(&[ILIAD, EMMA, HAMLET]));
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(:pages)")),
        table(&[HAMLET, EMMA, ILIAD])
    );
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(:pages; --desc)")),
        table(&[ILIAD, EMMA, HAMLET])
    );
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(:name; -d)")),
        table(&[ILIAD, HAMLET, EMMA])
    );
    // `--desc` flips the whole key list, a unary minus one key
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(:name; - :pages; --desc)")),
        table(&[ILIAD, HAMLET, EMMA])
    );
    // the chain of stable sorts, least significant first, is the
    // same ranking
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(:pages; --desc) @| sort_by(:name)")),
        table(&[EMMA, HAMLET, ILIAD])
    );
    assert_eq!(
        one(&format!("{TABLE} @| top(2; :pages)")),
        table(&[ILIAD, EMMA])
    );
    assert_eq!(
        one(&format!("{TABLE} @| bottom(1; :year)")),
        table(&[ILIAD])
    );
    assert_eq!(one(&format!("{TABLE} @| max_by(:year)")), table(&[EMMA]));
    assert_eq!(
        one(&format!("{DUPES} @| unique_by(:name)")),
        table(&[ILIAD, EMMA, HAMLET])
    );
    // the result is a table: the row predicate composes after it
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(:pages; --desc) | [:year > 0]")),
        table(&[EMMA, HAMLET])
    );
}

#[test]
fn the_ordering_reductions_read_the_rows() {
    assert_eq!(
        one(&format!("{TABLE} | reverse")),
        table(&[HAMLET, EMMA, ILIAD])
    );
    assert_eq!(
        one(&format!("{TABLE} @| reverse")),
        table(&[HAMLET, EMMA, ILIAD])
    );
    assert_eq!(
        one(&format!("{TABLE} | first")),
        "%(name = \"Iliad\"; pages = 683; year = -750)"
    );
    assert_eq!(
        one(&format!("{TABLE} | last")),
        "%(name = \"Hamlet\"; pages = 342; year = 1603)"
    );
    assert_eq!(one(&format!("{TABLE} | first(2)")), table(&[ILIAD, EMMA]));
    assert_eq!(
        one(&format!("{TABLE} | last(1..2)")),
        table(&[EMMA, HAMLET])
    );
    assert_eq!(
        one(&format!("{DUPES} | unique")),
        table(&[ILIAD, EMMA, HAMLET])
    );
    // a keyless sort has no column: refused with the pointer
    for q in [format!("{TABLE} | sort"), format!("{TABLE} @| sort")] {
        let e = run(&q).unwrap_err().to_string();
        assert!(e.contains("sorts by a column") && e.contains("sort_by(:col; --desc)"), "{q}: {e}");
    }
}

#[test]
fn desc_on_values_and_the_option_checks() {
    assert_eq!(
        run("/rows/*[1..3] | ::pages @| sort(--desc)").unwrap(),
        vec!["683", "474", "342"]
    );
    assert_eq!(
        run("/rows/*[1..3] | ::pages @| sort(-d)").unwrap(),
        vec!["683", "474", "342"]
    );
    assert_eq!(
        run("/rows/*[1..3] | ::name @| sort(atergo; --desc)").unwrap(),
        vec!["Hamlet", "Iliad", "Emma"]
    );
    assert_eq!(
        run("/rows/*[1..3] @| sort_by(::pages; --desc) | ::name").unwrap(),
        vec!["Iliad", "Emma", "Hamlet"]
    );
    assert_eq!(
        one("/rows/*[1..3] | ::pages @| @ | sort(--desc)"),
        "@(683; 474; 342)"
    );
    let e = run("/rows/* @| sort_by(--desc)").unwrap_err().to_string();
    assert!(e.contains("needs value-expression keys"), "{e}");
    let e = run("/rows/* @| sort_by(::pages; --nope)").unwrap_err().to_string();
    assert!(e.contains("--desc"), "{e}");
}

#[test]
fn the_glued_minus_descends_on_one_key_of_any_type() {
    // `-::name` descends on the text key alone; `- ::pages` is the
    // negated number (arithmetic, the spaced minus)
    assert_eq!(
        run("/rows/*[1..3] @| sort_by(-::name) | ::name").unwrap(),
        vec!["Iliad", "Hamlet", "Emma"]
    );
    assert_eq!(
        run("/rows/*[1..3] @| sort_by(-::pages) | ::name").unwrap(),
        vec!["Iliad", "Emma", "Hamlet"]
    );
    assert_eq!(
        run("/rows/*[1..3] @| sort_by(- ::pages) | ::name").unwrap(),
        vec!["Iliad", "Emma", "Hamlet"]
    );
    // under --desc a minus-marked key ascends
    assert_eq!(
        run("/rows/*[1..3] @| sort_by(-::name; --desc) | ::name").unwrap(),
        vec!["Emma", "Hamlet", "Iliad"]
    );
    // mixed directions on text keys: descending name, ascending year
    assert_eq!(
        run("/rows/* @| sort_by(-::name; ::year) | ::year").unwrap(),
        vec!["-750", "1603", "1815", "1815"]
    );
    // the chain of stable sorts is the same ranking
    assert_eq!(
        run("/rows/* @| sort_by(::year) @| sort_by(::name; --desc) | ::year").unwrap(),
        run("/rows/* @| sort_by(-::name; ::year) | ::year").unwrap()
    );
    // top/bottom take the marker too; elsewhere `-::n` is the negation
    assert_eq!(
        run("/rows/*[1..3] @| top(2; -::pages) | ::name").unwrap(),
        vec!["Hamlet", "Emma"]
    );
    assert_eq!(run("/rows/*[1] | %(n = -::pages)").unwrap(), vec!["%(n = -683)"]);
    let e = run("/rows/*[1] | ::pages -::year").unwrap_err().to_string();
    assert!(e.contains("spaced on both sides"), "{e}");
}

#[test]
fn the_rounded_minus_in_keyed_parentheses_is_the_descending_core_field() {
    // inside a keyed aggregate's parentheses `-:col` descends on a
    // core field (a table's column); the incoming arrow there is
    // written pointy, `<-col`; elsewhere `-:` stays the arrow
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(-:pages)")),
        table(&[ILIAD, EMMA, HAMLET])
    );
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(-:name; :pages)")),
        table(&[ILIAD, HAMLET, EMMA])
    );
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(-:name; --desc)")),
        table(&[EMMA, HAMLET, ILIAD])
    );
    assert_eq!(
        one(&format!("{TABLE} @| top(2; -:pages)")),
        table(&[HAMLET, EMMA])
    );
    // `<-author` is the crosslink key (none here: every key null,
    // the order kept); `/rows/*-:author` the rounded hop
    assert_eq!(
        one(&format!("{TABLE} @| sort_by(<-author)")),
        table(&[ILIAD, EMMA, HAMLET])
    );
    assert!(run("/rows/*-:author | ::name").unwrap().is_empty());
}
