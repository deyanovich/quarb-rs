//! Quarb values as Python objects and back — the one pyo3 mapping,
//! used in both directions: the Python bindings (`quarb-py`) hand
//! query results to Python with it, and a command library that
//! embeds Python (`quarb-lib-morph`, `quarb-lib-scipy`,
//! `quarb-lib-sklearn`) hands operands in and takes results out with
//! it; [`glue`] holds the call plumbing those libraries share.
//!
//! null ↔ None; booleans, integers, floats and strings to their
//! Python selves; a list ↔ `list`; a record ↔ `dict` (insertion
//! order kept); an instant ↔ a tz-aware `datetime`; a duration ↔
//! `timedelta`; a quantity ↔ [`Quantity`]; a period crosses as its
//! ISO text. Coming back, a tuple is a list too, a named tuple (and
//! scipy's result objects) a record, a numpy scalar its item, and
//! any other object a *library value* ([`PyForeign`]; spec: Command
//! Libraries, "A library value"), kept as it is for the next
//! command and handed back whole when it crosses into Python
//! again.

use pyo3::IntoPyObjectExt;
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use quarb::Value;

/// A dimensioned value: the magnitude in the engine's SI-base
/// expansion (`unit`, e.g. `m`, `kg*m^2/s^3`), with the authored
/// form kept for display (`written`, e.g. `42 km`) when the source
/// carried one.
#[pyclass(frozen, module = "quarb", from_py_object)]
#[derive(Clone, PartialEq)]
pub struct Quantity {
    #[pyo3(get)]
    pub magnitude: f64,
    #[pyo3(get)]
    pub unit: String,
    #[pyo3(get)]
    pub written: Option<String>,
}

#[pymethods]
impl Quantity {
    #[new]
    #[pyo3(signature = (magnitude, unit, written=None))]
    fn new(magnitude: f64, unit: String, written: Option<String>) -> Quantity {
        Quantity {
            magnitude,
            unit,
            written,
        }
    }

    fn __repr__(&self) -> String {
        match &self.written {
            Some(w) => format!(
                "Quantity(magnitude={}, unit={:?}, written={:?})",
                self.magnitude, self.unit, w
            ),
            None => format!(
                "Quantity(magnitude={}, unit={:?})",
                self.magnitude, self.unit
            ),
        }
    }

    fn __str__(&self) -> String {
        match &self.written {
            Some(w) => w.clone(),
            None => format!("{} {}", self.magnitude, self.unit),
        }
    }

    fn __eq__(&self, other: &Quantity) -> bool {
        self == other
    }
}

/// An engine [`Value`] as its natural Python object.
pub fn value_to_py(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_py_any(py)?,
        Value::Int(i) => i.into_py_any(py)?,
        Value::Float(f) => f.into_py_any(py)?,
        Value::Str(s) => s.into_py_any(py)?,
        // a projection coerces to text as its bare key
        Value::Projection { key, .. } => key.clone().into_py_any(py)?,
        // a quarq is its source text
        Value::Quarq(q) => q.source.clone().into_py_any(py)?,
        Value::List(items) => {
            let list = PyList::empty(py);
            for item in items.iter() {
                list.append(value_to_py(py, item)?)?;
            }
            list.unbind().into()
        }
        Value::Record(fields) => {
            let dict = PyDict::new(py);
            for (k, val) in fields {
                dict.set_item(k, value_to_py(py, val)?)?;
            }
            dict.unbind().into()
        }
        // A table crosses as its rows (records for a frame, lists
        // for an array): the construction then makes the frame or
        // the matrix of it.
        Value::Table(t) => value_to_py(py, &t.rows_value())?,
        Value::Instant {
            secs,
            nanos,
            offset_min,
            ..
        } => {
            // datetime is microsecond-precision; nanos round down.
            // No source offset means the UTC timeline — the result
            // is tz-aware either way.
            let datetime = py.import("datetime")?;
            let tz = match offset_min {
                Some(m) => {
                    let delta = datetime
                        .getattr("timedelta")?
                        .call1((0, i64::from(*m) * 60))?;
                    datetime.getattr("timezone")?.call1((delta,))?
                }
                None => datetime.getattr("timezone")?.getattr("utc")?,
            };
            datetime
                .getattr("datetime")?
                .call_method1("fromtimestamp", (*secs, &tz))?
                .call_method(
                    "replace",
                    (),
                    Some(&[("microsecond", nanos / 1000)].into_py_dict(py)?),
                )?
                .unbind()
        }
        // a span of time, the quantity of dimension T, is a timedelta
        Value::Quantity { base, value, .. } if base == "s" => {
            let (secs, nanos) = quarb::split_seconds_pub(*value);
            py.import("datetime")?
                .getattr("timedelta")?
                .call1((0, secs, nanos / 1000))?
                .unbind()
        }
        // Python has no value for a stretch of the calendar at a
        // grain: a period arrives as its ISO-8601 text at reduced
        // precision (`1899-10`, `1899-10-06T14:30`).
        Value::Period { .. } => v.to_string().into_py_any(py)?,
        Value::Quantity {
            value,
            base,
            written,
        } => Quantity {
            magnitude: *value,
            unit: base.clone(),
            written: written.as_ref().map(|(m, u)| format!("{m} {u}")),
        }
        .into_pyobject(py)?
        .into_any()
        .unbind(),
        // A node reference crosses as its locator text: no Python
        // form keeps nodeness.
        Value::Node(n) => n.locator.as_ref().into_py_any(py)?,
        // A library value of a Python library is its object, back as
        // it is; one of another library has no Python form.
        Value::Foreign(f) => match f.as_any().downcast_ref::<PyForeign>() {
            Some(p) => p.obj.clone_ref(py),
            None => {
                return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                    "{} is a library value of another library",
                    f.kind()
                )));
            }
        },
    })
}

/// A Python object as an engine [`Value`]: the mapping above read
/// the other way. A `datetime` crosses as an instant on its own
/// timeline (naive ones as UTC), a `timedelta` as a duration, a
/// tuple as a list, a named tuple (scipy's result objects among
/// them, their extra fields included) as a record, a `set` as a
/// list in iteration order, a numpy scalar as its item, and any
/// other object as a library value.
pub fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let py = obj.py();
    if obj.is_none() {
        return Ok(Value::Null);
    }
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    // A numpy scalar (float64, int64, bool_, datetime64) by its
    // item, a Python builtin; recognised by its type's module, so
    // numpy is never imported here.
    if !foreign::is_plain(obj) {
        let module = obj
            .get_type()
            .getattr("__module__")
            .ok()
            .and_then(|m| m.extract::<String>().ok())
            .unwrap_or_default();
        if module == "numpy" && !obj.hasattr("shape").unwrap_or(false)
            || module == "numpy"
                && obj
                    .getattr("shape")
                    .ok()
                    .is_some_and(|s| s.len().unwrap_or(1) == 0)
        {
            if let Ok(item) = obj.call_method0("item") {
                return py_to_value(&item);
            }
        }
    }
    // A named tuple, or scipy's result bunch (a tuple with extra
    // fields): a record of every field.
    if obj.hasattr("_fields").unwrap_or(false) {
        let mut fields = Vec::new();
        let mut names: Vec<String> = obj.getattr("_fields")?.extract()?;
        if let Ok(extra) = obj.getattr("_extra_fields") {
            names.extend(extra.extract::<Vec<String>>().unwrap_or_default());
        }
        for n in names {
            fields.push((n.clone(), py_to_value(&obj.getattr(n.as_str())?)?));
        }
        return Ok(Value::Record(fields.into()));
    }
    if obj.cast::<PyInt>().is_ok() {
        if let Ok(i) = obj.extract::<i64>() {
            return Ok(Value::Int(i));
        }
        return Ok(Value::Float(obj.extract::<f64>()?));
    }
    if obj.cast::<PyFloat>().is_ok() {
        return Ok(Value::Float(obj.extract::<f64>()?));
    }
    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(Value::Str(s.to_string_lossy().into_owned()));
    }
    if let Ok(q) = obj.extract::<Quantity>() {
        let written = q.written.as_ref().and_then(|w| {
            let (m, u) = w.split_once(' ')?;
            Some((m.parse::<f64>().ok()?, u.to_string()))
        });
        return Ok(Value::Quantity {
            value: q.magnitude,
            base: q.unit,
            written,
        });
    }
    if obj.cast::<PyList>().is_ok() || obj.cast::<PyTuple>().is_ok() {
        let mut items = Vec::new();
        for item in obj.try_iter()? {
            items.push(py_to_value(&item?)?);
        }
        return Ok(Value::list(items));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        let mut fields = Vec::new();
        for (k, v) in d.iter() {
            fields.push((k.str()?.to_string_lossy().into_owned(), py_to_value(&v)?));
        }
        return Ok(Value::Record(fields.into()));
    }
    let datetime = py.import("datetime")?;
    if obj.is_instance(&datetime.getattr("datetime")?)? {
        let ts: f64 = obj.call_method0("timestamp")?.extract()?;
        let secs = ts.floor() as i64;
        let nanos = ((ts - ts.floor()) * 1e9).round() as u32;
        let offset_min = match obj.call_method0("utcoffset")? {
            o if o.is_none() => None,
            o => Some((o.call_method0("total_seconds")?.extract::<f64>()? / 60.0) as i16),
        };
        return Ok(Value::Instant {
            secs,
            nanos,
            offset_min,
            calendar: Default::default(),
        });
    }
    if obj.is_instance(&datetime.getattr("timedelta")?)? {
        let total: f64 = obj.call_method0("total_seconds")?.extract()?;
        return Ok(Value::seconds(
            total.floor() as i64,
            ((total - total.floor()) * 1e9).round() as u32,
        ));
    }
    if foreign::is_plain(obj) {
        if obj.hasattr("__iter__")? && !obj.hasattr("read")? {
            // A set, a generator, a dict view: whatever builtin
            // iterates and is no mapping or text crosses as a list.
            let mut items = Vec::new();
            for item in obj.try_iter()? {
                items.push(py_to_value(&item?)?);
            }
            return Ok(Value::list(items));
        }
        return Ok(Value::Str(obj.str()?.to_string_lossy().into_owned()));
    }
    // A frame or a two-dimensional array comes back as a table, a
    // one-dimensional array or a positional Series as a list (spec:
    // The Table Kind): the engine's own kinds, not the library's.
    if let Some(v) = foreign::as_table(obj)? {
        return Ok(v);
    }
    // Anything else — a fitted model, a distribution — is a library
    // value, kept as it is.
    Ok(Value::Foreign(quarb::ForeignValue::new(PyForeign::new(
        obj.clone().unbind(),
    ))))
}

pub mod foreign;
pub mod glue;
pub mod reflect;
pub use foreign::PyForeign;
pub use glue::{PyGlue, importable};
pub use reflect::Reflective;
