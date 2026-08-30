//! Ruling #34 — the bash door on `${...}` holes, glued-only:
//! `${expr:?}` / `${expr:?message}` (the strict hole, refusing
//! where a plain hole splices nothing), `${expr:-fallback}` (the
//! default hole, `default(expr, fallback)` bash-faithfully
//! spelled), and pipe tails (`${expr | stage ...}`). Bash ops and
//! an unparenthesized pipe tail are one or the other per hole.

use quarb::Defs;
use quarb::expand;

fn canon(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn refuse(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn strict_hole_round_trips() {
    assert_eq!(canon(r#"= "${::x:?}""#), r#"^ | "${::x:?}""#);
    assert_eq!(
        canon(r#"= "${::x:?no form here}""#),
        r#"^ | "${::x:?no form here}""#
    );
}

#[test]
fn default_hole_round_trips() {
    assert_eq!(canon(r#"= "${::x:-"n/a"}""#), r#"^ | "${::x:-"n/a"}""#);
    // the fallback is a full value expression — a path works
    assert_eq!(canon(r#"= "${::x:-::y}""#), r#"^ | "${::x:-::y}""#);
    // a projection run before the operator is not the operator
    assert_eq!(canon(r#"= "${::a:-1}""#), r#"^ | "${::a:-1}""#);
}

#[test]
fn pipe_tail_in_holes() {
    // the tail parses; the canonical form is the parenthesized
    // piped operand (semantically identical on re-parse)
    let c = canon(r#"= "${::n | upper}""#);
    assert!(c.contains("| upper"), "{c}");
    let again = canon(&c);
    assert_eq!(c, again);
}

#[test]
fn glue_is_the_syntax() {
    let e = refuse(r#"= "${::x :- 1}""#);
    assert!(e.contains("glues to its expression"), "{e}");
    let e = refuse(r#"= "${::x :?}""#);
    assert!(e.contains("glues to its expression"), "{e}");
}

#[test]
fn bash_op_and_bare_pipe_tail_exclude() {
    let e = refuse(r#"= "${::n | upper:-"x"}""#);
    assert!(e.contains("one or the other"), "{e}");
    // parenthesized, they compose
    let c = canon(r#"= "${(::n | upper):-"x"}""#);
    assert!(c.contains(":-\"x\""), "{c}");
}

#[test]
fn quasiquote_holes_untouched() {
    // the macro layer's `${$p}` splice carries no colon and stays
    // exactly as it was
    assert_eq!(
        canon(r#"macro &hop($n): ^ | "/w{${$n}}"; /x&hop(2)/leaf::"#),
        "/x(/w){2}/leaf::"
    );
}
