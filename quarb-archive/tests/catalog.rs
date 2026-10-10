//! An archive's catalog: members and sizes from the table alone.

use quarb::Confidence;
use std::io::Write;
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-archive-catalog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn books() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            "shelf/emma.txt",
            b"Emma Woodhouse, handsome, clever, and rich".as_slice(),
        ),
        (
            "shelf/iliad.txt",
            b"Sing, O goddess, the anger of Achilles".as_slice(),
        ),
        (
            "shelf/odyssey.txt",
            b"Tell me, O Muse, of that ingenious hero".as_slice(),
        ),
    ]
}

#[test]
fn a_tarball_is_counted_from_its_headers() {
    let path = scratch("books.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(gz);
    let mut total = 0;
    for (name, text) in books() {
        let mut h = tar::Header::new_gnu();
        h.set_size(text.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, text).unwrap();
        total += text.len() as u64;
    }
    ar.into_inner().unwrap().finish().unwrap();
    let c = quarb_archive::catalog(&path, quarb::Cap::NONE).unwrap();
    assert_eq!(c.entries, (3, 3));
    assert_eq!(c.bytes, Some((total, total)));
    assert_eq!(c.confidence, Confidence::Exact);
}

#[test]
fn a_zip_is_counted_from_its_central_directory() {
    let path = scratch("books.zip");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    z.add_directory("shelf", opts).unwrap();
    let mut total = 0;
    for (name, text) in books() {
        z.start_file(name, opts).unwrap();
        z.write_all(text).unwrap();
        total += text.len() as u64;
    }
    z.finish().unwrap();
    // Three members and the folder entry the zip lists.
    let c = quarb_archive::catalog(&path, quarb::Cap::NONE).unwrap();
    assert_eq!(c.entries, (4, 4));
    assert_eq!(c.bytes, Some((total, total)));
    assert_eq!(c.confidence, Confidence::Exact);
}

#[test]
fn the_opened_archive_estimates_members_and_sizes() {
    let path = scratch("open.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(gz);
    for (name, text) in books() {
        let mut h = tar::Header::new_gnu();
        h.set_size(text.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, text).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
    let a = quarb_archive::ArchiveAdapter::open(&path).unwrap();
    let plan = quarb::Query::parse("/shelf/*", &a).unwrap().plan(&a);
    let e = plan.branches[0].steps[1].estimate.clone().unwrap();
    assert_eq!(e.nodes, (3, 3));
    assert_eq!(
        e.bytes.map(|b| b.1),
        Some(books().iter().map(|b| b.1.len() as u64).sum())
    );
    assert_eq!(e.confidence, Confidence::Exact);
}

#[test]
fn a_cap_stops_the_walk_at_the_first_member_past_it() {
    let path = scratch("capped.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(gz);
    for (name, text) in books() {
        let mut h = tar::Header::new_gnu();
        h.set_size(text.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, text).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
    let cap = quarb::Cap {
        nodes: Some(1),
        bytes: None,
    };
    let c = quarb_archive::catalog(&path, cap).unwrap();
    assert_eq!(c.entries, (2, u64::MAX));
    assert_eq!(c.confidence, Confidence::Bound);
}

#[test]
fn a_path_into_the_archive_narrows_the_catalog() {
    let path = scratch("narrow.tar.gz");
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&path).unwrap(),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(gz);
    for (name, text) in books() {
        let mut h = tar::Header::new_gnu();
        h.set_size(text.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, text).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
    let c = quarb_archive::catalog_at(&path, &["shelf"], quarb::Cap::NONE).unwrap();
    assert_eq!(c.entries, (3, 3));
    let c = quarb_archive::catalog_at(&path, &["shelf", "emma.txt"], quarb::Cap::NONE).unwrap();
    assert_eq!(c.entries, (1, u64::MAX));
    assert_eq!(c.bytes, Some((42, 42)));
    assert_eq!(c.confidence, Confidence::Bound);
    assert_eq!(
        quarb_archive::catalog_at(&path, &["prose"], quarb::Cap::NONE),
        None
    );
    let zip_path = scratch("narrow.zip");
    let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    for (name, text) in books() {
        z.start_file(name, opts).unwrap();
        z.write_all(text).unwrap();
    }
    z.finish().unwrap();
    let c =
        quarb_archive::catalog_at(&zip_path, &["shelf", "iliad.txt"], quarb::Cap::NONE).unwrap();
    assert_eq!(c.bytes, Some((38, 38)));
}
