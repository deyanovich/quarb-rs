//! JSON's own reference syntax, `$ref`, is its default reference
//! property: the bare arrows read it, forward and reverse.

use quarb::{AstAdapter as _, QueryResult, run};
use quarb_json::JsonAdapter;

const DOC: &str = r##"{"definitions": {"address": {"type": "object"},
                                      "name":    {"type": "string"}},
              "home":   {"$ref": "#/definitions/address"},
              "office": {"$ref": "#/definitions/address"},
              "owner":  {"$ref": "#/definitions/name", "note": "#/home"},
              "plain":  {"x": 1}}"##;

fn nodes(a: &JsonAdapter, q: &str) -> Vec<String> {
    match run(q, a).unwrap_or_else(|e| panic!("{q}: {e}")) {
        QueryResult::Nodes(ns) => ns
            .into_iter()
            .map(|n| a.name(n).unwrap_or_default())
            .collect(),
        other => panic!("expected nodes, got {other:?}"),
    }
}

#[test]
fn the_bare_arrows_read_dollar_ref_in_both_readings() {
    for a in [
        JsonAdapter::parse(DOC).unwrap(),
        JsonAdapter::parse_as(DOC, quarb_json::Reading::Property).unwrap(),
    ] {
        assert_eq!(nodes(&a, "/home-->"), ["address"]);
        assert_eq!(nodes(&a, "/definitions/address<--"), ["home", "office"]);
        assert_eq!(nodes(&a, "/definitions/address<--(/office)"), ["office"]);
        assert_eq!(nodes(&a, "/definitions/name<--"), ["owner"]);
        // a named property is still its own matter
        assert_eq!(nodes(&a, "/home<--(/owner::note)"), ["owner"]);
        // a scope none of whose nodes carries a `$ref` has nothing
        // to read: refused, not empty
        let err = run("/definitions/address<--(/plain)", &a).unwrap_err();
        assert!(err.to_string().contains("no reference property"), "{err}");
    }
}

// The adapter's reference rule, by the two standards JSON has. A
// `$ref` holds a URI reference: only its fragment points into this
// document. Any other property, when a query names it, is read as a
// JSON Pointer in the plain or the fragment form. A bare word is
// text, however it happens to match a key.
#[test]
fn what_a_value_points_at() {
    const DOC: &str = r##"{
        "users": [{"name": "Ada"}, {"name": "Bo"}],
        "definitions": {"address": {"t": 0}, "a/b": {"t": 1}, "~t": {"t": 2}},
        "post": {"kind": "users", "author": "#/users/0", "plain": "/users/1",
                 "empty": "", "whole": "#", "far": "other.json#/x"},
        "r1": {"$ref": "#/definitions/address"},
        "r2": {"$ref": "#"},
        "r3": {"$ref": ""},
        "r4": {"$ref": "/definitions/address"},
        "r5": {"$ref": "other.json#/shared/x"},
        "r6": {"$ref": "#/definitions/a~1b"},
        "r7": {"$ref": "#/definitions/%7Et"},
        "r8": {"$ref": "#name"},
        "r9": {"$ref": "users"}}"##;
    for a in [
        JsonAdapter::parse(DOC).unwrap(),
        JsonAdapter::parse_as(DOC, quarb_json::Reading::Property).unwrap(),
    ] {
        let at = |q: &str| match run(q, &a).unwrap_or_else(|e| panic!("{q}: {e}")) {
            QueryResult::Nodes(ns) => ns.into_iter().map(|n| a.pointer(n)).collect::<Vec<_>>(),
            other => panic!("expected nodes, got {other:?}"),
        };
        let nothing: Vec<String> = Vec::new();
        // a named property: a JSON Pointer, plain or as a fragment
        assert_eq!(at("/post::author-->"), ["/users/0"]);
        assert_eq!(at("/post::plain-->"), ["/users/1"]);
        assert_eq!(at("/post::whole-->"), ["/"]);
        // …and nothing else
        assert_eq!(at("/post::kind-->"), nothing, "a bare word is text");
        assert_eq!(at("/post::empty-->"), nothing, "an empty value");
        assert_eq!(at("/post::far-->"), nothing, "not the reference keyword");
        // `$ref`: a URI reference; its fragment is this document
        assert_eq!(at("/r1-->"), ["/definitions/address"]);
        assert_eq!(at("/r2-->"), ["/"]);
        assert_eq!(at("/r3-->"), ["/"]);
        // (`~1` is `/` and `%7E` is `~` in the reference; the locator
        // printed here is Quarb's path, which escapes neither)
        assert_eq!(at("/r6-->"), ["/definitions/a/b"]);
        assert_eq!(at("/r7-->"), ["/definitions/~t"]);
        // …and anything else is another resource, or nothing here
        assert_eq!(at("/r4-->"), nothing, "a path is another resource");
        assert_eq!(at("/r8-->"), nothing, "a named anchor is not read");
        assert_eq!(at("/r9-->"), nothing);
        // the adapter hands the other resource over in two parts
        let r = |q: &str| match run(q, &a).unwrap() {
            QueryResult::Nodes(ns) => ns[0],
            _ => unreachable!(),
        };
        assert_eq!(
            a.external_ref(r("/r5"), "$ref", None),
            Some(quarb::ExternalRef {
                document: "other.json".into(),
                within: Some("/shared/x".into()),
            })
        );
        assert_eq!(
            a.external_ref(r("/r4"), "$ref", None).map(|e| e.document),
            Some("/definitions/address".to_string())
        );
        assert_eq!(a.external_ref(r("/r1"), "$ref", None), None);
        assert_eq!(a.external_ref(r("/post"), "far", None), None);
        // under "any property" each value is read the same way: the
        // pointer counts, the bare word does not
        assert_eq!(at("/users/0<--(/post::*)"), ["/post"]);
        assert_eq!(at("/users<--(/post::*)"), nothing);
    }
}
