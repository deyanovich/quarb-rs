//! YAML adapter for Quarb. YAML is the JSON data model with a
//! different surface syntax, so a document parses to a
//! [`quarb_json::JsonAdapter`] — every JSON recipe applies to a
//! `.yaml`/`.yml` file unchanged.
//!
//! YAML admits one shape JSON does not: mappings whose keys are not
//! strings (numbers, booleans, `null`). Those keys are stringified on
//! the way in, since JSON object keys are always strings, so such a
//! document is still queryable rather than rejected.
//!
//! A file may hold several documents, `---` between them (a
//! Kubernetes manifest does). A file of two or more mounts with the
//! root a list of the documents in file order, as a JSON lines file's
//! root is the list of its lines: `/*[::kind = "Deployment"]` reads
//! one kind, `^ | :::length` counts them. A file of one document is
//! that document, a leading `---` or not, so a document does not
//! change shape for having a separator before it.

use quarb_json::{JsonAdapter, Reading};

/// Parse YAML `text` into a JSON-model adapter.
pub fn parse(text: &str) -> Result<JsonAdapter, serde_yaml_ng::Error> {
    parse_as(text, Reading::Node)
}

/// Parse YAML `text` under the given reading (the property reading
/// is `yaml2:FILE`).
pub fn parse_as(text: &str, reading: Reading) -> Result<JsonAdapter, serde_yaml_ng::Error> {
    // Fast path: a document whose mapping keys are all strings maps
    // straight onto the JSON model, exactly as before.
    match serde_yaml_ng::from_str::<serde_json::Value>(text) {
        Ok(value) => Ok(JsonAdapter::from_json_value_as(value, reading)),
        // Fall back for a shape `serde_json::Value` rejects but that is
        // still valid YAML: a mapping with non-string keys. Parsing into
        // YAML's own value model and stringifying the keys recovers it.
        Err(_) => match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) {
            Ok(value) => Ok(JsonAdapter::from_json_value_as(
                yaml_to_json(value),
                reading,
            )),
            // Neither reading takes a file of several documents:
            // the root is the list of them. Any other failure is
            // the one-document reading's own.
            Err(e) => match documents(text) {
                Ok(docs) if docs.len() > 1 => Ok(JsonAdapter::from_json_value_as(
                    serde_json::Value::Array(docs),
                    reading,
                )),
                _ => Err(e),
            },
        },
    }
}

/// A YAML header at the top of a text (a Markdown note's front
/// matter): its keys in order, each value in Quarb's data model, and
/// the text after it.
///
/// A header counts only when the first line is `---`, a later line
/// is `---` or `...`, and what stands between parses as a YAML
/// mapping. Anything else (a rule that opens a document, a block
/// that is a list or a sentence, a header never closed) is no
/// header, and the text is the caller's as it stands.
///
/// Values keep their YAML types: a number is a number, a list a
/// list, a nested mapping a record with its keys in order. A date is
/// the text the author wrote; reading it as an instant is the
/// caller's (a curated `published`).
pub fn front_matter(text: &str) -> Option<(Vec<(String, quarb::Value)>, &str)> {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = t.split_inclusive('\n');
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut at = t.find('\n')? + 1;
    let open = at;
    for line in lines {
        let l = line.trim_end();
        if l == "---" || l == "..." {
            let block = &t[open..at];
            let body = &t[at + line.len()..];
            let serde_yaml_ng::Value::Mapping(map) =
                serde_yaml_ng::from_str::<serde_yaml_ng::Value>(block).ok()?
            else {
                return None;
            };
            let entries = map
                .into_iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_to_value(v)))
                .collect();
            return Some((entries, body));
        }
        at += line.len();
    }
    None
}

/// A YAML value in Quarb's data model.
fn yaml_to_value(value: serde_yaml_ng::Value) -> quarb::Value {
    use quarb::Value;
    use serde_yaml_ng::Value as Yaml;
    match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(b) => Value::Bool(b),
        Yaml::Number(n) => match n.as_i64() {
            Some(i) => Value::Int(i),
            None => n.as_f64().map_or(Value::Null, Value::Float),
        },
        Yaml::String(s) => Value::Str(s),
        Yaml::Sequence(items) => Value::list(items.into_iter().map(yaml_to_value).collect()),
        Yaml::Mapping(map) => Value::record(
            map.into_iter()
                .map(|(k, v)| (yaml_key_to_string(k), yaml_to_value(v)))
                .collect(),
        ),
        Yaml::Tagged(tagged) => yaml_to_value(tagged.value),
    }
}

/// The documents of a `---`-separated stream, each in the JSON data
/// model (an empty document is null).
fn documents(text: &str) -> Result<Vec<serde_json::Value>, serde_yaml_ng::Error> {
    use serde::Deserialize;
    serde_yaml_ng::Deserializer::from_str(text)
        .map(|doc| serde_yaml_ng::Value::deserialize(doc).map(yaml_to_json))
        .collect()
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
