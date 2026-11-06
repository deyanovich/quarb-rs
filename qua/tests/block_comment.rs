//! The block comment `(!! comment )` (ruling 2026-11-06): whitespace
//! to the parser wherever whitespace may stand — query text, a
//! definitions file, a model file — closed by the `)` that balances
//! its opener, never printed back, refused at the opener when nothing
//! balances it.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-comment-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("rows.csv"), "id,title\n1,Iliad\n2,Emma\n").unwrap();
    std::fs::write(
        dir.join("lib.quarb"),
        "# the header line\ndef &two: = 1 (!! a comment; with ; and (parens) ) + 1;\n\
         (!! between the definitions )\ndef &three: = &two + 1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib.model"),
        format!(
            "(!! the model's header )\nmount t: {}; (!! the rows )\n",
            dir.join("rows.csv").display()
        ),
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (success, out, err) = qua(dir, args);
    assert!(success, "{args:?}: {err}");
    out
}

#[test]
fn a_comment_is_whitespace_in_query_text() {
    let d = fixture("query");
    assert_eq!(ok(&d, &["= 1 (!! one ) + 2"]), "3");
    assert_eq!(ok(&d, &["(!! leading )= 1 + 2"]), "3");
    assert_eq!(ok(&d, &["= 1 + 2 (!! trailing )"]), "3");
    assert_eq!(ok(&d, &["= 1 (!! (!! nested ) and (a; (b)) ) + 2"]), "3");
    assert_eq!(ok(&d, &["= 1 (!! | map(> 0 ? 1 : 2) ) + 2"]), "3");
    assert_eq!(
        ok(
            &d,
            &["/row(!! glued to the hop )[::id = 2] | ::title", "rows.csv"]
        ),
        "Emma"
    );
    assert_eq!(
        ok(
            &d,
            &["/row (!! before the pipe )| ::title @| count", "rows.csv"]
        ),
        "2"
    );
    // the comment is not part of a quarq's text
    assert_eq!(ok(&d, &["= q(= 1 (!! c ) + 2) | :unq"]), "($_ = 1 + 2)");
}

#[test]
fn an_unbalanced_comment_is_refused_at_its_opener() {
    let d = fixture("refused");
    let (success, _, err) = qua(&d, &["= 1 (!! open (x ) + 2"]);
    assert!(!success);
    assert!(err.contains("unterminated comment"), "{err}");
    assert!(err.contains("(at character 5)"), "{err}");
    // `(!!x)` opens a comment now; a double negation is two groups
    let (success, _, _) = qua(&d, &["= (!!x)"]);
    assert!(!success);
}

#[test]
fn a_comment_stands_in_a_definitions_file_and_a_model_file() {
    let d = fixture("files");
    assert_eq!(ok(&d, &["--defs", "lib.quarb", "= &three"]), "3");
    assert_eq!(ok(&d, &["--model", "lib.model", "/t/row @| count"]), "2");
    std::fs::write(d.join("bad.model"), "mount t: rows.csv; (!! open (x \n").unwrap();
    let (success, _, err) = qua(&d, &["--model", "bad.model", "/t/row @| count"]);
    assert!(!success);
    assert!(err.contains("unterminated comment"), "{err}");
}
