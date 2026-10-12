//! `sp:` — scipy as a Quarb command library. Two layers under one
//! prefix: the declared commands below (significance tests,
//! correlations with their p-values, a regression line and
//! hierarchical clustering over the stream after `@|`; spec: Command
//! Libraries, "The stream as a table"), and, for every other name,
//! the package itself by reflection (`sp:stats.ttest_ind(b)`,
//! `sp:signal.find_peaks`, `sp:stats.norm.pdf(--loc=0)`; spec:
//! "Reflected commands"), the synopsis read from the function's
//! signature, the stream crossing as an array, an array coming back
//! kept as a library value. A declared command shadows the
//! reflected name where the record shape matters (the k-sample
//! split, `--pairs`, cluster numbering, `--digits`).
//!
//! The declared commands: the synopsis the engine checks before the
//! run is here, in Rust; a page of Python glue turns the stream into
//! numpy arrays and scipy's result objects into records, and the
//! values cross through `quarb-pyvalue`.
//!
//! The table rule, as the glue reads it: a list of numbers is one
//! sample; a list of records is a table whose numeric fields (a
//! numeric reading per value: numbers, quantities by their
//! magnitude, numeric text) are the columns in record order and
//! whose first text field is the row's label, carried back on every
//! result that returns a row; a list of lists is the rows. A
//! k-sample command splits records of one label and one numeric
//! field into one sample per label, in the order the labels first
//! appear; a table read by its entries (`--pairs`) is two text
//! fields and one numeric field, row, column, entry, by position. A
//! hole (a record lacking a column its peers have, or carrying a
//! value with no numeric reading in one) refuses the command and is
//! named; the tests accept `--nan-policy=omit`, which drops it.
//!
//! The wrapper speaks scipy's names: every test returns
//! `%(statistic; pvalue; …)`, `linregress` its `slope`, `intercept`,
//! `rvalue`, `pvalue`, `stderr`, so scipy's documentation is the
//! documentation of these commands and the prefix tells
//! `sp:spearmanr` (with its p-value) from the stdlib's `spearman`.
//!
//! The stream crosses as a list and becomes an array in the glue, a
//! copy; the Arrow path waits for a measured need.

use quarb::Value;
use quarb::library::{
    CommandLibrary, InputSlot, LibCall, LibFailure, OperandSpec, OptionSpec, Pipe, Synopsis,
};
use quarb_pyvalue::{PyGlue, Reflective};

const GLUE: &str = r#"
import contextlib, math, sys
import numpy as np
from scipy import stats
from scipy.cluster import hierarchy
from scipy.spatial.distance import squareform

class Hole(ValueError):
    pass

def _as_list(x):
    return x if isinstance(x, list) else [x]

def _num(v):
    """The numeric reading of a value, or None where it has none."""
    if isinstance(v, bool):
        return 1.0 if v else 0.0
    if isinstance(v, (int, float)):
        return float(v)
    if hasattr(v, "magnitude"):
        return float(v.magnitude)
    if isinstance(v, str):
        try:
            return float(v)
        except ValueError:
            return None
    return None

def _sig(x, digits):
    if digits is None or x is None or not isinstance(x, float) or x == 0 or not math.isfinite(x):
        return x
    return round(x, int(digits) - 1 - int(math.floor(math.log10(abs(x)))))

def _finish(v, digits):
    if isinstance(v, dict):
        return {k: _finish(w, digits) for k, w in v.items()}
    if isinstance(v, list):
        return [_finish(w, digits) for w in v]
    if isinstance(v, (np.floating,)):
        v = float(v)
    if isinstance(v, (np.integer,)):
        v = int(v)
    if isinstance(v, float) and math.isnan(v):
        return None
    return _sig(v, digits) if isinstance(v, float) else v

def _table(x, nan_policy=None):
    """(labels, columns, matrix) from a list of records, lists or numbers."""
    x = _as_list(x)
    if not x:
        raise Hole("the stream is empty")
    if isinstance(x[0], dict):
        cols = None
        labels, rows = [], []
        # A field null in every record is no column (a node's empty
        # text, say); a field null in some is a hole.
        empty = {k for k in x[0] if all(isinstance(r, dict) and r.get(k) is None for r in x)}
        for i, r in enumerate(x):
            if not isinstance(r, dict):
                raise Hole(f"row {i} is no record")
            label, vals, mine = None, {}, []
            for k, v in r.items():
                if k in empty:
                    continue
                n = _num(v)
                if n is not None:
                    vals[k] = n; mine.append(k)
                elif v is None:
                    if nan_policy == "omit":
                        vals[k] = float("nan"); mine.append(k)
                    else:
                        raise Hole(f"row {label or i}: field '{k}' is null (a hole); --nan-policy=omit drops it")
                elif isinstance(v, str) and label is None:
                    label = v
            if cols is None:
                cols = mine
            if mine != cols:
                missing = [c for c in cols if c not in mine] + [c for c in mine if c not in cols]
                raise Hole(f"row {label or i}: field '{missing[0]}' is null (a hole; the other rows' numeric fields: {', '.join(cols)})")
            labels.append(label if label is not None else i)
            rows.append([vals[c] for c in cols])
        return labels, cols, np.array(rows, dtype=float).reshape(len(rows), len(cols))
    if isinstance(x[0], list):
        rows = []
        for i, r in enumerate(x):
            vals = [_num(v) for v in _as_list(r)]
            if any(v is None for v in vals):
                raise Hole(f"row {i}: a value with no numeric reading")
            rows.append(vals)
        width = max(len(r) for r in rows)
        if any(len(r) != width for r in rows):
            raise Hole("the rows differ in length")
        return list(range(len(rows))), [str(j) for j in range(width)], np.array(rows, dtype=float)
    vals = [_num(v) for v in x]
    for i, v in enumerate(vals):
        if v is None:
            if x[i] is None and nan_policy == "omit":
                vals[i] = float("nan")
            else:
                raise Hole(f"item {i} ({x[i]!r}) has no numeric reading")
    return list(range(len(vals))), ["0"], np.array(vals, dtype=float).reshape(-1, 1)

def _vector(x, nan_policy=None):
    """One sample: a list of numbers; a stream of one list is that
    list; a one-column table its column. A table of several rows
    and columns is no sample and says which column to name."""
    x = _as_list(x)
    if len(x) == 1 and isinstance(x[0], list):
        x = x[0]
    labels, cols, m = _table(x, nan_policy)
    if m.shape[1] == 1:
        v = m[:, 0]
    elif m.shape[0] == 1:
        v = m[0, :]
    else:
        raise Hole(f"a sample is a list of numbers; {m.shape[0]} rows of {m.shape[1]} columns given: name the column")
    return v[~np.isnan(v)] if nan_policy == "omit" else v

def _samples(x, other=None, nan_policy=None):
    """One array per sample: the stream and the operand; the rows of a
    list of lists; or records of one label and one value split by
    label in the order the labels first appear."""
    if other is not None:
        return [_vector(x, nan_policy), _vector(other, nan_policy)]
    x = _as_list(x)
    if x and isinstance(x[0], list):
        return [_vector(r, nan_policy) for r in x]
    if x and isinstance(x[0], dict):
        groups = {}
        for i, r in enumerate(x):
            label, val, null = None, None, None
            for k, v in r.items():
                n = _num(v)
                if n is not None and val is None:
                    val = n
                elif v is None and val is None:
                    if nan_policy == "omit":
                        val = float("nan")
                    else:
                        null = k
                elif isinstance(v, str) and label is None:
                    label = v
            if val is None and null is not None:
                raise Hole(f"row {label or i}: field '{null}' is null (a hole); --nan-policy=omit drops it")
            if label is None or val is None:
                raise Hole(f"row {i}: a sample row is one label and one value")
            groups.setdefault(label, []).append(val)
        if len(groups) < 2:
            raise Hole("one label only; a k-sample test needs two or more")
        return [np.array(g, dtype=float) for g in groups.values()]
    return [_vector(x, nan_policy)]

def _xy(x, nan_policy=None):
    labels, cols, m = _table(x, nan_policy)
    if m.shape[1] < 2:
        raise Hole("two numeric columns are needed (x, then y)")
    a, b = m[:, 0], m[:, 1]
    if nan_policy == "omit":
        # pairwise: a hole in either drops the row
        keep = ~(np.isnan(a) | np.isnan(b))
        a, b = a[keep], b[keep]
    return a, b

def _two(samples, name):
    if len(samples) != 2:
        raise Hole(f"{name} compares two samples; {len(samples)} given")
    return samples

def _res(r, *extra):
    out = {"statistic": float(r.statistic), "pvalue": float(r.pvalue)}
    for k in extra:
        out[k] = getattr(r, k)
    return out

def _policy(nan_policy):
    return nan_policy or "propagate"

# --- descriptive -------------------------------------------------------

def describe(x, nan_policy=None):
    d = stats.describe(_vector(x, nan_policy), nan_policy=_policy(nan_policy))
    return {"nobs": int(d.nobs), "mean": float(d.mean), "variance": float(d.variance),
            "skewness": float(d.skewness), "kurtosis": float(d.kurtosis),
            "min": float(d.minmax[0]), "max": float(d.minmax[1])}

# --- tests -------------------------------------------------------------

def ttest(x, sample=None, popmean=None, paired=False, equal_var=True, alternative="two-sided", nan_policy=None):
    s = _samples(x, sample, nan_policy)
    if len(s) == 1:
        if popmean is None:
            raise Hole("one sample: --popmean=… gives the mean to test against")
        r = stats.ttest_1samp(s[0], float(popmean), alternative=alternative, nan_policy=_policy(nan_policy))
    else:
        a, b = _two(s, "ttest")
        if paired:
            r = stats.ttest_rel(a, b, alternative=alternative, nan_policy=_policy(nan_policy))
        else:
            r = stats.ttest_ind(a, b, equal_var=bool(equal_var), alternative=alternative, nan_policy=_policy(nan_policy))
    return _res(r, "df")

def mannwhitneyu(x, sample=None, alternative="two-sided", nan_policy=None):
    a, b = _two(_samples(x, sample, nan_policy), "mannwhitneyu")
    return _res(stats.mannwhitneyu(a, b, alternative=alternative, nan_policy=_policy(nan_policy)))

def wilcoxon(x, sample=None, alternative="two-sided", nan_policy=None):
    a, b = _two(_samples(x, sample, nan_policy), "wilcoxon")
    return _res(stats.wilcoxon(a, b, alternative=alternative, nan_policy=_policy(nan_policy)))

def kstest(x, sample=None, cdf=None, alternative="two-sided", nan_policy=None):
    s = _samples(x, sample, nan_policy)
    if len(s) == 1:
        if cdf is None:
            raise Hole("one sample: --cdf=norm names the distribution to test against")
        a = s[0]
        dist = getattr(stats, cdf)
        params = dist.fit(a)
        return _res(stats.kstest(a, cdf, args=params, alternative=alternative))
    a, b = _two(s, "kstest")
    return _res(stats.ks_2samp(a, b, alternative=alternative))

def f_oneway(x, nan_policy=None):
    return _res(stats.f_oneway(*_samples(x, None, nan_policy)))

def kruskal(x, nan_policy=None):
    return _res(stats.kruskal(*_samples(x, None, nan_policy), nan_policy=_policy(nan_policy)))

def shapiro(x, nan_policy=None):
    return _res(stats.shapiro(_vector(x, nan_policy)))

def chi2_contingency(x):
    labels, cols, m = _table(x)
    r = stats.chi2_contingency(m)
    return {"statistic": float(r.statistic), "pvalue": float(r.pvalue), "dof": int(r.dof)}

def fisher_exact(x, alternative="two-sided"):
    labels, cols, m = _table(x)
    if m.shape != (2, 2):
        raise Hole(f"fisher_exact reads a 2×2 table; {m.shape[0]}×{m.shape[1]} given")
    return _res(stats.fisher_exact(m.astype(int), alternative=alternative))

# --- association -------------------------------------------------------

def pearsonr(x, alternative="two-sided", nan_policy=None):
    a, b = _xy(x, nan_policy)
    return _res(stats.pearsonr(a, b, alternative=alternative))

def spearmanr(x, alternative="two-sided", nan_policy=None):
    a, b = _xy(x, nan_policy)
    return _res(stats.spearmanr(a, b, alternative=alternative, nan_policy=_policy(nan_policy)))

def kendalltau(x, alternative="two-sided", nan_policy=None):
    a, b = _xy(x, nan_policy)
    return _res(stats.kendalltau(a, b, alternative=alternative, nan_policy=_policy(nan_policy)))

def linregress(x, alternative="two-sided", nan_policy=None):
    a, b = _xy(x, nan_policy)
    r = stats.linregress(a, b, alternative=alternative)
    return {"slope": float(r.slope), "intercept": float(r.intercept), "rvalue": float(r.rvalue),
            "pvalue": float(r.pvalue), "stderr": float(r.stderr)}

# --- hierarchical clustering ------------------------------------------

def _pairs(x):
    """A table read by its entries: two text fields and one numeric
    field per record, row, column, entry by position; the labels in
    the order they first appear; a condensed distance matrix."""
    x = _as_list(x)
    names, entries = [], {}
    for i, r in enumerate(x):
        if not isinstance(r, dict):
            raise Hole(f"row {i} is no record")
        texts, nums = [], []
        for v in r.values():
            n = _num(v)
            if n is not None:
                nums.append(n)
            elif isinstance(v, str):
                texts.append(v)
        if len(texts) < 2 or not nums:
            raise Hole(f"row {i}: a pair row is two labels and one distance")
        a, b, d = texts[0], texts[1], nums[0]
        for n in (a, b):
            if n not in entries:
                entries[n] = len(names); names.append(n)
        entries.setdefault((a, b), d)
    n = len(names)
    m = np.zeros((n, n))
    seen = np.zeros((n, n), dtype=bool)
    for k, d in entries.items():
        if isinstance(k, tuple):
            i, j = entries[k[0]], entries[k[1]]
            m[i, j] = m[j, i] = d; seen[i, j] = seen[j, i] = True
    np.fill_diagonal(seen, True)
    if not seen.all():
        i, j = np.argwhere(~seen)[0]
        raise Hole(f"no distance between {names[i]} and {names[j]}")
    return names, squareform(m, checks=False)

def _linkage(x, method, metric, pairs):
    if pairs:
        labels, condensed = _pairs(x)
        return labels, hierarchy.linkage(condensed, method=method)
    labels, cols, m = _table(x)
    return [str(l) for l in labels], hierarchy.linkage(m, method=method, metric=metric)

def linkage(x, method="average", metric="euclidean", pairs=False):
    labels, Z = _linkage(x, method, metric, pairs)
    n = len(labels)
    members = {i: [labels[i]] for i in range(n)}
    out = []
    for step, (a, b, d, size) in enumerate(Z):
        a, b = int(a), int(b)
        members[n + step] = members[a] + members[b]
        out.append({"a": members[a], "b": members[b], "distance": float(d), "size": int(size)})
    return out

def fcluster(x, t=2, method="average", metric="euclidean", pairs=False):
    labels, Z = _linkage(x, method, metric, pairs)
    flat = hierarchy.fcluster(Z, int(t), criterion="maxclust")
    seen = {}
    return [{"label": l, "cluster": seen.setdefault(int(c), len(seen) + 1)} for l, c in zip(labels, flat)]

def leaves(x, method="average", metric="euclidean", pairs=False):
    labels, Z = _linkage(x, method, metric, pairs)
    return [labels[i] for i in hierarchy.leaves_list(Z)]

def _call(name, args, kwargs):
    f = globals()[name]
    digits = kwargs.pop("digits", None)
    with contextlib.redirect_stdout(sys.stderr):
        try:
            return _finish(f(*args, **kwargs), digits)
        except Hole as e:
            raise ValueError(str(e)) from None
"#;

/// The library.
pub struct Scipy;

const NAN: OptionSpec = OptionSpec {
    short: None,
    long: "nan-policy",
    takes_value: true,
    doc: "omit drops a null (a hole) from the sample; without it a hole refuses the command",
};
const DIGITS: OptionSpec = OptionSpec {
    short: None,
    long: "digits",
    takes_value: true,
    doc: "round every float in the result to this many significant digits",
};
const ALT: OptionSpec = OptionSpec {
    short: None,
    long: "alternative",
    takes_value: true,
    doc: "two-sided (the default), less, greater",
};
const METHOD: OptionSpec = OptionSpec {
    short: None,
    long: "method",
    takes_value: true,
    doc: "the linkage: average (the default), single, complete, ward, weighted, centroid, median",
};
const METRIC: OptionSpec = OptionSpec {
    short: None,
    long: "metric",
    takes_value: true,
    doc: "the distance between rows: euclidean (the default), cityblock, cosine, correlation, chebyshev",
};
const PAIRS: OptionSpec = OptionSpec {
    short: None,
    long: "pairs",
    takes_value: false,
    doc: "the input is a table read by its entries: two labels and a distance per record",
};

const STREAM: OperandSpec = OperandSpec {
    name: "stream",
    doc: "the sample or table (the input)",
    values: &[],
    required: true,
};
const SAMPLE: OperandSpec = OperandSpec {
    name: "sample",
    doc: "the second sample, when the stream is the first",
    values: &[],
    required: false,
};
const TABLE: OperandSpec = OperandSpec {
    name: "table",
    doc: "the table (the input)",
    values: &[],
    required: true,
};

const TEST_OPTIONS: &[OptionSpec] = &[ALT, NAN, DIGITS];

const COMMANDS: &[Synopsis] = &[
    Synopsis {
        name: "describe",
        doc: "the sample described: nobs, mean, variance, skewness, kurtosis, min, max",
        pipes: Pipe::Agg,
        operands: &[STREAM],
        options: &[NAN, DIGITS],
        input: InputSlot::Operand(0),
        returns: "record %(nobs; mean; variance; skewness; kurtosis; min; max)",
    },
    Synopsis {
        name: "ttest",
        doc: "Student's t: two samples (the stream and the operand, or one sample per label), paired with --paired, Welch's with --equal-var=false; one sample against --popmean",
        pipes: Pipe::Agg,
        operands: &[STREAM, SAMPLE],
        options: &[
            OptionSpec {
                short: None,
                long: "popmean",
                takes_value: true,
                doc: "the mean a single sample is tested against",
            },
            OptionSpec {
                short: None,
                long: "paired",
                takes_value: false,
                doc: "the two samples are paired (ttest_rel)",
            },
            OptionSpec {
                short: None,
                long: "equal-var",
                takes_value: true,
                doc: "true (the default, Student's) or false (Welch's)",
            },
            ALT,
            NAN,
            DIGITS,
        ],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue; df)",
    },
    Synopsis {
        name: "mannwhitneyu",
        doc: "the Mann–Whitney U test over two samples",
        pipes: Pipe::Agg,
        operands: &[STREAM, SAMPLE],
        options: TEST_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "wilcoxon",
        doc: "the Wilcoxon signed-rank test over two paired samples",
        pipes: Pipe::Agg,
        operands: &[STREAM, SAMPLE],
        options: TEST_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "kstest",
        doc: "Kolmogorov–Smirnov: two samples, or one sample against a fitted distribution named by --cdf",
        pipes: Pipe::Agg,
        operands: &[STREAM, SAMPLE],
        options: &[
            OptionSpec {
                short: None,
                long: "cdf",
                takes_value: true,
                doc: "norm, lognorm, expon, …: the scipy distribution fitted to the one sample",
            },
            ALT,
            NAN,
            DIGITS,
        ],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "f_oneway",
        doc: "one-way ANOVA over the samples (one per label, or the rows)",
        pipes: Pipe::Agg,
        operands: &[STREAM],
        options: &[NAN, DIGITS],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "kruskal",
        doc: "the Kruskal–Wallis H test over the samples (one per label, or the rows)",
        pipes: Pipe::Agg,
        operands: &[STREAM],
        options: &[NAN, DIGITS],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "shapiro",
        doc: "the Shapiro–Wilk test of normality over the sample",
        pipes: Pipe::Agg,
        operands: &[STREAM],
        options: &[NAN, DIGITS],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "chi2_contingency",
        doc: "the chi-square test of independence over a contingency table (the rows' numeric fields)",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[DIGITS],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue; dof)",
    },
    Synopsis {
        name: "fisher_exact",
        doc: "Fisher's exact test over a 2×2 table",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[ALT, DIGITS],
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "pearsonr",
        doc: "Pearson's r with its p-value over two numeric columns (x, then y)",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: TEST_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "spearmanr",
        doc: "Spearman's rho with its p-value over two numeric columns",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: TEST_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "kendalltau",
        doc: "Kendall's tau with its p-value over two numeric columns",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: TEST_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "record %(statistic; pvalue)",
    },
    Synopsis {
        name: "linregress",
        doc: "the least-squares line of y on x over two numeric columns",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: TEST_OPTIONS,
        input: InputSlot::Operand(0),
        returns: "record %(slope; intercept; rvalue; pvalue; stderr)",
    },
    Synopsis {
        name: "linkage",
        doc: "hierarchical clustering of the rows: one record per merge, a and b the labels joined",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[METHOD, METRIC, PAIRS, DIGITS],
        input: InputSlot::Operand(0),
        returns: "list of records %(a; b; distance; size)",
    },
    Synopsis {
        name: "fcluster",
        doc: "the rows cut into -t clusters of the hierarchy, numbered in the order the clusters first appear",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[
            OptionSpec {
                short: Some("-t"),
                long: "t",
                takes_value: true,
                doc: "the number of clusters (2 by default)",
            },
            METHOD,
            METRIC,
            PAIRS,
        ],
        input: InputSlot::Operand(0),
        returns: "list of records %(label; cluster)",
    },
    Synopsis {
        name: "leaves",
        doc: "the row labels in the order the dendrogram draws them",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[METHOD, METRIC, PAIRS],
        input: InputSlot::Operand(0),
        returns: "list of labels",
    },
];

static PY: PyGlue = PyGlue::new(GLUE, "quarb_scipy.py", "quarb_scipy");

/// The rest of the package, by reflection.
static REFLECT: Reflective = Reflective::new("scipy", &[], "ndarray", false);

impl CommandLibrary for Scipy {
    fn prefix(&self) -> &str {
        "sp"
    }

    fn commands(&self) -> &[Synopsis] {
        COMMANDS
    }

    fn requirements(&self) -> &[&str] {
        &["scipy"]
    }

    fn available(&self) -> Result<(), String> {
        quarb_pyvalue::importable(&["numpy", "scipy"], "scipy")
    }

    fn resolve(&self, name: &str) -> Result<&Synopsis, String> {
        match self.synopsis(name) {
            Some(s) => Ok(s),
            None => REFLECT.resolve(name),
        }
    }

    fn call(&self, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        match self.synopsis(name) {
            Some(syn) => PY.call(syn, name, call),
            None => {
                let syn = REFLECT.resolve(name).map_err(LibFailure::Unknown)?;
                REFLECT.call(syn, name, call)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(name: &str, input: Value, options: Vec<(&str, Value)>) -> Result<Value, LibFailure> {
        Scipy.call(
            name,
            LibCall {
                agg: true,
                input: Some(input),
                operands: vec![],
                options: options
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), Some(v)))
                    .collect(),
            },
        )
    }

    fn rec(fields: &[(&str, Value)]) -> Value {
        Value::Record(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn field(v: &Value, name: &str) -> Value {
        let Value::Record(f) = v else {
            panic!("record, got {v:?}")
        };
        f.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap()
    }

    #[test]
    fn tests_return_statistic_and_pvalue() {
        if Scipy.available().is_err() {
            eprintln!("scipy not importable; skipping");
            return;
        }
        // Two samples by label, in the order the labels first appear.
        let rows = Value::list(vec![
            rec(&[("g", Value::Str("a".into())), ("v", Value::Int(1))]),
            rec(&[("g", Value::Str("a".into())), ("v", Value::Int(2))]),
            rec(&[("g", Value::Str("a".into())), ("v", Value::Int(3))]),
            rec(&[("g", Value::Str("b".into())), ("v", Value::Int(10))]),
            rec(&[("g", Value::Str("b".into())), ("v", Value::Int(11))]),
            rec(&[("g", Value::Str("b".into())), ("v", Value::Int(12))]),
        ]);
        let r = agg("ttest", rows.clone(), vec![("digits", Value::Int(4))]).unwrap();
        assert_eq!(field(&r, "statistic"), Value::Float(-11.02));
        assert_eq!(field(&r, "df"), Value::Float(4.0));
        let r = agg("mannwhitneyu", rows, vec![]).unwrap();
        assert_eq!(field(&r, "statistic"), Value::Float(0.0));
        // A hole refuses by name.
        let holed = Value::list(vec![
            rec(&[("g", Value::Str("a".into())), ("v", Value::Int(1))]),
            rec(&[("g", Value::Str("b".into())), ("v", Value::Null)]),
        ]);
        let e = agg("kruskal", holed, vec![]).unwrap_err();
        assert!(
            e.message().contains("hole") || e.message().contains("one label"),
            "{}",
            e.message()
        );
    }

    #[test]
    fn two_columns_and_a_line() {
        if Scipy.available().is_err() {
            return;
        }
        let xy = Value::list(
            (1..=5)
                .map(|i| rec(&[("x", Value::Int(i)), ("y", Value::Int(2 * i + 1))]))
                .collect(),
        );
        let r = agg("linregress", xy.clone(), vec![("digits", Value::Int(6))]).unwrap();
        assert_eq!(field(&r, "slope"), Value::Float(2.0));
        assert_eq!(field(&r, "intercept"), Value::Float(1.0));
        let r = agg("spearmanr", xy, vec![("digits", Value::Int(6))]).unwrap();
        assert_eq!(field(&r, "statistic"), Value::Float(1.0));
    }

    #[test]
    fn a_hierarchy_over_labelled_rows_and_over_pairs() {
        if Scipy.available().is_err() {
            return;
        }
        let rows = Value::list(vec![
            rec(&[
                ("name", Value::Str("p".into())),
                ("x", Value::Float(0.0)),
                ("y", Value::Float(0.0)),
            ]),
            rec(&[
                ("name", Value::Str("q".into())),
                ("x", Value::Float(0.1)),
                ("y", Value::Float(0.0)),
            ]),
            rec(&[
                ("name", Value::Str("r".into())),
                ("x", Value::Float(5.0)),
                ("y", Value::Float(5.0)),
            ]),
        ]);
        let Value::List(merges) =
            agg("linkage", rows.clone(), vec![("digits", Value::Int(3))]).unwrap()
        else {
            panic!()
        };
        assert_eq!(merges.len(), 2);
        assert_eq!(
            field(&merges[0], "a"),
            Value::list(vec![Value::Str("p".into())])
        );
        assert_eq!(
            field(&merges[0], "b"),
            Value::list(vec![Value::Str("q".into())])
        );
        assert_eq!(field(&merges[0], "distance"), Value::Float(0.1));
        let flat = agg("fcluster", rows.clone(), vec![("t", Value::Int(2))]).unwrap();
        let Value::List(flat) = flat else { panic!() };
        assert_eq!(field(&flat[0], "cluster"), field(&flat[1], "cluster"));
        assert_ne!(field(&flat[0], "cluster"), field(&flat[2], "cluster"));
        let pairs = Value::list(vec![
            rec(&[
                ("a", Value::Str("p".into())),
                ("b", Value::Str("q".into())),
                ("d", Value::Float(0.1)),
            ]),
            rec(&[
                ("a", Value::Str("p".into())),
                ("b", Value::Str("r".into())),
                ("d", Value::Float(7.0)),
            ]),
            rec(&[
                ("a", Value::Str("q".into())),
                ("b", Value::Str("r".into())),
                ("d", Value::Float(7.0)),
            ]),
        ]);
        let order = agg("leaves", pairs, vec![("pairs", Value::Bool(true))]).unwrap();
        assert_eq!(
            order,
            Value::list(vec![
                Value::Str("r".into()),
                Value::Str("p".into()),
                Value::Str("q".into())
            ])
        );
    }
}
