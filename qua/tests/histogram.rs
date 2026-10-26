//! `@| bins(n)` and `@| histogram(n; --width=w)`: the numbers of a
//! stream binned as a table, and the stream drawn as bars.
use quarb::QueryResult;

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"n":[1, 2, 2, 3, 10, null],
            "s":["a", "b", "a", "c", "a", "b", null],
            "c":[3, 1, 3, 2, 3],
            "t":["24", "22", "38"],
            "m":["a","a","a","a","a","a","a","a","a","a","a","a","a","a","a","a","a","a","a","a","b"]}"#,
    )
    .unwrap()
}

fn one(q: &str) -> String {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs
            .iter()
            .map(|v| v.display_form())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("{q}: {other:?}"),
    }
}

fn refuse(q: &str) -> String {
    quarb::run(q, &doc()).unwrap_err().to_string()
}

#[test]
fn bins_is_the_table_of_equal_width_bins() {
    assert_eq!(
        one("/n/*:: @| bins(3)"),
        "#(@(from; to; count);\n  %(1; 4; 4);\n  %(4; 7; 0);\n  %(7; 10; 1);\n  %(null; null; 1))"
    );
    // ten bins unless given; the last bin closed
    assert_eq!(one("/n/*:: @| bins | dim"), "@(11; 3)");
    assert_eq!(
        one("/n/*:: @| bins(1)"),
        "#(@(from; to; count);\n  %(1; 10; 5);\n  %(null; null; 1))"
    );
    // text that reads as a number is one (a CSV column)
    assert_eq!(
        one("/t/*:: @| bins(2)"),
        "#(@(from; to; count);\n  %(22; 30; 2);\n  %(30; 38; 1))"
    );
    // other values refuse, pointing at histogram
    let e = refuse("/s/*:: @| bins");
    assert!(e.contains("bins numbers, and the stream holds text"), "{e}");
    let e = refuse("/n/*:: @| bins(0)");
    assert!(e.contains("at least one bin"), "{e}");
}

#[test]
fn histogram_draws_the_bars() {
    assert_eq!(
        one("/n/*:: @| histogram(3; -w=8)"),
        "1–4   ████████  4\n4–7             0\n7–10  ██        1\nnull  ██        1"
    );
    // whole numbers taking no more distinct values than bins: one
    // bar per value, in numeric order
    assert_eq!(
        one("/c/*:: @| histogram(-w=6)"),
        "1  ██      1\n2  ██      1\n3  ██████  3"
    );
    // other values: one bar per distinct value, most frequent first,
    // n the most shown, the rest one bar, the nulls last
    assert_eq!(
        one("/s/*:: @| histogram(-w=6)"),
        "a     ██████  3\nb     ████    2\nc     ██      1\nnull  ██      1"
    );
    assert_eq!(
        one("/s/*:: @| histogram(2; -w=6)"),
        "a      ██████  3\nb      ████    2\nother  ██      1\nnull   ██      1"
    );
    // the "other" and null bars stand outside the scale: the values
    // keep their shape and a bar past the scale fills the width
    assert_eq!(
        one("/s/*:: @| histogram(1; -w=6)"),
        "a      ██████  3\nother  ██████  3\nnull   ██      1"
    );
    assert_eq!(
        one("/m/*:: @| histogram(1; -w=6)"),
        "a      ██████  20\nother  ▎        1"
    );
    // any count shows at least one eighth
    assert_eq!(one("/m/*:: @| histogram(-w=1)"), "a  █  20\nb  ▏   1");
    // the switch stands last; an unknown one names the options
    let e = refuse("/n/*:: @| histogram(-x)");
    assert!(e.contains("the options --width -w"), "{e}");
    let e = refuse("/n/*:: @| histogram(--width=0)");
    assert!(e.contains("at least 1"), "{e}");
}

/// `count(--values)` / `-v` counts the non-null values, `count(--nulls)`
/// / `-n` the nulls, bare `count` everything; `frequencies(--nulls)` /
/// `-n` adds a row for the nulls, `--values` / `-v` the default.
#[test]
fn count_and_frequencies_take_the_values_and_nulls_switches() {
    assert_eq!(one("/n/*:: @| count"), "6");
    assert_eq!(one("/n/*:: @| count(-v)"), "5");
    assert_eq!(one("/n/*:: @| count(--nulls)"), "1");
    assert_eq!(one("/s/*:: @| count(--values)"), "6");
    assert_eq!(
        one("/s/*:: @| frequencies(-n)"),
        "%(value = \"a\"; n = 3)\n%(value = \"b\"; n = 2)\n%(value = \"c\"; n = 1)\n%(value = null; n = 1)"
    );
    assert_eq!(
        one("/s/*:: @| frequencies(2; -n)"),
        "%(value = \"a\"; n = 3)\n%(value = \"b\"; n = 2)\n%(value = null; n = 1)"
    );
    assert_eq!(
        one("/s/*:: @| frequencies(--values)"),
        one("/s/*:: @| frequencies")
    );
    let e = refuse("/n/*:: @| sum(-n)");
    assert!(e.contains("'-n' given"), "{e}");
}
