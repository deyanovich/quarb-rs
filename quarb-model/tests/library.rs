//! Seams found writing the Model files recipes: a ref whose source
//! is written through a role, the reverse value form from a
//! role-named target, and two models over one base used by a third.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model, parse_model_file};

fn library(refs: &str) -> ModelAdapter<JsonAdapter> {
    let base = JsonAdapter::parse(
        r#"{"books": [
              {"id": "sawyer", "title": "Tom Sawyer",       "author": "twain"},
              {"id": "finn",   "title": "Huckleberry Finn", "author": "twain"},
              {"id": "emma",   "title": "Emma",             "author": "austen"}],
            "authors": [
              {"id": "twain",  "name": "Mark Twain"},
              {"id": "austen", "name": "Jane Austen"}]}"#,
    )
    .unwrap();
    let model = parse_model(&format!(
        "node /library/book: /books/*;\nnode /writers/author: /authors/*;\n{refs}"
    ))
    .unwrap();
    ModelAdapter::new(base, model)
}

fn values(a: &ModelAdapter<JsonAdapter>, q: &str) -> Vec<String> {
    match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    }
}

#[test]
fn a_ref_may_name_its_source_by_role() {
    let by_path = library("ref /books/*::author --> /writers/author[::id = $];");
    let by_role = library("ref /library/book::author --> /writers/author[::id = $];");
    for a in [&by_path, &by_role] {
        assert_eq!(
            values(a, "//book | ::author-->::name"),
            ["Mark Twain", "Mark Twain", "Jane Austen"]
        );
        assert_eq!(
            values(a, r#"//author[::id = "twain"]<-book::title"#),
            ["Tom Sawyer", "Huckleberry Finn"]
        );
    }
}

#[test]
fn the_reverse_value_form_meets_each_source_once() {
    let a = library("ref /books/*::author --> /writers/author[::id = $];");
    // from the role node and from the base row alike
    assert_eq!(
        values(&a, r#"//author[::id = "twain"]::author<--::title"#),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
    assert_eq!(
        values(&a, r#"/authors/*[::id = "twain"]::author<--::title"#),
        ["Tom Sawyer", "Huckleberry Finn"]
    );
    assert_eq!(
        values(&a, r#"//author[::id = "austen"] | (::author<-- @| count)"#),
        ["1"]
    );
}

#[test]
fn two_models_over_one_base_combine_in_a_third() {
    let dir = std::env::temp_dir().join(format!("quarb-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
    write("base.model", "node /library/book: /books/*;\n");
    write(
        "left.model",
        "use base.model;\nnode /writers/author: /authors/*;\n",
    );
    write(
        "right.model",
        "use base.model;\nnode /titles/title: /books/*::title | unique;\n",
    );
    write("both.model", "use left.model;\nuse right.model;\n");
    let m = parse_model_file(&dir.join("both.model")).unwrap();
    // the shared base is read once
    assert_eq!(m.nodes.len(), 3);
    // a real cycle is still refused
    write("a.model", "use b.model;\n");
    write("b.model", "use a.model;\n");
    let err = parse_model_file(&dir.join("a.model")).unwrap_err();
    assert!(err.contains("includes itself"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
