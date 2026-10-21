//! A command library that reflects the Python package it wraps
//! (spec: Command Libraries, "Reflected commands"): every function
//! and method of the package is a command, its synopsis read from
//! the function's own signature when the gate first meets the name,
//! its documentation the first line of the function's docstring.
//!
//! A method is a function whose first parameter is the receiver, and
//! the input fills it: `@| pd:describe` is `DataFrame.describe(frame)`.
//! A dotted name walks the package: a module, an instance, a class
//! (constructed on the way), a method, an attribute. Operands fill
//! the required positional parameters of the path in order, the
//! input the first named as data (`X`, `a`, `data`), else the first
//! of them; surplus operands fill the first call's remaining
//! positional parameters. An operand keeps the kind its expression
//! yields, and a list is read as the input is (the table rule). An option goes to the first call
//! on the path that declares the parameter, else to one taking
//! `**kwargs`. What the walk cannot resolve before the run (the
//! attribute of a result) is resolved during it, and the gate admits
//! what it cannot check. An object exists only inside one call: a
//! result that is plain data crosses as a value, any other result is
//! a library value, kept as it is for the next command.

use crate::glue::{PyGlue, placed, python_arguments};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use quarb::Value;
use quarb::library::{InputSlot, LibCall, LibFailure, OperandSpec, OptionSpec, Pipe, Synopsis};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const GLUE: &str = r#"
import contextlib, importlib, inspect, math, sys
import quarb_foreign as _fg

# What a method returns, where the signature does not say and the
# next segment of a path needs it: pandas' intermediate objects,
# the estimators that return themselves.
_RETURNS = {
    "pandas": {
        "groupby": "pandas.core.groupby.generic.DataFrameGroupBy",
        "rolling": "pandas.core.window.rolling.Rolling",
        "expanding": "pandas.core.window.expanding.Expanding",
        "ewm": "pandas.core.window.ewm.ExponentialMovingWindow",
        "resample": "pandas.core.resample.Resampler",
    },
    "sklearn": {"fit": "self", "set_params": "self", "partial_fit": "self", "set_output": "self"},
}

def _import(name):
    try:
        return importlib.import_module(name)
    except ImportError:
        return None

def _attr(cur, name):
    try:
        return getattr(cur, name)
    except AttributeError:
        if inspect.ismodule(cur):
            m = _import(cur.__name__ + "." + name)
            if m is not None:
                return m
        raise

def _class_of(path):
    mod, _, name = path.rpartition(".")
    return getattr(importlib.import_module(mod), name)

def _qual(o):
    if inspect.ismodule(o):
        return o.__name__
    if inspect.isclass(o):
        return o.__qualname__
    return type(o).__qualname__

def _nearest(o, name, limit=6):
    import difflib
    pool = [n for n in dir(o) if not n.startswith("_")]
    close = difflib.get_close_matches(name, pool, n=limit, cutoff=0.6)
    return f"; nearest: {', '.join(close)}" if close else ""

def _routine(a):
    """A function, method or ufunc: a segment that is called. A
    callable instance (`stats.norm`) is an attribute, walked into."""
    return inspect.isroutine(a) or (type(a).__module__ == "numpy" and type(a).__name__ == "ufunc")

# A library command is pure (spec: Command Libraries): a reflected
# name that reads or writes outside the query is withheld, by its
# module (the packages' I/O trees), its name, or a parameter that
# takes a file, a buffer or a connection.
_IO_MODULES = {
    "io", "_io", "npyio", "_npyio_impl", "_datasource", "memmap", "_memmap",
    "datasets", "_datasets", "externals", "pickle", "_pickle", "pytables",
    "clipboard", "sql", "parsers", "excel", "json", "formats", "feather_format",
    "parquet", "orc", "stata", "sas", "spss", "xml", "html", "gbq",
}
_IO_NAMES = ("read_", "load", "save", "fetch", "dump", "open", "tofile", "fromfile",
             "memmap", "to_clipboard", "to_sql", "to_gbq")
_IO_PARAMS = {
    "path", "path_or_buf", "path_or_buffer", "filepath_or_buffer", "file", "fname",
    "filename", "file_name", "fid", "buf", "excel_writer", "con", "url", "io",
    "data_home", "destination_table",
}

class Withheld(AttributeError):
    pass

def _withheld(a, name):
    """Why `a` (reached as `name`) is withheld, or None."""
    mod = a.__name__ if inspect.ismodule(a) else (getattr(a, "__module__", None) or "")
    if any(p in _IO_MODULES for p in mod.split(".")[1:]):
        return f"its module {mod}"
    if name.startswith(_IO_NAMES):
        return "its name"
    if _routine(a) or inspect.isclass(a):
        ps, _ = _params(a, False)
        if ps and any(p.name in _IO_PARAMS for p in ps):
            return "a file or connection parameter"
    return None

def _withhold(a, name, path, root):
    why = _withheld(a, name)
    if why is not None:
        qual = f"{root}.{path}" if not path.startswith(root) else path
        raise Withheld(f"'{path}' is withheld: {qual} reads or writes outside the query ({why}); a library command is pure, a file is a mount")

def _params(f, drop_self):
    try:
        sig = inspect.signature(f)
    except (TypeError, ValueError):
        return None, None
    ps = list(sig.parameters.values())
    if drop_self and ps and ps[0].name in ("self", "cls"):
        ps = ps[1:]
    return ps, sig.return_annotation

class Seg:
    __slots__ = ("name", "kind", "params", "obj")

def _plan(root, classes, path, dynamic):
    """Resolve a dotted name from the root package, or from one of
    the method classes, as far as the signatures allow. Where the
    receiver's attributes are its data (`dynamic`: a frame's columns),
    a name found nowhere is the receiver's, resolved during the run."""
    rootmod = importlib.import_module(root)
    names = path.split(".")
    start = "module"
    cur = rootmod
    standing = False      # cur is a class standing for the receiver's type
    try:
        _attr(rootmod, names[0])
    except AttributeError:
        for c in classes:
            cls = _class_of(c)
            if hasattr(cls, names[0]):
                cur, standing, start = cls, True, "input"
                break
        else:
            if dynamic:
                cur, start = None, "input"
            else:
                where = f"{root} and {', '.join(_class_of(c).__name__ for c in classes)} have" if classes else f"{root} has"
                raise AttributeError(f"{where} no '{names[0]}'{_nearest(rootmod, names[0])}")
    segs = []
    for n in names:
        s = Seg(); s.name = n; s.params = None; s.obj = None
        if cur is None:
            s.kind = "unknown"; segs.append(s); continue
        try:
            a = _attr(cur, n)
        except AttributeError:
            if standing:
                # An attribute of the result (a fitted model's
                # `components_`): resolved during the run.
                s.kind = "unknown"; cur = None; segs.append(s); continue
            raise AttributeError(f"{_qual(cur)} has no '{n}'{_nearest(cur, n)}")
        s.obj = a
        _withhold(a, n, path, root)
        if inspect.ismodule(a):
            s.kind = "module"; cur = a; standing = False
        elif inspect.isclass(a):
            s.kind = "class"; s.params, _ = _params(a, False); cur = a; standing = True
        elif _routine(a):
            unbound = standing and inspect.isclass(cur)
            s.kind = "method" if unbound else "function"
            s.params, ret = _params(a, unbound)
            nxt = None
            r = _RETURNS.get(root, {}).get(n)
            if r == "self":
                nxt = cur if inspect.isclass(cur) else type(cur)
            elif r:
                nxt = _class_of(r)
            elif ret in ("Self", "'Self'") and inspect.isclass(cur):
                nxt = cur
            elif inspect.isclass(ret):
                nxt = ret
            cur, standing = nxt, nxt is not None
        else:
            s.kind = "attr"
            if standing:
                # A descriptor on the class: the instance's value is
                # the run's (`DataFrame.index`).
                cur, standing = None, False
            else:
                cur = a
        segs.append(s)
    return start, segs

def _first_line(o):
    doc = inspect.getdoc(o) if o is not None else None
    if not doc:
        return ""
    line = doc.strip().splitlines()[0].strip()
    return line.rstrip(".")

def _synopsis(root, classes, path, dynamic):
    """A parameter is reachable as Python takes it (spec: "Reflected
    commands"): operands fill the positional parameters in signature
    order — the required ones of every call on the path, then the
    defaulted ones of the first call, where surplus operands go —
    and `--name=` fills the parameter so named wherever it stands;
    positional-only is an operand alone, keyword-only an option
    alone; `*args` admits surplus operands, `**kwargs` any option."""
    start, segs = _plan(root, classes, path, dynamic)
    operands, options, seen = [], [], set()
    deferred = []
    variadic = open_ = False
    first = True
    if start == "input":
        operands.append(("self", "the input: the receiver of the method", True))
    for s in segs:
        if s.kind in ("class", "function", "method"):
            if s.params is None:
                variadic = open_ = True
                first = False
                continue
            leading = _first_positional(s)
            for p in s.params:
                if p.kind == p.VAR_POSITIONAL:
                    variadic = True
                    continue
                if p.kind == p.VAR_KEYWORD:
                    open_ = True
                    continue
                required = p.default is p.empty
                d = p.default
                shown = "" if required or repr(d).startswith("<") else repr(d)
                doc = f"default {shown}" if shown else ""
                if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD):
                    if required:
                        operands.append((p.name, "", True))
                    elif first and p is leading and p.name in _DATA_NAMES:
                        # A class or function whose first parameter is
                        # its data, defaulted (`DataFrame(data=None)`):
                        # the input's slot, ahead of the rest.
                        operands.append((p.name, doc, False))
                    elif first:
                        deferred.append((p.name, doc, False))
                if p.kind != p.POSITIONAL_ONLY:
                    long = p.name.replace("_", "-")
                    if long not in seen:
                        seen.add(long)
                        options.append((long, doc))
            first = False
        elif s.kind == "unknown":
            variadic = open_ = True
    # The slot the input fills: the receiver of a method; else the
    # first parameter named as data (`X` in `cross_val_score(estimator,
    # X, y)`, `data` of a constructor), else the first — judged before
    # the first call's defaulted positionals join the operands.
    input_at = 0
    if start != "input":
        input_at = next((i for i, (n, _, _) in enumerate(operands) if n in _DATA_NAMES), 0)
    operands += deferred
    last = segs[-1]
    if last.kind == "module":
        raise AttributeError(
            f"'{path}' is a module of {root}, not a command: name a function or a class in it ({path}.<name>)")
    doc = _first_line(last.obj)
    returns = ""
    if last.kind in ("function", "method") and last.params is not None:
        try:
            ret = inspect.signature(last.obj).return_annotation
            if ret is not inspect.Signature.empty:
                returns = ret if isinstance(ret, str) else getattr(ret, "__name__", str(ret))
        except (TypeError, ValueError):
            pass
    elif last.kind == "class":
        returns = last.obj.__name__
    # A path that opens with a class may take the input as the
    # instance itself (a fitted estimator before `.predict(X)`).
    calls = [s for s in segs if s.kind in ("class", "function", "method", "unknown")]
    instance = start != "input" and bool(calls) and calls[0].kind == "class"
    return {"operands": operands, "options": options, "variadic": variadic,
            "open": open_, "doc": doc, "returns": returns.strip("'"),
            "input": input_at, "instance": instance}

def _numeric(v):
    # A boolean reads 1 for true, 0 for false (the table rule), so a
    # boolean field is a column.
    if isinstance(v, bool):
        return 1.0 if v else 0.0
    if isinstance(v, (int, float)):
        return float(v)
    if hasattr(v, "magnitude"):
        return float(v.magnitude)
    if isinstance(v, str):
        try:
            return float(v.replace(",", ""))
        except ValueError:
            return None
    return None

def _read(v):
    """A value by its numeric reading (the table rule): numeric text
    as the number it reads as (an integer form exact), a quantity by
    its magnitude, a boolean as 1 or 0, anything else as it is."""
    if isinstance(v, bool):
        return 1 if v else 0
    if isinstance(v, str):
        t = v.strip()
        try:
            return int(t)
        except ValueError:
            pass
        try:
            return float(t)
        except ValueError:
            return v
    if hasattr(v, "magnitude"):
        return v.magnitude
    return v

def _isnum(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)

_scalar = _read

def _array(x):
    import numpy as np
    import datetime as dt
    if isinstance(x, list):
        if x and all(isinstance(v, dt.datetime) for v in x):
            return np.array([v.replace(tzinfo=None) if v.tzinfo else v for v in x], dtype="datetime64[ns]")
        if x and all(isinstance(v, dt.timedelta) for v in x):
            return np.array(x, dtype="timedelta64[ns]")
        if x and all(isinstance(v, list) for v in x):
            return np.array([_array(v) for v in x])
        read = [_read(v) for v in x]
        if read and all(v is None or _isnum(v) for v in read):
            if any(v is None for v in read):
                return np.array([np.nan if v is None else float(v) for v in read], dtype=float)
            return np.array(read)
        return np.array(read)
    return x

def _matrix(records):
    """The table rule for an array: the numeric fields of the first
    record are the columns, the first text field the label (dropped:
    an array has no names; pd: keeps them)."""
    import numpy as np
    first = records[0]
    cols = [k for k, v in first.items() if _numeric(v) is not None]
    label = next((k for k, v in first.items() if isinstance(v, str) and _numeric(v) is None), None)
    if not cols:
        raise TypeError("no numeric field in the first record; nothing to make a matrix of")
    out = []
    for i, r in enumerate(records):
        row = []
        for c in cols:
            v = _numeric(r.get(c))
            if v is None:
                who = r.get(label, i + 1) if label else i + 1
                raise TypeError(f"row {who}: field '{c}' is {'null (a hole in the matrix)' if r.get(c) is None else 'not numeric'}")
            row.append(v)
        out.append(row)
    return np.array(out, dtype=float)

def _series(values, index=None):
    """A column: numeric when every non-null value reads as a number
    (the table rule), text otherwise. A null crosses as pd.NA in a
    nullable dtype (Float64, Int64, boolean, string), never as NaN,
    so it comes back as null; a nan is a float like any other."""
    import pandas as pd
    read = [_read(v) for v in values]
    present = [v for v in read if v is not None]
    if any(v is None for v in read):
        if present and all(_isnum(v) for v in present):
            dtype = "Int64" if all(isinstance(v, int) for v in present) else "Float64"
            return pd.array([pd.NA if v is None else v for v in read], dtype=dtype)
        return pd.array([pd.NA if v is None else str(v) for v in read], dtype="string")
    if present and all(_isnum(v) for v in present):
        return pd.Series(read, index=index)
    return pd.Series(read, index=index, dtype=object) if index is not None else pd.Series(read, dtype=object)

def _numeric_columns(df):
    """Every object column through `_series`: numeric readings,
    nulls as pd.NA in the nullable dtypes."""
    import pandas as pd
    for c in df.columns:
        col = df[c]
        # a boolean column reads 1 and 0 (the table rule)
        if str(col.dtype) == "bool":
            df[c] = col.astype("int64")
            continue
        if str(col.dtype) == "boolean":
            df[c] = col.astype("Int64")
            continue
        # pandas 3 infers its `str` dtype for text; it reads as
        # object does.
        if col.dtype != object and str(col.dtype) not in ("str", "string"):
            continue
        s = _series(col.tolist())
        df[c] = s if not isinstance(s, pd.Series) else s.values
    return df

def _frame(x):
    import pandas as pd
    if isinstance(x, list):
        if x and all(isinstance(r, dict) for r in x):
            df = pd.DataFrame(x)
            label = next((k for k, v in x[0].items() if isinstance(v, str) and _numeric(v) is None), None)
            if label is not None and label in df.columns:
                df = df.set_index(label)
            return _numeric_columns(df)
        if x and all(isinstance(r, list) for r in x):
            return _numeric_columns(pd.DataFrame([[_read(v) for v in r] for r in x]))
        s = _series(x)
        return s if isinstance(s, pd.Series) else pd.Series(s)
    if isinstance(x, dict):
        return pd.Series({k: _read(v) for k, v in x.items()})
    return x

def _construct(x, mode):
    if mode == "none" or not isinstance(x, (list, dict)):
        return x
    # A record is a one-row table by the table rule (the user's
    # ruling on constructors as operands): a frame of one row, or
    # the matrix of its numeric fields.
    if isinstance(x, dict):
        x = [x]
    if mode == "frame":
        return _frame(x)
    if x and all(isinstance(r, dict) for r in x):
        return _matrix(x)
    return _array(x)

_DATA_NAMES = {"data", "object", "a", "x", "X", "values", "arr", "obj", "shape"}

def _required(s, named=()):
    """How many operands a call takes as its required positional
    parameters, those filled by name left out."""
    if s.params is None:
        return 0
    return sum(1 for p in s.params
               if p.default is p.empty and p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)
               and p.name not in named)

_plans = {}

def _first_positional(s):
    if not s.params:
        return None
    return next((p for p in s.params if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)), None)

def _takes_data(s):
    """Whether a class's constructor takes data first (`DataFrame(data)`,
    `ndarray(shape)`); an estimator's (`GaussianNB(priors)`) does not."""
    first = _first_positional(s)
    return s.params is None or (first is not None and first.name in _DATA_NAMES)

def _call(root, classes, path, construct, dynamic, args, kwargs, input_at=None):
    with contextlib.redirect_stdout(sys.stderr):
        key = (root, tuple(classes), path)
        if key not in _plans:
            _plans[key] = _plan(root, classes, path, dynamic)
        start, segs = _plans[key]
        # Every operand keeps the kind its expression yields and a list
        # is read as the input is (the table rule): a list of records
        # a matrix or frame, numeric readings a vector, a library
        # value itself.
        queue = [_construct(a, construct) for a in args]
        # Where the input stands in the queue, followed through the
        # pops; None once a call has taken it.
        def taken(k):
            nonlocal input_at
            if input_at is not None:
                input_at = None if input_at < k else input_at - k
        calls = [s for s in segs if s.kind in ("class", "function", "method", "unknown")]
        receiver = None
        if start == "input":
            if not queue:
                raise TypeError(f"{path}: the receiver is missing (nothing on the pipe)")
            receiver = queue.pop(0)
            taken(1)
        pos = {id(s): [] for s in segs}
        instance = None
        if calls and calls[0].kind == "class" and queue:
            cls = calls[0].obj
            if inspect.isclass(cls) and isinstance(queue[0], cls):
                # The input is an instance of the class on the path (a
                # fitted estimator, a frame): the receiver of its
                # methods, not constructed again.
                instance = queue.pop(0)
                taken(1)
            else:
                # A class first on the path whose first parameter is its
                # data (`pd:Series.rolling.mean(12)`, `np:ndarray`) is
                # constructed from the input; an estimator's constructor
                # (KMeans(n_clusters)) is not, and the input reaches its
                # method.
                first = _first_positional(calls[0])
                if first is not None and first.name in _DATA_NAMES and first.default is not first.empty:
                    pos[id(calls[0])].append(queue.pop(0))
                    taken(1)
        # An option fills the parameter so named on the first call
        # that declares it, else one taking **kwargs; a required
        # parameter it fills is not taken from the operands.
        kw = {id(s): {} for s in segs}
        for k, v in kwargs.items():
            target = None
            for s in calls:
                if s.params is None or any(p.name == k for p in s.params):
                    target = s
                    break
            if target is None:
                for s in calls:
                    if s.params is not None and any(p.kind == p.VAR_KEYWORD for p in s.params):
                        target = s
                        break
            if target is None:
                raise TypeError(f"{path}: no parameter named '{k.replace('_', '-')}'")
            kw[id(target)][k] = v
        for s in calls:
            need = 0 if (instance is not None and s is calls[0]) else _required(s, kw[id(s)])
            pos[id(s)], queue = pos[id(s)] + queue[:need], queue[need:]
            taken(need)
        if queue:
            if not calls:
                raise TypeError(f"{path}: {len(queue)} operand(s) given, none taken (an attribute)")
            # The input passing a bare constructor that takes no data
            # (an estimator, built from its options alone) is not
            # handed to it: the instance is the result.
            if (input_at is not None and input_at < len(queue) and calls[0].kind == "class"
                    and instance is None and not _takes_data(calls[0])):
                del queue[input_at]
            pos[id(calls[0])] += queue
        cur = receiver if start == "input" else importlib.import_module(root)
        for s in segs:
            a = _attr(cur, s.name)
            if s.kind == "unknown":
                _withhold(a, s.name, path, root)
            if s.kind == "module":
                cur = a
            elif s.kind == "class" and instance is not None and s is calls[0]:
                cur = instance
            elif s.kind in ("class", "function", "method") or (s.kind == "unknown" and callable(a)):
                cur = a(*pos[id(s)], **kw[id(s)])
            elif pos[id(s)] or kw[id(s)]:
                # An attribute given arguments: a callable instance
                # (`stats.norm(0; 1)` freezes a distribution).
                if not callable(a):
                    raise TypeError(f"{path}: '{s.name}' is an attribute and takes no arguments")
                cur = a(*pos[id(s)], **kw[id(s)])
            else:
                cur = a
        return cur
"#;

static PY: PyGlue = PyGlue::new(GLUE, "quarb_reflect.py", "quarb_reflect");

/// The reflection of one package: its root module, the classes
/// whose methods a bare name may be (`DataFrame`, `Series` for
/// `pd:`), and how the input is constructed from a plain value
/// (`ndarray`, `frame`, or `none` to pass it as it is).
pub struct Reflective {
    root: &'static str,
    classes: &'static [&'static str],
    construct: &'static str,
    /// Whether the receiver's attributes are its data (a frame's
    /// columns: `pd:x.mean`), so a name the package and the classes
    /// lack is the receiver's, resolved during the run.
    dynamic: bool,
    cache: OnceLock<Mutex<HashMap<String, &'static Synopsis>>>,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

impl Reflective {
    pub const fn new(
        root: &'static str,
        classes: &'static [&'static str],
        construct: &'static str,
        dynamic: bool,
    ) -> Reflective {
        Reflective {
            root,
            classes,
            construct,
            dynamic,
            cache: OnceLock::new(),
        }
    }

    /// The package's name, for messages.
    pub fn root(&self) -> &'static str {
        self.root
    }

    /// The synopsis of a dotted name, read from the package once
    /// and kept for the process; `Err` is the refusal when the
    /// package has no such name (its nearest names included).
    pub fn resolve(&self, name: &str) -> Result<&'static Synopsis, String> {
        let cache = self.cache.get_or_init(|| Mutex::new(HashMap::new()));
        if let Some(s) = cache.lock().expect("synopsis cache").get(name) {
            return Ok(s);
        }
        let syn = Python::attach(|py| -> Result<Synopsis, String> {
            let m = PY.module(py).map_err(|e| e.to_string())?;
            let d = m
                .getattr("_synopsis")
                .and_then(|f| f.call1((self.root, self.classes, name, self.dynamic)))
                .map_err(|e| e.value(py).to_string())?;
            let d = d.cast::<PyDict>().map_err(|e| e.to_string())?;
            let get = |k: &str| d.get_item(k).ok().flatten();
            let operands: Vec<(String, String, bool)> = get("operands")
                .and_then(|v| v.extract().ok())
                .unwrap_or_default();
            let options: Vec<(String, String)> = get("options")
                .and_then(|v| v.extract().ok())
                .unwrap_or_default();
            let variadic: bool = get("variadic")
                .and_then(|v| v.extract().ok())
                .unwrap_or(false);
            let open: bool = get("open").and_then(|v| v.extract().ok()).unwrap_or(false);
            let doc: String = get("doc")
                .and_then(|v| v.extract().ok())
                .unwrap_or_default();
            let returns: String = get("returns")
                .and_then(|v| v.extract().ok())
                .unwrap_or_default();
            let input_at: usize = get("input").and_then(|v| v.extract().ok()).unwrap_or(0);
            let instance: bool = get("instance")
                .and_then(|v| v.extract().ok())
                .unwrap_or(false);
            let mut ops: Vec<OperandSpec> = operands
                .into_iter()
                .map(|(n, d, required)| OperandSpec {
                    name: leak(n),
                    doc: leak(d),
                    values: &[],
                    required,
                })
                .collect();
            if variadic {
                ops.push(OperandSpec {
                    name: "...",
                    doc: "further positional arguments, as the function takes them",
                    values: &[],
                    required: false,
                });
            }
            let mut opts: Vec<OptionSpec> = options
                .into_iter()
                .map(|(long, d)| OptionSpec {
                    short: None,
                    long: leak(long),
                    takes_value: true,
                    doc: leak(d),
                })
                .collect();
            if open {
                opts.push(OptionSpec {
                    short: None,
                    long: "*",
                    takes_value: true,
                    doc: "any other keyword argument the function takes",
                });
            }
            Ok(Synopsis {
                name: leak(name.to_string()),
                doc: leak(doc),
                pipes: Pipe::Both,
                operands: Box::leak(ops.into_boxed_slice()),
                options: Box::leak(opts.into_boxed_slice()),
                input: if instance {
                    InputSlot::OperandOrSelf(input_at)
                } else {
                    InputSlot::Operand(input_at)
                },
                returns: leak(if returns.is_empty() {
                    "a value, or a library value".to_string()
                } else {
                    returns
                }),
            })
        })?;
        let syn: &'static Synopsis = Box::leak(Box::new(syn));
        cache
            .lock()
            .expect("synopsis cache")
            .insert(name.to_string(), syn);
        Ok(syn)
    }

    /// Walk the name with the call's values; the result as a value
    /// (plain data) or a library value (anything else).
    pub fn call(&self, syn: &Synopsis, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        let (positional, keyword, input_at) = placed(syn, call);
        Python::attach(|py| -> Result<Value, LibFailure> {
            let failed = |e: PyErr| LibFailure::Failed(e.value(py).to_string());
            let m = PY.module(py).map_err(failed)?;
            let (args, kwargs) = python_arguments(py, &positional, &keyword).map_err(failed)?;
            let out = m
                .getattr("_call")
                .map_err(failed)?
                .call1((
                    self.root,
                    self.classes,
                    name,
                    self.construct,
                    self.dynamic,
                    args,
                    kwargs,
                    input_at,
                ))
                .map_err(failed)?;
            crate::py_to_value(&out).map_err(failed)
        })
    }
}
