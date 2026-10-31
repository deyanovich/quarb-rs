//! The scalar world.
//!
//! Navigation produces nodes; a projection turns each node into a
//! [`Value`]. Quarb is value-domain agnostic, so this is a small,
//! open set of scalar shapes an adapter can return.

use super::words::is_bool_word;
use std::fmt;

/// The value of a `q(…)` literal (spec: Quarqs): the body as parsed,
/// its canonical source, and — when the body is nothing but a
/// structure of projectors (`q(::x)`, `q(%(::Name; ::Fare))`) — that
/// structure, which is what the literal then answers: a projector is
/// the simplest quarq and keeps its own type word.
#[derive(Debug, Clone)]
pub struct Quarq {
    /// The canonical spelling, `q(+ 3)`.
    pub source: String,
    /// The commands of the body, as after a pipe.
    pub body: std::sync::Arc<Vec<crate::syntax::ast::Command>>,
    /// The projector or structured projector the body spells, when
    /// it spells one and nothing else.
    pub projector: Option<Box<Value>>,
}

impl PartialEq for Quarq {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

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
    /// A record: insertion-ordered fields (spec: Record). Field
    /// order is significant, matching kaiv namespaces and JSON key
    /// order. The keys are scalars, text as a rule (draft 166): the
    /// [`Record`] holds every key's text, the typed keys beside it
    /// where any is no text, and the fields' declarations.
    Record(Record),
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
    /// A projector (spec: Projectors, draft 164):
    /// a ladder — 1 the core field `:`, 2 the property `::`, 3 the
    /// adapter metadata `:::` — and its key. It prints as the
    /// projection is written (`::title`, the key quoted where it is
    /// no identifier) and coerces to text as the bare key. A TYPED
    /// projector (draft 166) carries a declared type in the trait
    /// brackets, `:A<int>`: it declares the field it names and
    /// stands only where a field is declared (a header item, a
    /// record key, a structured projector's item, `map`'s body);
    /// `:A<*; float>` is how a header reports an undeclared column
    /// beside its inferred type, and declares nothing.
    Projection {
        ladder: u8,
        key: String,
        typing: Option<Typing>,
    },
    /// A quarq (spec: Quarqs, draft 166): a command as it stands
    /// after a pipe, read as Quarb and not run — the value of the
    /// `q(…)` literal, the sibling of `sh(…)`. It prints as its
    /// source, two are equal when their source is, `| type` reads
    /// `quarq`; nothing runs it but the glued colon and the commands
    /// that take a command (`map`, the keyed aggregates), where it
    /// is late bound: `$_`, the registers and the cursor inside it
    /// are read where it is applied.
    Quarq(Quarq),
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

/// Fields whose names are paths of levels (`bar:A`, `bar:B`, `baz`)
/// nested into a record by level (spec: The Table, "Levels of the
/// header"): the outer names in first-met order, each holding the
/// record of what stands under it.
fn nest_by_levels(fields: Vec<(String, Value)>) -> Value {
    if !fields.iter().any(|(k, _)| k.contains(':')) {
        return Value::record(fields);
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    let mut groups: Vec<(String, Vec<(String, Value)>)> = Vec::new();
    for (k, v) in fields {
        match k.split_once(':') {
            Some((outer, inner)) => match groups.iter_mut().find(|(o, _)| *o == outer) {
                Some((_, g)) => g.push((inner.to_string(), v)),
                None => {
                    out.push((outer.to_string(), Value::Null));
                    groups.push((outer.to_string(), vec![(inner.to_string(), v)]));
                }
            },
            None => out.push((k, v)),
        }
    }
    for (o, g) in groups {
        if let Some(slot) = out
            .iter_mut()
            .find(|(k, v)| *k == o && matches!(v, Value::Null))
        {
            slot.1 = nest_by_levels(g);
        }
    }
    Value::record(out)
}

/// The cells nested level by level under their labels (spec: The
/// Table, "The index"): at each level a record keyed by the distinct
/// values of that level in first-met order, the innermost level
/// holding the cell.
fn nest_levels(levels: &[Vec<Value>], cells: &[Value], depth: usize) -> Value {
    let width = levels.first().map_or(0, Vec::len);
    if depth + 1 >= width {
        return Value::Record(Record::keyed(
            levels
                .iter()
                .zip(cells)
                .map(|(l, c)| (l.get(depth).cloned().unwrap_or(Value::Null), c.clone()))
                .collect(),
        ));
    }
    let mut keys: Vec<Value> = Vec::new();
    for l in levels {
        let k = l.get(depth).cloned().unwrap_or(Value::Null);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    Value::Record(Record::keyed(
        keys.into_iter()
            .map(|k| {
                let (sub_levels, sub_cells): (Vec<Vec<Value>>, Vec<Value>) = levels
                    .iter()
                    .zip(cells)
                    .filter(|(l, _)| l.get(depth) == Some(&k))
                    .map(|(l, c)| (l.clone(), c.clone()))
                    .unzip();
                let v = nest_levels(&sub_levels, &sub_cells, depth + 1);
                (k, v)
            })
            .collect(),
    ))
}

/// A record's fields (spec: Record, draft 166): the key's text and
/// the value per field, in order; beside them the keys as VALUES
/// where any key is no text (`%(1 = "a"; 2013-01-01 = 5)`: an
/// integer and an instant, equal as values, `1` and `"1"` two keys)
/// and the declaration per field (`%(price<float> = …)`, the field
/// then fixed to its type). A record with text keys and no
/// declaration carries nothing beside its fields, the common case.
/// It derefs to the fields, so a reader sees `Vec<(String, Value)>`
/// as ever; a text key's value is its text.
#[derive(Debug, Clone, Default)]
pub struct Record {
    fields: Vec<(String, Value)>,
    /// The keys as values, one per field, where any is no text.
    keys: Option<Vec<Value>>,
    /// The declared type per field, where any field is declared.
    declared: Option<Vec<Option<ColumnType>>>,
}

impl Record {
    /// A record of text keys.
    pub fn new(fields: Vec<(String, Value)>) -> Record {
        Record {
            fields,
            keys: None,
            declared: None,
        }
    }

    /// A record keyed by values: a text key stays text, any other
    /// scalar is held typed beside its text form.
    pub fn keyed(pairs: Vec<(Value, Value)>) -> Record {
        let typed = pairs.iter().any(|(k, _)| !matches!(k, Value::Str(_)));
        let keys = typed.then(|| pairs.iter().map(|(k, _)| k.clone()).collect());
        let fields = pairs
            .into_iter()
            .map(|(k, v)| (Record::key_text(&k), v))
            .collect();
        Record {
            fields,
            keys,
            declared: None,
        }
    }

    /// The text of a key: a text as it is, any other scalar in its
    /// display form (the text `:name` reads it by).
    pub fn key_text(k: &Value) -> String {
        match k {
            Value::Str(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// The key of field `i` as a value: the typed key, or the text.
    pub fn key(&self, i: usize) -> Value {
        match self.keys.as_ref().and_then(|k| k.get(i)) {
            Some(k) => k.clone(),
            None => Value::Str(
                self.fields
                    .get(i)
                    .map(|(k, _)| k.clone())
                    .unwrap_or_default(),
            ),
        }
    }

    /// The keys as values, in order (`| keys`).
    pub fn key_values(&self) -> Vec<Value> {
        (0..self.fields.len()).map(|i| self.key(i)).collect()
    }

    /// Whether any key is no text.
    pub fn has_typed_keys(&self) -> bool {
        self.keys.is_some()
    }

    /// The field whose key equals `k` AS A VALUE (`r[[k]]`): a text
    /// key equals its text, a typed key its value.
    pub fn find_key(&self, k: &Value) -> Option<usize> {
        (0..self.fields.len()).find(|&i| self.key(i) == *k)
    }

    /// The field under the text key `k` (`:name`).
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.fields.iter().find(|(f, _)| f == k).map(|(_, v)| v)
    }

    /// The declared type of field `i`, `Some` where it is declared.
    pub fn declaration(&self, i: usize) -> Option<ColumnType> {
        self.declared
            .as_ref()
            .and_then(|d| d.get(i))
            .cloned()
            .flatten()
    }

    /// Whether any field is declared.
    pub fn has_declarations(&self) -> bool {
        self.declared
            .as_ref()
            .is_some_and(|d| d.iter().any(Option::is_some))
    }

    /// Field `i` declared `ty` (`None` undeclares it).
    pub fn declare(&mut self, i: usize, ty: Option<ColumnType>) {
        let d = self
            .declared
            .get_or_insert_with(|| vec![None; self.fields.len()]);
        d.resize(self.fields.len(), None);
        d[i] = ty;
        if d.iter().all(Option::is_none) {
            self.declared = None;
        }
    }

    /// A field appended under a text key (a `push((k, v))` through
    /// the deref says the same).
    pub fn push_field(&mut self, k: String, v: Value) {
        self.push_keyed(Value::Str(k), v, None);
    }

    /// A field appended under a key value, with its declaration.
    pub fn push_keyed(&mut self, k: Value, v: Value, ty: Option<ColumnType>) {
        let n = self.fields.len();
        if !matches!(k, Value::Str(_)) {
            let keys = self.keys.get_or_insert_with(|| vec![Value::Null; n]);
            keys.resize(n, Value::Null);
            // the text keys before it, held as text
            for (i, slot) in keys.iter_mut().enumerate() {
                if matches!(slot, Value::Null) {
                    *slot = Value::Str(self.fields[i].0.clone());
                }
            }
            keys.push(k.clone());
        } else if let Some(keys) = &mut self.keys {
            keys.resize(n, Value::Null);
            keys.push(k.clone());
        }
        if let Some(d) = &mut self.declared {
            d.resize(n, None);
            d.push(ty);
        } else if ty.is_some() {
            let mut d = vec![None; n];
            d.push(ty);
            self.declared = Some(d);
        }
        self.fields.push((Record::key_text(&k), v));
    }

    /// The value of field `i` replaced.
    pub fn set(&mut self, i: usize, v: Value) {
        if let Some(f) = self.fields.get_mut(i) {
            f.1 = v;
        }
    }

    /// The fields alone, the keys as text.
    pub fn into_fields(self) -> Vec<(String, Value)> {
        self.fields
    }

    /// The record with every value replaced by `f`'s, the keys and
    /// the declarations kept.
    pub fn map_values(self, mut f: impl FnMut(Value) -> Value) -> Record {
        Record {
            fields: self.fields.into_iter().map(|(k, v)| (k, f(v))).collect(),
            keys: self.keys,
            declared: self.declared,
        }
    }

    /// The record with its values replaced in order (`shift` keeps
    /// the keys and the declarations).
    pub fn with_values(&self, values: Vec<Value>) -> Record {
        Record {
            fields: self
                .fields
                .iter()
                .zip(values)
                .map(|((k, _), v)| (k.clone(), v))
                .collect(),
            keys: self.keys.clone(),
            declared: self.declared.clone(),
        }
    }

    /// The record with the fields `keep(i)` admits, the keys and
    /// declarations kept in step.
    pub fn filter_fields(self, mut keep: impl FnMut(usize, &(String, Value)) -> bool) -> Record {
        let kept: Vec<bool> = self
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| keep(i, f))
            .collect();
        let pick = |v: Vec<Value>| -> Vec<Value> {
            v.into_iter()
                .zip(&kept)
                .filter(|(_, k)| **k)
                .map(|(x, _)| x)
                .collect()
        };
        Record {
            fields: self
                .fields
                .into_iter()
                .zip(&kept)
                .filter(|(_, k)| **k)
                .map(|(f, _)| f)
                .collect(),
            keys: self.keys.map(pick),
            declared: self.declared.map(|d| {
                d.into_iter()
                    .zip(&kept)
                    .filter(|(_, k)| **k)
                    .map(|(x, _)| x)
                    .collect()
            }),
        }
    }

    /// The key of field `i` in Quarb's text: a text key bare where
    /// it is an identifier, quoted otherwise; a typed key as its
    /// literal.
    pub fn key_quarb(&self, i: usize) -> String {
        match self.keys.as_ref().and_then(|k| k.get(i)) {
            Some(Value::Str(_)) | None => quarb_key(&self.fields[i].0),
            Some(k) => k.to_quarb(),
        }
    }
}

impl PartialEq for Record {
    /// Two records are equal item by item: the keys as values and
    /// the values; a declaration is no part of the value.
    fn eq(&self, other: &Record) -> bool {
        self.fields.len() == other.fields.len()
            && self
                .fields
                .iter()
                .zip(other.fields.iter())
                .enumerate()
                .all(|(i, ((_, a), (_, b)))| a == b && self.key(i) == other.key(i))
    }
}

impl std::ops::Deref for Record {
    type Target = Vec<(String, Value)>;
    fn deref(&self) -> &Vec<(String, Value)> {
        &self.fields
    }
}

impl std::ops::DerefMut for Record {
    /// Writes through to the fields; a push through here appends a
    /// text, undeclared field (the accessors read a shorter `keys`
    /// or `declared` as text and undeclared).
    fn deref_mut(&mut self) -> &mut Vec<(String, Value)> {
        &mut self.fields
    }
}

impl From<Vec<(String, Value)>> for Record {
    fn from(fields: Vec<(String, Value)>) -> Record {
        Record::new(fields)
    }
}

impl FromIterator<(String, Value)> for Record {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Record {
        Record::new(iter.into_iter().collect())
    }
}

impl IntoIterator for Record {
    type Item = (String, Value);
    type IntoIter = std::vec::IntoIter<(String, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.fields.into_iter()
    }
}

impl<'a> IntoIterator for &'a Record {
    type Item = &'a (String, Value);
    type IntoIter = std::slice::Iter<'a, (String, Value)>;
    fn into_iter(self) -> Self::IntoIter {
        self.fields.iter()
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
    /// The declared type per column (spec: The Table, "Columns",
    /// draft 166): `Some` where a typed projector declared the
    /// column, which is then fixed to it; `None` where the column
    /// is undeclared and typed by its cells. Empty when no column
    /// is declared (the common case, every file read).
    pub declared: Vec<Option<ColumnType>>,
    /// The number of leading columns that are the INDEX (spec: The
    /// Table, "The index", draft 166): 0 on a plain table, 1 where
    /// `#(-i=:col)` named a column the labels of the rows (the
    /// column stays a column, first in the header); more under a
    /// composite index. The labels are the keys of the records the
    /// table answers (`t:A`), looked up by `t[[label]]`.
    pub index: usize,
    /// The header's name (`#(--header=…; --name=word)`, draft 166),
    /// read back by `:::name`; `column` is the no-name default on
    /// both axes, so `transpose` exchanges it with the index
    /// column's name and undoes itself.
    pub name: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnType {
    Boolean,
    Integer,
    Float,
    Instant,
    Duration,
    Period,
    Quantity,
    /// A column of node references (spec: The graph as a table):
    /// the `node` column of `@| nodes`, `source` and `target` of
    /// `@| edges`. The cells stay references — node operands inside
    /// the query (`:source:.l`) — and print as the node's path.
    Node,
    Text,
    /// A TUPLE column (spec: The Table, draft 166), declared only:
    /// `key<(int; text)>` holds lists of that arity and those types,
    /// compared item by item, printed in the quarb form; never
    /// inferred, it arises by declaration or as the flattened index.
    Tuple(std::sync::Arc<Vec<ColumnType>>),
}

impl ColumnType {
    /// The type's word, as `| type` lists it (`(int; text)` for a
    /// tuple).
    pub fn word(&self) -> String {
        match self {
            ColumnType::Boolean => "bool".to_string(),
            ColumnType::Integer => "int".to_string(),
            ColumnType::Float => "float".to_string(),
            ColumnType::Instant => "instant".to_string(),
            ColumnType::Duration => "duration".to_string(),
            ColumnType::Period => "period".to_string(),
            ColumnType::Quantity => "quantity".to_string(),
            ColumnType::Node => "node".to_string(),
            ColumnType::Text => "text".to_string(),
            ColumnType::Tuple(items) => {
                format!(
                    "({})",
                    items
                        .iter()
                        .map(ColumnType::word)
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            }
        }
    }

    /// The type named by its word, as a declared header names it
    /// (`#(--header=@(:age<int>))`); a tuple by its words in
    /// parentheses, `(int; text)`; `None` for no such word.
    pub fn from_word(word: &str) -> Option<ColumnType> {
        if let Some(inner) = word.strip_prefix('(').and_then(|w| w.strip_suffix(')')) {
            let items: Option<Vec<ColumnType>> = inner
                .split([';', ','])
                .map(|w| ColumnType::from_word(w.trim()))
                .collect();
            return items
                .filter(|v| !v.is_empty())
                .map(|v| ColumnType::Tuple(std::sync::Arc::new(v)));
        }
        ColumnType::ORDER.into_iter().find(|t| t.word() == word)
    }

    /// The words, in lattice order, for a refusal to list.
    pub fn words() -> Vec<String> {
        ColumnType::ORDER.iter().map(ColumnType::word).collect()
    }

    const ORDER: [ColumnType; 9] = [
        ColumnType::Boolean,
        ColumnType::Integer,
        ColumnType::Float,
        ColumnType::Instant,
        ColumnType::Duration,
        ColumnType::Period,
        ColumnType::Quantity,
        ColumnType::Node,
        ColumnType::Text,
    ];

    /// The type a value is of, as a column would hold it; `None` on
    /// null.
    pub fn of_value(v: &Value) -> Option<ColumnType> {
        ColumnType::of(v)
    }

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
            Value::Node(_) => ColumnType::Node,
            _ => ColumnType::Text,
        })
    }
}

/// What a projector's trait brackets say of the field it names
/// (spec: Projectors, draft 166): `<int>` DECLARES the type, the
/// field then fixed to it, a value cast into it or refused; `<*>`
/// declares nothing, and `<*; float>` is the star beside the type a
/// header reports for an undeclared column, a report and no request
/// (ignored where the header is applied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typing {
    Declared(ColumnType),
    Star(Option<ColumnType>),
}

impl Typing {
    /// The brackets' text: `<int>`, `<(int; text)>`, `<*>`, `<*; float>`.
    pub fn spelling(&self) -> String {
        match self {
            Typing::Declared(t) => format!("<{}>", t.word()),
            Typing::Star(None) => "<*>".to_string(),
            Typing::Star(Some(t)) => format!("<*; {}>", t.word()),
        }
    }

    /// The type declared, `None` under the star.
    pub fn declared(&self) -> Option<ColumnType> {
        match self {
            Typing::Declared(t) => Some(t.clone()),
            Typing::Star(_) => None,
        }
    }
}

/// `v` cast to the declared column type `t`, `None` where it does
/// not cast: the coercion of [`coerce_cell`], a whole-valued float
/// under a declared integer (the cast is lossless), and under a
/// tuple a list of the arity whose items cast each.
pub(crate) fn cast_cell(v: &Value, t: &ColumnType) -> Option<Value> {
    if let ColumnType::Tuple(items) = t {
        if matches!(v, Value::Null) {
            return Some(Value::Null);
        }
        let Value::List(cells) = v else { return None };
        if cells.len() != items.len() {
            return None;
        }
        let cast: Option<Vec<Value>> = cells
            .iter()
            .zip(items.iter())
            .map(|(c, ty)| cast_cell(c, ty))
            .collect();
        return cast.map(Value::list);
    }
    coerce_cell(v, t).or_else(|| match (v, t) {
        (Value::Float(f), ColumnType::Integer) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
            Some(Value::Int(*f as i64))
        }
        _ => None,
    })
}

/// The cell as a refusal shows it: a text quoted, the rest in its
/// display form.
pub(crate) fn held_text(c: &Value) -> String {
    match c {
        Value::Str(s) => format!("\"{s}\""),
        other => other.display_form(),
    }
}

/// `v` coerced to the column type `t`, `None` where it has no such
/// reading. Text that reads as a type counts as that type; a
/// boolean reads as 1 and 0 among integers, an integer as a float;
/// every scalar has a text form.
fn coerce_cell(v: &Value, t: &ColumnType) -> Option<Value> {
    use ColumnType as T;
    if matches!(v, Value::Null) {
        return Some(Value::Null);
    }
    match t {
        // a tuple is cast, never coerced: see `cast_cell`
        T::Tuple(_) => None,
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
        // a reference stays one; nothing else is a node
        T::Node => match v {
            Value::Node(_) => Some(v.clone()),
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
pub(crate) fn type_column(cells: Vec<Value>) -> (ColumnType, Vec<Value>) {
    for t in ColumnType::ORDER {
        let coerced: Option<Vec<Value>> = cells.iter().map(|c| coerce_cell(c, &t)).collect();
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
    /// Every column is undeclared.
    pub fn new(header: Option<Vec<String>>, rows: Vec<Vec<Value>>) -> Table {
        Table::typed(header, rows, Vec::new())
            .unwrap_or_else(|_| unreachable!("no column is declared"))
    }

    /// A table built under a declared header (spec: The Table,
    /// `#(--header=@(:a<int>; :b))`): a column declared with a type
    /// has every cell cast to it, and a cell that does not cast
    /// refuses the build naming the column and the first offending
    /// row; a column declared without a type is typed as `new` types
    /// it. The rows are expected flat and of the header's width.
    pub fn declared(
        header: Vec<(String, Option<ColumnType>)>,
        rows: Vec<Vec<Value>>,
    ) -> Result<Table, String> {
        let names: Vec<String> = header.iter().map(|(n, _)| n.clone()).collect();
        let declared: Vec<Option<ColumnType>> = header.iter().map(|(_, t)| t.clone()).collect();
        Table::typed(Some(names), rows, declared)
    }

    /// The one builder (spec: The Table, "Columns", draft 166): the
    /// header, the rows, and the declaration per column (`declared`
    /// shorter than the width is padded with undeclared columns).
    /// A declared column has every cell cast into its type, a cell
    /// that does not cast refusing by column and row; an undeclared
    /// column takes the least type its cells coerce to. Rows
    /// shorter than the width are null-padded.
    pub fn typed(
        header: Option<Vec<String>>,
        rows: Vec<Vec<Value>>,
        mut declared: Vec<Option<ColumnType>>,
    ) -> Result<Table, String> {
        let mut t = Table {
            header,
            rows,
            declared: Vec::new(),
            index: 0,
            name: None,
        };
        let w = t.width();
        declared.resize(w, None);
        if declared.iter().all(Option::is_none) {
            declared.clear();
        }
        t.declared = declared;
        if w == 0 {
            return Ok(t);
        }
        for r in &mut t.rows {
            r.resize(w, Value::Null);
        }
        if t.rows.is_empty() {
            return Ok(t);
        }
        for j in 0..w {
            let cells: Vec<Value> = t.rows.iter().map(|r| r[j].clone()).collect();
            let coerced = match t.declared.get(j).cloned().flatten() {
                Some(ty) => {
                    let mut out = Vec::with_capacity(cells.len());
                    for (i, c) in cells.iter().enumerate() {
                        match cast_cell(c, &ty) {
                            Some(v) => out.push(v),
                            None => {
                                return Err(format!(
                                    "column '{}' is declared {}, and row {} holds {}, which does not cast to it",
                                    t.column_name(j),
                                    ty.word(),
                                    i + 1,
                                    held_text(c),
                                ));
                            }
                        }
                    }
                    out
                }
                None => type_column(cells).1,
            };
            for (r, c) in t.rows.iter_mut().zip(coerced) {
                r[j] = c;
            }
        }
        Ok(t)
    }

    /// The table with new rows under the same header and the same
    /// declarations (spec: The Table, "Columns"): what every command
    /// that keeps the columns and changes the cells rebuilds through
    /// — a declared column casts the new cells or refuses naming it,
    /// an undeclared one re-types by its cells.
    pub fn rebuild(&self, rows: Vec<Vec<Value>>) -> Result<Table, String> {
        Table::typed(self.header.clone(), rows, self.declared.clone())
            .map(|t| t.with_index(self.index).with_name(self.name.clone()))
    }

    /// The table with `n` leading columns as its index (0 none).
    pub fn with_index(mut self, n: usize) -> Table {
        self.index = n.min(self.width());
        self
    }

    /// `| #` with no option: the index role dropped.
    pub fn drop_index(self) -> Table {
        self.with_index(0)
    }

    /// The table indexed by the named column (spec: The Table, "The
    /// index"): the column moved first, declared where the projector
    /// declares it, and marked the index; a name the table lacks
    /// refuses. On the array face the name is a position.
    pub fn indexed_by(&self, name: &str, ty: Option<ColumnType>) -> Result<Table, String> {
        self.indexed_by_levels(&[(name.to_string(), ty)])
    }

    /// The table indexed by several columns, the levels of a
    /// composite index outer first (spec: The Table, "The index"):
    /// the columns moved first in that order, each declared where its
    /// projector declares it, and marked the index.
    pub fn indexed_by_levels(
        &self,
        levels: &[(String, Option<ColumnType>)],
    ) -> Result<Table, String> {
        let w = self.width();
        let mut order: Vec<usize> = Vec::with_capacity(w);
        for (name, _) in levels {
            let j = match &self.header {
                Some(h) => h.iter().position(|c| c == name),
                None => None,
            }
            .or_else(|| name.parse::<usize>().ok().filter(|j| *j >= 1 && *j <= w).map(|j| j - 1))
            .ok_or_else(|| {
                format!(
                    "--index names column '{name}', and the table has no such column (its columns: {})",
                    (0..w).map(|j| self.column_name(j)).collect::<Vec<_>>().join(", ")
                )
            })?;
            if order.contains(&j) {
                return Err(format!("--index names column '{name}' twice"));
            }
            order.push(j);
        }
        let n = order.len();
        let chosen = order.clone();
        order.extend((0..w).filter(|k| !chosen.contains(k)));
        let header = self
            .header
            .as_ref()
            .map(|h| order.iter().map(|k| h[*k].clone()).collect());
        let mut declared: Vec<Option<ColumnType>> =
            order.iter().map(|k| self.declaration(*k)).collect();
        for (i, (_, ty)) in levels.iter().enumerate() {
            if ty.is_some() {
                declared[i] = ty.clone();
            }
        }
        let rows = self
            .rows
            .iter()
            .map(|r| {
                order
                    .iter()
                    .map(|k| r.get(*k).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();
        Ok(Table::typed(header, rows, declared)?.with_index(n))
    }

    /// The labels (`:::index`): the index column's cells, or under a
    /// composite index the record of each row's levels keyed by the
    /// level names; `None` on a table with no index.
    pub fn labels(&self) -> Option<Vec<Value>> {
        if self.index == 0 {
            return None;
        }
        if self.index == 1 {
            return Some(
                self.rows
                    .iter()
                    .map(|r| r.first().cloned().unwrap_or(Value::Null))
                    .collect(),
            );
        }
        let names: Vec<String> = (0..self.index).map(|j| self.column_name(j)).collect();
        Some(
            self.rows
                .iter()
                .map(|r| {
                    Value::record(
                        names
                            .iter()
                            .enumerate()
                            .map(|(j, n)| (n.clone(), r.get(j).cloned().unwrap_or(Value::Null)))
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    /// A row's label as one name: the levels joined by `:` (a header
    /// path), one level its text.
    pub fn label_name(&self, row: &[Value]) -> String {
        row[..self.index]
            .iter()
            .map(Record::key_text)
            .collect::<Vec<_>>()
            .join(":")
    }

    /// The level names of the index, outer first.
    pub fn level_names(&self) -> Vec<String> {
        (0..self.index).map(|j| self.column_name(j)).collect()
    }

    /// The rows whose leading levels equal `levels`, each compared
    /// as a value against its column's type.
    pub fn rows_at_levels(&self, levels: &[Value]) -> Vec<usize> {
        let types = self.types();
        let wanted: Vec<Value> = levels
            .iter()
            .enumerate()
            .map(|(j, l)| match types.get(j) {
                Some(t) => cast_cell(l, t).unwrap_or_else(|| l.clone()),
                None => l.clone(),
            })
            .collect();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| wanted.iter().enumerate().all(|(j, w)| r.get(j) == Some(w)))
            .map(|(i, _)| i)
            .collect()
    }

    /// The columns under the outer name `prefix` of a header with
    /// levels (spec: The Table, "Levels of the header"): `t:bar` on a
    /// header `bar:A`, `bar:B`, `baz:A` is the table of `A` and `B`
    /// under the index; `None` where no column stands under the name.
    pub fn sub_columns(&self, prefix: &str) -> Option<Table> {
        let h = self.header.as_ref()?;
        let lead = format!("{prefix}:");
        let under: Vec<usize> = (self.index..self.width())
            .filter(|&j| h[j].starts_with(&lead))
            .collect();
        if under.is_empty() {
            return None;
        }
        let order: Vec<usize> = (0..self.index).chain(under.iter().copied()).collect();
        let header: Vec<String> = order
            .iter()
            .map(|&j| {
                if j < self.index {
                    h[j].clone()
                } else {
                    h[j][lead.len()..].to_string()
                }
            })
            .collect();
        let declared: Vec<Option<ColumnType>> =
            order.iter().map(|&j| self.declaration(j)).collect();
        let rows: Vec<Vec<Value>> = self
            .rows
            .iter()
            .map(|r| {
                order
                    .iter()
                    .map(|&j| r.get(j).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();
        Some(
            Table::typed(Some(header), rows, declared)
                .unwrap_or_else(|_| Table::new(None, Vec::new()))
                .with_index(self.index),
        )
    }

    /// Whether the header has levels: a column name with a colon.
    pub fn header_has_levels(&self) -> bool {
        self.header
            .as_ref()
            .is_some_and(|h| h[self.index..].iter().any(|n| n.contains(':')))
    }

    /// `stack` (spec: The Table, "Levels of the header"): the innermost
    /// level of the header moved into the index. A flat header makes a
    /// one-column table, `value`, indexed by the labels and the column
    /// name under `variable`; a header with levels keeps its outer
    /// levels as columns. Refused where the table has no index.
    pub fn stacked(&self) -> Result<Table, String> {
        if self.index == 0 {
            return Err("stack moves the header's innermost level into the index, and the table has no index: name one with #(-i=:col)".into());
        }
        let h = self
            .header
            .clone()
            .unwrap_or_else(|| (1..=self.width()).map(|j| j.to_string()).collect());
        let data: Vec<usize> = (self.index..self.width()).collect();
        // the innermost level of each data column's name, and what
        // stands before it
        let split = |name: &str| -> (Option<String>, String) {
            match name.rsplit_once(':') {
                Some((outer, inner)) => (Some(outer.to_string()), inner.to_string()),
                None => (None, name.to_string()),
            }
        };
        let mut inner_values: Vec<String> = Vec::new();
        let mut outers: Vec<Option<String>> = Vec::new();
        for &j in &data {
            let (outer, inner) = split(&h[j]);
            if !inner_values.contains(&inner) {
                inner_values.push(inner);
            }
            if !outers.contains(&outer) {
                outers.push(outer);
            }
        }
        let mut header: Vec<String> = h[..self.index].to_vec();
        header.push("variable".to_string());
        let new_cols: Vec<Option<String>> = outers.clone();
        header.extend(
            new_cols
                .iter()
                .map(|o| o.clone().unwrap_or_else(|| "value".to_string())),
        );
        let mut rows: Vec<Vec<Value>> = Vec::new();
        for r in &self.rows {
            for inner in &inner_values {
                let mut row: Vec<Value> = r[..self.index].to_vec();
                row.push(Value::Str(inner.clone()));
                for outer in &new_cols {
                    let name = match outer {
                        Some(o) => format!("{o}:{inner}"),
                        None => inner.clone(),
                    };
                    let cell = h
                        .iter()
                        .position(|n| *n == name)
                        .and_then(|j| r.get(j).cloned())
                        .unwrap_or(Value::Null);
                    row.push(cell);
                }
                rows.push(row);
            }
        }
        Ok(Table::new(Some(header), rows).with_index(self.index + 1))
    }

    /// `unstack`: the innermost level of the index moved into the
    /// header. With one data column the new columns are the level's
    /// values; with several, paths `column:value`. Refused where the
    /// index has no level to move.
    pub fn unstacked(&self) -> Result<Table, String> {
        if self.index == 0 {
            return Err("unstack moves the index's innermost level into the header, and the table has no index level to move".into());
        }
        let h = self
            .header
            .clone()
            .unwrap_or_else(|| (1..=self.width()).map(|j| j.to_string()).collect());
        let k = self.index - 1;
        let data: Vec<usize> = (self.index..self.width()).collect();
        let mut values: Vec<Value> = Vec::new();
        for r in &self.rows {
            let v = r.get(k).cloned().unwrap_or(Value::Null);
            if !values.contains(&v) {
                values.push(v);
            }
        }
        let mut header: Vec<String> = h[..k].to_vec();
        for &j in &data {
            for v in &values {
                header.push(if data.len() == 1 {
                    Record::key_text(v)
                } else {
                    format!("{}:{}", h[j], Record::key_text(v))
                });
            }
        }
        // one row per distinct outer label, in first-met order
        let mut keys: Vec<Vec<Value>> = Vec::new();
        for r in &self.rows {
            let key = r[..k].to_vec();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let rows: Vec<Vec<Value>> = keys
            .iter()
            .map(|key| {
                let mut row = key.clone();
                for &j in &data {
                    for v in &values {
                        let cell = self
                            .rows
                            .iter()
                            .find(|r| r[..k] == key[..] && r.get(k) == Some(v))
                            .and_then(|r| r.get(j).cloned())
                            .unwrap_or(Value::Null);
                        row.push(cell);
                    }
                }
                row
            })
            .collect();
        Ok(Table::new(Some(header), rows).with_index(k))
    }

    /// The sub-table under the first `k` levels dropped: the rows
    /// given, the leading `k` columns left out, indexed by what
    /// remains.
    pub fn sub_table(&self, rows: &[usize], k: usize) -> Table {
        let header = self.header.as_ref().map(|h| h[k..].to_vec());
        let declared: Vec<Option<ColumnType>> =
            (k..self.width()).map(|j| self.declaration(j)).collect();
        let body: Vec<Vec<Value>> = rows.iter().map(|&i| self.rows[i][k..].to_vec()).collect();
        Table::typed(header, body, declared)
            .unwrap_or_else(|_| {
                Table::new(
                    self.header.as_ref().map(|h| h[k..].to_vec()),
                    rows.iter().map(|&i| self.rows[i][k..].to_vec()).collect(),
                )
            })
            .with_index(self.index.saturating_sub(k))
    }

    /// The rows carrying `label`, compared as a value against the
    /// index's type (a text label cast to it where it reads).
    pub fn rows_labelled(&self, label: &Value) -> Vec<usize> {
        let Some(labels) = self.labels() else {
            return Vec::new();
        };
        let ty = self.types().first().cloned();
        let wanted = match ty {
            Some(t) => cast_cell(label, &t).unwrap_or_else(|| label.clone()),
            None => label.clone(),
        };
        labels
            .iter()
            .enumerate()
            .filter(|(_, l)| **l == wanted)
            .map(|(i, _)| i)
            .collect()
    }

    /// A column by name as the table answers it: on an indexed table
    /// the RECORD of the column keyed by the labels (refused where a
    /// label repeats, naming it), on a plain table the list; a name
    /// no column bears is null.
    pub fn column_value(&self, name: &str) -> Result<Value, String> {
        let Some(list) = self.column(name) else {
            // `t:bar` on a header with levels: the columns under the name
            return Ok(self
                .sub_columns(name)
                .map(Value::table)
                .unwrap_or(Value::Null));
        };
        let Some(labels) = self.labels() else {
            return Ok(list);
        };
        let mut seen: Vec<&Value> = Vec::with_capacity(labels.len());
        for l in &labels {
            if seen.contains(&l) {
                return Err(format!(
                    "the label {} stands on several rows, so the column '{name}' is no record keyed by it: read the rows with | @, or look the label up with [[{}]]",
                    l.to_quarb(),
                    l.to_quarb()
                ));
            }
            seen.push(l);
        }
        let cells = list.into_items().unwrap_or_default();
        if self.index == 1 {
            return Ok(Value::Record(Record::keyed(
                labels.into_iter().zip(cells).collect(),
            )));
        }
        // a composite index nests the record level by level
        let levels: Vec<Vec<Value>> = self.rows.iter().map(|r| r[..self.index].to_vec()).collect();
        Ok(nest_levels(&levels, &cells, 0))
    }

    /// The table with new rows, infallible: as [`Table::rebuild`],
    /// and where a declared column refuses a cell the columns come
    /// back undeclared (the callers: a filter keeping rows, an
    /// ordering, a walk that cannot change a cell's type).
    pub fn with_rows(&self, rows: Vec<Vec<Value>>) -> Table {
        if self.declared.is_empty() {
            return Table::new(self.header.clone(), rows)
                .with_index(self.index)
                .with_name(self.name.clone());
        }
        match Table::typed(self.header.clone(), rows.clone(), self.declared.clone()) {
            Ok(t) => t.with_index(self.index).with_name(self.name.clone()),
            Err(_) => Table::new(self.header.clone(), rows)
                .with_index(self.index)
                .with_name(self.name.clone()),
        }
    }

    /// The header's name set; `column` and the empty word are none.
    pub fn with_name(mut self, name: Option<String>) -> Table {
        self.name = name.filter(|n| !n.is_empty() && n != "column");
        self
    }

    /// The column's name for a message: the header's, or the
    /// position on the array face.
    pub fn column_name(&self, j: usize) -> String {
        match &self.header {
            Some(h) => h.get(j).cloned().unwrap_or_else(|| (j + 1).to_string()),
            None => (j + 1).to_string(),
        }
    }

    /// The declaration of column `j`: `Some` where it is declared.
    pub fn declaration(&self, j: usize) -> Option<ColumnType> {
        self.declared.get(j).cloned().flatten()
    }

    /// `| header` (spec: The Table, draft 166): the columns as typed
    /// projectors, a declared column with its type (`:A<int>`), an
    /// undeclared one with the star and the type inferred
    /// (`:A<*; float>`); by position on a headerless table
    /// (`:1<int>`). The list is a structured projector: applied to
    /// another table it narrows and casts it.
    pub fn header_projectors(&self) -> Vec<Value> {
        let types = self.types();
        (0..self.width())
            .map(|j| {
                let typing = match self.declaration(j) {
                    Some(t) => Typing::Declared(t),
                    None => Typing::Star(Some(types[j].clone())),
                };
                Value::typed_projection(1, self.column_name(j), Some(typing))
            })
            .collect()
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
        Table {
            header: None,
            rows,
            declared: Vec::new(),
            index: 0,
            name: None,
        }
    }

    /// The column types, one per column: the type of the cells as
    /// stored (text for a column of nulls).
    pub fn types(&self) -> Vec<ColumnType> {
        (0..self.width())
            .map(|j| {
                // a declared column answers its declaration (a tuple
                // column's cells are lists, which no cell types)
                if let Some(ty) = self.declaration(j) {
                    return ty;
                }
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
        let Table { header, rows, .. } = self;
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
            Some(h) if self.header_has_levels() => {
                nest_by_levels(h.iter().cloned().zip(row.iter().cloned()).collect())
            }
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
    /// or by position (`1`, `2`, …) on the array face; on an indexed
    /// table a record of records, each column keyed by the labels
    /// (the index column itself left out).
    pub fn columns_value(&self) -> Value {
        if self.index > 0 || self.header_has_levels() {
            let fields: Vec<(String, Value)> = (self.index..self.width())
                .map(|j| {
                    let name = self.column_name(j);
                    let v = self
                        .column_value(&name)
                        .unwrap_or_else(|_| self.column_at(j));
                    (name, v)
                })
                .collect();
            return nest_by_levels(fields);
        }
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
            declared: self.declared.clone(),
            index: self.index,
            name: self.name.clone(),
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
        // a headerless table is the matrix: its rows print as lists
        // (spec: Table Construction, draft 166); a frame's as records
        let sigil = if self.header.is_some() { '%' } else { '@' };
        for r in &self.rows {
            let cells: Vec<String> = r.iter().map(Value::to_quarb).collect();
            parts.push(format!("{sigil}({})", cells.join("; ")));
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
        let sigil = if self.header.is_some() { '%' } else { '@' };
        for r in &self.rows {
            let cells: Vec<String> = r.iter().map(Value::to_quarb).collect();
            parts.push(format!("{sigil}({})", cells.join("; ")));
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
    /// A record of text keys from its fields.
    pub fn record(fields: Vec<(String, Value)>) -> Value {
        Value::Record(Record::new(fields))
    }

    /// A projector: the ladder (1 `:`, 2 `::`, 3 `:::`) and
    /// its key.
    pub fn projection(ladder: u8, key: impl Into<String>) -> Value {
        Value::Projection {
            ladder,
            key: key.into(),
            typing: None,
        }
    }

    /// A projector with its trait brackets (spec: Projectors, draft
    /// 166): `:A<int>` declares, `:A<*; float>` reports.
    pub fn typed_projection(ladder: u8, key: impl Into<String>, typing: Option<Typing>) -> Value {
        Value::Projection {
            ladder,
            key: key.into(),
            typing,
        }
    }

    /// A typed projector's declared type: `Some` under `<int>`,
    /// `None` on a bare projector, under the star, and on every
    /// other value.
    pub fn declared_type(&self) -> Option<ColumnType> {
        match self {
            Value::Projection {
                typing: Some(t), ..
            } => t.declared(),
            _ => None,
        }
    }

    /// A quarq from its body (spec: Quarqs): the source is the
    /// body's canonical spelling in `q(…)`; `projector` is the
    /// structure of projectors the body folds to, if it is one.
    pub fn quarq(body: Vec<crate::syntax::ast::Command>, projector: Option<Value>) -> Value {
        let source = crate::syntax::unparse::quarq_source(&body);
        Value::Quarq(Quarq {
            source,
            body: std::sync::Arc::new(body),
            projector: projector.map(Box::new),
        })
    }

    /// Whether a projector stands in the value, at any depth of
    /// lists, records and tables: what the glued colon and the
    /// commands that take a held projector read on their topic.
    pub fn holds_projector(&self) -> bool {
        match self {
            Value::Projection { .. } => true,
            Value::List(items) => items.iter().any(Value::holds_projector),
            Value::Record(fields) => fields.iter().any(|(_, v)| v.holds_projector()),
            Value::Table(t) => t.rows.iter().flatten().any(Value::holds_projector),
            _ => false,
        }
    }

    /// A projector's spelling, as the projection is written:
    /// the colons and the key, quoted where it is no identifier.
    pub fn projector_spelling(ladder: u8, key: &str, typing: Option<&Typing>) -> String {
        let mut s = Value::projection_spelling(ladder, key);
        if let Some(t) = typing {
            s.push_str(&t.spelling());
        }
        s
    }

    pub fn projection_spelling(ladder: u8, key: &str) -> String {
        // an identifier, a position (`:1`, `::0`), or a path of
        // identifiers (`:bar:A`, a header level) stands bare
        let ident = |k: &str| {
            let mut cs = k.chars();
            (matches!(cs.next(), Some(c) if c.is_alphabetic() || c == '_')
                && cs.all(|c| c.is_alphanumeric() || c == '_' || c == '-'))
                || (!k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
        };
        let bare = ident(key) || (key.contains(':') && key.split(':').all(ident));
        let key = if bare {
            key.to_string()
        } else {
            format!("'{}'", key.replace('\\', "\\\\").replace('\'', "\\'"))
        };
        format!("{}{key}", ":".repeat(usize::from(ladder.clamp(1, 3))))
    }

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
            Value::Projection { .. } => true,
            Value::Quarq(_) => true,
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
                    .enumerate()
                    .map(|(i, (_, v))| format!("{} = {}", o.key_quarb(i), v.to_quarb()))
                    .collect();
                format!("%({})", inner.join("; "))
            }
            Value::Table(t) => t.to_quarb(),
            // A library value has no Quarb form that reads back; its
            // text is the object's own display.
            Value::Projection {
                ladder,
                key,
                typing,
            } => Value::projector_spelling(*ladder, key, typing.as_ref()),
            Value::Quarq(q) => q.source.clone(),
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
    /// The one rule for an implicit table (spec: The Table, the flat
    /// table): flat rows are the table, typed by `Table::new`; rows
    /// with a structured cell (a frame's object column of lists)
    /// keep their shape as a list of records. The flatness is read
    /// off the raw rows: `Table::new` would coerce the lists to text.
    pub fn table_or_records(header: Option<Vec<String>>, rows: Vec<Vec<Value>>) -> Value {
        let raw = Table {
            header,
            rows,
            declared: Vec::new(),
            index: 0,
            name: None,
        };
        if raw.is_flat() {
            Value::table(Table::new(raw.header, raw.rows))
        } else {
            raw.into_records()
        }
    }

    pub fn display_form(&self) -> String {
        match self {
            // a bare null prints as `null` (spec: the default print,
            // draft 166): an empty line is the empty text's, and what
            // a query shows reads back as what it says
            Value::Null => "null".to_string(),
            Value::List(_) | Value::Record(_) => self.to_quarb(),
            Value::Projection {
                ladder,
                key,
                typing,
            } => Value::projector_spelling(*ladder, key, typing.as_ref()),
            Value::Quarq(q) => q.source.clone(),
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
            Value::Projection {
                ladder,
                key,
                typing,
            } => json_string(&Value::projector_spelling(*ladder, key, typing.as_ref())),
            // The source text: no JSON form keeps a quarq.
            Value::Quarq(q) => json_string(&q.source),
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
            // a projection coerces to text as its bare key
            Value::Projection { key, .. } => Some(format!("s:{key}")),
            Value::Quarq(q) => Some(format!("q:{}", q.source)),
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
            (Line(a, i, da), Line(b, j, db)) => {
                da.cmp(db).then_with(|| match (a.is_nan(), b.is_nan()) {
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
                })
            }
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
        // a projection orders and equals as its bare key's text
        Value::Projection { key, .. } => sort_key(&Value::Str(key.clone()), scale),
        // a quarq orders and equals as its source text
        Value::Quarq(q) => sort_key(&Value::Str(q.source.clone()), scale),
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
            Value::Projection { key, .. } => write!(f, "{key}"),
            Value::Quarq(q) => write!(f, "{}", q.source),
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
/// A float as `pretty` draws it (spec: Output Commands, draft 166):
/// at most `precision` digits after the point, trailing zeros
/// dropped, a whole value bare (`-25.1988`, `-1.773333`, `506`);
/// `0` prints the value whole; NaN and the infinities as they are.
/// The value itself is untouched: the output form, `csv` and `json`
/// keep every digit.
pub fn pretty_float(f: f64, precision: usize) -> String {
    if f.is_nan() || f.is_infinite() {
        return format_float(f);
    }
    let fixed = format!("{f:.precision$}");
    match fixed.split_once('.') {
        Some((whole, frac)) => {
            let frac = frac.trim_end_matches('0');
            let whole = if whole == "-0" && frac.is_empty() {
                "0"
            } else {
                whole
            };
            if frac.is_empty() {
                whole.to_string()
            } else {
                format!("{whole}.{frac}")
            }
        }
        None => fixed,
    }
}

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
        let obj = Value::record(vec![
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
        assert!(!Value::record(Vec::new()).is_truthy());
        assert_eq!(Value::list(Vec::new()).to_quarb(), "@()");
        assert_eq!(Value::record(Vec::new()).to_quarb(), "%()");
        // a key that is not an identifier quotes
        assert_eq!(
            Value::record(vec![("my-key".into(), Value::Int(1))]).to_quarb(),
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
