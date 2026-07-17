//! The scalar world.
//!
//! Navigation produces nodes; a projection turns each node into a
//! [`Value`]. Quarb is value-domain agnostic, so this is a small,
//! open set of scalar shapes an adapter can return.

use std::fmt;

/// A projected scalar value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Absent / undefined.
    Null,
    /// A boolean (e.g. `:::is-leaf`).
    Bool(bool),
    /// An integer (e.g. `::;size`, `:::index`).
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A string (e.g. `:::name`, file content).
    Str(String),
    /// A list of values (produced by aggregating pipeline functions).
    List(Vec<Value>),
    /// A record: insertion-ordered named fields (spec: The Record
    /// Scalar). Constructed by `record(...)`; field order is
    /// significant, matching kaiv namespaces and JSON key order.
    Record(Vec<(String, Value)>),
    /// A point on the UTC timeline (spec: The Temporal Fragment).
    /// `offset_min` preserves the source's UTC offset for display
    /// only — comparison is always on the timeline. Displays as
    /// ISO-8601; a midnight instant with no offset prints as a
    /// bare date.
    Instant {
        secs: i64,
        nanos: u32,
        offset_min: Option<i16>,
    },
    /// A span of time (instant minus instant; the `days(n)` family).
    /// Displays as an ISO-8601 duration (`P1DT2H`).
    Duration {
        secs: i64,
        nanos: u32,
    },
    /// A value on a dimension (spec: The Quantital Fragment):
    /// the magnitude scaled to the dimension's SI-base expansion
    /// (`base`, e.g. `m`, `kg*m^2/s^3`), the written form kept
    /// for display only — exactly as instants keep their written
    /// offset. Minted by unit-aware adapters (kaiv); compared and
    /// combined on the base, so `42 km` orders above `5000 m`.
    Quantity {
        value: f64,
        base: String,
        /// The authored magnitude and unit, for display
        /// (`42 km`); absent for computed quantities, which
        /// display in the base.
        written: Option<(f64, String)>,
    },
}

impl Value {
    /// Truthiness, for predicate coercion: `Null`, `false`, `0`, the
    /// empty string, and the empty list and record are falsy;
    /// everything else is truthy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Record(o) => !o.is_empty(),
            Value::Instant { .. } => true,
            Value::Duration { secs, nanos } => *secs != 0 || *nanos != 0,
            Value::Quantity { value, .. } => *value != 0.0,
        }
    }

    /// The *temporal reading* (spec: The Temporal Fragment): the UTC
    /// timeline point a value denotes, if any — an instant itself,
    /// an integer or float read as epoch seconds, or ISO-8601 text.
    /// Booleans, lists, records, durations, and non-ISO text have
    /// none.
    pub fn temporal_reading(&self) -> Option<(i64, u32)> {
        match self {
            Value::Instant { secs, nanos, .. } => Some((*secs, *nanos)),
            Value::Int(n) => Some((*n, 0)),
            Value::Float(f) if f.is_finite() => {
                let secs = f.floor() as i64;
                let nanos = ((f - f.floor()) * 1e9) as u32;
                Some((secs, nanos))
            }
            Value::Str(s) => crate::temporal::parse_iso(s).map(|(s, n, _)| (s, n)),
            _ => None,
        }
    }

    /// The *unital reading* (spec: The Unital Reading): the
    /// dimensioned magnitude a value denotes, if any — a quantity
    /// itself, or unit text (`5km`, `0.2 kW`) scaled through the
    /// frozen built-in table. Bare numbers read as the *partner's*
    /// base at the comparison site (they carry no dimension of
    /// their own), so they are not read here.
    pub fn unital_reading(&self) -> Option<(f64, String)> {
        match self {
            Value::Quantity { value, base, .. } => Some((*value, base.clone())),
            Value::Str(s) => crate::quantity::parse_unit_text(s)
                .map(|(v, base, ..)| (v, base.to_string())),
            _ => None,
        }
    }

    /// The *durational reading* (spec: The Durational Reading): the
    /// span a value denotes, if any — a duration itself, a number
    /// read as seconds, or span text (an ISO-8601 duration or a
    /// systemd time-span like `5d3h5min`). Booleans, lists, records,
    /// instants, and other text have none.
    pub fn durational_reading(&self) -> Option<(i64, u32)> {
        match self {
            Value::Duration { secs, nanos } => Some((*secs, *nanos)),
            Value::Int(n) => Some((*n, 0)),
            Value::Float(f) if f.is_finite() => {
                let secs = f.floor() as i64;
                let nanos = ((f - f.floor()) * 1e9) as u32;
                Some((secs, nanos))
            }
            Value::Str(s) => crate::temporal::parse_span(s),
            _ => None,
        }
    }

    /// Strict-JSON rendering: strings quoted and escaped, numbers and
    /// booleans bare, null literal, lists as arrays, records as
    /// key-ordered JSON objects. This is the `| json` serialization
    /// and the display form of records, making a record stream
    /// JSONL.
    pub fn to_json(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => format_float(*f),
            Value::Str(s) => json_string(s),
            Value::List(l) => {
                let inner: Vec<String> = l.iter().map(Value::to_json).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Record(o) => {
                let inner: Vec<String> = o
                    .iter()
                    .map(|(k, v)| format!("{}: {}", json_string(k), v.to_json()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
            Value::Instant { .. } | Value::Duration { .. } | Value::Quantity { .. } => {
                json_string(&self.to_string())
            }
        }
    }

    /// The numeric value, for numeric comparison and aggregation.
    /// `Int` and `Float` coerce, and so does a string that parses as a
    /// finite number\,---\,without which numeric work would silently
    /// fail over text adapters, where every value is a string (an HTML
    /// attribute like `rank="3"`, a CSV cell). Non-numeric strings,
    /// booleans, lists, and null do not coerce.
    pub fn numeric(&self) -> Option<f64> {
        match self {
            Value::Int(n) => Some(*n as f64),
            Value::Float(f) => Some(*f),
            Value::Str(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
            // A quantity's base magnitude, so aggregates work
            // (`::power @| sum` sums watts). Arithmetic never sees
            // this: the quantital fragment intercepts first.
            Value::Quantity { value, .. } => Some(*value),
            _ => None,
        }
    }

    /// The *numeric reading* (spec: The Numeric Fragment) — like
    /// [`Value::numeric`], but type-preserving: text with integer
    /// form reads as an exact `Int`, so arithmetic over text-sourced
    /// values stays exact. At most one reading exists; booleans,
    /// lists, and null have none.
    pub fn numeric_reading(&self) -> Option<Value> {
        match self {
            Value::Int(n) => Some(Value::Int(*n)),
            Value::Float(f) => Some(Value::Float(*f)),
            Value::Str(s) => {
                let s = s.trim();
                if let Ok(n) = s.parse::<i64>() {
                    Some(Value::Int(n))
                } else {
                    s.parse::<f64>()
                        .ok()
                        .filter(|f| f.is_finite())
                        .map(Value::Float)
                }
            }
            _ => None,
        }
    }

    /// A total order for sorting: timeline when either side is an
    /// instant (the other coercing via its temporal reading), span
    /// when either side is a duration (the other coercing via its
    /// durational reading), numeric when both are numeric, else by
    /// string form.
    pub fn compare(&self, other: &Value) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        if matches!(self, Value::Instant { .. }) || matches!(other, Value::Instant { .. }) {
            if let (Some(a), Some(b)) = (self.temporal_reading(), other.temporal_reading()) {
                return a.cmp(&b);
            }
        }
        if matches!(self, Value::Duration { .. }) || matches!(other, Value::Duration { .. }) {
            if let (Some(a), Some(b)) = (self.durational_reading(), other.durational_reading()) {
                return a.cmp(&b);
            }
        }
        if matches!(self, Value::Quantity { .. }) || matches!(other, Value::Quantity { .. }) {
            if let Some((a, b)) = quantital_pair(self, other) {
                return a.partial_cmp(&b).unwrap_or(Ordering::Equal);
            }
        }
        if let (Some(x), Some(y)) = (self.numeric(), other.numeric()) {
            return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
        }
        self.to_string().cmp(&other.to_string())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => Ok(()),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "{}", parts.join(", "))
            }
            // Records display as strict JSON, so a stream of record
            // topics prints as JSONL.
            Value::Record(_) => write!(f, "{}", self.to_json()),
            Value::Instant {
                secs,
                nanos,
                offset_min,
            } => write!(
                f,
                "{}",
                crate::temporal::format_instant(*secs, *nanos, *offset_min)
            ),
            Value::Duration { secs, nanos } => {
                write!(f, "{}", crate::temporal::format_duration(*secs, *nanos))
            }
            // The written form when the source had one (`42 km`),
            // the base otherwise; single-factor forms re-parse
            // through the unital reading.
            Value::Quantity {
                value,
                base,
                written,
            } => match written {
                Some((v, u)) => write!(f, "{} {}", format_float(*v), u),
                None => write!(f, "{} {}", format_float(*value), base),
            },
        }
    }
}

/// The base-unit magnitudes of a comparison in which at least one
/// side is a quantity (spec: The Unital Reading). The other side
/// coerces through its unital reading; a bare number (or numeric
/// text) reads as the *partner's* base, since it carries no
/// dimension of its own. Dimensions must agree — a base mismatch
/// is no pair, so the comparison fails rather than lies.
pub(crate) fn quantital_pair(a: &Value, b: &Value) -> Option<(f64, f64)> {
    quantital_pair_with(a, b, &crate::quantity::scale_expr)
}

/// [`quantital_pair`] with an explicit unit-expression resolver —
/// the adapter's `unit_scale`, so criterion text may use the
/// mounted document's custom units.
pub(crate) fn quantital_pair_with(
    a: &Value,
    b: &Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<(f64, f64)> {
    let read = |v: &Value| -> Option<(f64, String)> {
        match v {
            Value::Quantity { value, base, .. } => Some((*value, base.clone())),
            Value::Str(s) => {
                crate::quantity::parse_unit_text_with(s, scale).map(|(bv, b, ..)| (bv, b))
            }
            _ => None,
        }
    };
    match (read(a), read(b)) {
        (Some((va, ba)), Some((vb, bb))) => (ba == bb).then_some((va, vb)),
        (Some((va, _)), None) => b.numeric().map(|n| (va, n)),
        (None, Some((vb, _))) => a.numeric().map(|n| (n, vb)),
        (None, None) => None,
    }
}

/// Render a float the way `Display` does (no trailing `.0` for whole
/// values); finite by construction everywhere quarb produces floats.
fn format_float(f: f64) -> String {
    f.to_string()
}

/// JSON-quote a string (the escapes JSON requires; forward slash
/// left alone).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rendering() {
        let obj = Value::Record(vec![
            ("name".into(), Value::Str("A\"da\n".into())),
            ("age".into(), Value::Int(36)),
            ("score".into(), Value::Float(2.5)),
            ("ok".into(), Value::Bool(true)),
            ("gone".into(), Value::Null),
            (
                "tags".into(),
                Value::List(vec![Value::Str("x".into()), Value::Int(1)]),
            ),
        ]);
        assert_eq!(
            obj.to_json(),
            r#"{"name": "A\"da\n", "age": 36, "score": 2.5, "ok": true, "gone": null, "tags": ["x", 1]}"#
        );
        // Display of a record is its JSON; an empty record is falsy
        assert_eq!(obj.to_string(), obj.to_json());
        assert!(!Value::Record(Vec::new()).is_truthy());
        // top-level list display is unchanged (join, not JSON)
        assert_eq!(
            Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]).to_string(),
            "a, b"
        );
    }
}
