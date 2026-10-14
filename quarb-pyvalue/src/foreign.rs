//! A Python object as a library value (spec: Command Libraries, "A
//! library value"): what a Python-wrapping library keeps in its own
//! form between two commands — a frame, an array, any object that is
//! not plain data. The engine sees its kind, its text and its two
//! exports; the libraries take the object back as it is.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use quarb::{Foreign, Value};
use std::ffi::CString;
use std::sync::OnceLock;

/// The Python side of the two readings and the kind. Numpy and
/// pandas are recognised by the module their types come from, so
/// neither is imported for an object that is not theirs.
const GLUE: &str = r#"
import math

def _mod(o):
    return type(o).__module__.split(".")[0]

def _plain(x):
    """A value out of numpy/pandas as plain Python: scalars by item(),
    timestamps as datetimes, NaT as None; containers recursively."""
    m = _mod(x)
    if m == "numpy":
        if hasattr(x, "tolist"):
            return _plain(x.tolist()) if hasattr(x, "shape") and x.shape != () else x.item()
        return x
    if m == "pandas":
        t = type(x).__name__
        if t == "Timestamp":
            return x.to_pydatetime()
        if t == "Timedelta":
            return x.to_pytimedelta()
        if t == "NaTType":
            return None
        if t in ("Period", "Interval"):
            return str(x)
    if isinstance(x, dict):
        return {str(k): _plain(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [_plain(v) for v in x]
    return x

def _kind(o):
    m, t = _mod(o), type(o).__name__
    if m == "pandas" and t == "DataFrame":
        r, c = o.shape
        return f"DataFrame {r}×{c}"
    if m == "pandas" and t == "Series":
        return f"Series {len(o)}"
    if m == "numpy" and hasattr(o, "shape"):
        return f"ndarray {o.shape} {o.dtype}"
    if m == "scipy" and hasattr(o, "toarray"):
        return f"sparse {t} {o.shape}"
    return type(o).__qualname__

def _records(df):
    import pandas as pd
    if isinstance(df.index, pd.RangeIndex):
        return _plain(df.to_dict("records"))
    name = df.index.name
    cols = [str(c) for c in df.columns]
    out = []
    for idx, row in zip(df.index.tolist(), df.itertuples(index=False, name=None)):
        rec = {name if name is not None else "index": idx}
        for c, v in zip(cols, row):
            rec[c] = v
        out.append(rec)
    return _plain(out)

def _rows(o):
    m, t = _mod(o), type(o).__name__
    if m == "pandas" and t == "DataFrame":
        return _records(o)
    if m == "pandas" and t in ("Series", "Index"):
        return _plain(o.tolist())
    if m == "numpy" and hasattr(o, "tolist"):
        return _plain(o.tolist())
    if m == "scipy" and hasattr(o, "toarray"):
        return _plain(o.toarray().tolist())
    if hasattr(o, "_fields"):
        names = list(o._fields) + list(getattr(o, "_extra_fields", ()))
        return {n: _plain(getattr(o, n)) for n in names}
    if hasattr(o, "__iter__") and not hasattr(o, "read"):
        return [_plain(v) for v in o]
    raise TypeError(f"{_kind(o)} has no row reading")

def _table(o):
    """A DataFrame or a 2-d ndarray as a table (header and rows, a
    null None), a 1-d ndarray or a Series on a RangeIndex as a sample
    (one column, no header), None for anything else. A RangeIndex is left out; another index
    is the first column, by its name or `index`."""
    m, t = _mod(o), type(o).__name__
    def cell(v):
        # null is the one missing value: None, NaT and pd.NA come back
        # as null; a nan is a float and stays one.
        try:
            import pandas as pd
            if v is pd.NA or v is pd.NaT:
                return None
        except ImportError:
            pass
        v = _plain(v)
        if v is None:
            return None
        try:
            import pandas as pd
            if v is pd.NA or v is pd.NaT:
                return None
        except ImportError:
            pass
        return v
    if m == "pandas" and t == "DataFrame":
        import pandas as pd
        cols = [str(c) for c in o.columns]
        rows = [[cell(v) for v in r] for r in o.itertuples(index=False, name=None)]
        if not isinstance(o.index, pd.RangeIndex):
            name = str(o.index.name) if o.index.name is not None else "index"
            cols = [name] + cols
            rows = [[cell(i)] + r for i, r in zip(o.index.tolist(), rows)]
        return {"header": cols, "rows": rows}
    if m == "pandas" and t == "Series":
        import pandas as pd
        if isinstance(o.index, pd.RangeIndex):
            return {"header": None, "rows": [[cell(v)] for v in o.tolist()]}
        name = str(o.name) if o.name is not None else "value"
        iname = str(o.index.name) if o.index.name is not None else "index"
        return {"header": [iname, name],
                "rows": [[cell(i), cell(v)] for i, v in zip(o.index.tolist(), o.tolist())]}
    if m == "numpy" and hasattr(o, "ndim"):
        if o.ndim == 2:
            return {"header": None, "rows": [[cell(v) for v in r] for r in o.tolist()]}
        if o.ndim == 1:
            return {"header": None, "rows": [[cell(v)] for v in o.tolist()]}
    return None

def _columns(o):
    m, t = _mod(o), type(o).__name__
    if m == "pandas" and t == "DataFrame":
        import pandas as pd
        if isinstance(o.index, pd.RangeIndex):
            return _plain({str(c): o[c].tolist() for c in o.columns})
        return _plain({str(c): {str(k): v for k, v in o[c].to_dict().items()} for c in o.columns})
    if m == "pandas" and t == "Series":
        return _plain({str(k): v for k, v in o.to_dict().items()})
    if m == "numpy":
        raise TypeError(f"{_kind(o)} has no names; '| @' reads its rows, pd: keeps names")
    if hasattr(o, "_fields"):
        return _rows(o)
    if hasattr(o, "items"):
        return {str(k): _plain(v) for k, v in o.items()}
    raise TypeError(f"{_kind(o)} has no column reading")
"#;

/// The site-packages the embedded interpreter would miss: an
/// active virtualenv's (`VIRTUAL_ENV`), and the prefix the running
/// binary was installed under (a wheel's `bin/qua` beside its
/// `lib/pythonX.Y/site-packages`), added once, after the
/// interpreter's own, through `site.addsitedir` so `.pth` files
/// count too.
pub(crate) fn extend_sys_path(py: Python<'_>) -> PyResult<()> {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_some() {
        return Ok(());
    }
    DONE.set(()).ok();
    let exe_prefix = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().and_then(|b| b.parent()).map(|p| p.display().to_string()))
        .unwrap_or_default();
    let code = c"
import os, site, sys
def _extend(prefix):
    if not prefix:
        return
    v = f'python{sys.version_info[0]}.{sys.version_info[1]}'
    for d in (os.path.join(prefix, 'lib', v, 'site-packages'),
              os.path.join(prefix, 'lib64', v, 'site-packages'),
              os.path.join(prefix, 'Lib', 'site-packages')):
        if os.path.isdir(d) and d not in sys.path:
            site.addsitedir(d)
_extend(os.environ.get('VIRTUAL_ENV'))
_extend(_EXE_PREFIX)
";
    // One namespace for the code: the function body resolves its
    // imports in the globals it was defined under.
    let ns = PyDict::new(py);
    ns.set_item("_EXE_PREFIX", exe_prefix)?;
    py.run(code, Some(&ns), Some(&ns))?;
    Ok(())
}

static MODULE: OnceLock<Py<PyModule>> = OnceLock::new();

fn module<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyModule>> {
    if let Some(m) = MODULE.get() {
        return Ok(m.bind(py).clone());
    }
    extend_sys_path(py)?;
    let m = PyModule::from_code(
        py,
        &CString::new(GLUE).expect("no NUL"),
        c"quarb_foreign.py",
        c"quarb_foreign",
    )?;
    // Importable as `quarb_foreign` from every other glue page.
    py.import("sys")?
        .getattr("modules")?
        .set_item("quarb_foreign", &m)?;
    let _ = MODULE.set(m.clone().unbind());
    Ok(m)
}

/// Compile the helpers (once) so a glue page can import them.
pub(crate) fn register(py: Python<'_>) -> PyResult<()> {
    module(py).map(|_| ())
}

/// Whether a Python object is plain data for the engine (converted
/// as it crosses) or a library value (kept as it is): the builtins
/// and the standard containers convert, numpy scalars convert by
/// their item, everything else is foreign.
pub(crate) fn is_plain(obj: &Bound<'_, PyAny>) -> bool {
    let module = obj
        .get_type()
        .getattr("__module__")
        .ok()
        .and_then(|m| m.extract::<String>().ok())
        .unwrap_or_default();
    matches!(
        module.split('.').next().unwrap_or(""),
        "builtins" | "collections" | "itertools" | "datetime" | "decimal" | "fractions" | "uuid" | "pathlib" | "quarb"
    )
}

/// A Python object held as a library value.
pub struct PyForeign {
    pub obj: Py<PyAny>,
}

impl PyForeign {
    pub fn new(obj: Py<PyAny>) -> PyForeign {
        PyForeign { obj }
    }

    fn glue<T>(&self, name: &str, f: impl Fn(Bound<'_, PyAny>) -> PyResult<T>) -> Result<T, String> {
        Python::attach(|py| {
            let m = module(py).map_err(|e| e.to_string())?;
            let out = m
                .getattr(name)
                .and_then(|g| g.call1((self.obj.bind(py),)))
                .map_err(|e| {
                    // The message without the exception's class.
                    e.value(py).to_string()
                })?;
            f(out).map_err(|e| e.to_string())
        })
    }
}

impl Foreign for PyForeign {
    fn kind(&self) -> String {
        self.glue("_kind", |o| o.extract::<String>())
            .unwrap_or_else(|_| "a Python object".to_string())
    }

    fn display(&self) -> String {
        Python::attach(|py| {
            self.obj
                .bind(py)
                .str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|e| e.to_string())
        })
    }

    fn rows(&self) -> Result<Value, String> {
        self.glue("_rows", |o| crate::py_to_value(&o))
    }

    fn columns(&self) -> Result<Value, String> {
        self.glue("_columns", |o| crate::py_to_value(&o))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A frame or an array out of the library as the engine's own
/// table, a one-dimensional one as a list; `None` for an object of
/// another kind (kept as a library value).
pub(crate) fn as_table(obj: &Bound<'_, PyAny>) -> PyResult<Option<Value>> {
    let py = obj.py();
    let m = module(py)?;
    let d = m.getattr("_table")?.call1((obj,))?;
    if d.is_none() {
        return Ok(None);
    }
    let d = d.cast::<PyDict>().map_err(|e| PyErr::from(e))?;
    if let Some(list) = d.get_item("list")? {
        return Ok(Some(crate::py_to_value(&list)?));
    }
    let header: Option<Vec<String>> = match d.get_item("header")? {
        Some(h) if !h.is_none() => Some(h.extract()?),
        _ => None,
    };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    if let Some(rs) = d.get_item("rows")? {
        for r in rs.try_iter()? {
            let mut row = Vec::new();
            for c in r?.try_iter()? {
                row.push(crate::py_to_value(&c?)?);
            }
            rows.push(row);
        }
    }
    // The engine's one rule for an implicit table: flat, it is the
    // table; a frame whose cells hold structures (an object column
    // of lists) keeps its shape as a list of records.
    Ok(Some(Value::table_or_records(quarb::Table { header, rows })))
}
