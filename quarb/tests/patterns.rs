//! Path-pattern round-trips: groups, alternation, quantifiers, and
//! their sugars reprint in canonical (strict) form, and the
//! canonical form is a fixpoint.

use quarb::Defs;
use quarb::expand;

fn canon(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn refuse(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn tolerated_reprints_strict() {
    assert_eq!(canon("//body/(p|div)"), "//body(/p|/div)");
    assert_eq!(canon("//body(/p|/div)"), "//body(/p|/div)");
}

#[test]
fn sugar_reprints_as_dot_group() {
    assert_eq!(canon("/{2}"), "(/.){2}");
    assert_eq!(canon("/div{2}"), "(/div){2}");
    assert_eq!(canon("(/.){2}"), "(/.){2}");
    // single-hop crosslink sugar
    assert_eq!(canon("/a->{2}"), "/a(->.){2}");
}

#[test]
fn quantifier_zoo_round_trips() {
    for q in [
        "/a(/b)+",
        "/a(/b)*",
        "/a(/b){2}",
        "/a(/b){1,3}",
        "/a(/b){2,}",
        "/a(/b)+?",
        "/a(/b)+!",
        "/a(/b){1,3}?",
        "/a(->mgr)+!",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // {2,2} normalizes to {2}
    assert_eq!(canon("/a(/b){2,2}"), "/a(/b){2}");
}

#[test]
fn nesting_round_trips() {
    let q = "//div(/ul/li|/ol/li|/dl(/dt|/dd))+";
    assert_eq!(canon(q), q);
    // tolerated form distributes at both levels
    assert_eq!(canon("//div/(ul/li|ol/li|dl/(dt|dd))+"), q);
}

#[test]
fn groups_in_operand_position() {
    assert_eq!(canon("/*[(->ref)+]"), "/*[(->ref)+]");
    // boolean groups keep their reading
    assert_eq!(canon("/*[(::a = 1 or ::b)]"), "/*[(::a = 1 || ::b)]");
    assert_eq!(canon("/*[(::a = 1 || ::b)]"), "/*[(::a = 1 || ::b)]");
    assert_eq!(canon("/*[(not ::a and ::b)]"), "/*[(!::a && ::b)]");
    for q in [
        "//order::amt <=>? //user[::id = $$::uid] | ...?",
        "//u <=> //v <=>? //order[::uid = $*1::id]",
        "//commit[::::short = ^/tags/*::::short]",
        "/movie .m <-ACTED_IN[::born > (m)::released] | rec(::name, (m)::title)",
        "/c/* | (::kind ?= 'a' ? 1 : ~(^b) ? 2 : 0)",
        "/tags/* | .t(:::name) | `cloc --git ${$.t}` | rec($.t, 'lines', $_)",
        "/x::v | base64 | decode('base64')",
        "/x::v @| sort('ru-RU')",
        "/a/*[^//error] | rec('n', :::name, 'of', (^//*.rs @| count))",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // a value group still computes
    assert_eq!(canon("/a | (::x + 1) * 2"), "/a | (::x + 1) * 2");
}

#[test]
fn pattern_errors() {
    assert!(refuse("(/a){3,2}").contains("max below min"));
    assert!(refuse("(|/a)").contains("at least one hop"));
    // a push alone does not walk
    assert!(refuse("(.(::q))+").contains("at least one hop"));
    assert!(refuse("/(p|/div)").contains("strict form"));
    assert!(refuse("//{2}").contains("single-hop"));
    // `+` and `*` are name characters: quantifying an unquoted named
    // hop with them is not expressible — the name absorbs the mark.
    assert_eq!(canon("/a/g++"), "/a/g++");
}

#[test]
fn edge_accessor_round_trips() {
    for q in [
        "/a->e[$-::qty > 1]",
        "/a->*[$- = 'e']",
        "/a<-mgr[$-::since = 2020]::name",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // core/adapter metadata do not live on edges
    assert!(refuse("/a->e[$-:::depth = 1]").contains("plain properties"));
}

#[test]
fn group_predicates_round_trip() {
    for q in [
        "/a(->e)+[::q > 1]?",
        "/a(/b|/c/d)[::x]",
        "(/employees.($-))+[::name = 'Meg']?",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a(/b)+[1]").contains("expression predicates only"));
    assert!(refuse("/a(/b)+[1..2]").contains("expression predicates only"));
}

#[test]
fn pattern_pushes_round_trip() {
    for q in [
        "/a(->e.($-::qty))+ | @. | product",
        "/a(->e .w($-))+",
        "(/employees.($-))+?",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // a glued named push after a named hop stays part of the name
    // (the /x.rs(...) rule): "e.q" becomes the matcher and the
    // parenthesized rest cannot parse as a group — the spaced form
    // is the push
    assert!(expand("/a(->e.q($-))+", &Defs::default()).is_err());
}

#[test]
fn arrived_edges_round_trip() {
    for q in [
        "/a->e | .(@-::qty)",
        "/a->e | .(@-)",
        "/a | rec(::x, 'l', @-)",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a | .(@-:::depth)").contains("plain properties"));
}

#[test]
fn context_and_piped_round_trip() {
    for q in [
        "/a | .p(((::x div (@*::x @| sum)) * 100 | round))",
        "/a | rec(::n, 'all', (@*:::name @| sort @| join(', ')))",
        "/a[@* = null]",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a | .x((@* | .bad))").contains("real capsae"));
}

#[test]
fn conditional_round_trip() {
    for q in [
        "/a | rec(::n, 'era', (::y < 2000 ? 'old' : 'new'))",
        "/a[(::x ? 1 : 2) = 1]",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // chains normalize to explicit nesting, and the normal form is
    // a fixpoint
    assert_eq!(
        canon("/a | (::x < 2 ? 'lo' : ::x < 9 ? 'mid' : 'hi')"),
        "/a | (::x < 2 ? 'lo' : (::x < 9 ? 'mid' : 'hi'))"
    );
    assert!(refuse("/a | (::x ? 'lo')").contains("':'"));
}

#[test]
fn trait_algebra_round_trip() {
    for q in [
        "/a<code>",
        "/a<block || inline>",
        "/a<Person && !Employee>",
        "/a<!*>",
        "/a<(a || b) && !c>",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // stacking still parses; canonical form is one bracket
    assert_eq!(canon("/a<x><y>"), "/a<x && y>");
    // distribution: OR over AND normalizes to CNF
    assert_eq!(canon("/a<(x && y) || z>"), "/a<(x || z) && (y || z)>");
    // double negation eliminates
    assert_eq!(canon("/a<!!x>"), "/a<x>");
}

#[test]
fn spread_round_trip() {
    for q in [
        "/a->e | @-::roles | ... | ...",
        "/a | (@*::x | ...) @| count",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // `each` is gone: it parses as an unknown function now
    assert!(refuse("/a | @-::x | each").contains("each"));
}

#[test]
fn map_pipe_round_trip() {
    for q in [
        "/a | @-::roles $| upper",
        "/a | @. $| [$_ > 1] $| [1..2]",
        "/a | .p((@. $| $_ * 2))",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a | @. $| .bad").contains("real capsae"));
}

#[test]
fn defs_substitute_into_groups() {
    // a fragment parameter inside a grouped step's predicate
    let src = "def &chain($n): /employees(/reports[::id = $n])+; &chain(7)";
    assert_eq!(canon(src), "/employees(/reports[::id = 7])+");
}

#[test]
fn headless_arrow_round_trips() {
    // `--` — the either-direction crosslink — parses in path,
    // group, and predicate-operand position, and reprints as
    // itself. Glued after a row name it is still the hop, like
    // `->` (the name lexer breaks on the dash run).
    assert_eq!(canon("/posts/5--ip"), "/posts/5--ip");
    assert_eq!(
        canon("/cookies/c1(--cookie--ip--ip--cookie)+::id"),
        "/cookies/c1(--cookie--ip--ip--cookie)+::id"
    );
    assert_eq!(canon("/a(--*--*)+::name"), "/a(--*--*)+::name");
    assert_eq!(canon("/ips/*[--ip]::id"), "/ips/*[--ip]::id");
    // A dash-leading matcher keeps a separating space on reprint,
    // so the axis survives the round trip.
    assert_eq!(canon("/a-- -b"), "/a-- -b");
}

#[test]
fn resolution_respell_canonicalizes() {
    // `-->` / `<--` are canonical; the tilde forms remain accepted
    // and canonicalize on unparse, like `;;;` to `::::`.
    assert_eq!(canon("/tracks/1::album_id~>::title"), "/tracks/1::album_id-->::title");
    assert_eq!(canon("/tracks/1::album_id-->::title"), "/tracks/1::album_id-->::title");
    assert_eq!(canon("/loan::book~>shelf"), "/loan::book-->shelf");
    assert_eq!(canon("/page::id<~"), "/page::id<--");
    assert_eq!(canon("/page::id<--cite"), "/page::id<--cite");
    // the digit guard holds: `<-3` is still a comparison shape
    assert_eq!(canon("/x[::a <-3]"), "/x[::a < -3]");
}

#[test]
fn tail_colon_aliases_canonicalize() {
    // `:-` and `:--` — the tail-colon spellings — are permanent
    // typing-friendly aliases of `->` and `-->`.
    assert_eq!(canon("/employees/6(:-manager_id)+::name"), "/employees/6(->manager_id)+::name");
    assert_eq!(canon("/tracks/1::album_id:--::title"), "/tracks/1::album_id-->::title");
    assert_eq!(canon("/a:-b"), "/a->b");
    // a conditional's glued else-negative is untouched
    assert_eq!(
        canon("/x | (::a ?= 1 ? 2 :-3)"),
        canon("/x | (::a ?= 1 ? 2 : -3)")
    );
}
