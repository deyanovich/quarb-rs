//! `qc` is `qua` after the expression head: the same bytes for the
//! same query, the `=` accepted and not required, and a query that
//! needs a source refused by name.
use std::process::Command;

fn qc(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_qc"))
        .args(args)
        .env("QUARB_LOCALE", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("qc runs");
    let text = String::from_utf8_lossy(&out.stdout).to_string()
        + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn the_head_is_accepted_and_not_required() {
    assert_eq!(qc(&["= @(1; 2) +| 3"]), (true, "@(1; 2; 3)\n".to_string()));
    assert_eq!(qc(&["@(1; 2) +| 3"]), (true, "@(1; 2; 3)\n".to_string()));
    assert_eq!(qc(&["2 + 3 * 4"]), (true, "14\n".to_string()));
    // Unquoted: the arguments are joined with spaces, and a leading
    // negative number is a value, not an option.
    assert_eq!(qc(&["3", "+", "5"]), (true, "8\n".to_string()));
    assert_eq!(qc(&["-3", "+", "5"]), (true, "2\n".to_string()));
    assert_eq!(qc(&["=", "2", "*", "-4"]), (true, "-8\n".to_string()));
    assert_eq!(qc(&["@(1; 2)", "+|", "3"]), (true, "@(1; 2; 3)\n".to_string()));
    assert_eq!(
        qc(&["#(@(title; year); %(\"Hamlet\"; 1603); %(\"Emma\"; \"1815\")) | types"]),
        (true, "@(\"text\"; \"integer\")\n".to_string())
    );
}

#[test]
fn the_output_forms_are_quas() {
    assert_eq!(qc(&["--json", "= @(1; 2)"]), (true, "[[1, 2]]\n".to_string()));
    assert_eq!(qc(&["--csv", "= #(@(a; b); %(1; 2))"]), (true, "a,b\n1,2\n".to_string()));
    let (ok, text) = qc(&["--pretty", "= #(@(a; b); %(1; 2))"]);
    assert!(ok && text.contains('a') && text.contains('1'), "{text}");
}

#[test]
fn a_source_is_refused_by_name() {
    // A file on disk among the arguments of a query that does not
    // parse is named as the source the calculator does not take; a
    // word that is no file is just part of the expression.
    let (ok, text) = qc(&["= 1", "Cargo.toml"]);
    assert!(!ok && text.contains("takes no input") && text.contains("'Cargo.toml'"), "{text}");
    let (ok, text) = qc(&["= 1", "data.json"]);
    assert!(!ok && text.contains("parsing the query"), "{text}");
    let (ok, text) = qc(&["/rows/*::x"]);
    assert!(!ok && text.contains("reads a document") && text.contains("'/rows/*::x'"), "{text}");
    let (ok, text) = qc(&["= (/rows/*::x) | count"]);
    assert!(!ok && text.contains("reads a document"), "{text}");
    let (ok, text) = qc(&["--model", "x.model", "= 1"]);
    assert!(!ok && text.contains("--model"), "{text}");
    let (ok, text) = qc(&["= 1 +"]);
    assert!(!ok && text.contains("parsing the query"), "{text}");
}
