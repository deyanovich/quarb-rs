//! The reflection vocabulary lock (v1).
//!
//! A corpus spanning the whole syntax surface is reflected, and the
//! resulting inventory — every node kind with its property keys —
//! plus the spelling sets (axes, operators, matcher kinds, ...) must
//! match the locked vocabulary exactly. New syntax that grows the
//! vocabulary must extend both this test and the spec's normative
//! table in the same change; anything else failing here is an
//! accidental compatibility break.

use quarb::reflect::QueryArbor;
use quarb::{AstAdapter, Value};
use std::collections::BTreeSet;

/// Every construct the reflector can emit, at least once.
const CORPUS: &[&str] = &[
    // descendant axis, traits, expr/index/range predicates, leaf
    // anchor, adapter-metadata projection
    "//book<block||inline>[::pages > 200][2][1..3]$;;;id",
    // the trait boolean algebra: &&, tight !, parens (CNF inside)
    "//sec<(a||b) && !deprecated>",
    // union branches, scalar + aggregate stages, literal args
    "/a || /b::x | upper @| join(', ')",
    // the axis zoo (child, both reaches each way, siblings — one
    // step and the reach family — links)
    "/a//b//?c//!d\\e\\\\f\\\\?g\\\\!h>i<j->k<-l/*",
    "/a>>b>>?c>>!d<<e<<?f<<!g",
    // glob and regex matchers
    "/src/*.rs//~(^ch[0-9]+$)",
    // resolution axes: with a hint, and reverse without
    "/loan::book~>shelf/page::id<~",
    // core metadata, and/or/not, a bare truthy operand
    "/x[:::index = 2 and not (::a = 1 or ::b)]",
    // comparison zoo
    "/x[::a != 1][::b < 2][::c <= 3][::d >= 4][::e *= 'q'][::f !~ /z/]",
    // literal types
    "/x[::a = true][::b = null][::c = 1.5]",
    // arithmetic zoo, parenthesized value group, unary minus
    "/x | ::a - ::b | ::c div ::d | ::e idiv ::f | (:::index + 1) * 3",
    "/x[- :::index = -2]",
    // pushes (named + bare), computed columns (named + bare),
    // register recalls, topic, ordinal filter, positional selection
    "/x | .n(::a * ::b) | $.n | .named | . | .(::a + 1) \
     | [$ord mod 2 = 1] @| [2..-1] @| [3]",
    "/x | $_ | @. | %. | $. | $.2",
    // subcontexts, named and bare
    "/users/* | .total(/orders/*/amt:: @| sum) | .(//y @| count)",
    // correlation: roles, context refs with and without index
    "//user <=> //order[::uid = $*1::id and ::amt > $*::limit]::",
    // match captures
    "/row | [::name =~ /^(\\w+), (\\w+)/] | rec('k', $1, 'j', $2)",
    // window spans: closed, open-start, open-end; keyed; shift
    "/row | ::fare @| window(-2..0, ::class) | mean @| shift(1)",
    "/row | ::fare @| window(..0) | sum @| window(0..) | max",
    // fragments reflect expanded
    "def &adults: /row[::age >= 18]; &adults @| count",
    // interpolated strings: text segments and holes
    "/row | \"${::name} is ${::age}\"",
    // the root anchor, semantic in subcontext bodies
    "/row | .t(^/row @| count) | $.t",
    // outer-scope operands (correlated subqueries)
    "/row | .d(::dept) | .m(^/row[::dept = $$.d][:::index = $$ord] | $$_)",
    // path patterns: grouping, alternation, nesting, + quantifier
    "//div(/ul/li|/ol/li|/dl(/dt|/dd))+",
    // dot wildcard, {m,n}, proximal suffix; a group in a predicate
    // path; bare-operator sugar (reflects as a dot group)
    "/a(/.){2,3}?/b[(->ref)+]/{2}",
    // quantified crosslink, open max, distal suffix; tolerated form
    "//user(->manager_id){2,}! || //body/(p|div)",
    // the arrived-by edge: bare and projected, in edge-hop predicates
    "/a->e[$-::qty > 1][$- = e]",
    // pattern pushes (breadcrumbs): expression and sub-query bodies,
    // bare and named — reflecting with the pipeline push kinds
    "/a(->e.($-::qty) .w(//x @| count))+ | @. | product",
    // group predicates: filtered before reach (targeted proximal)
    "/a(->e)+[::q > 1][$- = e]?",
    // the arrived-edges plural: bare and projected
    "/a->e | rec(@-, @-::qty)",
    // the context operand and an inline pipe tail
    "/a | .p((::x div (@*::x @| sum) | round))",
    // the conditional, chained
    "/a | rec(::n, 'bin', (::x < 2 ? 'lo' : ::x < 9 ? 'mid' : 'hi'))",
    // the spread
    "/a->e | @-::roles | ... | ...",
    // the outer spread and the outer correlation (2026-07-11)
    "//user <=>? //order[::uid = $*1::id]::amt | ...?",
    // the anchored operand (2026-07-11)
    "//commit[;;;short = ^/tags/*;;;short]",
    // marks and the (name) anchor (2026-07-11)
    "/movie .m <-ACTED_IN[::born > (m)::released] | rec(::name, (m)::title)",
    // the branch-position mark anchor, inside a subcontext body
    "/movie .m /cast | .n((m)/tags @| count) | %.",
    // the value match, equality and regex arms (2026-07-11)
    "/c/* | (::kind ?= 'a' ? 1 : ~(^b) ? 2 : 0)",
    // the shell stage, backtick-sugared (2026-07-11; reflects as
    // an ordinary func named sh)
    "/files/* | `wc -l`",
    // the map pipe: transform, filter, slice within the topic
    "/a | @-::roles $| upper $| [$_ != 'x'] $| [1..2]",
    // now() and call operands (2026-07-12, the duration-parsing
    // round; f(x, args) reflects as its (x | f(args)) desugaring)
    "/log/*[/at:: > now() - 12h] | (td(5d3h5min)) | (tp('02/15/2024', '%m/%d/%Y'))",
    // navigation stages (2026-07-23, pipeline navigation): plain,
    // projected, root- and mark-anchored, quantified — a branch in
    // stage position, wrapped in the nav kind
    "/teams/* | .team(::name) | /members/* | .who(::name) | %.",
    "/a .m | /b | ^/c:::name | . | (m)/d(->e)+::x",
];

/// kind → sorted property keys, as locked. `param` is reserved
/// (never emitted: reflection sees expanded queries).
const VOCABULARY: &[(&str, &[&str])] = &[
    ("agg", &["name"]),
    // v1 additive growth: alt, group (2026-07-08, path patterns).
    ("alt", &[]),
    ("and", &[]),
    ("arith", &["op"]),
    // v1 additive growth: mark key (2026-07-11, the (name) anchor).
    ("branch", &["anchored", "mark"]),
    // v1 additive growth: capsae, piped (2026-07-11, @* + inline
    // pipes).
    ("capsae", &[]),
    ("capture", &["group"]),
    ("compare", &["op"]),
    // v1 additive growth: cond (2026-07-11, the conditional).
    ("cond", &[]),
    ("context", &["index"]),
    // v1 additive growth: edge (2026-07-09, the $- accessor).
    ("edge", &[]),
    // v1 additive growth: edges (2026-07-11, the @- plural).
    ("edges", &[]),
    ("expr", &[]),
    ("expr-push", &["name"]),
    ("filter", &[]),
    ("func", &["name"]),
    // v1 additive growth: group (2026-07-08, path patterns).
    ("group", &["max", "min", "reach"]),
    // v1 additive growth: interp (2026-07-07, interpolation).
    ("interp", &[]),
    ("literal", &["type", "value"]),
    // v1 additive growth: map (2026-07-11, the $| pipe).
    ("map", &[]),
    // v1 additive growth: mark (2026-07-11, the mark store).
    ("mark", &["name"]),
    // v1 additive growth: match/when (2026-07-11, the value
    // match).
    ("match", &[]),
    // v1 additive growth: nav (2026-07-23, pipeline navigation —
    // a branch in stage position; its contents reflect with the
    // ordinary branch kinds).
    ("nav", &[]),
    ("neg", &[]),
    ("not", &[]),
    // v1 additive growth: now (2026-07-12, the invocation instant;
    // call operands reflect as their (x | f(args)) desugaring).
    ("now", &[]),
    ("or", &[]),
    ("ordinal", &[]),
    // v1 additive growth: outer (2026-07-07, correlated subqueries).
    ("outer", &[]),
    ("parens", &[]),
    // v1 additive growth: anchored key (2026-07-11, ^-operands);
    // mark key same day (the (name) anchor).
    ("path", &["anchored", "mark"]),
    ("piped", &[]),
    ("pipeline", &[]),
    ("predicate", &["from", "kind", "to", "value"]),
    ("projection", &["key", "kind"]),
    ("push", &["name"]),
    ("query", &["outer", "role"]),
    ("recall", &["ref"]),
    ("select", &[]),
    ("span", &["from", "kind", "to"]),
    // v1 additive growth: spread (2026-07-11, | ... core syntax);
    // outer key added same day (| ...?, the OPTIONAL MATCH round).
    ("spread", &["outer"]),
    (
        "step",
        &[
            "axis",
            "hint",
            "leaf",
            "matcher",
            "matcher-kind",
            "property",
        ],
    ),
    ("subcontext", &["name"]),
    ("topic", &[]),
    ("trait", &["alts"]),
    // v1 additive growth: when (2026-07-11, the value match's arm).
    ("when", &["regex"]),
];

fn corpus_inventory() -> Vec<(String, Vec<String>)> {
    let mut union: Vec<(String, Vec<String>)> = Vec::new();
    for q in CORPUS {
        let arbor = QueryArbor::parse(q).unwrap_or_else(|e| panic!("corpus query {q:?}: {e}"));
        for (kind, keys) in arbor.inventory() {
            let entry = match union.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, ks)) => ks,
                None => {
                    union.push((kind, Vec::new()));
                    &mut union.last_mut().expect("just pushed").1
                }
            };
            for key in keys {
                if !entry.contains(&key) {
                    entry.push(key);
                }
            }
        }
    }
    union.sort();
    for (_, keys) in &mut union {
        keys.sort();
    }
    union
}

#[test]
fn vocabulary_is_locked() {
    let locked: Vec<(String, Vec<String>)> = VOCABULARY
        .iter()
        .map(|(k, ps)| (k.to_string(), ps.iter().map(|p| p.to_string()).collect()))
        .collect();
    assert_eq!(
        corpus_inventory(),
        locked,
        "the reflection vocabulary drifted from the v1 lock: \
         extend the corpus/lock table and the spec's normative \
         table together, additively"
    );
}

/// Property values that tooling matches on are spellings; their sets
/// are part of the lock.
#[test]
fn spellings_are_locked() {
    let collect = |kind: &str, key: &str| -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for q in CORPUS {
            let arbor = QueryArbor::parse(q).unwrap();
            let mut stack = vec![arbor.root()];
            while let Some(n) = stack.pop() {
                stack.extend(arbor.children(n));
                if arbor.name(n).as_deref() == Some(kind)
                    && let Some(v) = arbor.property(n, key)
                {
                    out.insert(v.to_string());
                }
            }
        }
        out
    };
    let set =
        |items: &[&str]| -> BTreeSet<String> { items.iter().map(|s| s.to_string()).collect() };
    assert_eq!(
        collect("step", "axis"),
        set(&[
            "/", "//", "//?", "//!", "\\", "\\\\", "\\\\?", "\\\\!", ">", "<", ">>", ">>?", ">>!",
            "<<", "<<?", "<<!", "->", "<-", "~>", "<~",
        ])
    );
    assert_eq!(
        collect("compare", "op"),
        set(&["=", "!=", "<", "<=", ">", ">=", "=~", "!~", "*="])
    );
    assert_eq!(
        collect("arith", "op"),
        set(&["+", "-", "*", "div", "idiv", "mod"])
    );
    assert_eq!(
        collect("step", "matcher-kind"),
        set(&["name", "glob", "regex", "any", "dot"])
    );
    assert_eq!(collect("group", "reach"), set(&["?", "!"]));
    assert_eq!(
        collect("projection", "kind"),
        set(&["property", "core", "adapter"])
    );
    assert_eq!(
        collect("predicate", "kind"),
        set(&["expr", "index", "range"])
    );
    assert_eq!(collect("span", "kind"), set(&["range"]));
    assert_eq!(
        collect("literal", "type"),
        set(&["bool", "float", "int", "null", "text"])
    );
}

#[test]
fn root_carries_the_version() {
    let arbor = QueryArbor::parse("/x").unwrap();
    assert_eq!(
        arbor.metadata(arbor.root(), "vocabulary"),
        Some(Value::Int(1))
    );
    assert_eq!(
        arbor.metadata(arbor.root(), "source"),
        Some(Value::Str("/x".to_string()))
    );
}
