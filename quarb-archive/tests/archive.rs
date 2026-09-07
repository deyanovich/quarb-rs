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
    assert_eq!(values(&a, "/data/*::::size @| sum"), ["9 B"]);
    assert_eq!(values(&a, "/top.txt::::size"), ["3 B"]);
}

/// A tarball held in memory reads like one on disk.
#[test]
fn tar_from_bytes() {
    let bytes = {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut ar = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, "d/a.txt", &b"alpha"[..]).unwrap();
        ar.into_inner().unwrap().finish().unwrap()
    };
    let a = ArchiveAdapter::from_tar_bytes(&bytes).unwrap();
    assert_eq!(values(&a, "/d/a.txt::"), vec!["alpha"]);
    assert!(ArchiveAdapter::from_tar_bytes(b"PK\x03\x04 not a tar").is_err());
}
