//! `a ?? b` — the null default (spec: The Conditional, "The null
//! default"): `a` unless it is null, then `b`; null alone is tested,
//! so false, 0 and "" stay; the right side only where needed; chains
//! group to the right; below arithmetic and comparison, above the
//! conditional; spaced like every binary operator.

use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"letters":[
            {"id":"a","when":"1887-07-07","notBefore":"1887-01-01","subtitle":"To P. I. Biryukov"},
            {"id":"b","when":null,"notBefore":"1844-05-29","subtitle":null},
            {"id":"c","when":"","notBefore":"1900","n":0,"ok":false}]}"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &quarb::Run::ambient())?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

#[test]
fn the_null_default_fills_a_null_and_nothing_else() {
    assert_eq!(
        run("/letters/* | (::when ?? ::notBefore)").unwrap(),
        ["1887-07-07", "1844-05-29", ""]
    );
    // bare where parentheses already delimit
    assert_eq!(
        run("/letters/*[::id = \"b\"] | %(year = ::when ?? ::notBefore)").unwrap(),
        [r#"%(year = "1844-05-29")"#]
    );
    assert_eq!(
        run("/letters/* | (::subtitle ?? \"-\")").unwrap(),
        ["To P. I. Biryukov", "-", "-"]
    );
    // false, 0 and "" are values and stay
    assert_eq!(run("/letters/*[::id = \"c\"] | (::n ?? 9)").unwrap(), ["0"]);
    assert_eq!(run("/letters/*[::id = \"c\"] | (::ok ?? true)").unwrap(), ["false"]);
    assert_eq!(run("/letters/*[::id = \"c\"] | (::when ?? \"x\")").unwrap(), [""]);
    // the chain takes the first non-null, grouping to the right
    assert_eq!(
        run("/letters/*[::id = \"b\"] | (::when ?? ::nothing ?? ::notBefore)").unwrap(),
        ["1844-05-29"]
    );
    // below arithmetic, above the conditional
    assert_eq!(run("/letters/*[::id = \"c\"] | (::n + 1 ?? 7)").unwrap(), ["1"]);
    assert_eq!(run("/letters/*[::id = \"b\"] | (::missing + 1 ?? 7)").unwrap(), ["7"]);
    assert_eq!(
        run("/letters/*[::id = \"b\"] | (::when ?? ::notBefore ? \"dated\" : \"undated\")").unwrap(),
        ["dated"]
    );
    // spaced on both sides
    let e = run("/letters/* | (::when??::notBefore)").unwrap_err().to_string();
    assert!(e.contains("'??' is an operator and takes a space on both sides"), "{e}");
}
