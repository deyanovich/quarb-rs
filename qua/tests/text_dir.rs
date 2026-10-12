//! `text:DIR` over a folder of documents (lang/TODO.md): the folder
//! tree, each document leaf read at the text level — sections and
//! paragraphs under every file, subfolders walked. A folder that
//! holds a treebank still reads as the treebank
//! (corpus_conllu.rs covers that).

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
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_folder_of_documents_reads_at_the_text_level() {
    let dir = std::env::temp_dir().join(format!("quarb-manuscript-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("part2")).unwrap();
    std::fs::write(
        dir.join("01-one.md"),
        "# One\n\nTom *painted* the fence.\n\n## Aside\n\nBen watched.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("02-two.md"),
        "# Two\n\nHuck came by. He had a dead cat.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("part2/03-three.md"),
        "# Three\n\nAunt Polly sighed.\n",
    )
    .unwrap();
    let text = format!("text:{}", dir.display());

    // the files and the subfolder, by name
    assert_eq!(
        qua(&["/* | :::name", &text]),
        "01-one.md\n02-two.md\npart2\n"
    );
    // sections across the whole folder, the subfolder included
    assert_eq!(
        qua(&["//section::lemma", &text]),
        "One\nAside\nTwo\nThree\n"
    );
    // words per chapter file: the prose, without the markup
    assert_eq!(
        qua(&[
            r#"//*[:::name == *".md"] | "${:::name} ${(//paragraph | :: | wc @| sum)}""#,
            &text
        ]),
        "01-one.md 6\n02-two.md 8\n03-three.md 3\n"
    );
    // the same folder as a named mount beside another
    assert_eq!(
        qua(&[
            "/ms//section @| count",
            &format!("ms={text}"),
            &format!("again={text}")
        ]),
        "4\n"
    );
    // the corpus reading over the same folder: tokens and sentences
    // under every chapter, counted across the whole manuscript
    let corpus = format!("corpus:{}", dir.display());
    // five of prose, four headings: headings are sentences
    assert_eq!(qua(&["//sentence @| count", &corpus]), "9\n");
    assert_eq!(
        qua(&[
            r#"//*[:::name == *".md"] | "${:::name} ${(//token<word> @| count)}""#,
            &corpus
        ]),
        // the headings' words counted too: headings are sentences
        "01-one.md 8\n02-two.md 9\n03-three.md 4\n"
    );
    assert_eq!(
        qua(&[r#"//token[::lower = "tom"] @| count"#, &corpus]),
        "1\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folder_of_editions_reads_at_the_literary_level() {
    let dir = std::env::temp_dir().join(format!("quarb-editions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let play =
        "@@@!litogramma\n\n@#() A Play\n\n@: Tom\nHello there.\n:@\n\n@: Huck\nHello.\n:@\n\n#@\n";
    std::fs::write(dir.join("play.atd"), play).unwrap();
    std::fs::write(dir.join("notes.csv"), "a,b\n1,2\n").unwrap();
    let lit = format!("lit:{}", dir.display());
    // the edition is read with its speeches; the table stays a leaf
    assert_eq!(qua(&["//dialogue @| count", &lit]), "2\n");
    assert_eq!(qua(&["/* | :::name", &lit]), "notes.csv\nplay.atd\n");
    // the text level reads the same edition's blocks without tokens
    let text = format!("text:{}", dir.display());
    assert_eq!(qua(&["//token @| count", &text]), "0\n");
    let _ = std::fs::remove_dir_all(&dir);
}

fn qua_full(args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// A leaf whose declared identity the reading knows but whose import
/// it refuses stays a document: its declared fields answer null and
/// the refusal rides into --explain, so one unreadable file does not
/// refuse the shelf. The same file mounted alone is refused with the
/// reason. An `.xml` declaring no identity the readings know is left
/// to the default grafting, its markup as a DOM.
#[test]
fn an_unreadable_leaf_answers_null_with_the_reason_in_explain() {
    let dir = std::env::temp_dir().join(format!("quarb-letters-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("good.xml"),
        r#"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<text><body>
<div type="chapter"><head>The fence</head>
<p>Tom painted the fence.</p>
<p>Ben watched.</p></div>
</body></text></TEI>"#,
    )
    .unwrap();
    // An inline the importer does not take (atrep is strict by the
    // user's rule: litogramma says what it can say and refuses the
    // rest): `<sub>`, as some Tolstoy Digital letters carry it.
    std::fs::write(
        dir.join("bad.xml"),
        r#"<TEI xmlns="http://www.tei-c.org/ns/1.0">
<teiHeader><fileDesc><titleStmt><title>A letter</title></titleStmt></fileDesc></teiHeader>
<text><body><p>Dear friend, H<sub>2</sub>O.</p></body></text></TEI>"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("plain.xml"),
        "<things><p>no identity declared</p></things>\n",
    )
    .unwrap();
    for level in ["text", "corpus", "lit"] {
        let mount = format!("{level}:{}", dir.display());
        // the paragraphs of the readable TEI, none from the refused
        // leaf, none from the undeclared markup
        assert_eq!(qua(&["//paragraph @| count", &mount]), "2\n", "{level}");
        // the refused leaf is a document with nothing under it
        assert_eq!(
            qua(&[r#"//*[:::name = "bad.xml"]//* @| count"#, &mount]),
            "0\n",
            "{level}"
        );
        assert_eq!(
            qua(&[r#"//*[:::name = "bad.xml"] | %(::lemma)"#, &mount]),
            "%(lemma = null)\n",
            "{level}"
        );
        // the shelf still lists it
        assert_eq!(
            qua(&["/* | :::name", &mount]),
            "bad.xml\ngood.xml\nplain.xml\n",
            "{level}"
        );
        // the reason rides into --explain, and only there
        let (ok, out, err) = qua_full(&["--explain", "//paragraph @| count", &mount]);
        assert!(ok && out == "2\n", "{level}: {out} {err}");
        assert!(
            err.contains("bad.xml") && err.contains("answer null") && err.contains("sub"),
            "{level}: {err}"
        );
        let (_, _, err) = qua_full(&["//paragraph @| count", &mount]);
        assert!(!err.contains("answer null"), "{level}: {err}");
        // mounted alone, the same file is refused with the reason
        let (ok, _, err) = qua_full(&[
            "//paragraph @| count",
            &format!("{level}:{}", dir.join("bad.xml").display()),
        ]);
        assert!(!ok && err.contains("sub"), "{level}: {err}");
    }
    // the undeclared markup keeps its DOM under text:
    assert_eq!(
        qua(&["//p::", &format!("text:{}", dir.display())]),
        "no identity declared\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
