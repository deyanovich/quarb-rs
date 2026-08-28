//! The expression head: a query may open with `= expr` — sugar
//! for `^ | (expr)`, one row from the root anchor (spec: The
//! Expression Head). Calculator detection (`is_calculator`) gates
//! the no-target invocation in qua/quai. Ruling #12 rides along:
//! shaped literals type themselves in arithmetic position.

use quarb::Defs;
use quarb::{expand, is_calculator};

fn canon(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn refuse(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn head_desugars_to_root_anchor() {
    // The head and its spelled-out form canonicalize identically.
    assert_eq!(canon("= 2 + 2"), canon("^ | (2 + 2)"));
    // Pipe from the head — stages compose.
    canon("= 2 + 2 | rec(\"answer\", $_)");
    // Parenthesized sub-expressions stay ordinary operands.
    canon("= (2 + 2) * 3");
    // An aggregate over the one-row stream.
    canon("= 3 @| count");
}

#[test]
fn head_composes_with_fragments() {
    // A stage fragment applies to the head's row.
    canon("def &twice: | ($_ * 2); = 21 | &twice");
    // A query fragment may itself be expression-headed.
    canon("def &four: = 2 + 2; &four");
}

#[test]
fn head_stands_alone() {
    assert!(refuse("= 1 || /x").contains("expression head stands alone"));
}

#[test]
fn parens_keep_their_navigational_meanings() {
    // Group alternation, quantified walks, mark anchors — all as
    // before the head existed; a leading paren is never a head.
    canon("(/a|/b)");
    canon("(/a/(x.rs|y.txt)|/b/z.rs)");
    // (mark anchors print in double parentheses since ruling #43)
    assert_eq!(canon("/a.m | (m)/address"), "/a.m | ((m))/address");
    assert!(refuse("(|/a)").contains("at least one hop"));
}

#[test]
fn calculator_detection() {
    // Expression heads and the lone root anchor qualify.
    assert!(is_calculator("= 2 + 2"));
    assert!(is_calculator("= 2 + 2 | rec(\"x\", $_)"));
    assert!(is_calculator("^ | count"));
    assert!(is_calculator("def &four: = 2 + 2; &four"));
    // Anything that navigates does not.
    assert!(!is_calculator("/a"));
    assert!(!is_calculator("//x::y"));
    assert!(!is_calculator("= 2 || /x"));
    // Unparseable text answers false; the run path owns the error.
    assert!(!is_calculator("1 + 1"));
}
