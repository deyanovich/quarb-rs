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
        values(&a, "/*[::from = \"bo@x\"]::in-reply-to-->::subject"),
        ["Plan"]
    );
    assert_eq!(
        values(&a, "/*[::message-id *= \"p1\"]<-in-reply-to::from"),
        ["bo@x"]
    );
    // The parsed epoch: 09:00 +0200 = 07:00 UTC.
    assert_eq!(
        values(&a, "/*[::from = \"ada@x\"]::::epoch"),
        ["1783407600"]
    );
}

#[test]
fn encoded_word_headers_decode() {
    let dir = std::env::temp_dir().join("quarb-maildir-words");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("cur")).unwrap();
    std::fs::create_dir_all(dir.join("new")).unwrap();
    // Б: "Отчёт" in windows-1251, base64; Q: "Plan für Juni" in utf-8.
    std::fs::write(
        dir.join("cur/300.c:2,S"),
        "From: =?utf-8?Q?J=C3=BCrgen?= <j@x>\nSubject: =?windows-1251?B?zvL3uPI=?= =?utf-8?Q?_f=C3=BCr_Juni?=\n\
         Message-ID: <e1@x>\nDate: Tue, 7 Jul 2026 11:00:00 +0200\n\nText.\n",
    )
    .unwrap();
    let a = MaildirAdapter::open(&dir).unwrap();
    assert_eq!(
        values(&a, "/*[::message-id *= \"e1\"]::subject"),
        ["Отчёт für Juni"]
    );
    assert_eq!(
        values(&a, "/*[::message-id *= \"e1\"]::from"),
        ["Jürgen <j@x>"]
    );
    // Q text beginning with =XX, and a word folded onto the next line
    std::fs::write(
        dir.join("cur/400.d:2,S"),
        "From: x@x\nSubject: =?utf-8?Q?=D0=98=D0=BB=D0=B8=D0=B0=D0=B4=D0=B0?=\n\
         \x20=?utf-8?B?INCT0L7QvNC10YDQsA==?=\nMessage-ID: <e2@x>\n\nText.\n",
    )
    .unwrap();
    let a = MaildirAdapter::open(&dir).unwrap();
    assert_eq!(
        values(&a, "/*[::message-id *= \"e2\"]::subject"),
        ["Илиада Гомера"]
    );
}
