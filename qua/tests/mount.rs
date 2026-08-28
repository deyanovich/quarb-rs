//! The multi-mount flow carries the same JSON-column graft as the
//! single-input flow: `open_target` wraps relational adapters in
//! ComposeAdapter (a regression — mounts once returned the column
//! text flat while the single input grafted it).

use qua::{OpenOpts, open_target};
use quarb::{AstAdapter, NodeId, Value};

/// Forward the boxed mount through the trait (the session tools'
/// Dyn wrapper, locally).
struct Dyn(Box<dyn AstAdapter>);

impl AstAdapter for Dyn {
    fn root(&self) -> NodeId {
        self.0.root()
    }
    fn children(&self, n: NodeId) -> Vec<NodeId> {
        self.0.children(n)
    }
    fn name(&self, n: NodeId) -> Option<String> {
        self.0.name(n)
    }
    fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.0.parent(n)
    }
    fn property(&self, n: NodeId, key: &str) -> Option<Value> {
        self.0.property(n, key)
    }
    fn default_value(&self, n: NodeId) -> Option<Value> {
        self.0.default_value(n)
    }
    fn traits(&self, n: NodeId) -> Vec<String> {
        self.0.traits(n)
    }
}

#[test]
fn mounted_sqlite_grafts_json_columns() {
    let dir = std::env::temp_dir().join("qua-mount-graft-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("g.db");
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"CREATE TABLE s (id INTEGER PRIMARY KEY, meta TEXT);
             INSERT INTO s VALUES
               (1,'{"device":"phone","geo":{"city":"London"}}'),
               (2,'{"device":"laptop","geo":{"city":"Berlin"}}');"#,
        )
        .unwrap();
    }
    let (adapter, _loc) =
        open_target(path.to_str().unwrap(), &OpenOpts::default()).expect("mount");
    let adapter = Dyn(adapter);
    let run = |q: &str| -> Vec<String> {
        match quarb::run(q, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            _ => panic!("expected values"),
        }
    };
    // Navigation into the column, a filter over it, and dual
    // exposure at the graft root — all through the mount door.
    assert_eq!(run("/s/1/meta/geo/city::"), ["London"]);
    assert_eq!(run("/s/*[/meta/geo/city:: = 'Berlin']::id"), ["2"]);
    assert_eq!(run("/s/2/meta::device"), ["laptop"]);
}

/// The graft rename's semantics, through the same mount door:
/// `graft` opts a directory in, `no_graft` holds every boundary
/// opaque, and `code:` refuses the contradiction.
fn run_q(adapter: &Dyn, q: &str) -> Vec<String> {
    match quarb::run(q, adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        _ => panic!("expected values"),
    }
}

#[test]
fn dir_mount_grafts_only_with_graft() {
    let dir = std::env::temp_dir().join("qua-mount-dir-graft-test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cfg.json"), r#"{"port": 8080}"#).unwrap();

    let (a, _) = open_target(dir.to_str().unwrap(), &OpenOpts::default()).expect("mount");
    assert!(run_q(&Dyn(a), "/cfg.json/port::").is_empty(), "bare dir must not graft");

    let opts = OpenOpts { graft: true, ..OpenOpts::default() };
    let (a, _) = open_target(dir.to_str().unwrap(), &opts).expect("mount");
    assert_eq!(run_q(&Dyn(a), "/cfg.json/port::"), ["8080"]);
}

#[test]
fn archive_no_graft_holds_members_opaque() {
    use std::io::Write;
    let dir = std::env::temp_dir().join("qua-mount-archive-graft-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.zip");
    let f = std::fs::File::create(&path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = zip::write::SimpleFileOptions::default();
    z.start_file("cfg.json", o).unwrap();
    z.write_all(br#"{"port": 8080}"#).unwrap();
    z.finish().unwrap();

    // Composed by default: the member's parsed tree grafts.
    let (a, _) = open_target(path.to_str().unwrap(), &OpenOpts::default()).expect("mount");
    assert_eq!(run_q(&Dyn(a), "/cfg.json/port::"), ["8080"]);

    // no_graft: the tar -t view — members listed, sized, opaque.
    let opts = OpenOpts { no_graft: true, ..OpenOpts::default() };
    let (a, _) = open_target(path.to_str().unwrap(), &opts).expect("mount");
    let a = Dyn(a);
    assert!(run_q(&a, "/cfg.json/port::").is_empty(), "no_graft must not cross the boundary");
    assert_eq!(run_q(&a, "/cfg.json::::size").len(), 1, "the member stays listable and sizable");
}

#[test]
fn sqlite_no_graft_keeps_columns_flat() {
    let dir = std::env::temp_dir().join("qua-mount-no-graft-sql-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("g.db");
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"CREATE TABLE s (id INTEGER PRIMARY KEY, meta TEXT);
             INSERT INTO s VALUES (1,'{"device":"phone","geo":{"city":"London"}}');"#,
        )
        .unwrap();
    }
    let opts = OpenOpts { no_graft: true, ..OpenOpts::default() };
    let (a, _) = open_target(path.to_str().unwrap(), &opts).expect("mount");
    let a = Dyn(a);
    // The column stays the server's own scalar: no inner arbor,
    // the text intact.
    assert!(run_q(&a, "/s/1/meta/geo/city::").is_empty());
    assert_eq!(run_q(&a, "/s/1/meta::"), [r#"{"device":"phone","geo":{"city":"London"}}"#]);
}

#[test]
fn no_graft_refuses_code_prefix() {
    let dir = std::env::temp_dir().join("qua-mount-no-graft-code-test");
    std::fs::create_dir_all(&dir).unwrap();
    let target = format!("code:{}", dir.display());
    let opts = OpenOpts { no_graft: true, ..OpenOpts::default() };
    let err = match open_target(&target, &opts) {
        Err(e) => e,
        Ok(_) => panic!("must refuse"),
    };
    assert!(err.to_string().contains("code:"), "unexpected error: {err}");
}
