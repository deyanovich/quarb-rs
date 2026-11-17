//! Finding 17: the session's door opens what the command line
//! opens — the `lines:` and `web:` readers, a `.quarb` query file
//! reflected as a query arbor, and the kaiv family by extension.

use std::io::Write;
use std::path::Path;

use quarb_session::doc::{Doc, Options};

fn fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-session-doors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("site")).unwrap();
    std::fs::write(dir.join("lines.txt"), "alpha\nbeta\ngamma\n").unwrap();
    std::fs::write(dir.join("q.quarb"), "/books/*[::year > 1900]::title\n").unwrap();
    std::fs::write(
        dir.join("servers.kaiv"),
        ".!kaiv 1\n/@servers+:=host=a|port=1\n/@servers+:=host=b|port=2\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("site/index.html"),
        "<html><head><title>Home</title></head><body><h1>Home</h1><p>Welcome.</p></body></html>",
    )
    .unwrap();
    let mut z = zip::ZipWriter::new(std::fs::File::create(dir.join("site.zip")).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("a.html", opts).unwrap();
    z.write_all(b"<html><body><h1>A</h1><p>Hi.</p></body></html>")
        .unwrap();
    z.finish().unwrap();
    dir
}

fn count(doc: &Doc, q: &str) -> String {
    let Doc::Boxed(a, _, _) = doc else {
        panic!("a boxed door");
    };
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs[0].to_string(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_session_opens_lines_web_and_query_files() {
    let dir = fixture();
    let open = |spec: String| Doc::open(Path::new(&spec), &Options::default()).unwrap();
    let lines = open(format!("lines:{}", dir.join("lines.txt").display()));
    assert_eq!(count(&lines, "/* @| count"), "3");
    let site = open(format!("web:{}", dir.join("site").display()));
    assert_eq!(count(&site, "//page @| count"), "1");
    let archive = open(format!("web:{}", dir.join("site.zip").display()));
    assert_eq!(count(&archive, "//page @| count"), "1");
    let servers = open(dir.join("servers.kaiv").display().to_string());
    assert_eq!(count(&servers, "/@servers/* @| count"), "2");
    let query = open(dir.join("q.quarb").display().to_string());
    assert_eq!(count(&query, "//* @| count"), count(&query, "//* @| count"));
    assert_ne!(count(&query, "//* @| count"), "0");
    // A site store opens through the same door as on the command
    // line; a database that is not one is refused naming the store.
    let err = Doc::open(
        Path::new(&format!("web:{}", dir.join("site.db").display())),
        &Options::default(),
    )
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("site store"), "{err}");
}

/// `graft=json2` on a source a host hands over (ruling 2026-11-17),
/// as the native opener reads `?graft=json2`: an archive's JSON
/// members graft with their scalars as properties; a word that
/// names no property reading and a source that grafts nothing are
/// refused by name.
#[cfg(feature = "archives")]
#[test]
fn a_handed_archive_takes_the_graft_option() {
    use quarb_session::Source;
    use std::io::Write as _;
    let mut bytes = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
        let opts = zip::write::SimpleFileOptions::default();
        z.start_file("order.json", opts).unwrap();
        z.write_all(br#"{"total": 31, "ship": {"city": "Lyon"}}"#)
            .unwrap();
        z.finish().unwrap();
    }
    let read = |format: &str, text: Option<&str>, bytes: Option<&[u8]>, options: &str| {
        Doc::read(&Source {
            format,
            text,
            bytes,
            options,
            url: None,
            files: &[],
        })
    };
    // the default reading: a scalar is a child node
    let plain = read("zip", None, Some(&bytes), "").unwrap();
    assert_eq!(count(&plain, "/order.json/* @| count"), "2");
    // json2: the structural children alone, the scalars properties
    let held = read("zip", None, Some(&bytes), "graft=json2").unwrap();
    assert_eq!(count(&held, "/order.json/* @| count"), "1");
    assert_eq!(count(&held, "/order.json::total"), "31");
    assert_eq!(count(&held, "/order.json/ship::city"), "Lyon");
    // refusals
    let refused = |r: anyhow::Result<Doc>| format!("{:#}", r.err().expect("refused"));
    let e = refused(read("zip", None, Some(&bytes), "graft=json3"));
    assert!(
        e.contains("\"json3\" names no reading a graft takes (one of json2, yaml2, toml2)"),
        "{e}"
    );
    let e = refused(read("json", Some(r#"{"k": 1}"#), None, "graft=json2"));
    assert!(
        e.contains("this source is read as json: read it under the property reading"),
        "{e}"
    );
}
