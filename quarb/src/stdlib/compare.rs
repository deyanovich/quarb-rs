//! Part of the executor: see `exec/mod.rs`.

use crate::stdlib::UnitScale;
use crate::stdlib::text::regex_test;
use crate::syntax::ast::{CmpOp, Operand, PatSeg};
use crate::value::Value;
use std::cmp::Ordering;

/// A shaped literal in arithmetic position types itself (ruling
/// #12): instant text becomes an Instant, span text a Duration
/// (builtin units first, then the mounted unit table), unit text a
/// Quantity. Only an `Operand::Lit` retypes — a data read (`Rel`)
/// keeps its text, and comparisons never call this, so criteria
/// keep coercing at the use site. A bare number stays a number:
/// the no-guess and scaling-factor rules in `arith` still apply.
pub(crate) fn retype_shaped_lit(op: &Operand, v: Value, scale: UnitScale) -> Value {
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
            calendar: Default::default(),
        };
    }
    // unit text first, so a one-component span (`2h`) keeps its
    // written unit; then ISO-8601 and multi-component span text
    if let Some((value, base, wv, wu)) = crate::value::quantity::parse_unit_text_with(s, scale) {
        return Value::Quantity {
            value,
            base,
            written: Some((wv, wu)),
        };
    }
    if let Some((secs, nanos)) =
        crate::temporal::parse_span(s).or_else(|| crate::temporal::span_from_units(s, scale))
    {
        return Value::seconds(secs, nanos);
    }
    v
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
pub(crate) fn set_probe(ys: &std::sync::Arc<Vec<Value>>, needle: &str) -> Option<bool> {
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

pub(crate) fn compare(a: &Value, op: CmpOp, b: &Value, scale: UnitScale) -> bool {
    // A projection value compares as its bare key's text (spec:
    // Projection Value, draft 164).
    if let Value::Projection { key, .. } = a {
        return compare(&Value::Str(key.clone()), op, b, scale);
    }
    if let Value::Projection { key, .. } = b {
        return compare(a, op, &Value::Str(key.clone()), scale);
    }
    match op {
        CmpOp::Within => return within(a, b, scale),
        CmpOp::Encloses => return within(b, a, scale),
        _ => {}
    }
    match (a, b) {
        (Value::List(xs), Value::List(ys)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => {
            let same = xs.len() == ys.len()
                && xs.iter().zip(ys.iter()).all(|(x, y)| value_eq(x, y, scale));
            return if op == CmpOp::Eq { same } else { !same };
        }
        // Null beside a list: unequal, as beside anything.
        (Value::List(_), Value::Null) | (Value::Null, Value::List(_))
            if matches!(op, CmpOp::Eq | CmpOp::Ne) =>
        {
            return op == CmpOp::Ne;
        }
        // The set reading of `=` over a list (any item equal) is
        // retired (spec: Retired Forms): a list equals a list,
        // element by element, and membership is the containment
        // comparison. Refused with the pointer, as every retired
        // spelling is.
        (Value::List(_), _) | (_, Value::List(_)) if matches!(op, CmpOp::Eq | CmpOp::Ne) => {
            let spelled = if op == CmpOp::Eq { "=" } else { "!=" };
            crate::value::ambient::record_refusal(format!(
                "'{spelled}' between a list and a value is retired: membership is the \
                 containment comparison, 'VALUE <<= LIST' (negated '!(VALUE <<= LIST)')"
            ));
            return false;
        }
        // The ordering, pattern and substring comparisons over a
        // list operand read any item.
        (Value::List(xs), _) => {
            return xs.iter().any(|x| compare(x, op, b, scale));
        }
        (_, Value::List(ys)) => {
            return ys.iter().any(|y| compare(a, op, y, scale));
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
        CmpOp::Within | CmpOp::Encloses => unreachable!("handled above"),
    }
}

/// `a <<= b`, a within b (spec: Containment), by the container's
/// kind. A list holds a value equal to one of its items (a range is
/// the list it spreads to before it gets here; a large list of
/// strings probes a set built once per list, the list being shared,
/// so its address identifies it across reads). A period, or text at
/// reduced precision, holds an instant its span covers and a period
/// whose own span it covers, the element read through the calendar
/// coercion. A value that is no container contains itself: on any
/// other scalar the comparison is equality (`3 <<= 3`), so a
/// property that is a scalar for one value and a list for several
/// reads alike. Null holds nothing and lies in nothing.
pub(crate) fn within(a: &Value, b: &Value, scale: UnitScale) -> bool {
    if matches!(a, Value::Null) {
        return false;
    }
    match b {
        Value::Null => false,
        Value::List(ys) => {
            if let Value::Str(needle) = a
                && let Some(hit) = set_probe(ys, needle)
            {
                return hit;
            }
            ys.iter().any(|y| value_eq(a, y, scale))
        }
        Value::Period { .. } | Value::Instant { .. } | Value::Str(_) => {
            let container = b.calendar_reading();
            match container {
                Some(c) => match a.calendar_reading() {
                    Some(p) => c.start <= p.start && p.end() <= c.end(),
                    None => false,
                },
                // text that is no period: itself alone
                None => value_eq(a, b, scale),
            }
        }
        _ => value_eq(a, b, scale),
    }
}

/// Test one value against a pattern literal. Null
/// never matches (mirroring `*=`); everything else tests its
/// string rendering, as the substring operator always has.
pub(crate) fn pattern_test(segs: &[PatSeg], v: &Value) -> bool {
    if matches!(v, Value::Null) {
        return false;
    }
    glob_match(segs, &v.to_string())
}

/// Match `text` against the alternating literal/star segments.
/// Anchored at both ends; a star spans any run (including empty).
/// The parser enforces strict alternation, so stars never touch
/// and the backtracking below stays linear in practice.
pub(crate) fn glob_match(segs: &[PatSeg], text: &str) -> bool {
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

/// A library value in a comparison (spec: Command Libraries, "A
/// library value"): equal only to itself; against anything else it
/// has no reading, and the comparison is refused by kind.
fn foreign_in(a: &Value, b: &Value) -> Option<bool> {
    match (a, b) {
        (Value::Foreign(x), Value::Foreign(y)) => Some(x == y),
        (Value::Foreign(f), _) | (_, Value::Foreign(f)) => {
            crate::value::ambient::record_refusal(f.refusal("a comparison"));
            Some(false)
        }
        // A node reference equals a reference to the same node
        // (spec: The Capsa, "Node references") and nothing else:
        // its locator text is its print form, not its identity.
        (Value::Node(x), Value::Node(y)) => Some(x == y),
        (Value::Node(_), _) | (_, Value::Node(_)) => Some(false),
        _ => None,
    }
}

/// The instant a value stands at beside a period: its temporal
/// reading, else the first instant of its calendar reading (text at
/// reduced precision, `2025-11`).
fn first_instant(v: &Value) -> Option<(i64, u32)> {
    v.temporal_reading()
        .or_else(|| v.calendar_reading().map(|p| (p.start, 0)))
}

pub(crate) fn value_eq(a: &Value, b: &Value, scale: UnitScale) -> bool {
    if let Some(same) = foreign_in(a, b) {
        return same;
    }
    // A period beside anything reads through its temporal coercion,
    // its first instant (spec: Temporal Kinds): a month equals its
    // first day. Containment is the containment comparison.
    if matches!(a, Value::Period { .. }) || matches!(b, Value::Period { .. }) {
        return match (first_instant(a), first_instant(b)) {
            (Some(x), Some(y)) => x == y,
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
    // Dimension equality when either side is a quantity, the other
    // coercing via its unital reading (unit text like `5km`; a bare
    // number as the partner's base). Base mismatch: not equal.
    if matches!(a, Value::Quantity { .. }) || matches!(b, Value::Quantity { .. }) {
        return match crate::value::quantital_pair_with(a, b, scale) {
            Some((x, y)) => x == y,
            None => false,
        };
    }
    // Two texts that both coerce to quantities of one dimension
    // compare by magnitude (spec: Physical Quantities).
    if let Some((x, y)) = text_quantities(a, b, scale) {
        return x == y;
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

/// Two texts that both read as quantities of one dimension, as their
/// base magnitudes; `None` for any other pair (a bare number is not
/// unit text, so numeric texts keep the numeric coercion).
fn text_quantities(a: &Value, b: &Value, scale: UnitScale) -> Option<(f64, f64)> {
    let (Value::Str(x), Value::Str(y)) = (a, b) else {
        return None;
    };
    let (xv, xb, ..) = crate::value::quantity::parse_unit_text_with(x, scale)?;
    let (yv, yb, ..) = crate::value::quantity::parse_unit_text_with(y, scale)?;
    (xb == yb).then_some((xv, yv))
}

pub(crate) fn value_cmp(a: &Value, b: &Value, scale: UnitScale) -> Option<Ordering> {
    if let Some(same) = foreign_in(a, b) {
        return same.then_some(Ordering::Equal);
    }
    // A period sorts with its first instant (spec: Temporal Kinds).
    if matches!(a, Value::Period { .. }) || matches!(b, Value::Period { .. }) {
        return match (first_instant(a), first_instant(b)) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Instant { .. }) || matches!(b, Value::Instant { .. }) {
        return match (a.temporal_reading(), b.temporal_reading()) {
            (Some(x), Some(y)) => Some(x.cmp(&y)),
            _ => None,
        };
    }
    if matches!(a, Value::Quantity { .. }) || matches!(b, Value::Quantity { .. }) {
        return crate::value::quantital_pair_with(a, b, scale).and_then(|(x, y)| x.partial_cmp(&y));
    }
    if let Some((x, y)) = text_quantities(a, b, scale) {
        return x.partial_cmp(&y);
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
