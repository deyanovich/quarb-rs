//! An alias is declared once, in the `aliases` cell of its
//! command's row of the inventory (ruling 2026-11-15), and every
//! door reads it there.
//!
//! The inventory held a row per alias and the registry a second
//! list of them; the rows had drifted from their commands' (`tfmt`
//! lacked the locale and calendar operands in its synopsis, `avg`
//! the quantity form in its type). Now the registry has no entry
//! for an alias: the parser spells it by its command's name before
//! it reads the operands, so the checks, the refusals, the unparser
//! and the evaluator meet one name a command.
//!
//! The test reads the aliases from the binary's own listing, so an
//! alias a later inventory declares is held to the same answers the
//! day it is vendored.

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

/// Every alias with the command whose row declares it, from
/// `qua --commands`.
fn aliases() -> Vec<(String, String)> {
    let (success, out, err) = run(&["--commands"]);
    assert!(success, "{err}");
    let mut lines = out.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    let name = header.iter().position(|c| *c == "name").unwrap();
    let cell = header.iter().position(|c| *c == "aliases").unwrap();
    let mut out = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.split('\t').collect();
        for alias in cells[cell].split_whitespace() {
            out.push((alias.to_string(), cells[name].to_string()));
        }
    }
    out
}

/// Queries that call a command, the name left open as `{}`: after
/// the pipe, after the aggregation pipe, as a call, headless in a
/// record, and with operands it refuses.
fn samples(command: &str) -> &'static [&'static str] {
    const COUNTERS: &[&str] = &[
        r#"= "Arma virumque cano. Troiae qui primus" | {}"#,
        r#"= @("arma"; "virumque cano") | {}"#,
        r#"= {}("arma virumque")"#,
        "= null | {}",
        r#"= "arma" | {}(1)"#,
    ];
    const REDUCERS: &[&str] = &[
        "= @(2; 3; 7) | {}",
        "= @(2; 3; 7) | ... @| {}",
        "= {}(@(2; 3; 7))",
        "= @(2; 3; 7) | %(answer = {})",
        "= @(2_m; 3_m; 7_m) | {}",
        "= @() | {}",
        r#"= #(@(g; n); %("a"; 2); %("a"; 3); %("b"; 7)) | {}(--by=:g)"#,
        r#"= @(%(n = 2); %(n = 3); %(n = 7)) | {}(--of=.n)"#,
        "= @(2; 3; 7) | {}(1; 2)",
        "= @(2; 3; 7) | {}(--no-such)",
    ];
    match command {
        "cc" | "gc" | "bc" | "wc" | "lc" | "sc" => COUNTERS,
        "prod" | "mean" => REDUCERS,
        "std" | "var" => &[
            "= @(2; 3; 7) | {}",
            "= @(2; 3; 7) | ... @| {}",
            "= @(2; 3; 7) | {}(--sample)",
            "= {}(@(2; 3; 7))",
            "= @(2; 3; 7) | %(answer = {})",
            r#"= #(@(g; n); %("a"; 2); %("a"; 3); %("b"; 7)) | {}(--by=:g)"#,
            "= @(2; 3; 7) | {}(1; 2)",
        ],
        "choose" => &[
            "= 52 | {}(5)",
            "= {}(52; 5)",
            "= @(4; 5; 6) | {}(2)",
            "= 5 | {}",
            "= -1 | {}(2)",
            "= 5 | {}(1; 2; 3)",
        ],
        "rnd" => &[
            "= {}()",
            "= {}(1..6)",
            r#"= {}(@("alea"; "iacta"; "est"))"#,
            "= 1..4 | {}(1..100)",
            "= 0 | {}(--seed=7)",
            "= {}(1; 2)",
        ],
        "t_score" => &["= 12 | {}(40; 60; 1000)", "= 12 | {}(40; 60)"],
        "log_dice" => &["= 12 | {}(40; 60)", "= 12 | {}(40)"],
        "decode" => &[
            r#"= "YXJtYQ==" | {}("base64")"#,
            r#"= "[1, 2]" | {}("json")"#,
            r#"= "x" | {}("sha256")"#,
            r#"= "x" | {}"#,
        ],
        "duration" => &[
            r#"= "90 min" | {}"#,
            "= 2026-03-02 | {}(--to=2026-03-09)",
            "= {}(2026-03-02; --to=2026-03-09)",
            "= 2026-03-02 | {}(--from=2026-03-01; --to=2026-03-09)",
        ],
        "strftime" => &[
            r#"= 2026-03-02 | {}("%A %d %B %Y")"#,
            r#"= 2026-03-02 | {}("%A"; "fr_FR")"#,
            r#"= 2026-03-02 | {}("%A"; "fr_FR"; "gregory"; "more")"#,
        ],
        "strptime" => &[
            r#"= "02.03.2026" | {}("%d.%m.%Y")"#,
            r#"= "x" | {}("%d.%m.%Y")"#,
            r#"= 2026-03-02 | {}("%d.%m.%Y")"#,
        ],
        _ => &[],
    }
}

#[test]
fn an_alias_answers_what_its_command_answers() {
    let aliases = aliases();
    // the eighteen of the day, at least
    assert!(aliases.len() >= 18, "{aliases:?}");
    for (alias, command) in &aliases {
        let samples = samples(command);
        assert!(
            !samples.is_empty(),
            "the alias '{alias}' of '{command}' has no sample query in this test: add one"
        );
        for sample in samples {
            // no command's name has a hyphen (the long counters
            // lost theirs, 2026-11-15): a name with one stood after
            // a pipe alone, no call and no headless word of a record
            assert!(!alias.contains('-') && !command.contains('-'), "{alias}");
            let by_command = run(&["--seed", "42", &sample.replace("{}", command)]);
            let by_alias = run(&["--seed", "42", &sample.replace("{}", alias)]);
            // the answer, what is said beside it, and a refusal's
            // words: all the command's
            assert_eq!(by_alias, by_command, "{alias}: {sample}");
            assert!(
                !by_alias.2.contains(&format!("'{alias}'")),
                "{alias}: {sample}: {}",
                by_alias.2
            );
        }
    }
}

#[test]
fn an_alias_is_spelled_by_its_commands_name() {
    for (alias, command) in aliases() {
        // the canonical text of a query writes the command
        let sample = samples(&command)[0];
        let (success, written, err) = run(&["--expand", &sample.replace("{}", &alias)]);
        assert!(success, "{alias}: {err}");
        let (_, canonical, _) = run(&["--expand", &sample.replace("{}", &command)]);
        assert_eq!(written, canonical, "{alias}");
        let words: Vec<&str> = written
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .collect();
        assert!(
            words.contains(&command.as_str()) && !words.contains(&alias.as_str()),
            "{alias}: {written}"
        );
    }
    // headless in a record alike (it kept the alias there: the one
    // call the evaluator met under a second name)
    let (_, out, _) = run(&["--expand", "= @(2; 3) | %(a = avg; s = stddev)"]);
    assert_eq!(out, "= @(2; 3) | %(a = mean; s = std)");
}

#[test]
fn man_prints_one_page_under_either_name() {
    for (alias, command) in aliases() {
        let (success, page, err) = run(&["man", &command]);
        assert!(success, "{command}: {err}");
        // the command's page names its aliases
        let row = page
            .lines()
            .find(|l| l.starts_with("  aliases "))
            .unwrap_or_else(|| panic!("{command}: {page}"));
        assert!(
            row.split_whitespace().skip(1).any(|a| a == alias),
            "{command}: {row}"
        );
        // the alias says what it is, then prints that page
        let (success, out, err) = run(&["man", &alias]);
        assert!(success, "{alias}: {err}");
        assert_eq!(
            out,
            format!("{alias} — alias of {command}\n\n{page}"),
            "{alias}"
        );
        let (success, out, err) = run(&["man", "--ru", &alias]);
        assert!(success, "{alias}: {err}");
        assert!(
            out.starts_with(&format!("{alias} — псевдоним {command}\n\n{command} — ")),
            "{alias}: {out}"
        );
    }
    // the list has a line for each alias under its command
    let (_, list, _) = run(&["man"]);
    let lines: Vec<&str> = list.lines().collect();
    for (alias, command) in aliases() {
        let at = lines
            .iter()
            .position(|l| l.split_whitespace().next() == Some(alias.as_str()))
            .unwrap_or_else(|| panic!("{alias} is not in the list"));
        assert!(
            lines[at].ends_with(&format!("  alias of {command}")),
            "{}",
            lines[at]
        );
        let above = lines[..at]
            .iter()
            .rev()
            .find(|l| !l.contains("  alias of "))
            .unwrap();
        assert_eq!(above.split_whitespace().next(), Some(command.as_str()));
    }
}

#[test]
fn the_long_counters_have_no_hyphen() {
    // the user, 2026-11-15: "Rename to dashless forms, wordcount
    // etc." With the hyphen they stood after a pipe alone:
    // `word-count("x")` was a parse error and `%(n = word-count)` the
    // text "word-count". Renamed, with no alias and no grace period.
    for (long, short, answer) in [
        ("codepointcount", "cc", "13"),
        ("charcount", "gc", "13"),
        ("bytecount", "bc", "13"),
        ("wordcount", "wc", "2"),
        ("linecount", "lc", "1"),
        ("sentencecount", "sc", "1"),
    ] {
        for query in [
            format!(r#"= "arma virumque" | {long}"#),
            format!(r#"= {long}("arma virumque")"#),
        ] {
            let (success, out, err) = run(&[&query]);
            assert!(success, "{query}: {err}");
            assert_eq!(out, answer, "{query}");
        }
        // headless in a record, as its command stands there
        let (success, out, err) = run(&[&format!(
            r#"= "arma virumque" | %(long = {long}; short = {short})"#
        )]);
        assert!(success, "{long}: {err}");
        assert_eq!(
            out,
            format!("%(long = {answer}; short = {answer})"),
            "{long}"
        );
    }
    // the old spelling names no command, and is told the new one
    let (success, _, err) = run(&[r#"= "arma virumque" | word-count"#]);
    assert!(!success);
    assert!(
        err.contains("'word-count' is not a command; the nearest: wordcount"),
        "{err}"
    );
    // a bare word with a hyphen in a record is a text, as it was
    let (_, out, _) = run(&[r#"= 1 | %(n = word-count)"#]);
    assert_eq!(out, r#"%(n = "word-count")"#);
}

#[test]
fn strip_is_retired_with_a_pointer() {
    // it was an alias of `deaccent`, kept one release on 2026-11-03
    // (the user, 2026-11-15: "Retire strip in 0.65"). The name is
    // Python's `strip` to one reader and the old alias to another,
    // so the pointer names both commands.
    for query in [
        r#"= "café" | strip"#,
        r#"= "café" | strip("fr")"#,
        r#"= strip("café")"#,
        r#"= "café" | %(bare = strip)"#,
    ] {
        let (success, _, err) = run(&[query]);
        assert!(!success, "{query} was taken");
        assert!(
            err.contains(
                "'strip' is retired: 'deaccent' without an operand removes every combining \
                 mark, and 'trim' removes surrounding whitespace"
            ),
            "{query}: {err}"
        );
    }
    // every retired command is refused headless too (`rec` still
    // answered the empty record there), and none is advice for a
    // word that names no command
    let (success, _, err) = run(&["= 1 | %(a = rec)"]);
    assert!(!success && err.contains("'rec(…)' is retired"), "{err}");
    let (_, _, err) = run(&["= 1 | stri"]);
    assert!(
        err.contains("'stri' is not a command") && !err.contains("strip"),
        "{err}"
    );
    // what it pointed to answers
    assert_eq!(run(&[r#"= "café" | deaccent"#]).1, "cafe");
    assert_eq!(run(&[r#"= "  café " | trim"#]).1, "café");
    // no alias of any command, and its page says what became of it
    assert!(aliases().iter().all(|(alias, _)| alias != "strip"));
    let (success, page, err) = run(&["man", "strip"]);
    assert!(success, "{err}");
    assert!(page.starts_with("strip — retired: deaccent "), "{page}");
    assert!(!page.contains("alias"), "{page}");
}

#[test]
fn a_command_without_an_alias_has_no_such_row() {
    let (_, page, _) = run(&["man", "upper"]);
    assert!(!page.contains("aliases"), "{page}");
    // a word near an alias is told the alias
    let (success, _, err) = run(&["= 1 | stdde"]);
    assert!(!success);
    assert!(
        err.contains("'stdde' is not a command; the nearest: std, stddev"),
        "{err}"
    );
    // the registry's listing has one row a command, the aliases in
    // their cell
    let (_, listing, _) = run(&["--commands"]);
    assert!(
        listing.starts_with("name\taliases\tkind\tpipes\tlayer\tonnull\tdoc\n"),
        "{listing}"
    );
    assert!(
        listing
            .lines()
            .any(|l| l.starts_with("mean\tavg\treducing\t"))
    );
    assert!(!listing.lines().any(|l| l.starts_with("avg\t")));
}
