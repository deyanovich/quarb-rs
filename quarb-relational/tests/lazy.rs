//! The loading model: catalog eager, rows lazy — a table
//! materializes on first touch, exactly once, and untouched tables
//! are never fetched.

use quarb::Value;
use quarb_relational::{RelationalModel, RowSpec, TableSpec};
use std::cell::RefCell;
use std::rc::Rc;

fn spec(name: &str, columns: &[&str], pk: Option<usize>, fks: &[(usize, &str)]) -> TableSpec {
    TableSpec {
        name: name.to_string(),
        columns: columns.iter().map(|c| c.to_string()).collect(),
        pk,
        fks: fks
            .iter()
            .map(|(c, t)| (*c, t.to_string(), String::new()))
            .collect(),
    }
}

/// A two-table model whose fetcher counts its calls.
fn counting_model() -> (RelationalModel, Rc<RefCell<Vec<String>>>) {
    let fetched = Rc::new(RefCell::new(Vec::new()));
    let log = fetched.clone();
    let model = RelationalModel::lazy(
        vec![
            spec("users", &["id", "name"], Some(0), &[]),
            spec(
                "orders",
                &["id", "user_id", "amt"],
                Some(0),
                &[(1, "users")],
            ),
        ],
        Box::new(move |_, spec| {
            log.borrow_mut().push(spec.name.clone());
            Ok(match spec.name.as_str() {
                "users" => vec![
                    RowSpec {
                        rowid: 1,
                        values: vec![Value::Int(1), Value::Str("Ada".into())],
                    },
                    RowSpec {
                        rowid: 2,
                        values: vec![Value::Int(2), Value::Str("Bo".into())],
                    },
                ],
                _ => vec![RowSpec {
                    rowid: 1,
                    values: vec![Value::Int(10), Value::Int(2), Value::Int(99)],
                }],
            })
        }),
    );
    (model, fetched)
}

fn values(model: &RelationalModel, query: &str) -> Vec<String> {
    match quarb::run(query, model).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(_) => panic!("expected values"),
    }
}

#[test]
fn only_touched_tables_load() {
    let (model, fetched) = counting_model();
    // listing tables touches nothing
    assert_eq!(values(&model, "/*;;;loaded"), vec!["false", "false"]);
    assert!(fetched.borrow().is_empty());
    // querying users loads users only
    assert_eq!(values(&model, "/users/1::name"), vec!["Ada"]);
    assert_eq!(*fetched.borrow(), vec!["users"]);
    assert_eq!(values(&model, "/users;;;loaded"), vec!["true"]);
    assert_eq!(values(&model, "/orders;;;loaded"), vec!["false"]);
    // ... and only once, however often it is read
    let _ = values(&model, "/users/*::name @| count");
    assert_eq!(*fetched.borrow(), vec!["users"]);
}

#[test]
fn resolution_pulls_the_target() {
    let (model, fetched) = counting_model();
    // the FK hop from orders loads orders, then users on resolve
    assert_eq!(values(&model, "/orders/10::user_id~>::name"), vec!["Bo"]);
    assert_eq!(*fetched.borrow(), vec!["orders", "users"]);
}

#[test]
fn eager_build_never_fetches() {
    let model = RelationalModel::build(vec![(
        spec("t", &["id"], Some(0), &[]),
        vec![RowSpec {
            rowid: 1,
            values: vec![Value::Int(7)],
        }],
    )]);
    assert_eq!(values(&model, "/t;;;loaded"), vec!["true"]);
    assert_eq!(values(&model, "/t/7::id"), vec!["7"]);
}
