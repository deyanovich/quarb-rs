//! The surface a source declares (the user's ruling of 2026-11-10 on
//! the outside review's first concern, a misspelled edge label or
//! property that was silent): where a source declares every property
//! its members have and every label that leaves or reaches them (a
//! table's columns and keys, a CSV's header), a hop, a predicate or
//! a projection that names what the surface lacks raises a warning
//! naming what is there. A source that declares nothing stays
//! quiet, and no answer changes.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-surface-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("org.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT, title TEXT,
                                 manager_id INTEGER REFERENCES employees(id));
         INSERT INTO employees VALUES (1,'Ada','Director',NULL),(2,'Bo','Lead',1),(3,'Cy',NULL,1);",
    )
    .unwrap();
    std::fs::write(dir.join("people.csv"), "name,age\nAda,36\nBo,\n").unwrap();
    std::fs::write(
        dir.join("people.json"),
        r#"{"people":[{"name":"Ada","age":36},{"name":"Bo"}]}"#,
    )
    .unwrap();
    dir
}

/// The answer, the warnings of the two codes, and whether it ran.
fn run(dir: &Path, args: &[&str]) -> (String, Vec<String>) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{args:?}: {err}");
    (
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        err.lines()
            .filter(|l| l.contains("UNKNOWN_LABEL") || l.contains("UNKNOWN_PROPERTY"))
            .map(str::to_string)
            .collect(),
    )
}

const NO_LABEL: &str = "warning UNKNOWN_LABEL (/employees): no edge is labelled 'manager_id' from \
                        /employees; the labels there: manager";
const NO_PROPERTY: &str = "warning UNKNOWN_PROPERTY (/employees): no property 'nosuch' on \
                           /employees; its properties: id, name, title, manager_id";

#[test]
fn a_table_warns_of_a_label_and_a_property_it_lacks() {
    let d = fixture("table");
    // the hop: the label is `manager`, the column's name without its
    // suffix; the answer is as empty as it was, and the exit is 0
    let (out, warned) = run(&d, &["/employees/*[2](->manager_id)+ | ::name", "org.db"]);
    assert_eq!(out, "");
    assert_eq!(warned, [NO_LABEL]);
    // the label is asked in every direction a hop walks
    for hop in ["<-manager_id", "--manager_id", "(->manager_id){1;3}"] {
        let q = format!("/employees/*[2]{hop} | ::name");
        let (out, warned) = run(&d, &[&q, "org.db"]);
        assert_eq!(out, "", "{hop}");
        assert_eq!(warned, [NO_LABEL], "{hop}");
    }
    // the navop wildcard is not asked: its name may be a child's or
    // the parent's, which are data (here the cell `manager_id`)
    let (out, warned) = run(&d, &["/employees/*[2]~manager_id | :name", "org.db"]);
    assert_eq!(out, "manager_id");
    assert!(warned.is_empty(), "{warned:?}");
    let (_, warned) = run(&d, &["/employees~99 | :name", "org.db"]);
    assert!(warned.is_empty(), "{warned:?}");
    // the projection and the predicate
    let (out, warned) = run(&d, &["/employees/* | ::nosuch", "org.db"]);
    assert_eq!(out, "null\nnull\nnull");
    assert_eq!(warned, [NO_PROPERTY]);
    let (out, warned) = run(&d, &["/employees/*[::nosuch = 1] @| count", "org.db"]);
    assert_eq!(out, "0");
    assert_eq!(warned, [NO_PROPERTY]);
    // once per name, however many rows read it
    let (_, warned) = run(
        &d,
        &[
            "/employees/* | %(::name; a = ::nosuch; b = ::nosuch)",
            "org.db",
        ],
    );
    assert_eq!(warned, [NO_PROPERTY]);

    // what the surface has is quiet: a label that reaches no node
    // here, a column whose cell is null
    let (out, warned) = run(&d, &["/employees/*[1]->manager | ::name", "org.db"]);
    assert_eq!(out, "");
    assert!(warned.is_empty(), "{warned:?}");
    let (out, warned) = run(&d, &["/employees/*[3] | ::title", "org.db"]);
    assert_eq!(out, "null");
    assert!(warned.is_empty(), "{warned:?}");
    // a label that points at the table is one of its labels
    let (out, warned) = run(&d, &["/employees/1<-manager | ::name", "org.db"]);
    assert_eq!(out, "Bo\nCy");
    assert!(warned.is_empty(), "{warned:?}");
    // the surface is a row's: the table node and a cell declare
    // nothing of their own
    let (_, warned) = run(&d, &["/employees | ::nosuch", "org.db"]);
    assert!(warned.is_empty(), "{warned:?}");
    let (_, warned) = run(&d, &["/employees/1/name | ::nosuch", "org.db"]);
    assert!(warned.is_empty(), "{warned:?}");
}

/// Under a mount the container is named as the query walks to it.
#[test]
fn a_mounted_source_is_named_from_the_mount() {
    let d = fixture("mount");
    let mounts = ["org=org.db", "people=people.csv"];
    let (out, warned) = run(&d, &["/org/employees/* | ::nosuch", mounts[0], mounts[1]]);
    assert_eq!(out, "null\nnull\nnull");
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_PROPERTY (/org/employees): no property 'nosuch' on /org/employees; \
          its properties: id, name, title, manager_id"
        ]
    );
    let (_, warned) = run(
        &d,
        &["/org/employees/*[2]->manager_id", mounts[0], mounts[1]],
    );
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_LABEL (/org/employees): no edge is labelled 'manager_id' from \
          /org/employees; the labels there: manager"
        ]
    );
    let (_, warned) = run(&d, &["/people/row | ::agee", mounts[0], mounts[1]]);
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_PROPERTY (/people/row): no property 'agee' on /people/row; \
          its properties: name, age"
        ]
    );
    // the same name on two containers is told of each, and the nodes
    // that are no members (the tables, the cells) say nothing
    let (out, warned) = run(
        &d,
        &["//*[::name] | ::nosuch @| count", mounts[0], mounts[1]],
    );
    assert_eq!(out, "5");
    assert_eq!(warned.len(), 2, "{warned:?}");
}

#[test]
fn a_csv_declares_its_columns_and_a_document_stays_quiet() {
    let d = fixture("open");
    let (out, warned) = run(&d, &["/row | ::agee", "people.csv"]);
    assert_eq!(out, "null\nnull");
    assert_eq!(
        warned,
        ["warning UNKNOWN_PROPERTY (/row): no property 'agee' on /row; its properties: name, age"]
    );
    let (_, warned) = run(&d, &[r#"/row[::nme = "Ada"] | ::name"#, "people.csv"]);
    assert_eq!(
        warned,
        ["warning UNKNOWN_PROPERTY (/row): no property 'nme' on /row; its properties: name, age"]
    );
    // an empty cell of a column it has; a label (the header says
    // nothing of what links the rows)
    let (out, warned) = run(&d, &["/row | ::age", "people.csv"]);
    assert_eq!(out, "36\nnull");
    assert!(warned.is_empty(), "{warned:?}");
    let (_, warned) = run(&d, &["/row->boss | ::name", "people.csv"]);
    assert!(warned.is_empty(), "{warned:?}");

    // JSON: a key absent from one object is ordinary data
    let (out, warned) = run(&d, &["/people/* | ::agee", "people.json"]);
    assert_eq!(out, "null\nnull");
    assert!(warned.is_empty(), "{warned:?}");
    let (_, warned) = run(&d, &["/people/*->boss", "people.json"]);
    assert!(warned.is_empty(), "{warned:?}");
}

/// Under a model the base's properties stand with the model's own,
/// and the labels are open: a model's edge is labeled as it
/// declares.
#[test]
fn a_model_adds_its_properties_and_opens_the_labels() {
    let d = fixture("model");
    std::fs::write(
        d.join("org.model"),
        "property /employees/*::shout = (::name | upper);\n",
    )
    .unwrap();
    let m = ["--model", "org.model"];
    let (out, warned) = run(&d, &[m[0], m[1], "/employees/*[1] | ::shout", "org.db"]);
    assert_eq!(out, "ADA");
    assert!(warned.is_empty(), "{warned:?}");
    let (_, warned) = run(&d, &[m[0], m[1], "/employees/*[1] | ::nosuch", "org.db"]);
    assert_eq!(warned.len(), 1);
    assert!(
        warned[0].contains("no property 'nosuch' on /employees")
            && warned[0].contains("manager_id, shout"),
        "{warned:?}"
    );
    let (_, warned) = run(&d, &[m[0], m[1], "/employees/*[2]->nosuch", "org.db"]);
    assert!(warned.is_empty(), "{warned:?}");

    // a property declared under a predicate is a property of the
    // table: the rows outside the predicate read it null, in silence
    std::fs::write(
        d.join("lead.model"),
        "property /employees/*[::title = \"Lead\"]::lead<bool> = true;\n",
    )
    .unwrap();
    let m = ["--model", "lead.model"];
    let (out, warned) = run(&d, &[m[0], m[1], "/employees/*[::lead] @| count", "org.db"]);
    assert_eq!(out, "1");
    assert!(warned.is_empty(), "{warned:?}");
    let (out, warned) = run(&d, &[m[0], m[1], "/employees/* | ::lead", "org.db"]);
    assert_eq!(out, "null\ntrue\nnull");
    assert!(warned.is_empty(), "{warned:?}");
}

/// The names there are listed whole up to sixteen; past that the
/// sixteen nearest to the name asked, and the count.
#[test]
fn a_long_list_of_names_is_cut_to_the_nearest() {
    let d = fixture("long");
    let header: Vec<String> = (1..=20).map(|i| format!("col{i:02}")).collect();
    std::fs::write(
        d.join("wide.csv"),
        format!("{}\n{}\n", header.join(","), "1,".repeat(19) + "1"),
    )
    .unwrap();
    let (_, warned) = run(&d, &["/row | ::col21", "wide.csv"]);
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].ends_with("… (20 in all)"), "{warned:?}");
    // the name asked, and the sixteen nearest of the twenty
    assert_eq!(warned[0].matches("col").count(), 1 + 16, "{warned:?}");
    // thirteen names are listed whole
    let header: Vec<String> = (1..=13).map(|i| format!("c{i:02}")).collect();
    std::fs::write(
        d.join("mid.csv"),
        format!("{}\n{}\n", header.join(","), "1,".repeat(12) + "1"),
    )
    .unwrap();
    let (_, warned) = run(&d, &["/row | ::c14", "mid.csv"]);
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].ends_with("c11, c12, c13"), "{warned:?}");
}

/// Two tables, one with a `name` column and one without.
fn two_tables(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-presence-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("two.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT, title TEXT,
                                 manager_id INTEGER REFERENCES employees(id));
         INSERT INTO employees VALUES (1,'Ada','Director',NULL),(2,'Bo','Lead',1),(3,'Cy',NULL,1);
         CREATE TABLE rooms (id INTEGER PRIMARY KEY, floor INTEGER);
         INSERT INTO rooms VALUES (10,2),(11,NULL);",
    )
    .unwrap();
    std::fs::write(
        dir.join("people.json"),
        r#"{"people":[{"name":"Ada","age":36},{"name":"Bo"}]}"#,
    )
    .unwrap();
    dir
}

const ROOMS_NO_NAME: &str = "warning UNKNOWN_PROPERTY (/rooms): no property 'name' on /rooms; \
                             its properties: id, floor";

/// A test of presence (the user's ruling of 2026-11-11): a predicate
/// that is a property alone, `[::name]` or `[!::name]`, asks whether
/// it is there. Its warning is kept and raised only when nothing the
/// test asked has the name; every other read warns at once.
#[test]
fn a_test_of_presence_is_told_of_only_when_nothing_has_the_name() {
    let d = two_tables("presence");
    // one table has the column: the test tells the tables apart,
    // which is what it was written for, and nothing is told
    let (out, warned) = run(&d, &["/*/*[::name] | ::name", "two.db"]);
    assert_eq!(out, "Ada\nBo\nCy");
    assert_eq!(warned, [] as [&str; 0]);
    let (_, warned) = run(&d, &["//*[::name] @| count", "two.db"]);
    assert_eq!(warned, [] as [&str; 0]);
    // the other polarity is the same test
    let (out, warned) = run(&d, &["/*/*[!::name] | ::id", "two.db"]);
    assert_eq!(out, "10\n11");
    assert_eq!(warned, [] as [&str; 0]);
    // nothing asked has the name: a misspelling is told of, with
    // the columns, and so is a column the one table lacks
    let (out, warned) = run(&d, &["/employees/*[::manger_id] | ::name", "two.db"]);
    assert_eq!(out, "");
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_PROPERTY (/employees): no property 'manger_id' on /employees; \
          its properties: id, name, title, manager_id"
        ]
    );
    let (out, warned) = run(&d, &["/rooms/*[::name] | ::id", "two.db"]);
    assert_eq!(out, "");
    assert_eq!(warned, [ROOMS_NO_NAME]);
    let (out, warned) = run(&d, &["/rooms/*[!::name] | ::id", "two.db"]);
    assert_eq!(out, "10\n11");
    assert_eq!(warned, [ROOMS_NO_NAME]);
    // every container that lacks it, once each
    let (out, warned) = run(&d, &["/*/*[::nosuch] | ::id", "two.db"]);
    assert_eq!(out, "");
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_PROPERTY (/employees): no property 'nosuch' on /employees; \
             its properties: id, name, title, manager_id",
            "warning UNKNOWN_PROPERTY (/rooms): no property 'nosuch' on /rooms; \
             its properties: id, floor",
        ]
    );
    // a column the surface declares is there, though this row
    // reads null
    let (out, warned) = run(&d, &["/rooms/*[::floor] | ::id", "two.db"]);
    assert_eq!(out, "10");
    assert_eq!(warned, [] as [&str; 0]);
    let (out, warned) = run(&d, &["/rooms/*[2][::floor] | ::id", "two.db"]);
    assert_eq!(out, "");
    assert_eq!(warned, [] as [&str; 0]);
    // each operand of `&&` and `||` is such a test
    let (out, warned) = run(&d, &["/*/*[::name && ::title] | ::name", "two.db"]);
    assert_eq!(out, "Ada\nBo");
    assert_eq!(warned, [] as [&str; 0]);
    let (out, warned) = run(&d, &["/rooms/*[::floor && !::name] | ::id", "two.db"]);
    assert_eq!(out, "10");
    assert_eq!(warned, [ROOMS_NO_NAME]);
    // where a path leads to the tested property, the hops before it
    // ask as ever and the last name is the test
    let (out, warned) = run(
        &d,
        &["/employees/*[->manager::nickname] | ::name", "two.db"],
    );
    assert_eq!(out, "");
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_PROPERTY (/employees): no property 'nickname' on /employees; \
          its properties: id, name, title, manager_id"
        ]
    );
    let (out, warned) = run(&d, &["/employees/*[->manager::title] | ::name", "two.db"]);
    assert_eq!(out, "Bo\nCy");
    assert_eq!(warned, [] as [&str; 0]);
    let (out, warned) = run(&d, &["/employees/*[->manger::name] | ::name", "two.db"]);
    assert_eq!(out, "");
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_LABEL (/employees): no edge is labelled 'manger' from /employees; \
          the labels there: manager"
        ]
    );
    // every other read warns at once, beside a test that is quiet:
    // the rooms pass by their id, and their name is then read
    let (out, warned) = run(&d, &["/*/*[::name || ::id] | ::name", "two.db"]);
    assert_eq!(out, "Ada\nBo\nCy\nnull\nnull");
    assert_eq!(warned, [ROOMS_NO_NAME]);
    let (_, warned) = run(&d, &["/rooms/*[::name = \"x\"]", "two.db"]);
    assert_eq!(warned, [ROOMS_NO_NAME]);
    let (_, warned) = run(&d, &["/rooms/* | ::name ?? \"none\"", "two.db"]);
    assert_eq!(warned, [ROOMS_NO_NAME]);
    // the verdict falls when the run ends: a test's warning comes
    // after the ones raised at once
    let (_, warned) = run(&d, &["/rooms/*[!::name] | ::nosuch", "two.db"]);
    assert_eq!(
        warned,
        [
            "warning UNKNOWN_PROPERTY (/rooms): no property 'nosuch' on /rooms; \
             its properties: id, floor",
            ROOMS_NO_NAME,
        ]
    );
    // a source that declares nothing stays quiet, as ever
    let (out, warned) = run(&d, &["/people/*[::nosuch] | ::name", "people.json"]);
    assert_eq!(out, "");
    assert_eq!(warned, [] as [&str; 0]);
    let _ = std::fs::remove_dir_all(&d);
}
