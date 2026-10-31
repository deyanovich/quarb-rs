//! Draft 175: the navop star. `~name` walks every edge touching
//! the position — a tree edge or a crosslink, either direction —
//! and keeps the far ends whose edge matches the name: `~x` ≡
//! `(/x|\x|->x|<-x)`, `~*` every edge. `~~name` is the deep form,
//! the walk over every edge ending at a matching one, with the
//! reach marks of `//`. Siblings are an order, not edges. On the
//! org chart (the recipes' fixture): a row's edges are its cells,
//! its table and its manager link.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-navstar-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("org.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE employees (
           id INTEGER PRIMARY KEY, name TEXT NOT NULL, title TEXT NOT NULL,
           manager_id INTEGER REFERENCES employees(id));
         INSERT INTO employees VALUES
           (1, 'Ada', 'CEO', NULL), (2, 'Bo', 'VP Eng', 1), (3, 'Cy', 'VP Sales', 1),
           (4, 'Dee', 'Eng Manager', 2), (5, 'Eli', 'Sales Rep', 3),
           (6, 'Fay', 'Engineer', 4), (7, 'Gus', 'Engineer', 4);",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, query: &str) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args([query, "org.db"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{query}: {err}");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_star_walks_every_edge_of_a_row() {
    let dir = fixture("star");
    // Fay's edges: her four cells, her table, her manager.
    assert_eq!(
        qua(&dir, "/employees/6~*"),
        [
            "/employees/6/id",
            "/employees/6/name",
            "/employees/6/title",
            "/employees/6/manager_id",
            "/employees",
            "/employees/4"
        ]
    );
    // The name selects by the edge: the cell is named manager_id,
    // the link it declares is labeled manager (the column without
    // its suffix), so each name answers its own edge; `$-` tells
    // them apart, a tree edge reporting its structural label.
    assert_eq!(qua(&dir, "/employees/6~manager"), ["/employees/4"]);
    assert_eq!(
        qua(&dir, "/employees/6~manager_id"),
        ["/employees/6/manager_id"]
    );
    assert_eq!(qua(&dir, "/employees/6~*[$- = \"manager\"]::name"), ["Dee"]);
    assert_eq!(
        qua(&dir, "/employees/6~*[$- = \"[parent]\"]"),
        ["/employees"]
    );
    // Either direction: Dee's manager link reaches Bo above and
    // her reports below.
    assert_eq!(
        qua(&dir, "/employees/4~*[$- = \"manager\"]::name"),
        ["Bo", "Fay", "Gus"]
    );
}

#[test]
fn the_star_quantified_is_a_plain_walk() {
    let dir = fixture("walk");
    // Every walk is a thread (spec: Expansion paths): the walks
    // over every edge from employee 6 number 383; collapsed, every
    // node but the start — the table, the root, the rows and their
    // cells — once.
    assert_eq!(qua(&dir, "/employees/6(~*)+ @| count"), ["383"]);
    assert_eq!(qua(&dir, "/employees/6(~*)+ @| collapse @| count"), ["35"]);
    // The nearest Ada is two hops away through the table, not
    // three up the manager chain.
    assert_eq!(
        qua(&dir, "/employees/6(~*)+[::name = \"Ada\"]?"),
        ["/employees/1"]
    );
    assert_eq!(
        qua(&dir, "/employees/6(~*){2}[::name = \"Ada\"]"),
        ["/employees/1"]
    );
    // Along manager edges alone Ada is three hops up.
    assert_eq!(
        qua(&dir, "/employees/6(~manager){2}[::name = \"Ada\"]"),
        Vec::<String>::new()
    );
    assert_eq!(
        qua(&dir, "/employees/6(~manager){3}[::name = \"Ada\"]"),
        ["/employees/1"]
    );
}

#[test]
fn the_deep_star_ends_at_a_matching_edge() {
    let dir = fixture("deep");
    // The walk over every edge, then the matching hop: the nearest
    // manager edge is Fay's own link, the nearest manager_id edge
    // her own cell.
    assert_eq!(qua(&dir, "/employees/6~~?manager"), ["/employees/4"]);
    assert_eq!(
        qua(&dir, "/employees/6~~?manager_id"),
        ["/employees/6/manager_id"]
    );
    // All of them: every employee is an end of a manager link; the
    // six manager_id cells (Ada's is null).
    assert_eq!(qua(&dir, "/employees/6~~manager @| count"), ["7"]);
    assert_eq!(qua(&dir, "/employees/6~~manager_id @| count"), ["6"]);
    // The canonical text keeps the operator and its reach.
    assert_eq!(
        qua(&dir, "/employees/6~~!manager @| count"),
        qua(&dir, "/employees/6~~!manager @| count")
    );
}
