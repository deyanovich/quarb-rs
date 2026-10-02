//! `text:DIR` over a folder of documents (lang/TODO.md): the folder
//! tree, each document leaf read at the text level — sections and
//! paragraphs under every file, subfolders walked. A folder that
//! holds a treebank still reads as the treebank
//! (corpus_conllu.rs covers that).

fn qua(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
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
    assert_eq!(qua(&["//sentence @| count", &corpus]), "5\n");
    assert_eq!(
        qua(&[
            r#"//*[:::name == *".md"] | "${:::name} ${(//token<word> @| count)}""#,
            &corpus
        ]),
        "01-one.md 6\n02-two.md 8\n03-three.md 3\n"
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
