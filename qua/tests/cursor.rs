//! The cursor and the capsa (spec: The Cursor; The Capsa): `$$_` the
//! position, `$$__` the invoking position, `$$0` the driver; node
//! references as values; marks as regulae of the one register, read
//! by side.
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let test = std::thread::current().name().unwrap_or("t").replace(':', "-");
    let dir = std::env::temp_dir().join(format!("quarb-cursor-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("nest.json"),
        r#"{"a":[{"k":1,"b":[{"v":10},{"v":20}]},{"k":2,"b":[{"v":30}]}]}"#,
    )
    .unwrap();
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_qua"))
        .current_dir(dir)
        .env("QUARB_LOCALE", "C")
        .env("HOME", dir)
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "nest.json"]);
    assert!(good, "{query}: {err}");
    out
}

fn err(dir: &Path, query: &str) -> String {
    let (out, err, good) = qua(dir, &[query, "nest.json"]);
    assert!(!good, "{query} succeeded: {out}");
    err
}

/// `$$_` is the node the thread stands on: bare, a reference that
/// prints as the locator; with a tail, a read from the node.
#[test]
fn the_position_is_a_reference() {
    let d = fixture();
    assert_eq!(
        ok(&d, "/a/* | %(me = $$_; k = $$_::k)"),
        "%(me = /a/0; k = 1)\n%(me = /a/1; k = 2)"
    );
    assert_eq!(ok(&d, "/a/* | $$_ | type"), "node\nnode");
    assert_eq!(ok(&d, "/a/* | %(me = ((_)))"), "%(me = /a/0)\n%(me = /a/1)");
    // In a predicate it is the candidate.
    assert_eq!(ok(&d, "/a/*[$$_::k = 2]::k"), "2");
    assert_eq!(ok(&d, "/a/*[$$_ = $$_]::k"), "1\n2");
    // Exports are the locator text.
    assert_eq!(
        ok(&d, "/a/* | %(me = $$_) | json"),
        "{\"me\": \"/a/0\"}\n{\"me\": \"/a/1\"}"
    );
}

/// `$$__` is the opener of the last query body, held through the
/// body's hops and transparent through expression bodies; null at
/// the top level.
#[test]
fn the_invoking_position() {
    let d = fixture();
    assert_eq!(
        ok(&d, "/a/* | .s(/b/* | %(v = ::v; up = ($$__::k))) | $.s"),
        "@%(@(v; up); %(10; 1); %(20; 1))\n%(v = 30; up = 2)"
    );
    // the thread whose scoped match is empty answers null, printed
    assert_eq!(ok(&d, "/a/* | .s(/b/*[$$__::k = 1]::v) | $.s"), "@(10; 20)\nnull");
    assert_eq!(ok(&d, "/a/* | .s(/b/* | .t(((__))::k) | $.t) | $.s"), "@(1; 1)\n2");
    // Top level: null.
    assert_eq!(ok(&d, "/a/* | ($$__::k ?? \"none\")"), "none\nnone");
    // `_` is the cursor record now: a node projection written on it
    // says where to go.
    assert!(err(&d, "/a/* | .s(/b/* | %(up = _::k))").contains("'_' is the cursor"));
}

/// `$$0` is the driver inside a join condition and nowhere else.
#[test]
fn the_driver() {
    let d = fixture();
    assert_eq!(
        ok(
            &d,
            "/a/* <=> /a/*/b/*[::v > 10 && $$0::k = 1] | %(k = ::k; v = $$1::v)"
        ),
        "%(k = 1; v = 20)"
    );
    assert_eq!(
        ok(&d, "/a/* <=> /a/*/b/*[::v = 30 && ((0))::k = 2] | $$1::v"),
        "30"
    );
    assert!(err(&d, "/a/* | %(n = (/b/*[$$0::k = 2]::v))").contains("names the driver"));
    assert!(err(&d, "/a/* | $$0::k").contains("names the driver"));
}

/// A fragment body is spliced text: `$$0` in it is judged where
/// the expansion lands (ruling of 2026-10-20). The definition
/// loads, a join condition admits the invocation, anywhere else the
/// invocation is the parse error. A macro's expansion is judged the
/// same way. (A parameter in key position, `$$0::$k`, is not a form
/// yet: parameters are operands, and bare words are not arguments.)
#[test]
fn the_driver_in_a_fragment_body() {
    let d = fixture();
    std::fs::write(
        d.join("defs.quarb"),
        "def &under: /a/*/b/*[::v > 10 && $$0::k = 1] ;\n\
         def &twice: /a/*/b/*[::v > 10] | [$$0::k = 1] ;\n\
         def &viaunder: &under ;\n\
         macro &gen: ^ | \"/a/*/b/*[::v = 30 && $$0::k = 2]\" ;\n",
    )
    .unwrap();
    let run = |q: &str| qua(&d, &["--defs", "defs.quarb", q, "nest.json"]);
    // A defs file that carries `$$0` in a body loads; a query that
    // never invokes the fragment is untouched by it.
    let (out, err, good) = run("/a/*::k");
    assert!(good, "{err}");
    assert_eq!(out, "1\n2");
    // In joined position the body's `$$0` is the driver.
    let (out, err, good) = run("/a/* <=> &under | %(k = ::k; v = $$1::v)");
    assert!(good, "{err}");
    assert_eq!(out, "%(k = 1; v = 20)");
    // A fragment that invokes the fragment defers the verdict too.
    let (out, err, good) = run("/a/* <=> &viaunder | $$1::v");
    assert!(good, "{err}");
    assert_eq!(out, "20");
    // Outside a join condition the invocation is the parse error,
    // naming the fragment.
    let (out, err, good) = run("&under::v");
    assert!(!good, "{out}");
    assert!(err.contains("in expansion of '&under'") && err.contains("names the driver"), "{err}");
    let (_, err, good) = run("/a/* | %(n = (&twice::v))");
    assert!(!good);
    assert!(err.contains("in expansion of '&twice'"), "{err}");
    let (_, err, good) = run("&viaunder::v");
    assert!(!good);
    assert!(err.contains("in expansion of '&viaunder'"), "{err}");
    // A macro expansion carrying `$$0` is judged at its splice.
    let (out, err, good) = run("/a/* <=> &gen | $$1::v");
    assert!(good, "{err}");
    assert_eq!(out, "30");
    let (_, err, good) = run("&gen::v");
    assert!(!good);
    assert!(err.contains("in expansion of '&gen'") && err.contains("names the driver"), "{err}");
}

/// The capsa is one memory: marks are regulae holding references,
/// read by side — `$.` `@.` `%.` `%.*` the values, `$$.` `@@.` `%%.`
/// `%%.*` the references — under one name pool.
#[test]
fn marks_are_regulae() {
    let d = fixture();
    let q = "/a/*[::k = 1] | .A | /b/* | .B | .v(::v) | ";
    assert_eq!(
        ok(&d, &format!("{q}@@.")),
        "@(/a/0; /a/0/b/0)\n@(/a/0; /a/0/b/1)"
    );
    assert_eq!(
        ok(&d, &format!("{q}%%.")),
        "%(A = /a/0; B = /a/0/b/0)\n%(A = /a/0; B = /a/0/b/1)"
    );
    assert_eq!(
        ok(&d, &format!("{q}%%.*")),
        "%(A = /a/0; B = /a/0/b/0)\n%(A = /a/0; B = /a/0/b/1)"
    );
    assert_eq!(ok(&d, &format!("{q}@.")), "@(10)\n@(20)");
    assert_eq!(ok(&d, &format!("{q}%.")), "%(v = 10)\n%(v = 20)");
    assert_eq!(ok(&d, &format!("{q}%.*")), "%(v = 10)\n%(v = 20)");
    // Positions count per side: the first value, the first and
    // second reference.
    assert_eq!(ok(&d, &format!("{q}$.1")), "10\n20");
    assert_eq!(ok(&d, &format!("{q}$$.1::k")), "1\n1");
    assert_eq!(ok(&d, &format!("{q}$$.2::v")), "10\n20");
    // A name reads whatever was filed under it last: a reference
    // prints as its locator, is of type node, and `-->` resolves it.
    assert_eq!(ok(&d, "/a/*[::k = 1] | .A | /b/* | $.A"), "/a/0\n/a/0");
    assert_eq!(ok(&d, "/a/*[::k = 1] | .A | /b/* | $.A | type"), "node\nnode");
    assert_eq!(ok(&d, "/a/*[::k = 1] | .A | /b/* | $.A-->::k"), "1\n1");
    assert_eq!(
        ok(&d, "/a/*[::k = 1] | .A | /b/* | ((A))/b/*::v @| count"),
        "2"
    );
    // One name pool: a value pushed under the name after the mark
    // re-points it, and the node side finds nothing.
    assert_eq!(ok(&d, "/a/* | .n(::k) | .n(7) | $$.n::k @| count"), "0");
    assert_eq!(ok(&d, "/a/* | .M($$_) | /b/* | $$.M::k"), "1\n2");
    // Identity through references.
    assert_eq!(ok(&d, "/a/* | .A | /b/*[$$_ != $$.A] @| count"), "3");
    assert_eq!(ok(&d, "/a/* | .A | /b/*[$$.A = $$__] @| count"), "0");
}
