//! Draft 172: the name algebra. Parentheses in name position after
//! any navigation operator hold designators — names, globs, regex
//! literals — joined by `||`, `&&` and a leading `!`: `||` the
//! union of the classes, `&&` their intersection, `!` the complement
//! among the hop's own candidates, bare (`/(!id)` needs no wildcard
//! before it). A single `|` between whole subpaths stays the path
//! alternation; `(/…/)` stays the regex literal.

use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{
          "src": {"main.rs": 1, "lib.rs": 2, "test_a.rs": 3, "notes.txt": 4,
                  "util": {"a.rs": 5, "test_b.rs": 6}},
          "record": {"id": 7, "name": "x", "size": 8},
          "conf": {"a.json": 9, "b.yaml": 10, "c.toml": 11}
        }"#,
    )
    .unwrap()
}

fn names(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Nodes(ns) => {
            let d = doc();
            ns.iter()
                .map(|n| quarb::AstAdapter::name(&d, *n).unwrap_or_default())
                .collect()
        }
        QueryResult::Values(vs) => vs.iter().map(|v| v.display_form()).collect(),
        other => panic!("{q}: {other:?}"),
    }
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

fn canonical(q: &str) -> String {
    quarb::Query::parse(q, &doc()).unwrap().canonical()
}

#[test]
fn designators_join_by_union_intersection_and_complement() {
    assert_eq!(names("/src/(*.rs && !test_*)"), ["main.rs", "lib.rs"]);
    assert_eq!(names("//(*.json || *.yaml)"), ["a.json", "b.yaml"]);
    assert_eq!(names("/record/(!id)"), ["name", "size"]);
    assert_eq!(names("/record/(!id && !size)"), ["name"]);
    // the complement ranges over the hop's own candidates: the
    // descendants under `//`, so the containers are in
    assert_eq!(names("//(*.rs && !test_*)"), ["main.rs", "lib.rs", "a.rs"]);
    // regex designators, bare inside the algebra
    assert_eq!(
        names("/src/(/^[a-z]+\\.rs$/ && !/^test_/)"),
        ["main.rs", "lib.rs"]
    );
    assert_eq!(names("/record/(/^[a-z]/ && !/e$/)"), ["id"]);
    // precedence: `!` tightest, then `&&`, then `||`; parentheses nest
    assert_eq!(names("/src/(!*.rs || util && *)"), ["notes.txt", "util"]);
    assert_eq!(
        names("/src/(*.rs && (main* || lib*))"),
        ["main.rs", "lib.rs"]
    );
}

#[test]
fn the_parenthesis_stands_where_a_name_stands() {
    // and takes what a name takes: traits, predicates, further steps
    assert_eq!(names("/record/(!id)[:name = \"size\"]"), ["size"]);
    assert_eq!(names("/src/(!*.rs)/(!test_*)"), ["a.rs"]);
    // the sibling axes alike
    // the one next sibling is the hop's only candidate: `!` reads
    // among it
    assert_eq!(names("/src/lib.rs>(!main.rs)"), ["test_a.rs"]);
    assert_eq!(names("/src/main.rs>(!lib.rs)"), Vec::<String>::new());
    assert_eq!(names("/src/lib.rs<(*.rs && !test_*)"), ["main.rs"]);
    assert_eq!(names("/src/main.rs>>(!*.rs)"), ["notes.txt", "util"]);
    // the root is a candidate of the ancestors hop, unnamed, kept
    assert_eq!(names("/src/util/a.rs\\\\(!src)"), ["util", ""]);
}

#[test]
fn the_degenerate_cases() {
    // `(a || b)` is the path alternation `(a|b)`
    assert_eq!(names("/record/(id || size)"), names("/record/(id|size)"));
    // two concrete names intersect to nothing
    assert_eq!(names("/record/(id && size)"), Vec::<String>::new());
    // `(!*)` selects nothing
    assert_eq!(names("/record/(!*)"), Vec::<String>::new());
    // a single `|` between whole subpaths stays the path alternation
    assert_eq!(names("/(record/id | conf/a.json)"), ["id", "a.json"]);
    // `(/…/)` stays the regex literal: one regex, its slashes literal
    assert_eq!(names("/src/(/^test_.*\\.rs$/)"), ["test_a.rs"]);
}

#[test]
fn the_canonical_form_and_the_refusals() {
    assert_eq!(
        canonical("/src/(*.rs && !test_*)"),
        "/src/(*.rs && !test_*)"
    );
    assert_eq!(canonical("/record/(!id)"), "/record/(!id)");
    assert_eq!(canonical("//(*.json || *.yaml)"), "//(*.json || *.yaml)");
    assert_eq!(
        canonical("/src/(/^[a-z]/ && !/_test$/)"),
        "/src/(/^[a-z]/ && !/_test$/)"
    );
    // the algebra survives a round trip
    let q = canonical("/src/(!*.rs || util && (a || b))");
    assert_eq!(canonical(&q), q);
    let e = refuse("/src/(*.rs && !test_* lib.rs)");
    assert!(e.contains("joined by '&&' or '||'"), "{e}");
    let e = refuse("/src/(&& *.rs)");
    assert!(e.contains("a name algebra holds designators"), "{e}");
    let e = refuse("/src/(/^[a-z] && !test_*)");
    assert!(e.contains("closes with '/'"), "{e}");
}
