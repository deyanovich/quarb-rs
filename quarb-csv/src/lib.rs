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
//! - `:::columns` on the root lists the column names; `:::n-rows`
//!   counts records; `:::n-fields` on a row counts its cells;
//!   `:::delimiter` and `:::decimal` on the root say what was
//!   decided.
//!
//! The first record must be the header. Parsing is strict about
//! ragged rows (a record with the wrong field count is an error).
//!
//! The delimiter is decided, never sniffed: `?delimiter=` on the
//! mount, else an Excel `sep=X` first line (a declaration, not a
//! row), else the extension's (tab for `.tsv`, comma otherwise). A
//! header that never contains the decided delimiter but another
//! candidate throughout is refused, naming the option. Numbers in
//! cells read in the canonical form; a source that writes decimal
//! commas declares it (`?decimal=comma`), and its cells with a
//! reading under that convention arrive in canonical form.

use quarb::number::Decimal;
use quarb::{AstAdapter, NodeId, Value};

#[derive(Debug, thiserror::Error)]
pub enum CsvError {
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error(
        "the header has no {decided:?} but {found:?} throughout; say the delimiter with \
         ?delimiter={name}"
    )]
    Delimiter {
        decided: char,
        found: char,
        name: &'static str,
    },
    #[error("unknown delimiter {0:?} (a character, or comma, semicolon, tab, pipe)")]
    DelimiterLabel(String),
    #[error("unknown decimal convention {0:?} (point or comma)")]
    DecimalLabel(String),
}

/// How a delimited file is read: the options a mount carries
/// (`?delimiter=`, `?decimal=`) and the extension's default.
#[derive(Clone, Copy, Debug, Default)]
pub struct CsvOptions {
    /// `?delimiter=`, decided before anything is read.
    pub delimiter: Option<u8>,
    /// `?decimal=`; `point` when the source says nothing.
    pub decimal: Decimal,
}

impl CsvOptions {
    /// Read the two options off their labels: a delimiter is one
    /// character or `comma` / `semicolon` / `tab` / `pipe`; the
    /// decimal convention is `point` or `comma`.
    pub fn with_delimiter_label(mut self, label: &str) -> Result<Self, CsvError> {
        self.delimiter = Some(delimiter_label(label)?);
        Ok(self)
    }

    pub fn with_decimal_label(mut self, label: &str) -> Result<Self, CsvError> {
        self.decimal =
            Decimal::from_label(label).ok_or_else(|| CsvError::DecimalLabel(label.into()))?;
        Ok(self)
    }
}

/// A delimiter by its label.
pub fn delimiter_label(label: &str) -> Result<u8, CsvError> {
    match label {
        "comma" => Ok(b','),
        "semicolon" => Ok(b';'),
        "tab" => Ok(b'\t'),
        "pipe" => Ok(b'|'),
        _ => {
            let mut chars = label.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii() && c != '"' && c != '\n' => Ok(c as u8),
                _ => Err(CsvError::DelimiterLabel(label.into())),
            }
        }
    }
}

fn delimiter_name(d: u8) -> &'static str {
    match d {
        b',' => "comma",
        b';' => "semicolon",
        b'\t' => "tab",
        b'|' => "pipe",
        _ => "CHAR",
    }
}

/// A Quarb adapter over a parsed CSV table.
pub struct CsvAdapter {
    /// Column names, from the header record.
    columns: Vec<String>,
    /// One entry per record, each with one cell per column.
    rows: Vec<Vec<String>>,
    /// The delimiter as decided.
    delimiter: u8,
    /// The decimal convention the source declared.
    decimal: Decimal,
}

impl CsvAdapter {
    /// Parse comma-separated `text` (header row required) and build
    /// the adapter.
    pub fn parse(text: &str) -> Result<Self, csv::Error> {
        Self::parse_with_delimiter(text, b',')
    }

    /// Parse with an explicit field delimiter (e.g. `b'\t'` for TSV).
    pub fn parse_with_delimiter(text: &str, delimiter: u8) -> Result<Self, csv::Error> {
        match Self::parse_declared(text, delimiter, CsvOptions::default()) {
            Ok(a) => Ok(a),
            Err(CsvError::Csv(e)) => Err(e),
            // Without the option the refusal has nowhere to go:
            // read as the caller asked, and let the ragged rows say
            // so (the message before this crate decided anything).
            Err(_) => Self::read(text, delimiter, Decimal::Point),
        }
    }

    /// Parse under the decided delimiter: `?delimiter=` on the
    /// mount, else an Excel `sep=X` first line, else `by_extension`
    /// (tab for `.tsv`, comma otherwise). A header that never holds
    /// the decided delimiter while another candidate runs through
    /// every record is refused, naming the option.
    pub fn parse_declared(
        text: &str,
        by_extension: u8,
        options: CsvOptions,
    ) -> Result<Self, CsvError> {
        let (text, declared) = match sep_line(text) {
            Some((d, rest)) => (rest, Some(d)),
            None => (text, None),
        };
        let delimiter = options.delimiter.or(declared).unwrap_or(by_extension);
        let header = text.lines().next().unwrap_or("");
        if !header.as_bytes().contains(&delimiter) {
            for candidate in [b',', b';', b'\t', b'|'] {
                if candidate != delimiter
                    && header.as_bytes().contains(&candidate)
                    && text
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .all(|l| l.as_bytes().contains(&candidate))
                {
                    return Err(CsvError::Delimiter {
                        decided: delimiter as char,
                        found: candidate as char,
                        name: delimiter_name(candidate),
                    });
                }
            }
        }
        Ok(Self::read(text, delimiter, options.decimal)?)
    }

    fn read(text: &str, delimiter: u8, decimal: Decimal) -> Result<Self, csv::Error> {
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
        Ok(CsvAdapter {
            columns,
            rows,
            delimiter,
            decimal,
        })
    }

    /// A locator path to `node`, like `/row[3]`, for rendering.
    pub fn locator(&self, node: NodeId) -> String {
        if node.0 == 0 {
            "/".to_string()
        } else {
            format!("/row[{}]", node.0)
        }
    }

    /// The cell's text as the engine reads it: canonical numeric
    /// form under `?decimal=comma` when the cell reads as a number
    /// that way, the text as written otherwise.
    fn canonical(&self, cell: &str) -> String {
        if self.decimal == Decimal::Comma
            && let Some(c) = quarb::number::canonical(cell.trim(), Decimal::Comma)
        {
            return c;
        }
        cell.to_string()
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
    /// The arena knows how many nodes a step reaches; nothing is read.
    fn estimate(&self, scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        quarb::adapter::arena_estimate(self, scope, plan, quarb::Latency::Memory, None)
    }

    /// This source defines no references to resolve.
    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// In memory: an unscoped reverse resolution or value match may
    /// walk the arbor.
    fn walks_for_references(&self) -> bool {
        true
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

    /// A column cell, by header name. Empty cells are missing. Under
    /// a declared decimal convention a cell with a numeric reading
    /// arrives in canonical form, so every reading of it downstream
    /// (an aggregate, a comparison, a pushdown) is the number the
    /// source meant.
    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        self.cell(node, name).map(|c| Value::Str(self.canonical(c)))
    }

    /// A row's cells by header, in column order (an empty cell
    /// missing): the record a row stands for where one is wanted
    /// (`@| #`, `| json`).
    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        if node.0 == 0 {
            return Vec::new();
        }
        self.columns
            .iter()
            .filter_map(|c| {
                self.cell(node, c)
                    .map(|v| (c.clone(), Value::Str(self.canonical(v))))
            })
            .collect()
    }

    /// `:::columns` (root), `:::n-rows` (root), `:::delimiter` and
    /// `:::decimal` (root, as decided), `:::n-fields` (row), and any
    /// column by name.
    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            match key {
                "delimiter" => return Some(Value::Str((self.delimiter as char).to_string())),
                "decimal" => {
                    return Some(Value::Str(
                        match self.decimal {
                            Decimal::Point => "point",
                            Decimal::Comma => "comma",
                        }
                        .into(),
                    ));
                }
                _ => {}
            }
            return match key {
                "columns" => Some(Value::list(
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

/// An Excel `sep=X` first line: the declaration, and the text after
/// it. Not a row.
fn sep_line(text: &str) -> Option<(u8, &str)> {
    let rest = text.strip_prefix("sep=")?;
    let mut chars = rest.chars();
    let d = chars.next()?;
    let after = chars.as_str();
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))?;
    (d.is_ascii() && d != '\n').then_some((d as u8, after))
}
