//! The node overwrite and hygiene (design paper: the node
//! overwrite; Hygiene: renamed unless invited).

use quarb::{Defs, expand};

fn exp(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn err(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn the_overwrite_copies_a_node_with_a_property_replaced() {
    // every step's axis made deep; the predicate stays under it
    assert_eq!(
        exp("macro &loose($p): /p//step | $_ ++! %(axis = \"//\"); &loose(/replies/*[::who])::who"),
        "//replies//*[::who]::who"
    );
    // a literal's value, a projection's key
    assert_eq!(
        exp("macro &ten($p): /p//literal | $_ ++! %(value = 10); /row[::n > &ten(3)]"),
        "/row[::n > 10]"
    );
    assert_eq!(
        exp("macro &key($p): /p//projection | $_ ++! %(key = \"age\"); /row | &key(::name)"),
        "/row | ::age"
    );
    // `++` adds a property the node lacks
    assert_eq!(
        exp("macro &named($p): /p//expr-push | $_ ++ %(name = \"x\"); /row | &named(q(| .(1)))"),
        "/row | .x(1)"
    );
}

#[test]
fn the_overwrite_is_checked_against_the_kind() {
    let e = err("macro &bad($p): /p//step | $_ ++ %(axis = \"//\"); &bad(/a) | count");
    assert!(e.contains("carries 'axis' already"), "{e}");
    let e = err("macro &bad($p): /p//step | $_ ++! %(colour = 1); &bad(/a) | count");
    assert!(e.contains("step node has no property 'colour'"), "{e}");
    let e = err("macro &bad($p): /p//step | $_ ++! %(axis = \"=>\"); &bad(/a) | count");
    assert!(e.contains("no axis"), "{e}");
}

#[test]
fn a_template_born_binder_is_renamed_unless_invited() {
    // nobody invited `t`: a fresh name, consistent across the answer
    let out = exp(
        "macro &share($col): q(| .t(^/row$col @| sum) | %(::title; share = round($col div $.t; 4))); \
         /row | &share(::year) | %.",
    );
    assert!(out.contains(".t·1(^/row::year @| sum)"), "{out}");
    assert!(out.contains("div $.t·1"), "{out}");
    assert!(!out.contains(".t("), "{out}");
    // two expansions: two names
    let out = exp(
        "macro &share($col): q(| .t(^/row$col @| sum) | %(::title; share = round($col div $.t; 4))); \
         /row | &share(::year) | &share(::pages) | %.",
    );
    assert!(out.contains("t·1") && out.contains("t·2"), "{out}");
    // the caller spelled `it` in an argument: the name stays
    assert_eq!(
        exp("macro &aif($test; $then): q(| .it($test) | $then); /row | &aif(::a; q(| $.it)) | %."),
        "/row | .it(::a) | $.it | %."
    );
    // a binder an argument brought keeps its name
    assert_eq!(
        exp("macro &wrap($then): q(| .w(1) $then); /row | &wrap(q(| .x(2) | %.))"),
        "/row | .w·1(1) | .x(2) | %."
    );
}
