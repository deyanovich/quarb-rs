//! The quantital fragment: unit-text parsing behind
//! `Value::Quantity` (spec: The Quantital Fragment).
//!
//! A quantity is a value on a dimension: a numeric magnitude scaled
//! to its dimension's SI-base expansion, the written form kept for
//! display only — exactly as instants keep their written offset.
//! The scale table is the kaiv spec's frozen built-in set
//! (SPEC § Built-in Units), hand-pinned here like the civil-calendar
//! arithmetic: spec-law embedded, no dependency. Custom units never
//! reach the engine — the kaiv adapter scales them to base at the
//! mount boundary via kaiv's own `.faiv` machinery.
//!
//! The unit model is factor-only (kaiv excludes affine units by
//! design, so there is no °C to tempt anyone), and currencies never
//! scale: exchange rates are external and time-varying, which is
//! the determinism boundary — the same rule that priced `now()`
//! keeps live money out of queries.

/// The SI prefixes with their powers of ten ("da" first so the
/// two-character prefix wins the strip).
const PREFIX_POWERS: &[(&str, i32)] = &[
    ("da", 1),
    ("Y", 24),
    ("Z", 21),
    ("E", 18),
    ("P", 15),
    ("T", 12),
    ("G", 9),
    ("M", 6),
    ("k", 3),
    ("h", 2),
    ("d", -1),
    ("c", -2),
    ("m", -3),
    ("u", -6),
    ("n", -9),
    ("p", -12),
    ("f", -15),
    ("a", -18),
    ("z", -21),
    ("y", -24),
];

/// One unprefixed built-in unit: (exact factor, canonical SI-base
/// expansion). The expansion strings are the kaiv spec's, already
/// in canonical factor order.
fn full_name_scale(name: &str) -> Option<(f64, &'static str)> {
    Some(match name {
        // SI base units. Time units ARE here — compound
        // expressions need them as factors (`km/h`) — but a
        // *pure-time* result is rejected downstream: on the quarb
        // side a span of time is a Duration, one ontology per
        // dimension of time.
        // Information (bytes are not SI, but they are the most
        // ubiquitous unit in the substrates this engine mounts —
        // file sizes, blob sizes, object sizes). Decimal prefixes
        // ride the generic machinery (kB = 1000 B, MB, GB, TB, …);
        // the IEC binary forms are full names (KiB = 1024 B, MiB,
        // GiB, …). Strictly SI + IEC — there is no capital KB, the
        // same discipline that keeps `min` the only short minute:
        // decimal is kB, binary is KiB, and 1 GB = 10^9 B while
        // 1 GiB = 2^30 B, never confused. The table matches kaiv's
        // (harmonized by ruling 2026-07-18).
        "B" => (1.0, "B"),
        "KiB" => (1024.0, "B"),
        "MiB" => (1_048_576.0, "B"),
        "GiB" => (1_073_741_824.0, "B"),
        "TiB" => (1_099_511_627_776.0, "B"),
        "PiB" => (1_125_899_906_842_624.0, "B"),
        "EiB" => (1_152_921_504_606_846_976.0, "B"),
        "m" => (1.0, "m"),
        "s" => (1.0, "s"),
        "kg" => (1.0, "kg"),
        "A" => (1.0, "A"),
        "K" => (1.0, "K"),
        "mol" => (1.0, "mol"),
        "cd" => (1.0, "cd"),
        "g" => (0.001, "kg"),
        // Named SI-derived units (coherent: factor 1).
        "Hz" | "Bq" => (1.0, "1/s"),
        "N" => (1.0, "kg*m/s^2"),
        "Pa" => (1.0, "kg/m/s^2"),
        "J" => (1.0, "kg*m^2/s^2"),
        "W" => (1.0, "kg*m^2/s^3"),
        "C" => (1.0, "A*s"),
        "V" => (1.0, "kg*m^2/A/s^3"),
        "F" => (1.0, "A^2*s^4/kg/m^2"),
        "ohm" => (1.0, "kg*m^2/A^2/s^3"),
        "S" => (1.0, "A^2*s^3/kg/m^2"),
        "Wb" => (1.0, "kg*m^2/A/s^2"),
        "T" => (1.0, "kg/A/s^2"),
        "H" => (1.0, "kg*m^2/A^2/s^2"),
        "lm" => (1.0, "cd"),
        "lx" => (1.0, "cd/m^2"),
        "Gy" | "Sv" => (1.0, "m^2/s^2"),
        "kat" => (1.0, "mol/s"),
        // The litre (prefixable).
        "L" => (0.001, "m^3"),
        // Non-SI / US-imperial (exact factors per the spec).
        "min" => (60.0, "s"),
        "h" => (3600.0, "s"),
        "d" => (86400.0, "s"),
        "t" => (1000.0, "kg"),
        "in" => (0.0254, "m"),
        "ft" => (0.3048, "m"),
        "yd" => (0.9144, "m"),
        "mi" => (1609.344, "m"),
        "nmi" => (1852.0, "m"),
        "lb" => (0.45359237, "kg"),
        "oz" => (0.028349523125, "kg"),
        "gal" => (0.003785411784, "m^3"),
        _ => return None,
    })
}

/// Whether a prefix may attach to `stem` (SI base and derived
/// units, the gram, the litre — never non-SI/imperial).
fn prefixable(stem: &str) -> bool {
    matches!(
        stem,
        "B" | "m"
            | "s"
            | "g"
            | "A"
            | "K"
            | "mol"
            | "cd"
            | "L"
            | "Hz"
            | "N"
            | "Pa"
            | "J"
            | "W"
            | "C"
            | "V"
            | "F"
            | "ohm"
            | "S"
            | "Wb"
            | "T"
            | "H"
            | "lm"
            | "lx"
            | "Bq"
            | "Gy"
            | "Sv"
            | "kat"
    )
}

/// The scale of one built-in unit name, prefix included. Full
/// names win before prefix-stripping (`mi` is miles, never
/// milli-inches; `Pa` never splits).
pub fn unit_scale(name: &str) -> Option<(f64, &'static str)> {
    if let Some(hit) = full_name_scale(name) {
        return Some(hit);
    }
    for (p, pow) in PREFIX_POWERS {
        if let Some(stem) = name.strip_prefix(p)
            && prefixable(stem)
        {
            let (f, base) = full_name_scale(stem)?;
            return Some((f * 10f64.powi(*pow), base));
        }
    }
    None
}

/// The scale of a full unit *expression* over the built-in table:
/// factors joined by `*` and `/` with optional `^exp` exponents
/// (`km/h`, `kg*m/s^2`, `mi^2`). Returns the exact factor and the
/// canonical SI-base expansion (ASCII-sorted factors, numerator
/// then `/`-chained denominator — kaiv's canonical form, so bases
/// from either road compare equal). Pure time (base exactly `s`)
/// is excluded: a span of time is a Duration, and time text
/// belongs to the durational reading.
pub fn scale_expr(expr: &str) -> Option<(f64, String)> {
    if expr.is_empty() {
        return None;
    }
    let cs: Vec<char> = expr.chars().collect();
    let mut i = 0;
    let mut factor = 1.0f64;
    let mut base: std::collections::BTreeMap<&'static str, i64> = Default::default();
    let mut op = '*';
    loop {
        let start = i;
        while i < cs.len() && cs[i].is_ascii_alphabetic() {
            i += 1;
        }
        let name: String = cs[start..i].iter().collect();
        // The dimensionless identity contributes nothing.
        let one = name.is_empty() && cs.get(start) == Some(&'1');
        if one {
            i += 1;
        } else if name.is_empty() {
            return None;
        }
        let mut exp: i64 = 1;
        if cs.get(i) == Some(&'^') {
            i += 1;
            let neg = cs.get(i) == Some(&'-');
            if neg {
                i += 1;
            }
            let ds = i;
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
            if i == ds {
                return None;
            }
            let n: i64 = cs[ds..i].iter().collect::<String>().parse().ok()?;
            exp = if neg { -n } else { n };
        }
        if !one {
            let (f, expansion) = unit_scale(&name)?;
            let signed = if op == '*' { exp } else { -exp };
            factor *= f.powi(signed as i32);
            accumulate(&mut base, expansion, signed);
        }
        if i >= cs.len() {
            break;
        }
        op = cs[i];
        if op != '*' && op != '/' {
            return None;
        }
        i += 1;
    }
    base.retain(|_, e| *e != 0);
    let out = format_base(&base);
    if out == "s" {
        return None;
    }
    Some((factor, out))
}

/// Fold one canonical expansion string (`kg*m^2/s^3`) into the
/// exponent map, scaled by `signed`.
fn accumulate(
    base: &mut std::collections::BTreeMap<&'static str, i64>,
    expansion: &'static str,
    signed: i64,
) {
    let mut denom = false;
    for part in expansion.split_inclusive(['*', '/']) {
        let (tok, next_denom) = match part.strip_suffix('*') {
            Some(t) => (t, denom),
            None => match part.strip_suffix('/') {
                Some(t) => (t, true),
                None => (part, denom),
            },
        };
        let (name, e) = match tok.split_once('^') {
            Some((n, x)) => (n, x.parse::<i64>().unwrap_or(1)),
            None => (tok, 1),
        };
        if name != "1" {
            let key: &'static str = leak_base_name(name);
            let sign = if denom { -1 } else { 1 };
            *base.entry(key).or_insert(0) += e * sign * signed;
        }
        denom = next_denom;
    }
}

/// The seven SI base names as 'static strs (the expansions only
/// ever mention these).
fn leak_base_name(name: &str) -> &'static str {
    match name {
        "m" => "m",
        "kg" => "kg",
        "s" => "s",
        "A" => "A",
        "K" => "K",
        "mol" => "mol",
        "cd" => "cd",
        other => Box::leak(other.to_string().into_boxed_str()),
    }
}

/// Format the exponent map in kaiv's canonical form.
fn format_base(base: &std::collections::BTreeMap<&'static str, i64>) -> String {
    let fmt = |n: &str, e: i64| {
        if e == 1 {
            n.to_string()
        } else {
            format!("{n}^{e}")
        }
    };
    let num: Vec<String> = base
        .iter()
        .filter(|(_, e)| **e > 0)
        .map(|(n, e)| fmt(n, *e))
        .collect();
    let den: Vec<String> = base
        .iter()
        .filter(|(_, e)| **e < 0)
        .map(|(n, e)| fmt(n, -*e))
        .collect();
    let mut out = if num.is_empty() {
        "1".to_string()
    } else {
        num.join("*")
    };
    for d in den {
        out.push('/');
        out.push_str(&d);
    }
    out
}

/// Parse unit text (spec: The Unital Reading): a decimal magnitude
/// glued or space-separated to a unit expression — `5km`,
/// `0.2 kW`, `250mA`, `'100 km/h'` (compound forms carry `/`, so
/// in query text they ride quoted). Returns (base value, base
/// expansion, written value, written unit). A bare number is NOT
/// unit text (no default unit), a dimensionless result is a plain
/// number in disguise, and pure time belongs to span text and the
/// durational reading.
pub fn parse_unit_text(s: &str) -> Option<(f64, String, f64, String)> {
    parse_unit_text_with(s, &scale_expr)
}

/// [`parse_unit_text`] with an explicit unit-expression resolver —
/// the adapter hook road, so criterion text may use the mounted
/// document's own custom units.
pub fn parse_unit_text_with(
    s: &str,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<(f64, String, f64, String)> {
    let s = s.trim();
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let num_start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == num_start {
        return None;
    }
    let value: f64 = s[..i].parse().ok()?;
    let unit = s[i..].trim_start();
    if unit.is_empty()
        || !unit
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'*' | b'/' | b'^'))
        || !unit.bytes().next().is_some_and(|c| c.is_ascii_alphabetic())
    {
        return None;
    }
    let (factor, base) = scale(unit)?;
    if base == "s" || base == "1" {
        return None;
    }
    Some((value * factor, base, value, unit.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_table() {
        assert_eq!(unit_scale("km"), Some((1000.0, "m")));
        assert_eq!(unit_scale("mg"), Some((1e-6, "kg")));
        assert_eq!(unit_scale("kW"), Some((1000.0, "kg*m^2/s^3")));
        assert_eq!(unit_scale("mi"), Some((1609.344, "m")));
        // Full names beat prefixes; unknown names are out. Time
        // units exist as factors (compounds need them) but a
        // pure-time RESULT is rejected by scale_expr/unit text.
        assert_eq!(unit_scale("Pa").map(|(_, b)| b), Some("kg/m/s^2"));
        assert_eq!(unit_scale("min"), Some((60.0, "s")));
        assert_eq!(scale_expr("min"), None);
        assert_eq!(scale_expr("s"), None);
        assert_eq!(unit_scale("parsec"), None);
    }

    #[test]
    fn byte_units() {
        // The information base: decimal prefixes are powers of
        // ten, the IEC binary forms are full names. Strict SI:
        // there is no capital KB — decimal is kB, binary is KiB.
        assert_eq!(unit_scale("B"), Some((1.0, "B")));
        assert_eq!(unit_scale("kB"), Some((1000.0, "B")));
        assert_eq!(unit_scale("KB"), None);
        assert_eq!(unit_scale("KiB"), Some((1024.0, "B")));
        assert_eq!(unit_scale("MB"), Some((1e6, "B")));
        assert_eq!(unit_scale("MiB"), Some((1_048_576.0, "B")));
        assert_eq!(unit_scale("GB"), Some((1e9, "B")));
        assert_eq!(unit_scale("GiB"), Some((1_073_741_824.0, "B")));
        assert_eq!(unit_scale("TB"), Some((1e12, "B")));
        assert_eq!(unit_scale("TiB"), Some((1_099_511_627_776.0, "B")));
        // The seam, settled: a gibibyte outranks a gigabyte.
        let gb = unit_scale("GB").unwrap().0;
        let gib = unit_scale("GiB").unwrap().0;
        assert!(gib > gb);
        // Bytes flow through full expressions too (rates).
        assert_eq!(
            scale_expr("MB/s").map(|(f, b)| (f, b)),
            Some((1e6, "B/s".to_string()))
        );
    }

    #[test]
    fn unit_text() {
        let (bv, base, wv, wu) = parse_unit_text("5km").unwrap();
        assert_eq!(
            (bv, base.as_str(), wv, wu.as_str()),
            (5000.0, "m", 5.0, "km")
        );
        let (bv, base, ..) = parse_unit_text("0.2 kW").unwrap();
        assert_eq!((bv, base.as_str()), (200.0, "kg*m^2/s^3"));
        assert_eq!(parse_unit_text("-40mm").map(|t| t.0), Some(-0.04));
        // Compound expressions, quoted in query text.
        let (bv, base, ..) = parse_unit_text("100 km/h").unwrap();
        assert!((bv - 100.0 * 1000.0 / 3600.0).abs() < 1e-9);
        assert_eq!(base, "m/s");
        assert_eq!(
            scale_expr("kg*m/s^2").map(|(f, b)| (f, b)),
            Some((1.0, "kg*m/s^2".to_string()))
        );
        assert_eq!(scale_expr("N").unwrap().1, "kg*m/s^2");
        // Pure time stays span-text territory even as an expression.
        assert_eq!(scale_expr("h"), None);
        // Not unit text: bare numbers, span text, unknown units.
        assert_eq!(parse_unit_text("300"), None);
        assert_eq!(parse_unit_text("12h"), None);
        assert_eq!(parse_unit_text("5min"), None);
        assert_eq!(parse_unit_text("5 parsec"), None);
    }
}
