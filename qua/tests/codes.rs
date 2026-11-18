//! The diagnostic codes (ruling 2026-11-17): one word per reason
//! where `NOT_PUSHED` was the one kind, each line ending with the
//! door to its remedy, `qua man CODE` the page behind the door, and
//! every code the workspace raises a constant of the engine's list,
//! which a test in the engine holds against the inventory
//! (`langspec/diagnostics.tsv`, vendored).

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-codes-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, born INTEGER);
         INSERT INTO authors VALUES
           (1, 'Homer', -800), (2, 'Austen', 1775), (3, 'Twain', 1835), (4, 'Gibbon', 1737);
         CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, pages INTEGER,
           author_id INTEGER REFERENCES authors(id));
         INSERT INTO books VALUES
           (1, 'Iliad', 300, 1), (2, 'Emma', 320, 2), (3, 'Tom Sawyer', 240, 3),
           (4, 'Persuasion', 250, 2);",
    )
    .unwrap();
    dir
}

/// Whether the run succeeded, stdout, and stderr.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

/// The info lines of a query over the fixture, in order.
fn told(dir: &Path, query: &str) -> Vec<String> {
    let (ok, _, err) = run(dir, &["--diagnostics=info", query, "lib.db"]);
    assert!(ok, "{query}: {err}");
    err.lines().map(str::to_string).collect()
}

#[test]
fn a_refusal_is_told_under_its_word_with_the_door_to_its_remedy() {
    let dir = fixture("word");
    // the user's example of the ruling
    assert_eq!(
        told(&dir, "/books/* @| sort_by(::title) @| [..2] | ::title"),
        [
            "info SORT_NOT_PUSHED (query): ORDER BY collations differ per engine; scanning. \
             Narrow first: qua man SORT_NOT_PUSHED"
        ]
    );
    // a word whose remedy is another spelling
    assert_eq!(
        told(&dir, r#"/books/* <=> /authors/*[::name = "Homer"]"#),
        [
            "info JOIN_NOT_PUSHED (query): a correlation without a '$$' equality (no JOIN \
             condition); scanning. Spell it another way: qua man JOIN_NOT_PUSHED"
        ]
    );
    // a word with nothing to rewrite has no tail
    assert_eq!(
        told(&dir, "/books/*->author::name @| count"),
        [
            "info WALK_NOT_PUSHED (query): navigation beyond /table/* (SQL sees tables and \
             rows); scanning"
        ]
    );
    // a negation is pushed in the form that keeps the rows SQL's NOT
    // would drop; over a key comparison of a join it is SQL's own
    // NOT, whose unknown is the engine's there (0.71: it stayed with
    // the engine)
    assert_eq!(
        told(&dir, "/books/*[!(::pages > 245)] @| count"),
        [
            "info PUSHED (query): pushdown: SELECT COUNT(*) FROM books WHERE (CASE WHEN \
             (pages > 245) THEN 0 ELSE 1 END = 1)"
        ]
    );
    assert_eq!(
        told(
            &dir,
            "/books/* <=>* /authors/*[::id = $$0::author_id && !(::born > $$0::pages)] @| count"
        ),
        [
            "info PUSHED (query): pushdown: SELECT COUNT(*) FROM books JOIN authors ON \
             authors.id = books.author_id WHERE NOT ((authors.born > books.pages))"
        ]
    );
    // a negated key comparison alone is no JOIN condition
    assert_eq!(
        told(
            &dir,
            "/books/* <=>* /authors/*[!(::id = $$0::author_id)] @| count"
        ),
        [
            "info JOIN_NOT_PUSHED (query): a correlation without a '$$' equality (no JOIN \
             condition); scanning. Spell it another way: qua man JOIN_NOT_PUSHED"
        ]
    );
}

/// The prefilter's line (the first cleanup): told where a condition
/// was sent ahead, and where one could not be; silent where there
/// was none to send, and where its refusal is the whole plan's own.
#[test]
fn the_prefilter_speaks_only_of_a_condition() {
    let dir = fixture("prefilter");
    // a leading condition, sent ahead
    assert_eq!(
        told(
            &dir,
            "/books/*[::pages > 245] @| sort_by(::title) | ::title"
        ),
        [
            "info SORT_NOT_PUSHED (query): ORDER BY collations differ per engine; scanning. \
             Narrow first: qua man SORT_NOT_PUSHED",
            "info PUSHED (prefilter): partial pushdown: WHERE pages > 245 on books; the rest \
             scans the filtered set"
        ]
    );
    // a leading condition that what follows would see through
    assert_eq!(
        told(&dir, "/books/*[::pages > 245] | %(::title; n = :::n-rows)"),
        [
            "info OPERATOR_NOT_PUSHED (query): the adapter metadata projection; scanning",
            "info CONDITION_NOT_PUSHED (prefilter): metadata would observe the filtering \
             (:::n-rows, :index); scanning"
        ]
    );
    assert_eq!(
        told(
            &dir,
            "/books/*[::pages > 245] | %(::title; n = (/books/* @| count))"
        )[1],
        "info CONDITION_NOT_PUSHED (prefilter): the table is reached more than once; scanning"
    );
    // no leading condition: nothing more than the whole plan's line
    assert_eq!(
        told(&dir, "/books/* @| sort_by(::title) | ::title").len(),
        1
    );
    // the partial plan refused for the whole plan's reason: said once
    assert_eq!(
        told(&dir, "/books/*[::pages > 245](->author) || /authors/*"),
        ["info WALK_NOT_PUSHED (query): path patterns (groups and quantifiers); scanning"]
    );
}

/// The second cleanup: a query that does not parse is told by its
/// error alone; the planner, which reads with the same parser, adds
/// no line of its own.
#[test]
fn a_query_that_does_not_parse_has_no_planner_line() {
    let dir = fixture("syntax");
    let (ok, out, err) = run(&dir, &["--diagnostics=info", "/books/*[", "lib.db"]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(err.starts_with("Error: parse error: "), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn a_run_with_pushdown_off_names_what_turned_it_off() {
    let dir = fixture("option");
    let (ok, out, err) = run(
        &dir,
        &[
            "--diagnostics=info",
            "--no-pushdown",
            "/books/* @| count",
            "lib.db",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(out, "4");
    assert_eq!(
        err,
        "info OPTION_NOT_PUSHED (query): nothing is pushed under --no-pushdown; scanning. \
         A choice to make: qua man OPTION_NOT_PUSHED"
    );
}

/// A translator's refusal is one line under the word of its reason,
/// the bridge its site, and the run ends with status 1. The tail
/// names `--lenient` only where the flag would translate what was
/// refused, and never under the flag itself.
#[test]
fn a_translators_refusal_is_one_coded_line() {
    let dir = fixture("bridge");
    std::fs::write(dir.join("e.json"), r#"{"events":[{"id":1,"ms":5}]}"#).unwrap();
    let refused = |args: &[&str]| {
        let (ok, out, err) = run(&dir, args);
        assert!(!ok && out.is_empty(), "{args:?}: {out}");
        assert_eq!(err.lines().count(), 1, "{err}");
        err
    };
    // jq: the lenient translator takes a by-key function
    let err = refused(&["--jq", ".events | sort_by(.ms)", "e.json"]);
    assert!(
        err.starts_with("error SORT_NOT_TRANSLATED (jq): the sort_by function (")
            && err.ends_with("). Or pass --lenient: qua man SORT_NOT_TRANSLATED"),
        "{err}"
    );
    // under the flag a refusal of the same word names no flag
    let err = refused(&["--jq", "--lenient", ".events | group_by(.id)", "e.json"]);
    assert!(
        err.starts_with("error SORT_NOT_TRANSLATED (jq): the group_by function without")
            && !err.contains("qua man"),
        "{err}"
    );
    // a construct no flag lifts
    assert_eq!(
        refused(&["--jq", "..", "e.json"]),
        "error DESCENT_NOT_TRANSLATED (jq): recursive descent .. (use Quarb's // in a \
         hand-written query). Write it in Quarb: qua man DESCENT_NOT_TRANSLATED"
    );
    // XPath
    assert_eq!(
        refused(&["--xpath", "/a/following::b", "e.json"]),
        "error AXIS_NOT_TRANSLATED (xpath): the following:: axis (no Quarb equivalent). \
         Write it in Quarb: qua man AXIS_NOT_TRANSLATED"
    );
    // SQL: the bridge has no lenient form
    assert_eq!(
        refused(&[
            "--sql",
            "SELECT a.name FROM authors a FULL JOIN books b ON b.author_id = a.id",
            "lib.db"
        ]),
        "error JOIN_NOT_TRANSLATED (sql): a FULL JOIN (no correlation keeps the unmatched \
         rows of both tables). Write it in Quarb: qua man JOIN_NOT_TRANSLATED"
    );
    assert!(
        refused(&["--sql", "DELETE FROM books", "lib.db"])
            .starts_with("error STATEMENT_NOT_TRANSLATED (sql): only SELECT statements"),
    );
    // a statement that is no SQL at all is an error with no word
    let (ok, _, err) = run(&dir, &["--sql", "SELECT FROM", "lib.db"]);
    assert!(!ok);
    assert!(err.starts_with("Error: translating SQL"), "{err}");
}

/// The SQL bridge end to end, the plan on: what `qua --sql` answers
/// over a database is what SQLite answers (the importer's own rig
/// holds the translations against the scan; here the translated
/// query goes through the planner too).
#[test]
fn the_sql_bridge_answers_as_the_database() {
    let dir = fixture("sqlbridge");
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch("INSERT INTO books VALUES (5, 'Anonymous', NULL, NULL);")
        .unwrap();
    let cases = [
        "SELECT a.name, b.title FROM authors a JOIN books b ON b.author_id = a.id",
        "SELECT a.name, b.title FROM authors a LEFT JOIN books b ON b.author_id = a.id",
        "SELECT a.name, b.title FROM authors a LEFT JOIN books b ON b.author_id = a.id \
         WHERE b.pages IS NULL OR b.pages > 300",
        "SELECT title FROM books WHERE author_id <> 2",
        "SELECT title FROM books WHERE NOT (author_id = 2 OR pages < 250)",
        "SELECT title FROM books WHERE title LIKE '_m%' OR title LIKE '%a%n'",
        "SELECT COUNT(author_id) FROM books",
        "SELECT COUNT(pages) FROM books WHERE author_id = 2",
        "SELECT name FROM authors WHERE born > 1700 UNION ALL SELECT title FROM books \
         WHERE pages > 300",
    ];
    for sql in cases {
        let mut stmt = db.prepare(sql).unwrap();
        let n = stmt.column_count();
        let mut want: Vec<String> = stmt
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| match r.get_ref(i).unwrap() {
                        rusqlite::types::ValueRef::Null => "null".to_string(),
                        rusqlite::types::ValueRef::Integer(v) => v.to_string(),
                        rusqlite::types::ValueRef::Text(t) => {
                            format!("{:?}", String::from_utf8_lossy(t))
                        }
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("; "))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let (ok, out, err) = run(&dir, &["--sql", sql, "lib.db"]);
        assert!(ok, "{sql}: {err}");
        // a record prints as %(name = value; …): the values alone
        let mut got: Vec<String> = out
            .lines()
            .map(|l| {
                match l.strip_prefix("%(").and_then(|l| l.strip_suffix(')')) {
                    Some(fields) => fields
                        .split("; ")
                        .map(|f| f.split_once(" = ").map_or(f, |(_, v)| v))
                        .collect::<Vec<_>>()
                        .join("; "),
                    // a bare value: text prints unquoted
                    None if l.parse::<f64>().is_ok() => l.to_string(),
                    None => format!("{l:?}"),
                }
            })
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "{sql}\n{err}");
    }
}

/// The planner's exact edges of 0.69, through the tool: a sort by a
/// numeric column, the first rows, a fold by a numeric key and a
/// count of distinct values are pushed, the driver writing the
/// order; over a text column each stays with the engine under its
/// own word, the sort under the line of the ruling's example.
#[test]
fn a_numeric_order_is_pushed_and_a_text_one_is_told() {
    let dir = fixture("order");
    let pushed = |q: &str, sql: &str| {
        let lines = told(&dir, q);
        assert_eq!(
            lines,
            [format!("info PUSHED (query): pushdown: {sql}")],
            "{q}"
        );
        // the plan's answer is the scan's
        let plan = run(&dir, &[q, "lib.db"]).1;
        let scan = run(&dir, &["--no-pushdown", q, "lib.db"]).1;
        assert_eq!(plan, scan, "{q}");
        assert!(!plan.is_empty(), "{q}");
    };
    pushed(
        "/books/* @| sort_by(::pages) @| [..2] | ::title",
        "SELECT title FROM books ORDER BY (pages IS NULL), pages, books.id LIMIT 2",
    );
    pushed(
        "/books/* @| sort_by(::pages; --desc) | ::title",
        "SELECT title FROM books ORDER BY (pages IS NULL), pages DESC, books.id",
    );
    pushed(
        "/books/* @| reverse @| [..3] | ::id",
        "SELECT id FROM books ORDER BY books.id DESC LIMIT 3",
    );
    pushed(
        "/books/* | .a(::author_id) @|{.a} count | .n | %.",
        "SELECT author_id AS a, COUNT(*) AS n FROM books GROUP BY author_id ORDER BY \
         MIN(books.id)",
    );
    pushed(
        "/books/*::author_id @| unique @| count",
        "SELECT COUNT(DISTINCT author_id) + COALESCE(MAX(CASE WHEN author_id IS NULL THEN 1 \
         ELSE 0 END), 0) FROM books",
    );
    // more shapes: pushed, and what the tool prints is what it prints
    // with --no-pushdown, names and order included
    for q in [
        "/books/* @| sort_by(::pages; --desc) @| [..3] | %(::id; ::title; p = ::pages)",
        "/books/*[::pages > 245] @| sort_by(::author_id) @| reverse | %(::title)",
        "/books/* | ::pages | .a(::author_id) @|{.a} sum | .total | %.",
        "/books/* | .a(::author_id) @|{.a} count | .n | [$_ > 1] | %.",
        "/books/* | .a(::author_id) @|{.a} count",
        "/books/*[::pages != 300] @| [..2] | ::title",
        "/books/* @| top(2; ::pages) | %(::title; ::pages)",
        "/books/* @| bottom(3; ::author_id) | ::title",
        "/books/*[::pages > 245] | %(::title)",
        "/books/* @| sort_by(::pages) @| count",
    ] {
        let lines = told(&dir, q);
        assert!(
            lines.len() == 1 && lines[0].starts_with("info PUSHED (query): pushdown: SELECT "),
            "{q}: {lines:?}"
        );
        let plan = run(&dir, &[q, "lib.db"]).1;
        let scan = run(&dir, &["--no-pushdown", q, "lib.db"]).1;
        assert_eq!(plan, scan, "{q}");
        assert!(!plan.is_empty(), "{q}");
    }
    // a text key: each under its own word, the answer the scan's
    assert_eq!(
        told(&dir, "/books/* @| sort_by(::title) | ::title"),
        [
            "info SORT_NOT_PUSHED (query): ORDER BY collations differ per engine; scanning. \
             Narrow first: qua man SORT_NOT_PUSHED"
        ]
    );
    assert_eq!(
        told(&dir, "/books/* | .t(::title) @|{.t} count | .n | %."),
        [
            "info FOLD_NOT_PUSHED (query): a fold by title, which is no numeric column: two \
             texts that read as one number are one key to the engine and two to GROUP BY; \
             scanning. Narrow first: qua man FOLD_NOT_PUSHED"
        ]
    );
    assert_eq!(
        told(&dir, "/books/*::title @| unique @| count"),
        [
            "info UNIQUE_NOT_PUSHED (query): a count of the distinct values of title, which \
             is no numeric column: two texts that read as one number are one value to the \
             engine and two to DISTINCT; scanning. Narrow first: qua man UNIQUE_NOT_PUSHED"
        ]
    );
}

/// The page behind the door, the list, and a code that is none.
#[test]
fn man_answers_a_code() {
    let dir = fixture("man");
    let (ok, out, _) = run(&dir, &["man", "SORT_NOT_PUSHED"]);
    assert!(ok);
    assert!(
        out.starts_with("SORT_NOT_PUSHED — info, planner\n  raised    a sort"),
        "{out}"
    );
    assert!(out.contains("\n  remedy    "), "{out}");
    let (ok, out, _) = run(&dir, &["man", "diagnostics", "planner"]);
    assert!(ok);
    assert!(
        out.lines().count() >= 16 && out.lines().all(|l| l.contains("_NOT_PUSHED")),
        "{out}"
    );
    let (ok, _, err) = run(&dir, &["man", "SORT_NOT_PUSHD"]);
    assert!(!ok);
    assert!(
        err.contains("no diagnostic code 'SORT_NOT_PUSHD'; the nearest: SORT_NOT_PUSHED"),
        "{err}"
    );
}

/// No source of the workspace raises a code written out as a
/// literal: every code is a constant of `quarb::diagnostic`, the
/// list the engine's test holds against the inventory, so a new
/// code cannot be raised without its row. Test code is free to
/// make diagnostics of its own.
#[test]
fn no_code_is_raised_as_a_literal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let raisers = [
        "Diagnostic::error(",
        "Diagnostic::warning(",
        "Diagnostic::info(",
        "Diagnostic::new(",
        "record_info(",
        "record_warning(",
        "record_refusal_coded(",
        "record_debug(",
        "note_info(",
        "note_warning(",
        "note_debug(",
    ];
    let mut found: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                // sources only: no build output, no tests, no vendored trees
                if !matches!(
                    name.as_str(),
                    "target" | "tests" | "node_modules" | ".git" | "benches" | "examples"
                ) {
                    stack.push(path);
                }
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            // a file's own unit tests stand after `#[cfg(test)]`
            let code = text.split("#[cfg(test)]").next().unwrap();
            let lines: Vec<&str> = code.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let Some(at) = raisers
                    .iter()
                    .find_map(|r| line.find(r).map(|p| p + r.len()))
                else {
                    continue;
                };
                // the first argument: on the line, or on the next
                // (the severity of `Diagnostic::new` comes first)
                let rest = line[at..].trim();
                let args: Vec<&str> = if rest.is_empty() {
                    lines[i + 1..(i + 3).min(lines.len())]
                        .iter()
                        .map(|l| l.trim())
                        .collect()
                } else {
                    rest.split(',').map(str::trim).collect()
                };
                for arg in args.iter().take(2) {
                    let lit = arg.trim_end_matches(',');
                    if lit.len() > 4
                        && lit.starts_with('"')
                        && lit.ends_with('"')
                        && lit[1..lit.len() - 1]
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c == '_')
                    {
                        found.push(format!(
                            "{}:{}: {lit}",
                            path.strip_prefix(root).unwrap().display(),
                            i + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "codes raised as literals (name them in quarb::diagnostic's list, with a row in \
         langspec/diagnostics.tsv):\n{}",
        found.join("\n")
    );
}

/// DuckDB's door asks the catalog (information_schema.columns) for
/// the type of a plan's key column, as PostgreSQL's does: a sort, a
/// fold, a distinct count, `top` and `bottom` by a numeric column
/// are pushed, a text key is told under the construct's own word,
/// and either way the answer is the scan's.
#[test]
fn the_duckdb_door_pushes_a_numeric_key_and_tells_a_text_one() {
    let dir = std::env::temp_dir().join(format!("quarb-codes-{}-duck", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let db = duckdb::Connection::open(dir.join("lib.duckdb")).unwrap();
        db.execute_batch(
            "CREATE TABLE books (id INTEGER PRIMARY KEY, title VARCHAR, pages INTEGER,
               price DOUBLE);
             INSERT INTO books VALUES
               (1, 'Iliad', 300, 9.5), (2, 'Emma', 320, 7.25), (3, 'Tom Sawyer', 240, NULL),
               (4, 'Persuasion', 250, 7.25), (5, 'Odyssey', NULL, 8.0);",
        )
        .unwrap();
    }
    let told = |q: &str| -> Vec<String> {
        let (ok, _, err) = run(&dir, &["--diagnostics=info", q, "lib.duckdb"]);
        assert!(ok, "{q}: {err}");
        err.lines().map(str::to_string).collect()
    };
    let same = |q: &str| {
        let plan = run(&dir, &[q, "lib.duckdb"]).1;
        let scan = run(&dir, &["--no-pushdown", q, "lib.duckdb"]).1;
        assert_eq!(plan, scan, "{q}");
        assert!(!plan.is_empty(), "{q}");
    };
    for q in [
        "/books/* @| sort_by(::pages) | ::title",
        "/books/* @| sort_by(::price; --desc) @| [..3] | %(::id; ::price)",
        "/books/* @| top(2; ::pages) | ::title",
        "/books/* @| bottom(2; ::price) | %(::title; ::price)",
        "/books/* | .p(::price) @|{.p} count | .n | %.",
        "/books/*::price @| unique @| count",
        "/books/* @| reverse @| [..2] | ::id",
        "/books/*[::pages != 300]::id",
        // the whole row: its NULL cells are left out, as the scan's
        "/books/* | %",
        "/books/* @| sort_by(::price) | %",
    ] {
        let lines = told(q);
        assert!(
            lines.len() == 1 && lines[0].starts_with("info PUSHED (query): pushdown: SELECT "),
            "{q}: {lines:?}"
        );
        same(q);
    }
    for (q, word) in [
        ("/books/* @| sort_by(::title) | ::id", "SORT_NOT_PUSHED"),
        ("/books/* @| top(2; ::title) | ::id", "SORT_NOT_PUSHED"),
        (
            "/books/* | .t(::title) @|{.t} count | .n | %.",
            "FOLD_NOT_PUSHED",
        ),
        ("/books/*::title @| unique @| count", "UNIQUE_NOT_PUSHED"),
    ] {
        let lines = told(q);
        assert!(
            lines.len() == 1 && lines[0].starts_with(&format!("info {word} (query): ")),
            "{q}: {lines:?}"
        );
        same(q);
    }
}

/// A query that ends on row nodes answers their locators, pushed or
/// not (ruling 2026-11-18): a whole plan answers values, and the
/// rows' names are the scan's to write. It was pushed as SELECT *
/// and printed each row as a record. Its condition still goes ahead
/// of the scan, and no refusal is told: nothing was refused.
#[test]
fn a_selection_of_row_nodes_answers_locators_pushed_or_not() {
    let dir = fixture("nodes");
    let (ok, out, err) = run(
        &dir,
        &["--diagnostics=info", "/books/*[::pages > 245]", "lib.db"],
    );
    assert!(ok, "{err}");
    assert_eq!(out, "/books/1\n/books/2\n/books/4");
    assert_eq!(
        err,
        "info PUSHED (prefilter): partial pushdown: WHERE pages > 245 on books; the rest scans \
         the filtered set"
    );
    for q in [
        "/books/*[::pages > 245]",
        "/books/*",
        "/books/*[::author_id = 2 && ::pages != 320]",
        "/authors/* <=>* /books/*[::author_id = $$0::id]",
        "/authors/*[::born > 0] <=> /books/*[::author_id = $$0::id && ::pages > 245]",
    ] {
        let plan = run(&dir, &[q, "lib.db"]).1;
        let scan = run(&dir, &["--no-pushdown", q, "lib.db"]).1;
        assert_eq!(plan, scan, "{q}");
        assert!(plan.lines().all(|l| l.starts_with('/')), "{q}: {plan}");
    }
    // nothing to send ahead and nothing refused: no line at all
    assert_eq!(told(&dir, "/books/*"), Vec::<String>::new());
    // a column or a record after the selection is the plan's, whole
    assert_eq!(
        told(&dir, "/books/*[::pages > 245] | %(::title)"),
        [
            "info PUSHED (query): pushdown: SELECT title FROM books WHERE pages > 245 ORDER BY books.id"
        ]
    );
}

/// The rows themselves, as records: `| %` on a row node is the
/// record of its properties, and that is the plan a bare selection
/// had, SELECT *. A NULL cell is no property of its row: the scan's
/// record leaves it out, and so does the plan's.
#[test]
fn the_whole_row_as_a_record_is_pushed_and_leaves_null_cells_out() {
    let dir = std::env::temp_dir().join(format!("quarb-codes-{}-row", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("lib.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, born INTEGER);
         INSERT INTO authors VALUES (1, 'Homer', NULL), (2, 'Austen', 1775), (3, 'Twain', 1835);
         CREATE TABLE loose (t TEXT, n INTEGER);
         INSERT INTO loose VALUES ('a', 1), (NULL, 2), (NULL, NULL), ('', 4);",
    )
    .unwrap();
    drop(db);
    let (ok, out, err) = run(&dir, &["--diagnostics=info", "/authors/* | %", "lib.db"]);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        "%(id = 1; name = \"Homer\")\n%(id = 2; name = \"Austen\"; born = 1775)\n\
         %(id = 3; name = \"Twain\"; born = 1835)"
    );
    assert_eq!(
        err,
        "info PUSHED (query): pushdown: SELECT * FROM authors ORDER BY authors.id"
    );
    for q in [
        "/authors/* | %",
        "/authors/*[::born = null] | %",
        "/authors/*[::born > 1700] | %",
        "/authors/* @| sort_by(::born; --desc) | %",
        "/authors/* @| reverse @| [..2] | %",
        "/authors/* @| top(1; ::born) | %",
        "/loose/* | %",
        "/loose/*[::n != 2] | %",
    ] {
        let lines: Vec<String> = {
            let (ok, _, err) = run(&dir, &["--diagnostics=info", q, "lib.db"]);
            assert!(ok, "{q}: {err}");
            err.lines().map(str::to_string).collect()
        };
        assert!(
            lines.len() == 1 && lines[0].starts_with("info PUSHED (query): pushdown: SELECT * "),
            "{q}: {lines:?}"
        );
        let plan = run(&dir, &[q, "lib.db"]);
        let scan = run(&dir, &["--no-pushdown", q, "lib.db"]);
        assert!(plan.0 && scan.0, "{q}: {}{}", plan.2, scan.2);
        assert_eq!(plan.1, scan.1, "{q}");
        // every printer reads the same records
        for format in ["--json", "--jsonl", "--csv", "--pretty"] {
            let plan = run(&dir, &[format, q, "lib.db"]);
            let scan = run(&dir, &[format, "--no-pushdown", q, "lib.db"]);
            assert!(plan.0 && scan.0, "{format} {q}: {}{}", plan.2, scan.2);
            assert_eq!(plan.1, scan.1, "{format} {q}");
        }
    }
    // after a column the thread holds a value, and over a join the
    // record is the driver's alone: the engine's, and the same answer
    for q in [
        "/authors/*::name | %",
        "/authors/* | % @| count",
        "/authors/* <=>* /loose/*[::n = $$0::id] | %",
    ] {
        let plan = run(&dir, &[q, "lib.db"]);
        let scan = run(&dir, &["--no-pushdown", q, "lib.db"]);
        assert_eq!(plan.1, scan.1, "{q}");
    }
}

/// A mount's `crosslinks=` names references by their labels, and a
/// label no reference carries is refused. A query pushed whole opens
/// no adapter, so the label was not looked at there and a misspelled
/// one passed in silence; it is held against the catalog before the
/// plan answers, and refused as where the scan answers (0.71).
#[test]
fn a_crosslinks_label_is_checked_where_the_plan_is_pushed_whole() {
    let dir = fixture("crosslabel");
    let (ok, out, err) = run(
        &dir,
        &[
            "--diagnostics=info",
            "/books/* @| count",
            "lib.db?crosslinks=author",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(out, "4");
    assert_eq!(
        err,
        "info PUSHED (query): pushdown: SELECT COUNT(*) FROM books"
    );
    let (ok, out, err) = run(&dir, &["/books/* @| count", "lib.db?crosslinks=writer"]);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(
        err.contains(
            "crosslinks=\"writer\": no reference of this database is labelled \"writer\" (its \
             labels: author)"
        ),
        "{err}"
    );
    // the scan's own refusal, word for word
    let (ok, _, scanned) = run(
        &dir,
        &["/books/1->author::name", "lib.db?crosslinks=writer"],
    );
    assert!(!ok);
    assert_eq!(err, scanned);
}

/// DuckDB's door sends the leading conditions ahead of the scan, as
/// the other doors do (0.71: it opened the file whole), and a regex
/// narrows the fetch by its literals there (`strpos`). Each answer
/// is the scan's.
#[test]
fn the_duckdb_door_sends_leading_conditions_ahead() {
    let dir = std::env::temp_dir().join(format!("quarb-codes-{}-duckpart", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let db = duckdb::Connection::open(dir.join("lib.duckdb")).unwrap();
        db.execute_batch(
            "CREATE TABLE books (id INTEGER PRIMARY KEY, title VARCHAR, pages INTEGER);
             INSERT INTO books VALUES
               (1, 'Iliad', 300), (2, 'Emma', 320), (3, 'Tom Sawyer', 240),
               (4, 'Persuasion', 250), (5, 'Odyssey', NULL), (6, NULL, 700),
               (7, 'tom thumb', 90), (8, 'Tom Jones', 900);",
        )
        .unwrap();
    }
    let lines = |q: &str| -> Vec<String> {
        let (ok, plan, err) = run(&dir, &["--diagnostics=info", q, "lib.duckdb"]);
        assert!(ok, "{q}: {err}");
        let (ok, scan, _) = run(&dir, &["--no-pushdown", q, "lib.duckdb"]);
        assert!(ok, "{q}");
        assert_eq!(plan, scan, "{q}");
        assert!(!plan.is_empty(), "{q}");
        err.lines().map(str::to_string).collect()
    };
    let sent = |q: &str, filter: &str| {
        let lines = lines(q);
        assert!(
            lines.iter().any(|l| l
                == &format!(
                    "info PUSHED (prefilter): partial pushdown: WHERE {filter} on books; the \
                     rest scans the filtered set"
                )),
            "{q}: {lines:?}"
        );
    };
    // a plain condition before a selection the plan cannot take
    sent("/books/*[::pages > 245] @| [2] | ::title", "pages > 245");
    sent(
        "/books/*[::pages != 300 && ::pages < 800] @| [3] | ::id",
        "((pages <> 300 OR pages IS NULL) AND pages < 800)",
    );
    // a regex: the literals it cannot match without
    sent(
        "/books/*[::title == (/^Tom .*s$/)]::id",
        "strpos(title, 'Tom ') > 0 AND strpos(title, 's') > 0",
    );
    sent(
        "/books/*[::pages > 100 && ::title == (/on/)]::title",
        "(pages > 100 AND strpos(title, 'on') > 0)",
    );
    // not narrowed: no literal, and a number column (the database
    // would search its own rendering of the number)
    for q in [
        "/books/*[::title == (/(?i)^tom/)]::id",
        "/books/*[::pages == (/^3/)]::id",
    ] {
        let lines = lines(q);
        assert!(
            !lines.iter().any(|l| l.contains("partial pushdown")),
            "{q}: {lines:?}"
        );
    }
}
