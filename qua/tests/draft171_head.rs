//! Draft 171: the expression head's expression may be left out —
//! `= .f(0) | …` opens the root thread with no topic and the command
//! after `=` is the pipeline's first, i.e. `^ | .f(0) | …`; a leading
//! pipe stays a fragment. With it, the quantified push at the bound:
//! an open loop still moving when the bound is reached refuses
//! naming it (spec: Quantifier bound); an explicit ceiling inside the
//! bound completes silently.

use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":1}"#).unwrap()
}

fn rows(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        other => panic!("{q}: {other:?}"),
    }
}

fn one(q: &str) -> String {
    let v = rows(q);
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

const FIB: &str = "@(0; 1; 1; 2; 3; 5; 8; 13; 21; 34; 55; 89)";

#[test]
fn the_head_expression_may_be_left_out() {
    assert_eq!(one("= .f(0) | .f(1) | .f($. + $..){10} | @."), FIB);
    assert_eq!(one("^ | .f(0) | .f(1) | .f($. + $..){10} | @."), FIB);
    assert_eq!(one("= .x(5) | $.x"), "5");
    assert_eq!(one("= .x(5){2} | @. | count"), "2");
    // a library command stands there too, on the root thread
    assert_eq!(one("= count"), "1");
    // the same query: the canonical form is the caret's
    let q = quarb::Query::parse("= .f(0) | $.f", &doc()).unwrap();
    assert_eq!(q.canonical(), "^ | .f(0) | $.f");
    // a value head still reads as a value, a quarq as a quarq
    assert_eq!(one("= 5 | type"), "int");
    assert_eq!(one("= q(* 2) | type"), "quarq");
    assert_eq!(one("= null | type"), "null");
}

#[test]
fn a_leading_pipe_stays_a_fragment() {
    assert!(refuse("| .f(0) | @.").contains("at least one step"));
    let e = refuse("= | .f(0) | @.");
    assert!(e.contains("may be left out, not replaced by a pipe"), "{e}");
    assert!(e.contains("'= .f(0) | …'"), "{e}");
}

#[test]
fn an_open_loop_still_moving_at_the_bound_refuses() {
    let logistic = "= 0.5 | .x | .x(3.7 * $.x * (1 - $.x))";
    let e = refuse(&format!("{logistic}{{1;}} | @. | count"));
    assert!(
        e.contains("still moving at the quantifier bound (32)"),
        "{e}"
    );
    assert!(e.contains("raise --quantifier-bound"), "{e}");
    // an explicit ceiling inside the bound is the user's ask: silent
    assert_eq!(one(&format!("{logistic}{{1;32}} | @. | count")), "33");
    assert_eq!(one(&format!("{logistic}{{1;5}} | @. | count")), "6");
    // a fixpoint before the bound stops silently as before (the
    // round that reproduces the value is run, then seen)
    assert_eq!(one("= 1 | .x | .x($.x){1;} | @. | count"), "3");
    assert_eq!(
        one("= .v(5) | .v(($.v > 8 ? $.v : $.v + 1)){1;} | @. | count"),
        "6"
    );
    // a ceiling beyond the bound refuses up front, as before
    let e = refuse(&format!("{logistic}{{1;120}} | @. | count"));
    assert!(
        e.contains("explicit ceiling (120) exceeds the quantifier bound (32)"),
        "{e}"
    );
}
