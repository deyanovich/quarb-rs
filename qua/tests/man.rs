//! `qua man`: the command inventory at the command line — one
//! command's page, every command, one pipe's commands, the Russian
//! line; `--commands` lists the registry as TSV.

use std::process::Command;

fn qua(args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        out.status.success(),
    )
}

#[test]
fn man_is_the_door_to_the_inventory() {
    let (page, ok) = qua(&["man", "upper"]);
    assert!(
        ok && page.starts_with("upper — the text in upper case\n"),
        "{page}"
    );
    assert!(page.contains("  on null   null\n"), "{page}");
    let (ru, ok) = qua(&["man", "--ru", "upper"]);
    assert!(ok && ru.contains("верхнем"), "{ru}");
    let (all, ok) = qua(&["man"]);
    assert!(
        ok && all.lines().count() > 150 && all.contains("\ntree "),
        "{all}"
    );
    let (aggs, ok) = qua(&["man", "@|"]);
    assert!(
        ok && aggs.contains("\ntree ") && !aggs.contains("\nupper "),
        "{aggs}"
    );
    let (missing, ok) = qua(&["man", "uper"]);
    assert!(!ok && missing.contains("no command 'uper'"), "{missing}");
    let (tsv, ok) = qua(&["--commands"]);
    assert!(
        ok && tsv.starts_with("name\tkind\tpipes\tlayer\tonnull\tdoc\n"),
        "{tsv}"
    );
    assert!(tsv.contains("\ntree\tcontext\t@|\tnode\t"), "{tsv}");
}
