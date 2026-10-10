//! `@| ngrams(n)` / `bigrams` / `trigrams`: the adjacent n-windows
//! of the stream, full windows only, each a list in stream order —
//! the tokenization chosen upstream. `@| frequencies[(k)]`: each
//! distinct value with its count, most frequent first.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"words": ["the", "cat", "sat", "on", "the", "cat", "the", "end"],
            "short": ["one", "two"],
            "n": [3, 1, 3, 2, null, 1, 3]}"#,
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
fn ngrams_are_the_adjacent_full_windows_in_stream_order() {
    assert_eq!(values("/short/* | :: @| bigrams"), ["one, two"]);
    assert_eq!(values("/short/* | :: @| trigrams @| count"), ["0"]);
    assert_eq!(
        values("/words/* | :: @| ngrams(3) @| first"),
        ["the, cat, sat"]
    );
    assert_eq!(values("/words/* | :: @| bigrams @| count"), ["7"]);
    // Repeats stay repeats: the stream is not a set.
    assert_eq!(
        values("/words/* | :: @| bigrams | join(\" \") @| frequencies(1)"),
        ["%(value = \"the cat\"; n = 2)"]
    );
}

#[test]
fn frequencies_count_distinct_values_most_frequent_first() {
    assert_eq!(
        values("/words/* | :: @| frequencies(3)"),
        [
            "%(value = \"the\"; n = 3)",
            "%(value = \"cat\"; n = 2)",
            "%(value = \"end\"; n = 1)",
        ]
    );
    assert_eq!(values("/words/* | :: @| frequencies @| count"), ["5"]);
    // Nulls are left out; equal counts order by value.
    assert_eq!(
        values("/n/* | :: @| frequencies"),
        [
            "%(value = 3; n = 3)",
            "%(value = 1; n = 2)",
            "%(value = 2; n = 1)"
        ]
    );
    assert_eq!(values("/words/* | :: @| frequencies(2) | :n @| sum"), ["5"]);
}

#[test]
fn the_text_function_is_unchanged() {
    assert_eq!(
        values("/words/* | :: @| join(\" \") | ngrams(2) @| first"),
        ["the cat"]
    );
}

#[test]
fn a_rank_range_or_a_negative_rank_selects_rows_of_the_table() {
    // the 3, cat 2, end 1, on 1, sat 1
    assert_eq!(
        values("/words/* | :: @| frequencies(2..3)"),
        ["%(value = \"cat\"; n = 2)", "%(value = \"end\"; n = 1)"]
    );
    assert_eq!(
        values("/words/* | :: @| frequencies(-2)"),
        ["%(value = \"on\"; n = 1)", "%(value = \"sat\"; n = 1)"]
    );
    assert_eq!(
        values("/words/* | :: @| frequencies(-3..-2)"),
        ["%(value = \"end\"; n = 1)", "%(value = \"on\"; n = 1)"]
    );
    assert_eq!(
        values("/words/* | :: @| frequencies(4..) | :value"),
        ["on", "sat"]
    );
    assert_eq!(
        values("/words/* | :: @| frequencies(..1) | :value"),
        ["the"]
    );
    assert_eq!(values("/words/* | :: @| frequencies(9..) @| count"), ["0"]);
}

#[test]
fn first_last_top_and_bottom_take_a_rank_range_too() {
    assert_eq!(values("/words/* | :: @| first(2..3)"), ["cat", "sat"]);
    assert_eq!(values("/words/* | :: @| first(-2)"), ["the", "end"]);
    // `last` counts from the end: ranks 2..3 are the two before
    // the last, in stream order; a negative rank from the start.
    assert_eq!(values("/words/* | :: @| last(2..3)"), ["cat", "the"]);
    assert_eq!(values("/words/* | :: @| last(-2)"), ["the", "cat"]);
    assert_eq!(values("/words/* | :: @| last(2)"), ["the", "end"]);
    assert_eq!(values("/words/* | :: @| first"), ["the"]);
    // 3, 1, 3, 2, null, 1, 3: descending 3 3 3 2 1 1.
    assert_eq!(values("/n/* | :: @| top(2..3; $_)"), ["3", "3"]);
    assert_eq!(values("/n/* | :: @| top(-2; $_)"), ["1", "1"]);
    assert_eq!(values("/n/* | :: @| bottom(3..4; $_)"), ["2", "3"]);
    assert_eq!(values("/n/* | :: @| top(3; $_)"), ["3", "3", "3"]);
}

#[test]
fn jaccard_takes_the_other_set_from_any_list_value() {
    // {the, cat, sat, on, end} against {the, cat, dog}: 2 of 6.
    let third = |vs: Vec<String>| {
        let v: f64 = vs[0].parse().unwrap();
        (v - 1.0 / 3.0).abs() < 1e-9
    };
    assert!(third(values(
        "/words/* | .b(@(\"the\"; \"cat\"; \"dog\")) | :: @| jaccard($.b)"
    )));
    assert!(third(values(
        "/words/* | :: @| jaccard(@(\"the\"; \"cat\"; \"dog\"))"
    )));
    // A sub-pipeline's values: the other text's distinct words.
    assert_eq!(
        values("/words/* | :: @| jaccard((^/short/* | :: @| unique))"),
        ["0"]
    );
    assert_eq!(
        values("/words/* | :: @| unique @| jaccard((^/words/* | :: @| unique))"),
        ["1"]
    );
}
