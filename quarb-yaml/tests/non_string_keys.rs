//! Regression tests for YAML shapes that `serde_json::Value` alone
//! rejects. Only the crate's own public API is exercised (no `quarb`
//! dev-dependency), so these assert parse success/failure rather than
//! walking the resulting adapter.

/// A mapping with integer keys is valid YAML but `serde_json::Value`
/// rejects it with "invalid type: integer, expected a string". The
/// adapter must stringify the keys and parse it rather than error.
#[test]
fn integer_mapping_keys_parse() {
    assert!(quarb_yaml::parse("8080: web\n9090: metrics\n").is_ok());
}

/// Boolean and null keys are likewise stringified rather than rejected.
#[test]
fn bool_and_null_keys_parse() {
    assert!(quarb_yaml::parse("true: yes\nnull: nothing\n").is_ok());
}

/// The common all-string-key document still parses via the fast path.
#[test]
fn string_keys_still_parse() {
    assert!(quarb_yaml::parse("name: web\nport: 8080\n").is_ok());
}

/// The fallback still surfaces a genuine syntax error rather than
/// swallowing it into an empty document.
#[test]
fn malformed_yaml_still_errors() {
    assert!(quarb_yaml::parse("key: [unclosed\n").is_err());
}
