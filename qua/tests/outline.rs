//! Printing an outline (lang/TODO.md): `indent` per value, and the
//! row-aligned context command `outline` over a stream of levels
//! (`@| number` is retired: `number` is the locale cast alone).

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

/// The refusal's text.
fn refused(q: &str) -> String {
    match quarb::run(q, &doc()) {
        Ok(_) => panic!("{q}: expected a refusal"),
        Err(e) => e.to_string(),
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
fn number_is_the_locale_cast_alone() {
    // the outline numbering of `@| number` is retired (ruling
    // 2026-11-03): `number` rides the plain pipe only
    let e = refused("/toc/* @| number(::depth)");
    assert!(e.contains("aggregate command 'number'"), "{e}");
    let e = refused("/toc/* | ::depth @| number");
    assert!(e.contains("aggregate command 'number'"), "{e}");
    assert_eq!(values(r#"= "1 234,5" | number(fr)"#), ["1234.5"]);
    // the text is optional: a level alone under --style is the
    // number-only line `@| number` printed
    assert_eq!(
        values(r#"/toc/* @| outline(::depth; --style="1.")"#),
        ["1", "1.1", "1.1.1", "1.2", "2"]
    );
    assert_eq!(
        values(r#"/toc/* | ::depth @| outline(--style="1.")"#),
        ["1", "1.1", "1.1.1", "1.2", "2"]
    );
    assert_eq!(
        values("/toc/* @| outline(::depth)"),
        ["", "  ", "    ", "  ", ""]
    );
}

#[test]
fn outline_indents_and_numbers() {
    assert_eq!(
        values("/toc/* @| outline(::depth; ::title)"),
        ["Book", "  One", "    Aside", "  Two", "Appendix"]
    );
    assert_eq!(
        values(r#"/toc/* @| outline(::depth; ::title; --style="1.")"#),
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
        values(r#"/deep/* @| outline(::d; ::t; --style="1.")"#),
        ["1 a", "    1.1.1 b", "  1.2 c"]
    );
}

#[test]
fn outline_commands_ride_the_context_pipe_only() {
    assert!(quarb::run("/toc/* | outline(::depth; ::title)", &doc()).is_err());
    assert!(quarb::run(r#"/toc/* @| outline(::depth; ::title; "i.")"#, &doc()).is_err());
}
