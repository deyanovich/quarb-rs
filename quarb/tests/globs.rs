//! Ruling #33 — pattern literals on `=` / `!=`: a glued,
//! strictly alternating chain of bare `*` and quoted strings.
//! The stars live outside the quotes (no escape mechanism exists
//! or is needed); adjacency is the syntax — a spaced `*` stays
//! multiplication's spelling; `*=` remains the permanent
//! heritage-door alias whose literal form canonicalizes to the
//! pattern spelling.

use quarb::Defs;
use quarb::expand;

fn canon(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn refuse(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn pattern_forms_round_trip() {
    // contains / prefix / suffix / multi-segment, tight canonical.
    assert_eq!(canon("/x[::n = *'web'*]"), "/x[::n = *'web'*]");
    assert_eq!(canon("/x[::n = 'app'*]"), "/x[::n = 'app'*]");
    assert_eq!(canon("/x[::n = *'.gz']"), "/x[::n = *'.gz']");
    assert_eq!(canon("/x[::n = *'a'*'b'*]"), "/x[::n = *'a'*'b'*]");
    // double quotes canonicalize to the single-quoted literal
    assert_eq!(canon("/x[::n = *\"web\"*]"), "/x[::n = *'web'*]");
    // `!=` takes the same operand
    assert_eq!(canon("/x[::n != *'web'*]"), "/x[::n != *'web'*]");
    // glued to the operator is tolerated, canonical prints spaced
    assert_eq!(canon("/x[::n =*'web'*]"), "/x[::n = *'web'*]");
}

#[test]
fn contains_alias_canonicalizes() {
    // the heritage door: a literal `*=` prints as the pattern form
    assert_eq!(canon("/x[::n *= 'web']"), "/x[::n = *'web'*]");
    // a dynamic right operand keeps `*=` (patterns are literal
    // syntax only)
    assert_eq!(canon("/x[::n *= ::m]"), "/x[::n *= ::m]");
}

#[test]
fn lone_star_refuses_with_the_fix() {
    let e = refuse("/x[::n = *]");
    assert!(e.contains("a lone '*' is not a pattern"), "{e}");
    // the spaced form breaks the chain and hits the same refusal
    let e = refuse("/x[::n = * 'web']");
    assert!(e.contains("a lone '*' is not a pattern"), "{e}");
}

#[test]
fn malformed_segments_refuse() {
    // doubled stars arrive as the glued name `**`
    let e = refuse("/x[::n = *'a'**]");
    assert!(e.contains("quoted string or the glob star"), "{e}");
    // glued trailing garbage is a malformed segment, not a token
    let e = refuse("/x[::n = *'a'*x]");
    assert!(e.contains("quoted string or the glob star"), "{e}");
}

#[test]
fn interpolated_segments_refuse() {
    let e = refuse("/x[::n = *\"a${::k}\"*]");
    assert!(e.contains("literal string"), "{e}");
    assert!(e.contains("=~"), "{e}");
}

#[test]
fn patterns_are_comparison_operands_only() {
    // on an ordering comparison the star chain never assembles;
    // the bareword star is the literal, as it always was
    assert_eq!(canon("/x[::n > '*']"), "/x[::n > '*']");
}
