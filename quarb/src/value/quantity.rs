//! The quantity fragment: unit-text parsing behind
//! `Value::Quantity` (spec: Physical Quantities; the table in the
//! appendix Units and Prefixes).
//!
//! A quantity is a value on a dimension: a numeric magnitude scaled
//! to its dimension's base expansion, the written form kept for
//! display only, exactly as instants keep their written offset.
//! The built-in table is the appendix's, hand-pinned here like the
//! civil-calendar arithmetic: spec-law embedded, no dependency. A
//! model's `unit` declarations extend it through the resolver a
//! host passes in, and any other name in unit text is an opaque
//! unit, a base of its own dimension (`12 EUR`, `3 apples/kg`),
//! scaled by nothing: currencies are exchanged by a rate the query
//! supplies (`exchange`), never looked up.
//!
//! The unit model is factor-only (no affine units, so there is no
//! °C to tempt anyone) and pure time is excluded: a span of time is
//! a Duration.

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
];

/// The base units: the SI seven and the bit. Any other factor of a
/// base expansion is an opaque unit.
pub const BASE_NAMES: &[&str] = &["m", "kg", "s", "A", "K", "mol", "cd", "b"];

/// The dimension symbol of a base unit (spec: Dimensions); an
/// opaque unit is its own.
fn dimension_symbol(base: &str) -> &str {
    match base {
        "m" => "L",
        "kg" => "M",
        "s" => "T",
        "A" => "I",
        "K" => "Θ",
        "mol" => "N",
        "cd" => "J",
        "b" => "B",
        other => other,
    }
}

/// The full names of the table, each row the spellings of one unit
/// with its exact factor to the base expansion of its dimension
/// (the appendix's rows); the currencies and their prefix signs
/// stand apart below.
const UNIT_TABLE: &[(&[&str], f64, &str)] = &[
    // the base units
    (&["m"], 1.0, "m"),
    (&["kg"], 1.0, "kg"),
    (&["s"], 1.0, "s"),
    (&["A"], 1.0, "A"),
    (&["K"], 1.0, "K"),
    (&["mol"], 1.0, "mol"),
    (&["cd"], 1.0, "cd"),
    (&["b", "bit", "bits"], 1.0, "b"),
    (&["g"], 0.001, "kg"),
    (&["B", "byte", "bytes"], 8.0, "b"),
    // the named derived units, coherent
    (&["rad", "sr"], 1.0, "1"),
    (&["Hz", "Bq"], 1.0, "1/s"),
    (&["N"], 1.0, "kg*m/s^2"),
    (&["Pa"], 1.0, "kg/m/s^2"),
    (&["J"], 1.0, "kg*m^2/s^2"),
    (&["W"], 1.0, "kg*m^2/s^3"),
    (&["C"], 1.0, "A*s"),
    (&["V"], 1.0, "kg*m^2/A/s^3"),
    (&["F"], 1.0, "A^2*s^4/kg/m^2"),
    (&["ohm"], 1.0, "kg*m^2/A^2/s^3"),
    (&["S"], 1.0, "A^2*s^3/kg/m^2"),
    (&["Wb"], 1.0, "kg*m^2/A/s^2"),
    (&["T"], 1.0, "kg/A/s^2"),
    (&["H"], 1.0, "kg*m^2/A^2/s^2"),
    (&["lm"], 1.0, "cd"),
    (&["lx"], 1.0, "cd/m^2"),
    (&["Gy", "Sv"], 1.0, "m^2/s^2"),
    (&["kat"], 1.0, "mol/s"),
    (&["L"], 0.001, "m^3"),
    // non-SI, exact by definition; time units as factors of
    // compounds only (pure time is a Duration)
    // the time units and their long forms (spec: Duration);
    // `m` is the metre, `min` the only short minute
    (&["sec", "second", "seconds"], 1.0, "s"),
    (&["min", "minute", "minutes"], 60.0, "s"),
    (&["h", "hr", "hour", "hours"], 3600.0, "s"),
    (&["d", "day", "days"], 86400.0, "s"),
    (&["w", "week", "weeks"], 604_800.0, "s"),
    (&["au"], 149_597_870_700.0, "m"),
    (&["ha"], 10_000.0, "m^2"),
    (&["t"], 1000.0, "kg"),
    (&["eV"], 1.602_176_634e-19, "kg*m^2/s^2"),
    (&["bar"], 100_000.0, "kg/m/s^2"),
    (&["atm"], 101_325.0, "kg/m/s^2"),
    (&["cal"], 4.184, "kg*m^2/s^2"),
    (&["kcal"], 4184.0, "kg*m^2/s^2"),
    (&["in"], 0.0254, "m"),
    // The typographic point, 1/72 inch (spec: Units).
    (&["pt"], 0.0254 / 72.0, "m"),
    (&["ft"], 0.3048, "m"),
    (&["yd"], 0.9144, "m"),
    (&["mi"], 1609.344, "m"),
    (&["nmi"], 1852.0, "m"),
    (&["ac"], 4046.856_422_4, "m^2"),
    (&["lb"], 0.453_592_37, "kg"),
    (&["oz"], 0.028_349_523_125, "kg"),
    (&["st"], 6.350_293_18, "kg"),
    (&["gal"], 0.003_785_411_784, "m^3"),
    (&["floz"], 2.957_352_956_25e-5, "m^3"),
    (&["cup"], 2.365_882_365e-4, "m^3"),
    (&["psi"], 6894.757_293_168, "kg/m/s^2"),
    (&["kn"], 1852.0 / 3600.0, "m/s"),
    (&["mph"], 1609.344 / 3600.0, "m/s"),
    // the telecom spellings: decimal bits per second
    (&["bps"], 1.0, "b/s"),
    (&["kbps"], 1e3, "b/s"),
    (&["Mbps"], 1e6, "b/s"),
    (&["Gbps"], 1e9, "b/s"),
    (&["Tbps"], 1e12, "b/s"),
];

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

/// The scale of one full name: a row of the table, a currency code
/// (spec: Currencies — a base unit of its own dimension), or a
/// currency sign.
fn full_name_scale(name: &str) -> Option<(f64, &'static str)> {
    if let Some((_, factor, base)) = UNIT_TABLE
        .iter()
        .find(|(names, _, _)| names.contains(&name))
    {
        return Some((*factor, base));
    }
    if CURRENCIES.contains(&name) {
        return Some((1.0, intern_base_name(name)));
    }
    CURRENCY_SIGNS
        .iter()
        .find(|(sign, _)| *sign == name)
        .map(|(_, code)| (1.0, *code))
}

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

/// The units a decimal prefix attaches to: the base units, the
/// derived units, the gram, the litre, the bit and the byte.
const PREFIXABLE: &[&str] = &[
    "B", "b", "bit", "byte", "m", "s", "g", "A", "K", "mol", "cd", "L", "rad", "sr", "Hz", "N",
    "Pa", "J", "W", "C", "V", "F", "ohm", "S", "Wb", "T", "H", "lm", "lx", "Bq", "Gy", "Sv", "kat",
];

fn prefixable(stem: &str) -> bool {
    PREFIXABLE.contains(&stem)
}

/// What the table says of one name.
#[derive(Debug)]
enum Lookup {
    /// A unit of the table, with its factor and base expansion.
    Scaled(f64, &'static str),
    /// A spelling the table refuses: `KB` and `Kb` (ambiguous:
    /// `kB` or `KiB`), and a shrinking prefix on the bit or the
    /// byte (a millibyte is no unit, and `mB` is the MB typo).
    Refused,
    /// A name the table does not hold: an opaque unit in unit text.
    Unknown,
}

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

/// `v` divided by a unit factor, the inverse of [`scale_by`].
pub fn unscale_by(v: f64, factor: f64) -> f64 {
    match decimal_exponent(factor) {
        Some(e) => shift10(v, -e),
        None => v / factor,
    }
}

fn lookup(name: &str) -> Lookup {
    if let Some((f, base)) = full_name_scale(name) {
        return Lookup::Scaled(f, base);
    }
    if let Some(ascii) = micro_of(name) {
        return lookup(&ascii);
    }
    if let Some(ascii) = ohm_of(name) {
        return lookup(&ascii);
    }
    if name == "KB" || name == "Kb" || name == "Кбайт" || name == "Кбит" {
        return Lookup::Refused;
    }
    // a GOST designation reads as its SI symbol
    if let Some(latin) = latin_of(name) {
        return lookup(&latin);
    }
    for (p, pow) in BINARY_PREFIXES {
        if let Some(stem) = name.strip_prefix(p)
            && matches!(stem, "B" | "b" | "bit" | "byte")
        {
            let (f, base) = full_name_scale(stem).expect("the bit and the byte are in the table");
            return Lookup::Scaled(f * 2f64.powi(*pow), base);
        }
    }
    for (p, pow) in PREFIX_POWERS {
        if let Some(stem) = name.strip_prefix(p)
            && prefixable(stem)
        {
            if matches!(stem, "B" | "b" | "bit" | "byte") && *pow < 0 {
                return Lookup::Refused;
            }
            let (f, base) = full_name_scale(stem).expect("a prefixable unit is in the table");
            return Lookup::Scaled(f * pow10(*pow), base);
        }
    }
    Lookup::Unknown
}

/// The scale of one unit name over the built-in table: the factor
/// to the base and the base expansion. A full name is read before
/// a prefix is stripped (`mi` is the mile, `min` the minute).
pub fn unit_scale(name: &str) -> Option<(f64, &'static str)> {
    match lookup(name) {
        Lookup::Scaled(f, base) => Some((f, base)),
        _ => None,
    }
}

/// Every spelling the built-in table reads as a unit operand: the
/// full names, the currency codes, and the prefixed forms the
/// prefixes admit (a prefix below one never on the bit or the byte).
pub fn builtin_unit_names() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (names, _, _) in UNIT_TABLE {
        out.extend(names.iter().map(|n| n.to_string()));
    }
    out.extend(CURRENCIES.iter().map(|c| c.to_string()));
    for (prefix, pow) in PREFIX_POWERS {
        for stem in PREFIXABLE {
            if *pow < 0 && matches!(*stem, "B" | "b" | "bit" | "byte") {
                continue;
            }
            out.push(format!("{prefix}{stem}"));
        }
    }
    for (prefix, _) in BINARY_PREFIXES {
        for stem in ["B", "b", "bit", "byte"] {
            out.push(format!("{prefix}{stem}"));
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
        match lookup(factor) {
            Lookup::Scaled(..) => {}
            Lookup::Refused => return Some(refused_spelling(factor, declared)),
            Lookup::Unknown => {
                return Some(with_nearest(
                    format!("no unit '{factor}'"),
                    &nearest_units(factor, declared),
                ));
            }
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
/// [`Lookup::Refused`]): the ambiguous kilobyte and kilobit, in
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
    c.is_alphabetic() || is_currency_symbol(c)
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
    scale_expr_inner(expr, custom).map(|(f, base, _)| (f, base))
}

/// Whether a base expansion holds an opaque factor.
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

fn scale_expr_inner(
    expr: &str,
    custom: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<(f64, String, bool)> {
    if expr.is_empty() {
        return None;
    }
    let cs: Vec<char> = expr.chars().collect();
    let mut i = 0;
    let mut factor = 1.0f64;
    let mut base: std::collections::BTreeMap<&'static str, i64> = Default::default();
    let mut op = '*';
    let mut opaque = false;
    loop {
        let start = i;
        while i < cs.len() && is_name_char(cs[i]) {
            i += 1;
        }
        // a prefix alias (`€-`, `$-`) is a name with its trailing dash
        if i > start && i + 1 == cs.len() && cs[i] == '-' {
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
            // An exponent beyond any unit (`m^99999999999`) is not a
            // unit: it would overflow the exponent map and the
            // factor's `powi`.
            if n > MAX_EXPONENT {
                return None;
            }
            exp = if neg { -n } else { n };
        }
        if !one {
            let signed = if op == '*' { exp } else { -exp };
            let (f, expansion): (f64, String) = match custom(&name) {
                Some(hit) => hit,
                None => match lookup(&name) {
                    Lookup::Scaled(f, b) => (f, b.to_string()),
                    Lookup::Refused => return None,
                    Lookup::Unknown => {
                        if !OPAQUE_ALLOWED.with(|c| c.get()) {
                            return None;
                        }
                        opaque = true;
                        (1.0, name.clone())
                    }
                },
            };
            factor *= f.powi(signed as i32);
            accumulate(&mut base, intern_base_name(&expansion), signed);
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
    Some((factor, format_base(&base), opaque))
}

/// The dimension of a base expansion as text over the base
/// dimensions (spec: Dimensions): `L/T` for `m/s`, `M*L^2/T^2` for
/// `kg*m^2/s^2`, `EUR/M` for `EUR/kg`, `1` for a ratio. The
/// factors keep the expansion's canonical order, so two equal
/// dimensions print identically.
pub fn dimension_of(base: &str) -> String {
    if base == "1" || base.is_empty() {
        return "1".to_string();
    }
    let mut out = String::new();
    let mut denom = false;
    for part in base.split_inclusive(['*', '/']) {
        let (tok, sep) = match part.strip_suffix('*') {
            Some(t) => (t, Some('*')),
            None => match part.strip_suffix('/') {
                Some(t) => (t, Some('/')),
                None => (part, None),
            },
        };
        let (name, exp) = match tok.split_once('^') {
            Some((n, x)) => (n, Some(x)),
            None => (tok, None),
        };
        if denom && out.ends_with('*') {
            out.pop();
            out.push('/');
        }
        out.push_str(dimension_symbol(name));
        if let Some(x) = exp {
            out.push('^');
            out.push_str(x);
        }
        if let Some(c) = sep {
            if c == '/' {
                denom = true;
            }
            out.push(c);
        }
    }
    out
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
            Some((n, x)) => (
                n,
                x.parse::<i64>()
                    .unwrap_or(1)
                    .clamp(-MAX_EXPONENT, MAX_EXPONENT),
            ),
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

/// The base names the built-in table's expansions mention — the
/// seven SI bases plus the byte — as 'static strs. Anything else
/// (an adapter's custom base) is interned: one leaked copy per
/// distinct name for the process lifetime, never one per parse.
fn leak_base_name(name: &str) -> &'static str {
    match name {
        "m" => "m",
        "kg" => "kg",
        "s" => "s",
        "A" => "A",
        "K" => "K",
        "mol" => "mol",
        "cd" => "cd",
        "B" => "B",
        other => intern_base_name(other),
    }
}

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
    // an expression.
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
    if base == "1" && unit != "rad" && unit != "sr" && !unit.contains('/') {
        return None;
    }
    Some((scale_by(value, factor), base, value, unit.to_string()))
}

/// `scale` / `scale(binary)` (spec: Quantity Commands): the quantity
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
        return crate::value::format_float(seconds) + "s";
    }
    let neg = seconds < 0.0;
    let total = (seconds.abs() * 1e9).round();
    if total == 0.0 {
        return names.component(3, 0, &number("0"));
    }
    let sign = if neg { "-" } else { "" };
    if total >= 9.0e18 {
        // beyond the nanosecond count's exact range: days alone
        let days = crate::value::format_float(seconds.abs() / 86_400.0);
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
        assert_eq!(unit_scale("kW"), Some((1000.0, "kg*m^2/s^3")));
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
            "kB", "KiB", "km/h", "kg*m/s^2", "EUR/lb", "mi^2", "USD", "Mbps", "1/s",
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
            unit_operand_refusal("kilometer", &[]).as_deref(),
            Some("no unit 'kilometer'")
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
        assert_eq!(parse_unit_text("12_€").map(|t| t.1), Some("€".into()));
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
        assert_eq!(dimension_of("kg*m^2/s^2"), "M*L^2/T^2");
        assert_eq!(dimension_of("m/s"), "L/T");
        assert_eq!(dimension_of("1"), "1");
    }

    #[test]
    fn micro_reads_as_u_and_prefixes_are_one_power_of_ten() {
        for name in ["um", "µm", "μm", "мкм"] {
            match lookup(name) {
                Lookup::Scaled(f, base) => {
                    assert_eq!((f, base), (1e-6, "m"), "{name}");
                    assert_eq!(scale_by(5.0, f), 5e-6, "{name}");
                }
                other => panic!("{name}: {other:?}"),
            }
        }
        assert_eq!(canonical_unit("µm"), "um");
        assert_eq!(canonical_unit("μs"), "us");
        assert_eq!(canonical_unit("km/µs"), "km/us");
        assert!(matches!(lookup("µ"), Lookup::Unknown));
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
        match lookup(name) {
            Lookup::Scaled(f, b) => (f, b.to_string()),
            other => panic!("{name}: {other:?}"),
        }
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
        assert_eq!(unit_scale("МВт"), Some((1e6, "kg*m^2/s^3")));
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
