//! Structural expansion (design paper: a body may answer in
//! structure): a macro's final context of reflected nodes or quarqs
//! grafts where the invocation stood, a quarq argument mounts as its
//! content, and a parameter may stand in command position.

use quarb::{Defs, expand, expand_first};

fn exp(q: &str) -> String {
    expand(q, &Defs::default()).unwrap()
}

fn err(q: &str) -> String {
    expand(q, &Defs::default()).unwrap_err().to_string()
}

#[test]
fn a_node_answered_grafts_its_subtree() {
    // the identity macro: the parameter node is the whole argument,
    // predicate and all (the text form lost it)
    assert_eq!(
        exp("macro &same($p): /p; &same(/row[::x > 1]) | ::y"),
        "/row[::x > 1] | ::y"
    );
    // a predicate answered mid-walk refines the step before it
    assert_eq!(
        exp("macro &guard($p): /p//predicate; /row&guard(/a[::x > 1])::y"),
        "/row[::x > 1]::y"
    );
    // a step answered continues the walk; several steps are
    // consecutive hops in answer order
    assert_eq!(
        exp("macro &steps($p): /p//step; /root&steps(/a/b)::n"),
        "/root/a/b::n"
    );
    // a command answered after a pipe is that command
    assert_eq!(
        exp("macro &first($p): /p//cmd[1]; /row | &first(q(| count | upper))"),
        "/row | count"
    );
    // a projection answered in operand position is the projection
    assert_eq!(
        exp("macro &key($p): /p//projection; /row[&key(::age) > 3]"),
        "/row[::age > 3]"
    );
}

#[test]
fn a_quarq_answered_grafts_its_content() {
    // a template with parameters: the argument's form where the
    // parameter stands, the quarq's commands where $then stands
    assert_eq!(
        exp(
            "macro &on-shelf($shelf; $then): q(/row[::shelf = $shelf] $then); \
             &on-shelf(\"A1\"; q(| ::title))"
        ),
        "/row[::shelf = \"A1\"] | ::title"
    );
    // after the aggregate pipe the quarq's command is the aggregate
    assert_eq!(
        exp(
            "def &adults: /row[::age >= 18]; macro &wrap($agg): q(&adults @| $agg); \
             &wrap(q(count))"
        ),
        "/row[::age >= 18] @| count"
    );
    // a quarq answered after a pipe: its commands
    assert_eq!(
        exp("macro &tail: q(| ::name | upper); /row | &tail"),
        "/row | ::name | upper"
    );
    // a quarq in operand position: its expression
    assert_eq!(
        exp("macro &three: q(1 + 2); /row[::n > &three]"),
        "/row[::n > 1 + 2]"
    );
    // one answer per thread, grafted in thread order: each
    // projection node a hop after the pipe
    assert_eq!(
        exp("macro &cols(@c): /c/*; /row | &cols(::a, ::b)"),
        "/row | ::a | ::b"
    );
}

#[test]
fn a_parameter_in_command_position_takes_a_quarq() {
    let e = err("macro &on($then): q(/row $then); &on(5)");
    assert!(e.contains("takes a path") && e.contains("q(| …)"), "{e}");
    // a path argument continues the walk instead (`^/row$col`)
    assert_eq!(
        exp("macro &on($then): q(/row $then); &on(::title)"),
        "/row::title"
    );
    assert_eq!(
        exp("macro &sum($col): q(| .t(^/row$col @| sum) | $.t); /row | &sum(::year)"),
        "/row | .t·1(^/row::year @| sum) | $.t·1"
    );
    // `| $then` alone after a pipe: the quarq's commands
    assert_eq!(
        exp("macro &then($then): q(| .a(1) | $then); /row | &then(q(| .b(2) | %.))"),
        "/row | .a·1(1) | .b(2) | %."
    );
}

#[test]
fn a_quarq_argument_mounts_as_its_content() {
    // the body walks the quarq's commands, not a literal of type quarq
    assert_eq!(
        exp("macro &m($t): /t/pipeline/cmd; /row | &m(q(| count))"),
        "/row | count"
    );
    // ::form on the parameter is the content's text, pipe and all
    assert_eq!(
        exp("macro &m($t): /t | ::form; /row | &m(q(| count))"),
        "/row | count"
    );
}

#[test]
fn structural_answers_are_checked_by_kind() {
    // structure and text do not mix
    let e = err("macro &mix(@c): /c/* | ($ord = 1 ? q(| count) : \"| upper\"); /row | &mix(1, 2)");
    assert!(e.contains("structure or text, not both"), "{e}");
    // a pipeline answered at a path position
    let e = err("macro &p: q(| count); &p | ::x");
    assert!(e.contains("pipeline fragment"), "{e}");
    // two branches answered at a path position
    let e = err("macro &two($a; $b): (/a | /b); &two(/x, /y) | count");
    assert!(e.contains("branch-shaped"), "{e}");
    // a step answered after a pipe
    let e = err("macro &st($p): /p//step; /row | &st(/a)");
    assert!(e.contains("after a pipe"), "{e}");
    // two values in operand position
    let e = err("macro &vals($p): /p//literal; /row[::n > &vals(1 + 2)]");
    assert!(e.contains("operand position"), "{e}");
    // a text answer keeps its meaning: parsed as query
    assert_eq!(
        exp("macro &t: ^ | \"/row[::x > 1]\"; &t | ::y"),
        "/row[::x > 1] | ::y"
    );
}

#[test]
fn expand_1_prints_a_structural_answer() {
    let steps = expand_first(
        "macro &same($p): /p; macro &on($then): q(| $then); \
         &same(/row[::x > 1]) | &on(q(| count))",
        &Defs::default(),
    )
    .unwrap();
    assert_eq!(steps, vec!["/row[::x > 1]", "| count"]);
}
