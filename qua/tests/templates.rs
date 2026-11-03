//! Quarq templates over data (design paper: the quasiquote): a
//! data-aware macro filling an expansion from the dataset, and the
//! clock an expansion sees — the run's, `--now` included.

use quarb::Defs;

fn books() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"row":[
            {"title":"The Iliad","year":-750,"shelf":"A1","genre":"epic"},
            {"title":"Hamlet","year":1603,"shelf":"B2","genre":"tragedy"},
            {"title":"Emma","year":1815,"shelf":"B2","genre":"novel"},
            {"title":"The Adventures of Tom Sawyer","year":1876,"shelf":"C3","genre":"novel"},
            {"title":"Adventures of Huckleberry Finn","year":1884,"shelf":"C3","genre":"novel"}]}"#,
    )
    .unwrap()
}

fn events() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"events": [{"what": "deploy", "time": "2026-03-01T09:30"},
            {"what": "alert", "time": "2026-02-28T22:10"},
            {"what": "deploy", "time": "2026-02-27T08:00"},
            {"what": "alert", "time": "2026-03-01T11:45"}]}"#,
    )
    .unwrap()
}

fn values(q: &str, doc: &quarb_json::JsonAdapter, run: &quarb::Run) -> Vec<String> {
    let out = quarb::Query::parse_with(q, &Defs::default(), doc, run)
        .unwrap()
        .run(doc, run)
        .unwrap();
    match out.result {
        quarb::QueryResult::Values(vs) => vs.iter().map(quarb::Value::display_form).collect(),
        quarb::QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    }
}

#[test]
fn a_data_aware_macro_fills_an_expansion_from_the_data() {
    let doc = books();
    let run = quarb::Run::ambient();
    // (a JSON array's items stand under `/row/*`; the draft's CSV
    // rows are `/row` themselves)
    let q = "macro &later-than-median!($col): q([$col > ${/data/row/* | $col @| median}]); \
             /row/* | &later-than-median!(::year) | ::title";
    assert_eq!(
        quarb::expand_with(q, &Defs::default(), &doc, &run).unwrap(),
        "/row/* | [::year > 1815] | ::title"
    );
    assert_eq!(
        values(q, &doc, &run),
        [
            "The Adventures of Tom Sawyer",
            "Adventures of Huckleberry Finn"
        ]
    );
    // the same macro mid-walk refines the step before it
    let q = "macro &later-than-median!($col): q([$col > ${/data/row/* | $col @| median}]); \
             /row/*&later-than-median!(::year)::title";
    assert_eq!(
        quarb::expand_with(q, &Defs::default(), &doc, &run).unwrap(),
        "/row/*[::year > 1815]::title"
    );
    assert_eq!(
        values(q, &doc, &run),
        [
            "The Adventures of Tom Sawyer",
            "Adventures of Huckleberry Finn"
        ]
    );
}

#[test]
fn an_expansion_freezes_the_clock_a_template_keeps_it_live() {
    let doc = events();
    let mut run = quarb::Run::ambient();
    // 2026-03-01T12:00Z
    run.now = Some((1772366400, 0));
    // the template reads the clock when the query runs
    assert_eq!(
        values(
            "def &last($d): [::time > now() - $d]; /events/*&last(24_h)::what",
            &doc,
            &run
        ),
        ["deploy", "alert", "alert"]
    );
    // the macro writes the instant into the query
    let q = "macro &since($d): q(| [::time > ${now() - $d}]); /events/* | &since(24_h) | ::what";
    assert_eq!(
        quarb::expand_with(q, &Defs::default(), &doc, &run).unwrap(),
        "/events/* | [::time > 2026-02-28T12:00:00Z] | ::what"
    );
    assert_eq!(values(q, &doc, &run), ["deploy", "alert", "alert"]);
}

#[test]
fn a_fresh_name_hides_from_the_named_view_and_the_positions() {
    let doc = books();
    let run = quarb::Run::ambient();
    let share = "macro &share($col): \
                 q(| .t(^/row/*$col @| sum) | %(::title; share = round($col div $.t; 4)));";
    // the caller's capsa holds no `t`
    assert_eq!(
        values(
            &format!("{share} /row/*[::year > 1800] | .k(1) | &share(::year) | %."),
            &doc,
            &run
        )[0],
        "%(k = 1)"
    );
    // the full view shows it under its fresh spelling
    let full = &values(
        &format!("{share} /row/*[::year > 1800] | .k(1) | &share(::year) | %.*"),
        &doc,
        &run,
    )[0];
    assert!(full.contains("t·1"), "{full}");
    // a positional recall from outside skips it: the caller's own last push
    assert_eq!(
        values(
            &format!("{share} /row/*[::year > 1800] | .k(1) | &share(::year) | $.-1"),
            &doc,
            &run
        )[0],
        "1"
    );
    // and the macro's record reads its own push
    let rows = values(
        &format!("{share} /row/*[::year > 1800] | &share(::year)"),
        &doc,
        &run,
    );
    assert!(
        rows[0].starts_with("%(title = \"Emma\"; share = 0."),
        "{}",
        rows[0]
    );
}
