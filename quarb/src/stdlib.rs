//! The pipeline standard library.
//!
//! Functions come in two families, matching the two pipe operators:
//!
//! - **Scalar** functions (`|`) transform one capsa's topic; some
//!   expand it into several values (e.g. `lines`).
//! - **Aggregate** functions (`@|`) reduce the whole context's topics
//!   to a new list.

use crate::ast::{Arg, FnCall};
use crate::value::Value;

/// The unit-expression resolver threaded from the executor (the
/// adapter's `unit_scale`), so aggregates read custom units exactly
/// as criteria and ordering do.
type Scale<'a> = &'a dyn Fn(&str) -> Option<(f64, String)>;

const SCALAR: &[&str] = &[
    "upper",
    "lower",
    "trim",
    "chars",
    "wc",
    "lines",
    "words",
    "split",
    "round",
    "floor",
    "ceil",
    "abs",
    "json",
    "xml",
    "record",
    "rec",
    "default",
    // The temporal fragment.
    "datetime",
    "epoch",
    "isoformat",
    "year",
    "month",
    "day",
    "hour",
    "minute",
    "second",
    "weekday",
    "date",
    "seconds",
    "minutes",
    "hours",
    "days",
    "duration",
    "td",
    "strptime",
    "tp",
    "quantity",
    "convert",
    "isodate",
    "isomonth",
    "isoweek",
    "strftime",
    "tfmt",
    "sh",
    "sha256",
    "base64",
    "base64url",
    "base32",
    "crockford32",
    "hex",
    "decode",
    "dec",
];

const AGGREGATE: &[&str] = &[
    "count", "sum", "product", "min", "max", "mean", "avg", "median", "stddev", "variance", "sort",
    "unique", "reverse", "first", "last", "join", "ungroup", "window", "shift",
];

/// Whether `name` is a whole-context stage that never works per
/// capsa — it rides `@|` only. (Keyed and reducing aggregates also
/// take the plain pipe, working per capsa on a group's members.)
pub fn context_only(name: &str) -> bool {
    matches!(name, "ungroup" | "window" | "shift")
}

/// Keyed aggregates reorder or filter the capsae themselves (nodes
/// and registers preserved), keyed by per-capsa value expressions.
/// They are evaluated in the executor, which has the adapter at
/// hand; this module only names them.
const KEYED: &[&str] = &[
    "sort_by",
    "unique_by",
    "min_by",
    "max_by",
    "top",
    "bottom",
    "group",
];

/// The collator for a `sort(LOCALE)` call, where one was given
/// (the parser validated the tag's shape; root on surprise — a
/// tag colligo's registry doesn't know or support still sorts,
/// under the untailored base table). Approximate-tier locales
/// are accepted: a query sort wants a reasonable total order,
/// not certified fidelity.
#[cfg(feature = "colligo")]
pub(crate) fn collator_for(call: &FnCall) -> Option<colligo::Collator> {
    let tag = match call.args.first()? {
        crate::ast::Arg::Lit(l) => l.to_string(),
        _ => return None,
    };
    Some(
        colligo::Collator::builder(&tag)
            .allow_approximate(true)
            .build()
            .unwrap_or_else(|_| colligo::Collator::root()),
    )
}

/// Collation compiled out (`colligo` feature off): never a
/// collator, so both `sort(LOCALE)` call sites fall back to the
/// standard value comparison (codepoint order for text). The
/// uninhabited stub keeps the call sites feature-agnostic.
#[cfg(not(feature = "colligo"))]
pub(crate) struct NeverCollator(std::convert::Infallible);

#[cfg(not(feature = "colligo"))]
impl NeverCollator {
    pub(crate) fn compare(&self, _a: &str, _b: &str) -> std::cmp::Ordering {
        match self.0 {}
    }
}

#[cfg(not(feature = "colligo"))]
pub(crate) fn collator_for(_call: &FnCall) -> Option<NeverCollator> {
    None
}

/// Whether `name` is a keyed aggregate (for `@|`).
pub fn known_keyed(name: &str) -> bool {
    KEYED.contains(&name)
}

/// Whether `name` is a per-capsa scalar function (for `|`).
pub fn known_scalar(name: &str) -> bool {
    SCALAR.contains(&name)
}

/// Whether `name` is an aggregate function (for `@|`).
pub fn known_agg(name: &str) -> bool {
    AGGREGATE.contains(&name) || known_keyed(name)
}

/// Apply a per-capsa scalar function to a single topic. May return
/// several values (an expanding function like `lines`).
pub fn apply_scalar(
    call: &FnCall,
    topic: Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
    let text = topic.to_string();
    match call.name.as_str() {
        "upper" => vec![Value::Str(text.to_uppercase())],
        "lower" => vec![Value::Str(text.to_lowercase())],
        "trim" => vec![Value::Str(text.trim().to_string())],
        "chars" => vec![Value::Int(text.chars().count() as i64)],
        "wc" => vec![Value::Int(text.split_whitespace().count() as i64)],
        "lines" => text.lines().map(|s| Value::Str(s.to_string())).collect(),
        "words" => text
            .split_whitespace()
            .map(|s| Value::Str(s.to_string()))
            .collect(),
        "split" => {
            let sep = arg_str(call, 0, ",");
            text.split(sep).map(|s| Value::Str(s.to_string())).collect()
        }

        // Hashing and encodings (RFC 4648; SHA-256 per FIPS
        // 180-4), over the topic's text bytes.
        "sha256" => vec![Value::Str(crate::encoding::sha256_hex(text.as_bytes()))],
        "base64" => vec![Value::Str(crate::encoding::base64(text.as_bytes()))],
        "base64url" => vec![Value::Str(crate::encoding::base64url(text.as_bytes()))],
        "base32" => vec![Value::Str(crate::encoding::base32(text.as_bytes()))],
        "crockford32" => vec![Value::Str(crate::encoding::crockford32(text.as_bytes()))],
        // `decode(SCHEME)` (alias `dec`) inverts the reversible
        // encodings: the parser validated the scheme, so a bad one
        // is unreachable here. Malformed input or bytes that are
        // not UTF-8 become null (propagating — strings are text).
        "decode" | "dec" => {
            let scheme = arg_str(call, 0, "");
            // Structured formats parse to a value (record/list/
            // scalar); the byte encodings decode to text. Malformed
            // input, or non-UTF-8 bytes, become null.
            vec![match scheme {
                "json" => crate::encoding::json_to_value(&text).unwrap_or(Value::Null),
                "yaml" => crate::encoding::yaml_to_value(&text).unwrap_or(Value::Null),
                "toml" => crate::encoding::toml_to_value(&text).unwrap_or(Value::Null),
                "xml" => crate::encoding::xml_to_value(&text).unwrap_or(Value::Null),
                _ => crate::encoding::decode(scheme, &text)
                    .and_then(|b| String::from_utf8(b).ok())
                    .map(Value::Str)
                    .unwrap_or(Value::Null),
            }]
        }
        "hex" => vec![Value::Str(crate::encoding::hex(text.as_bytes()))],

        // Temporal scalars (spec: The Temporal Fragment). All are
        // defined via the temporal reading, so they serve typed
        // instants, epoch integers, and ISO text alike; no reading
        // means null.
        "datetime" => vec![match &topic {
            Value::Instant { .. } => topic.clone(),
            Value::Str(s) => crate::temporal::parse_iso(s)
                .map(|(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                })
                .unwrap_or(Value::Null),
            other => other
                .temporal_reading()
                .map(|(secs, nanos)| Value::Instant {
                    secs,
                    nanos,
                    offset_min: None,
                })
                .unwrap_or(Value::Null),
        }],
        "epoch" => vec![
            topic
                .temporal_reading()
                .map(|(secs, _)| Value::Int(secs))
                .unwrap_or(Value::Null),
        ],
        // `isoformat` is the reading, formatted UTC (spec: The
        // Temporal Fragment) — a typed instant renders in UTC, not
        // its written display offset, so it agrees with the text and
        // epoch paths rather than short-circuiting to Display.
        "isoformat" => vec![
            topic
                .temporal_reading()
                .map(|(s, n)| Value::Str(crate::temporal::format_instant(s, n, None)))
                .unwrap_or(Value::Null),
        ],
        "year" | "month" | "day" | "hour" | "minute" | "second" => {
            vec![
                topic
                    .temporal_reading()
                    .map(|(secs, _)| {
                        let (y, mo, d, h, mi, se) = crate::temporal::components(secs);
                        Value::Int(match call.name.as_str() {
                            "year" => y,
                            "month" => mo as i64,
                            "day" => d as i64,
                            "hour" => h as i64,
                            "minute" => mi as i64,
                            _ => se as i64,
                        })
                    })
                    .unwrap_or(Value::Null),
            ]
        }
        "isodate" | "isomonth" | "isoweek" => vec![
            topic
                .temporal_reading()
                .map(|(secs, _)| {
                    let (y, mo, d, ..) = crate::temporal::components(secs);
                    Value::Str(match call.name.as_str() {
                        "isodate" => format!("{y:04}-{mo:02}-{d:02}"),
                        "isomonth" => format!("{y:04}-{mo:02}"),
                        _ => {
                            let (gy, gw) = crate::temporal::iso_week(secs);
                            format!("{gy:04}-W{gw:02}")
                        }
                    })
                })
                .unwrap_or(Value::Null),
        ],
        // `strftime("%Y-%m-%d %H:%M")` (alias `tfmt`) — the
        // C/POSIX formatting standard Perl, Python, and Ruby
        // share, in the instant's own offset.
        "strftime" | "tfmt" => {
            let fmt = arg_str(call, 0, "%Y-%m-%dT%H:%M:%S").to_string();
            vec![match &topic {
                Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                } => Value::Str(crate::temporal::strftime(&fmt, *secs, *nanos, *offset_min)),
                other => other
                    .temporal_reading()
                    .map(|(s, n)| Value::Str(crate::temporal::strftime(&fmt, s, n, None)))
                    .unwrap_or(Value::Null),
            }]
        }
        "weekday" => vec![
            topic
                .temporal_reading()
                .map(|(secs, _)| Value::Int(crate::temporal::weekday(secs) as i64))
                .unwrap_or(Value::Null),
        ],
        "date" => vec![
            topic
                .temporal_reading()
                .map(|(secs, _)| Value::Instant {
                    secs: secs.div_euclid(86400) * 86400,
                    nanos: 0,
                    offset_min: None,
                })
                .unwrap_or(Value::Null),
        ],
        // Duration constructors: a numeric topic scales the unit
        // (`30 | days` = P30D).
        "seconds" | "minutes" | "hours" | "days" => {
            let unit: f64 = match call.name.as_str() {
                "seconds" => 1.0,
                "minutes" => 60.0,
                "hours" => 3600.0,
                _ => 86400.0,
            };
            vec![
                topic
                    .numeric()
                    .map(|n| {
                        let total = n * unit;
                        Value::Duration {
                            secs: total.floor() as i64,
                            nanos: ((total - total.floor()) * 1e9) as u32,
                        }
                    })
                    .unwrap_or(Value::Null),
            ]
        }

        // `duration` (alias `td`) — the span parser, defined via the
        // durational reading: span text (`5d3h5min`, `P5DT3H5M`) or
        // a number (seconds) to a duration, a duration passing
        // through. The untyped substrate's explicit opt-in, exactly
        // as `| datetime` is for instants.
        "duration" | "td" => vec![
            topic
                .durational_reading()
                .or_else(|| match &topic {
                    // The mounted unit table's time units read too
                    // (`5rep`), builtin span text having had first
                    // claim — same order as comparisons.
                    Value::Str(s) => crate::temporal::span_from_units(s, scale),
                    _ => None,
                })
                .map(|(secs, nanos)| Value::Duration { secs, nanos })
                .unwrap_or(Value::Null),
        ],
        // `strptime(fmt)` (alias `tp`) — strftime's inverse: a TEXT
        // topic parsed per the same C/POSIX specifiers with the same
        // fixed English names. A parsed %z is kept for display;
        // fields the format omits default to the Unix epoch's;
        // non-text topics and non-matching text are null.
        "strptime" | "tp" => {
            let fmt = arg_str(call, 0, "%Y-%m-%dT%H:%M:%S").to_string();
            vec![match &topic {
                Value::Str(s) => crate::temporal::strptime(s, &fmt)
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                    })
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            }]
        }

        // `quantity` — the unit-text parser, defined via the unital
        // reading: `5km` / `0.2 kW` to a quantity, a quantity
        // passing through, no reading null. The untyped substrate's
        // explicit opt-in, as `| datetime` and `| duration` are for
        // their fragments.
        "quantity" => vec![match &topic {
            Value::Quantity { .. } => topic.clone(),
            Value::Str(s) => crate::quantity::parse_unit_text_with(s, scale)
                .map(|(value, base, wv, wu)| Value::Quantity {
                    value,
                    base,
                    written: Some((wv, wu)),
                })
                .unwrap_or(Value::Null),
            _ => Value::Null,
        }],
        // `convert(unit)` — explicit unit conversion: a quantity
        // re-expressed in a compatible unit (the base magnitude is
        // untouched; only the written display form changes). A
        // dimension mismatch or a non-quantity topic is null —
        // this is also the sanctioned road where arithmetic
        // refuses to guess (numbers never lift into ±).
        "convert" => {
            let target = arg_str(call, 0, "").to_string();
            vec![match &topic {
                Value::Quantity { value, base, .. } => match scale(&target) {
                    Some((factor, tbase)) if &tbase == base => Value::Quantity {
                        value: *value,
                        base: base.clone(),
                        written: Some((*value / factor, target)),
                    },
                    _ => Value::Null,
                },
                _ => Value::Null,
            }]
        }

        // Numeric scalars: a non-numeric topic becomes Null. A
        // QUANTITY rounds in its display unit and stays a quantity
        // — `(::speed | convert('km/h')) | round` is a whole
        // number of km/h, not of base meters-per-second.
        "round" | "floor" | "ceil" if matches!(topic, Value::Quantity { .. }) => {
            let Value::Quantity {
                value,
                base,
                written,
            } = &topic
            else {
                unreachable!()
            };
            let (wv, wu) = written.clone().unwrap_or_else(|| (*value, base.clone()));
            let rounded = match call.name.as_str() {
                "round" => wv.round(),
                "floor" => wv.floor(),
                _ => wv.ceil(),
            };
            let factor = scale(&wu).map(|(f, _)| f).unwrap_or(1.0);
            vec![Value::Quantity {
                value: rounded * factor,
                base: base.clone(),
                written: Some((rounded, wu)),
            }]
        }
        "round" => vec![numeric_scalar(&topic, |n| Value::Int(n.round() as i64))],
        "floor" => vec![numeric_scalar(&topic, |n| Value::Int(n.floor() as i64))],
        "ceil" => vec![numeric_scalar(&topic, |n| Value::Int(n.ceil() as i64))],
        "abs" => vec![match topic {
            Value::Int(n) => Value::Int(n.abs()),
            // A quantity has no numeric reading and only the rounders
            // are quantity-aware (spec: The Quantital Fragment), so
            // `abs` over a quantity is null rather than silently
            // dropping its unit to a bare base-magnitude float.
            Value::Quantity { .. } => Value::Null,
            other => numeric_scalar(&other, |n| Value::Float(n.abs())),
        }],
        // `s/pat/repl/mods` — regex substitution on the topic's
        // text. Sed semantics: first occurrence by default, `g`
        // global, `i` case-insensitive; `$1`-style capture
        // references in the replacement. The pattern was validated
        // at parse time; a failed recompile passes the topic through.
        "s" => {
            let pattern = arg_str(call, 0, "");
            let replacement = arg_str(call, 1, "");
            let mods = arg_str(call, 2, "");
            let case = if mods.contains('i') { "(?i)" } else { "" };
            let Ok(re) = regex::Regex::new(&format!("{case}{pattern}")) else {
                return vec![topic];
            };
            let out = if mods.contains('g') {
                re.replace_all(&text, replacement)
            } else {
                re.replace(&text, replacement)
            };
            vec![Value::Str(out.into_owned())]
        }
        // `default(v)` — replace a null topic with `v` (pandas'
        // fillna, jq's `//`); non-null topics pass through.
        "default" => vec![match topic {
            Value::Null => call
                .args
                .first()
                .and_then(|a| match a {
                    Arg::Lit(v) => Some(v.clone()),
                    Arg::Expr(_) | Arg::Range(_, _) => None,
                })
                .unwrap_or(Value::Null),
            other => other,
        }],
        // Serialize the topic as strict JSON text. (`record` is also
        // named here but constructed in the executor, which has the
        // adapter at hand for its expression arguments.)
        "json" => vec![Value::Str(topic.to_json())],
        _ => vec![topic],
    }
}

/// Apply `f` to the topic's numeric value, or `Null` when it has
/// none.
fn numeric_scalar(topic: &Value, f: impl Fn(f64) -> Value) -> Value {
    topic.numeric().map(f).unwrap_or(Value::Null)
}

/// Apply a reducing aggregate to the whole value list. The
/// order/selection family (`sort`, `unique`, `reverse`, `first`,
/// `last`) and the keyed aggregates are capsa-preserving and live in
/// the executor instead.
pub fn apply(call: &FnCall, input: Vec<Value>, scale: Scale) -> Vec<Value> {
    match call.name.as_str() {
        "count" => vec![Value::Int(input.len() as i64)],
        "sum" => vec![sum(&input, scale)],
        "product" => vec![product(&input)],
        "min" => extreme(input, std::cmp::Ordering::Less, scale),
        "max" => extreme(input, std::cmp::Ordering::Greater, scale),
        "mean" | "avg" => vec![mean(&input, scale)],
        "median" => vec![median(&input, scale)],
        "stddev" => vec![spread(&input, f64::sqrt, scale)],
        "variance" => vec![spread(&input, |v| v, scale)],
        "join" => vec![Value::Str(join(&input, arg_str(call, 0, "")))],
        // The order/selection family over an explicit value list —
        // reached from the per-capsa list reductions (`| last` on a
        // group's list topic); the `@|` forms are capsa-preserving
        // and intercepted in the executor before this point.
        // `sort` orders by the standard value comparison; with a
        // locale argument (`sort(ru-RU)` — any Unicode locale
        // identifier), values sort by their text under that
        // locale's collation (colligo; CLDR-derived). Reverse by
        // piping into `| reverse`.
        "sort" => {
            let mut v = input;
            match collator_for(call) {
                Some(c) => v.sort_by(|a, b| c.compare(&a.to_string(), &b.to_string())),
                None => v.sort_by(|a, b| a.compare_with(b, scale)),
            }
            v
        }
        "reverse" => {
            let mut v = input;
            v.reverse();
            v
        }
        "unique" => {
            let mut seen: Vec<String> = Vec::new();
            input
                .into_iter()
                .filter(|v| {
                    let k = v.to_string();
                    if seen.contains(&k) {
                        false
                    } else {
                        seen.push(k);
                        true
                    }
                })
                .collect()
        }
        "first" => input.into_iter().next().into_iter().collect(),
        "last" => input.into_iter().next_back().into_iter().collect(),
        _ => input,
    }
}

/// The sum of the numeric readings: values with no reading are
/// skipped as missing and an empty (or wholly non-numeric) input
/// reduces to null (spec: numeric reductions). Exact while every
/// operand is an integer, promoting to float on overflow — as
/// arithmetic does — rather than wrapping.
/// The durational fold's verdict: engaged when a typed duration is
/// present, refusing entirely if any element lacks a durational
/// reading — a silent partial total would lie.
enum DurFold {
    /// No typed duration present — the numeric path proceeds.
    Absent,
    /// Every element read as a span: fold these.
    Spans(Vec<(i64, u32)>),
    /// A typed duration mixed with span-less elements: refuse.
    Unsound,
}

fn durational_fold(input: &[Value]) -> DurFold {
    if !input.iter().any(|v| matches!(v, Value::Duration { .. })) {
        return DurFold::Absent;
    }
    match input.iter().map(Value::durational_reading).collect() {
        Some(spans) => DurFold::Spans(spans),
        None => DurFold::Unsound,
    }
}

/// The quantital fold's verdict: engaged when a typed quantity is
/// present. Unit text then lifts through the unital reading — the
/// fold is iterated `+`, and `Q + '500 m'` lifts — but every
/// element must land on one shared base, and a bare number refuses
/// exactly as `Q + 5` does: a cross-dimension fold (watts plus
/// meters) or a dimensionless stowaway would total nonsense
/// silently.
enum QuantFold {
    /// No quantities present — the numeric path proceeds.
    Absent,
    /// Every element read on this base: fold `mags` (the base
    /// magnitudes, in order) and mint the result on it. When every
    /// element was also *written* in one unit, `unit` carries it
    /// as (base-per-unit factor, name), so the total can display
    /// as `570.5 W` instead of the raw base expression.
    Same {
        base: String,
        unit: Option<(f64, String)>,
        mags: Vec<f64>,
    },
    /// Mixed dimensions, a bare number, or unreadable text: refuse.
    Unsound,
}

fn quantital_fold(input: &[Value], scale: Scale) -> QuantFold {
    if !input.iter().any(|v| matches!(v, Value::Quantity { .. })) {
        return QuantFold::Absent;
    }
    let mut base: Option<String> = None;
    let mut unit: Option<(f64, String)> = None;
    let mut unit_ok = true;
    let mut mags = Vec::with_capacity(input.len());
    for v in input {
        let (value, b, written): (f64, String, Option<(f64, String)>) = match v {
            Value::Quantity {
                value,
                base,
                written,
            } => (*value, base.clone(), written.clone()),
            Value::Str(s) => match crate::quantity::parse_unit_text_with(s, scale) {
                Some((bv, b, wmag, wunit)) => (bv, b, Some((wmag, wunit))),
                None => return QuantFold::Unsound,
            },
            _ => return QuantFold::Unsound,
        };
        match &base {
            None => base = Some(b),
            Some(prev) if *prev == b => {}
            Some(_) => return QuantFold::Unsound,
        }
        match (unit_ok, &written) {
            (true, Some((mag, u))) if *mag != 0.0 => match &unit {
                None => unit = Some((value / mag, u.clone())),
                Some((_, prev)) if prev == u => {}
                Some(_) => unit_ok = false,
            },
            _ => unit_ok = false,
        }
        mags.push(value);
    }
    match base {
        Some(base) => QuantFold::Same {
            base,
            unit: unit.filter(|_| unit_ok),
            mags,
        },
        None => QuantFold::Absent,
    }
}

/// Mint a fold's result on its base, re-expressed in the shared
/// written unit where one survived.
fn quantital_result(value: f64, base: String, unit: Option<(f64, String)>) -> Value {
    Value::Quantity {
        value,
        base,
        written: unit.map(|(f, u)| (value / f, u)),
    }
}

fn sum(input: &[Value], scale: Scale) -> Value {
    // Quantities total to a typed quantity in the base unit —
    // mixed units of one dimension welcome (kW + W + BTU/h), unit
    // text lifted alongside; mixed dimensions refused.
    match quantital_fold(input, scale) {
        QuantFold::Unsound => return Value::Null,
        QuantFold::Same { base, unit, mags } => {
            let value = mags.iter().sum();
            return quantital_result(value, base, unit);
        }
        QuantFold::Absent => {}
    }
    // Durations total to a typed duration (`PT15H45M`), the
    // durational counterpart.
    let spans = match durational_fold(input) {
        DurFold::Unsound => return Value::Null,
        DurFold::Spans(spans) => Some(spans),
        DurFold::Absent => None,
    };
    if let Some(spans) = spans {
        let mut secs: i64 = 0;
        let mut nanos: i64 = 0;
        for (s, n) in spans {
            match secs.checked_add(s) {
                Some(t) => secs = t,
                None => return Value::Null,
            }
            nanos += n as i64;
        }
        match secs.checked_add(nanos.div_euclid(1_000_000_000)) {
            Some(t) => secs = t,
            None => return Value::Null,
        }
        return Value::Duration {
            secs,
            nanos: nanos.rem_euclid(1_000_000_000) as u32,
        };
    }
    if !input.iter().any(|v| v.numeric().is_some()) {
        return Value::Null;
    }
    if input.iter().all(|v| matches!(v, Value::Int(_))) {
        let mut acc: i64 = 0;
        let mut overflowed = false;
        for v in input {
            if let Value::Int(n) = v {
                match acc.checked_add(*n) {
                    Some(s) => acc = s,
                    None => {
                        overflowed = true;
                        break;
                    }
                }
            }
        }
        if !overflowed {
            return Value::Int(acc);
        }
    }
    Value::Float(input.iter().filter_map(Value::numeric).sum())
}

/// The product of the numeric readings: exact while every operand
/// is an integer, promoting to float on overflow (as arithmetic
/// does) rather than wrapping; float otherwise. An empty input
/// multiplies to 1 (the fold identity).
fn product(input: &[Value]) -> Value {
    if input.iter().all(|v| matches!(v, Value::Int(_))) {
        let mut acc: i64 = 1;
        let mut overflowed = false;
        for v in input {
            if let Value::Int(n) = v {
                match acc.checked_mul(*n) {
                    Some(p) => acc = p,
                    None => {
                        overflowed = true;
                        break;
                    }
                }
            }
        }
        if !overflowed {
            return Value::Int(acc);
        }
    }
    Value::Float(input.iter().filter_map(Value::numeric).product())
}

fn mean(input: &[Value], scale: Scale) -> Value {
    // The mean of same-base quantities is a quantity on that base.
    match quantital_fold(input, scale) {
        QuantFold::Unsound => return Value::Null,
        QuantFold::Same { base, unit, mags } => {
            let value = mags.iter().sum::<f64>() / mags.len() as f64;
            return quantital_result(value, base, unit);
        }
        QuantFold::Absent => {}
    }
    // The mean of durations is a duration.
    let spans = match durational_fold(input) {
        DurFold::Unsound => return Value::Null,
        DurFold::Spans(spans) => Some(spans),
        DurFold::Absent => None,
    };
    if let Some(spans) = spans {
        let total: i128 = spans
            .iter()
            .map(|(s, n)| *s as i128 * 1_000_000_000 + *n as i128)
            .sum();
        let avg = total / spans.len() as i128;
        return Value::Duration {
            secs: (avg.div_euclid(1_000_000_000)) as i64,
            nanos: avg.rem_euclid(1_000_000_000) as u32,
        };
    }
    let nums: Vec<f64> = input.iter().filter_map(Value::numeric).collect();
    if nums.is_empty() {
        Value::Null
    } else {
        Value::Float(nums.iter().sum::<f64>() / nums.len() as f64)
    }
}

/// The middle numeric value; an even count averages the two middle
/// values. All-integer input with an odd count stays an integer.
fn median(input: &[Value], scale: Scale) -> Value {
    // The median of same-base quantities is a quantity on that
    // base; mixed dimensions refuse.
    match quantital_fold(input, scale) {
        QuantFold::Unsound => return Value::Null,
        QuantFold::Same {
            base,
            unit,
            mut mags,
        } => {
            // total_cmp: query arithmetic can mint NaN (float
            // overflow subtraction), which must not panic the sort.
            mags.sort_by(f64::total_cmp);
            let mid = mags.len() / 2;
            let value = if mags.len() % 2 == 1 {
                mags[mid]
            } else {
                (mags[mid - 1] + mags[mid]) / 2.0
            };
            return quantital_result(value, base, unit);
        }
        QuantFold::Absent => {}
    }
    // A typed duration mixed with span-less numerics would skip the
    // durations and report the median of the leftovers — refuse,
    // like the folds.
    if matches!(durational_fold(input), DurFold::Unsound) {
        return Value::Null;
    }
    let mut nums: Vec<f64> = input.iter().filter_map(Value::numeric).collect();
    if nums.is_empty() {
        return Value::Null;
    }
    nums.sort_by(f64::total_cmp);
    let mid = nums.len() / 2;
    if nums.len() % 2 == 1 {
        let m = nums[mid];
        if input.iter().all(|v| matches!(v, Value::Int(_))) {
            Value::Int(m as i64)
        } else {
            Value::Float(m)
        }
    } else {
        Value::Float((nums[mid - 1] + nums[mid]) / 2.0)
    }
}

/// Population variance, post-processed by `finish` (identity for
/// `variance`, square root for `stddev`).
fn spread(input: &[Value], finish: impl Fn(f64) -> f64, scale: Scale) -> Value {
    // Statistical moments over mixed dimensions are nonsense too.
    // Same-base spreads stay bare magnitudes for now (a stddev
    // carries the base's unit, a variance its square — typing them
    // is a ruling for the quantital round's next pass).
    let nums: Vec<f64> = match quantital_fold(input, scale) {
        QuantFold::Unsound => return Value::Null,
        // The engaged fold's magnitudes include lifted unit text,
        // which the numeric path below would silently drop.
        QuantFold::Same { mags, .. } => mags,
        QuantFold::Absent => {
            if matches!(durational_fold(input), DurFold::Unsound) {
                return Value::Null;
            }
            input.iter().filter_map(Value::numeric).collect()
        }
    };
    if nums.is_empty() {
        return Value::Null;
    }
    let n = nums.len() as f64;
    let m = nums.iter().sum::<f64>() / n;
    let var = nums.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n;
    Value::Float(finish(var))
}

/// Whether a value carries a reading in some fragment (numeric,
/// temporal, durational, or unital) — the readings [`Value::compare`]
/// orders by beyond its bare string fallback. A value with none (a
/// non-numeric string, a bool, null) is missing to the numeric
/// reductions.
fn has_reading(v: &Value, scale: Scale) -> bool {
    v.numeric().is_some()
        || v.temporal_reading().is_some()
        || v.durational_reading().is_some()
        || match v {
            // The unital probe pays the threaded resolver, so
            // custom-unit text is admitted exactly as it compares.
            Value::Str(s) => crate::quantity::parse_unit_text_with(s, scale).is_some(),
            Value::Quantity { .. } => true,
            _ => false,
        }
}

/// `min` / `max`: numeric reductions that coerce through the reading,
/// skip a value with no reading as missing (so a junk string can no
/// longer win via the comparison's string fallback), and reduce an
/// empty (or wholly readingless) input to null (spec).
fn extreme(input: Vec<Value>, want: std::cmp::Ordering, scale: Scale) -> Vec<Value> {
    // Ordering across dimensions is meaningless: a min over watts
    // and meters would pick by raw magnitude. Refuse, like the
    // numeric folds.
    if matches!(quantital_fold(&input, scale), QuantFold::Unsound) {
        return vec![Value::Null];
    }
    match input
        .into_iter()
        .filter(|v| has_reading(v, scale))
        .reduce(|a, b| {
            if a.compare_with(&b, scale) == want {
                a
            } else {
                b
            }
        }) {
        Some(v) => vec![v],
        None => vec![Value::Null],
    }
}

fn join(input: &[Value], sep: &str) -> String {
    input
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(sep)
}

/// The `n`-th call argument as a literal string, or `default` if
/// absent (or not a string literal).
fn arg_str<'a>(call: &'a FnCall, n: usize, default: &'a str) -> &'a str {
    match call.args.get(n) {
        Some(Arg::Lit(Value::Str(s))) => s,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(name: &str, input: Vec<Value>) -> Vec<Value> {
        let call = FnCall {
            name: name.into(),
            args: Vec::new(),
        };
        apply(&call, input, &crate::quantity::scale_expr)
    }

    fn sc(name: &str, topic: Value) -> Vec<Value> {
        let call = FnCall {
            name: name.into(),
            args: Vec::new(),
        };
        apply_scalar(&call, topic, &crate::quantity::scale_expr)
    }

    fn ints(ns: &[i64]) -> Vec<Value> {
        ns.iter().map(|&n| Value::Int(n)).collect()
    }

    #[test]
    fn mean_and_alias() {
        assert_eq!(agg("mean", ints(&[1, 2, 3, 4])), vec![Value::Float(2.5)]);
        assert_eq!(agg("avg", ints(&[1, 2, 3, 4])), vec![Value::Float(2.5)]);
        assert_eq!(agg("mean", vec![]), vec![Value::Null]);
    }

    #[test]
    fn quantital_folds_type_and_refuse() {
        let q = |value: f64, written: Option<(f64, &str)>| Value::Quantity {
            value,
            base: "kg*m^2/s^3".into(),
            written: written.map(|(m, u)| (m, u.to_string())),
        };
        // Same written unit: the total keeps it.
        let watts = vec![q(142.5, Some((142.5, "W"))), q(290.0, Some((290.0, "W")))];
        assert_eq!(
            agg("sum", watts.clone()),
            vec![q(432.5, Some((432.5, "W")))]
        );
        assert_eq!(agg("mean", watts), vec![q(216.25, Some((216.25, "W")))]);
        // Mixed units of one dimension: base magnitudes, no unit.
        let mixed = vec![q(1200.0, Some((1.2, "kW"))), q(350.0, Some((350.0, "W")))];
        assert_eq!(agg("sum", mixed.clone()), vec![q(1550.0, None)]);
        assert_eq!(agg("median", mixed), vec![q(775.0, None)]);
        // Mixed DIMENSIONS refuse — watts plus meters is nonsense.
        let bad = vec![
            q(100.0, Some((100.0, "W"))),
            Value::Quantity {
                value: 2000.0,
                base: "m".into(),
                written: Some((2.0, "km".into())),
            },
        ];
        for f in ["sum", "mean", "median", "min", "max", "stddev"] {
            assert_eq!(agg(f, bad.clone()), vec![Value::Null], "{f}");
        }
        // A dimensionless stowaway refuses too.
        assert_eq!(
            agg("sum", vec![q(1.0, None), Value::Int(5)]),
            vec![Value::Null]
        );
    }

    #[test]
    fn durational_sum_and_mean() {
        let d = |secs: i64| Value::Duration { secs, nanos: 0 };
        // Durations total and average to typed durations.
        assert_eq!(
            agg("sum", vec![d(2700), d(43200), d(10800)]),
            vec![d(56700)] // PT15H45M
        );
        assert_eq!(
            agg("mean", vec![d(2700), d(43200), d(10800)]),
            vec![d(18900)] // PT5H15M
        );
        // Mixed with span text and numbers-as-seconds: still spans.
        assert_eq!(
            agg(
                "sum",
                vec![d(60), Value::Str("2min".into()), Value::Int(60)]
            ),
            vec![d(240)]
        );
        // Any unreadable element refuses the whole fold — a silent
        // partial total would lie.
        assert_eq!(
            agg("sum", vec![d(60), Value::Str("soon".into())]),
            vec![Value::Null]
        );
        // Including a *numeric* span-less element: the refusal must
        // not fall through to a numeric total that skips the
        // durations (sum over [PT1M, "5"] is not 5).
        assert_eq!(
            agg("sum", vec![d(60), Value::Str("5".into())]),
            vec![Value::Null]
        );
        assert_eq!(
            agg("mean", vec![d(60), Value::Str("5".into())]),
            vec![Value::Null]
        );
        assert_eq!(
            agg("median", vec![d(60), Value::Str("5".into())]),
            vec![Value::Null]
        );
        assert_eq!(
            agg("stddev", vec![d(60), Value::Str("5".into())]),
            vec![Value::Null]
        );
    }

    #[test]
    fn compare_is_a_total_order() {
        // The old pairwise fallbacks cycled (10 < "1z" < "9" < 10);
        // the canonical key sorts the same triple deterministically:
        // the magnitude line first, readingless text after.
        assert_eq!(
            agg(
                "sort",
                vec![
                    Value::Int(10),
                    Value::Str("1z".into()),
                    Value::Str("9".into())
                ]
            ),
            vec![
                Value::Str("9".into()),
                Value::Int(10),
                Value::Str("1z".into())
            ]
        );
        // Rank order: null, booleans, the line (readings included),
        // then text.
        assert_eq!(
            agg(
                "sort",
                vec![
                    Value::Str("b".into()),
                    Value::Str("2h".into()),
                    Value::Int(5),
                    Value::Null,
                    Value::Bool(true),
                ]
            ),
            vec![
                Value::Null,
                Value::Bool(true),
                Value::Int(5),
                Value::Str("2h".into()), // 7200 s on the line
                Value::Str("b".into()),
            ]
        );
    }

    #[test]
    fn compare_with_pays_the_custom_resolver() {
        use std::cmp::Ordering;
        // "zorkmid" exists only in the custom table: unital order
        // (10 > 5) with it, string order ("10…" < "5…") without.
        let custom = |e: &str| (e == "zorkmid").then(|| (3.0, "m".to_string()));
        let (a, b) = (
            Value::Str("10 zorkmid".into()),
            Value::Str("5 zorkmid".into()),
        );
        assert_eq!(a.compare_with(&b, &custom), Ordering::Greater);
        assert_eq!(
            a.compare_with(&b, &crate::quantity::scale_expr),
            Ordering::Less
        );
    }

    #[test]
    fn quantital_fold_lifts_text_refuses_numbers() {
        let q = |value: f64, written: Option<(f64, &str)>| Value::Quantity {
            value,
            base: "B".into(),
            written: written.map(|(v, u)| (v, u.to_string())),
        };
        // A typed quantity engages the fold; unit text lifts, as in
        // ± arithmetic — and the shared written unit survives.
        let mixed = vec![
            q(10_000_000.0, Some((10.0, "MB"))),
            Value::Str("5MB".into()),
        ];
        assert_eq!(agg("sum", mixed.clone())[0].to_string(), "15 MB");
        assert_eq!(agg("mean", mixed.clone())[0].to_string(), "7.5 MB");
        assert_eq!(agg("min", mixed)[0].to_string(), "5MB");
        // A bare number refuses, exactly as `Q + 5` does…
        assert_eq!(
            agg("sum", vec![q(10_000_000.0, None), Value::Int(5)]),
            vec![Value::Null]
        );
        // …and so do cross-dimension text and readingless text.
        assert_eq!(
            agg("sum", vec![q(10_000_000.0, None), Value::Str("5km".into())]),
            vec![Value::Null]
        );
        assert_eq!(
            agg(
                "sum",
                vec![q(10_000_000.0, None), Value::Str("soon".into())]
            ),
            vec![Value::Null]
        );
    }

    #[test]
    fn extremes_pair_unit_text_unitally() {
        // Two unit texts order by magnitude, not lexicographically
        // ("5MB" must lose to "10MB"), and a bare number reads in
        // the partner's base.
        assert_eq!(
            agg(
                "max",
                vec![Value::Str("5MB".into()), Value::Str("10MB".into())]
            ),
            vec![Value::Str("10MB".into())]
        );
        assert_eq!(
            agg(
                "min",
                vec![Value::Str("5MB".into()), Value::Str("10MB".into())]
            ),
            vec![Value::Str("5MB".into())]
        );
        assert_eq!(
            agg("max", vec![Value::Str("5MB".into()), Value::Int(10)]),
            vec![Value::Str("5MB".into())]
        );
    }

    #[test]
    fn median_survives_nan() {
        // Query arithmetic can mint NaN (float-overflow subtraction);
        // the sort must not panic on it.
        let vs = vec![Value::Float(f64::NAN), Value::Int(1), Value::Int(3)];
        assert_eq!(agg("median", vs).len(), 1);
    }

    #[test]
    fn median_odd_even_empty() {
        assert_eq!(agg("median", ints(&[5, 1, 3])), vec![Value::Int(3)]);
        assert_eq!(agg("median", ints(&[4, 1, 3, 2])), vec![Value::Float(2.5)]);
        assert_eq!(
            agg(
                "median",
                vec![Value::Float(1.5), Value::Float(2.5), Value::Float(9.0)]
            ),
            vec![Value::Float(2.5)]
        );
        assert_eq!(agg("median", vec![]), vec![Value::Null]);
    }

    #[test]
    fn spread_measures() {
        // population variance of 2,4,4,4,5,5,7,9 is 4, stddev 2
        let data = ints(&[2, 4, 4, 4, 5, 5, 7, 9]);
        assert_eq!(agg("variance", data.clone()), vec![Value::Float(4.0)]);
        assert_eq!(agg("stddev", data), vec![Value::Float(2.0)]);
        assert_eq!(agg("stddev", vec![]), vec![Value::Null]);
    }

    #[test]
    #[cfg(feature = "colligo")]
    fn locale_collation() {
        let call = |loc: Option<&str>| FnCall {
            name: "sort".into(),
            args: loc
                .map(|l| vec![crate::ast::Arg::Lit(Value::Str(l.into()))])
                .unwrap_or_default(),
        };
        let words = || {
            vec![
                Value::Str("ёж".into()),
                Value::Str("Öl".into()),
                Value::Str("еда".into()),
                Value::Str("Zebra".into()),
            ]
        };
        let texts =
            |vs: Vec<Value>| -> Vec<String> { vs.into_iter().map(|v| v.to_string()).collect() };
        // Russian: ё collates adjacent to е (colligo's deliberate
        // dictionary treatment orders it as a distinct letter
        // after е; еда < ёж either way); Cyrillic reordered first.
        assert_eq!(
            texts(apply(
                &call(Some("ru-RU")),
                words(),
                &crate::quantity::scale_expr
            )),
            ["еда", "ёж", "Öl", "Zebra"]
        );
        // Swedish: Ö is its own letter, after Z.
        assert_eq!(
            texts(apply(
                &call(Some("sv-SE")),
                words(),
                &crate::quantity::scale_expr
            )),
            ["Zebra", "Öl", "еда", "ёж"]
        );
        // German: Ö sorts with O, before Z.
        assert_eq!(
            texts(apply(
                &call(Some("de-DE")),
                words(),
                &crate::quantity::scale_expr
            )),
            ["Öl", "Zebra", "еда", "ёж"]
        );
        // No locale: the standard comparison (codepoint for text).
        assert_eq!(
            texts(apply(&call(None), words(), &crate::quantity::scale_expr)),
            ["Zebra", "Öl", "еда", "ёж"]
        );
    }

    #[test]
    fn encodings() {
        let apply1 = |name: &str, v: &str| {
            apply_scalar(
                &FnCall {
                    name: name.into(),
                    args: Vec::new(),
                },
                Value::Str(v.into()),
                &crate::quantity::scale_expr,
            )
            .pop()
            .unwrap()
            .to_string()
        };
        assert_eq!(apply1("base64", "Sapiens"), "U2FwaWVucw==");
        assert_eq!(apply1("base64url", "Sapiens"), "U2FwaWVucw");
        assert_eq!(apply1("base32", "foobar"), "MZXW6YTBOI======");
        assert_eq!(apply1("hex", "quarb"), "7175617262");
        assert_eq!(
            apply1("sha256", "abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn numeric_scalars() {
        assert_eq!(sc("round", Value::Float(2.5)), vec![Value::Int(3)]);
        assert_eq!(sc("round", Value::Str("2.4".into())), vec![Value::Int(2)]);
        assert_eq!(sc("floor", Value::Float(2.9)), vec![Value::Int(2)]);
        assert_eq!(sc("ceil", Value::Float(2.1)), vec![Value::Int(3)]);
        assert_eq!(sc("abs", Value::Int(-4)), vec![Value::Int(4)]);
        assert_eq!(sc("abs", Value::Float(-1.5)), vec![Value::Float(1.5)]);
        assert_eq!(sc("round", Value::Str("n/a".into())), vec![Value::Null]);
    }

    #[test]
    fn length_alias_is_gone() {
        assert!(!known_agg("length"));
        assert!(known_agg("mean") && known_agg("avg") && known_agg("median"));
        assert!(known_scalar("round") && known_scalar("abs"));
    }

    #[test]
    fn isoformat_renders_utc() {
        // isoformat is "the reading, formatted UTC": a typed instant
        // with a written +01:00 offset renders in UTC, not its
        // display offset — the same as the text path right beside it.
        let (secs, nanos, offset_min) =
            crate::temporal::parse_iso("2024-02-15T14:26:40+01:00").unwrap();
        assert_eq!(offset_min, Some(60));
        let inst = Value::Instant {
            secs,
            nanos,
            offset_min,
        };
        assert_eq!(
            sc("isoformat", inst),
            vec![Value::Str("2024-02-15T13:26:40".into())]
        );
        assert_eq!(
            sc("isoformat", Value::Str("2024-02-15T14:26:40+01:00".into())),
            vec![Value::Str("2024-02-15T13:26:40".into())]
        );
    }

    #[test]
    fn quantity_rounders_use_written_unit() {
        // A quantity rounds in its written display unit and stays a
        // quantity, recomputing the base magnitude as rounded*factor.
        let q = Value::Quantity {
            value: 5700.0,
            base: "m".into(),
            written: Some((5.7, "km".into())),
        };
        assert_eq!(
            sc("round", q.clone()),
            vec![Value::Quantity {
                value: 6000.0,
                base: "m".into(),
                written: Some((6.0, "km".into())),
            }]
        );
        assert_eq!(
            sc("floor", q.clone()),
            vec![Value::Quantity {
                value: 5000.0,
                base: "m".into(),
                written: Some((5.0, "km".into())),
            }]
        );
        assert_eq!(
            sc("ceil", q),
            vec![Value::Quantity {
                value: 6000.0,
                base: "m".into(),
                written: Some((6.0, "km".into())),
            }]
        );
    }

    #[test]
    fn abs_over_quantity_is_null() {
        // A quantity has no numeric reading and `abs` is not a
        // rounder, so it is null rather than a bare base float.
        let q = Value::Quantity {
            value: -5000.0,
            base: "m".into(),
            written: Some((-5.0, "km".into())),
        };
        assert_eq!(sc("abs", q), vec![Value::Null]);
    }

    #[test]
    fn numeric_reductions_over_empty_are_null() {
        assert_eq!(agg("sum", vec![]), vec![Value::Null]);
        assert_eq!(agg("min", vec![]), vec![Value::Null]);
        assert_eq!(agg("max", vec![]), vec![Value::Null]);
        // Wholly non-numeric input skips every value as missing.
        assert_eq!(agg("sum", vec![Value::Str("x".into())]), vec![Value::Null]);
        // `product` keeps the spec's fold-from-1 identity.
        assert_eq!(agg("product", vec![]), vec![Value::Int(1)]);
    }

    #[test]
    fn sum_and_product_promote_on_overflow() {
        // The all-integer fast path promotes to float on overflow,
        // matching the +/* operators rather than wrapping/panicking.
        let big = 9_000_000_000_000_000_000i64;
        assert_eq!(
            agg("sum", vec![Value::Int(big), Value::Int(big)]),
            vec![Value::Float(big as f64 + big as f64)]
        );
        let m = 4_000_000_000i64;
        assert_eq!(
            agg("product", vec![Value::Int(m), Value::Int(m)]),
            vec![Value::Float(m as f64 * m as f64)]
        );
    }

    #[test]
    fn extreme_skips_missing_and_keeps_typed() {
        // A value with no reading (a junk string) is skipped rather
        // than winning via the comparison's string fallback.
        assert_eq!(
            agg("max", vec![Value::Str("banana".into()), Value::Int(42)]),
            vec![Value::Int(42)]
        );
        assert_eq!(
            agg("min", vec![Value::Str("apple".into()), Value::Int(5)]),
            vec![Value::Int(5)]
        );
        // Numeric strings carry a reading: they compare numerically
        // and the original value is preserved (CSV-cell semantics).
        assert_eq!(
            agg(
                "max",
                vec![Value::Str("512.3292".into()), Value::Str("80".into())]
            ),
            vec![Value::Str("512.3292".into())]
        );
        // Instants keep working (the newest date), via the temporal
        // reading rather than being skipped.
        let a = Value::Instant {
            secs: 100,
            nanos: 0,
            offset_min: None,
        };
        let b = Value::Instant {
            secs: 200,
            nanos: 0,
            offset_min: None,
        };
        assert_eq!(agg("max", vec![a.clone(), b.clone()]), vec![b.clone()]);
        assert_eq!(agg("min", vec![a.clone(), b]), vec![a]);
    }
}
