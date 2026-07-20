//! Zip and tar fixtures built in-test.
use quarb_archive::ArchiveAdapter;

fn values(a: &ArchiveAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    }
}

#[test]
fn zip_tree_and_content() {
    let dir = std::env::temp_dir().join("quarb-archive-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.zip");
    let f = std::fs::File::create(&path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default();
    use std::io::Write as _;
    z.start_file("data/a.txt", o).unwrap();
    z.write_all(b"alpha").unwrap();
    z.start_file("data/b.txt", o).unwrap();
    z.write_all(b"beta").unwrap();
    z.start_file("top.txt", o).unwrap();
    z.write_all(b"top").unwrap();
    z.finish().unwrap();

    let a = ArchiveAdapter::open(&path).unwrap();
    assert_eq!(values(&a, "//*<file> @| count"), ["3"]);
    assert_eq!(values(&a, "/data/a.txt::"), ["alpha"]);
    // Sizes are typed byte quantities; totals stay typed.
    assert_eq!(values(&a, "/data/*;;;size @| sum"), ["9 B"]);
    assert_eq!(values(&a, "/top.txt;;;size"), ["3 B"]);
}
