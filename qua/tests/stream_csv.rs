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

/// A JSONL file streams the same way: each line its own arbor when
/// reached, the answers the whole reading's.
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
    ] {
        let (a, ea, ga) = qua(&d, &[q, "t.jsonl"]);
        let (b, eb, gb) = qua(&d, &["--no-stream", q, "t.jsonl"]);
        assert_eq!(ga, gb, "{q}: {ea} / {eb}");
        assert_eq!(a, b, "{q}");
    }
    let (out, err, good) = qua(&d, &["/* @| [..2] | ::name", "t.jsonl"]);
    assert!(good, "{err}");
    assert_eq!(out, "Braund\nCumings");
    // the line count is the stream's own metadata
    let (out, _, _) = qua(&d, &[":::n-lines", "t.jsonl"]);
    assert_eq!(out, "3");
}
