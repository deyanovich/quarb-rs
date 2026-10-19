//! `np:` — numpy as a Quarb command library, by reflection (spec:
//! Command Libraries, "Reflected commands"): every function of the
//! package and every method of `ndarray` is a command, named as
//! numpy names it (`np:mean`, `np:linalg.norm`, `np:percentile(50)`),
//! its synopsis read from the function's signature when the gate
//! first meets the name. A method is a function whose first
//! parameter is the receiver, and the input fills it.
//!
//! The stream crosses as an array: a list of numbers is a vector, a
//! list of lists a matrix, a list of records the matrix of its
//! numeric fields (the first text field, the label, is dropped: an
//! array has no names; `pd:` keeps them), instants `datetime64`,
//! durations `timedelta64`, null NaN. An array coming back is a
//! library value, kept as it is for the next command and read by
//! `| @` as nested lists; a numpy scalar crosses as a number.

use quarb::Value;
use quarb::library::{CommandLibrary, LibCall, LibFailure, Synopsis};
use quarb_pyvalue::Reflective;

static REFLECT: Reflective = Reflective::new("numpy", &["numpy.ndarray"], "ndarray", false);

/// The library.
pub struct Numpy;

impl CommandLibrary for Numpy {
    fn prefix(&self) -> &str {
        "np"
    }

    fn commands(&self) -> &[Synopsis] {
        &[]
    }

    fn requirements(&self) -> &[&str] {
        &["numpy"]
    }

    fn available(&self) -> Result<(), String> {
        quarb_pyvalue::importable(&["numpy"], "numpy")
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

    fn nums(v: &[i64]) -> Value {
        Value::list(v.iter().map(|&n| Value::Int(n)).collect())
    }

    fn agg(name: &str, input: Value, operands: Vec<Value>) -> Result<Value, LibFailure> {
        Numpy.call(
            name,
            LibCall {
                agg: true,
                input: Some(input),
                operands,
                options: vec![],
            },
        )
    }

    #[test]
    fn functions_and_methods_by_reflection() {
        if Numpy.available().is_err() {
            eprintln!("numpy not importable; skipping");
            return;
        }
        // A function of the package: the stream its first parameter.
        assert_eq!(
            agg("mean", nums(&[1, 2, 3, 6]), vec![]).unwrap(),
            Value::Float(3.0)
        );
        // A surplus operand fills the next positional parameter.
        assert_eq!(
            agg("percentile", nums(&[1, 2, 3, 4]), vec![Value::Int(50)]).unwrap(),
            Value::Float(2.5)
        );
        // A dotted name walks a submodule.
        assert_eq!(
            agg("linalg.norm", nums(&[3, 4]), vec![]).unwrap(),
            Value::Float(5.0)
        );
        // A method of ndarray, the input its receiver; the
        // two-dimensional array it returns comes back as a table
        // (an array, no header), read by its rows.
        let t = agg(
            "transpose",
            Value::list(vec![nums(&[1, 2, 3]), nums(&[4, 5, 6])]),
            vec![],
        )
        .unwrap();
        let Value::Table(f) = &t else { panic!("{t:?}") };
        assert!(
            f.header.is_none() && f.kind().starts_with("an array 3×2"),
            "{}",
            f.kind()
        );
        assert_eq!(
            f.rows_value(),
            Value::list(vec![nums(&[1, 4]), nums(&[2, 5]), nums(&[3, 6])])
        );
        // The synopsis comes from the signature.
        let syn = Numpy.resolve("percentile").unwrap();
        assert_eq!(syn.operands[0].name, "a");
        assert_eq!(syn.operands[1].name, "q");
        assert!(
            syn.options.iter().any(|o| o.long == "method"),
            "{:?}",
            syn.options
        );
        let e = Numpy.resolve("meen").unwrap_err();
        assert!(e.contains("numpy") && e.contains("mean"), "{e}");
    }
}
