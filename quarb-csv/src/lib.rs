//! CSV adapter for Quarb.
//!
//! Maps tabular data onto the arbor model the way a DataFrame maps
//! onto a table: one node per record, one property per column.
//!
//! - The unnamed root has one `row` child per record, in file
//!   order, so `/row` iterates the table, `/row[5]` is the fifth
//!   record, and `//row @| count` is the row count.
//! - Columns are properties, named by the header: `::age` reads the
//!   `age` cell. Numeric-looking cells participate in comparisons
//!   and arithmetic via the numeric reading (`::price * ::qty`).
//! - An empty cell is a *missing value*: the property projects as
//!   null, which numeric aggregates skip and null propagation
//!   respects — pandas' `NaN` behavior without a sentinel.
//! - Rows are leaves; there are no traits and no crosslinks.
//! - `;;;columns` on the root lists the column names; `;;;n-rows`
//!   counts records; `;;;n-fields` on a row counts its cells.
//!
//! The first record must be the header. Parsing is strict about
//! ragged rows (a record with the wrong field count is an error).

use quarb::{AstAdapter, NodeId, Value};

/// A Quarb adapter over a parsed CSV table.
pub struct CsvAdapter {
    /// Column names, from the header record.
    columns: Vec<String>,
    /// One entry per record, each with one cell per column.
    rows: Vec<Vec<String>>,
}

impl CsvAdapter {
    /// Parse comma-separated `text` (header row required) and build
    /// the adapter.
    pub fn parse(text: &str) -> Result<Self, csv::Error> {
        Self::parse_with_delimiter(text, b',')
    }

    /// Parse with an explicit field delimiter (e.g. `b'\t'` for TSV).
    pub fn parse_with_delimiter(text: &str, delimiter: u8) -> Result<Self, csv::Error> {
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .from_reader(text.as_bytes());
        let columns: Vec<String> = reader
            .headers()?
            .iter()
            .map(|h| h.trim().to_string())
            .collect();
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record?;
            rows.push(record.iter().map(|c| c.to_string()).collect());
        }
        Ok(CsvAdapter { columns, rows })
    }

    /// A locator path to `node`, like `/row[3]`, for rendering.
    pub fn locator(&self, node: NodeId) -> String {
        if node.0 == 0 {
            "/".to_string()
        } else {
            format!("/row[{}]", node.0)
        }
    }

    fn cell(&self, node: NodeId, column: &str) -> Option<&str> {
        // The root (id 0) has no cells; guard the 1-based index so
        // it cannot underflow when property() reaches here at root.
        let row = self.rows.get((node.0 as usize).checked_sub(1)?)?;
        let idx = self.columns.iter().position(|c| c == column)?;
        // An empty cell is a missing value, not an empty string.
        row.get(idx).map(|c| c.as_str()).filter(|c| !c.is_empty())
    }
}

impl AstAdapter for CsvAdapter {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            (1..=self.rows.len() as u64).map(NodeId).collect()
        } else {
            Vec::new()
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        if node.0 == 0 {
            None
        } else {
            Some("row".to_string())
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        if node.0 == 0 { None } else { Some(NodeId(0)) }
    }

    /// A column cell, by header name. Empty cells are missing.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.cell(node, name).map(|c| Value::Str(c.to_string()))
    }

    /// `;;;columns` (root), `;;;n-rows` (root), `;;;n-fields` (row),
    /// and any column by name.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            return match key {
                "columns" => Some(Value::List(
                    self.columns.iter().map(|c| Value::Str(c.clone())).collect(),
                )),
                "n-rows" => Some(Value::Int(self.rows.len() as i64)),
                _ => None,
            };
        }
        match key {
            "n-fields" => self
                .rows
                .get(node.0 as usize - 1)
                .map(|r| Value::Int(r.len() as i64)),
            other => self.property(node, other),
        }
    }
}
