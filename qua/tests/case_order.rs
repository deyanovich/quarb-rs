//! Under a collation, texts that differ only in case are no tie
//! (ruling 2026-11-12): the locale's own case order decides, lower
//! case first in most locales, upper case first where CLDR gives
//! the locale that order (Danish, Maltese, Church Slavic). Letters
//! decide first, then accents, then case: `a A b B`, never
//! `a b A B`. The order is colligo's (0.2.0, its default); the
//! answer no longer depends on the input order. Without a collation
//! text orders by code point, as ever.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(r#"{"w": [1]}"#).unwrap()
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

/// The answers of `q` under the output locale `locale`.
fn under(locale: &str, q: &str) -> Vec<String> {
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
}

/// Every order the words can come in.
fn orders(words: &[&str]) -> Vec<String> {
    fn permute(rest: &mut Vec<String>, taken: &mut Vec<String>, out: &mut Vec<String>) {
        if rest.is_empty() {
            out.push(taken.join("; "));
            return;
        }
        for i in 0..rest.len() {
            let w = rest.remove(i);
            taken.push(w.clone());
            permute(rest, taken, out);
            taken.pop();
            rest.insert(i, w);
        }
    }
    let mut out = Vec::new();
    let mut rest: Vec<String> = words.iter().map(|w| format!("\"{w}\"")).collect();
    permute(&mut rest, &mut Vec::new(), &mut out);
    out
}

/// Lower case first, whatever order the texts came in: the cases of
/// lang's measurement on 0.61.0, where the input order decided.
#[test]
fn case_is_no_tie_under_a_collation() {
    for list in orders(&["B", "b", "a"]) {
        assert_eq!(
            one(&format!(r#"= @({list}) | sort("de")"#)),
            r#"@("a"; "b"; "B")"#,
            "{list}"
        );
    }
    for list in orders(&["aB", "Ab", "ab", "AB"]) {
        assert_eq!(
            one(&format!(r#"= @({list}) | sort("en")"#)),
            r#"@("ab"; "aB"; "Ab"; "AB")"#,
            "{list}"
        );
    }
    // accents before case: the accented pair after the plain one
    for list in orders(&["Resume", "résumé", "resume", "Résumé"]) {
        assert_eq!(
            one(&format!(r#"= @({list}) | sort("en")"#)),
            r#"@("resume"; "Resume"; "résumé"; "Résumé")"#,
            "{list}"
        );
    }
    // letters before any case: a A b B, and `ac` after every `ab`
    assert_eq!(
        one(r#"= @("b"; "A"; "a"; "B") | sort("de")"#),
        r#"@("a"; "A"; "b"; "B")"#
    );
    assert_eq!(
        one(r#"= @("ac"; "AB"; "ab") | sort("de")"#),
        r#"@("ab"; "AB"; "ac")"#
    );
    // the whole order flipped
    assert_eq!(
        one(r#"= @("b"; "A"; "a"; "B") | sort("de"; --desc)"#),
        r#"@("B"; "b"; "A"; "a")"#
    );
}

/// The locale's own case order: upper case first in Danish, Maltese
/// and Church Slavic, as CLDR orders them.
#[test]
fn the_locale_decides_which_case_leads() {
    for locale in ["da", "mt"] {
        for list in orders(&["b", "A", "a", "B"]) {
            assert_eq!(
                one(&format!(r#"= @({list}) | sort("{locale}")"#)),
                r#"@("A"; "a"; "B"; "b")"#,
                "{locale}: {list}"
            );
        }
    }
    assert_eq!(
        one(r#"= @("слово"; "Слово") | sort("cu")"#),
        r#"@("Слово"; "слово")"#
    );
    for locale in ["de", "en", "fr", "sv", "ru"] {
        assert_eq!(
            one(&format!(r#"= @("B"; "A"; "b"; "a") | sort("{locale}")"#)),
            r#"@("a"; "A"; "b"; "B")"#,
            "{locale}"
        );
    }
}

/// Every command that orders text under a collation follows: the
/// keyed ones with `--locale=`, all of them under the output locale,
/// and the a tergo order under a collation.
#[test]
fn every_collated_order_follows() {
    let words = r#"= @("B"; "b"; "A"; "a") | ..."#;
    assert_eq!(
        values(&format!("{words} @| sort_by($_; --locale=de)")),
        [r#""a""#, r#""A""#, r#""b""#, r#""B""#]
    );
    assert_eq!(
        values(&format!("{words} @| sort_by($_; --locale=da)")),
        [r#""A""#, r#""a""#, r#""B""#, r#""b""#]
    );
    // an extreme is one text now, where the case pair tied
    assert_eq!(
        values(&format!("{words} @| min_by($_; --locale=de)")),
        [r#""a""#]
    );
    assert_eq!(
        values(&format!("{words} @| max_by($_; --locale=de)")),
        [r#""B""#]
    );
    assert_eq!(
        values(&format!("{words} @| top(2; $_; --locale=de)")),
        [r#""B""#, r#""b""#]
    );
    assert_eq!(
        values(&format!("{words} @| bottom(2; $_; --locale=da)")),
        [r#""A""#, r#""a""#]
    );
    // the output locale, for the bare sort and the keyed commands
    assert_eq!(
        under("de", r#"= @("B"; "b"; "A"; "a") | sort"#),
        [r#"@("a"; "A"; "b"; "B")"#]
    );
    assert_eq!(
        under("da", r#"= @("B"; "b"; "A"; "a") | sort"#),
        [r#"@("A"; "a"; "B"; "b")"#]
    );
    assert_eq!(
        under("de", &format!("{words} @| sort_by($_)")),
        [r#""a""#, r#""A""#, r#""b""#, r#""B""#]
    );
    assert_eq!(under("de", &format!("{words} @| min_by($_)")), [r#""a""#]);
    // a tergo under a collation: the endings decide, then the case
    assert_eq!(
        one(r#"= @("Ab"; "ab"; "ba"; "Ba") | sort("de"; --atergo)"#),
        r#"@("ba"; "Ba"; "ab"; "Ab")"#
    );
}

/// What is no tie stays where it was, and without a collation text
/// orders by code point.
#[test]
fn what_was_no_tie_is_unchanged() {
    assert_eq!(
        one(r#"= @("é"; "e"; "f") | sort("fr")"#),
        r#"@("e"; "é"; "f")"#
    );
    assert_eq!(
        one(r#"= @("f"; "é"; "e") | sort("fr")"#),
        r#"@("e"; "é"; "f")"#
    );
    // numbers keep their own order under a locale, case or no case
    assert_eq!(
        one(r#"= @("b"; 10; "B"; 9) | sort("de")"#),
        r#"@(9; 10; "b"; "B")"#
    );
    // no collation: code points, upper case letters first
    assert_eq!(
        one(r#"= @("b"; "A"; "a"; "B") | sort"#),
        r#"@("A"; "B"; "a"; "b")"#
    );
}

fn refused(q: &str) -> String {
    match quarb::run(q, &doc()) {
        Ok(v) => panic!("{q}: expected a refusal, got {v:?}"),
        Err(e) => e.to_string(),
    }
}

const MIXED: &str = r#"= @("b"; "A"; "a"; "B")"#;

/// The case options (ruling 2026-11-12: "leverage colligo's sort
/// options fully"): `-u` (`--upper-first`) and `-l`
/// (`--lower-first`) name one case order on any locale, whatever
/// CLDR says of it; `-i` (`--ignore-case`) makes texts that differ
/// only in case a tie again, the input order kept.
#[test]
fn a_case_option_overrides_the_locale() {
    let lower = r#"@("a"; "A"; "b"; "B")"#;
    let upper = r#"@("A"; "a"; "B"; "b")"#;
    // lower first on a locale that sorts upper first, and the other
    // way; short and long spellings
    for flag in ["-l", "--lower-first"] {
        assert_eq!(
            one(&format!(r#"{MIXED} | sort("da"; {flag})"#)),
            lower,
            "{flag}"
        );
        assert_eq!(
            one(&format!(r#"{MIXED} | sort("de"; {flag})"#)),
            lower,
            "{flag}"
        );
    }
    for flag in ["-u", "--upper-first"] {
        assert_eq!(
            one(&format!(r#"{MIXED} | sort("de"; {flag})"#)),
            upper,
            "{flag}"
        );
        assert_eq!(
            one(&format!(r#"{MIXED} | sort("da"; {flag})"#)),
            upper,
            "{flag}"
        );
    }
    // case ignored: the pairs tie and keep the order they came in
    for flag in ["-i", "--ignore-case"] {
        assert_eq!(
            one(&format!(r#"{MIXED} | sort("de"; {flag})"#)),
            r#"@("A"; "a"; "b"; "B")"#,
            "{flag}"
        );
        assert_eq!(
            one(&format!(r#"= @("B"; "a"; "A"; "b") | sort("da"; {flag})"#)),
            r#"@("a"; "A"; "B"; "b")"#,
            "{flag}"
        );
    }
    // with the other flags of `sort`
    assert_eq!(
        one(&format!(r#"{MIXED} | sort("de"; -u -d)"#)),
        r#"@("b"; "B"; "a"; "A")"#
    );
    assert_eq!(
        one(r#"= @("Ab"; "ab"; "ba"; "Ba") | sort("de"; -a -u)"#),
        r#"@("Ba"; "ba"; "Ab"; "ab")"#
    );
    // a record's keys
    assert_eq!(
        one(r#"= %(b = 1; A = 2; a = 3; B = 4) | sort("de"; -u)"#),
        "%(A = 2; a = 3; B = 4; b = 1)"
    );
}

/// The keyed commands that order take the three, beside `--locale=`
/// and under the output locale.
#[test]
fn the_keyed_commands_take_the_case_options() {
    let words = format!("{MIXED} | ...");
    assert_eq!(
        values(&format!("{words} @| sort_by($_; --locale=da -l)")),
        [r#""a""#, r#""A""#, r#""b""#, r#""B""#]
    );
    assert_eq!(
        values(&format!(
            "{words} @| sort_by($_; --locale=de --upper-first)"
        )),
        [r#""A""#, r#""a""#, r#""B""#, r#""b""#]
    );
    // an extreme under `-i` is every text of the tie again
    assert_eq!(
        values(&format!("{words} @| min_by($_; --locale=de -i)")),
        [r#""A""#, r#""a""#]
    );
    assert_eq!(
        values(&format!("{words} @| max_by($_; --locale=de -i)")),
        [r#""b""#, r#""B""#]
    );
    assert_eq!(
        values(&format!("{words} @| max_by($_; --locale=de -u)")),
        [r#""b""#]
    );
    assert_eq!(
        values(&format!("{words} @| top(2; $_; --locale=de -u)")),
        [r#""b""#, r#""B""#]
    );
    assert_eq!(
        values(&format!("{words} @| bottom(2; $_; --locale=da -l)")),
        [r#""a""#, r#""A""#]
    );
    // a table's rows the same
    assert_eq!(
        one(
            r#"= @(%(k = "b"); %(k = "A"); %(k = "a"); %(k = "B")) | # | sort_by(:k; --locale=de -u) | :k"#
        ),
        r#"@("A"; "a"; "B"; "b")"#
    );
    // the output locale's collation, in the order the call names
    assert_eq!(
        under("da", &format!("{MIXED} | sort(-l)")),
        [r#"@("a"; "A"; "b"; "B")"#]
    );
    assert_eq!(
        under("de", &format!("{words} @| sort_by($_; -u)")),
        [r#""A""#, r#""a""#, r#""B""#, r#""b""#]
    );
    assert_eq!(
        under("de", &format!("{words} @| min_by($_; -i)")),
        [r#""A""#, r#""a""#]
    );
}

/// A case order is a collation's to give: with no locale named and
/// no output locale, a case option sorts text under the root
/// collation, so the case-blind sort needs no locale. The whole text
/// order is then the root collation's, accents beside their letters.
#[test]
fn a_case_option_alone_sorts_under_the_root_collation() {
    assert_eq!(
        one(&format!("{MIXED} | sort(-l)")),
        r#"@("a"; "A"; "b"; "B")"#
    );
    assert_eq!(
        one(&format!("{MIXED} | sort(-u)")),
        r#"@("A"; "a"; "B"; "b")"#
    );
    assert_eq!(
        one(&format!("{MIXED} | sort(-i)")),
        r#"@("A"; "a"; "b"; "B")"#
    );
    assert_eq!(
        one(r#"= @("Banana"; "apple"; "cherry"; "Apple") | sort(-i)"#),
        r#"@("apple"; "Apple"; "Banana"; "cherry")"#
    );
    // without the option: code points
    assert_eq!(one(r#"= @("z"; "é"; "e") | sort"#), r#"@("e"; "z"; "é")"#);
    assert_eq!(
        one(r#"= @("z"; "é"; "e") | sort(-i)"#),
        r#"@("e"; "é"; "z")"#
    );
    // numbers keep their own order
    assert_eq!(
        one(r#"= @("b"; 10; "B"; 9) | sort(-i)"#),
        r#"@(9; 10; "b"; "B")"#
    );
    // the keyed commands and a record the same
    assert_eq!(
        values(&format!("{MIXED} | ... @| sort_by($_; -l)")),
        [r#""a""#, r#""A""#, r#""b""#, r#""B""#]
    );
    assert_eq!(
        values(&format!("{MIXED} | ... @| max_by($_; -i)")),
        [r#""b""#, r#""B""#]
    );
    assert_eq!(
        one("= %(b = 1; A = 2; a = 3; B = 4) | sort(-l)"),
        "%(a = 3; A = 2; b = 1; B = 4)"
    );
}

/// The three are one choice, and only the commands that order text
/// under a collation take them.
#[test]
fn the_case_options_are_one_choice() {
    for q in [
        r#"= @("a"; "B") | sort(-u -l)"#,
        r#"= @("a"; "B") | sort("de"; -i --upper-first)"#,
        r#"= @("a"; "B") | ... @| sort_by($_; -l -i)"#,
        r#"= @("a"; "B") | ... @| top(1; $_; -u -i)"#,
    ] {
        let e = refused(q);
        assert!(
            e.contains("--upper-first, --lower-first and --ignore-case are one choice: name one"),
            "{q}: {e}"
        );
    }
    for q in [
        r#"= @("a"; "B") | unique(-i)"#,
        r#"= @("a"; "B") | ... @| unique_by($_; -i)"#,
        r#"= @("a"; "B") | reverse(-u)"#,
        r#"= @("a"; "B") | first(-l)"#,
    ] {
        let e = refused(q);
        assert!(e.contains("no option"), "{q}: {e}");
    }
}

/// `"C"` and `"POSIX"` name the code-point order (ruling
/// 2026-11-12), the words the tools take for no locale: text by
/// code point, ASCII order for ASCII text, whatever the output
/// locale. It is no collation, so it has no case order to give.
#[test]
fn c_and_posix_name_the_code_point_order() {
    let list = r#"= @("b"; "A"; "a"; "B"; "é"; "Z")"#;
    let points = r#"@("A"; "B"; "Z"; "a"; "b"; "é")"#;
    let threads = [r#""A""#, r#""B""#, r#""Z""#, r#""a""#, r#""b""#, r#""é""#];
    for name in ["C", "POSIX"] {
        assert_eq!(
            one(&format!(r#"{list} | sort("{name}")"#)),
            points,
            "{name}"
        );
        // under an output locale the bare sort collates, and the
        // name asks for the code points all the same
        assert_eq!(
            under("de", &format!(r#"{list} | sort("{name}")"#)),
            [points],
            "{name}"
        );
        assert_eq!(
            values(&format!("{list} | ... @| sort_by($_; --locale={name})")),
            threads,
            "{name}"
        );
        assert_eq!(
            under(
                "de",
                &format!("{list} | ... @| sort_by($_; --locale={name})")
            ),
            threads,
            "{name}"
        );
        assert_eq!(
            under(
                "de",
                &format!("{list} | ... @| min_by($_; --locale={name})")
            ),
            [r#""A""#],
            "{name}"
        );
        assert_eq!(
            under(
                "de",
                &format!("{list} | ... @| max_by($_; --locale={name})")
            ),
            [r#""é""#],
            "{name}"
        );
        assert_eq!(
            under(
                "de",
                &format!("{list} | ... @| top(2; $_; --locale={name})")
            ),
            [r#""é""#, r#""b""#],
            "{name}"
        );
        assert_eq!(
            under(
                "de",
                &format!("{list} | ... @| bottom(2; $_; --locale={name})")
            ),
            [r#""A""#, r#""B""#],
            "{name}"
        );
        // no case order
        let e = refused(&format!(r#"{list} | sort("{name}"; -i)"#));
        assert!(
            e.contains(&format!(
                r#"sort: a case order is a collation's, and "{name}" names the code-point order"#
            )),
            "{name}: {e}"
        );
        let e = refused(&format!("{list} | ... @| sort_by($_; --locale={name} -u)"));
        assert!(e.contains("names the code-point order"), "{name}: {e}");
    }
    assert_eq!(
        under("de", &format!("{list} | sort")),
        [r#"@("a"; "A"; "b"; "B"; "é"; "Z")"#]
    );
    // the endings by code point, and the whole order flipped
    assert_eq!(
        one(r#"= @("ab"; "Ba"; "aB") | sort("C"; --atergo)"#),
        r#"@("aB"; "Ba"; "ab")"#
    );
    assert_eq!(
        one(&format!(r#"{list} | sort("C"; --desc)"#)),
        r#"@("é"; "b"; "a"; "Z"; "B"; "A")"#
    );
    // numbers keep their own order
    assert_eq!(
        one(r#"= @("b"; 10; "B"; 9) | sort("C")"#),
        r#"@(9; 10; "B"; "b")"#
    );
    // the two words as written, and no other
    for other in ["c", "C.UTF-8", "ascii"] {
        let e = refused(&format!(r#"{list} | sort("{other}")"#));
        assert!(
            e.contains("is not a Unicode locale identifier")
                || e.contains("no collation for locale"),
            "{other}: {e}"
        );
    }
    let e = refused(&format!(r#"{list} | sort("c")"#));
    assert!(e.contains(r#""C" for the code-point order"#), "{e}");
}
