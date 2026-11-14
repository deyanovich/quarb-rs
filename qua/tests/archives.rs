//! Archives and compressed files at the command line (the 0.63
//! queue): an archive is told by its bytes under any name; a
//! compressed file that holds one file is that file, with the
//! reading prefix and the mount options applying to it; an archive
//! inside an archive opens in place; `?format=` on a file chooses
//! its reader.

use std::io::Write as _;
use std::path::{Path, PathBuf};

fn run(args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.success(),
    )
}

fn lines(args: &[&str]) -> Vec<String> {
    let (out, err, ok) = run(args);
    assert!(ok, "qua {args:?}: {err}");
    out.lines().map(str::to_string).collect()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-archives-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const NOTES: &str = r#"{"keeper": "Callimachus", "rooms": ["east", "west"]}"#;

/// A tar of the room: notes.json and epic/iliad.txt.
fn room_tar() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, body) in [
        ("notes.json", NOTES.as_bytes()),
        ("epic/iliad.txt", b"Sing, goddess, the wrath of Achilles.\n"),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, body).unwrap();
    }
    b.into_inner().unwrap()
}

fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(bytes).unwrap();
    e.finish().unwrap()
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p.to_str().unwrap().to_string()
}

#[test]
fn an_archive_is_told_by_its_bytes_under_any_name() {
    let dir = scratch("names");
    let tar = room_tar();
    let members = ["/notes.json", "/epic/iliad.txt"];
    for (name, bytes) in [
        ("room.tar", tar.clone()),
        // the extension in capitals was refused
        ("ROOM.TAR", tar.clone()),
        // a plain tar under another name read as JSON, and failed
        ("backup.dat", tar.clone()),
        ("backup", tar.clone()),
        ("backup.bin", gz(&tar)),
    ] {
        let f = put(&dir, name, &bytes);
        assert_eq!(lines(&["//*<file> | :name-path", &f]), members, "{name}");
        assert_eq!(
            lines(&["/notes.json/keeper::", &f]),
            ["Callimachus"],
            "{name}"
        );
    }
    // a name the text dispatch owns is read as what it says
    let f = put(&dir, "room.json", NOTES.as_bytes());
    assert_eq!(lines(&["/keeper::", &f]), ["Callimachus"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_lone_compressed_file_is_the_file_inside_it() {
    let dir = scratch("lone");
    // path-bare, as the file inside would be
    let f = put(&dir, "notes.json.gz", &gz(NOTES.as_bytes()));
    assert_eq!(lines(&["/keeper::", &f]), ["Callimachus"]);
    assert_eq!(lines(&["/rooms/* | ::", &f]), ["east", "west"]);
    // the mount options are the inner file's
    let f = put(
        &dir,
        "grain.csv.gz",
        &gz(b"port;tons\nPiraeus;1,5\nKoper;NA\n"),
    );
    let target = format!("{f}?delimiter=semicolon&decimal=comma&null=NA");
    assert_eq!(lines(&["/row | ::tons", &target]), ["1.5", "null"]);
    // and so is a reading prefix
    let f = put(&dir, "hymn.md.gz", &gz(b"# Hymn\n\nSing, goddess.\n"));
    assert_eq!(
        lines(&["//section | ::lemma", &format!("text:{f}")]),
        ["Hymn"]
    );
    // a name nothing reads is refused as it would be bare, and
    // format= is the way through
    let bare = put(&dir, "dump", b"not json at all\n");
    let f = put(&dir, "dump.gz", &gz(b"not json at all\n"));
    let (_, bare_err, bare_ok) = run(&["//* @| count", &bare]);
    let (_, err, ok) = run(&["//* @| count", &f]);
    assert!(!bare_ok && !ok);
    assert!(
        bare_err.contains("parsing JSON") && err.contains("parsing JSON"),
        "{err}"
    );
    assert!(err.contains("the file inside"), "{err}");
    assert_eq!(
        lines(&["//paragraph @| count", &format!("{f}?format=text")]),
        ["1"]
    );
    // a compressed tar is an archive, as ever
    let f = put(&dir, "room.tar.gz", &gz(&room_tar()));
    assert_eq!(
        lines(&["//*<file> | :name-path", &f]),
        ["/notes.json", "/epic/iliad.txt"]
    );
    // nothing is left behind where the file was unpacked
    let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("quarb-unpacked-{}-", std::process::id()))
        })
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_archive_inside_an_archive_opens_in_place() {
    let dir = scratch("nested");
    let inner_tgz = gz(&room_tar());
    // a zip that holds a tar.gz and a zip
    let mut inner_zip = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut inner_zip);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("notes.json", o).unwrap();
        z.write_all(NOTES.as_bytes()).unwrap();
        z.finish().unwrap();
    }
    let inner_zip = inner_zip.into_inner();
    let f = dir.join("shipment.zip");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&f).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("room.tar.gz", o).unwrap();
        z.write_all(&inner_tgz).unwrap();
        z.start_file("east/room.zip", o).unwrap();
        z.write_all(&inner_zip).unwrap();
        z.start_file("manifest.txt", o).unwrap();
        z.write_all(b"two rooms\n").unwrap();
        z.finish().unwrap();
    }
    let f = f.to_str().unwrap();
    // the inner archives are archives, and their members walk
    assert_eq!(
        lines(&["/* | %(:name; kind = (:traits | join(\" \")))", f]),
        [
            r#"%(name = "room.tar.gz"; kind = "file archive")"#,
            r#"%(name = "east"; kind = "dir")"#,
            r#"%(name = "manifest.txt"; kind = "file document")"#,
        ]
    );
    assert_eq!(
        lines(&["/room.tar.gz/notes.json/keeper::", f]),
        ["Callimachus"]
    );
    assert_eq!(
        lines(&["/east/room.zip/notes.json/rooms/* | ::", f]),
        ["east", "west"]
    );
    assert_eq!(
        lines(&["/room.tar.gz/epic/iliad.txt::", f])[0],
        "Sing, goddess, the wrath of Achilles."
    );
    // a tar that holds the zip that holds them: three deep
    let mut b = tar::Builder::new(Vec::new());
    let zip_bytes = std::fs::read(f).unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_size(zip_bytes.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    b.append_data(&mut h, "shipment.zip", &zip_bytes[..])
        .unwrap();
    let outer = put(&dir, "crate.tar", &b.into_inner().unwrap());
    assert_eq!(
        lines(&["/shipment.zip/east/room.zip/notes.json/keeper::", &outer]),
        ["Callimachus"]
    );
    // under --no-graft every member stays a leaf
    assert_eq!(lines(&["--no-graft", "/room.tar.gz/* @| count", f]), ["0"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn format_on_a_file_chooses_its_reader() {
    let dir = scratch("format");
    // the option was accepted on a file and the file read by its
    // name all the same
    let f = put(&dir, "notes.txt", NOTES.as_bytes());
    assert_eq!(
        lines(&["/keeper::", &format!("{f}?format=json")]),
        ["Callimachus"]
    );
    let f = put(&dir, "grain", b"port;tons\nPiraeus;15\n");
    assert_eq!(
        lines(&[
            "/row | ::tons",
            &format!("{f}?format=csv&delimiter=semicolon")
        ]),
        ["15"]
    );
    let f = put(&dir, "hymn.txt", b"# Hymn\n\nSing, goddess.\n");
    assert_eq!(
        lines(&["//h1::", &format!("{f}?format=markdown")]),
        ["Hymn"]
    );
    // without it the file is what its name says
    let f = put(&dir, "plain.txt", NOTES.as_bytes());
    assert_eq!(lines(&["//paragraph @| count", &f]), ["1"]);
    // a word that names no notation is refused, as before
    let (_, err, ok) = run(&["/*", &format!("{f}?format=lines")]);
    assert!(!ok && err.contains("names no notation"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
