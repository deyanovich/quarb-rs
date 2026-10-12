//! `sklearn:` — scikit-learn as a Quarb command library: principal
//! components, k-means with its silhouette, tf-idf and a topic model
//! over the stream after `@|` (spec: Command Libraries, "The stream
//! as a table"), and every other estimator by reflection
//! (`sklearn:cluster.KMeans.fit_predict(3; --random-state=0)`,
//! `sklearn:decomposition.PCA.fit.explained_variance_ratio_(-n=2)`;
//! spec: "Reflected commands"): the class on the path is constructed
//! with the options its constructor declares, the method takes the
//! stream as its matrix, an attribute of the fitted result is read. The same table rule as `scipy:`: a list of records
//! is a table whose numeric fields are the columns and whose first
//! text field is the row's label, carried back on every row that
//! returns; a document for the text commands is a string, a list of
//! tokens, or a record whose `text` field (else its longest text
//! field) is the text and whose first other text field is the label. A hole (a column missing in one
//! record, a value with no numeric reading) refuses the command and
//! is named.
//!
//! The wrapper speaks scikit-learn's names (`n_components`, `random_state`,
//! `max_features`), so its documentation is scikit-learn's; the
//! results are rows the engine streams, one capsa per record.

use quarb::Value;
use quarb::library::{
    CommandLibrary, InputSlot, LibCall, LibFailure, OperandSpec, OptionSpec, Pipe, Synopsis,
};
use quarb_pyvalue::{PyGlue, Reflective};

const GLUE: &str = r#"
import contextlib, math, sys
import numpy as np

class Hole(ValueError):
    pass

def _as_list(x):
    return x if isinstance(x, list) else [x]

def _num(v):
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
    if digits is None or x == 0 or not math.isfinite(x):
        return x
    return round(x, int(digits) - 1 - int(math.floor(math.log10(abs(x)))))

def _finish(v, digits):
    if isinstance(v, dict):
        return {k: _finish(w, digits) for k, w in v.items()}
    if isinstance(v, list):
        return [_finish(w, digits) for w in v]
    if isinstance(v, np.floating):
        v = float(v)
    if isinstance(v, np.integer):
        v = int(v)
    if isinstance(v, float):
        return None if math.isnan(v) else _sig(v, digits)
    return v

def _table(x):
    x = _as_list(x)
    if not x:
        raise Hole("the stream is empty")
    if isinstance(x[0], dict):
        cols, labels, rows = None, [], []
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
                    raise Hole(f"row {label or i}: field '{k}' is null (a hole)")
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
        if len({len(r) for r in rows}) != 1:
            raise Hole("the rows differ in length")
        return list(range(len(rows))), [str(j) for j in range(len(rows[0]))], np.array(rows, dtype=float)
    raise Hole("a table is a list of records or of lists")

def _documents(x):
    """(labels, texts): a document is a string, a list of tokens, or a
    record whose first text field is the label and last the text."""
    x = _as_list(x)
    labels, texts = [], []
    for i, d in enumerate(x):
        if isinstance(d, str):
            labels.append(i); texts.append(d)
        elif isinstance(d, list):
            labels.append(i); texts.append(" ".join(str(t) for t in d))
        elif isinstance(d, dict):
            # The text is the `text` field when the record has one (a
            # node crosses with it), else a list field (tokens), else
            # the longest text field; the label is the first other
            # text field.
            key = None
            if isinstance(d.get("text"), (str, list)):
                key = "text"
            else:
                lists = [k for k, v in d.items() if isinstance(v, list)]
                strs = sorted((k for k, v in d.items() if isinstance(v, str)), key=lambda k: -len(d[k]))
                key = lists[-1] if lists else (strs[0] if strs else None)
            if key is None:
                raise Hole(f"row {i}: a document record needs a text field")
            body = d[key]
            label = next((v for k, v in d.items() if k != key and isinstance(v, str)), i)
            labels.append(label)
            texts.append(" ".join(str(t) for t in body) if isinstance(body, list) else body)
        else:
            raise Hole(f"item {i} is no document")
    if not texts:
        raise Hole("the stream is empty")
    return labels, texts

def _scaled(m, scale):
    if not scale:
        return m
    from sklearn.preprocessing import StandardScaler
    return StandardScaler().fit_transform(m)

# --- rows -------------------------------------------------------------

def pca(x, n_components=2, scale=False, variance=False):
    from sklearn.decomposition import PCA
    labels, cols, m = _table(x)
    n = int(n_components)
    p = PCA(n_components=n)
    coords = p.fit_transform(_scaled(m, scale))
    if variance:
        return [float(v) for v in p.explained_variance_ratio_]
    return [dict([("label", l)] + [(f"pc{j + 1}", float(c)) for j, c in enumerate(row)])
            for l, row in zip(labels, coords)]

def _renumber(assign):
    """Cluster numbers in the order the clusters first appear, from 1."""
    seen = {}
    return [seen.setdefault(int(c), len(seen) + 1) for c in assign]

def kmeans(x, n_clusters=2, random_state=0, scale=False):
    from sklearn.cluster import KMeans
    labels, cols, m = _table(x)
    km = KMeans(n_clusters=int(n_clusters), random_state=int(random_state), n_init=10)
    assign = _renumber(km.fit_predict(_scaled(m, scale)))
    return [{"label": l, "cluster": c} for l, c in zip(labels, assign)]

def silhouette(x, n_clusters=2, random_state=0, scale=False):
    from sklearn.cluster import KMeans
    from sklearn.metrics import silhouette_score
    labels, cols, m = _table(x)
    m = _scaled(m, scale)
    km = KMeans(n_clusters=int(n_clusters), random_state=int(random_state), n_init=10)
    return float(silhouette_score(m, km.fit_predict(m)))

# --- documents ----------------------------------------------------------

def _vectorizer(max_features, min_df, max_df, stop_words):
    from sklearn.feature_extraction.text import TfidfVectorizer
    return TfidfVectorizer(max_features=None if max_features is None else int(max_features),
                           min_df=int(min_df), max_df=float(max_df), stop_words=stop_words or None,
                           lowercase=True)

def tfidf(x, top=5, max_features=None, min_df=1, max_df=1.0, stop_words=None):
    labels, texts = _documents(x)
    v = _vectorizer(max_features, min_df, max_df, stop_words)
    m = v.fit_transform(texts)
    terms = v.get_feature_names_out()
    out = []
    for l, row in zip(labels, m):
        row = row.toarray().ravel()
        order = np.argsort(-row, kind="stable")[: int(top)]
        for j in order:
            if row[j] > 0:
                out.append({"label": l, "term": str(terms[j]), "weight": float(row[j])})
    return out

def nmf(x, n_components=5, top=8, documents=False, random_state=0, max_features=None, min_df=1, max_df=1.0, stop_words=None):
    from sklearn.decomposition import NMF
    labels, texts = _documents(x)
    v = _vectorizer(max_features, min_df, max_df, stop_words)
    m = v.fit_transform(texts)
    terms = v.get_feature_names_out()
    model = NMF(n_components=int(n_components), random_state=int(random_state), init="nndsvda", max_iter=500)
    w = model.fit_transform(m)
    if documents:
        return [{"label": l, "topic": int(np.argmax(row)) + 1, "weight": float(row.max())}
                for l, row in zip(labels, w)]
    out = []
    for t, comp in enumerate(model.components_):
        order = np.argsort(-comp, kind="stable")[: int(top)]
        out.append({"topic": t + 1, "terms": [str(terms[j]) for j in order]})
    return out

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
pub struct Sklearn;

const DIGITS: OptionSpec = OptionSpec {
    short: None,
    long: "digits",
    takes_value: true,
    doc: "round every float in the result to this many significant digits",
};
const SCALE: OptionSpec = OptionSpec {
    short: None,
    long: "scale",
    takes_value: false,
    doc: "standardize every column first (StandardScaler)",
};
const SEED: OptionSpec = OptionSpec {
    short: None,
    long: "random-state",
    takes_value: true,
    doc: "the seed (0 by default), so a run repeats",
};
const K: OptionSpec = OptionSpec {
    short: Some("-k"),
    long: "n-clusters",
    takes_value: true,
    doc: "the number of clusters (2 by default)",
};
const MAX_FEATURES: OptionSpec = OptionSpec {
    short: None,
    long: "max-features",
    takes_value: true,
    doc: "keep this many terms, the most frequent",
};
const MIN_DF: OptionSpec = OptionSpec {
    short: None,
    long: "min-df",
    takes_value: true,
    doc: "ignore a term in fewer documents than this (1 by default)",
};
const MAX_DF: OptionSpec = OptionSpec {
    short: None,
    long: "max-df",
    takes_value: true,
    doc: "ignore a term in more than this share of the documents (0.5: more than half), a stop list for any language",
};
const STOP: OptionSpec = OptionSpec {
    short: None,
    long: "stop-words",
    takes_value: true,
    doc: "english: drop scikit-learn's English stop list",
};
const TOP: OptionSpec = OptionSpec {
    short: None,
    long: "top",
    takes_value: true,
    doc: "this many terms per row",
};

const TABLE: OperandSpec = OperandSpec {
    name: "table",
    doc: "the rows (the input)",
    values: &[],
    required: true,
};
const DOCS: OperandSpec = OperandSpec {
    name: "documents",
    doc: "the documents (the input)",
    values: &[],
    required: true,
};

const COMMANDS: &[Synopsis] = &[
    Synopsis {
        name: "pca",
        doc: "the rows projected on their first -n principal components; --variance gives the explained-variance ratios instead",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[
            OptionSpec {
                short: Some("-n"),
                long: "n-components",
                takes_value: true,
                doc: "the components kept (2 by default)",
            },
            SCALE,
            OptionSpec {
                short: None,
                long: "variance",
                takes_value: false,
                doc: "return the explained-variance ratio per component",
            },
            DIGITS,
        ],
        input: InputSlot::Operand(0),
        returns: "list of records %(label; pc1; pc2; …), or the variance ratios",
    },
    Synopsis {
        name: "kmeans",
        doc: "the rows in -k clusters, numbered in the order the clusters first appear",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[K, SEED, SCALE],
        input: InputSlot::Operand(0),
        returns: "list of records %(label; cluster)",
    },
    Synopsis {
        name: "silhouette",
        doc: "the mean silhouette of a k-means partition into -k clusters",
        pipes: Pipe::Agg,
        operands: &[TABLE],
        options: &[K, SEED, SCALE, DIGITS],
        input: InputSlot::Operand(0),
        returns: "float",
    },
    Synopsis {
        name: "tfidf",
        doc: "the --top terms of every document by tf-idf weight",
        pipes: Pipe::Agg,
        operands: &[DOCS],
        options: &[TOP, MAX_FEATURES, MIN_DF, MAX_DF, STOP, DIGITS],
        input: InputSlot::Operand(0),
        returns: "list of records %(label; term; weight)",
    },
    Synopsis {
        name: "nmf",
        doc: "a topic model (non-negative matrix factorization over tf-idf): -n topics with their --top terms; --documents gives each document's strongest topic",
        pipes: Pipe::Agg,
        operands: &[DOCS],
        options: &[
            OptionSpec {
                short: Some("-n"),
                long: "n-components",
                takes_value: true,
                doc: "the number of topics (5 by default)",
            },
            TOP,
            OptionSpec {
                short: None,
                long: "documents",
                takes_value: false,
                doc: "one record per document: its strongest topic and weight",
            },
            SEED,
            MAX_FEATURES,
            MIN_DF,
            MAX_DF,
            STOP,
            DIGITS,
        ],
        input: InputSlot::Operand(0),
        returns: "list of records %(topic; terms), or %(label; topic; weight)",
    },
];

static PY: PyGlue = PyGlue::new(GLUE, "quarb_sklearn.py", "quarb_sklearn");

/// The rest of the package, by reflection.
static REFLECT: Reflective = Reflective::new("sklearn", &[], "ndarray", false);

impl CommandLibrary for Sklearn {
    fn prefix(&self) -> &str {
        "sklearn"
    }

    fn commands(&self) -> &[Synopsis] {
        COMMANDS
    }

    fn requirements(&self) -> &[&str] {
        &["scikit-learn"]
    }

    fn available(&self) -> Result<(), String> {
        quarb_pyvalue::importable(&["numpy", "sklearn"], "scikit-learn")
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
        Sklearn.call(
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

    fn rows() -> Value {
        Value::list(vec![
            rec(&[
                ("name", Value::Str("p".into())),
                ("x", Value::Float(0.0)),
                ("y", Value::Float(0.0)),
            ]),
            rec(&[
                ("name", Value::Str("q".into())),
                ("x", Value::Float(0.1)),
                ("y", Value::Float(0.1)),
            ]),
            rec(&[
                ("name", Value::Str("r".into())),
                ("x", Value::Float(5.0)),
                ("y", Value::Float(5.0)),
            ]),
            rec(&[
                ("name", Value::Str("s".into())),
                ("x", Value::Float(5.1)),
                ("y", Value::Float(4.9)),
            ]),
        ])
    }

    #[test]
    fn components_and_clusters_carry_the_label() {
        if Sklearn.available().is_err() {
            eprintln!("scikit-learn not importable; skipping");
            return;
        }
        let Value::List(pcs) = agg("pca", rows(), vec![("digits", Value::Int(3))]).unwrap() else {
            panic!()
        };
        assert_eq!(pcs.len(), 4);
        assert_eq!(field(&pcs[0], "label"), Value::Str("p".into()));
        assert!(matches!(field(&pcs[0], "pc1"), Value::Float(_)));
        let Value::List(ks) = agg("kmeans", rows(), vec![("n-clusters", Value::Int(2))]).unwrap()
        else {
            panic!()
        };
        assert_eq!(field(&ks[0], "cluster"), Value::Int(1));
        assert_eq!(field(&ks[1], "cluster"), Value::Int(1));
        assert_eq!(field(&ks[2], "cluster"), Value::Int(2));
        let Value::Float(s) = agg("silhouette", rows(), vec![]).unwrap() else {
            panic!()
        };
        assert!(s > 0.9, "{s}");
        let e = agg(
            "pca",
            Value::list(vec![
                rec(&[("x", Value::Int(1))]),
                rec(&[("y", Value::Int(1))]),
            ]),
            vec![],
        )
        .unwrap_err();
        assert!(e.message().contains("hole"), "{}", e.message());
    }

    #[test]
    fn documents_give_terms_and_topics() {
        if Sklearn.available().is_err() {
            return;
        }
        let docs = Value::list(vec![
            rec(&[
                ("book", Value::Str("a".into())),
                (
                    "text",
                    Value::Str("the ship sailed the sea and the sea was wide".into()),
                ),
            ]),
            rec(&[
                ("book", Value::Str("b".into())),
                (
                    "text",
                    Value::Str("the horse ran over the field and the field was green".into()),
                ),
            ]),
        ]);
        let Value::List(t) = agg(
            "tfidf",
            docs.clone(),
            vec![
                ("top", Value::Int(1)),
                ("stop-words", Value::Str("english".into())),
            ],
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(field(&t[0], "label"), Value::Str("a".into()));
        assert_eq!(field(&t[0], "term"), Value::Str("sea".into()));
        assert_eq!(field(&t[1], "term"), Value::Str("field".into()));
        let Value::List(topics) = agg(
            "nmf",
            docs,
            vec![
                ("n-components", Value::Int(2)),
                ("top", Value::Int(2)),
                ("stop-words", Value::Str("english".into())),
            ],
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(topics.len(), 2);
        assert_eq!(field(&topics[0], "topic"), Value::Int(1));
    }
}
