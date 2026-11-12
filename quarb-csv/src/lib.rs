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
//! The first record must be the header. A ragged row is read, as
//! the spec says: a short one fills its tail with null, a long one
//! is cut at the header's width, and `:::n-fields` on the row says
//! how many fields it wrote.
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
    /// A target option the reader cannot take as written.
    #[error("{0}")]
    Option(String),
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
    #[error("reading the file: {0}")]
    Io(#[from] std::io::Error),
    /// The file is not the UTF-8 the stream reads: the whole
    /// reader's refusal, in its words.
    #[error(transparent)]
    Source(#[from] quarb::source::SourceError),
}

/// How many of a file's first lines decide whether it is written
/// under another delimiter.
const DELIMITER_LINES: usize = 32;

/// The delimiter a file is written under where the decided one never
/// stands in its header: a candidate that stands in the header and
/// in every one of the first lines that say anything. Both readers
/// ask this of the same first lines (ruling 2026-11-10): the whole
/// reader looked at every line and the stream at thirty-two, so one
/// of them could refuse a file the other read.
fn other_delimiter(body: &str, delimiter: u8) -> Option<u8> {
    let header = body.lines().next().unwrap_or("");
    if header.as_bytes().contains(&delimiter) {
        return None;
    }
    [b',', b';', b'\t', b'|'].into_iter().find(|&candidate| {
        candidate != delimiter
            && header.as_bytes().contains(&candidate)
            && body
                .lines()
                .take(DELIMITER_LINES)
                .filter(|l| !l.trim().is_empty())
                .all(|l| l.as_bytes().contains(&candidate))
    })
}

/// The offset of the first byte of `path` that is not UTF-8, read in
/// chunks; `None` for a file that is UTF-8 throughout.
fn first_invalid_utf8(path: &std::path::Path) -> std::io::Result<Option<usize>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 16];
    // the bytes of a sequence cut by the chunk's end, carried over
    let mut carried = 0usize;
    let mut consumed = 0usize;
    loop {
        let n = file.read(&mut buf[carried..])?;
        if n == 0 {
            // a sequence left unfinished at the end of the file
            return Ok((carried > 0).then_some(consumed));
        }
        let have = carried + n;
        match std::str::from_utf8(&buf[..have]) {
            Ok(_) => {
                consumed += have;
                carried = 0;
            }
            Err(e) => match e.error_len() {
                Some(_) => return Ok(Some(consumed + e.valid_up_to())),
                None => {
                    let good = e.valid_up_to();
                    buf.copy_within(good..have, 0);
                    consumed += good;
                    carried = have - good;
                }
            },
        }
    }
}

/// How a delimited file is read: the options a mount carries
/// (`?delimiter=`, `?decimal=`) and the extension's default.
#[derive(Clone, Debug, Default)]
pub struct CsvOptions {
    /// `?delimiter=`, decided before anything is read.
    pub delimiter: Option<u8>,
    /// `?decimal=`; `point` when the source says nothing.
    pub decimal: Decimal,
    /// `?null=NA,n/a`: the cell texts read as missing values, beside
    /// the empty cell (ruling 2026-11-08); none by default, so a `-`
    /// in a CSV stays text.
    pub nulls: Vec<String>,
}

impl CsvOptions {
    /// Read the two options off their labels: a delimiter is one
    /// character or `comma` / `semicolon` / `tab` / `pipe`; the
    /// decimal convention is `point` or `comma`.
    pub fn with_delimiter_label(mut self, label: &str) -> Result<Self, CsvError> {
        self.delimiter = Some(delimiter_label(label)?);
        Ok(self)
    }

    /// `?null=NA,n/a`: a comma list of cell texts read as null; the
    /// option may repeat, the lists union. An empty list refuses.
    pub fn with_null_label(mut self, label: &str) -> Result<Self, CsvError> {
        let words: Vec<String> = label
            .split(',')
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            return Err(CsvError::Option(
                "null= names the cell texts read as null, a comma list ('null=NA,n/a')".into(),
            ));
        }
        for w in words {
            if !self.nulls.contains(&w) {
                self.nulls.push(w);
            }
        }
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
    /// Each record's own field count, before the header's width
    /// was imposed (`:::n-fields`).
    fields: Vec<u32>,
    /// The delimiter as decided.
    delimiter: u8,
    /// The decimal convention the source declared.
    decimal: Decimal,
    /// The cell texts read as null (`?null=`), beside the empty cell.
    nulls: Vec<String>,
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
            Err(_) => Self::read(text, delimiter, Decimal::Point, Vec::new()),
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
        if let Some(candidate) = other_delimiter(text, delimiter) {
            return Err(CsvError::Delimiter {
                decided: delimiter as char,
                found: candidate as char,
                name: delimiter_name(candidate),
            });
        }
        Ok(Self::read(
            text,
            delimiter,
            options.decimal,
            options.nulls.clone(),
        )?)
    }

    fn read(
        text: &str,
        delimiter: u8,
        decimal: Decimal,
        nulls: Vec<String>,
    ) -> Result<Self, csv::Error> {
        // a ragged row reads as the stream reads it (ruling
        // 2026-11-06): a short one fills its tail with null, a long
        // one drops the fields past the header
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .flexible(true)
            .from_reader(text.as_bytes());
        let columns: Vec<String> = reader
            .headers()?
            .iter()
            .map(|h| h.trim().to_string())
            .collect();
        let width = columns.len();
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        for record in reader.records() {
            let record = record?;
            fields.push(record.len() as u32);
            let mut row: Vec<String> = record.iter().take(width).map(|c| c.to_string()).collect();
            row.resize(width, String::new());
            rows.push(row);
        }
        Ok(CsvAdapter {
            columns,
            rows,
            fields,
            delimiter,
            decimal,
            nulls,
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
        // An empty cell is a missing value, not an empty string; so
        // is a cell the `null=` option names.
        row.get(idx)
            .map(|c| c.as_str())
            .filter(|c| !c.is_empty() && !self.nulls.iter().any(|n| n == c))
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

    /// The kind (spec: Traits): the root is a `<table>`, every node
    /// under it a `<row>`.
    fn traits(&self, node: NodeId) -> Vec<String> {
        vec![if node.0 == 0 { "table" } else { "row" }.to_string()]
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

    fn row_columns(&self, _node: NodeId) -> Option<Vec<String>> {
        Some(self.columns.clone())
    }

    /// The header is every property a row has; what links the rows
    /// is another layer's to say.
    fn surface(&self, node: NodeId) -> Option<quarb::Surface> {
        if node.0 == 0 {
            return None;
        }
        Some(quarb::Surface {
            container: "/row".to_string(),
            properties: Some(self.columns.clone()),
            labels: None,
        })
    }

    fn row_text(&self, node: NodeId, f: &mut dyn FnMut(&[&str])) -> bool {
        let Some(row) = (node.0 as usize)
            .checked_sub(1)
            .and_then(|i| self.rows.get(i))
        else {
            return false;
        };
        if self.decimal == Decimal::Comma || !self.nulls.is_empty() {
            let owned: Vec<String> = row
                .iter()
                .map(|c| {
                    if self.nulls.iter().any(|n| n == c) {
                        String::new()
                    } else {
                        self.canonical(c)
                    }
                })
                .collect();
            let cells: Vec<&str> = owned.iter().map(String::as_str).collect();
            f(&cells);
        } else {
            let cells: Vec<&str> = row.iter().map(String::as_str).collect();
            f(&cells);
        }
        true
    }

    /// `:::columns` (root), `:::n-rows` (root), `:::delimiter` and
    /// `:::decimal` (root, as decided), `:::n-fields` (row), and any
    /// column by name.
    /// The `:::` keys this adapter answers (`:meta` lists the ones
    /// a node answers).
    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &["delimiter", "decimal", "columns", "n-rows", "n-fields"]
    }

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
            "n-fields" => (node.0 as usize)
                .checked_sub(1)
                .and_then(|i| self.fields.get(i))
                .map(|n| Value::Int(*n as i64)),
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

/// The streaming reading of a CSV file (ruling 2026-11-05: under
/// `qua QUERY file.csv` the file is not held). The header is read at
/// the open and one scan records where every record starts — the
/// offset index, eight bytes a row — so the row count and the rows'
/// ids cost no cells; a row is parsed from its offset when a cell of
/// it is read, the last row parsed kept for the next read, the file
/// read through a [`quarb::source::Window`]. Every answer is the in-memory
/// [`CsvAdapter`]'s: the same ids, locators, metadata, cells (text,
/// the decimal comma canonicalized) and traits, so a query reads
/// alike whichever mounts the file.
pub struct CsvStream {
    path: std::path::PathBuf,
    columns: Vec<String>,
    /// The byte offset of each record, in order: row `k` (1-based)
    /// starts at `offsets[k - 1]`, counted from the data's start as
    /// the reader counts.
    offsets: Vec<u64>,
    size: u64,
    delimiter: u8,
    decimal: Decimal,
    /// The cell texts read as null (`?null=`), beside the empty cell.
    nulls: Vec<String>,
    reader: std::cell::RefCell<csv::Reader<quarb::source::Window>>,
    /// The row last parsed, by id, as the record the reader filled —
    /// one buffer, reused: a cell is read off it, nothing allocated
    /// per row beyond the parse.
    last: std::cell::RefCell<(u64, csv::StringRecord)>,
    /// The row the reader would parse next without a seek: the one
    /// after the row last parsed from the stream (a sequential read
    /// — a predicate over every row, the table reading — seeks once).
    next: std::cell::Cell<u64>,
}

impl CsvStream {
    /// Open the file: the delimiter decided as [`CsvAdapter::parse_declared`]
    /// decides it (the option, a `sep=` line, the extension; a header
    /// without the delimiter but with another that every sampled line
    /// carries is refused naming both), the header read, the offsets
    /// scanned.
    pub fn open(
        path: &std::path::Path,
        by_extension: u8,
        options: CsvOptions,
    ) -> Result<Self, CsvError> {
        use std::io::Read;
        let size = std::fs::metadata(path)?.len();
        // the head of the file: the `sep=` line and the delimiter check
        let head = {
            let f = std::fs::File::open(path)?;
            let mut r = std::io::BufReader::new(f).take(64 * 1024);
            let mut bytes = Vec::new();
            r.read_to_end(&mut bytes)?;
            let bytes = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
                Some(rest) => rest.to_vec(),
                None => bytes,
            };
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let bom = std::fs::File::open(path).and_then(|mut f| {
            let mut b = [0u8; 3];
            let n = f.read(&mut b)?;
            Ok(if n == 3 && b == [0xEF, 0xBB, 0xBF] {
                3u64
            } else {
                0
            })
        })?;
        let (data_start, declared) = match sep_line(&head) {
            Some((d, rest)) => (bom + (head.len() - rest.len()) as u64, Some(d)),
            None => (bom, None),
        };
        let delimiter = options.delimiter.or(declared).unwrap_or(by_extension);
        let body = &head[(data_start - bom) as usize..];
        // a line the head cut short is not one of the first lines
        let body = if size > head.len() as u64 + bom && !body.ends_with('\n') {
            body.rsplit_once('\n').map_or(body, |(whole, _)| whole)
        } else {
            body
        };
        if let Some(candidate) = other_delimiter(body, delimiter) {
            return Err(CsvError::Delimiter {
                decided: delimiter as char,
                found: candidate as char,
                name: delimiter_name(candidate),
            });
        }
        // The stream reads UTF-8 and answers rows by their offsets:
        // a byte that is not UTF-8 is refused here as the whole
        // reader refuses it. (The scan below counts byte records and
        // never looked, so such a row read as null and the table of
        // the rows came out with that row gone and the next one
        // twice.)
        if let Some(offset) = first_invalid_utf8(path)? {
            return Err(quarb::source::SourceError::NotUtf8 { offset }.into());
        }
        // the reader's own buffer is small: it is dropped at every
        // seek and filled again from the window, which holds the file
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .flexible(true)
            .buffer_capacity(1 << 10)
            .from_reader(quarb::source::Window::new(
                std::fs::File::open(path)?,
                data_start,
            ));
        let columns: Vec<String> = reader
            .headers()?
            .iter()
            .map(|h| h.trim().to_string())
            .collect();
        let mut offsets = Vec::new();
        let mut record = csv::ByteRecord::new();
        while reader.read_byte_record(&mut record)? {
            if let Some(pos) = record.position() {
                offsets.push(pos.byte());
            }
        }
        // the reader stands at the end; every read seeks first
        Ok(CsvStream {
            path: path.to_path_buf(),
            columns,
            offsets,
            size,
            delimiter,
            decimal: options.decimal,
            nulls: options.nulls.clone(),
            reader: std::cell::RefCell::new(reader),
            last: std::cell::RefCell::new((0, csv::StringRecord::new())),
            next: std::cell::Cell::new(0),
        })
    }

    pub fn locator(&self, node: NodeId) -> String {
        if node.0 == 0 {
            "/".to_string()
        } else {
            format!("/row[{}]", node.0)
        }
    }

    fn canonical(&self, cell: &str) -> String {
        if self.decimal == Decimal::Comma
            && let Some(c) = quarb::number::canonical(cell.trim(), Decimal::Comma)
        {
            return c;
        }
        cell.to_string()
    }

    /// Row `id` (1-based) parsed into the one record — from its
    /// offset, or as the stream's next record when it follows the
    /// row last parsed — and `f` read on it; `None` past the end.
    fn with_row<T>(&self, id: u64, f: impl FnOnce(&csv::StringRecord) -> T) -> Option<T> {
        {
            let last = self.last.borrow();
            if last.0 == id {
                return Some(f(&last.1));
            }
        }
        let offset = *self.offsets.get((id as usize).checked_sub(1)?)?;
        let mut last = self.last.borrow_mut();
        let mut reader = self.reader.borrow_mut();
        // the row after the one last parsed is the stream's next
        // record; any other, the reader seeks to its offset and reads
        // the one record there. The offset is the data's, as the
        // reader's own position is: counted from the file's start it
        // met the reader's position after the open's scan where a
        // BOM or a `sep=` line stood before the data, the seek was
        // taken for done, and a last row as long as what was skipped
        // read as null when it was the first row read
        // a read that fails leaves no row held and no next row known
        let lost = |last: &mut (u64, csv::StringRecord)| {
            self.next.set(0);
            last.0 = 0;
        };
        if self.next.get() != id {
            let mut pos = csv::Position::new();
            pos.set_byte(offset);
            if reader.seek(pos).is_err() {
                lost(&mut last);
                return None;
            }
        }
        if !matches!(reader.read_record(&mut last.1), Ok(true)) {
            lost(&mut last);
            return None;
        }
        self.next.set(id + 1);
        last.0 = id;
        Some(f(&last.1))
    }

    fn cell(&self, node: NodeId, column: &str) -> Option<String> {
        let idx = self.columns.iter().position(|c| c == column)?;
        self.with_row(node.0, |r| {
            r.get(idx)
                .filter(|c| !c.is_empty() && !self.nulls.iter().any(|n| n == c))
                .map(|c| self.canonical(c))
        })?
    }

    fn rows_plan(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> bool {
        from.0 == 0
            && plan.conjuncts.is_empty()
            && plan.traits.is_empty()
            && plan.name.is_none_or(|n| n == "row")
            && matches!(
                plan.axis,
                quarb::PlanAxis::Child | quarb::PlanAxis::Descendant
            )
    }
}

impl AstAdapter for CsvStream {
    fn estimate(&self, _scope: &[&str], plan: &quarb::StepPlan<'_>) -> Option<quarb::Estimate> {
        let n = self.offsets.len() as u64;
        let nodes = match plan.limit {
            Some(k) if self.rows_plan(NodeId(0), plan) => (n.min(k as u64), n.min(k as u64)),
            _ => (n, n),
        };
        Some(quarb::Estimate {
            nodes,
            bytes: Some((self.size, self.size)),
            requests: None,
            latency: quarb::Latency::Local,
            confidence: quarb::Confidence::Exact,
        })
    }

    fn resolves_references(&self) -> bool {
        false
    }

    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn walks_for_references(&self) -> bool {
        true
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            (1..=self.offsets.len() as u64).map(NodeId).collect()
        } else {
            Vec::new()
        }
    }

    fn children_named(&self, node: NodeId, name: &str) -> Vec<NodeId> {
        if node.0 == 0 && name == "row" {
            self.children(node)
        } else {
            Vec::new()
        }
    }

    /// The row count without a walk: `/row @| count` on the plain
    /// path (the engine's counted step) is the index's length.
    fn count(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<usize> {
        self.rows_plan(from, plan).then_some(self.offsets.len())
    }

    /// A prefix the plan asks for (`@| [..n]`, `first(n)` right after
    /// the hop): the first n rows' ids, nothing read.
    fn select(&self, from: NodeId, plan: &quarb::StepPlan<'_>) -> Option<quarb::Selection> {
        let k = plan.limit?;
        if !self.rows_plan(from, plan) {
            return None;
        }
        let n = self.offsets.len().min(k);
        Some(quarb::Selection {
            nodes: (1..=n as u64).map(|i| (NodeId(i), 1)).collect(),
            applied: Vec::new(),
        })
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

    fn traits(&self, node: NodeId) -> Vec<String> {
        vec![if node.0 == 0 { "table" } else { "row" }.to_string()]
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        if node.0 == 0 {
            return None;
        }
        self.cell(node, name).map(Value::Str)
    }

    fn properties(&self, node: NodeId) -> Vec<(String, Value)> {
        if node.0 == 0 {
            return Vec::new();
        }
        self.with_row(node.0, |r| {
            self.columns
                .iter()
                .zip(r.iter())
                // a missing value is an empty cell or a text the
                // `null=` option names, as `cell` reads it: the
                // stream's record reading kept "NA" where the whole
                // reader, and `::note` here, left the field out
                .filter(|(_, v)| !v.is_empty() && !self.nulls.iter().any(|n| n == v))
                .map(|(c, v)| (c.clone(), Value::Str(self.canonical(v))))
                .collect()
        })
        .unwrap_or_default()
    }

    fn row_columns(&self, _node: NodeId) -> Option<Vec<String>> {
        Some(self.columns.clone())
    }

    /// The header is every property a row has; what links the rows
    /// is another layer's to say.
    fn surface(&self, node: NodeId) -> Option<quarb::Surface> {
        if node.0 == 0 {
            return None;
        }
        Some(quarb::Surface {
            container: "/row".to_string(),
            properties: Some(self.columns.clone()),
            labels: None,
        })
    }

    /// The row's cells from the record as parsed, borrowed — the
    /// canonical form made only under a declared decimal comma.
    fn row_text(&self, node: NodeId, f: &mut dyn FnMut(&[&str])) -> bool {
        if node.0 == 0 {
            return false;
        }
        self.with_row(node.0, |r| {
            if self.decimal == Decimal::Comma || !self.nulls.is_empty() {
                let owned: Vec<String> = r
                    .iter()
                    .map(|c| {
                        if self.nulls.iter().any(|n| n == c) {
                            String::new()
                        } else {
                            self.canonical(c)
                        }
                    })
                    .collect();
                let cells: Vec<&str> = owned.iter().map(String::as_str).collect();
                f(&cells);
            } else {
                let cells: Vec<&str> = r.iter().collect();
                f(&cells);
            }
        })
        .is_some()
    }

    fn metadata_catalogue(&self) -> &'static [&'static str] {
        &[
            "delimiter",
            "decimal",
            "columns",
            "n-rows",
            "n-fields",
            "path",
            "size",
        ]
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        if node.0 == 0 {
            return match key {
                "delimiter" => Some(Value::Str((self.delimiter as char).to_string())),
                "decimal" => Some(Value::Str(
                    match self.decimal {
                        Decimal::Point => "point",
                        Decimal::Comma => "comma",
                    }
                    .into(),
                )),
                "columns" => Some(Value::list(
                    self.columns.iter().map(|c| Value::Str(c.clone())).collect(),
                )),
                "n-rows" => Some(Value::Int(self.offsets.len() as i64)),
                "path" => Some(Value::Str(self.path.display().to_string())),
                "size" => Some(Value::Int(self.size as i64)),
                _ => None,
            };
        }
        match key {
            "n-fields" => self.with_row(node.0, |r| Value::Int(r.len() as i64)),
            other => self.property(node, other),
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    fn file(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("quarb-csv-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    const TEXT: &str = "Name,Age,Note\nBraund,22,\"a, b\"\nCumings,38,\"line one\nline two\"\nHeikkinen,26,\nFutrelle,35,x\n";

    /// The streamed reading answers what the whole reading answers:
    /// the ids, the cells (a quoted newline, an empty cell missing),
    /// the names, the metadata.
    #[test]
    fn the_stream_reads_as_the_whole_file_does() {
        let p = file("t.csv", TEXT);
        let s = CsvStream::open(&p, b',', CsvOptions::default()).unwrap();
        let w = CsvAdapter::parse(TEXT).unwrap();
        assert_eq!(s.children(NodeId(0)), w.children(NodeId(0)));
        assert_eq!(s.children(NodeId(0)).len(), 4);
        for id in 1..=4u64 {
            let n = NodeId(id);
            assert_eq!(s.name(n), w.name(n));
            assert_eq!(s.properties(n), w.properties(n), "row {id}");
            for col in ["Name", "Age", "Note"] {
                assert_eq!(s.property(n, col), w.property(n, col), "row {id} {col}");
            }
            assert_eq!(s.locator(n), w.locator(n));
        }
        // out of order, and the same row twice
        assert_eq!(
            s.property(NodeId(4), "Name"),
            Some(Value::Str("Futrelle".into()))
        );
        assert_eq!(
            s.property(NodeId(2), "Note"),
            Some(Value::Str("line one\nline two".into()))
        );
        assert_eq!(s.property(NodeId(2), "Age"), Some(Value::Str("38".into())));
        assert_eq!(s.property(NodeId(3), "Note"), None);
        assert_eq!(s.metadata(NodeId(0), "n-rows"), Some(Value::Int(4)));
        assert_eq!(
            s.metadata(NodeId(0), "columns"),
            w.metadata(NodeId(0), "columns")
        );
        assert_eq!(s.metadata(NodeId(2), "n-fields"), Some(Value::Int(3)));
    }

    /// A `sep=` line behind a BOM, and the decimal comma, read as the
    /// whole reading reads them.
    #[test]
    fn the_stream_takes_the_sep_line_and_the_decimal() {
        let text = "\u{feff}sep=;\nName;Val\nA;1,5\nB;2,25\n";
        let p = file("sep.csv", text);
        let opts = CsvOptions {
            delimiter: None,
            decimal: Decimal::Comma,
            ..Default::default()
        };
        let s = CsvStream::open(&p, b',', opts.clone()).unwrap();
        let w =
            CsvAdapter::parse_declared(text.trim_start_matches('\u{feff}'), b',', opts).unwrap();
        assert_eq!(
            s.metadata(NodeId(0), "delimiter"),
            Some(Value::Str(";".into()))
        );
        assert_eq!(s.children(NodeId(0)).len(), 2);
        for id in 1..=2u64 {
            assert_eq!(
                s.properties(NodeId(id)),
                w.properties(NodeId(id)),
                "row {id}"
            );
        }
    }

    /// The row count and a prefix come from the index, nothing read;
    /// the estimate is the file's size.
    #[test]
    fn the_stream_counts_and_cuts_a_prefix_from_its_index() {
        let p = file("n.csv", TEXT);
        let s = CsvStream::open(&p, b',', CsvOptions::default()).unwrap();
        let plan = |limit: Option<usize>| quarb::StepPlan {
            traits: Vec::new(),
            cap: Default::default(),
            axis: quarb::PlanAxis::Child,
            name: Some("row"),
            conjuncts: Vec::new(),
            limit,
            order: None,
        };
        assert_eq!(s.count(NodeId(0), &plan(None)), Some(4));
        assert_eq!(s.count(NodeId(1), &plan(None)), None);
        let sel = s.select(NodeId(0), &plan(Some(2))).unwrap();
        assert_eq!(sel.nodes, vec![(NodeId(1), 1), (NodeId(2), 1)]);
        assert!(s.select(NodeId(0), &plan(None)).is_none());
        let e = s.estimate(&[], &plan(None)).unwrap();
        assert_eq!(e.nodes, (4, 4));
        assert_eq!(e.bytes, Some((TEXT.len() as u64, TEXT.len() as u64)));
    }
}
