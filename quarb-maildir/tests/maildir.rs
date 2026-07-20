//! A Maildir fixture with one thread.
use quarb_maildir::MaildirAdapter;

fn fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("quarb-maildir-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cur")).unwrap();
    std::fs::create_dir_all(dir.join("new")).unwrap();
    std::fs::write(
        dir.join("cur/100.a:2,S"),
        "From: ada@x\nSubject: Plan\nMessage-ID: <p1@x>\n\
         Date: Tue, 7 Jul 2026 09:00:00 +0200\n\nShip Friday.\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("new/200.b"),
        "From: bo@x\nSubject: Re: Plan\nMessage-ID: <r1@x>\nIn-Reply-To: <p1@x>\n\
         Date: Tue, 7 Jul 2026 10:30:00 +0200\n\nWorks.\n",
    )
    .unwrap();
    dir
}

fn values(a: &MaildirAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn headers_body_and_thread() {
    let a = MaildirAdapter::open(&fixture()).unwrap();
    assert_eq!(values(&a, "/* @| count"), ["2"]);
    assert_eq!(values(&a, "/*[::from = \"ada@x\"]::subject"), ["Plan"]);
    assert_eq!(values(&a, "/*[::from = \"bo@x\"]::"), ["Works.\n"]);
    // The thread: resolve up, backlink down.
    assert_eq!(
        values(&a, "/*[::from = \"bo@x\"]::in-reply-to~>::subject"),
        ["Plan"]
    );
    assert_eq!(
        values(&a, "/*[::message-id *= \"p1\"]<-in-reply-to::from"),
        ["bo@x"]
    );
    // The parsed epoch: 09:00 +0200 = 07:00 UTC.
    assert_eq!(values(&a, "/*[::from = \"ada@x\"];;;epoch"), ["1783407600"]);
}
