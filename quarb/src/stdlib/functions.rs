//! The pipeline standard library.
//!
//! Functions come in two families, matching the two control operators:
//!
//! - **Scalar** commands (`|`) transform one capsa's topic; some
//!   expand it into several values (e.g. `lines`).
//! - **Aggregate** commands (`@|`) reduce the whole context's topics
//!   to a new list.

use crate::syntax::ast::{Arg, ConcatMode, FnCall, IndexSpec};
#[cfg(test)]
use crate::syntax::registry::{known_agg, known_keyed, known_scalar};
use crate::value::Value;
use std::cmp::Ordering;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// The unit-expression resolver threaded from the executor (the
/// adapter's `unit_scale`), so aggregates read custom units exactly
/// as criteria and ordering do.
type Scale<'a> = &'a dyn Fn(&str) -> Option<(f64, String)>;
use std::ops::Deref;

/// The internal aggregate standing in for a closing `@| csv` /
/// `@| pretty` while a tool renders it (see `run_for_rendering`).
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
    grid(values, named, csv, delimiter, cell, None)
}

/// The digits after the point `pretty` draws a float with: six, or
/// what `-p=n` / `--precision=n` says.
pub fn pretty_precision(call: &FnCall) -> usize {
    call.args
        .iter()
        .find_map(|a| match a {
            Arg::Opt {
                name,
                value: Some(crate::syntax::ast::Operand::Lit(Value::Int(n))),
            } if name == "--precision" || name == "-p" => Some((*n).max(0) as usize),
            _ => None,
        })
        .unwrap_or_else(|| {
            // the session's digits (`session(--precision=n)`, draft 166), else six
            crate::pipeline::run_option(|o| o.session.as_ref().map(|s| s.borrow().precision))
                .unwrap_or(6)
        })
}

/// `@| pretty` / `| pretty` as the grid (spec: Output Commands, draft
/// 166): [`tabulate_in`] with a float drawn with at most `precision`
/// digits after the point, trailing zeros dropped, and a numeric
/// column right-aligned on its decimal point; a text column stays
/// left-aligned. The rounding is the drawing's alone.
pub fn tabulate_pretty(
    values: &[Value],
    named: Option<&[String]>,
    delimiter: u8,
    cell: &dyn Fn(&Value) -> String,
    precision: usize,
) -> String {
    grid(values, named, false, delimiter, cell, Some(precision))
}

fn grid(
    values: &[Value],
    named: Option<&[String]>,
    csv: bool,
    delimiter: u8,
    cell: &dyn Fn(&Value) -> String,
    pretty: Option<usize>,
) -> String {
    let columns = crate::value::table::table_columns(values, named);
    // Under `pretty` a float is written to the digits, in the
    // decimal mark the cell renderer writes (an output locale's
    // `98765,5`; the point otherwise), so the command and the flag
    // render alike (spec: Output Commands).
    let mark: Option<String> = pretty.map(|_| {
        let half = cell(&Value::Float(0.5));
        half.strip_prefix('0')
            .and_then(|r| r.strip_suffix('5'))
            .filter(|m| !m.is_empty() && *m != ".")
            .map(str::to_string)
            .unwrap_or_default()
    });
    let draw = |v: &Value| -> String {
        match (v, pretty) {
            (Value::Float(f), Some(p)) => {
                let text = crate::value::pretty_float(*f, p);
                match mark.as_deref() {
                    Some(m) if !m.is_empty() => text.replacen('.', m, 1),
                    _ => text,
                }
            }
            _ => cell(v),
        }
    };
    let field_of = |v: &Value, c: &str| -> Option<Value> {
        match v {
            Value::Record(fields) => fields.iter().find(|(k, _)| k == c).map(|(_, v)| v.clone()),
            other if c == "value" => Some(other.clone()),
            _ => None,
        }
    };
    let rows: Vec<Vec<String>> = values
        .iter()
        .map(|v| {
            columns
                .iter()
                .map(|c| field_of(v, c).map(|x| draw(&x)).unwrap_or_default())
                .collect()
        })
        .collect();
    // under `pretty` a column whose every present value is a number
    // is right-aligned on its decimal point
    let numeric: Vec<bool> = columns
        .iter()
        .map(|c| {
            pretty.is_some() && {
                let mut any = false;
                let all = values.iter().all(|v| match field_of(v, c) {
                    None | Some(Value::Null) => true,
                    Some(Value::Int(_) | Value::Float(_)) => {
                        any = true;
                        true
                    }
                    Some(_) => false,
                });
                all && any
            }
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
    // a numeric cell's two parts: before the point, and the point
    // with what follows (empty where the number is whole)
    let parts = |s: &str| -> (usize, usize) {
        match s.find('.') {
            Some(i) => (width(&s[..i]), width(&s[i..])),
            None => (width(s), 0),
        }
    };
    let mut whole: Vec<usize> = vec![0; columns.len()];
    let mut frac: Vec<usize> = vec![0; columns.len()];
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            if numeric[i] && !c.is_empty() {
                let (w, f) = parts(c);
                whole[i] = whole[i].max(w);
                frac[i] = frac[i].max(f);
            }
        }
    }
    let mut widths: Vec<usize> = columns.iter().map(|c| width(c)).collect();
    for (i, w) in widths.iter_mut().enumerate() {
        if numeric[i] {
            *w = (*w).max(whole[i] + frac[i]);
        }
    }
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(width(c));
        }
    }
    let line = |cells: &[String], header: bool| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if numeric[i] {
                    if header || c.is_empty() {
                        // the header and a missing value right-aligned to the column
                        return format!("{}{c}", " ".repeat(widths[i].saturating_sub(width(c))));
                    }
                    let (w, f) = parts(c);
                    let lead =
                        widths[i].saturating_sub(whole[i] + frac[i]) + whole[i].saturating_sub(w);
                    return format!(
                        "{}{c}{}",
                        " ".repeat(lead),
                        " ".repeat(frac[i].saturating_sub(f))
                    );
                }
                format!("{c}{}", " ".repeat(widths[i].saturating_sub(width(c))))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    lines.push(line(&columns, true));
    lines.push(
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    lines.extend(rows.iter().map(|r| line(r, false)));
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
            crate::value::ambient::record_refusal(format!(
                "{what}({n}): the result would exceed {} bytes",
                MAX_REPEAT_BYTES
            ));
            Value::Null
        }
    }
}

/// A padding past [`MAX_REPEAT_BYTES`]: the refusal, as `repeat`
/// refuses. A width is a count taken from the query or from data
/// (`lpad(::n)`), and `lpad(30000000000)` aborted the process on its
/// allocation.
fn pad_too_wide(what: &str, n: usize, have: usize, fill: &str) -> Option<Value> {
    let widest = fill.chars().map(char::len_utf8).max().unwrap_or(1);
    match (n - have).checked_mul(widest) {
        Some(bytes) if bytes <= MAX_REPEAT_BYTES => None,
        _ => {
            crate::value::ambient::record_refusal(format!(
                "{what}({n}): the result would exceed {} bytes",
                MAX_REPEAT_BYTES
            ));
            Some(Value::Null)
        }
    }
}

/// The most bins a histogram or `bins(n)` builds: a count from the
/// query must not size an allocation without a bound
/// (`bins(99999999999)` aborted the process).
pub const MAX_BINS: usize = 1 << 20;

/// The widest bar `histogram(--width=n)` draws, in cells.
pub const MAX_BAR_WIDTH: usize = 4096;

/// The most values `skipgrams` answers, over all its grams: the
/// count of grams times their length grows past memory on a long
/// stream.
pub const MAX_GRAM_VALUES: usize = 1 << 24;

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
    crate::syntax::registry::spec(name).is_some_and(|s| {
        matches!(
            s.agg,
            Some(crate::syntax::registry::Agg::Association { .. })
        )
    })
}

/// Whether `name` is an agreement measure — an association that
/// reads its two expressions as labels, not numbers (`kappa(:g; :p)`
/// over categories, `precision`/`recall`/`f1` over truthiness).
pub fn agreement(name: &str) -> bool {
    crate::syntax::registry::spec(name).is_some_and(|s| {
        matches!(
            s.agg,
            Some(crate::syntax::registry::Agg::Association { agreement: true })
        )
    })
}

/// Whether the call carries the switch `--sample` (draft 164; the
/// positional word is retired with a pointer): the dispersion
/// measures divide by n − 1 instead of n.
pub fn sample_flag(call: &FnCall) -> bool {
    call.args.iter().any(|a| {
        matches!(a, Arg::Opt { name, value: None } if name == "--sample")
            || matches!(a, Arg::Lit(Value::Str(s)) if s == "sample")
    })
}

/// Whether the call carries the bare word `atergo`: the a tergo
/// (inverse-lexicographic) order, in which texts compare from their
/// last character toward their first — the reverse dictionary's
/// order.
/// `--desc` (`-d`) on `sort` / `sort_by`: the whole order flipped.
pub fn desc_flag(call: &FnCall) -> bool {
    call.args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, value: None } if name == "--desc" || name == "-d"))
}

pub fn atergo_flag(call: &FnCall) -> bool {
    call.args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, value: None } if name == "--atergo" || name == "-a"))
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
    let tag = crate::value::ambient::ambient(|a| a.collation.clone())?;
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
    // The locale is the one literal operand (the modes are flags).
    let tag = call.args.iter().find_map(|a| match a {
        Arg::Lit(l) => Some(l.to_string()),
        _ => None,
    })?;
    match colligo::Collator::builder(&tag)
        .allow_approximate(true)
        .build()
    {
        Ok(c) => Some(c),
        Err(_) => {
            crate::value::ambient::record_refusal(format!(
                "{}: no collation for locale {tag:?}",
                call.name
            ));
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

/// The scalar registry, for enumerating consumers (completion,
/// highlighting): every per-capsa `|` command.
pub fn scalar_names() -> Vec<&'static str> {
    crate::syntax::registry::names(true, None).collect()
}

/// The aggregate registry: every `@|` reducer (keyed aggregates
/// are enumerated separately by [`keyed_names`]).
pub fn aggregate_names() -> Vec<&'static str> {
    use crate::syntax::registry::Agg;
    crate::syntax::registry::SPECS
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
    crate::syntax::registry::names(false, Some(crate::syntax::registry::Agg::Keyed)).collect()
}

/// The calendar a value is read in: an instant's or a period's,
/// the Gregorian for everything else.
fn topic_calendar(topic: &Value) -> crate::calendar::Calendar {
    match topic {
        Value::Instant { calendar, .. } | Value::Period { calendar, .. } => *calendar,
        _ => Default::default(),
    }
}

/// The calendar a name denotes: the canonical names, and the CLDR's
/// names of the calendars in the shipped languages (spec: Calendars).
fn calendar_named(name: &str) -> Option<crate::calendar::Calendar> {
    crate::calendar::Calendar::from_name(name).or_else(|| {
        crate::calnames::calendar_alias(name).and_then(crate::calendar::Calendar::from_name)
    })
}

/// The calendar argument of `strptime` / `strftime` at position `n`:
/// `Ok(None)` without one, `Err` (after a refusal) for a name no
/// calendar answers to.
fn calendar_arg(call: &FnCall, n: usize) -> Option<Option<crate::calendar::Calendar>> {
    let name = arg_str(call, n, "");
    if name.is_empty() {
        return Some(None);
    }
    match calendar_named(name) {
        Some(c) => Some(Some(c)),
        None => {
            crate::value::ambient::record_refusal(format!(
                "{}: no such calendar {name:?} (shipped: {})",
                call.name,
                crate::calendar::shipped()
            ));
            None
        }
    }
}

/// The locale argument of `strptime` / `strftime`: the table for
/// the second argument, English without one; a locale no table
/// covers is refused.
fn date_names(call: &FnCall) -> Option<&'static crate::datenames::Names> {
    let locale = arg_str(call, 1, "");
    match crate::datenames::names(locale) {
        Some(n) => Some(n),
        None => {
            crate::value::ambient::record_refusal(format!(
                "{}: no month and weekday names for locale {locale:?} (shipped: {})",
                call.name,
                crate::value::locale::SHIPPED.join(", ")
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
///
/// SCALAR COMMANDS BROADCAST (spec: Scalar commands, draft 166): a
/// command whose input is a scalar (the inventory's kind `scalar`)
/// applied to a list, a record or a table applies to every item,
/// field or cell one level deep and answers the structure (an
/// expanding command's values gathered per item); a table's index
/// columns stand, its declared columns cast or refuse by name.
/// List-input commands read the structure whole, as before.
pub fn apply_scalar(
    call: &FnCall,
    topic: Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
    if crate::syntax::man::is_scalar_kind(&call.name)
        && !BROADCAST_EXEMPT.contains(&call.name.as_str())
    {
        let numeric = NUMERIC_COMMANDS.contains(&call.name.as_str());
        let one = |v: Value| {
            // a cell of a kind a numeric command does not take stays
            // as it is (ruling 2026-11-10: `sqrt`, the logarithms,
            // `exp` and `pow` made it null, where `round` kept it)
            if numeric && untaken(call, &v) {
                return v;
            }
            let mut out = apply_scalar_inner(call, v, scale);
            match out.len() {
                0 => Value::Null,
                1 => out.pop().unwrap(),
                _ => Value::list(out),
            }
        };
        match topic {
            Value::List(items) => {
                return vec![Value::list(items.iter().cloned().map(one).collect())];
            }
            Value::Record(r) => return vec![Value::Record(r.map_values(one))],
            Value::Table(t) => {
                let rows: Vec<Vec<Value>> = t
                    .rows()
                    .map(|row| {
                        row.iter()
                            .enumerate()
                            .map(|(j, c)| {
                                if j < t.index {
                                    c.clone()
                                } else {
                                    one(c.clone())
                                }
                            })
                            .collect()
                    })
                    .collect();
                return vec![match t.rebuild(rows) {
                    Ok(nt) => Value::table(nt),
                    Err(e) => {
                        crate::value::ambient::record_refusal(format!("{}: {e}", call.name));
                        Value::Null
                    }
                }];
            }
            // A lone value of a kind a numeric command does not take
            // answers null, and the run warns once for the command,
            // counting them (the same ruling: every thread is a lone
            // value, so a refusal would end a query over one "n/a",
            // and the text kept under `round` looked like an answer).
            // The cell `map` hands its body stays, as above.
            other if numeric && untaken(call, &other) => {
                if crate::value::ambient::in_cells() {
                    return vec![other];
                }
                crate::value::ambient::record_untaken(
                    &call.name,
                    numeric_takes(&call.name),
                    || {
                        let shown = crate::value::held_text(&other);
                        let shown = match shown.char_indices().nth(40) {
                            Some((cut, _)) => format!("{}…", &shown[..cut]),
                            None => shown,
                        };
                        format!("{} {shown}", kind_of(&other))
                    },
                );
                return vec![Value::Null];
            }
            other => return apply_scalar_inner(call, other, scale),
        }
    }
    apply_scalar_inner(call, topic, scale)
}

/// The numeric commands: what they do with a value that is no number
/// is one rule ([`apply_scalar`]).
const NUMERIC_COMMANDS: [&str; 10] = [
    "round", "floor", "ceil", "abs", "log", "log2", "log10", "exp", "sqrt", "pow",
];

/// What a numeric command takes, for the warning.
fn numeric_takes(name: &str) -> &'static str {
    match name {
        "round" | "floor" | "ceil" | "abs" => "a number or a quantity",
        _ => "a number",
    }
}

/// Whether `v` is of a kind the numeric command `call` does not
/// apply to. Null is no kind (it answers null); a table and a
/// library value are refused by kind where the command runs.
fn untaken(call: &FnCall, v: &Value) -> bool {
    match v {
        Value::Null | Value::Table(_) | Value::Foreign(_) => false,
        // `pow` with no exponent is refused whatever stands before it
        _ if call.name == "pow" && arg_num(call, 0).is_none() => false,
        // the rounders and `abs` work in a quantity's written unit;
        // the elementary functions have no reading of one
        Value::Quantity { .. } => numeric_takes(&call.name) == "a number",
        other => other.numeric().is_none(),
    }
}

fn apply_scalar_inner(
    call: &FnCall,
    topic: Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
    // The two readings and the reshapes take the topic whole; a
    // library value is read by them and by the serializations, and
    // refused by kind anywhere else (spec: Command Libraries, "A
    // library value").
    match call.name.as_str() {
        // `type`: the type of the value in front of it as one word, a
        // table's one word per column (spec: Quantity Commands); the
        // retired `types` points at it.
        "type" => return vec![type_of(&topic)],
        "types" => return vec![refusing(call, Err(TYPES_RETIRED.into()))],
        "@" | "rows" | "%" | "columns" | "#" | "transpose" | "header" | "dim" | "[]" | "cell"
        | "row-to-header" | "header-to-row" | "flatten" | "keys" | "values" | "stack"
        | "unstack" | "autoindex" | "autocast" | "clean" | "correlate" => {
            return vec![refusing(
                call,
                match call.name.as_str() {
                    // the foreign-key join (ruling 2026-11-09)
                    "correlate" => correlate(call, topic),
                    // the columns typed again from the cells present
                    // (ruling 2026-11-06): the explicit form of what a
                    // rebuild did implicitly before a selection kept
                    // the columns' types
                    "autocast" => autocast(call, topic),
                    // a scraped table's text columns cleaned into values
                    // (the document-tables round, 2026-11-08)
                    "clean" => clean(call, topic),
                    // the implicit index made explicit (ruling
                    // 2026-11-05)
                    "autoindex" => match topic {
                        Value::Table(t) => autoindex(call, &t, scale),
                        other => Err(format!(
                            "autoindex labels the rows of a table, not {}",
                            kind_of(&other)
                        )),
                    },
                    // the two levels moved between index and header
                    // (spec: The Table, "Levels of the header")
                    "stack" | "unstack" => match topic {
                        Value::Table(t) if call.name == "stack" => t.stacked().map(Value::table),
                        Value::Table(t) => t.unstacked().map(Value::table),
                        other => Err(format!(
                            "{} moves a level of a table, not {}",
                            call.name,
                            kind_of(&other)
                        )),
                    },
                    "keys" => keys_reading(topic),
                    "values" => values_reading(topic),
                    "@" | "rows" => rows_reading(topic),
                    "%" | "columns" => columns_reading(topic),
                    "#" => table_command(call, topic),
                    "flatten" => flatten_value(call, topic),
                    "header" => table_meta(topic, "header"),
                    "dim" => table_meta(topic, "dim"),
                    "[]" => table_rows_at(call, topic),
                    "cell" => table_cell(call, topic),
                    "row-to-header" | "header-to-row" => header_shift(call, topic),
                    _ => transpose_reading(topic),
                },
            )];
        }
        // `| describe` on a table: one row per statistic over the
        // numeric columns, indexed by the statistic (draft 166)
        "describe" if matches!(topic, Value::Table(_)) => {
            let Value::Table(t) = topic else {
                unreachable!()
            };
            return vec![refusing(call, describe_table(&t, scale))];
        }
        "describe" if matches!(topic, Value::Record(_)) => {
            let Value::Record(r) = topic else {
                unreachable!()
            };
            let values: Vec<Value> = r.iter().map(|(_, v)| v.clone()).collect();
            return vec![describe_values(&values, scale)];
        }
        // `| melt` on a table value answers the long table itself
        "melt" if matches!(topic, Value::Table(_)) => {
            return vec![refusing(
                call,
                melt(call, vec![topic]).map(|mut v| v.pop().unwrap_or(Value::Null)),
            )];
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
            let precision = pretty_precision(call);
            if let Value::Table(t) = &topic {
                let (delimiter, args) = split_csv_delimiter(call);
                let columns = column_names(args);
                return vec![Value::Str(tabulate_table_pretty(
                    t,
                    columns.as_deref(),
                    delimiter.unwrap_or(b','),
                    precision,
                ))];
            }
            return vec![Value::Str(pretty_value(&topic, precision))];
        }
        "csv" | "table" => {
            let (delimiter, args) = split_csv_delimiter(call);
            let columns = column_names(args);
            return vec![refusing(
                call,
                match &topic {
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
                },
            )];
        }
        _ => {}
    }
    let kept = match &topic {
        Value::Foreign(f) => Some(f.refusal(&call.name)),
        // `dimension` reads a table column by column
        Value::Table(t) if call.name != "dimension" => Some(table_refusal(t, &call.name)),
        _ => None,
    };
    if let Some(refusal) = kept {
        match call.name.as_str() {
            "json" | "jsonl" | "kaiv" | "csv" | "table" => {}
            _ => {
                crate::value::ambient::record_refusal(refusal);
                return vec![Value::Null];
            }
        }
    }
    // A null topic has no coercion (spec: the inventory's rule): a
    // scalar reading it answers null, an expander nothing; the
    // commands that read null on purpose (`default`, `type`, the
    // serializers) answer in their own arms.
    if matches!(topic, Value::Null) && reads_topic(call) {
        match crate::syntax::registry::spec(&call.name).map(|s| s.null) {
            Some(crate::syntax::registry::OnNull::Null) => return vec![Value::Null],
            Some(crate::syntax::registry::OnNull::Nothing) => return Vec::new(),
            _ => {}
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
                Some(crate::value::number::Decimal::Point)
            } else {
                crate::value::number::locale_convention(locale)
            };
            if decimal.is_none() {
                // an uncarried locale is refused, naming the ones carried
                // (spec: the locale-taking functions, 2026-11-06)
                crate::value::ambient::record_refusal(format!(
                    "number: no number conventions for locale {locale:?} (carried: {}; the \
                     point: {}; the comma: {})",
                    crate::value::locale::SHIPPED.join(", "),
                    crate::value::number::POINT_LANGUAGES.join(", "),
                    crate::value::number::COMMA_LANGUAGES.join(", ")
                ));
                return vec![Value::Null];
            }
            return vec![match (&topic, decimal) {
                (Value::Int(_) | Value::Float(_), _) => topic.clone(),
                (Value::Str(s), Some(d)) => crate::value::number::read(s, d).unwrap_or(Value::Null),
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
            // the anchor: a period, a date, or the year as an integer
            let anchor = match call.args.first() {
                Some(Arg::Lit(Value::Int(y))) => {
                    crate::period::Period::of(*y, 1, 1, 0, 0, 0, crate::period::Grain::Year)
                }
                Some(Arg::Lit(v)) => v.calendar_reading(),
                _ => {
                    crate::value::ambient::record_refusal(
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
            // `2025 | period` names the year explicitly; the bare
            // integer is otherwise epoch seconds
            let reading = match &topic {
                Value::Int(y) => {
                    crate::period::Period::of(*y, 1, 1, 0, 0, 0, crate::period::Grain::Year)
                }
                _ => topic.calendar_reading(),
            };
            return vec![match (reading, call.args.first()) {
                (None, _) => Value::Null,
                (Some(p), None) => Value::period(p),
                (Some(p), Some(Arg::Lit(Value::Str(g)))) => {
                    // the English grain words, and the CLDR's names of
                    // the grains in the shipped languages
                    let word = crate::period::Grain::from_word(g).or_else(|| {
                        crate::calnames::grain_alias(g).and_then(crate::period::Grain::from_word)
                    });
                    match word {
                        Some(g) => p.at(g).map_or(Value::Null, Value::period),
                        None => {
                            crate::value::ambient::record_refusal(format!(
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
        // `slug(sep?)`: the text as a slug — lowercased, every run of
        // characters that are neither letters nor digits the separator
        // (`_`, or the argument), the ends trimmed; null where nothing
        // is left (the header-units round, 2026-11-09)
        "slug" => vec![
            crate::graph::autounits::slug_text(&text, arg_str(call, 0, "_"))
                .map(Value::Str)
                .unwrap_or(Value::Null),
        ],
        "ltrim" => vec![Value::Str(text.trim_start().to_string())],
        "rtrim" => vec![Value::Str(text.trim_end().to_string())],
        // `cc` counts codepoints (né `chars`, the pre-0.25 alias);
        // `bc` counts UTF-8 bytes; `wc` counts words.
        // (each with its long alias, `char-count` for `gc`: what
        // the eye calls a character)
        "cc" | "codepoint-count" => vec![Value::Int(text.chars().count() as i64)],
        // `gc` counts grapheme clusters (UAX #29): what the eye
        // counts — a stressed vowel with its acute is one.
        "gc" | "char-count" => vec![Value::Int(text.graphemes(true).count() as i64)],
        "bc" | "byte-count" => vec![Value::Int(text.len() as i64)],
        "wc" | "word-count" => vec![Value::Int(text.split_whitespace().count() as i64)],
        "lc" | "line-count" => vec![Value::Int(text.lines().count() as i64)],
        // UAX #29 sentence segmentation (unicode-segmentation):
        // `sentences` splits (each segment trimmed, empties
        // dropped), `sc` counts the same segments.
        "sentences" => sentences_of(&text)
            .into_iter()
            .map(|t| Value::Str(t.to_string()))
            .collect(),
        "sc" | "sentence-count" => {
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
            } else if let Some(refused) = pad_too_wide(&call.name, n, have, fill) {
                vec![refused]
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
            } else if let Some(refused) = pad_too_wide(&call.name, n, have, fill) {
                vec![refused]
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
                crate::value::ambient::record_refusal(
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
        // `deaccent(LANG)` — the marks the language counts as
        // accents removed, the letters kept (ñ, ç, ё, â); the bare
        // `deaccent` is the blunt form, every nonspacing mark off,
        // the same as `strip`. The locale never enters: the
        // result is a value.
        "deaccent" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            let lang = arg_str(call, 0, "");
            if lang.is_empty() {
                return vec![Value::Str(crate::value::accents::strip(&text))];
            }
            match crate::value::accents::deaccent(&text, lang) {
                Ok(s) => vec![Value::Str(s)],
                Err(e) => {
                    crate::value::ambient::record_refusal(e);
                    vec![Value::Null]
                }
            }
        }
        "strip" => {
            if matches!(topic, Value::Null) {
                return vec![Value::Null];
            }
            vec![Value::Str(crate::value::accents::strip(&text))]
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
                crate::value::ambient::record_refusal(format!(
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
                    crate::value::ambient::record_refusal(
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
                    crate::value::ambient::record_refusal(
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
            match crate::value::translit::translit(&text, scheme) {
                Ok(s) => vec![Value::Str(s)],
                Err(e) => {
                    crate::value::ambient::record_refusal(e);
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
            match crate::value::translit::modernize(&text, table) {
                Ok(s) => vec![Value::Str(s)],
                Err(e) => {
                    crate::value::ambient::record_refusal(e);
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
        "sha256" => vec![Value::Str(crate::value::encoding::sha256_hex(
            text.as_bytes(),
        ))],
        "base64" => vec![Value::Str(crate::value::encoding::base64(text.as_bytes()))],
        "base64url" => vec![Value::Str(crate::value::encoding::base64url(
            text.as_bytes(),
        ))],
        "base32" => vec![Value::Str(crate::value::encoding::base32(text.as_bytes()))],
        "crockford32" => vec![Value::Str(crate::value::encoding::crockford32(
            text.as_bytes(),
        ))],
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
                "json" => crate::value::encoding::json_to_value(&text).unwrap_or(Value::Null),
                #[cfg(feature = "decode-formats")]
                "yaml" => crate::value::encoding::yaml_to_value(&text).unwrap_or(Value::Null),
                #[cfg(feature = "decode-formats")]
                "toml" => crate::value::encoding::toml_to_value(&text).unwrap_or(Value::Null),
                #[cfg(feature = "decode-formats")]
                "xml" => crate::value::encoding::xml_to_value(&text).unwrap_or(Value::Null),
                #[cfg(not(feature = "decode-formats"))]
                "json" | "yaml" | "toml" | "xml" => {
                    crate::value::ambient::record_refusal(format!(
                        "decode({scheme}): the structured readings are not compiled in (feature decode-formats)"
                    ));
                    Value::Null
                }
                _ => crate::value::encoding::decode(scheme, &text)
                    .and_then(|b| String::from_utf8(b).ok())
                    .map(Value::Str)
                    .unwrap_or(Value::Null),
            }]
        }
        "hex" => vec![Value::Str(crate::value::encoding::hex(text.as_bytes()))],

        // Temporal scalars (spec: The Temporal Fragment). All are
        // defined via the temporal reading, so they serve typed
        // instants, epoch integers, and ISO text alike; no reading
        // means null.
        // `datetime` takes no operand (ruling 2026-11-10): it reads
        // ISO 8601, which no locale respells; `strptime(fmt; LOCALE)`
        // is the parser that reads a locale's words (the parser
        // refuses an operand here with that pointer).
        "datetime" => {
            vec![match &topic {
                Value::Instant { .. } => topic.clone(),
                Value::Str(s) => crate::temporal::parse_iso(s)
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                        calendar: Default::default(),
                    })
                    .unwrap_or(Value::Null),
                // a number is an epoch, a point of the timeline: `Z`
                other => other
                    .offset_reading()
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                        calendar: Default::default(),
                    })
                    .unwrap_or(Value::Null),
            }]
        }
        "epoch" => vec![
            topic
                .temporal_reading()
                .map(|(secs, _)| Value::Int(secs))
                .unwrap_or(Value::Null),
        ],
        // `truncate(d)` — the instant floored to a multiple of a
        // fixed-length duration counted from the Unix epoch, in UTC
        // (spec: Temporal Commands, draft 166): `truncate(5_min)`,
        // `truncate(6_h)`, `truncate(1_d)`, weeks too. A calendar
        // period (`1_mo`, `1_y`) refuses pointing at `period(GRAIN)`;
        // over the temporal coercion of the topic, null where there
        // is none, as `epoch`.
        "truncate" => {
            let refuse = |why: String| {
                crate::value::ambient::record_refusal(why);
                vec![Value::Null]
            };
            let d = match call.args.first() {
                Some(Arg::Lit(v)) => v.clone(),
                Some(other) => {
                    return refuse(format!(
                        "truncate: the duration must be a literal (truncate(5_min)), not {}",
                        arg_kind(other)
                    ));
                }
                None => {
                    return refuse(
                        "truncate takes the duration to floor to: truncate(5_min), truncate(6_h), truncate(1_d)"
                            .into(),
                    );
                }
            };
            let unit = match &d {
                Value::Quantity {
                    written: Some((_, u)),
                    ..
                } => u.as_str(),
                Value::Quantity { base, .. } => base.as_str(),
                _ => "",
            };
            if matches!(
                unit,
                "mo" | "month" | "months" | "y" | "yr" | "year" | "years"
            ) {
                return refuse(format!(
                    "truncate: {d} is a calendar period, not a fixed-length duration; a calendar grain is period's: period(\"{}\")",
                    if unit.starts_with('m') {
                        "month"
                    } else {
                        "year"
                    }
                ));
            }
            let Some((ds, dn)) = d.span_parts() else {
                return refuse(format!(
                    "truncate: the operand must be a duration of seconds to weeks (5_min, 6_h, 1_d), not {}",
                    kind_of(&d)
                ));
            };
            let step = ds as i128 * 1_000_000_000 + dn as i128;
            if step <= 0 {
                return refuse(format!("truncate: the duration must be positive, not {d}"));
            }
            // the floor is taken on the wall clock the instant is
            // written on, and the offset is kept (ruling 2026-11-10):
            // a day at +02:00 begins at its own midnight
            vec![match topic.offset_reading() {
                Some((secs, nanos, offset_min)) => {
                    let shift = i128::from(offset_min.unwrap_or(0)) * 60;
                    let total = (secs as i128 + shift) * 1_000_000_000 + nanos as i128;
                    let floored = total.div_euclid(step) * step;
                    Value::Instant {
                        secs: (floored.div_euclid(1_000_000_000) - shift) as i64,
                        nanos: floored.rem_euclid(1_000_000_000) as u32,
                        offset_min,
                        calendar: Default::default(),
                    }
                }
                None => Value::Null,
            }]
        }
        // `isoformat` is the reading, formatted UTC (spec: The
        // Temporal Fragment) — a typed instant renders in UTC, not
        // its written display offset, so it agrees with the text and
        // epoch paths rather than short-circuiting to Display. The
        // text carries the `Z` (ruling 2026-11-10), for every
        // instant, one read with no offset included: it is a point
        // on the UTC timeline and reads back as one.
        "isoformat" => vec![
            topic
                .temporal_reading()
                .map(|(s, n)| Value::Str(crate::temporal::format_instant(s, n, Some(0))))
                .unwrap_or(Value::Null),
        ],
        // `offset` (ruling 2026-11-10), the command of the offset
        // trait. Bare: the offset the instant is written at, as text,
        // "+02:00" or "Z"; null where it is written with none. With
        // an operand, the offset's word written or computed
        // (`offset(+2)`, `offset(Z)`, `offset(::tz)`): the instant
        // declared at it where it was written with none, the wall
        // time kept and the point moved; displayed at it where it
        // has one, the point kept. A topic that is no instant is
        // null.
        "offset" => {
            let written = match &topic {
                Value::Instant { offset_min, .. } => Some(*offset_min),
                Value::Str(s) => crate::temporal::parse_iso(s).map(|(_, _, o)| o),
                _ => None,
            };
            let word = match call.args.first() {
                None => {
                    return vec![match written {
                        Some(Some(m)) => Value::Str(crate::value::offset_spelling(m)),
                        _ => Value::Null,
                    }];
                }
                Some(Arg::Lit(Value::Null)) => return vec![Value::Null],
                Some(Arg::Lit(Value::Str(s))) => s.clone(),
                // a number written or computed is a count of hours
                Some(Arg::Lit(Value::Int(n))) => format!("{n:+}"),
                Some(Arg::Lit(other)) => other.to_string(),
                Some(other) => {
                    crate::value::ambient::record_refusal(format!(
                        "offset takes an offset from UTC, written or computed (offset(+2), \
                         offset(+02:00), offset(Z), offset(::tz)), not {}",
                        arg_kind(other)
                    ));
                    return vec![Value::Null];
                }
            };
            match crate::value::offset_word(&word) {
                Some(Ok(m)) => vec![match written {
                    Some(_) => crate::value::cast_offset(&topic, m).unwrap_or(Value::Null),
                    None => Value::Null,
                }],
                Some(Err(why)) => {
                    crate::value::ambient::record_refusal(format!("offset: {why}"));
                    vec![Value::Null]
                }
                None => {
                    crate::value::ambient::record_refusal(format!(
                        "offset: '{word}' names no offset from UTC: a signed count of hours \
                         ('+2'), hours and minutes ('+02:00', '+05:30'), or 'Z' for UTC"
                    ));
                    vec![Value::Null]
                }
            }
        }
        "year" | "month" | "day" | "hour" | "minute" | "second" => {
            // the date fields in the topic's calendar reading (spec:
            // Calendars): the year in its era, the month's ordinal,
            // the day; `month(code)` the CLDR month code (M05L for a
            // leap month); the time fields on the timeline
            let calendar = topic_calendar(&topic);
            let code = call.name == "month" && arg_str(call, 0, "") == "code";
            vec![
                topic
                    .offset_reading()
                    .and_then(|(secs, _, offset)| {
                        // in the written offset (ruling 2026-11-10)
                        let secs = Value::wall_secs(secs, offset);
                        let (gy, gmo, gd, h, mi, se) = crate::temporal::components(secs);
                        let cd = crate::calendar::date(calendar, secs.div_euclid(86400))?;
                        let (y, mo, d) = if calendar == Default::default() {
                            (gy, gmo as i64, gd as i64)
                        } else {
                            (cd.year, i64::from(cd.month), i64::from(cd.day))
                        };
                        Some(if code {
                            Value::Str(cd.code)
                        } else {
                            Value::Int(match call.name.as_str() {
                                "year" => y,
                                "month" => mo,
                                "day" => d,
                                "hour" => h as i64,
                                "minute" => mi as i64,
                                _ => se as i64,
                            })
                        })
                    })
                    .unwrap_or(Value::Null),
            ]
        }
        // `era`: the era of the topic's calendar reading, as its CLDR
        // code (`reiwa`, `ce`, `ah`); null where the calendar counts
        // without eras (spec: Calendars).
        "era" => {
            let calendar = topic_calendar(&topic);
            vec![
                topic
                    .offset_reading()
                    .and_then(|(secs, _, offset)| {
                        crate::calendar::date(
                            calendar,
                            Value::wall_secs(secs, offset).div_euclid(86400),
                        )
                    })
                    .and_then(|cd| cd.era)
                    .map(Value::Str)
                    .unwrap_or(Value::Null),
            ]
        }
        // `calendar(NAME)`: the topic instant or period read in a
        // calendar (spec: Calendars), the timeline unchanged; bare
        // `calendar`, the name of the topic's reading. A name no
        // calendar answers to is refused naming the shipped ones; the
        // CLDR's names in the shipped languages are read as aliases.
        "calendar" => {
            if call.args.is_empty() {
                return vec![match &topic {
                    Value::Instant { calendar, .. } | Value::Period { calendar, .. } => {
                        Value::Str(calendar.name().to_string())
                    }
                    _ => Value::Null,
                }];
            }
            let name = arg_str(call, 0, "");
            let Some(cal) = calendar_named(name) else {
                crate::value::ambient::record_refusal(format!(
                    "calendar({name}): no such calendar (shipped: {})",
                    crate::calendar::shipped()
                ));
                return vec![Value::Null];
            };
            vec![match &topic {
                Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                    ..
                } => Value::Instant {
                    secs: *secs,
                    nanos: *nanos,
                    offset_min: *offset_min,
                    calendar: cal,
                },
                Value::Period { .. } => topic
                    .calendar_reading()
                    .and_then(|p| p.in_calendar(cal).at(p.grain))
                    .map(Value::period)
                    .unwrap_or(Value::Null),
                other => other
                    .offset_reading()
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                        calendar: cal,
                    })
                    .unwrap_or(Value::Null),
            }]
        }
        "isodate" | "isomonth" | "isoweek" => vec![
            topic
                .offset_reading()
                .map(|(secs, _, offset)| {
                    let secs = Value::wall_secs(secs, offset);
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
        // `strftime(fmt; LOCALE; CALENDAR)` — a third argument names
        // the calendar whose fields `%Y %m %d %B %b %j` write (spec:
        // Calendars); without it, the topic's own reading.
        "strftime" | "tfmt" => {
            let fmt = arg_str(call, 0, "%Y-%m-%dT%H:%M:%S").to_string();
            let Some(names) = date_names(call) else {
                return vec![Value::Null];
            };
            let Some(calendar) = calendar_arg(call, 2).map(|c| c.unwrap_or(topic_calendar(&topic)))
            else {
                return vec![Value::Null];
            };
            let calnames = crate::calnames::names(arg_str(call, 1, ""), calendar);
            vec![match &topic {
                Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                    ..
                } => Value::Str(crate::temporal::strftime_cal(
                    &fmt,
                    *secs,
                    *nanos,
                    *offset_min,
                    names,
                    calendar,
                    calnames,
                )),
                other => other
                    .offset_reading()
                    .map(|(s, n, offset)| {
                        Value::Str(crate::temporal::strftime_cal(
                            &fmt, s, n, offset, names, calendar, calnames,
                        ))
                    })
                    .unwrap_or(Value::Null),
            }]
        }
        "weekday" => vec![
            topic
                .offset_reading()
                .map(|(secs, _, offset)| {
                    Value::Int(crate::temporal::weekday(Value::wall_secs(secs, offset)) as i64)
                })
                .unwrap_or(Value::Null),
        ],
        // the day of the written offset, as an offset-less date
        // (ruling 2026-11-10): 23:30 at -05:00 is that day, where the
        // UTC day answered the next
        "date" => vec![
            topic
                .offset_reading()
                .map(|(secs, _, offset)| Value::Instant {
                    secs: Value::wall_secs(secs, offset).div_euclid(86400) * 86400,
                    nanos: 0,
                    offset_min: None,
                    // the day keeps the reading of its instant
                    calendar: match &topic {
                        Value::Instant { calendar, .. } => *calendar,
                        _ => Default::default(),
                    },
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
        // `duration(FROM; TO)` (spec: Duration): the duration from
        // one instant to another, TO − FROM, negative when TO
        // precedes FROM; both take the instant coercion, null when
        // either does not. The topic fills FROM, the option names
        // TO: `::born | duration(--to=::died)`.
        "duration" | "td"
            if call.args.iter().any(
                |a| matches!(a, Arg::Opt { name, .. } if name == "--to" || name == "--from"),
            ) =>
        {
            // `--to=TO` (draft 203): the topic is FROM and the option
            // the other instant (`::born | duration(--to=::died)`; at
            // the head `duration(FROM; --to=TO)` rides FROM as the
            // topic); `--from=FROM` mirrors it, the topic then TO.
            // The parser refuses both at once.
            let option = |which: &str| {
                call.args.iter().find_map(|a| match a {
                    Arg::Opt {
                        name,
                        value: Some(crate::syntax::ast::Operand::Lit(v)),
                    } if name == which => Some(v.temporal_reading()),
                    _ => None,
                })
            };
            let (from, to) = match option("--to") {
                Some(to) => (Some(topic.temporal_reading()), Some(to)),
                None => (option("--from"), Some(topic.temporal_reading())),
            };
            let (Some(from), Some(to)) = (from, to) else {
                crate::value::ambient::record_refusal(format!(
                    "{}: two instants — FROM | duration(--to=TO), TO | duration(--from=FROM)",
                    call.name
                ));
                return vec![Value::Null];
            };
            vec![match (from, to) {
                (Some((fs, fn_)), Some((ts, tn))) => {
                    let seconds = (ts - fs) as f64 + (tn as f64 - fn_ as f64) / 1e9;
                    Value::Quantity {
                        value: seconds,
                        base: "s".to_string(),
                        written: None,
                    }
                }
                _ => Value::Null,
            }]
        }
        // `duration` (alias `td`): the durational coercion as a cast
        // (spec: Duration): a time quantity passes through, unit
        // text in a time unit keeps its written unit, ISO-8601 and
        // span text read in seconds, a number is seconds.
        "duration" | "td" => vec![match &topic {
            q if q.is_duration() => q.clone(),
            Value::Str(s)
                if let Some((value, base, wv, wu)) =
                    crate::value::quantity::parse_unit_text_with(s, scale)
                    && base == "s" =>
            {
                Value::Quantity {
                    value,
                    base,
                    written: Some((wv, wu)),
                }
            }
            _ => topic
                .durational_reading()
                .or_else(|| match &topic {
                    Value::Str(s) => crate::temporal::span_from_units(s, scale),
                    _ => None,
                })
                .map(|(secs, nanos)| Value::seconds(secs, nanos))
                .unwrap_or(Value::Null),
        }],
        // `dfmt` / `dfmt(--iso)` / `dfmt(LOCALE)`: the duration
        // formatter (spec: Duration): the mixed span text, largest
        // unit first, in the Latin designations under every
        // environment, in a locale's designations with a locale tag
        // (spec: Duration Designations by Locale), or the ISO-8601
        // duration; text, null for a topic with no durational
        // coercion. A locale no table covers is refused.
        "dfmt" => {
            let iso = call
                .args
                .iter()
                .any(|a| matches!(a, Arg::Opt { name, value: None } if name == "--iso"));
            let how = call
                .args
                .iter()
                .find_map(|a| match a {
                    Arg::Lit(Value::Str(s)) => Some(s.as_str()),
                    _ => None,
                })
                .unwrap_or("");
            let names = if how.is_empty() || iso {
                None
            } else {
                match crate::durnames::names(how) {
                    Some(names) => Some(names),
                    None => {
                        crate::value::ambient::record_refusal(format!(
                            "dfmt: no duration designations for locale {how:?} (shipped: {})",
                            crate::value::locale::SHIPPED.join(", ")
                        ));
                        return vec![Value::Null];
                    }
                }
            };
            vec![
                topic
                    .durational_reading()
                    .or_else(|| match &topic {
                        Value::Str(s) => crate::temporal::span_from_units(s, scale),
                        _ => None,
                    })
                    .map(|(secs, nanos)| {
                        let seconds = secs as f64 + nanos as f64 / 1e9;
                        Value::Str(match names {
                            Some(names) => {
                                crate::value::quantity::duration_text_with(seconds, names, &|n| {
                                    n.to_string()
                                })
                            }
                            None => crate::value::quantity::duration_text(seconds, iso),
                        })
                    })
                    .unwrap_or(Value::Null),
            ]
        }
        // `strptime(fmt)` (alias `tp`) — strftime's inverse: a TEXT
        // topic parsed per the same C/POSIX specifiers with the same
        // fixed English names. A parsed %z is kept for display;
        // fields the format omits default to the Unix epoch's;
        // non-text topics and non-matching text are null.
        // `strptime(fmt[; LOCALE])` — the names read under the
        // locale's table, in every form it lists; the format's
        // literal text matches as written (`'%d %B %Y г.'`).
        // `strptime(fmt; LOCALE; CALENDAR)` — the third argument names
        // the calendar the text's fields are in; the instant read
        // keeps that reading (spec: Calendars).
        "strptime" | "tp" => {
            let fmt = arg_str(call, 0, "%Y-%m-%dT%H:%M:%S").to_string();
            let Some(names) = date_names(call) else {
                return vec![Value::Null];
            };
            let Some(calendar) = calendar_arg(call, 2).map(|c| c.unwrap_or_default()) else {
                return vec![Value::Null];
            };
            let calnames = crate::calnames::names(arg_str(call, 1, ""), calendar);
            vec![match &topic {
                // an instant passes through unchanged (draft 166: a bare
                // ISO date is the instant literal, so `= 1881-12-25 |
                // tp("%Y-%m-%d")` reads what it already is)
                Value::Instant { .. } => topic.clone(),
                Value::Str(s) => crate::temporal::strptime_cal(s, &fmt, names, calendar, calnames)
                    .map(|(secs, nanos, offset_min)| Value::Instant {
                        secs,
                        nanos,
                        offset_min,
                        calendar,
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
            Value::Str(s) => crate::value::quantity::parse_unit_text_with(s, scale)
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
        // the HP48 trio in Quarb terms (spec: Quantity Commands):
        // `unit` reads the written unit as unit text, `unit(U)` puts
        // a number or a quantity in U, `magnitude` reads the number
        // in the written unit, `magnitude(U)` the number in U.
        "unit" => vec![match lift_quantity(topic.clone(), scale) {
            Some(q) if !arg_str(call, 0, "").is_empty() => convert_scalar(call, q, scale).remove(0),
            // the written unit in its canonical spelling, as the
            // display prints it (5_кг and 5_kΩ answer kg and kohm)
            Some(Value::Quantity { base, written, .. }) => Value::Str(
                crate::value::quantity::canonical_unit(&written.map_or(base, |(_, u)| u))
                    .replace(' ', ""),
            ),
            Some(_) => Value::Null,
            None if !arg_str(call, 0, "").is_empty() => match topic.numeric() {
                Some(n) => {
                    let target = arg_str(call, 0, "").to_string();
                    match scale(&target) {
                        Some((factor, base)) => Value::Quantity {
                            value: crate::value::quantity::scale_by(n, factor),
                            base,
                            written: Some((n, target)),
                        },
                        None => Value::Null,
                    }
                }
                None => Value::Null,
            },
            None => Value::Null,
        }],
        "magnitude" => vec![{
            let q = lift_quantity(topic.clone(), scale);
            let q = match q {
                Some(q) if !arg_str(call, 0, "").is_empty() => {
                    Some(convert_scalar(call, q, scale).remove(0))
                }
                other => other,
            };
            match q {
                Some(Value::Quantity { value, written, .. }) => {
                    let n = written.map_or(value, |(v, _)| v);
                    if n.fract() == 0.0 {
                        whole(n)
                    } else {
                        Value::Float(n)
                    }
                }
                _ => Value::Null,
            }
        }],
        // `scale` / `scale(--binary)` — the prefix of the unit's own
        // family that brings the magnitude to one or more.
        "scale" => vec![crate::value::quantity::scale_prefix(
            &topic,
            call.args
                .iter()
                .any(|a| matches!(a, Arg::Opt { name, value: None } if name == "--binary")),
            scale,
        )],
        // `dimension` — the dimension of the topic quantity as text
        // over the base dimensions (spec: The dimension); a
        // non-quantity topic is null.
        "dimension" => vec![match &topic {
            Value::Quantity { base, .. } => Value::Str(crate::value::quantity::dimension_of(base)),
            // a table: one entry per column in header order, null
            // where the column is no quantity
            Value::Table(t) => {
                let width = t.header.as_ref().map_or_else(|| t.width(), Vec::len);
                Value::list(
                    (0..width)
                        .map(|i| {
                            t.rows()
                                .find_map(|r| match r.get(i) {
                                    Value::Quantity { base, .. } => Some(Value::Str(
                                        crate::value::quantity::dimension_of(&base),
                                    )),
                                    Value::Str(s) => crate::value::quantity::parse_unit_text_with(
                                        &s, scale,
                                    )
                                    .map(|(_, base, ..)| {
                                        Value::Str(crate::value::quantity::dimension_of(&base))
                                    }),
                                    _ => None,
                                })
                                .unwrap_or(Value::Null)
                        })
                        .collect(),
                )
            }
            Value::Str(s) => crate::value::quantity::parse_unit_text_with(s, scale)
                .map(|(_, base, ..)| Value::Str(crate::value::quantity::dimension_of(&base)))
                .unwrap_or(Value::Null),
            _ => Value::Null,
        }],

        // Numeric scalars: a value with no numeric coercion never
        // arrives here (`apply_scalar`: a cell of a table, a field
        // of a record, an item of a list comes back as it was, a
        // lone one answers null under the run's warning; rulings
        // 2026-11-01 and 2026-11-10). A QUANTITY rounds in its display unit
        // and stays a quantity — `(::speed | convert('km/h')) |
        // round` is a whole number of km/h, not of base
        // meters-per-second.
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
        "round" => vec![numeric_or_same(topic, |n| match round_digits(call) {
            Some(d) => Value::Float(round_to(n, d)),
            None => whole(n.round()),
        })],
        "floor" => vec![numeric_or_same(topic, |n| whole(n.floor()))],
        "ceil" => vec![numeric_or_same(topic, |n| whole(n.ceil()))],
        "abs" => vec![match topic {
            // (`i64::MIN` has no integer absolute value)
            Value::Int(n) => n
                .checked_abs()
                .map_or(Value::Float(-(n as f64)), Value::Int),
            // quantity-aware like the rounders: the magnitude's
            // absolute value in the written unit
            Value::Quantity {
                value,
                base,
                written,
            } => Value::Quantity {
                value: value.abs(),
                base,
                written: written.map(|(v, u)| (v.abs(), u)),
            },
            other => numeric_or_same(other, |n| Value::Float(n.abs())),
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
                    crate::value::ambient::record_refusal(
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
                crate::value::ambient::record_refusal(format!(
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
        // global; `i m s x` are the pattern literal's flags; `$1`-style
        // capture references in the replacement. The pattern was
        // validated at parse time; a failed recompile passes the
        // topic through.
        "s" => {
            let pattern = arg_str(call, 0, "");
            let replacement = arg_str(call, 1, "");
            let mods = arg_str(call, 2, "");
            let flags = crate::syntax::lexer::subst_flags(&mods);
            let Some(re) = crate::stdlib::text::compiled_regex(&format!("{flags}{pattern}")) else {
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
                    Arg::Lit(v) | Arg::Key(v) => Some(v.clone()),
                    Arg::Expr(_)
                    | Arg::Range(_, _)
                    | Arg::Named { .. }
                    | Arg::Opt { .. }
                    | Arg::Pairs { .. }
                    | Arg::Hole(_) => None,
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
            crate::pipeline::kaiv_out::document(&exported(vec![topic])).unwrap_or_else(|e| e),
        )],
        #[cfg(not(feature = "kaiv"))]
        "kaiv" => {
            crate::value::ambient::record_refusal(
                "kaiv output is not compiled in (feature kaiv)".into(),
            );
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
            crate::value::ambient::record_refusal(format!(
                "command '{other}' has no implementation"
            ));
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
        crate::value::ambient::record_refusal(format!(
            "timex: no time-expression lists for locale {locale:?} (carried: {})",
            crate::timex::CARRIED
        ));
        return Vec::new();
    };
    let opt = |s: Option<String>| s.map_or(Value::Null, Value::Str);
    return found
        .into_iter()
        .map(|f| {
            Value::record(vec![
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
            crate::value::ambient::record_refusal(
                "substr takes a range — substr(1..3), substr(..3), \
                     substr(2..), substr(-2..) — or one position"
                    .into(),
            );
            return vec![Value::Null];
        }
    };
    if a == Some(0) || b == Some(0) {
        crate::value::ambient::record_refusal(
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
        crate::value::ambient::record_refusal(
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
                crate::value::ambient::record_refusal(format!(
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
    let (unit, written): (f64, &str) = match call.name.as_str() {
        "seconds" => (1.0, "s"),
        "minutes" => (60.0, "min"),
        "hours" => (3600.0, "h"),
        _ => (86400.0, "d"),
    };
    // over a span: the count of the unit; over a number: the span
    // written in the unit (`30 | days` is 30_d)
    if let Some((secs, nanos)) = topic.span_parts() {
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
            .map(|n| Value::Quantity {
                value: n * unit,
                base: "s".to_string(),
                written: Some((n, written.to_string())),
            })
            .unwrap_or(Value::Null),
    ]
}

fn bibref_scalar(call: &FnCall, topic: Value) -> Vec<Value> {
    let standard = arg_str(call, 0, "").to_string();
    if !crate::value::bibref::STANDARDS.contains(&standard.as_str()) {
        crate::value::ambient::record_refusal(format!(
            "bibref: unknown standard {standard:?} — this build reads {}",
            crate::value::bibref::STANDARDS.join(", ")
        ));
        return vec![Value::Null];
    }
    vec![match &topic {
        Value::Str(s) => match crate::value::bibref::parse(&standard, s) {
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
                Value::Record(rec.into())
            }
            _ => Value::Null,
        },
        _ => Value::Null,
    }]
}

/// Whether a call reads its topic: the full-argument form of a
/// command that takes its operands as arguments (`levenshtein(a;
/// b)`, `duration(FROM; TO)`, `haversine(lat1; lon1; lat2; lon2)`)
/// reads only them, so a null topic says nothing about its answer.
fn reads_topic(call: &FnCall) -> bool {
    let n = call.args.len();
    match call.name.as_str() {
        "levenshtein" | "jaro_winkler" | "pow" | "log" => n < 2,
        "log_dice" | "dice" => n < 3,
        "haversine" | "loglik" | "chi2" | "mi" | "t_score" => n < 4,
        _ => true,
    }
}

/// `convert(unit)`. `apply_scalar`'s arm.
/// A quantity, or unit text lifted to one; None for anything else.
fn lift_quantity(topic: Value, scale: &dyn Fn(&str) -> Option<(f64, String)>) -> Option<Value> {
    match topic {
        q @ Value::Quantity { .. } => Some(q),
        Value::Str(s) => {
            crate::value::quantity::parse_unit_text_with(&s, scale).map(|(value, base, wv, wu)| {
                Value::Quantity {
                    value,
                    base,
                    written: Some((wv, wu)),
                }
            })
        }
        _ => None,
    }
}

fn convert_scalar(
    call: &FnCall,
    topic: Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Vec<Value> {
    let target = arg_str(call, 0, "").to_string();
    // a bare number into a unit of dimension one (`0.12 | convert(%)`
    // is `12_%`, `3.14 | convert(rad)`): the number is the base
    // magnitude, dressed (ruling 2026-11-09)
    if !matches!(topic, Value::Quantity { .. } | Value::Str(_))
        && !target.is_empty()
        && let Some(n) = topic.numeric()
        && let Some((factor, base)) = scale(&target)
        && base == "1"
    {
        return vec![Value::Quantity {
            value: n,
            base,
            written: Some((crate::value::quantity::unscale_by(n, factor), target)),
        }];
    }
    let Some(topic) = lift_quantity(topic, scale) else {
        return vec![Value::Null];
    };
    // no operand: the base form, written in the base expansion
    // (spec: Quantity Commands), 5_km as 5000_m, 1.5_h as 5400_s
    if target.is_empty() {
        return vec![match topic {
            // the base form of dimension one is the bare number
            // (`12_% | convert` is 0.12)
            Value::Quantity { value, base, .. } if base == "1" => Value::Float(value),
            Value::Quantity { value, base, .. } => Value::Quantity {
                value,
                base: base.clone(),
                written: Some((value, base)),
            },
            _ => Value::Null,
        }];
    }
    vec![match &topic {
        Value::Quantity { value, base, .. } => match scale(&target) {
            Some((factor, tbase)) if &tbase == base => Value::Quantity {
                value: *value,
                base: base.clone(),
                written: Some((crate::value::quantity::unscale_by(*value, factor), target)),
            },
            // A unit of another dimension: refused (ruling
            // 2026-11-10; spec: Physical Quantities, the conversion
            // of unlike dimensions "is refused"), under `convert` as
            // under `unit(u)` and `magnitude(u)`, which convert the
            // same way. It answered null.
            Some((_, tbase)) => {
                // Two currencies have no fixed rate: the way on is a
                // rate stated, and the refusal points at it as the
                // others point at their remedy. The bases differ in
                // the currency alone (`EUR` and `USD`, `EUR/kg` and
                // `USD/kg`); the exchange takes the currency for its
                // target, and the conversion follows it.
                let currency = |b: &str| {
                    b.split(|c: char| !c.is_ascii_alphabetic())
                        .find(|w| crate::value::quantity::CURRENCIES.contains(w))
                        .map(str::to_string)
                };
                let pointer = match (currency(base), currency(&tbase)) {
                    (Some(a), Some(b)) if a != b && base.replace(&a, &b) == tbase => format!(
                        ": two currencies have no fixed rate; state one, as in \
                         exchange(%({a} = 1.08_{b}); {b})"
                    ),
                    _ => String::new(),
                };
                crate::record_refusal(format!(
                    "{}({target}): a quantity converts within its own dimension, and these \
                     are unlike ({base} and {tbase}){pointer}",
                    call.name
                ));
                Value::Null
            }
            // one no library declares: no reading
            None => Value::Null,
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
        value: crate::value::quantity::scale_by(rounded, factor),
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
                crate::value::ambient::record_refusal(if call.name == "log_dice" {
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

/// The finishers that read a number and leave what they cannot
/// round (spec: the stdlib appendix, `round`): the numeric coercion
/// of the topic under `f`, else the topic itself, unchanged.
fn numeric_or_same(topic: Value, f: impl Fn(f64) -> Value) -> Value {
    match topic.numeric() {
        Some(n) => f(n),
        None => topic,
    }
}

/// The digits argument of `round(d)`, when one was given. A
/// non-integer argument reads as absent, like `arg_str`'s fallback.
fn round_digits(call: &FnCall) -> Option<i64> {
    match call.args.first() {
        Some(Arg::Lit(Value::Int(d))) => Some(*d),
        None => None,
        Some(other) => {
            crate::value::ambient::record_refusal(format!(
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
/// The scalar commands that read a structure whole all the same: the
/// constructors, the serializers of a value, the shell, the exchange
/// over a rates record, the anchor of a timex record, and the
/// quantity words `unit` and `dimension`, one per column on a table
/// (spec: Quantity Commands). `default(v)` broadcasts as `??` does,
/// the substitution `s/…/…/` as `lower`.
/// The table reshapers `stack` and `unstack` and `map` itself take
/// `|` alone (the inventory's kind `scalar` since its 0.43.0 rows) and
/// read the structure whole.
const BROADCAST_EXEMPT: [&str; 20] = [
    "map",
    "stack",
    "unstack",
    "autoindex",
    "autocast",
    "clean",
    "correlate",
    "record",
    "rec",
    "xml",
    "html",
    "markdown",
    "atrep",
    "conllu",
    "link",
    "sh",
    "exchange",
    "time_anchor",
    "unit",
    "dimension",
];

/// The aggregate entry: a command over the items of a stream, or
/// over the one value of a thread.
pub fn apply(call: &FnCall, input: Vec<Value>, scale: Scale) -> Vec<Value> {
    apply_inner(call, input, scale)
}

/// Whether the items are records of one shape (the same keys in one
/// order), at least `min` of them: the list a table reads from.
fn one_shape(items: &[Value], min: usize) -> bool {
    if items.len() < min {
        return false;
    }
    let Some(Value::Record(first)) = items.first() else {
        return false;
    };
    !first.is_empty()
        && items.iter().all(|v| match v {
            Value::Record(o) => {
                o.len() == first.len() && o.iter().zip(first.iter()).all(|((a, _), (b, _))| a == b)
            }
            _ => false,
        })
}

fn apply_inner(call: &FnCall, input: Vec<Value>, scale: Scale) -> Vec<Value> {
    // A library value in the stream: read by the two readings,
    // serialized by its rows, counted and ordered as one value,
    // refused by kind anywhere else.
    // One table under an ordering reduction: its rows (spec: The
    // Table); `sort`, keyless, is refused with the pointer below.
    if let [Value::Table(t)] = input.as_slice()
        && let Some(v) = table_order_reading(call, t, scale)
    {
        return vec![v];
    }
    // `describe` on a table: one row per statistic over its numeric
    // columns, indexed by the statistic (draft 166); on a record
    // the statistics of its values
    if call.name == "describe" {
        match input.as_slice() {
            [Value::Table(t)] => return vec![refusing(call, describe_table(t, scale))],
            [Value::Record(r)] => {
                let values: Vec<Value> = r.iter().map(|(_, v)| v.clone()).collect();
                return vec![describe_values(&values, scale)];
            }
            _ => {}
        }
    }
    if let Some(refusal) = input.iter().find_map(|v| match v {
        Value::Foreign(f) => Some(f.refusal(&call.name)),
        Value::Table(t) => Some(table_refusal(t, &call.name)),
        _ => None,
    }) && !matches!(
        call.name.as_str(),
        "@" | "rows"
            | "%"
            | "columns"
            | "#"
            | "transpose"
            | "pivot"
            | "header"
            | "dim"
            | "type"
            | "types"
            | "[]"
            | "cell"
            | "row-to-header"
            | "header-to-row"
            | "melt"
            | "flatten"
            | "keys"
            | "fields"
            | "values"
            | "json"
            | "jsonl"
            | "kaiv"
            | "csv"
            | "table"
            | "pretty"
            | "count"
            | "nulls"
            | "first"
            | "last"
            | "reverse"
    ) {
        crate::value::ambient::record_refusal(refusal);
        return vec![Value::Null];
    }
    let input = match call.name.as_str() {
        "json" | "jsonl" | "kaiv" | "csv" | "table" | "pretty" => exported(input),
        _ => input,
    };
    match call.name.as_str() {
        // What `run_for_rendering` leaves where a closing `@| csv` /
        // `@| pretty` stood: the rows themselves, for the tool to
        // tabulate. Not a name a query can write.
        ROWS_FOR_RENDERING => input,
        // The stream as a table, read by rows or by columns, or
        // reshaped (spec: Command Libraries, "A library value";
        // the reshapes): one library value reads as itself, a
        // stream of records as the list of them.
        "@" | "rows" | "%" | "columns" | "#" | "transpose" | "header" | "dim" | "type"
        | "types" | "[]" | "cell" | "row-to-header" | "header-to-row" | "flatten" | "keys"
        | "fields" | "values" => {
            // One library value, or one record (a record of lists
            // after `|`, whose items the executor hands here), is
            // the whole; anything else is the stream as a list.
            // A stream of several values read as rows (`@| @`) is
            // the list of them, one value (a stream of records is
            // the record array), where one library value opens
            // into its rows.
            let stream = input.len() > 1 && matches!(call.name.as_str(), "@" | "rows" | "fields");
            // A command whose output is a list answers the list, a
            // list of one included (`%(a = 1) | keys` → @("a")): the
            // spread of a lone list into the thread is for the
            // readings that yield rows, never for these.
            let listed = input.len() == 1
                && matches!(
                    call.name.as_str(),
                    "keys" | "values" | "fields" | "header" | "flatten"
                );
            let whole = match input.as_slice() {
                [Value::Foreign(_)] | [Value::Table(_)] | [Value::Record(_)] => {
                    input.into_iter().next().expect("one")
                }
                // `| [n]` indexes the one topic as it stands (a list,
                // a table, or a scalar that refuses); `| type` names
                // the one value in front of it; `| flatten` opens the
                // one value, a scalar passing through.
                [_] if matches!(call.name.as_str(), "[]" | "type" | "flatten" | "header") => {
                    input.into_iter().next().expect("one")
                }
                _ => Value::list(input),
            };
            let out = refusing(
                call,
                match call.name.as_str() {
                    "@" | "rows" => rows_reading(whole),
                    "%" | "columns" => columns_reading(whole),
                    "#" => table_command(call, whole),
                    "flatten" => flatten_value(call, whole),
                    "keys" => keys_reading(whole),
                    "fields" => fields_reading(whole),
                    "values" => values_reading(whole),
                    "header" => table_meta(whole, "header"),
                    "dim" => table_meta(whole, "dim"),
                    "type" => Ok(type_of(&whole)),
                    "types" => Err(TYPES_RETIRED.into()),
                    "[]" => table_rows_at(call, whole),
                    "cell" => table_cell(call, whole),
                    "row-to-header" | "header-to-row" => header_shift(call, whole),
                    _ => transpose_reading(whole),
                },
            );
            match out {
                Value::List(_) if !stream && !listed => out.into_items().unwrap_or_default(),
                other => vec![other],
            }
        }
        "pivot" => match pivot(call, input) {
            Ok(rows) => rows,
            Err(why) => {
                crate::value::ambient::record_refusal(why);
                vec![Value::Null]
            }
        },
        "melt" => match melt(call, input) {
            Ok(rows) => rows,
            Err(why) => {
                crate::value::ambient::record_refusal(why);
                vec![Value::Null]
            }
        },
        // A numeric reducer over lists and no number: each list is
        // a value it cannot coerce, so the answer is null — said,
        // not silent, with the two spellings that reduce the lists.
        name @ ("sum" | "product" | "mean" | "avg" | "median" | "percentile" | "std" | "stddev"
        | "var" | "variance")
            if !input.iter().any(|v| v.numeric().is_some())
                && input.iter().any(|v| matches!(v, Value::List(_))) =>
        {
            crate::value::ambient::record_refusal(format!(
                "{name}: the input holds lists, not numbers — '| {name}' reduces each list, \
                 '| ... @| {name}' reduces all their items"
            ));
            vec![Value::Null]
        }
        // `count`: how many; `--values` (`-v`) the non-null values
        // alone, `--nulls` (`-n`) the nulls alone (spec: Aggregate
        // Commands).
        "count" => {
            let switch = |long: &str, short: &str| {
                call.args.iter().any(|a| matches!(a, Arg::Opt { name, value: None } if name == long || name == short))
            };
            let nulls = input.iter().filter(|v| matches!(v, Value::Null)).count();
            vec![Value::Int(if switch("--nulls", "-n") {
                nulls as i64
            } else if switch("--values", "-v") {
                (input.len() - nulls) as i64
            } else {
                input.len() as i64
            })]
        }
        // `any` / `all` (ruling 2026-11-05): over a list of booleans,
        // `any` true when any element is true (false on an empty
        // list), `all` true when every element is (true on an empty
        // list); a null element counts as false; any other element
        // refuses naming it.
        "any" | "all" => {
            let want = call.name == "any";
            let mut verdict = !want;
            for v in &input {
                let b = match v {
                    Value::Bool(b) => *b,
                    Value::Null => false,
                    other => {
                        crate::value::ambient::record_refusal(format!(
                            "{}: {} is no boolean; the reducer reads a list of booleans (a comparison over a list)",
                            call.name,
                            kind_of(other)
                        ));
                        return vec![Value::Null];
                    }
                };
                if b == want {
                    verdict = want;
                    break;
                }
            }
            vec![Value::Bool(verdict)]
        }
        // `nulls` (draft 166): the count of a record's null fields;
        // on a table one count per row; over records one per record
        // (so `[nulls = 0]` over a table keeps the complete rows)
        "nulls" => {
            let of_fields = |fields: &[(String, Value)]| {
                Value::Int(
                    fields
                        .iter()
                        .filter(|(_, v)| matches!(v, Value::Null))
                        .count() as i64,
                )
            };
            let of_row = |row: &[Value]| {
                Value::Int(row.iter().filter(|v| matches!(v, Value::Null)).count() as i64)
            };
            match input.as_slice() {
                [Value::Record(r)] => vec![of_fields(r)],
                [Value::Table(t)] => {
                    vec![Value::list(t.rows().map(|r| of_row(&r.cells())).collect())]
                }
                [Value::List(items)] if items.iter().all(|i| matches!(i, Value::Record(_))) => {
                    vec![Value::list(
                        items
                            .iter()
                            .map(|i| match i {
                                Value::Record(r) => of_fields(r),
                                _ => Value::Null,
                            })
                            .collect(),
                    )]
                }
                _ if !input.is_empty() && input.iter().all(|i| matches!(i, Value::Record(_))) => {
                    vec![Value::list(
                        input
                            .iter()
                            .map(|i| match i {
                                Value::Record(r) => of_fields(r),
                                _ => Value::Null,
                            })
                            .collect(),
                    )]
                }
                _ => vec![of_row(&input)],
            }
        }
        "sum" => vec![sum(&input, scale)],
        "product" => vec![product(&input)],
        "min" => extreme(input, std::cmp::Ordering::Less, scale),
        "max" => extreme(input, std::cmp::Ordering::Greater, scale),
        "mean" | "avg" => vec![mean(&input, scale)],
        "describe" => vec![describe_values(&input, scale)],
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
                    crate::value::ambient::record_refusal(
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
                // a gram needs n members at rising positions
                if input.len() - start < n {
                    break;
                }
                skipgrams_from(&input, start, n, k, &mut out);
                if out.len().saturating_mul(n) > MAX_GRAM_VALUES {
                    crate::value::ambient::record_refusal(format!(
                        "skipgrams({n}; {k}): the grams would hold more than {MAX_GRAM_VALUES} values"
                    ));
                    return vec![Value::Null];
                }
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
                        crate::value::ambient::record_refusal(
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
                    crate::value::ambient::record_refusal(
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
            // takes a closing `@| csv` / `@| pretty` off the query
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
            if call.name == "pretty" {
                return vec![Value::Str(tabulate_pretty(
                    &input,
                    columns,
                    delimiter.unwrap_or(b','),
                    &|v| match v {
                        Value::Null => String::new(),
                        Value::Str(s) => s.clone(),
                        other => other.display_form(),
                    },
                    pretty_precision(call),
                ))];
            }
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
            crate::pipeline::kaiv_out::document(&input).unwrap_or_else(|e| e),
        )],
        #[cfg(not(feature = "kaiv"))]
        "kaiv" => {
            crate::value::ambient::record_refusal(
                "kaiv output is not compiled in (feature kaiv)".into(),
            );
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
            // `--desc` (`-d`) is the whole order flipped.
            let v = input;
            let mut sorted: Vec<Value> = match text_order(call) {
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
            };
            if desc_flag(call) {
                sorted.reverse();
            }
            sorted
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
            // `--nulls` (`-n`) adds a row for the nulls, `%(value =
            // null; n = k)`, last (the former `--values` restated the
            // default and is gone, draft 203)
            let with_nulls = call
                .args
                .iter()
                .any(|a| matches!(a, Arg::Opt { name, value: None } if name == "--nulls" || name == "-n"));
            let mut nulls = 0i64;
            let mut counts: Vec<(Value, i64)> = Vec::new();
            let mut index: std::collections::HashMap<crate::value::SortKey, usize> =
                std::collections::HashMap::new();
            for v in input {
                if matches!(v, Value::Null) {
                    nulls += 1;
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
            if with_nulls && nulls > 0 {
                counts.push((Value::Null, nulls));
            }
            counts
                .into_iter()
                .map(|(v, n)| {
                    Value::record(vec![
                        ("value".to_string(), v),
                        ("n".to_string(), Value::Int(n)),
                    ])
                })
                .collect()
        }
        // `@| bins(n)`: the numbers binned into n equal-width bins
        // over min..max, as the table of (from; to; count), the
        // nulls counted in a last row. `@| histogram(n; --width=w)`:
        // the same bins drawn as bars, one text value; a stream of
        // other values draws one bar per distinct value.
        "bins" => vec![refusing(
            call,
            binned(call, input, scale).map(|b| b.table()),
        )],
        "histogram" => vec![refusing(call, histogram(call, input, scale))],
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
        // `first` / `last`: one value, or the first / last n; an
        // empty input reduces to null.
        "first" | "last" if input.is_empty() => vec![Value::Null],
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
    if !input.iter().any(Value::is_duration) {
        return DurFold::Absent;
    }
    // a null, a bool or text with no reading is missing, as it is to
    // the numeric fold (ruling 2026-11-08); a number, or numeric
    // text, is seconds — the durational coercion
    let present = input.iter().filter_map(|v| match v {
        Value::Null | Value::Bool(_) => None,
        Value::Str(s) => match crate::temporal::parse_span(s) {
            Some((secs, nanos)) => Some(Some((secs, nanos))),
            None => s
                .trim()
                .parse::<f64>()
                .ok()
                .map(|n| Value::Float(n).durational_reading()),
        },
        other => Some(other.durational_reading()),
    });
    match present.collect() {
        Some(spans) => DurFold::Spans(spans),
        None => {
            // A number without a unit among spans is refused, as it
            // is beside any other quantity (the user, 2026-11-10:
            // "fold refuse too"): time is a dimension like the
            // others. (One fold read it as seconds and another
            // answered null for it.)
            if input.iter().any(|v| {
                !v.is_duration()
                    && !matches!(v, Value::Null | Value::Bool(_))
                    && v.numeric_reading().is_some()
            }) {
                unsound_fold("s and a bare number");
            }
            DurFold::Unsound
        }
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
    /// Mixed dimensions, or a bare number off the time dimension:
    /// refuse. A null, a bool or unreadable text is missing, as it
    /// is to the numeric fold (ruling 2026-11-08: a column of
    /// quantities with "NA" cells still has a mean).
    Unsound(String),
}

fn quantital_fold(input: &[Value], scale: Scale) -> QuantFold {
    // A fold of quantities: a typed quantity among the values, or
    // unit text the quantity coercion reads (`'18 parasang'` under
    // a declared unit, `'12 km'` under the table) — a stream of
    // cells is text until a reducer lifts it, as `convert` lifts
    // one value. Plain text and bare numbers stay absent.
    let unital = |v: &Value| match v {
        Value::Quantity { .. } => true,
        Value::Str(s) => crate::value::quantity::parse_unit_text_with(s, scale).is_some(),
        _ => false,
    };
    if !input.iter().any(unital) {
        return QuantFold::Absent;
    }
    let mut base: Option<String> = None;
    let mut unit: Option<(f64, String)> = None;
    let mut first = true;
    let mut numbers: Vec<f64> = Vec::new();
    let mut mags = Vec::with_capacity(input.len());
    for v in input {
        let (value, b, written): (f64, String, Option<(f64, String)>) = match v {
            Value::Quantity {
                value,
                base,
                written,
            } => (*value, base.clone(), written.clone()),
            // unit text, else span text (a span in seconds), else
            // numeric text as a bare number; any other text is
            // missing, as a null and a bool are
            Value::Str(s) => match crate::value::quantity::parse_unit_text_with(s, scale) {
                Some((bv, b, wmag, wunit)) => (bv, b, Some((wmag, wunit))),
                None => match crate::temporal::parse_span(s) {
                    Some((secs, nanos)) => (secs as f64 + nanos as f64 / 1e9, "s".into(), None),
                    // numeric text as the numeric fold reads it
                    // (grouped thousands included)
                    None => match v.numeric_reading().and_then(|n| n.numeric()) {
                        Some(n) => {
                            numbers.push(n);
                            continue;
                        }
                        None => continue,
                    },
                },
            },
            Value::Null | Value::Bool(_) => continue,
            // a number without a unit: refused beside a dimensioned
            // quantity, time among them (below)
            Value::Int(_) | Value::Float(_) => match v.numeric() {
                Some(n) => {
                    numbers.push(n);
                    continue;
                }
                None => return QuantFold::Unsound(String::new()),
            },
            _ => return QuantFold::Unsound(String::new()),
        };
        match &base {
            None => base = Some(b),
            Some(prev) if *prev == b => {}
            Some(prev) => return QuantFold::Unsound(format!("{prev} and {b}")),
        }
        // the first value's written unit is the result's, the base
        // when it has none (spec: the display of a computed quantity)
        if first {
            first = false;
            if let Some((mag, u)) = &written
                && *mag != 0.0
            {
                // the unit's factor from the table (exact for a
                // decimal one, so 12_% + 2_% is 14_%), the quotient
                // where the table does not know it
                let factor = scale(u).map_or(value / mag, |(f, _)| f);
                unit = Some((factor, u.clone()));
            }
        }
        mags.push(value);
    }
    match base {
        Some(base) if numbers.is_empty() => QuantFold::Same { base, unit, mags },
        // quantities of dimension one (percents, angles) beside bare
        // numbers fold as the numbers they are (ruling 2026-11-09):
        // the numeric fold reads them through their numeric reading
        Some(base) if base == "1" => QuantFold::Absent,
        // a bare number beside a dimensioned quantity: refused, as
        // the operator refuses `5_km + 5` (ruling 2026-11-10; both
        // answered null)
        Some(base) => QuantFold::Unsound(format!("{base} and a bare number")),
        None => QuantFold::Absent,
    }
}

/// A fold over quantities of unlike dimensions: refused, naming the
/// two (spec: Physical Quantities, their sum "is refused"; the
/// operator refuses `5_km + 3_s`, and the fold answered null). A
/// value that is no quantity at all leaves the fold null as before.
fn unsound_fold(clash: &str) -> Value {
    if clash.ends_with("a bare number") {
        crate::record_refusal(format!(
            "a fold over a quantity and a number without a unit ({clash}): arithmetic is not \
             defined for them; give the number its unit, or fold the magnitudes (| magnitude)"
        ));
    } else if !clash.is_empty() {
        crate::record_refusal(format!(
            "a fold over quantities of unlike dimensions ({clash}): arithmetic is not defined \
             for them; convert first"
        ));
    }
    Value::Null
}

/// Mint a fold's result on its base, re-expressed in the shared
/// written unit where one survived.
fn quantital_result(value: f64, base: String, unit: Option<(f64, String)>) -> Value {
    // dimension one with no unit to wear is the bare number
    if base == "1" && unit.is_none() {
        return Value::Float(value);
    }
    Value::Quantity {
        value,
        base,
        written: unit.map(|(f, u)| (crate::value::quantity::unscale_by(value, f), u)),
    }
}

fn sum(input: &[Value], scale: Scale) -> Value {
    // Quantities total to a typed quantity in the base unit —
    // mixed units of one dimension welcome (kW + W + BTU/h), unit
    // text lifted alongside; mixed dimensions refused.
    match quantital_fold(input, scale) {
        QuantFold::Unsound(clash) => return unsound_fold(&clash),
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
        return Value::seconds(secs, nanos.rem_euclid(1_000_000_000) as u32);
    }
    // the inputs read as `+` reads them: an integer-looking text is
    // an integer, so a column of such cells sums to an integer
    let readings: Vec<Value> = input.iter().filter_map(Value::numeric_reading).collect();
    if readings.is_empty() {
        return Value::Null;
    }
    if readings.iter().all(|v| matches!(v, Value::Int(_))) {
        let mut acc: i64 = 0;
        let mut overflowed = false;
        for v in &readings {
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
    Value::Float(readings.iter().filter_map(Value::numeric).sum())
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
            crate::value::ambient::record_refusal(
                "product: two quantities would multiply their units, which the unital \
                 fragment does not combine; take the magnitudes first"
                    .into(),
            );
            return Value::Null;
        }
    }
    // the inputs read as `x` reads them: an integer-looking text is
    // an integer
    let readings: Vec<Value> = input.iter().filter_map(Value::numeric_reading).collect();
    // the empty product is the fold's identity, 1
    if readings.iter().all(|v| matches!(v, Value::Int(_))) {
        let mut acc: i64 = 1;
        let mut overflowed = false;
        for v in &readings {
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

/// The statistics `describe` answers, in order (pandas' summary).
const DESCRIBE_ROWS: [&str; 8] = ["count", "mean", "std", "min", "25%", "50%", "75%", "max"];

/// `describe` over values (spec: Tables and Reshapes, draft 166): the
/// record of the eight statistics, null skipped as missing, `count`
/// the non-null count, `std` the sample deviation.
fn describe_values(input: &[Value], scale: Scale) -> Value {
    let present: Vec<Value> = input
        .iter()
        .filter(|v| !matches!(v, Value::Null))
        .cloned()
        .collect();
    let stat = |name: &str| -> Value {
        if present.is_empty() {
            return if name == "count" {
                Value::Int(0)
            } else {
                Value::Null
            };
        }
        match name {
            "count" => Value::Int(present.len() as i64),
            "mean" => mean(&present, scale),
            "std" => spread(&present, f64::sqrt, scale, 1),
            "min" => extreme(present.clone(), std::cmp::Ordering::Less, scale)
                .pop()
                .unwrap_or(Value::Null),
            "max" => extreme(present.clone(), std::cmp::Ordering::Greater, scale)
                .pop()
                .unwrap_or(Value::Null),
            "25%" => quantile(&present, 0.25, scale),
            "50%" => quantile(&present, 0.5, scale),
            "75%" => quantile(&present, 0.75, scale),
            _ => Value::Null,
        }
    };
    Value::record(
        DESCRIBE_ROWS
            .iter()
            .map(|n| (n.to_string(), stat(n)))
            .collect(),
    )
}

/// `autoindex(--start=x? --step=d?)` (ruling 2026-11-05): the implicit
/// index made explicit — a column named `index` of consecutive labels,
/// one per row in order, prepended and named the index. The positions
/// from 1 unless `--start` says where to begin (`--start=0` is pandas'
/// default index); the start a number, a quantity or an instant, the
/// step 1 by default for a number and required otherwise — a quantity
/// of the start's dimension, a duration for an instant — a step of
/// another dimension, or missing where required, refused naming the
/// start. An index the table had stays a plain column.
/// `autocast` (ruling 2026-11-06): a table's undeclared columns typed
/// again from the cells present, by the rule of the build — every
/// one, or the columns named bare (`autocast(:a; :b)`) — the named
/// form of `map(<*>)` / `map(<*>; --to=(:a; :b))`; a declared column
/// is left as declared, named or not. A list reads as one column; a
/// record's fields are not one column.
fn autocast(call: &FnCall, topic: Value) -> Result<Value, String> {
    match topic {
        Value::Table(t) => {
            let names: Vec<String> = (0..t.width()).map(|j| t.column_name(j)).collect();
            let mut only: Vec<usize> = Vec::new();
            for a in &call.args {
                // `:col` in an argument is the projector literal; the
                // field form is taken too
                let name: &str = match a {
                    Arg::Lit(Value::Projection {
                        key, typing: None, ..
                    }) => key.as_str(),
                    Arg::Lit(Value::Projection { key, .. }) => {
                        return Err(format!(
                            "autocast names a column bare (:{key}); it re-types from the cells, it declares nothing — declare a type with a typed projector instead"
                        ));
                    }
                    Arg::Expr(crate::syntax::ast::Operand::Field {
                        base,
                        name,
                        bare: true,
                    }) if matches!(**base, crate::syntax::ast::Operand::Topic) => name.as_str(),
                    _ => {
                        return Err(
                            "autocast names its columns bare: autocast(:a; :b), or alone for every undeclared column"
                                .into(),
                        );
                    }
                };
                let Some(j) = names.iter().position(|n| n == name) else {
                    return Err(format!(
                        "autocast names column '{name}', and the table has no such column (its columns: {})",
                        names.join(", ")
                    ));
                };
                // a declared column is left as declared (the final
                // word of 2026-11-06): the promise stands
                only.push(j);
            }
            Ok(Value::table(t.autocast(
                (!call.args.is_empty()).then_some(only.as_slice()),
            )))
        }
        Value::List(items) => {
            if !call.args.is_empty() {
                return Err(
                    "autocast names columns on a table; a list is one column, autocast alone"
                        .into(),
                );
            }
            let cells: Vec<Value> = items.iter().cloned().collect();
            if cells.iter().any(|c| !crate::stdlib::flat_cell(c)) {
                return Err(
                    "autocast reads a list as one column of scalars; this list holds a structure"
                        .into(),
                );
            }
            Ok(Value::list(crate::value::type_column(cells).1))
        }
        other => Err(format!(
            "autocast re-types a table's columns, or a list as one column; {} is neither",
            kind_of(&other)
        )),
    }
}

/// `clean` (the document-tables round, rulings 2026-11-08): a
/// scraped table's text columns cleaned into values, in the order a
/// hand would — the cell trimmed, a footnote mark dropped, a null
/// word null, a percent a ratio, a scale suffix applied, the number
/// read under the locale — every text column or the columns named
/// bare (`clean(:a; :b)`), the columns retyped; a text or a list of
/// texts cleaned the same way.
fn clean(call: &FnCall, topic: Value) -> Result<Value, String> {
    use crate::syntax::ast::Operand;
    let nulls: Vec<String> = call
        .args
        .iter()
        .find_map(|a| match a {
            Arg::Opt {
                name,
                value: Some(Operand::Lit(Value::List(l))),
            } if name == "--null" => Some(l.iter().map(|v| v.to_string()).collect()),
            Arg::Opt {
                name,
                value: Some(Operand::Lit(Value::Str(w))),
            } if name == "--null" => Some(vec![w.clone()]),
            // `--null=@()` (none) and a list the parser kept as items
            Arg::Opt {
                name,
                value: Some(Operand::List(items)),
            } if name == "--null" => Some(
                items
                    .iter()
                    .filter_map(|i| match i {
                        Operand::Lit(v) => Some(v.to_string()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| {
            crate::graph::tabula::DEFAULT_NULLS
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let locale: crate::value::number::Decimal = match call.args.iter().find_map(|a| match a {
        Arg::Opt {
            name,
            value: Some(Operand::Lit(Value::Str(s))),
        } if name == "--locale" => Some(s.clone()),
        _ => None,
    }) {
        Some(tag) => crate::value::number::locale_convention(&tag).ok_or_else(|| {
            format!(
                "clean(--locale={tag}): no number conventions for that locale (carried: {})",
                crate::value::locale::SHIPPED.join(", ")
            )
        })?,
        None => crate::value::number::Decimal::Point,
    };
    // `--sci`: the scientific reading (ruling 2026-11-10)
    let sci = call
        .args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, .. } if name == "--sci"));
    let cell = |v: &Value| -> Value {
        match v {
            Value::Str(s) => clean_text(s, &nulls, locale, sci),
            other => other.clone(),
        }
    };
    match topic {
        Value::Table(t) => {
            let names: Vec<String> = (0..t.width()).map(|j| t.column_name(j)).collect();
            let mut only: Vec<usize> = Vec::new();
            for a in &call.args {
                let name: &str = match a {
                    Arg::Opt { .. } => continue,
                    Arg::Lit(Value::Projection {
                        key, typing: None, ..
                    }) => key.as_str(),
                    // `:col` or `:"2023"`: the field of the topic, quoted
                    // or not
                    Arg::Expr(Operand::Field { base, name, .. })
                        if matches!(**base, Operand::Topic) =>
                    {
                        name.as_str()
                    }
                    _ => {
                        return Err(
                            "clean names its columns bare: clean(:a; :b), or alone for every text column"
                                .into(),
                        );
                    }
                };
                let Some(j) = names.iter().position(|n| n == name) else {
                    return Err(format!(
                        "clean names column '{name}', and the table has no such column (its columns: {})",
                        names.join(", ")
                    ));
                };
                only.push(j);
            }
            let types = t.types();
            let chosen: Vec<bool> = (0..t.width())
                .map(|j| {
                    if only.is_empty() {
                        types[j] == crate::value::ColumnType::Text
                    } else {
                        only.contains(&j)
                    }
                })
                .collect();
            let rows: Vec<Vec<Value>> = t
                .rows()
                .map(|r| {
                    r.cells()
                        .into_iter()
                        .enumerate()
                        .map(|(j, c)| if chosen[j] { cell(&c) } else { c })
                        .collect()
                })
                .collect();
            // the cleaned columns retype from their cells; a declared
            // column keeps its declaration
            let declared: Vec<Option<crate::value::ColumnType>> = (0..t.width())
                .map(|j| if chosen[j] { None } else { t.declaration(j) })
                .collect();
            let header: Option<Vec<String>> = t.header.clone();
            let cleaned = crate::Table::typed(header, rows, declared)?
                .with_index(t.index)
                .with_name(t.name.clone())
                .with_levels(t.levels);
            // the dirt the constructor warned of is cleaned: its
            // warning leaves the run (the document-tables round, F4)
            let cleaned_names: Vec<String> = (0..t.width())
                .filter(|&j| chosen[j])
                .map(|j| t.column_name(j))
                .collect();
            crate::value::ambient::retract_diagnostics(|d| {
                d.code == "TABLE_TEXT"
                    && cleaned_names.iter().any(|n| {
                        d.message
                            .starts_with(&format!("column '{n}' falls to text"))
                    })
            });
            // the title rules after the cells are clean (the
            // header-units round, 2026-11-09), the scoped options
            // (`--null:col`, `--locale:col`, `--unit:col`, `--units`)
            // with them
            let scopes = read_scopes(call)?;
            let cleaned = apply_scopes(&cleaned, &scopes)?;
            let site = t.name.clone().unwrap_or_else(|| "the table".to_string());
            let (autounits, slugs) = title_options(call)?;
            let autounits = autounits.or_else(|| scopes.implied_autounits());
            let cleaned = match (autounits, slugs) {
                (None, None) if scopes.declare_by.is_empty() => cleaned,
                (autounits, slugs) => title_pass(
                    &cleaned,
                    autounits.as_deref(),
                    slugs.as_deref(),
                    &site,
                    &scopes,
                )?,
            };
            Ok(Value::table(cleaned))
        }
        Value::List(items) => Ok(Value::list(items.iter().map(cell).collect())),
        Value::Str(s) => Ok(clean_text(&s, &nulls, locale, sci)),
        other => Ok(other),
    }
}

/// A power as it is written: `6.02 × 10^23`, the mantissa by ten to
/// the exponent, and `10^3`, `2^10`, a number to it. The sign of
/// multiplication is `×`, `x`, `·`, `⋅` or `*`; the exponent stands
/// after a caret, which is how a superscript reads here. An integer
/// where the number is one and the power is whole and fits. A unit
/// after it makes the quantity (`5.97 × 10^24 kg`).
pub(crate) fn power(text: &str, locale: crate::value::number::Decimal) -> Option<Value> {
    use crate::value::number::{canonical, read};
    let (left, right) = text.trim().rsplit_once('^')?;
    let right = right.trim_start();
    let (negative, digits) = match right.chars().next()? {
        c @ ('-' | '−' | '–') => (true, &right[c.len_utf8()..]),
        '+' => (false, &right[1..]),
        _ => (false, right),
    };
    let n_digits = digits.bytes().take_while(u8::is_ascii_digit).count();
    let exp: i32 = digits[..n_digits].parse().ok()?;
    if exp > 308 {
        return None;
    }
    let exp = if negative { -exp } else { exp };
    let unit = digits[n_digits..].trim();
    let left = left.trim_end();
    let whole = |v: f64| v.is_finite().then_some(Value::Float(v));
    let times_ten = left
        .strip_suffix("10")
        .map(str::trim_end)
        .and_then(|r| r.strip_suffix(['×', 'x', 'X', '·', '⋅', '*']));
    let number = match times_ten {
        // the mantissa by ten to the exponent, read as the one
        // number it spells (5.1 × 10^8 is 510000000, and no product
        // of two floats)
        Some(m) => {
            let c = canonical(m.trim(), locale)?;
            let int = c
                .parse::<i64>()
                .ok()
                .filter(|_| exp >= 0)
                .and_then(|m| 10i64.checked_pow(exp as u32).and_then(|p| m.checked_mul(p)));
            match int {
                Some(n) => Value::Int(n),
                None => whole(format!("{c}e{exp}").parse::<f64>().ok()?)?,
            }
        }
        None => match read(left, locale)? {
            Value::Int(b) if exp >= 0 => match b.checked_pow(exp as u32) {
                Some(n) => Value::Int(n),
                None => whole((b as f64).powi(exp))?,
            },
            other => whole(other.numeric()?.powi(exp))?,
        },
    };
    if unit.is_empty() {
        return Some(number);
    }
    // the number written out in full, as the unit reader takes it
    let spelled = match &number {
        Value::Float(f) => format!("{f}"),
        other => other.to_string(),
    };
    crate::value::quantity::parse_unit_text(&format!("{spelled} {unit}")).map(
        |(value, base, wv, wu)| Value::Quantity {
            value,
            base,
            written: Some((wv, wu)),
        },
    )
}

/// The superscript digits and signs of a text written out
/// (`m²` as `m^2`, `10⁻³` as `10^-3`), each run read as a
/// document's `sup` is (`tabula::raised`): an exponent written with
/// its caret, a footnote mark dropped.
fn written_superscripts(text: &str, sci: bool) -> String {
    let plain = |c: char| -> Option<char> {
        Some(match c {
            '⁰' => '0',
            '¹' => '1',
            '²' => '2',
            '³' => '3',
            '⁴' => '4',
            '⁵' => '5',
            '⁶' => '6',
            '⁷' => '7',
            '⁸' => '8',
            '⁹' => '9',
            '⁺' => '+',
            '⁻' => '-',
            _ => return None,
        })
    };
    if !text.chars().any(|c| plain(c).is_some()) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |out: &mut String, run: &mut String| {
        if run.is_empty() {
            return;
        }
        match crate::graph::tabula::raised(out, run, sci, false) {
            crate::graph::tabula::Raised::Exponent(n) => {
                out.push('^');
                out.push_str(&n);
            }
            crate::graph::tabula::Raised::Footnote => {}
            crate::graph::tabula::Raised::Other => out.push_str(run),
        }
        run.clear();
    };
    for c in text.chars() {
        match plain(c) {
            Some(p) => run.push(p),
            None => {
                flush(&mut out, &mut run);
                out.push(c);
            }
        }
    }
    flush(&mut out, &mut run);
    out
}

/// `correlate(t; --on=:col?; --inner? --first-wins? --last-wins?
/// --many?)` (ruling 2026-11-09): the foreign-key join. The topic's
/// key — the column `--on` names, its own index when omitted — is
/// matched, as `=` matches, against the argument table's index (one
/// level, set with `#(-i=:col)`); the answer is the topic's rows in
/// the topic's order with the topic's index kept and the argument's
/// data columns appended in the argument's order, its index not
/// repeated. An unmatched topic row keeps its row with the appended
/// cells null; `--inner` drops it. A title the topic already carries
/// lands as `title_2` (`_3`, …), told. A label repeated in the
/// argument's index refuses by default; `--first-wins` and
/// `--last-wins` take one row of it, `--many` every row (the topic's
/// row repeated, its index with it). One info line (CORRELATE): the
/// counts, the repeats taken, the renames.
fn correlate(call: &FnCall, topic: Value) -> Result<Value, String> {
    use crate::syntax::ast::Operand;
    let Value::Table(t) = &topic else {
        return Err(format!("correlate joins a table, not {}", kind_of(&topic)));
    };
    let u = match call.args.iter().find(|a| !matches!(a, Arg::Opt { .. })) {
        Some(Arg::Lit(Value::Table(u))) => u.clone(),
        Some(Arg::Lit(other)) => {
            return Err(format!(
                "the operand is {}, not a table: correlate($,t2; --on=:col)",
                kind_of(other)
            ));
        }
        _ => return Err("takes the table to join: correlate($,t2; --on=:col)".into()),
    };
    let flag = |n: &str| {
        call.args
            .iter()
            .any(|a| matches!(a, Arg::Opt { name, value: None } if name == n))
    };
    let (inner, first_wins, last_wins, many) = (
        flag("--inner"),
        flag("--first-wins"),
        flag("--last-wins"),
        flag("--many"),
    );
    if [first_wins, last_wins, many].iter().filter(|b| **b).count() > 1 {
        return Err(
            "--first-wins, --last-wins and --many each choose a way with a repeated label; name one"
                .into(),
        );
    }
    // `--on=:col`: the key column by name — a projection, a quoted
    // title, or the field of the topic as the parser read it
    fn column_named(v: &Value) -> Option<String> {
        match v {
            Value::Projection { key, .. } => Some(key.to_string()),
            Value::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
    let on: Option<String> = call
        .args
        .iter()
        .find_map(|a| match a {
            Arg::Opt { name, value } if name == "--on" => Some(match value {
                Some(Operand::Lit(Value::List(items))) if items.len() == 1 => {
                    column_named(&items[0])
                        .ok_or_else(|| "--on names the topic's key column (--on=:col)".to_string())
                }
                Some(Operand::List(items)) if items.len() == 1 => match &items[0] {
                    Operand::Lit(v) => column_named(v)
                        .ok_or_else(|| "--on names the topic's key column (--on=:col)".to_string()),
                    Operand::Field { base, name, .. } if matches!(**base, Operand::Topic) => {
                        Ok(name.clone())
                    }
                    _ => Err("--on names the topic's key column (--on=:col)".to_string()),
                },
                Some(Operand::Lit(Value::List(_))) | Some(Operand::List(_)) => {
                    Err("one key column for now: --on=:col".to_string())
                }
                Some(Operand::Lit(v)) => column_named(v)
                    .ok_or_else(|| "--on names the topic's key column (--on=:col)".to_string()),
                Some(Operand::Field { base, name, .. }) if matches!(**base, Operand::Topic) => {
                    Ok(name.clone())
                }
                Some(other) => {
                    let text = crate::syntax::unparse::operand(other);
                    match text.strip_prefix(':') {
                        Some(n) => Ok(n.trim_matches(['"', '\'']).to_string()),
                        None => Err(format!(
                            "--on names the topic's key column (--on=:col); {text} given"
                        )),
                    }
                }
                None => Err("--on names the topic's key column (--on=:col)".to_string()),
            }),
            _ => None,
        })
        .transpose()?;
    let names =
        |x: &crate::Table| -> Vec<String> { (0..x.width()).map(|j| x.column_name(j)).collect() };
    let (tn, un) = (names(t), names(&u));
    let key_col = match &on {
        Some(name) => tn.iter().position(|n| n == name).ok_or_else(|| {
            format!(
                "the topic has no column '{name}'; its columns: {}",
                tn.join(", ")
            )
        })?,
        None if t.index == 0 => {
            return Err(
                "the topic has no index to match on: name the key with --on=:col, or index it with #(-i=:col)"
                    .into(),
            );
        }
        None if t.index > 1 => {
            return Err(
                "one key column for now: the topic's index has several levels, name one with --on=:col"
                    .into(),
            );
        }
        None => 0,
    };
    if u.index == 0 {
        return Err("the operand's index is the key: set it with #(-i=:col)".into());
    }
    if u.index > 1 {
        return Err("one key column for now: the operand's index has several levels".into());
    }
    // the key and the operand's index agree on a type first: the
    // topic's column stands as the declared or undeclared side it is
    if let Some((ta, ua)) = agreed_keys("correlate", t, &u, &[(key_col, 0)])? {
        let mut agreed = call.clone();
        if let Some(slot) = agreed
            .args
            .iter_mut()
            .find(|a| !matches!(a, Arg::Opt { .. }))
        {
            *slot = Arg::Lit(Value::table(ua));
        }
        return correlate(&agreed, Value::table(ta));
    }
    // The key is matched as `=` compares (spec: Concatenation,
    // correlated tables): two integers exactly, a number and its
    // text, one instant under two offsets, one magnitude under two
    // units; `true` is not "true". A null matches nothing. The
    // operand's labels are gathered one entry per label `=` tells
    // apart, each with its rows in order.
    let unit = |e: &str| crate::value::ambient::unit_scale(e).flatten();
    let mut labels = crate::stdlib::compare::EqIndex::new(&unit);
    let mut by_label: Vec<Vec<usize>> = Vec::new();
    for r in 0..u.n_rows() {
        let l = u.cell(r, 0);
        if matches!(l, Value::Null) {
            continue;
        }
        match labels.matching(&l).first() {
            Some(&class) => by_label[class].push(r),
            None => {
                labels.push(l);
                by_label.push(vec![r]);
            }
        }
    }
    let repeated: Vec<(String, Vec<usize>)> = by_label
        .iter()
        .filter(|rows| rows.len() > 1)
        .map(|rows| (u.cell(rows[0], 0).to_string(), rows.clone()))
        .collect();
    if !repeated.is_empty() && !(first_wins || last_wins || many) {
        let (label, rows) = &repeated[0];
        return Err(format!(
            "the operand's index is not unique: '{label}' {} (rows {}){}; --first-wins or --last-wins take one row of a repeated label, --many every row",
            match rows.len() {
                2 => "twice".to_string(),
                n => format!("{n} times"),
            },
            rows.iter()
                .map(|r| (r + 1).to_string())
                .collect::<Vec<_>>()
                .join(" and "),
            if repeated.len() > 1 {
                format!(", and {} more", repeated.len() - 1)
            } else {
                String::new()
            }
        ));
    }
    // the appended columns: the argument's data columns, renamed where
    // the topic has the title
    let mut header = tn.clone();
    let mut declared: Vec<Option<crate::value::ColumnType>> =
        (0..t.width()).map(|j| t.declaration(j)).collect();
    let mut renamed: Vec<(String, String)> = Vec::new();
    for j in u.index..u.width() {
        let mut name = un[j].clone();
        if header.contains(&name) {
            let mut n = 2;
            while header.contains(&format!("{}_{n}", un[j])) {
                n += 1;
            }
            name = format!("{}_{n}", un[j]);
            renamed.push((un[j].clone(), name.clone()));
        }
        header.push(name);
        declared.push(u.declaration(j));
    }
    let appended: Vec<usize> = (u.index..u.width()).collect();
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(t.n_rows());
    let (mut matched, mut unmatched) = (0usize, 0usize);
    for i in 0..t.n_rows() {
        let left: Vec<Value> = (0..t.width()).map(|j| t.cell(i, j)).collect();
        let key = t.cell(i, key_col);
        let hits: Vec<usize> = if matches!(key, Value::Null) {
            Vec::new()
        } else {
            let mut rows: Vec<usize> = labels
                .matching(&key)
                .into_iter()
                .flat_map(|class| by_label[class].iter().copied())
                .collect();
            rows.sort_unstable();
            rows
        };
        if hits.is_empty() {
            unmatched += 1;
            if !inner {
                let mut row = left;
                row.extend(appended.iter().map(|_| Value::Null));
                rows.push(row);
            }
            continue;
        }
        matched += 1;
        let taken: Vec<usize> = if many {
            hits
        } else if last_wins {
            vec![*hits.last().expect("a hit")]
        } else {
            vec![hits[0]]
        };
        for r in taken {
            let mut row = left.clone();
            row.extend(appended.iter().map(|&j| u.cell(r, j)));
            rows.push(row);
        }
    }
    let site = t.name.clone().unwrap_or_else(|| "the table".to_string());
    let mut msg = format!(
        "correlate on {site}: {} row{}, {matched} matched, {unmatched} unmatched {}",
        t.n_rows(),
        if t.n_rows() == 1 { "" } else { "s" },
        if inner { "dropped" } else { "kept with nulls" }
    );
    if many && rows.len() != t.n_rows() {
        msg.push_str(&format!(
            "; --many: {} rows from {}",
            rows.len(),
            t.n_rows()
        ));
    }
    if !repeated.is_empty() && (first_wins || last_wins) {
        msg.push_str(&format!(
            "; repeated in the operand's index: {}, the {} row taken",
            repeated
                .iter()
                .map(|(l, r)| format!("'{l}' ×{}", r.len()))
                .collect::<Vec<_>>()
                .join(", "),
            if first_wins { "first" } else { "last" }
        ));
    }
    if !renamed.is_empty() {
        msg.push_str(&format!(
            "; renamed: {}",
            renamed
                .iter()
                .map(|(a, b)| format!("{a} → {b}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    crate::value::ambient::record_diagnostic(crate::Diagnostic::info(
        "CORRELATE",
        site.clone(),
        msg,
    ));
    Ok(Value::table(
        crate::Table::typed(Some(header), rows, declared)?
            .with_index(t.index)
            .with_name(t.name.clone())
            .with_levels(t.levels),
    ))
}

/// The column-scoped options and the word dictionary of a call (`#`,
/// `clean`): `--null:col=list`, `--locale:col=tag`, `--unit:col=_u`
/// (a declaration), `--unit:col=%(word = _u)` (the column's own
/// dictionary), `--units=%(word = _u)` (every column's) — the
/// header-units round, ruling 2026-11-09.
pub(crate) fn read_scopes(call: &FnCall) -> Result<crate::graph::tabula::ReadScopes, String> {
    use crate::graph::tabula::ReadScopes;
    use crate::syntax::ast::Operand;
    fn dictionary(v: &Value, name: &str) -> Result<Vec<(String, String)>, String> {
        let unit_of = |k: &str, u: &str| -> Result<(String, String), String> {
            let unit = u.trim_start_matches('_').to_string();
            if !crate::value::quantity::known_unit_expr(&unit) {
                return Err(format!(
                    "{name}: '{k}' maps to '{u}', and '{unit}' is no unit the table knows"
                ));
            }
            Ok((k.to_lowercase(), unit))
        };
        match v {
            Value::Record(r) => r
                .iter()
                .map(|(k, v)| match v {
                    Value::Str(u) if !u.is_empty() => unit_of(k, u),
                    Value::Projection { key, .. } => unit_of(k, &key.to_string()),
                    other => Err(format!(
                        "{name}: '{k}' maps to {}, not a unit in the suffix form (_lb)",
                        kind_of(other)
                    )),
                })
                .collect(),
            other => Err(format!(
                "{name} takes a record of word → unit (%(pounds = _lb)), not {}",
                kind_of(other)
            )),
        }
    }
    let mut scopes = ReadScopes::default();
    for a in &call.args {
        let Arg::Opt { name, value } = a else {
            continue;
        };
        let (base, scope) = crate::syntax::parser::split_option_scope(name);
        match (base, scope) {
            ("--units", None) => match value
                .clone()
                .and_then(crate::syntax::parser::record_operand)
            {
                Some(Operand::Lit(v)) => scopes.units.extend(dictionary(&v, name)?),
                _ => return Err(format!("{name} takes a record of word → unit")),
            },
            ("--unit", Some(col)) => match value {
                Some(Operand::Lit(Value::Str(u))) => {
                    scopes
                        .declare_by
                        .push((col.to_string(), u.trim_start_matches('_').to_string()));
                }
                Some(Operand::Lit(Value::Projection { key, .. })) => {
                    scopes
                        .declare_by
                        .push((col.to_string(), key.trim_start_matches('_').to_string()));
                }
                Some(op)
                    if let Some(Operand::Lit(v)) =
                        crate::syntax::parser::record_operand(op.clone()) =>
                {
                    scopes
                        .units_by
                        .push((col.to_string(), dictionary(&v, name)?));
                }
                _ => {
                    return Err(format!(
                        "{name} takes a unit in the suffix form (_lb) or a record of word → unit"
                    ));
                }
            },
            ("--unit", None) => {
                return Err(
                    "--unit takes a column: --unit:col=_lb, or --unit:col=%(word = _u); --units=%(…) maps every title's words"
                        .into(),
                );
            }
            ("--null", Some(col)) => {
                let words: Vec<String> = match value {
                    Some(Operand::Lit(Value::List(l))) => l.iter().map(|v| v.to_string()).collect(),
                    Some(Operand::Lit(Value::Str(w))) => vec![w.clone()],
                    Some(Operand::List(items)) => items
                        .iter()
                        .filter_map(|i| match i {
                            Operand::Lit(v) => Some(v.to_string()),
                            _ => None,
                        })
                        .collect(),
                    _ => {
                        return Err(format!(
                            "{name} takes the cell texts read as null, a list of strings"
                        ));
                    }
                };
                scopes.nulls_by.push((col.to_string(), words));
            }
            ("--locale", Some(col)) => {
                let tag = match value {
                    Some(Operand::Lit(Value::Str(t))) if !t.is_empty() => t.clone(),
                    _ => {
                        return Err(format!(
                            "{name} names the locale the numbers are written in, a tag quoted"
                        ));
                    }
                };
                let d = crate::value::number::locale_convention(&tag).ok_or_else(|| {
                    format!(
                        "{name}={tag}: no number conventions for that locale (carried: {})",
                        crate::value::locale::SHIPPED.join(", ")
                    )
                })?;
                scopes.locale_by.push((col.to_string(), d));
            }
            _ => {}
        }
    }
    Ok(scopes)
}

/// The scoped null words and locale applied to a table in hand: the
/// named columns' text cells, a null word null, a number under the
/// column's locale that number; the columns retype.
fn apply_scopes(
    t: &crate::Table,
    scopes: &crate::graph::tabula::ReadScopes,
) -> Result<crate::Table, String> {
    if scopes.nulls_by.is_empty() && scopes.locale_by.is_empty() {
        return Ok(t.clone());
    }
    let names: Vec<String> = (0..t.width()).map(|j| t.column_name(j)).collect();
    let unknown = scopes.unknown_columns(&names);
    if !unknown.is_empty() {
        return Err(format!(
            "{} no column: the table's columns are {}",
            unknown
                .iter()
                .map(|c| format!("'{c}' names"))
                .collect::<Vec<_>>()
                .join(", "),
            names.join(", ")
        ));
    }
    let rows: Vec<Vec<Value>> = t
        .rows()
        .map(|r| {
            r.cells()
                .into_iter()
                .enumerate()
                .map(|(j, c)| {
                    let normalized = match &c {
                        Value::Str(text) => scopes.normalize(&names[j], text),
                        _ => None,
                    };
                    match normalized {
                        Some(n) if n.is_empty() => Value::Null,
                        Some(n) => {
                            crate::value::number::read(&n, crate::value::number::Decimal::Point)
                                .unwrap_or(Value::Str(n))
                        }
                        None => c,
                    }
                })
                .collect()
        })
        .collect();
    let declared: Vec<Option<crate::value::ColumnType>> = (0..t.width())
        .map(|j| {
            if scopes.nulls_by.iter().any(|(c, _)| *c == names[j])
                || scopes.locale_by.iter().any(|(c, _)| *c == names[j])
            {
                None
            } else {
                t.declaration(j)
            }
        })
        .collect();
    Ok(crate::Table::typed(t.header.clone(), rows, declared)?
        .with_index(t.index)
        .with_name(t.name.clone())
        .with_levels(t.levels))
}

/// The title options of `#` and `clean` on a table in hand:
/// `--autounits` (every column), `--autounit=:col…` (the named),
/// `--slugs[=sep]` — as `(autounits, slugs)`: `None` off, an empty
/// list every column.
pub(crate) fn title_options(
    call: &FnCall,
) -> Result<(Option<Vec<String>>, Option<String>), String> {
    use crate::syntax::ast::Operand;
    fn names_of(v: &Operand) -> Vec<String> {
        fn of_value(v: &Value) -> Vec<String> {
            match v {
                Value::Projection { key, .. } => vec![key.to_string()],
                Value::Str(s) => vec![s.clone()],
                Value::List(items) => items.iter().flat_map(of_value).collect(),
                other => vec![other.to_string()],
            }
        }
        match v {
            Operand::Lit(v) => of_value(v),
            Operand::List(items) => items.iter().flat_map(names_of).collect(),
            Operand::Field { base, name, .. } if matches!(**base, Operand::Topic) => {
                vec![name.clone()]
            }
            other => vec![crate::syntax::unparse::operand(other)],
        }
    }
    let mut autounits: Option<Vec<String>> = None;
    let mut slugs: Option<String> = None;
    for a in &call.args {
        let Arg::Opt { name, value } = a else {
            continue;
        };
        match name.as_str() {
            "--autounits" => {
                autounits.get_or_insert_with(Vec::new);
            }
            "--autounit" => {
                let names = match value {
                    Some(v) => names_of(v),
                    None => Vec::new(),
                };
                if names.is_empty() {
                    return Err(format!(
                        "{}(--autounit=:col…) names the columns whose titles to read; --autounits reads every title",
                        call.name
                    ));
                }
                let have = autounits.get_or_insert_with(Vec::new);
                if have.is_empty() && autounits_is_every(call) {
                    // `--autounits --autounit=:x`: the flag already
                    // covers every column
                } else {
                    have.extend(names);
                }
            }
            "--slugs" => {
                slugs = Some(match value {
                    Some(Operand::Lit(Value::Str(sep))) if !sep.is_empty() => sep.clone(),
                    Some(Operand::Lit(Value::Str(_))) | None => "_".to_string(),
                    Some(other) => {
                        return Err(format!(
                            "{}(--slugs={}) takes a separator in quotes: --slugs=\"-\"",
                            call.name,
                            crate::syntax::unparse::operand(other)
                        ));
                    }
                });
            }
            _ => {}
        }
    }
    Ok((autounits, slugs))
}

fn autounits_is_every(call: &FnCall) -> bool {
    call.args
        .iter()
        .any(|a| matches!(a, Arg::Opt { name, .. } if name == "--autounits"))
}

/// The title rules over a table in hand (`#(--autounits)`,
/// `#(--autounit=:col…)`, `#(--slugs)`, and `clean`'s): a unit a
/// title names at its end read as the column's unit, the title
/// losing it, the column cast to that unit; a `%` title read as
/// ratios; the titles slugged; collisions refused; one info line
/// (the header-units round, 2026-11-09). The index and the name
/// kept.
pub(crate) fn title_pass(
    t: &crate::Table,
    autounits: Option<&[String]>,
    slugs: Option<&str>,
    site: &str,
    scopes: &crate::graph::tabula::ReadScopes,
) -> Result<crate::Table, String> {
    use crate::graph::autounits::title_plan_with;
    let Some(header) = &t.header else {
        return Err(format!(
            "{} reads the column titles, and the table has none: name them first (#(--header=…))",
            match (autounits, slugs) {
                (Some(_), Some(_)) => "--autounits with --slugs",
                (Some(_), None) => "--autounits",
                _ => "--slugs",
            }
        ));
    };
    let sample = |j: usize| -> Option<String> {
        (0..t.n_rows()).find_map(|i| match t.cell(i, j) {
            Value::Null => None,
            Value::Str(s) if s.trim().is_empty() => None,
            other => Some(other.to_string()),
        })
    };
    let plan = title_plan_with(
        header,
        autounits,
        slugs,
        &sample,
        site,
        &|column, word| scopes.resolve(column, word),
        &|column| scopes.declared(column),
    )?;
    let rows: Vec<Vec<Value>> = t
        .rows()
        .map(|r| {
            r.cells()
                .into_iter()
                .enumerate()
                .map(|(j, c)| plan.scale_cell(j, c))
                .collect()
        })
        .collect();
    // a converted column retypes from its cells and is then cast to
    // its unit; every other column keeps its declaration
    let declared: Vec<Option<crate::value::ColumnType>> = (0..t.width())
        .map(|j| {
            if plan.declared[j].is_some() {
                None
            } else {
                t.declaration(j)
            }
        })
        .collect();
    let casts: Vec<(String, crate::value::ColumnType)> = plan
        .declared
        .iter()
        .enumerate()
        .filter_map(|(j, d)| d.clone().map(|d| (plan.header[j].clone(), d)))
        .collect();
    let passed = crate::Table::typed(Some(plan.header.clone()), rows, declared)?
        .with_index(t.index)
        .with_name(t.name.clone())
        .with_levels(t.levels);
    if casts.is_empty() {
        Ok(passed)
    } else {
        passed.cast_columns(&casts)
    }
}

/// One text cell cleaned: trimmed; a footnote mark at its end
/// (`[1]`, `*`, `†`) dropped; a null word null; `12 %` the ratio
/// 0.12; `330.5 k` scaled (k, M, G/bn, mn); a number under the
/// locale; a power of ten (`6.02 × 10^23`); else the text as
/// trimmed. Superscript digits are read as a document's are
/// (`5 m²` five square metres, `10⁻³` a thousandth). `sci` is the
/// scientific reading (ruling 2026-11-10): a bare superscript digit
/// right after a plain number is an exponent (`10³` is 1000, a
/// footnote mark without it), and an uppercase K after a number is
/// the kelvin (the thousand without it).
pub(crate) fn clean_text(
    text: &str,
    nulls: &[String],
    locale: crate::value::number::Decimal,
    sci: bool,
) -> Value {
    use crate::value::number::read;
    let mut t = written_superscripts(text.trim(), sci).trim().to_string();
    // footnote marks: `[n]`, `[a]`, trailing daggers and stars
    loop {
        let before = t.len();
        let s = t.trim_end();
        if let Some(open) = s.rfind('[')
            && s.ends_with(']')
            && {
                let inner = &s[open + 1..s.len() - 1];
                !inner.is_empty()
                    && (inner.chars().all(|c| c.is_ascii_digit())
                        || (inner.chars().count() == 1
                            && inner.chars().all(|c| c.is_ascii_lowercase())))
            }
        {
            t = s[..open].trim_end().to_string();
        } else {
            t = s
                .trim_end_matches(['*', '†', '‡', '§', '¶'])
                .trim_end()
                .to_string();
        }
        if t.len() == before {
            break;
        }
    }
    if t.is_empty() || nulls.iter().any(|n| *n == t) {
        return Value::Null;
    }
    if let Some(v) = read(&t, locale) {
        return v;
    }
    // a percent: the quantity `12_%` (the percent unit, ruling
    // 2026-11-09), holding the ratio
    if let Some(p) = t.strip_suffix('%')
        && let Some(n) = read(p.trim(), locale).and_then(|v| v.numeric())
        && let Some((factor, base)) = crate::value::quantity::scale_expr("%")
    {
        return Value::Quantity {
            value: crate::value::quantity::scale_by(n, factor),
            base,
            written: Some((n, "%".to_string())),
        };
    }
    // a power as written, `6.02 × 10^23`, `10^-3`
    if let Some(v) = power(&t, locale) {
        return v;
    }
    // a scale suffix. An uppercase `K` is the thousand, as the
    // lowercase is, and under the scientific reading the kelvin and
    // no scale (the user, 2026-11-10: "with ?sci K is kelvin, without
    // it it's the current behavior"): there `300 K` goes on to the
    // unit rung below and reads as any trailing unit does, `300_K`.
    let scaled = [
        ("bn", 1e9),
        ("mn", 1e6),
        ("G", 1e9),
        ("M", 1e6),
        ("k", 1e3),
        ("K", 1e3),
    ]
    .iter()
    .filter(|(suffix, _)| !(sci && *suffix == "K"))
    .find_map(|(suffix, factor)| {
        t.strip_suffix(suffix)
            .map(str::trim_end)
            .and_then(|num| read(num, locale))
            .and_then(|v| v.numeric())
            .map(|n| n * factor)
    });
    if let Some(n) = scaled {
        return if n.fract() == 0.0 && n.abs() < 9.0e15 {
            Value::Int(n as i64)
        } else {
            Value::Float(n)
        };
    }
    // unit text (`12 km`, `5 metric tonnes`): the quantity (the
    // header-units round, the word road); a currency stays text here,
    // as it does in every spaced reading
    if let Some((value, base, wv, wu)) = crate::value::quantity::parse_unit_text(&t) {
        return Value::Quantity {
            value,
            base,
            written: Some((wv, wu)),
        };
    }
    Value::Str(t)
}

/// `-a|--autoindex` on the table constructor (ruling 2026-11-06):
/// what `| autoindex` does with no option.
pub(crate) fn autoindex_bare(t: &crate::Table) -> Result<crate::Table, String> {
    let bare = FnCall {
        headless: false,
        name: "autoindex".into(),
        args: Vec::new(),
    };
    match autoindex(&bare, t, &|_| None)? {
        Value::Table(t) => Ok(std::sync::Arc::try_unwrap(t).unwrap_or_else(|a| (*a).clone())),
        other => Err(format!(
            "autoindex answered {}, not a table",
            kind_of(&other)
        )),
    }
}

fn autoindex(call: &FnCall, t: &crate::Table, scale: Scale) -> Result<Value, String> {
    let option = |name: &str| -> Option<Value> {
        call.args.iter().find_map(|a| match a {
            Arg::Opt {
                name: n,
                value: Some(crate::syntax::ast::Operand::Lit(v)),
            } if n == name => Some(v.clone()),
            _ => None,
        })
    };
    // a date written bare reads as the instant it spells
    let read = |v: Value| match v {
        Value::Str(s) => crate::temporal::parse_iso(&s)
            .map(|(secs, nanos, offset_min)| Value::Instant {
                secs,
                nanos,
                offset_min,
                calendar: Default::default(),
            })
            .unwrap_or(Value::Str(s)),
        v => v,
    };
    let start = option("--start").map(read).unwrap_or(Value::Int(1));
    let step = option("--step").map(read);
    let numeric = |v: &Value| matches!(v, Value::Int(_) | Value::Float(_));
    let step = match (&start, step) {
        (s, None) if numeric(s) => Value::Int(1),
        (s, Some(d)) if numeric(s) && numeric(&d) => d,
        (Value::Quantity { base, .. }, Some(d)) if matches!(&d, Value::Quantity { base: b, .. } if b == base) => {
            d
        }
        (Value::Instant { .. }, Some(d)) if d.is_duration() => d,
        (s @ (Value::Quantity { .. } | Value::Instant { .. }), None) => {
            return Err(format!(
                "the start {} takes a step of its own kind: --step={}",
                s.to_quarb(),
                if s.is_duration() || matches!(s, Value::Instant { .. }) {
                    "1_d"
                } else {
                    "1_<unit>"
                }
            ));
        }
        (s, Some(d))
            if numeric(s) || matches!(s, Value::Quantity { .. } | Value::Instant { .. }) =>
        {
            return Err(format!(
                "the step {} is of another kind than the start {}: a number steps by a \
                 number, a quantity by a quantity of its dimension, an instant by a duration",
                d.to_quarb(),
                s.to_quarb()
            ));
        }
        (s, _) => {
            return Err(format!(
                "--start takes a number, a quantity or an instant, not {}",
                kind_of(s)
            ));
        }
    };
    // the labels, one per row, as a column of their own, named `ix`
    // (ruling 2026-11-06: unambiguous against `:::index` the concept,
    // short to type as `:ix`), prepended to the table as it stands —
    // no row made on the way
    let mut labels: Vec<Value> = Vec::with_capacity(t.n_rows());
    let mut label = start;
    for _ in 0..t.n_rows() {
        labels.push(label.clone());
        label = crate::stdlib::arith::arith(crate::syntax::ast::ArithOp::Add, &label, &step, scale);
    }
    Ok(Value::table(
        t.prepend_column("ix".to_string(), labels, None)
            .with_index(1),
    ))
}

/// `describe` on a table: a table with one row per statistic, indexed
/// by the statistic's name, one column per numeric column of the
/// topic; the other columns and the index are left out; a table with
/// no numeric column answers the empty table.
fn describe_table(t: &crate::Table, scale: Scale) -> Result<Value, String> {
    use crate::value::ColumnType;
    let types = t.types();
    let numeric: Vec<usize> = (t.index..t.width())
        .filter(|&j| matches!(types[j], ColumnType::Integer | ColumnType::Float))
        .collect();
    let mut header = vec!["statistic".to_string()];
    header.extend(numeric.iter().map(|&j| t.column_name(j)));
    if numeric.is_empty() {
        return Ok(Value::table(crate::Table::new(Some(header), Vec::new())));
    }
    let columns: Vec<Value> = numeric
        .iter()
        .map(|&j| {
            let cells: Vec<Value> = t.rows().map(|r| r.get(j)).collect();
            describe_values(&cells, scale)
        })
        .collect();
    let rows: Vec<Vec<Value>> = DESCRIBE_ROWS
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut row = vec![Value::Str(name.to_string())];
            row.extend(columns.iter().map(|c| {
                match c {
                    Value::Record(r) => r
                        .deref()
                        .get(i)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            }));
            row
        })
        .collect();
    Ok(Value::table(
        crate::Table::new(Some(header), rows)
            .with_index(1)
            .with_levels(t.levels),
    ))
}

fn mean(input: &[Value], scale: Scale) -> Value {
    // The mean of same-base quantities is a quantity on that base.
    match quantital_fold(input, scale) {
        QuantFold::Unsound(clash) => return unsound_fold(&clash),
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
        return Value::seconds(
            (avg.div_euclid(1_000_000_000)) as i64,
            avg.rem_euclid(1_000_000_000) as u32,
        );
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
        QuantFold::Unsound(clash) => return unsound_fold(&clash),
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
        QuantFold::Unsound(clash) => return unsound_fold(&clash),
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
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
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
        QuantFold::Unsound(clash) => return unsound_fold(&clash),
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
fn skipgrams_from(input: &[Value], start: usize, n: usize, k: usize, out: &mut Vec<Value>) {
    // A walk with its own stack: the recursion was as deep as the
    // gram is long, and `skipgrams(200000; 0)` overflowed the stack.
    // Per depth: the position chosen, the skips left after it, the
    // next skip to try.
    let mut chosen: Vec<usize> = vec![start];
    let mut left: Vec<usize> = vec![k];
    let mut next_skip: Vec<usize> = vec![0];
    while let Some(&at) = chosen.last() {
        let d = chosen.len() - 1;
        let skip = next_skip[d];
        let next = at + 1 + skip;
        if chosen.len() == n {
            out.push(Value::list(
                chosen.iter().map(|&i| input[i].clone()).collect(),
            ));
        } else if skip <= left[d] && next < input.len() {
            next_skip[d] += 1;
            chosen.push(next);
            left.push(left[d] - skip);
            next_skip.push(0);
            continue;
        }
        chosen.pop();
        left.pop();
        next_skip.pop();
    }
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
            Value::Str(s) => crate::value::quantity::parse_unit_text_with(s, scale).is_some(),
            Value::Quantity { .. } => true,
            _ => false,
        }
}

/// `min` / `max`: numeric reductions that coerce through the reading,
/// skip a value with no reading as missing (so a junk string can no
/// longer win via the comparison's string fallback), and reduce an
/// empty (or wholly readingless) input to null (spec).
/// A float NaN: the number that orders after every other and is
/// never an extreme.
pub(crate) fn is_nan(v: &Value) -> bool {
    matches!(v, Value::Float(f) if f.is_nan())
}

fn extreme(input: Vec<Value>, want: std::cmp::Ordering, scale: Scale) -> Vec<Value> {
    // Ordering across dimensions is meaningless: a min over watts
    // and meters would pick by raw magnitude. Refuse, like the
    // numeric folds.
    // (unit text beside a bare number orders by the unital
    // comparison below; the fold's refusal is for typed quantities)
    if input.iter().any(|v| matches!(v, Value::Quantity { .. }))
        && let QuantFold::Unsound(clash) = quantital_fold(&input, scale)
    {
        // refused in the folds' words, where it answered null: two
        // dimensions, or a number without a unit beside a quantity
        return vec![unsound_fold(&clash)];
    }
    // NaN is never an extreme: it orders after every other number
    // and a stream's least or greatest is a number.
    match input
        .into_iter()
        .filter(|v| has_reading(v, scale) && !is_nan(v))
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
    let raw: Vec<&str> = match crate::value::ambient::ambient(|a| a.bonds.clone()) {
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
            crate::value::ambient::record_refusal(format!("{}: {why}", call.name));
            Value::Null
        }
    }
}

/// The values with every library value replaced by its rows, each
/// row a value of the stream — what a serialization reads.
fn exported(input: Vec<Value>) -> Vec<Value> {
    if !input
        .iter()
        .any(|v| matches!(v, Value::Foreign(_) | Value::Table(_)))
    {
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
const TYPES_RETIRED: &str = "'types' is retired: the type of a table's columns is 'type'";

/// `type`: the type of a value as one word (spec: Quantity Commands);
/// a table's one word per column, in header order.
pub(crate) fn type_of(v: &Value) -> Value {
    match v {
        Value::Table(t) => t.types_value(),
        Value::Null => Value::Str("null".into()),
        Value::Bool(_) => Value::Str("bool".into()),
        Value::Int(_) => Value::Str("int".into()),
        Value::Float(_) => Value::Str("float".into()),
        Value::Str(_) => Value::Str("text".into()),
        Value::Instant { .. } => Value::Str("instant".into()),
        Value::Quantity { base, .. } if base == "s" => Value::Str("duration".into()),
        Value::Period { .. } => Value::Str("period".into()),
        Value::Quantity { .. } => Value::Str("quantity".into()),
        Value::List(_) => Value::Str("list".into()),
        Value::Record(_) => Value::Str("record".into()),
        Value::Foreign(_) => Value::Str("opaque".into()),
        Value::Node(_) => Value::Str("node".into()),
        Value::Projection { .. } => Value::Str("projector".into()),
        Value::Quarq(_) => Value::Str("quarq".into()),
    }
}

pub(crate) fn kind_of(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "a boolean".into(),
        Value::Int(_) | Value::Float(_) => "a number".into(),
        Value::Str(_) => "text".into(),
        Value::List(_) => "a list".into(),
        Value::Record(_) => "a record".into(),
        Value::Instant { .. } => "an instant".into(),
        Value::Quantity { base, .. } if base == "s" => "a duration".into(),
        Value::Period { .. } => "a period".into(),
        Value::Quantity { .. } => "a quantity".into(),
        Value::Foreign(f) => f.kind(),
        Value::Table(t) => t.kind(),
        Value::Node(_) => "a node reference".into(),
        Value::Projection { .. } => "a projector".into(),
        Value::Quarq(_) => "a quarq".into(),
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
        .rows()
        .map(|r| {
            Value::Record(
                keys.iter()
                    .cloned()
                    .zip(r.iter().chain(std::iter::repeat(Value::Null)))
                    .collect(),
            )
        })
        .collect();
    tabulate_with(&rows, columns, csv, delimiter)
}

/// A table drawn by `pretty`: [`tabulate_table`] with the floats
/// rounded for the drawing and the numeric columns right-aligned.
pub(crate) fn tabulate_table_pretty(
    t: &crate::Table,
    columns: Option<&[String]>,
    delimiter: u8,
    precision: usize,
) -> String {
    let rows = table_records(t);
    tabulate_pretty(
        &rows,
        columns,
        delimiter,
        &|v| match v {
            Value::Null => String::new(),
            Value::Str(s) => s.clone(),
            other => other.display_form(),
        },
        precision,
    )
}

/// A table's rows as records under its column names (positions on
/// the array face), the shape the grid reads.
fn table_records(t: &crate::Table) -> Vec<Value> {
    let keys: Vec<String> = match &t.header {
        Some(h) => h.clone(),
        None => (1..=t.width()).map(|j| j.to_string()).collect(),
    };
    t.rows()
        .map(|r| {
            Value::Record(
                keys.iter()
                    .cloned()
                    .zip(r.iter().chain(std::iter::repeat(Value::Null)))
                    .collect(),
            )
        })
        .collect()
}

/// `| pretty` — a value pretty-printed: a table as the grid, a record
/// one field per line at every depth (a nested record opens under
/// its field, indented two spaces per level), a list on one line
/// when it fits and holds no record, else one item per line, a
/// scalar as it is.
pub fn pretty_value(v: &Value, precision: usize) -> String {
    match v {
        Value::Table(t) => tabulate_table_pretty(t, None, b',', precision),
        // records of one shape print as the grid the output form
        // tabulates them to (draft 166)
        Value::List(items) if one_shape(items, 2) => match table_reading(v.clone()) {
            Ok(Value::Table(t)) => tabulate_table_pretty(&t, None, b',', precision),
            _ => pretty_at(v, 0, precision),
        },
        Value::Record(_) | Value::List(_) => pretty_at(v, 0, precision),
        Value::Float(f) => crate::value::pretty_float(*f, precision),
        other => other.display_form(),
    }
}

/// The width a list may take on one line, from its indent.
const PRETTY_WIDTH: usize = 72;

/// A record or a list pretty-printed with its opening sigil at
/// column `col`: the items on lines of their own, each starting
/// under the first (two past the sigil), so a nested record's keys
/// align under its first key wherever the field began; the closing
/// parenthesis after the last item.
fn pretty_at(v: &Value, col: usize, precision: usize) -> String {
    let pad = " ".repeat(col + 2);
    match v {
        Value::Record(fields) => {
            if fields.is_empty() {
                return "%()".to_string();
            }
            let inner: Vec<String> = fields
                .iter()
                .enumerate()
                .map(|(i, (_, v))| {
                    let key = fields.key_quarb(i);
                    // the value's sigil stands after `key = `
                    let at = col + 2 + key.chars().count() + 3;
                    format!("{key} = {}", pretty_at(v, at, precision))
                })
                .collect();
            format!("%({})", inner.join(&format!(";\n{pad}")))
        }
        Value::List(items) => {
            let line = pretty_line(v, precision);
            let opens = items
                .iter()
                .any(|i| matches!(i, Value::Record(f) if !f.is_empty()))
                || col + line.chars().count() > PRETTY_WIDTH;
            if !opens {
                return line;
            }
            let inner: Vec<String> = items
                .iter()
                .map(|i| pretty_at(i, col + 2, precision))
                .collect();
            format!("@({})", inner.join(&format!(";\n{pad}")))
        }
        Value::Float(f) => crate::value::pretty_float(*f, precision),
        other => other.to_quarb(),
    }
}

/// A list on one line as `pretty` draws it: the quarb form with the
/// floats rounded for the drawing.
fn pretty_line(v: &Value, precision: usize) -> String {
    match v {
        Value::List(items) => format!(
            "@({})",
            items
                .iter()
                .map(|i| pretty_line(i, precision))
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Value::Float(f) => crate::value::pretty_float(*f, precision),
        other => other.to_quarb(),
    }
}

/// What a table refuses with: the two readings it admits; `sort`,
/// which has no key, points at the keyed sort of the rows.
fn table_refusal(t: &crate::Table, command: &str) -> String {
    if command == "sort" {
        return format!(
            "sort: {} is a table and sorts by a column: '@| sort_by(:col)', '@| sort_by(:col; --desc)'",
            t.kind()
        );
    }
    format!(
        "{command}: {} is a table; '| @' reads its rows, '| %' its columns",
        t.kind()
    )
}

/// The row reading of the ordering reductions on one table (spec:
/// The Table): `reverse` the rows reversed, `unique` the distinct
/// rows, each a table under the same header; `first` / `last` the
/// first / last row as the cursor record (what `| [1]` and `| [-1]`
/// answer), or with a count or a rank band those rows as a table.
/// `None` for any other command.
pub(crate) fn table_order_reading(
    call: &FnCall,
    t: &crate::Table,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<Value> {
    let rows_table = |rows: Vec<Vec<Value>>| Value::table(t.keep_rows(rows));
    let row_record = |row: &[Value]| {
        Value::Record(match &t.header {
            Some(h) => h.iter().cloned().zip(row.iter().cloned()).collect(),
            None => row
                .iter()
                .cloned()
                .enumerate()
                .map(|(j, v)| ((j + 1).to_string(), v))
                .collect(),
        })
    };
    Some(match call.name.as_str() {
        "reverse" => {
            let mut rows = t.rows_vec();
            rows.reverse();
            rows_table(rows)
        }
        "unique" => {
            let mut seen: std::collections::HashSet<Vec<crate::value::SortKey>> =
                std::collections::HashSet::new();
            let rows = t
                .rows()
                .filter(|r| seen.insert(r.iter().map(|v| v.sort_key_with(scale)).collect()))
                .map(|r| r.cells())
                .collect();
            rows_table(rows)
        }
        "first" | "last" => {
            let from_end = call.name == "last";
            match call.args.first() {
                None => match if from_end {
                    t.n_rows().checked_sub(1).and_then(|i| t.row_at(i))
                } else {
                    t.row_at(0)
                } {
                    Some(row) => row_record(&row.cells()),
                    None => Value::Null,
                },
                arg => {
                    let (s, e) = rank_span(arg, t.n_rows(), from_end).unwrap_or((0, 0));
                    rows_table((s..e).map(|i| t.row_cells(i)).collect())
                }
            }
        }
        _ => return None,
    })
}

/// The ordering reductions on one record (ruling 2026-11-05: a
/// record takes positions as a list of its fields does, keys kept):
/// `first` / `last` the first / last VALUE, with a count or a rank
/// band the record of those fields; `reverse` the fields reversed.
/// `None` for any other command.
pub(crate) fn record_order_reading(call: &FnCall, r: &crate::value::Record) -> Option<Value> {
    let fields: Vec<(String, Value)> = r.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    // the keys kept as the values they are, and the declarations
    let slice = |s: usize, e: usize| Value::Record(r.pick(s..e));
    Some(match call.name.as_str() {
        "reverse" => Value::Record(r.pick((0..r.len()).rev())),
        "first" | "last" => {
            let from_end = call.name == "last";
            match call.args.first() {
                None => match if from_end {
                    fields.last()
                } else {
                    fields.first()
                } {
                    Some((_, v)) => v.clone(),
                    None => Value::Null,
                },
                arg => {
                    let (s, e) = rank_span(arg, fields.len(), from_end).unwrap_or((0, 0));
                    slice(s, e)
                }
            }
        }
        _ => return None,
    })
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
        // the column view builds the table back (spec: Table views,
        // draft 166): a record whose fields are lists of one length
        // is the frame, a column per field in the record's order; a
        // scalar field repeats down the rows; a declared key is a
        // declared column; lists of two lengths refuse naming them
        Value::Record(fields) => {
            if !fields.is_empty() && fields.iter().any(|(_, v)| matches!(v, Value::List(_))) {
                let mut seen: Option<(usize, String)> = None;
                for (k, v) in fields.iter() {
                    if let Value::List(l) = v {
                        match &seen {
                            None => seen = Some((l.len(), k.clone())),
                            Some((m, k0)) if *m != l.len() => {
                                return Err(format!(
                                    "field '{k0}' holds {m} values, '{k}' holds {}; the columns must be of one length",
                                    l.len()
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                let n = seen.map(|(m, _)| m).unwrap_or(1);
                let header: Vec<(String, Option<crate::value::ColumnType>)> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| (k.clone(), fields.declaration(i)))
                    .collect();
                let rows: Vec<Vec<Value>> = (0..n)
                    .map(|i| {
                        fields
                            .iter()
                            .map(|(_, v)| match v {
                                Value::List(l) => l.get(i).cloned().unwrap_or(Value::Null),
                                other => other.clone(),
                            })
                            .collect()
                    })
                    .collect();
                return crate::Table::declared(header, rows).map(Value::table);
            }
            table_reading(Value::list(vec![Value::Record(fields)]))
        }
        Value::List(items) => {
            if items.is_empty() {
                return Ok(Value::table(crate::Table::new(None, Vec::new())));
            }
            if items.iter().all(|v| matches!(v, Value::Record(_))) {
                let mut header: Vec<String> = Vec::new();
                // the keys' declarations ride into the columns (ruling
                // 2026-11-06): a key declared on some records and bare on
                // others takes the declaration, two different ones refuse
                let mut declared: Vec<Option<crate::value::ColumnType>> = Vec::new();
                for (i, v) in items.iter().enumerate() {
                    if let Value::Record(f) = v {
                        for (fi, (k, v)) in f.iter().enumerate() {
                            let j = match header.iter().position(|h| h == k) {
                                Some(j) => j,
                                None => {
                                    header.push(k.clone());
                                    declared.push(None);
                                    header.len() - 1
                                }
                            };
                            if let Some(ty) = f.declaration(fi) {
                                match &declared[j] {
                                    Some(have) if *have != ty => {
                                        return Err(format!(
                                            "key '{k}' is declared {} on one record and {} on another: the records must agree",
                                            have.word(),
                                            ty.word()
                                        ));
                                    }
                                    _ => declared[j] = Some(ty),
                                }
                            }
                            // The table is flat: a record whose
                            // field holds a structure keeps its
                            // shape as a record.
                            if !flat_cell(v) {
                                return Err(format!(
                                    "a table is flat, its cell a scalar or null: '{k}' holds {} in row {}; the records keep their shape",
                                    kind_of(v),
                                    i + 1
                                ));
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
                return crate::Table::typed(Some(header), rows, declared).map(Value::table);
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
                    if let Some((j, v)) = l.iter().enumerate().find(|(_, v)| !flat_cell(v)) {
                        return Err(format!(
                            "a table is flat, its cell a scalar or null: row {}, column {} holds {}; the lists keep their shape",
                            i + 1,
                            j + 1,
                            kind_of(v)
                        ));
                    }
                    let mut row = l.to_vec();
                    row.resize(width, Value::Null);
                    rows.push(row);
                }
                return Ok(Value::table(crate::Table::new(None, rows)));
            }
            if items.iter().all(flat_cell) {
                return Ok(Value::table(crate::Table::new(
                    None,
                    items.iter().map(|v| vec![v.clone()]).collect(),
                )));
            }
            Err("a table reads a list of records (a frame), a list of lists (an array) or a list of values (a sample); the rows here are of mixed kinds".into())
        }
        other => Err(format!(
            "a table reads a list of records, a list of lists or a record of lists, not {}",
            kind_of(&other)
        )),
    }
}

/// The concatenation's shape rule (spec: Concatenation): `right`
/// joined to `left`, the left side giving the shape. Under `++` a
/// list concatenates a list and takes any other value as one
/// element; a table takes a record as one more row under the
/// literal's rules and a table row by row (columns by name on a
/// frame, by position on an array), its columns re-typed; a record
/// takes a record's fields in order, a duplicate name refused; a
/// text takes the right side's text (a structure by its output
/// form). `++!` is for records and lets a field be replaced; `+@+`
/// is for lists and takes the right side as one item whatever it
/// is. Null on either side is null, as in arithmetic; any other
/// left side is refused by kind.
pub(crate) fn concat(mode: ConcatMode, left: Value, right: Value) -> Result<Value, String> {
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(Value::Null);
    }
    match (mode, left) {
        (ConcatMode::Item, Value::List(l)) => {
            let mut out = l.to_vec();
            out.push(right);
            Ok(Value::list(out))
        }
        (ConcatMode::Item, other) => Err(format!(
            "'+@+' appends one item to a list; {} takes '++'",
            kind_of(&other)
        )),
        (ConcatMode::Overwrite, Value::Record(fields)) => record_concat(fields, right, true),
        // `t ++! u` — the right side's columns appended, overwriting
        // the left's of one name (the header rule, ruling 2026-11-05)
        (ConcatMode::Overwrite, Value::Table(t)) if matches!(&right, Value::Table(_)) => {
            table_concat(&t, right, true)
        }
        (ConcatMode::Overwrite, Value::Table(_)) => Err(format!(
            "'++!' on a table takes a table, its columns under the left side's names; {} takes '++'",
            kind_of(&right)
        )),
        (ConcatMode::Overwrite, other) => Err(format!(
            "'++!' overwrites a record's fields; {} takes '++'",
            kind_of(&other)
        )),
        (ConcatMode::Merge, Value::List(l)) => {
            let mut out = l.to_vec();
            match right {
                Value::List(more) => out.extend(more.iter().cloned()),
                other => out.push(other),
            }
            Ok(Value::list(out))
        }
        (ConcatMode::Merge, Value::Table(t)) => table_concat(&t, right, false),
        (ConcatMode::Merge, Value::Record(fields)) => record_concat(fields, right, false),
        (ConcatMode::Merge, Value::Str(s)) => {
            Ok(Value::Str(format!("{s}{}", right.display_form())))
        }
        (ConcatMode::Merge, other) => Err(format!(
            "{} takes no concatenation: the left side of '++' is a list, a record, a table or a text, which gives the shape",
            kind_of(&other)
        )),
    }
}

/// A record's fields joined by a record's, in order; a duplicate
/// name is refused under `++` and replaced under `++!`.
fn record_concat(
    mut fields: crate::value::Record,
    right: Value,
    overwrite: bool,
) -> Result<Value, String> {
    let Value::Record(more) = right else {
        return Err(format!(
            "a record appends fields; {} has no name to be one",
            kind_of(&right)
        ));
    };
    // the keys compared as values (1 and "1" two fields); a declared
    // field casts what `++!` sets into it or refuses (draft 166)
    for i in 0..more.len() {
        let key = more.key(i);
        let v = more[i].1.clone();
        match fields.find_key(&key) {
            Some(j) if overwrite => {
                let v = match fields.declaration(j) {
                    Some(ty) => crate::value::cast_cell(&v, &ty).ok_or_else(|| {
                        format!(
                            "field '{}' is declared {}, and {} does not cast to it",
                            fields[j].0,
                            ty.word(),
                            crate::value::held_text(&v)
                        )
                    })?,
                    None => v,
                };
                fields.set(j, v);
            }
            Some(_) => {
                return Err(format!(
                    "'{}' is already a field of the record; '++' never overwrites ('++!' does)",
                    crate::value::Record::key_text(&key)
                ));
            }
            None => fields.push_keyed(key, v, more.declaration(i)),
        }
    }
    Ok(Value::Record(fields))
}

/// A table's rows joined by a record (one row under the literal's
/// rules), or by a table under the header rule (ruling 2026-11-05):
/// the same header appends the right side's rows, by column name on
/// a frame and by position on an array, the index kept; disjoint
/// headers append its columns (`column_concat`: by label where both
/// tables are indexed, by position otherwise); headers that overlap
/// without being equal are refused naming the shared columns. Two
/// indexed tables compare their data columns for the overlap, their
/// index being the alignment. `++!` follows the same rule and
/// overwrites where `++` would repeat or refuse: under the same
/// header a right row whose label the left has replaces that row in
/// place (the update), under overlapping headers the right side's
/// values take the shared names.
fn table_concat(t: &crate::Table, right: Value, overwrite: bool) -> Result<Value, String> {
    // Rows appended under the header of an indexed table bring their
    // labels in the columns the index names, and `++!` updates by
    // them: the two meet as indexes do, and agree on a type first.
    if let Value::Table(more) = &right
        && t.index > 0
        && let (Some(h1), Some(h2)) = (&t.header, &more.header)
        && h1.len() == h2.len()
        && h1.iter().all(|c| h2.contains(c))
    {
        let pairs: Vec<(usize, usize)> = (0..t.index)
            .filter_map(|j| h2.iter().position(|c| *c == h1[j]).map(|r| (j, r)))
            .collect();
        let command = if overwrite { "++!" } else { "++" };
        if let Some((ta, ua)) = agreed_keys(command, t, more, &pairs)? {
            return table_concat(&ta, Value::table(ua), overwrite);
        }
    }
    let mut header = t.header.clone();
    let mut rows = t.rows_vec();
    let n = rows.len() + 1;
    match right {
        Value::Record(fields) => {
            let row = place_named(&mut header, &mut rows, &fields, n)?;
            rows.push(row);
        }
        Value::Table(more) => match (&header, &more.header) {
            (Some(h1), Some(h2)) => {
                let same = h1.len() == h2.len() && h1.iter().all(|c| h2.contains(c));
                if same {
                    // the right's label, read in its columns named like
                    // the left's index levels, for the update
                    let levels: Vec<usize> = if overwrite && t.index > 0 {
                        (0..t.index)
                            .map(|j| h2.iter().position(|c| *c == h1[j]).unwrap_or(0))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let label = |r: &Vec<Value>, cols: &[usize]| -> Vec<Value> {
                        cols.iter()
                            .map(|&j| r.get(j).cloned().unwrap_or(Value::Null))
                            .collect()
                    };
                    // the rows by (label, its n-th occurrence), the
                    // labels equal as values (spec: Indexed tables)
                    let mut classes = crate::value::LabelIndex::new();
                    let mut seen_left = crate::value::Occurrences::new();
                    let mut at: std::collections::HashMap<(usize, usize), usize> =
                        std::collections::HashMap::new();
                    if !levels.is_empty() {
                        let own: Vec<usize> = (0..t.index).collect();
                        for (i, r) in rows.iter().enumerate() {
                            let l = label(r, &own);
                            at.insert(seen_left.next(&mut classes, &l), i);
                        }
                    }
                    let mut seen_right = crate::value::Occurrences::new();
                    for r in more.rows() {
                        let fields: Vec<(String, Value)> =
                            h2.iter().cloned().zip(r.iter()).collect();
                        let n = rows.len() + 1;
                        let row = place_named(&mut header, &mut rows, &fields, n)?;
                        if levels.is_empty() {
                            rows.push(row);
                            continue;
                        }
                        let l = label(&r.cells(), &levels);
                        let slot = seen_right.next(&mut classes, &l);
                        match at.get(&slot) {
                            Some(&i) => rows[i] = row,
                            None => {
                                at.insert(slot, rows.len());
                                rows.push(row);
                            }
                        }
                    }
                } else {
                    // the data columns where both are indexed (the
                    // index aligns, it is not a column to repeat), the
                    // whole header otherwise
                    let both = t.index > 0 && more.index > 0;
                    let (left, right): (&[String], &[String]) = if both {
                        (&h1[t.index..], &h2[more.index..])
                    } else {
                        (h1, h2)
                    };
                    let shared: Vec<&String> = right.iter().filter(|c| left.contains(c)).collect();
                    if shared.is_empty() || overwrite {
                        return column_concat(t, &more, overwrite);
                    }
                    return Err(format!(
                        "the headers share {} but are not the same: one header appends rows, disjoint headers append columns; '++!' appends the columns and takes the right side's under the shared name{}",
                        shared
                            .iter()
                            .map(|c| format!("'{c}'"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        if shared.len() == 1 { "" } else { "s" }
                    ));
                }
            }
            (None, None) => {
                let w = t.width();
                for r in more.rows() {
                    let n = rows.len() + 1;
                    if r.len() > w {
                        return Err(format!(
                            "row {n} has {} values, the table {w} columns",
                            r.len()
                        ));
                    }
                    let mut row = r.cells();
                    row.resize(w, Value::Null);
                    rows.push(row);
                }
            }
            (Some(_), None) => {
                return Err("a frame takes rows by column name; the array appended has none (give it a header)".into());
            }
            (None, Some(_)) => {
                return Err("an array takes rows by position; the frame appended names its columns (take its rows with '| @' first)".into());
            }
        },
        other => {
            return Err(format!(
                "a table appends a record as a row, or a table row by row, not {}",
                kind_of(&other)
            ));
        }
    }
    // a declared column casts the appended cells or refuses naming
    // it; a column the header gained is undeclared
    Ok(Value::table(
        crate::Table::typed(header, rows, t.declared.clone())?
            .with_index(t.index)
            .with_name(t.name.clone())
            .with_levels(t.levels),
    ))
}

/// `t ++ u` with disjoint headers, or `t ++! u` with headers that
/// overlap (ruling 2026-11-05): the right side's columns appended,
/// the rows aligned by label where both tables are indexed
/// (`labeled_concat`) and by position otherwise, on tables of one
/// height; the left side's index and name kept.
fn column_concat(t: &crate::Table, u: &crate::Table, overwrite: bool) -> Result<Value, String> {
    if t.index > 0 && u.index > 0 {
        return labeled_concat(t, u, overwrite);
    }
    if t.n_rows() != u.n_rows() {
        return Err(format!(
            "the left table has {} row{}, the right {}; columns append by position on tables of one height (index both to align them by label)",
            t.n_rows(),
            if t.n_rows() == 1 { "" } else { "s" },
            u.n_rows()
        ));
    }
    let names =
        |x: &crate::Table| -> Vec<String> { (0..x.width()).map(|j| x.column_name(j)).collect() };
    let (tn, un) = (names(t), names(u));
    let mut header: Vec<String> = tn.clone();
    let mut declared: Vec<Option<crate::ColumnType>> =
        (0..t.width()).map(|j| t.declaration(j)).collect();
    let mut right_cols: Vec<(usize, usize)> = Vec::new(); // (column of u, column of the result)
    for j in 0..u.width() {
        match tn.iter().position(|n| *n == un[j]) {
            Some(p) if overwrite => {
                right_cols.push((j, p));
                declared[p] = u.declaration(j);
            }
            Some(_) => {
                return Err(format!(
                    "both tables have a column '{}'; '++!' takes the right side's",
                    un[j]
                ));
            }
            None => {
                header.push(un[j].clone());
                declared.push(u.declaration(j));
                right_cols.push((j, header.len() - 1));
            }
        }
    }
    let width = header.len();
    let rows: Vec<Vec<Value>> = t
        .rows()
        .zip(u.rows())
        .map(|(l, r)| {
            let mut row = l.cells();
            row.resize(width, Value::Null);
            for &(j, to) in &right_cols {
                row[to] = r.get(j);
            }
            row
        })
        .collect();
    Ok(Value::table(
        crate::Table::typed(Some(header), rows, declared)?
            .with_index(t.index)
            .with_name(t.name.clone())
            .with_levels(t.levels || u.levels),
    ))
}

/// Two indexes that meet agree on a type, level by level, before
/// their labels are compared (ruling 2026-11-10). `pairs` names the
/// key columns that meet, one of `t` with one of `u`:
///
/// - a declared one against an undeclared: the declared type wins,
///   the other side's labels are cast to it, and a label that does
///   not cast refuses, naming it;
/// - two undeclared: both are upcast to their common type, the
///   least type every label of the two reads as (an integer with a
///   float is a float, anything with text is text);
/// - two declared: the same type, or an integer with a float, which
///   meet as floats; any other pair refuses, naming both.
///
/// `None` where nothing changes; else the two tables with their key
/// columns cast, declared where either side declared them. (The
/// labels were compared as values across the two columns' types, so
/// 1 against "1" were two labels and a declaration bound its own
/// side alone.)
pub(crate) fn agreed_keys(
    command: &str,
    t: &crate::Table,
    u: &crate::Table,
    pairs: &[(usize, usize)],
) -> Result<Option<(crate::Table, crate::Table)>, String> {
    use crate::value::ColumnType as T;
    let mut out: Option<(crate::Table, crate::Table)> = None;
    for &(lj, rj) in pairs {
        let (left, right) = match &out {
            Some((a, b)) => (a, b),
            None => (t, u),
        };
        let (lcells, rcells) = (left.column_cells(lj), right.column_cells(rj));
        // what each side was, for the telling
        let was = |x: &crate::Table, j: usize, cells: &[Value]| -> (bool, String) {
            match x.declaration(j) {
                Some(ty) => (true, ty.word()),
                None => (false, crate::value::type_column(cells.to_vec()).0.word()),
            }
        };
        let (ldecl, lword) = was(left, lj, &lcells);
        let (rdecl, rword) = was(right, rj, &rcells);
        let (lname, rname) = (left.column_name(lj), right.column_name(rj));
        let (target, declared): (T, bool) = match (left.declaration(lj), right.declaration(rj)) {
            (Some(a), Some(b)) if a == b => continue,
            (Some(T::Integer), Some(T::Float)) | (Some(T::Float), Some(T::Integer)) => {
                (T::Float, true)
            }
            (Some(a), Some(b)) => {
                return Err(format!(
                    "the indexes meet as {} ('{}') and {} ('{}'): two declared indexes align \
                     where their types are the same, or an integer with a float; cast one \
                     to the other first (map(<{}>; --to=:::index))",
                    a.word(),
                    left.column_name(lj),
                    b.word(),
                    right.column_name(rj),
                    a.word()
                ));
            }
            (Some(a), None) | (None, Some(a)) => (a, true),
            (None, None) => {
                let (a, b) = (
                    crate::value::type_column(lcells.clone()).0,
                    crate::value::type_column(rcells.clone()).0,
                );
                if a == b {
                    continue;
                }
                let both: Vec<Value> = lcells.iter().chain(&rcells).cloned().collect();
                (crate::value::type_column(both).0, false)
            }
        };
        let cast = |cells: &[Value], name: String| -> Result<Vec<Value>, String> {
            cells
                .iter()
                .map(|c| match crate::value::cast_cell(c, &target) {
                    Some(v) => Ok(v),
                    // the common type of two undeclared columns takes
                    // every label by construction
                    None if !declared => Ok(c.clone()),
                    None => Err(format!(
                        "the indexes meet as {}, the declared one, and the label {} of '{name}' \
                         does not cast to it: cast that index first (map(<{}>; --to=:::index)), \
                         or drop the declaration (map(<*>; --to=:::index))",
                        target.word(),
                        crate::value::held_text(c),
                        target.word()
                    )),
                })
                .collect()
        };
        let lnew = cast(&lcells, left.column_name(lj))?;
        let rnew = cast(&rcells, right.column_name(rj))?;
        let declaration = declared.then(|| target.clone());
        let changed = lnew != lcells
            || rnew != rcells
            || left.declaration(lj) != declaration
            || right.declaration(rj) != declaration;
        if !changed {
            continue;
        }
        // The cast is told, once for the command (the user,
        // 2026-11-10: the upcasting and the merging of indexes are
        // always relayed): which indexes met, as what, and what they
        // became.
        let moved = lnew.iter().zip(&lcells).filter(|(a, b)| a != b).count()
            + rnew.iter().zip(&rcells).filter(|(a, b)| a != b).count();
        if moved > 0 {
            let cast = moved;
            let labels = if cast == 1 { "label" } else { "labels" };
            let word = target.word();
            let told = match (ldecl, rdecl) {
                (true, true) => format!(
                    "the declared indexes '{lname}' ({lword}) and '{rname}' ({rword}) met as \
                     {word}: {cast} {labels} cast"
                ),
                (true, false) => format!(
                    "the index '{rname}' ({rword}) met the declared index '{lname}' ({lword}) \
                     and was cast to it: {cast} {labels} cast"
                ),
                (false, true) => format!(
                    "the index '{lname}' ({lword}) met the declared index '{rname}' ({rword}) \
                     and was cast to it: {cast} {labels} cast"
                ),
                (false, false) => format!(
                    "the indexes '{lname}' ({lword}) and '{rname}' ({rword}) met as {word}: \
                     {cast} {labels} cast"
                ),
            };
            crate::value::ambient::record_info_once(
                crate::diagnostic::INDEX_CAST,
                "pipeline",
                format!("{command}: {told}"),
            );
        }
        // an undeclared side stays undeclared where neither declared
        let (ld, rd) = if declared {
            (declaration.clone(), declaration)
        } else {
            (None, None)
        };
        out = Some((
            left.with_column(lj, lnew, ld),
            right.with_column(rj, rnew, rd),
        ));
    }
    Ok(out)
}

/// The column append of two indexed tables, the rows aligned on the
/// index after pandas `join` (ruling 2026-11-05): two one-level
/// indexes align by label whatever their names, the left's name
/// kept; a composite index takes a plain one on the level that
/// carries the plain index's name, the plain side's row repeated
/// over the other levels (refused naming both where no level carries
/// it, and a plain label on several rows refused); two composite
/// indexes align on the same levels in the same order, refused
/// otherwise. The union of the labels in order first met, null where
/// a side lacks one; a label standing on several rows of a side
/// pairs its n-th row with the other side's n-th. A data column both
/// sides have is refused as a duplicate field (`++!` takes the right
/// side's).
fn labeled_concat(t: &crate::Table, u: &crate::Table, overwrite: bool) -> Result<Value, String> {
    let names =
        |x: &crate::Table| -> Vec<String> { (0..x.width()).map(|j| x.column_name(j)).collect() };
    let (tn, un) = (names(t), names(u));
    let (ti, ui) = (&tn[..t.index], &un[..u.index]);
    let no_level = |levels: &[String], name: &str| {
        format!(
            "no level of the index ({}) carries the name '{}'; a plain index aligns on the level of its name",
            levels.join("; "),
            name
        )
    };
    // the side whose index shapes the result, the key columns of
    // each side, and whether the plain side's row is repeated over
    // the composite side's other levels (broadcast) or paired n-th
    // with n-th
    let (shape_left, lk, rk, broadcast): (bool, Vec<usize>, Vec<usize>, bool) = match (
        t.index, u.index,
    ) {
        (1, 1) => (true, vec![0], vec![0], false),
        (_, 1) => match ti.iter().position(|n| *n == ui[0]) {
            Some(j) => (true, vec![j], vec![0], true),
            None => return Err(no_level(ti, &ui[0])),
        },
        (1, _) => match ui.iter().position(|n| *n == ti[0]) {
            Some(j) => (false, vec![0], vec![j], true),
            None => return Err(no_level(ui, &ti[0])),
        },
        (k, _) => {
            if ti != ui {
                return Err(format!(
                    "the composite indexes differ, ({}) against ({}); they align on the same levels in the same order",
                    ti.join("; "),
                    ui.join("; ")
                ));
            }
            (true, (0..k).collect(), (0..k).collect(), false)
        }
    };
    let pairs: Vec<(usize, usize)> = lk.iter().copied().zip(rk.iter().copied()).collect();
    let command = if overwrite { "++!" } else { "++" };
    if let Some((ta, ua)) = agreed_keys(command, t, u, &pairs)? {
        return labeled_concat(&ta, &ua, overwrite);
    }
    let (shape, other, sk, ok) = if shape_left {
        (t, u, &lk, &rk)
    } else {
        (u, t, &rk, &lk)
    };
    let k = shape.index;
    // the header: the shape side's index, the left's data columns,
    // the right's appended where new (under `++!` laid onto the
    // left's column of one name)
    let mut header: Vec<String> = (0..k).map(|j| shape.column_name(j)).collect();
    let mut declared: Vec<Option<crate::ColumnType>> =
        (0..k).map(|j| shape.declaration(j)).collect();
    let mut left_cols: Vec<(usize, usize)> = Vec::new(); // (column of t, column of the result)
    for j in t.index..t.width() {
        header.push(tn[j].clone());
        declared.push(t.declaration(j));
        left_cols.push((j, header.len() - 1));
    }
    let mut right_cols: Vec<(usize, usize)> = Vec::new();
    for j in u.index..u.width() {
        match left_cols.iter().find(|(lj, _)| tn[*lj] == un[j]) {
            Some(&(_, to)) if overwrite => {
                right_cols.push((j, to));
                declared[to] = u.declaration(j);
            }
            Some(_) => {
                return Err(format!(
                    "both tables have a column '{}'; '++!' takes the right side's",
                    un[j]
                ));
            }
            None => {
                header.push(un[j].clone());
                declared.push(u.declaration(j));
                right_cols.push((j, header.len() - 1));
            }
        }
    }
    let width = header.len();
    let (shape_cols, other_cols) = if shape_left {
        (&left_cols, &right_cols)
    } else {
        (&right_cols, &left_cols)
    };
    let key = |r: &Vec<Value>, cols: &[usize]| -> Vec<Value> {
        cols.iter()
            .map(|&j| r.get(j).cloned().unwrap_or(Value::Null))
            .collect()
    };
    let fill = |row: &mut Vec<Value>, r: &Vec<Value>, cols: &[(usize, usize)]| {
        for &(j, to) in cols {
            row[to] = r.get(j).cloned().unwrap_or(Value::Null);
        }
    };
    // the shape side's rows in order, keyed: by (label, its n-th
    // occurrence) for the pairing, by label for the broadcast; the
    // labels equal as values (spec: Indexed tables)
    let mut classes = crate::value::LabelIndex::new();
    let mut seen = crate::value::Occurrences::new();
    let mut at: std::collections::HashMap<(usize, usize), usize> = std::collections::HashMap::new();
    let mut of_label: Vec<Vec<usize>> = Vec::new();
    let place = |of_label: &mut Vec<Vec<usize>>, class: usize, row: usize| {
        if of_label.len() <= class {
            of_label.resize(class + 1, Vec::new());
        }
        of_label[class].push(row);
    };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    for r in shape.rows() {
        let l = key(&r.cells(), sk);
        let slot = seen.next(&mut classes, &l);
        let mut row = vec![Value::Null; width];
        for j in 0..k {
            row[j] = r.get(j);
        }
        fill(&mut row, &r.cells(), shape_cols);
        at.insert(slot, rows.len());
        place(&mut of_label, slot.0, rows.len());
        rows.push(row);
    }
    // the other side's rows: onto the shape row(s) of the same key,
    // or appended with the key in its level(s) and null elsewhere
    let mut seen_other = crate::value::Occurrences::new();
    for r in other.rows() {
        let l = key(&r.cells(), ok);
        let slot = seen_other.next(&mut classes, &l);
        if broadcast && slot.1 > 1 {
            return Err(format!(
                "the label {} stands on several rows of the plain index; one row per label aligns with a composite index (aggregate them first)",
                crate::value::held_text(&l[0])
            ));
        }
        let targets: Vec<usize> = if broadcast {
            of_label.get(slot.0).cloned().unwrap_or_default()
        } else {
            at.get(&slot).copied().into_iter().collect()
        };
        if targets.is_empty() {
            let mut row = vec![Value::Null; width];
            for (v, &lvl) in l.iter().zip(sk.iter()) {
                row[lvl] = v.clone();
            }
            fill(&mut row, &r.cells(), other_cols);
            at.insert(slot, rows.len());
            place(&mut of_label, slot.0, rows.len());
            rows.push(row);
        } else {
            for i in targets {
                fill(&mut rows[i], &r.cells(), other_cols);
            }
        }
    }
    Ok(Value::table(
        crate::Table::typed(Some(header), rows, declared)?
            .with_index(k)
            .with_name(t.name.clone())
            .with_levels(t.levels || u.levels),
    ))
}

/// One row of named fields laid under a table's header by name, a
/// name the header lacks extending it (null in the rows before);
/// on the array face a name names a position (`1`, `2`, …).
fn place_named(
    header: &mut Option<Vec<String>>,
    rows: &mut [Vec<Value>],
    fields: &[(String, Value)],
    n: usize,
) -> Result<Vec<Value>, String> {
    let w = header
        .as_ref()
        .map_or_else(|| rows.first().map_or(0, Vec::len), Vec::len);
    let mut row = vec![Value::Null; w];
    for (k, v) in fields {
        if !flat_cell(v) {
            return Err(format!(
                "row {n}: '{k}' holds {}; a table is flat, its cell a scalar or null",
                kind_of(v)
            ));
        }
        let j = match header {
            Some(h) => match h.iter().position(|c| c == k) {
                Some(j) => j,
                None => {
                    h.push(k.clone());
                    for r in rows.iter_mut() {
                        r.push(Value::Null);
                    }
                    row.push(Value::Null);
                    h.len() - 1
                }
            },
            None => k
                .parse::<usize>()
                .ok()
                .filter(|j| *j >= 1 && *j <= w)
                .map(|j| j - 1)
                .ok_or_else(|| {
                    format!("row {n}: '{k}' names no column of an array of {w} (its columns are positions)")
                })?,
        };
        row[j] = v.clone();
    }
    Ok(row)
}

/// `++ %(…)`: a row joined under the table literal's rules — on a
/// table, positional items by the header's order (or by position on
/// the array face) and named ones by name, a short row null-padded,
/// a long row or an unknown name refused; on a record or a list, a
/// named row is the record it spells, a positional one has no
/// field to be.
pub(crate) fn concat_row(
    mode: ConcatMode,
    left: Value,
    row: Vec<(Option<String>, Value)>,
) -> Result<Value, String> {
    match left {
        Value::Null => Ok(Value::Null),
        // a quarq standing for a node: the node copied with the
        // named properties replaced (`++!`) or added (`++`), its
        // children kept (design paper: the node overwrite)
        Value::Quarq(q) if q.form.is_some() => {
            let form = q.into_form().rewrite(mode, &row)?;
            Ok(Value::quarq_form(form))
        }
        Value::Table(t) if mode == ConcatMode::Merge => {
            let w = t.width();
            let n = t.n_rows() + 1;
            let mut header = t.header.clone();
            let mut rows = t.rows_vec();
            let mut cells = vec![Value::Null; w];
            let mut next = 0;
            for (name, v) in row {
                if !flat_cell(&v) {
                    return Err(format!(
                        "row {n}: a cell holds {}; a table is flat, its cell a scalar or null",
                        kind_of(&v)
                    ));
                }
                // a column name as written (a quoted key's marker dropped)
                let name = name.map(|k| crate::syntax::ast::row_key(&k).0.to_string());
                let j = match (&name, &mut header) {
                    // A name the header lacks extends it: null in
                    // the rows before.
                    (Some(k), Some(h)) => match h.iter().position(|c| c == k) {
                        Some(j) => j,
                        None => {
                            h.push(k.clone());
                            for r in rows.iter_mut() {
                                r.push(Value::Null);
                            }
                            cells.push(Value::Null);
                            h.len() - 1
                        }
                    },
                    (Some(k), None) => {
                        return Err(format!(
                            "row {n}: '{k}' names no column of an array (its columns are positions)"
                        ));
                    }
                    (None, _) => {
                        if next >= w {
                            return Err(format!(
                                "row {n} has more values than the table has columns ({w})"
                            ));
                        }
                        next += 1;
                        next - 1
                    }
                };
                cells[j] = v;
            }
            rows.push(cells);
            // a declared column casts the appended cell or refuses
            // naming it; a column the header gained is undeclared
            Ok(Value::table(
                crate::Table::typed(header, rows, t.declared.clone())?.with_levels(t.levels),
            ))
        }
        other => {
            // a row literal's bare key reads as the key it spells
            // (`++ %(1 = 2)` a typed key, an ISO date an instant), so
            // it meets a record's typed keys as values
            let mut pairs: Vec<(Value, Value)> = Vec::with_capacity(row.len());
            for (name, v) in row {
                match name {
                    Some(k) => {
                        let (text, quoted) = crate::syntax::ast::row_key(&k);
                        let key =
                            match crate::syntax::parser::literal_value(text, quoted, &|_| None) {
                                Value::Str(s) if quoted => Value::Str(s),
                                Value::Str(s) => crate::temporal::parse_iso(&s)
                                    .map(|(secs, nanos, offset_min)| Value::Instant {
                                        secs,
                                        nanos,
                                        offset_min,
                                        calendar: Default::default(),
                                    })
                                    .unwrap_or(Value::Str(s)),
                                typed => typed,
                            };
                        pairs.push((key, v));
                    }
                    None => {
                        return Err(format!(
                            "a positional row appends to a table; {} takes named fields (%(name = value))",
                            kind_of(&other)
                        ));
                    }
                }
            }
            concat(
                mode,
                other,
                Value::Record(crate::value::Record::keyed(pairs)),
            )
        }
    }
}

/// `| #` and `| #(…)`: bare, the table reading; with a column count
/// or a header list, the reshape (spec: the calculator's reshaping
/// constructor): the list topic laid into rows of that many columns
/// in reading order, an array of them or the frame of the named
/// columns, a count that does not divide the length refused naming
/// both numbers.
pub(crate) fn table_command(call: &FnCall, v: Value) -> Result<Value, String> {
    table_command_shaped(call, v, None)
}

/// A list of records from a source whose rows are of one shape (a
/// CSV mount: its `:::columns`) read as a table in the SOURCE'S column
/// order, a column a record lacks null — not in order of first
/// appearance (ruling 2026-11-06, the file's order kept); the record
/// rule stands where a record carries a name the shape does not.
pub(crate) fn table_reading_shaped(v: Value, shape: Option<&[String]>) -> Result<Value, String> {
    if let (Some(cols), Value::List(items)) = (shape, &v)
        && !items.is_empty()
    {
        let known: std::collections::HashSet<&str> = cols.iter().map(String::as_str).collect();
        let shaped = items.iter().all(
            |i| matches!(i, Value::Record(r) if r.iter().all(|(k, _)| known.contains(k.as_str()))),
        );
        if shaped {
            let mut declared: Vec<Option<crate::value::ColumnType>> = vec![None; cols.len()];
            for item in items.iter() {
                if let Value::Record(r) = item {
                    for (fi, (k, _)) in r.iter().enumerate() {
                        if let Some(ty) = r.declaration(fi)
                            && let Some(j) = cols.iter().position(|c| c == k)
                        {
                            match &declared[j] {
                                Some(have) if *have != ty => {
                                    return Err(format!(
                                        "key '{k}' is declared {} on one record and {} on another: the records must agree",
                                        have.word(),
                                        ty.word()
                                    ));
                                }
                                _ => declared[j] = Some(ty),
                            }
                        }
                    }
                }
            }
            let mut b = crate::TableBuilder::shaped(Some(cols.to_vec()), declared);
            for item in items.iter() {
                if let Value::Record(r) = item {
                    b.push_record(r.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
            return Ok(Value::table(b.finish()?));
        }
    }
    table_reading(v)
}

pub(crate) fn table_command_shaped(
    call: &FnCall,
    v: Value,
    shape: Option<&[String]>,
) -> Result<Value, String> {
    // `--index=:col` (`-i`): the index named, a typed projector
    // declaring it; applied after the build. `| #` with no option
    // drops the role.
    let index: Option<Vec<(String, Option<crate::value::ColumnType>)>> =
        call.args.iter().find_map(|a| match a {
            Arg::Opt { name, value } if name == "--index" => Some(match value {
                // `-i=null`: no index, the role dropped
                Some(crate::syntax::ast::Operand::Lit(Value::Null)) => Vec::new(),
                Some(crate::syntax::ast::Operand::Lit(v)) => index_levels(v),
                Some(other) => vec![(crate::syntax::unparse::operand(other), None)],
                None => Vec::new(),
            }),
            _ => None,
        });
    // `--name=word` names the header (draft 166), read back by `:::name`
    let header_name: Option<String> = call.args.iter().find_map(|a| match a {
        Arg::Opt {
            name,
            value: Some(crate::syntax::ast::Operand::Lit(Value::Str(s))),
        } if name == "--name" => Some(s.clone()),
        _ => None,
    });
    // `-a|--autoindex` (ruling 2026-11-06): the implicit index made
    // explicit as `| autoindex` makes it, no start and no step (those
    // stay the command's); never beside `-i`, a table having one index
    let autoindex_flag = call.args.iter().any(
        |a| matches!(a, Arg::Opt { name, value: None } if name == "--autoindex" || name == "-a"),
    );
    // `-c|--cast=:col<type>` (ruling 2026-11-06), repeated or a tuple:
    // the columns cast after the build as map(<type>; --to=:col) casts
    // them; a column the header declares otherwise is refused
    let mut casts: Vec<(String, crate::value::ColumnType)> = Vec::new();
    for a in &call.args {
        if let Arg::Opt {
            name,
            value: Some(crate::syntax::ast::Operand::Lit(v)),
        } = a
            && name == "--cast"
        {
            for (col, ty) in index_levels(v) {
                match ty {
                    Some(ty) => casts.push((col, ty)),
                    None => return Err(format!("-c=:{col} names no type: -c=:{col}<int>")),
                }
            }
        }
    }
    if autoindex_flag && index.is_some() {
        return Err(
            "#(-a -i=…) names two indexes, and a table has one: -a prepends the positions as the index, -i=:col makes a column the index"
                .into(),
        );
    }
    // `--null=@(…)` and `--locale="de"` on a table: a text cell that
    // is a null word is null, one that reads as a number under the
    // locale is that number; the columns retype (the document-tables
    // round, 2026-11-08; a document table applies both as it is read,
    // so a second pass here changes nothing)
    let nulls: Vec<String> = call
        .args
        .iter()
        .find_map(|a| match a {
            Arg::Opt {
                name,
                value: Some(crate::syntax::ast::Operand::Lit(Value::List(l))),
            } if name == "--null" => Some(l.iter().map(|v| v.to_string()).collect()),
            _ => None,
        })
        .unwrap_or_default();
    let locale: Option<crate::value::number::Decimal> = match call.args.iter().find_map(|a| match a
    {
        Arg::Opt {
            name,
            value: Some(crate::syntax::ast::Operand::Lit(Value::Str(s))),
        } if name == "--locale" => Some(s.clone()),
        _ => None,
    }) {
        Some(tag) => Some(
            crate::value::number::locale_convention(&tag).ok_or_else(|| {
                format!(
                    "#(--locale={tag}): no number conventions for that locale (carried: {})",
                    crate::value::locale::SHIPPED.join(", ")
                )
            })?,
        ),
        None => None,
    };
    // `--autounits` / `--autounit=:col…` / `--slugs` on a table in
    // hand: the title rules (the header-units round, 2026-11-09; a
    // document table applies them as it is read)
    let title_opts = title_options(call)?;
    let scopes = read_scopes(call)?;
    let finish = |v: Value| -> Result<Value, String> {
        let Value::Table(t) = v else { return Ok(v) };
        let t = if nulls.is_empty() && locale.is_none() {
            t
        } else {
            let rows: Vec<Vec<Value>> = t
                .rows()
                .map(|r| {
                    r.cells()
                        .into_iter()
                        .map(|c| match &c {
                            Value::Str(s) if nulls.iter().any(|n| n == s) => Value::Null,
                            Value::Str(s) => match locale {
                                Some(d) => crate::value::number::read(s, d).unwrap_or(c),
                                None => c,
                            },
                            _ => c,
                        })
                        .collect()
                })
                .collect();
            // the dirt the constructor warned of is gone from the
            // columns the pass retyped: the warning leaves the run
            crate::value::ambient::retract_diagnostics(|d| d.code == "TABLE_TEXT");
            std::sync::Arc::new(t.rebuild(rows)?)
        };
        let t = if scopes.nulls_by.is_empty() && scopes.locale_by.is_empty() {
            t
        } else {
            std::sync::Arc::new(apply_scopes(&t, &scopes)?)
        };
        let implied = (
            title_opts.0.clone().or_else(|| scopes.implied_autounits()),
            title_opts.1.clone(),
        );
        let t = match &implied {
            (None, None) if scopes.declare_by.is_empty() => t,
            (autounits, slugs) => {
                let site = t.name.clone().unwrap_or_else(|| "the table".to_string());
                std::sync::Arc::new(title_pass(
                    &t,
                    autounits.as_deref(),
                    slugs.as_deref(),
                    &site,
                    &scopes,
                )?)
            }
        };
        // The bare `| #` drops the index role; an option that is not
        // `-i` leaves the index as it is (ruling 2026-11-10: `| #(-c=
        // :A<float>)` cast a column and lost the labels with it).
        let bare = !call.args.iter().any(|a| matches!(a, Arg::Opt { .. }));
        let t = match &index {
            Some(levels) if levels.is_empty() => t.as_ref().clone().drop_index(),
            Some(levels) => t.indexed_by_levels(levels)?,
            None if t.index > 0 && bare => t.as_ref().clone().drop_index(),
            None => t.as_ref().clone(),
        };
        let t = match &header_name {
            Some(n) => t.with_name(Some(n.clone())),
            None => t,
        };
        let t = if casts.is_empty() {
            t
        } else {
            for (col, ty) in &casts {
                if let Some(j) = (0..t.width()).find(|&j| t.column_name(j) == *col)
                    && let Some(declared) = t.declaration(j)
                    && declared != *ty
                {
                    return Err(format!(
                        "column '{col}' is declared {} by the header and cast to {} by -c: the two must agree",
                        declared.word(),
                        ty.word()
                    ));
                }
            }
            t.cast_columns(&casts)?
        };
        if autoindex_flag {
            return autoindex_bare(&t).map(Value::table);
        }
        Ok(Value::table(t))
    };
    let Some(arg) = call.args.iter().find(|a| !matches!(a, Arg::Opt { .. })) else {
        return finish(table_reading_shaped(v, shape)?);
    };
    let (width, header): (usize, Option<Vec<String>>) = match arg {
        Arg::Lit(Value::Int(n)) if *n >= 1 => (*n as usize, None),
        Arg::Lit(Value::Int(n)) => return Err(format!("#(--width={n}): a table has at least one column")),
        // a header of projectors, typed or not: the declared build
        Arg::Lit(Value::List(items)) if items.iter().any(|i| matches!(i, Value::Projection { .. })) => {
            let built = typed_table(&header_items(items)?, v)?;
            // a header written with levels names its columns by path
            return finish(match built {
                Value::Table(t) if header_writes_levels(items) => {
                    Value::table((*t).clone().with_levels(true))
                }
                other => other,
            });
        }
        Arg::Lit(Value::List(names)) => (
            names.len(),
            Some(names.iter().map(|n| n.to_string()).collect()),
        ),
        Arg::Lit(Value::Record(fields)) => return Err(retired_header_record(fields)),
        Arg::Expr(_) => {
            return Err(
                "#(--header=…) with a computed header is read in the pipeline; here it has no value"
                    .into(),
            );
        }
        _ => return Err(
            "#(…) after a pipe takes an option: #(--width=n) lays the list in rows of n, #(--header=list) names its columns, #(--header=record) names and types them"
                .into(),
        ),
    };
    // on an existing table `#(--header=h)` is the columns the header
    // names, in its order, cast as declared (spec: The Table; the
    // selection rule, which the typed header already followed — the
    // bare names take the same road, 2026-11-09)
    if let (Value::Table(_), Some(names)) = (&v, &header) {
        let declared: Vec<(String, Option<crate::value::ColumnType>)> =
            names.iter().map(|n| (n.clone(), None)).collect();
        return finish(typed_table(&declared, v)?);
    }
    let items = match v {
        Value::List(l) => l,
        other => {
            return Err(format!(
                "#({}) lays out a list into rows; {} is not a list",
                match &header {
                    Some(h) => format!("--header=@({})", h.join("; ")),
                    None => format!("--width={width}"),
                },
                kind_of(&other)
            ));
        }
    };
    if width == 0 {
        return Err("#(--header=@()): a header names at least one column".into());
    }
    if items.len() % width != 0 {
        return Err(format!(
            "#(--width={width}): {} items do not divide into rows of {width}",
            items.len()
        ));
    }
    if let Some((i, v)) = items.iter().enumerate().find(|(_, v)| !flat_cell(v)) {
        return Err(format!(
            "#(--width={width}): item {} holds {}; a table is flat, its cell a scalar or null",
            i + 1,
            kind_of(v)
        ));
    }
    let rows: Vec<Vec<Value>> = items.chunks(width).map(|c| c.to_vec()).collect();
    finish(Value::table(crate::Table::new(header, rows)))
}

/// The levels `--index` names: one projector, or a list of them outer
/// first, each declared where its brackets declare a type.
pub(crate) fn index_levels(v: &Value) -> Vec<(String, Option<crate::value::ColumnType>)> {
    let one = |p: &Value| match p {
        Value::Projection { key, typing, .. } => (
            key.clone(),
            typing.as_ref().and_then(crate::value::Typing::declared),
        ),
        other => (other.to_string(), None),
    };
    match v {
        Value::List(items) => items.iter().map(one).collect(),
        other => vec![one(other)],
    }
}

/// A header computed for `#(--header=…)`: a list of names, or of
/// projectors typed or not (what `header` answers), as the literal
/// forms are; the record form is retired (draft 166); anything else
/// refuses.
pub(crate) fn computed_header(v: Value) -> Result<Arg, String> {
    match v {
        Value::List(items)
            if !items.is_empty()
                && items
                    .iter()
                    .all(|n| matches!(n, Value::Str(_) | Value::Projection { .. })) =>
        {
            Ok(Arg::Lit(Value::List(items)))
        }
        Value::Record(fields) if !fields.is_empty() => Err(retired_header_record(&fields)),
        other => Err(format!(
            "#(--header=…): a header is a list of names or of typed projectors (@(:zip<text>; :amount<float>)); the computed one is {}",
            kind_of(&other)
        )),
    }
}

/// The retired record form of the header, `%(zip = "text")`, with
/// the pointer to the projector list (draft 166).
fn retired_header_record(fields: &[(String, Value)]) -> String {
    let spelled: Vec<String> = fields
        .iter()
        .map(|(k, w)| match w {
            Value::Str(word) => format!(":{k}<{word}>"),
            _ => format!(":{k}"),
        })
        .collect();
    format!(
        "#(--header=%(…)) is retired: the header is a list of typed projectors, #(--header=@({}))",
        spelled.join("; ")
    )
}

/// Whether a header's items write levels: one holds the level below
/// it (`:bar:@(:A; :B)`).
fn header_writes_levels(items: &[Value]) -> bool {
    items.iter().any(|i| {
        matches!(
            i,
            Value::Projection {
                typing: Some(crate::value::Typing::Levels(_)),
                ..
            }
        )
    })
}

/// The columns a header's items declare: a name undeclared, a
/// projector by its key, declared where its brackets declare a type
/// (`:a<int>`), undeclared under the star or bare — the inferred
/// type a header reports (`<*; float>`) is a report, no request. An
/// item that holds a level (`:bar:@(:A<int>; :B<int>)`, what `header`
/// answers for a header with levels) names the columns under it by
/// their paths, `bar:A` and `bar:B`. A header has levels or names
/// with colons, not both: beside a level a name that holds a colon
/// of its own is refused, as the literal refuses it.
fn header_items(
    items: &[Value],
) -> Result<Vec<(String, Option<crate::value::ColumnType>)>, String> {
    use crate::value::Typing;
    fn walk(
        prefix: &str,
        items: &[Value],
        out: &mut Vec<(String, Option<crate::value::ColumnType>)>,
        colon: &mut Option<String>,
    ) {
        for i in items {
            let key = match i {
                Value::Projection { key, .. } => key.clone(),
                other => other.to_string(),
            };
            if colon.is_none() && key.contains(':') {
                *colon = Some(key.clone());
            }
            match i {
                Value::Projection {
                    typing: Some(Typing::Levels(below)),
                    ..
                } => walk(&format!("{prefix}{key}:"), below, out, colon),
                Value::Projection { typing, .. } => out.push((
                    format!("{prefix}{key}"),
                    typing.as_ref().and_then(Typing::declared),
                )),
                _ => out.push((format!("{prefix}{key}"), None)),
            }
        }
    }
    let mut out = Vec::new();
    let mut colon = None;
    walk("", items, &mut out, &mut colon);
    if header_writes_levels(items)
        && let Some(name) = colon
    {
        return Err(format!(
            "#(--header=…): a header that writes levels (:bar:@(:A; :B)) parts its names at \
             their colons, and the name '{name}' holds one of its own: a table's header has \
             levels or names with colons, not both; respell that name without its colon (the \
             constructor's slug option does it for a table's names: | #(--slugs)), or write \
             no level"
        ));
    }
    Ok(out)
}

/// `#(--header=@(:a<int>; :b<text>))` (spec: The Table, draft 166):
/// the header of projectors. Over a flat list the keys name the
/// columns (their count the width) and a declared column is cast to
/// its type. Over a table, or a list of records, the result is the
/// columns the header names, in its order, cast as declared — the
/// header applied as the structured projector it is, `t:#(h...)`;
/// a named column the data lacks refuses.
fn typed_table(
    declared: &[(String, Option<crate::value::ColumnType>)],
    v: Value,
) -> Result<Value, String> {
    use crate::value::ColumnType;
    let spelled = || {
        format!(
            "--header=@({})",
            declared
                .iter()
                .map(|(k, t)| match t {
                    Some(t) => format!(":{k}<{}>", t.word()),
                    None => format!(":{k}"),
                })
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    let wrap = |e: String| format!("#({}): {e}", spelled());
    match v {
        // the flat list laid in rows of the header's width (a list
        // item a cell under a declared tuple)
        Value::List(items)
            if items.iter().all(|i| {
                flat_cell(i)
                    || (matches!(i, Value::List(_))
                        && declared
                            .iter()
                            .any(|(_, t)| matches!(t, Some(crate::value::ColumnType::Tuple(_)))))
            }) =>
        {
            let width = declared.len();
            if items.len() % width != 0 {
                return Err(format!(
                    "#({}): {} items do not divide into rows of {width}",
                    spelled(),
                    items.len()
                ));
            }
            let rows: Vec<Vec<Value>> = items.chunks(width).map(|c| c.to_vec()).collect();
            crate::Table::declared(declared.to_vec(), rows)
                .map(Value::table)
                .map_err(wrap)
        }
        // a table, or what reads as one: the named columns, in the
        // header's order, cast as declared (`t:#(h...)`)
        Value::Table(_) | Value::Foreign(_) | Value::List(_) | Value::Record(_) => {
            let t = match table_reading(v)? {
                Value::Table(t) => t,
                other => return Err(wrap(format!("{} is no table", kind_of(&other)))),
            };
            let names: Vec<String> = match &t.header {
                Some(h) => h.clone(),
                // by position on the array face (`:1<int>`)
                None => (1..=t.width()).map(|j| j.to_string()).collect(),
            };
            let mut order: Vec<(usize, Option<ColumnType>)> = Vec::new();
            for (k, ty) in declared {
                match names.iter().position(|n| n == k) {
                    Some(j) => order.push((j, ty.clone())),
                    None => {
                        return Err(wrap(format!(
                            "the header names column '{k}', and the data has no such column (its columns: {})",
                            names.join(", ")
                        )));
                    }
                }
            }
            let header = order
                .iter()
                .map(|(j, ty)| (names[*j].clone(), ty.clone()))
                .collect();
            let rows = t
                .rows()
                .map(|r| order.iter().map(|(j, _)| r.get(*j)).collect())
                .collect();
            // the names are the table's own: paths where its are
            crate::Table::declared(header, rows)
                .map(|built| Value::table(built.with_levels(t.levels)))
                .map_err(wrap)
        }
        other => Err(wrap(format!("{} lays out no table", kind_of(&other)))),
    }
}

/// The bins of a number stream (spec: Aggregate Commands, bins).
struct Binned {
    /// `(from, to, count)` per bin, in order; `to` is open except
    /// on the last bin.
    bins: Vec<(f64, f64, i64)>,
    nulls: i64,
    /// Whether every edge is a whole number (printed bare then).
    whole: bool,
}

impl Binned {
    fn edge(&self, x: f64) -> Value {
        if self.whole {
            Value::Int(x as i64)
        } else {
            Value::Float(x)
        }
    }

    fn table(&self) -> Value {
        let mut rows: Vec<Vec<Value>> = self
            .bins
            .iter()
            .map(|(a, b, n)| vec![self.edge(*a), self.edge(*b), Value::Int(*n)])
            .collect();
        if self.nulls > 0 {
            rows.push(vec![Value::Null, Value::Null, Value::Int(self.nulls)]);
        }
        Value::table(crate::Table::new(
            Some(vec!["from".into(), "to".into(), "count".into()]),
            rows,
        ))
    }

    /// The bin's label, `a–b`, the edges to a precision that tells
    /// adjacent edges apart.
    fn labels(&self) -> Vec<String> {
        if self.whole {
            return self
                .bins
                .iter()
                .map(|(a, b, _)| format!("{}–{}", *a as i64, *b as i64))
                .collect();
        }
        let width = self
            .bins
            .first()
            .map(|(a, b, _)| b - a)
            .unwrap_or(1.0)
            .abs();
        let decimals = if width <= 0.0 {
            2
        } else {
            ((-width.log10()).ceil() as i64 + 1).clamp(0, 6) as usize
        };
        self.bins
            .iter()
            .map(|(a, b, _)| format!("{a:.decimals$}–{b:.decimals$}"))
            .collect()
    }
}

/// The bin count asked for: the first positional number, 10 unless
/// given; fewer than one refuses.
fn bin_count(call: &FnCall) -> Result<usize, String> {
    let first = call.args.iter().find_map(|a| match a {
        Arg::Lit(Value::Int(n)) => Some(Ok(*n)),
        Arg::Lit(other) => Some(Err(format!(
            "{}({}): the bin count is a number",
            call.name,
            other.display_form()
        ))),
        _ => None,
    });
    match first.transpose()? {
        None => Ok(10),
        Some(n) if n >= 1 && n as u64 <= MAX_BINS as u64 => Ok(n as usize),
        Some(n) if n >= 1 => Err(format!("{}({n}): at most {MAX_BINS} bins", call.name)),
        Some(n) => Err(format!("{}({n}): at least one bin", call.name)),
    }
}

/// The number a stream value reads as, under the column coercion
/// (a CSV cell's "24" is a number here), `None` for text that is
/// no number.
fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        Value::Str(_) => match v.numeric_reading() {
            Some(Value::Int(i)) => Some(i as f64),
            Some(Value::Float(f)) => Some(f),
            _ => None,
        },
        _ => None,
    }
}

fn binned(call: &FnCall, input: Vec<Value>, scale: Scale) -> Result<Binned, String> {
    let n = bin_count(call)?;
    let mut xs: Vec<f64> = Vec::with_capacity(input.len());
    let mut nulls = 0;
    let mut all_int = true;
    for v in &input {
        match (v, number_of(v)) {
            (Value::Null, _) => nulls += 1,
            (_, Some(x)) => {
                if x.fract() != 0.0 {
                    all_int = false;
                }
                xs.push(x);
            }
            (other, None) => {
                return Err(format!(
                    "{} bins numbers, and the stream holds {}; a stream of other values draws one bar per distinct value under histogram",
                    call.name,
                    kind_of(other)
                ));
            }
        }
    }
    let _ = scale;
    if xs.is_empty() {
        return Ok(Binned {
            bins: Vec::new(),
            nulls,
            whole: true,
        });
    }
    let min = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = max - min;
    let n = if span == 0.0 { 1 } else { n };
    let width = if n == 0 { 0.0 } else { span / n as f64 };
    let mut counts = vec![0i64; n];
    for x in &xs {
        let i = if width == 0.0 {
            0
        } else {
            (((x - min) / width).floor() as usize).min(n - 1)
        };
        counts[i] += 1;
    }
    // the edges to a precision that tells them apart and no
    // further, so the table prints 60.105, not 60.105000000000004
    let decimals = if width <= 0.0 {
        0
    } else {
        ((-width.log10()).ceil() as i64 + 3).clamp(0, 12) as i32
    };
    let scale10 = 10f64.powi(decimals);
    let edges: Vec<f64> = (0..=n)
        .map(|k| {
            let e = if k == n { max } else { min + width * k as f64 };
            (e * scale10).round() / scale10
        })
        .collect();
    let whole = all_int && edges.iter().all(|e| e.fract() == 0.0);
    Ok(Binned {
        bins: (0..n)
            .map(|k| (edges[k], edges[k + 1], counts[k]))
            .collect(),
        nulls,
        whole,
    })
}

/// A bar of up to `width` cells for `n` of `max`, in eighth blocks:
/// any count shows at least one eighth, and a count past the scale
/// (the "other" and null bars stand outside it) fills the width.
fn bar(n: i64, max: i64, width: usize) -> String {
    if max <= 0 || n <= 0 {
        return String::new();
    }
    let eighths = ((n as f64 / max as f64 * width as f64 * 8.0).round() as usize)
        .clamp(1, width.saturating_mul(8).max(1));
    let mut s = "█".repeat(eighths / 8);
    const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    s.push_str(PARTS[eighths % 8]);
    s
}

fn histogram(call: &FnCall, input: Vec<Value>, scale: Scale) -> Result<Value, String> {
    let width = call
        .args
        .iter()
        .find_map(|a| match a {
            Arg::Opt { name, value } if name == "--width" || name == "-w" => Some(value),
            _ => None,
        })
        .map(|v| match v {
            Some(crate::syntax::ast::Operand::Lit(Value::Int(w)))
                if *w >= 1 && *w as u64 <= MAX_BAR_WIDTH as u64 =>
            {
                Ok(*w as usize)
            }
            _ => Err(format!(
                "histogram(--width=n): the longest bar in cells, a number from 1 to {MAX_BAR_WIDTH}"
            )),
        })
        .transpose()?
        .unwrap_or(40);
    // numbers, or text that reads as numbers throughout, bin; any
    // other value makes it a bar per distinct value
    let numeric = input.iter().any(|v| !matches!(v, Value::Null))
        && input
            .iter()
            .all(|v| matches!(v, Value::Null) || number_of(v).is_some());
    // whole numbers taking no more distinct values than bins draw
    // one bar per value, in numeric order (a 1/2/3 class column)
    let few_wholes = numeric && {
        let n = bin_count(call)?;
        let mut seen: Vec<i64> = Vec::new();
        let mut whole = true;
        for v in &input {
            if let Some(x) = number_of(v) {
                if x.fract() != 0.0 {
                    whole = false;
                    break;
                }
                let i = x as i64;
                if !seen.contains(&i) {
                    seen.push(i);
                }
            }
        }
        whole && !seen.is_empty() && seen.len() <= n
    };
    // the rows and whether each sets the scale: the "other" and
    // null bars stand outside it, so the values keep their shape
    let rows: Vec<(String, i64, bool)> = if few_wholes {
        let mut counts: Vec<(i64, i64)> = Vec::new();
        let mut nulls = 0;
        for v in &input {
            match number_of(v) {
                Some(x) => {
                    let i = x as i64;
                    match counts.iter_mut().find(|(k, _)| *k == i) {
                        Some(c) => c.1 += 1,
                        None => counts.push((i, 1)),
                    }
                }
                None => nulls += 1,
            }
        }
        counts.sort_by_key(|(k, _)| *k);
        let mut rows: Vec<(String, i64, bool)> = counts
            .into_iter()
            .map(|(k, c)| (k.to_string(), c, true))
            .collect();
        if nulls > 0 {
            rows.push(("null".to_string(), nulls, false));
        }
        rows
    } else if numeric {
        let b = binned(call, input, scale)?;
        let mut rows: Vec<(String, i64, bool)> = b
            .labels()
            .into_iter()
            .zip(b.bins.iter().map(|(_, _, n)| *n))
            .map(|(l, n)| (l, n, true))
            .collect();
        if b.nulls > 0 {
            rows.push(("null".to_string(), b.nulls, false));
        }
        rows
    } else {
        // one bar per distinct value, the most frequent first, the
        // top n shown and the rest one bar
        let n = bin_count(call)?;
        let mut counts: Vec<(Value, i64)> = Vec::new();
        let mut index: std::collections::HashMap<crate::value::SortKey, usize> =
            std::collections::HashMap::new();
        let mut nulls = 0;
        for v in input {
            if matches!(v, Value::Null) {
                nulls += 1;
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
        let mut rows: Vec<(String, i64, bool)> = Vec::new();
        let mut other = 0;
        for (i, (v, c)) in counts.into_iter().enumerate() {
            if i < n {
                rows.push((v.display_form(), c, true));
            } else {
                other += c;
            }
        }
        if other > 0 {
            rows.push(("other".to_string(), other, false));
        }
        if nulls > 0 {
            rows.push(("null".to_string(), nulls, false));
        }
        rows
    };
    let max = rows
        .iter()
        .filter(|(_, _, scaled)| *scaled)
        .map(|(_, n, _)| *n)
        .max()
        .or_else(|| rows.iter().map(|(_, n, _)| *n).max())
        .unwrap_or(0);
    let label_w = rows
        .iter()
        .map(|(l, _, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let count_w = rows
        .iter()
        .map(|(_, n, _)| n.to_string().len())
        .max()
        .unwrap_or(1);
    let lines: Vec<String> = rows
        .iter()
        .map(|(label, n, _)| {
            let pad = " ".repeat(label_w - label.chars().count());
            let b = bar(*n, max, width);
            let fill = " ".repeat(width.saturating_sub(b.chars().count()));
            format!("{label}{pad}  {b}{fill}  {n:>count_w$}")
        })
        .collect();
    crate::pipeline::set_drawing_html(histogram_html(&rows, max));
    Ok(Value::Str(lines.join("\n")))
}

/// The HTML form of a histogram: a grid in the table's dress, one
/// row per bar — the label, the bar as a block whose width is the
/// count's share of the scale (any count at least a sliver, a
/// count past the scale the full width), the count — the "other"
/// and null bars marked `aside`.
fn histogram_html(rows: &[(String, i64, bool)], max: i64) -> String {
    use crate::value::table::escape;
    let mut out = String::from("<table class=\"quarb-table quarb-histogram\"><tbody>");
    for (label, n, scaled) in rows {
        let share = if max <= 0 || *n <= 0 {
            0.0
        } else {
            (*n as f64 / max as f64 * 100.0).clamp(1.0, 100.0)
        };
        out.push_str(&format!(
            "<tr{}><td class=\"label\">{}</td><td class=\"bar\"><div style=\"width:{share:.1}%\"></div></td><td class=\"num\">{n}</td></tr>",
            if *scaled { "" } else { " class=\"aside\"" },
            escape(label)
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// `| flatten` / `| flatten(n)`: every nested list opened to any
/// depth (or `n` levels), scalars and nulls kept in order; a table
/// yields its cells row by row as one flat list; a record is
/// refused by kind; any other value passes through.
pub(crate) fn flatten_value(call: &FnCall, v: Value) -> Result<Value, String> {
    let depth: Option<usize> =
        match call.args.first() {
            None => None,
            Some(Arg::Lit(Value::Int(n)))
            | Some(Arg::Expr(crate::syntax::ast::Operand::Lit(Value::Int(n))))
                if *n >= 0 =>
            {
                Some(*n as usize)
            }
            Some(_) => return Err(
                "flatten takes the number of levels to open, flatten(1), or none for every level"
                    .into(),
            ),
        };
    fn open(v: &Value, depth: Option<usize>, out: &mut Vec<Value>) {
        match v {
            Value::List(l) if depth != Some(0) => {
                for item in l.iter() {
                    open(item, depth.map(|d| d - 1), out);
                }
            }
            other => out.push(other.clone()),
        }
    }
    match v {
        Value::List(l) => {
            let mut out = Vec::new();
            for item in l.iter() {
                open(item, depth, &mut out);
            }
            Ok(Value::list(out))
        }
        Value::Table(t) => Ok(Value::list(t.rows().flat_map(|r| r.iter()).collect())),
        // a record flattens to the list of its values: a composite
        // label to the list of its levels (spec: The Table, "The index")
        Value::Record(r) => Ok(Value::list(r.iter().map(|(_, v)| v.clone()).collect())),
        other => Ok(other),
    }
}

/// `| keys`: a record's field names as a list, in order; a table's
/// header under another name.
/// `| fields` (spec: Projectors, draft 164): the field
/// projections of the topic as a list of projectors — a
/// record's fields (`:a`), a table's columns (`:name`, or `:1` by
/// position on the array face), a list of records' union of fields
/// in first-seen order (the stream after `@|`, as `header` reads);
/// a node's core fields are answered where the node is met.
pub(crate) fn fields_reading(v: Value) -> Result<Value, String> {
    let projections = |names: Vec<String>| {
        Value::list(names.into_iter().map(|k| Value::projection(1, k)).collect())
    };
    match v {
        Value::Record(f) => Ok(projections(f.into_iter().map(|(k, _)| k).collect())),
        Value::Table(t) => Ok(Value::list(t.field_projectors())),
        Value::List(l) if !l.is_empty() && l.iter().all(|v| matches!(v, Value::Record(_))) => {
            let mut names: Vec<String> = Vec::new();
            for r in l.iter() {
                if let Value::Record(f) = r {
                    for (k, _) in f {
                        if !names.contains(k) {
                            names.push(k.clone());
                        }
                    }
                }
            }
            Ok(projections(names))
        }
        other => Err(format!(
            "fields reads a record's fields, a table's columns or a node's core fields, not {}",
            kind_of(&other)
        )),
    }
}

pub(crate) fn keys_reading(v: Value) -> Result<Value, String> {
    match v {
        // the keys typed as they are (draft 166)
        Value::Record(f) => Ok(Value::list(f.key_values())),
        Value::Table(t) => Ok(t.header_value()),
        // A list of records answers per record: one list of names
        // each, in that record's own order (`| header` is the
        // collection's union).
        Value::List(l) if !l.is_empty() && l.iter().all(|v| matches!(v, Value::Record(_))) => {
            Ok(Value::list(l.iter().map(|r| keys_reading(r.clone()).unwrap_or(Value::Null)).collect()))
        }
        Value::List(_) => Err("keys reads a list of records, one list of names per record; this list is not all records".into()),
        other => Err(format!("keys reads a record's field names or a table's header, not {}", kind_of(&other))),
    }
}

/// `| values`: a record's values as a list, in order; a table's rows
/// as an array, the headerless table with its columns and their
/// types kept, so `keys` and `values` split a frame as they split a
/// record.
pub(crate) fn values_reading(v: Value) -> Result<Value, String> {
    match v {
        Value::Record(f) => Ok(Value::list(f.into_iter().map(|(_, v)| v).collect())),
        Value::List(l) if !l.is_empty() && l.iter().all(|v| matches!(v, Value::Record(_))) => {
            Ok(Value::list(l.iter().map(|r| values_reading(r.clone()).unwrap_or(Value::Null)).collect()))
        }
        Value::List(_) => Err("values reads a list of records, one list of values per record; this list is not all records".into()),
        // the columns and their types kept (draft 166)
        Value::Table(t) => Ok(Value::table(crate::Table::assemble(None, t.rows_vec(), t.declared.clone(), 0, None))),
        other => Err(format!("values reads a record's values or a table's rows, not {}", kind_of(&other))),
    }
}

pub(crate) fn table_meta(v: Value, what: &str) -> Result<Value, String> {
    // `| header` is the columns as typed projectors (spec: The
    // Table, draft 166): a declared column with its type, `:A<int>`,
    // an undeclared one with the star and the type inferred,
    // `:A<*; float>`; by position on a headerless table; on a record
    // its fields likewise; on a list of records the union of the
    // names in first-seen order, undeclared, each with the least
    // coercible type across the records. Null on null; anything
    // else refuses naming the two lists.
    if what == "header" {
        use crate::value::{ColumnType, Typing};
        let star = |ty: Option<ColumnType>| Some(Typing::Star(ty));
        let t = match &v {
            Value::Null => return Ok(Value::Null),
            Value::Table(_) | Value::Foreign(_) => table_reading(v)?,
            // a record's fields, each with the type of its value (a
            // structure under the bare star)
            Value::Record(fields) => {
                return Ok(Value::list(
                    fields
                        .iter()
                        .enumerate()
                        .map(|(i, (k, x))| {
                            let typing = match fields.declaration(i) {
                                Some(t) => Some(Typing::Declared(t)),
                                None => star(if flat_cell(x) {
                                    Some(crate::value::type_column(vec![x.clone()]).0)
                                } else {
                                    None
                                }),
                            };
                            Value::typed_projection(1, k.clone(), typing)
                        })
                        .collect(),
                ));
            }
            // the union of the names in first-seen order, each
            // column typed over the records' cells (a column holding
            // a structure stands under the bare star)
            Value::List(l) if !l.is_empty() && l.iter().all(|v| matches!(v, Value::Record(_))) => {
                let mut names: Vec<String> = Vec::new();
                for r in l.iter() {
                    if let Value::Record(f) = r {
                        for (k, _) in f {
                            if !names.contains(k) {
                                names.push(k.clone());
                            }
                        }
                    }
                }
                let items = names
                    .into_iter()
                    .map(|name| {
                        let cells: Vec<Value> = l
                            .iter()
                            .map(|r| match r {
                                Value::Record(f) => f
                                    .iter()
                                    .find(|(k, _)| *k == name)
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or(Value::Null),
                                _ => Value::Null,
                            })
                            .collect();
                        let ty = if cells.iter().all(flat_cell) {
                            Some(crate::value::type_column(cells).0)
                        } else {
                            None
                        };
                        Value::typed_projection(1, name, star(ty))
                    })
                    .collect();
                return Ok(Value::list(items));
            }
            Value::List(_) => {
                return Err("header reads a table's columns as typed projectors, or a list of records as the table it builds; this list is not all records".into());
            }
            other => {
                return Err(format!(
                    "header reads a table's columns as typed projectors (keys and type are the two lists), not {}",
                    kind_of(other)
                ));
            }
        };
        let Value::Table(t) = t else {
            return Err(format!("{} has no header", kind_of(&t)));
        };
        return Ok(Value::list(t.header_projectors()));
    }
    let t = match table_reading(v)? {
        Value::Table(t) => t,
        other => return Err(format!("{} has no {what}", kind_of(&other))),
    };
    Ok(match what {
        "types" => t.types_value(),
        _ => t.shape(),
    })
}

/// A 1-based position, negative from the end, resolved against a
/// length; `None` out of range.
/// The rows a spec names on a table, as positions in the table's
/// order; `None` for one position out of range (the value is null).
fn table_rows(t: &crate::Table, rows: &IndexSpec) -> Result<Option<Vec<usize>>, String> {
    Ok(Some(match rows {
        IndexSpec::At(n) => match position(*n, t.n_rows()) {
            Some(p) => vec![p - 1],
            None => return Ok(None),
        },
        IndexSpec::Range(a, b) => {
            let (a, b) = span(*a, *b, t.n_rows());
            (a..b).collect()
        }
        IndexSpec::List(_) => listed_positions(rows, t.n_rows())?,
        IndexSpec::Coords(parts) => listed_positions(&IndexSpec::List(parts.clone()), t.n_rows())?,
        IndexSpec::Name(n) => {
            return Err(format!("rows are indexed by position, not by name ('{n}')"));
        }
        IndexSpec::Expr(_) | IndexSpec::Cond(_) => {
            return Err(
                "an index read from an expression is an integer or a list of integers".into(),
            );
        }
    }))
}

/// The columns a spec names on a table, as positions in the table's
/// order.
fn table_columns(t: &crate::Table, spec: &IndexSpec) -> Result<Vec<usize>, String> {
    let width = t.width();
    let by_name = |n: &str| -> Result<usize, String> {
        match &t.header {
            Some(h) => h
                .iter()
                .position(|c| c == n)
                .ok_or_else(|| format!("no column '{n}' (the header: {})", h.join(", "))),
            None => Err(format!(
                "'{n}' names no column: the table has no header, its columns are positions"
            )),
        }
    };
    match spec {
        IndexSpec::At(j) => position(*j, width)
            .map(|p| vec![p - 1])
            .ok_or_else(|| format!("no column {j}: the table has {width}")),
        IndexSpec::Range(a, b) => {
            let (a, b) = span(*a, *b, width);
            Ok((a..b).collect())
        }
        IndexSpec::Name(n) => by_name(n).map(|p| vec![p]),
        IndexSpec::List(parts) | IndexSpec::Coords(parts) => {
            let mut keep = vec![false; width];
            for p in parts {
                match p {
                    IndexSpec::Name(n) => keep[by_name(n)?] = true,
                    IndexSpec::At(j) => {
                        if let Some(p) = position(*j, width) {
                            keep[p - 1] = true;
                        }
                    }
                    IndexSpec::Range(a, b) => {
                        let (a, b) = span(*a, *b, width);
                        keep[a..b].iter_mut().for_each(|k| *k = true);
                    }
                    _ => return Err("a column list holds positions, ranges and names".into()),
                }
            }
            Ok((0..width).filter(|j| keep[*j]).collect())
        }
        IndexSpec::Expr(_) | IndexSpec::Cond(_) => {
            Err("an index read from an expression is an integer or a list of integers".into())
        }
    }
}

/// The cells a value index names on a structure (`map(…; --to=[rows]
/// :col)`): the rows and columns of a table (every column when the
/// index names none, one column by name when given), the positions
/// of a list's items or a record's fields.
pub(crate) enum IndexMask {
    Cells { rows: Vec<usize>, cols: Vec<usize> },
    Positions(Vec<usize>),
}

pub(crate) fn index_mask(
    v: &Value,
    rows: &IndexSpec,
    cols: Option<&IndexSpec>,
    column: Option<&str>,
) -> Result<IndexMask, String> {
    match v {
        Value::Table(t) => {
            let row_set = table_rows(t, rows)?.unwrap_or_default();
            let col_set = match (cols, column) {
                (Some(c), _) => table_columns(t, c)?,
                (None, Some(name)) => table_columns(t, &IndexSpec::Name(name.to_string()))?,
                (None, None) => (0..t.width()).collect(),
            };
            Ok(IndexMask::Cells {
                rows: row_set,
                cols: col_set,
            })
        }
        Value::List(items) => {
            if cols.is_some() || column.is_some() {
                return Err("a list has one dimension: '[i]' or '[a..b]', not a column".into());
            }
            let len = items.len();
            Ok(IndexMask::Positions(match rows {
                IndexSpec::At(n) => position(*n, len).map(|p| vec![p - 1]).unwrap_or_default(),
                IndexSpec::Range(a, b) => {
                    let (a, b) = span(*a, *b, len);
                    (a..b).collect()
                }
                IndexSpec::List(_) => listed_positions(rows, len)?,
                IndexSpec::Coords(parts) => listed_positions(&IndexSpec::List(parts.clone()), len)?,
                IndexSpec::Name(n) => {
                    return Err(format!(
                        "a list is indexed by position, not by name ('{n}')"
                    ));
                }
                IndexSpec::Expr(_) | IndexSpec::Cond(_) => {
                    return Err(
                        "an index read from an expression is an integer or a list of integers"
                            .into(),
                    );
                }
            }))
        }
        Value::Record(fields) => {
            if cols.is_some() || column.is_some() {
                return Err("a record has one dimension: '[i]', not a column".into());
            }
            let len = fields.len();
            Ok(IndexMask::Positions(match rows {
                IndexSpec::At(n) => position(*n, len).map(|p| vec![p - 1]).unwrap_or_default(),
                IndexSpec::Range(a, b) => {
                    let (a, b) = span(*a, *b, len);
                    (a..b).collect()
                }
                IndexSpec::List(_) => listed_positions(rows, len)?,
                IndexSpec::Coords(parts) => listed_positions(&IndexSpec::List(parts.clone()), len)?,
                IndexSpec::Name(n) => fields
                    .iter()
                    .position(|(k, _)| k == n)
                    .map(|p| vec![p])
                    .ok_or_else(|| format!("no field '{n}'"))?,
                IndexSpec::Expr(_) | IndexSpec::Cond(_) => {
                    return Err(
                        "an index read from an expression is an integer or a list of integers"
                            .into(),
                    );
                }
            }))
        }
        other => Err(format!("{} has no index to map over", kind_of(other))),
    }
}

/// The positions a position list names, in the value's order, each
/// once (spec: Index Predicates).
fn listed_positions(spec: &IndexSpec, len: usize) -> Result<Vec<usize>, String> {
    let IndexSpec::List(parts) = spec else {
        return Err("not a position list".into());
    };
    let mut keep = vec![false; len];
    for p in parts {
        match p {
            IndexSpec::At(n) => {
                if let Some(p) = position(*n, len) {
                    keep[p - 1] = true;
                }
            }
            IndexSpec::Range(a, b) => {
                let (a, b) = span(*a, *b, len);
                keep[a..b].iter_mut().for_each(|k| *k = true);
            }
            IndexSpec::Name(n) => {
                return Err(format!(
                    "positions are listed by number, not by name ('{n}')"
                ));
            }
            IndexSpec::List(_) | IndexSpec::Expr(_) | IndexSpec::Cond(_) | IndexSpec::Coords(_) => {
                return Err("a position list holds positions and ranges".into());
            }
        }
    }
    Ok((0..len).filter(|i| keep[*i]).collect())
}

fn position(n: i64, len: usize) -> Option<usize> {
    let n = if n < 0 { len as i64 + 1 + n } else { n };
    (n >= 1 && n as usize <= len).then_some(n as usize)
}

/// A range's ends resolved against a length into `from..to` (0-based,
/// half-open), clamped; either end open.
fn span(a: Option<i64>, b: Option<i64>, len: usize) -> (usize, usize) {
    let l = len as i64;
    let from = a
        .map_or(1, |x| if x < 0 { l + 1 + x } else { x })
        .clamp(1, l + 1)
        - 1;
    let to = b
        .map_or(l, |x| if x < 0 { l + 1 + x } else { x })
        .clamp(0, l);
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
    // An expression index is resolved by the evaluator before it
    // reaches here.
    if matches!(rows, IndexSpec::Expr(_) | IndexSpec::Cond(_)) {
        return Err("an index read from an expression is an integer or a list of integers".into());
    }
    // Three or more written coordinates: a position list on a list
    // or a record; a table has two.
    let coords_as_list;
    let rows = match rows {
        IndexSpec::Coords(parts) => {
            if matches!(v, Value::Table(_)) {
                return Err(
                    "a table has two coordinates, t[i; j]: the listed rows are t[@(1; 2; 3)]"
                        .into(),
                );
            }
            coords_as_list = IndexSpec::List(parts.clone());
            &coords_as_list
        }
        other => other,
    };
    match v {
        Value::List(items) => {
            // `v[2; 4]` on a list: the two coordinates are a position
            // list, as `v[2; 4; 5]` is
            let listed;
            let rows = match cols {
                Some(c) => {
                    listed = IndexSpec::List(vec![rows.clone(), c.clone()]);
                    &listed
                }
                None => rows,
            };
            match rows {
                IndexSpec::At(n) => Ok(position(*n, items.len())
                    .map(|p| items[p - 1].clone())
                    .unwrap_or(Value::Null)),
                IndexSpec::Range(a, b) => {
                    let (from, to) = span(*a, *b, items.len());
                    Ok(Value::list(items[from..to].to_vec()))
                }
                IndexSpec::List(_) => {
                    let chosen = listed_positions(rows, items.len())?;
                    Ok(Value::list(
                        chosen.into_iter().map(|i| items[i].clone()).collect(),
                    ))
                }
                IndexSpec::Name(n) => Err(format!(
                    "a list is indexed by position, not by name ('{n}'); a record's field is ':{n}'"
                )),
                IndexSpec::Expr(_) | IndexSpec::Cond(_) | IndexSpec::Coords(_) => {
                    unreachable!("resolved above")
                }
            }
        }
        Value::Table(t) => {
            let width = t.width();
            let keys: Vec<String> = match &t.header {
                Some(h) => h.clone(),
                None => (1..=width).map(|j| j.to_string()).collect(),
            };
            let column = |spec: &IndexSpec| table_columns(&t, spec);
            let Some(row_set) = table_rows(&t, rows)? else {
                return Ok(Value::Null);
            };
            let one_row = matches!(rows, IndexSpec::At(_));
            let Some(cols) = cols else {
                return Ok(if one_row {
                    t.row(row_set[0] + 1).unwrap_or(Value::Null)
                } else {
                    Value::table(
                        crate::Table::assemble(
                            t.header.clone(),
                            row_set.iter().map(|i| t.row_cells(*i)).collect(),
                            t.declared.clone(),
                            t.index,
                            None,
                        )
                        .with_levels(t.levels),
                    )
                });
            };
            let col_set = column(cols)?;
            let one_col = matches!(cols, IndexSpec::At(_) | IndexSpec::Name(_));
            let cell = |i: usize, j: usize| t.cell(i, j);
            Ok(match (one_row, one_col) {
                (true, true) => cell(row_set[0], col_set[0]),
                (false, true) => {
                    Value::list(row_set.iter().map(|i| cell(*i, col_set[0])).collect())
                }
                (true, false) => Value::Record(
                    col_set
                        .iter()
                        .map(|j| (keys[*j].clone(), cell(row_set[0], *j)))
                        .collect(),
                ),
                (false, false) => Value::table(crate::Table::assemble_levelled(
                    t.levels,
                    t.header
                        .as_ref()
                        .map(|h| col_set.iter().map(|j| h[*j].clone()).collect()),
                    row_set
                        .iter()
                        .map(|i| col_set.iter().map(|j| cell(*i, *j)).collect())
                        .collect(),
                    if t.declared.is_empty() {
                        Vec::new()
                    } else {
                        col_set.iter().map(|j| t.declaration(*j)).collect()
                    },
                    usize::from(t.index > 0 && col_set.first() == Some(&0)),
                    None,
                )),
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
                IndexSpec::List(_) => {
                    let chosen = listed_positions(rows, fields.len())?;
                    Ok(Value::Record(fields.pick(chosen)))
                }
                IndexSpec::Expr(_) | IndexSpec::Cond(_) | IndexSpec::Coords(_) => {
                    unreachable!("resolved above")
                }
                IndexSpec::At(n) => Ok(position(*n, fields.len())
                    .map(|p| fields[p - 1].1.clone())
                    .unwrap_or(Value::Null)),
                IndexSpec::Range(a, b) => {
                    let (from, to) = span(*a, *b, fields.len());
                    Ok(Value::Record(fields.pick(from..to)))
                }
                IndexSpec::Name(n) => Ok(fields
                    .iter()
                    .find(|(k, _)| k == n)
                    .map(|(_, v)| v.clone())
                    .unwrap_or(Value::Null)),
            }
        }
        Value::Str(_) => {
            Err("a string is not indexed: 'substr' and 'cc' read its characters".into())
        }
        other => Err(format!("{} has no index", kind_of(&other))),
    }
}

/// `| [n]` / `| [a..b]` on a table or a list value: the row or the
/// item, the rows or the sublist ([`index_value`]). On anything else,
/// the whole-context refusal positional selection has always given on
/// the plain pipe.
pub(crate) fn table_rows_at(call: &FnCall, v: Value) -> Result<Value, String> {
    if !matches!(v, Value::Table(_) | Value::List(_) | Value::Record(_)) {
        return Err(
            "positional selection is whole-context; write '@| [n]' / '@| [a..b]' (a plain '| [cond]' filters per capsa; on a table, a list or a record value, '| [n]' is the row, the item or the field)".into(),
        );
    }
    // one position or range, a position list (`| [2; 4; 5]`), or a
    // value read from an expression (`| [$.pos]`: an integer, or a
    // list of integers)
    let part = |a: &Arg| -> Result<IndexSpec, String> {
        match a {
            Arg::Lit(Value::Int(n)) => Ok(IndexSpec::At(*n)),
            Arg::Range(a, b) => Ok(IndexSpec::Range(*a, *b)),
            Arg::Lit(Value::List(items)) => items
                .iter()
                .map(|v| match v {
                    Value::Int(n) => Ok(IndexSpec::At(*n)),
                    _ => Err(
                        "an index read from an expression is an integer or a list of integers"
                            .to_string(),
                    ),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(IndexSpec::List),
            _ => Err(
                "'[n]' takes a position, a range, or a list of them: [2], [2..4], [2; 4; 5]".into(),
            ),
        }
    };
    let spec = match call.args.as_slice() {
        [one] => part(one)?,
        many => IndexSpec::List(many.iter().map(part).collect::<Result<Vec<_>, _>>()?),
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
            let Some(first) = t.row_at(0) else {
                return Err("row-to-header: the table has no row to lift".into());
            };
            crate::Table::new(
                Some(first.iter().map(|v| v.to_string()).collect()),
                t.rows().skip(1).map(|r| r.cells()).collect(),
            )
        }
        _ => {
            let mut rows = Vec::with_capacity(t.n_rows() + 1);
            if let Some(h) = &t.header {
                rows.push(h.iter().cloned().map(Value::Str).collect());
            }
            rows.extend(t.rows().map(|r| r.cells()));
            crate::Table::new(None, rows)
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
                return Ok(Value::record(Vec::new()));
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
/// A transposition lays each row across the columns, so the cells
/// of columns of several kinds end in one column and are brought to
/// their common type (an integer beside a float is a float, anything
/// beside text is text, a boolean beside a number is 1 or 0). Told
/// once, as an info, naming the kinds in the columns' order and the
/// type they were brought to (the user, 2026-11-10).
fn transpose_cast_told(t: &crate::Table) {
    let data: Vec<usize> = (t.index..t.width()).collect();
    if data.len() < 2 || t.n_rows() == 0 {
        return;
    }
    let kinds: Vec<String> = data
        .iter()
        .map(|&j| match t.declaration(j) {
            Some(ty) => ty.word(),
            None => crate::value::type_column(t.column_cells(j)).0.word(),
        })
        .collect();
    if kinds.iter().all(|k| *k == kinds[0]) {
        return;
    }
    let all: Vec<Value> = data.iter().flat_map(|&j| t.column_cells(j)).collect();
    let brought = crate::value::type_column(all).0.word();
    crate::value::ambient::record_info_once(
        crate::diagnostic::TRANSPOSE_CAST,
        "pipeline",
        format!(
            "transpose: the cells of {} columns ({}) were brought to {brought}",
            data.len(),
            kinds.join(", ")
        ),
    );
}

/// A frame transposed: the header as the first column (`column`),
/// each old row a column numbered by position.
fn transposed_frame(t: &crate::Table) -> Result<Value, String> {
    transpose_cast_told(t);
    let names = t.header.clone().unwrap_or_default();
    // on an indexed table the index and the header exchange (draft
    // 166), with all their levels: the labels name the new columns
    // (several levels a path `bar:one`), the old names become the
    // labels, their levels split into the index's columns named by
    // position (`level`, `level2`, …) under the index's names
    // the names exchange with the axes (draft 166): the header's name
    // becomes the new index column's (`column` where it has none),
    // the index column's name the header's
    let first = t.name.clone().unwrap_or_else(|| "column".to_string());
    if t.index > 0 {
        // the old names part into the new index's columns where
        // they are paths; a title is one label, colons and all
        let parts = |name: &str| -> Vec<Value> {
            if t.levels {
                name.split(':').map(|s| Value::Str(s.to_string())).collect()
            } else {
                vec![Value::Str(name.to_string())]
            }
        };
        let depth = names[t.index..]
            .iter()
            .map(|n| parts(n).len())
            .max()
            .unwrap_or(1);
        let mut header: Vec<String> = (0..depth)
            .map(|i| {
                if i == 0 {
                    first.clone()
                } else {
                    format!("{first}{}", i + 1)
                }
            })
            .collect();
        // the labels of a composite index become paths, and the
        // header then parts every name at its colons: a level that
        // holds one of its own is refused naming it
        if t.index > 1
            && let Some(level) = t.rows().find_map(|r| {
                (0..t.index)
                    .map(|j| crate::value::Record::key_text(&r.get(j)))
                    .find(|l| l.contains(':'))
            })
        {
            return Err(format!(
                "the labels of a composite index become the paths outer:inner, and \
                 the label '{level}' holds a colon, which would read as a level: respell \
                 the labels first (map(…; --to=:::index)), or index by one level"
            ));
        }
        header.extend(t.rows().map(|r| t.label_name(&r.cells())));
        let rows: Vec<Vec<Value>> = names
            .iter()
            .enumerate()
            .skip(t.index)
            .map(|(j, name)| {
                let mut row: Vec<Value> = parts(name);
                row.resize(depth, Value::Null);
                row.extend(t.rows().map(|r| r.get(j)));
                row
            })
            .collect();
        let new_name = (t.index == 1).then(|| names[0].clone());
        return Ok(Value::table(
            crate::Table::new(Some(header), rows)
                .with_index(depth)
                .with_name(new_name)
                // the labels of a composite index are joined into
                // paths (`label_name`); one level's labels are names
                .with_levels(t.index > 1),
        ));
    }
    let mut header = vec![first];
    header.extend((1..=t.n_rows()).map(|i| i.to_string()));
    let rows: Vec<Vec<Value>> = names
        .iter()
        .enumerate()
        .map(|(j, name)| {
            let mut row = vec![Value::Str(name.clone())];
            row.extend(t.rows().map(|r| r.get(j)));
            row
        })
        .collect();
    Ok(Value::table(crate::Table::new(Some(header), rows)))
}

pub(crate) fn transpose_reading(v: Value) -> Result<Value, String> {
    // The rows of one width, from an array or a list of lists.
    let rows: Vec<Vec<Value>> = match v {
        Value::Foreign(f) => {
            return Err(format!(
                "{} is a library value; np:transpose exchanges its axes",
                f.kind()
            ));
        }
        Value::Table(t) => match &t.header {
            None => {
                transpose_cast_told(&t);
                t.rows_vec()
            }
            // A frame transposes to a frame (spec: Tables and
            // Reshapes): the header becomes the first column, named
            // `column`, each old row a column numbered by position;
            // `| values | transpose` is the matrix of the cells alone.
            Some(_) => return transposed_frame(&t),
        },
        // A record of lists is a frame's column reading, and a list
        // of records its row reading: both transpose as the frame.
        Value::Record(fields)
            if !fields.is_empty() && fields.iter().all(|(_, v)| matches!(v, Value::List(_))) =>
        {
            match table_reading(Value::Record(fields))? {
                Value::Table(t) => return transposed_frame(&t),
                other => return Err(format!("{} is no table", kind_of(&other))),
            }
        }
        Value::List(items)
            if !items.is_empty() && items.iter().all(|v| matches!(v, Value::Record(_))) =>
        {
            match table_reading(Value::List(items))? {
                Value::Table(t) => return transposed_frame(&t),
                other => return Err(format!("{} is no table", kind_of(&other))),
            }
        }
        Value::List(items) => {
            if items.is_empty() {
                return Ok(Value::table(crate::Table::new(None, Vec::new())));
            }
            let mut rows: Vec<Vec<Value>> = Vec::with_capacity(items.len());
            for (i, v) in items.iter().enumerate() {
                match v {
                    Value::List(l) => rows.push(l.to_vec()),
                    other => {
                        return Err(format!(
                            "row {} is {}, not a list; transpose takes an array or a list of lists",
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
            if let Some((i, j, c)) = rows.iter().enumerate().find_map(|(i, r)| {
                r.iter()
                    .enumerate()
                    .find(|(_, c)| !flat_cell(c))
                    .map(|(j, c)| (i, j, c))
            }) {
                return Err(format!(
                    "row {}, column {} holds {}; a matrix is flat, its cell a scalar or null",
                    i + 1,
                    j + 1,
                    kind_of(c)
                ));
            }
            rows
        }
        other => {
            return Err(format!(
                "transpose takes an array or a list of lists, not {}",
                kind_of(&other)
            ));
        }
    };
    // Every cell coerced together to one type, then the exchange:
    // the matrix is an array of one type.
    let m = crate::Table::matrix(rows);
    let w = m.width();
    let exchanged: Vec<Vec<Value>> = (0..w)
        .map(|j| m.rows().map(|r| r.get(j)).collect())
        .collect();
    Ok(Value::table(crate::Table::assemble(
        None,
        exchanged,
        Vec::new(),
        0,
        None,
    )))
}

/// The named fields of a call's bare-word operands.
/// A bare field names its column (`melt(:id)`), as a word and a
/// projector do; the command's names stay names, unevaluated.
fn field_name(o: &crate::syntax::ast::Operand) -> Option<&str> {
    use crate::syntax::ast::Operand;
    match o {
        Operand::Field {
            base,
            name,
            bare: true,
        } if matches!(**base, Operand::Topic) => Some(name.as_str()),
        Operand::Lit(Value::Str(s)) => Some(s.as_str()),
        Operand::Lit(Value::Projection { key, .. }) => Some(key.as_str()),
        _ => None,
    }
}

fn word_args(call: &FnCall) -> Vec<&str> {
    let mut out = Vec::new();
    for a in &call.args {
        match a {
            Arg::Lit(Value::Str(s)) => out.push(s.as_str()),
            Arg::Lit(Value::Projection { key, .. }) => out.push(key.as_str()),
            Arg::Expr(crate::syntax::ast::Operand::List(items)) => {
                out.extend(items.iter().filter_map(field_name))
            }
            Arg::Expr(e) => out.extend(field_name(e)),
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

/// The reducers over structures (spec: Aggregating Pipe, draft
/// 164): the numeric and statistical reducers over a record reduce
/// its values, over a table each column into a record keyed by the
/// header (a column with no numeric value is null); `count` counts a
/// record's fields and a table's rows. `None` for every other
/// pairing, which reads the structure as one value.
pub(crate) fn reduce_structure(call: &FnCall, topic: &Value, scale: Scale) -> Option<Value> {
    const NUMERIC: [&str; 12] = [
        "sum",
        "product",
        "min",
        "max",
        "mean",
        "avg",
        "median",
        "percentile",
        "std",
        "stddev",
        "var",
        "variance",
    ];
    let name = call.name.as_str();
    let one = |items: Vec<Value>| {
        apply(call, items, scale)
            .into_iter()
            .next()
            .unwrap_or(Value::Null)
    };
    let switch = |long: &str, short: &str| {
        call.args
            .iter()
            .any(|a| matches!(a, Arg::Opt { name, value: None } if name == long || name == short))
    };
    match topic {
        Value::Record(fields) if name == "count" => Some(Value::Int(fields.len() as i64)),
        // `count(-v)` / `count(-n)` over a table: the record of the
        // per-column counts of the values (of the nulls), the index
        // left out, as describe's count row has them (draft.213)
        Value::Table(t)
            if name == "count" && (switch("--values", "-v") || switch("--nulls", "-n")) =>
        {
            let nulls = switch("--nulls", "-n");
            Some(Value::record(
                (t.index..t.width())
                    .map(|j| {
                        let n = t
                            .rows()
                            .filter(|r| matches!(r.get(j), Value::Null) == nulls)
                            .count();
                        (t.column_name(j), Value::Int(n as i64))
                    })
                    .collect(),
            ))
        }
        Value::Table(t) if name == "count" => Some(Value::Int(t.n_rows() as i64)),
        Value::Record(fields) if NUMERIC.contains(&name) => {
            Some(one(fields.iter().map(|(_, v)| v.clone()).collect()))
        }
        // a list of records of one shape reduces the table it would
        // build, column by column (draft 166)
        Value::List(items) if NUMERIC.contains(&name) && one_shape(items, 1) => {
            match table_reading(topic.clone()) {
                Ok(t @ Value::Table(_)) => reduce_structure(call, &t, scale),
                _ => None,
            }
        }
        // the index columns are left out (as `describe` leaves them),
        // and a column's cells are read plain, whatever the labels
        // key its value by (draft 166)
        Value::Table(t) if NUMERIC.contains(&name) => Some(Value::record(
            (t.index..t.width())
                .map(|j| {
                    let cells: Vec<Value> = t.rows().map(|r| r.get(j)).collect();
                    (t.column_name(j), one(cells))
                })
                .collect(),
        )),
        _ => None,
    }
}

/// A declared option's literal text, when written.
fn opt_text(call: &FnCall, name: &str) -> Option<String> {
    call.args.iter().find_map(|a| match a {
        Arg::Opt { name: n, value } if n == name => Some(match value {
            Some(crate::syntax::ast::Operand::Lit(l)) => l.to_string(),
            Some(other) => crate::syntax::unparse::operand_text(other),
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
    // the three operands: names, projectors, or a list of projectors
    // as the row key (several fields, the result's composite index)
    let name_of = |a: &Arg| -> Option<Vec<String>> {
        match a {
            Arg::Lit(Value::Str(s)) => Some(vec![s.clone()]),
            Arg::Lit(Value::Projection { key, .. }) => Some(vec![key.clone()]),
            Arg::Lit(Value::List(items)) => items
                .iter()
                .map(|i| match i {
                    Value::Projection { key, .. } => Some(key.clone()),
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            Arg::Expr(crate::syntax::ast::Operand::Field { base, name, .. })
                if matches!(**base, crate::syntax::ast::Operand::Topic) =>
            {
                Some(vec![name.clone()])
            }
            Arg::Expr(crate::syntax::ast::Operand::List(items)) => items
                .iter()
                .map(|i| match i {
                    crate::syntax::ast::Operand::Field { base, name, .. }
                        if matches!(**base, crate::syntax::ast::Operand::Topic) =>
                    {
                        Some(name.clone())
                    }
                    crate::syntax::ast::Operand::Lit(Value::Str(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    };
    let operands: Vec<&Arg> = call
        .args
        .iter()
        .filter(|a| !matches!(a, Arg::Opt { .. }))
        .collect();
    if !(operands.is_empty() || operands.len() == 3) {
        return Err(
            "pivot takes the row, column and value fields (pivot(row; column; value), the row a projector list for several), or no operand to read the first three fields by position"
                .into(),
        );
    }
    let named: Option<(Vec<String>, String, String)> = if operands.is_empty() {
        None
    } else {
        let row = name_of(operands[0])
            .ok_or("pivot: the row key is a field name or a list of projectors")?;
        let col = name_of(operands[1])
            .and_then(|v| v.into_iter().next())
            .ok_or("pivot: the column key is a field name")?;
        let val = name_of(operands[2])
            .and_then(|v| v.into_iter().next())
            .ok_or("pivot: the value is a field name")?;
        Some((row, col, val))
    };
    // a table pivots its rows; a stream of records the records: both
    // answer ONE table indexed by the row key (composite when the key
    // is several fields), as the spec says (draft 166)
    let input: Vec<Value> = match input.as_slice() {
        [Value::Table(t)] => match rows_reading(Value::table(t.as_ref().clone().drop_index()))? {
            Value::List(l) => l.to_vec(),
            other => vec![other],
        },
        _ => input,
    };
    let wide = pivot_records(&named, input)?;
    let row_names: Vec<String> = match (&named, wide.first()) {
        (Some((row, _, _)), _) => row.clone(),
        (None, Some(Value::Record(r))) => {
            vec![r.first().map(|(k, _)| k.clone()).unwrap_or_default()]
        }
        _ => Vec::new(),
    };
    if wide.is_empty() {
        return Ok(Vec::new());
    }
    match table_reading(Value::list(wide))? {
        Value::Table(wt) if !row_names.is_empty() => {
            let levels: Vec<(String, Option<crate::value::ColumnType>)> =
                row_names.into_iter().map(|n| (n, None)).collect();
            Ok(vec![Value::table(wt.indexed_by_levels(&levels)?)])
        }
        other => Ok(vec![other]),
    }
}

/// The wide records of a pivot: one per row key, in first
/// appearance, the row fields then one field per column key.
fn pivot_records(
    named: &Option<(Vec<String>, String, String)>,
    input: Vec<Value>,
) -> Result<Vec<Value>, String> {
    let mut row_names: Vec<String> = Vec::new();
    // the row keys become the result's index: one row per label,
    // the labels equal as values (spec: Indexed tables)
    let mut row_classes = crate::value::LabelIndex::new();
    let mut row_keys: Vec<Vec<Value>> = Vec::new();
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
        let (rows, col, val): (Vec<(String, Value)>, &Value, &Value) = match named {
            None => {
                if fields.len() < 3 {
                    return Err(format!(
                        "pivot: record {} has {} field(s); three are needed (row, column, value)",
                        i + 1,
                        fields.len()
                    ));
                }
                (vec![fields[0].clone()], &fields[1].1, &fields[2].1)
            }
            Some((row, col, val)) => {
                let get = |n: &str| {
                    field_of(fields, n)
                        .ok_or_else(|| format!("pivot: record {} has no field '{n}'", i + 1))
                };
                let rows = row
                    .iter()
                    .map(|n| Ok::<_, String>((n.clone(), get(n)?.clone())))
                    .collect::<Result<Vec<_>, _>>()?;
                (rows, get(col)?, get(val)?)
            }
        };
        if row_names.is_empty() {
            row_names = rows.iter().map(|(k, _)| k.clone()).collect();
        }
        let key: Vec<Value> = rows.into_iter().map(|(_, v)| v).collect();
        let (r, new) = row_classes.intern(&key);
        if new {
            row_keys.push(key);
        }
        // a column has a title: a null key is refused (ruling
        // 2026-11-10); it landed under the title ""
        if matches!(col, Value::Null) {
            return Err(format!(
                "pivot: record {} has a null column key, and a column has a title: drop the \
                 record, or name the key (`?? \"none\"`) before the pivot",
                i + 1
            ));
        }
        let ck = col.to_string();
        let c = match col_keys.iter().position(|k| *k == ck) {
            Some(c) => c,
            None => {
                col_keys.push(ck);
                col_keys.len() - 1
            }
        };
        if let Some(prev) = cells.get(&(r, c)) {
            // a null prints as the word, not as nothing
            let shown = |v: &Value| match v {
                Value::Null => "null".to_string(),
                v => v.to_string(),
            };
            return Err(format!(
                "pivot: two values for row '{}', column '{}' ({} and {}); aggregate before the pivot",
                row_keys[r]
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                col_keys[c],
                shown(prev),
                shown(val)
            ));
        }
        cells.insert((r, c), val.clone());
    }
    Ok(row_keys
        .iter()
        .enumerate()
        .map(|(r, key)| {
            let mut rec: Vec<(String, Value)> =
                row_names.iter().cloned().zip(key.iter().cloned()).collect();
            for (c, ck) in col_keys.iter().enumerate() {
                rec.push((
                    ck.clone(),
                    cells.get(&(r, c)).cloned().unwrap_or(Value::Null),
                ));
            }
            Value::Record(rec.into())
        })
        .collect())
}

/// `melt(id; …)` — wide to long over records: for every field of a
/// record that is not an id, one record of the ids, the field's
/// name and its value, under `name` and `value` (`--name=`,
/// `--value=` rename them).
fn melt(call: &FnCall, input: Vec<Value>) -> Result<Vec<Value>, String> {
    let ids = word_args(call);
    let name_field = opt_text(call, "--name").unwrap_or_else(|| "variable".into());
    let value_field = opt_text(call, "--value").unwrap_or_else(|| "value".into());
    // A table value melts its rows and answers the long table (spec:
    // Tables and Reshapes).
    if let [Value::Table(_)] = input.as_slice() {
        let Some(Value::Table(t)) = input.into_iter().next() else {
            unreachable!()
        };
        // on an indexed table the index is the id, and the long table
        // is indexed by it with `variable` as the inner level (spec:
        // The Table, "Levels of the header"). An id named beside the
        // index is an id too (ruling 2026-11-10): it was melted as
        // one more variable, its values among the others'.
        if t.index > 0 {
            let levels: Vec<String> = t.level_names();
            let mut keyed = call.clone();
            let mut args: Vec<Arg> = levels
                .iter()
                .map(String::as_str)
                .chain(
                    ids.iter()
                        .copied()
                        .filter(|id| !levels.iter().any(|l| l == id)),
                )
                .map(|n| Arg::Lit(Value::Str(n.to_string())))
                .collect();
            args.extend(
                keyed
                    .args
                    .iter()
                    .filter(|a| matches!(a, Arg::Opt { .. }))
                    .cloned(),
            );
            keyed.args = args;
            let plain = t.as_ref().clone().drop_index();
            let records = match rows_reading(Value::table(plain))? {
                Value::List(l) => l.to_vec(),
                other => vec![other],
            };
            let long = melt(&keyed, records)?;
            let mut index: Vec<(String, Option<crate::value::ColumnType>)> =
                levels.iter().map(|n| (n.clone(), None)).collect();
            index.push((name_field.clone(), None));
            return match table_reading(Value::list(long))? {
                Value::Table(lt) => Ok(vec![Value::table(lt.indexed_by_levels(&index)?)]),
                other => Ok(vec![other]),
            };
        }
        let records = match rows_reading(Value::Table(t))? {
            Value::List(l) => l.to_vec(),
            other => vec![other],
        };
        let long = melt(call, records)?;
        return match table_reading(Value::list(long))? {
            t @ Value::Table(_) => Ok(vec![t]),
            other => Ok(vec![other]),
        };
    }
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
            out.push(Value::Record(rec.into()));
        }
    }
    Ok(out)
}

fn arg_int(call: &FnCall, n: usize) -> Option<i64> {
    match call.args.get(n) {
        Some(Arg::Lit(Value::Int(i))) => Some(*i),
        Some(Arg::Lit(Value::Null)) | None => None,
        Some(other) => {
            crate::value::ambient::record_refusal(format!(
                "{}: operand {} must be an integer, not {}",
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
            crate::value::ambient::record_refusal(format!(
                "{}: operand {} must be text, not {}",
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
        Arg::Key(_) => "a key",
        Arg::Expr(_) => "an expression",
        Arg::Hole(_) => "an expansion",
        Arg::Range(..) => "a range",
        Arg::Named { .. } | Arg::Opt { .. } => "an option",
        Arg::Pairs { .. } => "a pairwise field",
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
            headless: false,
            name: name.into(),
            args: Vec::new(),
        };
        apply(&call, input, &crate::value::quantity::scale_expr)
    }

    fn sc(name: &str, topic: Value) -> Vec<Value> {
        let call = FnCall {
            headless: false,
            name: name.into(),
            args: Vec::new(),
        };
        apply_scalar(&call, topic, &crate::value::quantity::scale_expr)
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
        let d = |secs: i64| Value::seconds(secs, 0);
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
        // over a number: the span, written in the unit (30_d)
        assert_eq!(
            scalar_with("days", Value::Int(30), vec![]),
            vec![Value::Quantity {
                value: 30.0 * 86400.0,
                base: "s".into(),
                written: Some((30.0, "d".into())),
            }]
        );
    }

    /// `ngrams(n)`: the n-word windows of a text as space-joined
    /// strings, one per window; a text shorter than n yields none;
    /// the default n is 2.
    #[test]
    fn ngrams_windows_the_words() {
        let call = |n: Option<i64>| FnCall {
            headless: false,
            name: "ngrams".into(),
            args: n.map(|n| vec![Arg::Lit(Value::Int(n))]).unwrap_or_default(),
        };
        let text = Value::Str("a  b c\nd".into());
        let strs = |vs: Vec<Value>| vs.into_iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(
            strs(apply_scalar(
                &call(Some(2)),
                text.clone(),
                &crate::value::quantity::scale_expr
            )),
            vec!["a b", "b c", "c d"]
        );
        assert_eq!(
            strs(apply_scalar(
                &call(Some(3)),
                text.clone(),
                &crate::value::quantity::scale_expr
            )),
            vec!["a b c", "b c d"]
        );
        assert_eq!(
            strs(apply_scalar(
                &call(None),
                text.clone(),
                &crate::value::quantity::scale_expr
            )),
            vec!["a b", "b c", "c d"]
        );
        assert!(apply_scalar(&call(Some(5)), text, &crate::value::quantity::scale_expr).is_empty());
    }

    /// The character classes keep their class (plus the named
    /// extras) and space out the rest, so `words` then spreads
    /// the kept runs; `digit` keeps characters, never a value.
    #[test]
    fn character_classes_space_out_the_rest() {
        let call = |name: &str, keep: Option<&str>| FnCall {
            headless: false,
            name: name.into(),
            args: keep
                .map(|k| vec![Arg::Lit(Value::Str(k.into()))])
                .unwrap_or_default(),
        };
        let one = |name: &str, keep: Option<&str>, text: &str| {
            apply_scalar(
                &call(name, keep),
                Value::Str(text.into()),
                &crate::value::quantity::scale_expr,
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

    fn scalar_with(name: &str, topic: Value, args: Vec<crate::syntax::ast::Arg>) -> Vec<Value> {
        let call = FnCall {
            headless: false,
            name: name.into(),
            args,
        };
        apply_scalar(&call, topic, &crate::value::quantity::scale_expr)
    }

    /// The 0.25 text round: slice/pads/repeat/title, the one-sided
    /// trims, and the count family (cc codepoints, bc bytes, wc
    /// words, lc lines; `chars` the pre-0.25 alias of cc).
    #[test]
    fn text_round_commands() {
        use crate::syntax::ast::Arg;
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
            base: "m^2*kg/s^3".into(),
            written: written.map(|(m, u)| (m, u.to_string())),
        };
        // Same written unit: the total keeps it.
        let watts = vec![q(142.5, Some((142.5, "W"))), q(290.0, Some((290.0, "W")))];
        assert_eq!(
            agg("sum", watts.clone()),
            vec![q(432.5, Some((432.5, "W")))]
        );
        assert_eq!(agg("mean", watts), vec![q(216.25, Some((216.25, "W")))]);
        // Mixed units of one dimension: the first value's unit.
        let mixed = vec![q(1200.0, Some((1.2, "kW"))), q(350.0, Some((350.0, "W")))];
        assert_eq!(
            agg("sum", mixed.clone()),
            vec![q(1550.0, Some((1.55, "kW")))]
        );
        assert_eq!(agg("median", mixed), vec![q(775.0, Some((0.775, "kW")))]);
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
        let d = |secs: i64| Value::seconds(secs, 0);
        // Durations total and average to typed durations.
        assert_eq!(
            agg("sum", vec![d(2700), d(43200), d(10800)]),
            vec![d(56700)] // PT15H45M
        );
        assert_eq!(
            agg("mean", vec![d(2700), d(43200), d(10800)]),
            vec![d(18900)] // PT5H15M
        );
        // Mixed with span text: still spans. The first value has no
        // written unit: the total in seconds.
        assert_eq!(
            agg("sum", vec![d(60), Value::Str("2min".into())]),
            vec![d(180)]
        );
        // A number without a unit among spans is refused, as beside
        // any quantity (the user, 2026-11-10: time is a dimension like
        // the others; it read as seconds). The run voids the answer.
        assert_eq!(
            agg(
                "sum",
                vec![d(60), Value::Str("2min".into()), Value::Int(60)]
            ),
            vec![Value::Null]
        );
        assert!(
            crate::value::ambient::take_refusal()
                .is_some_and(|r| r.contains("s and a bare number"))
        );
        // An unreadable element is missing to the fold, as it is to
        // the numeric one (ruling 2026-11-08): the durations total.
        assert_eq!(
            agg("sum", vec![d(60), Value::Str("soon".into())]),
            vec![d(60)]
        );
        assert_eq!(
            agg("sum", vec![d(60), Value::Null, Value::Str("NA".into())]),
            vec![d(60)]
        );
        // Numeric text reads as a bare number does, and is refused
        // with it, under every fold of time.
        for reducer in ["sum", "mean", "median", "stddev"] {
            assert_eq!(
                agg(reducer, vec![d(60), Value::Str("5".into())]),
                vec![Value::Null],
                "{reducer}"
            );
            assert!(
                crate::value::ambient::take_refusal()
                    .is_some_and(|r| r.contains("s and a bare number")),
                "{reducer}"
            );
        }
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
        // without the resolver a spaced unknown unit is text, and the
        // two compare as text
        assert_eq!(
            a.compare_with(&b, &crate::value::quantity::scale_expr),
            Ordering::Less
        );
    }

    #[test]
    fn quantital_fold_lifts_text_refuses_numbers() {
        let q = |value: f64, written: Option<(f64, &str)>| Value::Quantity {
            value,
            base: "b".into(),
            written: written.map(|(v, u)| (v, u.to_string())),
        };
        // A typed quantity engages the fold; unit text lifts, as in
        // ± arithmetic — and the shared written unit survives.
        let mixed = vec![
            q(80_000_000.0, Some((10.0, "MB"))),
            Value::Str("5MB".into()),
        ];
        assert_eq!(agg("sum", mixed.clone())[0].to_string(), "15_MB");
        assert_eq!(agg("mean", mixed.clone())[0].to_string(), "7.5_MB");
        assert_eq!(agg("min", mixed)[0].to_string(), "5MB");
        // A bare number refuses, exactly as `Q + 5` does…
        assert_eq!(
            agg("sum", vec![q(80_000_000.0, None), Value::Int(5)]),
            vec![Value::Null]
        );
        // …and so does cross-dimension text; readingless text, a null
        // and a bool are missing to the fold, as to the numeric one
        // (ruling 2026-11-08).
        assert_eq!(
            agg("sum", vec![q(80_000_000.0, None), Value::Str("5km".into())]),
            vec![Value::Null]
        );
        assert_eq!(
            agg(
                "sum",
                vec![
                    q(80_000_000.0, None),
                    Value::Str("soon".into()),
                    Value::Null,
                    Value::Str("NA".into()),
                ]
            ),
            vec![q(80_000_000.0, None)]
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
            headless: false,
            name: name.into(),
            args: vec![Arg::Opt {
                name: "--sample".into(),
                value: None,
            }],
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
            headless: false,
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
            headless: false,
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
            headless: false,
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
            crate::syntax::registry::spec("zscore")
                .is_some_and(|s| s.agg == Some(crate::syntax::registry::Agg::Context))
                && association("spearman")
        );
    }

    #[test]
    fn math_scalars() {
        let c = |name: &str, args: Vec<Arg>| FnCall {
            headless: false,
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
            headless: false,
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
            headless: false,
            name: "sort".into(),
            args: loc
                .map(|l| vec![crate::syntax::ast::Arg::Lit(Value::Str(l.into()))])
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
                &crate::value::quantity::scale_expr
            )),
            ["еда", "ёж", "Öl", "Zebra"]
        );
        // Swedish: Ö is its own letter, after Z.
        assert_eq!(
            texts(apply(
                &call(Some("sv-SE")),
                words(),
                &crate::value::quantity::scale_expr
            )),
            ["Zebra", "Öl", "еда", "ёж"]
        );
        // German: Ö sorts with O, before Z.
        assert_eq!(
            texts(apply(
                &call(Some("de-DE")),
                words(),
                &crate::value::quantity::scale_expr
            )),
            ["Öl", "Zebra", "еда", "ёж"]
        );
        // No locale: the standard comparison (codepoint for text).
        assert_eq!(
            texts(apply(
                &call(None),
                words(),
                &crate::value::quantity::scale_expr
            )),
            ["Zebra", "Öl", "еда", "ёж"]
        );
    }

    #[test]
    fn encodings() {
        let apply1 = |name: &str, v: &str| {
            apply_scalar(
                &FnCall {
                    headless: false,
                    name: name.into(),
                    args: Vec::new(),
                },
                Value::Str(v.into()),
                &crate::value::quantity::scale_expr,
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
        // a lone value with no numeric coercion answers null (ruling
        // 2026-11-10; the run warns once for the command)
        assert_eq!(sc("round", Value::Str("n/a".into())), vec![Value::Null]);
        assert_eq!(sc("floor", Value::Null), vec![Value::Null]);
        assert_eq!(sc("abs", Value::Bool(true)), vec![Value::Null]);
        assert_eq!(sc("sqrt", Value::Str("n/a".into())), vec![Value::Null]);
        // in a structure a cell of another kind stays as it is, and
        // a cell the command applies to and fails on is null
        let cells = || {
            Value::list(vec![
                Value::Float(2.5),
                Value::Str("n/a".into()),
                Value::Bool(true),
                Value::Null,
            ])
        };
        assert_eq!(
            sc("round", cells()),
            vec![Value::list(vec![
                Value::Int(3),
                Value::Str("n/a".into()),
                Value::Bool(true),
                Value::Null,
            ])]
        );
        assert_eq!(
            sc(
                "sqrt",
                Value::list(vec![Value::Int(-1), Value::Str("n/a".into())])
            ),
            vec![Value::list(vec![Value::Null, Value::Str("n/a".into())])]
        );
        // every numeric command is a scalar command: the rule is
        // decided where a scalar command broadcasts
        for name in NUMERIC_COMMANDS {
            assert!(crate::syntax::man::is_scalar_kind(name), "{name}");
        }
    }

    fn sc_args(name: &str, args: Vec<Arg>, topic: Value) -> Vec<Value> {
        let call = FnCall {
            headless: false,
            name: name.into(),
            args,
        };
        apply_scalar(&call, topic, &crate::value::quantity::scale_expr)
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
        // Numeric reading still applies; a lone value with no reading
        // answers null (ruling 2026-11-10), a cell stays as it is.
        assert_eq!(
            sc_args("round", d(1), Value::Str("2.44".into())),
            vec![Value::Float(2.4)]
        );
        assert_eq!(
            sc_args("round", d(2), Value::Str("n/a".into())),
            vec![Value::Null]
        );
        assert_eq!(
            sc_args("round", d(2), Value::list(vec![Value::Str("n/a".into())])),
            vec![Value::list(vec![Value::Str("n/a".into())])]
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
            calendar: Default::default(),
        };
        assert_eq!(
            sc("isoformat", inst),
            vec![Value::Str("2024-02-15T13:26:40Z".into())]
        );
        assert_eq!(
            sc("isoformat", Value::Str("2024-02-15T14:26:40+01:00".into())),
            vec![Value::Str("2024-02-15T13:26:40Z".into())]
        );
        // ruling 2026-11-10: an instant read with no offset is a
        // point on the UTC timeline too, and a date is its midnight
        assert_eq!(
            sc("isoformat", Value::Str("2024-02-15T10:00:00".into())),
            vec![Value::Str("2024-02-15T10:00:00Z".into())]
        );
        assert_eq!(
            sc("isoformat", Value::Str("2024-02-15".into())),
            vec![Value::Str("2024-02-15T00:00:00Z".into())]
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
    fn abs_over_quantity_keeps_its_unit() {
        // quantity-aware like the rounders (the inventory's rule)
        let q = Value::Quantity {
            value: -5000.0,
            base: "m".into(),
            written: Some((-5.0, "km".into())),
        };
        assert_eq!(
            sc("abs", q),
            vec![Value::Quantity {
                value: 5000.0,
                base: "m".into(),
                written: Some((5.0, "km".into())),
            }]
        );
    }

    #[test]
    fn a_null_topic_has_no_reading() {
        // a scalar reading a null topic answers null, an expander
        // nothing; the commands that read null on purpose keep theirs
        for name in [
            "upper", "cc", "gc", "bc", "wc", "lc", "sc", "sha256", "repeat", "nfc",
        ] {
            assert_eq!(sc(name, Value::Null), vec![Value::Null], "{name}");
        }
        let n = |i: i64| crate::syntax::ast::Arg::Lit(Value::Int(i));
        let a = crate::syntax::ast::Arg::Lit(Value::Str("a".into()));
        assert_eq!(
            scalar_with("pad", Value::Null, vec![n(3)]),
            vec![Value::Null]
        );
        assert_eq!(
            scalar_with("lpad", Value::Null, vec![n(3)]),
            vec![Value::Null]
        );
        assert_eq!(
            scalar_with("indent", Value::Null, vec![n(2)]),
            vec![Value::Null]
        );
        assert_eq!(
            scalar_with("levenshtein", Value::Null, vec![a]),
            vec![Value::Null]
        );
        for name in ["split", "tokens", "lines", "words", "sentences"] {
            assert_eq!(sc(name, Value::Null), Vec::<Value>::new(), "{name}");
        }
        assert_eq!(sc("type", Value::Null), vec![Value::Str("null".into())]);
        assert_eq!(sc("json", Value::Null), vec![Value::Str("null".into())]);
        // first and last of an empty input reduce to null
        let first = FnCall {
            headless: false,
            name: "first".into(),
            args: Vec::new(),
        };
        assert_eq!(
            apply(&first, Vec::new(), &crate::value::quantity::scale_expr),
            vec![Value::Null]
        );
        let last = FnCall {
            headless: false,
            name: "last".into(),
            args: Vec::new(),
        };
        assert_eq!(
            apply(&last, Vec::new(), &crate::value::quantity::scale_expr),
            vec![Value::Null]
        );
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
        assert!(
            crate::value::ambient::take_refusal()
                .unwrap()
                .contains("units")
        );
    }

    #[test]
    fn rounding_past_the_integers_stays_a_float() {
        let f = |name: &str, v: Value| {
            let call = FnCall {
                headless: false,
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
            calendar: Default::default(),
        };
        let b = Value::Instant {
            secs: 200,
            nanos: 0,
            offset_min: None,
            calendar: Default::default(),
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
            headless: false,
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
            headless: false,
            name: name.into(),
            args,
        }
    }
    fn flag(s: &str) -> Arg {
        Arg::Opt {
            name: s.into(),
            value: None,
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
        crate::syntax::lexer::lex(q)
            .and_then(|t| crate::syntax::parser::parse(&t))
            .is_ok()
    }

    #[test]
    fn atergo_compares_from_the_end() {
        let words = ["дом", "стол", "ёж", "еда", "нож"];
        let got = texts(apply(
            &call("sort", vec![flag("--atergo")]),
            strs(&words),
            &crate::value::quantity::scale_expr,
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
            &call("sort", vec![flag("--atergo")]),
            strs(&["стол", "ол", "л"]),
            &crate::value::quantity::scale_expr,
        ));
        assert_eq!(got, ["л", "ол", "стол"]);
    }

    #[test]
    #[cfg(feature = "colligo")]
    fn atergo_under_a_locale() {
        let words = ["ёж", "еж", "нож", "стол", "дом", "Öl"];
        let got = texts(apply(
            &call("sort", vec![word("ru-RU"), flag("--atergo")]),
            strs(&words),
            &crate::value::quantity::scale_expr,
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
        assert!(parses("/w @| sort(-a)"));
        assert!(parses("/w @| sort(--atergo)"));
        assert!(parses("/w @| sort(ru-RU; -a)"));
        assert!(parses("/w | sort(ru-RU, -a)"));
        assert!(parses("/w @| sort(ru-RU; -ad)"));
        // the word is a flag now (draft 203), and there is one locale
        assert!(!parses("/w @| sort(atergo)"));
        assert!(!parses("/w @| sort(atergo; ru-RU)"));
        assert!(!parses("/w @| sort(ru-RU; sv-SE)"));
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

/// Record-convention arguments with each field's provenance: every
/// field expression is evaluated under its own read frame.
/// A field's value is what its operand yields (as for
/// a computed push): one value itself, several as a list, none as
/// null — so `%(where = (//token->place::lemma @| unique))` holds
/// the list and an aggregate is not needed to keep it.
/// Build the table a literal describes (spec: The Table Kind, the
/// laws of the literal), `cell` evaluating one item.
/// Whether a value may stand in a table's cell: a scalar or null,
/// never a structure or an opaque value (spec: The Table).
pub(crate) fn flat_cell(v: &Value) -> bool {
    !matches!(
        v,
        Value::List(_) | Value::Record(_) | Value::Table(_) | Value::Foreign(_)
    )
}
