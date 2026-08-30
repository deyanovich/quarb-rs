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
        "/a(/b){1;3}",
        "/a(/b){2;}",
        "/a(/b)+?",
        "/a(/b)+!",
        "/a(/b){1;3}?",
        "/a(->mgr)+!",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // {2;2} normalizes to {2}
    assert_eq!(canon("/a(/b){2;2}"), "/a(/b){2}");
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
    assert_eq!(canon("/*[(::a = 1 || ::b)]"), "/*[(::a = 1 || ::b)]");
    assert_eq!(canon("/*[(::a = 1 || ::b)]"), "/*[(::a = 1 || ::b)]");
    assert_eq!(canon("/*[(!::a && ::b)]"), "/*[(!::a && ::b)]");
    for q in [
        "//order::amt <=>? //user[::id = _::uid] | ...?",
        "//u <=> //v <=>? //order[::uid = $$1::id]",
        "//commit[::::short = ^/tags/*::::short]",
        "/movie .m <-ACTED_IN[::born > $$.m::released] | %(::name; $$.m::title)",
        "/c/* | (::kind ?= \"a\" ? 1 : (/^b/) ? 2 : 0)",
        "/tags/* | .t(:::name) | `cloc --git ${$.t}` | %($.t; lines = $_)",
        "/x::v | base64 | decode(\"base64\")",
        "/x::v @| sort(\"ru-RU\")",
        "/a/*[^//error] | %(n = :::name; of = (^//*.rs @| count))",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // a value group still computes
    assert_eq!(canon("/a | (::x + 1) * 2"), "/a | (::x + 1) * 2");
}

#[test]
fn pattern_errors() {
    assert!(refuse("(/a){3;2}").contains("max below min"));
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
        "/a->*[$- = \"e\"]",
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
        "(/employees .($-))+[::name = \"Meg\"]?",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a(/b)+[1]").contains("expression predicates only"));
    assert!(refuse("/a(/b)+[1..2]").contains("expression predicates only"));
}

#[test]
fn pattern_pushes_round_trip() {
    for q in [
        "/a(->e .($-::qty))+ | @. | product",
        "/a(->e .w($-))+",
        "(/employees .($-))+?",
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
        "/a | %(::x; l = @-)",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a | .(@-:::depth)").contains("plain properties"));
}

#[test]
fn context_and_piped_round_trip() {
    for q in [
        "/a | .p(((::x div (@*::x @| sum)) * 100 | round))",
        "/a | %(::n; all = (@*:::name @| sort @| join(\", \")))",
        "/a[@* = null]",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    assert!(refuse("/a | .x((@* | .bad))").contains("real capsae"));
}

#[test]
fn conditional_round_trip() {
    for q in [
        "/a | %(::n; era = (::y < 2000 ? \"old\" : \"new\"))",
        "/a[(::x ? 1 : 2) = 1]",
    ] {
        assert_eq!(canon(q), q, "not a fixpoint: {q}");
    }
    // chains normalize to explicit nesting, and the normal form is
    // a fixpoint
    assert_eq!(
        canon("/a | (::x < 2 ? \"lo\" : ::x < 9 ? \"mid\" : \"hi\")"),
        "/a | (::x < 2 ? \"lo\" : (::x < 9 ? \"mid\" : \"hi\"))"
    );
    assert!(refuse("/a | (::x ? \"lo\")").contains("':'"));
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
    assert_eq!(canon("/tracks/1::album_id-->::title"), "/tracks/1::album_id-->::title");
    assert_eq!(canon("/tracks/1::album_id-->::title"), "/tracks/1::album_id-->::title");
    assert_eq!(canon("/loan::book-->shelf"), "/loan::book-->shelf");
    assert_eq!(canon("/page::id<--"), "/page::id<--");
    assert_eq!(canon("/page::id<--cite"), "/page::id<--cite");
    // the digit guard holds: `<-3` is still a comparison shape
    assert_eq!(canon("/x[::a <-3]"), "/x[::a < -3]");
}

#[test]
fn rounded_predicate_alias_canonicalizes() {
    // `(?…)` — the rounded predicate — is a permanent
    // typing-friendly alias of `[…]` in every bracket position:
    // predicate, index, range, map-pipe filter and slice. An
    // index is a positional predicate — one bracket family, so
    // one alias covers it.
    assert_eq!(
        canon("//user(?::age >= 18)(?1)->profile/name"),
        canon("//user[::age >= 18][1]->profile/name")
    );
    assert_eq!(canon("/a(?::x = \"y\")"), "/a[::x = \"y\"]");
    assert_eq!(canon("/a(?2..3)"), "/a[2..3]");
    assert_eq!(canon("/a(?-1)"), "/a[-1]");
    assert_eq!(canon("/a $| (?::n > 2)"), canon("/a $| [::n > 2]"));
    assert_eq!(canon("/a $| (?..3)"), canon("/a $| [..3]"));
    // inner parens balance independently of the predicate pair
    assert_eq!(
        canon("/a(?(::x = 1 || ::y = 2) && ::z = 3)"),
        canon("/a[(::x = 1 || ::y = 2) && ::z = 3]")
    );
    // the two spellings nest freely, and a regex body stays raw
    assert_eq!(canon("/a[::x = 1](?::y = 2)"), "/a[::x = 1][::y = 2]");
    assert!(refuse("/~((?i)bob)(?::x = 1)").contains("retired"));
    // the pair does not mix spellings
    assert!(refuse("/a(?::x = 1]").contains("closes with ')'"));
    // and the pointy family is untouched: a group stays a group
    assert_eq!(canon("/a(/b|/c)"), "/a(/b|/c)");
}

#[test]
fn rounded_family_canonicalizes() {
    // The second slice of the rounded family (ruling #40): every
    // alias canonicalizes to its pointy form.
    // reverse arrows
    assert_eq!(canon("/page::id--:cite"), "/page::id<--cite");
    assert_eq!(canon("/a-:b"), "/a<-b");
    assert_eq!(canon("/a(-:b)+::name"), canon("/a(<-b)+::name"));
    // the edge accessors are untouched, in either spelling
    assert_eq!(canon("/a:-b(?$-::w > 1)"), canon("/a->b[$-::w > 1]"));
    // sibling hops and reaches
    assert_eq!(canon("/a;-b"), canon("/a>b"));
    assert_eq!(canon("/a-;b"), canon("/a<b"));
    assert_eq!(canon("/a;;-?b"), canon("/a>>?b"));
    assert_eq!(canon("/a-;;!b"), canon("/a<<!b"));
    // the sibling alias names the hop only — never the comparison
    assert!(!refuse("/x[::a ;- 3]").is_empty());
    // ascent
    // the rounded ascent is a hop in name position: the parent, the
    // ancestors, then whatever follows as its own element
    assert_eq!(canon("/a/b/../c"), canon("/a/b\\/c"));
    assert_eq!(canon("/a/b/.../c"), canon("/a/b\\\\/c"));
    assert_eq!(canon("/a/b/...?/c"), canon("/a/b\\\\?/c"));
    assert_eq!(canon("/a/b/...!/c"), canon("/a/b\\\\!/c"));
    // traits
    assert_eq!(
        canon("//user(:admin && !banned)::name"),
        canon("//user<admin && !banned>::name")
    );
    assert_eq!(canon("//user(:admin)(?::age > 18)"), canon("//user<admin>[::age > 18]"));
    assert_eq!(canon("//x(:.custom)"), canon("//x<.custom>"));
    // a group opening on a projection, or on the :- arrow, stays a group
    assert_eq!(canon("/a(?(::x = 1 || ::y = 2))"), "/a[(::x = 1 || ::y = 2)]");
    assert_eq!(canon("/e(:-manager_id)+::name"), "/e(->manager_id)+::name");
    // quantifier
    assert_eq!(canon("/a(/b)(+2;3)"), "/a(/b){2;3}");
    assert_eq!(canon("/a(/b)(+2;2)"), "/a(/b){2}");
    assert_eq!(canon("/a(/b)(+2;)"), "/a(/b){2;}");
    // the appendix example, rounded to pointy
    assert_eq!(
        canon("//user(:admin)(?::age >= 18)(?1):-profile/name"),
        canon("//user<admin>[::age >= 18][1]->profile/name")
    );
}

#[test]
fn root_anchor_and_comparison_words_canonicalize() {
    // Ruling #41 (respelled by #43): `(())` is the root anchor's
    // rounded spelling …
    assert_eq!(canon("(())/a/b::x"), canon("^/a/b::x"));
    assert_eq!(canon("(())//x"), canon("^//x"));
    assert_eq!(canon("/a[::x = (())/set/*::x]"), canon("/a[::x = ^/set/*::x]"));
    assert_eq!(canon("(()) | count"), canon("^ | count"));
    // … but a glued pair is a call's argument list, untouched
    assert_eq!(canon("/x[::t < now()]"), canon("/x[::t < now()]"));
    // … and the spelled comparisons — Latin, French, Russian, Greek —
    // canonicalize to the symbols
    assert_eq!(canon("/u[::age .minor. 18]"), "/u[::age < 18]");
    assert_eq!(canon("/u[::age .nonmaior. 18]"), "/u[::age <= 18]");
    assert_eq!(canon("/u[::age .maior. 18]"), "/u[::age > 18]");
    assert_eq!(canon("/u[::age .nonminor. 18]"), "/u[::age >= 18]");
    assert_eq!(canon("/u[::age .inf. 18]"), "/u[::age < 18]");
    assert_eq!(canon("/u[::age .nonsup. 18]"), "/u[::age <= 18]");
    assert_eq!(canon("/u[::age .sup. 18]"), "/u[::age > 18]");
    assert_eq!(canon("/u[::age .noninf. 18]"), "/u[::age >= 18]");
    // a glued word is a name, never an operator
    assert_eq!(canon("/u[::age.minor.18]"), "/u[::age.minor.18]");
    assert_eq!(canon("/u(?::age .менее. 18)"), "/u[::age < 18]");
    assert_eq!(canon("/u(?::age .неболее. 18)"), "/u[::age <= 18]");
    assert_eq!(canon("/u(?::age .более. 18)"), "/u[::age > 18]");
    assert_eq!(canon("/u(?::age .неменее. 18)"), "/u[::age >= 18]");
    assert_eq!(canon("/u(?::age .μικρότερο. 18)"), "/u[::age < 18]");
    assert_eq!(canon("/u(?::age .τοπολύ. 18)"), "/u[::age <= 18]");
    assert_eq!(canon("/u(?::age .μεγαλύτερο. 18)"), "/u[::age > 18]");
    assert_eq!(canon("/u(?::age .τουλάχιστον. 18)"), "/u[::age >= 18]");
    assert_eq!(canon("/u(?::age .τουλαχιστον. 18)"), "/u[::age >= 18]");
    // the full rounded query, once more, with a word comparison
    assert_eq!(
        canon("(())//user(:admin)(?::age .nonminor. 18)(?1):-profile/name"),
        canon("^//user<admin>[::age >= 18][1]->profile/name")
    );
}

#[test]
fn boolean_words_canonicalize() {
    // Ruling #42: `and` / `or` / `not` in each language of the
    // rounded family, all canonicalizing to the symbols.
    let pointy = canon("/u[::a = 1 && ::b = 2 || !::c]");
    for q in [
        "/u[::a = 1 .and. ::b = 2 .or. .not. ::c]",
        "/u[::a = 1 .et. ::b = 2 .vel. .non. ::c]",
        "/u[::a = 1 .et. ::b = 2 .ou. .non. ::c]",
        "/u(?::a = 1 .y. ::b = 2 .o. .no. ::c)",
        "/u(?::a = 1 .и. ::b = 2 .или. .не. ::c)",
        "/u(?::a = 1 .και. ::b = 2 .ή. .όχι. ::c)",
        "/u(?::a = 1 .και. ::b = 2 .η. .οχι. ::c)",
    ] {
        assert_eq!(canon(q), pointy, "{q}");
    }
    // the words are loose like `not`: they negate a whole condition
    assert_eq!(canon("/u(?.не. ::age .nonminor. 18)"), canon("/u[!::age >= 18]"));
    // English joins the comparison words (the Fortran heritage)
    assert_eq!(canon("/u[::age .lt. 18 .or. ::age .ge. 65]"), canon("/u[::age < 18 || ::age >= 65]"));
    assert_eq!(canon("/u[::age .gt. 18 .and. ::age .le. 65]"), canon("/u[::age > 18 && ::age <= 65]"));
    // Spanish joins the comparison words
    assert_eq!(canon("/u[::age .menor. 18 .o. ::age .nomenor. 65]"), canon("/u[::age < 18 || ::age >= 65]"));
    assert_eq!(canon("/u[::age .mayor. 18 .y. ::age .nomayor. 65]"), canon("/u[::age > 18 && ::age <= 65]"));
    // a quoted field keeps a word as its name
    assert!(canon("/u[::\"и\" = 1]").contains("и"));
}

#[test]
fn rounded_pipes_registers_anchors_canonicalize() {
    // Ruling #43. The pipe family: `__` the bar lying down, `*__`
    // all-then, `,__` each-then.
    assert_eq!(canon("/a __ count"), canon("/a | count"));
    assert_eq!(canon("/a::x *__ max"), canon("/a::x @| max"));
    assert_eq!(canon("/a::x ,__ (?(_) .maior. 2)"), canon("/a::x $| [$_ > 2]"));
    assert_eq!(canon("/a __ ..."), canon("/a | ..."));
    assert_eq!(canon("/a __ ...?"), canon("/a | ...?"));
    // the correlation, mirrored like the arrows
    assert_eq!(
        canon("//user :=: //order(?::uid = _::id)"),
        canon("//user <=> //order[::uid = _::id]")
    );
    assert_eq!(
        canon("//order::amt :=:? //user(?::id = _::uid) __ ...?"),
        canon("//order::amt <=>? //user[::id = _::uid] | ...?")
    );
    // the value side: single parens
    assert_eq!(canon("/a ,__ (?(_) .maior. 2)"), canon("/a $| [$_ > 2]"));
    assert_eq!(canon("/a .s /b __ (.s)"), canon("/a .s /b | $.s"));
    assert_eq!(canon("/a . /b . __ (.2)"), canon("/a . /b . | $.2"));
    assert_eq!(canon("/a :-b(?(-)::w .maior. 1)"), canon("/a->b[$-::w > 1]"));
    assert_eq!(
        canon("/users/* :=: /orders/*(?/uid:: = _/id::) __ %(::name; amt = ((1))/amt::)"),
        canon("/users/* <=> /orders/*[/uid:: = _/id::] | %(::name; amt = $$1/amt::)")
    );
    // the node side: double parens are canonical, single parens the
    // deprecated alias — and `(.)` is now the register file
    assert_eq!(canon("/a . /b .m | $$.1/c | $$./d | ((@))/e | ((@m))/f"), "/a . /b .m | $$.1/c | $$./d | ((@))/e | ((@m))/f");
    // the single-paren anchors are retired (the value side): refused
    // with a pointer at the double form
    let e = quarb::expand("/a . /b .m | (@)/e", &quarb::Defs::default()).unwrap_err().to_string();
    assert!(e.contains("((@))"), "{e}");
    let e = quarb::expand("/a . /b .m | (@m)/f", &quarb::Defs::default()).unwrap_err().to_string();
    assert!(e.contains("((@m))"), "{e}");
    // `(N)` is the match capture `$N` — a value; the mark at N is `((N))`
    assert_eq!(
        canon("/row __ (?::name == (/^(\\w+), (\\w+)/)) __ %(k = (1); j = (2))"),
        canon("/row | [::name == (/^(\\w+), (\\w+)/)] | %(k = $1; j = $2)")
    );
    assert_eq!(canon("/a .s /b | ((*s))::x"), canon("/a .s /b | ((@s))::x"));
    assert_eq!(canon("$$.m/address"), "$$.m/address");
    // the single-paren anchor is retired: single parens are the value
    // side, and the refusal points at the double form
    let e = quarb::expand("/a .m /b[::x = (m)::y]", &quarb::Defs::default()).unwrap_err().to_string();
    assert!(e.contains("$$.m"), "{e}");
    let e = quarb::expand("/a | . | (@)::y", &quarb::Defs::default()).unwrap_err().to_string();
    assert!(e.contains("((@))"), "{e}");
}

#[test]
fn regex_literal_canonicalizes() {
    // Ruling #44: `(/…/)` is the regex literal, canonical in both
    // positions; `~(…)` is a deprecated alias, the bare `/…/` after
    // `=~` / `!~` sugar, and `== (/…/)` a match by the pattern
    // doctrine.
    assert_eq!(canon("//(/^ch/)"), "//(/^ch/)");
    assert_eq!(canon("//(/^ch/)"), "//(/^ch/)");
    assert_eq!(canon("/u[::name == (/^bob/i)]"), "/u[::name == (/^bob/i)]");
    assert_eq!(canon("/u[::name == (/^bob/i)]"), "/u[::name == (/^bob/i)]");
    assert_eq!(canon("/u(?::name == (/^bob/i))"), "/u[::name == (/^bob/i)]");
    assert_eq!(canon("/u[::name !== (/x/)]"), "/u[::name !== (/x/)]");
    assert_eq!(canon("/u[::name !== (/x/)]"), "/u[::name !== (/x/)]");
    assert_eq!(canon("/u[::name == \"x\"]"), "/u[::name == (/x/)]");
    assert!(refuse("/u[::name = (/x/)]").contains("retired"));
    // a slash in the body escapes on the way out
    assert_eq!(canon("//(/a\\/b/)"), "//(/a\\/b/)");
    // a dynamic right operand — a path — prints under `==` too;
    // a bare `/…/` after `==` is that path, never a regex literal
    assert_eq!(canon("/u[::name == ::pat]"), "/u[::name == ::pat]");
    assert_eq!(canon("/u[::name == /p/q::re]"), "/u[::name == /p/q::re]");
    assert_eq!(canon("/u[::name !== $.re]"), "/u[::name !== $.re]");
    // the value-match arms take the literal too
    assert_eq!(
        canon("/c/* | (::kind ?= \"a\" ? 1 : (/^b/) ? 2 : 0)"),
        "/c/* | (::kind ?= \"a\" ? 1 : (/^b/) ? 2 : 0)"
    );
    // a group ending in a hop named x is still a group
    assert_eq!(canon("/a(/src/x)+"), "/a(/src/x)+");
}

#[test]
fn boolean_words_in_traits_canonicalize() {
    // Ruling #42, completed: the trait algebra takes the words.
    let pointy = canon("//user<admin && !banned || staff>::name");
    for q in [
        "//user<admin && !banned || staff>::name",
        "//user(:admin .et. .non. banned .vel. staff)::name",
        "//user(:admin .et. .non. banned .ou. staff)::name",
        "//user(:admin .y. .no. banned .o. staff)::name",
        "//user(:admin .и. .не. banned .или. staff)::name",
        "//user(:admin .και. .όχι. banned .ή. staff)::name",
    ] {
        assert_eq!(canon(q), pointy, "{q}");
    }
    assert_eq!(canon("/div(:(a .ou. b) .et. .non. c)"), canon("/div<(a || b) && !c>"));
    // the bare foreign words are no longer keywords: `et` is a name
    assert!(quarb::expand("/u[::a = 1 et ::b = 2]", &quarb::Defs::default()).is_err());
    // a lone word is still a trait name
    assert_eq!(canon("/x<and>"), "/x<and>");
    assert_eq!(canon("/x<и>"), "/x<и>");
}

#[test]
fn record_field_colon_canonicalizes() {
    // Ruling #48: `:name` is a record's field — the bottom rung of
    // the colon ladder; `%+` the named-captures record.
    assert_eq!(canon("/a | .r | $.r:x"), "/a | .r | $.r:x");
    assert_eq!(canon("/a | .r | (.r):x"), "/a | .r | $.r:x");
    assert_eq!(canon("/a | $.r:a:b"), "/a | $.r:a:b");
    assert_eq!(canon("/a | :n"), "/a | :n");
    assert_eq!(canon("/a | $_:n"), "/a | :n");
    assert_eq!(canon("/a | [:n .maior. 1]"), canon("/a | [$_:n > 1]"));
    assert_eq!(canon("/a | %+"), "/a | %+");
    assert_eq!(canon("/a | %+:year"), "/a | %+:year");
    assert_eq!(canon("/a | [%+:year .maior. 2000]"), "/a | [%+:year > 2000]");
    // a node has properties, not fields
    assert!(refuse("/user/*:name").contains("::name"));
    // the else colon is spaced; glued, it reads as a field
    assert_eq!(canon("/x | (::a ?= 1 ? 2 : 0)"), "/x | (::a ?= 1 ? 2 : 0)");
    assert!(refuse("/x | (::a ?= 1 ? 2 :zero)").contains("':'"));
}

#[test]
fn keyed_aggregates_refuse_the_plain_pipe() {
    // A keyed aggregate ranks a context: `@|`, or per group after
    // `@| group`; alone on the plain pipe it is refused, loudly.
    assert!(refuse("/a | top(3, ::x)").contains("@| top"));
    assert!(refuse("/a | sort_by(::x)").contains("@| sort_by"));
    assert!(canon("/a @| top(3, ::x)").contains("@| top"));
    assert!(canon("/a @| group(::k) | top(3, ::x)").contains("| top"));
    assert!(canon("/a *__ top(3, ::x)").contains("@| top"));
    // a field of the topic record is a key like any other
    for q in [
        "/a | %(::n, \"t\", ::t) @| sort_by(:t)",
        "/a | %(::n, \"t\", ::t) @| unique_by(:t)",
        "/a | %(::n, \"t\", ::t) @| min_by(:t)",
        "/a | %(::n, \"t\", ::t) @| max_by(:t)",
        "/a | %(::n, \"t\", ::t) @| top(2, :t)",
        "/a | %(::n, \"t\", ::t) @| group(:t) | top(1, :n)",
    ] {
        assert!(canon(q).contains("(:"), "{q}: {}", canon(q));
    }
}

#[test]
fn record_push_canonicalizes() {
    // Ruling #49: the record push in one step, anonymous or named,
    // plain or enriched; the sigil spelling is canonical.
    assert_eq!(canon("/a | .%(::x, \"n\", 1)"), "/a | .%(::x; n = 1)");
    assert_eq!(canon("/a | .r%(::x)"), "/a | .r%(::x)");
    assert_eq!(canon("/a | .r%%(::x)"), "/a | .r%%(::x)");
    assert_eq!(canon("/a | .r%(::x) | $.r:x"), "/a | .r%(::x) | $.r:x");
    assert!(refuse("/a | .r%").contains("record push"));
    // a push returns the thread to navigation mode, as any push does
    assert_eq!(canon("/a | .r%(::x) | /b"), "/a | .r%(::x) | /b");
}

#[test]
fn operand_paths_ascend_and_step_sideways() {
    // A path operand may ascend and step sideways, as a branch may.
    assert_eq!(canon("/a/b[\\*::x = 1]"), "/a/b[\\::x = 1]");
    assert_eq!(canon("/a/b[\\\\?c::x = 1]"), "/a/b[\\\\?c::x = 1]");
    // (a bare key is a string; canonical output quotes it)
    assert_eq!(canon("/p | %(dir; \\*:::name; me; ::city)"), "/p | %(dir = \\:::name; me = ::city)");
    assert_eq!(canon("/a[;-*::x = 1]"), "/a[>*::x = 1]");
    assert_eq!(canon("/a[>>?*::x = 1]"), "/a[>>?*::x = 1]");
    assert_eq!(canon("/a | %(up; /..::n)"), "/a | %(up = \\::n)");
}

#[test]
fn record_convention_canonicalizes() {
    // Ruling #50: `%(k = v; k2 = v2)` — the kaiv form — is the
    // record's canonical spelling; keys bare when identifiers.
    assert_eq!(canon("/a | %(n = ::age; city = /profile::city)"), "/a | %(n = ::age; city = /profile::city)");
    // the flat Perl-list form and the comma both parse, and canonicalize
    assert_eq!(canon("/a | %(n = ::age; city = /profile::city)"), "/a | %(n = ::age; city = /profile::city)");
    assert_eq!(canon("/a | %(n = ::age, city = /profile::city)"), "/a | %(n = ::age; city = /profile::city)");
    assert_eq!(canon("/a | %(n; ::age)"), "/a | %(n = ::age)");
    // auto-named values mix with keyed ones
    assert_eq!(canon("/a | %(::name; kb = ::size)"), "/a | %(::name; kb = ::size)");
    // a non-identifier key quotes; a value may be a comparison
    assert_eq!(canon("/a | %(\"my-key\" = 1; adult = ::age >= 18)"), "/a | %(\"my-key\" = 1; adult = (::age >= 18))");
    assert_eq!(canon("/a | %(and = 1)"), "/a | %(and = 1)");
    // an identifier in any script is a bare key
    assert_eq!(canon("/a | %(имя = ::имя; 名前 = 1)"), "/a | %(имя = ::имя; 名前 = 1)");
    // the same convention in the enriched view, the record push, and group
    assert_eq!(canon("/a | %%(k = 2)"), "/a | %%(k = 2)");
    assert_eq!(canon("/a | .r%(k = ::x; j = 1)"), "/a | .r%(k = ::x; j = 1)");
    assert_eq!(canon("/a @| group(k = ::x; ::y)"), "/a @| group(k = ::x; ::y)");
    assert_eq!(canon("/a @| group(\"k\", ::x)"), "/a @| group(k = ::x)");
}

#[test]
fn list_literal_canonicalizes() {
    // Ruling #52: `@(a; b)` is the list literal; the comma also
    // separates on input; items are operands.
    assert_eq!(canon("/a | @(1; 2; 3)"), "/a | @(1; 2; 3)");
    assert_eq!(canon("/a | @(1, 2, 3)"), "/a | @(1; 2; 3)");
    assert_eq!(canon("/a | @()"), "/a | @()");
    assert_eq!(canon("/a | %(tags = @(::x; \"y\"); n = 1)"), "/a | %(tags = @(::x; \"y\"); n = 1)");
    assert_eq!(canon("/a | @(/tags/*::)"), "/a | @(/tags/*::)");
    // the expression head canonicalizes as `^ | …` (exprhead.rs)
    assert_eq!(canon("= @(1; 2) | count"), "^ | @(1; 2) | count");
    // the rounded spelling `*(…)` — only where a value can begin
    assert_eq!(canon("/a | *(1; 2)"), "/a | @(1; 2)");
    assert_eq!(canon("/a[::x = *(1; 2)]"), "/a[::x = @(1; 2)]");
    assert_eq!(canon("/*(?::x = 1)"), "/*[::x = 1]");
}

#[test]
fn decimal_comma_and_group_underscores_canonicalize() {
    // The decimal comma and `_` groups canonicalize to the plain
    // spelling; `1,5` is one argument, `1, 5` two.
    assert_eq!(canon("= 1,5 + 1"), canon("= 1.5 + 1"));
    assert_eq!(canon("= 1_000_000 + 1"), canon("= 1000000 + 1"));
    assert_eq!(canon("= 1_000,5"), canon("= 1000.5"));
    assert_eq!(canon("/a | %(a = 1,5; b = 2)"), "/a | %(a = 1.5; b = 2)");
    assert_eq!(canon("/a | @(1,5)"), "/a | @(1.5)");
    assert_eq!(canon("/a | @(1, 5)"), "/a | @(1; 5)");
    let e = quarb::expand("= 1,000,000", &quarb::Defs::default()).unwrap_err().to_string();
    assert!(e.contains("1_000_000"), "{e}");
    // a quantifier's bounds are integers: no ambiguity; the semicolon
    // is canonical, the regex comma sugar
    assert_eq!(canon("/a(->b){2;5}"), "/a(->b){2;5}");
    assert_eq!(canon("/a(->b)(+2;5)"), "/a(->b){2;5}");
    // the union word follows the glyph into branch position
    assert_eq!(canon("//p .or. //q"), "//p || //q");
    assert_eq!(canon("//p .или. //q | ::n"), "//p || //q | ::n");
    // the rounded shell literal prints as the backtick
    assert_eq!(canon("/f/* | (;wc -l)"), "/f/* | `wc -l`");
    assert_eq!(canon("/a(->b){2;}"), "/a(->b){2;}");
    assert_eq!(canon("/a(->b)(+2;)"), "/a(->b){2;}");
    // the join binds where it is written: the driver's stages before
    // `<=>` stay before it, and a served-node stage prints bare
    assert_eq!(canon("/a | .t(::x) <=> /b[::y = $.t] | %(::z)"), "/a | .t(::x) <=> /b[::y = $.t] | %(::z)");
    assert_eq!(canon("/a <=> /b[::y = _::t] | .t(::x)"), "/a <=> /b[::y = _::t] | .t(::x)");
    assert_eq!(canon("/a | _::x | _/kid::y"), "/a | _::x | _/kid::y");
    // the parent and the ancestors take no name: bare is canonical,
    // the star accepted; the rounded ascent is a hop in name position
    assert_eq!(canon("/a/b\\*/d::"), "/a/b\\/d::");
    assert_eq!(canon("/a/b\\::e"), "/a/b\\::e");
    assert_eq!(canon("/a/b\\\\*/x"), "/a/b\\\\/x");
    assert_eq!(canon("/a/b\\\\?::e"), "/a/b\\\\?::e");
    assert_eq!(canon("/a/b/../d::"), "/a/b\\/d::");
    assert_eq!(canon("/a/b/..::e"), "/a/b\\::e");
    assert_eq!(canon("/a/b/..->lbl"), "/a/b\\->lbl");
    assert_eq!(canon("/a/b/.../x"), "/a/b\\\\/x");
    assert_eq!(canon("/a/b/...?::e"), "/a/b\\\\?::e");
    assert_eq!(canon("/a/b/...!::e"), "/a/b\\\\!::e");
    assert_eq!(canon("/a/.git/..x"), "/a/\".git\"/\"..x\"");
    assert_eq!(canon("/a/b\\x"), "/a/b\\x");
}

#[test]
fn rounded_hole_canonicalizes() {
    // `(,expr)` in a double-quoted string is the rounded `${expr}`;
    // canonical text prints the pointy hole.
    assert_eq!(canon(r#"/r | "(,::name) is (,::age)""#), canon(r#"/r | "${::name} is ${::age}""#));
    assert_eq!(canon(r#"/r | ::f | .f | "fare (,(.f)), doubled (,(_) * 2)""#), canon(r#"/r | ::f | .f | "fare ${$.f}, doubled ${$_ * 2}""#));
    assert!(canon(r#"/r | "(,::name)""#).contains("${::name}"));
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
