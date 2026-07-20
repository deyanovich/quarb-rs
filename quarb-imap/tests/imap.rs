//! Live test against the bundled mini IMAP server
//! (tests/server.py). Needs python3; spawns the server on a
//! scratch port.

use quarb_imap::ImapAdapter;

#[test]
fn folders_messages_and_headers() {
    let server = std::process::Command::new("python3")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/server.py"))
        .arg("10944")
        .stdout(std::process::Stdio::piped())
        .spawn();
    let Ok(mut server) = server else {
        eprintln!("skipping: python3 not available");
        return;
    };
    // Wait for the ready line.
    {
        use std::io::{BufRead, BufReader};
        let out = server.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(out).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
    }
    // SAFETY: single-threaded test process at this point.
    unsafe {
        std::env::set_var("QUARB_IMAP_USER", "u");
        std::env::set_var("QUARB_IMAP_PASS", "p");
    }
    let a = ImapAdapter::connect("imap://127.0.0.1:10944").unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/* @| count"), ["2"]); // INBOX + Archive
    assert_eq!(v("/INBOX/* @| count"), ["3"]);
    assert_eq!(
        v("/INBOX/*[::from *= \"cy\"]::subject"),
        ["Re: Release plan"]
    );
    assert_eq!(v("/INBOX/2::"), ["Friday works. Docs are ready.\n"]);
    assert_eq!(v("/INBOX/*[;;;epoch > 1783410000] @| count"), ["2"]);
    let _ = server.kill();
}
