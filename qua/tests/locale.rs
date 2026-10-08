//! The output locale: rendering follows the locale in force
//! (`--locale`, QUARB_LOCALE, the config file, the system's), the
//! values never do; the interchange forms stay canonical.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-locale-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sales.json"),
        r#"{"rows": [{"city": "Ярославль", "n": 1234567.5, "when": "1899-10-06T14:30:00"},
                    {"city": "Ёлкино", "n": 42, "when": "1900-01-01T00:00:00"},
                    {"city": "Елец", "n": 0.5, "when": "1901-02-03T04:05:06"}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("config.toml"), "[output]\nlocale = \"de-DE\"\n").unwrap();
    dir
}

fn qua(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (String, String, bool) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_qua"));
    cmd.current_dir(dir)
        .args(args)
        .env_remove("QUARB_LOCALE")
        .env_remove("QUARB_CONFIG")
        .env_remove("LC_ALL")
        .env_remove("LANG")
        .env("HOME", dir)
        .stdin(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
    let (out, err, ok) = qua(dir, env, args);
    assert!(ok, "{args:?}: {err}");
    out
}

#[test]
fn rendering_follows_the_locale_the_values_do_not() {
    let dir = fixture("render");
    // canonical without a locale
    assert_eq!(ok(&dir, &[], &["/rows/0::n", "sales.json"]), "1234567.5");
    // --locale: numbers and instants in the locale's form
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "ru-RU", "/rows/0::n", "sales.json"]
        ),
        "1234567,5"
    );
    assert_eq!(
        ok(
            &dir,
            &[],
            &[
                "--locale",
                "ru-RU",
                "/rows/0::when | datetime",
                "sales.json"
            ]
        ),
        "6 окт. 1899 г., 14:30:00"
    );
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "de", "/rows/0 | %(::city; ::n)", "sales.json"]
        ),
        "%(city = \"Ярославль\"; n = 1234567,5)"
    );
    // the value is untouched: arithmetic and json stay canonical
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "ru-RU", "/rows/0 | ::n * 2", "sales.json"]
        ),
        "2469135"
    );
    assert_eq!(
        ok(
            &dir,
            &[],
            &[
                "--locale",
                "ru-RU",
                "/rows/0 | %(::n) @| json",
                "sales.json"
            ]
        ),
        "[{\"n\": 1234567.5}]"
    );
    // csv's delimiter follows the decimal mark; the table too
    assert_eq!(
        ok(
            &dir,
            &[],
            &[
                "--locale",
                "ru-RU",
                "--csv",
                "/rows/* | %(::city; ::n)",
                "sales.json"
            ]
        ),
        "city;n\nЯрославль;1234567,5\nЁлкино;42\nЕлец;0,5"
    );
    assert_eq!(
        ok(
            &dir,
            &[],
            &[
                "--locale",
                "en-US",
                "--csv",
                "/rows/* | %(::n)",
                "sales.json"
            ]
        ),
        "n\n1234567.5\n42\n0.5"
    );
}

#[test]
fn sort_collates_under_the_locale() {
    let dir = fixture("sort");
    // code points put Ё (U+0401) before Е; Russian collation puts it after
    assert_eq!(
        ok(&dir, &[], &["/rows/*::city @| sort", "sales.json"]),
        "Ёлкино\nЕлец\nЯрославль"
    );
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "ru-RU", "/rows/*::city @| sort", "sales.json"]
        ),
        "Елец\nЁлкино\nЯрославль"
    );
    // a locale named in the query wins over the output locale
    assert_eq!(
        ok(
            &dir,
            &[],
            &[
                "--locale",
                "de",
                "/rows/*::city @| sort(ru-RU)",
                "sales.json"
            ]
        ),
        "Елец\nЁлкино\nЯрославль"
    );
}

#[test]
fn the_layers_nearest_wins() {
    let dir = fixture("layers");
    let q = "/rows/0::n";
    // system locale
    assert_eq!(
        ok(&dir, &[("LANG", "ru_RU.UTF-8")], &[q, "sales.json"]),
        "1234567,5"
    );
    assert_eq!(
        ok(
            &dir,
            &[("LANG", "ru_RU.UTF-8"), ("LC_ALL", "C.UTF-8")],
            &[q, "sales.json"]
        ),
        "1234567.5"
    );
    // the config file over the system
    let cfg = dir.join("config.toml").display().to_string();
    assert_eq!(
        ok(
            &dir,
            &[("LANG", "ru_RU.UTF-8"), ("QUARB_CONFIG", &cfg)],
            &[q, "sales.json"]
        ),
        "1234567,5"
    );
    // the environment over the config
    assert_eq!(
        ok(
            &dir,
            &[("QUARB_CONFIG", &cfg), ("QUARB_LOCALE", "en-US")],
            &[q, "sales.json"]
        ),
        "1234567.5"
    );
    // the flag over everything; C is canonical
    assert_eq!(
        ok(
            &dir,
            &[("QUARB_LOCALE", "en-US")],
            &["--locale", "C", q, "sales.json"]
        ),
        "1234567.5"
    );
    // --explain names the locale and its layer
    let (_, err, _) = qua(
        &dir,
        &[("QUARB_LOCALE", "ru-RU")],
        &["--explain", q, "sales.json"],
    );
    assert!(err.contains("locale: ru-RU (from QUARB_LOCALE)"), "{err}");
    // an uncovered tag: refused on the flag, reported and canonical elsewhere
    let (_, err, ok_) = qua(&dir, &[], &["--locale", "xx", q, "sales.json"]);
    assert!(!ok_ && err.contains("no rendering rules"), "{err}");
    assert_eq!(
        ok(&dir, &[("LANG", "xx_XX.UTF-8")], &[q, "sales.json"]),
        "1234567.5"
    );
}

#[test]
fn a_closing_table_renders_and_the_value_never_changes() {
    let dir = fixture("stages");
    let ru = ["--locale", "ru-RU"];
    let q = |tail: &str| format!("/rows/* | %(::city; ::n) {tail}");
    // a closing @| csv / @| table renders as the flags do
    assert_eq!(
        ok(&dir, &[], &[ru[0], ru[1], &q("@| csv"), "sales.json"]),
        "city;n\nЯрославль;1234567,5\nЁлкино;42\nЕлец;0,5"
    );
    // a named delimiter wins over the locale's
    assert_eq!(
        ok(
            &dir,
            &[],
            &[ru[0], ru[1], &q("@| csv(\"|\")"), "sales.json"]
        ),
        "city|n\nЯрославль|1234567,5\nЁлкино|42\nЕлец|0,5"
    );
    // instants in the cells take the locale's form
    assert_eq!(
        ok(
            &dir,
            &[],
            &[
                ru[0],
                ru[1],
                "/rows/0 | %(w = (::when | datetime)) @| csv",
                "sales.json"
            ]
        ),
        "w\n6 окт. 1899 г., 14:30:00"
    );
    // anywhere else the stage is a value, the same under every locale
    let mid = q("@| csv | sha256");
    assert_eq!(
        ok(&dir, &[], &[ru[0], ru[1], &mid, "sales.json"]),
        ok(&dir, &[], &[&mid, "sales.json"])
    );
    // and the interchange forms hold it canonically
    assert_eq!(
        ok(
            &dir,
            &[],
            &[ru[0], ru[1], "--json", &q("@| csv"), "sales.json"]
        ),
        ok(&dir, &[], &["--json", &q("@| csv"), "sales.json"])
    );
    assert_eq!(
        ok(
            &dir,
            &[],
            &[ru[0], ru[1], "/rows/0 | %(::n) @| jsonl", "sales.json"]
        ),
        "{\"n\": 1234567.5}"
    );
}

#[test]
fn numbers_and_dates_keep_their_order_and_their_forms() {
    let dir = fixture("order");
    // numbers sort by magnitude under a locale; only text is collated
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "ru-RU", "/rows/*::n @| sort", "sales.json"]
        ),
        "0,5\n42\n1234567,5"
    );
    // a date takes the month's date form (May differs from its stand-alone)
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "ru", "= \"1899-05-06T14:30:00\" | datetime"]
        ),
        "6 мая 1899 г., 14:30:00"
    );
    // what a locale renders, the same locale's reader reads back
    assert_eq!(
        ok(&dir, &[], &["--locale", "de-CH", "--grouping", "= 1234.5"]),
        "1\u{2019}234.5"
    );
    assert_eq!(
        ok(&dir, &[], &["= \"1\u{2019}234.5\" | number(de-CH)"]),
        "1234.5"
    );
}

#[test]
fn grouping_is_asked_for() {
    let dir = fixture("grouping");
    let q = "/rows/0::n";
    // a year or an id prints as written under any locale
    assert_eq!(ok(&dir, &[], &["--locale", "en-US", "= 1876"]), "1876");
    assert_eq!(
        ok(&dir, &[], &["--locale", "ru-RU", q, "sales.json"]),
        "1234567,5"
    );
    // the flag, the environment, the config
    assert_eq!(
        ok(
            &dir,
            &[],
            &["--locale", "ru-RU", "--grouping", q, "sales.json"]
        ),
        "1\u{a0}234\u{a0}567,5"
    );
    assert_eq!(
        ok(
            &dir,
            &[("QUARB_GROUPING", "1")],
            &["--locale", "en-US", q, "sales.json"]
        ),
        "1,234,567.5"
    );
    std::fs::write(
        dir.join("grouped.toml"),
        "[output]\nlocale = \"de-DE\"\ngrouping = true\n",
    )
    .unwrap();
    let cfg = dir.join("grouped.toml").display().to_string();
    assert_eq!(
        ok(&dir, &[("QUARB_CONFIG", &cfg)], &[q, "sales.json"]),
        "1.234.567,5"
    );
}
