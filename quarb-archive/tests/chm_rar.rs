//! CHM through libchm (a found file: PyWin32.chm from the pywin32
//! wheel, read from `tests/fixtures/` or `$QUARB_CHM_FIXTURE`,
//! the test standing down when neither holds it — 2.6 MB is not
//! checked in) and RAR behind the `rar` feature (a tiny archive
//! from the unrar crate's own test data).

use quarb::{AstAdapter, QueryResult};
use quarb_archive::ArchiveAdapter;

fn values<A: AstAdapter>(a: &A, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| a.name(*n).unwrap_or_default()).collect(),
    }
}

fn chm_fixture() -> Option<std::path::PathBuf> {
    let local = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/PyWin32.chm");
    if local.exists() {
        return Some(local);
    }
    std::env::var_os("QUARB_CHM_FIXTURE")
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
}

#[test]
fn a_chm_is_an_archive_of_pages() {
    let Some(path) = chm_fixture() else {
        eprintln!(
            "no CHM fixture (tests/fixtures/PyWin32.chm or $QUARB_CHM_FIXTURE): standing down"
        );
        return;
    };
    let a = ArchiveAdapter::open(&path).unwrap();
    let n: usize = values(&a, "//*<file> @| count")[0].parse().unwrap();
    assert!(n > 1000, "{n} members");
    // The system records are not members; the sitemap is.
    assert_eq!(values(&a, "/*[:::name == (/^[#$]/)] @| count"), ["0"]);
    assert_eq!(values(&a, "//*[:::name == (/\\.hhc$/)] @| count"), ["1"]);
    // An HTML page reads as HTML through the compose graft.
    let a = quarb_compose::ComposeAdapter::new(ArchiveAdapter::open(&path).unwrap());
    let titles = values(&a, "//*[:::name == (/\\.html$/)][1]//title::");
    assert_eq!(titles.len(), 1, "{titles:?}");
    assert!(!titles[0].is_empty());
}

#[test]
fn a_rar_is_read_only_with_the_feature() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unicode.rar");
    let opened = ArchiveAdapter::open(&path);
    if cfg!(feature = "rar") {
        let a = opened.unwrap();
        let names = values(&a, "//*<file> | :::name");
        assert!(!names.is_empty(), "{names:?}");
    } else {
        let e = opened.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("`rar` feature"), "{e}");
    }
}
