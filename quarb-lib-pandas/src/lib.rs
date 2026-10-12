//! `pd:` — pandas as a Quarb command library, by reflection (spec:
//! Command Libraries, "Reflected commands"): every function of the
//! package and every method of `DataFrame` and `Series` is a
//! command, named as pandas names it (`pd:describe`, `pd:corr`,
//! `pd:pivot_table`), its synopsis read from the method's signature
//! when the gate first meets the name. A method is a function whose
//! first parameter is the receiver, and the input fills it:
//! `@| pd:describe` is `DataFrame.describe(frame)`.
//!
//! The stream of records crosses as a frame with its first text
//! field as the index (the table rule's label), the other fields as
//! columns, text included; a list of numbers as a Series, a list of
//! lists as a frame of positions. A frame or series coming back is a
//! library value, kept as it is for the next command (`| pd:corr |
//! pd:round(2)`), printed as pandas prints it, read by `| @` as
//! records with the index as the first field and by `| %` as a record
//! of columns. The intermediate objects (`groupby`, `rolling`,
//! `expanding`, `ewm`, `resample`) take their method on the path:
//! `pd:groupby.mean(author)`, `pd:rolling.mean(3)`.

use quarb::Value;
use quarb::library::{CommandLibrary, LibCall, LibFailure, Synopsis};
use quarb_pyvalue::Reflective;

static REFLECT: Reflective =
    Reflective::new("pandas", &["pandas.DataFrame", "pandas.Series"], "frame", true);

/// The library.
pub struct Pandas;

impl CommandLibrary for Pandas {
    fn prefix(&self) -> &str {
        "pd"
    }

    fn commands(&self) -> &[Synopsis] {
        &[]
    }

    fn requirements(&self) -> &[&str] {
        &["pandas"]
    }

    fn available(&self) -> Result<(), String> {
        quarb_pyvalue::importable(&["pandas"], "pandas")
    }

    fn resolve(&self, name: &str) -> Result<&Synopsis, String> {
        REFLECT.resolve(name)
    }

    fn call(&self, name: &str, call: LibCall) -> Result<Value, LibFailure> {
        let syn = REFLECT.resolve(name).map_err(LibFailure::Unknown)?;
        REFLECT.call(syn, name, call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(fields: &[(&str, Value)]) -> Value {
        Value::Record(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn rows() -> Value {
        Value::list(vec![
            rec(&[("book", Value::Str("a".into())), ("x", Value::Int(1)), ("y", Value::Float(2.0))]),
            rec(&[("book", Value::Str("b".into())), ("x", Value::Int(3)), ("y", Value::Float(1.0))]),
            rec(&[("book", Value::Str("c".into())), ("x", Value::Int(5)), ("y", Value::Float(0.0))]),
        ])
    }

    fn call(name: &str, input: Value, operands: Vec<Value>, options: Vec<(&str, Value)>) -> Value {
        Pandas
            .call(
                name,
                LibCall {
                    agg: true,
                    input: Some(input),
                    operands,
                    options: options
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), Some(v)))
                        .collect(),
                },
            )
            .unwrap()
    }

    #[test]
    fn methods_chain_on_the_frame_and_export_by_rows_and_columns() {
        if Pandas.available().is_err() {
            eprintln!("pandas not importable; skipping");
            return;
        }
        // A frame comes back as a table: the index (the statistic)
        // its first column, the columns after it.
        let frame = call("describe", rows(), vec![], vec![]);
        let Value::Table(f) = &frame else { panic!("{frame:?}") };
        assert_eq!(f.kind(), "a frame 8×3");
        assert_eq!(f.header.as_deref(), Some(&["index".to_string(), "x".into(), "y".into()][..]));
        // The table is the input of the next method, crossing as its
        // rows, so the frame is made of it again with its index.
        let rounded = call("round", frame.clone(), vec![Value::Int(1)], vec![]);
        let Value::Table(r) = &rounded else { panic!("{rounded:?}") };
        let Value::Record(cols) = r.columns_value() else { panic!() };
        assert_eq!(cols[1].0, "x");
        let Value::List(x) = &cols[1].1 else { panic!() };
        assert_eq!(x[1], Value::Float(3.0));
        // Rows carry the index back as the first field, named.
        let corr = call("corr", rows(), vec![], vec![("method", Value::Str("spearman".into()))]);
        let Value::Table(c) = &corr else { panic!("{corr:?}") };
        let Value::List(recs) = c.rows_value() else { panic!() };
        let Value::Record(first) = &recs[0] else { panic!() };
        assert_eq!(first[0].0, "index");
        assert_eq!(first[0].1, Value::Str("x".into()));
        assert_eq!(first[2].1, Value::Float(-1.0));
        // The label is the index; a method reaching it by name.
        let idx = call("index.tolist", rows(), vec![], vec![]);
        assert_eq!(
            idx,
            Value::list(vec![Value::Str("a".into()), Value::Str("b".into()), Value::Str("c".into())])
        );
        // An intermediate object takes its method on the path.
        let g = call("groupby.sum", Value::list(vec![
            rec(&[("k", Value::Str("p".into())), ("v", Value::Int(1))]),
            rec(&[("k", Value::Str("q".into())), ("v", Value::Int(2))]),
            rec(&[("k", Value::Str("p".into())), ("v", Value::Int(4))]),
        ]), vec![Value::Str("k".into())], vec![]);
        let Value::Table(g) = &g else { panic!("{g:?}") };
        let Value::List(sums) = g.rows_value() else { panic!() };
        assert_eq!(sums[0], rec(&[("k", Value::Str("p".into())), ("v", Value::Int(5))]));
        // A scalar comes back as a number.
        assert_eq!(call("x.mean", rows(), vec![], vec![]), Value::Float(3.0));
    }
}
