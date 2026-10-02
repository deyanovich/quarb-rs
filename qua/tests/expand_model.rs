//! `--expand` with `--model`: the model's definitions are in scope
//! for the expansion, as they are for the query itself.

#[test]
fn expand_sees_the_models_definitions() {
    let dir = std::env::temp_dir().join(format!("quarb-expand-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("books.csv"), "id,title\nemma,Emma\n").unwrap();
    std::fs::write(
        dir.join("desk.model"),
        "mount books: books.csv;\ndef &titled($t): /books/*[::title = $t];\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .args([
            "--model",
            dir.join("desk.model").to_str().unwrap(),
            "--expand",
            r#"&titled("Emma")::id"#,
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        r#"/books/*[::title = "Emma"]::id"#
    );
    let _ = std::fs::remove_dir_all(&dir);
}
