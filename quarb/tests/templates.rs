//! Quarq templates (design paper: the quasiquote is a quarq with
//! expansions): `${…}` in syntax position, its kind the position it
//! stands at, filled where the literal is evaluated.

use quarb::{Defs, expand, expand_first};

fn exp(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn err(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn a_sequence_expansion_fills_a_record() {
    // the rest parameter's projections, one field each, in order
    assert_eq!(
        exp(
            "macro &pick(@cols): q(| %(${/cols/*})); /row[::shelf = \"A1\"] | &pick(::title, ::year)"
        ),
        "/row[::shelf = \"A1\"] | %(::title; ::year)"
    );
}

#[test]
fn a_name_expansion_is_a_whole_token() {
    // a push named from the projection's key, the projection node
    // grafted after a path
    // (the argument `::year` reflects as a path holding the
    // projection, the locked shape; the walk lands on the projection)
    assert_eq!(
        exp(
            "macro &means(@cols): /cols/*/projection | q(| .${::key ++ \"-mean\"}(^/row${$_} @| mean)); \
             ^ | &means(::year) | %."
        ),
        "^ | .year-mean(^/row::year @| mean) | %."
    );
    // two columns: two pushes, in thread order
    assert_eq!(
        exp(
            "macro &means(@cols): /cols/*/projection | q(| .${::key ++ \"-mean\"}(^/row${$_} @| mean)); \
             ^ | &means(::year, ::pages) | %."
        ),
        "^ | .year-mean(^/row::year @| mean) | .pages-mean(^/row::pages @| mean) | %."
    );
    // a hop and a property named by a value
    assert_eq!(
        exp("macro &deep($g): q(/${$g}//*::${$g}); &deep(\"row\")"),
        "/row//*::row"
    );
}

#[test]
fn a_list_fills_an_operand_as_a_list_literal() {
    assert_eq!(
        exp("macro &one-of($col; @vals): q(| [$col <<= ${/vals/*}]); \
             /row | &one-of(::genre, \"epic\", \"tragedy\") | ::title"),
        "/row | [::genre <<= @(\"epic\"; \"tragedy\")] | ::title"
    );
}

#[test]
fn a_literal_expansion_computes_once() {
    // a number computed from parameters stands as a literal node
    assert_eq!(
        exp("macro &every($pct): q([$ord mod ${100 idiv $pct} = 1]); /row&every(25)::title"),
        "/row[$ord mod 4 = 1]::title"
    );
    // a text answered through an expansion is a string literal
    assert_eq!(
        exp("macro &tok($t): /t/literal | q(${::value ++ \"!\"}); /row[::name = &tok(\"x\")]"),
        "/row[::name = \"x!\"]"
    );
}

#[test]
fn a_group_expansion_is_the_union() {
    assert_eq!(
        exp("macro &any(@ps): /ps/* @| q((${$_})); &any(/a, /b//c) | count"),
        "(/a|/b//c) | count"
    );
}

#[test]
fn a_text_in_an_expansion_parses_in_its_kind() {
    // a command-position text: commands
    assert_eq!(
        exp("macro &t: q(| ${\"| ::name | upper\"}); /row | &t"),
        "/row | ::name | upper"
    );
    // a path-position text: a walk
    assert_eq!(
        exp("macro &w: q(/root${\"/a/b\"}); &w | count"),
        "/root/a/b | count"
    );
}

#[test]
fn expansions_are_checked() {
    // outside a quarq or a string an expansion has no place
    let e = err("/row | ${1 + 2}");
    assert!(e.contains("inside a quarq template"), "{e}");
    // a template fills where it stands: the form prints as written
    assert_eq!(
        exp("def &t: q(| %(${/cols/*})); /row | .x(&t) | %."),
        "/row | .x(= q(%(${/cols/*}))) | %."
    );
}

#[test]
fn expand_1_prints_the_filled_template() {
    let steps = expand_first(
        "macro &pick(@cols): q(| %(${/cols/*})); /row | &pick(::a, ::b)",
        &Defs::default(),
    )
    .unwrap();
    assert_eq!(steps, vec!["| %(::a; ::b)"]);
}

#[test]
fn two_holes_written_alike_fill_apart() {
    // the pivot's shape: a name hole and an operand hole, both `$_`,
    // one push per value of the stage before
    assert_eq!(
        exp(
            "macro &pivot($col; @v): /v/literal | ::value | q(| .${$_}(^/row[$col = ${$_}] @| count)); \
             ^ | &pivot(::k, \"a\", \"b\") | %."
        ),
        "^ | .a(^/row[::k = \"a\"] @| count) | .b(^/row[::k = \"b\"] @| count) | %."
    );
    // a literal body after a name expansion is the push's, not a
    // mark anchor; a lone expansion body is the value it brings
    assert_eq!(
        exp("macro &p(@v): /v/literal | ::value | q(| .${$_}(1) | .x(${$_})); ^ | &p(\"a\") | %."),
        "^ | .a(1) | .x·1(\"a\") | %."
    );
}

#[test]
fn a_group_expansion_takes_the_threads_quarqs() {
    // each thread builds a branch as a quarq; after `@|` the
    // gathered quarqs are the group's alternatives
    assert_eq!(
        exp(
            "macro &all(@v): /v/literal | ::value | q(/${$_}//*) @| q((${$_})); &all(\"a\", \"b\") @| count"
        ),
        "(/a//*|/b//*) @| count"
    );
}

#[test]
fn a_list_value_fills_as_the_list_constructor() {
    assert_eq!(
        exp("macro &t: ^ | .pt(@(\"za0d8e\")) | q(//*[:::classes *= ${$.pt}] @| count); &t"),
        "//*[:::classes *= @(\"za0d8e\")] @| count"
    );
    assert_eq!(
        exp("macro &t: ^ | .pt(@(\"a\"; \"b\")) | q(//*[::k <<= ${$.pt}] @| count); &t"),
        "//*[::k <<= @(\"a\"; \"b\")] @| count"
    );
}
