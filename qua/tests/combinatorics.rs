//! Orderings, subsets and the characters of a text (rulings
//! 2026-11-15, and four defects found while the combinatorics
//! chapter was probed).
//!
//! `permutations` joins `combinations`; a size may be an expression
//! and zero asks for the one empty subset; `split` with no separator
//! answers the characters. The defects: on the plain pipe a single
//! combination lost its enclosing list, a range could not head a
//! pipe item, `ngrams(n)` read a list as its text, and `split("")`
//! answered an empty text at each end.

fn run(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end()
            .to_string(),
        String::from_utf8(out.stderr)
            .unwrap()
            .trim_end()
            .to_string(),
    )
}

/// The answer of an expression that reads no file, one line per
/// thread.
fn lines(query: &str) -> Vec<String> {
    let (success, out, err) = run(&[query]);
    assert!(success, "{query}: {err}");
    out.lines().map(str::to_string).collect()
}

fn answer(query: &str) -> String {
    lines(query).join("\n")
}

/// The refusal a query answers.
fn refusal(query: &str) -> String {
    let (success, _, err) = run(&[query]);
    assert!(!success, "{query} was taken");
    err
}

#[test]
fn permutations_order_the_distinct_values() {
    assert_eq!(
        answer(r#"= @("a"; "b"; "c") | permutations"#),
        r#"@(@("a"; "b"; "c"); @("a"; "c"; "b"); @("b"; "a"; "c"); @("b"; "c"; "a"); @("c"; "a"; "b"); @("c"; "b"; "a"))"#
    );
    // of every two of them
    assert_eq!(
        answer(r#"= @("a"; "b"; "c") | permutations(2)"#),
        r#"@(@("a"; "b"); @("a"; "c"); @("b"; "a"); @("b"; "c"); @("c"; "a"); @("c"; "b"))"#
    );
    // the values in their own order, each once, a null left out
    assert_eq!(
        answer(r#"= @("b"; "a"; "b"; null) | permutations"#),
        r#"@(@("a"; "b"); @("b"; "a"))"#
    );
    // as many as the factorial counts
    assert_eq!(answer("= @(1..5) | permutations | count"), "120");
    assert_eq!(answer("= @(1..5) | permutations(2) | count"), "20");
    // across the threads, one ordering a thread
    assert_eq!(
        lines(r#"= @("a"; "b") | ... @| permutations"#),
        [r#"@("a"; "b")"#, r#"@("b"; "a")"#]
    );
    // one value has one ordering, none has the empty one, and a size
    // past the values has none
    assert_eq!(answer("= @(7) | permutations"), "@(@(7))");
    assert_eq!(answer("= @(1; 2) | permutations(0)"), "@(@())");
    assert_eq!(answer("= @(1; 2) | permutations(3)"), "@()");
}

#[test]
fn permutations_past_the_bound_are_refused() {
    // ten values have 3,628,800 orderings
    let err = refusal("= @(1..10) | permutations");
    assert!(
        err.contains("permutations: all of 10 values have more than 1000000 orderings"),
        "{err}"
    );
    // nine are written out, and two of twenty
    assert_eq!(answer("= @(1..9) | permutations | count"), "362880");
    assert_eq!(answer("= @(1..20) | permutations(2) | count"), "380");
}

#[test]
fn a_size_may_be_an_expression() {
    assert_eq!(
        answer(r#"= 2 | .k | @("a"; "b"; "c") | combinations($.k)"#),
        r#"@(@("a"; "b"); @("a"; "c"); @("b"; "c"))"#
    );
    assert_eq!(
        answer(r#"= 2 | .k | @("a"; "b"; "c") | permutations($.k) | count"#),
        "6"
    );
    // after the aggregation pipe too
    assert_eq!(
        lines(r#"= 3 | .k | @("a"; "b"; "c") | ... @| combinations($.k)"#),
        [r#"@("a"; "b"; "c")"#]
    );
    // subsets of every size, one query
    assert_eq!(
        lines(r#"= 0..3 | .k | @("a"; "b"; "c") | combinations($.k) | count"#),
        ["1", "3", "3", "1"]
    );
}

#[test]
fn the_empty_subset_is_one() {
    assert_eq!(answer(r#"= @("a"; "b") | combinations(0)"#), "@(@())");
    assert_eq!(answer(r#"= @("a"; "b") | combinations(0) | count"#), "1");
    assert_eq!(answer("= @() | combinations(0)"), "@(@())");
    // a size past the values still has none
    assert_eq!(answer(r#"= @("a"; "b") | combinations(3)"#), "@()");
}

#[test]
fn a_single_combination_keeps_its_list() {
    // one pair is a list of one pair (it was the pair, bare, so
    // `| count` counted its two members)
    assert_eq!(answer(r#"= @("a"; "b") | pairs"#), r#"@(@("a"; "b"))"#);
    assert_eq!(answer(r#"= @("a"; "b") | pairs | count"#), "1");
    assert_eq!(answer(r#"= @("a"; "b"; "c") | triples | count"#), "1");
    assert_eq!(
        answer(r#"= @("a"; "b"; "c") | combinations(3) | $_[1]"#),
        r#"@("a"; "b"; "c")"#
    );
    // the windows of a stream as well
    assert_eq!(answer(r#"= @("a"; "b") | bigrams"#), r#"@(@("a"; "b"))"#);
    assert_eq!(answer(r#"= @("a"; "b"; "c") | trigrams | count"#), "1");
    // several were a list already, and none is the empty list
    assert_eq!(answer(r#"= @("a"; "b"; "c") | pairs | count"#), "3");
    assert_eq!(answer(r#"= @("a") | pairs"#), "@()");
    // a reducer of one value still answers the value
    assert_eq!(answer("= @(5) | sum"), "5");
    assert_eq!(answer("= @(5) | first"), "5");
}

#[test]
fn ngrams_on_a_list_reads_its_items() {
    // as `trigrams` does (it read the list as its text, "a, b, c,")
    assert_eq!(
        answer(r#"= @("a"; "b"; "c"; "d") | ngrams(3)"#),
        r#"@(@("a"; "b"; "c"); @("b"; "c"; "d"))"#
    );
    assert_eq!(
        answer(r#"= @("a"; "b"; "c"; "d") | ngrams(3)"#),
        answer(r#"= @("a"; "b"; "c"; "d") | trigrams"#)
    );
    assert_eq!(
        answer(r#"= @("a"; "b"; "c") | ngrams(3)"#),
        r#"@(@("a"; "b"; "c"))"#
    );
    // on a text it is still the windows of its words, a thread each
    assert_eq!(
        lines(r#"= "arma virumque cano" | ngrams(2)"#),
        ["arma virumque", "virumque cano"]
    );
}

#[test]
fn a_range_heads_a_pipe_item() {
    // as it stands in a parenthesis and at an expression's head
    assert_eq!(lines("= 3 | 1..3"), ["1", "2", "3"]);
    assert_eq!(lines("= 3 | 1..3"), lines("= 3 | (1..3)"));
    assert_eq!(lines("= 0 | 0..8..4"), ["0", "4", "8"]);
    // with a computed end, and an expression after it
    assert_eq!(lines("= 3 | .n | 1..($.n)"), ["1", "2", "3"]);
    assert_eq!(lines("= 0 | 1..3 | $_ * 10"), ["10", "20", "30"]);
    // a number glued to letters is still a word that names no command
    let err = refusal("= 1 | 12h");
    assert!(err.contains("'12h' is not a command"), "{err}");
}

#[test]
fn chars_answers_the_characters_as_a_list() {
    assert_eq!(answer(r#"= "test" | chars"#), r#"@("t"; "e"; "s"; "t")"#);
    // a character is what the eye counts: a letter with its accent,
    // written as two code points, is one
    assert_eq!(answer("= \"cafe\u{301}\" | chars | count"), "4");
    assert_eq!(answer("= \"cafe\u{301}\" | gc"), "4");
    assert_eq!(answer("= \"cafe\u{301}\" | cc"), "5");
    // the letters of a word, to order and to combine
    assert_eq!(answer(r#"= "ROMA" | chars | sort | join("")"#), "AMOR");
    assert_eq!(answer(r#"= "ab" | chars | permutations | count"#), "2");
    // no character, the empty list; no text, null
    assert_eq!(answer(r#"= "" | chars"#), "@()");
    assert_eq!(answer("= null | chars"), "null");
    // a scalar command: a list of texts item by item
    assert_eq!(
        answer(r#"= @("ab"; "c") | chars"#),
        r#"@(@("a"; "b"); @("c"))"#
    );
}

#[test]
fn split_with_no_separator_cuts_at_whitespace() {
    // as a shell cuts its arguments: runs of whitespace, no empty
    // piece (the bare `split` cut at commas, a default no page
    // stated)
    assert_eq!(
        lines("= \"arma  virumque,\tcano \" | split"),
        ["arma", "virumque,", "cano"]
    );
    assert_eq!(lines(r#"= "a,b" | split"#), ["a,b"]);
    // a separator cuts at it, a comma as any other
    assert_eq!(lines(r#"= "a,b" | split(",")"#), ["a", "b"]);
    assert_eq!(lines(r#"= "a  b" | split(" ")"#), ["a", "", "b"]);
    // an empty separator answers the characters, a thread each (it
    // answered an empty text before the first and after the last)
    assert_eq!(lines(r#"= "ROMA" | split("")"#), ["R", "O", "M", "A"]);
    // no piece, no thread
    assert_eq!(lines(r#"= "  " | split"#), Vec::<String>::new());
}
