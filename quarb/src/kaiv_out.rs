//! kaiv emission of result values — the `| kaiv` / `@| kaiv`
//! output stages, and the shared machinery behind qua's `--kaiv`
//! emitter. The stages see values only, so the documents they
//! build carry no provenance; qua's emitter, which sees the
//! result nodes, layers per-row provenance over the same
//! placement rules (records open namespaces, lists open arrays,
//! typed leaves keep their units).

use crate::Value;
use std::collections::HashSet;

/// Sanitize a locator or field name into kaiv's identifier charset:
/// ASCII alphanumerics and `_` pass through; each run of any other
/// characters (including `.` and `-`) collapses to one `-`.
pub fn ident_of(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "value".to_string()
    } else {
        trimmed
    }
}

/// A field id unique within its namespace: sanitization can collide
/// distinct field names ("a b" and "a-b" both become "a-b"); suffix
/// rather than abort.
pub fn unique_ident(field: &str, used: &mut HashSet<String>) -> String {
    let mut id = ident_of(field);
    if !used.insert(id.clone()) {
        let mut k = 2;
        loop {
            let candidate = format!("{id}-{k}");
            if used.insert(candidate.clone()) {
                id = candidate;
                break;
            }
            k += 1;
        }
    }
    id
}

/// The kaiv type annotation and payload for one scalar. Lists and
/// records reach here only when they could not open a namespace
/// (empty, or nested in a list) and ride as JSON text.
fn kaiv_scalar(v: &Value) -> (&'static str, String) {
    match v {
        Value::Null => ("null", String::new()),
        Value::Bool(b) => ("bool", b.to_string()),
        Value::Int(n) => ("int", n.to_string()),
        Value::Float(f) => ("float", f.to_string()),
        Value::Str(s) => ("str", s.clone()),
        Value::List(_) | Value::Record(_) => ("str", v.to_json()),
        // The fallback route: instants normally emit typed
        // (std/time, in `kaiv_leaf`); durations have no kaiv type
        // yet and quantities normally emit unit-annotated. All ride
        // as text here.
        Value::Instant { .. } | Value::Duration { .. } | Value::Quantity { .. } => {
            ("str", v.to_string())
        }
    }
}

fn estr(e: kaiv::PipelineError) -> String {
    format!("emitting kaiv: {e}")
}

/// One typed leaf. Quantities emit unit-annotated (`!float:km`), in
/// their written unit so the authored form survives the loop;
/// durations on the seconds unit (a time-unit annotation mints a
/// duration at the re-mount, one ontology per dimension of time);
/// instants as their std/time type, so a re-mount re-mints them.
pub fn kaiv_leaf(
    b: &mut kaiv::KaivBuilder,
    namepath: &str,
    value: &Value,
    prov: &kaiv::Provenance,
) -> Result<(), String> {
    match value {
        Value::Quantity {
            value: bv,
            base,
            written,
        } => {
            let (v, u) = written.clone().unwrap_or((*bv, base.clone()));
            if b.leaf_with_unit(namepath, "float", Some(&u), &v.to_string(), Some(prov))
                .is_ok()
            {
                return Ok(());
            }
        }
        Value::Duration { secs, nanos } => {
            let v = *secs as f64 + *nanos as f64 / 1e9;
            if b.leaf_with_unit(namepath, "float", Some("s"), &v.to_string(), Some(prov))
                .is_ok()
            {
                return Ok(());
            }
        }
        Value::Instant {
            secs,
            nanos,
            offset_min,
        } => {
            let ty = if offset_min.is_some() {
                "std/time/datetime"
            } else if *nanos == 0 && secs.rem_euclid(86400) == 0 {
                "std/time/date"
            } else {
                "std/time/localdatetime"
            };
            b.declare_types("std/time").map_err(estr)?;
            if b.leaf(namepath, ty, &value.to_string(), Some(prov)).is_ok() {
                return Ok(());
            }
        }
        _ => {}
    }
    let (t, payload) = kaiv_scalar(value);
    if b.leaf(namepath, t, &payload, Some(prov)).is_err() {
        // Not flat-line representable: carry the JSON text.
        b.leaf(namepath, "str", &value.to_json(), Some(prov))
            .map_err(estr)?;
    }
    Ok(())
}

/// Place one field under `base`: a record opens the namespace
/// `base/id` and places its fields there; a list opens the array
/// `base/@id`, its scalar elements as `::n` leaves and its record
/// elements as `/n` namespaces; anything else is a leaf `base::id`.
/// An empty record or list, and a list nested in a list, ride as
/// JSON text — kaiv has no line for them.
pub fn kaiv_put(
    b: &mut kaiv::KaivBuilder,
    base: &str,
    field: &str,
    value: &Value,
    prov: &kaiv::Provenance,
    used: &mut HashSet<String>,
) -> Result<(), String> {
    let id = unique_ident(field, used);
    match value {
        Value::Record(fields) if !fields.is_empty() => {
            let sub = format!("{base}/{id}");
            let mut used = HashSet::new();
            for (k, v) in fields {
                kaiv_put(b, &sub, k, v, prov, &mut used)?;
            }
            Ok(())
        }
        Value::List(items) if !items.is_empty() => {
            let sub = format!("{base}/@{id}");
            for (n, item) in items.iter().enumerate() {
                match item {
                    Value::Record(fields) if !fields.is_empty() => {
                        let elem = format!("{sub}/{n}");
                        let mut used = HashSet::new();
                        for (k, v) in fields {
                            kaiv_put(b, &elem, k, v, prov, &mut used)?;
                        }
                    }
                    _ => kaiv_leaf(b, &format!("{sub}::{n}"), item, prov)?,
                }
            }
            Ok(())
        }
        _ => kaiv_leaf(b, &format!("{base}::{id}"), value, prov),
    }
}

/// The `@| kaiv` document: every value under `/@results`, a record
/// spreading as its namespace's fields, anything else as `::value`.
/// No provenance — the stage sees values, not their nodes; qua's
/// `--kaiv` is the provenance-rich emitter.
pub fn document(items: &[Value]) -> Result<String, String> {
    let mut b = kaiv::KaivBuilder::new();
    let prov = kaiv::Provenance::default();
    for (i, v) in items.iter().enumerate() {
        let base = format!("/@results/{i}");
        let mut used = HashSet::new();
        match v {
            Value::Record(fields) => {
                for (k, val) in fields {
                    kaiv_put(&mut b, &base, k, val, &prov, &mut used)?;
                }
            }
            other => kaiv_put(&mut b, &base, "value", other, &prov, &mut used)?,
        }
    }
    b.finish().map_err(estr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_places_records_and_scalars() {
        let items = vec![
            Value::Record(vec![
                ("name".into(), Value::Str("Ada".into())),
                ("tags".into(), Value::List(vec![Value::Int(1), Value::Int(2)])),
            ]),
            Value::Int(7),
        ];
        let doc = document(&items).unwrap();
        assert!(doc.starts_with(".!kaiv"));
        // The builder normalizes: the record opens the array
        // element inline, the nested list appends, the scalar
        // lands as `value` in the `/@results` section.
        assert!(doc.contains("+:=name=Ada\n"), "record entry: {doc}");
        assert!(doc.contains("/@results/0/@tags+=2\n"), "list: {doc}");
        assert!(doc.contains("value=7\n"), "scalar: {doc}");
    }

    #[test]
    fn document_keeps_units() {
        let items = vec![Value::Quantity {
            value: 3.5,
            base: "km".into(),
            written: None,
        }];
        let doc = document(&items).unwrap();
        assert!(doc.contains("!float:km"), "unit annotation kept: {doc}");
    }
}
