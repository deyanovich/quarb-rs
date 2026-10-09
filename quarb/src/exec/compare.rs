//! Part of the executor: see `exec/mod.rs`.

use super::*;

/// A shaped literal in arithmetic position types itself (ruling
/// #12): instant text becomes an Instant, span text a Duration
/// (builtin units first, then the mounted unit table), unit text a
/// Quantity. Only an `Operand::Lit` retypes — a data read (`Rel`)
/// keeps its text, and comparisons never call this, so criteria
/// keep coercing at the use site. A bare number stays a number:
/// the no-guess and scaling-factor rules in `arith` still apply.
pub(super) fn retype_shaped_lit(op: &Operand, v: Value, scale: UnitScale) -> Value {
    if !matches!(op, Operand::Lit(_)) {
        return v;
    }
    let Value::Str(s) = &v else {
        return v;
    };
    if let Some((secs, nanos, offset_min)) = crate::temporal::parse_iso(s) {
        return Value::Instant {
            secs,
            nanos,
            offset_min,
        };
    }
    if let Some((secs, nanos)) =
        crate::temporal::parse_span(s).or_else(|| crate::temporal::span_from_units(s, scale))
    {
        return Value::Duration { secs, nanos };
    }
    if let Some((value, base, wv, wu)) = crate::quantity::parse_unit_text_with(s, scale) {
        return Value::Quantity {
            value,
            base,
            written: Some((wv, wu)),
        };
    }
    v
}

/// The durational reading, extended by the mounted unit table:
/// builtin span text first (`min`/`h`/`d` keep their engine
/// meaning), then a magnitude on a custom time unit (`5rep`).
pub(super) fn durational_with(v: &Value, scale: UnitScale) -> Option<(i64, u32)> {
    v.durational_reading().or_else(|| match v {
        Value::Str(s) => crate::temporal::span_from_units(s, scale),
        _ => None,
    })
}

/// Compare two values under `op`. A list operand reads as a set:
/// `=` asks whether any element equals the other side (membership),
/// `!=` whether none does (its complement), and the ordering,
/// pattern and substring operators whether any element satisfies
/// them; two lists are equal when they match element by element.
/// The set behind a large list of strings used as a membership
/// operand: built once per list and kept while the list lives
/// (`Arc` identity), so `[$_ = $.set]` over thousands of rows
/// probes rather than scans. None when the list is small or not all
/// strings (the scan handles it).
pub(super) fn set_probe(ys: &std::sync::Arc<Vec<Value>>, needle: &str) -> Option<bool> {
    use std::cell::RefCell;
    use std::collections::HashSet;
    thread_local! {
        static SET: RefCell<Option<(std::sync::Weak<Vec<Value>>, HashSet<String>)>> =
            const { RefCell::new(None) };
    }
    if ys.len() < 64 {
        return None;
    }
    SET.with(|cell| {
        let mut slot = cell.borrow_mut();
        let fresh = match slot.as_ref() {
            // A dead weak may share the address of a newer list:
            // only a live one that is this list counts.
            Some((w, _)) => w.upgrade().is_none_or(|a| !std::sync::Arc::ptr_eq(&a, ys)),
            None => true,
        };
        if fresh {
            let mut set = HashSet::with_capacity(ys.len());
            for y in ys.iter() {
                match y {
                    Value::Str(s) => {
                        set.insert(s.clone());
                    }
                    _ => return None,
                }
            }
            *slot = Some((std::sync::Arc::downgrade(ys), set));
        }
        slot.as_ref().map(|(_, set)| set.contains(needle))
    })
}

pub(super) fn compare(a: &Value, op: CmpOp, b: &Value, scale: UnitScale) -> bool {
    match (a, b) {
        (Value::List(xs), Value::List(ys)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => {
            let same = xs.len() == ys.len()
                && xs.iter().zip(ys.iter()).all(|(x, y)| value_eq(x, y, scale));
            return if op == CmpOp::Eq { same } else { !same };
        }
        (Value::List(xs), _) => {
            return match op {
                CmpOp::Ne => !xs.iter().any(|x| value_eq(x, b, scale)),
                _ => xs.iter().any(|x| compare(x, op, b, scale)),
            };
        }
        (_, Value::List(ys)) => {
            // Membership in a large list of strings probes a set
            // built once per list (the list is shared, so its
            // address identifies it across reads).
            if matches!(op, CmpOp::Eq | CmpOp::Ne)
                && let Value::Str(needle) = a
                && let Some(hit) = set_probe(ys, needle)
            {
                return if op == CmpOp::Eq { hit } else { !hit };
            }
            return match op {
                CmpOp::Ne => !ys.iter().any(|y| value_eq(a, y, scale)),
                _ => ys.iter().any(|y| compare(a, op, y, scale)),
            };
        }
        _ => {}
    }
    match op {
        CmpOp::Eq => value_eq(a, b, scale),
        CmpOp::Ne => !value_eq(a, b, scale),
        CmpOp::Lt => value_cmp(a, b, scale) == Some(Ordering::Less),
        CmpOp::Le => matches!(
            value_cmp(a, b, scale),
            Some(Ordering::Less | Ordering::Equal)
        ),
        CmpOp::Gt => value_cmp(a, b, scale) == Some(Ordering::Greater),
        CmpOp::Ge => matches!(
            value_cmp(a, b, scale),
            Some(Ordering::Greater | Ordering::Equal)
        ),
        CmpOp::Match => regex_test(a, b, true),
        CmpOp::NotMatch => regex_test(a, b, false),
        // A null operand doesn't participate: null-propagate like
        // every other comparison rather than stringifying to "" —
        // otherwise a null right-hand side ("" is a substring of
        // everything) would vacuously match every value. An actual
        // empty string still matches, as it should.
        CmpOp::Contains => match (a, b) {
            (Value::Null, _) | (_, Value::Null) => false,
            _ => a.to_string().contains(&b.to_string()),
        },
    }
}

/// Test one value against a pattern literal. Null
/// never matches (mirroring `*=`); everything else tests its
/// string rendering, as the substring operator always has.
pub(super) fn pattern_test(segs: &[PatSeg], v: &Value) -> bool {
    if matches!(v, Value::Null) {
        return false;
    }
    glob_match(segs, &v.to_string())
}

/// Match `text` against the alternating literal/star segments.
/// Anchored at both ends; a star spans any run (including empty).
/// The parser enforces strict alternation, so stars never touch
/// and the backtracking below stays linear in practice.
pub(super) fn glob_match(segs: &[PatSeg], text: &str) -> bool {
    match segs.split_first() {
        None => text.is_empty(),
        Some((PatSeg::Lit(l), rest)) => text
            .strip_prefix(l.as_str())
            .is_some_and(|t| glob_match(rest, t)),
        Some((PatSeg::Star, rest)) => {
            let Some((PatSeg::Lit(l), after)) = rest.split_first() else {
                // A trailing star spans the remainder.
                return rest.is_empty();
            };
            // The star yields to each occurrence of the next
            // literal in turn.
            let mut from = 0;
            while let Some(hit) = text[from..].find(l.as_str()) {
                let at = from + hit;
                if glob_match(after, &text[at + l.len()..]) {
                    return true;
                }
                // Past this hit: the literal's length, or one
                // character when the literal is empty (a byte would
                // split a multi-byte character).
                from = at
                    + if l.is_empty() {
                        text[at..].chars().next().map_or(1, char::len_utf8)
                    } else {
                        l.len()
                    };
                if from > text.len() {
                    break;
                }
            }
            false
        }
    }
}

pub(super) fn value_eq(a: &Value, b: &Value, scale: UnitScale) -> bool {
    // A period beside anything: both through the calendar reading,
    // as intervals — nested periods are equal, so equality is
    // containment. This precedes the temporal rule where the other
    // side is an instant.
    if matches!(a, Value::Period { .. }) || matches!(b, Value::Period { .. }) {
        return match (a.calendar_reading(), b.calendar_reading()) {
            (Some(x), Some(y)) => x.interval_cmp(&y) == Ordering::Equal,
            _ => false,
        };
    }
    // Timeline equality when either side is an instant, the other
    // coercing via its temporal reading (epoch ints, ISO text).
    if matches!(a, Value::Instant { .. }) || matches!(b, Value::Instant { .. }) {
        return match (a.temporal_reading(), b.temporal_reading()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        };
    }
    // Span equality when either side is a duration, the other
    // coercing via its durational reading (numbers as seconds,
    // span text like `30d` or `P5DT3H`) — extended by the mounted
    // unit table, so criteria may speak the document's own time
    // units (`5rep`), exactly as quantity criteria do.
    if matches!(a, Value::Duration { .. }) || matches!(b, Value::Duration { .. }) {
        return match (durational_with(a, scale), durational_with(b, scale)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        };
    }
    // Dimension equality when either side is a quantity, the other
    // coercing via its unital reading (unit text like `5km`; a bare
    // number as the partner's base). Base mismatch: not equal.
    if matches!(a, Value::Quantity { .. }) || matches!(b, Value::Quantity { .. }) {
        return match crate::value::quantital_pair_with(a, b, scale) {
            Some((x, y)) => x == y,
            None => false,
        };
    }
    // Two integer readings compare exactly (a 64-bit id is not a
    // double); any other numeric pair by magnitude.
    match (a.numeric_reading(), b.numeric_reading()) {
        (Some(Value::Int(x)), Some(Value::Int(y))) => return x == y,
        (Some(x), Some(y)) => return x.numeric() == y.numeric(),
        _ => {}
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

pub(super) fn value_cmp(a: &Value, b: &Value, scale: UnitScale) -> Option<Ordering> {
    if matches!(a, Value::Period { .. }) || matches!(b, Value::Period { .. }) {
        return match (a.calendar_reading(), b.calendar_reading()) {
            (Some(x), Some(y)) => Some(x.interval_cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Instant { .. }) || matches!(b, Value::Instant { .. }) {
        return match (a.temporal_reading(), b.temporal_reading()) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Duration { .. }) || matches!(b, Value::Duration { .. }) {
        return match (durational_with(a, scale), durational_with(b, scale)) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Quantity { .. }) || matches!(b, Value::Quantity { .. }) {
        return crate::value::quantital_pair_with(a, b, scale).and_then(|(x, y)| x.partial_cmp(&y));
    }
    match (a.numeric_reading(), b.numeric_reading()) {
        (Some(Value::Int(x)), Some(Value::Int(y))) => return Some(x.cmp(&y)),
        (Some(x), Some(y)) => return x.numeric()?.partial_cmp(&y.numeric()?),
        _ => {}
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ => None,
    }
}
