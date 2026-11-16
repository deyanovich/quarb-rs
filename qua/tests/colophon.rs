//! A document's colophon is what it declares, not its text (atrep
//! 0.3.11).
//!
//! The importers read the publication statement of a TEI header
//! (`publisher`, `pubPlace`, `date`) and of a DocBook `info` into an
//! embedded block of the dialektos `kolophon`, the document's last:
//! the fields `officina`, `locus` and `datum`, the bibliography's
//! own names. Here the block is the head's: the record `::head`
//! carries each field under its name, the curated `::published`
//! reads a `datum` that is a full ISO date, by the value the date
//! carries (atrep 0.4: its chronos) or by its text, a year or a year
//! and month as the calendar period, an interval as the list of its
//! two ends, and the text has no node for it, so no count under any
//! reading moves. (Read as any other
//! embedded block it was a `verbatim` at the document's end, its
//! field names and values counted as words.)

use std::path::PathBuf;

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

fn qua(query: &str, target: &str) -> String {
    let (success, out, err) = run(&[query, target]);
    assert!(success, "{query} {target}: {err}");
    out
}

const TITLE: &str = "The History of the Decline and Fall of the Roman Empire";

/// A TEI edition whose header's publication statement is `stmt`.
fn tei(stmt: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n<TEI xmlns=\"http://www.tei-c.org/ns/1.0\"><teiHeader>\
         <fileDesc><titleStmt><title>{TITLE}</title><author>Edward Gibbon</author></titleStmt>\
         <publicationStmt>{stmt}</publicationStmt><sourceDesc><p>x</p></sourceDesc>\
         </fileDesc></teiHeader><text><body><div><head>Chapter I</head>\
         <p>In the second century of the Christian Aera, the empire of Rome comprehended \
         the fairest part of the earth.</p></div></body></text></TEI>"
    )
}

fn editions(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-colophon-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).unwrap();
    write(
        "printed.tei.xml",
        &tei(
            "<publisher>Strahan and Cadell</publisher><pubPlace>London</pubPlace>\
              <date when=\"1776-02-17\">17 February 1776</date>",
        ),
    );
    write(
        "iso.tei.xml",
        &tei(
            "<publisher>Strahan and Cadell</publisher><pubPlace>London</pubPlace>\
              <date>1776-02-17</date>",
        ),
    );
    write("prose.tei.xml", &tei("<p>Published in London.</p>"));
    // dates whose value is no single day
    write(
        "span.tei.xml",
        &tei("<publisher>Tolstoy Digital</publisher><date from=\"2015\" to=\"2022\"/>"),
    );
    write(
        "open.tei.xml",
        &tei("<publisher>Tolstoy Digital</publisher><date from=\"2015\"/>"),
    );
    write(
        "days.tei.xml",
        &tei("<publisher>Tolstoy Digital</publisher>\
             <date from=\"2015-03-01\" to=\"2022-06-30\">from spring 2015</date>"),
    );
    write(
        "year.tei.xml",
        &tei("<publisher>Strahan and Cadell</publisher><date>1776</date>"),
    );
    write(
        "month.tei.xml",
        &tei("<publisher>Strahan and Cadell</publisher><date when=\"1776-02\"/>"),
    );
    write(
        "smith.docbook.xml",
        "<?xml version=\"1.0\"?>\n<article xmlns=\"http://docbook.org/ns/docbook\" \
         version=\"5.0\"><info><title>The Wealth of Nations</title><publisher>\
         <publishername>Strahan and Cadell</publishername><address>London</address>\
         </publisher><pubdate>1776-03-09</pubdate></info><section>\
         <title>Of the Division of Labour</title><para>The greatest improvement in the \
         productive powers of labour seems to have been the effects of the division of \
         labour.</para></section></article>",
    );
    // a native document writes its colophon itself, here under an
    // alias of the first field's name
    write(
        "gibbon.atd",
        "@@@!litogramma\n\nIn the second century of the Christian Aera.\n\n\
         @@@!(kolophon)\n@: publisher\nStrahan and Cadell\n:@\n@: locus\nLondon\n:@\n\
         @: datum\n1776-02-17\n:@\n!@@@\n",
    );
    dir
}

#[test]
fn the_colophon_is_the_heads() {
    let dir = editions("head");
    let at = |file: &str, reading: &str| format!("{reading}:{}", dir.join(file).display());
    for reading in ["text", "corpus", "lit"] {
        // each field under its own name, after the title statement
        assert_eq!(
            qua("^ | ::head", &at("printed.tei.xml", reading)),
            format!(
                "%(title = \"{TITLE}\"; author = \"Edward Gibbon\"; \
                 officina = \"Strahan and Cadell\"; locus = \"London\"; \
                 datum = \"17 February 1776\")"
            ),
            "{reading}"
        );
        assert_eq!(
            qua(
                "^ | %(::head:officina; ::head:locus)",
                &at("iso.tei.xml", reading)
            ),
            r#"%(officina = "Strahan and Cadell"; locus = "London")"#,
            "{reading}"
        );
        // a date that is an ISO date is the curated `published`
        assert_eq!(
            qua("^ | ::published", &at("iso.tei.xml", reading)),
            "1776-02-17",
            "{reading}"
        );
        // and so is the value a date carries beside its wording (TEI's
        // `when`, the chronos atrep 0.4 opens the field with): the
        // head's text stays the wording, the value is no part of it
        // (it read "1776-02-1717 February 1776", and no instant)
        assert_eq!(
            qua(
                "^ | %(::head:datum; ::published)",
                &at("printed.tei.xml", reading)
            ),
            r#"%(datum = "17 February 1776"; published = 1776-02-17)"#,
            "{reading}"
        );
        // an interval is the list of its two ends (the user,
        // 2026-11-16), each at the precision it is written at: a year
        // the period, a full date the instant, an open end null in
        // its place. The head's text is the wording.
        assert_eq!(
            qua(
                "^ | %(::head:datum; ::published)",
                &at("span.tei.xml", reading)
            ),
            "%(datum = \"2015\u{2013}2022\"; published = @(2015; 2022))",
            "{reading}"
        );
        assert_eq!(
            qua("^ | ::published | ... | type", &at("span.tei.xml", reading)),
            "period\nperiod",
            "{reading}"
        );
        assert_eq!(
            qua("^ | ::published", &at("open.tei.xml", reading)),
            "@(2015; null)",
            "{reading}"
        );
        assert_eq!(
            qua(
                "^ | %(::head:datum; ::published)",
                &at("days.tei.xml", reading)
            ),
            r#"%(datum = "from spring 2015"; published = @(2015-03-01; 2022-06-30))"#,
            "{reading}"
        );
        // a year and month, and a year alone, are no instant: each
        // answers its calendar period, which prints as it is written
        // (the user, 2026-11-16: "Try not to return null for
        // ::published if you have data"; they answered null)
        assert_eq!(
            qua(
                "^ | %(::head:datum; ::published)",
                &at("month.tei.xml", reading)
            ),
            r#"%(datum = "1776-02"; published = 1776-02)"#,
            "{reading}"
        );
        assert_eq!(
            qua(
                "^ | %(::published; kind = (::published | type))",
                &at("year.tei.xml", reading)
            ),
            r#"%(published = 1776; kind = "period")"#,
            "{reading}"
        );
        // a period orders against a date as the stretch it is
        assert_eq!(
            qua("^ | ::published < 1800-01-01", &at("year.tei.xml", reading)),
            "true",
            "{reading}"
        );
        // a statement in prose declares nothing
        assert_eq!(
            qua("^ | ::head", &at("prose.tei.xml", reading)),
            format!("%(title = \"{TITLE}\"; author = \"Edward Gibbon\")"),
            "{reading}"
        );
    }
    // DocBook's `info` alike
    assert_eq!(
        qua("^ | ::head", &at("smith.docbook.xml", "text")),
        "%(title = \"The Wealth of Nations\"; officina = \"Strahan and Cadell\"; \
         locus = \"London\"; datum = \"1776-03-09\")"
    );
    assert_eq!(
        qua("^ | ::published", &at("smith.docbook.xml", "text")),
        "1776-03-09"
    );
}

#[test]
fn the_colophon_is_no_part_of_an_imported_text() {
    let dir = editions("text");
    let at = |file: &str, reading: &str| format!("{reading}:{}", dir.join(file).display());
    // the three editions differ in their publication statement alone
    // and read as the same text: no block, no sentence, no word
    for file in [
        "printed.tei.xml",
        "iso.tei.xml",
        "prose.tei.xml",
        "span.tei.xml",
        "month.tei.xml",
        "year.tei.xml",
    ] {
        assert_eq!(
            qua(r#"//* | :name @| unique @| join(" ")"#, &at(file, "text")),
            "section paragraph",
            "{file}"
        );
        assert_eq!(qua("//verbatim @| count", &at(file, "text")), "0", "{file}");
        assert_eq!(
            qua("//sentence @| count", &at(file, "corpus")),
            "2",
            "{file}"
        );
        assert_eq!(
            qua("//token<word> @| count", &at(file, "corpus")),
            "21",
            "{file}"
        );
        assert_eq!(
            qua(
                r#"//token[::lower = "strahan"] @| count"#,
                &at(file, "corpus")
            ),
            "0",
            "{file}"
        );
    }
    assert_eq!(
        qua("//token<word> @| count", &at("smith.docbook.xml", "corpus")),
        "25"
    );
}

#[test]
fn a_native_document_keeps_its_colophon_and_declares_it() {
    let dir = editions("native");
    let target = format!("text:{}", dir.join("gibbon.atd").display());
    // declared under the canonical names, whatever alias wrote them
    assert_eq!(
        qua("^ | ::head", &target),
        r#"%(officina = "Strahan and Cadell"; locus = "London"; datum = "1776-02-17")"#
    );
    assert_eq!(qua("^ | ::published", &target), "1776-02-17");
    // and a block of its text, as its title line would be
    assert_eq!(
        qua(r#"//* | :name @| unique @| join(" ")"#, &target),
        "paragraph verbatim"
    );
}

#[test]
fn a_tag_opened_inside_a_tag_is_refused() {
    // atrep 0.3.11: every XML importer refuses a `<` inside a tag,
    // naming the tag (the file was read on, to a refusal that named
    // a token of the importer).
    let dir = editions("refusal");
    let file = dir.join("broken.tei.xml");
    std::fs::write(
        &file,
        tei("<p>x</p>").replace("<p>In the", "<p rend=\"a\" <hi>In</hi> the"),
    )
    .unwrap();
    let (success, _, err) = run(&["//paragraph::", &format!("text:{}", file.display())]);
    assert!(!success);
    assert!(
        err.contains("tei import: `<` inside the tag `<p rend=\"a\" <hi`")
            && err.contains("the file is not well-formed XML"),
        "{err}"
    );
}
