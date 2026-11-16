//! A Markdown note's YAML header at the command line (approved
//! 2026-11-13): a folder of notes answers by what each declares,
//! under a reading prefix as declared identity and on the plain
//! mount as the head's elements.

use std::path::PathBuf;

fn lines(args: &[&str]) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "qua {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Three notes: two with a header, one without.
fn notes() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-notes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("iliad.md"),
        "---\ntitle: The Iliad\nauthor: Homer\ntags: [epic, greek]\nbooks: 24\n\
         date: 2026-03-01\n---\n\n# Book I\n\nSing, goddess, the wrath of Achilles.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("odyssey.md"),
        "---\ntitle: The Odyssey\nauthor: Homer\ntags: epic, travel\nbooks: 24\n\
         date: 2026-03-09\n...\n# Book I\n\nTell me, Muse, of the man of many ways.\n",
    )
    .unwrap();
    std::fs::write(dir.join("plain.md"), "# Plain\n\nNo header at all.\n").unwrap();
    dir
}

#[test]
fn a_folder_of_notes_answers_by_what_each_declares() {
    let dir = notes();
    for reading in ["text", "corpus"] {
        let target = format!("{reading}:{}", dir.display());
        assert_eq!(
            lines(&["/* | %(:name; ::title; ::tags)", &target]),
            [
                r#"%(name = "iliad.md"; title = "The Iliad"; tags = @("epic"; "greek"))"#,
                r#"%(name = "odyssey.md"; title = "The Odyssey"; tags = @("epic"; "travel"))"#,
                r#"%(name = "plain.md"; title = null; tags = null)"#,
            ],
            "{reading}"
        );
        // a tag is a trait of its document
        assert_eq!(
            lines(&["//*<tag:epic> | :name", &target]),
            ["iliad.md", "odyssey.md"],
            "{reading}"
        );
        assert_eq!(
            lines(&["//*<tag:travel> | ::title", &target]),
            ["The Odyssey"],
            "{reading}"
        );
        // a date is on the timeline; a number is a number
        assert_eq!(
            lines(&["/*[::published > 2026-03-05] | ::title", &target]),
            ["The Odyssey"],
            "{reading}"
        );
        assert_eq!(
            lines(&["/*::head:books @| sum", &target]),
            ["48"],
            "{reading}"
        );
        // the header is no part of any text
        assert_eq!(
            lines(&["//section | ::lemma", &target]),
            ["Book I", "Book I", "Plain"],
            "{reading}"
        );
    }
    // the plain mount of one note is its HTML rendering, the header
    // its head (in a plain folder a note stays a leaf, as ever)
    let iliad = dir.join("iliad.md");
    let iliad = iliad.to_str().unwrap();
    assert_eq!(lines(&["//head/title | ::", iliad]), ["The Iliad"]);
    assert_eq!(
        lines(&["//meta[::name = 'tags'] | ::content", iliad]),
        ["epic", "greek"]
    );
    assert_eq!(lines(&["/html/body/* | :name", iliad]), ["h1", "p"]);
    let odyssey = dir.join("odyssey.md");
    assert_eq!(
        lines(&[
            "//meta[::name = 'tags'] | ::content",
            odyssey.to_str().unwrap()
        ]),
        ["epic, travel"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A declared date at reduced precision answers its calendar period
/// (the user, 2026-11-16: "Try not to return null for ::published if
/// you have data. Why not just return the calendar period"): a year,
/// or a year and a month, where it answered null. So does a page's
/// and a LaTeX source's.
#[test]
fn a_year_or_a_month_is_the_period() {
    let dir = std::env::temp_dir().join(format!("quarb-notes-when-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).unwrap();
    // YAML reads a bare year as a number
    write(
        "year.md",
        "---\ntitle: Common Sense\ndate: 1776\n---\n\nText.\n",
    );
    write(
        "month.md",
        "---\ntitle: Common Sense\ndate: 1776-01\nupdated: 1776-02\n---\n\nText.\n",
    );
    write(
        "day.md",
        "---\ntitle: Common Sense\ndate: 1776-01-10\n---\n\nText.\n",
    );
    // wording that is no ISO date, and a number that is no year
    write(
        "prose.md",
        "---\ntitle: Common Sense\ndate: 10 January 1776\n---\n\nText.\n",
    );
    write(
        "number.md",
        "---\ntitle: Common Sense\ndate: 12\n---\n\nText.\n",
    );
    write(
        "page.html",
        "<html><head><title>Common Sense</title>\
         <meta property=\"article:published_time\" content=\"1776-01\"></head>\
         <body><p>Text.</p></body></html>",
    );
    write(
        "paper.tex",
        "\\documentclass{article}\n\\title{Common Sense}\n\\date{1776}\n\
         \\begin{document}\nText.\n\\end{document}\n",
    );
    let at = |file: &str| format!("text:{}", dir.join(file).display());
    let read = |file: &str| lines(&["^ | %(::published; kind = (::published | type))", &at(file)]);
    assert_eq!(read("year.md"), [r#"%(published = 1776; kind = "period")"#]);
    assert_eq!(
        read("month.md"),
        [r#"%(published = 1776-01; kind = "period")"#]
    );
    assert_eq!(
        read("day.md"),
        [r#"%(published = 1776-01-10; kind = "instant")"#]
    );
    assert_eq!(read("prose.md"), [r#"%(published = null; kind = "null")"#]);
    assert_eq!(read("number.md"), [r#"%(published = null; kind = "null")"#]);
    assert_eq!(
        read("page.html"),
        [r#"%(published = 1776-01; kind = "period")"#]
    );
    assert_eq!(
        read("paper.tex"),
        [r#"%(published = 1776; kind = "period")"#]
    );
    // the same reading for the date a note was changed
    assert_eq!(lines(&["^ | ::modified", &at("month.md")]), ["1776-02"]);
    // a period orders against a date as the stretch it is
    assert_eq!(
        lines(&["^ | ::published < 1776-07-04", &at("year.md")]),
        ["true"]
    );
}
