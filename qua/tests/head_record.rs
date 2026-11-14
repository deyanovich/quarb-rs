//! `::head` on a document node is the record of everything the
//! document declares about itself, in every text-level format
//! (ruling 2026-11-14): each field under the name it was declared
//! with and with the value its format gives it. A field of it ends
//! a path and, the root assumed, heads an expression
//! (`::head:year`). `:::name` for a declared name is retired: three
//! colons are the reader's own facts, and for one release the old
//! spelling says where the declaration went.

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

fn ok(args: &[&str]) -> String {
    let (success, out, err) = run(args);
    assert!(success, "qua {args:?}: {err}");
    out
}

fn notes() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("qua-head-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in [
        (
            "iliad.md",
            "---\ntitle: The Iliad\nauthor: Homer\nyear: -750\ntags: [epic, greek]\n\
             edition:\n  editor: Monro\n  year: 1902\nog:type: book\n---\n\n# Book One\n\n\
             Sing, goddess, the wrath of Achilles.\n",
        ),
        (
            "odyssey.md",
            "---\ntitle: The Odyssey\nyear: -725\n---\n\n# Book One\n\nTell me, Muse.\n",
        ),
        ("empty.md", "---\n---\n\n# Empty\n\nNo declarations.\n"),
        (
            "greek.md",
            "---\ntitle: Menin\nlang: grc\n---\n\nMenin aeide.\n\n```rust\nfn main() {}\n```\n",
        ),
        ("plain.md", "# Plain\n\nNo header at all.\n"),
        (
            "page.html",
            "<html lang=\"en\"><head><title>The Histories</title>\
             <meta name=\"author\" content=\"Herodotus\">\
             <meta property=\"og:type\" content=\"book\">\
             <meta property=\"article:tag\" content=\"war\">\
             <meta property=\"article:tag\" content=\"inquiry\">\
             <script type=\"application/ld+json\">{\"@type\": \"Book\", \
             \"author\": {\"name\": \"Herodotus\"}, \"numberOfPages\": 716}</script>\
             </head><body><h1>Clio</h1><p>These are the researches.</p></body></html>",
        ),
    ] {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

#[test]
fn the_head_is_the_record_of_what_a_document_declares() {
    let dir = notes();
    let at = |f: &str| format!("text:{}", dir.join(f).display());
    // a Markdown header: every key, its YAML type kept
    assert_eq!(
        ok(&["::head", &at("iliad.md")]),
        concat!(
            r#"%(title = "The Iliad"; author = "Homer"; year = -750; "#,
            r#"tags = @("epic"; "greek"); edition = %(editor = "Monro"; year = 1902); "#,
            r#""og:type" = "book")"#
        )
    );
    // a field heads an expression, the root assumed; a nested
    // mapping reads on; a name with a colon is quoted
    assert_eq!(ok(&["::head:year", &at("iliad.md")]), "-750");
    assert_eq!(ok(&["::head:edition:editor", &at("iliad.md")]), "Monro");
    assert_eq!(ok(&[r#"::head:"og:type""#, &at("iliad.md")]), "book");
    assert_eq!(ok(&["::head:year | $_ + 50", &at("iliad.md")]), "-700");
    // the curated core is untouched
    assert_eq!(
        ok(&["^ | %(::title; ::author; ::tags)", &at("iliad.md")]),
        r#"%(title = "The Iliad"; author = "Homer"; tags = @("epic"; "greek"))"#
    );
    // the declared language is the curated `::lang`: a page's
    // `<html lang>`, a note's `lang` key; a verbatim block keeps
    // answering its own
    assert_eq!(ok(&["::lang", &at("page.html")]), "en");
    assert_eq!(ok(&["::lang", &at("iliad.md")]), "null");
    assert_eq!(ok(&["::lang", &at("greek.md")]), "grc");
    assert_eq!(ok(&["//verbatim::lang", &at("greek.md")]), "rust");
    // a header with nothing in it is the empty record and no part
    // of the text; no header at all is null
    assert_eq!(ok(&["::head", &at("empty.md")]), "%()");
    assert_eq!(ok(&["//paragraph::", &at("empty.md")]), "No declarations.");
    assert_eq!(ok(&["//rule @| count", &at("empty.md")]), "0");
    assert_eq!(ok(&["::head", &at("plain.md")]), "null");
    // a page: its head as it is written, a name declared more than
    // once a list, a JSON-LD block one nested record
    assert_eq!(
        ok(&["::head", &at("page.html")]),
        concat!(
            r#"%(title = "The Histories"; lang = "en"; author = "Herodotus"; "#,
            r#""og:type" = "book"; "article:tag" = @("war"; "inquiry"); "#,
            r#"ld = %("@type" = "Book"; author = %(name = "Herodotus"); numberOfPages = 716))"#
        )
    );
    assert_eq!(
        ok(&["::head:ld:author:name", &at("page.html")]),
        "Herodotus"
    );
    assert_eq!(ok(&[r#"::head:ld:"@type""#, &at("page.html")]), "Book");
}

#[test]
fn a_field_of_the_head_ends_a_path() {
    let dir = notes();
    let folder = format!("text:{}", dir.display());
    assert_eq!(ok(&["/iliad.md::head:year", &folder]), "-750");
    assert_eq!(
        ok(&["/*.md::head:year", &folder]),
        "null\nnull\n-750\n-725\nnull"
    );
    assert_eq!(ok(&["^ | /iliad.md::head:edition:year", &folder]), "1902");
    // inside brackets and records the chain reads as it did
    assert_eq!(ok(&["/*[::head:year < -730]::title", &folder]), "The Iliad");
    assert_eq!(
        ok(&["/odyssey.md | %(:name; y = ::head:year)", &folder]),
        r#"%(name = "odyssey.md"; y = -725)"#
    );
    // the documents that declare something: the empty record and
    // null both test false
    assert_eq!(
        ok(&["/*[::head] | :name", &folder]),
        "greek.md\niliad.md\nodyssey.md\npage.html"
    );
    // a union: the same fields on every branch
    assert_eq!(
        ok(&["/iliad.md::head:year || /odyssey.md::head:year", &folder]),
        "-750\n-725"
    );
    let (success, _, err) = run(&["/iliad.md::head:year || /odyssey.md::head:title", &folder]);
    assert!(!success);
    assert!(
        err.contains(
            "the branches of a union read different fields after their projections (':year' \
             on one of them): read the field over the pipe ('… | :year'), or write two queries"
        ),
        "{err}"
    );
    // a field no record has: null, told as after a pipe
    let (success, out, err) = run(&["/iliad.md::head:isbn", &folder]);
    assert!(success);
    assert_eq!(out, "null");
    assert!(err.contains("warning UNKNOWN_FIELD (:isbn)"), "{err}");
}

#[test]
fn three_colons_no_longer_read_a_declaration() {
    let dir = notes();
    let at = |f: &str| format!("text:{}", dir.join(f).display());
    let (success, out, err) = run(&[":::year", &at("iliad.md")]);
    assert!(success);
    assert_eq!(out, "null");
    assert_eq!(
        err,
        "warning RETIRED_KEY (:::year): ':::year' no longer reads what the document \
         declares: the declarations are the record '::head', and this one is '::head:year'"
    );
    // a name that needs quotes is shown quoted
    let (_, out, err) = run(&[r#":::"og:type""#, &at("page.html")]);
    assert_eq!(out, "null");
    assert!(err.contains(r#"this one is '::head:"og:type"'"#), "{err}");
    // a flattened JSON-LD name of 0.63 is told the path into the block
    let (_, out, err) = run(&[r#":::"schema:author.name""#, &at("page.html")]);
    assert_eq!(out, "null");
    assert!(err.contains("this one is '::head:ld:author:name'"), "{err}");
    // a name the document does not declare: null, and nothing said
    let (_, out, err) = run(&[":::isbn", &at("iliad.md")]);
    assert_eq!((out.as_str(), err.as_str()), ("null", ""));
    // the reader's own keys are untouched
    assert_eq!(ok(&["//section:::level", &at("iliad.md")]), "1");
}
