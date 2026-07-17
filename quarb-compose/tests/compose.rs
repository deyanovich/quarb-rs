//! Composition over a filesystem fixture.
use quarb_compose::ComposeAdapter;
use quarb_fs::{FsAdapter, FsOptions};

fn fixture(name: &str) -> std::path::PathBuf {
    // A unique dir per test: the two tests run in parallel, so a shared
    // dir would let one's `remove_dir_all` wipe the other mid-read.
    let dir = std::env::temp_dir().join(format!("quarb-compose-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("store.json"),
        r#"{"books": [{"t": "Dune", "p": 9}, {"t": "Emma", "p": 7}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("names.csv"), "name,qty\nAda,2\nBo,1\n").unwrap();
    std::fs::write(dir.join("plain.txt"), "not a tree").unwrap();
    dir
}

fn values(a: &impl quarb::AstAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{:?}", n)).collect(),
    }
}

#[test]
fn grafts_parse_lazily_and_compose() {
    let dir = fixture("lazy");
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    // Through the boundary: fs path, then json path.
    assert_eq!(values(&a, "/store.json/books/*[/p:: < 8]/t::"), ["Emma"]);
    // CSV grafts too.
    assert_eq!(values(&a, "/names.csv/*[::qty > 1]::name"), ["Ada"]);
    // A plain text file stays a leaf.
    assert_eq!(values(&a, "/plain.txt/* @| count"), ["0"]);
    assert_eq!(values(&a, "/plain.txt::"), ["not a tree"]);
    // Inner parents climb back out to the outer tree.
    assert_eq!(
        values(&a, "/store.json/books\\\\store.json/books/*/t:: @| count"),
        ["2"]
    );
}

/// Regression: grafted node ids must stay inside the low 56 bits so
/// they survive being packed into a `MountAdapter`, which reserves the
/// high byte (bits 56–63) for the mount index. A graft tag at bit 63
/// spilled into that byte — corrupting the mount index and panicking
/// when a mount wrapped an archive/bucket `ComposeAdapter`. The tag now
/// lives at bit 55, inside the inner window.
#[test]
fn grafted_ids_fit_the_mount_inner_window() {
    let dir = fixture("mount-window");
    let a = ComposeAdapter::new(FsAdapter::with_options(&dir, FsOptions::default()).unwrap());
    match quarb::run("/store.json/books/*", &a).unwrap() {
        quarb::QueryResult::Nodes(ns) => {
            assert!(!ns.is_empty(), "expected grafted book nodes");
            for n in ns {
                assert_eq!(
                    n.0 >> 56,
                    0,
                    "grafted id {:#x} escapes the 56-bit mount inner window",
                    n.0
                );
            }
        }
        quarb::QueryResult::Values(_) => panic!("expected nodes"),
    }
}
