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
    fn has_trait(&self, n: NodeId, name: &str) -> bool {
        self.0.has_trait(n, name)
    }
    fn metadata(&self, n: NodeId, key: &str) -> Option<Value> {
        self.0.metadata(n, key)
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
            r#"CREATE TABLE s (id INTEGER PRIMARY KEY, meta JSON, note TEXT);
             INSERT INTO s VALUES
               (1,'{"device":"phone","geo":{"city":"London"}}','{"geo":{"city":"Paris"}}'),
               (2,'{"device":"laptop","geo":{"city":"Berlin"}}','{"geo":{"city":"Rome"}}');"#,
        )
        .unwrap();
    }
    let adapter = open_target(path.to_str().unwrap(), &OpenOpts::default())
        .expect("mount")
        .adapter;
    let adapter = Dyn(adapter);
    let run = |q: &str| -> Vec<String> {
        match quarb::run(q, &adapter).unwrap() {
            quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
            _ => panic!("expected values"),
        }
    };
    // Navigation into the column, a filter over it, and dual
    // exposure at the graft root — all through the mount door. The
    // column typed JSON declares its cells' notation (spec:
    // Grafting).
    assert_eq!(run("/s/1/meta/geo/city::"), ["London"]);
    assert_eq!(run("/s/*[/meta/geo/city:: = \"Berlin\"]::id"), ["2"]);
    assert_eq!(run("/s/2/meta::device"), ["laptop"]);
    // A TEXT column full of JSON declares nothing: its cell is a
    // leaf, a path through it is empty; the query's `<json>` at the
    // hop reads it.
    assert_eq!(run("/s/1/note/geo/city::"), Vec::<String>::new());
    assert_eq!(run("/s/1/note::"), [r#"{"geo":{"city":"Paris"}}"#]);
    assert_eq!(run("/s/1/note<json>/geo/city::"), ["Paris"]);
    assert_eq!(run("/s/*/note<json>/geo/city::"), ["Paris", "Rome"]);
}

/// `?doc=PATH` on a reading prefix over a source that is no directory
/// (spec: The Corpus Reading): each leaf the path reaches is a
/// document, its text read as prose with the corpus tokens, the row's
/// other columns riding as its declarations.
#[test]
fn rows_read_as_documents_with_doc() {
    let dir = std::env::temp_dir().join("qua-mount-doc-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("blog.db");
    let _ = std::fs::remove_file(&path);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT, author TEXT, body TEXT);
             INSERT INTO posts VALUES
               (1,'Ithaca','Homer','Tell me, Muse, of the man of many ways. He wandered far.'),
               (2,'Troy','Homer','Sing, goddess, the anger of Achilles.');"#,
        )
        .unwrap();
    }
    let opts = OpenOpts::default();
    let target = format!("corpus:{}?doc=posts/*/body", path.display());
    let a = Dyn(open_target(&target, &opts).expect("mount").adapter);
    assert_eq!(run_q(&a, "/posts/*/body:::title"), ["Ithaca", "Troy"]);
    assert_eq!(run_q(&a, "/posts/1/body:::author"), ["Homer"]);
    assert_eq!(run_q(&a, "/posts/1/body//sentence @| count"), ["2"]);
    assert_eq!(run_q(&a, "/posts/2/body//token @| count"), ["9"]);
    assert_eq!(run_q(&a, "/posts/*/title::"), ["Ithaca", "Troy"]);
    // the text reading, without tokens
    let t = Dyn(
        open_target(&format!("text:{}?doc=posts/*/body", path.display()), &opts)
            .expect("mount")
            .adapter,
    );
    assert_eq!(
        run_q(&t, "/posts/2/body//paragraph::"),
        ["Sing, goddess, the anger of Achilles."]
    );
    assert_eq!(run_q(&t, "/posts/2/body//token @| count"), ["0"]);
    // a path reaching no leaf is refused
    let err = open_target(
        &format!("corpus:{}?doc=posts/*/nothing", path.display()),
        &opts,
    )
    .err()
    .map(|e| e.to_string())
    .unwrap_or_default();
    assert!(err.contains("names no leaf"), "{err}");
}

/// A leaf grafts only as declared (spec: Grafting): by its name's
/// extension, by what its bytes say of themselves (an XML
/// declaration, a DOCTYPE), by the mount's `?format=`, or by the
/// query's `<NOTATION>` at the hop. A nameless leaf holding JSON
/// declares nothing and stays a leaf.
#[test]
fn a_leaf_grafts_only_as_declared() {
    let dir = std::env::temp_dir().join("qua-mount-declared-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.json"), r#"{"k":"named"}"#).unwrap();
    std::fs::write(dir.join("b"), r#"{"k":"bare"}"#).unwrap();
    std::fs::write(
        dir.join("c"),
        "<?xml version=\"1.0\"?><root><x>decl</x></root>",
    )
    .unwrap();
    std::fs::write(
        dir.join("d"),
        "<!DOCTYPE html><html><body><p>doc</p></body></html>",
    )
    .unwrap();
    std::fs::write(dir.join("e"), "<root><x>guess</x></root>").unwrap();
    let opts = OpenOpts {
        graft: true,
        ..OpenOpts::default()
    };
    let mount = |target: &str| Dyn(open_target(target, &opts).expect("mount").adapter);
    let a = mount(dir.to_str().unwrap());
    assert_eq!(run_q(&a, "/a.json/k::"), ["named"]);
    assert_eq!(run_q(&a, "/b/k::"), Vec::<String>::new());
    assert_eq!(run_q(&a, "/b::"), [r#"{"k":"bare"}"#]);
    assert_eq!(run_q(&a, "/b<json>/k::"), ["bare"]);
    assert_eq!(run_q(&a, "/c/root/x::"), ["decl"]);
    assert_eq!(run_q(&a, "//p::"), ["doc"]);
    assert_eq!(run_q(&a, "/e/root/x::"), Vec::<String>::new());
    assert_eq!(run_q(&a, "/e<xml>/root/x::"), ["guess"]);
    // the mount's `?format=` covers every leaf beneath
    let j = mount(&format!("{}?format=json", dir.display()));
    assert_eq!(run_q(&j, "/b/k::"), ["bare"]);
    assert_eq!(run_q(&j, "/a.json/k::"), ["named"]);
    // a format no notation answers to is refused naming the list
    let err = open_target(&format!("{}?format=jsonx", dir.display()), &opts)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        err.contains("names no notation") && err.contains("json, jsonl"),
        "{err}"
    );
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

    let a = open_target(dir.to_str().unwrap(), &OpenOpts::default())
        .expect("mount")
        .adapter;
    assert!(
        run_q(&Dyn(a), "/cfg.json/port::").is_empty(),
        "bare dir must !graft"
    );

    let opts = OpenOpts {
        graft: true,
        ..OpenOpts::default()
    };
    let a = open_target(dir.to_str().unwrap(), &opts)
        .expect("mount")
        .adapter;
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
    let a = open_target(path.to_str().unwrap(), &OpenOpts::default())
        .expect("mount")
        .adapter;
    assert_eq!(run_q(&Dyn(a), "/cfg.json/port::"), ["8080"]);

    // no_graft: the tar -t view — members listed, sized, opaque.
    let opts = OpenOpts {
        no_graft: true,
        ..OpenOpts::default()
    };
    let a = open_target(path.to_str().unwrap(), &opts)
        .expect("mount")
        .adapter;
    let a = Dyn(a);
    assert!(
        run_q(&a, "/cfg.json/port::").is_empty(),
        "no_graft must !cross the boundary"
    );
    assert_eq!(
        run_q(&a, "/cfg.json:::size").len(),
        1,
        "the member stays listable && sizable"
    );
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
    let opts = OpenOpts {
        no_graft: true,
        ..OpenOpts::default()
    };
    let a = open_target(path.to_str().unwrap(), &opts)
        .expect("mount")
        .adapter;
    let a = Dyn(a);
    // The column stays the server's own scalar: no inner arbor,
    // the text intact.
    assert!(run_q(&a, "/s/1/meta/geo/city::").is_empty());
    assert_eq!(
        run_q(&a, "/s/1/meta::"),
        [r#"{"device":"phone","geo":{"city":"London"}}"#]
    );
}

#[test]
fn no_graft_refuses_code_prefix() {
    let dir = std::env::temp_dir().join("qua-mount-no-graft-code-test");
    std::fs::create_dir_all(&dir).unwrap();
    let target = format!("code:{}", dir.display());
    let opts = OpenOpts {
        no_graft: true,
        ..OpenOpts::default()
    };
    let err = match open_target(&target, &opts) {
        Err(e) => e,
        Ok(_) => panic!("must refuse"),
    };
    assert!(err.to_string().contains("code:"), "unexpected error: {err}");
}
