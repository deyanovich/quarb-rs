//! `json2:FILE` — the property reading of a JSON-model document:
//! containers are nodes, scalars are properties (lang/TODO.md). JSON,
//! JSONL, YAML and TOML take the prefix alike; the plain file keeps
//! the node reading.

fn qua(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
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

#[test]
fn json2_is_the_property_reading() {
    let dir = std::env::temp_dir().join(format!("quarb-json2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let json = dir.join("conf.json");
    std::fs::write(
        &json,
        r#"{"server": {"port": 8080, "tags": ["a", "b"]}, "users": [{"name": "Emma"}]}"#,
    )
    .unwrap();
    let yaml = dir.join("conf.yaml");
    std::fs::write(
        &yaml,
        "server:\n  port: 8080\n  tags: [a, b]\nusers:\n  - name: Emma\n",
    )
    .unwrap();
    let toml = dir.join("conf.toml");
    std::fs::write(
        &toml,
        "[server]\nport = 8080\ntags = [\"a\", \"b\"]\n[[users]]\nname = \"Emma\"\n",
    )
    .unwrap();
    let j2 = |f: &std::path::Path| format!("json2:{}", f.display());

    // The node reading: port is a node. The property reading: it is not.
    assert_eq!(
        qua(&["//port @| count", json.to_str().unwrap()]).trim(),
        "1"
    );
    assert_eq!(qua(&["//port @| count", &j2(&json)]).trim(), "0");
    for f in [&json, &yaml, &toml] {
        assert_eq!(
            qua(&["/server::port", &j2(f)]).trim(),
            "8080",
            "{}",
            f.display()
        );
        assert_eq!(
            qua(&["/server::tags", &j2(f)]).trim(),
            r#"@("a"; "b")"#,
            "{}",
            f.display()
        );
        assert_eq!(
            qua(&["/users/*::name", &j2(f)]).trim(),
            "Emma",
            "{}",
            f.display()
        );
        assert_eq!(
            qua(&["*//* @| count", &j2(f)]).trim(),
            "4",
            "{}",
            f.display()
        );
    }
    // Serialization sees the properties: a scalar-only object is
    // not null, a mixed array comes back in order.
    std::fs::write(
        dir.join("m.json"),
        r#"{"server": {"port": 6000, "mixed": [1, {"x": 2}, "s"]}}"#,
    )
    .unwrap();
    let m = j2(&dir.join("m.json"));
    assert_eq!(qua(&["/server/mixed/* | json", &m]).trim(), r#"{"x": 2}"#);
    assert_eq!(
        qua(&["/server | json", &m]).trim(),
        r#"{"mixed": [1, {"x": 2}, "s"], "port": 6000}"#
    );
    // Mounted beside a node-reading file.
    let a = format!("a={}", j2(&json));
    let b = format!("b={}", json.display());
    assert_eq!(qua(&["/a/server::port", &a, &b]).trim(), "8080");
    assert_eq!(qua(&["/b/server/port::", &a, &b]).trim(), "8080");
    let _ = std::fs::remove_dir_all(&dir);
}
