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
        // the refusal principle (ruling 2026-11-06): an instant added to
        // an instant, or an instant under `*`, `/`, `div`, `mod`, is an
        // operation the arithmetic is not defined for — a refusal when
        // both operands have a reading; an operand without one keeps
        // the null that propagates
        (Instant { .. }, other, _) | (other, Instant { .. }, _) => {
            let reads = matches!(other, Instant { .. })
                || other.is_duration()
                || other.numeric_reading().is_some();
            if reads {
                crate::record_refusal(format!(
                    "{} is not defined for an instant and {}: an instant takes a span by '+'/'-' \
                     and gives a span by '-' from an instant",
                    match op {
                        ArithOp::Add => "the sum",
                        ArithOp::Sub => "the difference",
                        ArithOp::Mul => "the product",
                        ArithOp::Div | ArithOp::IDiv => "the quotient",
                        ArithOp::Mod => "the remainder",
                        ArithOp::MatMul => "the matrix product",
                    },
                    if matches!(other, Instant { .. }) {
                        "an instant"
                    } else if other.is_duration() {
                        "a span"
                    } else {
                        "a number"
                    }
                ));
            }
            Some(Value::Null)
        }
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
            // the written magnitude of a computed quantity rounds to 15
            // significant digits (ruling 2026-11-09)
            written: written.as_ref().map(|(v, u)| {
                (
                    crate::value::quantity::round_significant(v * k, 15),
                    u.clone(),
                )
            }),
        }
    };
    // Scaling factors stay numbers, as with durations.
    // Two quantities multiply or divide into one (ruling 2026-11-06,
    // computed by quanto): the dimension is the product or quotient
    // of theirs, the written unit the compound of the written ones
    // (`10_km div 2_h` is `5_km/h`, `2_m * 3_m` is `6_m^2`), and a
    // quotient of one dimension is a bare number (`10_km div 2_km`
    // is 5).
    let compound = |mul: bool| -> Option<Value> {
        let (
            Quantity {
                value: a,
                base: ba,
                written: wa,
            },
            Quantity {
                value: b,
                base: bb,
                written: wb,
            },
        ) = (left, right)
        else {
            return None;
        };
        if !mul && *b == 0.0 {
            return Some(Value::Null);
        }
        let value = if mul { a * b } else { a / b };
        let base = crate::value::quantity::compose_base(ba, bb, mul)?;
        if base == "1" || base.is_empty() {
            return Some(Value::Float(value));
        }
        let written = match (wa, wb) {
            // the written magnitude is the written magnitudes' product
            // or quotient, exact: the compound unit is theirs
            (Some((ma, ua)), Some((mb, ub))) => {
                crate::value::quantity::compose_written(ua, ub, mul).and_then(|u| {
                    if u.is_empty() {
                        None
                    } else {
                        Some((
                            crate::value::quantity::round_significant(
                                if mul { ma * mb } else { ma / mb },
                                15,
                            ),
                            u,
                        ))
                    }
                })
            }
            _ => None,
        };
        Some(Quantity {
            value,
            base,
            written,
        })
    };
    // A quantity of dimension one (a percent, an angle: base `1`)
    // computes with a bare number and with a dimensioned quantity as
    // the number it is (ruling 2026-11-09): `12_% * 50` is 6, `1 -
    // 12_%` is 0.88, `2_rad * 3` is 6, `12_% * 50_EUR` is `6_EUR`
    // (the partner's unit, its magnitude scaled); two of one
    // dimension-one unit still add to that unit below, and
    // `convert(%)` dresses a number again.
    let dimless = |v: &Value| matches!(v, Quantity { base, .. } if base == "1");
    let magnitude = |v: &Value| -> Option<f64> {
        match v {
            Quantity { value, .. } => Some(*value),
            other => other.numeric(),
        }
    };
    let plain = |a: f64, b: f64| -> Value {
        match op {
            ArithOp::Add => Value::Float(a + b),
            ArithOp::Sub => Value::Float(a - b),
            ArithOp::Mul => Value::Float(a * b),
            ArithOp::Div if b == 0.0 => Value::Null,
            ArithOp::Div => Value::Float(a / b),
            _ => Value::Null,
        }
    };
    let dimless_rule = || -> Option<Value> {
        match (dimless(left), dimless(right)) {
            // both of dimension one: a product or quotient is a bare
            // number; a sum keeps the unit (below)
            (true, true) => match op {
                ArithOp::Mul | ArithOp::Div => Some(plain(magnitude(left)?, magnitude(right)?)),
                _ => None,
            },
            // a bare number beside it: plain arithmetic in the base
            (true, false) if !typed(right) => Some(plain(magnitude(left)?, right.numeric()?)),
            (false, true) if !typed(left) => Some(plain(left.numeric()?, magnitude(right)?)),
            // a dimensioned partner: the number scales it in a
            // product, divides it in a quotient; a sum is the refusal
            // below, a quotient by it the compound
            (true, false) => match op {
                ArithOp::Mul => Some(rescale(right, magnitude(left)?)),
                _ => None,
            },
            (false, true) => match op {
                ArithOp::Mul => Some(rescale(left, magnitude(right)?)),
                ArithOp::Div => {
                    let b = magnitude(right)?;
                    Some(if b == 0.0 {
                        Value::Null
                    } else {
                        rescale(left, 1.0 / b)
                    })
                }
                _ => None,
            },
            _ => None,
        }
    };
    if let Some(v) = dimless_rule() {
        return Some(v);
    }
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
                _ => compound(true).or(Some(Value::Null)),
            };
        }
        ArithOp::Div if typed(left) && !typed(right) => {
            return match right.numeric() {
                Some(k) if k != 0.0 => Some(rescale(left, 1.0 / k)),
                _ => Some(Value::Null),
            };
        }
        ArithOp::Div if typed(left) && typed(right) => {
            return compound(false).or(Some(Value::Null));
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
            // the refusal principle (ruling 2026-11-06): two quantities of
            // unlike dimensions is an operation the arithmetic is not
            // defined for — a refusal, not a silence
            if ba != bb {
                crate::record_refusal(format!(
                    "{} of quantities of unlike dimensions ({ba} and {bb}): arithmetic is not defined \
                     for them; convert first",
                    if matches!(op, ArithOp::Add) {
                        "the sum"
                    } else {
                        "the difference"
                    }
                ));
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
/// Two indexed tables under an operator: the same index shape and
/// data header; the rows aligned on the index — the union of the
/// labels in order first met, a label on one side alone giving
/// null cells, a label on several rows pairing its n-th rows — the
/// data cells combined, the labels kept.
fn labeled_broadcast(
    op: ArithOp,
    t: &crate::Table,
    u: &crate::Table,
    scale: UnitScale,
) -> Result<Value, String> {
    let sym = op.symbol();
    let names =
        |x: &crate::Table| -> Vec<String> { (0..x.width()).map(|j| x.column_name(j)).collect() };
    let (tn, un) = (names(t), names(u));
    if t.index != u.index || tn[t.index..] != un[u.index..] {
        return Err(format!(
            "{sym}: the tables differ in shape, {} and {} (and in their headers: {} against {})",
            t.kind()
                .trim_start_matches("a frame ")
                .trim_start_matches("an array "),
            u.kind()
                .trim_start_matches("a frame ")
                .trim_start_matches("an array "),
            tn.join(", "),
            un.join(", ")
        ));
    }
    let k = t.index;
    let w = t.width();
    let label = |r: &Vec<Value>| -> Vec<Value> { r[..k].to_vec() };
    let occurrences = |seen: &mut Vec<(Vec<Value>, usize)>, l: &Vec<Value>| -> usize {
        match seen.iter_mut().find(|(x, _)| x == l) {
            Some((_, n)) => {
                *n += 1;
                *n
            }
            None => {
                seen.push((l.clone(), 1));
                1
            }
        }
    };
    let mut keys: Vec<(Vec<Value>, usize)> = Vec::new();
    let mut seen = Vec::new();
    let left: Vec<(Vec<Value>, usize, Vec<Value>)> = t
        .rows()
        .map(|r| {
            let r = r.cells();
            let l = label(&r);
            let n = occurrences(&mut seen, &l);
            keys.push((l.clone(), n));
            (l, n, r)
        })
        .collect();
    let mut seen = Vec::new();
    let right: Vec<(Vec<Value>, usize, Vec<Value>)> = u
        .rows()
        .map(|r| {
            let r = r.cells();
            let l = label(&r);
            let n = occurrences(&mut seen, &l);
            if !keys.iter().any(|(x, m)| *x == l && *m == n) {
                keys.push((l.clone(), n));
            }
            (l, n, r)
        })
        .collect();
    let rows = keys
        .iter()
        .map(|(l, n)| {
            let a = left
                .iter()
                .find(|(x, m, _)| x == l && m == n)
                .map(|(_, _, r)| r);
            let b = right
                .iter()
                .find(|(x, m, _)| x == l && m == n)
                .map(|(_, _, r)| r);
            let mut row = l.clone();
            for j in k..w {
                row.push(match (a, b) {
                    (Some(a), Some(b)) => broadcast(op, &a[j], &b[j], scale)?,
                    _ => Value::Null,
                });
            }
            Ok(row)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Value::table(t.rebuild(rows)?))
}

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
        // Two indexed tables: aligned by label, the index kept
        // (ruling 2026-11-05: a record read off an indexed table
        // meets another by label).
        (Value::Table(t), Value::Table(u)) if t.index > 0 && u.index > 0 => {
            labeled_broadcast(op, t, u, scale)
        }
        // Two tables: the same header and dimensions, cell by cell.
        (Value::Table(t), Value::Table(u)) => {
            if t.header != u.header || t.n_rows() != u.n_rows() || t.width() != u.width() {
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
                .rows()
                .zip(u.rows())
                .map(|(a, b)| {
                    a.iter()
                        .zip(b.iter())
                        .map(|(x, y)| broadcast(op, &x, &y, scale))
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::table(t.rebuild(rows)?))
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
                .rows()
                .map(|r| {
                    r.iter()
                        .zip(v.iter())
                        .map(|(cell, coord)| {
                            if matches!(coord, Value::Null) || matches!(cell, Value::Null) {
                                Ok(cell)
                            } else if list_left {
                                broadcast(op, coord, &cell, scale)
                            } else {
                                broadcast(op, &cell, coord, scale)
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::table(t.rebuild(rows)?))
        }
        (Value::Table(_), Value::Record(_)) | (Value::Record(_), Value::Table(_)) => Err(format!(
            "{sym}: a record against a table has no rule; a list goes coordinate by column, a record of one shape against a record"
        )),
        // A scalar over a table: every admitting cell, the others as
        // they are.
        (Value::Table(t), s) | (s, Value::Table(t)) => {
            let scalar_left = !matches!(left, Value::Table(_));
            let rows = t
                .rows()
                .map(|r| {
                    r.iter()
                        .map(|cell| {
                            if !admits(&cell) {
                                cell.clone()
                            } else if scalar_left {
                                arith(op, s, &cell, scale)
                            } else {
                                arith(op, &cell, s, scale)
                            }
                        })
                        .collect()
                })
                .collect();
            Ok(Value::table(t.rebuild(rows)?))
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
            Ok(Value::Record(fields.into()))
        }
        // Two records: the same field names, field by field.
        // Two records ALIGN BY KEY (spec: Structured Values, draft
        // 166): the union of the keys in first-met order, as values
        // (1 and "1" two keys), null where a side lacks one; the
        // keys kept typed as they are.
        (Value::Record(a), Value::Record(b)) => {
            let mut keys: Vec<Value> = a.key_values();
            for k in b.key_values() {
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
            let cell = |r: &crate::value::Record, k: &Value| -> Value {
                r.find_key(k).map(|i| r[i].1.clone()).unwrap_or(Value::Null)
            };
            let pairs = keys
                .iter()
                .map(|k| {
                    Ok::<_, String>((k.clone(), broadcast(op, &cell(a, k), &cell(b, k), scale)?))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Record(crate::value::Record::keyed(pairs)))
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
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
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
                && t.n_rows() != 0
                && t.width() > 0
                && t.types().iter().all(|c| {
                    matches!(c, crate::ColumnType::Integer | crate::ColumnType::Float)
                })
                && t.rows()
                    .flat_map(|r| r.iter())
                    .all(|c| !matches!(c, Value::Null)) =>
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
    let column = |t: &crate::Table, j: usize| -> Vec<Value> { t.column_cells(j) };
    match (matrix(left), vector(left), matrix(right), vector(right)) {
        (Some(m), _, _, Some(v)) => {
            if m.width() != v.len() {
                return Err(format!(
                    "x: a matrix of {}×{} against a vector of {}: the inner dimensions differ ({} and {})",
                    m.n_rows(),
                    m.width(),
                    v.len(),
                    m.width(),
                    v.len()
                ));
            }
            Ok(Value::list(
                m.rows()
                    .map(|r| dot(r.iter(), v.iter().cloned(), scale))
                    .collect(),
            ))
        }
        (_, Some(v), Some(m), _) => {
            if v.len() != m.n_rows() {
                return Err(format!(
                    "x: a vector of {} against a matrix of {}×{}: the inner dimensions differ ({} and {})",
                    v.len(),
                    m.n_rows(),
                    m.width(),
                    v.len(),
                    m.n_rows()
                ));
            }
            Ok(Value::list(
                (0..m.width())
                    .map(|j| dot(v.iter().cloned(), column(m, j).into_iter(), scale))
                    .collect(),
            ))
        }
        (Some(m), _, Some(n), _) => {
            if m.width() != n.n_rows() {
                return Err(format!(
                    "x: matrices of {}×{} and {}×{}: the inner dimensions differ ({} and {})",
                    m.n_rows(),
                    m.width(),
                    n.n_rows(),
                    n.width(),
                    m.width(),
                    n.n_rows()
                ));
            }
            let rows = m
                .rows()
                .map(|r| {
                    (0..n.width())
                        .map(|j| dot(r.iter(), column(n, j).into_iter(), scale))
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
