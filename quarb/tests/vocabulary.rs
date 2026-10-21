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
    "//book<block || inline>[::pages > 200][2][1..3]$:::ident",
    // the trait boolean algebra: &&, tight !, parens (CNF inside)
    "//sec<(a || b) && !deprecated>",
    // union branches, scalar + aggregate commands, literal args
    "/a || /b::x | upper @| join(\", \")",
    // the axis zoo (child, both reaches each way, siblings — one
    // step and the reach family — links)
    "/a//b//?c//!d\\e\\\\f\\\\?g\\\\!h>i<j->k<-l--m/*",
    "/a>>b>>?c>>!d<<e<<?f<<!g",
    // glob and regex matchers
    "/src/*.rs//(/^ch[0-9]+$/)",
    // resolution axes: with a reference label, and reverse without;
    // the reverse arrow's scope and label, the value match's target
    "/loan::book-->shelf/page<--(::id)",
    "/page<--cite(/a/*::id)",
    "/n::a=>(/s/*::b)",
    // core metadata, and/or/not, a bare truthy operand
    "/x[:index = 2 && !(::a = 1 || ::b)]",
    // comparison zoo
    "/x[::a != 1][::b < 2][::c <= 3][::d >= 4][::e *= \"q\"][::f !== (/z/)]",
    // literal types
    "/x[::a = true][::b = null][::c = 1.5]",
    // arithmetic zoo, parenthesized value group, unary minus
    "/x | ::a - ::b | ::c div ::d | ::e idiv ::f | (:index + 1) * 3",
    "/x[- :index = -2]",
    // pushes (named + bare), computed columns (named + bare),
    // register recalls, topic, ordinal filter, positional selection
    "/x | .n(::a * ::b) | $.n | .named | . | .(::a + 1) \
     | [$ord mod 2 = 1] @| [2..-1] @| [3]",
    "/x | $_ | @. | %. | %.* | $. | $.2",
    // v2 additive growth (2026-10-20, draft 163): the capsa counted
    // back on both faces, the cursor record, the context table, a
    // signed quantity literal
    "/x | .a | .b | $.-2 | $.. | (.-2) | (..) | _ | _:topic | _:position::k | #_ | (-1_km + 1_km)",
    "/x/* | .M | .N | ((.-2))/y | ((..)) | %(a = $$..::k; b = $$.-2::k) | #. | ##. | #.:M",
    // the node side of the capsa: marks as references, the node
    // views, the position as a value (`$$_`) and its tail
    "/x/* | .M | @@. | %%. | %%.* | %(me = $$_; k = $$_::k; up = $$__::k)",
    "/row | [::n == (/(?<y>\\d+)/)] | %+ | .% | $.y",
    // subcontexts, named and bare
    "/users/* | .total(/orders/*/amt:: @| sum) | .(//y @| count)",
    // correlation (driver-first): the ON clause references the
    // driver as `_::…`, earlier entries as `$*k`, the candidate
    // itself as `$*`; a second entry exercises the indexed ref
    "//user <=> //order[::uid = $$0::id && ::amt > $*::limit] <=> //note[::oid = $$1::id]",
    // match captures
    "/row | [::name == (/^(\\w+), (\\w+)/)] | %(k = $1; j = $2)",
    // window spans: closed, open-start, open-end; keyed; shift
    "/row | ::fare @| window(-2..0, ::class) | mean @| shift(1)",
    "/row | ::fare @| window(..0) | sum @| window(0..) | max",
    // fragments reflect expanded
    "def &adults: /row[::age >= 18]; &adults @| count",
    // interpolated strings: text segments and holes
    "/row | \"${::name} is ${::age}\"",
    // the root anchor, semantic in subcontext bodies
    "/row | .t(^/row @| count) | $.t",
    // the aggregate push, the aggregate record (enriched) and a
    // once-read register accessor after `@|`
    "/row | ::fare @| .total(count) @| %%(unique = unique @| count) @| $.total",
    // environment commands carry their alias as `lib`
    "/row | ::name | sh:tr(a-z; A-Z) @| go:sort(-r)",
    // a named option (the csv delimiter) reflects as an option node
    "/row | ::fare @| csv(\";\"; fare)",
    // outer-scope operands (correlated subqueries)
    "/row | .d(::dept) | .m(^/row[::dept = $.d][:index = $ord] | $$__::x | $$__/kid::y)",
    // path patterns: grouping, alternation, nesting, + quantifier
    "//div(/ul/li|/ol/li|/dl(/dt|/dd))+",
    // dot wildcard, {m,n}, proximal suffix; a group in a predicate
    // path; bare-operator sugar (reflects as a dot group)
    "/a(/.){2;3}?/b[(->ref)+]/{2}",
    // quantified crosslink, open max, distal suffix; tolerated form
    "//user(->manager_id){2;}! || //body/(p|div)",
    // the arrived-by edge: bare and projected, in edge-hop predicates
    "/a->e[$-::qty > 1][$- = \"e\"]",
    // pattern pushes (breadcrumbs): expression and sub-query bodies,
    // bare and named — reflecting with the pipeline push kinds
    "/a(->e .($-::qty) .w(//x @| count))+ | @. | product",
    // group predicates: filtered before reach (targeted proximal)
    "/a(->e)+[::q > 1][$- = \"e\"]?",
    // the arrived-edges plural: bare and projected
    "/a->e | %(@-; @-::qty)",
    // the context operand and an inline pipe tail
    "/a | .p((::x div (@@_::x @| sum) | round))",
    // the context family: the topics, the positions, the tables
    "/a/* | ::x | %(pct = $_ * 100 div sum(@_); who = @@_; n = (@_ @| count))",
    "/a/* | .k(::x) | .v(::x * 2) | #.*:k | ##.* | (*_) | ((*_))::x",
    // the conditional, chained
    "/a | %(::n; bin = (::x < 2 ? \"lo\" : ::x < 9 ? \"mid\" : \"hi\"))",
    // the spread
    "/a->e | @-::roles | ... | ...",
    // the outer spread and the outer correlation (2026-07-11;
    // driver-first since 2026-07-25)
    "//order::amt <=>? //user[::id = $$0::uid] | ...?",
    // the anchored operand (2026-07-11)
    "//commit[:::short = ^/tags/*:::short]",
    // marks and the (name) anchor (2026-07-11)
    "/movie .m <-ACTED_IN[::born > $$.m::released] | %(::name; $$.m::title)",
    // the branch-position mark anchor, inside a subcontext body
    "/movie .m /cast | .n($$.m/tags @| count) | %.",
    // the value match, equality and regex arms (2026-07-11)
    "/c/* | (::kind ?= \"a\" ? 1 : (/^b/) ? 2 : 0)",
    // the shell command, backtick-sugared (2026-07-11; reflects as
    // an ordinary func named sh)
    "/files/* | `wc -l`",
    // the value-side provenance projection (2026-09-06): `:key`
    // after a recall
    "/x | .v(::a) | $.v:%provenance:text",
    // the cross-capsa read (2026-09-07): a register accessor glued
    // to a node path reads the peer standing there
    "//x | .v(::a) | <-link:.v",
    // the quantified push (2026-09-07): the iteration loop
    "/a | .v(1) | .v($.v * 2){3} | .w($.v + 1){1;5}",
    // the node-id anchor and the shaped core keys (2026-09-07):
    // `((!N))` stands on a coordinate, `:%key` / `:@key` read
    // a key as a record / a list
    "/a | ((!3))::name | . | ((!3))/*:%provenance | . | $_:@origin | .k(((!3)):index + 1) | ((!$.k)):name",
    // the map pipe: transform, filter, slice within the topic
    "/a | @-::roles $| upper $| [$_ != \"x\"] $| [1..2]",
    // now() and call operands (2026-07-12, the duration-parsing
    // round; f(x, args) reflects as its (x | f(args)) desugaring)
    "/log/*[/at:: > now() - 12h] | (td(5d3h5min)) | (tp(\"02/15/2024\", \"%m/%d/%Y\"))",
    // navigation commands (2026-07-23, pipeline navigation): plain,
    // projected, root- and mark-anchored, quantified — a branch in
    // command position, wrapped in the nav kind
    "/teams/* | .team(::name) | /members/* | .who(::name) | %.",
    "/a .m | /b | ^/c:name | . | $$.m/d(->e)+::x",
    // the mark array (2026-07-24, marks symmetry): anonymous
    // marks, positional/recency/plural anchors — in branch and
    // in operand position
    "/a . /b .m | $$.1/c | $$./d | ((@))/e | ((@m))/f",
    "/a . /*[$$.2::x > 1][$$.::y = ((@))::z][((@m))::w]",
    // the record-field colon and the named-captures record
    // (rulings #47/#48): a field of the topic, of a register, of %+
    "/row | [::name == (/^(?<surname>[^,]+)/)] | %+:surname | [:n > 1] | .r | $.r:a:b | %+",
    // the record push, plain and enriched
    "/row | .r%(::x; n = 1) | .%%(k = 2) | $.r:n",
    // the list literal
    "/row | %(tags = @(::a; ::b); n = 1) | @(1; 2)",
    // concatenation (2026-10-20, draft 163): `++` merges by the
    // left side's shape, `++!` overwrites a record's fields (a row
    // on the right reflects as the record it spells), `+@+` appends
    // one item
    "/row | @(::a) ++ @(1) | ++! %(k = 2; ::b) | +@+ @(2; 3)",
    // the expression head (2026-10-21, draft 164): a literal or a
    // constructor after a pipe opens an expression; after `@|` it
    // is one expression over the context
    "/row | 0 + $_ | %(v = $_) ++ %(w = 1) @| @(0; $_) ++ @(1)",
];

/// kind → sorted property keys, as locked. `param` is reserved
/// (never emitted: reflection sees expanded queries).
const VOCABULARY: &[(&str, &[&str])] = &[
    ("agg", &["lib", "name"]),
    ("agg-expr", &[]),
    ("agg-push", &["name"]),
    ("agg-recall", &["ref"]),
    ("agg-record", &["enriched"]),
    // v1 additive growth: alt, group (2026-07-08, path patterns).
    ("alt", &[]),
    ("and", &[]),
    ("arith", &["op"]),
    // v1 additive growth: mark key (2026-07-11, the (name)
    // anchor); the mark-array anchor keys (2026-07-24, marks
    // symmetry: (N), (.), (@), (@name)).
    // v2 additive growth: mark-back (2026-10-20, draft 163: the
    // mark counted back from the latest, `$$.-N` / `$$..`).
    (
        "branch",
        &[
            "anchored",
            "mark",
            "mark-back",
            "mark-index",
            "mark-top",
            "marks-all",
            "marks-name",
            "node-id",
        ],
    ),
    // v1 additive growth: capsae, piped (2026-07-11, @_ + inline
    // pipes).
    ("capture", &["group"]),
    // v1 additive growth: captures, field (2026-08-27, rulings #47/#48).
    ("captures", &[]),
    ("cmd", &["lib", "name"]),
    ("compare", &["op"]),
    // v1 additive growth: cond (2026-07-11, the conditional).
    // v2 additive growth: concat (2026-10-20, draft 163: `++`,
    // `++!` with overwrite, `+@+` with item).
    ("concat", &["item", "overwrite"]),
    ("cond", &[]),
    ("context", &["index"]),
    // v1 additive growth: edge (2026-07-09, the $- accessor).
    ("edge", &[]),
    // v1 additive growth: edges (2026-07-11, the @- plural).
    ("edges", &[]),
    ("expr", &[]),
    ("expr-push", &["name"]),
    ("field", &["name"]),
    ("fields-push", &[]),
    ("filter", &[]),
    // v1 additive growth: group (2026-07-08, path patterns).
    ("group", &["max", "min", "reach"]),
    // v1 additive growth: interp (2026-07-07, interpolation).
    ("interp", &[]),
    // the invoking position `_` (2026-10-18; the node was spelled
    // outer from 2026-07-07 to draft 151, the row renamed with the
    // spec's term, the only non-additive change in the lock)
    ("invoking", &[]),
    ("list", &[]),
    ("literal", &["type", "value"]),
    // v1 additive growth: map (2026-07-11, the $| pipe).
    ("map", &[]),
    // v1 additive growth: mark (2026-07-11, the mark store).
    ("mark", &["name"]),
    // v1 additive growth: match/when (2026-07-11, the value
    // match).
    ("match", &[]),
    // v1 additive growth: nav (2026-07-23, pipeline navigation —
    // a branch in command position; its contents reflect with the
    // ordinary branch kinds).
    // v2 additive growth: the context family (2026-10-19, draft
    // 157): matrix (#* / ##*, ::side), positions (@@_), topics (@_).
    // v2 additive growth: view (2026-10-20, draft 163: `#.` the named
    // regulae, `#.*` every one; the context table `#_` a matrix of
    // the side `context`).
    ("matrix", &["side", "view"]),
    ("nav", &[]),
    ("neg", &[]),
    ("not", &[]),
    // v1 additive growth: now (2026-07-12, the invocation instant;
    // call operands reflect as their (x | f(args)) desugaring).
    ("now", &[]),
    ("option", &["name", "value"]),
    ("or", &[]),
    ("ordinal", &[]),
    ("parens", &[]),
    // v1 additive growth: anchored key (2026-07-11, ^-operands);
    // mark key same day (the (name) anchor); the mark-array
    // anchor keys (2026-07-24, marks symmetry).
    (
        "path",
        &[
            "anchored",
            "mark",
            "mark-back",
            "mark-index",
            "mark-top",
            "marks-all",
            "marks-name",
            "node-id",
        ],
    ),
    // v1 additive growth: peer-reg (2026-09-07, the cross-capsa read
    // `X:.r`).
    ("peer-reg", &["ref"]),
    ("piped", &[]),
    ("pipeline", &[]),
    // v2 additive growth: position (2026-10-18, the cursor `$$_`).
    ("position", &[]),
    ("positions", &[]),
    ("predicate", &["from", "kind", "to", "value"]),
    ("projection", &["key", "kind"]),
    ("push", &["name"]),
    ("query", &["outer", "role"]),
    ("recall", &["ref"]),
    ("record-push", &["enriched", "name"]),
    // v2 additive growth: record-with as an operand (2026-10-20,
    // draft 162: the cursor record `_`, reflected as the record of
    // the topic and the position).
    ("record-with", &[]),
    // v1 additive growth: repeat (2026-09-07, the quantified push —
    // the iteration loop).
    ("repeat", &["max", "min"]),
    ("seed", &[]),
    ("select", &[]),
    ("span", &["from", "kind", "to"]),
    // v1 additive growth: spread (2026-07-11, | ... core syntax);
    // outer key added same day (| ...?, the OPTIONAL MATCH round).
    ("spread", &["outer"]),
    (
        "step",
        &[
            "axis",
            "label",
            "leaf",
            "matcher",
            "matcher-kind",
            "property",
            "scope",
            "target",
        ],
    ),
    ("subcontext", &["name"]),
    ("topic", &[]),
    ("topics", &[]),
    ("trait", &["alts"]),
    // v1 additive growth: value-meta (2026-09-06, a value's `:key`
    // provenance projection).
    ("value-meta", &["key", "shape"]),
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
         extend the corpus/lock table && the spec's normative \
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
            "<<", "<<?", "<<!", "->", "<-", "--", "-->", "<--", "=>",
        ])
    );
    assert_eq!(
        collect("compare", "op"),
        set(&["=", "!=", "<", "<=", ">", ">=", "==", "!==", "*="])
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
        set(&["bool", "float", "int", "null", "quantity", "text"])
    );
}

#[test]
fn root_carries_the_version() {
    let arbor = QueryArbor::parse("/x").unwrap();
    assert_eq!(
        arbor.metadata(arbor.root(), "vocabulary"),
        Some(Value::Int(2))
    );
    assert_eq!(
        arbor.metadata(arbor.root(), "query"),
        Some(Value::Str("/x".to_string()))
    );
}
