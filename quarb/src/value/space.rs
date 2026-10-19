//! The scalar world.
//!
//! Navigation produces nodes; a projection turns each node into a
//! [`Value`]. Quarb is value-domain agnostic, so this is a small,
//! open set of scalar shapes an adapter can return.

use super::words::is_bool_word;
use std::fmt;

/// A projected scalar value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Absent / undefined.
    Null,
    /// A boolean (e.g. `:is-leaf`).
    Bool(bool),
    /// An integer (e.g. `:::size`, `:index`).
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A string (e.g. `:name`, file content).
    Str(String),
    /// A list of values (produced by aggregating pipeline commands).
    /// Shared behind an `Arc`: a register read or a group's topic
    /// copies a pointer, not the elements, so a large list used as a
    /// set operand costs its construction once.
    List(std::sync::Arc<Vec<Value>>),
    /// A record: insertion-ordered named fields (spec: The Record
    /// Scalar). Constructed by `record(...)`; field order is
    /// significant, matching kaiv namespaces and JSON key order.
    Record(Vec<(String, Value)>),
    /// A table (spec: The Table Kind): rows of one width, with a
    /// header naming the columns (a frame) or without one (an
    /// array). The reading a list of same-shaped records admits,
    /// written `#(...)`; a hole is `null`. Shared behind an `Arc`
    /// like a list.
    Table(std::sync::Arc<Table>),
    /// A point on the UTC timeline (spec: The Temporal Fragment).
    /// `offset_min` preserves the source's UTC offset for display
    /// only — comparison is always on the timeline. Displays as
    /// ISO-8601; a midnight instant with no offset prints as a
    /// bare date.
    Instant {
        secs: i64,
        nanos: u32,
        offset_min: Option<i16>,
        /// The calendar the instant is read in (spec: Calendars):
        /// like the offset, kept for reading and display only.
        calendar: crate::calendar::Calendar,
    },
    /// A stretch of the UTC calendar named at a grain (spec: The
    /// Temporal Fragment): its first second and the grain. Displays
    /// as ISO-8601 at reduced precision (`1812`, `2025-10`,
    /// `1899-10-06T14:30`). Compares as an interval; takes no part
    /// in arithmetic.
    Period {
        start: i64,
        grain: crate::period::Grain,
        /// The calendar whose month or year the period names.
        calendar: crate::calendar::Calendar,
    },
    /// A value on a dimension (spec: The Quantital Fragment):
    /// the magnitude scaled to the dimension's SI-base expansion
    /// (`base`, e.g. `m`, `kg*m^2/s^3`), the written form kept
    /// for display only — exactly as instants keep their written
    /// offset. Minted by unit-aware adapters (kaiv); compared and
    /// combined on the base, so `42 km` orders above `5000 m`.
    Quantity {
        value: f64,
        base: String,
        /// The authored magnitude and unit, for display
        /// (`42 km`); absent for computed quantities, which
        /// display in the base.
        written: Option<(f64, String)>,
    },
    /// A library value (spec: Command Libraries, "A library
    /// value"): what a command library built and keeps in its own
    /// form — a frame, an array, an object — so that its next
    /// command takes it as it is, with no crossing into records and
    /// lists between two commands. The engine knows its kind for
    /// messages, its text (the object's own display), and its two
    /// exports, the row reading (`| @`) and the column reading
    /// (`| %`); anything else done with one is refused by kind.
    /// Two library values are equal only when they are the same
    /// object.
    Foreign(ForeignValue),
    /// A node reference (spec: The Capsa, "Node references"): an
    /// engine-minted value naming one node of the arbor. It prints
    /// and exports as the node's locator, two references to one node
    /// are equal whatever the readings that minted them, and `-->`
    /// resolves it back to the node. `| type` reads `node`. The
    /// value layer knows the node only as a number: the graph mints
    /// the reference and reads it back.
    Node(NodeRef),
}

/// A node reference's payload: the node's coordinate in the run
/// and its locator, rendered when the reference was minted (the
/// value layer cannot reach an adapter). Equality and hashing are
/// by coordinate.
#[derive(Clone)]
pub struct NodeRef {
    pub id: u64,
    pub locator: std::sync::Arc<str>,
}

impl NodeRef {
    pub fn new(id: u64, locator: impl Into<std::sync::Arc<str>>) -> NodeRef {
        NodeRef {
            id,
            locator: locator.into(),
        }
    }
}

impl PartialEq for NodeRef {
    fn eq(&self, other: &NodeRef) -> bool {
        self.id == other.id
    }
}

impl Eq for NodeRef {}

impl std::hash::Hash for NodeRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl std::fmt::Debug for NodeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Node({}#{})", self.locator, self.id)
    }
}

/// A table: rows of one width, each cell a value, `null` the hole.
/// With a header it is a frame (the columns named, `t:age` a
/// column); without one an array (`#(%(1; 2); %(3; 4))`, the positions
/// its columns). One kind,
/// two faces: an ndarray and a DataFrame are its two boundary forms.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub header: Option<Vec<String>>,
    pub rows: Vec<Vec<Value>>,
}

/// The keys a list of two or more records share in one order, when
/// every item is such a record with at least one field: the header
/// of the record array the list prints as.
fn record_array_keys(items: &[Value]) -> Option<&[(String, Value)]> {
    if items.len() < 2 {
        return None;
    }
    let Value::Record(first) = &items[0] else {
        return None;
    };
    if first.is_empty() {
        return None;
    }
    let same = items.iter().all(|v| match v {
        Value::Record(o) => {
            o.len() == first.len() && o.iter().zip(first.iter()).all(|((a, _), (b, _))| a == b)
        }
        _ => false,
    });
    same.then_some(first.as_slice())
}

/// The type of a table column (spec: The Table): one scalar type
/// per column, every non-null cell a value of it. Decided when the
/// table is built, by [`Table::new`]: the least type every non-null
/// cell coerces to, tried in this order — the lattice boolean <
/// integer < float, then the types of their own, text the top, so
/// no column fails to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Boolean,
    Integer,
    Float,
    Instant,
    Duration,
    Period,
    Quantity,
    Text,
}

impl ColumnType {
    /// The type's word, as `| types` lists it.
    pub fn word(self) -> &'static str {
        match self {
            ColumnType::Boolean => "bool",
            ColumnType::Integer => "int",
            ColumnType::Float => "float",
            ColumnType::Instant => "instant",
            ColumnType::Duration => "duration",
            ColumnType::Period => "period",
            ColumnType::Quantity => "quantity",
            ColumnType::Text => "text",
        }
    }

    const ORDER: [ColumnType; 8] = [
        ColumnType::Boolean,
        ColumnType::Integer,
        ColumnType::Float,
        ColumnType::Instant,
        ColumnType::Duration,
        ColumnType::Period,
        ColumnType::Quantity,
        ColumnType::Text,
    ];

    /// The type a stored cell is of (null is of every type).
    fn of(v: &Value) -> Option<ColumnType> {
        Some(match v {
            Value::Null => return None,
            Value::Bool(_) => ColumnType::Boolean,
            Value::Int(_) => ColumnType::Integer,
            Value::Float(_) => ColumnType::Float,
            Value::Instant { .. } => ColumnType::Instant,
            Value::Quantity { base, .. } if base == "s" => ColumnType::Duration,
            Value::Period { .. } => ColumnType::Period,
            Value::Quantity { .. } => ColumnType::Quantity,
            _ => ColumnType::Text,
        })
    }
}

/// `v` coerced to the column type `t`, `None` where it has no such
/// reading. Text that reads as a type counts as that type; a
/// boolean reads as 1 and 0 among integers, an integer as a float;
/// every scalar has a text form.
fn coerce_cell(v: &Value, t: ColumnType) -> Option<Value> {
    use ColumnType as T;
    if matches!(v, Value::Null) {
        return Some(Value::Null);
    }
    match t {
        T::Boolean => match v {
            Value::Bool(_) => Some(v.clone()),
            Value::Str(s) if s == "true" => Some(Value::Bool(true)),
            Value::Str(s) if s == "false" => Some(Value::Bool(false)),
            _ => None,
        },
        T::Integer => match v {
            Value::Bool(b) => Some(Value::Int(i64::from(*b))),
            Value::Int(_) => Some(v.clone()),
            Value::Str(_) => match v.numeric_reading() {
                Some(n @ Value::Int(_)) => Some(n),
                _ => None,
            },
            _ => None,
        },
        T::Float => match v {
            Value::Bool(b) => Some(Value::Float(if *b { 1.0 } else { 0.0 })),
            Value::Int(n) => Some(Value::Float(*n as f64)),
            Value::Float(_) => Some(v.clone()),
            Value::Str(_) => match v.numeric_reading() {
                Some(Value::Int(n)) => Some(Value::Float(n as f64)),
                Some(f @ Value::Float(_)) => Some(f),
                _ => None,
            },
            _ => None,
        },
        T::Instant => match v {
            Value::Instant { .. } => Some(v.clone()),
            Value::Str(s) => {
                crate::temporal::parse_iso(s).map(|(secs, nanos, offset_min)| Value::Instant {
                    secs,
                    nanos,
                    offset_min,
                    calendar: Default::default(),
                })
            }
            _ => None,
        },
        T::Duration => match v {
            q if q.is_duration() => Some(q.clone()),
            Value::Str(s) => crate::temporal::parse_span(s)
                .map(|(secs, nanos)| Value::seconds(secs, nanos))
                .or_else(|| {
                    crate::value::quantity::parse_unit_text(s)
                        .filter(|(_, base, ..)| base == "s")
                        .map(|(value, base, wv, wu)| Value::Quantity {
                            value,
                            base,
                            written: Some((wv, wu)),
                        })
                }),
            _ => None,
        },
        T::Period => match v {
            Value::Period { .. } => Some(v.clone()),
            Value::Str(s) => crate::period::Period::parse(s).map(Value::period),
            _ => None,
        },
        T::Quantity => match v {
            Value::Quantity { .. } => Some(v.clone()),
            Value::Str(s) => {
                crate::value::quantity::parse_unit_text(s).map(|(value, base, mag, unit)| {
                    Value::Quantity {
                        value,
                        base,
                        written: Some((mag, unit)),
                    }
                })
            }
            _ => None,
        },
        T::Text => Some(match v {
            Value::Str(_) => v.clone(),
            other => Value::Str(other.to_string()),
        }),
    }
}

/// One column typed: the first type of the order every non-null
/// cell coerces to, and the cells coerced. A quantity column is of
/// one dimension; cells of several fall through to text. An
/// all-null column is text.
fn type_column(cells: Vec<Value>) -> (ColumnType, Vec<Value>) {
    for t in ColumnType::ORDER {
        let coerced: Option<Vec<Value>> = cells.iter().map(|c| coerce_cell(c, t)).collect();
        let Some(coerced) = coerced else { continue };
        if t == ColumnType::Quantity {
            let mut bases = coerced.iter().filter_map(|c| match c {
                Value::Quantity { base, .. } => Some(base.as_str()),
                _ => None,
            });
            if let Some(first) = bases.next()
                && bases.any(|b| b != first)
            {
                continue;
            }
        }
        return (t, coerced);
    }
    unreachable!("text is the top: every scalar has a text form")
}

impl Table {
    /// A table built from its header and rows, the columns typed
    /// (spec: The Table): each column takes the least type every
    /// non-null cell coerces to, and its cells are stored coerced,
    /// so a column of digit strings is an integer column that
    /// prints its numbers bare. The rows are expected flat and of
    /// one width; the one routine every builder of a table calls.
    pub fn new(header: Option<Vec<String>>, rows: Vec<Vec<Value>>) -> Table {
        let mut t = Table { header, rows };
        let w = t.width();
        if w == 0 || t.rows.is_empty() {
            return t;
        }
        for j in 0..w {
            let cells: Vec<Value> = t
                .rows
                .iter()
                .map(|r| r.get(j).cloned().unwrap_or(Value::Null))
                .collect();
            let (_, coerced) = type_column(cells);
            for (r, c) in t.rows.iter_mut().zip(coerced) {
                if j < r.len() {
                    r[j] = c;
                } else {
                    r.resize(j, Value::Null);
                    r.push(c);
                }
            }
        }
        t
    }

    /// A matrix from rows of one width: every cell coerced TOGETHER
    /// to the least type all of them admit, by the column lattice,
    /// so the array is of one type (spec: Tables and Reshapes,
    /// transpose). The rows are expected of one width.
    pub fn matrix(rows: Vec<Vec<Value>>) -> Table {
        let w = rows.first().map_or(0, Vec::len);
        let cells: Vec<Value> = rows.into_iter().flatten().collect();
        let (_, coerced) = type_column(cells);
        let rows: Vec<Vec<Value>> = if w == 0 {
            Vec::new()
        } else {
            coerced.chunks(w).map(|c| c.to_vec()).collect()
        };
        Table { header: None, rows }
    }

    /// The column types, one per column: the type of the cells as
    /// stored (text for a column of nulls).
    pub fn types(&self) -> Vec<ColumnType> {
        (0..self.width())
            .map(|j| {
                self.rows
                    .iter()
                    .find_map(|r| r.get(j).and_then(ColumnType::of))
                    .unwrap_or(ColumnType::Text)
            })
            .collect()
    }

    /// `| types`: the column types as a list of words.
    pub fn types_value(&self) -> Value {
        Value::list(
            self.types()
                .into_iter()
                .map(|t| Value::Str(t.word().to_string()))
                .collect(),
        )
    }

    /// Whether every cell holds a scalar or null — what a table
    /// promises (spec: The Table).
    pub fn is_flat(&self) -> bool {
        self.rows.iter().flatten().all(|c| {
            !matches!(
                c,
                Value::List(_) | Value::Record(_) | Value::Table(_) | Value::Foreign(_)
            )
        })
    }

    /// The rows as the list of records a frame stands for (positional
    /// lists on the array face).
    pub fn into_records(self) -> Value {
        let Table { header, rows } = self;
        Value::list(
            rows.into_iter()
                .map(|r| match &header {
                    Some(h) => Value::Record(h.iter().cloned().zip(r).collect()),
                    None => Value::list(r),
                })
                .collect(),
        )
    }

    /// The number of columns: the header's width, else the first
    /// row's (every row is padded or refused to that width).
    pub fn width(&self) -> usize {
        match &self.header {
            Some(h) => h.len(),
            None => self.rows.first().map_or(0, Vec::len),
        }
    }

    /// `@(rows; columns)`.
    pub fn shape(&self) -> Value {
        Value::list(vec![
            Value::Int(self.rows.len() as i64),
            Value::Int(self.width() as i64),
        ])
    }

    /// The kind, for messages: `a frame 3×2`, `an array 2×2`.
    pub fn kind(&self) -> String {
        format!(
            "{} {}×{}",
            if self.header.is_some() {
                "a frame"
            } else {
                "an array"
            },
            self.rows.len(),
            self.width()
        )
    }

    /// Whether the table is a sample: one column and no header, the
    /// reading a list of values admits, which reads back to it.
    pub fn is_sample(&self) -> bool {
        self.header.is_none() && self.width() == 1
    }

    /// The row reading: a frame's rows as records, an array's as
    /// lists, a sample's as its values.
    pub fn rows_value(&self) -> Value {
        Value::list(self.rows.iter().map(|r| self.row_value(r)).collect())
    }

    fn row_value(&self, row: &[Value]) -> Value {
        match &self.header {
            Some(h) => Value::Record(h.iter().cloned().zip(row.iter().cloned()).collect()),
            None if self.width() == 1 => row.first().cloned().unwrap_or(Value::Null),
            None => Value::list(row.to_vec()),
        }
    }

    /// The row at `i` (1-based), as the row reading gives it.
    pub fn row(&self, i: usize) -> Option<Value> {
        (i >= 1)
            .then(|| self.rows.get(i - 1))
            .flatten()
            .map(|r| self.row_value(r))
    }

    /// The column reading: a record of lists, keyed by the header,
    /// or by position (`1`, `2`, …) on the array face.
    pub fn columns_value(&self) -> Value {
        let cols: Vec<Value> = (0..self.width()).map(|j| self.column_at(j)).collect();
        let keys: Vec<String> = match &self.header {
            Some(h) => h.clone(),
            None => (1..=self.width()).map(|j| j.to_string()).collect(),
        };
        Value::Record(keys.into_iter().zip(cols).collect())
    }

    /// The header as a list of names; null on the array face.
    pub fn header_value(&self) -> Value {
        match &self.header {
            Some(h) => Value::list(h.iter().cloned().map(Value::Str).collect()),
            None => Value::Null,
        }
    }

    /// The rows `a..b` (1-based, inclusive, either end open) as a
    /// table under the same header.
    pub fn slice(&self, a: Option<i64>, b: Option<i64>) -> Table {
        let n = self.rows.len() as i64;
        let from = a.unwrap_or(1).clamp(1, n + 1) - 1;
        let to = b.unwrap_or(n).clamp(0, n);
        Table {
            header: self.header.clone(),
            rows: if from < to {
                self.rows[from as usize..to as usize].to_vec()
            } else {
                Vec::new()
            },
        }
    }

    /// The output form: the literal with the header and each row on
    /// a line of its own, so it pastes back as the constructor.
    pub fn to_quarb_lines(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(self.rows.len() + 1);
        if let Some(h) = &self.header {
            let names: Vec<String> = h.iter().map(|k| quarb_key(k)).collect();
            parts.push(format!("@({})", names.join("; ")));
        }
        for r in &self.rows {
            let cells: Vec<String> = r.iter().map(Value::to_quarb).collect();
            parts.push(format!("%({})", cells.join("; ")));
        }
        if parts.len() <= 1 {
            return format!("#({})", parts.join(""));
        }
        format!("#({})", parts.join(";\n  "))
    }

    fn column_at(&self, j: usize) -> Value {
        Value::list(
            self.rows
                .iter()
                .map(|r| r.get(j).cloned().unwrap_or(Value::Null))
                .collect(),
        )
    }

    /// A column by name, as a list: the header's name on a frame, the
    /// position's text (`"2"`) on the array face, where the positions
    /// are the column names.
    pub fn column(&self, name: &str) -> Option<Value> {
        match &self.header {
            // By name first; a number that names no column is the
            // position (`#*:2`, the second column; spec: Context
            // Accessors).
            Some(h) => h
                .iter()
                .position(|c| c == name)
                .or_else(|| {
                    name.parse::<usize>()
                        .ok()
                        .filter(|&j| j >= 1 && j <= self.width())
                        .map(|j| j - 1)
                })
                .map(|j| self.column_at(j)),
            None => name
                .parse::<usize>()
                .ok()
                .filter(|j| *j >= 1 && *j <= self.width())
                .map(|j| self.column_at(j - 1)),
        }
    }

    /// The rows and columns exchanged: an array's transpose.
    pub fn transposed(&self) -> Table {
        let w = self.width();
        Table::new(
            None,
            (0..w)
                .map(|j| {
                    self.rows
                        .iter()
                        .map(|r| r.get(j).cloned().unwrap_or(Value::Null))
                        .collect()
                })
                .collect(),
        )
    }

    /// The literal that reads back: `#(@(name; age); %("Braund";
    /// 22))` for a frame, `#(%(1; 2); %(3; 4))` for an array: every row a
    /// record, the header the one list.
    pub fn to_quarb(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(self.rows.len() + 1);
        if let Some(h) = &self.header {
            let names: Vec<String> = h.iter().map(|k| quarb_key(k)).collect();
            parts.push(format!("@({})", names.join("; ")));
        }
        for r in &self.rows {
            let cells: Vec<String> = r.iter().map(Value::to_quarb).collect();
            parts.push(format!("%({})", cells.join("; ")));
        }
        format!("#({})", parts.join("; "))
    }
}

/// What a command library implements for the values it keeps in
/// its own form; see [`Value::Foreign`].
pub trait Foreign: Send + Sync {
    /// The kind, for messages: `DataFrame 17×3`, `ndarray (17, 3)
    /// float64`.
    fn kind(&self) -> String;
    /// The object's own text (its display).
    fn display(&self) -> String;
    /// The row reading (`| @`): a frame as a list of records with
    /// its index as the first field, an array as nested lists, a
    /// series as a list. `Err` names why there is none.
    fn rows(&self) -> Result<Value, String>;
    /// The column reading (`| %`): a frame as a record of columns,
    /// each a record keyed by the index; a series on a named index
    /// as one record. `Err` names why there is none (an array has
    /// no names).
    fn columns(&self) -> Result<Value, String>;
    /// The object itself, for the library that made it.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// A shared library value; equal to another only as the same object.
#[derive(Clone)]
pub struct ForeignValue(pub std::sync::Arc<dyn Foreign>);

impl ForeignValue {
    pub fn new(f: impl Foreign + 'static) -> ForeignValue {
        ForeignValue(std::sync::Arc::new(f))
    }

    /// The refusal for a command that has no reading of a library
    /// value, naming the kind and the two exports.
    pub fn refusal(&self, command: &str) -> String {
        format!(
            "{command}: {} is a library value; '| @' exports its rows, '| %' its columns",
            self.0.kind()
        )
    }
}

impl std::ops::Deref for ForeignValue {
    type Target = dyn Foreign;
    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl PartialEq for ForeignValue {
    fn eq(&self, other: &ForeignValue) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for ForeignValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Foreign({})", self.0.kind())
    }
}

impl Value {
    /// A list value from its elements.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(std::sync::Arc::new(items))
    }

    /// A table value (shared like a list).
    pub fn table(t: Table) -> Value {
        Value::Table(std::sync::Arc::new(t))
    }

    /// The elements of a list value, taken out of their sharing
    /// (cloned only when another holder remains).
    pub fn into_items(self) -> Option<Vec<Value>> {
        match self {
            Value::List(l) => Some(std::sync::Arc::try_unwrap(l).unwrap_or_else(|a| (*a).clone())),
            _ => None,
        }
    }

    /// A byte count as a typed quantity on the information base —
    /// the mint for every adapter's `size` fact, so `[:::size >
    /// 1GiB]`, `| convert(MB)`, and typed size totals work
    /// uniformly. (Exact up to 2^53 bytes — 8 PiB — beyond which
    /// f64 granularity coarsens; no substrate this engine mounts
    /// reports single objects there.)
    pub fn bytes(n: i64) -> Value {
        // On the information base, the bit (B = 8 b); written as
        // the byte count it is, so a size prints `65 B` and reads
        // beside `200 kB` on one scale.
        Value::Quantity {
            value: n as f64 * 8.0,
            base: "b".to_string(),
            written: Some((n as f64, "B".to_string())),
        }
    }

    /// Truthiness, for predicate coercion: `Null`, `false`, `0`, the
    /// empty string, and the empty list and record are falsy;
    /// everything else is truthy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Record(o) => !o.is_empty(),
            Value::Table(t) => !t.rows.is_empty(),
            Value::Instant { .. } => true,
            Value::Period { .. } => true,
            Value::Quantity { value, .. } => *value != 0.0,
            // No truth value: a predicate on a library value is
            // refused by kind, and false.
            Value::Foreign(f) => {
                crate::value::ambient::record_refusal(f.refusal("a predicate"));
                false
            }
            // A reference names a node: true.
            Value::Node(_) => true,
        }
    }

    /// The *temporal reading* (spec: The Temporal Fragment): the UTC
    /// timeline point a value denotes, if any — an instant itself,
    /// an integer or float read as epoch seconds, or ISO-8601 text.
    /// Booleans, lists, records, durations, and non-ISO text have
    /// none.
    pub fn temporal_reading(&self) -> Option<(i64, u32)> {
        match self {
            Value::Instant { secs, nanos, .. } => Some((*secs, *nanos)),
            // A period reads as its first instant.
            Value::Period { start, .. } => Some((*start, 0)),
            Value::Int(n) => Some((*n, 0)),
            Value::Float(f) if f.is_finite() => {
                let secs = f.floor() as i64;
                let nanos = ((f - f.floor()) * 1e9) as u32;
                Some((secs, nanos))
            }
            Value::Str(s) => crate::temporal::parse_iso(s).map(|(s, n, _)| (s, n)),
            _ => None,
        }
    }

    /// The *calendar reading* (spec: The Calendar Reading): the
    /// period a value denotes, if any — a period itself, ISO-8601
    /// text at reduced precision (or a full instant's text), an
    /// integer (that year), or an instant at the grain it prints
    /// at (the day for a midnight instant with no offset, the
    /// second otherwise).
    pub fn calendar_reading(&self) -> Option<crate::period::Period> {
        use crate::period::Period;
        match self {
            Value::Period {
                start,
                grain,
                calendar,
            } => Some(Period {
                start: *start,
                grain: *grain,
                calendar: *calendar,
            }),
            // an integer is epoch seconds wherever an instant is
            // expected and leaves the calendar coercion; the year
            // is written with its trailing dash, `2025-`
            Value::Int(_) => None,
            Value::Instant {
                secs,
                nanos,
                offset_min,
                calendar,
            } => Some(Period::of_instant(*secs, *nanos, *offset_min).in_calendar(*calendar)),
            Value::Str(s) => year_word(s).or_else(|| Period::parse(s)).or_else(|| {
                crate::temporal::parse_iso(s)
                    .map(|(secs, nanos, off)| Period::of_instant(secs, nanos, off))
            }),
            _ => None,
        }
    }

    /// A period as a value.
    pub fn period(p: crate::period::Period) -> Value {
        Value::Period {
            start: p.start,
            grain: p.grain,
            calendar: p.calendar,
        }
    }

    /// The *unital reading* (spec: The Unital Reading): the
    /// dimensioned magnitude a value denotes, if any — a quantity
    /// itself, or unit text (`5km`, `0.2 kW`) scaled through the
    /// frozen built-in table. Bare numbers read as the *partner's*
    /// base at the comparison site (they carry no dimension of
    /// their own), so they are not read here.
    pub fn unital_reading(&self) -> Option<(f64, String)> {
        match self {
            Value::Quantity { value, base, .. } => Some((*value, base.clone())),
            Value::Str(s) => crate::value::quantity::parse_unit_text(s)
                .map(|(v, base, ..)| (v, base.to_string())),
            _ => None,
        }
    }

    /// The *durational reading* (spec: The Durational Reading): the
    /// span a value denotes, if any — a duration itself, a number
    /// read as seconds, or span text (an ISO-8601 duration or a
    /// systemd time-span like `5d3h5min`). Booleans, lists, records,
    /// instants, and other text have none.
    pub fn durational_reading(&self) -> Option<(i64, u32)> {
        match self {
            Value::Quantity { .. } => self.span_parts(),
            // A bare number is not seconds: time is a dimension like
            // the others (spec: the durational coercion, draft 155).
            // Unit text in a time unit first (`2h` keeps its written
            // unit), then ISO-8601 and the multi-component span text
            Value::Str(s) => crate::value::quantity::parse_unit_text(s)
                .filter(|(_, base, ..)| base == "s")
                .map(|(v, ..)| split_seconds(v))
                .or_else(|| crate::temporal::parse_span(s)),
            _ => None,
        }
    }

    /// A span of time: the quantity of dimension T on the base `s`
    /// (spec: Duration), seconds and nanoseconds in, displayed in
    /// seconds unless a written unit is set.
    pub fn seconds(secs: i64, nanos: u32) -> Value {
        Value::Quantity {
            value: secs as f64 + nanos as f64 / 1e9,
            base: "s".to_string(),
            written: None,
        }
    }

    /// Whether the value is a quantity of time.
    pub fn is_duration(&self) -> bool {
        matches!(self, Value::Quantity { base, .. } if base == "s")
    }

    /// The seconds and nanoseconds of a time quantity.
    pub fn span_parts(&self) -> Option<(i64, u32)> {
        match self {
            Value::Quantity { value, base, .. } if base == "s" && value.is_finite() => {
                Some(split_seconds(*value))
            }
            _ => None,
        }
    }

    /// The Quarb form: the value spelled as Quarb
    /// query text, so a printed result reads back as an expression.
    /// Strings quoted as literals, numbers and booleans bare,
    /// `null`, lists as `@(a; b)`, records as `%(k = v; k2 = v2)`
    /// with bare identifier keys; instants, durations, and
    /// quantities in their written form. This is what `qua` prints
    /// for a record or list result — JSON is `| json` / `--json`.
    pub fn to_quarb(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Str(s) => quarb_string(s),
            Value::List(l) => {
                // Two or more records of one shape (the same keys in
                // the same order) print as the record array, the
                // header once; records that differ in shape print
                // record by record, so no sparse grid is shown.
                if let Some(keys) = record_array_keys(l) {
                    let mut parts: Vec<String> = Vec::with_capacity(l.len() + 1);
                    let names: Vec<String> = keys.iter().map(|(k, _)| quarb_key(k)).collect();
                    parts.push(format!("@({})", names.join("; ")));
                    for item in l.iter() {
                        if let Value::Record(o) = item {
                            let cells: Vec<String> = o.iter().map(|(_, v)| v.to_quarb()).collect();
                            parts.push(format!("%({})", cells.join("; ")));
                        }
                    }
                    return format!("@%({})", parts.join("; "));
                }
                let inner: Vec<String> = l.iter().map(Value::to_quarb).collect();
                format!("@({})", inner.join("; "))
            }
            Value::Record(o) => {
                let inner: Vec<String> = o
                    .iter()
                    .map(|(k, v)| format!("{} = {}", quarb_key(k), v.to_quarb()))
                    .collect();
                format!("%({})", inner.join("; "))
            }
            Value::Table(t) => t.to_quarb(),
            // A library value has no Quarb form that reads back; its
            // text is the object's own display.
            other => other.to_string(),
        }
    }

    /// What a result line shows: a scalar bare (text unquoted), a
    /// record or list in its Quarb form.
    /// A table a boundary would make implicitly — a library's frame
    /// or two-dimensional array, an adapter's tabular source — as the
    /// engine's value: the table when it is flat, else the list of
    /// records that keeps its shape (spec: The Table). One rule for
    /// every boundary; the explicit constructors (`#(…)`, `| #`)
    /// refuse instead, since the query asked for a table.
    pub fn table_or_records(t: Table) -> Value {
        if t.is_flat() {
            Value::table(Table::new(t.header, t.rows))
        } else {
            t.into_records()
        }
    }

    pub fn display_form(&self) -> String {
        match self {
            Value::List(_) | Value::Record(_) => self.to_quarb(),
            Value::Table(t) => t.to_quarb_lines(),
            other => other.to_string(),
        }
    }

    /// Strict-JSON rendering: strings quoted and escaped, numbers and
    /// booleans bare, null literal, lists as arrays, records as
    /// key-ordered JSON objects. This is the `| json` / `| jsonl`
    /// serialization and the `--json` / `--jsonl` output.
    pub fn to_json(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) if f.is_finite() => format_float(*f),
            // JSON has no spelling for infinity or NaN.
            Value::Float(_) => "null".to_string(),
            Value::Str(s) => json_string(s),
            Value::List(l) => {
                let inner: Vec<String> = l.iter().map(Value::to_json).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Record(o) => {
                let inner: Vec<String> = o
                    .iter()
                    .map(|(k, v)| format!("{}: {}", json_string(k), v.to_json()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
            Value::Instant { .. } | Value::Period { .. } | Value::Quantity { .. } => {
                json_string(&self.to_string())
            }
            // The row reading, serialized.
            Value::Table(t) => t.rows_value().to_json(),
            // The row reading, serialized; its text where the object
            // has no rows.
            Value::Foreign(f) => match f.rows() {
                Ok(rows) => rows.to_json(),
                Err(_) => json_string(&f.display()),
            },
            // The locator text: no JSON form keeps nodeness.
            Value::Node(n) => json_string(&n.locator),
        }
    }

    /// The numeric value, for numeric comparison and aggregation.
    /// `Int` and `Float` coerce, and so does a string that parses as a
    /// finite number\,---\,without which numeric work would silently
    /// fail over text adapters, where every value is a string (an HTML
    /// attribute like `rank="3"`, a CSV cell). Non-numeric strings,
    /// booleans, lists, and null do not coerce.
    /// The key under which a value may be indexed for equality
    /// (`=`, the value match `=>`): two values with keys compare
    /// equal only if their keys are equal — the numeric reading
    /// when there is one (so `1`, `1.0` and `"1"` share a key, as
    /// they compare equal), else the text, booleans and null apart.
    /// `None` for values compared under another reading (instants,
    /// durations, quantities, lists, records): a search for one, or
    /// among them, takes the scan.
    pub fn equality_key(&self) -> Option<String> {
        match self {
            Value::Int(_) | Value::Float(_) | Value::Str(_) => Some(match self.numeric_reading() {
                // An integer keys as itself (exact past 2^53); a
                // whole double keys as that integer, so `1`, `1.0`
                // and `"1"` still share a key, and -0.0 is 0.
                Some(Value::Int(n)) => format!("n:{n}"),
                Some(Value::Float(f)) if f.fract() == 0.0 && f.abs() < 9.2e18 => {
                    format!("n:{}", f as i64)
                }
                Some(Value::Float(f)) => format!("n:{f}"),
                _ => format!("s:{self}"),
            }),
            Value::Bool(b) => Some(format!("b:{b}")),
            Value::Null => Some("0:".to_string()),
            Value::Node(n) => Some(format!("node:{}", n.id)),
            _ => None,
        }
    }

    pub fn numeric(&self) -> Option<f64> {
        match self {
            Value::Int(n) => Some(*n as f64),
            Value::Float(f) => Some(*f),
            Value::Str(s) => crate::value::number::read(s, crate::value::number::Decimal::Point)
                .and_then(|v| v.numeric()),
            // A quantity's base magnitude, so aggregates work
            // (`::power @| sum` sums watts). Arithmetic never sees
            // this: the quantital fragment intercepts first.
            Value::Quantity { value, .. } => Some(*value),
            _ => None,
        }
    }

    /// The *numeric reading* (spec: The Numeric Fragment) — like
    /// [`Value::numeric`], but type-preserving: text with integer
    /// form reads as an exact `Int`, so arithmetic over text-sourced
    /// values stays exact. At most one reading exists; booleans,
    /// lists, and null have none.
    pub fn numeric_reading(&self) -> Option<Value> {
        match self {
            Value::Int(n) => Some(Value::Int(*n)),
            Value::Float(f) => Some(Value::Float(*f)),
            // The canonical form, and under the default convention
            // grouped thousands (`1,234`, `1 234`); a source that
            // writes decimal commas declares it (`?decimal=comma`)
            // and its text arrives in canonical form.
            Value::Str(s) => crate::value::number::read(s, crate::value::number::Decimal::Point),
            _ => None,
        }
    }

    /// A total order for sorting — transitive by construction, via
    /// a per-value canonical key (see [`SortKey`]): null, then
    /// booleans, then the *magnitude lines* (numbers and instants
    /// as epoch seconds on the dimensionless line, durations as
    /// seconds on the line of `T`, quantities on their dimension's
    /// line by base magnitude, and text through its one reading —
    /// the text readings are disjoint by grammar), then readingless
    /// text, then lists and records by display form. Values meeting
    /// on one line compare by magnitude alone, so `PT1M` and `60_s`
    /// are equal, `1_km` and `1000_m` too; `60` is on another line
    /// (spec: Relations, draft 155), so `group` and `unique` keep
    /// "£30" and "30" apart.
    ///
    /// Ordering contexts with an adapter at hand should call
    /// [`Value::compare_with`] so mounted custom units read; this
    /// wrapper uses the frozen built-in table.
    pub fn compare(&self, other: &Value) -> std::cmp::Ordering {
        self.compare_with(other, &crate::value::quantity::scale_expr)
    }

    /// [`Value::compare`] with an explicit unit-expression resolver
    /// (the adapter's `unit_scale`), so custom-unit text takes its
    /// place on the magnitude line exactly as it does in criteria.
    pub fn compare_with(
        &self,
        other: &Value,
        scale: &dyn Fn(&str) -> Option<(f64, String)>,
    ) -> std::cmp::Ordering {
        sort_key(self, scale).cmp_key(&sort_key(other, scale))
    }

    /// The value's sort key under an explicit unit resolver — the
    /// hashable form of [`Value::compare_with`]'s equality, for the
    /// aggregates that partition by key.
    pub(crate) fn sort_key_with(&self, scale: &dyn Fn(&str) -> Option<(f64, String)>) -> SortKey {
        sort_key(self, scale)
    }
}

/// The canonical sort key behind [`Value::compare`]: every value
/// maps to exactly one class, so the order is total and transitive
/// where the old pairwise fragment activation was not (`10 < "1z" <
/// "9" < 10` cycled through the string fallback).
///
/// Equality and hashing agree with `cmp_key` exactly (a magnitude
/// hashes by its bit pattern, which is `total_cmp` equality), so
/// a `HashMap` keyed on sort keys partitions the same way the
/// pairwise comparison does — `group` and `unique_by` look a key
/// up in one probe instead of scanning every group formed so far.
#[derive(Debug, Clone)]
pub(crate) enum SortKey {
    Null,
    Bool(bool),
    /// The magnitude lines, one per dimension: everything with a
    /// numeric, temporal, durational, or unital reading, by its
    /// dimension (`""` for numbers and instants, `T` for durations
    /// and spans, the unit's dimension for a quantity; spec:
    /// Relations) and then by the reading's magnitude — and, where
    /// the magnitude is a whole number, the integer itself, so two
    /// 64-bit integers a double cannot tell apart still order and
    /// key exactly. Lines of different dimensions never meet: a
    /// quantity is equal, and ordered, only to one of its own
    /// dimension, and never to a bare number.
    Line(f64, i64, String),
    /// A period: by its start, the coarser first where two start
    /// together.
    Period(i64, u8),
    /// Text with no reading, by codepoint order.
    Text(String),
    /// Lists then records order after scalars, by display form.
    ListText(String),
    RecordText(String),
}

impl PartialEq for SortKey {
    fn eq(&self, other: &SortKey) -> bool {
        self.cmp_key(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for SortKey {}

impl std::hash::Hash for SortKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.rank().hash(state);
        match self {
            SortKey::Null => {}
            SortKey::Bool(b) => b.hash(state),
            SortKey::Line(f, i, dim) => {
                dim.hash(state);
                // -0.0 and 0.0 are one point of the line.
                (if *f == 0.0 { 0.0f64 } else { *f }).to_bits().hash(state);
                i.hash(state);
            }
            SortKey::Period(start, grain) => {
                start.hash(state);
                grain.hash(state);
            }
            SortKey::Text(s) | SortKey::ListText(s) | SortKey::RecordText(s) => s.hash(state),
        }
    }
}

impl SortKey {
    /// A point of the dimensionless line from a double: a whole
    /// number in range carries its integer, anything else zero.
    fn line(f: f64) -> SortKey {
        SortKey::line_of(f, String::new())
    }

    /// A point of the line of dimension `dim`.
    fn line_of(f: f64, dim: String) -> SortKey {
        let whole = f.fract() == 0.0 && f.abs() < 9.2e18;
        SortKey::Line(f, if whole { f as i64 } else { 0 }, dim)
    }

    fn rank(&self) -> u8 {
        match self {
            SortKey::Null => 0,
            SortKey::Bool(_) => 1,
            SortKey::Line(..) => 2,
            SortKey::Period(..) => 3,
            SortKey::Text(_) => 4,
            SortKey::ListText(_) => 5,
            SortKey::RecordText(_) => 6,
        }
    }

    pub(crate) fn cmp_key(&self, other: &SortKey) -> std::cmp::Ordering {
        use SortKey::*;
        match (self, other) {
            (Bool(a), Bool(b)) => a.cmp(b),
            // NaN (mintable by query arithmetic) orders after every
            // other number, whichever sign bit it carries (`0 div 0`
            // mints a negative NaN, which `total_cmp` alone would
            // put before -inf); two NaNs are equal.
            // The dimensionless line first, then the dimensions in
            // text order; magnitudes meet only within one line.
            (Line(a, i, da), Line(b, j, db)) => da.cmp(db).then_with(|| match (a.is_nan(), b.is_nan()) {
                (true, true) => i.cmp(j),
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => {
                    if *a == 0.0 && *b == 0.0 {
                        i.cmp(j)
                    } else {
                        a.total_cmp(b).then(i.cmp(j))
                    }
                }
            }),
            (Period(a, g), Period(b, h)) => a.cmp(b).then(g.cmp(h)),
            (Text(a), Text(b)) | (ListText(a), ListText(b)) | (RecordText(a), RecordText(b)) => {
                a.cmp(b)
            }
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

fn sort_key(v: &Value, scale: &dyn Fn(&str) -> Option<(f64, String)>) -> SortKey {
    match v {
        Value::Null => SortKey::Null,
        Value::Bool(b) => SortKey::Bool(*b),
        Value::Int(n) => SortKey::Line(*n as f64, *n, String::new()),
        Value::Float(f) => SortKey::line(*f),
        Value::Instant { secs, nanos, .. } => SortKey::line(*secs as f64 + *nanos as f64 / 1e9),
        Value::Period { start, grain, .. } => SortKey::Period(*start, *grain as u8),
        Value::Quantity { value, base, .. } => {
            SortKey::line_of(*value, crate::value::quantity::dimension_of(base))
        }
        // A library value keys by identity: equal only to itself,
        // ordered after lists and records.
        Value::Foreign(x) => {
            SortKey::RecordText(format!("\u{0}foreign:{:p}", std::sync::Arc::as_ptr(&x.0)))
        }
        // A node reference keys by its locator, after lists and
        // records: references sort in locator order.
        Value::Node(n) => SortKey::RecordText(format!("\u{0}node:{}", n.locator)),
        // A table keys by its literal, after lists and records.
        Value::Table(t) => SortKey::RecordText(format!("\u{0}table:{}", t.to_quarb())),
        // A string has at most one reading — numeric text carries
        // no unit, unital text excludes pure time (that is span
        // territory), span text needs its span grammar, ISO text
        // is none of those — so the priority order below never
        // actually chooses between readings.
        Value::Str(s) => {
            if let Some(f) = crate::value::number::read(s, crate::value::number::Decimal::Point)
                .and_then(|v| v.numeric())
            {
                SortKey::line(f)
            } else if let Some((bv, base, ..)) =
                crate::value::quantity::parse_unit_text_with(s, scale)
            {
                SortKey::line_of(bv, crate::value::quantity::dimension_of(&base))
            } else if let Some((secs, nanos)) = crate::temporal::parse_span(s) {
                SortKey::line_of(secs as f64 + nanos as f64 / 1e9, "T".to_string())
            } else if let Some((secs, nanos, _)) = crate::temporal::parse_iso(s) {
                SortKey::line(secs as f64 + nanos as f64 / 1e9)
            } else {
                SortKey::Text(s.clone())
            }
        }
        Value::List(_) => SortKey::ListText(v.to_string()),
        Value::Record(_) => SortKey::RecordText(v.to_string()),
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => Ok(()),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(x) => write!(f, "{}", format_float(*x)),
            Value::Str(s) => write!(f, "{s}"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "{}", parts.join(", "))
            }
            // A record's text is its Quarb form.
            Value::Record(_) => write!(f, "{}", self.to_quarb()),
            Value::Instant {
                secs,
                nanos,
                offset_min,
                ..
            } => write!(
                f,
                "{}",
                crate::temporal::format_instant(*secs, *nanos, *offset_min)
            ),
            Value::Period {
                start,
                grain,
                calendar,
            } => write!(
                f,
                "{}",
                crate::period::Period {
                    start: *start,
                    grain: *grain,
                    calendar: *calendar
                }
                .format()
            ),
            // The written form when the source had one (`42 km`),
            // the base otherwise; single-factor forms re-parse
            // through the unital reading.
            Value::Quantity {
                value,
                base,
                written,
            } => match written {
                // magnitude, underscore, unit as written (spec: the
                // output form of a quantity), so it pastes back; a
                // Cyrillic designation prints as its SI symbol here,
                // the locale re-spelling it
                Some((v, u)) => write!(
                    f,
                    "{}_{}",
                    format_float(*v),
                    crate::value::quantity::canonical_unit(u)
                ),
                // a span with no written unit prints in the full
                // mixed form, `1h 30min` (spec: Duration)
                None if base == "s" => {
                    write!(
                        f,
                        "{}",
                        crate::value::quantity::duration_text(*value, false)
                    )
                }
                None => write!(f, "{}_{}", format_float(*value), base),
            },
            Value::Foreign(x) => write!(f, "{}", x.display()),
            Value::Table(t) => write!(f, "{}", t.to_quarb()),
            Value::Node(n) => write!(f, "{}", n.locator),
        }
    }
}

/// The base-unit magnitudes of a comparison in which at least one
/// side is a quantity (spec: The Unital Reading), under an explicit
/// unit-expression resolver — the adapter's `unit_scale`, so
/// criterion text may use the mounted document's custom units. The
/// non-quantity side coerces through its unital reading. Dimensions
/// must agree — a base mismatch is no pair, so the comparison fails
/// rather than lies — and a bare number (or numeric text) carries
/// no dimension, so it pairs only with a quantity of dimension `1`
/// (spec: Relations, draft 155: a quantity is equal, and ordered,
/// only to a quantity of its own dimension; `30_km = 30` is false,
/// as is `60_s = 60`, time a dimension like the others).
pub(crate) fn quantital_pair_with(
    a: &Value,
    b: &Value,
    scale: &dyn Fn(&str) -> Option<(f64, String)>,
) -> Option<(f64, f64)> {
    let read = |v: &Value| -> Option<(f64, String, Option<String>)> {
        match v {
            Value::Quantity {
                value,
                base,
                written,
            } => Some((
                *value,
                base.clone(),
                written.as_ref().map(|(_, u)| u.clone()),
            )),
            // unit text, else ISO-8601 or span text, a span in seconds
            Value::Str(s) => crate::value::quantity::parse_unit_text_with(s, scale)
                .map(|(bv, b, _, wu)| (bv, b, Some(wu)))
                .or_else(|| {
                    crate::temporal::parse_span(s).map(|(secs, nanos)| {
                        (secs as f64 + nanos as f64 / 1e9, "s".to_string(), None)
                    })
                }),
            _ => None,
        }
    };
    // A bare number is dimensionless: it pairs with a ratio
    // (dimension `1`) and with nothing dimensioned.
    let ratio = |base: &str| crate::value::quantity::dimension_of(base) == "1";
    match (read(a), read(b)) {
        (Some((va, ba, _)), Some((vb, bb, _))) => (ba == bb).then_some((va, vb)),
        (Some((va, ba, _)), None) if ratio(&ba) => b.numeric().map(|n| (va, n)),
        (None, Some((vb, bb, _))) if ratio(&bb) => a.numeric().map(|n| (n, vb)),
        _ => None,
    }
}

/// Seconds as a float split into whole seconds and nanoseconds.
/// Seconds as a float split into whole seconds and nanoseconds, for
/// the bindings that hand a span to a host language.
pub fn split_seconds_pub(f: f64) -> (i64, u32) {
    split_seconds(f)
}

pub(crate) fn split_seconds(f: f64) -> (i64, u32) {
    let secs = f.floor();
    let nanos = ((f - secs) * 1e9).round();
    if nanos >= 1e9 {
        (secs as i64 + 1, 0)
    } else {
        (secs as i64, nanos as u32)
    }
}

/// `2025-`, the year as a word (spec: Temporal Kinds): digits and a
/// trailing dash, the sign before them for a year before the era.
fn year_word(s: &str) -> Option<crate::period::Period> {
    let body = s.strip_suffix('-')?;
    let (neg, digits) = match body.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, body),
    };
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = digits.parse().ok()?;
    crate::period::Period::of(
        if neg { -y } else { y },
        1,
        1,
        0,
        0,
        0,
        crate::period::Grain::Year,
    )
}

/// Render a float the way `Display` does (no trailing `.0` for whole
/// values). Arithmetic minted no infinity or NaN (an overflow is
/// null); a source that carries one still renders here, as text.
/// A float's text (spec: The Numeric Fragment): the shortest digit
/// string that reads back to the same double, as Python, JavaScript,
/// jq and PostgreSQL print it, in exponent form when the decimal
/// exponent is below -4 or at least 16, exactly Python's repr
/// (`2.89e-82`, `1e-05`, `1.2345678901234568e+29`), positional
/// otherwise (`0.001`, `1234567890123456`); a whole double prints
/// without a fraction (`3`), the engine's policy since the first
/// release. NaN and the infinities as Rust spells them.
pub(crate) fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == 0.0 {
        return f.to_string();
    }
    let sci = format!("{f:e}");
    let (mantissa, exponent) = sci.split_once('e').expect("an exponent");
    let exponent: i32 = exponent.parse().expect("a decimal exponent");
    if (-4..16).contains(&exponent) {
        return f.to_string();
    }
    format!(
        "{mantissa}e{}{:02}",
        if exponent < 0 { '-' } else { '+' },
        exponent.abs()
    )
}

/// JSON-quote a string (the escapes JSON requires; forward slash
/// left alone).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn floats_print_as_python_prints_them() {
        for (f, want) in [
            (2.89e-82, "2.89e-82"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (1.2345678901234568e29, "1.2345678901234568e+29"),
            (0.001, "0.001"),
            (0.0001, "0.0001"),
            (1234567890123456.0, "1234567890123456"),
            (3.0, "3"),
            (-5e-8, "-5e-08"),
            (0.0, "0"),
        ] {
            assert_eq!(format_float(f), want, "{f}");
            assert_eq!(Value::Float(f).to_string(), want);
        }
        assert_eq!(Value::Float(2.89e-82).to_json(), "2.89e-82");
    }

    use super::*;

    /// The hashable sort key partitions exactly as `compare_with`
    /// does: a numeric string, an integer and a float on the same
    /// magnitude share one key (so `group` merges them), text with
    /// no reading stays apart, and a quantity keys by its base
    /// magnitude.
    #[test]
    fn sort_key_hashes_like_it_compares() {
        use std::collections::HashSet;
        let scale = |_: &str| None;
        let keys: HashSet<SortKey> = [
            Value::Str("1".into()),
            Value::Int(1),
            Value::Float(1.0),
            Value::Str("x".into()),
            Value::Str("X".into()),
            Value::Null,
            Value::Bool(true),
        ]
        .iter()
        .map(|v| v.sort_key_with(&scale))
        .collect();
        assert_eq!(keys.len(), 5);
        assert_eq!(
            Value::Int(1).sort_key_with(&scale),
            Value::Str(" 1.0 ".into()).sort_key_with(&scale)
        );
        assert_ne!(
            Value::Str("x".into()).sort_key_with(&scale),
            Value::Str("y".into()).sort_key_with(&scale)
        );
    }

    #[test]
    fn json_rendering() {
        let obj = Value::Record(vec![
            ("name".into(), Value::Str("A\"da\n".into())),
            ("age".into(), Value::Int(36)),
            ("score".into(), Value::Float(2.5)),
            ("ok".into(), Value::Bool(true)),
            ("gone".into(), Value::Null),
            (
                "tags".into(),
                Value::list(vec![Value::Str("x".into()), Value::Int(1)]),
            ),
        ]);
        assert_eq!(
            obj.to_json(),
            r#"{"name": "A\"da\n", "age": 36, "score": 2.5, "ok": true, "gone": null, "tags": ["x", 1]}"#
        );
        // Display of a record is its Quarb form; an
        // empty record is falsy
        assert_eq!(
            obj.to_quarb(),
            "%(name = \"A\\\"da\n\"; age = 36; score = 2.5; ok = true; gone = null; tags = @(\"x\"; 1))"
        );
        assert_eq!(obj.to_string(), obj.to_quarb());
        assert!(!Value::Record(Vec::new()).is_truthy());
        assert_eq!(Value::list(Vec::new()).to_quarb(), "@()");
        assert_eq!(Value::Record(Vec::new()).to_quarb(), "%()");
        // a key that is not an identifier quotes
        assert_eq!(
            Value::Record(vec![("my-key".into(), Value::Int(1))]).to_quarb(),
            "%(\"my-key\" = 1)"
        );
        // top-level list display (string coercion) is unchanged
        // (join, not the Quarb form — that is display_form's job)
        assert_eq!(
            Value::list(vec![Value::Str("a".into()), Value::Str("b".into())]).to_string(),
            "a, b"
        );
    }
}

/// A string as a Quarb literal: double-quoted — the quote every
/// keyboard layout has — with `"`, `\`, and a `${` hole opener
/// escaped, so the result reparses exactly (a lone `$` is literal
/// in a double-quoted string). The single-quoted verbatim form
/// stays as input.
pub fn quarb_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '$' if chars.peek() == Some(&'{') => {
                out.push('\\');
                out.push('$');
            }
            // `(,` opens the rounded hole; `\(` is the literal.
            '(' if chars.peek() == Some(&',') => {
                out.push('\\');
                out.push('(');
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A record key as written in `%(k = v)`: bare when it is an
/// identifier that is not a reserved word, quoted otherwise.
pub fn quarb_key(k: &str) -> String {
    // An identifier in any script (a name is Unicode alphanumerics
    // and `_`), never a boolean word.
    let position = k.len() > 1 && k.starts_with('.') && k[1..].bytes().all(|c| c.is_ascii_digit());
    let bare = position
        || !k.is_empty()
            && k.chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_')
            && k.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !is_bool_word(k);
    if bare { k.to_string() } else { quarb_string(k) }
}
