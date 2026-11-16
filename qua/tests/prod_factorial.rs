//! `prod` with its alias `product`, the command `factorial`, and
//! the refusal of a word that names no command (rulings 2026-11-15).
//!
//! The user looked for the multiplicative `sum` under `prod`, was
//! told "not yet supported: pipeline command 'prod'", and took the
//! command for missing; it was `product`. So `prod` is the name and
//! `product` its alias, and a word that names no command is
//! refused as that, with the nearest names.

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

/// The answer of an expression that reads no file.
fn answer(query: &str) -> String {
    let (success, out, err) = run(&[query]);
    assert!(success, "{query}: {err}");
    out
}

/// The answer and what was said beside it.
fn answer_and_warnings(query: &str) -> (String, String) {
    let (success, out, err) = run(&[query]);
    assert!(success, "{query}: {err}");
    (out, err)
}

/// The refusal a query answers.
fn refusal(query: &str) -> String {
    let (success, _, err) = run(&[query]);
    assert!(!success, "{query} was taken");
    err
}

#[test]
fn prod_is_the_command_and_product_its_alias() {
    for name in ["prod", "product"] {
        assert_eq!(answer(&format!("= @(2; 3; 4) | {name}")), "24", "{name}");
        // across the threads
        assert_eq!(
            answer(&format!("= @(2; 3; 4) | ... @| {name}")),
            "24",
            "{name}"
        );
        assert_eq!(answer(&format!("= {name}(@(2; 3; 4))")), "24", "{name}");
        // the product of nothing is null, as the sum of nothing is
        // (it answered 1 for the empty list and for no thread)
        assert_eq!(answer(&format!("= @() | {name}")), "null", "{name}");
        assert_eq!(answer(&format!("= @(null) | {name}")), "null", "{name}");
        assert_eq!(answer(&format!("= @(\"x\") | {name}")), "null", "{name}");
        assert_eq!(answer(&format!("= null | {name}")), "null", "{name}");
        // one quantity among numbers scales
        assert_eq!(answer(&format!("= @(2; 3_km) | {name}")), "6_km", "{name}");
    }
}

#[test]
fn prod_multiplies_quantities_with_their_units() {
    // two lengths are an area, three a volume: what `*` answers
    // (two quantities were refused)
    assert_eq!(answer("= @(2_m; 3_m) | prod"), answer("= 2_m * 3_m"));
    assert_eq!(answer("= @(2_m; 3_m) | prod"), "6_m^2");
    assert_eq!(
        answer("= @(2_m; 3_m; 4_m) | prod"),
        answer("= 2_m * 3_m * 4_m")
    );
    // a bare number among them scales, a null is skipped
    assert_eq!(
        answer("= @(2_m; 3; null; 4_m) | prod"),
        answer("= 2_m * 3 * 4_m")
    );
    assert_eq!(answer("= @(2_m; 3_m) | product"), "6_m^2");
}

#[test]
fn a_grouped_product_names_its_columns_after_the_main_name() {
    // `n_prod` under both names (the alias wrote `n_product` when it
    // was the command)
    for name in ["prod", "product"] {
        assert_eq!(
            answer(&format!(
                "= #(@(g; n); %(\"a\"; 2); %(\"a\"; 3); %(\"b\"; 4)) | {name}(--by=:g)"
            )),
            "#(@(g; n_prod);\n  %(\"a\"; 6);\n  %(\"b\"; 4))",
            "{name}"
        );
    }
}

#[test]
fn man_reads_the_alias_as_one() {
    let (success, out, err) = run(&["man", "prod"]);
    assert!(success, "{err}");
    assert!(out.contains("prod{stream}("), "{out}");
    // the alias is told as one and answered by the page of the
    // command it names
    let (success, out, err) = run(&["man", "product"]);
    assert!(success, "{err}");
    assert!(out.starts_with("product — alias of prod"), "{out}");
    let (_, main, _) = run(&["man", "prod"]);
    assert!(out.ends_with(&main), "{out}");
    assert!(out.contains("prod{stream}("), "{out}");
    // every alias alike
    for (alias, named) in [("avg", "mean"), ("tfmt", "strftime"), ("stddev", "std")] {
        let (success, out, err) = run(&["man", alias]);
        assert!(success, "{err}");
        let (_, main, _) = run(&["man", named]);
        assert!(
            out.starts_with(&format!("{alias} — ")) && out.ends_with(&main),
            "{alias}: {out}"
        );
    }
}

#[test]
fn factorial_of_the_integers_it_takes() {
    assert_eq!(answer("= 0 | factorial"), "1");
    assert_eq!(answer("= 1 | factorial"), "1");
    assert_eq!(answer("= 5 | factorial"), "120");
    // the last whose factorial is an integer
    assert_eq!(answer("= 20 | factorial"), "2432902008176640000");
    // text of integer form is read as the integer
    assert_eq!(answer("= \"5\" | factorial"), "120");
    assert_eq!(answer("= factorial(4)"), "24");
    assert_eq!(answer("= null | factorial"), "null");
    // a scalar command: a list item by item
    assert_eq!(answer("= @(0; 3; 5) | factorial"), "@(1; 6; 120)");
}

#[test]
fn factorial_answers_null_for_what_it_does_not_take_and_says_so() {
    // past the integers, a negative, a number that is no integer
    // (a whole float is none either), and a value of another kind
    for (v, said) in [
        ("21", "1 value is outside that (21)"),
        ("-1", "1 value is outside that (-1)"),
        ("2.5", "1 value is outside that (2.5)"),
        ("5.0", "1 value is outside that (5.0)"),
        ("\"x\"", "1 value is of another kind"),
        ("true", "1 value is of another kind"),
    ] {
        let (out, err) = answer_and_warnings(&format!("= {v} | factorial"));
        assert_eq!(out, "null", "{v}");
        assert!(
            err.contains("factorial takes a non-negative integer up to 20") && err.contains(said),
            "{v}: {err}"
        );
    }
    // several are counted in one line, the first shown
    let (_, err) = answer_and_warnings("= @(21; -1; 3) | factorial");
    assert!(
        err.contains("2 values are outside that (the first: 21) and answer null"),
        "{err}"
    );
    // in a list a number it does not take is null in its place; a
    // value of another kind stays as it is
    let (out, err) = answer_and_warnings("= @(3; -1; \"x\") | factorial");
    assert_eq!(out, "@(6; null; \"x\")");
    assert!(err.contains("factorial takes"), "{err}");
    // it takes no operand
    let err = refusal("= 5 | factorial(2)");
    assert!(err.contains("'factorial' takes no operand"), "{err}");
}

#[test]
fn a_word_that_names_no_command_is_refused_with_the_nearest() {
    // the word is part of a command's name
    let err = refusal("= @(1; 2) | summ");
    assert!(
        err.contains("'summ' is not a command; the nearest: sum"),
        "{err}"
    );
    // one slip away
    let err = refusal("= \"a\" | uper");
    assert!(
        err.contains("'uper' is not a command; the nearest: upper"),
        "{err}"
    );
    let err = refusal("= @(1; 2) @| cuont");
    assert!(
        err.contains("'cuont' is not a command; the nearest: count"),
        "{err}"
    );
    // near nothing
    let err = refusal("= 1 | frobnicate");
    assert!(
        err.contains("'frobnicate' is not a command ('man' lists the commands)"),
        "{err}"
    );
    // never the words of a feature on its way
    for q in ["= 1 | frobnicate", "= 1 @| frobnicate", "= 1 | summ"] {
        assert!(!refusal(q).contains("not yet supported"), "{q}");
    }
}

#[test]
fn a_command_on_the_other_pipe_is_told_where_it_is_written() {
    let err = refusal("= @(\"a\"; \"b\") @| upper");
    assert!(
        err.contains("'upper' takes one value at a time: write '| upper'"),
        "{err}"
    );
    // a number glued to a unit is a bare word
    let err = refusal("= 1 | 12h");
    assert!(
        err.contains("'12h' is not a command")
            && err.contains("the quantity literal is written 12_h"),
        "{err}"
    );
}
