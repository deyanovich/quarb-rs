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
