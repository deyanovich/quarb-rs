use quarb::{QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"rows":[
            {"id":1,"size":"2 KiB","price":"12_EUR","rate":"3_EUR/kg","speed":"10 m/s","tag":"2nd","when":"2025-10-06"},
            {"id":2,"size":"1 MB","price":"8_USD","rate":"5_USD/lb","speed":"36 km/h","tag":"3D","when":"2024-01-01"},
            {"id":3,"size":"8 b","price":"soon","rate":"2_GBP/kg","speed":"1 kn","tag":"5 apples","when":"2025-11-02"}]}"#,
    )
    .unwrap()
}

fn run(q: &str) -> quarb::Result<Vec<String>> {
    let run = quarb::Run::ambient();
    let out = quarb::Query::parse(q, &doc())?.run(&doc(), &run)?;
    Ok(match out.result {
        QueryResult::Values(vs) => vs.iter().map(Value::display_form).collect(),
        QueryResult::Nodes(ns) => ns.iter().map(|n| format!("{n:?}")).collect(),
    })
}

fn one(q: &str) -> String {
    let v = run(q).unwrap();
    assert_eq!(v.len(), 1, "{q}: {v:?}");
    v.into_iter().next().unwrap()
}

fn refuse(q: &str) -> String {
    run(q).unwrap_err().to_string()
}

/// A calendar is a reading of an instant (spec: Calendars): the
/// timeline stays, the date fields answer in the calendar.
#[test]
fn a_calendar_reads_the_date_fields() {
    let h = "= '2024-03-05' | datetime | calendar(hebrew)";
    assert_eq!(one(h), "2024-03-05");
    assert_eq!(one(&format!("{h} | calendar")), "hebrew");
    assert_eq!(one(&format!("{h} | year")), "5784");
    assert_eq!(one(&format!("{h} | month")), "6");
    assert_eq!(one(&format!("{h} | month(code)")), "M05L");
    assert_eq!(one(&format!("{h} | day")), "25");
    assert_eq!(one(&format!("{h} | era")), "am");
    assert_eq!(one(&format!("{h} | weekday")), "2");
    assert_eq!(one("= '2024-03-05' | datetime | calendar"), "gregorian");
    assert_eq!(one("= '2024-03-05' | datetime | month(code)"), "M03");
    assert_eq!(one("= '2024-03-05' | datetime | era"), "ce");
    let j = "= '2024-03-05' | datetime | calendar(japanese)";
    assert_eq!(one(&format!("{j} | year")), "6");
    assert_eq!(one(&format!("{j} | era")), "reiwa");
    assert_eq!(
        one("= '2024-03-05' | datetime | calendar(chinese) | month(code)"),
        "M01"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | calendar(chinese) | era"),
        "null"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | calendar(islamic) | year"),
        "1445"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | calendar(roc) | year"),
        "113"
    );
    // the timeline compares, whatever the reading
    assert_eq!(
        one("= ('2024-03-05' | datetime | calendar(hebrew)) - ('2024-03-05' | datetime)"),
        "0s"
    );
    // the reading survives through the day
    assert_eq!(one(&format!("{h} | date | calendar")), "hebrew");
    // a fresh instant is Gregorian
    assert_eq!(one(&format!("{h} | + 1_d | calendar")), "gregorian");
}

/// The period of a calendar's month or year begins where the
/// calendar says; under C it prints as the day it begins on.
#[test]
fn a_calendar_period_has_the_calendars_bounds() {
    let h = "= '2024-03-05' | datetime | calendar(hebrew)";
    assert_eq!(one(&format!("{h} | period(month)")), "2024-02-10");
    assert_eq!(one(&format!("{h} | period(month) | grain")), "month");
    assert_eq!(one(&format!("{h} | period(month) | calendar")), "hebrew");
    assert_eq!(one(&format!("{h} | period(year)")), "2023-09-16");
    assert_eq!(one("= '2024-03-05' | datetime | period(month)"), "2024-03");
    // the CLDR's grain names in the shipped languages
    assert_eq!(
        one("= '2024-03-05' | datetime | period('месяц')"),
        "2024-03"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | period('Monat')"),
        "2024-03"
    );
    assert_eq!(one("= '2024-03-05' | datetime | period('год')"), "2024");
    assert_eq!(one("= '2024-03-05' | datetime | period('年')"), "2024");
}

/// The formatters' third argument names the calendar whose fields
/// the specifiers mean; the names come from the CLDR in the locale.
#[test]
fn the_formatters_take_a_calendar() {
    assert_eq!(
        one("= '1828-08-28' | tp('%Y-%m-%d'; ru; julian)"),
        "1828-09-09"
    );
    assert_eq!(
        one("= '1828-08-28' | tp('%Y-%m-%d'; ru; julian) | calendar"),
        "julian"
    );
    assert_eq!(
        one("= '1828-09-09' | datetime | strftime('%d %B %Y'; ru; julian)"),
        "28 августа 1828"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | strftime('%-d %B %Y'; en; hebrew)"),
        "25 Adar I 5784"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | strftime('%-d %B %Y'; ru; hebrew)"),
        "25 адар I 5784"
    );
    assert_eq!(
        one("= '2024-03-15' | datetime | strftime('%-d %B %Y'; en; hebrew)"),
        "5 Adar II 5784"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | strftime('%EC%Ey年%-m月%-d日'; ja; japanese)"),
        "令和6年3月5日"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | strftime('%-d %B %Y'; en; islamic)"),
        "24 Shaʻban 1445"
    );
    // without a third argument the topic's own reading writes
    assert_eq!(
        one("= '2024-03-05' | datetime | calendar(hebrew) | strftime('%F')"),
        "5784-06-25"
    );
    assert_eq!(
        one("= '2024-03-05' | datetime | calendar(islamic) | strftime('%-d %B %Y'; en)"),
        "24 Shaʻban 1445"
    );
    // reading: the names and the eras
    assert_eq!(
        one("= '25 Adar I 5784' | tp('%d %B %Y'; en; hebrew)"),
        "2024-03-05"
    );
    assert_eq!(
        one("= '5 Adar II 5784' | tp('%d %B %Y'; en; hebrew)"),
        "2024-03-15"
    );
    assert_eq!(
        one("= '5784-06-25' | tp('%Y-%m-%d'; en; hebrew)"),
        "2024-03-05"
    );
    assert_eq!(
        one("= '令和6年3月5日' | tp('%EC%Ey年%m月%d日'; ja; japanese)"),
        "2024-03-05"
    );
    assert_eq!(
        one("= '6年3月5日' | tp('%Y年%m月%d日'; ja; japanese)"),
        "2024-03-05"
    );
    assert_eq!(
        one("= '24 Shaʻban 1445' | tp('%d %B %Y'; en; islamic)"),
        "2024-03-05"
    );
    // the refusals
    let err = refuse("= '2024-03-05' | datetime | calendar(xx)");
    assert!(err.contains("calendar(xx): no such calendar"), "{err}");
    assert!(err.contains("gregorian, julian, hebrew"), "{err}");
    let err = refuse("= '2024-03-05' | datetime | strftime('%Y'; en; xx)");
    assert!(err.contains("no such calendar \"xx\""), "{err}");
}

/// The CLDR's names of the calendars in the shipped languages are
/// aliases of the canonical names; only the canonical ever prints.
#[test]
fn calendar_names_in_every_language() {
    for (alias, name) in [
        ("hebrew", "hebrew"),
        ("еврейский", "hebrew"),
        ("hébraïque", "hebrew"),
        ("Jüdischer", "hebrew"),
        ("Hidschra", "islamic"),
        ("hijri", "islamic"),
        ("Hicri", "islamic"),
        ("календарь хиджры", "islamic"),
        ("农历", "chinese"),
        ("和暦", "japanese"),
        ("юлианский", "julian"),
        ("julien", "julian"),
        ("Miladi Takvim", "gregorian"),
        ("ethiopic", "ethiopian"),
        ("Minguo", "roc"),
        ("العبري", "hebrew"),
    ] {
        assert_eq!(
            one(&format!(
                "= '2024-03-05' | datetime | calendar('{alias}') | calendar"
            )),
            name,
            "{alias}"
        );
    }
}
