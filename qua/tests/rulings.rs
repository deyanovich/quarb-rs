//! Rulings #33, #34, and #38, end to end over a JSON mount:
//! pattern literals match (and push down elsewhere — see
//! quarb-sql), the strict hole refuses, the default hole
//! coalesces, pipe tails run stages inside `${...}`, and the
//! record sigil builds and enriches records.

use quarb::{AstAdapter, QueryResult, Value};

fn doc() -> quarb_json::JsonAdapter {
    quarb_json::JsonAdapter::parse(
        r#"{"items":[
            {"name":"app-web"},
            {"name":"data.gz"},
            {"name":"a*b!"},
            {"x":7}
        ]}"#,
    )
    .unwrap()
}

fn values(q: &str) -> Vec<String> {
    match quarb::run(q, &doc()).unwrap() {
        QueryResult::Values(vs) => vs.iter().map(Value::to_string).collect(),
        _ => panic!("expected values"),
    }
}

#[test]
fn pattern_literals_match() {
    assert_eq!(values(r#"/items/*[::name = *"web"*]::name"#), ["app-web"]);
    assert_eq!(values(r#"/items/*[::name = "app"*]::name"#), ["app-web"]);
    assert_eq!(values(r#"/items/*[::name = *".gz"]::name"#), ["data.gz"]);
    assert_eq!(
        values(r#"/items/*[::name = "app"*"web"]::name"#),
        ["app-web"]
    );
    // the quoted text is literal — a data star needs no escape
    assert_eq!(values(r#"/items/*[::name = *"a*b"*]::name"#), ["a*b!"]);
    // `!=` keeps the holes, shape-identical to the plain `!=`
    assert_eq!(
        values(r#"/items/*[::name != *"zzz"*] @| count"#),
        values(r#"/items/*[::name != "zzz"] @| count"#),
    );
}

#[test]
fn strict_hole_refuses() {
    let a = doc();
    // the refusal surfaces as run()'s Refused error (ruling #24)
    let e = quarb::run(r#"= "${/items/1::missing:?}""#, &a).unwrap_err();
    assert!(e.to_string().contains("produced nothing"), "{e}");
    let e = quarb::run(r#"= "${/items/1::missing:?no gz name}""#, &a).unwrap_err();
    assert!(e.to_string().contains("no gz name"), "{e}");
    // a present value splices with no refusal
    assert_eq!(values(r#"= "got ${/items/0::name:?}""#), ["got app-web"]);
}

#[test]
fn default_hole_coalesces() {
    assert_eq!(
        values(r#"= "got ${/items/0::missing:-'n/a'}""#),
        ["got n/a"]
    );
    // the fallback is a full value expression
    assert_eq!(
        values(r#"= "got ${/items/0::missing:-/items/1::name}""#),
        ["got data.gz"]
    );
    assert_eq!(values(r#"= "got ${/items/0::name:-'n/a'}""#), ["got app-web"]);
}

#[test]
fn pipe_tails_in_holes() {
    assert_eq!(values(r#"= "got ${/items/0::name | upper}""#), ["got APP-WEB"]);
    assert_eq!(
        values(r#"= "got ${/items/0::name | s/app/svc/ | upper}""#),
        ["got SVC-WEB"]
    );
}

/// Ruling #38: `%(...)` is the record constructor's canonical
/// spelling (`rec`/`record` are aliases); `%%(...)` is the named
/// register view enriched with the args — registers first in the
/// `%.` layout, args after, an arg overriding a register field
/// that shares its name.
#[test]
fn record_sigil_builds_and_enriches() {
    // constructor == rec, explicit and auto-named fields
    assert_eq!(
        values(r#"/items/0 | %("n", ::name, ::name)"#),
        ["%(n = 'app-web'; name = 'app-web')"]
    );
    assert_eq!(
        values(r#"/items/0 | rec("n", ::name)"#),
        values(r#"/items/0 | %("n", ::name)"#)
    );
    // enrichment: register fields first, args after
    assert_eq!(
        values(r#"/items/0 | .n(::name) | %%("upper", (::name | upper))"#),
        ["%(n = 'app-web'; upper = 'APP-WEB')"]
    );
    // an arg overrides the register field sharing its name
    assert_eq!(
        values(r#"/items/0 | .n(::name) | %%("n", 'won')"#),
        ["%(n = 'won')"]
    );
}

#[test]
fn record_sigil_refusals() {
    let e = quarb::run("/items/* | %()", &doc()).unwrap_err();
    assert!(e.to_string().contains("at least one field"), "{e}");
    let e = quarb::run("/items/* | %%()", &doc()).unwrap_err();
    assert!(e.to_string().contains("the register view is %."), "{e}");
    // the duplicate-name check covers the sigil spellings too
    let e = quarb::run(r#"/items/* | %("a", 1, "a", 2)"#, &doc()).unwrap_err();
    assert!(e.to_string().contains("twice"), "{e}");
}
