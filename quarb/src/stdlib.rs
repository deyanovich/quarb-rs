//! The pipeline standard library.
//!
//! Functions come in two families, matching the two control operators:
//!
//! - **Scalar** commands (`|`) transform one capsa's topic; some
//!   expand it into several values (e.g. `lines`).
//! - **Aggregate** commands (`@|`) reduce the whole context's topics
//!   to a new list.

use crate::ast::{IndexSpec, Arg, FnCall};
use crate::value::Value;
use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// The unit-expression resolver threaded from the executor (the
/// adapter's `unit_scale`), so aggregates read custom units exactly
/// as criteria and ordering do.
type Scale<'a> = &'a dyn Fn(&str) -> Option<(f64, String)>;

/// The internal aggregate standing in for a closing `@| csv` /
/// `@| table` while a tool renders it (see `run_for_rendering`).
pub(crate) const ROWS_FOR_RENDERING: &str = "\u{1}rows";

/// Records as columns: the named columns in their order, or the
/// union of the records' fields in first-seen order (a scalar rides
/// a `value` column). A field a record lacks is an empty cell.
/// Aligned text with a rule under the header (widths in grapheme
/// clusters), or RFC 4180 CSV; lines joined, no trailing newline.
pub fn tabulate(values: &[Value], named: Option<&[String]>, csv: bool) -> String {
    tabulate_with(values, named, csv, b',')
}

/// [`tabulate`] with the CSV delimiter chosen (`@| csv(";")`); the
/// table form ignores it.
pub fn tabulate_with(
    values: &[Value],
    named: Option<&[String]>,
    csv: bool,
    delimiter: u8,
) -> String {
    tabulate_in(values, named, csv, delimiter, &|v| match v {
        Value::Null => String::new(),
        Value::Str(s) => s.clone(),
        other => other.display_form(),
    })
}

/// [`tabulate_with`] with the cell renderer chosen — the output
/// locale's, when a tool prints under one.
pub fn tabulate_in(
    values: &[Value],
    named: Option<&[String]>,
    csv: bool,
    delimiter: u8,
    cell: &dyn Fn(&Value) -> String,
) -> String {
    let columns = crate::table::table_columns(values, named);
    let rows: Vec<Vec<String>> = values
        .iter()
        .map(|v| match v {
            Value::Record(fields) => columns
                .iter()
                .map(|c| {
                    fields
                        .iter()
                        .find(|(k, _)| k == c)
                        .map(|(_, v)| cell(v))
                        .unwrap_or_default()
                })
                .collect(),
            other => columns
                .iter()
                .map(|c| {
                    if c == "value" {
                        cell(other)
                    } else {
                        String::new()
                    }
                })
                .collect(),
        })
        .collect();
    let mut lines: Vec<String> = Vec::with_capacity(rows.len() + 2);
    if csv {
        let sep = (delimiter as char).to_string();
        let quote = |s: &str| -> String {
            if s.contains([delimiter as char, '"', '\n', '\r']) {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        let row = |cells: &[String]| {
            cells
                .iter()
                .map(|c| quote(c))
                .collect::<Vec<_>>()
                .join(&sep)
        };
        lines.push(row(&columns));
        lines.extend(rows.iter().map(|r| row(r)));
        return lines.join("\n");
    }
    let width = |s: &str| s.graphemes(true).count();
    let mut widths: Vec<usize> = columns.iter().map(|c| width(c)).collect();
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(width(c));
        }
    }
    let line = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}{}", " ".repeat(widths[i].saturating_sub(width(c)))))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    lines.push(line(&columns));
    lines.push(
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    lines.extend(rows.iter().map(|r| line(r)));
    lines.join("\n")
}

/// The outline of a stream of levels: for each row its depth below
/// the shallowest level seen and its dotted number (`1`, `1.1`,
/// `1.2`, `2`). One counter per depth; a row increments its own and
/// forgets the deeper ones. A level that is skipped (1, then 3)
/// counts as present once (`1.1.1`); a row without a level stands
/// at the shallowest.
/// The longest text a repetition may build (`repeat`, `indent`):
/// 64 MiB. Past it the query is refused — a count taken from data
/// (`repeat(::n)`) must not be able to exhaust memory.
pub const MAX_REPEAT_BYTES: usize = 64 << 20;

/// `text` repeated `n` times within [`MAX_REPEAT_BYTES`], else a
/// refusal naming the command and the size it would have built.
fn bounded_repeat(text: &str, n: usize, what: &str) -> Value {
    match text.len().checked_mul(n) {
        Some(bytes) if bytes <= MAX_REPEAT_BYTES => Value::Str(text.repeat(n)),
        _ => {
            crate::exec::record_refusal(format!(
                "{what}({n}): the result would exceed {} bytes",
                MAX_REPEAT_BYTES
            ));
            Value::Null
        }
    }
}

/// How far below the shallowest level an outline may reach: the
/// executor refuses `number` / `outline` past it (levels are data).
pub const MAX_OUTLINE_DEPTH: i64 = 64;

pub fn outline_numbers(levels: &[Option<i64>]) -> Vec<(usize, String)> {
    let base = levels.iter().flatten().copied().min().unwrap_or(0);
    let mut counters: Vec<u64> = Vec::new();
    levels
        .iter()
        .map(|l| {
            let depth = (l.unwrap_or(base) - base).clamp(0, MAX_OUTLINE_DEPTH) as usize;
            counters.truncate(depth + 1);
            if counters.len() == depth + 1 {
                counters[depth] += 1;
            } else {
                counters.resize(depth + 1, 1);
            }
            let number = counters
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(".");
            (depth, number)
        })
        .collect()
}

/// Whether `name` is an association aggregate — a reduction of two
/// per-capsa expressions to one number (`corr(:a; :b)`). Evaluated
/// in the executor like the keyed aggregates; this module holds the
/// arithmetic ([`associate`]).
pub fn association(name: &str) -> bool {
    crate::registry::spec(name)
        .is_some_and(|s| matches!(s.agg, Some(crate::registry::Agg::Association { .. })))
}

/// Whether `name` is an agreement measure — an association that
/// reads its two expressions as labels, not numbers (`kappa(:g; :p)`
/// over categories, `precision`/`recall`/`f1` over truthiness).
pub fn agreement(name: &str) -> bool {
    crate::registry::spec(name).is_some_and(|s| {
        matches!(
            s.agg,
            Some(crate::registry::Agg::Association { agreement: true })
        )
    })
}

/// Whether the call carries the bare word `sample` (a string
/// literal, as the parser reads a bare name): the dispersion
/// measures divide by n − 1 instead of n.
pub fn sample_flag(call: &FnCall) -> bool {
    call.args
        .iter()
        .any(|a| matches!(a, Arg::Lit(Value::Str(s)) if s == "sample"))
}

/// Whether the call carries the bare word `atergo`: the a tergo
/// (inverse-lexicographic) order, in which texts compare from their
/// last character toward their first — the reverse dictionary's
/// order.
pub fn atergo_flag(call: &FnCall) -> bool {
    call.args
        .iter()
        .any(|a| matches!(a, Arg::Lit(Value::Str(s)) if s == "atergo"))
}

/// The text ordering a `sort` call asks for, where it asks for one:
/// a locale's collation, the a tergo order, or both — the mirror is
/// then taken over the collation's elements, so a contracting
/// digraph stays one unit. `None` when the call names neither: the
/// standard value comparison applies, and that is the caller's,
/// being unit- and number-aware.
pub(crate) fn text_order(call: &FnCall) -> Option<Box<dyn Fn(&str, &str) -> Ordering>> {
    match (collator_for(call), atergo_flag(call)) {
        (Some(c), true) => Some(Box::new(move |a, b| c.compare_reverse(a, b))),
        (Some(c), false) => Some(Box::new(move |a, b| c.compare(a, b))),
        // Code points from the end; a tie (one text a suffix of the
        // other, or equal) falls to the forward order.
        (None, true) => Some(Box::new(|a, b| {
            a.chars().rev().cmp(b.chars().rev()).then_with(|| a.cmp(b))
        })),
        (None, false) => None,
    }
}

/// The text order `sort` falls to when the query names no locale:
/// the output locale's collation, when the tool set one.
pub(crate) fn default_text_order() -> Option<Box<dyn Fn(&str, &str) -> Ordering>> {
    default_collator()
        .map(|c| -> Box<dyn Fn(&str, &str) -> Ordering> { Box::new(move |a, b| c.compare(a, b)) })
}

/// `sort` without a locale argument: the standard value comparison,
/// with text that has no reading collated under `collate` (the
/// output locale's order) — numbers, dates, quantities and text
/// that reads as one keep their magnitude order whatever the
/// locale.
pub(crate) fn default_cmp(
    a: &Value,
    b: &Value,
    scale: Scale,
    collate: Option<&dyn Fn(&str, &str) -> Ordering>,
) -> Ordering {
    if let (Some(c), Value::Str(x), Value::Str(y)) = (collate, a, b)
        && matches!(a.sort_key_with(scale), crate::value::SortKey::Text(_))
        && matches!(b.sort_key_with(scale), crate::value::SortKey::Text(_))
    {
        return c(x, y);
    }
    a.compare_with(b, scale)
}

/// The collator for the run's collation, when the tool set one
/// (`Run::collation`, its output locale).
#[cfg(feature = "colligo")]
fn default_collator() -> Option<colligo::Collator> {
    let tag = crate::exec::run_option(|o| o.collation.clone())?;
    Some(
        colligo::Collator::builder(&tag)
            .allow_approximate(true)
            .build()
            .unwrap_or_else(|_| colligo::Collator::root()),
    )
}

#[cfg(not(feature = "colligo"))]
fn default_collator() -> Option<NeverCollator> {
    None
}

/// The collator for a `sort(LOCALE)` call, where one was given
/// (the parser validated the tag's shape; root on surprise — a
/// tag colligo's registry doesn't know or support still sorts,
/// under the untailored base table). Approximate-tier locales
/// are accepted: a query sort wants a reasonable total order,
/// not certified fidelity.
#[cfg(feature = "colligo")]
pub(crate) fn collator_for(call: &FnCall) -> Option<colligo::Collator> {
    // The locale is the literal argument that is not the mode word.
    let tag = call.args.iter().find_map(|a| match a {
        Arg::Lit(Value::Str(s)) if s == "atergo" => None,
        Arg::Lit(l) => Some(l.to_string()),
        _ => None,
    })?;
    match colligo::Collator::builder(&tag)
        .allow_approximate(true)
        .build()
    {
        Ok(c) => Some(c),
        Err(_) => {
            crate::exec::record_refusal(format!("{}: no collation for locale {tag:?}", call.name));
            None
        }
    }
}

/// Collation compiled out (`colligo` feature off): never a
/// collator, so both `sort(LOCALE)` call sites fall back to the
/// standard value comparison (codepoint order for text). The
/// uninhabited stub keeps the call sites feature-agnostic.
#[cfg(not(feature = "colligo"))]
pub(crate) struct NeverCollator(std::convert::Infallible);

#[cfg(not(feature = "colligo"))]
impl NeverCollator {
    pub(crate) fn compare(&self, _a: &str, _b: &str) -> Ordering {
        match self.0 {}
    }
    pub(crate) fn compare_reverse(&self, _a: &str, _b: &str) -> Ordering {
        match self.0 {}
    }
}

#[cfg(not(feature = "colligo"))]
pub(crate) fn collator_for(_call: &FnCall) -> Option<NeverCollator> {
    None
}

/// Whether `name` is a keyed aggregate (for `@|`).
pub fn known_keyed(name: &str) -> bool {
    crate::registry::spec(name).is_some_and(|s| s.agg == Some(crate::registry::Agg::Keyed))
}

/// Whether `name` is a per-capsa scalar command (for `|`).
pub fn known_scalar(name: &str) -> bool {
    crate::registry::spec(name).is_some_and(|s| s.scalar)
}

/// Whether `name` is an aggregate command (for `@|`).
pub fn known_agg(name: &str) -> bool {
    crate::registry::spec(name).is_some_and(|s| s.agg.is_some())
}

/// The scalar registry, for enumerating consumers (completion,
/// highlighting): every per-capsa `|` command.
pub fn scalar_names() -> Vec<&'static str> {
    crate::registry::names(true, None).collect()
}

/// The aggregate registry: every `@|` reducer (keyed aggregates
/// are enumerated separately by [`keyed_names`]).
pub fn aggregate_names() -> Vec<&'static str> {
    use crate::registry::Agg;
    crate::registry::SPECS
        .iter()
        .filter(|s| {
            matches!(
                s.agg,
                Some(Agg::Reducing | Agg::Context | Agg::Association { .. })
            )
        })
        .map(|s| s.name)
        .collect()
}

/// The keyed-aggregate registry: `sort_by`, `group`, and friends.
pub fn keyed_names() -> Vec<&'static str> {
    crate::registry::names(false, Some(crate::registry::Agg::Keyed)).collect()
}

/// The locale argument of `strptime` / `strftime`: the table for
/// the second argument, English without one; a locale no table
/// covers is refused.
fn date_names(call: &FnCall) -> Option<&'static crate::datenames::Names> {
    let locale = arg_str(call, 1, "");
    match crate::datenames::names(locale) {
        Some(n) => Some(n),
        None => {
            crate::exec::record_refusal(format!(
                "{}: no month and weekday names for locale {locale:?} (shipped: {})",
                call.name,
                crate::locale::SHIPPED.join(", ")
            ));
            None
        }
    }
}

/// The value text of a time expression: a `timex` record's `value`
/// field, text in the value notation as it stands, or anything
/// else with a calendar reading as its period (a complete date).
pub(crate) fn timex_value(v: &Value) -> Option<String> {
    match v {
        Value::Record(fields) => match fields.iter().find(|(k, _)| k == "value") {
            Some((_, Value::Str(s))) => Some(s.clone()),
            Some((_, other)) => other.calendar_reading().map(|p| p.format()),
            None => None,
        },
        Value::Str(s) => Some(s.clone()),
        other => other.calendar_reading().map(|p| p.format()),
    }
}

/// A whole double as an integer where one holds it, else as it is
/// (the cast would saturate at the rim of the integers).
fn whole(n: f64) -> Value {
    if n.abs() < 9.2e18 {
        Value::Int(n as i64)
    } else {
        Value::Float(n)
    }
}

/// The delimiter `csv(";"; …)` names, if it names one, and the
/// arguments after it (the columns). The parser carries the quoted
/// first argument of `csv` as `delimiter=X`; `table` takes none.
pub(crate) fn split_csv_delimiter(call: &FnCall) -> (Option<u8>, &[Arg]) {
    if call.name == "csv"
        && let Some((
            Arg::Named {
                name,
                value: Value::Str(d),
            },
            rest,
        )) = call.args.split_first()
        && name == "delimiter"
    {
        return (d.as_bytes().first().copied(), rest);
    }
    (None, call.args.as_slice())
}

/// Apply a per-capsa scalar command to a single topic. May return
/// several values (an expanding command like `lines`).
pub fn apply_scalar(
    call: &FnCall,
    topic: Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
    // The two readings and the reshapes take the topic whole; a
    // library value is read by them and by the serializations, and
    // refused by kind anywhere else (spec: Command Libraries, "A
    // library value").
    match call.name.as_str() {
        "@" | "rows" | "%" | "columns" | "#" | "transpose" | "header" | "dim" | "[]" | "cell"
        | "row-to-header" | "header-to-row" => {
            return vec![refusing(call, match call.name.as_str() {
                "@" | "rows" => rows_reading(topic),
                "%" | "columns" => columns_reading(topic),
                "#" => table_reading(topic),
                "header" => table_meta(topic, "header"),
                "dim" => table_meta(topic, "dim"),
                "[]" => table_rows_at(call, topic),
                "cell" => table_cell(call, topic),
                "row-to-header" | "header-to-row" => header_shift(call, topic),
                _ => transpose_reading(topic),
            })];
        }
        "pivot" | "melt" => {
            let items = match topic {
                Value::List(_) => topic.into_items().unwrap_or_default(),
                other => vec![other],
            };
            return vec![Value::list(apply(call, items, scale))];
        }
        // `| pretty` pretty-prints the value; `| csv` and `| table`
        // after a table value render it (spec: output forms).
        "pretty" => {
            // `| pretty(year; title)` after a table selects and
            // orders the columns as the stream form does.
            if let Value::Table(t) = &topic {
                let (delimiter, args) = split_csv_delimiter(call);
                let columns = column_names(args);
                return vec![Value::Str(tabulate_table(
                    t,
                    false,
                    columns.as_deref(),
                    delimiter.unwrap_or(b','),
                ))];
            }
            return vec![Value::Str(pretty_value(&topic))];
        }
        "csv" | "table" => {
            let (delimiter, args) = split_csv_delimiter(call);
            let columns = column_names(args);
            return vec![refusing(call, match &topic {
                Value::Table(t) => Ok(Value::Str(tabulate_table(
                    t,
                    call.name == "csv",
                    columns.as_deref(),
                    delimiter.unwrap_or(b','),
                ))),
                other => Err(format!(
                    "on the plain pipe '{}' renders a table value, not {}; over a stream of records write '@| {}'",
                    call.name,
                    kind_of(other),
                    call.name
                )),
            })];
        }
        _ => {}
    }
    let kept = match &topic {
        Value::Foreign(f) => Some(f.refusal(&call.name)),
        Value::Table(t) => Some(table_refusal(t, &call.name)),
        _ => None,
    };
    if let Some(refusal) = kept {
        match call.name.as_str() {
            "json" | "jsonl" | "kaiv" | "csv" | "table" => {}
            _ => {
                crate::exec::record_refusal(refusal);
                return vec![Value::Null];
            }
        }
    }
    let text = topic.to_string();
    match call.name.as_str() {
        // `number(LOCALE)` — the numeric reading of text under the
        // locale's conventions (`number(ru-RU)` reads `1 234,56`,
        // `number(de-CH)` reads `1'234.56`); a number stays itself;
        // text with no reading, or a locale with no table, is null.
        "number" => {
            let locale = arg_str(call, 0, "");
            let decimal = if locale.is_empty() {
                Some(crate::number::Decimal::Point)
            } else {
                crate::number::locale_convention(locale)
            };
            if decimal.is_none() {
                crate::exec::record_refusal(format!(
                    "number: no number conventions for locale {locale:?}"
                ));
                return vec![Value::Null];
            }
            return vec![match (&topic, decimal) {
                (Value::Int(_) | Value::Float(_), _) => topic.clone(),
                (Value::Str(s), Some(d)) => crate::number::read(s, d).unwrap_or(Value::Null),
                _ => Value::Null,
            }];
        }
        // `timex(LOCALE)` — the time expressions of a text, one
        // record each in the order of the text: `%(text; at; kind;
        // value; grain)`. Found by the locale's finite lists and
        // nothing else; a locale that carries none is refused. A
        // text with no time expression yields nothing.
        "timex" => timex_scalar(call, topic.clone(), &text),
        // `time_anchor(T)` — a time expression valued against a
        // point: the topic is a `timex` record or its value text,
        // `T` anything with a calendar reading, the result a
        // period. A complete date passes through; a duration, a
        // vague expression, and an offset the anchor's grain cannot
        // carry are null.
        "time_anchor" => {
            let anchor = match call.args.first() {
                Some(Arg::Lit(v)) => v.calendar_reading(),
                _ => {
                    crate::exec::record_refusal(
                        "time_anchor takes the anchor — time_anchor(1899-10-06), \
                         time_anchor($.d)"
                            .into(),
                    );
                    return vec![Value::Null];
                }
            };
            return vec![
                timex_value(&topic)
                    .and_then(|v| crate::period::anchor(&v, anchor.as_ref(), false))
                    .map_or(Value::Null, Value::period),
            ];
        }
        // `period[(GRAIN)]` — the calendar reading as a period; with
        // a grain, the period of that grain holding the value (a
        // finer grain than the value's own is null).
        "period" => {
            let reading = topic.calendar_reading();
            return vec![match (reading, call.args.first()) {
                (None, _) => Value::Null,
                (Some(p), None) => Value::period(p),
                (Some(p), Some(Arg::Lit(Value::Str(g)))) => {
                    match crate::period::Grain::from_word(g) {
                        Some(g) => p.at(g).map_or(Value::Null, Value::period),
                        None => {
                            crate::exec::record_refusal(format!(
                                "period: {g:?} is not a grain (year, month, day, hour, minute, second)"
                            ));
                            Value::Null
                        }
                    }
                }
                _ => Value::Null,
            }];
        }
        // `grain` — a period's grain as its word.
        "grain" => {
            return vec![match &topic {
                Value::Period { grain, .. } => Value::Str(grain.word().to_string()),
                _ => Value::Null,
            }];
        }
        "upper" => vec![Value::Str(text.to_uppercase())],
        "lower" => vec![Value::Str(text.to_lowercase())],
        // Title case, Python-shaped: each word's first alphabetic
        // character uppercases, the rest of the word lowercases.
        "title" => {
            let mut out = String::with_capacity(text.len());
            let mut boundary = true;
            for c in text.chars() {
                if c.is_alphabetic() {
                    if boundary {
                        out.extend(c.to_uppercase());
                        boundary = false;
                    } else {
                        out.extend(c.to_lowercase());
                    }
                } else {
                    out.push(c);
                    boundary = true;
                }
            }
            vec![Value::Str(out)]
        }
        "trim" => vec![Value::Str(text.trim().to_string())],
        "ltrim" => vec![Value::Str(text.trim_start().to_string())],
        "rtrim" => vec![Value::Str(text.trim_end().to_string())],
        // `cc` counts codepoints (né `chars`, the pre-0.25 alias);
        // `bc` counts UTF-8 bytes; `wc` counts words.
        "cc" => vec![Value::Int(text.chars().count() as i64)],
        // `gc` counts grapheme clusters (UAX #29): what the eye
        // counts — a stressed vowel with its acute is one.
        "gc" => vec![Value::Int(text.graphemes(true).count() as i64)],
        "bc" => vec![Value::Int(text.len() as i64)],
        "wc" => vec![Value::Int(text.split_whitespace().count() as i64)],
        "lc" => vec![Value::Int(text.lines().count() as i64)],
        // UAX #29 sentence segmentation (unicode-segmentation):
        // `sentences` splits (each segment trimmed, empties
        // dropped), `sc` counts the same segments.
        "sentences" => sentences_of(&text)
            .into_iter()
            .map(|t| Value::Str(t.to_string()))
            .collect(),
        "sc" => {
            vec![Value::Int(sentences_of(&text).len() as i64)]
        }
        // `substr(a..b)` — the range predicate's spellings, on
        // characters: `substr(1..3)`, `substr(..3)`, `substr(2..)`,
        // `substr(..-3)`, `substr(4..-4)`; a bare position
        // (`substr(3)`) is the `[n]` reading, one character.
        // 1-based like its ancestors (SQL SUBSTR, XPath
        // substring, awk/Perl substr).
        // 1-based, inclusive, negative counts from the end;
        // out-of-range clamps, a crossed range is the empty
        // string, and 0 refuses — it is no position.
        "substr" => substr_scalar(call, &text),
        // `pad(n)` centers to n characters (the extra character
        // of an odd split falls right, as Python's center);
        // `lpad(n)` / `rpad(n)` pad one side. Spaces by default
        // (`lpad(n, "0")` with the given text, repeated and cut
        // to fit, SQL-shaped); text already n or longer passes
        // through untouched.
        // Widths count grapheme clusters, so a column of stressed
        // words aligns: замо́к is five cells, not six.
        "pad" => {
            let n = arg_int(call, 0).unwrap_or(0).max(0) as usize;
            let fill = arg_str(call, 1, " ");
            let have = text.graphemes(true).count();
            if have >= n || fill.is_empty() {
                vec![Value::Str(text.clone())]
            } else {
                let left = (n - have) / 2;
                let right = n - have - left;
                let l: String = fill.chars().cycle().take(left).collect();
                let r: String = fill.chars().cycle().take(right).collect();
                vec![Value::Str(format!("{l}{text}{r}"))]
            }
        }
        "lpad" | "rpad" => {
            let n = arg_int(call, 0).unwrap_or(0).max(0) as usize;
            let fill = arg_str(call, 1, " ");
            let have = text.graphemes(true).count();
            if have >= n || fill.is_empty() {
                vec![Value::Str(text.clone())]
            } else {
                let pad: String = fill.chars().cycle().take(n - have).collect();
                vec![Value::Str(if call.name == "lpad" {
                    format!("{pad}{text}")
                } else {
                    format!("{text}{pad}")
                })]
            }
        }
        // `repeat(n)` — the text n times (0 is the empty string).
        // Repetition lives here alone: `"a" * 3` stays
        // null-propagating arithmetic.
        "repeat" => {
            let n = arg_int(call, 0).unwrap_or(0).max(0) as usize;
            vec![bounded_repeat(&text, n, "repeat")]
        }
        // `indent(n)` / `indent(n; unit)` — the text behind n copies
        // of the unit (two spaces by default): the level of a tree
        // listing made visible. A null or negative n indents by
        // nothing.
        "indent" => {
            let n = arg_num(call, 0).map_or(0, |f| f as i64).max(0) as usize;
            let unit = arg_str(call, 1, "  ");
            vec![match bounded_repeat(&unit, n, "indent") {
                Value::Str(pad) => Value::Str(format!("{pad}{text}")),
                v => v,
            }]
        }
        "lines" => text.lines().map(|s| Value::Str(s.to_string())).collect(),
        // The character classes — POSIX's alpha / digit / alnum,
        // Rust's `is_alphabetic` / `is_numeric` / `is_alphanumeric`
        // (Unicode: \p{L}, \p{N}, both): keep the class, turn every
        // other character into a space, so `| words` then spreads
        // the kept runs. One optional argument names extra
        // characters to keep — `alpha("’")` for apostrophes,
        // `digit(".,-")` for decimals and signs. Classes, not
        // parsers: `digit` never reads the string it keeps.
        "alpha" | "digit" | "alnum" => {
            let keep = arg_str(call, 0, "");
            let class: fn(char) -> bool = match call.name.as_str() {
                "alpha" => char::is_alphabetic,
                "digit" => char::is_numeric,
                _ => char::is_alphanumeric,
            };
            vec![Value::Str(
                text.chars()
                    .map(|c| if class(c) || keep.contains(c) { c } else { ' ' })
                    .collect(),
            )]
        }
        "words" => text
            .split_whitespace()
            .map(|s| Value::Str(s.to_string()))
            .collect(),
        // `ngrams(n)` — the n-word windows of the text (whitespace
        // words, as `words` splits them), each joined by one space,
        // one value per window; `ngrams(2)` the bigrams. A text
        // shorter than n yields nothing. Windows stay inside the
        // value they came from, so over a paragraph spread no
        // window straddles two paragraphs.
        "ngrams" => {
            let n = arg_int(call, 0).unwrap_or(2).max(1) as usize;
            let ws: Vec<&str> = text.split_whitespace().collect();
            ws.windows(n).map(|w| Value::Str(w.join(" "))).collect()
        }
        // `haversine(lat1; lon1; lat2; lon2)` — the great-circle
        // distance between two points in kilometres (a sphere of
        // 6371 km), for a gazetteer's coordinates. A missing
        // coordinate gives null.
        "haversine" => {
            // the call form puts the first latitude in topic position
            let (lat1, from) = if call.args.len() >= 4 {
                (arg_num(call, 0), 1)
            } else {
                (topic.numeric(), 0)
            };
            let (Some(lat1), Some(lon1), Some(lat2), Some(lon2)) = (
                lat1,
                arg_num(call, from),
                arg_num(call, from + 1),
                arg_num(call, from + 2),
            ) else {
                return vec![Value::Null];
            };
            let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
            let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
            let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
            vec![Value::Float(2.0 * 6371.0 * a.sqrt().asin())]
        }
        // `tokens` — the corpus tier's tokenizer over the text: the
        // word-bound segments, whitespace dropped, words and
        // punctuation alike, as a list; `tokens(word)` keeps the
        // word class (a segment with a letter), `tokens(number)`
        // and `tokens(punct)` the others.
        "tokens" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let class = arg_str(call, 0, "");
            if !matches!(class, "" | "word" | "number" | "punct") {
                crate::exec::record_refusal(
                    "tokens takes nothing, or a class to keep: tokens(word), tokens(number), \
                     tokens(punct)"
                        .into(),
                );
                return vec![Value::Null];
            }
            use unicode_segmentation::UnicodeSegmentation;
            // One value per token, as `words` and `sentences`
            // spread, so the aggregates follow.
            text.split_word_bounds()
                .filter(|seg| !seg.chars().all(char::is_whitespace))
                .filter(|seg| class.is_empty() || token_class(seg) == class)
                .map(|seg| Value::Str(seg.to_string()))
                .collect()
        }
        // `casefold` — Unicode case folding, where `lower` is the
        // simple mapping: ß and ẞ fold to ss, a final sigma to σ,
        // the long s to s, the f-ligatures to their letters.
        "casefold" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            vec![Value::Str(casefold(&text))]
        }
        // `deaccent` — the combining marks stripped after NFD:
        // polytonic Greek to bare letters, diacritics off Latin.
        // The Cyrillic letters й and ё stand (their breve and
        // diaeresis are the letter); `deaccent(ru)` folds ё to е,
        // the orthographic habit.
        "deaccent" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let mode = arg_str(call, 0, "");
            if !matches!(mode, "" | "ru") {
                crate::exec::record_refusal(
                    "deaccent takes nothing, or ru to fold ё as well: deaccent(ru)".into(),
                );
                return vec![Value::Null];
            }
            vec![Value::Str(deaccent(&text, mode == "ru"))]
        }
        // `stem(LANG)` — the Snowball stem (the rust-stemmers
        // crate's algorithms); a language it does not ship is
        // refused naming the shipped ones.
        "stem" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let lang = arg_str(call, 0, "");
            let Some(algorithm) = stemmer_for(lang) else {
                crate::exec::record_refusal(format!(
                    "stem takes a language the stemmer ships — stem(en), stem(ru); one of {}",
                    STEMMER_LANGS.join(", ")
                ));
                return vec![Value::Null];
            };
            // Snowball stems one word: each word of the text is
            // stemmed in place, the rest of the text untouched.
            use unicode_segmentation::UnicodeSegmentation;
            let stemmer = rust_stemmers::Stemmer::create(algorithm);
            let out: String = text
                .split_word_bounds()
                .map(|seg| {
                    if seg.chars().any(char::is_alphabetic) {
                        stemmer.stem(seg).into_owned()
                    } else {
                        seg.to_string()
                    }
                })
                .collect();
            vec![Value::Str(out)]
        }
        // `jaro_winkler(other)` — the Jaro–Winkler similarity of
        // the topic's text and the argument's, 0 (nothing shared)
        // to 1 (the same), for names across lists and spellings.
        "jaro_winkler" => {
            let other = match call.args.first() {
                Some(Arg::Lit(Value::Null)) => return vec![Value::Null],
                Some(Arg::Lit(v)) => v.to_string(),
                _ => {
                    crate::exec::record_refusal(
                        "jaro_winkler takes the other text — jaro_winkler(\"word\"), \
                         jaro_winkler(::name)"
                            .into(),
                    );
                    return vec![Value::Null];
                }
            };
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            vec![Value::Float(strsim::jaro_winkler(&text, &other))]
        }
        // `levenshtein(other)` — the edit distance between the
        // topic's text and the argument's, in codepoints (insert,
        // delete, substitute, each 1). A null argument gives null.
        "levenshtein" => {
            let other = match call.args.first() {
                Some(Arg::Lit(Value::Null)) => return vec![Value::Null],
                Some(Arg::Lit(v)) => v.to_string(),
                _ => {
                    crate::exec::record_refusal(
                        "levenshtein takes the other text — levenshtein(\"word\"), \
                         levenshtein(::name)"
                            .into(),
                    );
                    return vec![Value::Null];
                }
            };
            vec![Value::Int(levenshtein(&text, &other) as i64)]
        }
        // Unicode normalization (UAX #15): `nfc` composes, `nfd`
        // decomposes. A stress mark is a combining acute (U+0301)
        // after its vowel and no Cyrillic vowel has a precomposed
        // acute form, so a stressed Russian word keeps its two code
        // points under either form.
        "nfc" | "nfd" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let out: String = if call.name == "nfc" {
                text.nfc().collect()
            } else {
                text.nfd().collect()
            };
            vec![Value::Str(out)]
        }
        // `vowels` counts the vowel letters of the composed text —
        // Cyrillic and Latin, case-folded; an optional argument
        // names the alphabet (`vowels("аеиоу")`).
        "vowels" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let alphabet = arg_str(call, 0, DEFAULT_VOWELS);
            let n = text.nfc().filter(|c| is_vowel(*c, alphabet)).count();
            vec![Value::Int(n as i64)]
        }
        // `stress(n)` places the combining acute (U+0301) after the
        // n-th vowel of the composed text — 1-based, negative from
        // the end (`substr`'s convention). A word with fewer vowels
        // has no such position and gives null; 0 refuses. A vowel
        // already marked stays as it is, and ё, which carries its
        // stress in the letter, counts as a vowel but is never
        // marked. `stress(n; "аеиоу")` names the vowel alphabet.
        "stress" => stress_scalar(call, topic.clone(), &text),
        // `stress_at` — the ordinal of the stressed vowel in a marked
        // form: the vowel before the acute, else a ё, else the one
        // vowel of a monosyllable; null when nothing decides.
        "stress_at" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let alphabet = arg_str(call, 0, DEFAULT_VOWELS);
            match stress_at(&text, alphabet) {
                Some(n) => vec![Value::Int(n as i64)],
                None => vec![Value::Null],
            }
        }
        // `unstress` — the form with its stress marks removed (the
        // acute and the grave); ё stays, being a letter.
        "unstress" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            vec![Value::Str(
                text.nfc()
                    .filter(|c| !matches!(c, '\u{301}' | '\u{300}'))
                    .collect(),
            )]
        }
        // `accent(style)` — a marked form in a display convention:
        // "acute" (the mark, as is), "upper" (the stressed vowel in
        // capitals: замОк), "apostrophe" (an apostrophe after it:
        // замо'к), "none" (unmarked). ё is left as the letter it is.
        "accent" => accent_scalar(call, topic.clone(), &text),
        // `translit(scheme)` — the text in Latin letters under an ISO
        // transliteration scheme; no argument picks each script's own
        // ISO scheme; for Russian the systems in use beside ISO 9
        // answer to their own names: gost/гост, scientific/научная,
        // bgn/бгн, passport/паспорт.
        "translit" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let scheme = arg_str(call, 0, "");
            match crate::translit::translit(&text, scheme) {
                Ok(s) => vec![Value::Str(s)],
                Err(e) => {
                    crate::exec::record_refusal(e);
                    vec![Value::Null]
                }
            }
        }
        // `modernize(table)` — a historical spelling folded to the
        // modern one, letter by letter, case kept: `ru-1918` (the
        // pre-reform Russian alphabet, the final ъ dropped) and
        // `long-s` (the long s and the typographic ligatures).
        "modernize" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let table = arg_str(call, 0, "");
            match crate::translit::modernize(&text, table) {
                Ok(s) => vec![Value::Str(s)],
                Err(e) => {
                    crate::exec::record_refusal(e);
                    vec![Value::Null]
                }
            }
        }
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
                #[cfg(feature = "decode-formats")]
                "json" => crate::encoding::json_to_value(&text).unwrap_or(Value::Null),
                #[cfg(feature = "decode-formats")]
                "yaml" => crate::encoding::yaml_to_value(&text).unwrap_or(Value::Null),
                #[cfg(feature = "decode-formats")]
                "toml" => crate::encoding::toml_to_value(&text).unwrap_or(Value::Null),
                #[cfg(feature = "decode-formats")]
                "xml" => crate::encoding::xml_to_value(&text).unwrap_or(Value::Null),
                #[cfg(not(feature = "decode-formats"))]
                "json" | "yaml" | "toml" | "xml" => {
                    crate::exec::record_refusal(format!(
                        "decode({scheme}): the structured readings are not compiled in (feature decode-formats)"
                    ));
                    Value::Null
                }
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
        // `strftime(fmt[; LOCALE])` — a second argument names the
        // locale whose month and weekday names `%B %b %A %a` write;
        // without it the English names. A locale with no table
        // refuses.
        "strftime" | "tfmt" => {
            let fmt = arg_str(call, 0, "%Y-%m-%dT%H:%M:%S").to_string();
            let Some(names) = date_names(call) else {
                return vec![Value::Null];
            };
            vec![match &topic {
                Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                } => Value::Str(crate::temporal::strftime_in(
                    &fmt,
                    *secs,
                    *nanos,
                    *offset_min,
                    names,
                )),
                other => other
                    .temporal_reading()
                    .map(|(s, n)| Value::Str(crate::temporal::strftime_in(&fmt, s, n, None, names)))
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
        // The unit words: a numeric topic scales the unit into a
        // duration (`30 | days` = P30D); a duration topic reads in
        // the unit (`P342D | days` = 342, `P1DT12H | days` = 1.5) —
        // the constructor's inverse, decided by the topic's type.
        "seconds" | "minutes" | "hours" | "days" => duration_units(call, topic.clone()),

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
        // `strptime(fmt[; LOCALE])` — the names read under the
        // locale's table, in every form it lists; the format's
        // literal text matches as written (`'%d %B %Y г.'`).
        "strptime" | "tp" => {
            let fmt = arg_str(call, 0, "%Y-%m-%dT%H:%M:%S").to_string();
            let Some(names) = date_names(call) else {
                return vec![Value::Null];
            };
            vec![match &topic {
                Value::Str(s) => crate::temporal::strptime_in(s, &fmt, names)
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                    })
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            }]
        }

        // `bibref(STANDARD)` — one reference text read under a
        // citation standard (gost, gb7714, cmos, ams) into a record
        // of its fields, `:genus` and the list `:label`; a text
        // that is no reference answers null, as `strptime` does for
        // a text that is no date; an unknown standard is refused
        // naming the ones this build reads.
        "bibref" => bibref_scalar(call, topic.clone()),

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
        // refuses to guess (numbers never lift into ±). Unit-shaped
        // TEXT lifts through the unital reading first: invoking
        // convert by name is explicit intent, so `2KiB |
        // convert(B)` needs no separate `| quantity` (the expression head's
        // author-spelling scope).
        "convert" => convert_scalar(call, topic.clone(), scale),

        // Numeric scalars: a non-numeric topic becomes Null. A
        // QUANTITY rounds in its display unit and stays a quantity
        // — `(::speed | convert('km/h')) | round` is a whole
        // number of km/h, not of base meters-per-second.
        "round" | "floor" | "ceil" if matches!(topic, Value::Quantity { .. }) => {
            round_quantity(topic, call, scale)
        }
        // Bare `round` reads to an integer; `round(d)` keeps `d`
        // decimal places and stays a float (`round(0)` included —
        // the argument form always answers in the topic's float
        // world, the bare form in the integer one). Serialization
        // stays shortest-round-trip — `round(d)` is the in-query
        // remedy for presenting arithmetic like 68.99000000000001.
        // (a whole number past the integers stays a float: the
        // cast would saturate at i64::MAX)
        "round" => vec![numeric_scalar(&topic, |n| match round_digits(call) {
            Some(d) => Value::Float(round_to(n, d)),
            None => whole(n.round()),
        })],
        "floor" => vec![numeric_scalar(&topic, |n| whole(n.floor()))],
        "ceil" => vec![numeric_scalar(&topic, |n| whole(n.ceil()))],
        "abs" => vec![match topic {
            // (`i64::MIN` has no integer absolute value)
            Value::Int(n) => n
                .checked_abs()
                .map_or(Value::Float(-(n as f64)), Value::Int),
            // A quantity has no numeric reading and only the rounders
            // are quantity-aware (spec: The Quantital Fragment), so
            // `abs` over a quantity is null rather than silently
            // dropping its unit to a bare base-magnitude float.
            Value::Quantity { .. } => Value::Null,
            other => numeric_scalar(&other, |n| Value::Float(n.abs())),
        }],
        // The elementary functions over the numeric reading: an
        // undefined value (a logarithm of a non-positive, a root of
        // a negative, an overflow) is null. `log(base)` takes a base;
        // a quantity has no numeric reading here, as for `abs`.
        "log" | "log2" | "log10" | "exp" | "sqrt" => vec![match topic {
            Value::Quantity { .. } => Value::Null,
            other => numeric_scalar(&other, |n| match call.name.as_str() {
                "log" => match arg_num(call, 0) {
                    None if call.args.is_empty() => finite(n.ln()),
                    Some(b) if b > 0.0 && b != 1.0 => finite(n.ln() / b.ln()),
                    _ => Value::Null,
                },
                "log2" => finite(n.log2()),
                "log10" => finite(n.log10()),
                "exp" => finite(n.exp()),
                _ => finite(n.sqrt()),
            }),
        }],
        // `pow(y)` — the topic to the power `y`; an integer topic
        // with a non-negative integer exponent stays an integer
        // while it fits.
        "pow" => vec![match (topic, call.args.first()) {
            (Value::Quantity { .. }, _) => Value::Null,
            (Value::Int(x), Some(Arg::Lit(Value::Int(y)))) if *y >= 0 => {
                match u32::try_from(*y).ok().and_then(|y| x.checked_pow(y)) {
                    Some(v) => Value::Int(v),
                    None => finite((x as f64).powf(*y as f64)),
                }
            }
            (other, _) => match arg_num(call, 0) {
                Some(y) => numeric_scalar(&other, |x| finite(x.powf(y))),
                None => {
                    crate::exec::record_refusal(
                        "pow takes the exponent — pow(2), pow(::a; ::b)".into(),
                    );
                    Value::Null
                }
            },
        }],
        // The keyness statistics: the topic is the word's count in
        // the first corpus, the arguments its corpus size, then the
        // count and size in the second corpus.
        "loglik" | "chi2" => vec![match (
            topic.numeric(),
            arg_num(call, 0),
            arg_num(call, 1),
            arg_num(call, 2),
        ) {
            (Some(a), Some(na), Some(b), Some(nb)) => {
                if call.name == "loglik" {
                    loglik(a, na, b, nb)
                } else {
                    chi2(a, na, b, nb)
                }
            }
            _ if call.args.len() < 3 => {
                crate::exec::record_refusal(format!(
                    "{0} takes the corpus size, then the other count and size — \
                     {0}(na; b; nb), {0}(::a; ::na; ::b; ::nb)",
                    call.name
                ));
                Value::Null
            }
            _ => Value::Null,
        }],
        // The collocation measures: the topic is the pair's count,
        // the arguments the two words' counts and the corpus size
        // (`log_dice` needs no size).
        "mi" | "t_score" | "log_dice" | "tscore" | "logdice" | "dice" => {
            collocation_measure(call, topic.clone())
        }
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
            let Some(re) = crate::exec::compiled_regex(&format!("{case}{pattern}")) else {
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
                    Arg::Expr(_) | Arg::Range(_, _) | Arg::Named { .. } | Arg::Opt { .. } => None,
                })
                .unwrap_or(Value::Null),
            other => other,
        }],
        // Serialize the topic as strict JSON text. (`record` is also
        // named here but constructed in the executor, which has the
        // adapter at hand for its expression arguments.)
        "json" | "jsonl" => vec![Value::Str(topic.to_json())],
        // The one-value kaiv document; `@| kaiv` renders the whole
        // stream as one (the serialization family, the kaiv member).
        // A library value rides as its rows.
        #[cfg(feature = "kaiv")]
        "kaiv" => vec![Value::Str(
            crate::kaiv_out::document(&exported(vec![topic])).unwrap_or_else(|e| e),
        )],
        #[cfg(not(feature = "kaiv"))]
        "kaiv" => {
            crate::exec::record_refusal("kaiv output is not compiled in (feature kaiv)".into());
            vec![topic]
        }
        // `| link` reads the capsa's node (the executor, which has
        // the adapter, handles it); a bare value has nothing to
        // link and passes through.
        "link" => vec![topic],
        other => {
            // Every name the parser admits has an arm here or in the
            // executor; reaching this is a registry/dispatch drift,
            // reported rather than passed through silently.
            crate::exec::record_refusal(format!("command '{other}' has no implementation"));
            vec![topic]
        }
    }
}

/// `timex(LOCALE)`. `apply_scalar`'s arm.
fn timex_scalar(call: &FnCall, topic: Value, text: &str) -> Vec<Value> {
    let locale = arg_str(call, 0, "en");
    if matches!(topic, Value::Null) {
        return Vec::new();
    }
    let Some(found) = crate::timex::find(&text, locale) else {
        crate::exec::record_refusal(format!(
            "timex: no time-expression lists for locale {locale:?} (carried: {})",
            crate::timex::CARRIED
        ));
        return Vec::new();
    };
    let opt = |s: Option<String>| s.map_or(Value::Null, Value::Str);
    return found
        .into_iter()
        .map(|f| {
            Value::Record(vec![
                ("text".to_string(), Value::Str(f.text)),
                ("at".to_string(), Value::Int(f.at as i64)),
                ("kind".to_string(), Value::Str(f.kind.word().to_string())),
                ("value".to_string(), opt(f.value)),
                (
                    "grain".to_string(),
                    opt(f.grain.map(|g| g.word().to_string())),
                ),
            ])
        })
        .collect();
}

/// `substr(range)`. `apply_scalar`'s arm.
fn substr_scalar(call: &FnCall, text: &str) -> Vec<Value> {
    let (a, b) = match call.args.first() {
        Some(Arg::Range(a, b)) => (*a, *b),
        Some(Arg::Lit(Value::Int(i))) => (Some(*i), Some(*i)),
        _ => {
            crate::exec::record_refusal(
                "substr takes a range — substr(1..3), substr(..3), \
                     substr(2..), substr(-2..) — or one position"
                    .into(),
            );
            return vec![Value::Null];
        }
    };
    if a == Some(0) || b == Some(0) {
        crate::exec::record_refusal(
            "substr(0): positions are 1-based — 1 is the first \
                 character, -1 the last"
                .into(),
        );
        return vec![Value::Null];
    }
    let n = text.chars().count() as i64;
    let pos = |i: i64| -> i64 { if i < 0 { n + i + 1 } else { i } };
    let from = pos(a.unwrap_or(1)).max(1);
    let to = pos(b.unwrap_or(-1)).min(n);
    if from > to {
        vec![Value::Str(String::new())]
    } else {
        vec![Value::Str(
            text.chars()
                .skip(from as usize - 1)
                .take((to - from + 1) as usize)
                .collect(),
        )]
    }
}

/// `stress(n)`. `apply_scalar`'s arm.
fn stress_scalar(call: &FnCall, topic: Value, text: &str) -> Vec<Value> {
    let n = match call.args.first() {
        Some(Arg::Lit(v)) => v.numeric().map(|f| f as i64),
        _ => None,
    };
    let Some(n) = n.filter(|n| *n != 0) else {
        crate::exec::record_refusal(
            "stress takes a vowel position — stress(3), stress(-1); \
                 positions are 1-based, negative from the end"
                .into(),
        );
        return vec![Value::Null];
    };
    if matches!(topic, Value::Null) {
        return vec![Value::Null];
    }
    let alphabet = arg_str(call, 1, DEFAULT_VOWELS);
    let chars: Vec<char> = text.nfc().collect();
    let vowels: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter(|(_, c)| is_vowel(**c, alphabet))
        .map(|(i, _)| i)
        .collect();
    let k = if n < 0 {
        vowels.len() as i64 + n
    } else {
        n - 1
    };
    if k < 0 || k as usize >= vowels.len() {
        return vec![Value::Null];
    }
    let at = vowels[k as usize];
    let mut out = String::with_capacity(text.len() + 2);
    for (i, c) in chars.iter().enumerate() {
        out.push(*c);
        if i == at && !matches!(c, 'ё' | 'Ё') && chars.get(i + 1) != Some(&'\u{301}') {
            out.push('\u{301}');
        }
    }
    vec![Value::Str(out)]
}

/// `accent(style)`. `apply_scalar`'s arm.
fn accent_scalar(call: &FnCall, topic: Value, text: &str) -> Vec<Value> {
    if matches!(topic, Value::Null) {
        return vec![Value::Null];
    }
    let style = arg_str(call, 0, "acute");
    let alphabet = arg_str(call, 1, DEFAULT_VOWELS);
    let chars: Vec<char> = text.nfc().collect();
    let bare: String = chars
        .iter()
        .filter(|c| !matches!(c, '\u{301}' | '\u{300}'))
        .collect();
    let Some(n) = stress_at(&text, alphabet) else {
        return vec![Value::Str(bare)];
    };
    let bare_chars: Vec<char> = bare.chars().collect();
    let at = bare_chars
        .iter()
        .enumerate()
        .filter(|(_, c)| is_vowel(**c, alphabet))
        .nth(n - 1)
        .map(|(i, _)| i);
    let Some(at) = at else {
        return vec![Value::Str(bare)];
    };
    let is_yo = matches!(bare_chars[at], 'ё' | 'Ё');
    let mut out = String::with_capacity(bare.len() + 2);
    for (i, c) in bare_chars.iter().enumerate() {
        match style {
            "upper" if i == at => out.extend(c.to_uppercase()),
            "apostrophe" if i == at => {
                out.push(*c);
                if !is_yo {
                    out.push('\'');
                }
            }
            "acute" if i == at => {
                out.push(*c);
                if !is_yo {
                    out.push('\u{301}');
                }
            }
            "none" | "upper" | "apostrophe" | "acute" => out.push(*c),
            other => {
                crate::exec::record_refusal(format!(
                    "accent takes a style — accent(\"acute\"), accent(\"upper\"), \
                         accent(\"apostrophe\"), accent(\"none\") — not '{other}'"
                ));
                return vec![Value::Null];
            }
        }
    }
    vec![Value::Str(out)]
}

/// `seconds` / `minutes` / `hours` / `days`. `apply_scalar`'s arm.
fn duration_units(call: &FnCall, topic: Value) -> Vec<Value> {
    let unit: f64 = match call.name.as_str() {
        "seconds" => 1.0,
        "minutes" => 60.0,
        "hours" => 3600.0,
        _ => 86400.0,
    };
    if let Value::Duration { secs, nanos } = topic {
        let n = (secs as f64 + nanos as f64 / 1e9) / unit;
        return vec![if n.fract() == 0.0 && n.abs() < 1e15 {
            Value::Int(n as i64)
        } else {
            Value::Float(n)
        }];
    }
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

/// `bibref(STANDARD)` — a reference read into a record. `apply_scalar`'s arm.
fn bibref_scalar(call: &FnCall, topic: Value) -> Vec<Value> {
    let standard = arg_str(call, 0, "").to_string();
    if !crate::bibref::STANDARDS.contains(&standard.as_str()) {
        crate::exec::record_refusal(format!(
            "bibref: unknown standard {standard:?} — this build reads {}",
            crate::bibref::STANDARDS.join(", ")
        ));
        return vec![Value::Null];
    }
    vec![match &topic {
        Value::Str(s) => match crate::bibref::parse(&standard, s) {
            Ok(Some(r)) => {
                let mut rec: Vec<(String, Value)> = r
                    .fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::Str(v.clone())))
                    .collect();
                if let Some(g) = r.genus {
                    rec.push(("genus".into(), Value::Str(g.into())));
                }
                if let Some(l) = r.label {
                    rec.push(("label".into(), Value::Str(l)));
                }
                Value::Record(rec)
            }
            _ => Value::Null,
        },
        _ => Value::Null,
    }]
}

/// `convert(unit)`. `apply_scalar`'s arm.
fn convert_scalar(
    call: &FnCall,
    topic: Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
    let target = arg_str(call, 0, "").to_string();
    let lifted;
    let topic = match &topic {
        Value::Str(s) => match crate::quantity::parse_unit_text_with(s, scale) {
            Some((value, base, wv, wu)) => {
                lifted = Value::Quantity {
                    value,
                    base,
                    written: Some((wv, wu)),
                };
                &lifted
            }
            None => &topic,
        },
        t => t,
    };
    vec![match &topic {
        Value::Quantity { value, base, .. } => match scale(&target) {
            Some((factor, tbase)) if &tbase == base => Value::Quantity {
                value: *value,
                base: base.clone(),
                written: Some((*value / factor, target)),
            },
            // A unit of another dimension, or one no library
            // declares: no reading (the recipes rely on a null
            // here — `bushels = null` beside a weight).
            _ => Value::Null,
        },
        _ => Value::Null,
    }]
}

/// `round` / `floor` / `ceil` on a quantity. `apply_scalar`'s arm.
fn round_quantity(
    topic: Value,
    call: &FnCall,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
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
        "round" => match round_digits(call) {
            Some(d) => round_to(wv, d),
            None => wv.round(),
        },
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

/// `mi`, `t_score`, `log_dice` — the collocation measures. `apply_scalar`'s arm.
fn collocation_measure(call: &FnCall, topic: Value) -> Vec<Value> {
    vec![{
        let need = if matches!(call.name.as_str(), "log_dice" | "logdice" | "dice") {
            2
        } else {
            3
        };
        match (
            topic.numeric(),
            arg_num(call, 0),
            arg_num(call, 1),
            arg_num(call, 2),
        ) {
            (Some(fxy), Some(fx), Some(fy), n) if call.args.len() >= need => {
                match call.name.as_str() {
                    "mi" => n.map_or(Value::Null, |n| mi(fxy, fx, fy, n)),
                    "t_score" | "tscore" => n.map_or(Value::Null, |n| t_score(fxy, fx, fy, n)),
                    "dice" => dice(fxy, fx, fy),
                    _ => log_dice(fxy, fx, fy),
                }
            }
            _ if call.args.len() < need => {
                crate::exec::record_refusal(if call.name == "log_dice" {
                    "log_dice takes the two words' counts — log_dice(fx; fy), \
                 log_dice(::fxy; ::fx; ::fy)"
                        .to_string()
                } else {
                    format!(
                        "{0} takes the two words' counts and the corpus size — \
                     {0}(fx; fy; n), {0}(::fxy; ::fx; ::fy; ::n)",
                        call.name
                    )
                });
                Value::Null
            }
            _ => Value::Null,
        }
    }]
}

/// Apply `f` to the topic's numeric value, or `Null` when it has
/// none.
fn numeric_scalar(topic: &Value, f: impl Fn(f64) -> Value) -> Value {
    topic.numeric().map(f).unwrap_or(Value::Null)
}

/// The digits argument of `round(d)`, when one was given. A
/// non-integer argument reads as absent, like `arg_str`'s fallback.
fn round_digits(call: &FnCall) -> Option<i64> {
    match call.args.first() {
        Some(Arg::Lit(Value::Int(d))) => Some(*d),
        None => None,
        Some(other) => {
            crate::exec::record_refusal(format!(
                "{}: the digits must be an integer, not {}",
                call.name,
                arg_kind(other)
            ));
            None
        }
    }
}

/// Round to `d` decimal places (negative `d` reaches left of the
/// point, SQL-style). Scale-round-descale over f64: the answer is
/// the nearest representable double, so `68.99000000000001 |
/// round(2)` prints `68.99`. A scaling overflow (astronomically
/// large topic with large `d`) leaves the value untouched — beyond
/// f64's 17 significant digits the rounding is an identity anyway.
fn round_to(n: f64, d: i64) -> f64 {
    let factor = 10f64.powi(d.clamp(-30, 30) as i32);
    let scaled = n * factor;
    if scaled.is_finite() {
        scaled.round() / factor
    } else {
        n
    }
}

/// Apply a reducing aggregate to the whole value list. The
/// order/selection family (`sort`, `unique`, `reverse`, `first`,
/// `last`) and the keyed aggregates are capsa-preserving and live in
/// the executor instead.
pub fn apply(call: &FnCall, input: Vec<Value>, scale: Scale) -> Vec<Value> {
    // A library value in the stream: read by the two readings,
    // serialized by its rows, counted and ordered as one value,
    // refused by kind anywhere else.
    if let Some(refusal) = input.iter().find_map(|v| match v {
        Value::Foreign(f) => Some(f.refusal(&call.name)),
        Value::Table(t) => Some(table_refusal(t, &call.name)),
        _ => None,
    }) && !matches!(
        call.name.as_str(),
        "@" | "rows" | "%" | "columns" | "#" | "transpose" | "header" | "dim" | "[]" | "cell" | "row-to-header" | "header-to-row" | "json" | "jsonl" | "kaiv" | "csv" | "table" | "pretty" | "count" | "first" | "last" | "reverse"
    ) {
        crate::exec::record_refusal(refusal);
        return vec![Value::Null];
    }
    let input = match call.name.as_str() {
        "json" | "jsonl" | "kaiv" | "csv" | "table" | "pretty" => exported(input),
        _ => input,
    };
    match call.name.as_str() {
        // What `run_for_rendering` leaves where a closing `@| csv` /
        // `@| table` stood: the rows themselves, for the tool to
        // tabulate. Not a name a query can write.
        ROWS_FOR_RENDERING => input,
        // The stream as a table, read by rows or by columns, or
        // reshaped (spec: Command Libraries, "A library value";
        // the reshapes): one library value reads as itself, a
        // stream of records as the list of them.
        "@" | "rows" | "%" | "columns" | "#" | "transpose" | "header" | "dim" | "[]" | "cell"
        | "row-to-header" | "header-to-row" => {
            // One library value, or one record (a record of lists
            // after `|`, whose items the executor hands here), is
            // the whole; anything else is the stream as a list.
            let whole = match input.as_slice() {
                [Value::Foreign(_)] | [Value::Table(_)] | [Value::Record(_)] => {
                    input.into_iter().next().expect("one")
                }
                // `| [n]` indexes the one topic as it stands (a list,
                // a table, or a scalar that refuses).
                [_] if call.name == "[]" => input.into_iter().next().expect("one"),
                _ => Value::list(input),
            };
            let out = refusing(call, match call.name.as_str() {
                "@" | "rows" => rows_reading(whole),
                "%" | "columns" => columns_reading(whole),
                "#" => table_reading(whole),
                "header" => table_meta(whole, "header"),
                "dim" => table_meta(whole, "dim"),
                "[]" => table_rows_at(call, whole),
                "cell" => table_cell(call, whole),
                "row-to-header" | "header-to-row" => header_shift(call, whole),
                _ => transpose_reading(whole),
            });
            match out {
                Value::List(_) => out.into_items().unwrap_or_default(),
                other => vec![other],
            }
        }
        "pivot" => match pivot(call, input) {
            Ok(rows) => rows,
            Err(why) => {
                crate::exec::record_refusal(why);
                vec![Value::Null]
            }
        },
        "melt" => match melt(call, input) {
            Ok(rows) => rows,
            Err(why) => {
                crate::exec::record_refusal(why);
                vec![Value::Null]
            }
        },
        "count" => vec![Value::Int(input.len() as i64)],
        "sum" => vec![sum(&input, scale)],
        "product" => vec![product(&input)],
        "min" => extreme(input, std::cmp::Ordering::Less, scale),
        "max" => extreme(input, std::cmp::Ordering::Greater, scale),
        "mean" | "avg" => vec![mean(&input, scale)],
        "median" => vec![median(&input, scale)],
        // `percentile(p)` — the p-th percentile, 0 ≤ p ≤ 100, by
        // linear interpolation between the order statistics (R's
        // type 7, NumPy's default): `percentile(50)` is the median,
        // `percentile(0)` the minimum, `percentile(100)` the maximum.
        "percentile" => {
            let p = match call.args.first() {
                Some(Arg::Lit(Value::Int(i))) => Some(*i as f64),
                Some(Arg::Lit(Value::Float(f))) => Some(*f),
                _ => None,
            };
            match p {
                Some(p) if (0.0..=100.0).contains(&p) => vec![quantile(&input, p / 100.0, scale)],
                _ => {
                    crate::exec::record_refusal(
                        "percentile takes a number from 0 to 100 — percentile(90)".into(),
                    );
                    vec![Value::Null]
                }
            }
        }
        // `std(sample)` / `var(sample)` divide by n − 1; `stddev` and
        // `variance` are the long aliases.
        "std" | "stddev" => vec![spread(
            &input,
            f64::sqrt,
            scale,
            usize::from(sample_flag(call)),
        )],
        "var" | "variance" => vec![spread(&input, |v| v, scale, usize::from(sample_flag(call)))],
        // Lexical diversity. `entropy` and `yule_k` read the stream
        // as counts (the output of `group | count`): Shannon entropy
        // in bits, Yule's characteristic K. `mtld(t)` reads it as
        // tokens: McCarthy–Jarvis's measure of textual lexical
        // diversity, the type-token-ratio factor `t` (default 0.72),
        // forward and backward averaged.
        // Over a stream of counts (every value numeric) the entropy
        // of those counts; over a stream of values the entropy of
        // their distribution — the words of a text.
        "entropy" => {
            let present: Vec<&Value> = input.iter().filter(|v| !matches!(v, Value::Null)).collect();
            if !present.is_empty() && present.iter().all(|v| v.numeric().is_some()) {
                vec![entropy(&counts(&input))]
            } else {
                vec![entropy(&distribution(&input, scale))]
            }
        }
        // The type-token ratio: the distinct values over all, nulls
        // left out; null for an empty stream.
        "ttr" => {
            let present: Vec<Value> = input
                .into_iter()
                .filter(|v| !matches!(v, Value::Null))
                .collect();
            if present.is_empty() {
                return vec![Value::Null];
            }
            let mut seen = std::collections::HashSet::new();
            let distinct = present
                .iter()
                .filter(|v| seen.insert(v.sort_key_with(scale)))
                .count();
            vec![Value::Float(distinct as f64 / present.len() as f64)]
        }
        // The values that occur once, in first-occurrence order.
        "hapax" => {
            let mut counts: std::collections::HashMap<crate::value::SortKey, usize> =
                std::collections::HashMap::new();
            for v in &input {
                if !matches!(v, Value::Null) {
                    *counts.entry(v.sort_key_with(scale)).or_insert(0) += 1;
                }
            }
            input
                .into_iter()
                .filter(|v| !matches!(v, Value::Null) && counts[&v.sort_key_with(scale)] == 1)
                .collect()
        }
        // `skipgrams(n; k)` — the n-grams of the stream with up to
        // k skipped positions between members, in order of their
        // first member then of the skips, each a list.
        "skipgrams" => {
            let n = arg_int(call, 0).unwrap_or(2).max(1) as usize;
            let k = arg_int(call, 1).unwrap_or(0).max(0) as usize;
            let mut out = Vec::new();
            for start in 0..input.len() {
                skipgrams_from(&input, start, n, k, &mut Vec::new(), &mut out);
            }
            out
        }
        // `jaccard(LIST)` — the Jaccard similarity of the stream's
        // distinct values to the argument's: the intersection over
        // the union; null when both are empty.
        "jaccard" => {
            // The list literal `jaccard(@("a"; "b"))`, or the values
            // as arguments.
            let mut other: Vec<Value> = Vec::new();
            for a in &call.args {
                match a {
                    Arg::Lit(Value::List(items)) => other.extend(items.iter().cloned()),
                    Arg::Lit(v) => other.push(v.clone()),
                    _ => {
                        crate::exec::record_refusal(
                            "jaccard takes the other set as a list — jaccard(@(\"a\"; \"b\"))"
                                .into(),
                        );
                        return vec![Value::Null];
                    }
                }
            }
            let a: std::collections::HashSet<crate::value::SortKey> = input
                .iter()
                .filter(|v| !matches!(v, Value::Null))
                .map(|v| v.sort_key_with(scale))
                .collect();
            let b: std::collections::HashSet<crate::value::SortKey> = other
                .iter()
                .filter(|v| !matches!(v, Value::Null))
                .map(|v| v.sort_key_with(scale))
                .collect();
            let union = a.union(&b).count();
            if union == 0 {
                return vec![Value::Null];
            }
            vec![Value::Float(
                a.intersection(&b).count() as f64 / union as f64,
            )]
        }
        "yule_k" => vec![yule_k(&counts(&input))],
        "mtld" => {
            let t = match call.args.first() {
                None => Some(0.72),
                Some(Arg::Lit(Value::Float(f))) if *f > 0.0 && *f < 1.0 => Some(*f),
                _ => None,
            };
            match t {
                Some(t) => vec![mtld(&input, t)],
                None => {
                    crate::exec::record_refusal(
                        "mtld takes a type-token ratio between 0 and 1 — mtld(0.72)".into(),
                    );
                    vec![Value::Null]
                }
            }
        }
        "join" => vec![Value::Str(join(&input, arg_str(call, 0, "")))],
        // The stream as one JSON document — an array — or as JSON
        // Lines, one document per line.
        "json" => {
            let items: Vec<String> = input.iter().map(Value::to_json).collect();
            vec![Value::Str(format!("[{}]", items.join(", ")))]
        }
        "jsonl" => {
            let items: Vec<String> = input.iter().map(Value::to_json).collect();
            vec![Value::Str(items.join("\n"))]
        }
        // The stream as one CSV document, or as one aligned text
        // table: a header and a row per value, the header never
        // apart from its rows. Named columns select and order the
        // fields; without them, every field.
        "csv" | "table" | "pretty" => {
            // `csv(";"; year; title)`: a quoted first argument is the
            // delimiter (a bare word is a column, so no collision).
            // The command is a value like any other and never
            // depends on a locale: a tool that prints under one
            // takes a closing `@| csv` / `@| table` off the query
            // and renders it itself (`run_for_rendering`).
            let (delimiter, args) = split_csv_delimiter(call);
            let columns: Vec<String> = args
                .iter()
                .filter_map(|a| match a {
                    Arg::Lit(Value::Str(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            let columns = (!columns.is_empty()).then_some(columns.as_slice());
            vec![Value::Str(tabulate_with(
                &input,
                columns,
                call.name == "csv",
                delimiter.unwrap_or(b','),
            ))]
        }
        // The stream as one kaiv document: results under
        // `/@results`, records as namespaces, lists as arrays,
        // units kept. Provenance rides qua's `--kaiv`, not the
        // command (the command sees values, not nodes).
        #[cfg(feature = "kaiv")]
        "kaiv" => vec![Value::Str(
            crate::kaiv_out::document(&input).unwrap_or_else(|e| e),
        )],
        #[cfg(not(feature = "kaiv"))]
        "kaiv" => {
            crate::exec::record_refusal("kaiv output is not compiled in (feature kaiv)".into());
            input
        }
        // The order/selection family over an explicit value list —
        // reached from the per-capsa list reductions (`| last` on a
        // group's list topic); the `@|` forms are capsa-preserving
        // and intercepted in the executor before this point.
        // `sort` orders by the standard value comparison; with a
        // locale argument (`sort(ru-RU)` — any Unicode locale
        // identifier), values sort by their text under that
        // locale's collation (colligo; CLDR-derived), and with the
        // word `atergo` in the a tergo order. Reverse by piping
        // into `| reverse`.
        "sort" => {
            // Keys once per value, not two per comparison: the
            // display form under a text order, the sort key (and
            // the text, for a collation) under the default.
            let v = input;
            match text_order(call) {
                Some(cmp) => {
                    let mut keyed: Vec<(String, Value)> =
                        v.into_iter().map(|x| (x.to_string(), x)).collect();
                    keyed.sort_by(|a, b| cmp(&a.0, &b.0));
                    keyed.into_iter().map(|(_, x)| x).collect()
                }
                None => {
                    let collate = default_text_order();
                    let mut keyed: Vec<(crate::value::SortKey, Value)> =
                        v.into_iter().map(|x| (x.sort_key_with(scale), x)).collect();
                    keyed.sort_by(|a, b| match (collate.as_deref(), &a.0, &b.0, &a.1, &b.1) {
                        (
                            Some(c),
                            crate::value::SortKey::Text(_),
                            crate::value::SortKey::Text(_),
                            Value::Str(x),
                            Value::Str(y),
                        ) => c(x, y),
                        _ => a.0.cmp_key(&b.0),
                    });
                    keyed.into_iter().map(|(_, x)| x).collect()
                }
            }
        }
        "reverse" => {
            let mut v = input;
            v.reverse();
            v
        }
        // `combinations(n)` — every unordered n-member subset of
        // the stream's distinct values, each once, as a list whose
        // members stand in the values' own order: (a, b) and (b, a)
        // are one combination and one value, so a group counts
        // them together. `pairs` and `triples` are n = 2 and 3.
        // The adjacent n-windows of the stream, in stream order,
        // full windows only (`window(n)` keeps the partial ones at
        // the start); each a list. What is tokenized is chosen
        // upstream: `//token<word>::lower @| bigrams`.
        "ngrams" | "bigrams" | "trigrams" => {
            let n = match call.name.as_str() {
                "bigrams" => 2,
                "trigrams" => 3,
                _ => arg_int(call, 0).unwrap_or(2).max(1) as usize,
            };
            input.windows(n).map(|w| Value::list(w.to_vec())).collect()
        }
        // Each distinct value (under the value comparison, nulls
        // left out) with its count, the most frequent first and
        // equal counts by value, as records; `frequencies(k)` keeps
        // the top k.
        "frequencies" => {
            let mut counts: Vec<(Value, i64)> = Vec::new();
            let mut index: std::collections::HashMap<crate::value::SortKey, usize> =
                std::collections::HashMap::new();
            for v in input {
                if matches!(v, Value::Null) {
                    continue;
                }
                let key = v.sort_key_with(scale);
                match index.get(&key) {
                    Some(&i) => counts[i].1 += 1,
                    None => {
                        index.insert(key, counts.len());
                        counts.push((v, 1));
                    }
                }
            }
            counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.compare_with(&b.0, scale)));
            // The rows asked for: the first k, a rank band
            // (`11..20`), or from the bottom (`-10`, `-20..-11`).
            if let Some((s, e)) = rank_span(call.args.first(), counts.len(), false) {
                counts.truncate(e);
                counts.drain(..s);
            }
            counts
                .into_iter()
                .map(|(v, n)| {
                    Value::Record(vec![
                        ("value".to_string(), v),
                        ("n".to_string(), Value::Int(n)),
                    ])
                })
                .collect()
        }
        "combinations" | "pairs" | "triples" => {
            let n = match call.name.as_str() {
                "pairs" => 2,
                "triples" => 3,
                _ => arg_int(call, 0).unwrap_or(2).max(0) as usize,
            };
            let mut seen = std::collections::HashSet::new();
            let mut items: Vec<Value> = input
                .into_iter()
                .filter(|v| !matches!(v, Value::Null) && seen.insert(v.sort_key_with(scale)))
                .collect();
            items.sort_by(|a, b| a.compare_with(b, scale));
            let mut out = Vec::new();
            if n >= 1 && n <= items.len() {
                let mut idx: Vec<usize> = (0..n).collect();
                loop {
                    out.push(Value::list(idx.iter().map(|&i| items[i].clone()).collect()));
                    // the next index set in lexicographic order
                    let Some(k) = (0..n).rev().find(|&k| idx[k] != k + items.len() - n) else {
                        break;
                    };
                    idx[k] += 1;
                    for j in k + 1..n {
                        idx[j] = idx[j - 1] + 1;
                    }
                }
            }
            out
        }
        // Distinct under the value comparison (the sort key), as
        // `unique_by` is: null and "" stay apart, 1 and 1.0 meet.
        "unique" => {
            let mut seen: std::collections::HashSet<crate::value::SortKey> =
                std::collections::HashSet::new();
            input
                .into_iter()
                .filter(|v| seen.insert(v.sort_key_with(scale)))
                .collect()
        }
        // `first` / `last`: one value, or the first / last n.
        "first" => {
            let n = arg_int(call, 0).unwrap_or(1).max(0) as usize;
            input.into_iter().take(n).collect()
        }
        "last" => {
            let n = arg_int(call, 0).unwrap_or(1).max(0) as usize;
            let skip = input.len().saturating_sub(n);
            input.into_iter().skip(skip).collect()
        }
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
/// The product of the numeric readings. Empty input is the fold's
/// identity, 1; input with no reading at all is null, as `sum`'s
/// is. One quantity among numbers scales (`2 km` by 3 is `6 km`);
/// two quantities would multiply their dimensions, which the
/// unital fragment does not combine, so that is refused rather
/// than answered on a dropped unit.
fn product(input: &[Value]) -> Value {
    if !input.is_empty() && !input.iter().any(|v| v.numeric().is_some()) {
        return Value::Null;
    }
    let quantities: Vec<&Value> = input
        .iter()
        .filter(|v| matches!(v, Value::Quantity { .. }))
        .collect();
    match quantities.as_slice() {
        [] => {}
        [
            Value::Quantity {
                value,
                base,
                written,
            },
        ] => {
            let factor: f64 = input
                .iter()
                .filter(|v| !matches!(v, Value::Quantity { .. }))
                .filter_map(Value::numeric)
                .product();
            return Value::Quantity {
                value: value * factor,
                base: base.clone(),
                written: written.as_ref().map(|(m, u)| (m * factor, u.clone())),
            };
        }
        _ => {
            crate::exec::record_refusal(
                "product: two quantities would multiply their units, which the unital \
                 fragment does not combine; take the magnitudes first"
                    .into(),
            );
            return Value::Null;
        }
    }
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

/// The q-th quantile (0 ≤ q ≤ 1) by linear interpolation between
/// the sorted values (R type 7): at rank h = (n − 1)·q the value
/// is v[⌊h⌋] + (h − ⌊h⌋)·(v[⌊h⌋+1] − v[⌊h⌋]). Same-base quantities
/// yield a quantity; mixed dimensions refuse; integers stay
/// integers when the rank lands on an element.
fn quantile(input: &[Value], q: f64, scale: Scale) -> Value {
    fn at(mut sorted: Vec<f64>, q: f64) -> Option<(f64, bool)> {
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        let h = (sorted.len() - 1) as f64 * q;
        let lo = h.floor() as usize;
        let frac = h - lo as f64;
        let v = if frac == 0.0 || lo + 1 >= sorted.len() {
            sorted[lo]
        } else {
            sorted[lo] + frac * (sorted[lo + 1] - sorted[lo])
        };
        Some((v, frac == 0.0))
    }
    match quantital_fold(input, scale) {
        QuantFold::Unsound => return Value::Null,
        QuantFold::Same { base, unit, mags } => {
            return match at(mags, q) {
                Some((v, _)) => quantital_result(v, base, unit),
                None => Value::Null,
            };
        }
        QuantFold::Absent => {}
    }
    if matches!(durational_fold(input), DurFold::Unsound) {
        return Value::Null;
    }
    let nums: Vec<f64> = input.iter().filter_map(Value::numeric).collect();
    match at(nums, q) {
        Some((v, exact)) if exact && input.iter().all(|v| matches!(v, Value::Int(_))) => {
            Value::Int(v as i64)
        }
        Some((v, _)) => Value::Float(v),
        None => Value::Null,
    }
}

/// The Levenshtein distance between two texts, in codepoints.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Variance, post-processed by `finish` (identity for `variance`,
/// square root for `stddev`); `ddof` 0 is the population measure,
/// 1 the sample measure (null below two values).
fn spread(input: &[Value], finish: impl Fn(f64) -> f64, scale: Scale, ddof: usize) -> Value {
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
    if nums.len() <= ddof {
        return Value::Null;
    }
    let n = nums.len() as f64;
    let m = nums.iter().sum::<f64>() / n;
    let var = nums.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - ddof as f64);
    finite(finish(var))
}

/// A float as a value, null when it is not finite (an overflow, a
/// logarithm of nothing, a zero denominator).
fn finite(x: f64) -> Value {
    if x.is_finite() {
        Value::Float(x)
    } else {
        Value::Null
    }
}

/// The numeric readings of a stream, non-readings dropped.
fn readings(input: &[Value]) -> Vec<f64> {
    input.iter().filter_map(Value::numeric).collect()
}

/// A stream read as counts: the positive numeric readings.
fn counts(input: &[Value]) -> Vec<f64> {
    readings(input).into_iter().filter(|c| *c > 0.0).collect()
}

/// The counts of a stream's distinct values (nulls left out).
fn distribution(input: &[Value], scale: Scale) -> Vec<f64> {
    let mut counts: std::collections::HashMap<crate::value::SortKey, f64> =
        std::collections::HashMap::new();
    for v in input {
        if !matches!(v, Value::Null) {
            *counts.entry(v.sort_key_with(scale)).or_insert(0.0) += 1.0;
        }
    }
    counts.into_values().collect()
}

/// The skip-grams starting at `start`: `chosen` so far, `n` to
/// choose in all, `k` skips still allowed.
fn skipgrams_from(
    input: &[Value],
    start: usize,
    n: usize,
    k: usize,
    chosen: &mut Vec<Value>,
    out: &mut Vec<Value>,
) {
    chosen.push(input[start].clone());
    if chosen.len() == n {
        out.push(Value::list(chosen.clone()));
    } else {
        for skip in 0..=k {
            let next = start + 1 + skip;
            if next >= input.len() {
                break;
            }
            skipgrams_from(input, next, n, k - skip, chosen, out);
        }
    }
    chosen.pop();
}

/// The corpus tier's class of a token: a word (a letter in it), a
/// number (a digit, no letter), else punctuation.
fn token_class(seg: &str) -> &'static str {
    if seg.chars().any(char::is_alphabetic) {
        "word"
    } else if seg.chars().any(char::is_numeric) {
        "number"
    } else {
        "punct"
    }
}

/// Unicode case folding: the simple lowercase mapping plus the
/// full foldings that change length or letter (CaseFolding.txt's
/// C + F statuses for the letters a text meets).
fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'ß' | 'ẞ' => out.push_str("ss"),
            'ς' => out.push('σ'),
            'ſ' => out.push('s'),
            'İ' => out.push_str("i\u{307}"),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            'ŉ' => out.push_str("\u{2bc}n"),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// The text without its combining marks: NFD, the marks (general
/// category M) dropped, the Cyrillic letters й and ё kept whole
/// (`ru` folds ё to е).
fn deaccent(text: &str, ru: bool) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'й' | 'Й' => out.push(c),
            'ё' if !ru => out.push(c),
            'Ё' if !ru => out.push(c),
            'ё' => out.push('е'),
            'Ё' => out.push('Е'),
            c => out.extend(c.to_string().nfd().filter(|m| !is_combining_mark(*m))),
        }
    }
    out
}

/// The languages the stemmer ships, by their two-letter tags.
const STEMMER_LANGS: &[&str] = &[
    "ar", "da", "nl", "en", "fi", "fr", "de", "el", "hu", "it", "no", "pt", "ro", "ru", "es", "sv",
    "ta", "tr",
];

fn stemmer_for(lang: &str) -> Option<rust_stemmers::Algorithm> {
    use rust_stemmers::Algorithm::*;
    Some(match lang {
        "ar" => Arabic,
        "da" => Danish,
        "nl" => Dutch,
        "en" => English,
        "fi" => Finnish,
        "fr" => French,
        "de" => German,
        "el" => Greek,
        "hu" => Hungarian,
        "it" => Italian,
        "no" => Norwegian,
        "pt" => Portuguese,
        "ro" => Romanian,
        "ru" => Russian,
        "es" => Spanish,
        "sv" => Swedish,
        "ta" => Tamil,
        "tr" => Turkish,
        _ => return None,
    })
}

/// Shannon entropy in bits of a distribution given as counts.
fn entropy(counts: &[f64]) -> Value {
    let total: f64 = counts.iter().sum();
    if total <= 0.0 {
        return Value::Null;
    }
    let h = -counts
        .iter()
        .map(|c| {
            let p = c / total;
            p * p.log2()
        })
        .sum::<f64>();
    finite(h)
}

/// Yule's characteristic K over counts: 10⁴ (Σc² − N) / N², N = Σc.
fn yule_k(counts: &[f64]) -> Value {
    let total: f64 = counts.iter().sum();
    if total <= 0.0 {
        return Value::Null;
    }
    let squares: f64 = counts.iter().map(|c| c * c).sum();
    finite(10_000.0 * (squares - total) / (total * total))
}

/// MTLD (McCarthy & Jarvis 2010) over a token stream: the mean
/// length of the runs whose type-token ratio stays above `t`,
/// forward and backward averaged; the trailing partial run counts
/// as (1 − TTR) / (1 − t) of a factor.
fn mtld(input: &[Value], t: f64) -> Value {
    let tokens: Vec<String> = input.iter().map(Value::to_string).collect();
    if tokens.is_empty() {
        return Value::Null;
    }
    let one_way = |toks: &mut dyn Iterator<Item = &String>| -> f64 {
        let mut factors = 0.0;
        let mut types = std::collections::HashSet::new();
        let mut n = 0usize;
        let mut ttr = 1.0;
        for tok in toks {
            n += 1;
            types.insert(tok.as_str());
            ttr = types.len() as f64 / n as f64;
            if ttr <= t {
                factors += 1.0;
                types.clear();
                n = 0;
                ttr = 1.0;
            }
        }
        if n > 0 {
            factors += (1.0 - ttr) / (1.0 - t);
        }
        factors
    };
    let forward = one_way(&mut tokens.iter());
    let backward = one_way(&mut tokens.iter().rev());
    let n = tokens.len() as f64;
    let mean_of = |f: f64| if f > 0.0 { n / f } else { f64::INFINITY };
    let (a, b) = (mean_of(forward), mean_of(backward));
    if !a.is_finite() || !b.is_finite() {
        // No run ever fell below `t`: the text is too short or too
        // varied to measure.
        return Value::Null;
    }
    finite((a + b) / 2.0)
}

/// The association measures over paired readings: Pearson's `corr`,
/// Spearman's rank correlation, the cosine of the two vectors.
/// Fewer than two pairs, or a zero spread on either side, is null.
pub fn associate(name: &str, pairs: &[(f64, f64)]) -> Value {
    if pairs.len() < 2 {
        return Value::Null;
    }
    match name {
        "corr" => pearson(pairs),
        "spearman" => {
            let xs = average_ranks(pairs.iter().map(|p| p.0));
            let ys = average_ranks(pairs.iter().map(|p| p.1));
            pearson(&xs.into_iter().zip(ys).collect::<Vec<_>>())
        }
        "cosine" => {
            let dot: f64 = pairs.iter().map(|(x, y)| x * y).sum();
            let nx: f64 = pairs.iter().map(|(x, _)| x * x).sum::<f64>().sqrt();
            let ny: f64 = pairs.iter().map(|(_, y)| y * y).sum::<f64>().sqrt();
            if nx == 0.0 || ny == 0.0 {
                Value::Null
            } else {
                finite(dot / (nx * ny))
            }
        }
        "delta" => delta(pairs),
        "dp" => dp(pairs),
        _ => Value::Null,
    }
}

/// Burrows's Delta between two profiles given as paired standard
/// scores: the mean absolute difference over the pairs.
fn delta(pairs: &[(f64, f64)]) -> Value {
    let n = pairs.len() as f64;
    finite(pairs.iter().map(|(x, y)| (x - y).abs()).sum::<f64>() / n)
}

/// Gries's deviation of proportions: each pair is a part's count of
/// the word and the part's size; DP is half the sum over the parts
/// of |observed share − expected share|, where the expected share is
/// the part's share of the corpus. 0 is an even spread, 1 a word
/// confined to one vanishing part; a corpus without the word, or
/// without tokens, is null.
fn dp(pairs: &[(f64, f64)]) -> Value {
    let total: f64 = pairs.iter().map(|p| p.0).sum();
    let size: f64 = pairs.iter().map(|p| p.1).sum();
    if !(total > 0.0) || !(size > 0.0) || pairs.iter().any(|p| p.0 < 0.0 || p.1 < 0.0) {
        return Value::Null;
    }
    let sum: f64 = pairs
        .iter()
        .map(|(n, s)| (n / total - s / size).abs())
        .sum();
    finite(sum / 2.0)
}

/// The agreement measures over paired labels: Cohen's κ over the
/// categories the two sides name, and precision, recall and F₁ of
/// the second side against the first, a label read as positive by
/// its truthiness. No pairs is null; κ with one category on both
/// sides is null (no chance agreement to correct for).
pub fn agree(name: &str, pairs: &[(Value, Value)]) -> Value {
    if pairs.is_empty() {
        return Value::Null;
    }
    match name {
        "kappa" => {
            let n = pairs.len() as f64;
            let mut labels: Vec<String> = Vec::new();
            let mut key = |v: &Value| {
                let k = v.to_string();
                if !labels.contains(&k) {
                    labels.push(k.clone());
                }
                k
            };
            let keyed: Vec<(String, String)> =
                pairs.iter().map(|(a, b)| (key(a), key(b))).collect();
            let observed = keyed.iter().filter(|(a, b)| a == b).count() as f64 / n;
            let expected: f64 = labels
                .iter()
                .map(|l| {
                    let pa = keyed.iter().filter(|(a, _)| a == l).count() as f64 / n;
                    let pb = keyed.iter().filter(|(_, b)| b == l).count() as f64 / n;
                    pa * pb
                })
                .sum();
            if (1.0 - expected).abs() < f64::EPSILON {
                Value::Null
            } else {
                finite((observed - expected) / (1.0 - expected))
            }
        }
        _ => {
            let tp = pairs
                .iter()
                .filter(|(g, p)| g.is_truthy() && p.is_truthy())
                .count() as f64;
            let fp = pairs
                .iter()
                .filter(|(g, p)| !g.is_truthy() && p.is_truthy())
                .count() as f64;
            let fn_ = pairs
                .iter()
                .filter(|(g, p)| g.is_truthy() && !p.is_truthy())
                .count() as f64;
            let precision = if tp + fp > 0.0 {
                Some(tp / (tp + fp))
            } else {
                None
            };
            let recall = if tp + fn_ > 0.0 {
                Some(tp / (tp + fn_))
            } else {
                None
            };
            match name {
                "precision" => precision.map_or(Value::Null, finite),
                "recall" => recall.map_or(Value::Null, finite),
                _ => match (precision, recall) {
                    (Some(p), Some(r)) if p + r > 0.0 => finite(2.0 * p * r / (p + r)),
                    (Some(_), Some(_)) => Value::Float(0.0),
                    _ => Value::Null,
                },
            }
        }
    }
}

fn pearson(pairs: &[(f64, f64)]) -> Value {
    let n = pairs.len() as f64;
    let mx = pairs.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pairs.iter().map(|p| p.1).sum::<f64>() / n;
    let sxy: f64 = pairs.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
    let sxx: f64 = pairs.iter().map(|(x, _)| (x - mx) * (x - mx)).sum();
    let syy: f64 = pairs.iter().map(|(_, y)| (y - my) * (y - my)).sum();
    if sxx == 0.0 || syy == 0.0 {
        Value::Null
    } else {
        finite(sxy / (sxx * syy).sqrt())
    }
}

/// Ranks 1..n with ties given the average of their positions.
fn average_ranks(values: impl Iterator<Item = f64>) -> Vec<f64> {
    let values: Vec<f64> = values.collect();
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
    let mut ranks = vec![0.0; values.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len() && values[order[j + 1]] == values[order[i]] {
            j += 1;
        }
        // Positions i..=j (0-based) share the rank (i + j) / 2 + 1.
        let rank = (i + j) as f64 / 2.0 + 1.0;
        for &k in &order[i..=j] {
            ranks[k] = rank;
        }
        i = j + 1;
    }
    ranks
}

/// Standard scores: each reading becomes (x − mean) / sd over the
/// readings present (population sd, or sample with `sample`); a
/// missing reading, or a stream with no spread, is null.
pub fn zscores(xs: &[Option<f64>], sample: bool) -> Vec<Value> {
    let present: Vec<f64> = xs.iter().flatten().copied().collect();
    let ddof = usize::from(sample);
    if present.len() <= ddof.max(1) {
        return vec![Value::Null; xs.len()];
    }
    let n = present.len() as f64;
    let m = present.iter().sum::<f64>() / n;
    let var = present.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - ddof as f64);
    let sd = var.sqrt();
    if sd == 0.0 || !sd.is_finite() {
        return vec![Value::Null; xs.len()];
    }
    xs.iter()
        .map(|x| match x {
            Some(x) => finite((x - m) / sd),
            None => Value::Null,
        })
        .collect()
}

/// Dunning's log-likelihood G² of a word seen `a` times in `na`
/// tokens against `b` times in `nb`, signed positive where the first
/// corpus over-represents it (0 · log 0 = 0).
fn loglik(a: f64, na: f64, b: f64, nb: f64) -> Value {
    if !(na > 0.0 && nb > 0.0) || a < 0.0 || b < 0.0 || a > na || b > nb || a + b == 0.0 {
        return Value::Null;
    }
    let e1 = na * (a + b) / (na + nb);
    let e2 = nb * (a + b) / (na + nb);
    let term = |o: f64, e: f64| if o > 0.0 { o * (o / e).ln() } else { 0.0 };
    let g2 = 2.0 * (term(a, e1) + term(b, e2));
    finite(if a / na >= b / nb { g2 } else { -g2 })
}

/// Pearson's χ² on the 2×2 table [[a, na − a], [b, nb − b]], no
/// continuity correction, signed like [`loglik`]; a zero marginal
/// is null.
fn chi2(a: f64, na: f64, b: f64, nb: f64) -> Value {
    if !(na > 0.0 && nb > 0.0) || a < 0.0 || b < 0.0 || a > na || b > nb {
        return Value::Null;
    }
    let (c, d) = (na - a, nb - b);
    let n = na + nb;
    let (col1, col2) = (a + b, c + d);
    if col1 == 0.0 || col2 == 0.0 {
        return Value::Null;
    }
    let diff = a * d - b * c;
    let x2 = n * diff * diff / (col1 * col2 * na * nb);
    finite(if a / na >= b / nb { x2 } else { -x2 })
}

/// Pointwise mutual information of a pair seen `fxy` times whose
/// words are seen `fx` and `fy` times in `n` tokens: log₂ of the
/// observed count over the expected `fx · fy / n`. A zero count or
/// size, or a pair count above either word's, is null.
/// The Dice coefficient: 2·fxy / (fx + fy).
fn dice(fxy: f64, fx: f64, fy: f64) -> Value {
    if !(fxy > 0.0 && fx > 0.0 && fy > 0.0) || fxy > fx || fxy > fy {
        return Value::Null;
    }
    finite(2.0 * fxy / (fx + fy))
}

fn mi(fxy: f64, fx: f64, fy: f64, n: f64) -> Value {
    if !(fxy > 0.0 && fx > 0.0 && fy > 0.0 && n > 0.0) || fxy > fx || fxy > fy {
        return Value::Null;
    }
    finite((fxy * n / (fx * fy)).log2())
}

/// The t-score of the same pair: (observed − expected) / √observed.
fn t_score(fxy: f64, fx: f64, fy: f64, n: f64) -> Value {
    if !(fxy > 0.0 && fx > 0.0 && fy > 0.0 && n > 0.0) || fxy > fx || fxy > fy {
        return Value::Null;
    }
    finite((fxy - fx * fy / n) / fxy.sqrt())
}

/// Rychlý's logDice: 14 + log₂ of the Dice coefficient
/// `2 · fxy / (fx + fy)`, needing no corpus size; at most 14.
fn log_dice(fxy: f64, fx: f64, fy: f64) -> Value {
    if !(fxy > 0.0 && fx > 0.0 && fy > 0.0) || fxy > fx || fxy > fy {
        return Value::Null;
    }
    finite(14.0 + (2.0 * fxy / (fx + fy)).log2())
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
/// The UAX #29 sentences of a text, trimmed, empties dropped —
/// with the session's sentence bonds applied when any are loaded.
fn sentences_of(text: &str) -> Vec<&str> {
    let raw: Vec<&str> = match crate::exec::run_option(|o| o.bonds.clone()) {
        Some(b) => b.unicode_sentences(text),
        None => {
            use unicode_segmentation::UnicodeSegmentation;
            text.unicode_sentences().collect()
        }
    };
    raw.into_iter()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect()
}

/// The vowel letters `vowels` and `stress` count by default: the
/// Cyrillic and the Latin ones, case-folded at the comparison.
const DEFAULT_VOWELS: &str = "аеёиоуыэюяaeiouy";

fn is_vowel(c: char, alphabet: &str) -> bool {
    c.to_lowercase().any(|l| alphabet.contains(l))
}

/// The 1-based ordinal of the stressed vowel of a marked form, by
/// the same rules `stress(n)` writes it: the vowel carrying the
/// acute; else a ё; else the one vowel of a monosyllable.
fn stress_at(text: &str, alphabet: &str) -> Option<usize> {
    let chars: Vec<char> = text.nfc().collect();
    let mut n = 0;
    for (i, c) in chars.iter().enumerate() {
        if is_vowel(*c, alphabet) {
            n += 1;
            if chars.get(i + 1) == Some(&'\u{301}') {
                return Some(n);
            }
        }
    }
    let mut n = 0;
    for c in &chars {
        if is_vowel(*c, alphabet) {
            n += 1;
            if matches!(c, 'ё' | 'Ё') {
                return Some(n);
            }
        }
    }
    (chars.iter().filter(|c| is_vowel(**c, alphabet)).count() == 1).then_some(1)
}

/// The n-th argument as an integer, `None` when there is none. An
/// argument that is there but is not an integer refuses the call
/// — `pad("5")` is a mistake, not a pad to width 0.
/// The ranks a count or range argument selects over `len` items, as
/// a half-open index span: `k` the first k, `-k` the last k, `a..b`
/// the 1-based ranks a to b inclusive, a negative rank counted from
/// the end (`-1` the last), either end of the range open. With
/// `from_end` (for `last`) the ranks run the other way: positive
/// from the end, negative from the start. None when the argument is
/// not a count or a range.
pub(crate) fn rank_span(arg: Option<&Arg>, len: usize, from_end: bool) -> Option<(usize, usize)> {
    let n = len as i64;
    // A rank to its 1-based position from the start.
    let pos = |i: i64| -> i64 {
        match (i >= 0, from_end) {
            (true, false) => i,
            (true, true) => n + 1 - i,
            (false, false) => n + 1 + i,
            (false, true) => -i,
        }
    };
    let (a, b) = match arg? {
        Arg::Lit(Value::Int(0)) => return Some((0, 0)),
        Arg::Lit(Value::Int(k)) if *k > 0 => (1, *k),
        Arg::Lit(Value::Int(k)) => (*k, -1),
        Arg::Range(a, b) => (a.unwrap_or(1), b.unwrap_or(n)),
        _ => return None,
    };
    // The band's ends as positions; `last` runs the ranks the other
    // way, so its ends swap. A band past the items is empty.
    let (lo, hi) = if from_end {
        (pos(b), pos(a))
    } else {
        (pos(a), pos(b))
    };
    let (lo, hi) = (lo.max(1), hi.min(n));
    if hi < lo {
        return Some((0, 0));
    }
    Some(((lo - 1) as usize, hi as usize))
}

// ── The two readings of a table, and the reshapes ─────────────────
// (spec: Command Libraries, "A library value"; the reshapes carry
// no algorithm: what rearranges a table is here, what computes over
// one is a library's.)

/// A refused reading is a refusal and a null topic.
fn refusing(call: &FnCall, r: Result<Value, String>) -> Value {
    match r {
        Ok(v) => v,
        Err(why) => {
            crate::exec::record_refusal(format!("{}: {why}", call.name));
            Value::Null
        }
    }
}

/// The values with every library value replaced by its rows, each
/// row a value of the stream — what a serialization reads.
fn exported(input: Vec<Value>) -> Vec<Value> {
    if !input.iter().any(|v| matches!(v, Value::Foreign(_) | Value::Table(_))) {
        return input;
    }
    let mut out = Vec::with_capacity(input.len());
    for v in input {
        match v {
            Value::Foreign(f) => match f.rows() {
                Ok(Value::List(rows)) => out.extend(rows.iter().cloned()),
                Ok(other) => out.push(other),
                Err(_) => out.push(Value::Str(f.display())),
            },
            Value::Table(t) => match t.rows_value() {
                Value::List(rows) => out.extend(rows.iter().cloned()),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    out
}

/// A value's kind, for a reshape's refusal.
fn kind_of(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "a boolean".into(),
        Value::Int(_) | Value::Float(_) => "a number".into(),
        Value::Str(_) => "text".into(),
        Value::List(_) => "a list".into(),
        Value::Record(_) => "a record".into(),
        Value::Instant { .. } => "an instant".into(),
        Value::Duration { .. } => "a duration".into(),
        Value::Period { .. } => "a period".into(),
        Value::Quantity { .. } => "a quantity".into(),
        Value::Foreign(f) => f.kind(),
        Value::Table(t) => t.kind(),
    }
}

/// The column names among a renderer's arguments (`csv(year;
/// title)`), `None` when none is written.
fn column_names(args: &[Arg]) -> Option<Vec<String>> {
    let columns: Vec<String> = args
        .iter()
        .filter_map(|a| match a {
            Arg::Lit(Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    (!columns.is_empty()).then_some(columns)
}

/// A table as the aligned grid or as CSV: its rows as records, the
/// array face keyed by position; `columns` selects and orders them.
pub(crate) fn tabulate_table(
    t: &crate::Table,
    csv: bool,
    columns: Option<&[String]>,
    delimiter: u8,
) -> String {
    let keys: Vec<String> = match &t.header {
        Some(h) => h.clone(),
        None => (1..=t.width()).map(|j| j.to_string()).collect(),
    };
    let rows: Vec<Value> = t
        .rows
        .iter()
        .map(|r| {
            Value::Record(
                keys.iter()
                    .cloned()
                    .zip(r.iter().cloned().chain(std::iter::repeat(Value::Null)))
                    .collect(),
            )
        })
        .collect();
    tabulate_with(&rows, columns, csv, delimiter)
}

/// `| pretty` — a value pretty-printed: a table as the grid, a record
/// one field per line, a list one item per line, a scalar as it is.
pub(crate) fn pretty_value(v: &Value) -> String {
    match v {
        Value::Table(t) => tabulate_table(t, false, None, b','),
        Value::Record(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{} = {}", crate::value::quarb_key(k), v.to_quarb()))
                .collect();
            format!("%({})", inner.join(";\n  "))
        }
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(Value::to_quarb).collect();
            format!("@({})", inner.join(";\n  "))
        }
        other => other.display_form(),
    }
}

/// What a table refuses with: the two readings it admits.
fn table_refusal(t: &crate::Table, command: &str) -> String {
    format!(
        "{command}: {} is a table; '| @' reads its rows, '| %' its columns",
        t.kind()
    )
}

/// The table reading (`| #`): a table as itself; a list of records
/// as a frame whose header is the names in order of first
/// appearance, a record lacking one leaving a hole; a list of lists
/// as an array (a short row padded with holes, a long one refused);
/// a list of scalars as a one-column array (a sample); a record of
/// lists as a frame by columns.
pub(crate) fn table_reading(v: Value) -> Result<Value, String> {
    match v {
        Value::Table(_) => Ok(v),
        Value::Foreign(f) => f.rows().and_then(table_reading),
        // A record of lists is the columns (the inverse of `| %`);
        // any other record is one row.
        Value::Record(fields) => {
            if !fields.is_empty() && fields.iter().all(|(_, v)| matches!(v, Value::List(_))) {
                let rows = rows_reading(Value::Record(fields))?;
                return table_reading(rows);
            }
            table_reading(Value::list(vec![Value::Record(fields)]))
        }
        Value::List(items) => {
            if items.is_empty() {
                return Ok(Value::table(crate::Table {
                    header: None,
                    rows: Vec::new(),
                }));
            }
            if items.iter().all(|v| matches!(v, Value::Record(_))) {
                let mut header: Vec<String> = Vec::new();
                for v in items.iter() {
                    if let Value::Record(f) = v {
                        for (k, _) in f {
                            if !header.contains(k) {
                                header.push(k.clone());
                            }
                        }
                    }
                }
                let rows = items
                    .iter()
                    .map(|v| match v {
                        Value::Record(f) => header
                            .iter()
                            .map(|k| {
                                f.iter()
                                    .find(|(n, _)| n == k)
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or(Value::Null)
                            })
                            .collect(),
                        _ => unreachable!("records"),
                    })
                    .collect();
                return Ok(Value::table(crate::Table {
                    header: Some(header),
                    rows,
                }));
            }
            if items.iter().all(|v| matches!(v, Value::List(_))) {
                let width = match &items[0] {
                    Value::List(l) => l.len(),
                    _ => unreachable!("lists"),
                };
                let mut rows = Vec::with_capacity(items.len());
                for (i, v) in items.iter().enumerate() {
                    let Value::List(l) = v else {
                        unreachable!("lists")
                    };
                    if l.len() > width {
                        return Err(format!(
                            "row {} has {} values, the table {width} columns",
                            i + 1,
                            l.len()
                        ));
                    }
                    let mut row = l.to_vec();
                    row.resize(width, Value::Null);
                    rows.push(row);
                }
                return Ok(Value::table(crate::Table { header: None, rows }));
            }
            if items
                .iter()
                .all(|v| !matches!(v, Value::List(_) | Value::Record(_) | Value::Table(_)))
            {
                return Ok(Value::table(crate::Table {
                    header: None,
                    rows: items.iter().map(|v| vec![v.clone()]).collect(),
                }));
            }
            Err("a table reads a list of records (a frame), a list of lists (an array) or a list of values (a sample); the rows here are of mixed kinds".into())
        }
        other => Err(format!(
            "a table reads a list of records, a list of lists or a record of lists, not {}",
            kind_of(&other)
        )),
    }
}

/// `header` and `dim` of a table: the list of names (null on the
/// array face) and `@(rows; columns)`; a list of records answers as
/// its table reading would.
pub(crate) fn table_meta(v: Value, what: &str) -> Result<Value, String> {
    let t = match table_reading(v)? {
        Value::Table(t) => t,
        other => return Err(format!("{} has no {what}", kind_of(&other))),
    };
    Ok(match what {
        "header" => t.header_value(),
        _ => t.shape(),
    })
}

/// A 1-based position, negative from the end, resolved against a
/// length; `None` out of range.
fn position(n: i64, len: usize) -> Option<usize> {
    let n = if n < 0 { len as i64 + 1 + n } else { n };
    (n >= 1 && n as usize <= len).then_some(n as usize)
}

/// A range's ends resolved against a length into `from..to` (0-based,
/// half-open), clamped; either end open.
fn span(a: Option<i64>, b: Option<i64>, len: usize) -> (usize, usize) {
    let l = len as i64;
    let from = a.map_or(1, |x| if x < 0 { l + 1 + x } else { x }).clamp(1, l + 1) - 1;
    let to = b.map_or(l, |x| if x < 0 { l + 1 + x } else { x }).clamp(0, l);
    if from < to {
        (from as usize, to as usize)
    } else {
        (0, 0)
    }
}

/// The index of a value (spec: Indexing a Value): on a list the item
/// or the sublist; on a table the row (as the table has it), the rows
/// as a table, or with a second index the cell, a column slice (a
/// list), a row slice (a record, keyed by the header or by position)
/// or a sub-table; a column by position or by name. A record has no
/// positional form, a string is not indexed.
pub(crate) fn index_value(
    v: Value,
    rows: &IndexSpec,
    cols: Option<&IndexSpec>,
) -> Result<Value, String> {
    match v {
        Value::List(items) => {
            if cols.is_some() {
                return Err("a list has one dimension: '[i]' or '[a..b]', not '[i; j]'".into());
            }
            match rows {
                IndexSpec::At(n) => Ok(position(*n, items.len())
                    .map(|p| items[p - 1].clone())
                    .unwrap_or(Value::Null)),
                IndexSpec::Range(a, b) => {
                    let (from, to) = span(*a, *b, items.len());
                    Ok(Value::list(items[from..to].to_vec()))
                }
                IndexSpec::Name(n) => Err(format!(
                    "a list is indexed by position, not by name ('{n}'); a record's field is ':{n}'"
                )),
            }
        }
        Value::Table(t) => {
            let width = t.width();
            let keys: Vec<String> = match &t.header {
                Some(h) => h.clone(),
                None => (1..=width).map(|j| j.to_string()).collect(),
            };
            let column = |spec: &IndexSpec| -> Result<(usize, usize), String> {
                match spec {
                    IndexSpec::At(j) => position(*j, width)
                        .map(|p| (p - 1, p))
                        .ok_or_else(|| format!("no column {j}: the table has {width}")),
                    IndexSpec::Range(a, b) => Ok(span(*a, *b, width)),
                    IndexSpec::Name(n) => match &t.header {
                        Some(h) => h
                            .iter()
                            .position(|c| c == n)
                            .map(|p| (p, p + 1))
                            .ok_or_else(|| format!("no column '{n}' (the header: {})", h.join(", "))),
                        None => Err(format!(
                            "'{n}' names no column: the table has no header, its columns are positions"
                        )),
                    },
                }
            };
            let (r0, r1) = match rows {
                IndexSpec::At(n) => match position(*n, t.rows.len()) {
                    Some(p) => (p - 1, p),
                    None => return Ok(Value::Null),
                },
                IndexSpec::Range(a, b) => span(*a, *b, t.rows.len()),
                IndexSpec::Name(n) => {
                    return Err(format!("rows are indexed by position, not by name ('{n}')"));
                }
            };
            let one_row = matches!(rows, IndexSpec::At(_));
            let Some(cols) = cols else {
                return Ok(if one_row {
                    t.row(r0 + 1).unwrap_or(Value::Null)
                } else {
                    Value::table(crate::Table {
                        header: t.header.clone(),
                        rows: t.rows[r0..r1].to_vec(),
                    })
                });
            };
            let (c0, c1) = column(cols)?;
            let one_col = !matches!(cols, IndexSpec::Range(..));
            let cell = |i: usize, j: usize| t.rows[i].get(j).cloned().unwrap_or(Value::Null);
            Ok(match (one_row, one_col) {
                (true, true) => cell(r0, c0),
                (false, true) => Value::list((r0..r1).map(|i| cell(i, c0)).collect()),
                (true, false) => Value::Record(
                    (c0..c1).map(|j| (keys[j].clone(), cell(r0, j))).collect(),
                ),
                (false, false) => Value::table(crate::Table {
                    header: t.header.as_ref().map(|h| h[c0..c1].to_vec()),
                    rows: (r0..r1).map(|i| (c0..c1).map(|j| cell(i, j)).collect()).collect(),
                }),
            })
        }
        // A row of a frame is a record: `t[2][1]` agrees with
        // `t[2; 1]`, so a record indexed by position gives the value
        // of its n-th field, by range the fields as a record.
        Value::Record(fields) => {
            if cols.is_some() {
                return Err("a record has one dimension: '[i]', not '[i; j]'".into());
            }
            match rows {
                IndexSpec::At(n) => Ok(position(*n, fields.len())
                    .map(|p| fields[p - 1].1.clone())
                    .unwrap_or(Value::Null)),
                IndexSpec::Range(a, b) => {
                    let (from, to) = span(*a, *b, fields.len());
                    Ok(Value::Record(fields[from..to].to_vec()))
                }
                IndexSpec::Name(n) => Ok(fields
                    .iter()
                    .find(|(k, _)| k == n)
                    .map(|(_, v)| v.clone())
                    .unwrap_or(Value::Null)),
            }
        }
        Value::Str(_) => Err("a string is not indexed: 'substr' and 'cc' read its characters".into()),
        other => Err(format!("{} has no index", kind_of(&other))),
    }
}

/// `| [n]` / `| [a..b]` on a table or a list value: the row or the
/// item, the rows or the sublist ([`index_value`]). On anything else,
/// the whole-context refusal positional selection has always given on
/// the plain pipe.
pub(crate) fn table_rows_at(call: &FnCall, v: Value) -> Result<Value, String> {
    if !matches!(v, Value::Table(_) | Value::List(_)) {
        return Err(
            "positional selection is whole-context; write '@| [n]' / '@| [a..b]' (a plain '| [cond]' filters per capsa; on a table or a list value, '| [n]' is the row or the item)".into(),
        );
    }
    let spec = match call.args.first() {
        Some(Arg::Lit(Value::Int(n))) => IndexSpec::At(*n),
        Some(Arg::Range(a, b)) => IndexSpec::Range(*a, *b),
        _ => return Err("'[n]' takes a position or a range".into()),
    };
    index_value(v, &spec, None)
}

/// `cell(i; j)` — one cell of a table by row position and column
/// position or name: the pipeline spelling of `t[i; j]`.
pub(crate) fn table_cell(call: &FnCall, v: Value) -> Result<Value, String> {
    let spec = |a: Option<&Arg>| -> Result<IndexSpec, String> {
        match a {
            Some(Arg::Lit(Value::Int(n))) => Ok(IndexSpec::At(*n)),
            Some(Arg::Range(a, b)) => Ok(IndexSpec::Range(*a, *b)),
            Some(Arg::Lit(Value::Str(s))) => Ok(IndexSpec::Name(s.clone())),
            _ => Err("cell(i; j) takes a row position and a column position or name".into()),
        }
    };
    let i = spec(call.args.first())?;
    let j = spec(call.args.get(1))?;
    if !matches!(v, Value::Table(_)) {
        return Err(format!("cell(i; j) reads a table, not {}", kind_of(&v)));
    }
    index_value(v, &i, Some(&j))
}

/// `row-to-header` — the first row up into the header (its values
/// as the names); `header-to-row` — the header down into the rows
/// (as text), the table then an array.
pub(crate) fn header_shift(call: &FnCall, v: Value) -> Result<Value, String> {
    let t = match table_reading(v)? {
        Value::Table(t) => t,
        other => return Err(format!("{} has no header to move", kind_of(&other))),
    };
    Ok(Value::table(match call.name.as_str() {
        "row-to-header" => {
            let Some(first) = t.rows.first() else {
                return Err("row-to-header: the table has no row to lift".into());
            };
            crate::Table {
                header: Some(first.iter().map(Value::to_string).collect()),
                rows: t.rows[1..].to_vec(),
            }
        }
        _ => {
            let mut rows = Vec::with_capacity(t.rows.len() + 1);
            if let Some(h) = &t.header {
                rows.push(h.iter().cloned().map(Value::Str).collect());
            }
            rows.extend(t.rows.iter().cloned());
            crate::Table { header: None, rows }
        }
    }))
}

/// The row reading (`| @`): a library value as its rows; a record
/// whose fields are all lists as one record per position, the
/// lists zipped; a list as itself.
pub(crate) fn rows_reading(v: Value) -> Result<Value, String> {
    match v {
        Value::Foreign(f) => f.rows(),
        Value::Table(t) => Ok(t.rows_value()),
        Value::Record(fields)
            if !fields.is_empty() && fields.iter().all(|(_, v)| matches!(v, Value::List(_))) =>
        {
            let len = |v: &Value| match v {
                Value::List(l) => l.len(),
                _ => 0,
            };
            let n = len(&fields[0].1);
            if let Some((k, v)) = fields.iter().find(|(_, v)| len(v) != n) {
                return Err(format!(
                    "field '{}' holds {n} values, '{k}' holds {}; the columns must be of one length",
                    fields[0].0,
                    len(v)
                ));
            }
            let rows = (0..n)
                .map(|i| {
                    Value::Record(
                        fields
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    match v {
                                        Value::List(l) => l[i].clone(),
                                        _ => Value::Null,
                                    },
                                )
                            })
                            .collect(),
                    )
                })
                .collect();
            Ok(Value::list(rows))
        }
        Value::Record(_) => Err(
            "a record reads as rows when every field is a list (one column each); this one's are not"
                .into(),
        ),
        other => Ok(other),
    }
}

/// The column reading (`| %`): a library value as its columns; a
/// list of records as one record of lists, a field per name in
/// first appearance, null where a record lacks it; a record as
/// itself.
pub(crate) fn columns_reading(v: Value) -> Result<Value, String> {
    match v {
        Value::Foreign(f) => f.columns(),
        Value::Table(t) => Ok(t.columns_value()),
        Value::List(items) => {
            if items.is_empty() {
                return Ok(Value::Record(Vec::new()));
            }
            if let Some(odd) = items.iter().find(|v| !matches!(v, Value::Record(_))) {
                return Err(format!(
                    "the rows have no field names ({} among them); a list of records reads by column, transpose exchanges the rows and columns of a list of lists",
                    kind_of(odd)
                ));
            }
            let mut names: Vec<&String> = Vec::new();
            for v in items.iter() {
                if let Value::Record(f) = v {
                    for (k, _) in f {
                        if !names.contains(&k) {
                            names.push(k);
                        }
                    }
                }
            }
            Ok(Value::Record(
                names
                    .into_iter()
                    .map(|k| {
                        let column = items
                            .iter()
                            .map(|v| match v {
                                Value::Record(f) => f
                                    .iter()
                                    .find(|(n, _)| n == k)
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or(Value::Null),
                                _ => Value::Null,
                            })
                            .collect();
                        (k.clone(), Value::list(column))
                    })
                    .collect(),
            ))
        }
        other => Ok(other),
    }
}

/// `transpose` — a list of lists with its rows and columns
/// exchanged; the rows must be of one length.
pub(crate) fn transpose_reading(v: Value) -> Result<Value, String> {
    match v {
        Value::Foreign(f) => Err(format!(
            "{} is a library value; np:transpose exchanges its axes",
            f.kind()
        )),
        // An array transposes to an array; a frame's columns are
        // named, so its exchange is the column reading.
        Value::Table(t) => match t.header {
            None => Ok(Value::table(t.transposed())),
            Some(_) => Err(format!(
                "{} names its columns; '| %' reads them, transpose takes an array",
                t.kind()
            )),
        },
        Value::List(items) => {
            if items.is_empty() {
                return Ok(Value::list(Vec::new()));
            }
            let mut rows: Vec<&[Value]> = Vec::with_capacity(items.len());
            for (i, v) in items.iter().enumerate() {
                match v {
                    Value::List(l) => rows.push(l.as_slice()),
                    other => {
                        return Err(format!(
                            "row {} is {}, not a list; transpose takes a list of lists ('| %' reads records by column)",
                            i + 1,
                            kind_of(other)
                        ));
                    }
                }
            }
            let n = rows[0].len();
            if let Some(i) = rows.iter().position(|r| r.len() != n) {
                return Err(format!(
                    "row 1 holds {n} values, row {} holds {}; the rows must be of one length",
                    i + 1,
                    rows[i].len()
                ));
            }
            Ok(Value::list(
                (0..n)
                    .map(|j| Value::list(rows.iter().map(|r| r[j].clone()).collect()))
                    .collect(),
            ))
        }
        other => Err(format!(
            "transpose takes a list of lists, not {}",
            kind_of(&other)
        )),
    }
}

/// The named fields of a call's bare-word operands.
fn word_args(call: &FnCall) -> Vec<&str> {
    let mut out = Vec::new();
    for a in &call.args {
        match a {
            Arg::Lit(Value::Str(s)) => out.push(s.as_str()),
            // A list constructor of names stands for the names
            // (`melt(@(year; life))`).
            Arg::Lit(Value::List(items)) => out.extend(items.iter().filter_map(|v| match v {
                Value::Str(s) => Some(s.as_str()),
                _ => None,
            })),
            _ => {}
        }
    }
    out
}

/// A declared option's literal text, when written.
fn opt_text(call: &FnCall, name: &str) -> Option<String> {
    call.args.iter().find_map(|a| match a {
        Arg::Opt { name: n, value } if n == name => Some(match value {
            Some(crate::ast::Operand::Lit(l)) => l.to_string(),
            Some(other) => crate::unparse::operand_text(other),
            None => String::new(),
        }),
        _ => None,
    })
}

fn field_of<'a>(fields: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// `pivot(row; column; value)` — long to wide over records: one
/// record per row key, in first appearance, carrying the row field
/// and one field per column key, in first appearance; a cell no
/// record fills is null. Without operands the first three fields
/// serve by position (row, column, value), as `--pairs` reads a
/// table of entries. Two records on one cell refuse: aggregating
/// is the group's work before the pivot.
fn pivot(call: &FnCall, input: Vec<Value>) -> Result<Vec<Value>, String> {
    let names = word_args(call);
    if !(names.is_empty() || names.len() == 3) {
        return Err(
            "pivot takes the row, column and value fields (pivot(row; column; value)), or no operand to read the first three fields by position"
                .into(),
        );
    }
    let mut row_name: Option<String> = None;
    let mut row_keys: Vec<(String, Value)> = Vec::new();
    let mut col_keys: Vec<String> = Vec::new();
    let mut cells: std::collections::HashMap<(usize, usize), Value> =
        std::collections::HashMap::new();
    for (i, v) in input.iter().enumerate() {
        let Value::Record(fields) = v else {
            return Err(format!(
                "pivot: value {} is {}, not a record",
                i + 1,
                kind_of(v)
            ));
        };
        let (rn, row, col, val) = if names.is_empty() {
            if fields.len() < 3 {
                return Err(format!(
                    "pivot: record {} has {} field(s); three are needed (row, column, value)",
                    i + 1,
                    fields.len()
                ));
            }
            (
                fields[0].0.as_str(),
                &fields[0].1,
                &fields[1].1,
                &fields[2].1,
            )
        } else {
            let get = |n: &str| {
                field_of(fields, n).ok_or_else(|| {
                    format!("pivot: record {} has no field '{n}'", i + 1)
                })
            };
            (names[0], get(names[0])?, get(names[1])?, get(names[2])?)
        };
        row_name.get_or_insert_with(|| rn.to_string());
        let rk = row.to_string();
        let r = match row_keys.iter().position(|(k, _)| *k == rk) {
            Some(r) => r,
            None => {
                row_keys.push((rk, row.clone()));
                row_keys.len() - 1
            }
        };
        let ck = col.to_string();
        let c = match col_keys.iter().position(|k| *k == ck) {
            Some(c) => c,
            None => {
                col_keys.push(ck);
                col_keys.len() - 1
            }
        };
        if let Some(prev) = cells.get(&(r, c)) {
            return Err(format!(
                "pivot: two values for row '{}', column '{}' ({} and {}); aggregate before the pivot",
                row_keys[r].0, col_keys[c], prev, val
            ));
        }
        cells.insert((r, c), val.clone());
    }
    let row_name = row_name.unwrap_or_else(|| "row".into());
    Ok(row_keys
        .iter()
        .enumerate()
        .map(|(r, (_, row))| {
            let mut rec = vec![(row_name.clone(), row.clone())];
            for (c, ck) in col_keys.iter().enumerate() {
                rec.push((
                    ck.clone(),
                    cells.get(&(r, c)).cloned().unwrap_or(Value::Null),
                ));
            }
            Value::Record(rec)
        })
        .collect())
}

/// `melt(id; …)` — wide to long over records: for every field of a
/// record that is not an id, one record of the ids, the field's
/// name and its value, under `name` and `value` (`--name=`,
/// `--value=` rename them).
fn melt(call: &FnCall, input: Vec<Value>) -> Result<Vec<Value>, String> {
    let ids = word_args(call);
    let name_field = opt_text(call, "--name").unwrap_or_else(|| "name".into());
    let value_field = opt_text(call, "--value").unwrap_or_else(|| "value".into());
    let mut out = Vec::new();
    for (i, v) in input.iter().enumerate() {
        let Value::Record(fields) = v else {
            return Err(format!(
                "melt: value {} is {}, not a record",
                i + 1,
                kind_of(v)
            ));
        };
        let head: Vec<(String, Value)> = ids
            .iter()
            .map(|id| {
                (
                    id.to_string(),
                    field_of(fields, id).cloned().unwrap_or(Value::Null),
                )
            })
            .collect();
        for (k, val) in fields {
            if ids.contains(&k.as_str()) {
                continue;
            }
            let mut rec = head.clone();
            rec.push((name_field.clone(), Value::Str(k.clone())));
            rec.push((value_field.clone(), val.clone()));
            out.push(Value::Record(rec));
        }
    }
    Ok(out)
}

fn arg_int(call: &FnCall, n: usize) -> Option<i64> {
    match call.args.get(n) {
        Some(Arg::Lit(Value::Int(i))) => Some(*i),
        Some(Arg::Lit(Value::Null)) | None => None,
        Some(other) => {
            crate::exec::record_refusal(format!(
                "{}: argument {} must be an integer, not {}",
                call.name,
                n + 1,
                arg_kind(other)
            ));
            None
        }
    }
}

/// The n-th argument as text, or `default` when there is none. An
/// argument that is there but is not text refuses the call —
/// `join(3)` is a mistake, not a join on "".
fn arg_str<'a>(call: &'a FnCall, n: usize, default: &'a str) -> &'a str {
    match call.args.get(n) {
        Some(Arg::Lit(Value::Str(s))) => s,
        Some(Arg::Lit(Value::Null)) | None => default,
        Some(other) => {
            crate::exec::record_refusal(format!(
                "{}: argument {} must be text, not {}",
                call.name,
                n + 1,
                arg_kind(other)
            ));
            default
        }
    }
}

/// What kind of argument this is, for a refusal.
fn arg_kind(a: &Arg) -> &'static str {
    match a {
        Arg::Lit(Value::Int(_)) => "a number",
        Arg::Lit(Value::Float(_)) => "a number",
        Arg::Lit(Value::Bool(_)) => "a boolean",
        Arg::Lit(Value::Str(_)) => "text",
        Arg::Lit(_) => "a value",
        Arg::Expr(_) => "an expression",
        Arg::Range(..) => "a range",
        Arg::Named { .. } | Arg::Opt { .. } => "an option",
    }
}

/// The numeric reading of the n-th literal argument (an expression
/// argument has been evaluated to a literal by the time the call
/// runs).
fn arg_num(call: &FnCall, n: usize) -> Option<f64> {
    match call.args.get(n) {
        Some(Arg::Lit(v)) => v.numeric(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_numbers_count_per_level() {
        let n = |ls: &[i64]| -> Vec<(usize, String)> {
            outline_numbers(&ls.iter().map(|&l| Some(l)).collect::<Vec<_>>())
        };
        let nums = |ls: &[i64]| n(ls).into_iter().map(|(_, s)| s).collect::<Vec<_>>();
        assert_eq!(
            nums(&[1, 2, 2, 3, 2, 1, 2]),
            ["1", "1.1", "1.2", "1.2.1", "1.3", "2", "2.1"]
        );
        // the shallowest level seen is the top, whatever its number
        assert_eq!(
            n(&[3, 4, 3]),
            [
                (0, "1".to_string()),
                (1, "1.1".to_string()),
                (0, "2".to_string())
            ]
        );
        // a skipped level counts as present once
        assert_eq!(nums(&[1, 3, 3, 2]), ["1", "1.1.1", "1.1.2", "1.2"]);
        // a row without a level stands at the top
        assert_eq!(
            outline_numbers(&[Some(1), None, Some(2)]),
            [
                (0, "1".to_string()),
                (0, "2".to_string()),
                (1, "2.1".to_string())
            ]
        );
    }

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

    /// `haversine` is the great-circle distance in kilometres, the
    /// first latitude in topic position as the call form puts it.
    #[test]
    fn haversine_in_kilometres() {
        let args = |v: &[f64]| {
            v.iter()
                .map(|x| Arg::Lit(Value::Float(*x)))
                .collect::<Vec<_>>()
        };
        let km = scalar_with(
            "haversine",
            Value::Float(40.71),
            args(&[-74.01, 48.86, 2.35]),
        );
        let Value::Float(d) = km[0] else {
            panic!("{km:?}")
        };
        assert!((d - 5837.4).abs() < 1.0, "{d}");
        let same = scalar_with(
            "haversine",
            Value::Float(31.78),
            args(&[35.23, 31.78, 35.23]),
        );
        assert_eq!(same, vec![Value::Float(0.0)]);
        assert_eq!(
            scalar_with("haversine", Value::Null, args(&[1.0, 2.0, 3.0])),
            vec![Value::Null]
        );
    }

    /// The unit words read a duration as a number of that unit —
    /// the constructor's inverse — whole as an integer, else a
    /// float; a numeric topic still makes a duration.
    #[test]
    fn unit_words_read_a_duration() {
        let d = |secs: i64| Value::Duration { secs, nanos: 0 };
        assert_eq!(
            scalar_with("days", d(30 * 86400), vec![]),
            vec![Value::Int(30)]
        );
        assert_eq!(
            scalar_with("hours", d(90 * 60), vec![]),
            vec![Value::Float(1.5)]
        );
        assert_eq!(
            scalar_with("minutes", d(90), vec![]),
            vec![Value::Float(1.5)]
        );
        assert_eq!(scalar_with("seconds", d(90), vec![]), vec![Value::Int(90)]);
        assert_eq!(
            scalar_with("days", Value::Int(30), vec![]),
            vec![d(30 * 86400)]
        );
    }

    /// `ngrams(n)`: the n-word windows of a text as space-joined
    /// strings, one per window; a text shorter than n yields none;
    /// the default n is 2.
    #[test]
    fn ngrams_windows_the_words() {
        let call = |n: Option<i64>| FnCall {
            name: "ngrams".into(),
            args: n.map(|n| vec![Arg::Lit(Value::Int(n))]).unwrap_or_default(),
        };
        let text = Value::Str("a  b c\nd".into());
        let strs = |vs: Vec<Value>| vs.into_iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(
            strs(apply_scalar(
                &call(Some(2)),
                text.clone(),
                &crate::quantity::scale_expr
            )),
            vec!["a b", "b c", "c d"]
        );
        assert_eq!(
            strs(apply_scalar(
                &call(Some(3)),
                text.clone(),
                &crate::quantity::scale_expr
            )),
            vec!["a b c", "b c d"]
        );
        assert_eq!(
            strs(apply_scalar(
                &call(None),
                text.clone(),
                &crate::quantity::scale_expr
            )),
            vec!["a b", "b c", "c d"]
        );
        assert!(apply_scalar(&call(Some(5)), text, &crate::quantity::scale_expr).is_empty());
    }

    /// The character classes keep their class (plus the named
    /// extras) and space out the rest, so `words` then spreads
    /// the kept runs; `digit` keeps characters, never a value.
    #[test]
    fn character_classes_space_out_the_rest() {
        let call = |name: &str, keep: Option<&str>| FnCall {
            name: name.into(),
            args: keep
                .map(|k| vec![Arg::Lit(Value::Str(k.into()))])
                .unwrap_or_default(),
        };
        let one = |name: &str, keep: Option<&str>, text: &str| {
            apply_scalar(
                &call(name, keep),
                Value::Str(text.into()),
                &crate::quantity::scale_expr,
            )
            .pop()
            .unwrap()
            .to_string()
        };
        assert_eq!(
            one("alpha", None, "Tom’s 2 dogs—a fence!"),
            "Tom s   dogs a fence "
        );
        assert_eq!(one("alpha", Some("’"), "Tom’s 2 dogs"), "Tom’s   dogs");
        assert_eq!(
            one("digit", None, "Hartford, 1876: $1.50"),
            "          1876   1 50"
        );
        assert_eq!(one("digit", Some("."), "$1.50 x 3,000"), " 1.50   3 000");
        assert_eq!(one("alnum", None, "No. 2—Huck’s"), "No  2 Huck s");
        assert_eq!(
            one("alnum", Some("’-"), "well-to-do Huck’s"),
            "well-to-do Huck’s"
        );
        // Unicode classes, not ASCII: letters and digits of any script.
        assert_eq!(one("alpha", None, "Пушкин 1799"), "Пушкин     ");
        assert_eq!(one("digit", None, "٣ apples"), "٣       ");
    }

    fn ints(ns: &[i64]) -> Vec<Value> {
        ns.iter().map(|&n| Value::Int(n)).collect()
    }

    fn scalar_with(name: &str, topic: Value, args: Vec<crate::ast::Arg>) -> Vec<Value> {
        let call = FnCall {
            name: name.into(),
            args,
        };
        apply_scalar(&call, topic, &crate::quantity::scale_expr)
    }

    /// The 0.25 text round: slice/pads/repeat/title, the one-sided
    /// trims, and the count family (cc codepoints, bc bytes, wc
    /// words, lc lines; `chars` the pre-0.25 alias of cc).
    #[test]
    fn text_round_commands() {
        use crate::ast::Arg;
        let s = |t: &str| Value::Str(t.to_string());
        let int = |n: i64| Arg::Lit(Value::Int(n));
        let lit = |t: &str| Arg::Lit(Value::Str(t.to_string()));
        assert_eq!(
            sc("title", s("emu WAR of 1932")),
            vec![s("Emu War Of 1932")]
        );
        assert_eq!(sc("ltrim", s("  x  ")), vec![s("x  ")]);
        assert_eq!(sc("rtrim", s("  x  ")), vec![s("  x")]);
        // counts: codepoints vs bytes vs words vs lines
        assert_eq!(sc("cc", s("льюис")), vec![Value::Int(5)]);
        assert_eq!(sc("cc", s("льюис")), vec![Value::Int(5)]);
        assert_eq!(sc("bc", s("льюис")), vec![Value::Int(10)]);
        assert_eq!(sc("wc", s("a b c")), vec![Value::Int(3)]);
        assert_eq!(sc("lc", s("a\nb\nc")), vec![Value::Int(3)]);
        // Plain UAX #29: terminators followed by space+uppercase
        // break; a mid-sentence number's dot does not. (The base
        // algorithm carries no abbreviation list — "Dr. Smith"
        // splits; that tailoring is locale data, documented as
        // out of scope.)
        let prose = s("He paid 3.50 today. Then he slept! Did he?");
        assert_eq!(
            sc("sentences", prose.clone()),
            vec![s("He paid 3.50 today."), s("Then he slept!"), s("Did he?"),]
        );
        assert_eq!(sc("sc", prose), vec![Value::Int(3)]);
        assert_eq!(
            sc("sc", s("Dr. Smith went home.")),
            vec![Value::Int(2)],
            "the untailored algorithm splits after the abbreviation"
        );
        // substr: the range predicate's spellings, on characters
        let range = |a: Option<i64>, b: Option<i64>| vec![Arg::Range(a, b)];
        assert_eq!(
            scalar_with("substr", s("abcdef"), range(Some(2), Some(4))),
            vec![s("bcd")]
        );
        assert_eq!(
            scalar_with("substr", s("abcdef"), range(None, Some(3))),
            vec![s("abc")]
        );
        assert_eq!(
            scalar_with("substr", s("abcdef"), range(Some(-2), None)),
            vec![s("ef")]
        );
        assert_eq!(
            scalar_with("substr", s("abcdef"), range(Some(4), Some(-4))),
            vec![s("")]
        );
        assert_eq!(
            scalar_with("substr", s("льюис"), range(None, Some(2))),
            vec![s("ль")]
        );
        // a bare position is the [n] reading: one character
        assert_eq!(
            scalar_with("substr", s("abcdef"), vec![int(3)]),
            vec![s("c")]
        );
        assert_eq!(
            scalar_with("substr", s("abcdef"), range(Some(5), Some(3))),
            vec![s("")]
        );
        // pads: to width, spaces by default, custom fill cut to fit
        assert_eq!(scalar_with("lpad", s("7"), vec![int(3)]), vec![s("  7")]);
        assert_eq!(
            scalar_with("lpad", s("7"), vec![int(3), lit("0")]),
            vec![s("007")]
        );
        assert_eq!(
            scalar_with("rpad", s("ab"), vec![int(5), lit("xy")]),
            vec![s("abxyx")]
        );
        assert_eq!(
            scalar_with("lpad", s("abcd"), vec![int(3)]),
            vec![s("abcd")]
        );
        // centered: the odd extra falls right (Python's center)
        assert_eq!(scalar_with("pad", s("ab"), vec![int(5)]), vec![s(" ab  ")]);
        assert_eq!(
            scalar_with("indent", s("ab"), vec![int(2)]),
            vec![s("    ab")]
        );
        assert_eq!(
            scalar_with("indent", s("ab"), vec![int(2), lit("\t")]),
            vec![s("\t\tab")]
        );
        assert_eq!(scalar_with("indent", s("ab"), vec![int(0)]), vec![s("ab")]);
        assert_eq!(scalar_with("indent", s("ab"), vec![int(-3)]), vec![s("ab")]);
        assert_eq!(
            scalar_with("pad", s("ab"), vec![int(6), lit("-")]),
            vec![s("--ab--")]
        );
        // repeat: the one home for repetition
        assert_eq!(
            scalar_with("repeat", s("ab"), vec![int(3)]),
            vec![s("ababab")]
        );
        assert_eq!(scalar_with("repeat", s("ab"), vec![int(0)]), vec![s("")]);
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
        assert_eq!(agg("stddev", data.clone()), vec![Value::Float(2.0)]);
        assert_eq!(agg("stddev", vec![]), vec![Value::Null]);
        // The compact spellings are the same measures.
        assert_eq!(agg("var", data.clone()), agg("variance", data.clone()));
        assert_eq!(agg("std", data.clone()), agg("stddev", data));
        assert!(known_agg("std") && known_agg("var"));
    }

    fn sample(name: &str) -> FnCall {
        FnCall {
            name: name.into(),
            args: vec![Arg::Lit(Value::Str("sample".into()))],
        }
    }

    #[test]
    fn sample_spread_uses_n_minus_one() {
        // 1,3,5,7: population variance 5, sample variance 20/3.
        let data = ints(&[1, 3, 5, 7]);
        assert_eq!(agg("variance", data.clone()), vec![Value::Float(5.0)]);
        let v = apply(&sample("variance"), data.clone(), &|_| None);
        assert_eq!(v, vec![Value::Float(20.0 / 3.0)]);
        let s = apply(&sample("stddev"), data, &|_| None);
        assert_eq!(s, vec![Value::Float((20.0f64 / 3.0).sqrt())]);
        // One value has a population spread of 0 and no sample spread.
        assert_eq!(agg("variance", ints(&[4])), vec![Value::Float(0.0)]);
        assert_eq!(
            apply(&sample("variance"), ints(&[4]), &|_| None),
            vec![Value::Null]
        );
        assert!(sample_flag(&sample("stddev")));
        assert!(!sample_flag(&FnCall {
            name: "stddev".into(),
            args: Vec::new(),
        }));
    }

    #[test]
    fn diversity_aggregates() {
        // Two equal counts: one bit; a single count: no entropy.
        assert_eq!(agg("entropy", ints(&[1, 1])), vec![Value::Float(1.0)]);
        assert_eq!(agg("entropy", ints(&[4])), vec![Value::Float(-0.0)]);
        assert_eq!(agg("entropy", ints(&[1, 1, 1, 1])), vec![Value::Float(2.0)]);
        assert_eq!(agg("entropy", vec![]), vec![Value::Null]);
        // Yule's K over counts 2,1,1: N = 4, Σc² = 6.
        assert_eq!(
            agg("yule_k", ints(&[2, 1, 1])),
            vec![Value::Float(10_000.0 * 2.0 / 16.0)]
        );
        assert_eq!(agg("yule_k", vec![]), vec![Value::Null]);
        // MTLD: with t = 0.5, `a a b b c c` — forward: a(1) a(.5 → factor,
        // reset) b(1) b(.5 → factor) c(1) c(.5 → factor): 3 factors, no
        // remainder; backward the same; 6 / 3 = 2.
        let toks: Vec<Value> = ["a", "a", "b", "b", "c", "c"]
            .iter()
            .map(|s| Value::Str(s.to_string()))
            .collect();
        let half = FnCall {
            name: "mtld".into(),
            args: vec![Arg::Lit(Value::Float(0.5))],
        };
        assert_eq!(
            apply(&half, toks.clone(), &|_| None),
            vec![Value::Float(2.0)]
        );
        // All distinct: the ratio never falls to t, so the partial
        // remainder is the only factor — (1 − 1) / (1 − t) = 0 → null.
        let distinct: Vec<Value> = ["a", "b", "c"]
            .iter()
            .map(|s| Value::Str(s.to_string()))
            .collect();
        assert_eq!(apply(&half, distinct, &|_| None), vec![Value::Null]);
        assert_eq!(agg("mtld", vec![]), vec![Value::Null]);
        // A threshold outside (0, 1) is refused.
        let bad = FnCall {
            name: "mtld".into(),
            args: vec![Arg::Lit(Value::Int(2))],
        };
        assert_eq!(apply(&bad, toks, &|_| None), vec![Value::Null]);
    }

    #[test]
    fn collocation_measures() {
        // Twenty pairs of a 100-word x and a 200-word y in a million
        // tokens: expected 0.02, so MI = log2(1000) and the t-score
        // is (20 − 0.02) / √20.
        assert_eq!(mi(20.0, 100.0, 200.0, 1e6), Value::Float(1000f64.log2()));
        assert_eq!(
            t_score(20.0, 100.0, 200.0, 1e6),
            Value::Float((20.0 - 0.02) / 20f64.sqrt())
        );
        // logDice caps at 14 when the pair is both words entirely.
        assert_eq!(log_dice(5.0, 5.0, 5.0), Value::Float(14.0));
        assert_eq!(
            log_dice(1.0, 5.0, 3.0),
            Value::Float(14.0 + (2.0 / 8.0f64).log2())
        );
        assert_eq!(mi(0.0, 1.0, 1.0, 10.0), Value::Null);
        assert_eq!(mi(3.0, 2.0, 5.0, 10.0), Value::Null);
        assert_eq!(log_dice(1.0, 0.0, 3.0), Value::Null);
    }

    #[test]
    fn distance_and_dispersion() {
        assert_eq!(
            associate("delta", &[(1.0, 0.0), (0.0, 2.0)]),
            Value::Float(1.5)
        );
        // Two equal parts: a word all in one part has DP 0.5, an
        // even split 0.
        assert_eq!(
            associate("dp", &[(4.0, 10.0), (0.0, 10.0)]),
            Value::Float(0.5)
        );
        assert_eq!(
            associate("dp", &[(2.0, 10.0), (2.0, 10.0)]),
            Value::Float(0.0)
        );
        assert_eq!(associate("dp", &[(0.0, 10.0), (0.0, 10.0)]), Value::Null);
    }

    #[test]
    fn agreement_measures() {
        let s = |x: &str| Value::Str(x.to_string());
        let pairs = vec![
            (s("a"), s("a")),
            (s("a"), s("b")),
            (s("b"), s("b")),
            (s("b"), s("b")),
        ];
        // observed 0.75; expected 0.5·0.25 + 0.5·0.75 = 0.5 → κ = 0.5
        assert_eq!(agree("kappa", &pairs), Value::Float(0.5));
        assert_eq!(agree("kappa", &[(s("a"), s("a"))]), Value::Null);
        let b = |x: bool| Value::Bool(x);
        let pairs = vec![
            (b(true), b(true)),
            (b(true), b(false)),
            (b(false), b(true)),
            (b(false), b(false)),
        ];
        assert_eq!(agree("precision", &pairs), Value::Float(0.5));
        assert_eq!(agree("recall", &pairs), Value::Float(0.5));
        assert_eq!(agree("f1", &pairs), Value::Float(0.5));
        assert_eq!(agree("precision", &[(b(false), b(false))]), Value::Null);
        assert_eq!(agree("f1", &[]), Value::Null);
    }

    #[test]
    fn association_pairs() {
        let line = [(1.0, 2.0), (2.0, 4.0), (3.0, 6.0)];
        assert_eq!(associate("corr", &line), Value::Float(1.0));
        let anti = [(1.0, 3.0), (2.0, 2.0), (3.0, 1.0)];
        assert_eq!(associate("corr", &anti), Value::Float(-1.0));
        // Spearman on a monotone but non-linear relation is 1; ties
        // take the average rank.
        let curve = [(1.0, 1.0), (2.0, 10.0), (3.0, 100.0)];
        assert_eq!(associate("spearman", &curve), Value::Float(1.0));
        assert_eq!(
            average_ranks([5.0, 1.0, 5.0].into_iter()),
            vec![2.5, 1.0, 2.5]
        );
        // Orthogonal vectors have cosine 0; parallel ones 1.
        let orth = [(1.0, 0.0), (0.0, 1.0)];
        assert_eq!(associate("cosine", &orth), Value::Float(0.0));
        assert_eq!(associate("cosine", &line), Value::Float(1.0));
        // Degenerate: one pair, or no spread.
        assert_eq!(associate("corr", &[(1.0, 2.0)]), Value::Null);
        assert_eq!(associate("corr", &[(1.0, 2.0), (1.0, 3.0)]), Value::Null);
    }

    #[test]
    fn standard_scores() {
        // 2, 4, 6: mean 4, population sd √(8/3).
        let z = zscores(&[Some(2.0), Some(4.0), Some(6.0), None], false);
        let sd = (8.0f64 / 3.0).sqrt();
        assert_eq!(
            z,
            vec![
                Value::Float(-2.0 / sd),
                Value::Float(0.0),
                Value::Float(2.0 / sd),
                Value::Null
            ]
        );
        // Sample sd is 2.
        let z = zscores(&[Some(2.0), Some(4.0), Some(6.0)], true);
        assert_eq!(
            z,
            vec![Value::Float(-1.0), Value::Float(0.0), Value::Float(1.0)]
        );
        // No spread, or a single reading: every score null.
        assert_eq!(
            zscores(&[Some(3.0), Some(3.0)], false),
            vec![Value::Null; 2]
        );
        assert_eq!(zscores(&[Some(3.0)], false), vec![Value::Null]);
        assert!(
            crate::registry::spec("zscore")
                .is_some_and(|s| s.agg == Some(crate::registry::Agg::Context))
                && association("spearman")
        );
    }

    #[test]
    fn math_scalars() {
        let c = |name: &str, args: Vec<Arg>| FnCall {
            name: name.into(),
            args,
        };
        let run = |call: &FnCall, topic: Value| apply_scalar(call, topic, &|_| None);
        assert_eq!(
            run(&c("sqrt", vec![]), Value::Int(16)),
            vec![Value::Float(4.0)]
        );
        assert_eq!(run(&c("sqrt", vec![]), Value::Int(-1)), vec![Value::Null]);
        assert_eq!(
            run(&c("log2", vec![]), Value::Int(8)),
            vec![Value::Float(3.0)]
        );
        assert_eq!(
            run(&c("log10", vec![]), Value::Int(1000)),
            vec![Value::Float(3.0)]
        );
        assert_eq!(run(&c("log", vec![]), Value::Int(0)), vec![Value::Null]);
        assert_eq!(
            run(&c("log", vec![Arg::Lit(Value::Int(2))]), Value::Int(8)),
            vec![Value::Float(3.0)]
        );
        assert_eq!(
            run(&c("log", vec![Arg::Lit(Value::Int(1))]), Value::Int(8)),
            vec![Value::Null]
        );
        assert_eq!(
            run(&c("exp", vec![]), Value::Int(0)),
            vec![Value::Float(1.0)]
        );
        assert_eq!(run(&c("exp", vec![]), Value::Int(1000)), vec![Value::Null]);
        // pow: integers stay integers while they fit.
        assert_eq!(
            run(&c("pow", vec![Arg::Lit(Value::Int(10))]), Value::Int(2)),
            vec![Value::Int(1024)]
        );
        assert_eq!(
            run(&c("pow", vec![Arg::Lit(Value::Int(100))]), Value::Int(10)),
            vec![Value::Float(1e100)]
        );
        assert_eq!(
            run(&c("pow", vec![Arg::Lit(Value::Float(0.5))]), Value::Int(9)),
            vec![Value::Float(3.0)]
        );
        assert_eq!(run(&c("pow", vec![]), Value::Int(9)), vec![Value::Null]);
        assert_eq!(
            run(
                &c("sqrt", vec![]),
                Value::Quantity {
                    value: 4.0,
                    base: "m".into(),
                    written: None,
                }
            ),
            vec![Value::Null]
        );
    }

    #[test]
    fn keyness_scalars() {
        let c = |name: &str, args: &[i64]| FnCall {
            name: name.into(),
            args: args.iter().map(|&n| Arg::Lit(Value::Int(n))).collect(),
        };
        let run = |call: &FnCall, a: i64| apply_scalar(call, Value::Int(a), &|_| None);
        // 10 of 1000 against 2 of 1000: over-represented in the first.
        let g = run(&c("loglik", &[1000, 2, 1000]), 10);
        let Value::Float(g) = g[0] else {
            panic!("{g:?}")
        };
        assert!(g > 0.0);
        // 2·[10·ln(10/6) + 2·ln(2/6)] = 5.822.
        assert!((g - 5.822).abs() < 0.001, "{g}");
        // Swapping the corpora negates the sign, same magnitude.
        let h = run(&c("loglik", &[1000, 10, 1000]), 2);
        assert_eq!(h, vec![Value::Float(-g)]);
        // A word absent from both, or a count above its total: null.
        assert_eq!(run(&c("loglik", &[1000, 0, 1000]), 0), vec![Value::Null]);
        assert_eq!(run(&c("loglik", &[5, 2, 1000]), 10), vec![Value::Null]);
        // χ² on [[10, 990], [2, 998]] is 5.37 (no correction).
        let x = run(&c("chi2", &[1000, 2, 1000]), 10);
        let Value::Float(x) = x[0] else {
            panic!("{x:?}")
        };
        assert!((x - 5.37).abs() < 0.01, "{x}");
        assert_eq!(run(&c("chi2", &[1000, 0, 1000]), 0), vec![Value::Null]);
        // Too few arguments: refused (null).
        assert_eq!(run(&c("loglik", &[1000]), 10), vec![Value::Null]);
        assert!(known_scalar("loglik") && known_agg("entropy") && known_agg("corr"));
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
        assert_eq!(apply1("base64", "Odyssey"), "T2R5c3NleQ==");
        assert_eq!(apply1("base64url", "Odyssey"), "T2R5c3NleQ");
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

    fn sc_args(name: &str, args: Vec<Arg>, topic: Value) -> Vec<Value> {
        let call = FnCall {
            name: name.into(),
            args,
        };
        apply_scalar(&call, topic, &crate::quantity::scale_expr)
    }

    #[test]
    fn round_with_digits() {
        let d = |n: i64| vec![Arg::Lit(Value::Int(n))];
        // The motivating case: arithmetic noise prints clean.
        assert_eq!(
            sc_args("round", d(2), Value::Float(68.99000000000001)),
            vec![Value::Float(68.99)]
        );
        assert_eq!(
            sc_args("round", d(2), Value::Float(3.5700000000000003)),
            vec![Value::Float(3.57)]
        );
        // The argument form always answers float, `round(0)` included.
        assert_eq!(
            sc_args("round", d(0), Value::Float(2.5)),
            vec![Value::Float(3.0)]
        );
        // Negative digits reach left of the point, SQL-style.
        assert_eq!(
            sc_args("round", d(-1), Value::Float(2568.5)),
            vec![Value::Float(2570.0)]
        );
        // Numeric reading still applies; no reading is still null.
        assert_eq!(
            sc_args("round", d(1), Value::Str("2.44".into())),
            vec![Value::Float(2.4)]
        );
        assert_eq!(
            sc_args("round", d(2), Value::Str("n/a".into())),
            vec![Value::Null]
        );
        // A quantity rounds in its display unit and stays one.
        let q = Value::Quantity {
            value: 3.14159,
            base: "m".into(),
            written: Some((3.14159, "m".into())),
        };
        assert_eq!(
            sc_args("round", d(2), q),
            vec![Value::Quantity {
                value: 3.14,
                base: "m".into(),
                written: Some((3.14, "m".into())),
            }]
        );
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
        assert_eq!(
            agg("product", vec![Value::Str("x".into())]),
            vec![Value::Null]
        );
        // one quantity scales; two are refused, never answered bare
        let km = |v: f64| Value::Quantity {
            value: v * 1000.0,
            base: "m".into(),
            written: Some((v, "km".into())),
        };
        assert_eq!(agg("product", vec![km(2.0), Value::Int(3)]), vec![km(6.0)]);
        assert_eq!(agg("product", vec![km(2.0), km(3.0)]), vec![Value::Null]);
        assert!(crate::exec::take_refusal().unwrap().contains("units"));
    }

    #[test]
    fn rounding_past_the_integers_stays_a_float() {
        let f = |name: &str, v: Value| {
            let call = FnCall {
                name: name.into(),
                args: vec![],
            };
            apply_scalar(&call, v, &|_| None).remove(0)
        };
        assert_eq!(f("round", Value::Float(2.5)), Value::Int(3));
        assert_eq!(f("round", Value::Float(1e30)), Value::Float(1e30));
        assert_eq!(f("floor", Value::Float(-1e30)), Value::Float(-1e30));
        assert_eq!(f("ceil", Value::Float(1e19)), Value::Float(1e19));
        assert_eq!(f("abs", Value::Int(-7)), Value::Int(7));
        assert_eq!(
            f("abs", Value::Int(i64::MIN)),
            Value::Float(9_223_372_036_854_775_808.0)
        );
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

#[cfg(test)]
mod distance_and_quantile_tests {
    use super::*;

    fn call(name: &str, args: Vec<Arg>) -> FnCall {
        FnCall {
            name: name.into(),
            args,
        }
    }

    fn ints(ns: &[i64]) -> Vec<Value> {
        ns.iter().map(|&n| Value::Int(n)).collect()
    }

    #[test]
    fn levenshtein_counts_codepoint_edits() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("same", "same"), 0);
        // Codepoints, not bytes.
        assert_eq!(levenshtein("льюис", "льюиc"), 1);
        let c = call("levenshtein", vec![Arg::Lit(Value::Str("colour".into()))]);
        assert_eq!(
            apply_scalar(&c, Value::Str("color".into()), &|_| None),
            vec![Value::Int(1)]
        );
        let null = call("levenshtein", vec![Arg::Lit(Value::Null)]);
        assert_eq!(
            apply_scalar(&null, Value::Str("color".into()), &|_| None),
            vec![Value::Null]
        );
    }

    #[test]
    fn percentile_interpolates_between_the_order_statistics() {
        let p = |n: i64, input: Vec<Value>| {
            apply(
                &call("percentile", vec![Arg::Lit(Value::Int(n))]),
                input,
                &|_| None,
            )
        };
        let ten: Vec<Value> = ints(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(p(90, ten.clone()), vec![Value::Float(9.1)]);
        assert_eq!(p(50, ten.clone()), vec![Value::Float(5.5)]);
        assert_eq!(p(0, ten.clone()), vec![Value::Int(1)]);
        assert_eq!(p(100, ten.clone()), vec![Value::Int(10)]);
        // The median's own results, where the rank lands on a value.
        assert_eq!(p(50, ints(&[5, 1, 3])), vec![Value::Int(3)]);
        assert_eq!(p(50, ints(&[4, 1, 3, 2])), vec![Value::Float(2.5)]);
        assert_eq!(p(50, ints(&[7])), vec![Value::Int(7)]);
        assert_eq!(p(50, vec![]), vec![Value::Null]);
        // A fractional p.
        let q = apply(
            &call("percentile", vec![Arg::Lit(Value::Float(25.5))]),
            ten.clone(),
            &|_| None,
        );
        assert_eq!(q, vec![Value::Float(3.295)]);
        // Out of range or missing: refused (null).
        assert_eq!(p(101, ten.clone()), vec![Value::Null]);
        assert_eq!(
            apply(&call("percentile", vec![]), ten, &|_| None),
            vec![Value::Null]
        );
        assert!(known_agg("percentile") && known_scalar("levenshtein"));
    }
}

#[cfg(test)]
mod atergo_and_stress_tests {
    use super::*;

    fn call(name: &str, args: Vec<Arg>) -> FnCall {
        FnCall {
            name: name.into(),
            args,
        }
    }
    fn word(s: &str) -> Arg {
        Arg::Lit(Value::Str(s.into()))
    }
    fn strs(ws: &[&str]) -> Vec<Value> {
        ws.iter().map(|w| Value::Str((*w).into())).collect()
    }
    fn texts(vs: Vec<Value>) -> Vec<String> {
        vs.into_iter().map(|v| v.to_string()).collect()
    }
    fn scalar(name: &str, args: Vec<Arg>, topic: &str) -> Vec<Value> {
        apply_scalar(&call(name, args), Value::Str(topic.into()), &|_| None)
    }
    fn parses(q: &str) -> bool {
        crate::lexer::lex(q)
            .and_then(|t| crate::parser::parse(&t))
            .is_ok()
    }

    #[test]
    fn atergo_compares_from_the_end() {
        let words = ["дом", "стол", "ёж", "еда", "нож"];
        let got = texts(apply(
            &call("sort", vec![word("atergo")]),
            strs(&words),
            &crate::quantity::scale_expr,
        ));
        // the reversed-code-point rule, spelled out
        let mut want: Vec<&str> = words.to_vec();
        want.sort_by(|a, b| a.chars().rev().cmp(b.chars().rev()));
        assert_eq!(got, want);
        // concretely: …а, then …ож before …ёж (о < ё in code points),
        // then …л, then …м
        assert_eq!(got, ["еда", "нож", "ёж", "стол", "дом"]);
        // a suffix sorts before what it ends: ties fall forward
        let got = texts(apply(
            &call("sort", vec![word("atergo")]),
            strs(&["стол", "ол", "л"]),
            &crate::quantity::scale_expr,
        ));
        assert_eq!(got, ["л", "ол", "стол"]);
    }

    #[test]
    #[cfg(feature = "colligo")]
    fn atergo_under_a_locale() {
        let words = ["ёж", "еж", "нож", "стол", "дом", "Öl"];
        let got = texts(apply(
            &call("sort", vec![word("ru-RU"), word("atergo")]),
            strs(&words),
            &crate::quantity::scale_expr,
        ));
        // Russian: ё follows е, so …же < …жё < …жо; Cyrillic before
        // Latin under the locale's reorder, from the end as well.
        assert_eq!(got, ["еж", "ёж", "нож", "стол", "дом", "Öl"]);
        // A tailoring with no contractions mirrors exactly: the
        // locale a tergo order is the locale order of the reversed
        // texts.
        let coll = colligo::Collator::builder("ru-RU")
            .allow_approximate(true)
            .build()
            .unwrap();
        let rev = |s: &str| s.chars().rev().collect::<String>();
        for a in words {
            for b in words {
                assert_eq!(
                    coll.compare_reverse(a, b),
                    coll.compare(&rev(a), &rev(b)),
                    "{a} {b}"
                );
            }
        }
    }

    #[test]
    fn sort_argument_shapes() {
        assert!(parses("/w @| sort"));
        assert!(parses("/w @| sort(ru-RU)"));
        assert!(parses("/w @| sort(atergo)"));
        assert!(parses("/w @| sort(ru-RU; atergo)"));
        assert!(parses("/w | sort(ru-RU, atergo)"));
        // the mode follows the locale, and there is one locale
        assert!(!parses("/w @| sort(atergo; ru-RU)"));
        assert!(!parses("/w @| sort(ru-RU; sv-SE)"));
        assert!(!parses("/w @| sort(atergo; atergo)"));
        assert!(!parses("/w @| sort(nonsense!)"));
    }

    #[test]
    fn stress_places_the_acute_after_the_nth_vowel() {
        let s = |n: i64, w: &str| scalar("stress", vec![Arg::Lit(Value::Int(n))], w);
        let ok = |w: &str| vec![Value::Str(w.into())];
        assert_eq!(s(3, "молоко"), ok("молоко\u{301}"));
        assert_eq!(s(-1, "молоко"), ok("молоко\u{301}"));
        assert_eq!(s(1, "молоко"), ok("мо\u{301}локо"));
        assert_eq!(s(-3, "молоко"), ok("мо\u{301}локо"));
        assert_eq!(s(4, "молоко"), vec![Value::Null]);
        assert_eq!(s(-4, "молоко"), vec![Value::Null]);
        assert_eq!(s(0, "молоко"), vec![Value::Null]);
        // ё carries its stress and is never marked; it still counts
        assert_eq!(s(1, "Ёлка"), ok("Ёлка"));
        assert_eq!(s(2, "Ёлка"), ok("Ёлка\u{301}"));
        // idempotent on a vowel already marked
        assert_eq!(s(3, "молоко\u{301}"), ok("молоко\u{301}"));
        // the Latin vowels are in the default alphabet
        assert_eq!(s(-1, "kefir"), ok("kefi\u{301}r"));
        // a named alphabet
        assert_eq!(
            scalar("stress", vec![Arg::Lit(Value::Int(1)), word("ы")], "сыр"),
            ok("сы\u{301}р")
        );
        // a position that arrives as text (the expression path)
        assert_eq!(scalar("stress", vec![word("2")], "вода"), ok("вода\u{301}"));
        // no position refuses
        assert_eq!(scalar("stress", vec![], "вода"), vec![Value::Null]);
        // a decomposed input is composed first: й is one letter
        assert_eq!(s(-1, "и\u{306}од"), ok("йо\u{301}д"));
        assert!(known_scalar("stress") && known_scalar("vowels"));
    }

    #[test]
    fn stress_family_reads_and_displays() {
        let one = |name: &str, args: Vec<Arg>, w: &str| scalar(name, args, w);
        assert_eq!(
            one("stress_at", vec![], "молоко\u{301}"),
            vec![Value::Int(3)]
        );
        assert_eq!(
            one("stress_at", vec![], "за\u{301}мок"),
            vec![Value::Int(1)]
        );
        assert_eq!(one("stress_at", vec![], "ёлка"), vec![Value::Int(1)]);
        assert_eq!(one("stress_at", vec![], "дом"), vec![Value::Int(1)]);
        assert_eq!(one("stress_at", vec![], "молоко"), vec![Value::Null]);
        assert_eq!(
            one("unstress", vec![], "молоко\u{301}"),
            vec![Value::Str("молоко".into())]
        );
        assert_eq!(
            one("unstress", vec![], "обою\u{300}доо\u{301}стрый"),
            vec![Value::Str("обоюдоострый".into())]
        );
        assert_eq!(
            one("accent", vec![word("upper")], "замо\u{301}к"),
            vec![Value::Str("замОк".into())]
        );
        assert_eq!(
            one("accent", vec![word("apostrophe")], "замо\u{301}к"),
            vec![Value::Str("замо'к".into())]
        );
        assert_eq!(
            one("accent", vec![word("acute")], "замОк"),
            vec![Value::Str("замОк".into())]
        );
        assert_eq!(
            one("accent", vec![word("none")], "замо\u{301}к"),
            vec![Value::Str("замок".into())]
        );
        assert_eq!(
            one("accent", vec![word("upper")], "ёж"),
            vec![Value::Str("Ёж".into())]
        );
        assert_eq!(
            one("accent", vec![word("apostrophe")], "ёж"),
            vec![Value::Str("ёж".into())]
        );
        assert_eq!(
            one("accent", vec![word("bogus")], "замо\u{301}к"),
            vec![Value::Null]
        );
        // widths by grapheme: the acute takes no cell
        assert_eq!(one("gc", vec![], "замо\u{301}к"), vec![Value::Int(5)]);
        assert_eq!(one("cc", vec![], "замо\u{301}к"), vec![Value::Int(6)]);
        assert_eq!(
            one("rpad", vec![Arg::Lit(Value::Int(7))], "замо\u{301}к"),
            vec![Value::Str("замо\u{301}к  ".into())]
        );
        assert_eq!(
            one("translit", vec![word("scholarly")], "щу\u{301}ка"),
            vec![Value::Str("šču\u{301}ka".into())]
        );
        assert_eq!(
            one("translit", vec![], "Москва"),
            vec![Value::Str("Moskva".into())]
        );
        assert!(known_scalar("translit") && known_scalar("accent") && known_scalar("gc"));
    }

    #[test]
    fn vowels_counts_the_letters() {
        let v = |w: &str| scalar("vowels", vec![], w);
        assert_eq!(v("молоко"), vec![Value::Int(3)]);
        assert_eq!(v("стрх"), vec![Value::Int(0)]);
        assert_eq!(v("Ёж"), vec![Value::Int(1)]);
        assert_eq!(v("kefir"), vec![Value::Int(2)]);
        assert_eq!(
            scalar("vowels", vec![word("ы")], "сыры"),
            vec![Value::Int(2)]
        );
    }

    #[test]
    fn normalization_forms() {
        let nfd = |w: &str| scalar("nfd", vec![], w);
        let nfc = |w: &str| scalar("nfc", vec![], w);
        assert_eq!(nfd("й"), vec![Value::Str("и\u{306}".into())]);
        assert_eq!(nfc("и\u{306}"), vec![Value::Str("й".into())]);
        assert_eq!(nfd("ёж"), vec![Value::Str("е\u{308}ж".into())]);
        // no precomposed Cyrillic vowel with an acute: two code
        // points under either form
        assert_eq!(nfc("о\u{301}")[0].to_string().chars().count(), 2);
        assert_eq!(nfd("о\u{301}")[0].to_string().chars().count(), 2);
        assert_eq!(
            apply_scalar(&call("nfc", vec![]), Value::Null, &|_| None),
            vec![Value::Null]
        );
        assert!(known_scalar("nfc") && known_scalar("nfd"));
    }
}
