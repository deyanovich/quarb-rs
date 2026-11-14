//! A file of several documents, `---` between them, parses: the root
//! is the list of the documents (walked in qua/tests/
//! yaml_documents.rs; no `quarb` dev-dependency here, as in
//! non_string_keys.rs). A stream that does not parse is refused.

const MANIFEST: &str = "---
kind: Deployment
metadata:
  name: library
---
kind: Service
metadata:
  name: library
";

#[test]
fn several_documents_parse() {
    assert!(quarb_yaml::parse(MANIFEST).is_ok());
    assert!(quarb_yaml::parse_as(MANIFEST, quarb_json::Reading::Property).is_ok());
    // an empty document between two, and keys that are no strings
    assert!(quarb_yaml::parse("a: 1\n---\n---\n1: one\ntrue: yes\n").is_ok());
}

#[test]
fn one_document_parses_with_or_without_a_separator() {
    for text in [
        "kind: ConfigMap\n",
        "---\nkind: ConfigMap\n",
        "---\nkind: ConfigMap\n...\n",
    ] {
        assert!(quarb_yaml::parse(text).is_ok(), "{text:?}");
    }
}

#[test]
fn a_stream_that_does_not_parse_is_refused() {
    assert!(quarb_yaml::parse("a: 1\n---\nb: [1, 2\n").is_err());
    assert!(quarb_yaml::parse("a: [1, 2\n").is_err());
}
