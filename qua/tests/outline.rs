//! Printing an outline (lang/TODO.md): `indent` per value, and the
//! row-aligned context stages `number` and `outline` over a stream
//! of levels.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"toc":[{"depth":1,"title":"Book"},
                   {"depth":2,"title":"One"},
                   {"depth":3,"title":"Aside"},
                   {"depth":2,"title":"Two"},
                   {"depth":1,"title":"Appendix"}],
            "deep":[{"d":3,"t":"a"},{"d":5,"t":"b"},{"d":4,"t":"c"}]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

#[test]
fn indent_prefixes_the_value_at_hand() {
    assert_eq!(
        values("/toc/* | ::title | indent(::depth)"),
        ["  Book", "    One", "      Aside", "    Two", "  Appendix"]
    );
    assert_eq!(
        values(r#"/toc/*[::depth = 2] | ::title | indent(1; "- ")"#),
        ["- One", "- Two"]
    );
}

#[test]
fn number_counts_per_level() {
    assert_eq!(
        values("/toc/* @| number(::depth)"),
        ["1", "1.1", "1.1.1", "1.2", "2"]
    );
    // without an argument the value at hand is the level
    assert_eq!(
        values("/toc/* | ::depth @| number"),
        ["1", "1.1", "1.1.1", "1.2", "2"]
    );
}

#[test]
fn outline_indents_and_numbers() {
    assert_eq!(
        values("/toc/* @| outline(::depth; ::title)"),
        ["Book", "  One", "    Aside", "  Two", "Appendix"]
    );
    assert_eq!(
        values(r#"/toc/* @| outline(::depth; ::title; "1.")"#),
        [
            "1 Book",
            "  1.1 One",
            "    1.1.1 Aside",
            "  1.2 Two",
            "2 Appendix"
        ]
    );
    // bare over a record stream: the fields depth and title
    assert_eq!(
        values("/toc/* | %(depth = ::depth; title = ::title) @| outline"),
        ["Book", "  One", "    Aside", "  Two", "Appendix"]
    );
}

#[test]
fn the_shallowest_level_is_the_top_and_a_skipped_one_counts_once() {
    assert_eq!(
        values(r#"/deep/* @| outline(::d; ::t; "1.")"#),
        ["1 a", "    1.1.1 b", "  1.2 c"]
    );
}

#[test]
fn outline_stages_ride_the_context_pipe_only() {
    assert!(quarb::run("/toc/* | outline(::depth; ::title)", &doc()).is_err());
    assert!(quarb::run("/toc/* @| outline(::depth)", &doc()).is_err());
    assert!(quarb::run(r#"/toc/* @| outline(::depth; ::title; "i.")"#, &doc()).is_err());
}
