//! Part of the executor: see `exec/mod.rs`.

use super::*;

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
pub(super) fn temporal_arith(
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
            Value::Instant { .. } | Value::Duration { .. } => Some(v.clone()),
            Value::Str(s) => crate::temporal::parse_iso(s)
                .map(|(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                })
                .or_else(|| {
                    crate::temporal::parse_span(s)
                        .or_else(|| crate::temporal::span_from_units(s, scale))
                        .map(|(secs, nanos)| Value::Duration { secs, nanos })
                }),
            _ => None,
        }
    };
    let typed = |v: &Value| matches!(v, Value::Instant { .. } | Value::Duration { .. });
    // Duration × n / ÷ n keep the number a NUMBER — handle before
    // lifting so a scaling factor is never read as an epoch.
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
    use Value::{Duration, Instant};
    // Checked throughout: `parse_span` admits spans up to i64::MAX
    // seconds, so an extreme instant/duration text must null-
    // propagate (None here → the caller returns null), never panic
    // under debug assertions or wrap silently in release —
    // arithmetic never aborts a query.
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
            Some(Duration { secs, nanos })
        }
        (
            Instant {
                secs,
                nanos,
                offset_min,
            },
            Duration { secs: d, nanos: dn },
            ArithOp::Add | ArithOp::Sub,
        ) => {
            let sign = if matches!(op, ArithOp::Add) { 1 } else { -1 };
            let Some((secs, nanos)) = add(*secs, *nanos, *d, *dn, sign) else {
                return Some(Value::Null);
            };
            Some(Instant {
                secs,
                nanos,
                offset_min: *offset_min,
            })
        }
        (
            Duration { secs: d, nanos: dn },
            Instant {
                secs,
                nanos,
                offset_min,
            },
            ArithOp::Add,
        ) => {
            let Some((secs, nanos)) = add(*secs, *nanos, *d, *dn, 1) else {
                return Some(Value::Null);
            };
            Some(Instant {
                secs,
                nanos,
                offset_min: *offset_min,
            })
        }
        (
            Duration { secs: a, nanos: an },
            Duration { secs: b, nanos: bn },
            ArithOp::Add | ArithOp::Sub,
        ) => {
            let sign = if matches!(op, ArithOp::Add) { 1 } else { -1 };
            let Some((secs, nanos)) = add(*a, *an, *b, *bn, sign) else {
                return Some(Value::Null);
            };
            Some(Duration { secs, nanos })
        }
        // A Quantity partner is dimensional nonsense here, not a
        // scaling factor: its `numeric()` is the base magnitude,
        // which must not leak in as a bare number. Let it fall to
        // the temporal catch-all (null), mirroring Q × Q.
        (Duration { secs, nanos }, n, ArithOp::Mul)
        | (n, Duration { secs, nanos }, ArithOp::Mul)
            if !matches!(n, Value::Quantity { .. }) =>
        {
            let k = n.numeric()?;
            let total = (*secs as f64 + *nanos as f64 / 1e9) * k;
            Some(Duration {
                secs: total.floor() as i64,
                nanos: ((total - total.floor()) * 1e9) as u32,
            })
        }
        (Duration { secs, nanos }, n, ArithOp::Div) if !matches!(n, Value::Quantity { .. }) => {
            let k = n.numeric()?;
            if k == 0.0 {
                return Some(Value::Null);
            }
            let total = (*secs as f64 + *nanos as f64 / 1e9) / k;
            Some(Duration {
                secs: total.floor() as i64,
                nanos: ((total - total.floor()) * 1e9) as u32,
            })
        }
        (Instant { .. } | Duration { .. }, _, _) | (_, Instant { .. } | Duration { .. }, _) => {
            Some(Value::Null)
        }
        _ => None,
    }
}

/// Quantital arithmetic (spec: The Quantital Fragment). A TYPED
/// quantity activates it: Q ± Q on the same base (the result a
/// base-displayed quantity); Q × n and Q ÷ n scale — the written
/// form scales along, keeping the display unit; a TEXT partner
/// lifts through the unital reading. Numbers never lift into ±
/// (the explicit road is `| convert(unit)`); a base mismatch or
/// any other combination is null, propagating.
pub(super) fn quantital_arith(
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
    let lift = |v: &Value| -> Option<(f64, String)> {
        match v {
            Quantity { value, base, .. } => Some((*value, base.clone())),
            Value::Str(s) => {
                crate::quantity::parse_unit_text_with(s, scale).map(|(bv, b, ..)| (bv, b))
            }
            _ => None,
        }
    };
    match op {
        ArithOp::Add | ArithOp::Sub => {
            let (Some((a, ba)), Some((b, bb))) = (lift(left), lift(right)) else {
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
            Some(Value::Quantity {
                value: v,
                base: ba,
                written: None,
            })
        }
        _ => Some(Value::Null),
    }
}

pub(super) fn arith(op: ArithOp, left: &Value, right: &Value, scale: UnitScale) -> Value {
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
