//! Spreadsheet adapter for the Quarb query engine.
//!
//! Workbooks (`.xlsx`, `.xls`, `.ods`) map onto the relational
//! model's shape: each sheet is a table, its first row the column
//! headers, every following row a node named by its spreadsheet
//! row number (2, 3, ...), cells as properties typed by what the
//! cell holds (numbers arrive numeric, not stringly). No keys, no
//! foreign keys — a spreadsheet declares none — but the `~>` hint
//! form and a `--refs` document work as on any schemaless source.
//!
//! Headerless or ragged sheets get positional column names
//! (`c1`, `c2`, ...) where a header cell is empty or duplicated.
//! Everything loads eagerly (workbooks are local files); the
//! adapter never writes.

use calamine::{Data, Reader};
use quarb::{AstAdapter, NodeId, Value};
use quarb_relational::{RelationalModel, RowSpec, TableSpec};

/// An error opening a workbook.
#[derive(Debug, thiserror::Error)]
pub enum XlsxError {
    #[error("xlsx: {0}")]
    Calamine(String),
}

/// A workbook, exposed as an arbor of sheets and rows.
pub struct XlsxAdapter {
    model: RelationalModel,
}

fn cell_value(d: &Data) -> Value {
    match d {
        Data::Empty => Value::Null,
        Data::Int(n) => Value::Int(*n),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9e15 {
                Value::Int(*f as i64)
            } else {
                Value::Float(*f)
            }
        }
        Data::Bool(b) => Value::Bool(*b),
        Data::String(s) => Value::Str(s.clone()),
        Data::DateTime(dt) => {
            // Excel stores a date/time as a serial day count in the
            // 1900 date system (serial 0 = 1899-12-30, serial 25569 =
            // the Unix epoch). Shift and scale to a UTC instant so the
            // cell reads and compares as a date, rather than surfacing
            // the raw serial as an opaque number. Spreadsheet dates
            // carry no timezone, so the offset is absent.
            let unix = (dt.as_f64() - 25569.0) * 86_400.0;
            Value::Instant {
                secs: unix.floor() as i64,
                nanos: ((unix - unix.floor()) * 1e9) as u32,
                offset_min: None,
            }
        }
        other => Value::Str(other.to_string()),
    }
}

impl XlsxAdapter {
    /// Open a workbook; every sheet becomes a table.
    pub fn open(path: &std::path::Path) -> Result<Self, XlsxError> {
        let mut wb =
            calamine::open_workbook_auto(path).map_err(|e| XlsxError::Calamine(e.to_string()))?;
        let mut tables = Vec::new();
        let sheet_names = wb.sheet_names().to_vec();
        for sheet in sheet_names {
            let Ok(range) = wb.worksheet_range(&sheet) else {
                continue;
            };
            let mut rows_iter = range.rows();
            // Headers from the first row; empty/duplicate cells
            // become positional names.
            let mut columns: Vec<String> = Vec::new();
            if let Some(head) = rows_iter.next() {
                for (i, c) in head.iter().enumerate() {
                    let raw = match c {
                        Data::String(s) => s.trim().to_string(),
                        Data::Empty => String::new(),
                        other => other.to_string(),
                    };
                    let name = if raw.is_empty() || columns.contains(&raw) {
                        format!("c{}", i + 1)
                    } else {
                        raw
                    };
                    columns.push(name);
                }
            }
            let mut rows = Vec::new();
            for (i, r) in rows_iter.enumerate() {
                let mut values: Vec<Value> = r.iter().map(cell_value).collect();
                values.resize(columns.len(), Value::Null);
                rows.push(RowSpec {
                    // Spreadsheet row numbers: headers are row 1.
                    rowid: i as i64 + 2,
                    values,
                });
            }
            tables.push((
                TableSpec {
                    name: sheet,
                    columns,
                    pk: None,
                    fks: Vec::new(),
                },
                rows,
            ));
        }
        Ok(XlsxAdapter {
            model: RelationalModel::build(tables),
        })
    }

    /// A human-readable locator: `/sheet/rownum`.
    pub fn locator(&self, node: NodeId) -> String {
        self.model.locator(node)
    }
}

impl AstAdapter for XlsxAdapter {
    fn root(&self) -> NodeId {
        self.model.root()
    }
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.model.children(node)
    }
    fn name(&self, node: NodeId) -> Option<String> {
        self.model.name(node)
    }
    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.model.parent(node)
    }
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.model.property(node, name)
    }
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        self.model.metadata(node, key)
    }
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        self.model.resolve(node, property, hint)
    }
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.links(node)
    }
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        self.model.backlinks(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{ExcelDateTime, ExcelDateTimeType};

    #[test]
    fn datetime_cell_mints_instant() {
        // Excel serial 45292 is 2024-01-01 in the 1900 date system;
        // it must surface as an instant (which displays as the bare
        // date), not the opaque serial string "45292".
        let cell = Data::DateTime(ExcelDateTime::new(
            45292.0,
            ExcelDateTimeType::DateTime,
            false,
        ));
        assert_eq!(
            cell_value(&cell),
            Value::Instant {
                secs: 1_704_067_200,
                nanos: 0,
                offset_min: None,
            }
        );
    }
}
