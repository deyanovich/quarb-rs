//! The quantity fragment: unit-text parsing behind
//! `Value::Quantity` (spec: Physical Quantities; the table in the
//! appendix Units and Prefixes).
//!
//! A quantity is a value on a dimension: a numeric magnitude scaled
//! to its dimension's base expansion, the written form kept for
//! display only, exactly as instants keep their written offset.
//! The built-in table is the appendix's, read through the quanto
//! crate (every pack of it: the SI, the accepted and common units,
//! the CGS and imperial sets, information, the long names), which
//! parses unit text, keeps the factors exact and spells the base
//! expansion in the canonical order. What stays here is the engine's
//! own layer: a model's `unit` declarations through the resolver a
//! host passes in (a declaration shadows the table), the currency
//! codes and signs as known units of their own dimension, the
//! Cyrillic aliases, and the rule that any other name in unit text
//! is an opaque unit, a base of its own dimension (`12 EUR`,
//! `3 apples/kg`), scaled by nothing: currencies are exchanged by a
//! rate the query supplies (`exchange`), never looked up.
//!
//! The unit model is factor-only (no affine units, so there is no
//! °C to tempt anyone) and pure time is excluded: a span of time is
//! a Duration.

use quanto::{Factor, Prefixes, Resolver, Table, Unit, Unknown};

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

/// The binary prefixes, `2^(10n)`, on the bit and the byte alone.
const BINARY_PREFIXES: &[(&str, i32)] = &[
    ("Ki", 10),
    ("Mi", 20),
    ("Gi", 30),
    ("Ti", 40),
    ("Pi", 50),
    ("Ei", 60),
    ("Zi", 70),
    ("Yi", 80),
];

/// The base units: the SI seven and the bit. Any other factor of a
/// base expansion is an opaque unit.
pub const BASE_NAMES: &[&str] = &["m", "kg", "s", "A", "K", "mol", "cd", "b"];

/// The canonical order of dimensions by their base spelling (spec:
/// the unit appendix): the bases m, kg, s, A, K, mol, cd, then the
/// bit, then the opaque units by name in code point order; a compound
/// base (`m/s`, `m^2`) sorts by its leading base after the plain ones.
pub fn dimension_order(a: &str, b: &str) -> std::cmp::Ordering {
    fn key(base: &str) -> (u8, usize, &str) {
        if let Some(i) = BASE_NAMES.iter().position(|n| *n == base) {
            return (0, i, "");
        }
        let lead = base.split(['*', '/', '^']).next().unwrap_or(base);
        match BASE_NAMES.iter().position(|n| *n == lead) {
            Some(i) => (1, i, base),
            None => (2, 0, base),
        }
    }
    key(a).cmp(&key(b))
}

/// The currency signs a prefix alias reads (`$12`, `€ 12.50`): the
/// sign with its trailing dash, and the ISO 4217 code it stands for.
const CURRENCY_SIGNS: &[(&str, &str)] = &[
    ("$-", "USD"),
    ("€-", "EUR"),
    ("£-", "GBP"),
    ("¥-", "JPY"),
    ("₩-", "KRW"),
    ("₹-", "INR"),
    ("₽-", "RUB"),
    ("₦-", "NGN"),
    ("₱-", "PHP"),
    ("₺-", "TRY"),
    ("฿-", "THB"),
    ("₴-", "UAH"),
    ("₸-", "KZT"),
    ("₪-", "ILS"),
    ("₲-", "PYG"),
    ("₾-", "GEL"),
    ("₮-", "MNT"),
    ("֏-", "AMD"),
];

/// Every currency ISO 4217 lists as circulating (spec: Currencies),
/// 154 codes alphabetically, each a built-in base unit of its own
/// dimension; no rate relates two of them, `exchange` is the only
/// road. The second codes, the retired codes and the metal, fund
/// and test codes are not units.
pub const CURRENCIES: &[&str] = &[
    "AED", "AFN", "ALL", "AMD", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BGN",
    "BHD", "BIF", "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF",
    "CHF", "CLP", "CNY", "COP", "CRC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD", "EGP",
    "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP", "GMD", "GNF", "GTQ", "GYD",
    "HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD", "JOD", "JPY",
    "KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR", "LRD",
    "LSL", "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR", "MWK",
    "MXN", "MYR", "MZN", "NAD", "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN", "PGK",
    "PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD", "RUB", "RWF", "SAR", "SBD", "SCR", "SDG",
    "SEK", "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SYP", "SZL", "THB", "TJS", "TMT",
    "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX", "USD", "UYU", "UZS", "VES", "VND",
    "VUV", "WST", "XAF", "XCD", "XCG", "XOF", "XPF", "YER", "ZAR", "ZMW", "ZWG",
];

/// The GOST 8.417 designations (spec: Cyrillic Designations): each
/// a built-in alias of an SI symbol, read always, printed under the
/// Russian locale.
const CYRILLIC_UNITS: &[(&str, &str)] = &[
    ("м", "m"),
    ("г", "g"),
    ("с", "s"),
    ("А", "A"),
    ("К", "K"),
    ("моль", "mol"),
    ("кд", "cd"),
    ("бит", "b"),
    ("байт", "B"),
    ("Б", "B"),
    ("рад", "rad"),
    ("ср", "sr"),
    ("Гц", "Hz"),
    ("Н", "N"),
    ("Па", "Pa"),
    ("Дж", "J"),
    ("Вт", "W"),
    ("Кл", "C"),
    ("В", "V"),
    ("Ф", "F"),
    ("Ом", "ohm"),
    ("См", "S"),
    ("Вб", "Wb"),
    ("Тл", "T"),
    ("Гн", "H"),
    ("лм", "lm"),
    ("лк", "lx"),
    ("Бк", "Bq"),
    ("Гр", "Gy"),
    ("Зв", "Sv"),
    ("мин", "min"),
    ("ч", "h"),
    ("сут", "d"),
    ("л", "L"),
    ("т", "t"),
    ("га", "ha"),
    ("эВ", "eV"),
    ("бар", "bar"),
    ("атм", "atm"),
    ("кал", "cal"),
    ("уз", "kn"),
    ("эрг", "erg"),
    ("дин", "dyn"),
    ("Гс", "G"),
    ("Э", "Oe"),
    ("Ст", "St"),
    ("Гал", "Gal"),
    ("Мкс", "Mx"),
    ("Торр", "Torr"),
    ("Ба", "Ba"),
];

/// The GOST prefixes, the two-letter ones first so they win the
/// strip; `г` is hecto and `с` centi, `м` milli, as the symbols are
/// gram, second and metre on their own (a full name is read first).
const CYRILLIC_PREFIXES: &[(&str, &str)] = &[
    ("да", "da"),
    ("мк", "u"),
    ("Ки", "Ki"),
    ("Ми", "Mi"),
    ("Ги", "Gi"),
    ("Ти", "Ti"),
    ("Пи", "Pi"),
    ("Эи", "Ei"),
    ("Зи", "Zi"),
    ("Йи", "Yi"),
    ("И", "Y"),
    ("З", "Z"),
    ("Э", "E"),
    ("П", "P"),
    ("Т", "T"),
    ("Г", "G"),
    ("М", "M"),
    ("к", "k"),
    ("г", "h"),
    ("д", "d"),
    ("с", "c"),
    ("м", "m"),
    ("н", "n"),
    ("п", "p"),
    ("ф", "f"),
    ("а", "a"),
];

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04ff}').contains(&c)
}

/// The SI spelling of a Cyrillic designation, a full name or a
/// prefix on one: `кг` is kg, `мкс` us, `МВт` MW, `Кибайт` KiB.
/// None for a name that is no designation.
fn latin_of(name: &str) -> Option<String> {
    if !name.chars().any(is_cyrillic) {
        return None;
    }
    if let Some((_, latin)) = CYRILLIC_UNITS.iter().find(|(c, _)| *c == name) {
        return Some(latin.to_string());
    }
    for (cp, lp) in CYRILLIC_PREFIXES {
        if let Some(stem) = name.strip_prefix(cp)
            && let Some((_, latin)) = CYRILLIC_UNITS.iter().find(|(c, _)| *c == stem)
        {
            let binary = BINARY_PREFIXES.iter().any(|(p, _)| p == lp);
            if (binary && (*latin == "b" || *latin == "B")) || (!binary && prefixable(latin)) {
                return Some(format!("{lp}{latin}"));
            }
        }
    }
    None
}

/// The Cyrillic designation of an SI spelling (`kg` is кг, `KiB`
/// Кибайт); None where the table has none (a currency, an opaque or
/// declared unit).
fn cyrillic_of(latin: &str) -> Option<String> {
    if let Some((c, _)) = CYRILLIC_UNITS.iter().find(|(_, l)| *l == latin) {
        return Some(c.to_string());
    }
    let prefixes: Vec<(&str, &str)> = BINARY_PREFIXES
        .iter()
        .map(|(p, _)| *p)
        .chain(PREFIX_POWERS.iter().map(|(p, _)| *p))
        .filter_map(|lp| {
            CYRILLIC_PREFIXES
                .iter()
                .find(|(_, l)| *l == lp)
                .map(|(c, _)| (lp, *c))
        })
        .collect();
    for (lp, cp) in prefixes {
        if let Some(stem) = latin.strip_prefix(lp)
            && (prefixable(stem) || stem == "b" || stem == "B")
            && let Some((c, _)) = CYRILLIC_UNITS.iter().find(|(_, l)| *l == stem)
        {
            return Some(format!("{cp}{c}"));
        }
    }
    None
}

/// A unit expression re-spelled factor by factor: `f` maps one
/// factor's name, keeping it where it answers None.
fn respell(unit: &str, f: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut name = String::new();
    let flush = |name: &mut String, out: &mut String| {
        if !name.is_empty() {
            match f(name) {
                Some(s) => out.push_str(&s),
                None => out.push_str(name),
            }
            name.clear();
        }
    };
    for c in unit.chars() {
        if is_name_char(c) {
            name.push(c);
        } else {
            flush(&mut name, &mut out);
            out.push(c);
        }
    }
    flush(&mut name, &mut out);
    out
}

/// The canonical (SI) spelling of a written unit: every Cyrillic
/// designation to its symbol, the rest as written. What the plain
/// display and the interchange forms print.
pub fn canonical_unit(unit: &str) -> String {
    respell(unit, |f| {
        latin_of(f).or_else(|| micro_of(f)).or_else(|| ohm_of(f))
    })
}

/// The written unit under a language's designations (spec: The
/// Output Locale): Russian re-spells each factor the table knows,
/// every other language prints the SI spelling.
pub fn designated_unit(unit: &str, language: &str) -> String {
    let canonical = canonical_unit(unit);
    match language {
        "ru" => respell(&canonical, cyrillic_of),
        _ => canonical,
    }
}

/// Ten to a power as the nearest double, the way the literal `1e-6`
/// reads: a prefix is one power of ten applied once, so `5_um` is
/// exactly `5e-06` metres (repeated multiplication would drift).
fn pow10(exp: i32) -> f64 {
    format!("1e{exp}").parse().expect("a power of ten parses")
}

/// The micro prefix is written `u`; the micro sign µ (U+00B5) and
/// the Greek small mu μ (U+03BC) read as the same prefix.
fn micro_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix('µ').or_else(|| name.strip_prefix('μ'))?;
    (!rest.is_empty()).then(|| format!("u{rest}"))
}

/// The ohm is written `ohm`; the Greek capital omega Ω (U+03A9) and
/// the ohm sign Ω (U+2126) read as the same unit, prefixes included.
fn ohm_of(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix('\u{3a9}')
        .or_else(|| name.strip_suffix('\u{2126}'))?;
    Some(format!("{stem}ohm"))
}

/// A unit factor that is a power of ten, as its decimal exponent.
fn decimal_exponent(factor: f64) -> Option<i32> {
    if !(factor > 0.0 && factor.is_finite()) {
        return None;
    }
    let e = factor.log10().round() as i32;
    (pow10(e) == factor).then_some(e)
}

/// `v` with its decimal exponent shifted by `e`: the double the
/// decimal literal reads to, where a multiplication can land one
/// ulp off (`5.0 * 1e-6` is `4.9999999999999996e-6`).
fn shift10(v: f64, e: i32) -> f64 {
    if e == 0 || v == 0.0 || !v.is_finite() {
        return v;
    }
    let s = format!("{v:e}");
    let Some((mantissa, exp)) = s.split_once('e') else {
        return v;
    };
    let Ok(exp) = exp.parse::<i32>() else {
        return v;
    };
    format!("{mantissa}e{}", exp + e).parse().unwrap_or(v)
}

/// `v` times a unit factor: a prefix (a power of ten) is applied
/// once, as a shift of the decimal exponent, so `5_um` is exactly
/// `5e-06` metres; any other factor multiplies.
pub fn scale_by(v: f64, factor: f64) -> f64 {
    match decimal_exponent(factor) {
        Some(e) => shift10(v, e),
        None => v * factor,
    }
}

/// `v` divided by a unit factor, the inverse of [`scale_by`]: the
/// written magnitude of a COMPUTED quantity, rounded to 15
/// significant digits (ruling 2026-11-09: `4.8999999999999995_cm` is
/// `4.9_cm`; a literal never passes here).
pub fn unscale_by(v: f64, factor: f64) -> f64 {
    let w = match decimal_exponent(factor) {
        Some(e) => shift10(v, -e),
        None => v / factor,
    };
    settled(w)
}

/// The written magnitude of a computed quantity: rounded to 15
/// significant digits, which takes the noise of a conversion out of
/// the print. An exact whole magnitude carries no such noise and is
/// left alone (ruling 2026-11-10): `1234567890123450_B + 7_B` is
/// `…457_B`, and `magnitude` hands on the full value (the rounding
/// answered `…460`, where the difference to `…450_B` is `7_B`).
pub fn settled(w: f64) -> f64 {
    // (a double holds every whole number below 2^53)
    if w.fract() == 0.0 && w.abs() < 9_007_199_254_740_992.0 {
        return w;
    }
    round_significant(w, 15)
}

/// Whether `expr` is a unit expression the table reads strictly — the
/// packs, the engine's declarations, the run's custom units, the
/// currencies, the dictionary's words — with no opaque reading, so a
/// dictionary entry mapping to `_nope` refuses.
pub fn known_unit_expr(expr: &str) -> bool {
    with_opaque(false, || {
        crate::value::ambient::unit_scale(expr)
            .flatten()
            .or_else(|| scale_expr(expr))
    })
    .is_some()
}

/// `x` rounded to `digits` significant digits, through its decimal
/// spelling (so the double is the one the rounded literal reads to).
pub fn round_significant(x: f64, digits: usize) -> f64 {
    if x == 0.0 || !x.is_finite() || digits == 0 {
        return x;
    }
    format!("{:.*e}", digits - 1, x).parse().unwrap_or(x)
}

/// The scale of one unit name over the built-in table: the factor
/// to the base and the base expansion. A full name is read before
/// a prefix is stripped (`mi` is the mile, `min` the minute).
pub fn unit_scale(name: &str) -> Option<(f64, &'static str)> {
    let u = lookup_strict(name)?;
    Some((u.factor.to_f64(), canonical_base(&u.dimension)))
}

/// Every spelling the built-in table reads as a unit operand: the
/// full names, the currency codes, and the prefixed forms the
/// prefixes admit (a prefix below one never on the bit or the byte).
pub fn builtin_unit_names() -> Vec<String> {
    let table = &tables().0;
    let mut out: Vec<String> = Vec::new();
    for e in table.entries() {
        out.extend(e.names.iter().map(|n| n.to_string()));
    }
    out.extend(CURRENCIES.iter().map(|c| c.to_string()));
    for (name, year) in ENGINE_UNITS {
        out.push(name.to_string());
        if *year {
            out.extend(YEAR_PREFIXES.iter().map(|(p, _)| format!("{p}{name}")));
        }
    }
    for e in table.entries() {
        for stem in e.names {
            match e.prefixes {
                Prefixes::None => {}
                Prefixes::Decimal => {
                    out.extend(PREFIX_POWERS.iter().map(|(p, _)| format!("{p}{stem}")));
                }
                Prefixes::Information => {
                    out.extend(
                        PREFIX_POWERS
                            .iter()
                            .filter(|(_, pow)| *pow > 0)
                            .map(|(p, _)| format!("{p}{stem}")),
                    );
                    out.extend(BINARY_PREFIXES.iter().map(|(p, _)| format!("{p}{stem}")));
                }
            }
        }
    }
    out
}

/// The known symbols nearest to `name`: the spellings within a
/// short edit distance of it, a slip of case counting a third of
/// an edit (`KBps` is `kbps`, `mB` is `MB` before `Mb`, `KM` is
/// `km` before `K`), the budget growing with the name's length;
/// when one edit or less reaches a symbol, only the symbols that
/// near. `declared` (a model's declarations, a source's own units)
/// stands beside the built-in table. At most four, nearest first.
pub fn nearest_units(name: &str, declared: &[String]) -> Vec<String> {
    // distances in thirds of an edit
    let budget = match name.chars().count() {
        0..=3 => 3,
        4..=6 => 6,
        _ => 9,
    };
    let mut hits: Vec<(usize, String)> = declared
        .iter()
        .cloned()
        .chain(builtin_unit_names())
        .filter(|cand| cand != name)
        .filter_map(|cand| {
            let d = case_distance(name, &cand);
            (d <= budget).then_some((d, cand))
        })
        .collect();
    hits.sort();
    hits.dedup();
    if let Some(&(best, _)) = hits.first()
        && best <= 3
    {
        hits.retain(|(d, _)| *d == best);
    }
    hits.into_iter().map(|(_, c)| c).take(4).collect()
}

/// The edit distance of two names in thirds of an edit: an
/// insertion, a deletion or a substitution counts three, a
/// substitution that changes case alone counts one.
fn case_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).map(|j| 3 * j).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = 3 * (i + 1);
        for (j, cb) in b.iter().enumerate() {
            let sub = if ca == cb {
                0
            } else if ca.to_lowercase().eq(cb.to_lowercase()) {
                1
            } else {
                3
            };
            cur[j + 1] = (prev[j] + sub).min(prev[j + 1] + 3).min(cur[j] + 3);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// What refuses a unit operand (`convert(U)`, `unit(U)`; spec: The
/// conversion): None when every factor of `expr` names a unit the
/// query knows — the built-in table or `declared` (a model's
/// declarations, an adapter's minted units) — else the refusal's
/// text: the first factor that is no unit, with the nearest known
/// symbols, or the table's own reason for refusing the spelling.
/// A value's unknown unit stays opaque; only the operand must be
/// known.
pub fn unit_operand_refusal(expr: &str, declared: &[String]) -> Option<String> {
    for factor in operand_factors(expr) {
        if declared.iter().any(|d| d == factor) {
            continue;
        }
        if refused(factor) {
            return Some(refused_spelling(factor, declared));
        }
        match tables().0.lookup_with(factor, &Names(NO_NAMES)) {
            Ok(_) => {}
            Err(quanto::Error::Unknown { .. }) => {
                // a word the dictionary knows is a unit (the word road)
                if crate::value::unit_words::word_symbol(factor).is_some() {
                    continue;
                }
                return Some(with_nearest(
                    format!("no unit '{factor}'"),
                    &nearest_units(factor, declared),
                ));
            }
            Err(_) => return Some(refused_spelling(factor, declared)),
        }
    }
    None
}

/// The factor names of a unit expression operand, in order: `EUR/lb`
/// is EUR and lb, `kg*m/s^2` is kg, m and s; the identity `1` and
/// the exponents are no names.
fn operand_factors(expr: &str) -> Vec<&str> {
    expr.split(['*', '/'])
        .map(|f| f.split_once('^').map_or(f, |(n, _)| n))
        .map(str::trim)
        .filter(|f| !f.is_empty() && *f != "1")
        .collect()
}

fn with_nearest(mut msg: String, near: &[String]) -> String {
    if !near.is_empty() {
        msg.push_str(" (the nearest: ");
        msg.push_str(&near.join(", "));
        msg.push(')');
    }
    msg
}

/// The refusal of a spelling the table refuses by name (see
/// `refused`): the ambiguous kilobyte and kilobit, in
/// Latin and in Cyrillic, say what the two readings are; a prefix
/// below one on the bit or the byte says so.
fn refused_spelling(name: &str, declared: &[String]) -> String {
    match name {
        "KB" => "no unit 'KB' (kB is 1000 B, KiB is 1024 B)".to_string(),
        "Kb" => "no unit 'Kb' (kb is 1000 b, Kib is 1024 b)".to_string(),
        "Кбайт" => "no unit 'Кбайт' (кбайт is 1000 байт, Кибайт is 1024 байт)".to_string(),
        "Кбит" => "no unit 'Кбит' (кбит is 1000 бит, Кибит is 1024 бит)".to_string(),
        _ => {
            let stem = if name.ends_with('B') || name.ends_with("byte") {
                "byte"
            } else {
                "bit"
            };
            let near = nearest_units(name, declared);
            let mut msg = format!("no unit '{name}' (no prefix below one on the {stem}");
            if !near.is_empty() {
                msg.push_str("; the nearest: ");
                msg.push_str(&near.join(", "));
            }
            msg.push(')');
            msg
        }
    }
}

/// A character a unit name is made of: a letter of any script, or
/// a currency symbol (`€`, `$`, `£`, `¥`, `₽`), so a declared
/// currency alias reads.
pub fn is_name_char(c: char) -> bool {
    c.is_alphabetic() || is_currency_symbol(c) || c == '%'
}

fn is_currency_symbol(c: char) -> bool {
    matches!(
        c,
        '$' | '€'
            | '֏'
            | '₾'
            | '₼'
            | '₭'
            | '₣'
            | '₧'
            | '₨'
            | '₿'
            | '£'
            | '¥'
            | '₽'
            | '₴'
            | '₹'
            | '₩'
            | '₺'
            | '₪'
            | '₫'
            | '฿'
            | '₦'
            | '₱'
            | '₡'
            | '₲'
            | '₵'
            | '₸'
            | '₮'
            | '¢'
    )
}

thread_local! {
    /// Whether an unknown name may read as an opaque unit: it may in
    /// an expression a command names and in underscored unit text,
    /// never in text that spaces or glues its unit to the magnitude
    /// (`2 copies`, `5G` are text). Set by [`parse_unit_text_with`]
    /// around the resolution, read by every resolver, since a host's
    /// resolver ends in [`scale_expr_with`].
    static OPAQUE_ALLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Run `f` with opaque units allowed or refused.
fn with_opaque<T>(allowed: bool, f: impl FnOnce() -> T) -> T {
    let before = OPAQUE_ALLOWED.with(|c| c.replace(allowed));
    let out = f();
    OPAQUE_ALLOWED.with(|c| c.set(before));
    out
}

/// The unit tables (quanto, every pack): the strict one refuses an
/// unknown name, the open one reads it as an opaque unit.
fn tables() -> &'static (Table, Table) {
    static TABLES: std::sync::OnceLock<(Table, Table)> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        (
            with_percent(Table::all()),
            with_percent(Table::all().unknown(Unknown::Opaque)),
        )
    })
}

/// The characters a unit name may hold beside letters: quanto's
/// currency signs, and the percent sign.
const SYMBOL_CHARS: &str = "$€£¥₽₴₹₩₺₪₫฿₦₱₡₲₵₸₮¢֏₾₼₭₣₧₨₿%";

/// The prefixes the year takes, each a power of a thousand.
const YEAR_PREFIXES: &[(&str, u32)] = &[("k", 1), ("M", 2), ("G", 3)];

/// The engine's own units (`with_percent`) and whether each takes
/// the year's prefixes: what the prefix refusal and the suggestions
/// read beside quanto's entries, so a prefix on one of these is
/// refused as on a built-in unit that takes none. (`5_kton` and
/// `5_k%` minted opaque units in silence.)
const ENGINE_UNITS: &[(&str, bool)] = &[("%", false), ("a", true), ("yr", true), ("ton", false)];

/// The engine's own units beside quanto's packs, declared table-wide
/// (they take no prefix): the percent, a unit of dimension one with
/// factor 1/100 of the ratio (ruling 2026-11-09), so `12_%`, `12 %`,
/// `convert(%)` and a `<_%>` trait all read it; the year `a` (with
/// `yr`), 365.25 days, the denominator of `t/a`; the short ton
/// `ton`, 2000 lb, what "tons" alone names in a title (the
/// header-units round).
fn with_percent(t: Table) -> Table {
    let mut t = t.symbol_chars(SYMBOL_CHARS);
    t.declare(
        "%",
        Unit::new(Factor::ratio(1, 100), quanto::Dimension::ONE),
    );
    let year = Unit::new(Factor::integer(31_557_600), quanto::Dimension::TIME);
    t.declare("a", year.clone());
    t.declare("yr", year);
    // the year's thousands, millions and billions (ruling
    // 2026-11-10): `ka`, `Ma`, `Ga` and `kyr`, `Myr`, `Gyr`
    for (prefix, thousands) in YEAR_PREFIXES {
        let unit = Unit::new(
            Factor::integer(31_557_600u64 * 1000u64.pow(*thousands)),
            quanto::Dimension::TIME,
        );
        for stem in ["a", "yr"] {
            t.declare(&format!("{prefix}{stem}"), unit.clone());
        }
    }
    t.declare(
        "ton",
        Unit::new(Factor::decimal(90_718_474, -5), quanto::Dimension::MASS),
    );
    t
}

/// The engine's names in front of the table: a run's declared units
/// first (a declaration shadows the table), then the Cyrillic aliases
/// of the built-in units. The table reads these before its packs.
struct Names<'a>(&'a dyn Fn(&str) -> Option<(f64, String)>);

impl Resolver for Names<'_> {
    fn resolve(&self, name: &str) -> Option<Unit> {
        if let Some((factor, base)) = (self.0)(name) {
            return unit_of(factor, &base);
        }
        if let Some(code) = currency_code(name) {
            return Some(Unit::base(quanto::Dimension::opaque(code)));
        }
        // the micro sign and the ohm sign in their other code points,
        // then the Cyrillic aliases
        let spelled = micro_of(name)
            .or_else(|| ohm_of(name))
            .or_else(|| latin_of(name))?;
        tables().0.lookup(&spelled).ok()
    }
}

/// A factor and a base expansion as a unit: the float exactly as the
/// dyadic rational it is, the base read by the open table.
fn unit_of(factor: f64, base: &str) -> Option<Unit> {
    Some(Unit::new(
        Factor::from_f64(factor)?,
        tables().1.dimension(base).ok()?,
    ))
}

const NO_NAMES: &dyn Fn(&str) -> Option<(f64, String)> = &|_| None;

/// A currency as the engine knows it: an ISO code of the table, or a
/// sign standing for one (`€` for EUR, `₸` for KZT). A known unit of
/// a dimension of its own, exchanged by a rate the query supplies.
fn currency_code(name: &str) -> Option<&'static str> {
    if let Some(code) = CURRENCIES.iter().find(|c| **c == name) {
        return Some(code);
    }
    CURRENCY_SIGNS
        .iter()
        .find(|(sign, _)| *sign == name)
        .map(|(_, code)| *code)
}

/// The refusal a quantity literal (`1_KB`) earns when its unit is a
/// spelling the table refuses by name: the message, or `None`.
pub(crate) fn refused_literal(text: &str) -> Option<String> {
    let (_, unit) = text.split_once('_')?;
    if refused(unit) {
        return Some(refused_spelling(unit, &[]));
    }
    misprefixed(unit)
}

/// The refusal a unit name earns when it is a prefix on a built-in
/// unit that does not take it (ruling 2026-11-06): a binary prefix on
/// anything but the bit and the byte (`Kim`), a fraction of a bit or a
/// byte (`mB`), any prefix on a unit that takes none (`kh`). A name the
/// table knows as written is never one; a name that splits into no
/// known unit stays an opaque unit of its own.
pub(crate) fn misprefixed(unit: &str) -> Option<String> {
    if lookup_strict(unit).is_some() {
        return None;
    }
    // the engine's own units first: none takes a prefix but the
    // year, and the year takes three
    for (stem, year) in ENGINE_UNITS {
        let prefixed = BINARY_PREFIXES
            .iter()
            .chain(PREFIX_POWERS.iter())
            .any(|(p, _)| unit.strip_prefix(p) == Some(*stem));
        if prefixed {
            return Some(if *year {
                format!(
                    "no unit '{unit}': the year takes the prefixes k, M and G \
                     (k{stem}, M{stem}, G{stem})"
                )
            } else {
                format!(
                    "no unit '{unit}': '{stem}' takes no prefix (write the quantity in \
                     '{stem}', or convert)"
                )
            });
        }
    }
    let entry = |stem: &str| tables().0.entries().find(|e| e.names.contains(&stem));
    for (p, _) in BINARY_PREFIXES {
        if let Some(stem) = unit.strip_prefix(p)
            && let Some(e) = entry(stem)
            && e.prefixes != Prefixes::Information
        {
            return Some(format!(
                "no unit '{unit}': '{p}' is a binary prefix, for the bit and the byte alone \
                 ({p}B is 2^{} B); '{stem}' takes {}",
                BINARY_PREFIXES
                    .iter()
                    .find(|(q, _)| q == p)
                    .map_or(0, |(_, n)| *n),
                match e.prefixes {
                    Prefixes::Decimal => format!("the decimal prefixes (k{stem} is 1000 {stem})"),
                    _ => "no prefix".to_string(),
                }
            ));
        }
    }
    let mut decimal: Vec<&(&str, i32)> = PREFIX_POWERS.iter().collect();
    decimal.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    for (p, power) in decimal {
        if let Some(stem) = unit.strip_prefix(p)
            && !stem.is_empty()
            && let Some(e) = entry(stem)
        {
            return match e.prefixes {
                Prefixes::None => Some(format!(
                    "no unit '{unit}': '{stem}' takes no prefix (write the quantity in '{stem}', \
                     or convert)"
                )),
                Prefixes::Information if *power < 0 => Some(format!(
                    "no unit '{unit}': no fraction of a '{stem}' — it takes the positive decimal \
                     prefixes (k{stem} is 1000 {stem}) and the binary ones (Ki{stem} is 1024 {stem})"
                )),
                _ => None,
            };
        }
    }
    None
}

/// Spellings read as a refusal, never as a unit: the ambiguous
/// kilobyte and kilobit, in Latin and in Cyrillic.
fn refused(name: &str) -> bool {
    matches!(name, "KB" | "Kb" | "Кбайт" | "Кбит")
}

/// One name as a unit under the strict table, Cyrillic aliases
/// included; `None` for an unknown or a refused name.
fn lookup_strict(name: &str) -> Option<Unit> {
    if refused(name) {
        return None;
    }
    tables().0.lookup_with(name, &Names(NO_NAMES)).ok()
}

/// A dimension spelled in base units, interned for the callers that
/// keep a `&'static str`.
fn canonical_base(dimension: &quanto::Dimension) -> &'static str {
    intern_base_name(&tables().0.spell(dimension))
}

/// Whether a built-in unit takes prefixes.
fn prefixable(stem: &str) -> bool {
    tables()
        .0
        .entries()
        .any(|e| e.names.contains(&stem) && e.prefixes != Prefixes::None)
}

/// A built-in unit by its own name, no prefix: the factor and the
/// canonical base expansion.
fn full_name_scale(name: &str) -> Option<(f64, &'static str)> {
    if name == "%" {
        return Some((0.01, canonical_base(&quanto::Dimension::ONE)));
    }
    let e = tables().0.entries().find(|e| e.names.contains(&name))?;
    Some((e.factor.to_f64(), canonical_base(&e.dimension)))
}

/// The scale of a full unit *expression* over the built-in table
/// alone (the adapter hook's default): see [`scale_expr_with`].
pub fn scale_expr(expr: &str) -> Option<(f64, String)> {
    scale_expr_with(expr, &|_| None)
}

/// The scale of a full unit expression: factors joined by `*` and
/// `/` with optional `^exp` exponents (`km/h`, `kg*m/s^2`, `mi^2`),
/// each resolved by `custom` first (a model's declarations), then
/// the built-in table, and any other name an opaque unit, a base of
/// its own. Returns the exact factor and the canonical base
/// expansion (ASCII-sorted factors, numerator then `/`-chained
/// denominator), so bases from either road compare equal. Pure time
/// (base exactly `s`) is excluded: a span of time is a Duration,
/// and time text belongs to the durational reading.
pub fn scale_expr_with(
    expr: &str,
    custom: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<(f64, String)> {
    if expr.is_empty() {
        return None;
    }
    // a prefix alias (`€-`, `$-`) is a name with its trailing dash: a
    // declared name, a currency sign, or an opaque unit where allowed
    if let Some(name) = expr.strip_suffix('-')
        && !name.is_empty()
        && name.chars().all(is_name_char)
    {
        if let Some(hit) = custom(expr) {
            return Some(hit);
        }
        if let Some(code) = currency_code(expr) {
            return Some((1.0, code.to_string()));
        }
        return OPAQUE_ALLOWED
            .with(|c| c.get())
            .then(|| (1.0, expr.to_string()));
    }
    // the exponent ceiling and the refused spellings, before the
    // table reads the expression
    for term in expr.split(['*', '/']) {
        let (name, exp) = term
            .split_once('^')
            .map_or((term, None), |(n, x)| (n, Some(x)));
        if exp.is_some_and(|x| {
            x.trim_start_matches('-')
                .parse::<i64>()
                .is_ok_and(|n| n > MAX_EXPONENT)
        }) {
            return None;
        }
        if refused(name) {
            return None;
        }
    }
    // the strict table first: the packs, the engine's declarations,
    // the run's custom units, the currencies
    let (strict, open) = tables();
    if let Ok(unit) = strict.unit_with(expr, &Names(custom)) {
        return Some((unit.factor.to_f64(), strict.spell(&unit.dimension)));
    }
    // the word road (the header-units round, 2026-11-09): a unit
    // written as a word the dictionary knows (`tonnes`, `miles`,
    // `percent`), before any opaque reading, with no spaces here — a
    // title's tail of several words goes through
    // `unit_words::parse_unit_words`
    if !expr.contains([' ', '*', '/', '^'])
        && let Some(symbol) = crate::value::unit_words::word_symbol(expr)
    {
        // a currency is an opaque unit: it reads where opaque units
        // may (`12_euros`, a title, `convert(euros)`), not glued or
        // spaced in text (`12€` stays text, as today)
        if currency_code(symbol).is_some() && !OPAQUE_ALLOWED.with(|c| c.get()) {
            return None;
        }
        if let Ok(unit) = open.unit_with(symbol, &Names(custom)) {
            return Some((unit.factor.to_f64(), open.spell(&unit.dimension)));
        }
    }
    // an opaque unit where the moment allows one
    if OPAQUE_ALLOWED.with(|c| c.get()) {
        let unit = open.unit_with(expr, &Names(custom)).ok()?;
        return Some((unit.factor.to_f64(), open.spell(&unit.dimension)));
    }
    None
}

/// Whether a base expansion holds an opaque factor.
/// The written unit of a product (`mul` true) or quotient of two
/// quantities written in `a` and `b`: the compound spelling quanto
/// composes (`km/h`, `m^2`), the empty text when every term cancels.
pub fn compose_written(a: &str, b: &str, mul: bool) -> Option<String> {
    let op = if mul {
        quanto::Op::Mul
    } else {
        quanto::Op::Div
    };
    tables().1.compose(a, op, b).ok()
}

/// The base expansion of a product (`mul` true) or quotient of two
/// base expansions, spelled canonically; `None` when a base does not
/// parse as unit text.
pub(crate) fn compose_base(a: &str, b: &str, mul: bool) -> Option<String> {
    // the open table: a base's opaque factor (a currency, a declared
    // unit) is a base of its own
    let table = &tables().1;
    let da = table.dimension(a).ok()?;
    let db = table.dimension(b).ok()?;
    let d = if mul {
        da.checked_mul(&db)?
    } else {
        da.checked_div(&db)?
    };
    Some(table.spell(&d))
}

pub fn has_opaque_factor(base: &str) -> bool {
    base_factors(base)
        .iter()
        .any(|(name, _)| !BASE_NAMES.contains(name))
}

/// The factors of a base expansion with their signed exponents, in
/// the expansion's order.
pub fn base_factors(base: &str) -> Vec<(&str, i64)> {
    let mut out = Vec::new();
    let mut denom = false;
    for part in base.split_inclusive(['*', '/']) {
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
        if name != "1" && !name.is_empty() {
            out.push((name, if denom { -e } else { e }));
        }
        denom = next_denom;
    }
    out
}

/// The dimension of a base expansion as text over the base
/// dimensions (spec: Dimensions): `L/T` for `m/s`, `L^2*M/T^2` for
/// `m^2*kg/s^2`, `EUR/M` for `EUR/kg`, `1` for a ratio. The
/// factors keep the expansion's canonical order, so two equal
/// dimensions print identically.
pub fn dimension_of(base: &str) -> String {
    if base == "1" || base.is_empty() {
        return "1".to_string();
    }
    tables()
        .1
        .dimension(base)
        .map(|d| d.to_string())
        .unwrap_or_else(|_| base.to_string())
}

/// `exchange(RATES[; TARGET])` (spec: The exchange): the one
/// opaque factor of the topic's unit re-expressed in the target
/// through the rates record, the other factors carried. `rates`
/// is a record whose keys are opaque units and whose values are
/// quantities in another (one key being that much of the value),
/// read in both directions, chaining allowed. Buy and sell rates
/// differ, so a direct entry source→target is taken as written
/// before the inverse of the entry target→source, and a chain
/// through other currencies comes last, the shortest first, each
/// leg a direct entry before an inverted one; a record holding
/// both directions never inverts. Without a target the record
/// holds one entry and the target is its other side. `Err` is a
/// refusal by name; the null cases are `Ok(Null)`.
pub fn exchange(
    topic: &crate::value::Value,
    rates: &crate::value::Value,
    target: Option<&str>,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Result<crate::value::Value, String> {
    use crate::value::Value;
    let lift = |v: &Value| -> Option<(f64, String, Option<(f64, String)>)> {
        match v {
            Value::Quantity {
                value,
                base,
                written,
            } => Some((*value, base.clone(), written.clone())),
            Value::Str(s) => parse_unit_text_with(s, scale)
                .map(|(value, base, wv, wu)| (value, base, Some((wv, wu)))),
            _ => None,
        }
    };
    // A unit name canonicalized through the table: an alias of a
    // currency is the currency.
    let canon = |name: &str| -> Option<String> {
        let name = name.trim();
        let (_, base) = scale(name).or_else(|| scale_expr(name))?;
        let factors = base_factors(&base);
        match factors.as_slice() {
            [(one, 1)] if !BASE_NAMES.contains(one) => Some(one.to_string()),
            _ => None,
        }
    };
    let Value::Record(fields) = rates else {
        return Err("exchange: the rates are a record of currency = quantity pairs".into());
    };
    // The rate graph: unit → (unit, factor), both directions, an
    // entry as written marked direct.
    let mut edges: Vec<(String, String, f64, bool)> = Vec::new();
    for (key, v) in fields.iter() {
        let Some(from) = canon(key) else {
            return Err(format!("exchange: '{key}' is not an opaque unit"));
        };
        let Some((rate, base, _)) = lift(v) else {
            return Err(format!("exchange: the rate of '{key}' is not a quantity"));
        };
        let factors = base_factors(&base);
        let [(to, 1)] = factors.as_slice() else {
            return Err(format!(
                "exchange: the rate of '{key}' is not in one opaque unit"
            ));
        };
        if BASE_NAMES.contains(to) || rate <= 0.0 {
            return Err(format!(
                "exchange: the rate of '{key}' is not in one opaque unit"
            ));
        }
        edges.push((from.clone(), to.to_string(), rate, true));
        edges.push((to.to_string(), from, 1.0 / rate, false));
    }
    let Some((value, base, written)) = lift(topic) else {
        return Ok(Value::Null);
    };
    let factors = base_factors(&base);
    let opaque: Vec<(&str, i64)> = factors
        .iter()
        .filter(|(n, _)| !BASE_NAMES.contains(n))
        .copied()
        .collect();
    let [(from, exp)] = opaque.as_slice() else {
        return Ok(Value::Null);
    };
    let to = match target {
        Some(t) => match canon(t) {
            Some(t) => t,
            None => return Ok(Value::Null),
        },
        None => {
            if fields.len() != 1 {
                return Err(
                    "exchange: a rates record of several entries needs a target: exchange(RATES; TARGET)"
                        .into(),
                );
            }
            let (a, b, _, _) = &edges[0];
            if *from == a.as_str() {
                b.clone()
            } else if *from == b.as_str() {
                a.clone()
            } else {
                return Ok(Value::Null);
            }
        }
    };
    if *from == to.as_str() {
        return Ok(Value::Quantity {
            value,
            base,
            written,
        });
    }
    // The cheapest way over the rates, the product of its legs: the
    // fewest legs first, and among ways of one length the fewest
    // inverted legs, so a direct entry beats its inverse and a
    // one-entry way beats a chain.
    let mut best: Vec<(String, (usize, usize), f64)> = vec![(from.to_string(), (0, 0), 1.0)];
    let mut queue = std::collections::VecDeque::from([from.to_string()]);
    while let Some(at) = queue.pop_front() {
        let (cost, k) = best
            .iter()
            .find(|(n, _, _)| *n == at)
            .map(|(_, c, k)| (*c, *k))
            .expect("queued nodes are known");
        for (a, b, r, direct) in &edges {
            if *a != at {
                continue;
            }
            let c = (cost.0 + 1, cost.1 + usize::from(!*direct));
            match best.iter_mut().find(|(n, _, _)| n == b) {
                None => {
                    best.push((b.clone(), c, k * r));
                    queue.push_back(b.clone());
                }
                Some(known) if c < known.1 => {
                    known.1 = c;
                    known.2 = k * r;
                    queue.push_back(b.clone());
                }
                Some(_) => {}
            }
        }
    }
    let Some(rate) = best.iter().find(|(n, _, _)| *n == to).map(|(_, _, k)| *k) else {
        return Ok(Value::Null);
    };
    let k = rate.powi(*exp as i32);
    let swap = |text: &str| -> String {
        let mut out = String::new();
        let mut name = String::new();
        let flush = |name: &mut String, out: &mut String| {
            if !name.is_empty() {
                // the factor itself, or an alias of it (`€` for EUR)
                if name.as_str() == *from || canon(name).as_deref() == Some(*from) {
                    out.push_str(&to);
                } else {
                    out.push_str(name);
                }
                name.clear();
            }
        };
        for c in text.chars() {
            if is_name_char(c) {
                name.push(c);
            } else {
                flush(&mut name, &mut out);
                out.push(c);
            }
        }
        flush(&mut name, &mut out);
        out
    };
    Ok(Value::Quantity {
        value: value * k,
        base: swap(&base),
        written: written.map(|(wv, wu)| (wv * k, swap(&wu))),
    })
}

/// The largest exponent a unit factor may carry (`m^32`); beyond
/// it the text is not a unit.
pub const MAX_EXPONENT: i64 = 32;

fn intern_base_name(name: &str) -> &'static str {
    static INTERNED: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut table = INTERNED.lock().expect("base-name intern table");
    if let Some(hit) = table.iter().find(|s| **s == name) {
        return hit;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    table.push(leaked);
    leaked
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
    // the run's resolver first, so a declared unit reads here as it
    // does under `convert`; the built-in table outside a run
    parse_unit_text_with(s, &|e| {
        crate::value::ambient::unit_scale(e).unwrap_or_else(|| scale_expr(e))
    })
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
    // A prefix alias before the magnitude (`$12`, `€ 12.50`): a
    // declared alias ending in `-`, glued or spaced, canonicalizing
    // to the primary name.
    if !b
        .first()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
    {
        let pre: String = s.chars().take_while(|&c| is_name_char(c)).collect();
        if pre.is_empty() {
            return None;
        }
        let rest = s[pre.len()..].trim_start();
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
            .unwrap_or(rest.len());
        if end == 0 || !rest[end..].trim().is_empty() {
            return None;
        }
        let value: f64 = rest[..end].parse().ok()?;
        let (factor, base) = with_opaque(false, || scale(&format!("{pre}-")))?;
        let shown = match base_factors(&base).as_slice() {
            [(one, 1)] if !BASE_NAMES.contains(one) => one.to_string(),
            _ => pre,
        };
        return Some((scale_by(value, factor), base, value, shown));
    }
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
    // Nothing, a space or an underscore between the magnitude and
    // the unit; the underscore is the canonical separator and the
    // one that reads an opaque unit.
    let (underscored, unit) = match s[i..].strip_prefix('_') {
        Some(u) => (true, u),
        None => (false, s[i..].trim_start()),
    };
    // A unit name is letters in any script (a declaration may name
    // `пуд`) or a currency symbol, with digits and the operators of
    // an expression. Words with spaces (`5 metric tonnes`, `12
    // million dollars`) read through the dictionary (the
    // header-units round, 2026-11-09): the expression they name, the
    // scale they carry on the magnitude.
    if !underscored
        && unit.contains(' ')
        && let Ok(Some(w)) = crate::value::unit_words::parse_unit_words(unit, &|_| None, &|sym| {
            with_opaque(false, || scale(sym)).is_some()
        })
    {
        let (factor, base) = with_opaque(false, || scale(&w.expr))?;
        if base == "1" && w.expr != "rad" && w.expr != "sr" && w.expr != "%" {
            return None;
        }
        let magnitude = value * w.factor;
        return Some((scale_by(magnitude, factor), base, magnitude, w.expr));
    }
    if unit.is_empty()
        || !unit
            .chars()
            .all(|c| is_name_char(c) || c.is_ascii_digit() || matches!(c, '*' | '/' | '^'))
        || !unit.chars().next().is_some_and(is_name_char)
    {
        return None;
    }
    // An opaque unit reads through the underscore alone: `12_EUR`
    // is a price, `2nd`, `5G` and `2 copies` are text.
    let (factor, base) = with_opaque(underscored, || scale(unit))?;
    // the dimension-one units read as quantities (the radian, the
    // steradian, the percent); any other dimensionless result is a
    // plain number in disguise
    if base == "1" && unit != "rad" && unit != "sr" && unit != "%" && !unit.contains('/') {
        return None;
    }
    Some((scale_by(value, factor), base, value, unit.to_string()))
}

/// `scale` / `scale(--binary)` (spec: Quantity Commands): the quantity
/// re-expressed in the prefix of its unit's own family that brings
/// the magnitude to one or more (`1400000_B` is `1.4_MB`), display
/// only. A compound unit, a unit without prefixes (`mi`) and an
/// opaque unit are unchanged; `binary` steps Ki..Ei on the bit and
/// the byte instead of the decimal prefixes.
pub fn scale_prefix(
    q: &crate::value::Value,
    binary: bool,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> crate::value::Value {
    use crate::value::Value;
    let lifted;
    let q = match q {
        Value::Str(s) => match parse_unit_text_with(s, scale) {
            Some((value, base, wv, wu)) => {
                lifted = Value::Quantity {
                    value,
                    base,
                    written: Some((wv, wu)),
                };
                &lifted
            }
            None => return Value::Null,
        },
        q => q,
    };
    let Value::Quantity {
        value,
        base,
        written,
    } = q
    else {
        return Value::Null;
    };
    let unit = written.as_ref().map_or(base.as_str(), |(_, u)| u.as_str());
    // a span of time walks the time ladder, not the SI prefixes
    if base == "s" {
        let (v, u) = time_scaled(*value);
        return Value::Quantity {
            value: *value,
            base: base.clone(),
            written: Some((v, u.to_string())),
        };
    }
    let Some(stem) = prefix_stem(unit) else {
        return q.clone();
    };
    let Some((stem_factor, _)) = unit_scale(stem) else {
        return q.clone();
    };
    let in_stem = value / stem_factor;
    let information = stem == "b" || stem == "B";
    let mut steps: Vec<(&str, f64)> = Vec::new();
    if binary && information {
        for (p, pow) in BINARY_PREFIXES.iter().rev() {
            steps.push((p, 2f64.powi(*pow)));
        }
    } else {
        // the thousand steps only: k, M, G … and m, u, n …, never
        // the hecto, deca, deci or centi
        let mut decimal: Vec<(&str, i32)> = PREFIX_POWERS
            .iter()
            .copied()
            .filter(|(_, pow)| pow % 3 == 0)
            .collect();
        decimal.sort_by_key(|(_, pow)| -pow);
        for (p, pow) in decimal {
            if information && pow < 0 {
                continue;
            }
            steps.push((p, pow10(pow)));
        }
    }
    steps.push(("", 1.0));
    steps.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let magnitude = in_stem.abs();
    let (prefix, factor) = steps
        .iter()
        .find(|(_, f)| magnitude / f >= 1.0)
        .or_else(|| steps.last())
        .copied()
        .unwrap_or(("", 1.0));
    Value::Quantity {
        value: *value,
        base: base.clone(),
        written: Some((unscale_by(in_stem, factor), format!("{prefix}{stem}"))),
    }
}

/// The time ladder `scale` walks, largest first: days down to
/// nanoseconds, never the week (spec: Duration).
const TIME_LADDER: &[(&str, f64)] = &[
    ("d", 86_400.0),
    ("h", 3600.0),
    ("min", 60.0),
    ("s", 1.0),
    ("ms", 1e-3),
    ("us", 1e-6),
    ("ns", 1e-9),
];

/// A span in seconds on the time ladder: the magnitude and the unit
/// that brings it to one or above (`5400` is `1.5 h`); zero is
/// `0 s`.
pub fn time_scaled(seconds: f64) -> (f64, &'static str) {
    if seconds == 0.0 || !seconds.is_finite() {
        return (seconds, "s");
    }
    let magnitude = seconds.abs();
    let (u, f) = TIME_LADDER
        .iter()
        .find(|(_, f)| magnitude / f >= 1.0)
        .or_else(|| TIME_LADDER.last())
        .copied()
        .unwrap_or(("s", 1.0));
    (unscale_by(seconds, f), u)
}

/// The full mixed form of a span (spec: Duration): every non-zero
/// component from days down, space-separated, in span text that
/// reads back (`1h 30min`, `5d 3h 5min`, `1ms 500us`, `0s`); the
/// default display of a span with no written unit and what `dfmt`
/// writes. `iso` gives the ISO-8601 duration instead.
pub fn duration_text(seconds: f64, iso: bool) -> String {
    if iso {
        let (secs, nanos) = crate::value::split_seconds(seconds);
        return crate::temporal::format_duration(secs, nanos);
    }
    duration_text_with(seconds, &crate::durnames::EN, &|n| n.to_string())
}

/// The mixed form in a language's designations (spec: Duration
/// Designations by Locale): every non-zero component from days
/// down, each written as the table has it (`5 j`, `3 Std.`,
/// `2 giorni`, `5天`), the number as `number` renders it, the
/// components separated by a space; what `dfmt(LOCALE)` and the
/// output locale write. The English table is the canonical form.
pub fn duration_text_with(
    seconds: f64,
    names: &crate::durnames::DurationNames,
    number: &dyn Fn(&str) -> String,
) -> String {
    if !seconds.is_finite() {
        return crate::value::format_magnitude(seconds) + "s";
    }
    let neg = seconds < 0.0;
    let total = (seconds.abs() * 1e9).round();
    if total == 0.0 {
        return names.component(3, 0, &number("0"));
    }
    let sign = if neg { "-" } else { "" };
    if total >= 9.0e18 {
        // beyond the nanosecond count's exact range: days alone
        let days = crate::value::format_magnitude(seconds.abs() / 86_400.0);
        return format!("{sign}{}", names.days(&number(&days)));
    }
    let mut rest = total as i128;
    let mut parts = Vec::new();
    for (i, (_, f)) in TIME_LADDER.iter().enumerate() {
        let unit_ns = (*f * 1e9).round() as i128;
        let n = rest / unit_ns;
        if n > 0 {
            parts.push(names.component(i, n as u64, &number(&n.to_string())));
            rest -= n * unit_ns;
        }
    }
    format!("{sign}{}", parts.join(" "))
}

/// The prefixable stem of a single-factor unit: the unit itself
/// when the table prefixes it, else what a decimal or binary prefix
/// leaves (`kB` → `B`, `us` → `s`, `KiB` → `B`). None for a
/// compound, an opaque unit or one without prefixes.
fn prefix_stem(unit: &str) -> Option<&str> {
    if unit.is_empty() || unit.contains(['*', '/', '^']) {
        return None;
    }
    if full_name_scale(unit).is_some() && prefixable(unit) {
        return Some(unit);
    }
    for (p, _) in BINARY_PREFIXES {
        if let Some(stem) = unit.strip_prefix(p)
            && (stem == "b" || stem == "B")
        {
            return Some(stem);
        }
    }
    for (p, _) in PREFIX_POWERS {
        if let Some(stem) = unit.strip_prefix(p)
            && prefixable(stem)
            && full_name_scale(stem).is_some()
        {
            return Some(stem);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn standard_units_resolve_however_spelled() {
        // The ruling of 2026-10-31: kWh is an energy, keV an energy,
        // mbar a pressure; the CGS and other pre-SI units are in the
        // table; the names that were in it keep their reading.
        let close = |a: f64, b: f64| ((a - b) / b).abs() < 1e-12;
        for (text, factor, base) in [
            ("kWh", 3.6e6, "m^2*kg/s^2"),
            ("MWh", 3.6e9, "m^2*kg/s^2"),
            ("mAh", 3.6, "s*A"),
            ("mbar", 100.0, "kg/m/s^2"),
            ("keV", 1.602_176_634e-16, "m^2*kg/s^2"),
            ("GeV", 1.602_176_634e-10, "m^2*kg/s^2"),
            ("Mt", 1e9, "kg"),
            ("Mcal", 4.184e6, "m^2*kg/s^2"),
            ("erg", 1e-7, "m^2*kg/s^2"),
            ("dyn", 1e-5, "m*kg/s^2"),
            ("cP", 1e-3, "kg/m/s"),
            ("cSt", 1e-6, "m^2/s"),
            ("mGal", 1e-5, "m/s^2"),
            ("kG", 0.1, "kg/s^2/A"),
            ("kOe", 1e6 / (4.0 * std::f64::consts::PI), "A/m"),
            ("Torr", 101_325.0 / 760.0, "kg/m/s^2"),
            ("Å", 1e-10, "m"),
            ("ft", 0.3048, "m"),
            ("pt", 0.0254 / 72.0, "m"),
            ("kcal", 4184.0, "m^2*kg/s^2"),
            ("gal", 0.003_785_411_784, "m^3"),
            ("эрг", 1e-7, "m^2*kg/s^2"),
            ("кэВ", 1.602_176_634e-16, "m^2*kg/s^2"),
        ] {
            let (f, b) = scale_expr(text).unwrap_or_else(|| panic!("{text} resolves"));
            assert!(close(f, factor), "{text}: {f} vs {factor}");
            assert_eq!(b, base, "{text}");
        }
        for opaque in ["Kbps", "widget", "apples"] {
            let (_, b) = scale_expr(opaque).unwrap();
            assert!(has_opaque_factor(&b), "{opaque} stays opaque");
        }
    }

    use super::*;

    /// Finding 19: an exponent beyond any unit is not a unit (it
    /// would overflow the exponent map and the factor).
    #[test]
    fn absurd_exponents_are_not_units() {
        assert!(parse_unit_text("1 m^99999999999").is_none());
        assert!(parse_unit_text("1 m^-99999999999").is_none());
        assert!(parse_unit_text("1 m^3").is_some());
    }

    #[test]
    fn scale_table() {
        assert_eq!(unit_scale("km"), Some((1000.0, "m")));
        assert_eq!(unit_scale("mg"), Some((1e-6, "kg")));
        assert_eq!(unit_scale("kW"), Some((1000.0, "m^2*kg/s^3")));
        assert_eq!(unit_scale("mi"), Some((1609.344, "m")));
        // Full names beat prefixes; unknown names are out. Time is
        // the dimension T, a quantity like the others.
        assert_eq!(unit_scale("Pa").map(|(_, b)| b), Some("kg/m/s^2"));
        assert_eq!(unit_scale("min"), Some((60.0, "s")));
        assert_eq!(scale_expr("min"), Some((60.0, "s".to_string())));
        assert_eq!(scale_expr("s"), Some((1.0, "s".to_string())));
        assert_eq!(unit_scale("w"), Some((604_800.0, "s")));
        assert_eq!(unit_scale("hours"), Some((3600.0, "s")));
        assert_eq!(unit_scale("m"), Some((1.0, "m")));
        assert_eq!(unit_scale("parsec"), None);
    }

    #[test]
    fn a_unit_operand_must_be_known() {
        // the table, prefixed forms and compounds pass
        for ok in [
            "kB", "KiB", "km/h", "m*kg/s^2", "EUR/lb", "mi^2", "USD", "Mbps", "1/s",
        ] {
            assert_eq!(unit_operand_refusal(ok, &[]), None, "{ok}");
        }
        // the ambiguous kilobyte names its two readings
        assert_eq!(
            unit_operand_refusal("KB", &[]).as_deref(),
            Some("no unit 'KB' (kB is 1000 B, KiB is 1024 B)")
        );
        assert_eq!(
            unit_operand_refusal("Kb", &[]).as_deref(),
            Some("no unit 'Kb' (kb is 1000 b, Kib is 1024 b)")
        );
        assert_eq!(
            unit_operand_refusal("Кбайт", &[]).as_deref(),
            Some("no unit 'Кбайт' (кбайт is 1000 байт, Кибайт is 1024 байт)")
        );
        // (the readings the message names do read)
        assert_eq!(unit_scale("кбайт"), Some((8000.0, "b")));
        // the short byte designation Б with the ГОСТ prefixes, and
        // the IEC Zi and Yi in Cyrillic (the ruling of 2026-10-31)
        assert_eq!(unit_scale("Б"), Some((8.0, "b")));
        assert_eq!(unit_scale("кБ"), Some((8000.0, "b")));
        assert_eq!(unit_scale("МБ"), Some((8e6, "b")));
        assert_eq!(unit_scale("Мбит"), Some((1e6, "b")));
        assert_eq!(unit_scale("ЗиБ").map(|(f, _)| f), Some(8.0 * 2f64.powi(70)));
        assert_eq!(unit_scale("Йибит").map(|(f, _)| f), Some(2f64.powi(80)));
        assert_eq!(designated_unit("kB", "ru"), "кбайт");
        assert_eq!(scale_expr("ml"), Some((1e-6, "m^3".to_string())));
        assert_eq!(unit_scale("Кибайт"), Some((8192.0, "b")));
        assert_eq!(unit_scale("кбит"), Some((1000.0, "b")));
        assert_eq!(unit_scale("Кибит"), Some((1024.0, "b")));
        // a prefix below one on the byte
        assert_eq!(
            unit_operand_refusal("mB", &[]).as_deref(),
            Some("no unit 'mB' (no prefix below one on the byte; the nearest: MB)")
        );
        // a case slip finds its spelling; a typo its neighbours
        assert_eq!(
            unit_operand_refusal("KBps", &[]).as_deref(),
            Some("no unit 'KBps' (the nearest: kbps)")
        );
        assert_eq!(
            unit_operand_refusal("EUR/lbs", &[]).as_deref(),
            Some("no unit 'lbs' (the nearest: lb)")
        );
        assert_eq!(
            unit_operand_refusal("kilometr", &[]).as_deref(),
            Some("no unit 'kilometr'")
        );
        // a declared unit is known, and a neighbour of one
        let mery = vec!["пуд".to_string(), "верста".to_string()];
        assert_eq!(unit_operand_refusal("пуд", &mery), None);
        assert_eq!(unit_operand_refusal("EUR/пуд", &mery), None);
        assert_eq!(
            unit_operand_refusal("пудъ", &mery).as_deref(),
            Some("no unit 'пудъ' (the nearest: пуд)")
        );
        // every name the table lists resolves
        for name in builtin_unit_names() {
            assert!(unit_scale(&name).is_some(), "{name}");
        }
    }

    #[test]
    fn byte_units() {
        // the bit is the base of information; the byte is eight
        assert_eq!(unit_scale("b"), Some((1.0, "b")));
        assert_eq!(unit_scale("B"), Some((8.0, "b")));
        assert_eq!(unit_scale("kB"), Some((8000.0, "b")));
        assert_eq!(unit_scale("KB"), None);
        assert_eq!(unit_scale("Kb"), None);
        assert_eq!(unit_scale("KiB"), Some((8192.0, "b")));
        assert_eq!(unit_scale("MB"), Some((8e6, "b")));
        assert_eq!(unit_scale("MiB"), Some((8.0 * 1_048_576.0, "b")));
        assert_eq!(unit_scale("GB"), Some((8e9, "b")));
        assert_eq!(unit_scale("GiB"), Some((8.0 * 1_073_741_824.0, "b")));
        assert_eq!(unit_scale("TB"), Some((8e12, "b")));
        assert_eq!(unit_scale("TiB"), Some((8.0 * 1_099_511_627_776.0, "b")));
        assert_eq!(unit_scale("kb"), Some((1000.0, "b")));
        assert_eq!(unit_scale("Kib"), Some((1024.0, "b")));
        assert_eq!(unit_scale("Gb"), Some((1e9, "b")));
        // a millibyte is no unit, nor a decibel, nor a kibimetre
        assert_eq!(unit_scale("mb"), None);
        assert_eq!(unit_scale("mB"), None);
        assert_eq!(unit_scale("db"), None);
        assert_eq!(unit_scale("Kim"), None);
        let gb = unit_scale("GB").unwrap().0;
        let gib = unit_scale("GiB").unwrap().0;
        assert!(gib > gb);
        assert_eq!(
            scale_expr("MB/s").map(|(f, b)| (f, b)),
            Some((8e6, "b/s".to_string()))
        );
        assert_eq!(scale_expr("Mbps"), scale_expr("Mb/s"));
        // the refused spellings never fall through to opaque units
        assert_eq!(scale_expr("KB"), None);
        assert_eq!(scale_expr("mB"), None);
    }

    #[test]
    fn unit_text() {
        let (bv, base, wv, wu) = parse_unit_text("5km").unwrap();
        assert_eq!(
            (bv, base.as_str(), wv, wu.as_str()),
            (5000.0, "m", 5.0, "km")
        );
        let (bv, base, ..) = parse_unit_text("0.2 kW").unwrap();
        assert_eq!((bv, base.as_str()), (200.0, "m^2*kg/s^3"));
        assert_eq!(parse_unit_text("-40mm").map(|t| t.0), Some(-0.04));
        // Compound expressions, quoted in query text.
        let (bv, base, ..) = parse_unit_text("100 km/h").unwrap();
        assert!((bv - 100.0 * 1000.0 / 3600.0).abs() < 1e-9);
        assert_eq!(base, "m/s");
        assert_eq!(
            scale_expr("m*kg/s^2").map(|(f, b)| (f, b)),
            Some((1.0, "m*kg/s^2".to_string()))
        );
        assert_eq!(scale_expr("N").unwrap().1, "m*kg/s^2");
        // a span of time is the quantity of dimension T
        assert_eq!(scale_expr("h"), Some((3600.0, "s".to_string())));
        assert_eq!(
            parse_unit_text("12h").map(|t| (t.0, t.1)),
            Some((43200.0, "s".into()))
        );
        assert_eq!(parse_unit_text("5min").map(|t| t.0), Some(300.0));
        // Not unit text: bare numbers
        assert_eq!(parse_unit_text("300"), None);
        // an undeclared name is an opaque unit through the underscore
        // alone; spaced or glued it is text
        assert_eq!(
            parse_unit_text("5_parsec").map(|t| t.1),
            Some("parsec".into())
        );
        assert_eq!(parse_unit_text("5 parsec"), None);
        assert_eq!(parse_unit_text("5parsec"), None);
        // a currency sign names its currency (the word road): one base
        // with the code, so `12_€ = 12_EUR`
        assert_eq!(parse_unit_text("12_€").map(|t| t.1), Some("EUR".into()));
        assert_eq!(parse_unit_text("12€"), None);
        assert_eq!(parse_unit_text("2nd"), None);
        assert_eq!(
            parse_unit_text("3_EUR/kg").map(|t| t.1),
            Some("EUR/kg".into())
        );
        // the built-in units read glued, spaced or underscored
        assert_eq!(parse_unit_text("5km").map(|t| t.0), Some(5000.0));
        assert_eq!(parse_unit_text("5 km").map(|t| t.0), Some(5000.0));
        assert_eq!(parse_unit_text("5_km").map(|t| t.0), Some(5000.0));
        // the scale command
        let q = |s: &str| {
            parse_unit_text(s)
                .map(|(value, base, wv, wu)| crate::value::Value::Quantity {
                    value,
                    base,
                    written: Some((wv, wu)),
                })
                .unwrap()
        };
        let sc = |v: &crate::value::Value, b: bool| scale_prefix(v, b, &scale_expr).to_string();
        assert_eq!(sc(&q("1400000_B"), false), "1.4_MB");
        assert!(
            sc(&q("1400000_B"), true).starts_with("1.335")
                && sc(&q("1400000_B"), true).ends_with("_MiB")
        );
        assert_eq!(sc(&q("0.002_m"), false), "2_mm");
        assert_eq!(sc(&q("3_mi"), false), "3_mi");
        assert_eq!(sc(&q("2500_kg"), false), "2.5_Mg");
        assert_eq!(sc(&q("0.5_kB"), false), "500_B");
        assert_eq!(dimension_of("EUR/kg"), "EUR/M");
        assert_eq!(dimension_of("m^2*kg/s^2"), "L^2*M/T^2");
        assert_eq!(dimension_of("m/s"), "L/T");
        assert_eq!(dimension_of("1"), "1");
    }

    #[test]
    fn micro_reads_as_u_and_prefixes_are_one_power_of_ten() {
        for name in ["um", "µm", "μm", "мкм"] {
            let (f, base) = unit_scale(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!((f, base), (1e-6, "m"), "{name}");
            assert_eq!(scale_by(5.0, f), 5e-6, "{name}");
        }
        assert_eq!(canonical_unit("µm"), "um");
        assert_eq!(canonical_unit("μs"), "us");
        assert_eq!(canonical_unit("km/µs"), "km/us");
        assert!(unit_scale("µ").is_none());
        assert_eq!(pow10(-9), 1e-9);
        assert_eq!(pow10(24), 1e24);
        // a prefix is one power of ten applied once
        assert_eq!(scale_by(5.0, 1e-6), 5e-6);
        assert_eq!(scale_by(1.5, 1e3), 1500.0);
        assert_eq!(unscale_by(0.005, 1e-3), 5.0);
        assert_eq!(unscale_by(5e-6, 1e-6), 5.0);
        assert_eq!(scale_by(2.0, 3600.0), 7200.0);
        assert_eq!(scale_by(0.0, 1e-6), 0.0);
    }

    fn scaled(name: &str) -> (f64, String) {
        let (f, b) = unit_scale(name).unwrap_or_else(|| panic!("{name}"));
        (f, b.to_string())
    }

    #[test]
    fn omega_reads_as_ohm() {
        let ohm = scaled("ohm");
        assert_eq!(scaled("Ω"), ohm);
        assert_eq!(scaled("\u{2126}"), ohm);
        assert_eq!(scaled("kΩ"), (ohm.0 * 1000.0, ohm.1.clone()));
        assert_eq!(scaled("µΩ"), (ohm.0 * 1e-6, ohm.1.clone()));
        assert_eq!(canonical_unit("kΩ"), "kohm");
        assert_eq!(canonical_unit("V/Ω"), "V/ohm");
    }

    #[test]
    fn cyrillic_designations_read_and_print() {
        assert_eq!(unit_scale("кг"), Some((1.0, "kg")));
        assert_eq!(unit_scale("г"), Some((0.001, "kg")));
        assert_eq!(unit_scale("с"), Some((1.0, "s")));
        assert_eq!(unit_scale("м"), Some((1.0, "m")));
        assert_eq!(unit_scale("см"), Some((0.01, "m")));
        assert_eq!(unit_scale("мс"), Some((0.001, "s")));
        assert_eq!(unit_scale("мкс"), Some((1e-6, "s")));
        assert_eq!(unit_scale("МВт"), Some((1e6, "m^2*kg/s^3")));
        assert_eq!(unit_scale("гПа"), Some((100.0, "kg/m/s^2")));
        assert_eq!(unit_scale("Кибайт"), Some((8192.0, "b")));
        assert_eq!(unit_scale("Мбит"), Some((1e6, "b")));
        assert_eq!(unit_scale("Кбайт"), None);
        assert_eq!(unit_scale("уз").map(|(_, b)| b), Some("m/s"));
        let (f, base) = scale_expr("км/ч").unwrap();
        assert!((f - 1000.0 / 3600.0).abs() < 1e-12);
        assert_eq!(base, "m/s");
        assert_eq!(canonical_unit("км/ч"), "km/h");
        assert_eq!(canonical_unit("кг*м/с^2"), "kg*m/s^2");
        assert_eq!(canonical_unit("EUR/кг"), "EUR/kg");
        assert_eq!(designated_unit("km/h", "ru"), "км/ч");
        assert_eq!(designated_unit("kg", "ru"), "кг");
        assert_eq!(designated_unit("KiB", "ru"), "Кибайт");
        assert_eq!(designated_unit("us", "ru"), "мкс");
        assert_eq!(designated_unit("EUR/kg", "ru"), "EUR/кг");
        assert_eq!(designated_unit("mi", "ru"), "mi");
        assert_eq!(designated_unit("км/ч", "en"), "km/h");
        assert_eq!(designated_unit("пуд", "ru"), "пуд");
        // read with a designation, displayed canonically
        let q =
            parse_unit_text("5 кг").map(|(value, base, wv, wu)| crate::value::Value::Quantity {
                value,
                base,
                written: Some((wv, wu)),
            });
        assert_eq!(q.map(|v| v.to_string()), Some("5_kg".into()));
    }

    /// The currency table is the enumeration the appendix states by
    /// rule: every circulating ISO 4217 code, kept beside the source
    /// `currencies.tsv` (code, name, country, in English and Russian,
    /// prefix sign), which the test holds the table to.
    #[test]
    fn the_currency_table_matches_its_source() {
        let tsv = include_str!("currencies.tsv");
        let mut codes = Vec::new();
        let mut prefixes = Vec::new();
        for line in tsv.lines().filter(|l| !l.trim().is_empty()) {
            let cols: Vec<&str> = line.split('\t').collect();
            codes.push(cols[0]);
            if let Some(sign) = cols.get(5).filter(|s| !s.is_empty()) {
                prefixes.push((sign.to_string(), cols[0]));
            }
        }
        assert_eq!(codes.len(), 154);
        assert_eq!(CURRENCIES, codes.as_slice());
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "alphabetical");
        assert_eq!(prefixes.len(), 18);
        for (sign, code) in &prefixes {
            assert_eq!(
                unit_scale(sign).map(|(f, b)| (f, b)),
                Some((1.0, *code)),
                "{sign} is {code}"
            );
            assert!(is_currency_symbol(sign.chars().next().unwrap()), "{sign}");
        }
        for code in CURRENCIES {
            assert_eq!(unit_scale(code), Some((1.0, *code)));
        }
    }

    #[test]
    fn a_minted_size_reads_beside_unit_text() {
        use crate::value::{Value, quantital_pair_with};
        let size = Value::bytes(2_400_000);
        assert_eq!(size.to_string(), "2400000_B");
        let pair =
            |t: &str| quantital_pair_with(&size, &Value::Str(t.into()), &scale_expr).unwrap();
        let (a, b) = pair("200 kB");
        assert!(a > b);
        let (a, b) = pair("200kB");
        assert!(a > b);
        let (a, b) = pair("1600000 b");
        assert!(a > b);
        let (a, b) = pair("3 MB");
        assert!(a < b);
        // a bare number carries no dimension: no pair with a size
        assert!(quantital_pair_with(&size, &Value::Int(2_400_000), &scale_expr).is_none());
        assert_eq!(
            dimension_of(match &size {
                Value::Quantity { base, .. } => base,
                _ => unreachable!(),
            }),
            "B"
        );
    }
}
