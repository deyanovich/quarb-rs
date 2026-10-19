//! Time expressions and the timeline: `timex` finds them by the
//! locale's lists, `time_anchor` values one against a point,
//! `@| timeline` carries the point from capsa to capsa, and a
//! period is a value that compares as an interval.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-timeline-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("story.txt"),
        "On 6 October 1899 he arrived.\n\nThe next day he wrote to her.\n\n\
         Two days later the answer came.\n\nThat evening he left.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("diary.md"),
        "# 3 May 1897\n\nLeft Munich at 8:35 P.M. We reached Vienna the next morning.\n\n\
         Two days later we came to Bistritz.\n\n# 30 December 1897\n\n\
         Yesterday the snow fell. On 2 January we left.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("log.json"),
        r#"{"rows": [{"id": 1, "at": "2025-10-06T14:30:00Z"}, {"id": 2, "at": "2025-10-31"},
                     {"id": 3, "at": "2025-11-02"}, {"id": 4, "at": "2024"}, {"id": 5, "at": "soon"}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end()
            .to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, err) = qua(dir, args);
    assert!(err.is_empty(), "{args:?}: {err}");
    out
}

#[test]
fn an_expression_and_its_anchor() {
    let dir = fixture("anchor");
    assert_eq!(
        ok(&dir, &["= 'On 8 April he left.' | timex(en)"]),
        "%(text = \"8 April\"; at = 3; kind = \"date\"; value = \"XXXX-04-08\"; grain = \"day\")"
    );
    let anchored = |text: &str, at: &str| {
        ok(
            &dir,
            &[&format!("= '{text}' | timex(en) | time_anchor({at})")],
        )
    };
    assert_eq!(anchored("two months later", "1899-10-06"), "1899-12-06");
    assert_eq!(anchored("the following Monday", "1899-10-06"), "1899-10-09");
    // an offset finer than the anchor's grain has no result
    assert_eq!(anchored("the next day", "1812"), "");
    // a duration and a vague expression have none either
    assert_eq!(anchored("two weeks", "1899-10-06"), "");
    assert_eq!(anchored("that evening", "1899-10-06"), "");
    // the value text alone reads too; fields left out come from the anchor
    assert_eq!(
        ok(&dir, &["= 'XXXX-04-08' | time_anchor(1846-03-20)"]),
        "1846-04-08"
    );
    assert_eq!(
        ok(&dir, &["= '+P1M' | time_anchor(1899-01-31)"]),
        "1899-02-28"
    );
    // a locale without lists is refused, naming those carried
    let (_, err) = qua(&dir, &["= 'x' | timex(pl)"]);
    assert!(err.contains("carried: en, zh, es"), "{err}");
    // the anchor is an expression of the capsa: a diary's entry date
    assert_eq!(
        ok(
            &dir,
            &[
                "//section | .d(::lemma | tp('%d %B %Y')) | /paragraph | :: | timex(en) | time_anchor($.d)",
                "text:diary.md"
            ]
        ),
        "1897-05-03T20:35\n1897-05-04\n1897-05-05\n1897-12-29\n1897-01-02"
    );
}

#[test]
fn the_point_is_carried_from_capsa_to_capsa() {
    let dir = fixture("timeline");
    assert_eq!(
        ok(
            &dir,
            &[
                "--table",
                "/paragraph | :: | timex(en) @| timeline($_) | %(:text; :kind; :when; :from)",
                "text:story.txt"
            ]
        ),
        "text            kind      when        from\n\
         --------------  --------  ----------  ----\n\
         6 October 1899  date      1899-10-06\n\
         The next day    relative  1899-10-07  1\n\
         Two days later  relative  1899-10-09  2\n\
         That evening    vague"
    );
    // an anchor cuts the context into runs: each entry starts at its
    // date; a day offset after a time of day is a day
    let diary = |tail: &str| {
        ok(
            &dir,
            &[
                "--csv",
                &format!(
                    "//section | .d(::lemma | tp('%d %B %Y')) | /paragraph | :: | timex(en) \
                     @| timeline($_; $.d{tail}) | %(:text; :when; :from)"
                ),
                "text:diary.md",
            ],
        )
    };
    assert_eq!(
        diary(""),
        "text,when,from\n\
         8:35 P.M,1897-05-03T20:35,0\n\
         the next morning,1897-05-04,1\n\
         Two days later,1897-05-06,2\n\
         Yesterday,1897-12-29,0\n\
         2 January,1897-01-02,4"
    );
    // forward: the point never moves back (yesterday is reported and
    // left behind), and a date with its year left out is the first
    // such date at or after the point
    assert_eq!(
        diary("; forward"),
        "text,when,from\n\
         8:35 P.M,1897-05-03T20:35,0\n\
         the next morning,1897-05-04,1\n\
         Two days later,1897-05-06,2\n\
         Yesterday,1897-12-29,0\n\
         2 January,1898-01-02,0"
    );
    // with no point a relative stays unvalued
    assert_eq!(
        ok(
            &dir,
            &[
                "--csv",
                "= 'the next day; on 3 May 1897; a week later' | timex(en) @| timeline($_) | %(:value; :when; :from)"
            ]
        ),
        "value,when,from\n+P1D,,\n1897-05-03,1897-05-03,\n+P1W,1897-05-10,2"
    );
    let (_, err) = qua(&dir, &["= 1 @| timeline($_; 1; 2; 3)"]);
    assert!(err.contains("timeline takes the time expression"), "{err}");
}

#[test]
fn a_period_compares_as_an_interval() {
    let dir = fixture("period");
    let ids = |pred: &str| ok(&dir, &[&format!("/rows/*[{pred}]::id"), "log.json"]);
    // a period reads as its first instant: a month equals its first
    // day and no other; containment is the containment comparison
    assert_eq!(ids("(::at | period) = 2025-10-31"), "2");
    assert_eq!(ids("(::at | period) = 2025-10"), "");
    assert_eq!(ids("(::at | period) <<= 2025-10"), "1\n2");
    assert_eq!(ids("::at <<= ('2025-10' | period)"), "1\n2");
    assert_eq!(ids("2025-10 >>= ::at"), "1\n2");
    // periods are ordered by their first instants
    assert_eq!(ids("(::at | period) < 2025-11"), "1\n2\n4");
    assert_eq!(ids("(::at | period) >= ('2025-10-31' | period)"), "2\n3");
    // constructing, coarsening, and the grain
    assert_eq!(ok(&dir, &["= 2025-10-06 | period(month)"]), "2025-10");
    assert_eq!(ok(&dir, &["= 2025-10-06 | period | grain"]), "day");
    assert_eq!(ok(&dir, &["= 1812 | period"]), "1812");
    // a grain finer than the value's own is null
    assert_eq!(
        ok(
            &dir,
            &[
                "/rows/*[::id <= 2] | ::at | period(hour) | %(p = $_) @| json",
                "log.json"
            ]
        ),
        "[{\"p\": \"2025-10-06T14\"}, {\"p\": null}]"
    );
    // a period sorts by its start, takes no part in arithmetic, reads
    // as its first instant, and is ISO text in the interchange forms
    assert_eq!(
        ok(
            &dir,
            &["/rows/*[::id < 5] | ::at | period @| sort", "log.json"]
        ),
        "2024\n2025-10-06T14:30:00\n2025-10-31\n2025-11-02"
    );
    assert_eq!(ok(&dir, &["= (2025-10 | period) + 1"]), "");
    assert_eq!(ok(&dir, &["= (2025-10 | period) | datetime"]), "2025-10-01");
    assert_eq!(
        ok(
            &dir,
            &[
                "/rows/*[::id = 2] | %(p = (::at | period(month))) @| json",
                "log.json"
            ]
        ),
        "[{\"p\": \"2025-10\"}]"
    );
    let (_, err) = qua(&dir, &["= 2025-10-06 | period(decade)"]);
    assert!(err.contains("is not a grain"), "{err}");
}
