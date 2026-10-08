//! A model over several mounts: a model node's properties are read
//! on the model's side and never handed to the mounts beneath (a
//! mount reads its index out of a node id; a model node's id
//! holds none).

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-model-mounts-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("books.csv"),
        "title,year,author\nThe Iliad,-750,1\nHamlet,1601,2\nTom Sawyer,1876,3\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("authors.csv"),
        "id,name,language\n1,Homer,Greek\n2,Shakespeare,English\n3,Twain,English\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib.model"),
        "node /library/book: /books/*;\n\
         node /writers/author: /authors/*;\n\
         ref /books/*::author --> /writers/author[::id = $];\n\
         trait <classic>: [::year < 1800];\n\
         trait <greek>: book[->author::language = \"Greek\"];\n",
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, query: &str) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args([
            "--model",
            "lib.model",
            query,
            "books=books.csv",
            "authors=authors.csv",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn a_model_node_reads_its_properties_over_mounts() {
    let dir = fixture("props");
    assert_eq!(qua(&dir, "//book<classic>::title"), "The Iliad\nHamlet");
    assert_eq!(qua(&dir, "//book<greek>::title"), "The Iliad");
    assert_eq!(qua(&dir, "//book[::year > 1800]::title"), "Tom Sawyer");
    // a base node under the same model keeps its mount's metadata
    assert_eq!(qua(&dir, "/books/* @| count"), "3");
}

/// A model that `use`s another, called by a relative path from
/// another directory: the used model's mounts are found beside it.
#[test]
fn a_used_models_mounts_are_found_from_any_directory() {
    let dir = fixture("use");
    let lib = dir.join("lib");
    std::fs::create_dir_all(lib.join("data")).unwrap();
    std::fs::copy(dir.join("books.csv"), lib.join("data/books.csv")).unwrap();
    std::fs::write(
        lib.join("data/catalogue.model"),
        "mount books: books.csv;\nnode /library/book: /books/*;\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("same.model"),
        "use data/catalogue.model;\ntrait <classic>: [::year < 1800];\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("data/beside.model"),
        "use catalogue.model;\ntrait <classic>: [::year < 1800];\n",
    )
    .unwrap();
    let run = |cwd: &Path, model: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
            .env("QUARB_LOCALE", "C")
            .current_dir(cwd)
            .args(["--model", model, "//book<classic>::title"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{model}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let want = "The Iliad\nHamlet";
    // beside the used model, one directory up, two up, and from inside
    assert_eq!(run(&dir, "lib/data/beside.model"), want);
    assert_eq!(run(&lib, "data/beside.model"), want);
    assert_eq!(run(&lib.join("data"), "beside.model"), want);
    // a used model in a subdirectory of the using one
    assert_eq!(run(&dir, "lib/same.model"), want);
    assert_eq!(run(&lib, "same.model"), want);
    assert_eq!(run(&lib.join("data"), "../same.model"), want);
}
