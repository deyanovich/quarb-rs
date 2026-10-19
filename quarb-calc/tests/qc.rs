//! `qc` is `qua` after the expression head: the same bytes for the
//! same query, the `=` accepted and not required, and a query that
//! needs a source refused by name.
use std::process::Command;

fn qc(args: &[&str]) -> (bool, String) {
    qc_in(args, None)
}

/// `qc` with `input` on its standard input (`None`: the null
/// device, an empty pipe — a terminal cannot be had under a test).
fn qc_in(args: &[&str], input: Option<&str>) -> (bool, String) {
    use std::io::Write;
    use std::process::Stdio;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qc"));
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("qc runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.unwrap_or("").as_bytes())
        .expect("writes");
    let out = child.wait_with_output().expect("qc runs");
    let text = if out.status.success() {
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
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
    assert_eq!(
        qc(&["@(1; 2)", "+|", "3"]),
        (true, "@(1; 2; 3)\n".to_string())
    );
    assert_eq!(
        qc(&["#(@(title; year); %(\"Hamlet\"; 1603); %(\"Emma\"; \"1815\")) | type"]),
        (true, "@(\"text\"; \"int\")\n".to_string())
    );
}

#[test]
fn the_output_forms_are_quas() {
    assert_eq!(
        qc(&["--json", "= @(1; 2)"]),
        (true, "[[1, 2]]\n".to_string())
    );
    assert_eq!(
        qc(&["--csv", "= #(@(a; b); %(1; 2))"]),
        (true, "a,b\n1,2\n".to_string())
    );
    let (ok, text) = qc(&["--pretty", "= #(@(a; b); %(1; 2))"]);
    assert!(ok && text.contains('a') && text.contains('1'), "{text}");
}

#[test]
fn a_source_is_refused_by_name() {
    // A file on disk among the arguments of a query that does not
    // parse is named as the source the calculator does not take; a
    // word that is no file is just part of the expression.
    let (ok, text) = qc(&["= 1", "Cargo.toml"]);
    assert!(
        !ok && text.contains("takes no input") && text.contains("'Cargo.toml'"),
        "{text}"
    );
    let (ok, text) = qc(&["= 1", "data.json"]);
    assert!(!ok && text.contains("parsing the query"), "{text}");
    let (ok, text) = qc(&["/rows/*::x"]);
    assert!(
        !ok && text.contains("reads a document") && text.contains("'/rows/*::x'"),
        "{text}"
    );
    let (ok, text) = qc(&["= (/rows/*::x) | count"]);
    assert!(!ok && text.contains("reads a document"), "{text}");
    let (ok, text) = qc(&["--model", "x.model", "= 1"]);
    assert!(!ok && text.contains("--model"), "{text}");
    let (ok, text) = qc(&["= 1 +"]);
    assert!(!ok && text.contains("parsing the query"), "{text}");
}

/// The pipe is the topic: read whole, as one text, never parsed;
/// `-l` is one thread per line; a body opening with a pipe pipes
/// from the topic.
#[test]
fn the_pipe_is_the_topic() {
    // The whole text, final newline included.
    assert_eq!(
        qc_in(&["$_"], Some("hello\n")),
        (true, "hello\n\n".to_string())
    );
    assert_eq!(
        qc_in(&["$_ | wc"], Some("a b\nc\n")),
        (true, "3\n".to_string())
    );
    // Never parsed: a JSON document is text here.
    assert_eq!(
        qc_in(&["$_ | wc"], Some("{\"a\": 1}\n")),
        (true, "2\n".to_string())
    );
    // An empty pipe is the empty text; no pipe at all is not had under a test.
    assert_eq!(qc_in(&["$_"], None), (true, "\n".to_string()));
    // -l: one thread per line, the final newline closing the last
    // line, a blank line a thread with the empty text.
    assert_eq!(
        qc_in(&["-l", "$_ * 2"], Some("1\n2\n3\n")),
        (true, "2\n4\n6\n".to_string())
    );
    assert_eq!(
        qc_in(&["-l", "@| count"], Some("a\n\nb\n")),
        (true, "3\n".to_string())
    );
    assert_eq!(
        qc_in(&["-l", "@| count"], Some("a\nb")),
        (true, "2\n".to_string())
    );
    assert_eq!(
        qc_in(&["-l", "@| sum"], Some("1\n2\n3\n4\n5\n")),
        (true, "15\n".to_string())
    );
    assert_eq!(
        qc_in(&["-l", "upper"], Some("a\nb\n")),
        (true, "A\nB\n".to_string())
    );
    // The flag is sugar for `$_ | lines |` at the head.
    assert_eq!(
        qc_in(&["$_ | lines @| count"], Some("a\nb\n")),
        (true, "2\n".to_string())
    );
    assert_eq!(
        qc_in(&["--expand", "-l", "| upper"], None),
        (true, "= $_ | lines | upper\n".to_string())
    );
    // A body opening with a pipe stands for `$_ | …`.
    assert_eq!(
        qc_in(&["| words @| count"], Some("a b c\n")),
        (true, "3\n".to_string())
    );
    assert_eq!(
        qc_in(&["-l", "| upper"], Some("a\nb\n")),
        (true, "A\nB\n".to_string())
    );
    assert_eq!(
        qc_in(&["--expand", "| upper"], None),
        (true, "= $_ | upper\n".to_string())
    );
}

#[test]
fn nan_orders_after_every_number() {
    // The spec: NaN orders after every other number, ascending and
    // descending alike, and is never an extreme.
    assert_eq!(qc(&["@(3; 0 div 0; 1) | ... @| sort"]).1, "1\n3\nnan\n");
    assert_eq!(
        qc(&["@(3; 0 div 0; 1) | ... @| sort_by(-$_)"]).1,
        "3\n1\nnan\n"
    );
    assert_eq!(
        qc(&["@(3; 0 div 0; 1) | ... @| sort_by($_)"]).1,
        "1\n3\nnan\n"
    );
    assert_eq!(qc(&["@(3; 0 div 0; 1) | ... @| top(2; $_)"]).1, "3\n1\n");
    assert_eq!(qc(&["@(3; 0 div 0; 1) | ... @| bottom(2; $_)"]).1, "1\n3\n");
    assert_eq!(qc(&["@(3; 0 div 0; 1) | ... @| max"]).1, "3\n");
    assert_eq!(qc(&["@(3; 0 div 0; 1) | ... @| min"]).1, "1\n");
    assert_eq!(qc(&["@(3; 0 div 0; 1) | ... @| max_by($_)"]).1, "3\n");
    // Equality still drops it, as the spec says.
    assert_eq!(
        qc(&["@(3; 0 div 0; 1) | ... | [$_ = $_] @| count"]).1,
        "2\n"
    );
}

#[test]
fn a_command_of_the_graphs_layer_is_refused_by_name() {
    // The registry tags `link` as the graph's: it reads a node.
    let (ok, text) = qc(&["1 | link"]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("names a node") && text.contains("`link`"),
        "{text}"
    );
    // A value command with a node fallback stays the calculator's.
    assert_eq!(qc(&["%(a = 1) | json"]).1, "{\"a\": 1}\n");
}
