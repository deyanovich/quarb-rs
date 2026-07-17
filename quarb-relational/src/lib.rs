//! The shared relational model behind Quarb's database adapters.
//!
//! A relational database maps onto the arbor as a two-level tree
//! with the schema's foreign keys as its crosslink fabric: tables
//! as the unnamed root's children, rows named by primary key,
//! columns as properties, and declared foreign keys driving `~>`
//! resolution (with chains), `->`/`<-` crosslinks labeled by
//! column, and `<~` reverse resolution. The resolution hint names a
//! target table for undeclared references.
//!
//! This crate is engine-independent: an engine adapter
//! (`quarb-sqlite`, `quarb-postgres`, …) is a thin *catalog
//! driver* — it connects, introspects its catalog into
//! [`TableSpec`]s, and supplies rows.
//!
//! **Loading model: catalog eager, rows lazy.** The schema is read
//! once at open; a table's rows materialize on *first touch* —
//! navigating into the table, a `~>` resolution landing in it, a
//! backlink scan crossing it, or its `::;n-rows` — via the
//! driver-supplied [`Fetcher`], and are cached for the adapter's
//! lifetime. Untouched tables are never read, so `/artists/2::name`
//! against a hundred-table schema loads one table. A fetch failure
//! surfaces as an empty table plus a warning on stderr (the adapter
//! trait has no error channel mid-navigation); `::;loaded` on a
//! table node reports whether it has materialized.
//! [`RelationalModel::build`] remains the fully-eager form (used by
//! in-memory loads and tests).

use quarb::{AstAdapter, NodeId, Value};
use std::cell::OnceCell;
use std::collections::HashMap;

/// One table's shape, as introspected by an engine adapter.
pub struct TableSpec {
    pub name: String,
    pub columns: Vec<String>,
    /// The single-column primary key's index, if any — it names the
    /// rows. `None` (absent or composite) falls back to the row id.
    pub pk: Option<usize>,
    /// Declared foreign keys: (column index, target table, target
    /// column; an empty target column means the target's key).
    pub fks: Vec<(usize, String, String)>,
}

/// One row: an engine-side row identifier (SQLite's `rowid`, or an
/// ordinal) and the column values.
pub struct RowSpec {
    pub rowid: i64,
    pub values: Vec<Value>,
}

/// The driver-supplied row source: given a table's index and spec,
/// stream its rows. Called at most once per table.
pub type Fetcher = Box<dyn Fn(usize, &TableSpec) -> Result<Vec<RowSpec>, String>>;

struct Row {
    /// The row's edge name: its primary key (or rowid) as text.
    key: String,
    rowid: i64,
    values: Vec<Value>,
}

/// A table's materialized rows plus the key index for `~>`.
struct TableData {
    rows: Vec<Row>,
    by_key: HashMap<String, usize>,
}

struct LazyTable {
    spec: TableSpec,
    data: OnceCell<TableData>,
}

/// Row ids live in the low bits, the table in the high bits, so
/// node ids are stable regardless of load order.
const ROW_BITS: u64 = 40;
const ROW_MASK: u64 = (1 << ROW_BITS) - 1;

/// A database exposed as an arbor: eager or lazily materialized.
pub struct RelationalModel {
    tables: Vec<LazyTable>,
    fetcher: Fetcher,
}

fn materialize(spec: &TableSpec, rows_in: Vec<RowSpec>) -> TableData {
    let rows: Vec<Row> = rows_in
        .into_iter()
        .map(|r| {
            let key = match spec.pk {
                Some(i) => r.values[i].to_string(),
                None => r.rowid.to_string(),
            };
            Row {
                key,
                rowid: r.rowid,
                values: r.values,
            }
        })
        .collect();
    let by_key = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.key.clone(), i))
        .collect();
    TableData { rows, by_key }
}

impl RelationalModel {
    /// The fully-eager form: every table's rows supplied up front.
    pub fn build(input: Vec<(TableSpec, Vec<RowSpec>)>) -> Self {
        let tables = input
            .into_iter()
            .map(|(spec, rows)| {
                let data = OnceCell::new();
                let _ = data.set(materialize(&spec, rows));
                LazyTable { spec, data }
            })
            .collect();
        RelationalModel {
            tables,
            fetcher: Box::new(|_, spec| {
                unreachable!("eager model refetched table '{}'", spec.name)
            }),
        }
    }

    /// The lazy form: the catalog now, each table's rows on first
    /// touch via `fetcher`.
    pub fn lazy(specs: Vec<TableSpec>, fetcher: Fetcher) -> Self {
        RelationalModel {
            tables: specs
                .into_iter()
                .map(|spec| LazyTable {
                    spec,
                    data: OnceCell::new(),
                })
                .collect(),
            fetcher,
        }
    }

    /// A table's rows, materializing them on first touch.
    fn data(&self, t: usize) -> &TableData {
        let table = &self.tables[t];
        table.data.get_or_init(|| {
            let rows = (self.fetcher)(t, &table.spec).unwrap_or_else(|e| {
                eprintln!("quarb-relational: loading table '{}': {e}", table.spec.name);
                Vec::new()
            });
            materialize(&table.spec, rows)
        })
    }

    fn table_node(t: usize) -> NodeId {
        NodeId((t as u64 + 1) << ROW_BITS)
    }

    fn row_node(t: usize, r: usize) -> NodeId {
        NodeId((t as u64 + 1) << ROW_BITS | (r as u64 + 1))
    }

    /// Decode a node id: `(table index, Some(row index))` for rows,
    /// `(table index, None)` for table nodes.
    fn entry(&self, node: NodeId) -> Option<(usize, Option<usize>)> {
        let t = (node.0 >> ROW_BITS).checked_sub(1)? as usize;
        if t >= self.tables.len() {
            return None;
        }
        let r = node.0 & ROW_MASK;
        Some((t, r.checked_sub(1).map(|r| r as usize)))
    }

    fn row(&self, node: NodeId) -> Option<(&TableSpec, &Row)> {
        let (t, r) = self.entry(node)?;
        let row = self.data(t).rows.get(r?)?;
        Some((&self.tables[t].spec, row))
    }

    /// The row node of `table` whose key column equals `value`.
    /// An empty target column means the table's key.
    fn find_target(&self, table: &str, column: &str, value: &Value) -> Option<NodeId> {
        let t = self.tables.iter().position(|t| t.spec.name == table)?;
        let data = self.data(t);
        if column.is_empty() {
            let idx = data.by_key.get(&value.to_string())?;
            return Some(Self::row_node(t, *idx));
        }
        let col = self.tables[t]
            .spec
            .columns
            .iter()
            .position(|c| c == column)?;
        let idx = data
            .rows
            .iter()
            .position(|r| r.values[col].to_string() == value.to_string())?;
        Some(Self::row_node(t, idx))
    }

    /// A human-readable locator: `/table/key` for rows.
    pub fn locator(&self, node: NodeId) -> String {
        match self.entry(node) {
            None => "/".to_string(),
            Some((t, None)) => format!("/{}", self.tables[t].spec.name),
            Some((t, Some(r))) => {
                let table = &self.tables[t];
                match self.data(t).rows.get(r) {
                    Some(row) => format!("/{}/{}", table.spec.name, row.key),
                    None => format!("/{}", table.spec.name),
                }
            }
        }
    }
}

impl AstAdapter for RelationalModel {
    fn root(&self) -> NodeId {
        NodeId(0)
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        if node.0 == 0 {
            return (0..self.tables.len()).map(Self::table_node).collect();
        }
        match self.entry(node) {
            Some((t, None)) => {
                let n = self.data(t).rows.len();
                (0..n).map(|r| Self::row_node(t, r)).collect()
            }
            _ => Vec::new(),
        }
    }

    fn name(&self, node: NodeId) -> Option<String> {
        match self.entry(node) {
            None => None,
            Some((t, None)) => Some(self.tables[t].spec.name.clone()),
            Some((t, Some(r))) => Some(self.data(t).rows.get(r)?.key.clone()),
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        match self.entry(node) {
            None => None,
            Some((_, None)) => Some(NodeId(0)),
            Some((t, Some(_))) => Some(Self::table_node(t)),
        }
    }

    fn property(&self, node: NodeId, name: &str) -> Option<Value> {
        let (spec, row) = self.row(node)?;
        let col = spec.columns.iter().position(|c| c == name)?;
        Some(row.values[col].clone())
    }

    fn metadata(&self, node: NodeId, key: &str) -> Option<Value> {
        match self.entry(node) {
            Some((t, None)) => match key {
                "n-rows" => Some(Value::Int(self.data(t).rows.len() as i64)),
                "columns" => Some(Value::List(
                    self.tables[t]
                        .spec
                        .columns
                        .iter()
                        .map(|c| Value::Str(c.clone()))
                        .collect(),
                )),
                // Whether this table has materialized (reads do not
                // trigger a load — this is the lazy model's
                // introspection surface).
                "loaded" => Some(Value::Bool(self.tables[t].data.get().is_some())),
                _ => None,
            },
            Some((t, Some(r))) => match key {
                "table" => Some(Value::Str(self.tables[t].spec.name.clone())),
                "rowid" => Some(Value::Int(self.data(t).rows.get(r)?.rowid)),
                _ => None,
            },
            None => None,
        }
    }

    /// Follow a reference column to its target row. A declared
    /// foreign key knows its target; the hint names a target table
    /// for undeclared references (`::owner~>users`, resolved
    /// against that table's key).
    fn resolve(&self, node: NodeId, property: &str, hint: Option<&str>) -> Option<NodeId> {
        let (t, r) = self.entry(node)?;
        let spec = &self.tables[t].spec;
        let col = spec.columns.iter().position(|c| c == property)?;
        let value = self.data(t).rows.get(r?)?.values[col].clone();
        if matches!(value, Value::Null) {
            return None;
        }
        if let Some(target) = hint {
            return self.find_target(target, "", &value);
        }
        let (_, target, to) = spec.fks.iter().find(|(c, _, _)| *c == col)?;
        self.find_target(target, to, &value)
    }

    /// Each declared FK column with a non-null value is an outgoing
    /// crosslink, labeled by the column name.
    fn links(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some((t, Some(r))) = self.entry(node) else {
            return Vec::new();
        };
        let spec = &self.tables[t].spec;
        let mut out = Vec::new();
        for (col, target, to) in &spec.fks {
            let value = self.data(t).rows[r].values[*col].clone();
            if matches!(value, Value::Null) {
                continue;
            }
            if let Some(n) = self.find_target(target, to, &value) {
                out.push((spec.columns[*col].clone(), n));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Which rows point here: scan every table with a FK into this
    /// row's table and keep the rows whose FK value matches. Loads
    /// each referencing table.
    fn backlinks(&self, node: NodeId) -> Vec<(String, NodeId)> {
        let Some((tt, Some(tr))) = self.entry(node) else {
            return Vec::new();
        };
        let target_name = self.tables[tt].spec.name.clone();
        let mut out = Vec::new();
        for t in 0..self.tables.len() {
            let fks = self.tables[t].spec.fks.clone();
            for (col, target, to) in &fks {
                if *target != target_name {
                    continue;
                }
                let key = if to.is_empty() {
                    self.data(tt).rows[tr].key.clone()
                } else {
                    let spec = &self.tables[tt].spec;
                    match spec.columns.iter().position(|c| c == to) {
                        Some(i) => self.data(tt).rows[tr].values[i].to_string(),
                        None => continue,
                    }
                };
                let data = self.data(t);
                for (r, row) in data.rows.iter().enumerate() {
                    if row.values[*col].to_string() == key
                        && !matches!(row.values[*col], Value::Null)
                    {
                        out.push((
                            self.tables[t].spec.columns[*col].clone(),
                            Self::row_node(t, r),
                        ));
                    }
                }
            }
        }
        out
    }
}
