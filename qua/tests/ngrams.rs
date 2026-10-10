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
