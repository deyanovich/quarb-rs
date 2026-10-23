//! `project(…)` is retired at 0.42.0 (spec: Projectors): the
//! projection operator — `v:%(…)`, `t:#(:A; :B)`, `v:$.p` — and `| %`
//! for a node's record take its place; the name refuses with that
//! pointer.

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"a":{"x":1}}"#).unwrap()
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

#[test]
fn project_is_retired_with_the_pointer_at_the_projection_operator() {
    for q in ["/a | project(::x)", "/a | project(%(n = ::x); -r)", "= %(a = 1) | project(:a)"] {
        let e = refuse(q);
        assert!(e.contains("'project(…)' is retired") && e.contains("v:%(…)") && e.contains("| %"), "{q}: {e}");
    }
}
