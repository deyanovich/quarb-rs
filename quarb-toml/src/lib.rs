//! TOML adapter for Quarb. TOML maps onto the JSON data model, so
//! a document parses to a [`quarb_json::JsonAdapter`] — the JSON
//! recipes apply to a `.toml` file unchanged.

use quarb_json::JsonAdapter;

/// Parse TOML `text` into a JSON-model adapter.
pub fn parse(text: &str) -> Result<JsonAdapter, toml::de::Error> {
    let mut value: serde_json::Value = toml::from_str(text)?;
    flatten_datetimes(&mut value);
    Ok(JsonAdapter::from_json_value(value))
}

/// The `toml` crate tunnels every datetime value through serde as a
/// single-field struct keyed by a private marker, so deserializing
/// straight into a [`serde_json::Value`] materializes each datetime
/// as an object `{"$__toml_private_datetime": "…"}` instead of a
/// scalar — the internal key then leaks into the arbor. The JSON
/// data model has no datetime type, so collapse those wrappers back
/// to the plain datetime string (matching how JSON itself carries a
/// timestamp), leaving the rest of the tree untouched.
fn flatten_datetimes(value: &mut serde_json::Value) {
    // The field name `toml_datetime` serializes a datetime under.
    const TOML_DATETIME_FIELD: &str = "$__toml_private_datetime";
    match value {
        serde_json::Value::Object(map) => {
            let datetime = if map.len() == 1 {
                match map.get(TOML_DATETIME_FIELD) {
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(s) = datetime {
                *value = serde_json::Value::String(s);
                return;
            }
            for child in map.values_mut() {
                flatten_datetimes(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items.iter_mut() {
                flatten_datetimes(child);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_tunnel_collapses_to_scalar_string() {
        // toml deserializes a datetime as a single-field struct; a
        // naive parse leaves `when` as an object hiding the private
        // key. After flattening it is the plain datetime string.
        let mut v: serde_json::Value =
            toml::from_str("when = 2026-07-14T10:00:00Z\n").unwrap();
        assert!(v["when"].is_object(), "precondition: raw datetime tunnel");
        flatten_datetimes(&mut v);
        assert_eq!(
            v["when"],
            serde_json::Value::String("2026-07-14T10:00:00Z".to_string())
        );
    }

    #[test]
    fn nested_and_array_datetimes_flatten() {
        let mut v: serde_json::Value = toml::from_str(
            "stamps = [2026-07-14T10:00:00Z, 2026-07-15]\n\
             [meta]\ncreated = 1979-05-27\n",
        )
        .unwrap();
        flatten_datetimes(&mut v);
        assert!(v["stamps"][0].is_string());
        assert!(v["stamps"][1].is_string());
        assert!(v["meta"]["created"].is_string());
    }

    #[test]
    fn plain_values_are_untouched() {
        let mut v: serde_json::Value =
            toml::from_str("name = \"quarb\"\ncount = 3\nok = true\n").unwrap();
        let before = v.clone();
        flatten_datetimes(&mut v);
        assert_eq!(v, before);
    }

    #[test]
    fn parse_builds_adapter_for_datetime_document() {
        assert!(parse("when = 2026-07-14T10:00:00Z\n").is_ok());
    }
}
