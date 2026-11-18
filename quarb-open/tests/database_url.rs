//! A database reached by a URL takes the mount's own options in the
//! URL's query string (ruling 2026-11-18): the engine takes out the
//! names it knows, `graft` and `crosslinks`, and the rest goes to
//! the driver as it was written.

use quarb_open::{SourceCtx, split_database_url, take_encoding_option};
use std::path::Path;

#[test]
fn the_mounts_options_come_out_of_a_database_urls_query_string() {
    let split = |s: &str| split_database_url(s).map(|(url, options)| (url, options.len()));
    // the driver's parameters stay, in their order
    assert_eq!(
        split("postgres://b@h/shop?sslmode=require&crosslinks=order&connect_timeout=5"),
        Some((
            "postgres://b@h/shop?sslmode=require&connect_timeout=5".to_string(),
            1
        ))
    );
    // nothing of the driver's left: no question mark either
    assert_eq!(
        split("mysql://root@127.0.0.1:3306/shop?crosslinks=order,customer&graft=json2"),
        Some(("mysql://root@127.0.0.1:3306/shop".to_string(), 2))
    );
    // none of the engine's: the URL as it was written, to the byte
    for url in [
        "postgres://b@h/shop?sslmode=require",
        "postgresql://h/shop",
        "mysql://u@localhost/db?socket=/run/mysqld/mysqld.sock",
        "bigquery://p/quarb_ga?account=a@b.c",
        "mssql://sa@h/db?",
    ] {
        assert_eq!(split(url), Some((url.to_string(), 0)), "{url}");
    }
    // a parameter is the engine's by its key alone
    assert_eq!(
        split("postgres://h/db?application_name=graft&options=crosslinks"),
        Some((
            "postgres://h/db?application_name=graft&options=crosslinks".to_string(),
            0
        ))
    );
    // no database URL: a file, a reading, PostgreSQL's keyword form
    for target in [
        "shop.db?crosslinks=order",
        "text:tom.txt?encoding=latin1",
        "host=/tmp dbname=shop",
        "https://example.org/data.json?graft=json2",
    ] {
        assert_eq!(split_database_url(target), None, "{target}");
    }
}

#[test]
fn a_database_url_sets_the_reading_and_the_held_references() {
    let ctx = SourceCtx::default();
    let url = take_encoding_option(
        Path::new("postgres://b@h/shop?sslmode=require&crosslinks=order, customer&graft=json2"),
        &ctx,
    )
    .unwrap();
    assert_eq!(url.to_str(), Some("postgres://b@h/shop?sslmode=require"));
    assert_eq!(ctx.crosslinks(), ["order", "customer"]);
    assert!(ctx.data_graft().json);

    // a URL with no option of the engine's leaves both as they were
    let ctx = SourceCtx::default();
    let url = take_encoding_option(Path::new("mysql://u@h/db?socket=/run/s.sock"), &ctx).unwrap();
    assert_eq!(url.to_str(), Some("mysql://u@h/db?socket=/run/s.sock"));
    assert!(ctx.crosslinks().is_empty());
    assert!(!ctx.data_graft().json);

    // refused as on a file, and the refusal does not print the URL,
    // which may carry a password
    for (target, says) in [
        (
            "postgres://b:secret@h/shop?crosslinks=",
            "crosslinks= names",
        ),
        ("mysql://b:secret@h/shop?graft=prose", "graft"),
    ] {
        let err = take_encoding_option(Path::new(target), &SourceCtx::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains(says), "{target}: {err}");
        assert!(!err.contains("secret"), "{err}");
    }
}
