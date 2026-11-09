//! The unit suffix convention on demand (spec: Mounts, the autounits
//! option; ruling 2026-11-08, the 0.57 round): `?autounits` on any
//! target reads every property whose last `_` segment names a unit
//! the run knows — `body_mass_g`, `latency_ms`, `масса_кг` — as the
//! property without the suffix holding quantities in that unit;
//! `?autounit=_km,_g` converts the named suffixes only. Never on by
//! default: an unknown suffix reads as an opaque unit in query text,
//! so a convention that fired by itself would turn `created_at` into
//! attotonnes and flip `::body_mass_g > 4000` silently (lang's
//! corpus scan, 2026-11-07).
//!
//! The suffix is matched byte for byte against the run's unit table —
//! the built-ins, the GOST designations, a model's `unit`
//! declarations — as spaced unit text, so no opaque unit qualifies
//! and a spelling the grammar refuses (`mb`) is skipped, not an
//! error. A numeric cell or numeric text is minted in the unit; a
//! quantity cell of the dimension keeps its base value and is shown
//! in the unit; null stays null; any other cell reads as before. The
//! suffixed name no longer answers. Two columns stripping to one
//! name, or a stripped name the rows already carry, refuse. The
//! conversions are told under `--info`, once per header.
//!
//! Two suffixes are temporal, not unital (ruling 2026-11-08): `_at`
//! types the column as instants, `_on` as dates at day precision —
//! in place, the name kept (`::created_at`, since `::created` would
//! read as a boolean) — a text cell by the ISO reading, a number by
//! the one epoch rule (seconds; milliseconds from 10^11 up, as the
//! `<instant>` cast and `datetime` read it). Under the global flag a
//! short list of English
//! words a column's tail uses is never read as a unit (`in` is the
//! preposition there, not the inch); the named form converts what
//! it names. Either way a column converts only when the deciding
//! row's cell is a number, numeric text or a reading of the kind;
//! a text cell that is none of these leaves the column as it is.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::graph::adapter::{AstAdapter, Conjunct, Estimate, Selection, StepPlan};
use crate::value::quantity::{is_name_char, parse_unit_text, unscale_by};
use crate::{Diagnostic, NodeId, Value};

/// Which suffixes a mount converts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suffixes {
    /// `?autounits`: every suffix the run's unit table knows.
    Known,
    /// `?autounit=_km,_g`: the named ones, held without their
    /// underscore (`km`, `g`).
    Named(Vec<String>),
}

impl Suffixes {
    /// The union of two option values on one target: the flag wins
    /// over any list, two lists merge.
    pub fn merge(self, other: Suffixes) -> Suffixes {
        match (self, other) {
            (Suffixes::Known, _) | (_, Suffixes::Known) => Suffixes::Known,
            (Suffixes::Named(mut a), Suffixes::Named(b)) => {
                for s in b {
                    if !a.contains(&s) {
                        a.push(s);
                    }
                }
                Suffixes::Named(a)
            }
        }
    }

    /// The option as written, for messages: `autounits` or
    /// `autounit=_km,_g`.
    fn spelled(&self) -> String {
        match self {
            Suffixes::Known => "autounits".to_string(),
            Suffixes::Named(list) => format!(
                "autounit={}",
                list.iter()
                    .map(|s| format!("_{s}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

/// One named suffix as the option writes it, `_km`: the underscore
/// a quantity writes (`12_km`), then a unit name. The name without
/// its underscore; a spelling without one, or shaped as no unit, is
/// refused with the pointer at the spelling.
pub fn named_suffix(text: &str) -> Result<String, String> {
    let text = text.trim();
    let Some(u) = text.strip_prefix('_') else {
        return Err(if text.is_empty() {
            "autounit= names no suffix: the suffix of 'length_km' is '_km' ('autounit=_km'); \
             every known suffix is 'autounits'"
                .to_string()
        } else {
            format!(
                "autounit={text}: a suffix is written with its underscore, as the column \
                 carries it — the suffix of 'length_{text}' is '_{text}' ('autounit=_{text}')"
            )
        });
    };
    if u.is_empty()
        || !u.chars().next().is_some_and(is_name_char)
        || !u.chars().all(|c| is_name_char(c) || c.is_ascii_digit())
    {
        return Err(format!(
            "autounit=_{u}: '{u}' is shaped as no unit (a unit is letters, as 'km' or 'кг')"
        ));
    }
    Ok(u.to_string())
}

/// How a converted column reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// In the unit: `body_mass_g` → `body_mass` in g.
    Unit(String),
    /// As instants: `created_at` → `created`.
    Instant,
    /// As dates, day precision: `published_on` → `published`.
    Date,
    /// A unit read from the words of a title with a scale on the
    /// cells (`[millions of cubic feet per second]`: `ft^3/s`, the
    /// cells ×1e6) — the unit expression and the factor (the
    /// header-units round, the word road).
    Scaled(String, f64),
    /// A rename alone: the slugged title under `slugs`, the cells as
    /// they are.
    Plain,
}

impl Reading {
    /// The unit a reading converts into, when it converts.
    pub fn unit(&self) -> Option<&str> {
        match self {
            Reading::Unit(u) | Reading::Scaled(u, _) => Some(u),
            _ => None,
        }
    }

    /// The factor the cells take before the unit (1 but for a
    /// scaled reading).
    pub fn factor(&self) -> f64 {
        match self {
            Reading::Scaled(_, f) => *f,
            _ => 1.0,
        }
    }
}

impl Reading {
    /// Whether the reading keeps the property's name: the temporal
    /// ones type the column in place, a unit renames.
    fn in_place(&self) -> bool {
        !matches!(
            self,
            Reading::Unit(_) | Reading::Scaled(..) | Reading::Plain
        )
    }

    /// The info line's entry for one column: `length_km → length in
    /// km`, `created_at: instants (epoch seconds)`, `published_on:
    /// dates`.
    fn told(&self, name: &str, stem: &str, cell: Option<&Value>) -> String {
        match self {
            Reading::Unit(u) => format!("{name} → {stem} in {u}"),
            Reading::Scaled(u, f) => format!("{name} → {stem} in {u}, cells ×{}", scale_text(*f)),
            Reading::Plain => format!("{name} → {stem}"),
            Reading::Instant | Reading::Date => {
                let kind = if matches!(self, Reading::Instant) {
                    "instants"
                } else {
                    "dates"
                };
                let epoch = match cell {
                    Some(Value::Int(n)) => Some(*n as f64),
                    Some(Value::Float(x)) => Some(*x),
                    Some(Value::Str(s)) => s.trim().parse::<f64>().ok(),
                    _ => None,
                };
                match epoch {
                    Some(x) if x.abs() >= crate::value::EPOCH_MILLIS_FROM => {
                        format!("{name}: {kind} (epoch milliseconds)")
                    }
                    Some(_) => format!("{name}: {kind} (epoch seconds)"),
                    None => format!("{name}: {kind}"),
                }
            }
        }
    }
}

/// The temporal suffixes and their readings.
const TEMPORAL: &[(&str, Reading)] = &[("at", Reading::Instant), ("on", Reading::Date)];

/// The English words the global flag never reads as a unit: a
/// column's tail says a preposition there, never the attotonne (`at`)
/// or the inch (`in`). The named form converts what it names, stop
/// list or not. Checked against the table by `stop_list_covers_the_
/// table_words`.
const STOP: &[&str] = &[
    "a", "an", "at", "in", "on", "by", "to", "of", "as", "is", "it", "or", "no", "so", "do", "if",
    "am", "pm", "up", "for", "per",
];

/// A text cell as an instant: the ISO reading, extended or compact.
fn text_instant(s: &str) -> Option<Value> {
    let t = s.trim();
    crate::temporal::parse_iso(t)
        .or_else(|| crate::temporal::parse_iso_compact(t))
        .map(|(secs, nanos, offset_min)| Value::Instant {
            secs,
            nanos,
            offset_min,
            calendar: Default::default(),
        })
}

/// An instant at the start of its UTC day, no offset: what a date
/// column holds (a midnight instant with no offset prints as a bare
/// date).
fn to_date(v: Value) -> Value {
    match v {
        Value::Instant { secs, .. } => Value::Instant {
            secs: secs - secs.rem_euclid(86_400),
            nanos: 0,
            offset_min: None,
            calendar: Default::default(),
        },
        other => other,
    }
}

/// The unit a column TITLE names at its end — `Draught, m`, `Length
/// (km)`, `Mass [kg]`, `Price, EUR`, `Speed km/h` — as `(u)`, `[u]`,
/// `, u` or a last word after a space (the header rule, ruling
/// 2026-11-09): the stem and the unit, the unit checked against the
/// run's table (spaced, no opaque unit) under the same stop list as
/// the suffix rule; `%` is the percent unit. When no symbol stands
/// there, the tail reads as WORDS (`Throughput, metric tonnes per
/// year` → `t/a`, `Flow [millions of cubic feet per second]` →
/// `ft^3/s` with the cells ×1e6; `unit_words`), the whole parsed
/// tail leaving the title. `None` for a title that names no unit
/// either way; `Err` for a tail that nearly does, with what was read
/// (`'per yer' is no unit`), for the skipped line.
pub fn header_unit(title: &str) -> Option<(String, Reading)> {
    header_unit_with(title, &|_| None).ok().flatten()
}

/// [`header_unit`] with an override of the dictionary (the call's
/// `--units`, a scoped `--unit:col`): a word → the unit's suffix form
/// consulted before the dictionary.
pub fn header_unit_with(
    title: &str,
    resolve: crate::value::unit_words::Override<'_>,
) -> Result<Option<(String, Reading)>, String> {
    let t = title.trim();
    if t.is_empty() {
        return Ok(None);
    }
    // the tails, longest first: `(…)`/`[…]` at the end, the text after
    // the first comma, then the last k words of a comma-less title
    // (`Area in square kilometres`: `in square kilometres`, `square
    // kilometres`, `kilometres`)
    let mut candidates: Vec<(String, String)> = Vec::new();
    let mut push = |stem: &str, unit: &str| {
        let stem = stem.trim_end_matches([',', ' ', ':']).trim();
        let unit = unit.trim();
        if !stem.is_empty() && !unit.is_empty() {
            candidates.push((stem.to_string(), unit.to_string()));
        }
    };
    for (open, close) in [('(', ')'), ('[', ']')] {
        if let Some(inner) = t.strip_suffix(close)
            && let Some(at) = inner.rfind(open)
        {
            push(&inner[..at], &inner[at + 1..]);
        }
    }
    if let Some((stem, unit)) = t.split_once(',') {
        push(stem, unit);
    } else {
        let starts: Vec<usize> = t
            .char_indices()
            .filter(|(i, c)| {
                !c.is_whitespace() && (*i == 0 || t[..*i].ends_with(char::is_whitespace))
            })
            .map(|(i, _)| i)
            .collect();
        for &at in starts.iter().skip(1) {
            push(&t[..at], &t[at..]);
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    // a word of the dictionary that is no symbol of the table takes
    // the word road (`year` is `a`, `tons` the short ton); a symbol
    // keeps its spelling (`кг`, `EUR`)
    let is_word = |u: &str| {
        crate::value::unit_words::word_symbol(u).is_some()
            && crate::value::quantity::unit_scale(u).is_none()
    };
    let symbol_known = |sym: &str| {
        !STOP.contains(&sym) && !TEMPORAL.iter().any(|(s, _)| *s == sym) && scale_of(sym).is_some()
    };
    // the symbol road: a tail that is one symbol the table reads (a
    // word of the dictionary takes the word road, so `year` is `a`)
    for (stem, unit) in &candidates {
        if STOP.contains(&unit.as_str()) || TEMPORAL.iter().any(|(s, _)| *s == unit) {
            continue;
        }
        if let Some(over) = resolve(&unit.to_lowercase())
            && scale_of(&over).is_some()
        {
            return Ok(Some((stem.clone(), Reading::Unit(over))));
        }
        if !unit.contains(' ') && !is_word(unit) && scale_of(unit).is_some() {
            return Ok(Some((stem.clone(), Reading::Unit(unit.clone()))));
        }
    }
    // the word road: the longest tail that reads as words
    let mut told: Option<String> = None;
    for (stem, unit) in &candidates {
        match crate::value::unit_words::parse_unit_words(unit, resolve, &symbol_known) {
            Ok(Some(w)) if scale_of(&w.expr).is_some() => {
                let reading = if w.factor == 1.0 {
                    Reading::Unit(w.expr)
                } else {
                    Reading::Scaled(w.expr, w.factor)
                };
                return Ok(Some((stem.clone(), reading)));
            }
            Ok(_) => {}
            Err(why) => {
                if told.is_none() {
                    told = Some(why.told());
                }
            }
        }
    }
    match told {
        // only a tail of several words is "nearly" a unit; a last
        // word alone that is no unit is an ordinary title
        Some(why) if candidates.iter().any(|(_, u)| u.contains(' ')) => Err(why),
        _ => Ok(None),
    }
}

/// A scale factor for a message: `1e6`, `1000`.
fn scale_text(f: f64) -> String {
    if f >= 1e6 && f.log10().fract() == 0.0 {
        format!("1e{}", f.log10() as i32)
    } else if f.fract() == 0.0 {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
}

/// A title slugged (the `slug` command, `--slugs`, `?slugs`; ruling
/// 2026-11-09): lowercased, every run of characters that are neither
/// letters nor digits replaced by `sep`, the ends trimmed of it —
/// `Draught, m` → `draught_m`, `Порт назначения` → `порт_назначения`.
/// `None` when nothing is left (`—`).
pub fn slug_text(text: &str, sep: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            if pending && !out.is_empty() {
                out.push_str(sep);
            }
            pending = false;
            out.extend(c.to_lowercase());
        } else {
            pending = true;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The title rules applied to one header (the constructor's
/// `--autounits` / `--autounit=:col…` / `--slugs`, `clean`'s, and the
/// pass over a table in hand): the new header, the columns declared
/// in their units, and how a cell of a converted column is respelled
/// for the typed build. Collisions refuse by name; the conversions
/// are told under `--info` (code AUTOUNITS, site the table's).
pub struct TitlePlan {
    pub header: Vec<String>,
    /// Per column, the declared type: the unit of a converted column.
    pub declared: Vec<Option<crate::value::ColumnType>>,
    /// Per column, the factor its cells take before the unit (1 but
    /// for a title whose words name a scale: `millions of …`).
    pub factors: Vec<f64>,
}

impl TitlePlan {
    /// A cell of column `j` scaled as its title asks: a number ×
    /// the factor (text respelled), anything else as it is.
    pub fn scale_cell(&self, j: usize, cell: Value) -> Value {
        let f = self.factors.get(j).copied().unwrap_or(1.0);
        if f == 1.0 {
            return cell;
        }
        match &cell {
            Value::Int(n) => Value::Float(*n as f64 * f),
            Value::Float(x) => Value::Float(x * f),
            Value::Str(t) => match t.trim().parse::<f64>() {
                Ok(x) => Value::Str(format_scaled(x * f)),
                Err(_) => cell,
            },
            _ => cell,
        }
    }
}

/// A scaled number as cell text: whole when whole.
fn format_scaled(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        x.to_string()
    }
}

/// The title rules over `header`: `autounits` is `None` for no unit
/// reading, `Some(&[])` for every column, `Some(names)` for the
/// named columns; `slugs` the separator when the titles are slugged;
/// `sample(j)` the first non-empty cell of column `j` (the
/// deciding-cell guard). Told as one info line on `site`.
pub fn title_plan(
    header: &[String],
    autounits: Option<&[String]>,
    slugs: Option<&str>,
    sample: &dyn Fn(usize) -> Option<String>,
    site: &str,
) -> Result<TitlePlan, String> {
    title_plan_with(
        header,
        autounits,
        slugs,
        sample,
        site,
        &|_, _| None,
        &|_| None,
    )
}

/// [`title_plan`] with the call's dictionary: `resolve(column, word)`
/// answers the unit the call maps a title's word to, for that column
/// (`--unit:col=%(…)`) or for every column (`--units=%(…)`);
/// `declare(column)` the unit a column is declared in regardless of
/// its title (`--unit:col=_lb`): the title loses a unit tail it has,
/// the cells are cast to the unit, told "(declared)".
pub fn title_plan_with(
    header: &[String],
    autounits: Option<&[String]>,
    slugs: Option<&str>,
    sample: &dyn Fn(usize) -> Option<String>,
    site: &str,
    resolve: &dyn Fn(&str, &str) -> Option<String>,
    declare: &dyn Fn(&str) -> Option<String>,
) -> Result<TitlePlan, String> {
    let mut names: Vec<String> = header.to_vec();
    let mut declared: Vec<Option<crate::value::ColumnType>> = vec![None; header.len()];
    let mut factors: Vec<f64> = vec![1.0; header.len()];
    let mut converted: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut slugged: Vec<String> = Vec::new();
    let label = match (
        autounits.is_some() || header.iter().any(|t| declare(t).is_some()),
        slugs,
    ) {
        (true, Some(_)) => "autounits+slugs",
        (true, None) => "autounits",
        _ => "slugs",
    };
    // `--unit:col=_lb`: the column declared in the unit whatever its
    // title says, a unit tail dropped from the title
    let mut declared_cols: Vec<usize> = Vec::new();
    for (j, title) in header.iter().enumerate() {
        let Some(unit) = declare(title) else { continue };
        if scale_of(&unit).is_none() {
            return Err(format!(
                "--unit:{title}={unit}: '{unit}' is no unit the table knows"
            ));
        }
        // the title loses a unit tail it has (a symbol, or words the
        // dictionary knows, the ambiguous `pounds` among them)
        let as_declared = |w: &str| crate::value::unit_words::is_unit_word(w).then(|| unit.clone());
        let stem = match header_unit_with(title, &as_declared) {
            Ok(Some((stem, _))) => stem,
            _ => title.clone(),
        };
        converted.push(format!("{title} → {stem} in {unit} (declared)"));
        declared[j] = Some(crate::value::ColumnType::Unit(unit.as_str().into()));
        names[j] = stem;
        declared_cols.push(j);
    }
    if let Some(only) = autounits {
        // the named form names columns the table has
        for name in only {
            if !header.iter().any(|h| h == name) {
                return Err(format!(
                    "--autounit names column '{name}', and the table has no such column (its columns: {})",
                    header.join(", ")
                ));
            }
        }
        for (j, title) in header.iter().enumerate() {
            if declared_cols.contains(&j) {
                continue;
            }
            if !only.is_empty() && !only.iter().any(|n| n == title) {
                continue;
            }
            let column_resolve = |word: &str| resolve(title, word);
            let (stem, reading) = match header_unit_with(title, &column_resolve) {
                Ok(Some(found)) => found,
                Ok(None) => {
                    if !only.is_empty() {
                        skipped.push(format!("{title} (its title names no unit at its end)"));
                    }
                    continue;
                }
                // a tail of words that nearly names a unit: told
                Err(why) => {
                    skipped.push(format!("{title} ({why})"));
                    continue;
                }
            };
            let cell = sample(j).map(Value::Str);
            let admits = match (&reading, &cell) {
                (_, None) => true,
                (_, Some(Value::Str(c))) => {
                    c.trim().parse::<f64>().is_ok()
                        || (reading.unit().is_some() && parse_unit_text(c).is_some())
                }
                _ => true,
            };
            if !admits {
                skipped.push(format!("{title} (its cells are no numbers)"));
                continue;
            }
            let mut told_line = reading.told(title, &stem, cell.as_ref());
            if reading.unit() == Some("ton") && !title.to_lowercase().contains("short") {
                told_line.push_str(" (short tons; write tonnes for t)");
            }
            converted.push(told_line);
            if let Some(u) = reading.unit() {
                declared[j] = Some(crate::value::ColumnType::Unit(u.into()));
            }
            factors[j] = reading.factor();
            names[j] = stem;
        }
    }
    if let Some(sep) = slugs {
        for (j, name) in names.iter_mut().enumerate() {
            if let Some(slug) = slug_text(name, sep)
                && slug != *name
            {
                slugged.push(format!("{} → {slug}", header[j]));
                *name = slug;
            }
        }
    }
    // the names must be distinct: two titles landing on one refuse
    for (j, name) in names.iter().enumerate() {
        if let Some(k) = names.iter().position(|n| n == name)
            && k != j
        {
            return Err(format!(
                "{label}: '{}' and '{}' would both become '{name}'",
                header[k.min(j)],
                header[k.max(j)]
            ));
        }
    }
    let mut parts: Vec<String> = Vec::new();
    if autounits.is_some() || !declared_cols.is_empty() {
        parts.push(if converted.is_empty() {
            "no title names a unit at its end".to_string()
        } else {
            converted.join(", ")
        });
    }
    if slugs.is_some() {
        // alone the label says it; beside the units the part is named
        let head = if autounits.is_some() { "slugs: " } else { "" };
        parts.push(if slugged.is_empty() {
            format!("{head}every title is a slug already")
        } else {
            format!("{head}{}", slugged.join(", "))
        });
    }
    let mut msg = format!("{label} on {site}: {}", parts.join("; "));
    if !skipped.is_empty() {
        msg.push_str(&format!("; skipped: {}", skipped.join(", ")));
    }
    crate::value::ambient::record_diagnostic(Diagnostic::info("AUTOUNITS", site.to_string(), msg));
    Ok(TitlePlan {
        header: names,
        declared,
        factors,
    })
}

/// The scale of one unit name under the run's table: the factor to
/// the base and the base expansion, as spaced unit text reads it (no
/// opaque unit). `None` for a name that is no unit, or a
/// dimensionless one.
fn scale_of(unit: &str) -> Option<(f64, String)> {
    // a spelling the grammar refuses in query text (`mb`: no fraction
    // of a bit) is no suffix either
    if crate::value::quantity::misprefixed(unit).is_some() {
        return None;
    }
    parse_unit_text(&format!("1 {unit}")).map(|(v, base, _, _)| (v, base))
}

/// The conversions decided for one list of property names.
#[derive(Debug, Default)]
struct Plan {
    /// The stripped name → the source property and its reading.
    by_new: HashMap<String, (String, Reading)>,
    /// The source property → the stripped name.
    by_source: HashMap<String, String>,
    /// Per unit, its factor to the base and the base.
    scales: HashMap<String, (f64, String)>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.by_new.is_empty()
    }

    /// Whether a plan's property names touch a converted column —
    /// under its new name, or under the suffixed name that answers no
    /// more.
    fn touches(&self, plan: &StepPlan<'_>) -> bool {
        let names = plan
            .conjuncts
            .iter()
            .map(|c| match c {
                Conjunct::Equals(n, _) | Conjunct::Compare(n, _, _) => *n,
            })
            .chain(plan.order.map(|(n, _)| n));
        for n in names {
            if self.by_new.contains_key(n) || self.by_source.contains_key(n) {
                return true;
            }
        }
        false
    }

    /// A cell of a converted column as the property answers it.
    fn convert(&self, v: Value, reading: &Reading) -> Value {
        let unit = match reading {
            Reading::Plain => return v,
            Reading::Unit(u) => u,
            // the words' scale on the cell, then the unit
            Reading::Scaled(u, f) => {
                let scaled = match v {
                    Value::Int(n) => Value::Float(n as f64 * f),
                    Value::Float(x) => Value::Float(x * f),
                    Value::Str(s) => match s.trim().parse::<f64>() {
                        Ok(x) => Value::Float(x * f),
                        Err(_) => Value::Str(s),
                    },
                    other => other,
                };
                return self.convert(scaled, &Reading::Unit(u.clone()));
            }
            Reading::Instant | Reading::Date => {
                let inst = match v {
                    Value::Null => Value::Null,
                    Value::Int(n) => Value::from_epoch(n as f64),
                    Value::Float(x) => Value::from_epoch(x),
                    Value::Str(s) => match s.trim().parse::<f64>() {
                        Ok(x) => Value::from_epoch(x),
                        Err(_) => text_instant(&s).unwrap_or(Value::Str(s)),
                    },
                    other => other,
                };
                return match reading {
                    Reading::Date => to_date(inst),
                    _ => inst,
                };
            }
        };
        let Some((f, base)) = self.scales.get(unit) else {
            return v;
        };
        let mint = |n: f64| Value::Quantity {
            value: n * f,
            base: base.clone(),
            written: Some((n, unit.to_string())),
        };
        match v {
            Value::Null => Value::Null,
            Value::Int(n) => mint(n as f64),
            Value::Float(x) => mint(x),
            Value::Str(s) => match s.trim().parse::<f64>() {
                Ok(n) => mint(n),
                Err(_) => match parse_unit_text(&s) {
                    Some((value, held, _, _)) if &held == base => Value::Quantity {
                        value,
                        base: base.clone(),
                        written: Some((unscale_by(value, *f), unit.to_string())),
                    },
                    _ => Value::Str(s),
                },
            },
            Value::Quantity {
                value, base: held, ..
            } if &held == base => Value::Quantity {
                value,
                base: base.clone(),
                written: Some((unscale_by(value, *f), unit.to_string())),
            },
            other => other,
        }
    }
}

/// The adapter `inner` under the convention: node ids are its own,
/// the properties of every node read through the plan its names
/// decide.
pub struct AutoUnits<A: AstAdapter> {
    pub inner: A,
    suffixes: Option<Suffixes>,
    /// `?slugs` / `?slugs=-`: the separator of the slugged names.
    slugs: Option<String>,
    /// The dictionary override of the target (`?units=pounds:_lb,
    /// tons:_t`): a word (lowercase) → the unit it names here, read
    /// before the dictionary.
    units: Vec<(String, String)>,
    /// The target as written, without its options, for the info
    /// line and the refusals.
    target: String,
    /// Per distinct list of property names, the plan: a table's rows
    /// share one header, so one entry serves them all; a document's
    /// nodes each bring their own.
    plans: RefCell<HashMap<Vec<String>, Rc<Plan>>>,
    /// Per node whose rows are asked for, the header beneath it.
    headers: RefCell<HashMap<NodeId, Option<(NodeId, Vec<String>)>>>,
    /// Per parent of rows with a shape, their plan: a table's rows
    /// share one header, so one entry serves them all and no row
    /// read allocates the header to find it.
    by_parent: RefCell<HashMap<NodeId, Rc<Plan>>>,
}

impl<A: AstAdapter> AutoUnits<A> {
    pub fn new(inner: A, suffixes: Suffixes, target: &str) -> Self {
        Self::with(inner, Some(suffixes), None, target)
    }

    /// The wrapper under `?autounits` / `?autounit=` and/or `?slugs`.
    pub fn with(inner: A, suffixes: Option<Suffixes>, slugs: Option<String>, target: &str) -> Self {
        AutoUnits {
            inner,
            suffixes,
            slugs,
            units: Vec::new(),
            target: target.to_string(),
            plans: RefCell::new(HashMap::new()),
            headers: RefCell::new(HashMap::new()),
            by_parent: RefCell::new(HashMap::new()),
        }
    }

    /// This wrapper with a dictionary override (`?units=pounds:_lb`):
    /// each word → the unit it names, in the suffix form or bare.
    pub fn with_units(mut self, units: Vec<(String, String)>) -> Self {
        self.units = units
            .into_iter()
            .map(|(w, u)| (w.to_lowercase(), u.trim_start_matches('_').to_string()))
            .collect();
        self
    }

    /// The unit the target's override names for a word, if any.
    fn units_override(&self, word: &str) -> Option<String> {
        self.units
            .iter()
            .find(|(w, _)| w == word)
            .map(|(_, u)| u.clone())
    }

    /// The options in force, for messages: `autounits`, `autounit=_km`,
    /// `slugs`, `autounits+slugs`.
    fn label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = &self.suffixes {
            parts.push(s.spelled());
        }
        if let Some(sep) = &self.slugs {
            parts.push(if sep == "_" {
                "slugs".to_string()
            } else {
                format!("slugs={sep}")
            });
        }
        parts.join("+")
    }

    /// The plan for one list of names, decided once; the first
    /// decision tells the conversions (an info) or refuses a
    /// collision.
    fn plan_for(&self, names: &[String], sample: &dyn Fn(&str) -> Option<Value>) -> Rc<Plan> {
        if let Some(p) = self.plans.borrow().get(names) {
            return p.clone();
        }
        let plan = Rc::new(self.decide(names, sample));
        self.plans.borrow_mut().insert(names.to_vec(), plan.clone());
        plan
    }

    /// Whether the deciding cell admits the reading: a number,
    /// numeric text, a value of the kind, or null (undecided,
    /// converted); a text cell that is none of these keeps its
    /// column as it is.
    fn admits(reading: &Reading, v: Option<Value>) -> bool {
        match (reading, v) {
            (Reading::Plain, _) => true,
            (_, None | Some(Value::Null)) => true,
            (_, Some(Value::Int(_) | Value::Float(_))) => true,
            (Reading::Unit(_) | Reading::Scaled(..), Some(Value::Quantity { .. })) => true,
            (Reading::Unit(_) | Reading::Scaled(..), Some(Value::Str(s))) => {
                s.trim().parse::<f64>().is_ok() || parse_unit_text(&s).is_some()
            }
            (Reading::Instant | Reading::Date, Some(Value::Instant { .. })) => true,
            (Reading::Instant | Reading::Date, Some(Value::Str(s))) => {
                s.trim().parse::<f64>().is_ok() || text_instant(&s).is_some()
            }
            _ => false,
        }
    }

    fn decide(&self, names: &[String], sample: &dyn Fn(&str) -> Option<Value>) -> Plan {
        let mut plan = Plan::default();
        let mut skipped: Vec<String> = Vec::new();
        let mut converted: Vec<String> = Vec::new();
        let mut slugged: Vec<String> = Vec::new();
        let label = self.label();
        let refuse = |msg: String| {
            crate::value::ambient::record_refusal_coded(
                "AUTOUNITS",
                self.target.clone(),
                format!("{label} on {}: {msg}", self.target),
            );
        };
        // Pass one, the unit convention: the suffix rule, then the
        // header rule (the unit at the end of the title), each name
        // read once; what neither converts keeps its name.
        let mut readings: Vec<(String, String, Reading)> = Vec::new(); // (source, stem, reading)
        for name in names {
            let Some(suffixes) = &self.suffixes else {
                readings.push((name.clone(), name.clone(), Reading::Plain));
                continue;
            };
            // the suffix rule
            if let Some((stem, seg)) = name.rsplit_once('_')
                && !stem.is_empty()
                && !seg.is_empty()
            {
                let named = match suffixes {
                    Suffixes::Named(list) => list.iter().any(|s| s == seg),
                    Suffixes::Known => false,
                };
                let applies = named || matches!(suffixes, Suffixes::Known);
                if applies {
                    let reading = match TEMPORAL.iter().find(|(s, _)| *s == seg) {
                        Some((_, r)) => Some(r.clone()),
                        None if !named && STOP.contains(&seg) => {
                            skipped.push(format!("{name} ('{seg}' is a word here, not a unit)"));
                            None
                        }
                        None if !named
                            && seg == "min"
                            && names.iter().any(|n| n == &format!("{stem}_max")) =>
                        {
                            skipped
                                .push(format!("{name} ('min' beside {stem}_max is the minimum)"));
                            None
                        }
                        None => match scale_of(seg) {
                            Some(scale) => {
                                plan.scales.insert(seg.to_string(), scale);
                                Some(Reading::Unit(seg.to_string()))
                            }
                            None if named => {
                                refuse(format!(
                                    "'{seg}' is no unit this run knows (a unit statement declares \
                                     one: 'unit {seg} = …;')"
                                ));
                                return Plan::default();
                            }
                            None => {
                                skipped.push(format!("{name} ('{seg}' is no unit)"));
                                None
                            }
                        },
                    };
                    if let Some(reading) = reading {
                        let cell = sample(name);
                        if !Self::admits(&reading, cell.clone()) {
                            skipped.push(format!(
                                "{name} (its cells are no {})",
                                Self::kind_word(&reading)
                            ));
                        } else {
                            converted.push(reading.told(name, stem, cell.as_ref()));
                            let new = if reading.in_place() {
                                name.as_str()
                            } else {
                                stem
                            };
                            readings.push((name.clone(), new.to_string(), reading));
                        }
                        continue;
                    }
                    if named {
                        continue;
                    }
                }
            }
            // the header rule: `Draught, m`, `Length (km)`, `Share, %`,
            // and the words (`Throughput (metric tonnes per year)`)
            let resolve = |word: &str| self.units_override(word);
            let header = match header_unit_with(name, &resolve) {
                Ok(found) => found,
                Err(why) => {
                    skipped.push(format!("{name} ({why})"));
                    None
                }
            };
            if let Some((stem, reading)) = header {
                let applies = match suffixes {
                    Suffixes::Known => true,
                    Suffixes::Named(list) => match &reading {
                        Reading::Unit(u) | Reading::Scaled(u, _) => list.iter().any(|s| s == u),
                        _ => false,
                    },
                };
                if applies {
                    if let Some(u) = reading.unit()
                        && let Some(scale) = scale_of(u)
                    {
                        plan.scales.insert(u.to_string(), scale);
                    }
                    let cell = sample(name);
                    if !Self::admits(&reading, cell.clone()) {
                        skipped.push(format!(
                            "{name} (its cells are no {})",
                            Self::kind_word(&reading)
                        ));
                    } else {
                        converted.push(reading.told(name, &stem, cell.as_ref()));
                        readings.push((name.clone(), stem, reading));
                    }
                    continue;
                }
            }
            readings.push((name.clone(), name.clone(), Reading::Plain));
        }
        // Pass two, the slugs: every resulting name slugged.
        if let Some(sep) = &self.slugs {
            for (source, new, _) in readings.iter_mut() {
                if let Some(slug) = slug_text(new, sep)
                    && slug != *new
                {
                    slugged.push(format!("{source} → {slug}"));
                    *new = slug;
                }
            }
        }
        // The names must be free: a renamed column may not take a
        // name the rows carry, nor may two columns land on one.
        for (source, new, reading) in readings {
            if new == source && matches!(reading, Reading::Plain) {
                continue;
            }
            if new != source && names.iter().any(|n| *n == new) {
                refuse(format!(
                    "'{source}' would become '{new}', a property the rows already carry"
                ));
                return Plan::default();
            }
            if let Some((other, _)) = plan.by_new.get(&new) {
                refuse(format!(
                    "'{other}' and '{source}' would both become '{new}'"
                ));
                return Plan::default();
            }
            plan.by_new.insert(new.clone(), (source.clone(), reading));
            plan.by_source.insert(source, new);
        }
        let mut msg = format!("{label} on {}: ", self.target);
        let mut parts: Vec<String> = Vec::new();
        if self.suffixes.is_some() {
            parts.push(if converted.is_empty() {
                match &self.suffixes {
                    Some(Suffixes::Named(_)) => "no property ends in a named suffix".to_string(),
                    _ => "no property names a unit".to_string(),
                }
            } else {
                converted.join(", ")
            });
        }
        if self.slugs.is_some() {
            // alone the label says it; beside the units the part is named
            let head = if self.suffixes.is_some() {
                "slugs: "
            } else {
                ""
            };
            parts.push(if slugged.is_empty() {
                format!("{head}every name is a slug already")
            } else {
                format!("{head}{}", slugged.join(", "))
            });
        }
        msg.push_str(&parts.join("; "));
        if !skipped.is_empty() {
            msg.push_str(&format!("; skipped: {}", skipped.join(", ")));
        }
        crate::value::ambient::record_diagnostic(Diagnostic::info(
            "AUTOUNITS",
            self.target.clone(),
            msg,
        ));
        plan
    }

    /// The word of a reading's cells, for "its cells are no …".
    fn kind_word(reading: &Reading) -> &'static str {
        match reading {
            Reading::Unit(_) | Reading::Scaled(..) => "numbers",
            Reading::Instant => "instants",
            Reading::Date => "dates",
            Reading::Plain => "names",
        }
    }

    /// The plan a node's properties read through: by its row shape
    /// when it has one (remembered under its parent, so a table's
    /// rows find it without allocating the header), else by its own
    /// property names (a document's nodes each bring their own).
    fn plan_of(&self, node: NodeId) -> Rc<Plan> {
        let parent = self.inner.parent(node);
        if let Some(p) = parent
            && let Some(plan) = self.by_parent.borrow().get(&p)
        {
            return plan.clone();
        }
        let shaped = self.inner.row_columns(node);
        let names = shaped
            .clone()
            .unwrap_or_else(|| self.inner.property_names(node));
        let plan = self.plan_for(&names, &|n| self.inner.property(node, n));
        if let (Some(p), Some(_)) = (parent, shaped) {
            self.by_parent.borrow_mut().insert(p, plan.clone());
        }
        plan
    }

    /// The plan a member node reads through — a document's record
    /// members are its properties and its children at once (a JSON
    /// object's keys, a row's columns in a database), so the child
    /// view renames and converts as the property view does. `None`
    /// for a node its parent's plan leaves alone.
    fn member_reading(&self, node: NodeId) -> Option<(Rc<Plan>, String, String)> {
        let base = self.inner.name(node)?;
        let parent = self.inner.parent(node)?;
        let plan = self.plan_of(parent);
        if plan.is_empty() {
            return None;
        }
        let new = plan.by_source.get(&base)?.clone();
        Some((plan, base, new))
    }

    /// The plan for a header seen through one of its rows.
    fn plan_for_row(&self, cols: &[String], row: NodeId) -> Rc<Plan> {
        self.plan_for(cols, &|n| self.inner.property(row, n))
    }

    /// The locator of `node` with a renamed member spelled by its new
    /// name: the source's own rendering, its last segment replaced.
    pub fn locate(&self, node: NodeId, inner: &dyn Fn(NodeId) -> String) -> String {
        let loc = inner(node);
        match self.member_reading(node) {
            Some((_, base, new)) if base != new => match loc.strip_suffix(base.as_str()) {
                Some(head) if head.ends_with('/') => format!("{head}{new}"),
                _ => loc,
            },
            _ => loc,
        }
    }

    /// The header of the rows beneath `node`: one row's columns,
    /// reached through the source's own selection when it has one,
    /// so a table is never listed to learn its shape.
    fn header_below(&self, node: NodeId, like: &StepPlan<'_>) -> Option<(NodeId, Vec<String>)> {
        if let Some(h) = self.headers.borrow().get(&node) {
            return h.clone();
        }
        // any child, so a renamed step name never empties the probe
        let probe = StepPlan {
            axis: like.axis,
            name: None,
            conjuncts: Vec::new(),
            traits: Vec::new(),
            limit: Some(1),
            cap: like.cap,
            order: None,
        };
        let first = match self.inner.select(node, &probe) {
            Some(sel) => sel.nodes.first().map(|(n, _)| *n),
            None => self.inner.children(node).into_iter().next(),
        };
        let header = first.map(|n| {
            (
                n,
                self.inner
                    .row_columns(n)
                    .unwrap_or_else(|| self.inner.property_names(n)),
            )
        });
        self.headers.borrow_mut().insert(node, header.clone());
        header
    }

    /// Whether a step plan from `node` touches a conversion: its
    /// step name is a member of `node` renamed (the source knows the
    /// name as written only), or a predicate or an order names a
    /// converted column of the rows beneath. Then the engine reads
    /// the nodes itself.
    fn plan_touches_below(&self, node: NodeId, plan: &StepPlan<'_>) -> bool {
        if let Some(name) = plan.name {
            let members = self.plan_of(node);
            if members.by_new.contains_key(name) || members.by_source.contains_key(name) {
                return true;
            }
        }
        if plan.conjuncts.is_empty() && plan.order.is_none() {
            return false;
        }
        match self.header_below(node, plan) {
            Some((row, h)) => self.plan_for_row(&h, row).touches(plan),
            None => false,
        }
    }

    /// The node a scope path names, each segment a child by name.
    fn node_of_scope(&self, scope: &[&str]) -> Option<NodeId> {
        let mut cur = self.inner.root();
        for seg in scope {
            let kids = self.inner.children_named(cur, seg);
            cur = *kids.first()?;
        }
        Some(cur)
    }

    /// Whether any plan decided so far touches the step plan — what
    /// a scope-level hook can know without a node.
    fn any_plan_touches(&self, plan: &StepPlan<'_>) -> bool {
        if let Some(name) = plan.name
            && self
                .plans
                .borrow()
                .values()
                .any(|p| p.by_new.contains_key(name) || p.by_source.contains_key(name))
        {
            return true;
        }
        if plan.conjuncts.is_empty() && plan.order.is_none() {
            return false;
        }
        self.plans.borrow().values().any(|p| p.touches(plan))
    }
}

impl<A: AstAdapter> AstAdapter for AutoUnits<A> {
    crate::forward_ast_adapter!(inner; except property, properties, property_names,
        row_columns, row_text, prefetch_properties, select, select_plan, estimate, count,
        index_lookup, property_withheld, reference_properties, resolve,
        name, children_named, answers_to, default_value, descendants_named, descendants_where);

    // The child view of a record's members: renamed and converted
    // as the property view is, so a serialization by children (`%()`,
    // `@| #` over a database's rows or a document's records) and a
    // path step by name (`/legs/*/distance`) agree with `::distance`.
    fn name(&self, node: NodeId) -> Option<String> {
        match self.member_reading(node) {
            Some((_, _, new)) => Some(new),
            None => self.inner.name(node),
        }
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        let plan = self.plan_of(node);
        if plan.is_empty() {
            return self.inner.children_named(node, name);
        }
        if let Some((source, _)) = plan.by_new.get(name) {
            return self.inner.children_named(node, source);
        }
        if plan.by_source.contains_key(name) {
            return Vec::new();
        }
        self.inner.children_named(node, name)
    }

    fn answers_to(&self, node: NodeId, name: &str) -> bool {
        // a renamed member answers to its new name alone
        match self.member_reading(node) {
            Some((_, _, new)) => name == new,
            None => self.inner.answers_to(node, name),
        }
    }

    fn default_value(&self, node: NodeId) -> Option<Value> {
        match self.member_reading(node) {
            Some((plan, _, new)) => {
                let reading = &plan.by_new[&new].1;
                self.inner
                    .default_value(node)
                    .map(|v| plan.convert(v, reading))
            }
            None => self.inner.default_value(node),
        }
    }

    // A descendant search by name is the engine's walk here: a
    // source's own index knows the names as written, not as renamed.
    fn descendants_named(&self, _node: NodeId, _name: &str) -> Option<Vec<(NodeId, usize)>> {
        None
    }

    fn descendants_where(
        &self,
        _node: NodeId,
        _name: &str,
        _property: &str,
        _value: &Value,
    ) -> Option<Vec<(NodeId, usize)>> {
        None
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let plan = self.plan_of(node);
        if plan.is_empty() {
            return self.inner.property(node, name);
        }
        if let Some((source, reading)) = plan.by_new.get(name) {
            return self
                .inner
                .property(node, source)
                .map(|v| plan.convert(v, reading));
        }
        if plan.by_source.contains_key(name) {
            return None;
        }
        self.inner.property(node, name)
    }

    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        let plan = self.plan_of(node);
        let props = self.inner.properties(node);
        if plan.is_empty() {
            return props;
        }
        props
            .into_iter()
            .map(|(k, v)| match plan.by_source.get(&k) {
                Some(new) => {
                    let reading = &plan.by_new[new].1;
                    (new.clone(), plan.convert(v, reading))
                }
                None => (k, v),
            })
            .collect()
    }

    fn property_names(&self, node: NodeId) -> Vec<String> {
        let names = self.inner.property_names(node);
        let plan = self.plan_for_row(
            &self
                .inner
                .row_columns(node)
                .unwrap_or_else(|| names.clone()),
            node,
        );
        if plan.is_empty() {
            return names;
        }
        names
            .into_iter()
            .map(|n| plan.by_source.get(&n).cloned().unwrap_or(n))
            .collect()
    }

    fn row_columns(&self, node: NodeId) -> Option<Vec<String>> {
        let cols = self.inner.row_columns(node)?;
        let plan = self.plan_for_row(&cols, node);
        if plan.is_empty() {
            return Some(cols);
        }
        Some(
            cols.into_iter()
                .map(|c| plan.by_source.get(&c).cloned().unwrap_or(c))
                .collect(),
        )
    }

    fn row_text(&self, node: NodeId, f: &mut dyn FnMut(&[&str])) -> bool {
        let Some(cols) = self.inner.row_columns(node) else {
            return self.inner.row_text(node, f);
        };
        let plan = self.plan_for_row(&cols, node);
        if plan.is_empty() {
            return self.inner.row_text(node, f);
        }
        // the converted columns' readings by position: a numeric cell
        // there is respelled as the value it reads as (`12` in a
        // column of grams → `12_g`, an epoch number → its ISO text),
        // so the one-pass typed build types the column as the
        // property answers
        let readings: Vec<Option<&Reading>> = cols
            .iter()
            .map(|c| plan.by_source.get(c).map(|new| &plan.by_new[new].1))
            .collect();
        self.inner.row_text(node, &mut |cells| {
            let respelled: Vec<Option<String>> = cells
                .iter()
                .enumerate()
                .map(|(j, c)| match readings.get(j).copied().flatten() {
                    Some(Reading::Unit(u)) if c.trim().parse::<f64>().is_ok() => {
                        Some(format!("{}_{u}", c.trim()))
                    }
                    Some(Reading::Scaled(u, f)) if c.trim().parse::<f64>().is_ok() => {
                        let x: f64 = c.trim().parse().unwrap_or(0.0);
                        Some(format!("{}_{u}", format_scaled(x * f)))
                    }
                    Some(r @ (Reading::Instant | Reading::Date))
                        if c.trim().parse::<f64>().is_ok() =>
                    {
                        Some(plan.convert(Value::Str(c.to_string()), r).to_string())
                    }
                    _ => None,
                })
                .collect();
            let refs: Vec<&str> = cells
                .iter()
                .enumerate()
                .map(|(j, c)| respelled[j].as_deref().unwrap_or(c))
                .collect();
            f(&refs)
        })
    }

    fn prefetch_properties(&self, nodes: &[NodeId], names: &[&str]) {
        let Some(first) = nodes.first() else {
            return;
        };
        let plan = self.plan_of(*first);
        if plan.is_empty() {
            return self.inner.prefetch_properties(nodes, names);
        }
        let mapped: Vec<&str> = names
            .iter()
            .map(|n| plan.by_new.get(*n).map(|(s, _)| s.as_str()).unwrap_or(n))
            .collect();
        self.inner.prefetch_properties(nodes, &mapped)
    }

    fn select(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<Selection> {
        if self.plan_touches_below(from, plan) {
            return None;
        }
        self.inner.select(from, plan)
    }

    fn select_plan(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Vec<bool>> {
        if self.any_plan_touches(plan)
            || self
                .node_of_scope(scope)
                .is_some_and(|n| self.plan_touches_below(n, plan))
        {
            return None;
        }
        self.inner.select_plan(scope, plan)
    }

    fn estimate(&self, scope: &[&str], plan: &StepPlan<'_>) -> Option<Estimate> {
        if self.any_plan_touches(plan)
            || self
                .node_of_scope(scope)
                .is_some_and(|n| self.plan_touches_below(n, plan))
        {
            return None;
        }
        self.inner.estimate(scope, plan)
    }

    fn count(&self, from: NodeId, plan: &StepPlan<'_>) -> Option<usize> {
        if self.plan_touches_below(from, plan) {
            return None;
        }
        self.inner.count(from, plan)
    }

    fn index_lookup(&self, scope: &str, property: &str, value: &Value) -> Option<Vec<NodeId>> {
        let converted = self
            .plans
            .borrow()
            .values()
            .any(|p| p.by_new.contains_key(property) || p.by_source.contains_key(property));
        if converted {
            return None;
        }
        self.inner.index_lookup(scope, property, value)
    }

    fn property_withheld(&self, node: NodeId, name: &str) -> bool {
        let plan = self.plan_of(node);
        match plan.by_new.get(name) {
            Some((source, _)) => self.inner.property_withheld(node, source),
            None => self.inner.property_withheld(node, name),
        }
    }

    fn reference_properties(&self, source: NodeId, target: NodeId) -> Vec<String> {
        let plan = self.plan_of(source);
        self.inner
            .reference_properties(source, target)
            .into_iter()
            .map(|n| plan.by_source.get(&n).cloned().unwrap_or(n))
            .collect()
    }

    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let plan = self.plan_of(node);
        match plan.by_new.get(property) {
            Some((source, _)) => self.inner.resolve(node, source, hint),
            None => self.inner.resolve(node, property, hint),
        }
    }
}

/// The two options off a target's query, merged: `autounits` (no
/// value) and `autounit=_km,_g` (a comma list; the option may repeat).
/// `Ok(None)` when neither stands in the pairs; the pairs that are
/// neither are returned for the reader's own parsing.
pub fn take_options<'a>(
    pairs: impl IntoIterator<Item = &'a str>,
) -> Result<(Option<Suffixes>, Option<String>, Vec<&'a str>), String> {
    let (suffixes, slugs, _, rest) = take_options_with_units(pairs)?;
    Ok((suffixes, slugs, rest))
}

/// [`take_options`] with the dictionary override too: `units=
/// pounds:_lb,tons:_t` (the header-units round), each pair a word and
/// the unit it names in the suffix form.
pub fn take_options_with_units<'a>(
    pairs: impl IntoIterator<Item = &'a str>,
) -> Result<
    (
        Option<Suffixes>,
        Option<String>,
        Vec<(String, String)>,
        Vec<&'a str>,
    ),
    String,
> {
    let mut found: Option<Suffixes> = None;
    let mut slugs: Option<String> = None;
    let mut units: Vec<(String, String)> = Vec::new();
    let mut rest = Vec::new();
    for pair in pairs {
        if let Some(("units", list)) = pair.split_once('=') {
            for item in list.split(',').filter(|s| !s.trim().is_empty()) {
                let Some((word, unit)) = item.split_once(':') else {
                    return Err(format!(
                        "units= takes word:unit pairs ('units=pounds:_lb,tons:_t'); '{item}' given"
                    ));
                };
                let unit = named_suffix(unit.trim())?;
                units.push((word.trim().to_lowercase(), unit));
            }
            continue;
        }
        // `slugs` / `slugs=-`: the titles slugged, with the separator
        match pair.split_once('=') {
            None if pair == "slugs" => {
                slugs = Some("_".to_string());
                continue;
            }
            Some(("slugs", sep)) => {
                if sep.is_empty() {
                    return Err(
                        "slugs= names the separator ('slugs=-'); bare 'slugs' uses '_'".into(),
                    );
                }
                slugs = Some(sep.to_string());
                continue;
            }
            _ => {}
        }
        let this = match pair.split_once('=') {
            Some(("autounits", v)) => {
                return Err(format!(
                    "autounits={v}: autounits takes no value (it converts every known suffix); \
                     the named form is 'autounit=_km,_g'"
                ));
            }
            Some(("autounit", list)) => {
                let names = list
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(named_suffix)
                    .collect::<Result<Vec<_>, _>>()?;
                if names.is_empty() {
                    return Err(named_suffix("").unwrap_err());
                }
                Suffixes::Named(names)
            }
            None if pair == "autounits" => Suffixes::Known,
            None if pair == "autounit" => {
                return Err(
                    "autounit names its suffixes: 'autounit=_km,_g'; every known suffix is \
                     'autounits'"
                        .to_string(),
                );
            }
            _ => {
                rest.push(pair);
                continue;
            }
        };
        found = Some(match found {
            Some(prev) => prev.merge(this),
            None => this,
        });
    }
    Ok((found, slugs, units, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe;
    impl AstAdapter for Probe {
        fn root(&self) -> NodeId {
            NodeId(1)
        }
        fn children(&self, _node: NodeId) -> Vec<NodeId> {
            Vec::new()
        }
        fn name(&self, _node: NodeId) -> Option<String> {
            None
        }
    }

    #[test]
    fn named_suffixes_are_written_with_their_underscore() {
        assert_eq!(named_suffix("_km").unwrap(), "km");
        assert_eq!(named_suffix("_кг").unwrap(), "кг");
        assert!(named_suffix("km").unwrap_err().contains("'_km'"));
        assert!(named_suffix("_").unwrap_err().contains("shaped as no unit"));
        assert!(
            named_suffix("_k/m")
                .unwrap_err()
                .contains("shaped as no unit")
        );
    }

    #[test]
    fn the_options_merge_and_refuse() {
        let (s, slugs, rest) =
            take_options(["autounit=_km,_g", "root=/x", "autounit=_min"]).unwrap();
        assert_eq!(
            s,
            Some(Suffixes::Named(vec!["km".into(), "g".into(), "min".into()]))
        );
        assert_eq!(slugs, None);
        assert_eq!(rest, vec!["root=/x"]);
        let (s, _, _) = take_options(["autounit=_km", "autounits"]).unwrap();
        assert_eq!(s, Some(Suffixes::Known));
        assert_eq!(take_options(["encoding=utf-8"]).unwrap().0, None);
        let (s, slugs, _) = take_options(["slugs", "autounits"]).unwrap();
        assert_eq!((s, slugs), (Some(Suffixes::Known), Some("_".into())));
        assert_eq!(take_options(["slugs=-"]).unwrap().1, Some("-".into()));
        assert!(take_options(["slugs="]).unwrap_err().contains("separator"));
        assert!(
            take_options(["autounits=yes"])
                .unwrap_err()
                .contains("takes no value")
        );
        assert!(
            take_options(["autounit"])
                .unwrap_err()
                .contains("names its suffixes")
        );
        assert!(take_options(["autounit=km"]).unwrap_err().contains("'_km'"));
    }

    #[test]
    fn stop_list_covers_the_table_words() {
        // every English function word the table reads as a unit
        // stands on the stop list; the test names any newcomer
        let candidates = [
            "a", "an", "at", "in", "on", "by", "to", "of", "as", "is", "it", "or", "no", "so",
            "do", "if", "am", "pm", "up", "for", "per", "via", "from", "with", "the", "and", "but",
            "not", "nor", "yet", "off", "out", "over", "into", "onto", "upon", "than", "then",
            "when", "was", "be", "has", "had", "can", "may", "must", "will", "id", "nb", "nr",
            "num", "no", "ref", "key", "idx", "raw", "old", "new", "min", "max", "avg", "sum",
            "cnt", "pct", "abs", "rel", "end", "top", "low", "mid", "all", "any",
        ];
        let units: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|w| scale_of(w).is_some())
            .collect();
        let uncovered: Vec<&str> = units
            .iter()
            .copied()
            .filter(|w| !STOP.contains(w) && !["min", "max"].contains(w))
            .collect();
        assert!(
            uncovered.is_empty(),
            "units among the words, not on the stop list: {uncovered:?} (all units: {units:?})"
        );
    }

    #[test]
    fn temporal_suffixes_read_instants_and_dates() {
        let plan = Plan::default();
        let v = plan.convert(Value::Str("2024-03-05T10:20:30Z".into()), &Reading::Instant);
        assert_eq!(v.to_string(), "2024-03-05T10:20:30Z");
        let d = plan.convert(Value::Str("2024-03-05T10:20:30Z".into()), &Reading::Date);
        assert_eq!(d.to_string(), "2024-03-05");
        // epoch seconds, then milliseconds from 10^11 up
        let e = plan.convert(Value::Int(1_700_000_000), &Reading::Instant);
        assert_eq!(e.to_string(), "2023-11-14T22:13:20");
        let m = plan.convert(Value::Int(1_700_000_000_000), &Reading::Instant);
        assert_eq!(m.to_string(), "2023-11-14T22:13:20");
        assert_eq!(
            Reading::Instant.told("created_at", "created", Some(&Value::Int(1_700_000_000))),
            "created_at: instants (epoch seconds)"
        );
        assert_eq!(
            Reading::Date.told(
                "published_on",
                "published",
                Some(&Value::Str("2024-01-05".into()))
            ),
            "published_on: dates"
        );
        assert_eq!(
            Reading::Unit("km".into()).told("length_km", "length", None),
            "length_km → length in km"
        );
        let t = plan.convert(Value::Str("n/a".into()), &Reading::Instant);
        assert_eq!(t, Value::Str("n/a".into()));
        assert!(AutoUnits::<Probe>::admits(
            &Reading::Instant,
            Some(Value::Str("2024-01-05".into()))
        ));
        assert!(!AutoUnits::<Probe>::admits(
            &Reading::Instant,
            Some(Value::Str("yesterday".into()))
        ));
        assert!(!AutoUnits::<Probe>::admits(
            &Reading::Unit("t".into()),
            Some(Value::Str("2024-01-05".into()))
        ));
    }

    #[test]
    fn a_plan_converts_numbers_and_same_dimension_quantities() {
        let mut plan = Plan::default();
        plan.scales.insert("g".into(), scale_of("g").unwrap());
        let g = Reading::Unit("g".into());
        let q = plan.convert(Value::Str("3750".into()), &g);
        match q {
            Value::Quantity { written, .. } => assert_eq!(written, Some((3750.0, "g".into()))),
            other => panic!("{other:?}"),
        }
        assert_eq!(plan.convert(Value::Null, &g), Value::Null);
        assert_eq!(
            plan.convert(Value::Str("n/a".into()), &g),
            Value::Str("n/a".into())
        );
        match plan.convert(Value::Str("2 kg".into()), &g) {
            Value::Quantity { written, .. } => assert_eq!(written, Some((2000.0, "g".into()))),
            other => panic!("{other:?}"),
        }
        // a quantity of another dimension stays as it is
        let min = Value::Str("12 min".into());
        assert_eq!(plan.convert(min.clone(), &g), min);
    }

    #[test]
    fn a_title_names_its_unit_at_the_end() {
        let unit = |t: &str| match header_unit(t) {
            Some((stem, Reading::Unit(u))) => Some((stem, u)),
            _ => None,
        };
        assert_eq!(unit("Draught, m"), Some(("Draught".into(), "m".into())));
        assert_eq!(unit("Length (km)"), Some(("Length".into(), "km".into())));
        assert_eq!(unit("Mass [kg]"), Some(("Mass".into(), "kg".into())));
        assert_eq!(unit("Price, EUR"), Some(("Price".into(), "EUR".into())));
        assert_eq!(unit("Speed km/h"), Some(("Speed".into(), "km/h".into())));
        assert_eq!(unit("Масса, кг"), Some(("Масса".into(), "кг".into())));
        assert_eq!(unit("Checked in"), None);
        assert_eq!(unit("Created at"), None);
        assert_eq!(unit("Country"), None);
        assert_eq!(unit("Port name"), None);
        assert!(
            matches!(header_unit("Share, %"), Some((s, Reading::Unit(u))) if s == "Share" && u == "%")
        );
        assert!(
            matches!(header_unit("Growth (%)"), Some((s, Reading::Unit(u))) if s == "Growth" && u == "%")
        );
    }

    #[test]
    fn a_title_slugs() {
        assert_eq!(slug_text("Draught, m", "_"), Some("draught_m".into()));
        assert_eq!(slug_text("Quay (m)", "-"), Some("quay-m".into()));
        assert_eq!(
            slug_text("Порт назначения", "_"),
            Some("порт_назначения".into())
        );
        assert_eq!(slug_text("  2023 ", "_"), Some("2023".into()));
        assert_eq!(slug_text("—", "_"), None);
    }

    #[test]
    fn the_unit_table_decides_a_suffix() {
        assert!(scale_of("g").is_some());
        assert!(scale_of("min").is_some());
        assert!(scale_of("кг").is_some());
        // the attotonne: a prefix on the tonne, so a unit by the
        // table; the cell probe is what keeps `created_at` a text
        // column
        assert!(scale_of("at").is_some());
        assert!(scale_of("mb").is_none());
        // the typographic point: a unit too
        assert!(scale_of("pt").is_some());
        assert!(scale_of("name").is_none());
        assert!(scale_of("id").is_none());
        let t = Reading::Unit("t".into());
        assert!(!AutoUnits::<Probe>::admits(
            &t,
            Some(Value::Str("2024-01-01".into()))
        ));
        assert!(AutoUnits::<Probe>::admits(
            &t,
            Some(Value::Str(" 12.5 ".into()))
        ));
        assert!(AutoUnits::<Probe>::admits(&t, None));
    }
}
