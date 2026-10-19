//! Every archive format the crate reads, each told by its magic:
//! tar under gzip, xz, bzip2 and zstd; 7z; a zip whose members are
//! bzip2-compressed. One tree, five containers, one set of answers.

use quarb::QueryResult;
use quarb_archive::ArchiveAdapter;
use std::io::Write;
use std::path::PathBuf;

fn values(a: &ArchiveAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.locator(*n)).collect(),
    }
}

/// The tree: a catalogue and a note, under one directory.
fn tree(dir: &PathBuf) {
    std::fs::create_dir_all(dir.join("shelf")).unwrap();
    std::fs::write(
        dir.join("shelf/books.json"),
        r#"{"books": [{"title": "The Iliad"}, {"title": "The Odyssey"}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("shelf/note.txt"), "Гнев, богиня, воспой\n").unwrap();
}

fn plain_tar(dir: &PathBuf) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    b.append_dir_all("shelf", dir.join("shelf")).unwrap();
    b.into_inner().unwrap()
}

fn check(a: &ArchiveAdapter) {
    assert_eq!(values(a, "//note.txt::"), ["Гнев, богиня, воспой\n"]);
    assert_eq!(values(a, "//books.json:::size"), ["61_B"]);
    assert_eq!(values(a, "/shelf/* @| count"), ["2"]);
}

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "quarb-archive-formats-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    tree(&dir);
    dir
}

#[test]
fn tar_under_every_compression() {
    let dir = fixture("tar");
    let tar = plain_tar(&dir);
    // gzip
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar).unwrap();
    std::fs::write(dir.join("a.tar.gz"), gz.finish().unwrap()).unwrap();
    // xz
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(&tar), &mut xz).unwrap();
    std::fs::write(dir.join("a.tar.xz"), xz).unwrap();
    // bzip2
    let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    bz.write_all(&tar).unwrap();
    std::fs::write(dir.join("a.tar.bz2"), bz.finish().unwrap()).unwrap();
    // zstd
    let zst = ruzstd::encoding::compress_to_vec(
        std::io::Cursor::new(&tar),
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    std::fs::write(dir.join("a.tar.zst"), zst).unwrap();
    // plain, by extension
    std::fs::write(dir.join("a.tar"), &tar).unwrap();
    for name in ["a.tar.gz", "a.tar.xz", "a.tar.bz2", "a.tar.zst", "a.tar"] {
        let a = ArchiveAdapter::open(&dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        check(&a);
    }
    // the in-memory door tells the compression the same way
    let a = ArchiveAdapter::from_tar_bytes(&std::fs::read(dir.join("a.tar.xz")).unwrap()).unwrap();
    check(&a);
}

#[test]
fn seven_zip() {
    let dir = fixture("7z");
    sevenz_rust::compress_to_path(dir.join("shelf"), dir.join("a.7z")).unwrap();
    let a = ArchiveAdapter::open(&dir.join("a.7z")).unwrap();
    // 7z stores the directory's content at the top: no `shelf/`
    assert_eq!(values(&a, "/note.txt::"), ["Гнев, богиня, воспой\n"]);
    assert_eq!(values(&a, "/* @| count"), ["2"]);
    assert_eq!(values(&a, "/books.json:::size"), ["61_B"]);
}

#[test]
fn zip_with_bzip2_members() {
    let dir = fixture("zipbz");
    let f = std::fs::File::create(dir.join("a.zip")).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Bzip2);
    z.start_file("shelf/note.txt", opts).unwrap();
    z.write_all("Гнев, богиня, воспой\n".as_bytes()).unwrap();
    z.start_file("shelf/books.json", opts).unwrap();
    z.write_all(br#"{"books": [{"title": "The Iliad"}, {"title": "The Odyssey"}]}"#)
        .unwrap();
    z.finish().unwrap();
    let a = ArchiveAdapter::open(&dir.join("a.zip")).unwrap();
    check(&a);
}

#[test]
fn an_unknown_container_is_refused() {
    let dir = fixture("refuse");
    std::fs::write(dir.join("x.bin"), b"\x00\x01\x02\x03\x04\x05\x06").unwrap();
    let err = ArchiveAdapter::open(&dir.join("x.bin"))
        .err()
        .map(|e| e.to_string());
    assert!(
        err.as_deref()
            .is_some_and(|e| e.contains("zip, 7z, rar, chm, mhtml, or tar")),
        "{err:?}"
    );
}

/// Finding 20: a tar (or 7z) archive is held in memory, so its
/// decompressed size is bounded at open — past the bound the open
/// is refused naming the member it stopped at; an xz stream, which
/// lzma-rs decodes whole, stops at the bound inside the stream.
#[test]
fn an_archive_past_the_memory_bound_is_refused() {
    let dir = fixture("bound");
    let tar = plain_tar(&dir);
    std::fs::write(dir.join("a.tar"), &tar).unwrap();
    let mut xz = Vec::new();
    lzma_rs::xz_compress(&mut std::io::Cursor::new(&tar), &mut xz).unwrap();
    std::fs::write(dir.join("a.tar.xz"), xz).unwrap();
    let err = ArchiveAdapter::open_bounded(&dir.join("a.tar"), Default::default(), 8)
        .err()
        .map(|e| e.to_string())
        .unwrap();
    assert!(err.contains("exceed 8 bytes decompressed"), "{err}");
    let err = ArchiveAdapter::open_bounded(&dir.join("a.tar.xz"), Default::default(), 8)
        .err()
        .map(|e| e.to_string())
        .unwrap();
    assert!(err.contains("(xz stream)"), "{err}");
    // The xz check reads the declared sizes, never the stream: a
    // bound just under the tar's size refuses, one comfortably
    // over it opens (lzma-rs's writer stores rather than
    // compresses, so the compressed form is the larger of the two
    // and is bounded as well).
    assert!(
        ArchiveAdapter::open_bounded(
            &dir.join("a.tar.xz"),
            Default::default(),
            tar.len() as u64 - 1
        )
        .is_err()
    );
    ArchiveAdapter::open_bounded(
        &dir.join("a.tar.xz"),
        Default::default(),
        2 * tar.len() as u64,
    )
    .unwrap_or_else(|e| panic!("tar of {} bytes: {e}", tar.len()));
    assert!(ArchiveAdapter::open_bounded(&dir.join("a.tar"), Default::default(), 1 << 20).is_ok());
}

/// A stored zip with one member under a raw (undeclared) name —
/// the shape a legacy archiver writes under its code page.
fn raw_zip(name: &[u8], data: &[u8]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut c: u32 = 0xFFFF_FFFF;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xEDB8_8320
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    let crc = crc32(data).to_le_bytes();
    let size = (data.len() as u32).to_le_bytes();
    let nlen = (name.len() as u16).to_le_bytes();
    let mut out = Vec::new();
    // Local file header: no flags (no UTF-8 bit), stored.
    out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&crc);
    out.extend_from_slice(&size);
    out.extend_from_slice(&size);
    out.extend_from_slice(&nlen);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    let cd = out.len() as u32;
    out.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&crc);
    out.extend_from_slice(&size);
    out.extend_from_slice(&size);
    out.extend_from_slice(&nlen);
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(name);
    let cd_size = (out.len() as u32 - cd).to_le_bytes();
    out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 1, 0, 1, 0]);
    out.extend_from_slice(&cd_size);
    out.extend_from_slice(&cd.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// Finding 22: an entry name is bytes under the mount's rule —
/// strict UTF-8 by default, `?encoding=` for an archive written
/// under a legacy code page — never the format's CP437 guess.
#[test]
fn zip_entry_names_decode_under_the_mounts_rule() {
    let dir = fixture("names");
    // «привет.txt» in CP866, no UTF-8 flag.
    let bytes = raw_zip(b"\xaf\xe0\xa8\xa2\xa5\xe2.txt", b"ok\n");
    let path = dir.join("ru.zip");
    std::fs::write(&path, bytes).unwrap();
    let err = match ArchiveAdapter::open(&path) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("decoded where a refusal was due"),
    };
    assert!(err.contains("?encoding="), "{err}");
    let d = quarb::source::SourceDecoding::default()
        .with_option(Some("cp866"))
        .unwrap();
    let a = ArchiveAdapter::open_with_decoding(&path, d).unwrap();
    assert_eq!(values(&a, "/'привет.txt'::"), ["ok\n"]);
}
