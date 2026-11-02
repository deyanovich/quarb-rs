//! --plan and --budget price the inputs from their catalogs before
//! anything is opened: a tarball of books by its headers, a
//! directory by its listing.

use std::path::{Path, PathBuf};

fn scratch() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("qua-preflight-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shelf")).unwrap();
    for (name, text) in [
        ("emma.txt", "Emma Woodhouse, handsome, clever, and rich"),
        ("iliad.txt", "Sing, O goddess, the anger of Achilles"),
        ("odyssey.txt", "Tell me, O Muse, of that ingenious hero"),
    ] {
        std::fs::write(dir.join("shelf").join(name), text).unwrap();
    }
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(dir.join("books.tar.gz")).unwrap(),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(gz);
    ar.append_dir_all("shelf", dir.join("shelf")).unwrap();
    ar.into_inner().unwrap().finish().unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

#[test]
fn a_shelf_is_priced_from_the_tarballs_headers() {
    let dir = scratch();
    let (out, err, ok) = qua(
        &dir,
        &[
            "--plan",
            "/books/*[::name = \"emma.txt\"]",
            "books=books.tar.gz",
        ],
    );
    assert!(ok, "{err}");
    // Three members and the folder entry: four entries.
    assert!(out.contains("nodes-min=0\n!int\nnodes-max=4"), "{out}");
    assert!(out.contains("confidence=bound"), "{out}");
    let (out, err, ok) = qua(&dir, &["--plan", "//*", "text:books.tar.gz"]);
    assert!(ok, "{err}");
    // Every member is a node of `//*`, with what the reading finds
    // beneath: a lower bound.
    assert!(
        out.contains(
            "nodes-min=4
"
        ),
        "{out}"
    );
    assert!(!out.contains("nodes-max="), "{out}");
    assert!(out.contains("latency=local"), "{out}");
    assert!(out.contains("confidence=bound"), "{out}");
}

#[test]
fn a_directory_is_priced_from_its_listing() {
    let dir = scratch();
    let (out, err, ok) = qua(&dir, &["--plan", "/*", "corpus:shelf"]);
    assert!(ok, "{err}");
    assert!(out.contains("nodes-max=3"), "{out}");
    assert!(out.contains("bytes-max=119"), "{out}");
}

#[test]
fn a_budget_refuses_before_opening_and_admits_within() {
    let dir = scratch();
    // The listing stops at the first file past the budget: the
    // refusal names what it reached, not the whole.
    let (_, err, ok) = qua(&dir, &["--budget", "bytes=50", "/*", "corpus:shelf"]);
    assert!(!ok);
    assert!(err.contains("reaches at least"), "{err}");
    assert!(err.contains("bytes; the budget allows 50"), "{err}");
    let (_, err, ok) = qua(&dir, &["--budget", "nodes=1", "/*", "corpus:shelf"]);
    assert!(!ok);
    assert!(
        err.contains("reaches at least 2 nodes; the budget allows 1"),
        "{err}"
    );
    let (out, err, ok) = qua(&dir, &["--budget", "bytes=500", "/*", "corpus:shelf"]);
    assert!(ok, "{err}");
    assert!(out.contains("emma.txt"), "{out}");
    // A plain file's node count is unknown before the reading: a
    // rows budget passes the catalog's pricing, and the opened
    // reading prices it exactly — under strict too, which refuses
    // only what stays unknown or unbounded once the inputs are open.
    let (_, err, ok) = qua(&dir, &["--budget", "nodes=1", "/*", "shelf/emma.txt"]);
    assert!(ok, "{err}");
    let (_, err, ok) = qua(
        &dir,
        &["--budget", "nodes=1,strict", "/*", "shelf/emma.txt"],
    );
    assert!(ok, "{err}");
}

#[test]
fn a_budget_takes_units_and_two_inputs_are_priced_together() {
    let dir = scratch();
    let (_, err, ok) = qua(&dir, &["--budget", "bytes=1MB", "/*", "corpus:shelf"]);
    assert!(ok, "{err}");
    let (_, err, ok) = qua(&dir, &["--budget", "bytes=1x", "/*", "corpus:shelf"]);
    assert!(!ok);
    assert!(err.contains("--budget bytes"), "{err}");
    let (out, err, ok) = qua(
        &dir,
        &[
            "--plan",
            "//*",
            "text:shelf/emma.txt",
            "text:shelf/iliad.txt",
        ],
    );
    assert!(ok, "{err}");
    assert!(out.contains("(/plan/estimate)"), "{out}");
    assert!(out.contains("bytes-min=80"), "{out}");
}

/// A database file is its store's to price: the pre-flight leaves
/// it to the open, and the planned or budgeted run goes through the
/// engine (never the SQL pushdown, which would answer outright).
#[test]
fn a_database_is_priced_by_its_store_once_open() {
    let dir = scratch();
    let db = dir.join("loans.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    // Two indexes, as a large table always has: the statistics
    // then come one row per index, none with a null idx.
    conn.execute_batch(
        "CREATE TABLE loans (id INTEGER PRIMARY KEY, book TEXT, reader INTEGER);
         CREATE INDEX loans_book ON loans (book);
         CREATE INDEX loans_reader ON loans (reader);",
    )
    .unwrap();
    for i in 0..500 {
        conn.execute(
            "INSERT INTO loans (book) VALUES (?1)",
            [if i % 5 == 0 { "Emma" } else { "Iliad" }],
        )
        .unwrap();
    }
    // Before ANALYZE the store has no statistics: the plan carries
    // no estimate and a strict budget refuses as unknown.
    let (out, err, ok) = qua(&dir, &["--plan", "/loans/*", "loans.db"]);
    assert!(ok, "{err}");
    assert!(out.contains("[/plan/@steps]"), "{out}");
    assert!(!out.contains("(/plan/estimate)"), "{out}");
    let (_, err, ok) = qua(
        &dir,
        &[
            "--budget",
            "nodes=10,strict",
            "/loans/* @| count",
            "loans.db",
        ],
    );
    assert!(!ok);
    assert!(err.contains("unknown"), "{err}");
    conn.execute_batch("ANALYZE").unwrap();
    drop(conn);
    let (out, err, ok) = qua(&dir, &["--plan", "/loans/*", "loans.db"]);
    assert!(ok, "{err}");
    assert!(out.contains("nodes-max=500"), "{out}");
    let (_, err, ok) = qua(
        &dir,
        &["--budget", "nodes=10", "/loans/* @| count", "loans.db"],
    );
    assert!(!ok);
    assert!(
        err.contains("estimates 500 nodes; the budget allows 10"),
        "{err}"
    );
    let (out, err, ok) = qua(
        &dir,
        &["--budget", "nodes=1000", "/loans/* @| count", "loans.db"],
    );
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "500");
}

#[test]
fn a_narrower_path_lowers_the_estimate() {
    let dir = scratch();
    std::fs::create_dir_all(dir.join("shelf/verse")).unwrap();
    std::fs::rename(
        dir.join("shelf/iliad.txt"),
        dir.join("shelf/verse/iliad.txt"),
    )
    .unwrap();
    // The whole tree, then one folder, then one file.
    let (out, err, ok) = qua(&dir, &["--plan", "//*", "corpus:shelf"]);
    assert!(ok, "{err}");
    assert!(out.contains("bytes-max=119"), "{out}");
    let (out, err, ok) = qua(&dir, &["--plan", "/verse//*", "corpus:shelf"]);
    assert!(ok, "{err}");
    assert!(
        out.contains("(/plan/estimate)\n!int\nnodes-min=1\n!int\nbytes-min=38\n!int\nbytes-max=38"),
        "{out}"
    );
    let (out, err, ok) = qua(
        &dir,
        &["--plan", "/verse/iliad.txt//paragraph", "corpus:shelf"],
    );
    assert!(ok, "{err}");
    assert!(
        out.contains("(/plan/estimate)\n!int\nnodes-min=0\n!int\nbytes-min=38\n!int\nbytes-max=38"),
        "{out}"
    );
    let (_, err, ok) = qua(
        &dir,
        &["--budget", "bytes=100", "/verse//*", "corpus:shelf"],
    );
    assert!(ok, "{err}");
    let (_, err, ok) = qua(&dir, &["--budget", "bytes=100", "//*", "corpus:shelf"]);
    assert!(!ok);
    assert!(err.contains("119 bytes"), "{err}");
}

#[test]
fn a_listing_prices_a_size_filter_and_statistics_price_a_key() {
    let dir = scratch();
    // A size is a quantity, so the filter says its unit (spec:
    // Relations: a bare number never compares with a dimensioned
    // value, and `::size > 40` prices to no node at all).
    let (out, err, ok) = qua(&dir, &["--plan", "//*<file>[::size > 40]", "corpus:shelf"]);
    assert!(ok, "{err}");
    assert!(out.contains("nodes-max=0\n"), "{out}");
    let (out, err, ok) = qua(
        &dir,
        &["--plan", "//*<file>[::size > 40_B]", "corpus:shelf"],
    );
    assert!(ok, "{err}");
    assert!(out.contains("trait=file"), "{out}");
    // Priced by the listing, still the engine's to run.
    assert!(out.contains("conjunct=::size > 40_B (unknown)\n"), "{out}");
    assert!(
        out.contains("nodes-min=0\n!int\nnodes-max=1\n!int\nbytes-min=42\n!int\nbytes-max=42"),
        "{out}"
    );
    // A store's statistics: one value of an indexed column selects
    // the index's rows per key.
    let db = dir.join("loans.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE loans (id INTEGER PRIMARY KEY, book TEXT, reader INTEGER);
         CREATE INDEX loans_book ON loans (book);
         CREATE INDEX loans_reader ON loans (reader);",
    )
    .unwrap();
    for i in 0..500 {
        conn.execute(
            "INSERT INTO loans (book, reader) VALUES (?1, ?2)",
            (if i % 5 == 0 { "Emma" } else { "Iliad" }, i % 50),
        )
        .unwrap();
    }
    conn.execute_batch("ANALYZE").unwrap();
    drop(conn);
    let (out, err, ok) = qua(&dir, &["--plan", "/loans/*[::reader = 7]", "loans.db"]);
    assert!(ok, "{err}");
    assert!(out.contains("conjunct=::reader = 7 (residual)\n"), "{out}");
    assert!(out.contains("nodes-min=0\n!int\nnodes-max=10\n"), "{out}");
    let (out, err, ok) = qua(&dir, &["--plan", "/loans/*[::book = \"Emma\"]", "loans.db"]);
    assert!(ok, "{err}");
    assert!(out.contains("(pushed)"), "{out}");
    assert!(out.contains("nodes-min=0\n!int\nnodes-max=250\n"), "{out}");
    let (out, err, ok) = qua(&dir, &["--plan", "/loans/7", "loans.db"]);
    assert!(ok, "{err}");
    assert!(out.contains("nodes-min=0\n!int\nnodes-max=1\n"), "{out}");
}

#[test]
fn a_strict_budget_names_the_bound_the_walk_reached() {
    // A text file's catalog says nothing of its paragraphs, so the
    // deep count is unbounded before opening: the strict budget used
    // to refuse there with "at least 0", where the lenient one let
    // the opened adapter walk to the cap and named 1001. Both name
    // the walked bound now.
    let dir = scratch();
    std::fs::write(
        dir.join("prose.txt"),
        "One.\n\nTwo.\n\nThree.\n\nFour.\n\nFive.\n",
    )
    .unwrap();
    let (_, lenient, ok) = qua(
        &dir,
        &[
            "--budget",
            "nodes=2",
            "//paragraph @| count",
            "text:prose.txt",
        ],
    );
    assert!(!ok);
    assert!(
        lenient.contains("reaches at least 3 nodes; the budget allows 2"),
        "{lenient}"
    );
    let (_, strict, ok) = qua(
        &dir,
        &[
            "--budget",
            "nodes=2,strict",
            "//paragraph @| count",
            "text:prose.txt",
        ],
    );
    assert!(!ok);
    assert_eq!(strict, lenient);
    // Within the budget, the opened reading prices the paragraphs
    // exactly, so strict runs.
    let (out, err, ok) = qua(
        &dir,
        &[
            "--budget",
            "nodes=100,strict",
            "//paragraph @| count",
            "text:prose.txt",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "5");
}

#[test]
fn a_folder_of_documents_is_unbounded_below_its_leaves() {
    // The folder's tree prices its files; what a document holds is
    // read only when the walk crosses into it, so a step into the
    // leaves has no upper bound before the reading: a strict budget
    // refuses it as unbounded, a lenient one lets the walk count.
    let dir = scratch();
    let (out, err, ok) = qua(
        &dir,
        &[
            "--budget",
            "nodes=1000",
            "//paragraph @| count",
            "text:shelf",
        ],
    );
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "3");
    let (_, err, ok) = qua(
        &dir,
        &[
            "--budget",
            "nodes=1000,strict",
            "//paragraph @| count",
            "text:shelf",
        ],
    );
    assert!(!ok);
    assert!(
        err.contains("the plan's nodes are unbounded (at least 0) and the budget is strict"),
        "{err}"
    );
    // The files themselves are the folder's to count: a child step
    // from the root stays exact under strict.
    let (out, err, ok) = qua(
        &dir,
        &["--budget", "nodes=10,strict", "/* @| count", "text:shelf"],
    );
    assert!(ok, "{err}");
    assert_eq!(out.trim(), "3");
}

#[test]
fn a_parse_error_prints_one_line_with_or_without_a_target() {
    // The same refusal, whether the query runs as a calculator or
    // over a target: one line, no context chain.
    let dir = scratch();
    let (_, head, ok) = qua(&dir, &["= null | mi(22; 355; 690; 71275)"]);
    assert!(!ok);
    assert!(
        head.starts_with("Error: parse error: 'mi' takes 3 operands at most, 4 given"),
        "{head}"
    );
    assert!(!head.contains("Caused by"), "{head}");
    let (_, target, ok) = qua(&dir, &["/* | upper(::name)", "shelf/emma.txt"]);
    assert!(!ok);
    assert!(
        target.starts_with("Error: parse error: 'upper' takes no operand, 1 given"),
        "{target}"
    );
    assert!(!target.contains("Caused by"), "{target}");
}
