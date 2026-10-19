//! Documents declared in a model file and the notation of standard
//! input (spec: The Corpus Reading; the qua chapter).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-declared-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str], input: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str], input: &str) -> String {
    let (out, err, ok) = qua(dir, args, input);
    assert!(ok, "{args:?}: {err}");
    out
}

/// A node constructor whose role wears the trait `document` declares
/// a corpus: `text` is the body, every other key a declaration.
#[test]
fn a_model_document_container_is_a_corpus() {
    let dir = fixture("model");
    std::fs::write(
        dir.join("posts.json"),
        r#"{"posts": [
          {"id": 1, "title": "Ithaca", "author": "Homer", "posted": "1899-05-06",
           "body": "Tell me, Muse, of the man of many ways. He wandered far."},
          {"id": 2, "title": "Troy", "author": "Homer", "posted": "1899-05-07",
           "body": "Sing, goddess, the anger of Achilles."}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("blog.model"),
        "node /blog/post<document>: /posts/* | %(text = ::body; title = ::title; author = ::author; date = ::posted);\n",
    )
    .unwrap();
    let q = |query: &str| ok(&dir, &["--model", "blog.model", query, "posts.json"], "");
    assert_eq!(q("/blog/post:::title"), "Ithaca\nTroy");
    assert_eq!(q("/blog/post[:::title = \"Troy\"]:::author"), "Homer");
    assert_eq!(q("/blog/post//sentence @| count"), "3");
    assert_eq!(q("/blog/post[:::title = \"Troy\"]//token @| count"), "9");
    assert_eq!(q("/blog/post<document> @| count"), "2");
}

/// Standard input is read as JSON by default and never sniffed; a
/// notation flag names anything else.
#[test]
fn stdin_is_json_unless_a_flag_says() {
    let dir = fixture("stdin");
    assert_eq!(ok(&dir, &["/a::"], r#"{"a": 1}"#), "1");
    // JSON Lines are named: the default takes one JSON text
    let (_, err, good) = qua(&dir, &["/a::"], "{\"a\": 1}\n{\"a\": 2}\n");
    assert!(!good && err.contains("default notation"), "{err}");
    assert_eq!(
        ok(
            &dir,
            &["--format", "jsonl", "/*/a::"],
            "{\"a\": 1}\n{\"a\": 2}\n"
        ),
        "1\n2"
    );
    // markup is not guessed from a bracket
    let (_, err, good) = qua(&dir, &["/root/x::"], "<root><x>1</x></root>");
    assert!(!good && err.contains("JSON"), "{err}");
    assert_eq!(
        ok(&dir, &["--xml", "/root/x::"], "<root><x>1</x></root>"),
        "1"
    );
    assert_eq!(
        ok(
            &dir,
            &["--format", "xml", "/root/x::"],
            "<root><x>1</x></root>"
        ),
        "1"
    );
    assert_eq!(ok(&dir, &["--yaml", "/a::"], "a: 1\n"), "1");
    assert_eq!(ok(&dir, &["--toml", "/a::"], "a = 1\n"), "1");
    assert_eq!(
        ok(
            &dir,
            &["--html", "//p::"],
            "<html><body><p>hi</p></body></html>"
        ),
        "hi"
    );
    assert_eq!(
        ok(&dir, &["--text", "//paragraph @| count"], "One.\n\nTwo.\n"),
        "2"
    );
    assert_eq!(
        ok(&dir, &["--markdown", "//h1::"], "# Title\n\ntext\n"),
        "Title"
    );
    assert_eq!(ok(&dir, &["--format", "csv", "/*::a"], "a,b\n1,2\n"), "1");
    assert_eq!(ok(&dir, &["--format", "yml", "/a::"], "a: 1\n"), "1");
    let (_, err, good) = qua(&dir, &["--format", "jsonx", "/a::"], "{}");
    assert!(!good && err.contains("no such notation"), "{err}");
    let (_, err, good) = qua(&dir, &["--yaml", "--xml", "/a::"], "a: 1");
    assert!(!good, "{err}");
}
