//! Live tests against a real BigQuery dataset. Gated on
//! `QUARB_BQ` (a `bigquery://PROJECT/DATASET[?account=...]`
//! target with the quarb_test music fixture); run with
//! `cargo test -p quarb-bigquery -- --ignored`.

use quarb_bigquery::BigqueryAdapter;

fn target() -> Option<String> {
    std::env::var("QUARB_BQ").ok().filter(|s| !s.is_empty())
}

fn values(adapter: &BigqueryAdapter, q: &str) -> Vec<String> {
    match quarb::run(q, adapter).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| adapter.locator(n)).collect(),
    }
}

#[test]
#[ignore = "needs QUARB_BQ and network"]
fn catalog_rows_and_fk_chain() {
    let Some(t) = target() else { return };
    let a = BigqueryAdapter::connect(&t).unwrap();
    assert_eq!(values(&a, "/artists/2::name"), ["Bartok"]);
    assert_eq!(values(&a, "/tracks/* @| count"), ["7"]);
    // The declared (unenforced) FK constraints drive resolution.
    assert_eq!(
        values(
            &a,
            "/tracks/*[::secs > 1000]::album_id~>::artist_id~>::name"
        ),
        ["Bartok"]
    );
    // Reverse resolution.
    assert_eq!(values(&a, "/artists/1::id<~[::secs > 400] @| count"), ["0"]);
}

#[test]
#[ignore = "needs QUARB_BQ and network"]
fn pushdown_matches_scan() {
    let Some(t) = target() else { return };
    let cases = [
        "/tracks/* @| count",
        "/tracks/*[::price < 1] | rec(::title, ::secs)",
        "/invoices/* | ::qty @| sum",
    ];
    for q in cases {
        let plan = quarb_sql::pushdown(q).expect("plan");
        let (cols, rows) =
            quarb_bigquery::raw_query(&t, &plan.sql, plan.order_table.as_deref()).unwrap();
        let pushed: Vec<String> = rows
            .into_iter()
            .map(|row| {
                if cols.len() <= 1 {
                    row[0].to_string()
                } else {
                    quarb::Value::Record(cols.iter().cloned().zip(row).collect()).to_string()
                }
            })
            .collect();
        let a = BigqueryAdapter::connect(&t).unwrap();
        assert_eq!(pushed, values(&a, q), "pushdown/scan divergence for {q}");
    }
}
