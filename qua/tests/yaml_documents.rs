//! A YAML file of several documents, `---` between them (a
//! Kubernetes manifest): the root is the list of the documents in
//! file order, as a JSON lines file's root is the list of its
//! lines. A file of one document is that document, with a
//! separator before it or without (the user's request of
//! 2026-11-13; until then such a file was refused).

fn qua(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "qua {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn lines(args: &[&str]) -> Vec<String> {
    qua(args).lines().map(str::to_string).collect()
}

const MANIFEST: &str = "---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: library
spec:
  replicas: 3
---
apiVersion: v1
kind: Service
metadata:
  name: library
spec:
  ports:
    - port: 80
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: shelf
data:
  keeper: Callimachus
";

#[test]
fn several_documents_are_a_list_in_file_order() {
    let dir = std::env::temp_dir().join(format!("quarb-yaml-docs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("manifest.yaml");
    std::fs::write(&f, MANIFEST).unwrap();
    let f = f.to_str().unwrap();
    assert_eq!(lines(&["^ | :::length", f]), ["3"]);
    assert_eq!(
        lines(&["/* | ::kind", f]),
        ["Deployment", "Service", "ConfigMap"]
    );
    assert_eq!(
        lines(&[r#"/*[::kind = "Deployment"]/spec::replicas"#, f]),
        ["3"]
    );
    assert_eq!(lines(&["/1/spec/ports/0::port", f]), ["80"]);
    assert_eq!(
        lines(&[r#"//*[::kind = "ConfigMap"]/data::keeper"#, f]),
        ["Callimachus"]
    );
    // the property reading takes the same list
    let y2 = format!("yaml2:{f}");
    assert_eq!(
        lines(&["/* | ::kind", &y2]),
        ["Deployment", "Service", "ConfigMap"]
    );
    // an empty document between two is null
    let g = dir.join("gap.yaml");
    std::fs::write(&g, "a: 1\n---\n---\nb: 2\n").unwrap();
    let g = g.to_str().unwrap();
    assert_eq!(lines(&["^ | :::length", g]), ["3"]);
    assert_eq!(lines(&["/* | ::", g]).len(), 3);
    assert_eq!(lines(&["/2::b", g]), ["2"]);
    // one document is the document, a separator before it or not
    for text in [
        "kind: ConfigMap\nmetadata:\n  name: shelf\n",
        "---\nkind: ConfigMap\nmetadata:\n  name: shelf\n",
        "---\nkind: ConfigMap\nmetadata:\n  name: shelf\n...\n",
    ] {
        let one = dir.join("one.yaml");
        std::fs::write(&one, text).unwrap();
        let one = one.to_str().unwrap();
        assert_eq!(lines(&["/metadata::name", one]), ["shelf"], "{text:?}");
        assert_eq!(lines(&["::kind", one]), ["ConfigMap"], "{text:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
