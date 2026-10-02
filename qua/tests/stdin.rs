//! `text:-` and `corpus:-` read standard input — the pipe spelling
//! of `text:/dev/stdin` (a seam from the corpus-recipes round).

use std::io::Write;
use std::process::{Command, Stdio};

fn qua(query: &str, target: &str, input: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_qua"))
        .args([query, target])
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
    assert!(
        out.status.success(),
        "qua {query} {target}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

const PROSE: &str = "One two. Three four.\n\nFive six.\n";

#[test]
fn text_dash_reads_stdin() {
    assert_eq!(qua("//paragraph @| count", "text:-", PROSE), "2\n");
    assert_eq!(qua("//paragraph | :: | sc @| sum", "text:-", PROSE), "3\n");
}

#[test]
fn corpus_dash_reads_stdin() {
    assert_eq!(qua("//token<word> @| count", "corpus:-", PROSE), "6\n");
}

/// CoNLL-U on the pipe (ruling #63): the treebank reading, by the
/// sniff or by `?format=conllu`, never a prose tokenization.
const TREEBANK: &str = "# sent_id = 1
# text = One two.
1	One	one	NUM	CD	_	0	root	_	_
2	two	two	NUM	CD	_	1	flat	_	SpaceAfter=No
3	.	.	PUNCT	.	_	1	punct	_	_

# sent_id = 2
# text = Three four.
1	Three	three	NUM	CD	_	0	root	_	_
2	four	four	NUM	CD	_	1	flat	_	SpaceAfter=No
3	.	.	PUNCT	.	_	1	punct	_	_
";

#[test]
fn corpus_dash_reads_conllu() {
    assert_eq!(qua("//token @| count", "corpus:-", TREEBANK), "6\n");
    assert_eq!(
        qua("//token[::::n = 1]::lemma", "corpus:-", TREEBANK),
        "one\n"
    );
    assert_eq!(
        qua("//token @| count", "corpus:-?format=conllu", TREEBANK),
        "6\n"
    );
    assert_eq!(qua("//sentence @| count", "text:-", TREEBANK), "2\n");
    // Prose on the pipe keeps the prose reading.
    assert_eq!(qua("//sentence @| count", "text:-", PROSE), "0\n");
}
