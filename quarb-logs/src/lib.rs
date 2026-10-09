//! `quarb-logs` — what the log adapters share (Cloud Logging,
//! CloudWatch, Datadog, Azure Monitor, Cloudflare): the decoded
//! field tree, the JSON decoding into it, the timestamp reading,
//! the percent-decoding of a target's query string, and the
//! duration spellings of a `since=` window.

use quarb::Value;
use quarb::temporal::parse_iso;
use serde_json::Value as Json;

/// One decoded field of a log record: a scalar, a map of fields,
/// or a list of them.
#[derive(Clone, Debug)]
pub enum Field {
    Scalar(Value),
    Map(Vec<(String, Field)>),
    List(Vec<Field>),
}

impl Field {
    pub fn scalar(&self) -> Option<Value> {
        match self {
            Field::Scalar(v) => Some(v.clone()),
            _ => None,
        }
    }
}

/// A JSON value as a field tree: numbers and booleans as scalars,
/// strings through [`str_value`], arrays and objects recursively.
pub fn decode_json(v: &Json) -> Field {
    match v {
        Json::Null => Field::Scalar(Value::Null),
        Json::Bool(b) => Field::Scalar(Value::Bool(*b)),
        Json::Number(n) => Field::Scalar(match n.as_i64() {
            Some(i) => Value::Int(i),
            None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        }),
        Json::String(s) => Field::Scalar(str_value(s)),
        Json::Array(a) => Field::List(a.iter().map(decode_json).collect()),
        Json::Object(o) => Field::Map(o.iter().map(|(k, v)| (k.clone(), decode_json(v))).collect()),
    }
}

/// Full RFC 3339 timestamps become instants; other strings stay
/// strings.
pub fn str_value(s: &str) -> Value {
    if s.contains('T')
        && let Some((secs, nanos, offset_min)) = parse_iso(s)
    {
        return Value::Instant {
            secs,
            nanos,
            offset_min,
        };
    }
    Value::Str(s.to_string())
}

/// `%XX` sequences of a target's query string decoded (bytes that
/// are not UTF-8 replaced).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `s` is a duration spelling: digits and one of `s`, `m`,
/// `h`, `d`.
pub fn is_duration(s: &str) -> bool {
    s.len() >= 2
        && s.ends_with(['s', 'm', 'h', 'd'])
        && s[..s.len() - 1].chars().all(|c| c.is_ascii_digit())
}

/// A `since=` duration in seconds: `30m`, `2h`, `7d`.
pub fn duration_secs(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    Some(match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return None,
    })
}

/// The same duration in milliseconds.
pub fn duration_ms(s: &str) -> Option<i64> {
    duration_secs(s).map(|n| n * 1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_decoding() {
        assert_eq!(duration_secs("30m"), Some(1800));
        assert_eq!(duration_ms("2s"), Some(2000));
        assert_eq!(duration_secs("x"), None);
        assert!(is_duration("7d") && !is_duration("7"));
        assert_eq!(percent_decode("a%20b"), "a b");
        assert!(matches!(
            str_value("2024-01-02T03:04:05Z"),
            Value::Instant { .. }
        ));
    }
}
