//! `@| csv` and `@| table` (lang/TODO.md): the whole stream as one
//! CSV document or one aligned table, with an optional column list.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"books":[{"title":"Emma","year":1815,"pages":474},
                     {"title":"Persuasion","year":1817},
                     {"title":"Lady Susan, a novel","year":1871,"pages":80}]}"#,
    )
    .unwrap()
}

fn one(q: &str) -> String {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => {
            assert_eq!(vs.len(), 1, "one document");
            vs[0].to_string()
        }
        _ => panic!("expected values"),
    }
}

#[test]
fn csv_is_one_document_with_its_header() {
    assert_eq!(
        one("/books/* | %(::title; ::year; ::pages) @| csv"),
        "title,year,pages\nEmma,1815,474\nPersuasion,1817,\n\"Lady Susan, a novel\",1871,80"
    );
}

#[test]
fn named_columns_select_and_order() {
    assert_eq!(
        one("/books/* | %(::title; ::year; ::pages) @| csv(year; title)"),
        "year,title\n1815,Emma\n1817,Persuasion\n1871,\"Lady Susan, a novel\""
    );
    // a column no record has is a column of empty cells
    assert_eq!(
        one("/books/* | %(::title) @| [..2] @| csv(title; isbn)"),
        "title,isbn\nEmma,\nPersuasion,"
    );
}

#[test]
fn table_aligns_under_a_rule() {
    assert_eq!(
        one("/books/* | %(::title; ::year) @| [..2] @| table"),
        "title       year\n----------  ----\nEmma        1815\nPersuasion  1817"
    );
    assert_eq!(
        one("/books/* | %(::title; ::year) @| [..2] @| table(year)"),
        "year\n----\n1815\n1817"
    );
}

#[test]
fn scalars_ride_a_value_column() {
    assert_eq!(one("/books/* | ::year @| csv"), "value\n1815\n1817\n1871");
}

#[test]
fn the_stages_ride_the_context_pipe_only() {
    assert!(quarb::run("/books/* | %(::title) | csv", &doc()).is_err());
    assert!(quarb::run("/books/* | %(::title) @| csv(::title)", &doc()).is_err());
}
