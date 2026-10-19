//! Part of the executor: see `exec/mod.rs`.

use crate::syntax::ast::ArithOp;
use crate::value::Value;

/// Combine two values arithmetically (spec: Value Expressions and
/// Arithmetic). Operands combine via their numeric readings; a
/// missing reading yields null, and null propagates. Integer
/// results that overflow promote to floats; `div` is always a
/// float; division by zero gives inf, -inf or nan as IEEE 754 does,
/// idiv and mod by zero nan.
/// Temporal arithmetic (spec: The Temporal Fragment). A TYPED
/// temporal operand (Instant or Duration) activates it — two plain
/// numbers or strings never reach here, so the numeric fragment's
/// rules stand — and then the partner coerces through its temporal
/// reading, exactly as comparisons do: "2024-02-15" + (30 | days)
/// is an instant. Instant − Instant = Duration; Instant ± Duration
/// = Instant; Duration ± Duration = Duration; Duration × n and
/// Duration ÷ n scale. Anything else involving a temporal value is
/// null (propagating).
pub(crate) fn temporal_arith(
    op: ArithOp,
    left: &Value,
    right: &Value,
    scale: UnitScale,
) -> Option<Value> {
    // Coerce a TEXT partner facing a typed temporal operand,
    // through whichever temporal-family reading the text has —
    // the two grammars are disjoint: instant text is date-shaped
    // (the written offset kept for the result's display), span
    // text is P-prefixed or unit-suffixed (builtin units first,
    // then the mounted unit table's time units, e.g. `1jaj`).
    // Numbers never lift in arithmetic (epoch point or span of
    // seconds? — ambiguous).
    let lift = |v: &Value| -> Option<Value> {
        match v {
            Value::Instant { .. } => Some(v.clone()),
            q if q.is_duration() => Some(q.clone()),
            Value::Str(s) => crate::temporal::parse_iso(s)
                .map(|(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                    calendar: Default::default(),
                })
                .or_else(|| {
                    // unit text in a time unit first (`2h` keeps its
                    // written unit), then ISO-8601 and the
                    // multi-component span text, in seconds
                    crate::value::quantity::parse_unit_text_with(s, scale)
                        .filter(|(_, base, ..)| base == "s")
                        .map(|(value, base, wv, wu)| Value::Quantity {
                            value,
                            base,
                            written: Some((wv, wu)),
                        })
                        .or_else(|| {
                            crate::temporal::parse_span(s)
                                .or_else(|| crate::temporal::span_from_units(s, scale))
                                .map(|(secs, nanos)| Value::seconds(secs, nanos))
                        })
                }),
            _ => None,
        }
    };
    let typed = |v: &Value| matches!(v, Value::Instant { .. }) || v.is_duration();
    // A time quantity × n / ÷ n keeps the number a NUMBER: handled
    // by the quantity arithmetic, never lifted as an epoch.
    if !(matches!(op, ArithOp::Mul) && (typed(left) || typed(right))
        || matches!(op, ArithOp::Div) && typed(left))
    {
        match (typed(left), typed(right)) {
            (true, false) => {
                if let Some(l) = lift(right) {
                    return temporal_arith(op, left, &l, scale);
                }
            }
            (false, true) => {
                if let Some(l) = lift(left) {
                    return temporal_arith(op, &l, right, scale);
                }
            }
            _ => {}
        }
    }
    use Value::Instant;
    let add = |s: i64, n: u32, ds: i64, dn: u32, sign: i64| -> Option<(i64, u32)> {
        let mut secs = s.checked_add(sign.checked_mul(ds)?)?;
        let total = n as i64 + sign * dn as i64;
        let mut nanos = total;
        if nanos < 0 {
            nanos += 1_000_000_000;
            secs = secs.checked_sub(1)?;
        } else if nanos >= 1_000_000_000 {
            nanos -= 1_000_000_000;
            secs = secs.checked_add(1)?;
        }
        Some((secs, nanos as u32))
    };
    match (left, right, op) {
        // instant − instant: a span, in seconds
        (
            Instant {
                secs: a, nanos: an, ..
            },
            Instant {
                secs: b, nanos: bn, ..
            },
            ArithOp::Sub,
        ) => {
            let Some((secs, nanos)) = add(*a, *an, *b, *bn, -1) else {
                return Some(Value::Null);
            };
            Some(Value::seconds(secs, nanos))
        }
        // instant ± span
        (
            Instant {
                secs,
                nanos,
                offset_min,
                ..
            },
            d,
            ArithOp::Add | ArithOp::Sub,
        ) if d.is_duration() => {
            let sign = if matches!(op, ArithOp::Add) { 1 } else { -1 };
            let Some((ds, dn)) = d.span_parts() else {
                return Some(Value::Null);
            };
            let Some((secs, nanos)) = add(*secs, *nanos, ds, dn, sign) else {
                return Some(Value::Null);
            };
            Some(Instant {
                secs,
                nanos,
                offset_min: *offset_min,
                calendar: Default::default(),
            })
        }
        (
            d,
            Instant {
                secs,
                nanos,
                offset_min,
                ..
            },
            ArithOp::Add,
        ) if d.is_duration() => {
            let Some((ds, dn)) = d.span_parts() else {
                return Some(Value::Null);
            };
            let Some((secs, nanos)) = add(*secs, *nanos, ds, dn, 1) else {
                return Some(Value::Null);
            };
            Some(Instant {
                secs,
                nanos,
                offset_min: *offset_min,
                calendar: Default::default(),
            })
        }
        // span ± span, the lifted text included: the quantity
        // arithmetic of dimension T
        (l, r, ArithOp::Add | ArithOp::Sub) if l.is_duration() && r.is_duration() => {
            quantital_arith(op, l, r, scale)
        }
        (Instant { .. }, _, _) | (_, Instant { .. }, _) => Some(Value::Null),
        _ => None,
    }
}

pub(crate) fn quantital_arith(
    op: ArithOp,
    left: &Value,
    right: &Value,
    scale: UnitScale,
) -> Option<Value> {
    use Value::Quantity;
    let typed = |v: &Value| matches!(v, Quantity { .. });
    if !typed(left) && !typed(right) {
        return None;
    }
    let rescale = |q: &Value, k: f64| -> Value {
        let Quantity {
            value,
            base,
            written,
        } = q
        else {
            unreachable!("rescale is called on quantities only");
        };
        Quantity {
            value: value * k,
            base: base.clone(),
            written: written.as_ref().map(|(v, u)| (v * k, u.clone())),
        }
    };
    // Scaling factors stay numbers, as with durations.
    match op {
        ArithOp::Mul => {
            return match (typed(left), typed(right)) {
                (true, false) => right
                    .numeric()
                    .map(|k| rescale(left, k))
                    .or(Some(Value::Null)),
                (false, true) => left
                    .numeric()
                    .map(|k| rescale(right, k))
                    .or(Some(Value::Null)),
                _ => Some(Value::Null), // Q × Q: undefined (v1)
            };
        }
        ArithOp::Div if typed(left) && !typed(right) => {
            return match right.numeric() {
                Some(k) if k != 0.0 => Some(rescale(left, 1.0 / k)),
                _ => Some(Value::Null),
            };
        }
        _ => {}
    }
    let lift = |v: &Value| -> Option<(f64, String, Option<String>)> {
        match v {
            Quantity {
                value,
                base,
                written,
            } => Some((
                *value,
                base.clone(),
                written.as_ref().map(|(_, u)| u.clone()),
            )),
            Value::Str(s) => crate::value::quantity::parse_unit_text_with(s, scale)
                .map(|(bv, b, _, wu)| (bv, b, Some(wu))),
            _ => None,
        }
    };
    match op {
        ArithOp::Add | ArithOp::Sub => {
            let (Some((a, ba, ua)), Some((b, bb, ub))) = (lift(left), lift(right)) else {
                return Some(Value::Null);
            };
            if ba != bb {
                return Some(Value::Null);
            }
            let v = if matches!(op, ArithOp::Add) {
                a + b
            } else {
                a - b
            };
            // displayed in the first operand's written unit (spec:
            // the display of a computed quantity), the base when it
            // has none
            let _ = ub;
            let written = ua.and_then(|u| {
                scale(&u).map(|(f, _)| (crate::value::quantity::unscale_by(v, f), u))
            });
            Some(Value::Quantity {
                value: v,
                base: ba,
                written,
            })
        }
        _ => Some(Value::Null),
    }
}

pub(crate) fn arith(op: ArithOp, left: &Value, right: &Value, scale: UnitScale) -> Value {
    if let Some(v) = temporal_arith(op, left, right, scale) {
        return v;
    }
    if let Some(v) = quantital_arith(op, left, right, scale) {
        return v;
    }
    let (Some(l), Some(r)) = (left.numeric_reading(), right.numeric_reading()) else {
        return Value::Null;
    };
    if let (Value::Int(a), Value::Int(b)) = (&l, &r) {
        let (a, b) = (*a, *b);
        let promoted = |f: fn(f64, f64) -> f64| Value::Float(f(a as f64, b as f64));
        return match op {
            ArithOp::Add => a
                .checked_add(b)
                .map(Value::Int)
                .unwrap_or_else(|| promoted(|x, y| x + y)),
            ArithOp::Sub => a
                .checked_sub(b)
                .map(Value::Int)
                .unwrap_or_else(|| promoted(|x, y| x - y)),
            ArithOp::Mul => a
                .checked_mul(b)
                .map(Value::Int)
                .unwrap_or_else(|| promoted(|x, y| x * y)),
            // A zero divisor gives what IEEE 754 gives: inf, -inf,
            // nan (spec: the float values nan, inf, -inf).
            ArithOp::Div => Value::Float(a as f64 / b as f64),
            ArithOp::IDiv => {
                if b == 0 {
                    Value::Float(f64::NAN)
                } else {
                    // The one overflow (i64::MIN idiv -1) promotes.
                    a.checked_div(b)
                        .map(Value::Int)
                        .unwrap_or_else(|| promoted(|x, y| (x / y).trunc()))
                }
            }
            ArithOp::Mod => {
                if b == 0 {
                    Value::Float(f64::NAN)
                } else {
                    // i64::MIN mod -1 is mathematically 0.
                    Value::Int(a.checked_rem(b).unwrap_or(0))
                }
            }
            // The matrix product is defined over a matrix and a
            // vector (`broadcast`), never between scalars.
            ArithOp::MatMul => Value::Null,
        };
    }
    let (Some(a), Some(b)) = (l.numeric(), r.numeric()) else {
        return Value::Null;
    };
    // Overflow to infinity and the indeterminate forms are the
    // floats IEEE 754 gives, inf, -inf and nan: they print as Python
    // prints them and read back (spec: the float values).
    match op {
        ArithOp::Add => Value::Float(a + b),
        ArithOp::Sub => Value::Float(a - b),
        ArithOp::Mul => Value::Float(a * b),
        ArithOp::IDiv | ArithOp::Mod if b == 0.0 => Value::Float(f64::NAN),
        ArithOp::Div => Value::Float(a / b),
        ArithOp::MatMul => Value::Null,
        ArithOp::IDiv => {
            let t = (a / b).trunc();
            if t >= i64::MIN as f64 && t <= i64::MAX as f64 {
                Value::Int(t as i64)
            } else {
                Value::Float(t)
            }
        }
        ArithOp::Mod => Value::Float(a % b),
    }
}

// ---------------------------------------------------------------------------
// Broadcasting and the matrix product (spec: Arithmetic over
// structures; The matrix).
// ---------------------------------------------------------------------------

/// Whether a value is one of the three structures an operator
/// broadcasts over.
fn is_structure(v: &Value) -> bool {
    matches!(v, Value::List(_) | Value::Record(_) | Value::Table(_))
}

/// Whether a cell's type admits an arithmetic operation under a
/// scalar: a number, a duration (under `*` and `div` by a number,
/// `+`/`-` with another span), an instant with a span, a quantity.
/// Text, booleans, periods and null do not; such a cell stays as it
/// is when a scalar broadcasts over its structure.
fn admits(cell: &Value) -> bool {
    matches!(
        cell,
        Value::Int(_) | Value::Float(_) | Value::Instant { .. } | Value::Quantity { .. }
    )
}

/// The kind words for the refusals.
fn kind(v: &Value) -> String {
    crate::stdlib::kind_of(v)
}

/// An arithmetic operator applied with broadcasting (spec:
/// Arithmetic over structures): cell by cell where an operand is a
/// list, a record or a table, the lower-dimensional operand
/// stretched along the higher one's last axis, the result in the
/// higher shape whichever side it stands on. A scalar applies to
/// every cell whose type admits the operation and leaves the other
/// cells as they are; two structures of one shape combine cell by
/// cell; a list against a table goes coordinate by column, against
/// a record coordinate by field; a mismatch is refused naming the
/// dimensions. A table's columns re-type after. `x` is the matrix
/// product.
pub(crate) fn broadcast(
    op: ArithOp,
    left: &Value,
    right: &Value,
    scale: UnitScale,
) -> Result<Value, String> {
    if op == ArithOp::MatMul {
        return matmul(left, right, scale);
    }
    let sym = op.symbol();
    match (left, right) {
        (l, r) if !is_structure(l) && !is_structure(r) => Ok(arith(op, l, r, scale)),
        // Two tables: the same header and dimensions, cell by cell.
        (Value::Table(t), Value::Table(u)) => {
            if t.header != u.header || t.rows.len() != u.rows.len() || t.width() != u.width() {
                return Err(format!(
                    "{sym}: the tables differ in shape, {} and {}{}",
                    t.kind()
                        .trim_start_matches("a frame ")
                        .trim_start_matches("an array "),
                    u.kind()
                        .trim_start_matches("a frame ")
                        .trim_start_matches("an array "),
                    if t.header != u.header {
                        " (and in their headers)"
                    } else {
                        ""
                    }
                ));
            }
            let rows = t
                .rows
                .iter()
                .zip(u.rows.iter())
                .map(|(a, b)| {
                    a.iter()
                        .zip(b.iter())
                        .map(|(x, y)| broadcast(op, x, y, scale))
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::table(crate::Table::new(t.header.clone(), rows)))
        }
        // A list against a table: coordinate by column.
        (Value::Table(t), Value::List(v)) | (Value::List(v), Value::Table(t)) => {
            let list_left = matches!(left, Value::List(_));
            if v.len() != t.width() {
                return Err(format!(
                    "{sym}: a list of {} against a table of {} columns: a list goes coordinate by column",
                    v.len(),
                    t.width()
                ));
            }
            let types = t.types();
            let names: Vec<String> = match &t.header {
                Some(h) => h.clone(),
                None => (1..=t.width()).map(|j| j.to_string()).collect(),
            };
            for (j, coord) in v.iter().enumerate() {
                let admitted = matches!(
                    types[j],
                    crate::ColumnType::Integer
                        | crate::ColumnType::Float
                        | crate::ColumnType::Duration
                        | crate::ColumnType::Instant
                        | crate::ColumnType::Quantity
                );
                if !matches!(coord, Value::Null) && !admitted {
                    return Err(format!(
                        "{sym}: column '{}' is {} and takes no {sym}; a null coordinate leaves it as it is",
                        names[j],
                        types[j].word()
                    ));
                }
            }
            let rows = t
                .rows
                .iter()
                .map(|r| {
                    r.iter()
                        .zip(v.iter())
                        .map(|(cell, coord)| {
                            if matches!(coord, Value::Null) || matches!(cell, Value::Null) {
                                Ok(cell.clone())
                            } else if list_left {
                                broadcast(op, coord, cell, scale)
                            } else {
                                broadcast(op, cell, coord, scale)
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::table(crate::Table::new(t.header.clone(), rows)))
        }
        (Value::Table(_), Value::Record(_)) | (Value::Record(_), Value::Table(_)) => Err(format!(
            "{sym}: a record against a table has no rule; a list goes coordinate by column, a record of one shape against a record"
        )),
        // A scalar over a table: every admitting cell, the others as
        // they are.
        (Value::Table(t), s) | (s, Value::Table(t)) => {
            let scalar_left = !matches!(left, Value::Table(_));
            let rows = t
                .rows
                .iter()
                .map(|r| {
                    r.iter()
                        .map(|cell| {
                            if !admits(cell) {
                                cell.clone()
                            } else if scalar_left {
                                arith(op, s, cell, scale)
                            } else {
                                arith(op, cell, s, scale)
                            }
                        })
                        .collect()
                })
                .collect();
            Ok(Value::table(crate::Table::new(t.header.clone(), rows)))
        }
        // Two lists: of one length, item by item; a list of lists
        // against the shorter list along the last axis.
        (Value::List(a), Value::List(b)) => {
            // The lower-dimensional list stretches along the last
            // axis of the higher: a list of lists against a flat
            // list takes it row by row.
            let flat = |l: &[Value]| l.iter().all(|x| !matches!(x, Value::List(_)));
            let nested =
                |l: &[Value]| !l.is_empty() && l.iter().all(|x| matches!(x, Value::List(_)));
            if nested(a) && flat(b) {
                return Ok(Value::list(
                    a.iter()
                        .map(|x| broadcast(op, x, right, scale))
                        .collect::<Result<Vec<_>, _>>()?,
                ));
            }
            if flat(a) && nested(b) {
                return Ok(Value::list(
                    b.iter()
                        .map(|y| broadcast(op, left, y, scale))
                        .collect::<Result<Vec<_>, _>>()?,
                ));
            }
            if a.len() == b.len() {
                return Ok(Value::list(
                    a.iter()
                        .zip(b.iter())
                        .map(|(x, y)| broadcast(op, x, y, scale))
                        .collect::<Result<Vec<_>, _>>()?,
                ));
            }
            Err(format!(
                "{sym}: lists of {} and {} elements differ in shape",
                a.len(),
                b.len()
            ))
        }
        // A list against a record: coordinate by field in order.
        (Value::Record(f), Value::List(v)) | (Value::List(v), Value::Record(f)) => {
            let list_left = matches!(left, Value::List(_));
            if v.len() != f.len() {
                return Err(format!(
                    "{sym}: a list of {} against a record of {} fields: a list goes coordinate by field",
                    v.len(),
                    f.len()
                ));
            }
            let fields = f
                .iter()
                .zip(v.iter())
                .map(|((k, cell), coord)| {
                    if matches!(coord, Value::Null) || matches!(cell, Value::Null) {
                        return Ok((k.clone(), cell.clone()));
                    }
                    if !is_structure(cell) && !admits(cell) {
                        return Err(format!(
                            "{sym}: field '{k}' is {} and takes no {sym}",
                            kind(cell)
                        ));
                    }
                    let v = if list_left {
                        broadcast(op, coord, cell, scale)?
                    } else {
                        broadcast(op, cell, coord, scale)?
                    };
                    Ok((k.clone(), v))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Record(fields))
        }
        // Two records: the same field names, field by field.
        (Value::Record(a), Value::Record(b)) => {
            let same = a.len() == b.len() && a.iter().all(|(k, _)| b.iter().any(|(n, _)| n == k));
            if !same {
                let names = |r: &Vec<(String, Value)>| {
                    r.iter()
                        .map(|(k, _)| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                return Err(format!(
                    "{sym}: the records differ in shape, fields {} and {}",
                    names(a),
                    names(b)
                ));
            }
            let fields = a
                .iter()
                .map(|(k, x)| {
                    let y = &b.iter().find(|(n, _)| n == k).expect("same names").1;
                    Ok::<_, String>((k.clone(), broadcast(op, x, y, scale)?))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Record(fields))
        }
        // A scalar over a list or a record: every admitting cell,
        // a nested structure recursively, the others as they are.
        (Value::List(a), s) | (s, Value::List(a)) => {
            let scalar_left = !matches!(left, Value::List(_));
            Ok(Value::list(
                a.iter()
                    .map(|cell| {
                        if is_structure(cell) {
                            if scalar_left {
                                broadcast(op, s, cell, scale)
                            } else {
                                broadcast(op, cell, s, scale)
                            }
                        } else if !admits(cell) {
                            Ok(cell.clone())
                        } else if scalar_left {
                            Ok(arith(op, s, cell, scale))
                        } else {
                            Ok(arith(op, cell, s, scale))
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        (Value::Record(f), s) | (s, Value::Record(f)) => {
            let scalar_left = !matches!(left, Value::Record(_));
            Ok(Value::Record(
                f.iter()
                    .map(|(k, cell)| {
                        let v = if is_structure(cell) {
                            if scalar_left {
                                broadcast(op, s, cell, scale)?
                            } else {
                                broadcast(op, cell, s, scale)?
                            }
                        } else if !admits(cell) {
                            cell.clone()
                        } else if scalar_left {
                            arith(op, s, cell, scale)
                        } else {
                            arith(op, cell, s, scale)
                        };
                        Ok::<_, String>((k.clone(), v))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        (l, r) => Ok(arith(op, l, r, scale)),
    }
}

/// A vector: a list of numbers, non-empty.
fn vector(v: &Value) -> Option<&[Value]> {
    match v {
        Value::List(l)
            if !l.is_empty()
                && l.iter()
                    .all(|x| matches!(x, Value::Int(_) | Value::Float(_))) =>
        {
            Some(l)
        }
        _ => None,
    }
}

/// A matrix: an array (a table without a header) whose columns are
/// all numeric, non-empty.
fn matrix(v: &Value) -> Option<&crate::Table> {
    match v {
        Value::Table(t)
            if t.header.is_none()
                && !t.rows.is_empty()
                && t.width() > 0
                && t.types().iter().all(|c| {
                    matches!(c, crate::ColumnType::Integer | crate::ColumnType::Float)
                })
                && t.rows.iter().flatten().all(|c| !matches!(c, Value::Null)) =>
        {
            Some(t)
        }
        _ => None,
    }
}

/// The dot product of two equal-length numeric rows.
fn dot(a: impl Iterator<Item = Value>, b: impl Iterator<Item = Value>, scale: UnitScale) -> Value {
    a.zip(b).fold(Value::Int(0), |acc, (x, y)| {
        arith(
            ArithOp::Add,
            &acc,
            &arith(ArithOp::Mul, &x, &y, scale),
            scale,
        )
    })
}

/// `x` — the matrix product (spec: The matrix): a matrix times a
/// vector is the vector of the row sums, a vector times a matrix
/// the vector of the column sums (the vector read as a row), a
/// matrix times a matrix a matrix, a vector times a vector of one
/// length their dot product; the inner dimensions must agree, and
/// anything else is refused by kind.
fn matmul(left: &Value, right: &Value, scale: UnitScale) -> Result<Value, String> {
    let column = |t: &crate::Table, j: usize| -> Vec<Value> {
        t.rows.iter().map(|r| r[j].clone()).collect()
    };
    match (matrix(left), vector(left), matrix(right), vector(right)) {
        (Some(m), _, _, Some(v)) => {
            if m.width() != v.len() {
                return Err(format!(
                    "x: a matrix of {}×{} against a vector of {}: the inner dimensions differ ({} and {})",
                    m.rows.len(),
                    m.width(),
                    v.len(),
                    m.width(),
                    v.len()
                ));
            }
            Ok(Value::list(
                m.rows
                    .iter()
                    .map(|r| dot(r.iter().cloned(), v.iter().cloned(), scale))
                    .collect(),
            ))
        }
        (_, Some(v), Some(m), _) => {
            if v.len() != m.rows.len() {
                return Err(format!(
                    "x: a vector of {} against a matrix of {}×{}: the inner dimensions differ ({} and {})",
                    v.len(),
                    m.rows.len(),
                    m.width(),
                    v.len(),
                    m.rows.len()
                ));
            }
            Ok(Value::list(
                (0..m.width())
                    .map(|j| dot(v.iter().cloned(), column(m, j).into_iter(), scale))
                    .collect(),
            ))
        }
        (Some(m), _, Some(n), _) => {
            if m.width() != n.rows.len() {
                return Err(format!(
                    "x: matrices of {}×{} and {}×{}: the inner dimensions differ ({} and {})",
                    m.rows.len(),
                    m.width(),
                    n.rows.len(),
                    n.width(),
                    m.width(),
                    n.rows.len()
                ));
            }
            let rows = m
                .rows
                .iter()
                .map(|r| {
                    (0..n.width())
                        .map(|j| dot(r.iter().cloned(), column(n, j).into_iter(), scale))
                        .collect()
                })
                .collect();
            Ok(Value::table(crate::Table::new(None, rows)))
        }
        (_, Some(v), _, Some(w)) => {
            if v.len() != w.len() {
                return Err(format!(
                    "x: vectors of {} and {}: the lengths differ",
                    v.len(),
                    w.len()
                ));
            }
            Ok(dot(v.iter().cloned(), w.iter().cloned(), scale))
        }
        _ => Err(format!(
            "x is the matrix product: it takes a matrix (an array of one numeric type) or a vector (a list of numbers) on each side, not {} and {}",
            kind(left),
            kind(right)
        )),
    }
}

/// A unit-expression resolver for the unital reading's criterion
/// text — the adapter's [`crate::graph::adapter::AstAdapter::unit_scale`],
/// threaded so custom units resolve where comparisons happen.
pub(crate) type UnitScale<'a> = &'a dyn Fn(&str) -> Option<(f64, String)>;
