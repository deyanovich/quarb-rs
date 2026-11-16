//! A title the document only declares is no sentence at the word
//! level (ruling 2026-11-15; Text Readings, Corpus Reading).
//!
//! The text level reads a declared title (an HTML head's, a Markdown
//! header's, a TEI header's, LaTeX's `\title`, an RTF file's `\info`)
//! as declared identity: a property of the document node, no heading
//! and no paragraph. The word level follows the text level: where
//! the one has no node the other has no sentence. It had one, under
//! the document node, in every format the shared text model reads:
//! an edition that declares its title and also prints it as the
//! heading of its outer division counted the title twice (Hamlet,
//! four words and one sentence over).

use std::path::PathBuf;

fn qua(args: &[&str]) -> String {
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
        .trim_end()
        .to_string()
}

/// One work in five formats: the title declared, a heading in the
/// body where the format has one, and one sentence of prose.
fn editions(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-declared-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).unwrap();
    write(
        "caesar.md",
        "---\ntitle: De Bello Gallico\n---\n\n# Liber primus\n\n\
         Gallia est omnis divisa in partes tres.\n",
    );
    write(
        "caesar.html",
        "<html><head><title>De Bello Gallico</title></head><body>\
         <h1>Liber primus</h1><p>Gallia est omnis divisa in partes tres.</p>\
         </body></html>",
    );
    write(
        "caesar.tei.xml",
        "<?xml version=\"1.0\"?>\n<TEI xmlns=\"http://www.tei-c.org/ns/1.0\"><teiHeader>\
         <fileDesc><titleStmt><title>De Bello Gallico</title></titleStmt>\
         <publicationStmt><p>x</p></publicationStmt><sourceDesc><p>x</p></sourceDesc>\
         </fileDesc></teiHeader><text><body><div><head>Liber primus</head>\
         <p>Gallia est omnis divisa in partes tres.</p></div></body></text></TEI>",
    );
    write(
        "caesar.tex",
        "\\documentclass{article}\n\\title{De Bello Gallico}\n\\begin{document}\n\
         \\maketitle\n\\section{Liber primus}\nGallia est omnis divisa in partes tres.\n\
         \\end{document}\n",
    );
    write(
        "caesar.rtf",
        "{\\rtf1{\\info{\\title De Bello Gallico}}\\pard Gallia est omnis divisa \
         in partes tres.\\par}",
    );
    dir
}

#[test]
fn a_declared_title_leaves_no_sentence_and_no_token() {
    let dir = editions("counts");
    // the heading and the prose; the RTF file has no heading
    for (file, sentences, words, first) in [
        ("caesar.md", "2", "9", "Liber primus"),
        ("caesar.html", "2", "9", "Liber primus"),
        ("caesar.tei.xml", "2", "9", "Liber primus"),
        ("caesar.tex", "2", "9", "Liber primus"),
        (
            "caesar.rtf",
            "1",
            "7",
            "Gallia est omnis divisa in partes tres.",
        ),
    ] {
        let target = format!("corpus:{}", dir.join(file).display());
        assert_eq!(qua(&["//sentence @| count", &target]), sentences, "{file}");
        assert_eq!(qua(&["//token<word> @| count", &target]), words, "{file}");
        assert_eq!(qua(&["//sentence[1]::", &target]), first, "{file}");
        // no sentence stands under the document node itself, and no
        // word of the title is a token
        assert_eq!(qua(&["/sentence @| count", &target]), "0", "{file}");
        assert_eq!(
            qua(&["//token[::lower = \"bello\"] @| count", &target]),
            "0",
            "{file}"
        );
    }
}

#[test]
fn the_title_is_still_what_the_document_declares() {
    let dir = editions("identity");
    for file in [
        "caesar.md",
        "caesar.html",
        "caesar.tei.xml",
        "caesar.tex",
        "caesar.rtf",
    ] {
        for reading in ["text", "corpus"] {
            let target = format!("{reading}:{}", dir.join(file).display());
            assert_eq!(
                qua(&["^ | %(::title)", &target]),
                r#"%(title = "De Bello Gallico")"#,
                "{reading}:{file}"
            );
        }
    }
}

#[test]
fn a_title_printed_in_the_body_is_counted_once() {
    // the edition's case: the header declares the title and the
    // outer division prints it as its heading. The heading is a
    // sentence, as every heading is; the declaration adds none.
    let dir = editions("printed");
    let file = dir.join("printed.md");
    std::fs::write(
        &file,
        "---\ntitle: De Bello Gallico\n---\n\n# De Bello Gallico\n\n\
         Gallia est omnis divisa in partes tres.\n",
    )
    .unwrap();
    let target = format!("corpus:{}", file.display());
    assert_eq!(qua(&["//sentence @| count", &target]), "2");
    assert_eq!(qua(&["//token<word> @| count", &target]), "10");
    assert_eq!(
        qua(&["//token[::lower = \"bello\"] @| count", &target]),
        "1"
    );
    assert_eq!(qua(&["//sentence[1]::", &target]), "De Bello Gallico");
    // a document that declares nothing is as it was
    let bare = dir.join("bare.md");
    std::fs::write(
        &bare,
        "# Liber primus\n\nGallia est omnis divisa in partes tres.\n",
    )
    .unwrap();
    let target = format!("corpus:{}", bare.display());
    assert_eq!(qua(&["//sentence @| count", &target]), "2");
    assert_eq!(qua(&["//token<word> @| count", &target]), "9");
}
