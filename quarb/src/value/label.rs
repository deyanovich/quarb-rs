//! Keys and labels as values (spec: Record, "Keys are equal as
//! values"; Indexed tables): when two record keys are one key and
//! two index labels one label.
//!
//! A key is equal to another as a value, and nothing coerces it:
//! `1` and `1.0` are one number, `1` and `"1"` two keys; one point
//! of the timeline is one instant however its offset was written,
//! one magnitude of one dimension one quantity whatever its unit
//! (`1_km`, `1000_m`). The value's derived equality compares what
//! was written as well, and so told those apart; the `=`
//! comparison coerces across kinds, and so would merge `1` with
//! `"1"`. This is the rule between them, with a hashed form so
//! that a lookup among n labels is one probe.

use super::Value;
use std::collections::HashMap;

/// What two equal keys share: a scalar's value without its
/// spelling. Every scalar a key can be has one; a structure has
/// none and is compared by its structure.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LabelKey {
    Null,
    Bool(bool),
    /// An integer, and a float that is that integer: one number.
    Int(i64),
    /// Any other float, by its bits (`-0.0` is `0`, every NaN one).
    Float(u64),
    Text(String),
    /// A point of the timeline; the written offset and the calendar
    /// are display.
    Instant(i64, u32),
    Period(i64, crate::period::Grain, crate::calendar::Calendar),
    /// A dimension's base and the magnitude in it; the written unit
    /// is display.
    Quantity(String, u64),
}

fn float_bits(f: f64) -> u64 {
    if f == 0.0 {
        0.0f64.to_bits()
    } else if f.is_nan() {
        f64::NAN.to_bits()
    } else {
        f.to_bits()
    }
}

impl Value {
    /// The key this value is as a record key or an index label;
    /// `None` for a structure (a list, a record, a table, a node),
    /// which is equal to another by its structure alone.
    pub fn label_key(&self) -> Option<LabelKey> {
        Some(match self {
            Value::Null => LabelKey::Null,
            Value::Bool(b) => LabelKey::Bool(*b),
            Value::Int(n) => LabelKey::Int(*n),
            // a whole float in the integers' range is that integer
            // (exact: every such float converts without rounding)
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => LabelKey::Int(*f as i64),
            Value::Float(f) => LabelKey::Float(float_bits(*f)),
            Value::Str(s) => LabelKey::Text(s.clone()),
            Value::Instant { secs, nanos, .. } => LabelKey::Instant(*secs, *nanos),
            Value::Period {
                start,
                grain,
                calendar,
            } => LabelKey::Period(*start, *grain, *calendar),
            Value::Quantity { value, base, .. } => {
                LabelKey::Quantity(base.clone(), float_bits(*value))
            }
            _ => return None,
        })
    }

    /// Whether `self` and `other` are one key (spec: Record): equal
    /// as values, nothing coerced.
    pub fn same_key(&self, other: &Value) -> bool {
        match (self.label_key(), other.label_key()) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self == other,
            _ => false,
        }
    }
}

/// Whether two labels, one value per index level, are one label.
pub fn same_label(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same_key(y))
}

/// Labels gathered into classes of equal labels, each class
/// numbered in the order its first label was met: the union of two
/// tables' labels, the rows one label stands on, the distinct keys
/// of a column. A label of scalars is found in one probe; one that
/// holds a structure takes the scan among its like.
#[derive(Default)]
pub struct LabelIndex {
    keyed: HashMap<Vec<LabelKey>, usize>,
    loose: Vec<(Vec<Value>, usize)>,
    len: usize,
}

impl LabelIndex {
    pub fn new() -> LabelIndex {
        LabelIndex::default()
    }

    /// How many classes there are (the tests' reading).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }

    fn keys(label: &[Value]) -> Option<Vec<LabelKey>> {
        label.iter().map(Value::label_key).collect()
    }

    /// The class of `label`, if a label equal to it was met (the
    /// tests' reading; the build asks through [`LabelIndex::intern`]).
    #[cfg(test)]
    pub fn find(&self, label: &[Value]) -> Option<usize> {
        match Self::keys(label) {
            Some(k) => self.keyed.get(&k).copied(),
            None => self
                .loose
                .iter()
                .find(|(l, _)| same_label(l, label))
                .map(|(_, c)| *c),
        }
    }

    /// The class of `label`, a new one where none is equal to it;
    /// the flag says the class is new.
    pub fn intern(&mut self, label: &[Value]) -> (usize, bool) {
        let next = self.len;
        let (class, new) = match Self::keys(label) {
            Some(k) => {
                let class = *self.keyed.entry(k).or_insert(next);
                (class, class == next)
            }
            None => match self.loose.iter().find(|(l, _)| same_label(l, label)) {
                Some((_, c)) => (*c, false),
                None => {
                    self.loose.push((label.to_vec(), next));
                    (next, true)
                }
            },
        };
        if new {
            self.len += 1;
        }
        (class, new)
    }
}

/// Labels counted as they are met on one side: a label standing on
/// several rows is its first, its second, … occurrence, and two
/// sides pair their n-th rows of one label. The classes are shared
/// between the sides (one [`LabelIndex`]); the counts are a side's.
#[derive(Default)]
pub struct Occurrences(Vec<usize>);

impl Occurrences {
    pub fn new() -> Occurrences {
        Occurrences::default()
    }

    /// The class of `label` and which occurrence of it this is on
    /// this side, counted from one.
    pub fn next(&mut self, classes: &mut LabelIndex, label: &[Value]) -> (usize, usize) {
        let (class, _) = classes.intern(label);
        if self.0.len() <= class {
            self.0.resize(class + 1, 0);
        }
        self.0[class] += 1;
        (class, self.0[class])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(secs: i64, offset_min: Option<i16>) -> Value {
        Value::Instant {
            secs,
            nanos: 0,
            offset_min,
            calendar: Default::default(),
        }
    }

    fn quantity(value: f64, written: (f64, &str)) -> Value {
        Value::Quantity {
            value,
            base: "m".into(),
            written: Some((written.0, written.1.into())),
        }
    }

    #[test]
    fn keys_are_equal_as_values_and_nothing_coerces_them() {
        // one number, however it is held
        assert!(Value::Int(1).same_key(&Value::Float(1.0)));
        assert!(Value::Float(0.0).same_key(&Value::Float(-0.0)));
        assert!(!Value::Int(1).same_key(&Value::Float(1.5)));
        // a number and its text are two keys; so are true and "true"
        assert!(!Value::Int(1).same_key(&Value::Str("1".into())));
        assert!(!Value::Bool(true).same_key(&Value::Str("true".into())));
        // exact past 2^53: two integers a double cannot tell apart
        assert!(!Value::Int(9007199254740993).same_key(&Value::Int(9007199254740992)));
        assert!(!Value::Int(9007199254740993).same_key(&Value::Float(9007199254740992.0)));
        // the written offset and the written unit are display
        assert!(instant(1_356_998_400, Some(60)).same_key(&instant(1_356_998_400, Some(0))));
        assert!(!instant(1_356_998_400, None).same_key(&instant(1_356_998_401, None)));
        assert!(quantity(1000.0, (1.0, "km")).same_key(&quantity(1000.0, (1000.0, "m"))));
        // a structure is equal by its structure
        let l = |v: Vec<Value>| Value::List(std::sync::Arc::new(v));
        assert!(l(vec![Value::Int(1)]).same_key(&l(vec![Value::Int(1)])));
        assert!(!l(vec![Value::Int(1)]).same_key(&Value::Int(1)));
    }

    #[test]
    fn the_index_numbers_classes_in_the_order_first_met() {
        let mut ix = LabelIndex::new();
        assert_eq!(ix.intern(&[Value::Int(2)]), (0, true));
        assert_eq!(ix.intern(&[Value::Str("2".into())]), (1, true));
        assert_eq!(ix.intern(&[Value::Float(2.0)]), (0, false));
        let l = Value::List(std::sync::Arc::new(vec![Value::Int(2)]));
        assert_eq!(ix.intern(std::slice::from_ref(&l)), (2, true));
        assert_eq!(ix.intern(std::slice::from_ref(&l)), (2, false));
        assert_eq!(ix.len(), 3);
        assert_eq!(ix.find(&[Value::Float(2.0)]), Some(0));
        assert_eq!(ix.find(&[Value::Int(3)]), None);
        // a composite label is one label level by level
        let mut ix = LabelIndex::new();
        ix.intern(&[Value::Int(1), Value::Str("a".into())]);
        assert_eq!(
            ix.find(&[Value::Float(1.0), Value::Str("a".into())]),
            Some(0)
        );
        assert_eq!(ix.find(&[Value::Int(1), Value::Str("b".into())]), None);
    }
}
