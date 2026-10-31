//! The shared plumbing of a command library that wraps a Python
//! package: a page of Python glue compiled once into a module, the
//! input slot filled from the synopsis, operands as positional
//! arguments and options as keyword arguments, the result mapped
//! back, a Python exception as a failed call, and the importability
//! check behind a library's run-time requirement.
//!
//! The glue module exports `_call(name, args, kwargs)`; what it does
//! with a list input (map it item by item, or take it as the array)
//! is the library's own business, declared in its synopsis.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyModule, PyTuple};
use quarb::Value;
use quarb::library::{InputSlot, LibCall, LibFailure, Synopsis};
use std::ffi::CString;
use std::sync::OnceLock;

/// A Python module from source, compiled on first use and kept for
/// the process.
pub struct PyGlue {
    source: &'static str,
    file: &'static str,
    name: &'static str,
    module: OnceLock<Py<PyModule>>,
}

impl PyGlue {
    pub const fn new(source: &'static str, file: &'static str, name: &'static str) -> Self {
        PyGlue {
            source,
            file,
            name,
            module: OnceLock::new(),
        }
    }

    pub(crate) fn module<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyModule>> {
        if let Some(m) = self.module.get() {
            return Ok(m.bind(py).clone());
        }
        // The foreign-value helpers first, importable by name from
        // any glue.
        crate::foreign::register(py)?;
        let m = PyModule::from_code(
            py,
            &CString::new(self.source).expect("glue has no NUL"),
            &CString::new(self.file).expect("file name has no NUL"),
            &CString::new(self.name).expect("module name has no NUL"),
        )?;
        let _ = self.module.set(m.clone().unbind());
        Ok(m)
    }

    /// Run the glue's `_call(name, args, kwargs)` with the call's
    /// values: the input in the synopsis' slot (the first positional
    /// unless `$_` placed it among the written operands), the
    /// operands in order, the options by their long name. A Python
    /// exception is a failed call carrying its text.
    pub fn call(&self, syn: &Synopsis, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        let (positional, keyword) = arguments(syn, call);
        Python::attach(|py| -> Result<Value, LibFailure> {
            let failed = |e: PyErr| LibFailure::Failed(format!("{e}"));
            let m = self.module(py).map_err(failed)?;
            let (args, kwargs) = python_arguments(py, &positional, &keyword).map_err(failed)?;
            let out = m
                .getattr("_call")
                .map_err(failed)?
                .call1((name, args, kwargs))
                .map_err(failed)?;
            crate::py_to_value(&out).map_err(failed)
        })
    }
}

/// The call's values in Python's shape: the input in the synopsis'
/// slot (the first positional unless `$_` placed it among the
/// written operands), the operands in order, the options by their
/// long name with hyphens as underscores, a switch as `True`.
pub(crate) fn arguments(syn: &Synopsis, call: LibCall) -> (Vec<Value>, Vec<(String, Value)>) {
    let (positional, keyword, _) = placed(syn, call);
    (positional, keyword)
}

/// The call's values in Python's order, and where among the
/// positional ones the input landed: at its slot, or first (the
/// receiver) when every operand of an `OperandOrSelf` synopsis is
/// written.
pub(crate) fn placed(
    syn: &Synopsis,
    call: LibCall,
) -> (Vec<Value>, Vec<(String, Value)>, Option<usize>) {
    let mut positional: Vec<Value> = call.operands;
    let mut keyword: Vec<(String, Value)> = Vec::new();
    let mut at = None;
    if let Some(input) = call.input {
        match syn.input {
            InputSlot::Operand(i) => {
                let i = i.min(positional.len());
                positional.insert(i, input);
                at = Some(i);
            }
            InputSlot::OperandOrSelf(i) => {
                let fixed = syn.operands.iter().filter(|o| o.name != "...").count();
                let i = if positional.len() >= fixed {
                    0
                } else {
                    i.min(positional.len())
                };
                positional.insert(i, input);
                at = Some(i);
            }
            InputSlot::Option(long) => keyword.push((long.to_string(), input)),
            // the context itself (the graph library): the engine
            // reads it, nothing crosses to the callee
            InputSlot::Context => {}
        }
    }
    for (long, value) in call.options {
        keyword.push((long.replace('-', "_"), value.unwrap_or(Value::Bool(true))));
    }
    (positional, keyword, at)
}

pub(crate) fn python_arguments<'py>(
    py: Python<'py>,
    positional: &[Value],
    keyword: &[(String, Value)],
) -> PyResult<(Bound<'py, PyTuple>, Bound<'py, PyDict>)> {
    let args = PyTuple::new(
        py,
        positional
            .iter()
            .map(|v| crate::value_to_py(py, v))
            .collect::<PyResult<Vec<_>>>()?,
    )?;
    let kwargs = PyDict::new(py);
    for (k, v) in keyword {
        kwargs.set_item(k, crate::value_to_py(py, v)?)?;
    }
    Ok((args, kwargs))
}

/// Whether every named module imports; the error names the first
/// that does not and the pip line that installs the packages.
pub fn importable(modules: &[&str], pip: &str) -> Result<(), String> {
    Python::attach(|py| {
        // The packages beside the wheel and in the active virtualenv
        // count, so the path is extended before the first import.
        crate::foreign::extend_sys_path(py).map_err(|e| e.to_string())?;
        for m in modules {
            py.import(*m).map_err(|e| {
                format!(
                    "the Python package {m} is not importable ({e}); install it: pip install {pip}"
                )
            })?;
        }
        Ok(())
    })
}
