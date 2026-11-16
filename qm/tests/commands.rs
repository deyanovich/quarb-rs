//! Every subcommand that builds a query runs on a small mailbox.
//!
//! `qm` writes its queries itself (`from`, `grep`, `unread`,
//! `since`, `count`), so a spelling the language retires breaks a
//! subcommand without touching this crate: `grep` went on writing
//! `=~` and the bare `or` after both were retired, and answered a
//! lex error. These tests run the binary, so each built query is
//! parsed by the engine of the day.
use std::path::{Path, PathBuf};
use std::process::Command;

fn mailbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qm-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["cur", "new", "tmp"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).unwrap();
    write(
        "cur/100.a:2,S",
        "From: ada@x\nTo: team@x\nSubject: Release plan\nMessage-ID: <p1@x>\n\
         Date: Mon, 2 Mar 2026 09:00:00 +0000\n\nShip on Friday (maybe).\n",
    );
    write(
        "cur/200.b:2,S",
        "From: bo@x\nTo: team@x\nSubject: Re: Release plan\nMessage-ID: <r1@x>\n\
         In-Reply-To: <p1@x>\nDate: Tue, 3 Mar 2026 10:05:00 +0000\n\nWorks for me.\n",
    );
    write(
        "cur/300.c:2,S",
        "From: dee@x\nTo: ops@x\nTo: team@x\nCc: ada@x\nCc: bo@x\n\
         Subject: Billing pipeline is down\nMessage-ID: <b1@x>\n\
         Date: Fri, 10 Apr 2026 14:00:00 +0000\n\n\
         The INVOICE job failed in the caf\u{e9} run: 100% [stuck].\n",
    );
    write(
        "new/400.d",
        "From: dee@x\nTo: team@x\nSubject: Q2 invoice summary\nMessage-ID: <q1@x>\n\
         Date: Tue, 7 Jul 2026 08:05:00 +0000\n\nTotals attached.\n",
    );
    dir
}

/// The standard output of `qm -m DIR ARGS`; a failure is the test's.
fn qm(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_qm"))
        .arg("-m")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "qm {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The inbox numbers a listing prints, in its order.
fn numbers(listing: &str) -> Vec<usize> {
    listing
        .lines()
        .map(|l| l.split_whitespace().next().unwrap().parse().unwrap())
        .collect()
}

#[test]
fn ls_lists_newest_first_and_stars_the_unread() {
    let dir = mailbox("ls");
    let out = qm(&dir, &["ls"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(numbers(&out), [1, 2, 3, 4]);
    assert!(lines[0].ends_with("Q2 invoice summary"), "{out}");
    assert!(lines[0].starts_with("   1 * "), "{out}");
    assert!(lines[1].ends_with("Billing pipeline is down"), "{out}");
    assert!(lines[1].starts_with("   2   "), "{out}");
    assert!(lines[3].ends_with("Release plan"), "{out}");
}

#[test]
fn grep_reads_subject_and_body_in_either_case() {
    let dir = mailbox("grep");
    // The subject of 1, the body of 2 (written INVOICE there).
    assert_eq!(numbers(&qm(&dir, &["grep", "invoice"])), [1, 2]);
    assert_eq!(numbers(&qm(&dir, &["grep", "WORKS FOR"])), [3]);
    assert_eq!(qm(&dir, &["grep", "no such text"]), "");
}

#[test]
fn grep_takes_its_pattern_as_literal_text() {
    let dir = mailbox("literal");
    // Regex operators are text: the brackets, the percent sign,
    // the parentheses and the dot.
    assert_eq!(numbers(&qm(&dir, &["grep", "100% [stuck]."])), [2]);
    assert_eq!(numbers(&qm(&dir, &["grep", "(maybe)."])), [4]);
    assert_eq!(qm(&dir, &["grep", "Frida."]), "");
    assert_eq!(qm(&dir, &["grep", "Ship|Works"]), "");
    // A letter outside ASCII is no operator and takes no escape
    // (`\é` is a regex error).
    assert_eq!(numbers(&qm(&dir, &["grep", "caf\u{e9} run"])), [2]);
    // A slash would close the regex literal, a quote a string.
    assert_eq!(qm(&dir, &["grep", "a/b\"c'd\\e"]), "");
}

#[test]
fn from_unread_and_since_filter_the_listing() {
    let dir = mailbox("filters");
    assert_eq!(numbers(&qm(&dir, &["from", "dee"])), [1, 2]);
    assert_eq!(numbers(&qm(&dir, &["from", "a\"b"])), Vec::<usize>::new());
    assert_eq!(numbers(&qm(&dir, &["unread"])), [1]);
    assert_eq!(numbers(&qm(&dir, &["since", "2026-04-01"])), [1, 2]);
    assert_eq!(
        numbers(&qm(&dir, &["since", "2026-07-08"])),
        Vec::<usize>::new()
    );
}

#[test]
fn count_tallies_by_sender() {
    let dir = mailbox("count");
    let out = qm(&dir, &["count"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "%(from = \"ada@x\"; messages = 1)",
            "%(from = \"bo@x\"; messages = 1)",
            "%(from = \"dee@x\"; messages = 2)",
        ]
    );
}

#[test]
fn thread_and_replies_follow_in_reply_to() {
    let dir = mailbox("thread");
    // From the reply (3) up to the root (4) and down again.
    let out = qm(&dir, &["thread", "3"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].starts_with("#4 ada@x"), "{out}");
    assert!(lines[1].starts_with("  #3 bo@x"), "{out}");
    assert_eq!(numbers(&qm(&dir, &["replies", "4"])), [3]);
    assert_eq!(qm(&dir, &["replies", "1"]), "");
}

#[test]
fn read_prints_the_headers_and_the_body() {
    let dir = mailbox("read");
    let out = qm(&dir, &["read", "1"]);
    assert_eq!(
        out,
        "from: dee@x\nto: team@x\ndate: Tue, 7 Jul 2026 08:05:00 +0000\n\
         subject: Q2 invoice summary\n\nTotals attached.\n"
    );
    // A header written twice is read whole, one address after the
    // other, as a mail reader shows it.
    let out = qm(&dir, &["read", "2"]);
    assert!(out.starts_with("from: dee@x\nto: ops@x, team@x\n"), "{out}");
}

#[test]
fn seen_moves_the_message_out_of_new() {
    let dir = mailbox("seen");
    assert_eq!(numbers(&qm(&dir, &["unread"])), [1]);
    qm(&dir, &["seen", "1"]);
    assert_eq!(qm(&dir, &["unread"]), "");
    assert_eq!(numbers(&qm(&dir, &["ls"])), [1, 2, 3, 4]);
}

#[test]
fn query_is_the_whole_language() {
    let dir = mailbox("query");
    let out = qm(&dir, &["query", "/*[::cc = \"bo@x\"]::subject"]);
    assert_eq!(out.trim_end(), "Billing pipeline is down");
}
