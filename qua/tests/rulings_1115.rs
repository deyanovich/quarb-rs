//! Four rulings of 2026-11-15: a list of one stays a list under the
//! ordering commands, `combinations` has the bound `permutations`
//! has, `sample` and `shuffle` draw the rows of a table, and the
//! frontier bound counts the paths beyond one per node.

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

/// The answer of an expression that reads no file, under a seed.
fn answer(query: &str) -> String {
    let (success, out, err) = run(&["--seed", "42", query]);
    assert!(success, "{query}: {err}");
    out
}

/// The refusal a query answers.
fn refusal(query: &str) -> String {
    let (success, _, err) = run(&["--seed", "42", query]);
    assert!(!success, "{query} was taken");
    err
}

#[test]
fn a_list_of_one_stays_a_list_under_the_ordering_commands() {
    // it was handed over bare, as the one value of a `sum` is
    assert_eq!(answer("= @(5) | sort"), "@(5)");
    assert_eq!(answer("= @(5) | unique"), "@(5)");
    assert_eq!(answer("= @(5) | reverse"), "@(5)");
    assert_eq!(answer("= @(5) | frequencies"), "@(%(value = 5; n = 1))");
    assert_eq!(answer("= @(%(n = 5)) | sort(--of=:n)"), "@(5)");
    // so what follows reads a list
    assert_eq!(answer("= @(5) | sort | count"), "1");
    // a count asks for a list, of one as of several; no count is
    // the one value
    assert_eq!(answer("= @(5; 6) | first(1)"), "@(5)");
    assert_eq!(answer("= @(5; 6) | last(1)"), "@(6)");
    assert_eq!(answer("= @(5; 6) | first(2)"), "@(5; 6)");
    assert_eq!(answer("= @(5; 6) | first(0)"), "@()");
    assert_eq!(answer("= @(5; 6) | first"), "5");
    assert_eq!(answer("= @(5; 6) | last"), "6");
    // several were a list already, and none is the empty list
    assert_eq!(answer("= @(3; 1; 2) | sort"), "@(1; 2; 3)");
    assert_eq!(answer("= @() | sort"), "@()");
    // a topic that is no list is one value before and after
    assert_eq!(answer("= 5 | sort"), "5");
    assert_eq!(answer("= 5 | first(1)"), "5");
    assert_eq!(answer("= 5 | frequencies"), "%(value = 5; n = 1)");
    assert_eq!(answer("= null | sort"), "null");
    // a record keeps its own reading, and a value its own
    assert_eq!(answer("= %(a = 1; b = 2) | first(1)"), "%(a = 1)");
    assert_eq!(answer("= @(5) | sum"), "5");
    // the windows with a skip are windows
    assert_eq!(
        answer(r#"= @("a"; "b") | skipgrams(2; 1)"#),
        r#"@(@("a"; "b"))"#
    );
}

#[test]
fn combinations_past_the_bound_are_refused() {
    // twenty-two choose eleven is written out; twenty-five choose
    // twelve (5,200,300) was, and thirty choose fifteen ran until
    // the memory ended
    assert_eq!(answer("= @(1..22) | combinations(11) | count"), "705432");
    let err = refusal("= @(1..25) | combinations(12) | count");
    assert!(
        err.contains(
            "combinations: 12 of 25 values have more than 1000000 subsets: choose fewer of \
             them, or count them (25 | choose(12))"
        ),
        "{err}"
    );
    let err = refusal("= @(1..30) | combinations(15)");
    assert!(err.contains("(30 | choose(15))"), "{err}");
    // the count it names answers
    assert_eq!(answer("= 30 | choose(15)"), "155117520");
    // the pairs are combinations of two
    assert_eq!(answer("= @(1..1414) | pairs | count"), "998991");
    let err = refusal("= @(1..1415) | pairs | count");
    assert!(
        err.contains("pairs: 2 of 1415 values have more than 1000000 subsets"),
        "{err}"
    );
}

#[test]
fn sample_and_shuffle_draw_the_rows_of_a_table() {
    let table = r#"#(@(g; n); %("a"; 2); %("b"; 3); %("c"; 4); %("d"; 5))"#;
    // rows drawn, a table back, in the table's own order (they were
    // refused: "a frame 4×2 is a table")
    assert_eq!(
        answer(&format!("= {table} | sample(2)")),
        "#(@(g; n);\n  %(\"b\"; 3);\n  %(\"c\"; 4))"
    );
    assert_eq!(answer(&format!("= {table} | sample(2) | count")), "2");
    assert_eq!(
        answer(&format!("= {table} | sample(--share=0.5) | count")),
        "2"
    );
    // one row where no count is written: still a table
    assert_eq!(answer(&format!("= {table} | sample | count")), "1");
    assert!(answer(&format!("= {table} | sample")).starts_with("#(@(g; n);\n"));
    // every row, in a random order
    assert_eq!(
        answer(&format!("= {table} | shuffle")),
        "#(@(g; n);\n  %(\"b\"; 3);\n  %(\"c\"; 4);\n  %(\"a\"; 2);\n  %(\"d\"; 5))"
    );
    // with replacement a row may come twice, in the order drawn
    assert_eq!(
        answer(&format!("= {table} | sample(6; --replace) | count")),
        "6"
    );
    let err = refusal(&format!("= {table} | sample(5)"));
    assert!(
        err.contains("sample: 5 of 4 cannot be drawn without replacement"),
        "{err}"
    );
    // the rows are drawn by the positions a list of that length
    // gives: the same seed, the same places
    assert_eq!(answer("= @(1; 2; 3; 4) | sample(2)"), "@(2; 3)");
}

#[test]
fn the_frontier_bound_counts_the_paths_beyond_one_per_node() {
    // forty branches of a tree, one path each at every round: the
    // size of the document, not routes that multiply. Under a bound
    // of ten the walk was refused at round 1.
    let dir = std::env::temp_dir().join(format!("qua-rulings-1115-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let branches: Vec<String> = (1..=40)
        .map(|i| format!(r#""x{i}": {{"k": {{"v": {i}}}}}"#))
        .collect();
    let file = dir.join("tree.json");
    std::fs::write(&file, format!(r#"{{"a": {{{}}}}}"#, branches.join(", "))).unwrap();
    let file = file.to_string_lossy().into_owned();
    for query in [
        "/a(/*){2} @| count",
        "/a(/*){3} @| count",
        "/a(/*)+! @| count",
    ] {
        let (success, out, err) = run(&["--frontier-bound", "10", query, &file]);
        assert!(success, "{query}: {err}");
        assert_eq!(out, "40", "{query}");
    }
}
