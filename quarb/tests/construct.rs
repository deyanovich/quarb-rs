//! The syntax library (design paper, draft 5: the constructors): a
//! node built by vocabulary, and the bottom-up rebuild of a whole
//! tree by forking and a fixpoint.

use quarb::{Defs, expand};

fn exp(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn err(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn a_constructor_builds_a_node_by_vocabulary() {
    // a step from its axis and matcher
    assert_eq!(
        exp("macro &hop: ^ | syntax:step(\"//\"; \"row\"); /a&hop | count"),
        "/a//row | count"
    );
    // a comparison over two operands, a predicate over it
    assert_eq!(
        exp(
            "macro &old: ^ | syntax:predicate(syntax:compare(\">\"; q(::age); q(30))); /row&old::name"
        ),
        "/row[::age > 30]::name"
    );
    // a projection by its key, after a pipe
    assert_eq!(
        exp("macro &key($k): /k/literal | syntax:projection(::value); /row | &key(\"name\")"),
        "/row | ::name"
    );
    // the generic form: a kind's name over a record of properties
    assert_eq!(
        exp(
            "macro &deep: ^ | syntax:node(\"step\"; %(axis = \"//\"; matcher = \"*\")); /a&deep | count"
        ),
        "/a//* | count"
    );
    // like a node, over other children: the step keeps its kind and
    // properties, its predicate replaced
    assert_eq!(
        exp(
            "macro &swap($p; $q): /p//step | syntax:node($_; ^/q//predicate); &swap(/row[::a = 1]; /x[::b = 2]) | count"
        ),
        "/row[::b = 2] | count"
    );
}

#[test]
fn a_constructor_is_checked_against_the_vocabulary() {
    let e = err("macro &bad: ^ | syntax:node(\"step\"; %(colour = 1)); /a&bad | count");
    assert!(e.contains("step node has no property 'colour'"), "{e}");
    let e = err("macro &bad: ^ | syntax:compare(\">\"; q(::age)); /a&bad | count");
    assert!(
        e.contains("compare node holds two operands; 1 given"),
        "{e}"
    );
    let e = err("macro &bad: ^ | syntax:projection(\"x\"; \"y\"; q(::age)); /a&bad | count");
    assert!(
        e.contains("no projection kind") || e.contains("holds no children"),
        "{e}"
    );
    let e = err("macro &bad: ^ | syntax:node(\"quarq\"); /a&bad | count");
    assert!(e.contains("not built by a constructor"), "{e}");
}

#[test]
fn a_tree_is_rebuilt_bottom_up_one_node_per_thread() {
    // the ninth draft: one thread per node, each attaching its node
    // built anew — itself (the literal with its value moved by ten)
    // over its children's built nodes, read across the capsa by the
    // node they stand at — once every child has built; the
    // quantified push repeats until nothing changes, and the root's
    // regula is the whole query
    let later = "macro &later($p): \
                 /p//* \
                 | .:b(([!/*[!:.b]] \
                        | syntax:node(([:name = \"literal\"] | $_ ++! %(value = ::value + 10)) ?? $_; \
                                      /*:.b)) ?? null){1;} \
                 | ^/p/*:.b @| first;";
    assert_eq!(
        exp(&format!("{later} &later(/row[::year > 1810]::title)")),
        "/row[::year > 1820]::title"
    );
    // the node itself, in a value, is the quarq of its form
    assert_eq!(
        exp("macro &same($p): /p/* | $_; &same(/row[::x]::y) | count"),
        "/row[::x]::y | count"
    );
}

#[test]
fn a_quarq_child_takes_parameters_and_expansions() {
    // a quarq among a constructor's operands is read like any quarq
    // in the body: parameters substituted, expansions filled
    assert_eq!(
        exp(
            "macro &over($col; $n): ^ | syntax:predicate(syntax:compare(\">\"; q($col); q(${$n}))); \
             /row&over(::Age; 60) @| count"
        ),
        "/row[::Age > 60] @| count"
    );
    assert_eq!(
        exp(
            "macro &over($col; $n): ^ | syntax:predicate(syntax:compare(\">\"; q($col); q(${$n * 2}))); \
             /row&over(::Age; 30) @| count"
        ),
        "/row[::Age > 60] @| count"
    );
}

#[test]
fn a_trait_is_a_form_and_has_a_constructor() {
    // a trait block built, and set under a step among its children
    assert_eq!(
        exp("macro &lk: ^ | syntax:step(\"/\"; \"*\"; syntax:trait(\"link\")); /a&lk | count"),
        "/a/*<link> | count"
    );
    assert_eq!(
        exp(
            "macro &lk: ^ | syntax:step(\"/\"; \"*\"; syntax:trait(@(\"block\"; \"inline\"))); /a&lk | count"
        ),
        "/a/*<block || inline> | count"
    );
    // a reflected trait node is itself; a step rebuilt like itself over
    // its children keeps its trait and its predicate
    assert_eq!(
        exp("macro &m($q): /q//step | syntax:node($_; /*); &m(/a<link>[::x]) | count"),
        "/a<link>[::x] | count"
    );
    // the whole-query rebuild passes a trait leaf
    let same = "macro &same($p): \
                /p//* \
                | .:b(([!/*[!:.b]] | syntax:node($_; /*:.b)) ?? null){1;} \
                | ^/p/*:.b @| first;";
    assert_eq!(
        exp(&format!("{same} &same(/row<link>[::year > 1810]::title)")),
        "/row<link>[::year > 1810]::title"
    );
    // a quarq of a bare projection is the truth of its value
    assert_eq!(
        exp("macro &t: ^ | syntax:step(\"/\"; \"*\"; syntax:predicate(q(::x))); /a&t | count"),
        "/a/*[::x] | count"
    );
    // nothing to build like is said as such
    let e = err("macro &bad(@r): /r | syntax:node($_); &bad(1) | count");
    assert!(e.contains("nothing given to build like"), "{e}");
}
