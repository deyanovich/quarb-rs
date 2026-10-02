//! Composition: a model `use`s another (mounts resolved against the
//! included file, statements and defs joined, cycles and duplicate
//! mounts refused), and a ref may target any path — another mount's
//! rows — without an aliasing node.

use quarb::AstAdapter as _;
use quarb_json::JsonAdapter;
use quarb_model::{ModelAdapter, parse_model, parse_model_file};

fn write(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn use_joins_models_and_locates_their_mounts() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    write(&sub, "words.jsonl", "{\"w\":\"дом\"}\n");
    write(
        &sub,
        "words.model",
        "mount слова: words.jsonl;\nnode /статьи/статья: /слова/*;\ndef &все: /статьи/статья;\n",
    );
    let top = write(
        dir.path(),
        "top.model",
        "use sub/words.model;\nnode /копии/копия: /слова/*;\n",
    );
    let m = parse_model_file(&top).unwrap();
    assert_eq!(m.mounts.len(), 1);
    assert_eq!(m.mounts[0].name, "слова");
    assert!(
        m.mounts[0].target.ends_with("sub/words.jsonl"),
        "{}",
        m.mounts[0].target
    );
    assert_eq!(
        m.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
        ["статьи", "копии"]
    );
    assert!(m.defs_text.contains("&все"));
    // a mount declared twice, a cycle, and text without a location all refuse
    write(
        dir.path(),
        "twice.model",
        "use sub/words.model;\nmount слова: other.jsonl;\n",
    );
    assert!(
        parse_model_file(&dir.path().join("twice.model"))
            .unwrap_err()
            .contains("already declared")
    );
    write(dir.path(), "loop.model", "use loop.model;\n");
    assert!(
        parse_model_file(&dir.path().join("loop.model"))
            .unwrap_err()
            .contains("includes itself")
    );
    assert!(
        parse_model("use x.model;")
            .unwrap_err()
            .contains("location")
    );
}

fn dictionaries() -> ModelAdapter<JsonAdapter> {
    // two "mounts" as two top-level arrays of one document
    let base = JsonAdapter::parse(
        r#"{"зализняк": [{"ключ": "дом", "класс": "1"}, {"ключ": "стол", "класс": "1"}],
            "кузнецова": [{"ключ": "дом", "морфы": ["дом"]}, {"ключ": "стол", "морфы": ["стол"]}, {"ключ": "окно", "морфы": ["окн", "о"]}]}"#,
    )
    .unwrap();
    let model = parse_model(
        r#"
        node /статьи/статья: /зализняк/*;
        ref /зализняк/*::ключ --> /кузнецова/*[::ключ = $];
        "#,
    )
    .unwrap();
    ModelAdapter::new(base, model)
}

fn values(a: &ModelAdapter<JsonAdapter>, q: &str) -> Vec<String> {
    let mut got: Vec<String> = match quarb::run(q, a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
    };
    got.sort();
    got
}

#[test]
fn a_ref_may_target_another_mount_directly() {
    let a = dictionaries();
    // resolution into the other mount's rows, keyed on the target property
    assert_eq!(
        values(&a, "/зализняк/*[::ключ = 'дом']::ключ-->/морфы/*::"),
        vec!["дом"]
    );
    // the hop is labelled by the target path's last named segment
    assert_eq!(
        values(&a, "/зализняк/*[::ключ = 'стол']--кузнецова/морфы/*::"),
        vec!["стол"]
    );
    // and back, on the alias role
    assert_eq!(
        values(&a, "/кузнецова/*[::ключ = 'дом']--статья::класс"),
        vec!["1"]
    );
    assert_eq!(
        values(&a, "/кузнецова/*[::ключ = 'окно']--статья @| count"),
        vec!["0"]
    );
}
