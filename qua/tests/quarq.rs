//! The quarq (spec: Quarqs, draft 166): `q(cmd)` is a literal whose
//! value is the command — or the pipeline — as it stands after a
//! pipe, read as Quarb and not run; `| type` reads `quarq`, it
//! prints as its source and equals by it. A quarq written in place
//! applies (`map(q(+ 3))`); a held one is a value wherever a value
//! stands (`map($.f)` fills with it) and is spliced by the fragment
//! sigil before its register (`| &$.f`, `map(&$.f)`, `sort_by(&$.k)`)
//! or applied by the glued sigil (`v&$.f`; the colon projects). A projector is the
//! simplest quarq and keeps its own type word.

use quarb::{Defs, QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"name":"Iliad","pages":683},
            {"name":"Emma","pages":474}]}"#,
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
    let v = run(q).unwrap_or_else(|e| panic!("{q}: {e}"));
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn rows(q: &str) -> Vec<String> {
    run(q).unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// The query unparses to text that parses back to the same text.
fn fixpoint(q: &str) -> String {
    let once = quarb::expand(q, &Defs::default()).unwrap();
    let twice = quarb::expand(&once, &Defs::default()).unwrap();
    assert_eq!(once, twice, "{q}");
    once
}

#[test]
fn a_quarq_is_a_literal_that_prints_as_its_source() {
    assert_eq!(one("= q(+ 3) | type"), "quarq");
    // the canonical source, as every unparse is canonical
    assert_eq!(one("= q(+ 3)"), "q($_ + 3)");
    assert_eq!(one("= q(| round(3))"), "q(round(3))");
    // a pipeline in the parenthesis, as a def body holds one
    assert_eq!(one("= q(mean | $_ * 5.6)"), "q(mean | $_ * 5.6)");
    // held in a register, recalled bare: the quarq itself, not run
    assert_eq!(one("= q(+ 3) | .f | $.f"), "q($_ + 3)");
    // two are equal when their source is
    assert_eq!(one("= q(+ 3) | .a | q(+  3) | .b | $.a = $.b"), "true");
    assert_eq!(one("= q(+ 3) | .a | q(+ 4) | .b | $.a = $.b"), "false");
    assert_eq!(
        fixpoint("= q(+ 3) | .f | 10 | &$.f"),
        "= q($_ + 3) | .f | 10 | &$.f"
    );
    assert_eq!(
        fixpoint("= @(1; 2) | map(q(+ 3))"),
        "= @(1; 2) | map(q($_ + 3))"
    );
}

#[test]
fn a_projector_is_the_simplest_quarq_and_keeps_its_type_word() {
    assert_eq!(one("= q(::x) | type"), "projector");
    assert_eq!(one("= q(::x)"), "::x");
    assert_eq!(one("= q(:x)"), ":x");
    // the structured projector the `=` head gives
    assert_eq!(
        one("= q(%(::Name; ::Fare))"),
        "%(Name = ::Name; Fare = ::Fare)"
    );
    assert_eq!(one("= q(%(::Name; ::Fare)) | type"), "record");
    // a read in the body leaves it a quarq
    assert_eq!(one("= q(::x + 1) | type"), "quarq");
    // held, it reads on each row through the splice and the colon
    assert_eq!(rows("/rows/* | .k(q(::pages)) | &$.k"), ["683", "474"]);
    assert_eq!(rows("/rows/* | .k(q(::pages)) | $_:$.k"), ["683", "474"]);
    assert_eq!(
        rows("/rows/* | .f(q(%(::name; ::pages))) | $_:$.f"),
        [
            "%(name = \"Iliad\"; pages = 683)",
            "%(name = \"Emma\"; pages = 474)"
        ]
    );
}

#[test]
fn a_quarq_written_in_place_applies_and_a_held_one_is_spliced() {
    // written in place: map(q(+ 3)) is map(+ 3)
    assert_eq!(one("= @(1; 2) | map(q(+ 3))"), "@(4; 5)");
    // a bare register is its value wherever a value stands
    assert_eq!(
        one("= q(+ 3) | .f | @(1; 2) | map($.f)"),
        "@(q($_ + 3); q($_ + 3))"
    );
    // the fragment sigil before the register splices the held quarq
    assert_eq!(one("= q(+ 3) | .f | @(1; 2) | map(&$.f)"), "@(4; 5)");
    assert_eq!(one("= q(+ 3) | .f | 10 | &$.f"), "13");
    assert_eq!(one("= q(+ 3) | .f | 10 | &$.f | &$.f"), "16");
    assert_eq!(one("= q(round(3)) | .f | 3.14159 | &$.f"), "3.142");
    assert_eq!(one("= q(mean | $_ * 5.6) | .f | @(1; 2; 3) | &$.f"), "11.2");
    // the glued colon applies a quarq as it applies a projector
    assert_eq!(one("= @(1; 2) | .f(q(+ 1)) | $_&$.f"), "@(2; 3)");
    // a regula holding no quarq refuses the splice
    let e = run("= \"x\" | .f | 10 | &$.f").unwrap_err().to_string();
    assert!(
        e.contains("&$.f splices a quarq, and the regula holds text"),
        "{e}"
    );
}

#[test]
fn a_quarq_is_late_bound_where_it_is_applied() {
    // `$_` and the registers are read at the application
    assert_eq!(one("= 5 | .n | q($_ + $.n) | .f | 10 | &$.f"), "15");
    assert_eq!(one("= 5 | .n | q($_ + $.n) | .f | 10 | $_&$.f"), "15");
    assert_eq!(
        one("= 5 | .n | q($_ + $.n) | .f | 7 | .n | 10 | &$.f"),
        "17"
    );
}

#[test]
fn keyed_aggregates_take_a_quarq_in_place_or_spliced() {
    assert_eq!(
        rows("/rows/* @| sort_by(q(::pages)) | ::name"),
        ["Emma", "Iliad"]
    );
    assert_eq!(
        rows("/rows/* @| sort_by(q(::pages); --desc) | ::name"),
        ["Iliad", "Emma"]
    );
    assert_eq!(
        rows("/rows/* | .k(q(::pages)) @| sort_by(&$.k) | ::name"),
        ["Emma", "Iliad"]
    );
    assert_eq!(
        rows("/rows/* | .k(q(::pages | * -1)) @| sort_by(&$.k) | ::name"),
        ["Iliad", "Emma"]
    );
    // a bare register ranks by a constant: the order stands
    assert_eq!(
        rows("/rows/* | .k(q(::pages)) @| sort_by($.k) | ::name"),
        ["Iliad", "Emma"]
    );
}

#[test]
fn a_navigating_quarq_is_the_inline_fragment() {
    // pushed on a node thread, spliced later: `def &name` inline
    assert_eq!(
        one("/rows | .p(q(/*[::pages > 500] | ::name)) | &$.p"),
        "Iliad"
    );
    assert_eq!(
        one("= q(//rows/*[::pages > 500] | ::name) | .p | &$.p"),
        "Iliad"
    );
}

/// The quarq's own core field (`QUARQ_TEXT_FIELD`) is the engine's
/// fact about the value: the canonical text the quarq stands for, the
/// line `--expand` prints (spec: Quarqs; Core Fields). The quarq
/// itself keeps printing as `q(…)`, `::form` stays null on a value,
/// and any other core field reads the thread's position as it does
/// for any value.
#[test]
fn a_quarq_answers_its_text_through_its_core_field() {
    let f = quarb::QUARQ_TEXT_FIELD;
    assert_eq!(
        one(&format!("= q(/rows/*[::pages > 500] @| count) | :{f}")),
        "/rows/*[::pages > 500] @| count"
    );
    // a fragment inside the quarq is expanded: the text is the expansion
    assert_eq!(
        one(&format!(
            "def &long: /rows/*[::pages > 500]; = q(&long @| count) | :{f}"
        )),
        "/rows/*[::pages > 500] @| count"
    );
    assert_eq!(
        one("def &long: /rows/*[::pages > 500]; = q(&long @| count)"),
        "q(/rows/*[::pages > 500] @| count)"
    );
    assert_eq!(one(&format!("= q(+ 3) | :{f}")), "$_ + 3");
    // a held quarq, a record's field, a list's items
    assert_eq!(one(&format!("= q(+ 3) | .f | $.f:{f}")), "$_ + 3");
    assert_eq!(one(&format!("= %(f = q(+ 3)) | :f:{f}")), "$_ + 3");
    assert_eq!(
        one(&format!("= @(q(/a); q(/b)) | map(:{f})")),
        "@(\"/a\"; \"/b\")"
    );
    // not a quarq: no core field of the position, null without a refusal
    assert_eq!(one(&format!("= 3 | :{f}")), "null");
    assert_eq!(one("= q(+ 3) | ::form"), "null");
}

/// The held quarq's three appliers (spec: Quarqs, the held quarq's
/// appliers): bare `&$.x` is late bound (the applying capsa and
/// cursor), `&$.x!` resolves the body's recalls against the capsa
/// windowed before the regula holding `x` with the applying cursor,
/// `&$.x#` with the topic recorded at the push; the glued colon and
/// `map` take the suffixes alike; pushes the body makes land in the
/// applying thread under all three; a never-pushed quarq refuses.
#[test]
fn a_held_quarq_applies_late_windowed_or_as_pushed() {
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | &$.f"), "15");
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | &$.f!"), "10");
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | 7 | &$.f!"), "14");
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | 7 | &$.f#"), "10");
    assert_eq!(
        one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | 7 | &$.f#"),
        "10"
    );
    // the glued sigil and map take the suffixes; the colon projects only
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | 7&$.f"), "21");
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | 7&$.f!"), "14");
    assert_eq!(one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | 7&$.f#"), "10");
    assert_eq!(
        one("= 5 | .k(2) | .f(q($_ * $.k)) | .k(3) | @(1; 2) | map(&$.f!)"),
        "@(2; 4)"
    );
    assert_eq!(one("= %(a = 1; b = 2) | .p(q(:a)) | $_:$.p"), "1");
    let err = run("= 5 | .f(q(* 2)) | $_:$.f").unwrap_err().to_string();
    assert!(err.contains("the colon projects"), "{err}");
    let err = run("= 5 | .k(2) | .f(q($_ * $.k)) | 7:$.f!")
        .unwrap_err()
        .to_string();
    assert!(err.contains("takes no suffix"), "{err}");
    // a push the body makes lands in the applying thread
    assert_eq!(
        one("= 5 | .k(2) | .f(q(.g($_ * $.k) | $.g)) | .k(3) | 7 | &$.f! | $.g"),
        "14"
    );
    // the canonical text keeps the suffix
    assert_eq!(
        fixpoint("= 5 | .k(2) | .f(q($_ * $.k)) | 7 | &$.f#"),
        "= 5 | .k(2) | .f(q($_ * $.k)) | 7 | &$.f#"
    );
    assert_eq!(
        fixpoint("= 5 | .k(2) | .f(q($_ * $.k)) | 7&$.f!"),
        "= 5 | .k(2) | .f(q($_ * $.k)) | (7&$.f!)"
    );
    // a never-pushed quarq has no regula to window before
    let err = run("= 5 | .k(2) | &$.k#").unwrap_err().to_string();
    assert!(err.contains("quarq"), "{err}");
}
