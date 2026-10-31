//! A reference declared by a property labels its edge by the
//! property's name without a trailing `_id` (or camel-case `Id`):
//! the column `manager_id` holds the value, the edge it declares is
//! `manager` (spec: A table with foreign keys). A model `ref` on a
//! column the catalog already declares refines that one edge, and
//! `ref … as NAME` names the edge.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-reflabel-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("org.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT NOT NULL,
           manager_id INTEGER REFERENCES employees(id));
         INSERT INTO employees VALUES
           (1, 'Ada', NULL), (2, 'Bo', 1), (3, 'Cy', 1), (4, 'Dee', 2), (5, 'Eve', 2), (6, 'Fay', 4);
         CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT NOT NULL,
           authorId INTEGER REFERENCES employees(id),
           editor INTEGER REFERENCES employees(id),
           editor_id INTEGER REFERENCES employees(id));
         INSERT INTO books VALUES (1, 'Emma', 2, 3, 4);",
    )
    .unwrap();
    std::fs::write(
        dir.join("org.model"),
        "ref /employees/*::manager_id --> /employees/*[::id = $] acyclic;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("boss.model"),
        "ref /employees/*::manager_id --> /employees/*[::id = $] as boss acyclic;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("figures.json"),
        r#"{"figures": [{"id": 1, "name": "Gaia", "mother_id": null},
                       {"id": 2, "name": "Uranus", "mother_id": 1},
                       {"id": 3, "name": "Cronus", "mother_id": 1}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("figures.model"),
        "ref /figures/*::mother_id --> /figures/*[::id = $] as mother;\n",
    )
    .unwrap();
    dir
}

fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn lines(dir: &Path, args: &[&str]) -> Vec<String> {
    let (ok, out, err) = run(dir, args);
    assert!(ok, "{args:?}: {err}");
    out.lines().map(str::to_string).collect()
}

fn qua(dir: &Path, query: &str) -> Vec<String> {
    lines(dir, &[query, "org.db"])
}

fn qua_model(dir: &Path, model: &str, query: &str) -> Vec<String> {
    lines(dir, &["--model", model, query, "org.db"])
}

#[test]
fn a_foreign_key_labels_its_edge_without_the_suffix() {
    let dir = fixture("fk");
    assert_eq!(qua(&dir, "/employees/*->manager @| count"), ["5"]);
    assert_eq!(
        qua(&dir, "/employees/6(->manager)+::name"),
        ["Dee", "Bo", "Ada"]
    );
    assert_eq!(qua(&dir, "/employees/1<-manager::name"), ["Bo", "Cy"]);
    // the value keeps its column name; the edge accessor says the
    // label, on the hop and on the resolution alike
    assert_eq!(qua(&dir, "/employees/6::manager_id"), ["4"]);
    assert_eq!(
        qua(&dir, "/employees/*->*[$- = \"manager\"] @| count"),
        ["5"]
    );
    assert_eq!(
        qua(&dir, "/employees/*->*[$- = \"manager_id\"] @| count"),
        ["0"]
    );
    assert_eq!(
        qua(
            &dir,
            "/employees/6::manager_id-->[$- = \"manager\"] @| count"
        ),
        ["1"]
    );
    // the column's full name no longer names an edge
    assert_eq!(qua(&dir, "/employees/*->manager_id @| count"), ["0"]);
    // the reverse resolution still reads the property
    assert_eq!(
        qua(&dir, "/employees/1<--(/employees/*::manager_id)::name"),
        ["Bo", "Cy"]
    );
    // the edges table says the label
    let table = qua(&dir, "/employees/* @| edges(->*)").join("\n");
    assert!(table.contains("\"manager\""), "{table}");
    assert!(!table.contains("manager_id"), "{table}");
}

#[test]
fn camel_case_and_collisions() {
    let dir = fixture("camel");
    // `authorId` → `author`; `editor` keeps its name and the later
    // column `editor_id`, which would share it, keeps its full name
    assert_eq!(qua(&dir, "/books/1->author::name"), ["Bo"]);
    assert_eq!(qua(&dir, "/books/1->editor::name"), ["Cy"]);
    assert_eq!(qua(&dir, "/books/1->editor_id::name"), ["Dee"]);
    assert_eq!(qua(&dir, "/books/1->*[$- = \"author\"]::name"), ["Bo"]);
    assert_eq!(qua(&dir, "/books/1->*[$- = \"editor\"]::name"), ["Cy"]);
    assert_eq!(qua(&dir, "/books/1->*[$- = \"editor_id\"]::name"), ["Dee"]);
    assert_eq!(qua(&dir, "/books/1->authorId @| count"), ["0"]);
}

#[test]
fn a_model_ref_on_a_declared_column_refines_the_one_edge() {
    let dir = fixture("model");
    // one edge per parent, under the catalog's label, with the
    // promise on it: the graph is the same with the model
    assert_eq!(
        qua_model(&dir, "org.model", "/employees/* @| edges(->*)"),
        qua(&dir, "/employees/* @| edges(->*)")
    );
    assert_eq!(
        qua_model(&dir, "org.model", "/employees/*(->manager)+ @| count"),
        qua(&dir, "/employees/*(->manager)+ @| count")
    );
    assert_eq!(
        qua_model(&dir, "org.model", "/employees/6(->manager)+::name"),
        ["Dee", "Bo", "Ada"]
    );
    assert_eq!(
        qua_model(&dir, "org.model", "/employees/*->employees @| count"),
        ["0"]
    );
    // the promise is the catalog edge's: a cycle under it refuses
    let db = rusqlite::Connection::open(dir.join("org.db")).unwrap();
    db.execute_batch("UPDATE employees SET manager_id = 6 WHERE id = 1;")
        .unwrap();
    let (ok, _, err) = run(
        &dir,
        &[
            "--model",
            "org.model",
            "/employees/6(->manager)+::name",
            "org.db",
        ],
    );
    assert!(!ok, "a cycle under the promise should refuse");
    assert!(err.contains("a cycle under the promise"), "{err}");
}

#[test]
fn a_pushed_down_prefilter_keeps_every_edge() {
    // An equality predicate on the scanned table is pushed down as a
    // prefilter: the table's LISTING is narrowed to the matching rows.
    // The model's fabric indexes a ref's target by listing it, so a
    // narrowed listing would drop every edge whose far end the
    // predicate excluded; the fabric asks for whole listings instead.
    let dir = fixture("prefilter");
    assert_eq!(
        qua_model(
            &dir,
            "org.model",
            "/employees/*[::name = \"Dee\"]->manager::name"
        ),
        ["Bo"]
    );
    assert_eq!(
        qua_model(
            &dir,
            "org.model",
            "/employees/*[::name = \"Bo\"]<-manager @| count"
        ),
        ["2"]
    );
    assert_eq!(
        qua_model(&dir, "org.model", "/employees/*[::id = 1]<-manager::name"),
        ["Bo", "Cy"]
    );
    // The catalog's own edge too (no model): the hop into the table
    // goes by key beside the prefilter.
    assert_eq!(
        qua(&dir, "/employees/*[::name = \"Dee\"]->manager::name"),
        ["Bo"]
    );
    assert_eq!(
        qua(&dir, "/employees/*[::name = \"Bo\"]<-manager @| count"),
        ["2"]
    );
    // A bare path predicate is an existence test: the walk inside
    // expands by node, and answers the same.
    assert_eq!(
        qua_model(
            &dir,
            "org.model",
            "/employees/*[(->manager)+[::name = \"Ada\"]] @| count"
        ),
        ["5"]
    );
}

#[test]
fn a_ref_as_name_renames_the_edge() {
    let dir = fixture("as");
    let boss = |q: &str| qua_model(&dir, "boss.model", q);
    // on a column the catalog declares, `as NAME` renames that one
    // edge: `boss` both ways, `manager` gone, the promise on `boss`
    let table = boss("/employees/* @| edges(->*)").join("\n");
    assert!(table.contains("\"boss\""), "{table}");
    // no edge under the old label (the `manager_id` column is among
    // the edge's own properties: a ref's edge is its source row)
    assert!(!table.contains("\"manager\""), "{table}");
    assert_eq!(boss("/employees/*->* @| count"), ["5"]);
    assert_eq!(boss("/employees/6(->boss)+::name"), ["Dee", "Bo", "Ada"]);
    assert_eq!(boss("/employees/1<-boss::name"), ["Bo", "Cy"]);
    assert_eq!(boss("/employees/*->manager @| count"), ["0"]);
    assert_eq!(boss("/employees/6->*[$- = \"boss\"] @| count"), ["1"]);
    assert_eq!(boss("/employees/6::manager_id-->::name"), ["Dee"]);
    let db = rusqlite::Connection::open(dir.join("org.db")).unwrap();
    db.execute_batch("UPDATE employees SET manager_id = 6 WHERE id = 1;")
        .unwrap();
    let (ok, _, err) = run(
        &dir,
        &[
            "--model",
            "boss.model",
            "/employees/6(->boss)+::name",
            "org.db",
        ],
    );
    assert!(!ok, "a cycle under the promise should refuse");
    assert!(err.contains("a cycle under the promise"), "{err}");
    // on a document with no declared reference the ref declares
    // the edge under NAME instead of the role (the array's children
    // are numbered from 0: /figures/0 is Gaia)
    let fig = |q: &str| lines(&dir, &["--model", "figures.model", q, "figures.json"]);
    assert_eq!(fig("/figures/1->mother::name"), ["Gaia"]);
    assert_eq!(fig("/figures/0<-mother::name"), ["Uranus", "Cronus"]);
    assert_eq!(fig("/figures/1->*[$- = \"mother\"] @| count"), ["1"]);
    assert_eq!(fig("/figures/1->figures @| count"), ["0"]);
    assert_eq!(fig("/figures/*->* @| count"), ["2"]);
}
