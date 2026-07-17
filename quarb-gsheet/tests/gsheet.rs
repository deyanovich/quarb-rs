//! Live test against Google's public sample sheet. Gated: needs
//! QUARB_GSHEET_KEY (an API key) and network; run with --ignored.

use quarb_gsheet::GsheetAdapter;

#[test]
#[ignore = "needs QUARB_GSHEET_KEY and network"]
fn sample_sheet() {
    if std::env::var("QUARB_GSHEET_KEY").is_err() {
        return;
    }
    let a =
        GsheetAdapter::connect("gsheet://1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms").unwrap();
    let v = |q: &str| match quarb::run(q, &a).unwrap() {
        quarb::QueryResult::Values(vs) => vs.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|&n| a.locator(n)).collect(),
    };
    assert_eq!(v("/* @| count"), ["1"]);
    assert_eq!(v("/'Class Data'/*[::Gender = \"Female\"] @| count"), ["15"]);
}
