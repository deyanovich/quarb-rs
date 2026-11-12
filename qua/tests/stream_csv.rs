//! The streamed CSV mount (ruling 2026-11-05): `qua QUERY file.csv`
//! does not hold the file — the rows are read by offset as the query
//! reaches them — and answers what the whole-file mount
//! (`--no-stream`) answers.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-stream-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("t.csv"),
        "Name,Age,Fare,Note\nBraund,22,7.25,\"a, b\"\nCumings,38,71.5,\"line one\nline two\"\nHeikkinen,26,8.75,\nFutrelle,35,53.1,x\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

/// Every query answers alike streamed and whole.
#[test]
fn the_streamed_mount_answers_as_the_whole_one() {
    let d = fixture();
    for q in [
        "/row @| count",
        "/row[::Age > 30] @| count",
        "/row @| [..2] | ::Name",
        "/row @| [2]",
        ":::n-rows",
        ":::columns",
        "/row[2]::Note",
        "/row[3]::Note",
        "/row @| # | [..2]",
        "/row[::Name = \"Futrelle\"]::Fare",
        "/row @| first(2) | ::Age",
        "/row @| last | ::Name",
        "/row[-1]::Name",
        "/row @| sort_by(::Age) @| [..2] | ::Name",
        "/row/* @| # | mean(--by=:Note)",
        "/row | %",
    ] {
        let (a, ea, ga) = qua(&d, &[q, "t.csv"]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, "t.csv"]);
        assert_eq!(ga, gb, "{q}: {ea} / {eb}");
        assert_eq!(a, b, "{q}");
    }
    let (out, err, good) = qua(&d, &["/row @| [..2] | ::Name", "t.csv"]);
    assert!(good, "{err}");
    assert_eq!(out, "Braund\nCumings");
    let (out, _, _) = qua(&d, &[":::n-rows", "t.csv"]);
    assert_eq!(out, "4");
    // an error is an answer too: the wrong delimiter refuses with
    // the same lines streamed and whole (lang's finding 2026-11-05)
    std::fs::write(d.join("semi.csv"), "Name;Val\nA;1\nB;2\n").unwrap();
    let (_, ea, ga) = qua(&d, &["/row @| count", "semi.csv"]);
    let (_, eb, gb) = qua(&d, &["--no-stream", "/row @| count", "semi.csv"]);
    assert!(!ga && !gb);
    assert_eq!(ea, eb);
    assert!(
        ea.contains("parsing CSV") && ea.contains("?delimiter=semicolon"),
        "{ea}"
    );
}

/// A JSONL file streams the same way where it is large or `--stream`
/// asks: each line its own arbor when reached, the answers the whole
/// reading's.
#[test]
fn the_streamed_jsonl_answers_as_the_whole_one() {
    let d = fixture();
    std::fs::write(
        d.join("t.jsonl"),
        "{\"name\":\"Braund\",\"age\":22,\"tags\":[\"a\",\"b\"]}\n{\"name\":\"Cumings\",\"age\":38,\"home\":{\"city\":\"X\"}}\n{\"name\":\"Heikkinen\",\"age\":26}\n",
    )
    .unwrap();
    for q in [
        "/* @| count",
        "/*[::age > 30] @| count",
        "/* @| [..2] | ::name",
        "/*[2]/home/city::",
        "/*[1]::tags",
        "/* | % | :name",
        "/* @| # | count",
        "/2/home | %",
        // lines read out of the file's order
        "/* @| sort_by(::age) @| [..2] | ::name",
        "/* @| reverse | ::name",
        "/* | .a(::age > 25) @|{.a} max_by(::age) | ::name",
    ] {
        let (a, ea, ga) = qua(&d, &["--stream", q, "t.jsonl"]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, "t.jsonl"]);
        assert_eq!(ga, gb, "{q}: {ea} / {eb}");
        assert_eq!(a, b, "{q}");
    }
    let (out, err, good) = qua(&d, &["--stream", "/* @| [..2] | ::name", "t.jsonl"]);
    assert!(good, "{err}");
    assert_eq!(out, "Braund\nCumings");
    // the line count is the stream's own metadata
    let (out, _, _) = qua(&d, &["--stream", ":::n-lines", "t.jsonl"]);
    assert_eq!(out, "3");
}

/// A file of `n` rows over seven routes, the rows of a route
/// scattered among the others: larger than the stream's window at
/// its least, so a read out of order leaves the bytes held.
fn requests(dir: &Path, n: usize) {
    let routes = [
        "/", "/search", "/item", "/cart", "/login", "/help", "/about",
    ];
    let mut text = String::from("ts,route,ms,bytes\n");
    for i in 0..n {
        // a spread with no period the routes share
        let route = routes[(i * 5 + i / 3) % routes.len()];
        let ms = (i * 7919) % 1009;
        let bytes = 100 + (i * 104_729) % 9973;
        text.push_str(&format!(
            "2026-07-03T08:{:02}:{:02}Z,{route},{ms},{bytes}\n",
            (i / 60) % 60,
            i % 60
        ));
    }
    std::fs::write(dir.join("requests.csv"), text).unwrap();
}

/// The rows read out of the file's order answer as the whole reading
/// does: a fold that reads its members group by group (a seek and a
/// read per row before the stream read through a window: 875
/// thousand of each over a million rows), an order by one column and
/// a cell of another, the ends of the file one after the other.
#[test]
fn rows_read_out_of_order_answer_as_the_whole_reading() {
    let d = fixture();
    requests(&d, 20_000);
    for q in [
        "/row | .r(::route) @|{.r} sum(--of=::bytes) | %($.r; b = $_)",
        "/row | .r(::route) @|{.r} max_by(::ms) | %($.r; ::ts; ::ms)",
        "/row @| sort_by(::ms) @| [..5] | %(::ts; ::route)",
        "/row @| sort_by(-::bytes) | ::ts @| last",
        "/row @| top(3; ::ms) | %(::route; ::ms)",
        "/row @| reverse @| [..3] | ::ts",
        "@(/row[-1]::ts; /row[1]::ts; /row[10000]::ts; /row[2]::ts; /row[-2]::ts)",
        "/row[::route = \"/cart\"] @| last | %(::ts; ::bytes)",
    ] {
        let (a, ea, ga) = qua(&d, &[q, "requests.csv"]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, "requests.csv"]);
        assert!(ga && gb, "{q}: {ea} / {eb}");
        assert_eq!(a, b, "{q}");
        assert!(!a.is_empty(), "{q}");
    }
}

/// A row is found by its offset in the data, whatever stands before
/// the data in the file. Counted from the file's start, the offset of
/// a last row as long as what was skipped (a BOM's three bytes, a
/// `sep=` line) met the reader's position after the open's scan: no
/// seek was made, and the row read as null when it was the first row
/// read (0.53.0 to 0.61.0).
#[test]
fn the_last_row_reads_after_a_bom_or_a_sep_line() {
    let d = fixture();
    std::fs::write(d.join("bom.csv"), b"\xEF\xBB\xBFa\n1\n2\n12\n").unwrap();
    std::fs::write(d.join("sep.csv"), "sep=;\na;b\n1;2\n3;4\n5;678\n").unwrap();
    for (q, file, answer) in [
        ("/row[3]::a", "bom.csv", "12"),
        ("/row[-1]::a", "bom.csv", "12"),
        ("/row @| last | ::a", "bom.csv", "12"),
        ("/row[-1]::b", "sep.csv", "678"),
        (
            "/row[3] | %(::a; ::b)",
            "sep.csv",
            r#"%(a = "5"; b = "678")"#,
        ),
    ] {
        let (out, err, good) = qua(&d, &[q, file]);
        assert!(good, "{q}: {err}");
        assert_eq!(out, answer, "{q} over {file}");
        let (whole, _, _) = qua(&d, &["--no-stream", q, file]);
        assert_eq!(out, whole, "{q} over {file}");
    }
}
