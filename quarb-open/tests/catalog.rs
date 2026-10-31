//! A target's catalog before it is opened: a directory's listing,
//! an archive's table, a file's size, a reading prefix looked
//! through, a database left to its adapter.

use quarb::{Cap, Confidence};
use std::path::PathBuf;

fn scratch() -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("quarb-open-catalog-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shelf/verse")).unwrap();
    std::fs::write(
        dir.join("shelf/emma.txt"),
        "Emma Woodhouse, handsome, clever, and rich",
    )
    .unwrap();
    std::fs::write(
        dir.join("shelf/persuasion.txt"),
        "Sir Walter Elliot, of Kellynch Hall",
    )
    .unwrap();
    std::fs::write(
        dir.join("shelf/verse/iliad.txt"),
        "Sing, O goddess, the anger of Achilles",
    )
    .unwrap();
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(dir.join("books.tar.gz")).unwrap(),
        flate2::Compression::default(),
    );
    let mut ar = tar::Builder::new(gz);
    for name in ["emma.txt", "persuasion.txt", "verse/iliad.txt"] {
        ar.append_path_with_name(dir.join("shelf").join(name), name)
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
    dir
}

#[test]
fn a_directory_is_listed_not_read() {
    let dir = scratch();
    let shelf = dir.join("shelf");
    let total: u64 = ["emma.txt", "persuasion.txt", "verse/iliad.txt"]
        .iter()
        .map(|n| std::fs::metadata(shelf.join(n)).unwrap().len())
        .sum();
    // Three files and one directory: four entries, the files' bytes.
    let c = quarb_open::catalog(shelf.to_str().unwrap(), Cap::NONE).unwrap();
    assert_eq!(c.entries, (4, 4));
    assert_eq!(c.bytes, Some((total, total)));
    assert_eq!(c.confidence, Confidence::Exact);
    // A reading prefix is looked through to the path.
    let c = quarb_open::catalog(&format!("corpus:{}", shelf.display()), Cap::NONE).unwrap();
    assert_eq!(c.entries, (4, 4));
}

#[test]
fn an_archive_is_counted_from_its_table() {
    let dir = scratch();
    let tar = dir.join("books.tar.gz");
    let c = quarb_open::catalog(&format!("text:{}?notes=omit", tar.display()), Cap::NONE).unwrap();
    assert_eq!(c.entries, (3, 3));
    assert_eq!(c.confidence, Confidence::Exact);
    assert!(c.bytes.unwrap().1 > 100);
}

#[test]
fn a_plain_file_is_its_size_and_a_database_is_its_adapters() {
    let dir = scratch();
    let emma = dir.join("shelf/emma.txt");
    let c = quarb_open::catalog(emma.to_str().unwrap(), Cap::NONE).unwrap();
    assert_eq!(c.entries, (1, u64::MAX));
    assert_eq!(c.bytes, Some((42, 42)));
    assert_eq!(c.confidence, Confidence::Bound);
    assert_eq!(
        quarb_open::catalog("postgres://db.test/library", Cap::NONE),
        None
    );
    // A database file is its store's to price once open.
    let db = dir.join("library.db");
    std::fs::write(&db, b"SQLite format 3\0").unwrap();
    assert_eq!(quarb_open::catalog(db.to_str().unwrap(), Cap::NONE), None);
    assert_eq!(
        quarb_open::catalog(dir.join("missing.txt").to_str().unwrap(), Cap::NONE),
        None
    );
}

#[test]
fn a_cap_stops_the_listing_past_the_budget() {
    let dir = scratch();
    let shelf = dir.join("shelf");
    let cap = Cap {
        nodes: Some(1),
        bytes: None,
    };
    let c = quarb_open::catalog(shelf.to_str().unwrap(), cap).unwrap();
    assert_eq!(c.entries, (2, u64::MAX));
    assert_eq!(c.confidence, Confidence::Bound);
    let cap = Cap {
        nodes: None,
        bytes: Some(10),
    };
    let c = quarb_open::catalog(shelf.to_str().unwrap(), cap).unwrap();
    assert_eq!(c.entries.1, u64::MAX);
    assert!(c.bytes.unwrap().0 > 10);
}

#[test]
fn a_path_under_the_directory_narrows_the_listing() {
    let dir = scratch();
    let shelf = dir.join("shelf");
    let c = quarb_open::catalog_at(shelf.to_str().unwrap(), &["verse"], Cap::NONE).unwrap();
    assert_eq!(c.entries, (1, 1));
    assert_eq!(c.bytes, Some((38, 38)));
    let c = quarb_open::catalog_at(shelf.to_str().unwrap(), &["emma.txt"], Cap::NONE).unwrap();
    assert_eq!(c.entries, (1, u64::MAX));
    assert_eq!(c.bytes, Some((42, 42)));
    assert_eq!(
        quarb_open::catalog_at(shelf.to_str().unwrap(), &["prose"], Cap::NONE),
        None
    );
    assert_eq!(
        quarb_open::catalog_at(shelf.to_str().unwrap(), &[".."], Cap::NONE),
        None
    );
    let tar = dir.join("books.tar.gz");
    let c = quarb_open::catalog_at(
        &format!("text:{}", tar.display()),
        &["verse", "iliad.txt"],
        Cap::NONE,
    )
    .unwrap();
    assert_eq!(c.bytes, Some((38, 38)));
}

#[test]
fn a_listing_applies_size_and_kind_filters() {
    use quarb::{Ask, Cmp, Conjunct, Value};
    let dir = scratch();
    let shelf = dir.join("shelf");
    // A size is a quantity: the bound says its unit (spec: Relations).
    let forty = Value::bytes(40);
    let ask = Ask {
        scope: &[],
        cap: Cap::NONE,
        conjuncts: &[Conjunct::Compare("size", Cmp::Gt, &forty)],
        traits: &["file"],
    };
    // emma.txt (42 bytes) alone passes; the directory and the two
    // shorter files do not.
    let c = quarb_open::catalog_for(shelf.to_str().unwrap(), &ask).unwrap();
    assert_eq!(c.entries, (1, 1));
    assert_eq!(c.bytes, Some((42, 42)));
    assert_eq!(c.applied, vec![true]);
    assert!(c.traits_applied);
    // A property the listing does not have stays the engine's.
    let ask = Ask {
        scope: &[],
        cap: Cap::NONE,
        conjuncts: &[Conjunct::Compare("mood", Cmp::Gt, &forty)],
        traits: &["dir"],
    };
    let c = quarb_open::catalog_for(shelf.to_str().unwrap(), &ask).unwrap();
    assert_eq!(c.entries, (1, 1));
    assert_eq!(c.applied, vec![false]);
    // The same sieve over the archive's member table.
    let tar = dir.join("books.tar.gz");
    let ask = Ask {
        scope: &[],
        cap: Cap::NONE,
        conjuncts: &[Conjunct::Compare("size", Cmp::Gt, &forty)],
        traits: &[],
    };
    let c = quarb_open::catalog_for(tar.to_str().unwrap(), &ask).unwrap();
    assert_eq!(c.entries, (1, 1));
    assert_eq!(c.bytes, Some((42, 42)));
}

#[test]
fn a_listing_counts_what_the_mount_exposes() {
    let dir = scratch();
    let shelf = dir.join("shelf");
    // An ignore file covers a subtree and a file; a hidden note
    // and the ignore file itself are hidden entries.
    std::fs::write(shelf.join(".gitignore"), "verse/\npersuasion.txt\n").unwrap();
    std::fs::write(shelf.join(".notes"), "not a book").unwrap();
    let size = |n: &str| std::fs::metadata(shelf.join(n)).unwrap().len();
    // Listed as the mount walks it: emma.txt alone.
    let c = quarb_open::catalog(shelf.to_str().unwrap(), Cap::NONE).unwrap();
    assert_eq!(c.entries, (1, 1));
    assert_eq!(c.bytes, Some((size("emma.txt"), size("emma.txt"))));
    assert_eq!(c.confidence, Confidence::Exact);
    // The reading prefix changes nothing: the folder is the same.
    let c = quarb_open::catalog(&format!("text:{}", shelf.display()), Cap::NONE).unwrap();
    assert_eq!(c.entries, (1, 1));
    // Listed as `--hidden --no-ignore` mounts it: every entry.
    let all = quarb_fs::FsOptions {
        hidden: true,
        respect_ignore: false,
    };
    let c = quarb_open::catalog_seen(shelf.to_str().unwrap(), Cap::NONE, all).unwrap();
    assert_eq!(c.entries, (6, 6));
    let total = size("emma.txt")
        + size("persuasion.txt")
        + size("verse/iliad.txt")
        + size(".gitignore")
        + size(".notes");
    assert_eq!(c.bytes, Some((total, total)));
    // Hidden entries shown, the ignore file still honoured.
    let shown = quarb_fs::FsOptions {
        hidden: true,
        respect_ignore: true,
    };
    let c = quarb_open::catalog_seen(shelf.to_str().unwrap(), Cap::NONE, shown).unwrap();
    assert_eq!(c.entries, (3, 3));
}
