//! Ruling #84: a group's members are its rows. A relative operand
//! from a group reads every member's node (as `:field` reads every
//! member's record), `$_ | …` runs its stages over the members, and
//! a projection stage after a group is the members' values as a
//! list.
use quarb::{AstAdapter, QueryResult, Value};
fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"book":"mark","word":"The"},
            {"book":"mark","word":"beginning"},
            {"book":"luke","word":"Since"},
            {"book":"mark","word":"of"}
        ]}"#,
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
fn a_property_reads_every_member() {
    assert_eq!(
        values(r#"/rows/* @| group(b = ::book) | %(b = $.b; words = ($_ | ::word @| join(" ")))"#),
        [
            "%(b = \"mark\"; words = \"The beginning of\")",
            "%(b = \"luke\"; words = \"Since\")"
        ]
    );
    assert_eq!(
        values(r#"/rows/* @| group(b = ::book) | ::word"#),
        ["The, beginning, of", "Since"]
    );
    assert_eq!(
        values(
            r#"/rows/* @| group(b = ::book) | %(n = ($_ @| count); ws = (::word @| join("+")))"#
        ),
        [
            "%(n = 3; ws = \"The+beginning+of\")",
            "%(n = 1; ws = \"Since\")"
        ]
    );
}
#[test]
fn a_record_field_still_reads_every_member() {
    assert_eq!(
        values(
            r#"/rows/* | %(b = ::book; w = ::word) @| group(b = $_:b) | %(b = $.b; ws = ($_ | :w @| join(" ")))"#
        ),
        [
            "%(b = \"mark\"; ws = \"The beginning of\")",
            "%(b = \"luke\"; ws = \"Since\")"
        ]
    );
}

// Ruling #85: a computed push lands every value its operand
// yields, and a parenthesized operand may open on the topic's
// field (`(:t | …)`).
#[test]
fn a_computed_push_keeps_every_value() {
    assert_eq!(
        values(
            r#"/rows/*[::book = "luke"] | ::word | .s(($_ | lower | ngrams(1))) | %(n = ($.s @| count))"#
        ),
        ["%(n = 1)"]
    );
    assert_eq!(
        values(
            r#"/rows/* | ::word @| join(" ") | .s(($_ | lower | ngrams(2))) | %(n = ($.s @| count); first = ($.s @| first))"#
        ),
        ["%(n = 3; first = \"the beginning\")"]
    );
}

#[test]
fn a_parenthesized_operand_opens_on_a_field() {
    assert_eq!(
        values(r#"/rows/* | %(w = ::word) | %(u = (:w | upper))"#),
        [
            "%(u = \"THE\")",
            "%(u = \"BEGINNING\")",
            "%(u = \"SINCE\")",
            "%(u = \"OF\")"
        ]
    );
    // a filter stage inside the operand pipe, against a pushed set
    assert_eq!(
        values(
            r#"/rows/* | .s(("the of" | ngrams(1))) | %(w = ::word) | %(hit = (:w | lower | [$_ = $.s] @| count))"#
        ),
        ["%(hit = 1)", "%(hit = 0)", "%(hit = 0)", "%(hit = 1)"]
    );
}

#[test]
fn membership_in_a_large_list_hashes() {
    // past the probe threshold (64), with hits and misses: "the"
    // and "of" are in the set, the rest are not
    let set: Vec<String> = (0..70).map(|i| format!("w{i}")).collect();
    let q = format!(
        r#"/rows/* | .set(("{} the of" | ngrams(1))) | %(w = ::word) | %(w = :w; hit = ((:w | lower) = $.set))"#,
        set.join(" ")
    );
    assert_eq!(
        values(&q),
        [
            "%(w = \"The\"; hit = true)",
            "%(w = \"beginning\"; hit = false)",
            "%(w = \"Since\"; hit = false)",
            "%(w = \"of\"; hit = true)"
        ]
    );
}

// A record's field holds every value its operand yields (the
// push rule of ruling #85 applied to records), and the stages of a
// parenthesized operand pipe keep a group's keys for the next
// stage.
#[test]
fn a_record_field_keeps_every_value() {
    assert_eq!(
        values(r#"/rows/* @| group(b = ::book) | %(b = $.b; words = ($_ | ::word))"#),
        [
            "%(b = \"mark\"; words = @(\"The\"; \"beginning\"; \"of\"))",
            "%(b = \"luke\"; words = \"Since\")"
        ]
    );
}

#[test]
fn an_operand_pipe_keeps_the_group_keys() {
    assert_eq!(
        values(
            r#"/rows @| first | %(top = (/* | %(b = ::book) @| group(b = $_:b) | %(b = $.b; n = ($_ @| count)) @| top(1; :n) | :b @| first))"#
        ),
        ["%(top = \"mark\")"]
    );
}
