//! The orders of `sort` and of the keyed commands (rulings
//! 2026-11-11): a record's fields by key, or by value under `-v`;
//! numbers keeping their own order under any locale; a number that
//! is only part of a text never read; `--locale=tag` on the keyed
//! commands that order.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"w":[{"name":"b","n":1},{"name":"a","n":2},{"name":"ä","n":3},
                 {"name":"z","n":4}]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Values(vs) => vs.iter().map(Value::to_quarb).collect(),
        other => panic!("{q}: expected values, got {other:?}"),
    }
}

fn one(q: &str) -> String {
    let v = values(q);
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refused(q: &str) -> String {
    match quarb::run(q, &doc()) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

/// A bare `| sort` on a record orders its fields by key; `-v`
/// (`--value`) by their values; the answer is a record.
#[test]
fn a_record_sorts_by_key_and_by_value() {
    assert_eq!(
        one("= %(b = 2; a = 1; c = 0) | sort"),
        "%(a = 1; b = 2; c = 0)"
    );
    assert_eq!(
        one("= %(b = 2; a = 1; c = 0) | sort(-v)"),
        "%(c = 0; a = 1; b = 2)"
    );
    assert_eq!(
        one("= %(b = 2; a = 1; c = 0) | sort(--value)"),
        "%(c = 0; a = 1; b = 2)"
    );
    // `--desc` flips whichever it goes by
    assert_eq!(
        one("= %(b = 2; a = 1; c = 0) | sort(-d)"),
        "%(c = 0; b = 2; a = 1)"
    );
    assert_eq!(
        one("= %(b = 2; a = 1; c = 0) | sort(-v -d)"),
        "%(b = 2; a = 1; c = 0)"
    );
    // the locale orders whichever the sort goes by
    assert_eq!(
        one(r#"= %("ä" = 1; z = 2; a = 3) | sort"#),
        "%(a = 3; z = 2; ä = 1)"
    );
    assert_eq!(
        one(r#"= %("ä" = 1; z = 2; a = 3) | sort("de")"#),
        "%(a = 3; ä = 1; z = 2)"
    );
    assert_eq!(
        one(r#"= %(x = "ä"; y = "z"; w = "a"; n = 10; m = 9) | sort("de"; -v)"#),
        r#"%(m = 9; n = 10; w = "a"; x = "ä"; y = "z")"#
    );
    // a list of one record stays the list: its item is the answer
    assert_eq!(one("= @(%(b = 2; a = 1)) | sort"), "%(b = 2; a = 1)");
}

/// `--of=` answers a list's sorted values and would lose a record's
/// keys; `-v` orders a record's fields and a list has none.
#[test]
fn the_options_keep_to_their_shape() {
    let e = refused("= %(b = 2; a = 1) | sort(--of=$_)");
    assert!(
        e.contains("--of orders the values of a list") && e.contains("-v (--value)"),
        "{e}"
    );
    for q in ["= @(3; 1; 2) | sort(-v)", "= @(3; 1; 2) | ... @| sort(-v)"] {
        let e = refused(q);
        assert!(
            e.contains("-v (--value) orders a record's fields by their values"),
            "{q}: {e}"
        );
    }
    // a list still sorts by `--of`
    assert_eq!(one("= @(%(k = 2); %(k = 1)) | sort(--of=:k)"), "@(1; 2)");
}

/// The locale does not touch numbers: under a collation operand
/// they keep their numeric order and stand before text, as without
/// one; the collation orders the text.
#[test]
fn numbers_keep_their_order_under_a_locale() {
    let list = r#"= @("b"; "a"; "ä"; "z"; 10; 9)"#;
    assert_eq!(
        one(&format!("{list} | sort")),
        r#"@(9; 10; "a"; "b"; "z"; "ä")"#
    );
    assert_eq!(
        one(&format!(r#"{list} | sort("de")"#)),
        r#"@(9; 10; "a"; "ä"; "b"; "z")"#
    );
    // the context's threads the same
    assert_eq!(
        values(&format!(r#"{list} | ... @| sort("de")"#)),
        ["9", "10", r#""a""#, r#""ä""#, r#""b""#, r#""z""#]
    );
    assert_eq!(
        values(&format!(r#"{list} | ... @| sort("de"; -d)"#)),
        [r#""z""#, r#""b""#, r#""ä""#, r#""a""#, "10", "9"]
    );
    // text that reads whole as a number counts as the number, with
    // or without a locale
    assert_eq!(
        one(r#"= @("10"; "9"; "x") | sort("en")"#),
        r#"@("9"; "10"; "x")"#
    );
    // a number that is only part of a text is never read: the texts
    // order by their characters
    for q in [
        r#"= @("item 9"; "item 10"; "item 2") | sort"#,
        r#"= @("item 9"; "item 10"; "item 2") | sort("en")"#,
    ] {
        assert_eq!(one(q), r#"@("item 10"; "item 2"; "item 9")"#, "{q}");
    }
    // the a tergo order is a text order too
    assert_eq!(
        one(r#"= @("ab"; "ba"; 10; 9) | sort(--atergo)"#),
        r#"@(9; 10; "ba"; "ab")"#
    );
}

/// `--locale=tag` on the keyed commands that order: text keys under
/// that locale's collation, every other key as it compares anywhere.
#[test]
fn the_keyed_commands_take_a_locale() {
    assert_eq!(
        values("/w/* @| sort_by(::name) | ::name"),
        [r#""a""#, r#""b""#, r#""z""#, r#""ä""#]
    );
    assert_eq!(
        values("/w/* @| sort_by(::name; --locale=de) | ::name"),
        [r#""a""#, r#""ä""#, r#""b""#, r#""z""#]
    );
    assert_eq!(
        values("/w/* @| sort_by(::name; --locale=de --desc) | ::name"),
        [r#""z""#, r#""b""#, r#""ä""#, r#""a""#]
    );
    // Swedish puts the letter after z
    assert_eq!(
        values("/w/* @| sort_by(::name; --locale=sv) | ::name"),
        [r#""a""#, r#""b""#, r#""z""#, r#""ä""#]
    );
    // the commands that order by the same comparison
    assert_eq!(values("/w/* @| max_by(::name) | ::name"), [r#""ä""#]);
    assert_eq!(
        values("/w/* @| max_by(::name; --locale=de) | ::name"),
        [r#""z""#]
    );
    assert_eq!(
        values("/w/* @| min_by(::name; --locale=de) | ::name"),
        [r#""a""#]
    );
    assert_eq!(
        values("/w/* @| top(2; ::name; --locale=de) | ::name"),
        [r#""z""#, r#""b""#]
    );
    assert_eq!(
        values("/w/* @| bottom(2; ::name; --locale=de) | ::name"),
        [r#""a""#, r#""ä""#]
    );
    // a number key is untouched
    assert_eq!(
        values("/w/* @| sort_by(-::n; --locale=de) | ::n"),
        ["4", "3", "2", "1"]
    );
    // a table's rows by a column
    assert_eq!(
        one(r#"= @(%(k = "ä"); %(k = "z"); %(k = "a")) | sort_by(:k; --locale=de) | :k"#),
        r#"@("a"; "ä"; "z")"#
    );
    // the tag's shape is held at parse; equality takes no locale
    let e = refused("/w/* @| sort_by(::name; --locale=C) | ::name");
    assert!(
        e.contains("sort_by: 'C' is not a Unicode locale identifier"),
        "{e}"
    );
    let e = refused("/w/* @| unique_by(::name; --locale=de) | ::name");
    assert!(e.contains("'unique_by' has no option '--locale'"), "{e}");
}

/// Without `--locale=` the keyed commands that order follow the
/// output locale, as the bare `sort` does (ruling 2026-11-11);
/// `--locale=tag` overrides it. They share one comparison, so the
/// five follow together.
#[test]
fn the_keyed_commands_follow_the_output_locale() {
    let under = |locale: &str, q: &str| -> Vec<String> {
        let mut run = quarb::Run::ambient();
        run.collation = Some(locale.into());
        let out = quarb::Query::parse(q, &doc())
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .run(&doc(), &run)
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        match out.result {
            QueryResult::Values(vs) => vs.iter().map(Value::to_quarb).collect(),
            other => panic!("{q}: expected values, got {other:?}"),
        }
    };
    let german = [r#""a""#, r#""ä""#, r#""b""#, r#""z""#];
    // the bare sort under the output locale, and `sort_by` as it
    assert_eq!(under("de", "/w/*::name @| sort"), german);
    assert_eq!(under("de", "/w/* @| sort_by(::name) | ::name"), german);
    assert_eq!(
        under("de", "/w/* @| sort_by(::name; --desc) | ::name"),
        [r#""z""#, r#""b""#, r#""ä""#, r#""a""#]
    );
    // the commands that share the comparison
    assert_eq!(under("de", "/w/* @| max_by(::name) | ::name"), [r#""z""#]);
    assert_eq!(under("de", "/w/* @| min_by(::name) | ::name"), [r#""a""#]);
    assert_eq!(
        under("de", "/w/* @| top(2; ::name) | ::name"),
        [r#""z""#, r#""b""#]
    );
    assert_eq!(
        under("de", "/w/* @| bottom(2; ::name) | ::name"),
        [r#""a""#, r#""ä""#]
    );
    // the option overrides the output locale
    assert_eq!(
        under("de", "/w/* @| sort_by(::name; --locale=sv) | ::name"),
        [r#""a""#, r#""b""#, r#""z""#, r#""ä""#]
    );
    // a table's rows the same
    assert_eq!(
        under(
            "de",
            r#"= @(%(k = "ä"); %(k = "z"); %(k = "a")) | sort_by(:k) | :k"#
        ),
        [r#"@("a"; "ä"; "z")"#]
    );
    // numbers keep their order under it
    assert_eq!(
        under("de", "/w/* @| sort_by(-::n) | ::n"),
        ["4", "3", "2", "1"]
    );
}
