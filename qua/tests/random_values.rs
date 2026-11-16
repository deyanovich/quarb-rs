//! Random values and the binomial coefficient (rulings 2026-11-15;
//! spec: Random Values).
//!
//! `rnd` (alias `random`) draws a value, `sample` and `shuffle` draw
//! from a stream, and a seed makes every draw repeat: `qua --seed
//! N` for the run, `--seed=n` on `sample` and `shuffle` for the one
//! command, `session(--seed=n)` for a session. The generator is the
//! engine's own (SplitMix64 to derive, xoshiro256** to draw) and is
//! frozen by the spec, so the answers pinned below are the
//! language's: they were computed by an implementation written
//! apart from the engine's, and a change to them is a change of the
//! language. `choose` (alias `binomial`) counts by cancellation.

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

/// The lines a seeded run answers.
fn seeded(seed: &str, query: &str) -> Vec<String> {
    let (success, out, err) = run(&["--seed", seed, query]);
    assert!(success, "{query}: {err}");
    out.lines().map(str::to_string).collect()
}

/// The answer of a seeded run as one text.
fn answer(seed: &str, query: &str) -> String {
    seeded(seed, query).join("\n")
}

/// The refusal a query answers.
fn refusal(query: &str) -> String {
    let (success, _, err) = run(&["--seed", "1", query]);
    assert!(!success, "{query} was taken");
    err
}

#[test]
fn choose_counts_by_cancellation() {
    assert_eq!(answer("1", "= 52 | choose(5)"), "2598960");
    assert_eq!(answer("1", "= 5 | choose(0)"), "1");
    assert_eq!(answer("1", "= 5 | choose(5)"), "1");
    assert_eq!(answer("1", "= 0 | choose(0)"), "1");
    // a size above the number has no way
    assert_eq!(answer("1", "= 5 | choose(6)"), "0");
    // no factorial is formed, so it answers wherever the result
    // fits an integer, far past where `factorial` ends
    assert_eq!(answer("1", "= 1000000 | choose(3)"), "166666166667000000");
    assert_eq!(answer("1", "= 60 | choose(30)"), "118264581564861424");
    assert_eq!(answer("1", "= 66 | choose(33)"), "7219428434016265740");
    assert_eq!(
        answer("1", "= 9223372036854775807 | choose(1)"),
        "9223372036854775807"
    );
    // the alias, the expression form and the computed size
    assert_eq!(answer("1", "= 52 | binomial(5)"), "2598960");
    assert_eq!(answer("1", "= choose(52; 5)"), "2598960");
    assert_eq!(answer("1", "= 5 | .k | 52 | choose($.k)"), "2598960");
    // a scalar command: a list item by item
    assert_eq!(answer("1", "= @(4; 5; 6) | choose(2)"), "@(6; 10; 15)");
    // against the product of a range, the long way
    assert_eq!(
        answer("1", "= (@(48..52) | prod) idiv (@(1..5) | prod)"),
        answer("1", "= 52 | choose(5)")
    );
}

#[test]
fn choose_answers_null_past_the_integers_and_says_so() {
    for (query, said) in [
        (
            "= 67 | choose(33)",
            "outside that (67 choose 33, past the integers)",
        ),
        ("= -1 | choose(2)", "outside that (-1)"),
        ("= 5 | choose(-1)", "outside that (the size -1)"),
        ("= 2.5 | choose(1)", "outside that (2.5)"),
        ("= \"x\" | choose(1)", "of another kind"),
    ] {
        let (success, out, err) = run(&[query]);
        assert!(success, "{query}: {err}");
        assert_eq!(out, "null", "{query}");
        assert!(
            err.contains("choose takes a non-negative integer") && err.contains(said),
            "{query}: {err}"
        );
    }
    assert_eq!(answer("1", "= null | choose(2)"), "null");
    // the size is not optional
    let err = refusal("= 5 | choose");
    assert!(err.contains("choose: it takes the size to choose"), "{err}");
}

#[test]
fn rnd_draws_a_float_an_integer_a_member_or_a_value() {
    // a float from 0 up to 1
    let f: f64 = answer("7", "= rnd()").parse().unwrap();
    assert!((0.0..1.0).contains(&f), "{f}");
    // an integer of the inclusive range, one draw per thread
    let dice = seeded("7", "= 1..200 | rnd(1..6)");
    assert_eq!(dice.len(), 200);
    for face in ["1", "2", "3", "4", "5", "6"] {
        assert!(dice.iter().any(|d| d == face), "no {face} in 200 throws");
    }
    assert!(
        dice.iter().all(|d| ("1"..="6").contains(&d.as_str())),
        "{dice:?}"
    );
    // a range of one value
    assert_eq!(answer("7", "= rnd(5..5)"), "5");
    // one member of a list, one value of a record (the key left
    // behind); none of an empty one
    let member = answer("7", r#"= rnd(@("a"; "b"; "c"))"#);
    assert!(["a", "b", "c"].contains(&member.as_str()), "{member}");
    let value = answer("7", r#"= rnd(%(x = "a"; y = "b"))"#);
    assert!(["a", "b"].contains(&value.as_str()), "{value}");
    assert_eq!(answer("7", "= rnd(@())"), "null");
    // it reads no topic: a list before it is one thread, one draw
    let one = seeded("7", "= @(1; 2; 3) | rnd()");
    assert_eq!(one.len(), 1);
    assert!(one[0].parse::<f64>().is_ok(), "{one:?}");
    // in a record, a predicate and after a pipe, bare
    let rows = seeded("7", "= 1..3 | %(die = rnd(1..6))");
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.starts_with("%(die = ")), "{rows:?}");
    let kept = seeded("7", "= 1..400 | [rnd() < 0.5] @| count");
    let kept: i64 = kept[0].parse().unwrap();
    assert!((150..250).contains(&kept), "{kept} of 400 kept at one half");
    assert!(answer("7", "= 0 | rnd").parse::<f64>().is_ok());
    // what it does not take
    let err = refusal("= rnd(6)");
    assert!(err.contains("rnd takes nothing, a range"), "{err}");
    let err = refusal("= rnd(6..1)");
    assert!(err.contains("rnd takes nothing, a range"), "{err}");
}

#[test]
fn random_is_the_alias_of_rnd() {
    assert_eq!(answer("7", "= random()"), answer("7", "= rnd()"));
    assert_eq!(answer("7", "= random(1..6)"), answer("7", "= rnd(1..6)"));
    let (success, out, err) = run(&["--expand", "= 0 | random(1..6)"]);
    assert!(success, "{err}");
    assert_eq!(out, "= 0 | rnd(1..6)");
}

#[test]
fn a_seed_makes_every_draw_repeat() {
    for query in [
        "= rnd()",
        "= 1..20 | rnd(1..1000000)",
        "= 1..5 | %(a = rnd(); b = rnd())",
        "= @(1..50) | sample(5)",
        "= @(1..50) | shuffle",
        "= 1..50 @| sample(5)",
        "= 1..10 @| shuffle",
    ] {
        assert_eq!(seeded("42", query), seeded("42", query), "{query}");
        assert_ne!(seeded("42", query), seeded("43", query), "{query}");
    }
    // one draw per thread, and each its own: twenty throws of a
    // million-sided die are not one number, and two draws in one
    // record are two
    let throws = seeded("42", "= 1..20 | rnd(1..1000000)");
    let distinct: std::collections::HashSet<&String> = throws.iter().collect();
    assert!(distinct.len() > 15, "{throws:?}");
    for row in seeded("42", "= 1..5 | %(a = rnd(); b = rnd())") {
        let (a, b) = row
            .trim_start_matches("%(a = ")
            .trim_end_matches(')')
            .split_once("; b = ")
            .unwrap();
        assert_ne!(a, b, "{row}");
    }
}

/// The answers of a seed, computed by scripts/random-reference.py,
/// an implementation of the generator written apart from the
/// engine's. They are the language's: a change here is a change of
/// what every seeded run answers.
#[test]
fn the_answers_of_a_seed_are_frozen() {
    // the first command that draws is place 1; one thread alone is
    // at position 1
    assert_eq!(answer("42", "= rnd()"), "0.6597917789409309");
    assert_eq!(answer("42", "= rnd(1..6)"), "6");
    assert_eq!(
        answer("42", r#"= rnd(@("a"; "b"; "c"; "d"; "e"; "f"; "g"; "h"))"#),
        "d"
    );
    assert_eq!(answer("7", "= rnd()"), "0.23349038612316275");
    // the place is counted among the commands that draw: the head
    // before this one draws nothing, so the place is 1 again, the
    // threads at positions 1 to 3
    assert_eq!(
        seeded("42", "= 1..3 | rnd()"),
        [
            "0.6597917789409309",
            "0.48125006170743845",
            "0.48322554775683924"
        ]
    );
    // a second command that draws is place 2
    assert_eq!(
        seeded("42", "= 1..3 | [rnd() < 2] | rnd()"),
        [
            "0.7843785876073921",
            "0.8400299109043515",
            "0.011291710533225752"
        ]
    );
    // a thread's second draw in one command
    assert_eq!(
        seeded("42", "= 1..3 | %(a = rnd(); b = rnd())")[0],
        "%(a = 0.6597917789409309; b = 0.9258260899840721)"
    );
    // a shuffle and a sample on the plain pipe (thread 1) and after
    // the aggregation pipe (no one thread: position 0)
    let letters = r#"@("a"; "b"; "c"; "d"; "e"; "f"; "g"; "h")"#;
    assert_eq!(
        answer("42", &format!("= {letters} | shuffle | join(\"\")")),
        "bgechafd"
    );
    assert_eq!(
        answer("42", &format!("= {letters} | ... @| shuffle @| join(\"\")")),
        "becdfhga"
    );
    assert_eq!(
        answer("42", "= @(0; 1; 2; 3; 4; 5; 6; 7) | sample(3)"),
        "@(1; 4; 6)"
    );
    assert_eq!(
        answer(
            "42",
            "= @(0; 1; 2; 3; 4; 5; 6; 7) | ... @| sample(3) @| join(\" \")"
        ),
        "1 2 4"
    );
    assert_eq!(
        answer("42", "= @(0; 1; 2) | sample(5; --replace)"),
        "@(2; 0; 0; 0; 0)"
    );
}

#[test]
fn the_threads_drawn_keep_their_nodes() {
    // after the aggregation pipe the threads drawn are the threads,
    // nodes with them (they answered the threads' values, null for
    // a node), and the same seed picks the same places of nodes and
    // of values, whatever is written before the command
    let dir = std::env::temp_dir().join(format!("qua-random-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rows: Vec<String> = (1..=20)
        .map(|i| format!(r#"{{"id": {i}, "name": "n{i}"}}"#))
        .collect();
    let file = dir.join("rows.json");
    std::fs::write(&file, format!(r#"{{"rows": [{}]}}"#, rows.join(", "))).unwrap();
    let file = file.to_string_lossy().into_owned();
    let ask = |query: &str| {
        let (success, out, err) = run(&["--seed", "42", query, &file]);
        assert!(success, "{query}: {err}");
        out.lines().map(str::to_string).collect::<Vec<_>>()
    };
    let nodes = ask("/rows/* @| sample(3) | ::id");
    assert_eq!(nodes.len(), 3);
    assert!(
        nodes.iter().all(|id| id.parse::<i64>().is_ok()),
        "{nodes:?}"
    );
    assert_eq!(nodes, ask("/rows/* | ::id @| sample(3)"));
    assert_eq!(nodes, ask("/rows/* | %(::id; ::name) @| sample(3) | :id"));
    // every property of a row drawn is there to read
    let names = ask("/rows/* @| sample(3) | ::name");
    assert_eq!(
        names,
        nodes.iter().map(|id| format!("n{id}")).collect::<Vec<_>>()
    );
    // a shuffle keeps the nodes too, and a row drawn twice stands
    // twice
    let shuffled = ask("/rows/* @| shuffle | ::id");
    assert_eq!(shuffled.len(), 20);
    assert_ne!(shuffled, ask("/rows/* | ::id"));
    assert_eq!(ask("/rows/* @| sample(30; --replace) | ::id").len(), 30);
    // under a fold, one of each group
    assert_eq!(
        ask("/rows/* | .k(::id mod 2) @|{.k} sample(1) | ::id").len(),
        2
    );
}

#[test]
fn an_unpinned_run_draws_a_seed_and_names_it() {
    // two runs differ (one chance in 2^53 that they do not)
    let (_, first, _) = run(&["= rnd()"]);
    let (_, second, _) = run(&["= rnd()"]);
    assert_ne!(first, second);
    // the info names the seed, and the seed repeats the run
    let (success, out, err) = run(&["--info", "= 1..3 | rnd(1..1000000)"]);
    assert!(success, "{err}");
    let line = err
        .lines()
        .find(|l| l.contains("RANDOM_SEED"))
        .unwrap_or_else(|| panic!("no seed named: {err}"));
    let seed = line
        .split("--seed=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no seed to write back: {line}"));
    assert_eq!(answer(seed, "= 1..3 | rnd(1..1000000)"), out, "{line}");
    // a pinned run names none
    let (_, _, err) = run(&["--info", "--seed", "5", "= rnd()"]);
    assert!(!err.contains("RANDOM_SEED"), "{err}");
}

#[test]
fn sample_draws_from_the_stream_in_its_order() {
    // n members, each once, in the stream's own order
    let picked = answer("42", "= @(1..20) | sample(5)");
    let items: Vec<i64> = picked
        .trim_start_matches("@(")
        .trim_end_matches(')')
        .split("; ")
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(items.len(), 5, "{picked}");
    assert!(items.windows(2).all(|w| w[0] < w[1]), "{picked}");
    assert!(items.iter().all(|n| (1..=20).contains(n)), "{picked}");
    // across the threads: the threads kept, in their order
    assert_eq!(seeded("42", "= 1..20 @| sample(5)").len(), 5);
    // one member where no count is written, and it stays a list
    let one = answer("42", "= @(1..20) | sample");
    assert!(one.starts_with("@(") && !one.contains(';'), "{one}");
    // the whole stream, and none of it
    assert_eq!(answer("42", "= @(1; 2; 3) | sample(3)"), "@(1; 2; 3)");
    assert_eq!(answer("42", "= @(1; 2; 3) | sample(0)"), "@()");
    // a share of the stream, rounded to the nearest whole member
    assert_eq!(seeded("42", "= 1..20 @| sample(--share=0.25)").len(), 5);
    assert_eq!(seeded("42", "= 1..10 @| sample(--share=0.25)").len(), 3);
    // a computed count
    assert_eq!(seeded("42", "= 2 | .n | 1..9 @| sample($.n)").len(), 2);
}

#[test]
fn sample_past_the_size_is_refused_without_replacement() {
    let err = refusal("= @(1; 2; 3) | sample(5)");
    assert!(
        err.contains("sample: 5 of 3 cannot be drawn without replacement"),
        "{err}"
    );
    // with replacement the count may pass the size, in the order
    // drawn
    let drawn = seeded("42", "= 1..3 @| sample(10; --replace)");
    assert_eq!(drawn.len(), 10);
    assert!(
        drawn.iter().all(|d| ["1", "2", "3"].contains(&d.as_str())),
        "{drawn:?}"
    );
    // a count and a share are one too many
    let err = refusal("= @(1; 2; 3) | sample(2; --share=0.5)");
    assert!(
        err.contains("sample: it takes a count or a share, not both"),
        "{err}"
    );
    let err = refusal("= @(1; 2; 3) | sample(--share=2)");
    assert!(err.contains("--share takes a number from 0 to 1"), "{err}");
    let err = refusal("= @() | sample(1; --replace)");
    assert!(err.contains("there is nothing to draw from"), "{err}");
}

#[test]
fn shuffle_keeps_every_member() {
    let shuffled = seeded("42", "= 1..30 @| shuffle");
    assert_eq!(shuffled.len(), 30);
    let mut sorted: Vec<i64> = shuffled.iter().map(|n| n.parse().unwrap()).collect();
    assert_ne!(sorted, (1..=30).collect::<Vec<i64>>(), "left in order");
    sorted.sort_unstable();
    assert_eq!(sorted, (1..=30).collect::<Vec<i64>>());
    // a list on the plain pipe stays one list, of one member too
    assert_eq!(answer("42", "= @(5) | shuffle"), "@(5)");
    assert_eq!(answer("42", "= @() | shuffle"), "@()");
}

#[test]
fn a_command_s_own_seed_outranks_the_run_s() {
    let query = "= @(1..50) | sample(5; --seed=9)";
    assert_eq!(seeded("1", query), seeded("2", query));
    let query = "= @(1..50) | shuffle(--seed=9)";
    assert_eq!(seeded("1", query), seeded("2", query));
    // and the session's setting pins the runs after it
    let query = "= 0 | session(--seed=9) | rnd()";
    let (_, first, _) = run(&[query]);
    let (_, second, _) = run(&[query]);
    assert_eq!(first, second);
    assert!(first.parse::<f64>().is_ok(), "{first}");
    let err = refusal("= @(1; 2) | shuffle(--seed=-1)");
    assert!(err.contains("--seed takes a whole number"), "{err}");
}
