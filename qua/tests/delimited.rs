//! Delimited text: the delimiter decided (`?delimiter=`, an Excel
//! `sep=` line, the extension), the refusal when it is plainly
//! another, the decimal convention (`?decimal=comma`), `number(LOCALE)`,
//! and `@| csv(";")` on the way out.

use std::path::{Path, PathBuf};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quarb-delimited-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shelf")).unwrap();
    let w = |p: &str, bytes: &[u8]| std::fs::write(dir.join(p), bytes).unwrap();
    // One price list, saved three ways.
    w(
        "prices.csv",
        b"title,price\nThe Iliad,12.50\nThe Odyssey,1234.5\nAeneid,9.75\n",
    );
    w(
        "prices-semicolon.csv",
        "title;price\nThe Iliad;12,50\nThe Odyssey;1 234,5\nAeneid;9,75\n".as_bytes(),
    );
    w(
        "prices.tsv",
        b"title\tprice\nThe Iliad\t12.50\nThe Odyssey\t1,234.5\nAeneid\t9.75\n",
    );
    // Excel's declaration line.
    w(
        "prices-excel.csv",
        b"sep=;\ntitle;price\nThe Iliad;12.50\nThe Odyssey;1234.5\nAeneid;9.75\n",
    );
    // The same list in the shelf, for a folder graft.
    w(
        "shelf/prices.csv",
        "title;price\nThe Iliad;12,50\nThe Odyssey;1 234,5\nAeneid;9,75\n".as_bytes(),
    );
    // windows-1251, semicolons, decimal commas: the Russian shop.
    let ru = "название;цена\nИлиада;12,50\nОдиссея;1 234,5\nЭнеида;9,75\n";
    let cp1251: Vec<u8> = ru
        .chars()
        .map(|c| match c as u32 {
            0x0410..=0x044F => (c as u32 - 0x0410 + 0xC0) as u8,
            n if n < 0x80 => n as u8,
            _ => panic!("not in the fixture alphabet: {c:?}"),
        })
        .collect();
    w("ceny.csv", &cp1251);
    dir
}

fn qua(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qua"))
        .env("QUARB_LOCALE", "C")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, err, ok) = qua(dir, args);
    assert!(ok, "{args:?}: {err}");
    out
}

#[test]
fn one_list_three_ways_one_total() {
    let dir = fixture("total");
    let sum = "/* | ::price @| sum";
    assert_eq!(ok(&dir, &[sum, "prices.csv"]), "1256.75");
    assert_eq!(ok(&dir, &[sum, "prices.tsv"]), "1256.75");
    assert_eq!(
        ok(
            &dir,
            &[
                sum,
                "prices-semicolon.csv?delimiter=semicolon&decimal=comma"
            ]
        ),
        "1256.75"
    );
    assert_eq!(
        ok(
            &dir,
            &[sum, "prices-semicolon.csv?delimiter=;&decimal=comma"]
        ),
        "1256.75"
    );
    // Excel's `sep=;` line decides the delimiter and is not a row
    assert_eq!(ok(&dir, &[sum, "prices-excel.csv"]), "1256.75");
    assert_eq!(ok(&dir, &["/* @| count", "prices-excel.csv"]), "3");
    assert_eq!(ok(&dir, &["^::::delimiter", "prices-excel.csv"]), ";");
    // what was decided, on the root
    assert_eq!(
        ok(
            &dir,
            &[
                "^::::decimal",
                "prices-semicolon.csv?delimiter=;&decimal=comma"
            ]
        ),
        "comma"
    );
    assert_eq!(ok(&dir, &["^::::decimal", "prices.csv"]), "point");
}

#[test]
fn the_wrong_delimiter_is_refused_and_the_wrong_decimal_is_null() {
    let dir = fixture("refuse");
    let (_, err, ok) = qua(&dir, &["/* @| count", "prices-semicolon.csv"]);
    assert!(!ok);
    assert!(err.contains("?delimiter=semicolon"), "{err}");
    // under the default convention `12,50` has no reading: omitted,
    // not counted wrong; `1,234.5` groups
    // (a sum over no readings prints nothing, as the Quarb form of
    // null)
    assert_eq!(
        ok_(
            &dir,
            &["/* | ::price @| sum", "prices-semicolon.csv?delimiter=;"]
        ),
        ""
    );
    assert_eq!(ok_(&dir, &["/row[2]::price", "prices.tsv"]), "1,234.5");
    assert_eq!(
        ok_(&dir, &["/row[2] | ::price + 0", "prices.tsv"]),
        "1234.5"
    );
    // unknown labels refuse at open
    let (_, err, ok) = qua(&dir, &["/* @| count", "prices.csv?delimiter=semi"]);
    assert!(!ok && err.contains("unknown delimiter"), "{err}");
    let (_, err, ok) = qua(&dir, &["/* @| count", "prices.csv?decimal=dot"]);
    assert!(!ok && err.contains("unknown decimal convention"), "{err}");
}

fn ok_(dir: &Path, args: &[&str]) -> String {
    ok(dir, args)
}

#[test]
fn canonical_form_rides_the_declared_convention() {
    let dir = fixture("canonical");
    // the cell arrives in canonical form under the convention
    assert_eq!(
        ok(
            &dir,
            &[
                "/row[2]::price",
                "prices-semicolon.csv?delimiter=;&decimal=comma"
            ]
        ),
        "1234.5"
    );
    // a folder's grafted leaves read under the folder's options
    assert_eq!(
        ok(
            &dir,
            &[
                "/prices.csv/* | ::price @| sum",
                "--graft",
                "shelf?delimiter=;&decimal=comma"
            ]
        ),
        "1256.75"
    );
    // the Russian shop: encoding, delimiter and decimal together
    assert_eq!(
        ok(
            &dir,
            &[
                "/* | ::цена @| sum",
                "ceny.csv?encoding=windows-1251&delimiter=;&decimal=comma"
            ]
        ),
        "1256.75"
    );
    assert_eq!(
        ok(
            &dir,
            &[
                "/row[1]::название",
                "ceny.csv?encoding=windows-1251&delimiter=;&decimal=comma"
            ]
        ),
        "Илиада"
    );
}

#[test]
fn number_by_locale_and_csv_out() {
    let dir = fixture("out");
    assert_eq!(ok(&dir, &["= \"1 234,56\" | number(ru-RU)"]), "1234.56");
    assert_eq!(ok(&dir, &["= \"1'234.56\" | number(de-CH)"]), "1234.56");
    assert_eq!(ok(&dir, &["= \"1,234.56\" | number(en-US)"]), "1234.56");
    assert_eq!(ok(&dir, &["= \"12,50\" | number(en-US)"]), "");
    assert_eq!(ok(&dir, &["= \"1,234\" | number"]), "1234");
    // out: the delimiter as the quoted first argument, numbers canonical
    assert_eq!(
        ok(
            &dir,
            &[
                "/* | %(::title; ::price) @| csv(\";\"; title; price)",
                "prices-semicolon.csv?delimiter=;&decimal=comma"
            ]
        ),
        "title;price\nThe Iliad;12.50\nThe Odyssey;1234.5\nAeneid;9.75"
    );
    assert_eq!(
        ok(&dir, &["/* | %(::title; ::price) @| csv", "prices.csv"]),
        "title,price\nThe Iliad,12.50\nThe Odyssey,1234.5\nAeneid,9.75"
    );
}
