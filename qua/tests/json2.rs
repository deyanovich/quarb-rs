//! `json2:FILE` — the property reading of a JSON-model document:
//! containers are nodes, scalars are properties (lang/TODO.md). Each
//! notation has the reading under its own prefix (ruling
//! 2026-11-12): `json2:` for JSON and JSON lines, `yaml2:` for YAML,
//! `toml2:` for TOML, `cbor2:` for CBOR; `json2:` in front of a
//! YAML, TOML or CBOR file is refused naming the right one. The
//! plain file keeps the node reading.

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
    // each notation under its own prefix
    let j2 = |f: &std::path::Path| {
        let prefix = match f.extension().and_then(|e| e.to_str()) {
            Some("yaml") => "yaml2",
            Some("toml") => "toml2",
            _ => "json2",
        };
        format!("{prefix}:{}", f.display())
    };

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

fn refused(args: &[&str]) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "qua {args:?} answered");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `json2:` is JSON's: a YAML, TOML or CBOR file under it is refused
/// with the prefix that reads it (ruling 2026-11-12: "A user should
/// not be made to write json2:file.yaml").
#[test]
fn json2_refuses_another_notation_naming_its_prefix() {
    let dir = std::env::temp_dir().join(format!("quarb-json2-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, body, what, right) in [
        ("room.yaml", "library: Mouseion\n", "YAML", "yaml2"),
        ("room.yml", "library: Mouseion\n", "YAML", "yaml2"),
        ("room.toml", "library = \"Mouseion\"\n", "TOML", "toml2"),
        ("room.cbor", "\u{0}", "CBOR", "cbor2"),
    ] {
        let f = dir.join(file);
        std::fs::write(&f, body).unwrap();
        let why = refused(&["^ | ::library", &format!("json2:{}", f.display())]);
        assert!(
            why.contains(&format!("is {what}"))
                && why.contains(&format!("{right}:{}", f.display())),
            "{file}: {why}"
        );
    }
    // the right prefix reads it; a JSON lines file stays json2's
    let yaml = dir.join("room.yaml");
    assert_eq!(
        qua(&["^ | ::library", &format!("yaml2:{}", yaml.display())]).trim(),
        "Mouseion"
    );
    let toml = dir.join("room.toml");
    assert_eq!(
        qua(&["^ | ::library", &format!("toml2:{}", toml.display())]).trim(),
        "Mouseion"
    );
    let lines = dir.join("loans.jsonl");
    std::fs::write(
        &lines,
        "{\"scroll\": \"Iliad\"}\n{\"scroll\": \"Odyssey\"}\n",
    )
    .unwrap();
    assert_eq!(
        qua(&["/*::scroll", &format!("json2:{}", lines.display())]).trim(),
        "Iliad\nOdyssey"
    );
    // a prefix reads its notation whatever the file is called
    let plain = dir.join("room.conf");
    std::fs::write(&plain, "library: Mouseion\n").unwrap();
    assert_eq!(
        qua(&["^ | ::library", &format!("yaml2:{}", plain.display())]).trim(),
        "Mouseion"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
