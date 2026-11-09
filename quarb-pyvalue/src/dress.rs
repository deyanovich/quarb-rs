//! The unit dressing of a library call (the user's rule, 2026-11-09):
//! a sample of quantities of one dimension — the stream a `sp:`,
//! `np:`, `pd:` command receives — computes on the magnitudes IN THE
//! WRITTEN UNIT (the first value's), and the answer wears the unit
//! again: the moments that carry it (mean, min, max, std, median, the
//! quantiles, a sum) in the unit, a variance in the unit squared, the
//! counts and the shape statistics (skewness, kurtosis, a test
//! statistic, a p-value) bare — as the engine's own reducers answer
//! (`mean` → `4201.75_g`). Two dimensions mixed, or quantities beside
//! bare numbers, refuse by name.

use quarb::Value;
use quarb::library::{LibCall, LibFailure};

/// What a sample's quantities wore: the written unit, its factor to
/// the base, and the base — enough to dress an answer again.
#[derive(Debug, Clone)]
pub struct Dress {
    unit: String,
    factor: f64,
    base: String,
}

/// How a field of the answer wears the unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wear {
    /// The unit itself: a moment of the sample's dimension.
    Same,
    /// The unit squared: a variance.
    Squared,
    /// A bare number: a count, a shape statistic, a probability.
    Bare,
}

/// The field names of the answers and what they wear; a scalar
/// answer wears what its command's name says.
fn wear(name: &str) -> Wear {
    match name.trim_start_matches(|c: char| !c.is_alphabetic()) {
        "mean"
        | "min"
        | "max"
        | "median"
        | "mode"
        | "std"
        | "sem"
        | "sum"
        | "range"
        | "iqr"
        | "mad"
        | "trim_mean"
        | "gmean"
        | "hmean"
        | "pmean"
        | "percentile"
        | "quantile"
        | "nanmean"
        | "nanmedian"
        | "nanstd"
        | "nanmin"
        | "nanmax"
        | "nansum"
        | "q1"
        | "q3"
        | "lower"
        | "upper"
        | "minmax"
        | "median_abs_deviation"
        | "average"
        | "amin"
        | "amax"
        | "ptp"
        | "first"
        | "last"
        | "low"
        | "high"
        | "center"
        | "scale"
        | "loc" => Wear::Same,
        "variance" | "var" | "nanvar" | "cov" => Wear::Squared,
        _ => Wear::Bare,
    }
}

/// Take a one-dimension quantity sample out of the call's input,
/// leaving the magnitudes in the first value's written unit; `None`
/// when the input holds no quantity. A mixture refuses by name.
pub fn lift(name: &str, call: &mut LibCall) -> Result<Option<Dress>, LibFailure> {
    let Some(input) = call.input.as_mut() else {
        return Ok(None);
    };
    let items: Vec<&Value> = match input {
        Value::List(items) => items.iter().collect(),
        Value::Table(t) if t.width() == t.index + 1 => {
            // a one-column table: its column is the sample
            let j = t.index;
            return lift_column(name, input, j);
        }
        _ => return Ok(None),
    };
    let Some(dress) = dress_of(name, &items)? else {
        return Ok(None);
    };
    let Value::List(items) = input else {
        unreachable!()
    };
    let lifted: Vec<Value> = items.iter().map(|v| dress.magnitude(v)).collect();
    *input = Value::list(lifted);
    Ok(Some(dress))
}

fn lift_column(name: &str, input: &mut Value, j: usize) -> Result<Option<Dress>, LibFailure> {
    let Value::Table(t) = &*input else {
        return Ok(None);
    };
    let cells: Vec<Value> = (0..t.n_rows()).map(|i| t.cell(i, j)).collect();
    let refs: Vec<&Value> = cells.iter().collect();
    let Some(dress) = dress_of(name, &refs)? else {
        return Ok(None);
    };
    // the sample as a list: the library reads a list of numbers
    *input = Value::list(cells.iter().map(|v| dress.magnitude(v)).collect());
    Ok(Some(dress))
}

/// The dress of a sample: `None` when no quantity stands in it; the
/// first quantity's written unit (its base when it wears none).
fn dress_of(name: &str, items: &[&Value]) -> Result<Option<Dress>, LibFailure> {
    let mut dress: Option<Dress> = None;
    let mut numbers = false;
    for v in items {
        match v {
            Value::Quantity {
                value,
                base,
                written,
            } => match &dress {
                None => {
                    let (unit, factor) = match written {
                        Some((mag, u)) if *mag != 0.0 => (u.clone(), value / mag),
                        _ => (base.clone(), 1.0),
                    };
                    dress = Some(Dress {
                        unit,
                        factor,
                        base: base.clone(),
                    });
                }
                Some(d) if d.base != *base => {
                    return Err(LibFailure::Failed(format!(
                        "{name}: quantities of unlike dimensions ({} and {base}) in one sample; convert first",
                        d.base
                    )));
                }
                Some(_) => {}
            },
            Value::Int(_) | Value::Float(_) => numbers = true,
            _ => {}
        }
    }
    match dress {
        Some(d) if numbers => Err(LibFailure::Failed(format!(
            "{name}: quantities in {} beside bare numbers in one sample; convert first",
            d.unit
        ))),
        other => Ok(other),
    }
}

impl Dress {
    /// A sample value as the library should see it: the magnitude in
    /// the written unit; anything else as it is.
    fn magnitude(&self, v: &Value) -> Value {
        match v {
            Value::Quantity { value, .. } => {
                Value::Float(quarb::quantity::unscale_by(*value, self.factor))
            }
            other => other.clone(),
        }
    }

    /// A number in the written unit as the quantity it is — a
    /// computed one, its written magnitude rounded to 15 significant
    /// digits as every computed quantity's is.
    fn wear_same(&self, x: f64) -> Value {
        let x = quarb::quantity::round_significant(x, 15);
        Value::Quantity {
            value: quarb::quantity::scale_by(x, self.factor),
            base: self.base.clone(),
            written: Some((x, self.unit.clone())),
        }
    }

    /// A number in the written unit squared (a variance).
    fn wear_squared(&self, x: f64) -> Value {
        let Some(unit) = quarb::quantity::compose_written(&self.unit, &self.unit, true) else {
            return Value::Float(x);
        };
        let Some((factor, base)) = quarb::quantity::scale_expr(&unit) else {
            return Value::Float(x);
        };
        let x = quarb::quantity::round_significant(x, 15);
        Value::Quantity {
            value: quarb::quantity::scale_by(x, factor),
            base,
            written: Some((x, unit)),
        }
    }

    fn wear_as(&self, w: Wear, v: Value) -> Value {
        match (w, &v) {
            (Wear::Bare, _) => v,
            (Wear::Same, Value::Int(n)) => self.wear_same(*n as f64),
            (Wear::Same, Value::Float(x)) => self.wear_same(*x),
            (Wear::Squared, Value::Int(n)) => self.wear_squared(*n as f64),
            (Wear::Squared, Value::Float(x)) => self.wear_squared(*x),
            (w, Value::List(items)) => {
                Value::list(items.iter().map(|i| self.wear_as(w, i.clone())).collect())
            }
            _ => v,
        }
    }

    /// The answer dressed: a scalar as its command's name says, a
    /// record field by field by its name, a list item by item.
    pub fn apply(&self, name: &str, value: Value) -> Value {
        match value {
            Value::Record(fields) => Value::Record(
                fields
                    .iter()
                    .map(|(k, v)| (k.clone(), self.wear_as(wear(k), v.clone())))
                    .collect(),
            ),
            other => self.wear_as(wear(name), other),
        }
    }
}
