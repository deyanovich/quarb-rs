//! YAML adapter for Quarb. YAML is the JSON data model with a
//! different surface syntax, so a document parses to a
//! [`quarb_json::JsonAdapter`] — every JSON recipe applies to a
//! `.yaml`/`.yml` file unchanged.
//!
//! YAML admits one shape JSON does not: mappings whose keys are not
//! strings (numbers, booleans, `null`). Those keys are stringified on
//! the way in, since JSON object keys are always strings, so such a
//! document is still queryable rather than rejected. Multi-document
//! streams (`---`-separated) are not yet supported — a file must hold
//! a single document.

use quarb_json::JsonAdapter;

/// Parse YAML `text` into a JSON-model adapter.
pub fn parse(text: &str) -> Result<JsonAdapter, serde_yaml_ng::Error> {
    // Fast path: a document whose mapping keys are all strings maps
    // straight onto the JSON model, exactly as before.
    match serde_yaml_ng::from_str::<serde_json::Value>(text) {
        Ok(value) => Ok(JsonAdapter::from_json_value(value)),
        // Fall back for a shape `serde_json::Value` rejects but that is
        // still valid YAML: a mapping with non-string keys. Parsing into
        // YAML's own value model and stringifying the keys recovers it.
        Err(_) => {
            let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)?;
            Ok(JsonAdapter::from_json_value(yaml_to_json(value)))
        }
    }
}

/// Convert a natively-parsed YAML value into the JSON data model,
/// stringifying any non-string mapping keys (JSON object keys are
/// always strings).
fn yaml_to_json(value: serde_yaml_ng::Value) -> serde_json::Value {
    use serde_json::Value as Json;
    use serde_yaml_ng::Value as Yaml;
    match value {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(b),
        Yaml::Number(n) => {
            // Mirror how serde_yaml_ng drives serde_json directly:
            // signed int, else unsigned int, else float.
            let num = if let Some(i) = n.as_i64() {
                Some(serde_json::Number::from(i))
            } else if let Some(u) = n.as_u64() {
                Some(serde_json::Number::from(u))
            } else {
                n.as_f64().and_then(serde_json::Number::from_f64)
            };
            num.map_or(Json::Null, Json::Number)
        }
        Yaml::String(s) => Json::String(s),
        Yaml::Sequence(items) => Json::Array(items.into_iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => Json::Object(
            map.into_iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_to_json(v)))
                .collect(),
        ),
        // A tagged node (`!Foo value`) carries its tag out of band; the
        // JSON model has no tags, so project to the underlying value.
        Yaml::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

/// Render a YAML mapping key as the string JSON uses for its object
/// keys.
fn yaml_key_to_string(key: serde_yaml_ng::Value) -> String {
    use serde_yaml_ng::Value as Yaml;
    match key {
        Yaml::String(s) => s,
        Yaml::Bool(b) => b.to_string(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Null => "null".to_string(),
        // Collection keys are rare; render them the way YAML would.
        other => serde_yaml_ng::to_string(&other)
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default(),
    }
}
