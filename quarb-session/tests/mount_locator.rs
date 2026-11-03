//! A mounted node renders as the mount's name and the source's own
//! locator (`/books/row[2]`), as the command line renders it — the
//! notebook door keeps each source's renderer at the mount.

use quarb_session::doc::Doc;
use quarb_session::{Executor, LocalExecutor};

const BOOKS: &str = "title,shelf\nIliad,A1\nOdyssey,B2\nAeneid,A1\n";

#[test]
fn a_mounted_node_renders_with_its_source_locator() {
    let doc = Doc::mount_docs(vec![
        ("books".to_string(), Doc::parse(BOOKS, "csv").unwrap()),
        (
            "notes".to_string(),
            Doc::parse(r#"{"k": [1, 2]}"#, "json").unwrap(),
        ),
    ])
    .unwrap();
    let ex = LocalExecutor::new(doc, (1_753_500_000, 0), false);
    let cells = ex.run(r#"/books/row[::shelf = "A1"]"#).unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, ["/books/row[1]", "/books/row[3]"]);
    let cells = ex.run("/notes/k/*").unwrap();
    let lines: Vec<String> = cells.iter().map(|c| c.display()).collect();
    assert_eq!(lines, ["/notes/k/0", "/notes/k/1"]);
}
